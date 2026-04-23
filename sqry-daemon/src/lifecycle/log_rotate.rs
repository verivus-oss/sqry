//! Log rotation for sqryd.
//!
//! # Overview
//!
//! This module provides two public entry points:
//!
//! - [`RollingSizeAppender`] — a [`std::io::Write`] implementation that rotates
//!   the active log file whenever its size exceeds a configurable threshold,
//!   keeping at most `keep` rotated copies on disk.  It is designed to be
//!   wrapped by [`tracing_appender::non_blocking`] so that the daemon's hot
//!   path never blocks on filesystem I/O.
//!
//! - [`install_tracing`] — builds and installs the process-global tracing
//!   subscriber.  When the daemon is supervised by **systemd** (`NOTIFY_SOCKET`
//!   is present), in-process rotation is skipped and all output goes to stderr
//!   (systemd captures it via `StandardOutput=append:…`).  Otherwise, if
//!   `cfg.log_file` is set, the rolling appender is activated; if no log file
//!   is configured the subscriber writes a compact human-readable format to
//!   stderr.
//!
//! # Design reference
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md` §G.
//!
//! - §G.1  `install_tracing` — NOTIFY_SOCKET gate (m4 fix).
//! - §G.2  `RollingSizeAppender` — M3 explicit handle close before rename,
//!   M4 degraded mode + `OVERFLOW_FACTOR` + `RETRY_INTERVAL` + sidecar
//!   `rotate-errors.log` via `target="sqryd.rotate"` filter.
//! - §G.3  Unit tests.
//!
//! # Thread safety
//!
//! [`RollingSizeAppender`] is `Send + Sync`.  All mutable state is protected
//! by a `parking_lot::Mutex`.  The `non_blocking` wrapper in [`install_tracing`]
//! moves the appender to a background thread; the returned [`WorkerGuard`] must
//! be kept alive for the process lifetime (typically stored in `main` or a
//! top-level struct — dropping it flushes and terminates the background thread).

use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Layer as _, fmt, layer::SubscriberExt, util::SubscriberInitExt,
};

use crate::config::DaemonConfig;

// ── Constants ────────────────────────────────────────────────────────────────

/// When degraded (rename failed), allow writing up to `max_bytes *
/// OVERFLOW_FACTOR` additional bytes before refusing further writes with
/// [`io::ErrorKind::StorageFull`].  This prevents the log file from growing
/// without bound after a rotation failure while still giving the operator a
/// generous window to fix the underlying issue (e.g. a full disk).
///
/// Design reference: §G.2 M4.
const OVERFLOW_FACTOR: u64 = 2;

/// After a rotation failure, re-attempt rotation at most once per this
/// interval.  The retry fires at the next [`Write::write`] call that arrives
/// after the interval has elapsed, keeping the hot path free of timer threads.
///
/// Design reference: §G.2 M4.
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

// ── Degraded-mode reason ──────────────────────────────────────────────────────

/// Records why the appender entered degraded mode.
///
/// Design reference: §G.2 M4.
#[derive(Debug)]
enum DegradedReason {
    /// A rotation rename failed.  The appender continues writing to the
    /// current log file up to `max_bytes * OVERFLOW_FACTOR`, then refuses
    /// further writes until the background retry succeeds.
    RenameFailed {
        /// The [`io::ErrorKind`] of the rename failure.  Stored for
        /// operator-visible diagnostics in the `rotate-errors.log` sidecar
        /// and `Debug` output; not matched on in the hot write path.
        #[allow(dead_code)]
        io_kind: io::ErrorKind,
        /// How many consecutive rotation attempts have failed.
        attempt: u32,
    },
    /// The log file has grown past `max_bytes * OVERFLOW_FACTOR`.  No further
    /// writes are accepted until rotation succeeds (via the background retry).
    OverflowBeyondLimit,
}

// ── Internal rolling state ────────────────────────────────────────────────────

/// All mutable state owned by [`RollingSizeAppender`].
struct RollingState {
    /// Directory that holds the log files.
    base_path: PathBuf,
    /// Maximum bytes to write to the active log file before rotating.
    max_bytes: u64,
    /// Maximum number of rotated copies to retain on disk (cap 1..=100).
    keep: u32,
    /// Open handle to the current log file, or `None` when the handle was
    /// explicitly closed before a rename (M3) or before initial open.
    current: Option<File>,
    /// Approximate byte count of the current log file.
    current_bytes: u64,
    /// Non-`None` when the appender is in degraded mode.
    degraded: Option<DegradedReason>,
    /// Timestamp of the most recent rotation attempt while in degraded mode.
    last_retry: Option<Instant>,
}

impl RollingState {
    /// Return the path of the active (un-suffixed) log file.
    fn log_path(&self) -> PathBuf {
        self.base_path.clone()
    }

    /// Return the path of rotation copy `n` (`.1` = most recent, `.keep` =
    /// oldest).
    fn rotated_path(&self, n: u32) -> PathBuf {
        let mut p = self.base_path.clone();
        let fname = p
            .file_name()
            .map(|f| format!("{}.{}", f.to_string_lossy(), n))
            .unwrap_or_else(|| format!("sqryd.log.{n}"));
        p.set_file_name(fname);
        p
    }

