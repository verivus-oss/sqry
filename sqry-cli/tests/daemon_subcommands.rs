//! Task 10 U5 — integration tests for `sqry daemon` subcommands.
//!
//! Tests the full `sqry daemon {start, stop, status, logs}` CLI surface against
//! real processes. Hermetic: each test uses isolated tempdir for socket, pidfile,
//! config and log paths so no test can collide with the user's real sqryd
//! instance.
//!
//! # Test matrix
//!
//! | Test | Requires live daemon | Strategy |
//! |------|---------------------|----------|
//! | `daemon_status_when_not_running_exits_nonzero` | No | Isolated tempdir, no daemon |
//! | `daemon_start_locates_sqryd_binary` | No | `--sqryd-path /nonexistent` → exit nonzero |
//! | `daemon_logs_without_daemon_prints_error` | No | Log file absent → exit nonzero |
//! | `daemon_start_idempotent` | Yes (sqryd) | Skipped if sqryd not found |
//! | `daemon_start_stop_round_trip` | Yes (sqryd) | Skipped if sqryd not found |
//!
//! # Skipping
//!
//! Tests that require the `sqryd` binary are skipped (not `#[ignore]`d) when
//! the binary cannot be located at runtime. This keeps CI green in environments
//! that don't build `sqryd` (e.g. `cargo test -p sqry-cli` without a prior
//! `cargo build -p sqry-daemon`). The skip message is printed to stderr so CI
//! operators can diagnose the gap.
//!
//! # Isolation
//!
//! Every test that interacts with the daemon writes a custom `daemon.toml` via
//! `SQRY_DAEMON_CONFIG` pointing the socket + pidfile + runtime-dir to a
//! private `tempfile::TempDir`. A `DaemonGuard` RAII wrapper ensures the daemon
//! process is killed and the tempdir is cleaned up even when a test panics.
//!
//! For the `daemon_start_stop_round_trip` test, the daemon is started via the
//! `sqry daemon start` CLI path (which calls `sqryd start --detach`). Because
//! the detached grandchild is not a direct child of the test process, a
//! `DetachedDaemonGuard` holds the grandchild PID and sends SIGTERM on drop,
//! providing cleanup parity with `DaemonGuard`.

// Unix-specific: UnixStream connect probes and SIGTERM.
#![cfg(unix)]

mod common;
use common::sqry_bin;

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// ─────────────────────────────────────────────────────────────────────────────
// Binary locators
// ─────────────────────────────────────────────────────────────────────────────

/// Locate the `sqryd` binary produced by `cargo build`.
///
/// Checks `CARGO_BIN_EXE_sqryd` first (set by Cargo during `cargo test
/// --workspace`), then walks up from `current_exe()` to `target/<profile>/`.
fn find_sqryd_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_sqryd") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
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

/// Locate the `sqry` binary for use as the test subject.
fn find_sqry_binary() -> PathBuf {
    sqry_bin()
}

// ─────────────────────────────────────────────────────────────────────────────
// Config writer
// ─────────────────────────────────────────────────────────────────────────────

/// Write a minimal daemon config TOML to `config_path`, pointing the socket
/// to `socket_path` and the runtime dir to `runtime_dir`.
///
/// The runtime dir is where sqryd writes its pidfile (`sqry/sqryd.pid`).
fn write_daemon_config(config_path: &Path, socket_path: &Path, runtime_dir: &Path) {
    let contents = format!(
        "[socket]\npath = {:?}\n",
        socket_path.to_string_lossy().as_ref()
    );
    std::fs::write(config_path, &contents)
        .unwrap_or_else(|e| panic!("write daemon config TOML to {}: {e}", config_path.display()));
    // Ensure the runtime_dir sub-directory exists so the daemon can write
    // its pidfile without racing a mkdir.
    let sqry_runtime = runtime_dir.join("sqry");
    std::fs::create_dir_all(&sqry_runtime)
        .unwrap_or_else(|e| panic!("create runtime dir {}: {e}", sqry_runtime.display()));
}

// ─────────────────────────────────────────────────────────────────────────────
// Socket-readiness poll
// ─────────────────────────────────────────────────────────────────────────────

