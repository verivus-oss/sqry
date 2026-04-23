//! Phase 8c U16 — live sqryd daemon fixture for end-to-end shim-client tests.
//!
//! Spawns `sqryd-test-server` as a child process bound to a tempdir socket,
//! waits for the `READY\n` signal on its stdout, and provides the socket path
//! so tests can call [`sqry_daemon_client::connect_shim_with_timeouts`]
//! directly. Drop the fixture to kill the child and clean up the socket.
//!
//! # How the binary is located
//!
//! `sqryd-test-server` is built as part of the `sqry-daemon` workspace crate.
//! When `cargo test --workspace` runs, Cargo builds all workspace binaries
//! before running tests, placing them in the same `target/{profile}/`
//! directory as the integration test executables. We locate the binary by
//! checking `CARGO_BIN_EXE_sqryd-test-server` (set by Cargo's test harness)
//! first, then walking from `std::env::current_exe()` up to the
//! `target/<profile>/` directory and appending the binary name — the same
//! approach used by `find_sqry_mcp_binary` in `sqry-mcp/tests/common/mod.rs`.
//!
//! # READY handshake
//!
//! The startup timeout is enforced via a background thread that does the
//! blocking `BufRead::read_line` and sends the result over a channel. The
//! main thread uses `mpsc::Receiver::recv_timeout` so that a stalling child
//! process fails the test within the deadline instead of blocking indefinitely.

#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Locate the `sqryd-test-server` binary built by `sqry-daemon`.
///
/// Checks `CARGO_BIN_EXE_sqryd-test-server` first (set by Cargo during
/// `cargo test --workspace`), then walks up from `current_exe()` to the
/// `target/<profile>/` directory.
pub fn find_sqryd_test_server_binary() -> Option<PathBuf> {
    // Prefer the Cargo-provided path set during integration test execution.
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_sqryd-test-server") {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }
    let binary_name = format!("sqryd-test-server{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().ok()?;
    // Test binaries live in target/debug/deps/; the sqryd-test-server
    // binary is at target/debug/sqryd-test-server.
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

/// A running sqryd test-server process bound to a tempdir socket.
///
/// Dropping this guard kills the child process.
pub struct DaemonFixture {
    child: Child,
    /// Path to the UDS socket the daemon is listening on.
    pub socket_path: PathBuf,
    _stdout_reader: BufReader<ChildStdout>,
    _tmp: tempfile::TempDir,
}

impl DaemonFixture {
    /// Spawn a `sqryd-test-server` process on a fresh tempdir socket.
    ///
    /// Returns `None` when the binary cannot be located (e.g., the workspace
    /// was not built with `cargo build` before running the test). Panics on
    /// any other process-spawn or fixture-init failure so test failures have
    /// a clear root cause.
    ///
    /// Blocks until the process prints `READY\n` to stdout (indicating the
    /// socket is accepting connections) or until the 5-second startup timeout
    /// fires. The timeout is enforced via a background reader thread + channel
    /// so a stalling child cannot hang the test forever.
    pub fn start() -> Option<Self> {
        let binary = find_sqryd_test_server_binary()?;
        let tmp = tempfile::TempDir::new().expect("create tempdir for daemon socket");
        let socket_path = tmp.path().join("sqryd-test.sock");

        let mut child = Command::new(&binary)
            .env("SQRYD_TEST_SOCKET", &socket_path)
            .env("SQRYD_TEST_LOG", "warn")
            // Capture stdout so we can read the READY signal.
            // Inherit stderr so CI logs carry any startup errors.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", binary.display()));

        let stdout = child.stdout.take().expect("child stdout");
        let reader = wait_for_ready(stdout, Duration::from_secs(5), &socket_path);

        // Belt-and-suspenders: also poll the socket file to appear.
        wait_for_socket(&socket_path, Duration::from_secs(2));

        Some(Self {
            child,
            socket_path,
            _stdout_reader: reader,
            _tmp: tmp,
        })
    }
}

impl Drop for DaemonFixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Public entry-point for the bounded READY wait, used by tests that spawn
/// their own child processes (e.g. the cap-exceeded test) and cannot use the
/// `DaemonFixture` helper directly.
pub fn wait_for_ready_pub(
    stdout: ChildStdout,
    timeout: Duration,
    socket_path: &Path,
) -> BufReader<ChildStdout> {
    wait_for_ready(stdout, timeout, socket_path)
}

/// Wait for `READY\n` on `stdout` within `timeout`.
///
/// Runs the blocking `read_line` on a background thread and communicates via
/// a channel, so the calling thread is only blocked up to `timeout` — a
/// stalling child process will trigger a panic rather than hanging forever.
fn wait_for_ready(
    stdout: ChildStdout,
    timeout: Duration,
    socket_path: &Path,
) -> BufReader<ChildStdout> {
    // Channel: background thread sends the BufReader back once READY is seen
    // (or sends None on EOF / error so we can panic with a clear message).
    let (tx, rx) = mpsc::channel::<Result<BufReader<ChildStdout>, String>>();

    let sock_display = socket_path.display().to_string();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = tx.send(Err(
                        "sqryd-test-server exited before sending READY (EOF on stdout)".into(),
                    ));
                    return;
                }
                Ok(_) if line.trim() == "READY" => {
                    let _ = tx.send(Ok(reader));
                    return;
                }
                Ok(_) => {} // unexpected line — keep reading
                Err(e) => {
                    let _ = tx.send(Err(format!("error reading sqryd-test-server stdout: {e}")));
                    return;
                }
            }
        }
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(reader)) => reader,
        Ok(Err(msg)) => panic!("{msg}"),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "sqryd-test-server did not send READY within {}s (socket: {})",
            timeout.as_secs(),
            socket_path.display()
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!(
            "sqryd-test-server reader thread disconnected before READY (socket: {sock_display})"
        ),
    }
}

/// Block until the socket file appears or `timeout` elapses.
fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if path.exists() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("sqryd-test-server socket {} never appeared", path.display());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
