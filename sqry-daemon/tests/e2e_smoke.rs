//! Task 9 U14 — end-to-end smoke tests for the `sqryd` binary.
//!
//! These tests spawn the real `sqryd` binary (not `sqryd-test-server`) and
//! exercise the full startup → handshake → teardown lifecycle.  Each test:
//!
//! 1. Writes a custom daemon config TOML pointing the socket and runtime-dir
//!    to a tempdir so tests never collide with the user's real sqryd instance.
//! 2. Spawns `sqryd foreground` (or `sqryd start --detach`) via
//!    `std::process::Command`.
//! 3. Polls the socket until it becomes connectable (bounded, no hard sleep).
//! 4. Connects via [`sqry_daemon_client::connect_shim_with_timeouts`] and
//!    asserts that `daemon_version` matches `env!("CARGO_PKG_VERSION")`.
//! 5. Sends `SIGTERM` (Unix) / `Child::kill` (Windows), then asserts the
//!    process exits cleanly within 5 seconds.
//!
//! # Binary location
//!
//! The `sqryd` binary is located via:
//!
//! 1. `CARGO_BIN_EXE_sqryd` — set by Cargo for integration tests in the same
//!    workspace package.
//! 2. Walk from `std::env::current_exe()` up to `target/<profile>/sqryd`.
//!
//! If the binary cannot be found, the test is skipped with an explanatory
//! message (same pattern as the `daemon_mode.rs` tests in sqry-mcp).
//!
//! # Platform scope
//!
//! Both tests are Unix-only:
//!
//! - On Windows, `DaemonConfig::socket_path()` always returns a named-pipe
//!   path (`\\.\pipe\<name>`) regardless of the `socket.path` config key, so
//!   the config-isolation approach used here (pointing the socket to a TempDir
//!   path) does not work.  A Windows e2e test would need a different strategy
//!   (named-pipe readiness probing, `socket.pipe_name` config key, and a
//!   `Child::kill()` termination path).
//! - The `start --detach` test additionally requires Unix fork semantics.

// Both smoke tests use Unix-specific APIs, so compile the whole module only on Unix.
#![cfg(unix)]

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Binary locator
// ---------------------------------------------------------------------------