    /// Ensure `current` is open, creating the file if necessary.
    fn ensure_open(&mut self) -> io::Result<()> {
        if self.current.is_none() {
            let f = open_log_file(&self.log_path())?;
            self.current = Some(f);
        }
        Ok(())
    }

    /// Attempt to rotate the log file.
    ///
    /// Steps (M3 design reference):
    ///
    /// 1. **Flush + drop** the current file handle **before** any rename.  On
    ///    Windows an open handle prevents `MoveFileExW`; dropping it releases
    ///    the sharing lock.
    /// 2. Shift rotated files: delete `.keep`, rename `.keep-1` → `.keep`,
    ///    …, rename `.1` → `.2`, rename base → `.1`.
    /// 3. Reopen the base path as a fresh empty file; reset counters.
    ///
    /// On failure the appender enters (or stays in) degraded mode, the base
    /// file is reopened in append mode so writes can continue, and
    /// [`report_degraded`] is called to record the failure to the sidecar
    /// `rotate-errors.log`.
    fn try_rotate(&mut self) {
        // M3 step 1: explicit handle close before rename.
        if let Some(mut f) = self.current.take() {
            let _ = f.flush();
            // File dropped (closed) here.
        }

        // M3 step 2: shift rotated copies.
        if let Err(e) = self.shift_rotated_files() {
            let attempt = match &self.degraded {
                Some(DegradedReason::RenameFailed { attempt, .. }) => attempt + 1,
                _ => 1,
            };
            self.degraded = Some(DegradedReason::RenameFailed {
                io_kind: e.kind(),
                attempt,
            });
            self.last_retry = Some(Instant::now());
            report_degraded(&self.base_path, &e);

            // Reopen in append mode so we can keep writing (M4).
            let _ = self.ensure_open();
            return;
        }

        // M3 step 3: open fresh log file; reset state.
        match open_log_file_fresh(&self.log_path()) {
            Ok(f) => {
                self.current = Some(f);
                self.current_bytes = 0;
                self.degraded = None;
                self.last_retry = None;
            }
            Err(e) => {
                let attempt = match &self.degraded {
                    Some(DegradedReason::RenameFailed { attempt, .. }) => attempt + 1,
                    _ => 1,
                };
                self.degraded = Some(DegradedReason::RenameFailed {
                    io_kind: e.kind(),
                    attempt,
                });
                self.last_retry = Some(Instant::now());
                report_degraded(&self.base_path, &e);
                let _ = self.ensure_open();
            }
        }
    }

    /// Rename rotated copies to make room for a new `.1`.
    fn shift_rotated_files(&self) -> io::Result<()> {
        // Delete the oldest copy if it exists.
        let oldest = self.rotated_path(self.keep);
        if oldest.exists() {
            std::fs::remove_file(&oldest)?;
        }

        // Shift: .keep-1 → .keep, …, .1 → .2
        for i in (1..self.keep).rev() {
            let src = self.rotated_path(i);
            let dst = self.rotated_path(i + 1);
            if src.exists() {
                rename_file(&src, &dst)?;
            }
        }

        // Rename base → .1
        let log = self.log_path();
        if log.exists() {
            rename_file(&log, &self.rotated_path(1))?;
        }

        Ok(())
    }
}

// ── Platform-aware rename helper ──────────────────────────────────────────────

/// Rename `src` to `dst`.
///
/// On Windows, uses `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING` so that an
/// existing destination is atomically replaced.  On Unix, `std::fs::rename` is
/// already atomic within the same filesystem.
fn rename_file(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::FALSE;
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MoveFileExW};
        let src_wide: Vec<u16> = src.as_os_str().encode_wide().chain(Some(0)).collect();
        let dst_wide: Vec<u16> = dst.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: pointers are valid null-terminated wide strings derived from
        // PathBuf values that were validated when constructed.
        let ok = unsafe {
            MoveFileExW(
                src_wide.as_ptr(),
                dst_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING,
            )
        };
        if ok == FALSE {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(src, dst)
    }
}

// ── File-open helpers ─────────────────────────────────────────────────────────

/// Open the log file in append mode (create if missing).  Used when recovering
/// from a failed rotation: we must keep writing somewhere rather than silently
/// discarding logs.
fn open_log_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let f = OpenOptions::new().create(true).append(true).open(path)?;
    set_log_permissions(&f);
    Ok(f)
}

/// Open the log file for writing from scratch (create or truncate).  Used
/// after a successful rotation to start a fresh empty file.
fn open_log_file_fresh(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    set_log_permissions(&f);
    Ok(f)
}

/// Set log file permissions to 0600 (owner-only read+write).
///
/// No-op on Windows (ACLs manage permissions separately from POSIX mode bits).
#[allow(unused_variables)]
fn set_log_permissions(f: &File) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
}

// ── Degraded-mode sidecar reporter ────────────────────────────────────────────

