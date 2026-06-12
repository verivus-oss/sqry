//! Pidfile and exclusive-lock management for `sqryd`.
//!
//! ## Overview
//!
//! Every running `sqryd` process holds an **OFD-level exclusive flock** on
//! `sqryd.lock` for its entire lifetime.  Because the flock is tied to the
//! *open file description* (OFD) rather than the process, it survives
//! `fork`+`exec` when the FD is inherited with `FD_CLOEXEC` cleared — this
//! is the mechanism used in the `--detach` double-fork path (see §C.3.2 in
//! the design doc).
//!
//! ## Key types
//!
//! - [`PidfileOwnership`] — state machine governing which process is
//!   responsible for unlinking `sqryd.pid` on drop.
//! - [`PidfileLock`] — RAII handle that holds the exclusive flock and the
//!   pidfile.  Drop behaviour depends on the ownership state.
//! - [`acquire_pidfile_lock`] — the normal entry point; fails with
//!   [`DaemonError::AlreadyRunning`] if the lock is contended.
//!
//! ## Ownership model (design §D.2 / iter-2 M6 fix)
//!
//! ```text
//! WriteOwner  ──hand_off_to_adopter()──►  Handoff
//!     │                                      │
//!     │ drop                                 │ drop
//!     ▼                                      ▼
//! unlink pidfile + unlock           close FD only (no LOCK_UN)
//!
//! Adopted  ──────────────────────────────────►
//!     │ drop
//!     ▼
//! unlink pidfile + unlock
//! ```
//!
//! In the detach path the parent starts as `WriteOwner`, the grandchild wraps
//! the inherited FD via [`PidfileLock::adopt`] (starting as `Adopted`), and
//! the parent transitions to `Handoff` once the grandchild signals ready.
//! Exactly one party — the grandchild — unlinks the pidfile at graceful
//! shutdown.
//!
//! **OFD flock invariant (M-2):** `Handoff` drop must NOT call `flock(LOCK_UN)`.
//! OFD-level locks are released when ALL FDs sharing the OFD are closed or
//! when an explicit `LOCK_UN` is issued on any of them.  The grandchild holds
//! the inherited FD; if the parent calls `LOCK_UN`, the grandchild's lock is
//! released too.  The parent therefore only closes its FD (decrementing the
//! OFD reference count) — the lock remains held by the grandchild until it
//! drops its `Adopted` [`PidfileLock`].
//!
//! ## Stale-pidfile recovery (design §D.3)
//!
//! On **abnormal** process termination (SIGKILL, panic=abort, segfault, OOM)
//! the `Drop` impl does **not** run.  However, the kernel releases OFD-level
//! flocks unconditionally when the process's FD table is torn down by
//! `do_exit`, so the lock is released even without Rust `Drop` executing.
//! The next `sqryd start` calls `try_lock_exclusive` on the same lockfile
//! inode; success proves there is no live owner, and the fresh daemon
//! overwrites the stale `sqryd.pid` atomically.
//!
//! ## Lockfile unlink policy (design §D.4)
//!
//! The lockfile (`sqryd.lock`) is **never** unlinked.  Inode stability is the
//! linchpin of the stale-recovery correctness argument — all contenders must
//! flock the same inode.
//!
//! ## Design reference
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
//! §D (pidfile locking), §C.3.2 (detach path), §C.3.3 (adopted-FD API).

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    path::PathBuf,
};

use fs2::FileExt as _;
use tracing::{debug, warn};

use crate::{
    config::DaemonConfig,
    error::{DaemonError, DaemonResult},
};

#[cfg(all(unix, target_os = "linux"))]
const NFS_SUPER_MAGIC: libc::c_long = 0x6969;

// ---------------------------------------------------------------------------
// Ownership state machine
// ---------------------------------------------------------------------------

/// Tracks which entity is responsible for unlinking the pidfile on [`Drop`].
///
/// Transitions:
/// - [`WriteOwner`] is the initial state after [`acquire_pidfile_lock`].
/// - [`Handoff`] is set by the parent process in the detach path after the
///   grandchild signals ready (via [`PidfileLock::hand_off_to_adopter`]).
/// - [`Adopted`] is the initial state of a [`PidfileLock`] constructed with
///   [`PidfileLock::adopt`].
///
/// [`WriteOwner`]: PidfileOwnership::WriteOwner
/// [`Handoff`]: PidfileOwnership::Handoff
/// [`Adopted`]: PidfileOwnership::Adopted
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidfileOwnership {
    /// This handle wrote the pidfile.  Drop must unlink it.
    WriteOwner,
    /// This handle transferred pidfile lifecycle to an adopter.  Drop must
    /// **not** unlink the pidfile; only the flock is released.
    Handoff,
    /// This handle adopted a pre-locked FD.  Drop must unlink the pidfile
    /// (matching the behaviour of [`WriteOwner`]).
    Adopted,
}

// ---------------------------------------------------------------------------
// PidfileLock
// ---------------------------------------------------------------------------