/// Poll `socket_path` at 50 ms intervals until a `UnixStream::connect`
/// succeeds or `timeout` elapses. Returns `true` when connectable.
fn wait_for_socket_connectable(socket_path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if UnixStream::connect(socket_path).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Poll `socket_path` until `UnixStream::connect` *fails* (socket gone) or
/// `timeout` elapses. Returns `true` when the socket is gone.
fn wait_for_socket_gone(socket_path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if UnixStream::connect(socket_path).is_err() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SIGTERM helper
// ─────────────────────────────────────────────────────────────────────────────

/// Send SIGTERM to a child process.
fn send_sigterm(child: &Child) {
    let pid = child.id();
    // SAFETY: child.id() is a valid OS PID; kill(SIGTERM) is safe to call
    // from any thread and does not cause UB.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DaemonGuard — RAII process cleanup
// ─────────────────────────────────────────────────────────────────────────────

/// RAII wrapper that kills the foreground-mode daemon child on drop.
///
/// Ensures that a panicking test never leaves a stray sqryd process behind.
/// Also holds the `TempDir` so temp files remain alive for the daemon's
/// lifetime and are cleaned up on drop.
struct DaemonGuard {
    child: Child,
    _tmp: tempfile::TempDir,
}

impl DaemonGuard {
    fn new(child: Child, tmp: tempfile::TempDir) -> Self {
        Self { child, _tmp: tmp }
    }

    /// Wait for the child to exit within `timeout`. Returns the exit status.
    ///
    /// Panics if the deadline passes without the process exiting.
    fn wait_exit(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait().expect("try_wait on daemon child") {
                Some(status) => return status,
                None => {
                    if Instant::now() >= deadline {
                        panic!("sqryd did not exit within {}s; killing", timeout.as_secs());
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        // Best-effort: send SIGTERM first so the daemon can clean up its
        // pidfile, then wait briefly before escalating to SIGKILL.
        send_sigterm(&self.child);
        std::thread::sleep(Duration::from_millis(200));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// DetachedDaemonGuard — RAII cleanup for grandchild-mode daemon
// ─────────────────────────────────────────────────────────────────────────────

/// RAII guard for a daemon started via `sqryd start --detach`.
///
/// When the daemon is started via the `sqry daemon start` CLI path, `sqryd
/// start --detach` double-forks so the live daemon process is a grandchild of
/// the test. The test cannot call `wait()` on a grandchild, so `DaemonGuard`
/// (which wraps a direct `Child`) cannot be used.
///
/// `DetachedDaemonGuard` holds the grandchild PID read from the daemon's
/// pidfile and sends SIGTERM on drop (best-effort) to prevent stray processes
/// when a test panics.
///
/// # Cleanup contract
///
/// Drop sends SIGTERM and then SIGKILL after a brief sleep. It does NOT
/// `waitpid` (impossible for non-child processes). The caller must have
/// verified the socket is gone before relying on the daemon to have truly
/// exited.
struct DetachedDaemonGuard {
    pid: Option<u32>,
    /// The tempdir holding the socket and config must outlive the daemon.
    _tmp: tempfile::TempDir,
}

impl DetachedDaemonGuard {
    fn new(pid: u32, tmp: tempfile::TempDir) -> Self {
        Self {
            pid: Some(pid),
            _tmp: tmp,
        }
    }

    /// Disarm the guard: SIGTERM will no longer be sent on drop.
    /// Call this after the daemon has been cleanly stopped.
    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for DetachedDaemonGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid.take() {
            // SAFETY: pid is a live OS PID read from the daemon's own pidfile.
            // kill(SIGTERM) is async-signal-safe.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            std::thread::sleep(Duration::from_millis(200));
            // Escalate to SIGKILL if the daemon did not honour SIGTERM.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PID-file reader
// ─────────────────────────────────────────────────────────────────────────────

/// Read the PID written by the daemon to `pidfile_path`.
/// Returns `None` if the file cannot be read or the contents don't parse.
fn read_pid_from_file(path: &Path) -> Option<u32> {
    let contents = std::fs::read_to_string(path).ok()?;
    contents.trim().parse::<u32>().ok()
}

/// Poll `pidfile_path` until it exists AND contains a valid PID, or until
/// `timeout` elapses. Returns the PID on success.
fn wait_for_pidfile(pidfile_path: &Path, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(pid) = read_pid_from_file(pidfile_path) {
            return Some(pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// T1: daemon_status_when_not_running_exits_nonzero
// ─────────────────────────────────────────────────────────────────────────────

/// Run `sqry daemon status` when no daemon is running and verify the command
/// exits with a nonzero status code (per the design: exit code 1 when not
/// running).
///
/// Uses an isolated tempdir config so the command never finds the user's real
/// running daemon. The `SQRY_DAEMON_CONFIG` env var points to a daemon.toml
/// that names a socket in the tempdir which will never exist.
#[test]
fn daemon_status_when_not_running_exits_nonzero() {
    let tmp = tempfile::TempDir::new().expect("create tempdir");
    let socket_path = tmp.path().join("sqryd-status-test.sock");
    let config_path = tmp.path().join("daemon.toml");

    // Write config pointing to a socket that will never exist.
    write_daemon_config(&config_path, &socket_path, tmp.path());

    let sqry = find_sqry_binary();
    let output = Command::new(&sqry)
        .args(["daemon", "status"])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        // Isolated runtime dir — no real sqryd here.
        .env("XDG_RUNTIME_DIR", tmp.path())
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn sqry daemon status: {e}"));

    // Exit code must be nonzero (1) when the daemon is not running.
    assert!(
        !output.status.success(),
        "sqry daemon status must exit nonzero when no daemon is running; \
         got exit code {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Stderr must contain a human-readable message about the daemon not running.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not running") || stderr.contains("daemon"),
        "stderr must contain diagnostic message; got:\n{stderr}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// T2: daemon_start_locates_sqryd_binary
// ─────────────────────────────────────────────────────────────────────────────

/// Verify that `sqry daemon start --sqryd-path /nonexistent` exits nonzero with
/// a clear error message when the specified `sqryd` binary does not exist.
///
/// This confirms the binary-resolution path is fully wired end-to-end through
/// the `sqry` CLI into `run_daemon_start` → `resolve_sqryd_binary`, without
/// requiring a live daemon.
#[test]
fn daemon_start_locates_sqryd_binary() {
    let tmp = tempfile::TempDir::new().expect("create tempdir");
    let socket_path = tmp.path().join("sqryd-start-locate-test.sock");
    let config_path = tmp.path().join("daemon.toml");

    write_daemon_config(&config_path, &socket_path, tmp.path());

    let sqry = find_sqry_binary();
    let nonexistent = tmp.path().join("no-such-sqryd");

    let output = Command::new(&sqry)
        .args([
            "daemon",
            "start",
            "--sqryd-path",
            nonexistent.to_str().expect("valid UTF-8 path"),
        ])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        .env("XDG_RUNTIME_DIR", tmp.path())
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn sqry daemon start: {e}"));

    // Must exit nonzero when the binary doesn't exist.
    assert!(
        !output.status.success(),
        "sqry daemon start with nonexistent --sqryd-path must exit nonzero; \
         got exit code {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // The error message should mention the path or the failure reason.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("does not exist")
            || combined.contains("not found")
            || combined.contains("no-such-sqryd"),
        "error output must mention missing binary; got:\nstderr: {stderr}\nstdout: {stdout}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// T3: daemon_logs_without_daemon_prints_error
// ─────────────────────────────────────────────────────────────────────────────

/// Verify that `sqry daemon logs` exits nonzero with a meaningful error when
/// the log file is explicitly configured in `daemon.toml` but does not exist
/// on disk (the `!log_path.exists()` branch in `run_daemon_logs`).
///
/// This covers the file-missing error path that is **not** covered by the
/// existing unit test for `resolve_log_path` (which tests the "no log_file
/// configured" branch). The integration test writes a config that explicitly
/// sets `log_file = "<tmp>/missing.log"` pointing to a non-existent file, then
/// verifies the CLI exits nonzero with a diagnostic.
#[test]
fn daemon_logs_without_daemon_prints_error() {
    let tmp = tempfile::TempDir::new().expect("create tempdir");
    let socket_path = tmp.path().join("sqryd-logs-test.sock");
    let config_path = tmp.path().join("daemon.toml");
    // Explicitly configure a log_file path that does NOT exist on disk.
    // This hits the `!log_path.exists()` branch in `run_daemon_logs`, which is
    // distinct from (and not covered by) the unit test for `resolve_log_path`.
    let missing_log = tmp.path().join("missing.log");

    // Write config WITH log_file pointing to a file that will never exist.
    let contents = format!(
        "[socket]\npath = {:?}\nlog_file = {:?}\n",
        socket_path.to_string_lossy().as_ref(),
        missing_log.to_string_lossy().as_ref(),
    );
    std::fs::write(&config_path, &contents)
        .unwrap_or_else(|e| panic!("write daemon config TOML: {e}"));
    let sqry_runtime = tmp.path().join("sqry");
    std::fs::create_dir_all(&sqry_runtime).unwrap_or_else(|e| panic!("create runtime dir: {e}"));

    let sqry = find_sqry_binary();
    let output = Command::new(&sqry)
        .args(["daemon", "logs"])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        .env("XDG_RUNTIME_DIR", tmp.path())
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn sqry daemon logs: {e}"));

    // Must exit nonzero when the configured log file does not exist.
    assert!(
        !output.status.success(),
        "sqry daemon logs must exit nonzero when log_file does not exist; \
         got exit code {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // The error must mention the missing file or its path.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing.log")
            || stderr.contains("not found")
            || stderr.contains("log file"),
        "error message must mention missing log file; got:\n{stderr}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// T4: daemon_start_idempotent
// ─────────────────────────────────────────────────────────────────────────────

/// Start the daemon twice and verify both invocations exit 0 (idempotent start).
///
/// The second `sqry daemon start` should detect the already-running daemon via
/// the socket probe and return 0 (not an error). This mirrors the `systemctl
/// start` idempotency contract described in the design.
///
/// **Requires `sqryd` binary.** Skipped when not available.
#[test]
fn daemon_start_idempotent() {
    let sqryd = match find_sqryd_binary() {
        Some(b) => b,
        None => {
            eprintln!(
                "SKIP daemon_start_idempotent: sqryd binary not found. \
                 Build with `cargo build -p sqry-daemon` first."
            );
            return;
        }
    };

    let tmp = tempfile::TempDir::new().expect("create tempdir");
    let socket_path = tmp.path().join("sqryd-idempotent.sock");
    let config_path = tmp.path().join("daemon.toml");

    write_daemon_config(&config_path, &socket_path, tmp.path());

    // Spawn the daemon in foreground mode (easier to control in tests than
    // --detach, which requires reading the pidfile asynchronously).
    let child = Command::new(&sqryd)
        .args(["foreground"])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        .env("XDG_RUNTIME_DIR", tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn sqryd foreground: {e}"));

    let mut guard = DaemonGuard::new(child, tmp);

    // Wait for the daemon socket to become connectable (up to 15s).
    assert!(
        wait_for_socket_connectable(&socket_path, Duration::from_secs(15)),
        "sqryd foreground socket never became connectable at {} within 15s",
        socket_path.display()
    );

    let sqry = find_sqry_binary();
    let config_path = guard._tmp.path().join("daemon.toml");

    // First `sqry daemon start` — daemon is already running → must exit 0.
    let status1 = Command::new(&sqry)
        .args(["daemon", "start", "--sqryd-path", sqryd.to_str().unwrap()])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        .env("XDG_RUNTIME_DIR", guard._tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn sqry daemon start (1st)");

    assert!(
        status1.success(),
        "first `sqry daemon start` when daemon already running must exit 0; \
         got {status1}"
    );

    // Second `sqry daemon start` — same expectation.
    let status2 = Command::new(&sqry)
        .args(["daemon", "start", "--sqryd-path", sqryd.to_str().unwrap()])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        .env("XDG_RUNTIME_DIR", guard._tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn sqry daemon start (2nd)");

    assert!(
        status2.success(),
        "second `sqry daemon start` when daemon already running must exit 0; \
         got {status2}"
    );

    // Graceful teardown: SIGTERM the daemon and wait for it to exit.
    // `DaemonGuard::drop` will also call kill+wait, but calling wait_exit
    // here first ensures a clean exit assertion rather than a silent kill.
    send_sigterm(&guard.child);
    guard.wait_exit(Duration::from_secs(5));
    // guard drops here, calling kill+wait on the (already-exited) child.
    // `Child::kill` / `Child::wait` on an already-exited process returns
    // an error which we ignore in the Drop impl — this is safe.
}

// ─────────────────────────────────────────────────────────────────────────────
// Best-effort daemon stop helper (for leak prevention)
// ─────────────────────────────────────────────────────────────────────────────

/// Attempt a best-effort `sqry daemon stop` using the same isolated config and
/// runtime-dir env as the live daemon. Used to prevent process leaks in T5
/// when setup steps (pidfile poll, assertions) fail *after* the daemon has
/// already been started but *before* a `DetachedDaemonGuard` can be armed.
///
/// Failures are silently ignored — this is cleanup-only, not a correctness
/// assertion.
fn stop_daemon_best_effort(sqry: &Path, config_path: &Path, runtime_dir: &Path) {
    let _ = Command::new(sqry)
        .args(["daemon", "stop", "--timeout", "5"])
        .env("SQRY_DAEMON_CONFIG", config_path)
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

// ─────────────────────────────────────────────────────────────────────────────
// T5: daemon_start_stop_round_trip
// ─────────────────────────────────────────────────────────────────────────────

/// Full lifecycle round-trip exercising the actual `sqry daemon start` CLI path:
///
/// 1. `sqry daemon start --sqryd-path <sqryd>` — starts the daemon via
///    `sqryd start --detach` (the double-fork path in `run_daemon_start`).
/// 2. Verify `sqry daemon status` exits 0 and produces version output.
/// 3. `sqry daemon stop` — sends `daemon/stop` JSON-RPC and polls for the
///    socket to become unreachable.
/// 4. Verify `sqry daemon status` exits nonzero after the daemon has stopped.
///
/// **Why `sqry daemon start` and not `sqryd foreground`**: this test is the
/// only test in U5 that exercises `run_daemon_start` end-to-end (binary
/// resolution → `sqryd start --detach` → socket readiness poll). All other
/// live-daemon tests use `sqryd foreground` because they need a direct-child
/// handle; this test intentionally uses the detach path and compensates with
/// a `DetachedDaemonGuard` reading the PID from the pidfile.
///
/// **Requires `sqryd` binary.** Skipped when not available.
#[test]
fn daemon_start_stop_round_trip() {
    let sqryd = match find_sqryd_binary() {
        Some(b) => b,
        None => {
            eprintln!(
                "SKIP daemon_start_stop_round_trip: sqryd binary not found. \
                 Build with `cargo build -p sqry-daemon` first."
            );
            return;
        }
    };

    let tmp = tempfile::TempDir::new().expect("create tempdir");
    let socket_path = tmp.path().join("sqryd-round-trip.sock");
    let config_path = tmp.path().join("daemon.toml");
    // With a custom socket configured, the pidfile co-locates beside the
    // socket using its file stem (issue #519 part b): `sqryd-round-trip.sock`
    // yields `sqryd-round-trip.pid` in the same dir, not the default
    // `<XDG_RUNTIME_DIR>/sqry/sqryd.pid`.
    let pidfile_path = tmp.path().join("sqryd-round-trip.pid");

    write_daemon_config(&config_path, &socket_path, tmp.path());

    let sqry = find_sqry_binary();

    // ── Step 1: sqry daemon start → must exit 0 ────────────────────────────
    //
    // This exercises the full `run_daemon_start` code path:
    //   - `resolve_sqryd_binary` via `--sqryd-path`
    //   - exec `sqryd start --detach`
    //   - `poll_until_reachable` waiting for the socket

    let start_status = Command::new(&sqry)
        .args([
            "daemon",
            "start",
            "--sqryd-path",
            sqryd.to_str().expect("sqryd path is valid UTF-8"),
            "--timeout",
            "20",
        ])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        .env("XDG_RUNTIME_DIR", tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn sqry daemon start");

    assert!(
        start_status.success(),
        "sqry daemon start must exit 0 when daemon starts successfully; \
         got {start_status}"
    );

    // The daemon is now running as a detached grandchild.  Read its PID from
    // the pidfile written by `sqryd start --detach` so we can arm the cleanup
    // guard before any assertion can panic.
    //
    // LEAK PREVENTION: if `wait_for_pidfile` times out the daemon is already
    // live but no guard has been armed yet.  Use `stop_daemon_best_effort` to
    // issue a `sqry daemon stop` before panicking, closing the window where a
    // stray process could survive a test failure.
    let grandchild_pid =
        wait_for_pidfile(&pidfile_path, Duration::from_secs(5)).unwrap_or_else(|| {
            stop_daemon_best_effort(&sqry, &config_path, tmp.path());
            panic!(
                "daemon pidfile never appeared at {} within 5s after sqry daemon start",
                pidfile_path.display()
            )
        });

    // Arm the RAII guard *before* any assertion.  If a test assertion below
    // panics, the guard's Drop will SIGTERM the grandchild so no stray daemon
    // is left behind.
    let mut daemon_guard = DetachedDaemonGuard::new(grandchild_pid, tmp);

    // Also verify the socket is connectable (belt-and-suspenders: sqry daemon
    // start already waited for readiness, but double-check before proceeding).
    assert!(
        wait_for_socket_connectable(&socket_path, Duration::from_secs(5)),
        "socket {} not connectable after sqry daemon start succeeded",
        socket_path.display()
    );

    // ── Step 2: sqry daemon status → exit 0 ────────────────────────────────

    let config_path = daemon_guard._tmp.path().join("daemon.toml");

    let status_output = Command::new(&sqry)
        .args(["daemon", "status"])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        .env("XDG_RUNTIME_DIR", daemon_guard._tmp.path())
        .stdin(Stdio::null())
        .output()
        .expect("spawn sqry daemon status (running)");

    assert!(
        status_output.status.success(),
        "sqry daemon status must exit 0 when daemon is running; \
         got {:?}\nstdout: {}\nstderr: {}",
        status_output.status.code(),
        String::from_utf8_lossy(&status_output.stdout),
        String::from_utf8_lossy(&status_output.stderr),
    );

    // Output must contain daemon version information.
    let stdout = String::from_utf8_lossy(&status_output.stdout);
    assert!(
        stdout.contains("sqryd") || stdout.contains("v8"),
        "sqry daemon status output must contain daemon info; got:\n{stdout}"
    );

    // ── Step 3: sqry daemon stop ────────────────────────────────────────────

    let stop_status = Command::new(&sqry)
        .args(["daemon", "stop", "--timeout", "10"])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        .env("XDG_RUNTIME_DIR", daemon_guard._tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn sqry daemon stop");

    assert!(
        stop_status.success(),
        "sqry daemon stop must exit 0 after stopping running daemon; \
         got {stop_status}"
    );

    // Wait for the socket to become unreachable.  The daemon closes its
    // socket as part of graceful shutdown.
    assert!(
        wait_for_socket_gone(&socket_path, Duration::from_secs(10)),
        "socket {} still connectable 10s after sqry daemon stop",
        socket_path.display()
    );

    // Disarm the cleanup guard: the daemon has cleanly shut down.
    // If we do NOT disarm here and the Drop runs, the SIGTERM/SIGKILL are
    // harmless (kill on an already-exited PID fails silently), but disarming
    // is the correct statement of intent.
    daemon_guard.disarm();

    // ── Step 4: sqry daemon status → exit nonzero ──────────────────────────

    let status_after = Command::new(&sqry)
        .args(["daemon", "status"])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        .env("XDG_RUNTIME_DIR", daemon_guard._tmp.path())
        .stdin(Stdio::null())
        .output()
        .expect("spawn sqry daemon status (stopped)");

    assert!(
        !status_after.status.success(),
        "sqry daemon status must exit nonzero after daemon is stopped; \
         got exit code {:?}\nstdout: {}\nstderr: {}",
        status_after.status.code(),
        String::from_utf8_lossy(&status_after.stdout),
        String::from_utf8_lossy(&status_after.stderr),
    );
    // daemon_guard drops here; pid is already None (disarmed), so no kill is sent.
}