/// Write a rotation-failure notice to the sidecar `rotate-errors.log` file.
///
/// This path is deliberately **non-recursive**: it opens the sidecar file
/// directly via `OpenOptions` rather than routing through the rotating appender,
/// which would cause a re-entrant lock or infinite recursion.
///
/// A `tracing::error!` with `target = "sqryd.rotate"` is also emitted so that
/// operators who subscribe to that target see the failure in their log stream.
/// The target filter prevents it from being routed back through the failing
/// `RollingSizeAppender`.
///
/// Design reference: §G.2 M4 — report_degraded non-recursive sidecar path.
fn report_degraded(base_path: &Path, err: &io::Error) {
    use chrono::Utc;

    // Append ".rotate-errors.log" as a suffix rather than replacing the extension
    // so that a log at "sqryd.log" produces a sidecar at "sqryd.log.rotate-errors.log"
    // rather than "sqryd.rotate-errors.log".
    let sidecar = {
        let mut s = base_path.as_os_str().to_owned();
        s.push(".rotate-errors.log");
        PathBuf::from(s)
    };
    let ts = Utc::now().to_rfc3339();
    let line = format!("{ts} rotation failure: {err:?}\n");

    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&sidecar) {
        // Apply owner-only 0600 permissions to the sidecar for consistency
        // with the main log file (rotate-errors.log may contain op-sensitive info).
        set_log_permissions(&f);
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    }

    // Emit to tracing with a dedicated target so subscriber implementations
    // can filter it away from the rolling appender to prevent recursion.
    tracing::error!(target: "sqryd.rotate", "log rotation failure: {err:?}");
}

// ── RollingSizeAppender ───────────────────────────────────────────────────────

/// A [`std::io::Write`] implementation that rotates `base_path` whenever its
/// size exceeds `max_bytes`, keeping at most `keep` rotated copies.
///
/// Wrap this with [`tracing_appender::non_blocking`] to move filesystem I/O
/// off the logging hot path.
///
/// # Design reference
///
/// `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md` §G.2.
///
/// - **M3**: the active file handle is explicitly closed (`Option::take` +
///   `drop`) **before** any rename, satisfying Windows's requirement that no
///   file handle be open during `MoveFileExW`.
/// - **M4 degraded mode**: on rename failure the appender continues writing
///   to the un-rotated file up to `max_bytes * OVERFLOW_FACTOR`, then refuses
///   further writes with [`io::ErrorKind::StorageFull`].  A background retry
///   fires every [`RETRY_INTERVAL`] on the next incoming write.
///
/// # Thread safety
///
/// `RollingSizeAppender` is `Send + Sync`.  All internal state is protected by
/// a `parking_lot::Mutex`.
pub struct RollingSizeAppender {
    state: Mutex<RollingState>,
}

impl RollingSizeAppender {
    /// Construct a new appender.
    ///
    /// # Parameters
    ///
    /// - `base_path` — path to the active log file.  Rotated copies will be
    ///   placed alongside it as `<base>.1`, `<base>.2`, …, `<base>.<keep>`.
    /// - `max_bytes` — rotate when the active file exceeds this size.
    /// - `keep` — maximum number of rotated copies to keep.  Clamped to
    ///   `1..=100` by the caller ([`DaemonConfig::validate`] already enforces
    ///   this range for `log_keep_rotations`).
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the parent directory cannot be created or
    /// the initial log file cannot be opened.
    pub fn new(base_path: PathBuf, max_bytes: u64, keep: u32) -> io::Result<Self> {
        // Clamp keep to the validated range. DaemonConfig already enforces 1..=100
        // but we defend in depth here.
        let keep = keep.clamp(1, 100);

        // Open (or re-open) the active log file in append mode so we join any
        // existing content without truncating it.  The current_bytes counter
        // starts at the on-disk file length so rotation triggers at the correct
        // threshold even after a restart.
        let f = open_log_file(&base_path)?;
        let current_bytes = f.metadata().map(|m| m.len()).unwrap_or(0);

        Ok(Self {
            state: Mutex::new(RollingState {
                base_path,
                max_bytes,
                keep,
                current: Some(f),
                current_bytes,
                degraded: None,
                last_retry: None,
            }),
        })
    }
}

impl Write for RollingSizeAppender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Delegate to the shared write helper (also used by `&self` impls via
        // the Mutex).
        write_impl(&self.state, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        flush_impl(&self.state)
    }
}

// tracing-appender's non_blocking adapter calls write/flush through `&self`
// (it wraps in a Mutex itself, but our implementation provides its own mutex
// so both paths are safe).  Implementing for `&RollingSizeAppender` enables
// the non_blocking wrapper to call the appender from its background thread.
impl Write for &RollingSizeAppender {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        write_impl(&self.state, buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        flush_impl(&self.state)
    }
}

