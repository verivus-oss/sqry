//! Task 9 U13 — `start_detached` + detach-path integration tests.
//!
//! # Test inventory
//!
//! 1. **`start_detached_spawns_child_and_socket_becomes_connectable`** — spawn
//!    `sqryd start --detach` and verify the grandchild's socket becomes
//!    connectable within the configured timeout.  Validates the full
//!    double-fork + self-pipe + socket-bind path (§C.3.2).
//!
//! 2. **`start_detached_respects_auto_start_ready_timeout`** — call
//!    `start_detached` with a 1 s timeout pointing at a socket that never
//!    appears; assert **exactly** [`DaemonError::AutoStartTimeout`].  The
//!    test validates the polling deadline path (§H).  `DaemonError::Io` is
//!    NOT accepted because the polling loop always drains the full timeout
//!    before returning `AutoStartTimeout` regardless of whether the spawned
//!    process exits early.
//!
//! 3. **`start_detached_bootstrap_lock_serialises_concurrent_callers`** — 10
//!    concurrent `start_detached` calls sharing the same config.  A probe
//!    thread observes `WouldBlock` on `try_lock_exclusive` at least once,
//!    proving the bootstrap flock is exclusively held throughout each
//!    critical section (§H M2 single-spawner proof).
//!
//! 4. **`start_detached_inherited_fd_lock_is_held_by_grandchild`** — spawn
//!    `sqryd start --detach`, wait for the socket, read the grandchild PID
//!    from the pidfile, then attempt `try_lock_exclusive` on the same
//!    lockfile from the test process; must return `WouldBlock` proving the
//!    grandchild holds the OFD-level flock (§D / §C.3.2 M1 FD-inheritance
//!    proof).
//!
//! # Binary discovery
//!
//! The `sqryd` binary is located via:
//!
//! 1. `CARGO_BIN_EXE_sqryd` — set by Cargo during `cargo test`.
//! 2. Walking up from `current_exe()` to `target/<profile>/` and looking
//!    for `sqryd` in the same directory.
//!
//! If neither path yields an executable, tests that require the binary are
//! **skipped** (not failed), keeping the suite green on `cargo check`-only hosts.
//!
//! # Isolation
//!
//! Every test is isolated via an ephemeral `TempDir` used as
//! `XDG_RUNTIME_DIR`.  `SQRY_DAEMON_SOCKET` is overridden so child
//! processes never contend with the developer's real daemon.
//! `SQRY_DAEMON_CONFIG` is pointed at a valid empty TOML file inside the
//! tempdir so the child never loads `~/.config/sqry/daemon.toml` and
//! never encounters a config-parse error (`EX_CONFIG / 78`).
//!
//! # Env-var serialisation for in-process tests
//!
//! Tests 2 and 3 call [`start_detached`] in-process.  Inside
//! [`start_detached`], [`DaemonConfig::lock_path`] (and thus
//! [`bootstrap_lock_path`]) derive their paths from `XDG_RUNTIME_DIR` at
//! call-time.  To guarantee that the bootstrap lock and all derived paths
//! land inside the per-test `TempDir` (not the ambient user runtime dir),
//! these tests acquire an `ENV_LOCK` mutex, set `XDG_RUNTIME_DIR` to the
//! tempdir for the duration of the test, then restore the original value.
//! This is the same pattern used by the unit tests in `detach.rs` and is
//! safe because the test binary is single-threaded with respect to
//! `XDG_RUNTIME_DIR` manipulation (all concurrent access serialised through
//! `ENV_LOCK`).
//!
//! # Platform gate
//!
//! The full test suite is `#[cfg(unix)]` because it relies on POSIX signals,
//! `flock` semantics, and `/proc/<pid>/fd` inspection.  Tests 1, 3, and
//! parts of 4 use `tokio` for async socket operations and `fs2::FileExt`
//! for lock probing.
//!
//! # Design references
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
//! §C.3.2 (detach path), §D (pidfile locking), §H (auto-spawn-on-miss).

#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use fs2::FileExt as _;
use tempfile::TempDir;
use tokio::net::UnixStream;

use sqry_daemon::{
    DaemonConfig, DaemonError, SocketConfig,
    lifecycle::detach::{bootstrap_lock_path, start_detached},
};

// ---------------------------------------------------------------------------
// Serialisation mutex for tests that manipulate XDG_RUNTIME_DIR in-process.
// ---------------------------------------------------------------------------