/// Locate the `sqryd` binary produced by `cargo build`.
///
/// Checks `CARGO_BIN_EXE_sqryd` first (set by Cargo during `cargo test
/// --workspace`), then walks up from `current_exe()` to `target/<profile>/`.
fn find_sqryd_binary() -> Option<PathBuf> {
    // Cargo sets this for binaries declared in the same package.
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_sqryd") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }

    let binary_name = format!("sqryd{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().ok()?;

    // current_exe is usually target/debug/deps/<test-binary>
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
// Config writer
// ---------------------------------------------------------------------------

/// Write a minimal daemon config TOML to `config_path`, pointing the socket
/// to `socket_path`.
fn write_daemon_config(config_path: &Path, socket_path: &Path) {
    let contents = format!(
        "[socket]\npath = {:?}\n",
        socket_path.to_string_lossy().as_ref()
    );
    std::fs::write(config_path, contents).expect("write daemon config TOML");
}

// ---------------------------------------------------------------------------
// Socket-readiness poll (Unix blocking, for use before async context is set up)
// ---------------------------------------------------------------------------

/// Poll `socket_path` until a `UnixStream::connect` succeeds or `timeout`
/// elapses.
///
/// Returns `true` when the socket becomes connectable, `false` on timeout.
fn wait_for_socket_connectable(socket_path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if std::os::unix::net::UnixStream::connect(socket_path).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// SIGTERM helper
// ---------------------------------------------------------------------------

/// Send `SIGTERM` to the child process.
fn send_sigterm(child: &Child) {
    // SAFETY: child.id() returns the OS PID.  libc::kill with SIGTERM is
    // safe to call from any thread and cannot fail in practice when the
    // process is still alive.
    let pid = child.id();
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

// ---------------------------------------------------------------------------
// Process guard (kills on drop)
// ---------------------------------------------------------------------------

/// RAII guard that kills the child process on drop.  Used to ensure
/// we never leave a stray daemon process behind even when a test panics.
struct ChildGuard {
    child: Child,
    _stdin: Option<ChildStdin>,
    _stdout_reader: Option<BufReader<ChildStdout>>,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            _stdin: None,
            _stdout_reader: None,
        }
    }

    /// Wait for the process to exit within `timeout`.  Returns the exit status
    /// on success, panics on timeout.
    fn wait_exit(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait().expect("try_wait failed") {
                Some(status) => return status,
                None => {
                    if Instant::now() >= deadline {
                        panic!(
                            "sqryd did not exit within {}s after SIGTERM",
                            timeout.as_secs()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Drop stdin first so the child's stdin reads EOF (for foreground mode
        // where we pass stdin=piped as a convenient handle to detect test drop).
        self._stdin = None;
        self._stdout_reader = None;
        // Best-effort kill; ignore errors (process may have already exited).
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Detached-daemon RAII guard (Unix only)
// ---------------------------------------------------------------------------

/// RAII guard for a detached daemon grandchild process identified only by PID.
///
/// The detach test cannot wrap the grandchild in a [`ChildGuard`] because
/// `std::process::Child` is only available for directly-spawned children.
/// `DetachedDaemonGuard` keeps the resolved PID and sends `SIGTERM`
/// (best-effort) on drop so that a test failure (panic or assertion) before
/// the explicit teardown step cannot leave an orphaned daemon.
///
/// Drop does NOT call `waitpid`—it is fire-and-forget to avoid blocking the
/// test runner's panic-unwind path. The grandchild is adopted by `init` once
/// it exits.
struct DetachedDaemonGuard {
    pid: Option<u32>,
}

impl DetachedDaemonGuard {
    /// Create a guard wrapping `pid`. Pass `None` when the PID is not yet
    /// known; call [`Self::arm`] once the pidfile has been read.
    fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    /// Arm (or re-arm) the guard with a resolved PID.
    fn arm(&mut self, pid: u32) {
        self.pid = Some(pid);
    }

    /// Explicitly disarm the guard: no kill will be sent on drop.
    ///
    /// Call this after the grandchild has been confirmed to have shut down.
    fn disarm(&mut self) {
        self.pid = None;
    }

    /// Send SIGTERM to the grandchild and block until the socket stops
    /// accepting connections (or `timeout` elapses).
    ///
    /// Because the grandchild is not a direct child of the test process,
    /// `waitpid` is unavailable.  Readiness is proxied via
    /// `UnixStream::connect(socket_path).is_err()`: once the IpcServer stops
    /// accepting, connect() returns `Err(ECONNREFUSED)`.
    ///
    /// This method also disarms the guard (takes the PID via `self.pid.take()`).
    ///
    /// Panics if `kill` returns an error (process was not alive at call time)
    /// or if the socket is still connectable after `timeout`.
    fn kill_and_wait(&mut self, socket_path: &Path, timeout: Duration) {
        let pid = match self.pid.take() {
            Some(p) => p,
            None => return,
        };
        // SAFETY: pid is a live OS PID obtained from the pidfile.
        // kill(pid, SIGTERM) is async-signal-safe; no UB.
        let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        assert_eq!(rc, 0, "kill(grandchild={pid}, SIGTERM) failed");

        // Wait for the socket to become unreachable as the readiness proxy
        // for process exit (we cannot waitpid on a non-child).  The socket
        // file stays on disk after the daemon exits; connect() fails with
        // ECONNREFUSED once the IpcServer stops accepting.  Either
        // ECONNREFUSED or ENOENT satisfies is_err(), so the poll is correct
        // regardless of whether the file is eventually cleaned up.
        let socket_gone = {
            let deadline = Instant::now() + timeout;
            loop {
                if std::os::unix::net::UnixStream::connect(socket_path).is_err() {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        assert!(
            socket_gone,
            "sqryd grandchild (pid={pid}) socket still connectable {}s after SIGTERM",
            timeout.as_secs()
        );
    }
}

impl Drop for DetachedDaemonGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid.take() {
            // Best-effort: fire SIGTERM, ignore all errors. The process may
            // have already exited (test passed) or the pid may be stale.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Core smoke-test logic (shared between foreground and detach variants)
// ---------------------------------------------------------------------------

/// Run the smoke protocol against `socket_path`:
/// 1. Connect via `connect_shim_with_timeouts`.
/// 2. Assert `daemon_version == CARGO_PKG_VERSION`.
///
/// Returns `()` on success; panics with a descriptive message on failure.
async fn run_shim_handshake_and_version_check(socket_path: &Path) {
    let conn = sqry_daemon_client::connect_shim_with_timeouts(
        socket_path,
        sqry_daemon_client::ShimProtocol::Mcp,
        std::process::id(),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "connect_shim_with_timeouts failed against sqryd at {}: {e}",
            socket_path.display()
        )
    });

    let got_version = conn.daemon_version().to_owned();
    let expected_version = env!("CARGO_PKG_VERSION");

    assert_eq!(
        got_version, expected_version,
        "daemon_version mismatch: got {got_version:?}, expected {expected_version:?} \
         (hint: sqryd binary may be stale; run `cargo build -p sqry-daemon` before the test)"
    );
}

// ---------------------------------------------------------------------------
// Test: sqryd foreground — bind, version check, SIGTERM
// ---------------------------------------------------------------------------

/// Spawn `sqryd foreground`, wait for socket, handshake, assert `daemon_version`,
/// send SIGTERM, assert exit 0 within 5 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_foreground_bind_version_sigterm() {
    let binary = match find_sqryd_binary() {
        Some(b) => b,
        None => {
            eprintln!(
                "SKIP smoke_foreground_bind_version_sigterm: sqryd binary not found. \
                 Build with `cargo build -p sqry-daemon` first."
            );
            return;
        }
    };

    let tmp = tempfile::TempDir::new().expect("create tempdir for e2e smoke test");
    let socket_path = tmp.path().join("sqryd-e2e-foreground.sock");
    let config_path = tmp.path().join("daemon.toml");

    write_daemon_config(&config_path, &socket_path);

    let child = Command::new(&binary)
        .args(["foreground"])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        // Point XDG_RUNTIME_DIR to the tempdir so sqryd.ready and the pidfile
        // land there instead of the user's real runtime directory.
        .env("XDG_RUNTIME_DIR", tmp.path())
        // Redirect to /dev/null: the test doesn't inspect daemon log output.
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", binary.display()));

    let mut guard = ChildGuard::new(child);

    // Wait up to 10 s for the socket to become connectable.
    assert!(
        wait_for_socket_connectable(&socket_path, Duration::from_secs(10)),
        "sqryd foreground socket never became connectable at {} within 10s",
        socket_path.display()
    );

    // Handshake and version check.
    run_shim_handshake_and_version_check(&socket_path).await;

    // Send SIGTERM and wait for clean exit.
    send_sigterm(&guard.child);

    let status = guard.wait_exit(Duration::from_secs(5));
    assert!(
        status.success(),
        "sqryd foreground did not exit 0 after SIGTERM: {status}"
    );
}

// ---------------------------------------------------------------------------
// Test: sqryd start --detach — bind, version check, SIGTERM (Unix only)
// ---------------------------------------------------------------------------

/// Spawn `sqryd start --detach`, wait for socket, handshake, assert
/// `daemon_version`, then SIGTERM the grandchild and assert the socket
/// becomes unreachable within 5 s.
///
/// **Why exit-code observation is not possible for the detached path**:
/// The detached grandchild is not a direct child of the test process; `wait`
/// and `waitpid` only work for direct children.  The test verifies teardown
/// through connection-refusal instead: once the grandchild stops accepting on
/// its socket, `UnixStream::connect` returns `Err(ECONNREFUSED)`.  The socket
/// file remains on disk (no `IpcServer::Drop` unlink), but `is_err()` is true
/// for both `ECONNREFUSED` and `ENOENT`, so the condition holds regardless.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smoke_detach_bind_version_sigterm() {
    let binary = match find_sqryd_binary() {
        Some(b) => b,
        None => {
            eprintln!(
                "SKIP smoke_detach_bind_version_sigterm: sqryd binary not found. \
                 Build with `cargo build -p sqry-daemon` first."
            );
            return;
        }
    };

    let tmp = tempfile::TempDir::new().expect("create tempdir for e2e smoke detach test");
    let socket_path = tmp.path().join("sqryd-e2e-detach.sock");
    let config_path = tmp.path().join("daemon.toml");
    // The pidfile is written to <XDG_RUNTIME_DIR>/sqry/sqryd.pid
    // (runtime_dir() appends "sqry" to XDG_RUNTIME_DIR per config.rs).
    let pidfile_path = tmp.path().join("sqry").join("sqryd.pid");

    write_daemon_config(&config_path, &socket_path);

    // Arm a cleanup guard *before* spawning so that any early panic still
    // attempts cleanup.  We arm it with the actual PID once the pidfile is
    // readable.
    let mut grandchild_guard = DetachedDaemonGuard::new(None);

    // Step 1: spawn the parent (detach mode). The parent exits 0 once the
    // grandchild signals ready.
    let mut parent = Command::new(&binary)
        .args(["start", "--detach"])
        .env("SQRY_DAEMON_CONFIG", &config_path)
        .env("XDG_RUNTIME_DIR", tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {} --detach: {e}", binary.display()));

    // Step 2: wait for the socket to become connectable (grandchild ready).
    // Use a generous 15-second timeout: the grandchild needs its own startup
    // sequence including plugin manager + IpcServer::bind.
    assert!(
        wait_for_socket_connectable(&socket_path, Duration::from_secs(15)),
        "sqryd start --detach: grandchild socket never connectable at {} within 15s",
        socket_path.display()
    );

    // Step 3: the parent must have exited 0 by now (it exits once the ready
    // pipe delivers EOF, which happens when the grandchild closes its pipe
    // write-end at step 15 of the foreground startup path).
    let parent_status = parent.wait().expect("wait for detach parent");
    assert!(
        parent_status.success(),
        "sqryd start --detach: parent exited with non-zero status: {parent_status}"
    );

    // Step 4: read the grandchild PID from the pidfile and arm the cleanup guard.
    let grandchild_pid = read_pid_from_file(&pidfile_path);
    grandchild_guard.arm(grandchild_pid);

    // Step 5: handshake and version check against the grandchild.
    run_shim_handshake_and_version_check(&socket_path).await;

    // Step 6: send SIGTERM to the grandchild and assert the socket stops
    // accepting within 5s.  kill_and_wait() internally takes the PID out of
    // the guard (disarming it as a side effect), so the explicit disarm() is
    // redundant but kept for clarity.
    grandchild_guard.kill_and_wait(&socket_path, Duration::from_secs(5));
    grandchild_guard.disarm(); // side-effect of kill_and_wait; explicit for readability
}

// ---------------------------------------------------------------------------
// PID file reader
// ---------------------------------------------------------------------------

/// Read the daemon PID from `path` (written atomically by the lifecycle module).
/// Panics if the file cannot be read or parsed.
fn read_pid_from_file(path: &Path) -> u32 {
    let contents = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("could not read pidfile at {}: {e}", path.display()));
    contents.trim().parse::<u32>().unwrap_or_else(|e| {
        panic!(
            "pidfile at {} does not contain a valid PID ({}): {e}",
            path.display(),
            contents.trim()
        )
    })
}
