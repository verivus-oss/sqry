//! Phase 8c U16 — live sqryd daemon fixture for end-to-end shim-client tests
//! in sqry-mcp.
//!
//! Identical in structure to `sqry-lsp/tests/common/daemon_fixture.rs`.
//! Spawns `sqryd-test-server` as a child process bound to a tempdir socket,
//! waits for the `READY\n` signal on its stdout, and provides the socket path
//! so tests can call [`sqry_daemon_client::connect_shim_with_timeouts`]
//! directly.
//!
//! The startup timeout is enforced via a background reader thread + channel
//! so a stalling child cannot hang the test forever (MAJOR fix from Codex
//! iter-0 review: `BufRead::read_line` blocks indefinitely and the deadline
//! check only ran after a line arrived).

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
    /// Returns `None` when the binary cannot be located. Panics on any other
    /// process-spawn or fixture-init failure. Blocks until the process
    /// prints `READY\n` to stdout or until the 5-second startup timeout fires.
    /// The timeout is enforced via a background reader thread + channel so a
    /// stalling child cannot hang the test forever.
    pub fn start() -> Option<Self> {
        let binary = find_sqryd_test_server_binary()?;
        let tmp = tempfile::TempDir::new().expect("create tempdir for daemon socket");
        let socket_path = tmp.path().join("sqryd-mcp-test.sock");

        let mut child = Command::new(&binary)
            .env("SQRYD_TEST_SOCKET", &socket_path)
            .env("SQRYD_TEST_LOG", "warn")
            // Capture stdout for READY handshake; inherit stderr for CI diagnostics.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", binary.display()));

        let stdout = child.stdout.take().expect("child stdout");
        let reader = wait_for_ready(stdout, Duration::from_secs(5), &socket_path);

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
                Ok(_) => {}
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