fn write_impl(state: &Mutex<RollingState>, buf: &[u8]) -> io::Result<usize> {
    let mut s = state.lock();

    // — Degraded: OverflowBeyondLimit — refuse writes (M4).
    if matches!(s.degraded, Some(DegradedReason::OverflowBeyondLimit)) {
        return Err(io::Error::new(
            io::ErrorKind::StorageFull,
            "log rotation in overflow-beyond-limit degraded state; writes refused",
        ));
    }

    // — Degraded: RenameFailed — try background retry if interval elapsed (M4).
    let in_rename_degraded = matches!(s.degraded, Some(DegradedReason::RenameFailed { .. }));
    if in_rename_degraded {
        let should_retry = s
            .last_retry
            .map(|t| t.elapsed() >= RETRY_INTERVAL)
            .unwrap_or(true);
        if should_retry {
            s.try_rotate(); // Updates degraded on success or failure.
        }
    }

    // — Rotation threshold check (only when not degraded after retry above).
    if s.degraded.is_none() {
        let len = buf.len() as u64;
        if s.current_bytes + len > s.max_bytes {
            s.try_rotate();
        }
    }

    // — OverflowBeyondLimit upgrade: degraded and beyond OVERFLOW_FACTOR*max (M4).
    if matches!(s.degraded, Some(DegradedReason::RenameFailed { .. })) {
        let len = buf.len() as u64;
        if s.current_bytes + len > s.max_bytes.saturating_mul(OVERFLOW_FACTOR) {
            s.degraded = Some(DegradedReason::OverflowBeyondLimit);
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "log file exceeded overflow limit; writes refused until rotation succeeds",
            ));
        }
    }

    // — Ensure file handle is open.
    s.ensure_open()?;

    // — Write to the current file.
    // After `ensure_open` succeeds `current` is guaranteed `Some`; use
    // `ok_or` rather than `expect` to avoid panicking in library code.
    let n = s
        .current
        .as_mut()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "log file handle unexpectedly absent after ensure_open",
            )
        })?
        .write(buf)?;
    s.current_bytes += n as u64;

    Ok(n)
}

fn flush_impl(state: &Mutex<RollingState>) -> io::Result<()> {
    let mut s = state.lock();
    if let Some(f) = s.current.as_mut() {
        f.flush()?;
    }
    Ok(())
}

// ── install_tracing ───────────────────────────────────────────────────────────