/// RAII handle holding an exclusive OFD-level flock on `sqryd.lock`.
///
/// Construct via [`acquire_pidfile_lock`] (normal start) or
/// [`PidfileLock::adopt`] (grandchild after `--detach` FD inheritance).
///
/// # Drop behaviour
///
/// | [`PidfileOwnership`]    | Unlinks pidfile? | Calls `unlock()`? |
/// |-------------------------|------------------|-------------------|
/// | [`WriteOwner`]          | yes              | yes               |
/// | [`Handoff`]             | no               | **no** (M-2 fix)  |
/// | [`Adopted`]             | yes              | yes               |
///
/// `Handoff` closes the parent's `File` handle (decrementing the OFD
/// reference count) WITHOUT calling `flock(LOCK_UN)`.  This is intentional:
/// `LOCK_UN` on a shared OFD releases the lock for ALL processes sharing that
/// OFD — including the grandchild — which would break the singleton guarantee.
///
/// Drop MUST NOT panic — all cleanup uses `let _ = ...`.
///
/// [`WriteOwner`]: PidfileOwnership::WriteOwner
/// [`Handoff`]: PidfileOwnership::Handoff
/// [`Adopted`]: PidfileOwnership::Adopted
pub struct PidfileLock {
    /// Open file handle for `sqryd.lock`; the exclusive flock is attached to
    /// this handle's OFD.
    lock: File,
    /// Path to `sqryd.pid`.
    pidfile: PathBuf,
    /// Path to `sqryd.lock` (never unlinked; kept for diagnostics and adopt).
    lockfile: PathBuf,
    /// Who is responsible for unlinking the pidfile on drop.
    ownership: PidfileOwnership,
}

impl std::fmt::Debug for PidfileLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PidfileLock")
            .field("pidfile", &self.pidfile)
            .field("lockfile", &self.lockfile)
            .field("ownership", &self.ownership)
            .finish_non_exhaustive()
    }
}

impl PidfileLock {
    /// Returns the path to the pidfile (`sqryd.pid`).
    #[must_use]
    pub fn pidfile_path(&self) -> &PathBuf {
        &self.pidfile
    }

    /// Returns the path to the lockfile (`sqryd.lock`).
    #[must_use]
    pub fn lockfile_path(&self) -> &PathBuf {
        &self.lockfile
    }

    /// Returns the current ownership state.
    #[must_use]
    pub fn ownership(&self) -> PidfileOwnership {
        self.ownership
    }

    /// Returns the raw OS file descriptor for the underlying lock file.
    ///
    /// This is used by the `--detach` parent path in
    /// `sqry_daemon::entrypoint` to obtain the FD that must be passed to
    /// the grandchild process via `SQRYD_LOCK_FD` so the grandchild can call
    /// [`PidfileLock::adopt`].
    ///
    /// The returned FD is owned by this `PidfileLock`; the caller MUST NOT
    /// close it and MUST NOT use it after this lock is dropped.
    #[cfg(unix)]
    #[must_use]
    pub fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd as _;
        self.lock.as_raw_fd()
    }

    /// Transition from [`WriteOwner`] to [`Handoff`].
    ///
    /// Called by the parent process in the `--detach` path after the grandchild
    /// has signalled ready via the self-pipe.  After this call the parent's
    /// `Drop` will release the flock but **not** unlink the pidfile — the
    /// grandchild's [`Adopted`] drop owns that responsibility.
    ///
    /// Panics if the current state is not [`WriteOwner`] — calling this on a
    /// [`Handoff`] or [`Adopted`] handle is always a programming error and
    /// must be caught in both debug and release builds to prevent incorrect
    /// drop behaviour (skipping required pidfile unlink).
    ///
    /// # Panics
    ///
    /// Panics when called on a lock that is not in [`WriteOwner`] state.
    ///
    /// [`WriteOwner`]: PidfileOwnership::WriteOwner
    /// [`Handoff`]: PidfileOwnership::Handoff
    /// [`Adopted`]: PidfileOwnership::Adopted
    pub fn hand_off_to_adopter(&mut self) {
        assert_eq!(
            self.ownership,
            PidfileOwnership::WriteOwner,
            "hand_off_to_adopter called on non-WriteOwner PidfileLock (ownership={:?})",
            self.ownership,
        );
        self.ownership = PidfileOwnership::Handoff;
    }

    /// Write the current process's PID (decimal + newline) to the pidfile
    /// atomically (tmp + rename).
    ///
    /// This is called by:
    /// - The foreground / `run_start` path immediately after
    ///   [`acquire_pidfile_lock`] succeeds.
    /// - The grandchild in the `--detach` path after
    ///   [`PidfileLock::adopt`] to overwrite the parent's PID with the
    ///   grandchild's PID.
    ///
    /// The pidfile is created with mode `0644` (world-readable so that
    /// diagnostic tools can read it without elevated privileges).
    ///
    /// # Errors
    ///
    /// Returns an error if the temporary pidfile cannot be written, flushed, or
    /// atomically renamed into place.
    pub fn write_pid(&self, pid: u32) -> DaemonResult<()> {
        write_pidfile_atomic(&self.pidfile, pid)
    }

    /// Wrap an already-locked FD inherited across `fork`+`exec` into a
    /// `PidfileLock` with `ownership = Adopted`.
    ///
    /// # Safety
    ///
    /// The caller MUST ensure:
    ///
    /// 1. `fd` is a valid, open file descriptor in the current process.
    /// 2. `fd` is the target of an active OFD-level exclusive flock (typically
    ///    inherited across `fork`+`exec` with `FD_CLOEXEC` cleared).
    /// 3. No other [`PidfileLock`] in this process owns `fd`.
    /// 4. The caller MUST NOT close `fd` separately — dropping the returned
    ///    [`PidfileLock`] will close it (and release the flock) via the
    ///    [`File`] destructor.
    ///
    /// Violating any of these invariants is undefined behaviour (double-free,
    /// double-close, or stale-lock ABA).
    #[cfg(unix)]
    #[must_use]
    pub unsafe fn adopt(fd: std::os::fd::RawFd, pidfile: PathBuf, lockfile: PathBuf) -> Self {
        use std::os::unix::io::FromRawFd as _;
        // SAFETY: caller guarantees fd is valid and exclusively owned.
        let lock = unsafe { File::from_raw_fd(fd) };
        Self {
            lock,
            pidfile,
            lockfile,
            ownership: PidfileOwnership::Adopted,
        }
    }
}

