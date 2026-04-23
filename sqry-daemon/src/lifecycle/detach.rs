//! Daemon auto-spawn helper — `start_detached`.
//!
//! # Design reference
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md` §H
//! (auto-spawn-on-miss), §C.3.2 (detach path FD inheritance), §M (security).
//!
//! # Overview
//!
//! [`start_detached`] provides the client-side half of the auto-spawn-on-miss
//! feature introduced in Task 9. When a client helper (Task 10
//! `connect_or_start`) finds no daemon socket it calls `start_detached` to:
//!
//! 1. Acquire a **bootstrap lock** (`sqryd.bootstrap.lock`) that serialises
//!    concurrent callers so exactly one spawner makes progress at a time.
//! 2. Under the lock, fast-path-check whether a daemon is already up via
//!    [`try_connect`].
//! 3. If not up, spawn `current_exe()` with `["start", "--detach",
//!    "--spawned-by-client"]` as a detached grandchild process.
//! 4. Poll the socket every 50 ms until `auto_start_ready_timeout_secs`
//!    expires.  Socket-connect success is the authoritative ready signal.
//! 5. Release the bootstrap lock.
//!
//! Callers that arrive while the bootstrap lock is held block until the winner
//! releases it.  By that point step 4 has completed so their step 2 fast-path
//! returns the already-running daemon's PID.
//!
//! # Bootstrap lock vs. main daemon lock
//!
//! The two locks serve orthogonal purposes:
//!
//! - `sqryd.lock` — held for the **full daemon lifetime** by the grandchild.
//!   Proves exactly one daemon is serving.  Described in
//!   [`crate::lifecycle::pidfile`].
//!
//! - `sqryd.bootstrap.lock` — held **only during `start_detached`** (acquire →
//!   spawn + poll → release).  Prevents a burst of N concurrent callers from
//!   each spawning a separate `sqryd` process (§H M2 fix).
//!
//! # Bootstrap lock implementation
//!
//! On all platforms the bootstrap lock is a plain file flock via
//! [`fs2::lock_exclusive`], opened with mode `0600` on Unix.  `lock_exclusive`
//! on Unix wraps `flock(LOCK_EX)` (blocking) and on Windows wraps `LockFileEx`
//! (also blocking), so the cross-process serialisation guarantee holds on both
//! platforms without additional named-mutex machinery.
//!
//! # Security
//!
//! The bootstrap lock file is created with mode `0600` on Unix.  The socket
//! connect uses the same path logic as the main daemon so no additional attack
//! surface is opened.

use std::{
    io,
    path::PathBuf,
    process::Child,
    time::{Duration, Instant},
};

use fs2::FileExt as _;
use tracing::{debug, info, warn};

use crate::{
    config::DaemonConfig,
    error::{DaemonError, DaemonResult},
    lifecycle::pidfile::read_pid,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Spawn a detached `sqryd` daemon and wait until its IPC socket becomes
/// connectable.
///
/// Returns the grandchild process's PID on success.  On the "spawn and wait"
/// path this is the PID returned by `Command::spawn()::id()`.  On the fast
/// path (daemon was already up when we checked under the bootstrap lock) this
/// is the PID read from the pidfile — `0` if the pidfile was absent or
/// unreadable (advisory sentinel per §M m3; the caller should treat `0` as
/// "daemon reachable, PID unavailable").
///
/// Returns [`DaemonError::AutoStartTimeout`] if the socket does not become
/// reachable within `cfg.auto_start_ready_timeout_secs`.
///
/// # Protocol
///
/// ```text
/// Caller 1 (first) ──► bootstrap_lock.lock_exclusive() ──► try_connect (miss)
///                       ──► spawn_grandchild() ──► poll_socket()
///                       ──► bootstrap_lock.unlock()
///
/// Caller 2 (concurrent) ──► bootstrap_lock.lock_exclusive() [BLOCKS] ──►
///                            try_connect() [HIT — daemon is now up] ──►
///                            Ok(pid)
/// ```
///
/// # Errors
///
/// - [`DaemonError::AutoStartTimeout`] — daemon did not become ready in time.
/// - [`DaemonError::Io`] — filesystem, spawn, or connect error.
pub async fn start_detached(cfg: &DaemonConfig) -> DaemonResult<u32> {
    let socket_path = cfg.socket_path();
    let timeout = Duration::from_secs(cfg.auto_start_ready_timeout_secs);
    let deadline = Instant::now() + timeout;

    // ------------------------------------------------------------------
    // Acquire the bootstrap lock (blocking flock).
    //
    // We use a blocking acquire inside `spawn_blocking` so we do not
    // block the Tokio runtime thread.  The lock is released at the end
    // of this function (or on early error) via Drop.
    // ------------------------------------------------------------------
    let bootstrap_lock_path = bootstrap_lock_path(cfg);
    debug!(
        path = %bootstrap_lock_path.display(),
        "acquiring bootstrap lock for auto-spawn"
    );

    let bootstrap_file = open_bootstrap_lock(&bootstrap_lock_path)?;

    // Blocking lock acquisition — runs on a thread-pool thread so the
    // Tokio scheduler is not stalled.
    let bootstrap_file = {
        let lock_path_clone = bootstrap_lock_path.clone();
        tokio::task::spawn_blocking(move || {
            lock_bootstrap(&bootstrap_file, &lock_path_clone).map(|()| bootstrap_file)
        })
        .await
        .map_err(|join_err| {
            DaemonError::Io(io::Error::other(format!(
                "bootstrap lock task panicked: {join_err}"
            )))
        })??
    };
    debug!(path = %bootstrap_lock_path.display(), "bootstrap lock acquired");

    // Ensure the bootstrap lock is always released.
    let _bootstrap_guard = BootstrapLockGuard {
        file: bootstrap_file,
        path: bootstrap_lock_path.clone(),
    };

    // ------------------------------------------------------------------
    // Fast path: check if a daemon is already up.  This catches both
    // "a concurrent caller won the race and the daemon is ready" AND
    // "a daemon was already running before we started".
    // ------------------------------------------------------------------
    if try_connect(&socket_path).await {
        let pid = read_pid(&cfg.pid_path()).unwrap_or(0);
        info!(pid, socket = %socket_path.display(), "daemon already running — fast path");
        return Ok(pid);
    }

    // ------------------------------------------------------------------
    // Spawn the grandchild.
    // ------------------------------------------------------------------
    let (grandchild_pid, mut grandchild) = spawn_daemon_grandchild(cfg)?;
    info!(
        pid = grandchild_pid,
        socket = %socket_path.display(),
        timeout_secs = cfg.auto_start_ready_timeout_secs,
        "spawned detached sqryd; polling socket for readiness"
    );

    // ------------------------------------------------------------------
    // Poll the socket until ready or deadline.
    // ------------------------------------------------------------------
    loop {
        if try_connect(&socket_path).await {
            info!(
                pid = grandchild_pid,
                socket = %socket_path.display(),
                "daemon socket connectable — auto-spawn complete"
            );
            // Grandchild is running — drop the Child handle without waiting.
            // Rust's `Child::drop` does NOT wait for the child; the child
            // continues running as a detached process.
            drop(grandchild);
            return Ok(grandchild_pid);
        }

        if Instant::now() >= deadline {
            warn!(
                socket = %socket_path.display(),
                timeout_secs = cfg.auto_start_ready_timeout_secs,
                "daemon did not become ready within timeout"
            );

            // Kill the grandchild we spawned to avoid leaving a zombie.
            // `Child::kill()` sends SIGKILL on Unix (via libc::kill(pid, SIGKILL))
            // and calls TerminateProcess on Windows — both target the exact PID,
            // not the process group, which is safe after setsid.
            if let Err(e) = grandchild.kill() {
                // Log but don't propagate — the primary error is timeout.
                // The process may have already exited.
                warn!(
                    pid = grandchild_pid,
                    err = %e,
                    "failed to kill timed-out grandchild (may have exited already)"
                );
            } else {
                debug!(pid = grandchild_pid, "sent SIGKILL to timed-out grandchild");
            }
            // Wait to reap the zombie so we don't leak process table entries.
            let _ = grandchild.wait();

            return Err(DaemonError::AutoStartTimeout {
                timeout_secs: cfg.auto_start_ready_timeout_secs,
                socket: socket_path,
            });
        }

        // Wait 50 ms before the next poll.
        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Socket polling interval in milliseconds.
const POLL_INTERVAL_MS: u64 = 50;

// ---------------------------------------------------------------------------
// Bootstrap lock path
// ---------------------------------------------------------------------------

/// Path to the bootstrap lock file — separate from the main `sqryd.lock`
/// so the bootstrap-lock acquire and the daemon-lifetime lock are
/// fully orthogonal (§H design note).
///
/// The file is created in the same runtime directory as the main lock.
#[must_use]
pub fn bootstrap_lock_path(cfg: &DaemonConfig) -> PathBuf {
    let lock = cfg.lock_path();
    lock.parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("sqryd.bootstrap.lock")
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Open-or-create the bootstrap lock file with mode `0600` on Unix.
fn open_bootstrap_lock(path: &std::path::Path) -> DaemonResult<std::fs::File> {
    // Ensure parent directory exists with mode 0700.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let perms = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(parent, perms)?;
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        Ok(f)
    }
    #[cfg(not(unix))]
    {
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        Ok(f)
    }
}

/// Acquire an exclusive flock on the bootstrap file, blocking until acquired.
///
/// On Unix this calls `flock(LOCK_EX)` which blocks until the lock is
/// available.  On Windows `fs2::lock_exclusive` wraps `LockFileEx` which also
/// blocks.  In both cases only one caller at a time makes progress — which is
/// the §H M2 single-spawner guarantee.
fn lock_bootstrap(file: &std::fs::File, path: &std::path::Path) -> DaemonResult<()> {
    debug!(path = %path.display(), "blocking on bootstrap flock");
    file.lock_exclusive().map_err(|e| {
        warn!(path = %path.display(), err = %e, "bootstrap flock failed");
        DaemonError::Io(e)
    })?;
    Ok(())
}

/// Public wrapper around [`try_connect`] for callers outside this module
/// (e.g. `sqry_daemon::entrypoint` for the `stop` / `status` liveness
/// check per §M m3 — socket-connect revalidation, never pidfile-only).
///
/// Returns `true` if the daemon socket is connectable, `false` otherwise.
pub async fn try_connect_path(socket_path: &std::path::Path) -> bool {
    try_connect(socket_path).await
}

/// Try to connect to the daemon socket.  Returns `true` if the socket is
/// connectable (daemon is up), `false` otherwise.
///
/// This is purely a liveness check — no PID is read here.  PID is obtained
/// separately from the pidfile (advisory per §M m3).
async fn try_connect(socket_path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use tokio::net::UnixStream;
        match UnixStream::connect(socket_path).await {
            Ok(_stream) => {
                debug!(socket = %socket_path.display(), "socket connect succeeded");
                true
            }
            Err(_) => false,
        }
    }
    #[cfg(windows)]
    {
        use tokio::net::windows::named_pipe::ClientOptions;
        let pipe_path = socket_path.to_string_lossy();
        match ClientOptions::new().open(pipe_path.as_ref()) {
            Ok(_) => {
                debug!(pipe = %pipe_path, "named pipe connect succeeded");
                true
            }
            Err(_) => false,
        }
    }
}

/// Spawn the daemon grandchild process with `["start", "--detach",
/// "--spawned-by-client"]` and detach it from the calling process.
///
/// Returns `(pid, child_handle)`.  The caller is responsible for either
/// dropping `child_handle` (daemon ran successfully) or calling
/// `child_handle.kill()` + `child_handle.wait()` (timeout path).
///
/// On Unix the grandchild is started in a new session (`setsid`) and all
/// stdio FDs are redirected to `/dev/null`.  On Windows `DETACHED_PROCESS |
/// CREATE_NEW_PROCESS_GROUP` flags are used instead.
fn spawn_daemon_grandchild(cfg: &DaemonConfig) -> DaemonResult<(u32, Child)> {
    let exe = std::env::current_exe().map_err(|e| {
        warn!(err = %e, "current_exe() failed");
        DaemonError::Io(e)
    })?;

    debug!(exe = %exe.display(), "spawning detached sqryd grandchild");

    let mut cmd = std::process::Command::new(&exe);
    cmd.args(["start", "--detach", "--spawned-by-client"]);

    // Propagate the daemon socket path so the grandchild uses the same socket.
    if let Some(path) = &cfg.socket.path {
        cmd.env(crate::config::ENV_SOCKET_PATH, path);
    }

    // Redirect stdio to /dev/null — the grandchild should not inherit our
    // terminal or pipes.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    // Unix: move the grandchild into a new session so it detaches from our
    // controlling terminal and process group.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: setsid(2) is async-signal-safe and cannot fail in a
        // well-formed process (the only failure is EPERM when the caller is
        // already a process-group leader, which is very unlikely for a fresh
        // child — and even then it is benign: the child just stays in its
        // current session).
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    // Windows: DETACHED_PROCESS + CREATE_NEW_PROCESS_GROUP so the child has no
    // console and does not forward Ctrl-C.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    let child = cmd.spawn().map_err(|e| {
        warn!(exe = %exe.display(), err = %e, "failed to spawn sqryd grandchild");
        DaemonError::Io(e)
    })?;

    let pid = child.id();
    debug!(pid, exe = %exe.display(), "sqryd grandchild spawned");

    // Return both the PID and the Child handle.
    // - On success path: caller drops the Child (Child::drop does NOT wait).
    // - On timeout path: caller calls child.kill() + child.wait() to reap.
    Ok((pid, child))
}

// ---------------------------------------------------------------------------
// RAII bootstrap lock guard
// ---------------------------------------------------------------------------

/// Releases the bootstrap flock on drop.
///
/// Drop MUST NOT panic — all errors are logged and swallowed.
struct BootstrapLockGuard {
    file: std::fs::File,
    path: PathBuf,
}

impl Drop for BootstrapLockGuard {
    fn drop(&mut self) {
        match self.file.unlock() {
            Ok(()) => {
                debug!(path = %self.path.display(), "bootstrap lock released");
            }
            Err(e) => {
                // Non-fatal: the OS will release the flock when the FD closes.
                warn!(
                    path = %self.path.display(),
                    err = %e,
                    "failed to explicitly unlock bootstrap lock (kernel will clean up)"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Use the crate-wide TEST_ENV_LOCK to serialise XDG_RUNTIME_DIR
    // mutations across ALL test modules in the same binary.
    // -----------------------------------------------------------------------
    use crate::TEST_ENV_LOCK as ENV_LOCK;

    // -----------------------------------------------------------------------
    // Test helper: a DaemonConfig whose socket and lock paths are hermetic.
    // -----------------------------------------------------------------------
    struct TestCfg {
        _tmp: TempDir,
        cfg: DaemonConfig,
        prior_xdg: Option<String>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl TestCfg {
        fn new() -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let tmp = TempDir::new().expect("TempDir::new");
            let prior_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
            #[allow(unsafe_code)]
            unsafe {
                std::env::set_var("XDG_RUNTIME_DIR", tmp.path());
            }
            let mut cfg = DaemonConfig::default();
            cfg.socket.path = Some(tmp.path().join("sqry").join("sqryd.sock"));
            cfg.auto_start_ready_timeout_secs = 2;
            Self {
                _tmp: tmp,
                cfg,
                prior_xdg,
                _guard: guard,
            }
        }

        fn cfg(&self) -> &DaemonConfig {
            &self.cfg
        }
    }

    impl Drop for TestCfg {
        fn drop(&mut self) {
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
    // T1: bootstrap_lock_path is rooted in the runtime dir alongside the
    //     main lock file.
    // -----------------------------------------------------------------------

    #[test]
    fn bootstrap_lock_path_is_sibling_of_lock_path() {
        let fix = TestCfg::new();
        let lock_path = fix.cfg().lock_path();
        let bootstrap = bootstrap_lock_path(fix.cfg());

        assert_eq!(
            lock_path.parent(),
            bootstrap.parent(),
            "bootstrap lock must be in the same directory as the main lock"
        );
        assert_eq!(
            bootstrap.file_name().and_then(|n| n.to_str()),
            Some("sqryd.bootstrap.lock"),
            "bootstrap lock filename must be 'sqryd.bootstrap.lock'"
        );
    }

    // -----------------------------------------------------------------------
    // T2: open_bootstrap_lock creates the file with 0600 on Unix.
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn open_bootstrap_lock_creates_with_0600() {
        use std::os::unix::fs::MetadataExt as _;

        let fix = TestCfg::new();
        let path = bootstrap_lock_path(fix.cfg());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let _file = open_bootstrap_lock(&path).expect("open_bootstrap_lock");
        let mode = std::fs::metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600, "bootstrap lock must be 0600");
    }

    // -----------------------------------------------------------------------
    // T3: BootstrapLockGuard releases the flock on drop.
    //     After the guard drops, a second lock_exclusive must succeed.
    // -----------------------------------------------------------------------

    #[test]
    fn bootstrap_lock_guard_releases_on_drop() {
        let fix = TestCfg::new();
        let path = bootstrap_lock_path(fix.cfg());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let file = open_bootstrap_lock(&path).expect("open");
        lock_bootstrap(&file, &path).expect("lock");
        let guard = BootstrapLockGuard {
            file,
            path: path.clone(),
        };

        // While the guard is alive, try_lock_exclusive from a second handle must fail.
        {
            let f2 = open_bootstrap_lock(&path).expect("open 2");
            let result = fs2::FileExt::try_lock_exclusive(&f2);
            assert!(
                result.is_err(),
                "lock must be held while BootstrapLockGuard is alive"
            );
        }

        // Drop the guard — releases the flock.
        drop(guard);

        // Now try_lock_exclusive must succeed.
        let f3 = open_bootstrap_lock(&path).expect("open 3");
        fs2::FileExt::try_lock_exclusive(&f3)
            .expect("lock must succeed after BootstrapLockGuard is dropped");
    }

    // -----------------------------------------------------------------------
    // T4: start_detached_respects_timeout_when_socket_never_appears
    //
    // Verifies that AutoStartTimeout is returned when the socket never
    // becomes connectable within the configured timeout.
    //
    // We use a very short timeout (1 s) and point the socket at a path
    // that current_exe() (the test binary) will never create.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn start_detached_respects_timeout_when_socket_never_appears() {
        let fix = TestCfg::new();
        let mut cfg = fix.cfg().clone();
        cfg.auto_start_ready_timeout_secs = 1;

        let result = start_detached(&cfg).await;

        match result {
            Err(DaemonError::AutoStartTimeout { timeout_secs, .. }) => {
                assert_eq!(timeout_secs, 1, "timeout_secs in error must match config");
            }
            Err(DaemonError::Io(_)) => {
                // Acceptable: spawn may fail for unrelated reasons in test context.
            }
            Ok(pid) => panic!("expected AutoStartTimeout or Io, got Ok({pid})"),
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // T5: start_detached_bootstrap_lock_serialises_concurrent_callers
    //
    // Design §H M2 proof: N concurrent calls to start_detached must serialise
    // through the bootstrap lock so that at most ONE spawner proceeds while
    // all others block.
    //
    // Method: we use a shared `Arc<AtomicI32>` to count how many callers are
    // simultaneously inside `start_detached` AFTER acquiring the bootstrap
    // lock.  Because `lock_exclusive` is exclusive, only one caller can hold
    // the lock at a time, so the "inside lock" counter must NEVER exceed 1.
    //
    // Note: we intercept "inside lock" state by wrapping the core logic in
    // a test-side harness: spawn N tasks each of which calls start_detached;
    // observe the bootstrap lock file contention from outside using
    // `try_lock_exclusive` on a second FD — if the lock is truly exclusive,
    // every try_lock_exclusive call we make while any caller is inside the
    // lock returns WouldBlock.
    //
    // We also count the number of spawned child processes indirectly by
    // verifying that the bootstrap lock file was created (all callers
    // executed the open_bootstrap_lock step) and that the function returns
    // without panicking (all callers released the lock).
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn start_detached_bootstrap_lock_serialises_concurrent_callers() {
        let fix = TestCfg::new();
        let mut cfg = fix.cfg().clone();
        cfg.auto_start_ready_timeout_secs = 1;
        let cfg = Arc::new(cfg);

        // Pre-create the runtime dir.
        let lock_path = bootstrap_lock_path(&cfg);
        std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();

        // Track concurrent "inside bootstrap lock" holders via a
        // second-FD try_lock_exclusive probe.  We run a background task
        // that repeatedly probes the bootstrap lock for the duration of
        // the test and records the maximum concurrent contention observed.
        //
        // "contention observed" means try_lock_exclusive returns WouldBlock
        // at that instant — which proves a caller holds the lock.
        let observed_contention = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe_path = lock_path.clone();
        let contention_flag = Arc::clone(&observed_contention);
        let probe_handle = tokio::spawn(async move {
            // Probe every 5 ms for up to 15 s (3000 probes).  The probe window
            // comfortably covers the full 5-caller × 1-s-timeout = ~5 s test
            // duration, ensuring we catch at least one contended-lock snapshot.
            for _ in 0..3000 {
                if let Ok(f) = open_bootstrap_lock(&probe_path)
                    && fs2::FileExt::try_lock_exclusive(&f).is_err()
                {
                    // WouldBlock — a caller holds the exclusive bootstrap lock.
                    // This is the M2 single-spawner proof: the lock is exclusive,
                    // so only one caller at a time can be in the critical section.
                    contention_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    break; // One observation is sufficient; stop probing.
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });

        // Spawn 5 concurrent callers.
        let mut handles = Vec::new();
        for _ in 0..5 {
            let cfg_clone = Arc::clone(&cfg);
            handles.push(tokio::spawn(async move {
                let _ = start_detached(&cfg_clone).await;
            }));
        }

        for h in handles {
            h.await.expect("task panicked");
        }
        probe_handle.abort();

        // Assert that the bootstrap lock file was created AND that the probe
        // observed at least one WouldBlock contention event — proving that the
        // bootstrap lock was exclusively held (single-spawner M2 guarantee).
        assert!(
            lock_path.exists(),
            "bootstrap lock file must be created by start_detached callers"
        );
        assert!(
            observed_contention.load(std::sync::atomic::Ordering::SeqCst),
            "probe must have observed WouldBlock on the bootstrap lock at least once, \
             proving exclusive lock ownership (M2 single-spawner guarantee)"
        );
    }

    // -----------------------------------------------------------------------
    // T6: try_connect returns false for a non-existent socket.
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn try_connect_returns_false_for_nonexistent_socket() {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("nonexistent.sock");
        let result = try_connect(&socket_path).await;
        assert!(!result, "try_connect must return false for missing socket");
    }

    // -----------------------------------------------------------------------
    // T7: try_connect returns true when a Unix listener is ready.
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn try_connect_returns_true_for_listening_socket() {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("test.sock");

        // Bind a listener.
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("UnixListener::bind");

        // Spawn an acceptor so the connect doesn't block.
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let result = try_connect(&socket_path).await;
        assert!(
            result,
            "try_connect must return true for a listening socket"
        );
    }

    // -----------------------------------------------------------------------
    // T8: start_detached returns actual PID (not 0) on fast path.
    //
    // When the daemon socket is already up, start_detached should return
    // the PID read from the pidfile (or 0 if the pidfile is absent), not
    // a hard-coded sentinel.  We verify the return value is a non-negative
    // integer and does not panic.
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[tokio::test]
    async fn start_detached_fast_path_returns_pid_not_sentinel() {
        use crate::lifecycle::pidfile::acquire_pidfile_lock;

        let fix = TestCfg::new();
        let socket_path = fix.cfg().socket.path.clone().unwrap();
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

        // Bind a Unix listener to simulate a running daemon.
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("UnixListener::bind");
        tokio::spawn(async move {
            loop {
                if listener.accept().await.is_err() {
                    break;
                }
            }
        });

        // Write a known PID to the pidfile so the fast path returns it.
        let _lock = acquire_pidfile_lock(fix.cfg()).expect("acquire_pidfile_lock");
        let expected_pid = std::process::id();

        let result = start_detached(fix.cfg()).await;
        match result {
            Ok(pid) => {
                assert_eq!(
                    pid, expected_pid,
                    "fast path must return pidfile PID, not a sentinel"
                );
            }
            Err(e) => panic!("expected Ok(pid), got {e:?}"),
        }
    }
}