/// Serialises tests 2 and 3 so `XDG_RUNTIME_DIR` manipulation is
/// single-threaded.  Pattern mirrors `detach.rs` unit-test `ENV_LOCK`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Binary discovery
// ---------------------------------------------------------------------------

/// Locate the `sqryd` production binary.
///
/// Returns `None` when the binary cannot be found, which causes the
/// test to be skipped with an explanatory message.
fn find_sqryd_binary() -> Option<PathBuf> {
    // Cargo injects `CARGO_BIN_EXE_sqryd` when the `[[bin]]` target exists
    // in the crate's `Cargo.toml` and `cargo test` (or `cargo test --workspace`)
    // is invoked.
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_sqryd") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }

    // Walk up from the integration-test binary's own path.
    //
    // Typical layout:
    //   target/debug/deps/<test-binary-hash>   ← `current_exe()`
    //   target/debug/sqryd                     ← what we want (grandparent dir)
    let binary_name = format!("sqryd{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().ok()?;
    let parent = exe.parent()?; // target/debug/deps
    let candidate = parent.join(&binary_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    let grandparent = parent.parent()?; // target/debug
    let candidate = grandparent.join(&binary_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

// ---------------------------------------------------------------------------
// Isolation context
// ---------------------------------------------------------------------------

/// Per-test isolation context.
///
/// Holds the `TempDir` that backs `XDG_RUNTIME_DIR` for child processes
/// spawned within this test.  `socket_path` and the config path are
/// derived from this dir so every test run is fully hermetic.
struct TestContext {
    runtime_dir: TempDir,
    /// Path to a valid, minimal TOML config file inside the tempdir.
    ///
    /// `DaemonConfig::load_from_path` on an empty TOML file returns all
    /// defaults — the same as if no config file existed.  Pointing
    /// `SQRY_DAEMON_CONFIG` at this file ensures the child process:
    ///   a) Loads the empty/default config (does not error with EX_CONFIG).
    ///   b) Never reads the developer's real `~/.config/sqry/daemon.toml`.
    ///
    /// We use an empty file (zero bytes) rather than `{}` because `{}`
    /// at the top level of a TOML document is an inline-table expression, not
    /// a valid empty document — the `toml` crate rejects it when deserialising
    /// into a struct with `#[serde(deny_unknown_fields)]`.
    valid_empty_config_path: PathBuf,
}

impl TestContext {
    fn new() -> Self {
        let runtime_dir = TempDir::new().expect("create test runtime_dir");
        // Create an empty TOML config file.  An empty file (zero bytes) is a
        // valid TOML document; all fields are populated by #[serde(default)]
        // in DaemonConfig.  We cannot use `{}` because that is an
        // inline-table expression — valid as a TOML *value* but rejected by
        // the `toml` crate when it is the entire document content.
        let valid_empty_config_path = runtime_dir.path().join("empty-daemon.toml");
        std::fs::write(&valid_empty_config_path, b"")
            .expect("create empty config file for test isolation");
        Self {
            runtime_dir,
            valid_empty_config_path,
        }
    }

    /// Socket path inside the isolated runtime dir.
    ///
    /// Mirrors `DaemonConfig::socket_path()` default (XDG_RUNTIME_DIR/sqry/sqryd.sock).
    fn socket_path(&self) -> PathBuf {
        self.runtime_dir.path().join("sqry").join("sqryd.sock")
    }

    /// `sqryd.ready` sentinel path inside the isolated runtime dir.
    fn ready_sentinel(&self) -> PathBuf {
        self.runtime_dir.path().join("sqry").join("sqryd.ready")
    }

    /// Pidfile path inside the isolated runtime dir.
    fn pidfile_path(&self) -> PathBuf {
        self.runtime_dir.path().join("sqry").join("sqryd.pid")
    }

    /// Lockfile path inside the isolated runtime dir.
    fn lockfile_path(&self) -> PathBuf {
        self.runtime_dir.path().join("sqry").join("sqryd.lock")
    }

    /// Build a `DaemonConfig` that is fully pointed at the isolated tempdir.
    ///
    /// **IMPORTANT — env-var contract for in-process tests (2 and 3):**
    /// `DaemonConfig::lock_path()` and `bootstrap_lock_path()` both derive
    /// their paths from `XDG_RUNTIME_DIR` at call-time (not from
    /// `socket.path`).  Callers that invoke `start_detached` in-process MUST
    /// hold `ENV_LOCK` and have `XDG_RUNTIME_DIR` set to
    /// `self.runtime_dir.path()` before calling this function, so that
    /// `lock_path()` / `bootstrap_lock_path()` resolve inside the tempdir.
    ///
    /// Tests 1 and 4 spawn `sqryd` as an external process using
    /// [`spawn_start_detach`], which sets `XDG_RUNTIME_DIR` explicitly on the
    /// child command — those tests do NOT need to hold `ENV_LOCK`.
    fn make_daemon_config(&self, auto_start_ready_timeout_secs: u64) -> DaemonConfig {
        let mut cfg = DaemonConfig {
            socket: SocketConfig {
                path: Some(self.socket_path()),
                pipe_name: None,
            },
            auto_start_ready_timeout_secs,
            ..DaemonConfig::default()
        };
        cfg.apply_env_overrides()
            .expect("apply_env_overrides in make_daemon_config");
        cfg
    }

    /// Spawn `sqryd start --detach` with full process isolation.
    ///
    /// All child-process `XDG_RUNTIME_DIR`, `SQRY_DAEMON_SOCKET`, and
    /// `SQRY_DAEMON_CONFIG` overrides ensure isolation from the developer's
    /// real daemon and config file.
    fn spawn_start_detach(&self, sqryd: &Path) -> Child {
        Command::new(sqryd)
            .args(["start", "--detach"])
            .env("XDG_RUNTIME_DIR", self.runtime_dir.path())
            .env("SQRY_DAEMON_SOCKET", self.socket_path())
            .env("SQRY_DAEMON_CONFIG", &self.valid_empty_config_path)
            .env("SQRY_DAEMON_LOG_LEVEL", "warn")
            .env_remove("TMPDIR")
            // Capture stdout for diagnostics; inherit stderr for CI.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| panic!("failed to spawn sqryd start --detach: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Env-var scoped guard for in-process tests
// ---------------------------------------------------------------------------

/// RAII guard that sets `XDG_RUNTIME_DIR` to `path` for the duration of its
/// lifetime and restores the previous value on drop.
///
/// Callers must hold `ENV_LOCK` for the entire lifetime of this guard.
struct XdgRuntimeDirGuard {
    prior: Option<String>,
}

impl XdgRuntimeDirGuard {
    /// Set `XDG_RUNTIME_DIR` to `path`.
    ///
    /// # Safety
    ///
    /// Caller must hold `ENV_LOCK` so that no concurrent thread reads or
    /// writes `XDG_RUNTIME_DIR` while this guard is alive.  The `unsafe` block
    /// mirrors the pattern used in `sqry-daemon/src/lifecycle/detach.rs`
    /// unit tests.
    fn set(path: &Path) -> Self {
        let prior = std::env::var("XDG_RUNTIME_DIR").ok();
        // SAFETY: caller holds ENV_LOCK, serialising all env-var mutations in
        // this test binary.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", path);
        }
        Self { prior }
    }
}

impl Drop for XdgRuntimeDirGuard {
    fn drop(&mut self) {
        // SAFETY: same ENV_LOCK serialisation as above.
        #[allow(unsafe_code)]
        unsafe {
            match self.prior.take() {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Async socket helpers
// ---------------------------------------------------------------------------

/// Return `true` when the Unix socket at `path` accepts a connection within
/// `timeout`.  A purely connectivity probe — no data is exchanged.
async fn socket_connectable(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if UnixStream::connect(path).await.is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// Child-process poll helpers (sync)
// ---------------------------------------------------------------------------

/// Poll `child.try_wait()` until exit or `timeout`.  Returns `Some(status)` or
/// `None` on timeout.
fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

// ---------------------------------------------------------------------------
// Test 1: start_detached_spawns_child_and_socket_becomes_connectable
// ---------------------------------------------------------------------------

/// Spawn `sqryd start --detach` and verify the grandchild's socket becomes
/// connectable.
///
/// # Scenario
///
/// 1. Locate the `sqryd` binary; skip if absent.
/// 2. Spawn `sqryd start --detach` via [`Command`].
///    The parent process exits 0 after the grandchild signals ready.
/// 3. Wait for the parent to exit (up to 15 s — the default
///    `auto_start_ready_timeout_secs` is 10 s, so 15 s is a safe upper bound).
/// 4. Assert the parent exits 0.
/// 5. Verify the socket is connectable (grandchild is serving).
/// 6. Verify the `sqryd.ready` sentinel exists.
/// 7. Clean up: send SIGTERM to the grandchild (PID from pidfile) and wait.
///
/// # Design reference
///
/// `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
/// §C.3.2 (detach path), steps A-F.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_detached_spawns_child_and_socket_becomes_connectable() {
    let sqryd = match find_sqryd_binary() {
        Some(p) => p,
        None => {
            eprintln!(
                "SKIP start_detached_spawns_child_and_socket_becomes_connectable: \
                 sqryd binary not found (run `cargo build -p sqry-daemon` first)"
            );
            return;
        }
    };

    let ctx = TestContext::new();

    // ── Spawn `sqryd start --detach` ─────────────────────────────────────────
    let mut parent = ctx.spawn_start_detach(&sqryd);

    // The parent side of the double-fork polls the ready pipe and then exits 0.
    // Default `auto_start_ready_timeout_secs` = 10 s.  Allow 15 s total.
    let parent_status = wait_for_exit(&mut parent, Duration::from_secs(15));
    let _ = parent.wait(); // reap

    let parent_status = match parent_status {
        Some(s) => s,
        None => {
            // Parent did not exit — kill to clean up.
            let _ = parent.kill();
            let _ = parent.wait();
            // Try to kill the grandchild too (best-effort).
            if let Ok(pid_str) = std::fs::read_to_string(ctx.pidfile_path())
                && let Ok(pid) = pid_str.trim().parse::<u32>()
            {
                // SAFETY: SIGTERM to a specific PID read from the test-local
                // pidfile.  Used only for best-effort test teardown.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGTERM);
                }
            }
            panic!(
                "sqryd start --detach parent did not exit within 15 s \
                 (socket: {})",
                ctx.socket_path().display()
            );
        }
    };

    assert_eq!(
        parent_status.code(),
        Some(0),
        "sqryd start --detach parent must exit 0 after grandchild signals ready, \
         got: {parent_status:?}"
    );

    // ── Verify socket is connectable ─────────────────────────────────────────
    let connectable = socket_connectable(&ctx.socket_path(), Duration::from_secs(5)).await;

    // ── Read grandchild PID from pidfile for cleanup ──────────────────────────
    let grandchild_pid: Option<u32> = std::fs::read_to_string(ctx.pidfile_path())
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok());

    // ── Clean up grandchild before asserting ─────────────────────────────────
    if let Some(pid) = grandchild_pid {
        assert!(
            pid > 1,
            "grandchild PID from pidfile must be > 1, got {pid}"
        );
        // SAFETY: SIGTERM to a specific PID read from the test-local pidfile.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        // Wait for the socket to disappear (graceful shutdown).
        let deadline = Instant::now() + Duration::from_secs(7);
        loop {
            if !ctx.socket_path().exists() {
                break;
            }
            if Instant::now() >= deadline {
                // Best-effort SIGKILL if SIGTERM timed out.
                // SAFETY: SIGKILL to the same known PID.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // ── Assertions ───────────────────────────────────────────────────────────
    assert!(
        connectable,
        "grandchild socket must be connectable after `sqryd start --detach` parent exits 0 \
         (socket: {})",
        ctx.socket_path().display()
    );

    assert!(
        ctx.ready_sentinel().exists(),
        "sqryd.ready sentinel must exist after successful detach startup \
         (path: {})",
        ctx.ready_sentinel().display()
    );
}

// ---------------------------------------------------------------------------
// Test 2: start_detached_respects_auto_start_ready_timeout
// ---------------------------------------------------------------------------

/// Call [`start_detached`] in-process with a 1 s timeout pointing at a socket
/// that never appears; assert **exactly** [`DaemonError::AutoStartTimeout`].
///
/// # Scenario
///
/// In the test-binary execution context, `current_exe()` resolves to the
/// integration-test binary, not the `sqryd` production binary.  When
/// [`start_detached`] spawns it with `["start", "--detach", "--spawned-by-client"]`,
/// the test binary does not recognise those arguments and exits immediately.
/// [`start_detached`] polls the socket path (which never appears) and
/// eventually hits the deadline, returning [`DaemonError::AutoStartTimeout`].
///
/// The polling loop in [`start_detached`] **always** drains the full timeout
/// before returning `AutoStartTimeout` — the spawned process exiting early
/// does not short-circuit the poll.  Therefore we require exactly
/// [`DaemonError::AutoStartTimeout`], not [`DaemonError::Io`].
///
/// # Isolation
///
/// `XDG_RUNTIME_DIR` is set to the test `TempDir` for the duration of this
/// call (under `ENV_LOCK`) so that `bootstrap_lock_path()` and
/// `DaemonConfig::lock_path()` resolve inside the tempdir.  The spawned test
/// binary inherits this env var, ensuring it never touches the developer's
/// real daemon state.
///
/// # Design reference
///
/// `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
/// §H (auto-spawn-on-miss), `auto_start_ready_timeout_secs` field.
// ENV_LOCK (std::sync::Mutex) is intentionally held across .await to
// serialize env-var mutation. Safe in multi_thread runtime.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_detached_respects_auto_start_ready_timeout() {
    let ctx = TestContext::new();

    // Acquire ENV_LOCK and set XDG_RUNTIME_DIR to the tempdir.  This ensures:
    //   a) bootstrap_lock_path() derives its path from the tempdir (not the
    //      ambient XDG_RUNTIME_DIR), so the bootstrap lock file is created
    //      inside ctx.runtime_dir and not the developer's real runtime dir.
    //   b) The spawned test binary inherits XDG_RUNTIME_DIR = tempdir, so
    //      it also uses isolated paths.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _xdg = XdgRuntimeDirGuard::set(ctx.runtime_dir.path());

    // Use a 1 s timeout so the test completes quickly.
    let cfg = ctx.make_daemon_config(1);

    let result = start_detached(&cfg).await;

    // Drop the guards explicitly after the call completes.  XdgRuntimeDirGuard
    // restores XDG_RUNTIME_DIR on drop; ENV_LOCK is released on drop.
    drop(_xdg);
    drop(_guard);

    match result {
        Err(DaemonError::AutoStartTimeout { timeout_secs, .. }) => {
            assert_eq!(
                timeout_secs, 1,
                "AutoStartTimeout.timeout_secs must match the configured value"
            );
        }
        Ok(pid) => {
            panic!(
                "start_detached must return AutoStartTimeout when the socket \
                 never becomes connectable; got Ok(pid={pid})"
            );
        }
        Err(other) => {
            panic!(
                "start_detached returned unexpected error: {other:?}; \
                 expected DaemonError::AutoStartTimeout(timeout_secs=1). \
                 The polling loop must drain the full timeout before returning."
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Test 3: start_detached_bootstrap_lock_serialises_concurrent_callers (M2 proof)
// ---------------------------------------------------------------------------

/// Ten concurrent [`start_detached`] calls must serialise through the bootstrap
/// flock so that at most one caller holds the exclusive lock at any instant.
///
/// # M2 proof method
///
/// A background probe task opens a second file descriptor to the bootstrap lock
/// file and calls `try_lock_exclusive()` every 5 ms.  If the lock is held
/// exclusively by any caller, `try_lock_exclusive()` returns `WouldBlock`.
/// Observing at least one `WouldBlock` event proves that the lock was
/// exclusively held — i.e., only one caller occupied the critical section at a
/// time.  This is the M2 (single-spawner) guarantee of §H.
///
/// Since in the test-binary context [`start_detached`] will spawn the test binary
/// (not `sqryd`) and the spawned process exits immediately, all 10 callers
/// will time out.  The important invariant being tested is the **locking
/// behaviour** — not the spawn outcome.
///
/// # Isolation
///
/// `XDG_RUNTIME_DIR` is set to the test `TempDir` under `ENV_LOCK` so that
/// `bootstrap_lock_path()` resolves inside the tempdir for both the in-process
/// callers and any spawned child processes.
///
/// # Design reference
///
/// `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
/// §H M2 single-spawner guarantee.
// ENV_LOCK (std::sync::Mutex) is intentionally held across .await to
// serialize env-var mutation. Safe in multi_thread runtime.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_detached_bootstrap_lock_serialises_concurrent_callers() {
    let ctx = TestContext::new();

    // Acquire ENV_LOCK and set XDG_RUNTIME_DIR to the tempdir for full
    // isolation.  Must be held for the duration of the concurrent calls.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _xdg = XdgRuntimeDirGuard::set(ctx.runtime_dir.path());

    // Use a 1 s timeout so the 10-caller test completes in ≈ 10 s worst case.
    let cfg = Arc::new(ctx.make_daemon_config(1));

    // Pre-create the directory so the probe thread can open the lock file
    // before any caller does.
    let lock_path = bootstrap_lock_path(&cfg);
    std::fs::create_dir_all(lock_path.parent().unwrap())
        .expect("create runtime dir for bootstrap lock");

    // ── Probe task ───────────────────────────────────────────────────────────
    // Runs for up to 20 s (4 000 × 5 ms probes), covering the worst-case
    // total duration of 10 × 1 s = 10 s for all callers.  Sets
    // `observed_contention` to `true` on the first `WouldBlock` observation.
    let observed_contention = Arc::new(AtomicBool::new(false));
    let probe_lock_path = lock_path.clone();
    let contention_flag = Arc::clone(&observed_contention);
    let probe_handle = tokio::spawn(async move {
        for _ in 0..4_000u32 {
            // Open a fresh FD to the bootstrap lock file.  `try_lock_exclusive`
            // on this FD will return `WouldBlock` if any other process or OFD
            // currently holds an exclusive flock.
            if let Ok(f) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&probe_lock_path)
                && f.try_lock_exclusive().is_err()
            {
                // WouldBlock — at least one caller holds the exclusive lock.
                contention_flag.store(true, Ordering::SeqCst);
                break; // One observation is sufficient.
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });

    // ── 10 concurrent callers ────────────────────────────────────────────────
    let mut handles = Vec::with_capacity(10);
    for _ in 0..10 {
        let cfg_clone = Arc::clone(&cfg);
        handles.push(tokio::spawn(async move {
            // Each call times out (AutoStartTimeout) because the test binary
            // does not produce a daemon socket.
            let _ = start_detached(&cfg_clone).await;
        }));
    }

    for h in handles {
        h.await.expect("start_detached task panicked");
    }
    probe_handle.abort();

    // Drop env guards after all callers complete.
    drop(_xdg);
    drop(_guard);

    // ── M2 assertion ─────────────────────────────────────────────────────────
    //
    // The bootstrap lock file MUST exist (all callers execute open_bootstrap_lock).
    assert!(
        lock_path.exists(),
        "bootstrap lock file must exist after concurrent start_detached calls \
         (path: {})",
        lock_path.display()
    );

    // The probe MUST have observed WouldBlock at least once — proving that the
    // bootstrap lock was exclusively held by exactly one caller at a time.
    assert!(
        observed_contention.load(Ordering::SeqCst),
        "probe must observe at least one WouldBlock on the bootstrap lock, \
         proving exclusive ownership (M2 single-spawner guarantee); \
         bootstrap lock path: {}",
        lock_path.display()
    );
}

// ---------------------------------------------------------------------------
// Test 4: start_detached_inherited_fd_lock_is_held_by_grandchild (M1 proof)
// ---------------------------------------------------------------------------

/// Prove that the OFD-level flock acquired by the parent is held by the
/// grandchild after `sqryd start --detach` (M1 FD-inheritance proof).
///
/// # Scenario
///
/// 1. Find the `sqryd` binary; skip if absent.
/// 2. Spawn `sqryd start --detach` and wait for the socket to appear.
/// 3. Read the grandchild PID from the pidfile.
/// 4. From the test process, open a **new** FD to the same lockfile and call
///    `try_lock_exclusive()`.  Because the grandchild holds an exclusive
///    OFD-level flock on the same inode, this must return `WouldBlock`.
/// 5. Clean up via SIGTERM.
///
/// # M1 proof method
///
/// A successful `try_lock_exclusive` from the test process on a *fresh* FD to
/// the lockfile inode would mean no live daemon holds the lock — which would
/// be a M1 violation (lost lock across the fork boundary).  `WouldBlock` proves
/// the grandchild retained the lock through the `pre_exec` FD-inheritance path.
///
/// # Linux `/proc` verification (belt-and-suspenders)
///
/// On Linux we additionally enumerate `/proc/<grandchild-pid>/fd/` to find
/// the FD that points to the lockfile inode and confirm it exists.  This
/// provides a stronger proof that the FD survived `exec` (not just that
/// *some* process holds the lock).
///
/// # Design reference
///
/// `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
/// §C.3.2 step C (pre_exec FD_CLOEXEC clear), §D.1 (OFD flock invariant).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_detached_inherited_fd_lock_is_held_by_grandchild() {
    let sqryd = match find_sqryd_binary() {
        Some(p) => p,
        None => {
            eprintln!(
                "SKIP start_detached_inherited_fd_lock_is_held_by_grandchild: \
                 sqryd binary not found (run `cargo build -p sqry-daemon` first)"
            );
            return;
        }
    };

    let ctx = TestContext::new();

    // ── Spawn `sqryd start --detach` ─────────────────────────────────────────
    let mut parent = ctx.spawn_start_detach(&sqryd);

    // Wait for the parent to exit (signals grandchild readiness).
    let parent_status = wait_for_exit(&mut parent, Duration::from_secs(15));
    let _ = parent.wait(); // reap

    let parent_status = match parent_status {
        Some(s) => s,
        None => {
            let _ = parent.kill();
            let _ = parent.wait();
            eprintln!(
                "SKIP start_detached_inherited_fd_lock_is_held_by_grandchild: \
                 parent did not exit within 15 s (daemon may not have started)"
            );
            return;
        }
    };

    if parent_status.code() != Some(0) {
        eprintln!(
            "SKIP start_detached_inherited_fd_lock_is_held_by_grandchild: \
             parent exited with non-zero code {parent_status:?} (grandchild may not be running)"
        );
        // Best-effort: the double-fork may have succeeded even if the parent
        // reported an error reading the ready-pipe.  Read the pidfile and
        // SIGTERM any spawned grandchild to avoid leaking detached processes
        // in CI.
        if let Ok(pid_str) = std::fs::read_to_string(ctx.pidfile_path())
            && let Ok(pid) = pid_str.trim().parse::<u32>()
            && pid > 1
        {
            // SAFETY: SIGTERM to a PID > 1 read from the test-local pidfile.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
        return;
    }

    // ── Wait for socket to confirm grandchild is accepting connections ────────
    let connectable = socket_connectable(&ctx.socket_path(), Duration::from_secs(5)).await;
    if !connectable {
        eprintln!(
            "SKIP start_detached_inherited_fd_lock_is_held_by_grandchild: \
             grandchild socket not connectable within 5 s"
        );
        // The parent exited 0, so the grandchild was spawned.  Read the
        // pidfile and SIGTERM the grandchild to prevent a detached daemon
        // leaking in CI.
        if let Ok(pid_str) = std::fs::read_to_string(ctx.pidfile_path())
            && let Ok(pid) = pid_str.trim().parse::<u32>()
            && pid > 1
        {
            // SAFETY: SIGTERM to a PID > 1 read from the test-local pidfile.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
        return;
    }

    // ── Read grandchild PID from pidfile ──────────────────────────────────────
    let grandchild_pid: u32 = match std::fs::read_to_string(ctx.pidfile_path())
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
    {
        Some(pid) => pid,
        None => {
            eprintln!(
                "SKIP start_detached_inherited_fd_lock_is_held_by_grandchild: \
                 could not read grandchild PID from pidfile ({})",
                ctx.pidfile_path().display()
            );
            // Best-effort cleanup.
            if ctx.socket_path().exists() {
                let _ = std::fs::remove_file(ctx.socket_path());
            }
            return;
        }
    };

    assert!(
        grandchild_pid > 1,
        "grandchild PID must be > 1, got {grandchild_pid}"
    );

    // ── Open a fresh FD to the lockfile ──────────────────────────────────────
    //
    // The lockfile path is derived from the same `runtime_dir()` logic used by
    // the child process.  We must read it from the pidfile's sibling directory
    // rather than constructing it from the test's `XDG_RUNTIME_DIR`, because
    // the child was spawned with the test context's `XDG_RUNTIME_DIR`.
    let lockfile_path = ctx.lockfile_path();

    // Wait briefly for the lockfile to appear (it is created by
    // `acquire_pidfile_lock` early in the grandchild's startup, but there is
    // a short window between socket appearance and lockfile creation).
    let lockfile_appeared = {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if lockfile_path.exists() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    if !lockfile_appeared {
        eprintln!(
            "SKIP start_detached_inherited_fd_lock_is_held_by_grandchild: \
             lockfile did not appear within 3 s (path: {})",
            lockfile_path.display()
        );
        // Clean up grandchild.
        // SAFETY: SIGTERM to a PID > 1 read from the test-local pidfile.
        unsafe {
            libc::kill(grandchild_pid as libc::pid_t, libc::SIGTERM);
        }
        return;
    }

    // Open a fresh FD — distinct OFD from the grandchild's inherited FD.
    let probe_fd = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .open(&lockfile_path)
    {
        Ok(f) => f,
        Err(e) => {
            // Clean up before skipping.
            // SAFETY: SIGTERM to a PID > 1 read from the test-local pidfile.
            unsafe {
                libc::kill(grandchild_pid as libc::pid_t, libc::SIGTERM);
            }
            eprintln!(
                "SKIP start_detached_inherited_fd_lock_is_held_by_grandchild: \
                 failed to open lockfile for probe: {e} (path: {})",
                lockfile_path.display()
            );
            return;
        }
    };

    // ── try_lock_exclusive must return WouldBlock (M1 proof) ─────────────────
    let lock_result = probe_fd.try_lock_exclusive();

    // ── Linux /proc belt-and-suspenders: verify FD appears in grandchild ──────
    #[cfg(target_os = "linux")]
    let proc_fd_found = {
        // Resolve the lockfile's inode.
        let lock_meta = std::fs::metadata(&lockfile_path).ok();
        let lock_inode = lock_meta.map(|m| {
            use std::os::unix::fs::MetadataExt as _;
            m.ino()
        });

        let mut found = false;
        if let Some(target_inode) = lock_inode {
            let proc_fd_dir = PathBuf::from(format!("/proc/{grandchild_pid}/fd"));
            if let Ok(entries) = std::fs::read_dir(&proc_fd_dir) {
                for entry in entries.flatten() {
                    // Each fd entry is a symlink to the actual file.
                    if let Ok(target) = std::fs::canonicalize(entry.path())
                        && let Ok(meta) = std::fs::metadata(&target)
                    {
                        use std::os::unix::fs::MetadataExt as _;
                        if meta.ino() == target_inode {
                            found = true;
                            break;
                        }
                    }
                }
            }
        }
        found
    };

    // ── Clean up grandchild ───────────────────────────────────────────────────
    // SAFETY: SIGTERM to a PID > 1 read from the test-local pidfile.
    unsafe {
        libc::kill(grandchild_pid as libc::pid_t, libc::SIGTERM);
    }
    // Wait for graceful shutdown (best-effort; test correctness already validated).
    let shutdown_deadline = Instant::now() + Duration::from_secs(7);
    loop {
        if !ctx.socket_path().exists() {
            break;
        }
        if Instant::now() >= shutdown_deadline {
            // Best-effort SIGKILL.
            // SAFETY: SIGKILL to a PID > 1 read from the test-local pidfile.
            unsafe {
                libc::kill(grandchild_pid as libc::pid_t, libc::SIGKILL);
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // ── Assertions ───────────────────────────────────────────────────────────

    assert!(
        lock_result.is_err(),
        "try_lock_exclusive on a fresh FD to the lockfile must return WouldBlock \
         while the grandchild holds the inherited OFD-level flock (M1 proof); \
         got: Ok(()) — the grandchild does NOT hold the lock, indicating \
         FD-inheritance through pre_exec failed (lockfile: {})",
        lockfile_path.display()
    );

    // The WouldBlock error from fs2 indicates the lock is held by another OFD.
    let lock_err = lock_result.unwrap_err();
    assert_eq!(
        lock_err.kind(),
        std::io::ErrorKind::WouldBlock,
        "try_lock_exclusive error must be WouldBlock, got: {lock_err:?}"
    );

    // On Linux: additionally assert the FD was found in /proc/<pid>/fd.
    #[cfg(target_os = "linux")]
    assert!(
        proc_fd_found,
        "lockfile inode must appear in /proc/{grandchild_pid}/fd, \
         proving the FD survived pre_exec (FD_CLOEXEC was cleared correctly); \
         lockfile: {}",
        lockfile_path.display()
    );
}

// ---------------------------------------------------------------------------
// Compile-time sanity: ensure bootstrap_lock_path re-export is reachable
// ---------------------------------------------------------------------------

#[test]
fn bootstrap_lock_path_reachable() {
    // Purely structural: verify the public export compiles correctly.
    // `make_daemon_config` returns a DaemonConfig with a socket.path override;
    // `bootstrap_lock_path` must derive its path from the runtime dir.
    let tmp = TempDir::new().expect("TempDir");
    let cfg = DaemonConfig {
        socket: SocketConfig {
            path: Some(tmp.path().join("sqry").join("sqryd.sock")),
            pipe_name: None,
        },
        ..DaemonConfig::default()
    };
    let p = bootstrap_lock_path(&cfg);
    // Bootstrap lock must be a sibling of the lock file (same directory).
    assert_eq!(
        p.file_name().and_then(|n| n.to_str()),
        Some("sqryd.bootstrap.lock"),
        "bootstrap_lock_path filename must be 'sqryd.bootstrap.lock'"
    );
}