impl Drop for PidfileLock {
    /// Release the flock and, when appropriate, unlink the pidfile.
    ///
    /// Drop MUST NOT panic — every operation is wrapped in `let _ = ...`.
    ///
    /// # Handoff and OFD-level flock invariant (M-2 fix)
    ///
    /// In the `--detach` double-fork path the parent transitions to `Handoff`
    /// after the grandchild signals ready.  The grandchild inherits the same
    /// OFD (open file description) via FD inheritance across `fork`+`exec`.
    ///
    /// `flock(LOCK_UN)` operates at the *OFD* level: it releases the lock for
    /// every FD in every process that shares the same OFD.  Calling `unlock()`
    /// in the `Handoff` drop would therefore release the grandchild's lock too,
    /// breaking the singleton guarantee.
    ///
    /// The correct behaviour is to **close** the parent's FD without explicitly
    /// unlocking it.  Closing a single FD that shares an OFD with another
    /// process's FD is safe: the kernel decrements the OFD reference count but
    /// keeps the lock alive as long as any process still holds an open FD on
    /// that OFD.  The lock remains held until the grandchild closes its
    /// inherited FD (which happens in `Adopted` drop).
    fn drop(&mut self) {
        // Unlink the pidfile for WriteOwner and Adopted; skip for Handoff.
        match self.ownership {
            PidfileOwnership::WriteOwner | PidfileOwnership::Adopted => {
                let result = fs::remove_file(&self.pidfile);
                match result {
                    Ok(()) => {
                        debug!(path = %self.pidfile.display(), "pidfile removed");
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        // Already gone — benign.
                        debug!(path = %self.pidfile.display(), "pidfile already absent on drop");
                    }
                    Err(e) => {
                        warn!(
                            path = %self.pidfile.display(),
                            err = %e,
                            "failed to remove pidfile on drop"
                        );
                    }
                }
            }
            PidfileOwnership::Handoff => {
                debug!(
                    path = %self.pidfile.display(),
                    "pidfile handoff — skipping unlink on parent drop"
                );
            }
        }

        // Release the flock for WriteOwner and Adopted.
        //
        // For Handoff: do NOT call unlock() — see the doc comment above.
        // Closing the parent's File handle decrements the OFD reference count
        // without calling LOCK_UN, which preserves the grandchild's lock.
        // The File destructor closes the FD automatically, so no explicit
        // action is needed here; we just skip the `unlock()` call.
        match self.ownership {
            PidfileOwnership::WriteOwner | PidfileOwnership::Adopted => {
                // fs2's `unlock` calls flock(fd, LOCK_UN) on Unix and
                // UnlockFile on Windows.  We log but do not propagate errors
                // because (a) the kernel releases OFD locks on process exit
                // anyway, and (b) Drop must not panic.
                let result = self.lock.unlock();
                match result {
                    Ok(()) => {
                        debug!(lockfile = %self.lockfile.display(), "flock released");
                    }
                    Err(e) => {
                        warn!(
                            lockfile = %self.lockfile.display(),
                            err = %e,
                            "flock release failed on drop (kernel will clean up on exit)"
                        );
                    }
                }
            }
            PidfileOwnership::Handoff => {
                // The File handle is dropped here by RAII (closing the FD),
                // but we do NOT call unlock() — see the doc comment.
                debug!(
                    lockfile = %self.lockfile.display(),
                    "pidfile handoff — closing parent FD without LOCK_UN (grandchild retains lock)"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Acquire
// ---------------------------------------------------------------------------

/// Attempt to acquire the exclusive pidfile lock for this daemon instance.
///
/// ## Algorithm
///
/// 1. Ensure the runtime directory exists with mode `0700` (Unix).
/// 2. Open-or-create `sqryd.lock` for read+write.
/// 3. Set the lockfile permissions to `0600` (Unix).
/// 4. Call [`fs2::FileExt::try_lock_exclusive`]:
///    - `WouldBlock` → read the pidfile for the owner PID (best-effort) and
///      return [`DaemonError::AlreadyRunning`].
///    - Other I/O error → propagate as [`DaemonError::Io`].
/// 5. Write the current process's PID to `sqryd.pid` atomically
///    (tmp-file + rename) with mode `0644`.
/// 6. Return a [`PidfileLock`] with `ownership = WriteOwner`.
///
/// ## Stale-pidfile recovery (§D.3)
///
/// Step 4 succeeding on the same lockfile inode proves there is no live OFD
/// owner — the kernel releases flocks on process death via any mechanism
/// (SIGKILL, segfault, panic=abort, OOM kill) because the reference count on
/// the OFD drops to zero when the process's FD table is torn down.  Step 5
/// then overwrites any stale `sqryd.pid` atomically.
///
/// ## NFS warning
///
/// If the runtime directory resides on an NFS mount, `flock(2)` semantics are
/// not guaranteed.  A [`tracing::warn`] is emitted if the mount type is
/// detectable; NFS operators are out of scope.
///
/// # Errors
///
/// Returns [`DaemonError::AlreadyRunning`] when another process owns the
/// configured lockfile. Returns [`DaemonError::Io`] for runtime-directory
/// creation, lockfile open, PID-file write, stale PID-file cleanup, inherited
/// descriptor validation, or platform locking failures.
pub fn acquire_pidfile_lock(cfg: &DaemonConfig) -> DaemonResult<PidfileLock> {
    let lock_path = cfg.lock_path();
    let pidfile = cfg.pid_path();

    // 1. Ensure runtime dir (and its parent) with secure permissions.
    ensure_runtime_dir(&lock_path)?;

    // 2. Open-or-create the lockfile with mode 0600 atomically on Unix to
    //    avoid a creation-time window where the file is visible with the
    //    process umask permissions.  On Windows, ACLs govern access; the
    //    extra `set_permissions` call is a no-op but kept for portability.
    #[cfg(unix)]
    let lock_file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)?
    };
    #[cfg(not(unix))]
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;

    // 3. Ensure lockfile permissions are 0600.  On Unix this is a no-op for
    //    newly-created files (mode was set above), but also correctly
    //    normalises pre-existing lockfiles left behind by a previous sqryd
    //    build that did not use OpenOptionsExt::mode.
    #[cfg(unix)]
    set_permissions_0600(&lock_path)?;

    // Warn if the runtime dir appears to be on NFS (best-effort detection).
    #[cfg(unix)]
    warn_if_nfs(lock_path.parent().unwrap_or(&lock_path));

    // 4. Try to acquire the exclusive flock.
    match lock_file.try_lock_exclusive() {
        Ok(()) => {
            debug!(lockfile = %lock_path.display(), "exclusive flock acquired");
        }
        Err(e) if is_would_block(&e) => {
            // Another daemon holds the lock.  Read the pidfile for diagnostics.
            let owner_pid = read_pid(&pidfile);
            debug!(
                lockfile = %lock_path.display(),
                owner_pid = ?owner_pid,
                "flock contended — daemon already running"
            );
            return Err(DaemonError::AlreadyRunning {
                owner_pid,
                socket: cfg.socket_path(),
                lock: lock_path,
            });
        }
        Err(e) => {
            return Err(DaemonError::Io(e));
        }
    }

    // 5. Write the current PID atomically.
    let pid = std::process::id();
    write_pidfile_atomic(&pidfile, pid)?;

    Ok(PidfileLock {
        lock: lock_file,
        pidfile,
        lockfile: lock_path,
        ownership: PidfileOwnership::WriteOwner,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Ensure the directory containing `lockfile` exists with mode `0700` (Unix).
fn ensure_runtime_dir(lockfile: &std::path::Path) -> DaemonResult<()> {
    let dir = lockfile.parent().ok_or_else(|| {
        DaemonError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "lockfile path has no parent directory",
        ))
    })?;

    fs::create_dir_all(dir)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let perms = fs::Permissions::from_mode(0o700);
        // Propagate permission errors: if we created this directory we must be
        // able to chmod it.  If we are merely re-using a pre-existing directory
        // owned by the same UID the chmod should succeed; if the directory is
        // owned by a different user (e.g. a shared XDG_RUNTIME_DIR) the error
        // is surfaced to the operator rather than silently running with an
        // insecure mode.
        fs::set_permissions(dir, perms)?;
    }

    Ok(())
}

/// Set Unix permissions on `path` to `0600`.
#[cfg(unix)]
fn set_permissions_0600(path: &std::path::Path) -> DaemonResult<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let perms = fs::Permissions::from_mode(0o600);
    fs::set_permissions(path, perms)?;
    Ok(())
}

/// Emit a warning if the parent directory of `path` appears to be on NFS.
///
/// Detection is best-effort: on Linux we check the filesystem type via
/// `statfs(2)`.  On macOS we check `f_fstypename`.  On other platforms we
/// do nothing.  Failure to detect does not affect correctness.
#[cfg(unix)]
fn warn_if_nfs(dir: &std::path::Path) {
    if is_nfs(dir) {
        warn!(
            dir = %dir.display(),
            "sqryd runtime directory appears to be on NFS; flock(2) semantics \
             are not guaranteed on NFS mounts — consider using a local \
             filesystem for SQRY_DAEMON_SOCKET / XDG_RUNTIME_DIR"
        );
    }
}

/// Returns `true` if `dir` is on an NFS filesystem (best-effort).
#[cfg(all(unix, target_os = "linux"))]
fn is_nfs(dir: &std::path::Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let Ok(c_path) = CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: c_path is a valid null-terminated string.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &raw mut buf) };
    if rc != 0 {
        return false;
    }
    buf.f_type == NFS_SUPER_MAGIC
}

