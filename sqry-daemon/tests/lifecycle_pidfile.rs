//! Task 9 U11 — lifecycle + pidfile integration tests.
//!
//! These tests spawn the real `sqryd` binary to verify three invariants of
//! the pidfile-locking and process-lifecycle design:
//!
//! 1. **`first_sqryd_binds_second_rejected_with_ex_tempfail`** — when a `sqryd
//!    foreground` instance already holds the pidfile lock, a second concurrent
//!    `sqryd foreground` call in the same runtime directory must exit 75
//!    (`EX_TEMPFAIL` / `DaemonError::AlreadyRunning`).
//!
//! 2. **`sigterm_triggers_graceful_shutdown_within_drain_deadline`** — sending
//!    `SIGTERM` to a running `sqryd foreground` instance triggers a clean
//!    shutdown that completes within the `ipc_shutdown_drain_secs` + 2s
//!    tolerance window and exits 0.
//!
//! 3. **`sigkill_leaves_stale_pidfile_but_next_start_reclaims`** — `SIGKILL`
//!    terminates the daemon without running `Drop`; the pidfile survives but
//!    the OFD flock is released by the kernel.  The next `sqryd foreground`
//!    call must successfully reclaim the stale pidfile and bind its socket.
//!
//! # Binary discovery
//!
//! The `sqryd` binary is located via:
//!
//! 1. `CARGO_BIN_EXE_sqryd` environment variable — set by Cargo during
//!    `cargo test --workspace` or `cargo test -p sqry-daemon` (Cargo integration
//!    test auto-injection).
//! 2. Walking up from `std::env::current_exe()` to `target/<profile>/` and
//!    looking for `sqryd` in the same directory.
//!
//! If neither path yields an executable the test is **skipped** (not failed) so
//! the suite stays green on hosts that have only built `cargo check` rather than
//! `cargo build`.
//!
//! # Isolation
//!
//! Every test spawns child processes that use an isolated `TempDir` as their
//! `XDG_RUNTIME_DIR`.  The socket path is also overridden via
//! `SQRY_DAEMON_SOCKET` so no two concurrent test runs contend on the same
//! path.  `SQRY_DAEMON_CONFIG` is set to a minimal test config written into
//! the tempdir so the child never loads a developer's `~/.config/sqry/daemon.toml`.
//!
//! # Platform gate
//!
//! All three tests are `#[cfg(unix)]` because they rely on POSIX signals
//! (`SIGTERM`, `SIGKILL`).  On Windows the entire file compiles to an empty
//! test module — Windows lifecycle tests belong in a separate
//! `lifecycle_windows.rs` if/when added.
//!
//! # Design reference
//!
//! `docs/reviews/sqryd-daemon/2026-04-19/task-9-design_iter3_request.md`
//! §D (pidfile locking), §C.3.1 (foreground startup), §C.3.2 (detach path).

#![cfg(unix)]

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Binary discovery
// ---------------------------------------------------------------------------