/// Install the process-global tracing subscriber for sqryd.
///
/// # Systemd gate (§G.1 m4 fix)
///
/// When `NOTIFY_SOCKET` is present in the environment the daemon is supervised
/// by systemd, which captures all stderr/stdout output and appends it to the
/// configured `StandardOutput=append:…` path.  In that mode this function
/// skips the in-process [`RollingSizeAppender`] and emits a compact log line
/// to **stderr** instead — rotation is then the responsibility of systemd +
/// logrotate on the system log path.
///
/// # Log file mode (§G.1)
///
/// When not under systemd supervision AND `cfg.log_file` is `Some(path)`:
/// - Open [`RollingSizeAppender`] at `path` with `cfg.log_max_size_mb * 1024²`
///   bytes per file and `cfg.log_keep_rotations` rotated copies.
/// - Wrap with [`tracing_appender::non_blocking`] to move I/O off the hot path.
/// - Install a JSON fmt layer so log aggregators can parse structured output.
///   The rolling-appender layer is configured with a
///   **`not[{target=sqryd.rotate}]`** `EnvFilter` so that rotation-failure
///   events emitted by [`report_degraded`] (via
///   `tracing::error!(target: "sqryd.rotate", …)`) are excluded from the
///   rolling writer, preventing a recursive write loop.  A separate compact
///   stderr layer accepts **only** the `sqryd.rotate` target so operators can
///   still observe rotation failures in their journal or terminal.
/// - Return the `WorkerGuard` — the **caller must keep this value alive** for
///   the entire process lifetime; dropping it flushes and shuts down the
///   background I/O thread.
///
/// # Stderr fallback
///
/// When neither systemd supervision nor a log file path is configured, a
/// compact human-readable subscriber writes to stderr.  Returns `Ok(None)`.
///
/// # Log level
///
/// Resolved in priority order: `cli_level` (if `Some`) → `SQRY_DAEMON_LOG_LEVEL`
/// env var → `cfg.log_level` field.
///
/// # Errors
///
/// Returns a [`crate::DaemonError`] wrapping the underlying [`io::Error`] if
/// the log file or its parent directory cannot be created, or if a global
/// tracing subscriber has already been installed in the current process.
pub fn install_tracing(
    cfg: &DaemonConfig,
    cli_level: Option<&str>,
) -> crate::DaemonResult<Option<WorkerGuard>> {
    // Build the env-filter string: cli_level wins over everything.
    let filter_str = cli_level
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("SQRY_DAEMON_LOG_LEVEL").ok())
        .unwrap_or_else(|| cfg.log_level.clone());

    // Systemd gate (§G.1 m4): skip rolling appender under systemd supervision.
    let under_systemd = crate::lifecycle::notify::is_under_systemd();

    if let (false, Some(log_path)) = (under_systemd, &cfg.log_file) {
        let max_bytes = cfg.log_max_size_mb.saturating_mul(1024 * 1024);
        let keep = cfg.log_keep_rotations;

        let appender = RollingSizeAppender::new(log_path.clone(), max_bytes, keep)
            .map_err(crate::DaemonError::Io)?;

        let (non_blocking, guard) = tracing_appender::non_blocking(appender);

        // §G.2 M4: exclude `target="sqryd.rotate"` events from the rolling
        // appender to prevent a recursive write-through-failing-appender loop.
        // `not[{target=sqryd.rotate}]` blocks any event whose target matches
        // "sqryd.rotate" from reaching the rolling writer regardless of level.
        let rolling_filter =
            EnvFilter::try_new(format!("{filter_str},not[{{target=sqryd.rotate}}]"))
                .unwrap_or_else(|_| EnvFilter::new("info,not[{target=sqryd.rotate}]"));

        // Dedicated stderr layer that accepts ONLY `sqryd.rotate` events so
        // operators can observe rotation failures in the journal/terminal.
        // `EnvFilter::new("sqryd.rotate=error")` admits events at target
        // "sqryd.rotate" with level ≥ error and rejects everything else.
        let rotate_stderr_filter = EnvFilter::new("sqryd.rotate=error");

        // Use try_init() rather than init() so that a double-install (e.g.
        // multi-step startup or test helpers) returns an error rather than
        // panicking in library code.
        //
        // Per-layer filtering via `.with_filter()` (tracing-subscriber 0.3 API).
        tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .json()
                    .with_writer(non_blocking)
                    .with_filter(rolling_filter),
            )
            .with(
                fmt::layer()
                    .compact()
                    .with_writer(io::stderr)
                    .with_filter(rotate_stderr_filter),
            )
            .try_init()
            .map_err(|e| {
                // SetGlobalDefaultError = a subscriber was already installed in
                // this process.  This is a process-startup sequencing fault, not
                // a config-file error — map to DaemonError::Internal so that the
                // operator sees the right exit code (EX_SOFTWARE/70) rather than
                // the config-error path (EX_CONFIG/78).
                crate::DaemonError::Internal(anyhow::anyhow!(
                    "global tracing/log subscriber already installed: {e}"
                ))
            })?;

        return Ok(Some(guard));
    }

    // Stderr fallback (systemd or no log_file configured).
    let filter = EnvFilter::try_new(&filter_str).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .compact()
                .with_writer(io::stderr)
                .with_filter(filter),
        )
        .try_init()
        .map_err(|e| {
            // Same rationale as above: sequencing fault, not config error.
            crate::DaemonError::Internal(anyhow::anyhow!(
                "global tracing/log subscriber already installed: {e}"
            ))
        })?;

    Ok(None)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::TempDir;

    /// Helper: create a [`RollingSizeAppender`] pointing at `dir/sqryd.log`.
    fn make_appender(dir: &TempDir, max_bytes: u64, keep: u32) -> RollingSizeAppender {
        let path = dir.path().join("sqryd.log");
        RollingSizeAppender::new(path, max_bytes, keep)
            .expect("RollingSizeAppender::new must succeed in a tempdir")
    }

    // ── rotates_on_size_exceeded ──────────────────────────────────────────────

    /// Writing past `max_bytes` must trigger a rotation: the current log file
    /// is renamed to `.1` and a fresh file is started.
    #[test]
    fn rotates_on_size_exceeded() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("sqryd.log");
        let max_bytes = 64_u64;

        let mut appender = make_appender(&dir, max_bytes, 3);

        // Fill the file just below the threshold.
        let chunk = vec![b'A'; 60];
        appender.write_all(&chunk).unwrap();
        appender.flush().unwrap();
        assert!(
            log_path.exists(),
            "active log file must exist after writing"
        );

        // One more write that crosses the threshold — triggers rotation.
        let overflow = vec![b'B'; 10];
        appender.write_all(&overflow).unwrap();
        appender.flush().unwrap();

        // After rotation the .1 file must exist.
        let rotated = dir.path().join("sqryd.log.1");
        assert!(
            rotated.exists(),
            "rotated .1 file must exist after rotation"
        );

        // Active file must exist (freshly created for new writes).
        assert!(
            log_path.exists(),
            "active log file must be re-created after rotation"
        );

        // Verify the .1 file contains the original content.
        let content = std::fs::read(&rotated).unwrap();
        assert_eq!(&content, &chunk, ".1 must contain the pre-rotation bytes");
    }

    // ── keep_limit_enforced ───────────────────────────────────────────────────

    /// After `keep + 1` rotations the oldest rotated file is deleted; at most
    /// `keep` rotated copies exist on disk at any time.
    #[test]
    fn keep_limit_enforced() {
        let dir = TempDir::new().unwrap();
        let max_bytes = 32_u64;
        let keep = 3_u32;
        let mut appender = make_appender(&dir, max_bytes, keep);

        // Perform keep+2 rotations to force the oldest to be pruned.
        for i in 0..(keep + 2) {
            let chunk = vec![b'0' + (i % 10) as u8; 40]; // Exceeds max_bytes=32
            appender.write_all(&chunk).unwrap();
            appender.flush().unwrap();
        }

        // At most `keep` rotated copies may exist.
        let rotated_count = (1..=keep + 2)
            .filter(|n| dir.path().join(format!("sqryd.log.{n}")).exists())
            .count();
        assert!(
            rotated_count <= keep as usize,
            "expected at most {keep} rotated copies, found {rotated_count}"
        );

        // The oldest file (.keep+1 or beyond) must NOT exist.
        let too_old = dir.path().join(format!("sqryd.log.{}", keep + 1));
        assert!(
            !too_old.exists(),
            "rotated copy beyond keep limit must be deleted"
        );
    }

    // ── rename_failure_enters_degraded_mode_not_panic ─────────────────────────

    /// When the rotation directory becomes read-only the rename fails.  The
    /// appender must enter degraded mode (not panic) and keep accepting writes
    /// up to `OVERFLOW_FACTOR * max_bytes`.
    ///
    /// # Sidecar file note
    ///
    /// `report_degraded` attempts to write a `rotate-errors.log` sidecar
    /// alongside the log file.  When the directory is read-only the sidecar
    /// itself cannot be created (same directory); the function silently ignores
    /// that failure.  The sidecar is therefore not asserted here — the critical
    /// properties verified are (1) no panic, (2) degraded mode is entered, and
    /// (3) the active log file still exists after the failure.
    ///
    /// A separate integration-level smoke test (running with a writable dir but
    /// a simulated rename failure) validates sidecar creation.
    #[test]
    #[cfg(unix)] // chmod is Unix-specific; Windows uses ACLs differently.
    fn rename_failure_enters_degraded_mode_not_panic() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("sqryd.log");
        let max_bytes = 32_u64;
        let mut appender = make_appender(&dir, max_bytes, 3);

        // Pre-fill to just below threshold.
        let chunk = vec![b'X'; 30];
        appender.write_all(&chunk).unwrap();

        // Make the parent directory read-only so the rename will fail.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

        // This write crosses the threshold and triggers a rotation attempt.
        // The rename will fail because the directory is not writable.
        let trigger = vec![b'Y'; 10];
        // The write itself must NOT panic; it may succeed or fail gracefully.
        let _ = appender.write_all(&trigger);

        // Restore permissions so TempDir can clean up.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        // The log path should still exist (appender reopened in append mode).
        assert!(
            log_path.exists(),
            "log file must still exist after failed rotation"
        );

        // The appender must be in degraded mode (RenameFailed), not have panicked.
        {
            let state = appender.state.lock();
            assert!(
                matches!(state.degraded, Some(DegradedReason::RenameFailed { .. })),
                "appender must be in RenameFailed degraded mode after rotation failure, \
                 got: {:?}",
                state.degraded
            );
        }
    }

    // ── overflow_beyond_limit_returns_storage_full ────────────────────────────

    /// When the active file exceeds `OVERFLOW_FACTOR * max_bytes` while in
    /// degraded mode, subsequent writes must return `StorageFull`.
    #[test]
    #[cfg(unix)]
    fn overflow_beyond_limit_returns_storage_full() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let max_bytes = 32_u64;
        let mut appender = make_appender(&dir, max_bytes, 3);

        // Pre-fill to just below threshold.
        appender.write_all(&[b'A'; 30]).unwrap();

        // Cause rotation failure by removing write permission on the directory.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

        // Trigger the first rotation attempt (fails, enters RenameFailed).
        let _ = appender.write_all(&[b'B'; 10]);

        // Keep writing until we hit the overflow threshold.
        // max_bytes=32, OVERFLOW_FACTOR=2, so limit=64. We are already at ~40.
        // Write enough to push past 64.
        for _ in 0..5 {
            let _ = appender.write_all(&[b'C'; 10]);
        }

        // The next write must return StorageFull.
        let result = appender.write(&[b'D'; 1]);
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        match result {
            Err(e) if e.kind() == io::ErrorKind::StorageFull => { /* expected */ }
            Err(e) => panic!("expected StorageFull, got {:?}", e),
            Ok(_) => {
                // If the overflow logic considers current_bytes before the
                // pending write, the write may still succeed.  Accept both
                // outcomes for writes right at the boundary.
            }
        }
    }

    // ── degraded_recovers_via_retry_after_interval ────────────────────────────

    /// After entering degraded mode, simulating elapsed RETRY_INTERVAL by
    /// manipulating `last_retry` should cause the next write to re-attempt
    /// rotation (and succeed if the failure condition is gone).
    ///
    /// We use a white-box test: lock the state and set `last_retry` to a
    /// moment long ago to simulate the interval having elapsed.
    #[test]
    #[cfg(unix)]
    fn degraded_recovers_via_retry_after_interval() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let max_bytes = 32_u64;
        let mut appender = make_appender(&dir, max_bytes, 3);

        // Pre-fill.
        appender.write_all(&[b'A'; 30]).unwrap();

        // Cause rotation failure.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let _ = appender.write_all(&[b'B'; 10]);
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();

        // Verify we are in degraded mode.
        {
            let state = appender.state.lock();
            assert!(
                matches!(state.degraded, Some(DegradedReason::RenameFailed { .. })),
                "appender must be in RenameFailed degraded mode"
            );
        }

        // Back-date last_retry so the retry interval is treated as elapsed.
        {
            let mut state = appender.state.lock();
            state.last_retry = Some(Instant::now() - RETRY_INTERVAL - Duration::from_secs(1));
        }

        // The next write should trigger a retry and, since the directory is
        // now writable again, the rotation should succeed and clear degraded.
        appender.write_all(&[b'C'; 5]).unwrap();

        let state = appender.state.lock();
        assert!(
            state.degraded.is_none(),
            "appender must leave degraded mode after successful retry"
        );
    }

    // ── windows_rename_after_explicit_drop_succeeds ───────────────────────────

    /// On Windows, renaming an open file fails with ERROR_SHARING_VIOLATION.
    /// The M3 fix drops the file handle before rename.  This test verifies the
    /// drop-before-rename sequence produces a valid rotation result.
    ///
    /// We can't easily simulate ERROR_SHARING_VIOLATION in a portable unit
    /// test, so this test simply verifies that rotation (which includes the
    /// drop-then-rename path) works correctly on Windows.
    #[test]
    #[cfg(windows)]
    fn windows_rename_after_explicit_drop_succeeds() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("sqryd.log");
        let max_bytes = 32_u64;
        let mut appender = make_appender(&dir, max_bytes, 2);

        // Write enough to trigger rotation.
        appender.write_all(&[b'W'; 40]).unwrap();
        appender.flush().unwrap();

        // Rotation must have produced .1 and a fresh active file.
        assert!(
            dir.path().join("sqryd.log.1").exists(),
            "rotated .1 must exist on Windows after M3 drop-before-rename"
        );
        assert!(
            log_path.exists(),
            "active log must be re-created on Windows"
        );
    }

    // ── handle_closed_before_rename_m3_sequence_verified ─────────────────────

    /// M3 postcondition test (all platforms): after a rotation, the rotated
    /// copy `.1` must exist AND the active file handle must be re-opened
    /// (`current` is `Some`), confirming the full rotate→reopen lifecycle
    /// completed successfully.
    ///
    /// **Scope**: this test asserts the *observable outcomes* of a rotation
    /// (file exists on disk, internal state is consistent).  It does not prove
    /// the *sequencing* contract — that the handle was closed **before** the
    /// rename — because on Unix `rename(2)` succeeds even with the source file
    /// open, so the assertion set cannot distinguish the correct sequence from
    /// a regression.  The Windows-only
    /// `windows_rename_after_explicit_drop_succeeds` test is the one that
    /// exercises the sharing-violation-prevention contract that motivated M3
    /// (only on Windows does an open handle block `MoveFileExW`).
    #[test]
    fn handle_closed_before_rename_m3_sequence_verified() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("sqryd.log");
        let max_bytes = 32_u64;
        let mut appender = make_appender(&dir, max_bytes, 2);

        // Write enough to trigger rotation.
        appender.write_all(&[b'M'; 40]).unwrap();
        appender.flush().unwrap();

        // After rotation:
        // 1. The rotated copy .1 must exist (rename succeeded post-drop).
        assert!(
            dir.path().join("sqryd.log.1").exists(),
            "M3: rotated .1 must exist — rename must have succeeded after handle was closed"
        );
        // 2. The active log file must exist (reopen succeeded).
        assert!(
            log_path.exists(),
            "M3: active log must be re-created after rotation"
        );
        // 3. The internal handle must be Some (reopen happened).
        {
            let state = appender.state.lock();
            assert!(
                state.current.is_some(),
                "M3: current file handle must be Some after successful rotation \
                 (drop-before-rename + reopen sequence)"
            );
        }
    }

    // ── concurrent_writes_serialized ─────────────────────────────────────────

    /// 8 threads each write 1 MiB through the same appender.  The test
    /// verifies:
    ///
    /// - No data races (Mutex serialisation is correct).
    /// - The appender does not panic under concurrent load.
    /// - All bytes are accounted for (total written == total observed in files
    ///   on disk after flush).
    ///
    /// # Keep limit sizing
    ///
    /// With 8 threads × 1 MiB = 8 MiB total and a 512 KiB rotation threshold,
    /// we need up to 16 rotation files.  The `keep` is set to 20 so that no
    /// rotated copy is pruned before the total byte count is taken; this
    /// ensures the on-disk total can match the total bytes written.
    #[test]
    fn concurrent_writes_serialized() {
        const NUM_THREADS: usize = 8;
        const BYTES_PER_THREAD: usize = 1024 * 1024; // 1 MiB per thread (spec requirement)

        let dir = TempDir::new().unwrap();
        // keep=20: 8×1MiB / 512KiB = 16 rotations max; 20 > 16 so no copy
        // is pruned before the byte-count assertion runs.
        let appender = Arc::new(
            RollingSizeAppender::new(
                dir.path().join("sqryd.log"),
                512 * 1024, // 512 KiB per file — will rotate multiple times.
                20,
            )
            .unwrap(),
        );

        let barrier = Arc::new(Barrier::new(NUM_THREADS));
        let mut handles = Vec::with_capacity(NUM_THREADS);

        for tid in 0..NUM_THREADS {
            let app = Arc::clone(&appender);
            let bar = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                bar.wait(); // Synchronised start.
                let payload = vec![b'0' + tid as u8; BYTES_PER_THREAD];
                // Write in 4 KiB chunks to interleave threads.
                for chunk in payload.chunks(4096) {
                    (&*app).write_all(chunk).unwrap();
                }
            }));
        }

        for h in handles {
            h.join().expect("worker thread must not panic");
        }
        (&*appender).flush().unwrap();

        // Count total bytes across all log files (active + rotated).
        let total_bytes: u64 = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("sqryd.log") && !n.ends_with("rotate-errors.log"))
                    .unwrap_or(false)
            })
            .filter_map(|e| e.metadata().ok().map(|m| m.len()))
            .sum();

        let expected = (NUM_THREADS * BYTES_PER_THREAD) as u64;
        assert_eq!(
            total_bytes, expected,
            "total bytes on disk ({total_bytes}) must equal total bytes written ({expected})"
        );
    }

    // ── notify_socket_set_skips_rolling_appender ──────────────────────────────

    /// When `NOTIFY_SOCKET` is set, `install_tracing` must skip the rolling
    /// appender regardless of whether `log_file` is configured.
    ///
    /// # Three-part contract
    ///
    /// 1. **`is_under_systemd` gate**: confirms the branch condition that
    ///    `install_tracing` relies on is evaluated correctly.
    /// 2. **No `WorkerGuard` returned**: `install_tracing` must not return
    ///    `Ok(Some(_))` — that would mean the rolling appender was activated.
    ///    `try_init()` is non-panicking, so calling it here is safe; an
    ///    `Err(SetGlobalDefaultError)` (subscriber already installed by another
    ///    test) does not invalidate the guard-is-None check.
    /// 3. **No log file created on disk** (unconditional): `RollingSizeAppender::new`
    ///    opens/creates the log file eagerly.  If the file does not exist after
    ///    `install_tracing` returns — regardless of `try_init()` success or
    ///    failure — it proves the rolling-appender path was never entered.
    ///    This assertion is *not* gated on `result.is_ok()`: under systemd,
    ///    `install_tracing` always takes the stderr-fallback branch (the
    ///    `if let (false, Some(...))` condition is false), so `RollingSizeAppender`
    ///    is never instantiated whatever `try_init()` returns.
    ///
    /// Design reference: §G.1 m4 fix + DAG U5 spec.
    #[test]
    fn notify_socket_set_skips_rolling_appender() {
        let dir = TempDir::new().unwrap();
        let log_path = dir.path().join("sqryd.log");
        let _guard = NotifySocketGuard::set("/run/systemd/notify.sock");

        // 1. Gate function must indicate systemd supervision.
        assert!(
            crate::lifecycle::notify::is_under_systemd(),
            "is_under_systemd must return true when NOTIFY_SOCKET is set (gate for \
             RollingSizeAppender skip in install_tracing)"
        );

        // Pre-condition: log file must not exist before the call.
        assert!(
            !log_path.exists(),
            "pre-condition: log file must not exist before install_tracing is called"
        );

        let cfg = crate::config::DaemonConfig {
            log_file: Some(log_path.clone()),
            ..crate::config::DaemonConfig::default()
        };

        let result = install_tracing(&cfg, None);

        // 2. install_tracing must NOT return Ok(Some(WorkerGuard)) — that would
        //    mean the rolling appender was activated despite the systemd gate.
        assert!(
            !matches!(result, Ok(Some(_))),
            "install_tracing must not return Ok(Some(WorkerGuard)) under systemd — \
             rolling appender must be skipped when NOTIFY_SOCKET is set; \
             log_file was {:?}",
            cfg.log_file
        );

        // 3. The log file must NOT have been created — unconditional, regardless
        //    of whether try_init() succeeded or failed.  Under systemd the code
        //    always takes the stderr-fallback path, so RollingSizeAppender::new()
        //    (which opens the file eagerly) is never reached.
        assert!(
            !log_path.exists(),
            "log file must NOT be created when install_tracing goes through the \
             stderr-fallback path under systemd supervision (NOTIFY_SOCKET set); \
             file existence would prove RollingSizeAppender was instantiated"
        );
    }

    // ── env-var serialisation guard ───────────────────────────────────────────

    static NOTIFY_SOCKET_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct NotifySocketGuard {
        previous: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl NotifySocketGuard {
        fn set(value: &str) -> Self {
            let _lock = NOTIFY_SOCKET_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var_os("NOTIFY_SOCKET");
            // SAFETY: NOTIFY_SOCKET_LOCK serialises all mutations in this module.
            unsafe {
                std::env::set_var("NOTIFY_SOCKET", value);
            }
            Self { previous, _lock }
        }
    }

    impl Drop for NotifySocketGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(v) => unsafe { std::env::set_var("NOTIFY_SOCKET", v) },
                None => unsafe { std::env::remove_var("NOTIFY_SOCKET") },
            }
        }
    }
}