#[cfg(all(unix, target_os = "macos"))]
fn is_nfs(dir: &std::path::Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let c_path = match CString::new(dir.as_os_str().as_bytes()) {
        Ok(p) => p,
        Err(_) => return false,
    };
    // SAFETY: c_path is a valid null-terminated string.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut buf) };
    if rc != 0 {
        return false;
    }
    // On macOS the filesystem type name is a C string in f_fstypename.
    // SAFETY: f_fstypename is a NUL-terminated array in the statfs struct.
    let ftype = unsafe { std::ffi::CStr::from_ptr(buf.f_fstypename.as_ptr()) };
    ftype.to_bytes() == b"nfs"
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn is_nfs(_dir: &std::path::Path) -> bool {
    false
}

/// Write `pid` (decimal + newline) to `pidfile` atomically via a tmp file +
/// rename.  The file is created with mode `0644`.
fn write_pidfile_atomic(pidfile: &std::path::Path, pid: u32) -> DaemonResult<()> {
    let dir = pidfile.parent().ok_or_else(|| {
        DaemonError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pidfile path has no parent directory",
        ))
    })?;

    // Write to a sibling tmp file so the rename is atomic within the same dir.
    let tmp_path = dir.join(format!(".sqryd.pid.tmp.{}", std::process::id()));

    {
        let mut tmp = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;

        // Set 0644 before writing so the content is never visible with wrong perms.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let perms = fs::Permissions::from_mode(0o644);
            tmp.set_permissions(perms)?;
        }

        writeln!(tmp, "{pid}")?;
        tmp.flush()?;
        // tmp dropped here — file is closed before rename.
    }

    // Atomic replace.
    fs::rename(&tmp_path, pidfile)?;
    debug!(path = %pidfile.display(), pid, "pidfile written");
    Ok(())
}