/// Locate the `sqryd` production binary.
///
/// Returns `None` when the binary cannot be found, which causes the test to
/// be skipped.
fn find_sqryd_binary() -> Option<PathBuf> {
    // Cargo injects this during `cargo test -p sqry-daemon` / `cargo test
    // --workspace` when the `[[bin]]` entry exists in Cargo.toml.
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_sqryd") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }

    // Walk up from the test binary's own path to locate `sqryd` in the same
    // `target/<profile>/` directory.
    let binary_name = format!("sqryd{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().ok()?;

    // Typical path: target/debug/deps/<test-binary>
    //   parent:      target/debug/deps/
    //   grandparent: target/debug/
    let parent = exe.parent()?;
    let candidate = parent.join(&binary_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    let grandparent = parent.parent()?;
    let candidate = grandparent.join(&binary_name);
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

// ---------------------------------------------------------------------------
// Isolation helpers
// ---------------------------------------------------------------------------

/// Per-test process isolation context.
///
/// Holds the `TempDir` that backs `XDG_RUNTIME_DIR` for all child processes
/// spawned within this context.  On drop, the directory (and any socket,
/// pidfile, and lockfile inside it) is removed.
struct TestContext {
    runtime_dir: TempDir,
    /// Path to a minimal valid TOML config written inside the tempdir.
    ///
    /// We pass this via `SQRY_DAEMON_CONFIG` (which clap picks up via
    /// `env = "SQRY_DAEMON_CONFIG"`), so the child never loads the developer's
    /// real `~/.config/sqry/daemon.toml`.  The config sets
    /// `ipc_shutdown_drain_secs = 2` to keep SIGTERM test wall-clock time low.
    config_path: PathBuf,
}

impl TestContext {
    /// Create a fresh isolation context with a minimal daemon config.
    fn new() -> Self {
        let runtime_dir = TempDir::new().expect("create test runtime_dir");
        // Write a minimal valid daemon config with a short drain window so
        // SIGTERM tests complete in reasonable time.  All other fields default.
        let config_path = runtime_dir.path().join("test-daemon.toml");
        std::fs::write(
            &config_path,
            "# sqryd test isolation config\nipc_shutdown_drain_secs = 2\n",
        )
        .expect("write test daemon config");
        Self {
            runtime_dir,
            config_path,
        }
    }

    /// Path to the sqryd socket for this context.
    ///
    /// Uses a fixed name inside the runtime_dir's `sqry/` subdirectory,
    /// mirroring the `DaemonConfig::socket_path()` default.  `SQRY_DAEMON_SOCKET`
    /// is also set explicitly so the child never falls back to the user's real
    /// `XDG_RUNTIME_DIR`.
    fn socket_path(&self) -> PathBuf {
        self.runtime_dir.path().join("sqry").join("sqryd.sock")
    }

    /// Spawn a `sqryd foreground` child with full isolation.
    ///
    /// All `SQRY_DAEMON_*` environment-variable overrides that the daemon
    /// reads via `DaemonConfig::apply_env_overrides()` are stripped from the
    /// child's inherited environment before re-setting only the values this
    /// test intentionally controls.  This prevents a developer's exported
    /// `SQRY_DAEMON_MEMORY_MB`, `SQRY_DAEMON_LOG_FILE`,
    /// `SQRY_DAEMON_AUTO_START_READY_TIMEOUT_SECS`, etc. from influencing the
    /// child process or writing outside the tempdir.
    ///
    /// stdout is piped (unused) to prevent the child from writing to the
    /// test harness terminal.  stderr is inherited so CI captures diagnostic
    /// output on failure.
    fn spawn_foreground(&self, sqryd: &Path) -> Child {
        Command::new(sqryd)
            .arg("foreground")
            // ── Clear the full SQRY_DAEMON_* override surface ──────────────
            // Any of these inherited from the developer's shell could change
            // child behaviour or write files outside the tempdir.
            .env_remove("SQRY_DAEMON_CONFIG")
            .env_remove("SQRY_DAEMON_MEMORY_MB")
            .env_remove("SQRY_DAEMON_SOCKET")
            .env_remove("SQRY_DAEMON_PIPE")
            .env_remove("SQRY_DAEMON_LOG_LEVEL")
            .env_remove("SQRY_DAEMON_LOG_FILE")
            .env_remove("SQRY_DAEMON_STALE_MAX_AGE_HOURS")
            .env_remove("SQRY_DAEMON_TOOL_TIMEOUT_SECS")
            .env_remove("SQRY_DAEMON_MAX_SHIM_CONNECTIONS")
            .env_remove("SQRY_DAEMON_AUTO_START_READY_TIMEOUT_SECS")
            .env_remove("SQRY_DAEMON_LOG_KEEP_ROTATIONS")
            // ── Re-apply only the values this test controls ─────────────────
            // Use the isolation runtime dir so socket / pidfile / lockfile
            // all land inside the tempdir.
            .env("XDG_RUNTIME_DIR", self.runtime_dir.path())
            // Override the socket path explicitly so it is predictable even
            // if the daemon's XDG derivation differs.
            .env("SQRY_DAEMON_SOCKET", self.socket_path())
            // Point to the minimal test config inside the tempdir.
            .env("SQRY_DAEMON_CONFIG", &self.config_path)
            // Quiet log output in CI; errors still surface via exit code.
            .env("SQRY_DAEMON_LOG_LEVEL", "warn")
            // Isolate other path overrides.
            .env_remove("TMPDIR")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| panic!("failed to spawn sqryd: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Readiness polling
// ---------------------------------------------------------------------------

/// Wait for either the socket or the ready sentinel to appear.
///
/// Returns `true` when either indicator is present within `timeout`.
fn wait_for_daemon_up(ctx: &TestContext, timeout: Duration) -> bool {
    // Prefer the ready sentinel (step 15, definitive), but also accept the
    // socket file appearing (step 14, slightly earlier) as sufficient proof
    // that the daemon is alive and past the point where it holds the lock.
    let deadline = Instant::now() + timeout;
    loop {
        let ready_sentinel = ctx.runtime_dir.path().join("sqry").join("sqryd.ready");
        if ready_sentinel.exists() || ctx.socket_path().exists() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ---------------------------------------------------------------------------
// Child-process wait / signal helpers
// ---------------------------------------------------------------------------

/// Poll `child.try_wait()` until the process exits or `timeout` elapses.
///
/// Returns `Some(status)` when the child exits within the window, `None`
/// on timeout.  Never blocks indefinitely.
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

/// Send `signal` to `pid`, logging a warning if the kernel rejects the call.
///
/// # Safety
///
/// Caller must ensure `pid` is a valid process ID that was not yet waited on
/// (i.e. the `Child` is still alive or was very recently killed).  The signal
/// number must be a defined POSIX signal constant.
///
/// This function is safe to call in a multi-threaded test harness: `kill(2)`
/// is async-signal-safe and does not access Rust memory.
fn send_signal(pid: libc::pid_t, signal: libc::c_int) {
    // SAFETY: `pid` is a valid child PID and `signal` is a known POSIX signal.
    // `libc::kill` does not touch Rust objects; it issues a raw syscall.
    let rc = unsafe { libc::kill(pid, signal) };
    if rc < 0 {
        let err = std::io::Error::last_os_error();
        // ESRCH means the process already exited before we sent the signal —
        // that is acceptable (and expected in a race between the bounded wait
        // and the signal delivery).  Log everything else as a warning so test
        // output carries a breadcrumb.
        if err.raw_os_error() != Some(libc::ESRCH) {
            eprintln!(
                "warn: kill({pid}, {signal}) failed: {err} (process may have already exited)"
            );
        }
    }
}

/// Kill `child` forcefully and wait for it to exit with a bounded timeout.
///
/// Best-effort: if the bounded wait times out we do a final `try_wait` and
/// move on without blocking.  This prevents test hangs from leaking zombie
/// processes into subsequent tests.
fn kill_and_reap(child: &mut Child, timeout: Duration) {
    let _ = child.kill(); // std::process::Child::kill sends SIGKILL
    if wait_for_exit(child, timeout).is_none() {
        // Last-ditch: reap if the process has exited between our poll rounds.
        let _ = child.try_wait();
    }
}

// ---------------------------------------------------------------------------
// Test 1: second instance exits 75
// ---------------------------------------------------------------------------

/// The second `sqryd foreground` in the same runtime directory must exit 75.
///
/// # Scenario
///
/// 1. Spawn `sqryd foreground` (first instance).  Wait up to 10 s for it to
///    bind the socket / touch `sqryd.ready` (step 15 of §C.3.1).
/// 2. Spawn a second `sqryd foreground` with the same `XDG_RUNTIME_DIR` and
///    `SQRY_DAEMON_SOCKET`.
/// 3. Assert the second process exits within 5 s with code 75 (`EX_TEMPFAIL`,
///    mapped from `DaemonError::AlreadyRunning`).
/// 4. Send `SIGTERM` to the first process, then bound-wait for its exit.
#[test]
fn first_sqryd_binds_second_rejected_with_ex_tempfail() {
    let sqryd = match find_sqryd_binary() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: sqryd binary not found (run `cargo build -p sqry-daemon` first)");
            return;
        }
    };

    let ctx = TestContext::new();

    // ── Step 1: start first instance ────────────────────────────────────────
    let mut first = ctx.spawn_foreground(&sqryd);
    let first_pid = first.id();

    // Wait for the first instance to be ready (socket or ready sentinel).
    let up = wait_for_daemon_up(&ctx, Duration::from_secs(10));
    if !up {
        kill_and_reap(&mut first, Duration::from_secs(3));
        panic!(
            "first sqryd instance did not become ready within 10 s \
             (socket: {}, runtime_dir: {})",
            ctx.socket_path().display(),
            ctx.runtime_dir.path().display()
        );
    }

    // ── Step 2: start second instance (same runtime dir + socket) ───────────
    let mut second = ctx.spawn_foreground(&sqryd);

    // ── Step 3: second must exit 75 ─────────────────────────────────────────
    let second_status = wait_for_exit(&mut second, Duration::from_secs(5));

    // ── Step 4: clean up the first instance ─────────────────────────────────
    // Signal first; the bounded wait below prevents a hang if SIGTERM is
    // ignored or delayed.
    send_signal(first_pid as libc::pid_t, libc::SIGTERM);
    // Give first a moment to drain and exit; if it does not, kill it.
    if wait_for_exit(&mut first, Duration::from_secs(5)).is_none() {
        kill_and_reap(&mut first, Duration::from_secs(2));
    }

    // ── Assert second's exit code ────────────────────────────────────────────
    let second_status = second_status.unwrap_or_else(|| {
        kill_and_reap(&mut second, Duration::from_secs(2));
        panic!("second sqryd instance did not exit within 5 s");
    });

    assert_eq!(
        second_status.code(),
        Some(75),
        "second sqryd instance must exit 75 (EX_TEMPFAIL / AlreadyRunning), \
         got: {second_status:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 2: SIGTERM triggers graceful shutdown within the drain deadline
// ---------------------------------------------------------------------------

/// `SIGTERM` must cause a running `sqryd foreground` to exit 0 within the
/// `ipc_shutdown_drain_secs` + 2 s tolerance window.
///
/// # Scenario
///
/// 1. Spawn `sqryd foreground`.  Wait up to 10 s for readiness.
/// 2. Send `SIGTERM` to the daemon.
/// 3. Assert the process exits within `ipc_shutdown_drain_secs + 2 s` with
///    exit code 0.  The test config sets `ipc_shutdown_drain_secs = 2`, so
///    the deadline is 4 s total.
///
/// # Note on socket/sentinel cleanup
///
/// This test does NOT assert that the socket file or `sqryd.ready` sentinel
/// are removed on exit.  Socket-path cleanup is performed by `IpcServer::Drop`
/// which runs on the async task thread; its timing relative to process exit
/// may vary.  The lifecycle invariant being tested here is the exit code and
/// drain deadline, not the socket file state.
#[test]
fn sigterm_triggers_graceful_shutdown_within_drain_deadline() {
    let sqryd = match find_sqryd_binary() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: sqryd binary not found (run `cargo build -p sqry-daemon` first)");
            return;
        }
    };

    let ctx = TestContext::new();

    // ── Step 1: start and wait for readiness ─────────────────────────────────
    let mut daemon = ctx.spawn_foreground(&sqryd);
    let daemon_pid = daemon.id();

    let up = wait_for_daemon_up(&ctx, Duration::from_secs(10));
    if !up {
        kill_and_reap(&mut daemon, Duration::from_secs(3));
        panic!(
            "sqryd did not become ready within 10 s \
             (socket: {}, runtime_dir: {})",
            ctx.socket_path().display(),
            ctx.runtime_dir.path().display()
        );
    }

    // ── Step 2: send SIGTERM ──────────────────────────────────────────────────
    send_signal(daemon_pid as libc::pid_t, libc::SIGTERM);

    // ── Step 3: bounded wait for clean exit ──────────────────────────────────
    // Test config: ipc_shutdown_drain_secs = 2; tolerance = 2 s → deadline = 4 s.
    let deadline_secs = 2u64 + 2;
    let status = wait_for_exit(&mut daemon, Duration::from_secs(deadline_secs));

    // If the daemon did not exit within the deadline, kill it so we do not
    // leave a zombie.  We then fail the test.
    let status = status.unwrap_or_else(|| {
        kill_and_reap(&mut daemon, Duration::from_secs(2));
        panic!("sqryd did not exit within {deadline_secs} s after SIGTERM (pid={daemon_pid})")
    });

    assert_eq!(
        status.code(),
        Some(0),
        "sqryd must exit 0 after SIGTERM, got: {status:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: SIGKILL leaves stale pidfile; next start reclaims it
// ---------------------------------------------------------------------------

/// `SIGKILL` does not run `Drop`; the pidfile survives.  The next `sqryd
/// foreground` must detect the stale lock (OFD released by kernel), reclaim
/// the pidfile, and bind the socket successfully.
///
/// # Scenario
///
/// 1. Spawn `sqryd foreground` (first instance).  Wait for readiness.
/// 2. `SIGKILL` the daemon.  Wait for the OS to reap the process.
/// 3. Assert the pidfile still exists (stale — Drop did not run).
/// 4. Remove the stale `sqryd.ready` sentinel and socket file if either still
///    exists.  Both MUST be cleared to prevent a spurious readiness poll result
///    in step 6: `wait_for_daemon_up` accepts the sentinel OR the socket as
///    proof of readiness, so leaving the first daemon's sentinel in place would
///    cause the poll to return `true` immediately without the second daemon
///    ever having started.  The socket is also cleared as test-fixture hygiene
///    (the invariant under test is pidfile reclaim, not `IpcServer::bind`
///    handling a stale socket path).
/// 5. Spawn a second `sqryd foreground` with the same runtime dir.
/// 6. Assert the second instance becomes ready within 10 s.
/// 7. Clean up by sending SIGTERM to the second instance; bound-wait for exit.
#[test]
fn sigkill_leaves_stale_pidfile_but_next_start_reclaims() {
    let sqryd = match find_sqryd_binary() {
        Some(p) => p,
        None => {
            eprintln!("SKIP: sqryd binary not found (run `cargo build -p sqry-daemon` first)");
            return;
        }
    };

    let ctx = TestContext::new();

    // ── Step 1: start first instance ────────────────────────────────────────
    let mut first = ctx.spawn_foreground(&sqryd);
    let first_pid = first.id();

    let up = wait_for_daemon_up(&ctx, Duration::from_secs(10));
    if !up {
        kill_and_reap(&mut first, Duration::from_secs(3));
        panic!(
            "first sqryd instance did not become ready within 10 s \
             (socket: {}, runtime_dir: {})",
            ctx.socket_path().display(),
            ctx.runtime_dir.path().display()
        );
    }

    // Capture pidfile path before SIGKILL races with runtime_dir cleanup.
    let pidfile_path = ctx.runtime_dir.path().join("sqry").join("sqryd.pid");

    // ── Step 2: SIGKILL the daemon ────────────────────────────────────────────
    send_signal(first_pid as libc::pid_t, libc::SIGKILL);

    // Wait for the OS to reap the killed process.
    if wait_for_exit(&mut first, Duration::from_secs(5)).is_none() {
        // SIGKILL is never ignored.  Do one last try_wait; if the process is
        // still not reaped after 5 s the test environment is broken and we must
        // fail immediately rather than continue into the second-start path with
        // a ghost process still holding the OFD lock.
        let still_alive = first
            .try_wait()
            .expect("try_wait on first after SIGKILL")
            .is_none();
        if still_alive {
            panic!(
                "first sqryd instance (pid={first_pid}) was not reaped within 5 s after \
                 SIGKILL; test environment is broken \
                 (runtime_dir: {})",
                ctx.runtime_dir.path().display()
            );
        }
    }

    // ── Step 3: pidfile must still exist (Drop did not run) ──────────────────
    assert!(
        pidfile_path.exists(),
        "pidfile must survive SIGKILL (Drop did not run): {}",
        pidfile_path.display()
    );

    // ── Step 4: remove stale socket and ready-sentinel if present ───────────
    // The socket is scoped out per the doc comment above.
    //
    // The `sqryd.ready` sentinel MUST be removed here.  `wait_for_daemon_up`
    // accepts either the socket or the sentinel as proof of readiness.  If the
    // first daemon left the sentinel behind (SIGKILL does not run Drop), the
    // second readiness poll would return `true` immediately before the second
    // daemon ever reclaims the pidfile or binds its socket — producing a
    // spurious pass.  Clearing the sentinel forces the poll to wait for a fresh
    // socket bind from the reclaiming daemon.
    if ctx.socket_path().exists() {
        let _ = std::fs::remove_file(ctx.socket_path());
    }
    let ready_sentinel = ctx.runtime_dir.path().join("sqry").join("sqryd.ready");
    if ready_sentinel.exists() {
        let _ = std::fs::remove_file(&ready_sentinel);
    }

    // ── Step 5: spawn second instance (same runtime dir) ────────────────────
    let mut second = ctx.spawn_foreground(&sqryd);
    let second_pid = second.id();

    // ── Step 6: second instance must reclaim stale pidfile and start ─────────
    let up = wait_for_daemon_up(&ctx, Duration::from_secs(10));

    // ── Step 7: clean up second instance ─────────────────────────────────────
    send_signal(second_pid as libc::pid_t, libc::SIGTERM);
    if wait_for_exit(&mut second, Duration::from_secs(5)).is_none() {
        kill_and_reap(&mut second, Duration::from_secs(2));
    }

    assert!(
        up,
        "second sqryd instance must reclaim the stale pidfile and bind the socket, \
         but it did not become ready within 10 s \
         (socket: {}, runtime_dir: {})",
        ctx.socket_path().display(),
        ctx.runtime_dir.path().display()
    );
}