/// Read the PID from `pidfile` (best-effort; returns `None` on any failure).
#[must_use]
pub fn read_pid(pidfile: &std::path::Path) -> Option<u32> {
    let text = fs::read_to_string(pidfile).ok()?;
    text.trim().parse::<u32>().ok()
}

/// Returns `true` if `e` indicates a "would block" / "resource busy" locking
/// failure — i.e. the lock is held by another process.
fn is_would_block(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::WouldBlock
        // fs2 on Linux returns EWOULDBLOCK / EAGAIN; on Windows it returns a
        // raw OS error that maps to `WouldBlock`.  Some older kernels may also
        // return `ResourceBusy` which is represented differently.
        || e.raw_os_error()
            .is_some_and(|c| {
                #[cfg(unix)]
                {
                    c == libc::EWOULDBLOCK || c == libc::EAGAIN
                }
                #[cfg(not(unix))]
                {
                    // Windows: ERROR_LOCK_VIOLATION (33)
                    c == 33
                }
            })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Use the crate-wide TEST_ENV_LOCK to serialise XDG_RUNTIME_DIR
    // mutations across ALL test modules in the same binary (pidfile,
    // detach, config).  A per-module mutex is insufficient because cargo
    // test runs all #[test] fns as threads in one process.
    // -----------------------------------------------------------------------
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    // -----------------------------------------------------------------------
    // Helper: build a minimal DaemonConfig pointing at a temp directory.
    //
    // The fixture holds both the `TempDir` (to keep it alive) and a mutex
    // guard that prevents other env-manipulating tests from running
    // concurrently.  On drop, `XDG_RUNTIME_DIR` is restored to whatever it
    // was before the fixture was created (typically the real XDG runtime dir).
    // -----------------------------------------------------------------------

    struct TestCfg {
        _tmp: TempDir,
        cfg: DaemonConfig,
        _guard: std::sync::MutexGuard<'static, ()>,
        prior_xdg: Option<String>,
    }

    impl TestCfg {
        /// Creates a temp dir, sets `XDG_RUNTIME_DIR` to it (under an
        /// exclusive mutex), and returns a `DaemonConfig` whose `lock_path()`
        /// and `pid_path()` resolve inside the temp dir.
        fn new() -> Self {
            // Acquire the serialisation lock BEFORE reading the env var so
            // we see the canonical pre-test state.
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

            let tmp = TempDir::new().expect("TempDir::new");

            // Override XDG_RUNTIME_DIR so `runtime_dir()` / `lock_path()` /
            // `pid_path()` resolve inside our temp dir.
            let prior_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
            // SAFETY: single-threaded by virtue of the mutex above.
            #[allow(unsafe_code)]
            unsafe {
                std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
            }

            let mut cfg = DaemonConfig::default();
            cfg.socket.path = Some(tmp.path().join("sqry").join("sqryd.sock"));

            Self {
                _tmp: tmp,
                cfg,
                _guard: guard,
                prior_xdg,
            }
        }

        fn cfg(&self) -> &DaemonConfig {
            &self.cfg
        }
    }

    impl Drop for TestCfg {
        fn drop(&mut self) {
            // Restore the original XDG_RUNTIME_DIR.
            #[allow(unsafe_code)]
            unsafe {
                match self.prior_xdg.take() {
                    Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                    None => std::env::remove_var("XDG_RUNTIME_DIR"),
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // T1: acquire_succeeds_on_clean_dir
    // -----------------------------------------------------------------------

    #[test]
    fn acquire_succeeds_on_clean_dir() {
        let fix = TestCfg::new();
        let lock = acquire_pidfile_lock(fix.cfg()).expect("acquire should succeed on clean dir");
        assert_eq!(lock.ownership(), PidfileOwnership::WriteOwner);
        assert!(fix.cfg().pid_path().exists(), "pidfile must be created");
        drop(lock);
    }

    // -----------------------------------------------------------------------
    // T2: acquire_rejects_already_held
    // -----------------------------------------------------------------------

    #[test]
    fn acquire_rejects_already_held() {
        let fix = TestCfg::new();
        let _lock1 = acquire_pidfile_lock(fix.cfg()).expect("first acquire should succeed");
        let err = acquire_pidfile_lock(fix.cfg()).expect_err("second acquire must fail");
        match err {
            DaemonError::AlreadyRunning { owner_pid, .. } => {
                // Owner PID must be readable (this process wrote the pidfile).
                assert_eq!(
                    owner_pid,
                    Some(std::process::id()),
                    "owner_pid must match current process"
                );
            }
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // T3: drop_removes_pidfile_but_not_lockfile
    // -----------------------------------------------------------------------

    #[test]
    fn drop_removes_pidfile_but_not_lockfile() {
        let fix = TestCfg::new();
        let lock = acquire_pidfile_lock(fix.cfg()).expect("acquire");
        let pidfile = fix.cfg().pid_path();
        let lockfile = fix.cfg().lock_path();

        assert!(pidfile.exists(), "pidfile must exist before drop");
        assert!(lockfile.exists(), "lockfile must exist before drop");

        drop(lock);

        assert!(!pidfile.exists(), "pidfile must be removed after drop");
        assert!(
            lockfile.exists(),
            "lockfile must NOT be removed after drop (§D.4)"
        );
    }

    // -----------------------------------------------------------------------
    // T4: acquire_reclaims_stale_pidfile
    //
    // Simulates §D.3: a previous process crashed leaving a stale pidfile and
    // lockfile on disk.  The lockfile must NOT be locked (simulating kernel
    // flock release on process death).  The new acquire must succeed and
    // overwrite the stale pidfile.
    // -----------------------------------------------------------------------

    #[test]
    fn acquire_reclaims_stale_pidfile() {
        let fix = TestCfg::new();

        // Simulate stale state: write a lockfile and pidfile but hold no lock.
        let lockfile = fix.cfg().lock_path();
        let pidfile = fix.cfg().pid_path();
        fs::create_dir_all(lockfile.parent().unwrap()).unwrap();
        fs::write(&lockfile, b"").unwrap();
        fs::write(&pidfile, b"99999\n").unwrap(); // stale PID

        // Acquire must succeed because nobody holds the flock.
        let lock = acquire_pidfile_lock(fix.cfg()).expect("stale recovery must succeed");
        assert_eq!(lock.ownership(), PidfileOwnership::WriteOwner);

        // The pidfile must now contain the current PID, not the stale one.
        let new_pid = read_pid(&pidfile).expect("pidfile must be legible");
        assert_eq!(
            new_pid,
            std::process::id(),
            "pidfile must be overwritten with current PID"
        );

        drop(lock);
    }

    // -----------------------------------------------------------------------
    // T5: pidfile_is_0644_lockfile_is_0600_dir_is_0700 (Unix only)
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn pidfile_is_0644_lockfile_is_0600_dir_is_0700() {
        use std::os::unix::fs::MetadataExt as _;

        let fix = TestCfg::new();
        let lock = acquire_pidfile_lock(fix.cfg()).expect("acquire");

        let dir = fix.cfg().lock_path();
        let dir = dir.parent().expect("lock_path has parent");

        let dir_mode = fs::metadata(dir).unwrap().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "runtime dir must be 0700");

        let lockfile_mode = fs::metadata(fix.cfg().lock_path()).unwrap().mode() & 0o777;
        assert_eq!(lockfile_mode, 0o600, "lockfile must be 0600");

        let pidfile_mode = fs::metadata(fix.cfg().pid_path()).unwrap().mode() & 0o777;
        assert_eq!(pidfile_mode, 0o644, "pidfile must be 0644");

        drop(lock);
    }

    // -----------------------------------------------------------------------
    // T6: hand_off_to_adopter_transitions_writeowner_to_handoff_skips_unlink
    // -----------------------------------------------------------------------

    #[test]
    fn hand_off_to_adopter_transitions_writeowner_to_handoff_skips_unlink() {
        let fix = TestCfg::new();
        let mut lock = acquire_pidfile_lock(fix.cfg()).expect("acquire");
        assert_eq!(lock.ownership(), PidfileOwnership::WriteOwner);

        lock.hand_off_to_adopter();
        assert_eq!(lock.ownership(), PidfileOwnership::Handoff);

        let pidfile = fix.cfg().pid_path();
        drop(lock);

        // Handoff drop must NOT unlink the pidfile.
        assert!(
            pidfile.exists(),
            "pidfile must NOT be unlinked by Handoff drop"
        );
    }

    // -----------------------------------------------------------------------
    // T7: adopt_preserves_ofd_lock_via_duped_fd_flock_contention (Unix only)
    //
    // Design doc iter-2 m7 fix: validates adoption by dup-ing the locked FD
    // and asserting that the adopted lock still holds the OFD-level lock via
    // try_lock_shared WouldBlock from a second thread.
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn adopt_preserves_ofd_lock_via_duped_fd_flock_contention() {
        use std::os::unix::io::{AsRawFd as _, RawFd};

        let tmp = TempDir::new().unwrap();
        let lockfile = tmp.path().join("test.lock");
        let pidfile = tmp.path().join("test.pid");

        // (a) Open and lock the file exclusively.
        let original = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&lockfile)
            .unwrap();
        original.lock_exclusive().expect("lock_exclusive");

        // (b) Dup the FD.
        let raw_fd: RawFd = original.as_raw_fd();
        // SAFETY: raw_fd is a valid open FD in this process.
        let duped_fd: RawFd = unsafe { libc::dup(raw_fd) };
        assert!(duped_fd >= 0, "libc::dup failed");

        // (c) Pass the duped FD to PidfileLock::adopt.
        // SAFETY: duped_fd is valid, locked, and exclusively owned for this test.
        let adopted = unsafe { PidfileLock::adopt(duped_fd, pidfile.clone(), lockfile.clone()) };
        assert_eq!(adopted.ownership(), PidfileOwnership::Adopted);

        // (d) Assert the OFD-level lock is still held: a second thread
        //     attempting try_lock_shared must get WouldBlock.
        //     Use fully-qualified fs2::FileExt::try_lock_shared to avoid
        //     Rust 1.80+ std::fs::File::try_lock_shared() ambiguity (which
        //     returns Result<(), std::fs::TryLockError>, not io::Error).
        let lockfile_clone = lockfile.clone();
        let contention_blocked = std::thread::spawn(move || {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lockfile_clone)
                .unwrap();
            // Explicitly use fs2's trait to get Result<(), io::Error>.
            let result = fs2::FileExt::try_lock_shared(&f);
            match result {
                Err(ref e) if is_would_block_err(e) => true,
                Ok(()) => {
                    // Unexpected: lock was granted — release and report failure.
                    let _ = fs2::FileExt::unlock(&f);
                    false
                }
                Err(_) => false,
            }
        })
        .join()
        .unwrap();

        assert!(
            contention_blocked,
            "try_lock_shared from a second thread must return WouldBlock while adopted lock is held"
        );

        // (e) Drop the original + adopted lock; assert release.
        drop(original);
        drop(adopted);

        // (f) After both drops, try_lock_exclusive on a fresh FD must succeed.
        let fresh = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lockfile)
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&fresh)
            .expect("try_lock_exclusive must succeed after release");
    }

    #[cfg(unix)]
    fn is_would_block_err(e: &io::Error) -> bool {
        e.kind() == io::ErrorKind::WouldBlock
            || e.raw_os_error()
                .is_some_and(|c| c == libc::EWOULDBLOCK || c == libc::EAGAIN)
    }

    // -----------------------------------------------------------------------
    // T8: drop_of_adopted_unlinks_pidfile (design M6 fix)
    //
    // Verifies that Adopted state's Drop DOES unlink the pidfile (this was
    // the M6 finding: previously adopted was specified to skip unlink, but the
    // correct ownership model requires Adopted to unlink).
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn drop_of_adopted_unlinks_pidfile() {
        use std::os::unix::io::{AsRawFd as _, RawFd};

        let tmp = TempDir::new().unwrap();
        let lockfile = tmp.path().join("test.lock");
        let pidfile = tmp.path().join("test.pid");

        // Create a lockfile and pidfile.
        let original = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&lockfile)
            .unwrap();
        original.lock_exclusive().unwrap();
        fs::write(&pidfile, b"12345\n").unwrap();

        // Dup and adopt.
        let raw_fd: RawFd = original.as_raw_fd();
        let duped_fd: RawFd = unsafe { libc::dup(raw_fd) };
        assert!(duped_fd >= 0);
        // SAFETY: duped_fd is valid and owned for this test.
        let adopted = unsafe { PidfileLock::adopt(duped_fd, pidfile.clone(), lockfile.clone()) };

        assert!(pidfile.exists(), "pidfile must exist before drop");

        drop(original); // Release the original lock handle first.
        drop(adopted); // Adopted drop must unlink the pidfile.

        assert!(
            !pidfile.exists(),
            "Adopted drop must unlink the pidfile (design M6 fix)"
        );
    }

    // -----------------------------------------------------------------------
    // T9: write_pid writes and overwrites the pidfile correctly
    // -----------------------------------------------------------------------

    #[test]
    fn write_pid_overwrites_correctly() {
        let fix = TestCfg::new();
        let lock = acquire_pidfile_lock(fix.cfg()).expect("acquire");

        // Overwrite with an explicit PID (simulates the grandchild writing its
        // own PID into the pidfile after adoption).
        lock.write_pid(42_000).expect("write_pid");
        let written = read_pid(&fix.cfg().pid_path()).expect("read_pid");
        assert_eq!(written, 42_000);

        drop(lock);
    }

    // -----------------------------------------------------------------------
    // T10: is_would_block covers all variants
    // -----------------------------------------------------------------------

    #[test]
    fn is_would_block_covers_would_block_kind() {
        let e = io::Error::new(io::ErrorKind::WouldBlock, "would block");
        assert!(is_would_block(&e));
    }

    #[test]
    fn is_would_block_returns_false_for_other_errors() {
        let e = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        assert!(!is_would_block(&e));
    }

    // -----------------------------------------------------------------------
    // T11: handoff_drop_does_not_release_adopted_ofd_lock (Unix only) — M-2 fix
    //
    // Simulates the detach path: a `WriteOwner` lock transitions to `Handoff`
    // (parent) while a duped `Adopted` lock represents the grandchild's
    // inherited FD sharing the same OFD.  Dropping the `Handoff` parent must
    // NOT release the lock (no LOCK_UN) — a third opener must still see
    // WouldBlock after the parent drops.  Only when the `Adopted` child drops
    // does the lock become acquirable.
    //
    // This test does NOT use TestCfg / ENV_LOCK because it operates on its
    // own isolated TempDir paths and never reads/writes XDG_RUNTIME_DIR —
    // avoiding spurious cross-test interference with tests that change the
    // XDG_RUNTIME_DIR env var concurrently (like the detach fast-path test).
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn handoff_drop_does_not_release_adopted_ofd_lock() {
        use std::os::unix::io::{AsRawFd as _, RawFd};

        // Use isolated temp paths — no TestCfg / ENV_LOCK needed.
        let tmp = TempDir::new().unwrap();
        let lockfile = tmp.path().join("test.lock");
        let pidfile = tmp.path().join("test.pid");

        // Open and exclusively lock the file (simulates WriteOwner's acquired lock).
        let original = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&lockfile)
            .unwrap();
        original.lock_exclusive().expect("lock_exclusive");
        fs::write(&pidfile, b"42\n").unwrap();

        // Dup the lock FD — simulates the grandchild inheriting the FD across
        // fork+exec (the inherited FD shares the same OFD as `original`).
        let raw_fd: RawFd = original.as_raw_fd();
        // SAFETY: raw_fd is valid and open for the duration of this test.
        let parent_fd: RawFd = unsafe { libc::dup(raw_fd) };
        assert!(parent_fd >= 0, "libc::dup(parent) failed");
        let child_fd: RawFd = unsafe { libc::dup(raw_fd) };
        assert!(child_fd >= 0, "libc::dup(child) failed");

        // Wrap parent_fd as Adopted and transition to WriteOwner+Handoff manually
        // by building the parent PidfileLock via adopt and calling hand_off.
        // Note: adopt() creates Adopted state; we need WriteOwner to call
        // hand_off_to_adopter.  We simulate the parent by using `original`
        // directly (keep it in scope) and adopting parent_fd separately to hold
        // the "parent simulation" handle.  The parent handle transitions to Handoff
        // using the adopt + custom flow below.
        //
        // Simpler approach: the parent just needs to hold a PidfileLock with
        // WriteOwner state.  We build this from parent_fd via adopt(), then use
        // an unsafe transmute of the ownership field.  To avoid unsafe hacks,
        // we instead construct the test around the known behaviour:
        //
        // - Keep `original` as the "parent" File that holds the lock.
        // - Build the child_lock (Adopted) from child_fd.
        // - Simulate Handoff drop by dropping `original` (which closes the parent
        //   FD WITHOUT calling LOCK_UN — exactly what our Handoff drop does).
        // - Assert the lock is still held by child_lock after original drops.
        //
        // Then drop child_lock (Adopted) and assert lock is released.

        // Build the Adopted lock (grandchild simulation) from child_fd.
        // SAFETY: child_fd is valid, locked via the original OFD, and exclusively
        // owned by this test for the duration of the `Adopted` lock.
        let child_lock = unsafe { PidfileLock::adopt(child_fd, pidfile.clone(), lockfile.clone()) };
        assert_eq!(child_lock.ownership(), PidfileOwnership::Adopted);

        // Close parent_fd (simulates Handoff drop: close FD without LOCK_UN).
        // SAFETY: parent_fd is a valid open FD owned by this test.
        unsafe { libc::close(parent_fd) };
        // Also drop `original` — this closes the original FD.  The OFD still has
        // one reference: child_fd (inside child_lock).
        drop(original);

        // After closing all "parent" FDs, the child_lock must still hold the
        // OFD-level flock — a third opener must see WouldBlock.
        let lockfile_clone = lockfile.clone();
        let still_locked = std::thread::spawn(move || {
            let f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lockfile_clone)
                .unwrap();
            let result = fs2::FileExt::try_lock_exclusive(&f);
            match result {
                Err(ref e) if is_would_block_err(e) => true, // correct: still locked
                Ok(()) => {
                    // Wrong: lock was released when parent FDs closed (LOCK_UN bug).
                    let _ = fs2::FileExt::unlock(&f);
                    false
                }
                Err(_) => false,
            }
        })
        .join()
        .unwrap();

        assert!(
            still_locked,
            "Handoff drop (close FD without LOCK_UN) must NOT release the OFD-level lock \
             (M-2 fix): the adopted grandchild lock must still hold after all parent FDs close"
        );

        // Drop the child lock (Adopted); this calls unlock() + remove_file.
        drop(child_lock);

        // After Adopted drop, the lock must be released.
        let fresh = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lockfile)
            .unwrap();
        fs2::FileExt::try_lock_exclusive(&fresh)
            .expect("try_lock_exclusive must succeed after Adopted child drop");
    }
}
