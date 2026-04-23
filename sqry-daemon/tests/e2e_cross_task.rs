//! Task 13 -- End-to-End Cross-Task Integration Tests
//!
//! Four e2e tests covering the cross-task gap identified in the master plan
//! (lines 1932-1958).  Each test exercises the full lifecycle of one or more
//! integrated subsystems end-to-end.
//!
//! # Test inventory
//!
//! 1. **`auto_start_from_mcp_shim`** (U2) -- `sqry-mcp --daemon` with no
//!    running daemon → resolves `sqryd` binary → spawns `sqryd start --detach`
//!    → polls socket → connects shim → MCP `initialize` + `notifications/initialized`
//!    succeeds.
//!
//! 2. **`file_change_triggers_rebuild`** (U3) -- load workspace, start watcher,
//!    modify source file, verify updated query results.
//!
//! 3. **`lru_eviction_under_memory_pressure`** (U4) -- load 2 workspaces under
//!    low memory budget, verify synchronous LRU eviction, verify reload.
//!
//! 4. **`concurrent_lsp_mcp_cli_against_one_daemon`** (U5) -- three client
//!    types simultaneously against one daemon, verify all succeed.
//!
//! # Platform scope
//!
//! All tests are Unix-only: they rely on UDS sockets, POSIX signals, and POSIX
//! process lifecycle.
//!
//! # Binary discovery (U2)
//!
//! The `sqry-mcp` and `sqryd` binaries are located via:
//! 1. `CARGO_BIN_EXE_<name>` — set by Cargo during `cargo test --workspace`.
//! 2. Walk from `current_exe()` up to `target/<profile>/`.
//!
//! If either binary is not found, the test is **skipped** (not failed), keeping
//! the suite green on `cargo check`-only hosts.
//!
//! # Design references
//!
//! - `docs/reviews/sqryd-daemon/2026-04-20/task-13-design_iter1_request.md`
//! - `docs/development/sqryd-daemon/TASK_13_IMPLEMENTATION_DAG.md`

#![cfg(unix)]
#![allow(clippy::too_many_lines)]

use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Binary discovery helpers
// ---------------------------------------------------------------------------

/// Locate a production binary by `name` (e.g. `"sqry-mcp"` or `"sqryd"`).
///
/// Search order:
/// 1. `CARGO_BIN_EXE_<name>` — Cargo sets this for binaries in the same
///    workspace when `cargo test --workspace` is invoked.
/// 2. Walk up from `current_exe()`:
///    a. Same directory (`target/debug/deps`).
///    b. Parent directory (`target/debug`).
///
/// Returns `None` when the binary cannot be found; callers skip the test
/// rather than failing.
fn find_binary(name: &str) -> Option<PathBuf> {
    // 1. Cargo env var (only set for binaries in the same crate under test;
    //    sqry-mcp is in a sibling crate, so this will usually be unset for
    //    the sqry-daemon test binary — but we check it for completeness and
    //    future-proofing).
    let env_key = format!("CARGO_BIN_EXE_{name}");
    if let Ok(path) = std::env::var(&env_key) {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(p);
        }
    }

    let binary_name = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().ok()?;

    // Walk up from the test binary's own path.
    //
    // Typical layout under `cargo test --workspace`:
    //   target/debug/deps/<test-binary>  ← current_exe()
    //   target/debug/sqry-mcp            ← what we want (grandparent dir)
    //   target/debug/sqryd               ← likewise
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
// RAII guard: kills a direct child process on drop
// ---------------------------------------------------------------------------

/// RAII guard wrapping a direct child process (`std::process::Child`).
///
/// On drop, sends `kill()` (SIGKILL on Unix) and reaps the child to prevent
/// zombie accumulation. Used for the `sqry-mcp` child spawned by the
/// `auto_start_from_mcp_shim` test.
struct ChildGuard {
    child: Child,
    /// Held here so it is not dropped (and thus closed) until `ChildGuard`
    /// is dropped — dropping `ChildStdin` sends EOF to the child's stdin.
    _stdin: Option<ChildStdin>,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            _stdin: None,
        }
    }

    /// Attach the child's stdin handle so it lives as long as this guard.
    fn with_stdin(mut self, stdin: ChildStdin) -> Self {
        self._stdin = Some(stdin);
        self
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Close stdin first (EOF signal to the child).
        self._stdin = None;
        // Best-effort kill + reap; ignore all errors — the child may have
        // already exited (test passed normally).
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// RAII guard: SIGTERMs a detached daemon grandchild on drop
// ---------------------------------------------------------------------------

/// RAII guard for a detached daemon grandchild identified only by PID.
///
/// `sqryd start --detach` exits immediately after the grandchild is ready;
/// the grandchild cannot be wrapped in `std::process::Child` because it is
/// not a direct child of the test process.  `DetachedDaemonGuard` stores the
/// PID and sends `SIGTERM` (best-effort, fire-and-forget) on drop so that
/// test failures do not leak live daemon processes.
struct DetachedDaemonGuard {
    pid: Option<u32>,
}

impl DetachedDaemonGuard {
    fn unarmed() -> Self {
        Self { pid: None }
    }

    /// Arm the guard with the daemon's PID (obtained from the pidfile).
    fn arm(&mut self, pid: u32) {
        self.pid = Some(pid);
    }

    /// Explicitly disarm: do not send SIGTERM on drop.
    fn disarm(&mut self) {
        self.pid = None;
    }

    /// Send SIGTERM and poll until the process is gone (via `kill(pid, 0)` /
    /// ESRCH) or the timeout elapses, then escalate to SIGKILL if needed.
    ///
    /// # Why ESRCH instead of socket condition
    ///
    /// The daemon can close its IpcServer listener (causing connection
    /// attempts to fail) *before* the process has fully exited. Using
    /// socket-connection-refusal as the "process is gone" signal could
    /// return too early and orphan a shutting-down daemon. Polling
    /// `kill(pid, 0)` / `ESRCH` proves the kernel has reclaimed the PID.
    fn kill_and_wait(&mut self, timeout: Duration) {
        let pid = match self.pid.take() {
            Some(p) => p,
            None => return,
        };
        // Send SIGTERM.
        // SAFETY: pid was obtained from the daemon pidfile and is a live
        // OS process.  SIGTERM is async-signal-safe.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        // Poll via kill(pid, 0): when the PID no longer exists (ESRCH),
        // the process has exited (or been reaped by init after double-fork).
        let deadline = Instant::now() + timeout;
        loop {
            // SAFETY: kill(pid, 0) sends no signal; it only tests existence.
            let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
            if rc != 0 {
                // ESRCH or EPERM both indicate the PID is gone or inaccessible.
                break;
            }
            if Instant::now() >= deadline {
                // Escalate to SIGKILL if the process is still alive after timeout.
                // SAFETY: SIGKILL to a known PID — unconditional.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
                // Brief grace period for SIGKILL to take effect.
                std::thread::sleep(Duration::from_millis(100));
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for DetachedDaemonGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid.take() {
            // Fire-and-forget; ignore errors — the daemon may have already
            // exited (test passed) or the PID may be stale.
            // SAFETY: SIGTERM to a PID obtained from the daemon pidfile.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            // Brief poll: give the process a chance to exit before the
            // test runner tears down any shared resources.  On panic paths
            // we do not block indefinitely — 500 ms maximum.
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                // SAFETY: kill(pid, 0) sends no signal; tests existence only.
                let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
                if rc != 0 {
                    break; // Process has exited.
                }
                if Instant::now() >= deadline {
                    // Best-effort SIGKILL on slow shutdown during panic paths.
                    // SAFETY: SIGKILL to a known PID — unconditional.
                    unsafe {
                        libc::kill(pid as libc::pid_t, libc::SIGKILL);
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Socket readiness helper (sync, for use before async context is set up)
// ---------------------------------------------------------------------------

/// Poll `socket_path` until a `UnixStream::connect` succeeds or the timeout
/// elapses. Returns `true` on success, `false` on timeout.
fn wait_for_socket_sync(socket_path: &Path, timeout: Duration) -> bool {
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

/// Poll `pidfile_path` until a valid PID (> 1) can be read and parsed, or
/// until the deadline expires. Returns the PID on success, `None` on timeout.
///
/// Rationale: the pidfile is written by the daemon after binding the socket.
/// Even after `pidfile_path.exists()` returns `true`, the file may briefly
/// contain zero bytes or a partially-written value if the OS flushes the
/// `write()` call in stages. Retrying until a valid PID is present eliminates
/// the TOCTOU race between file creation and content completion.
fn poll_pidfile_for_valid_pid(pidfile_path: &Path, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(s) = std::fs::read_to_string(pidfile_path)
            && let Ok(pid) = s.trim().parse::<u32>()
            && pid > 1
        {
            return Some(pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// U2: auto_start_from_mcp_shim
// ---------------------------------------------------------------------------

/// End-to-end auto-start test: spawn `sqry-mcp --daemon` with no running
/// daemon and verify the full auto-start + MCP `initialize` lifecycle.
///
/// # Scenario
///
/// 1. Locate `sqry-mcp` and `sqryd` binaries; skip if either is absent.
/// 2. Create an isolated `TempDir` for socket / pidfile / config.
/// 3. Write an empty `daemon.toml` inside the tempdir (prevents `EX_CONFIG`
///    errors from the grandchild reading the developer's real config).
/// 4. Assert no daemon is running (socket does not exist yet).
/// 5. Spawn `sqry-mcp --daemon --daemon-socket <sock>` with:
///    - `XDG_RUNTIME_DIR` → tempdir (so pidfile / ready-sentinel land there)
///    - `SQRYD_PATH` → resolved sqryd binary (auto-start resolution)
///    - `SQRY_DAEMON_CONFIG` → empty config path (grandchild isolation)
///    - `SQRY_DAEMON_SOCKET` → same socket path (grandchild binds here)
///    - `SQRY_DAEMON_NO_AUTO_START` removed (must not suppress auto-start)
///    - stdin piped, stdout piped, stderr inherited
/// 6. Write an MCP `initialize` JSON-RPC request (newline-delimited) to the
///    child's stdin.
/// 7. Read the child's stdout lines within a 30-second timeout; locate the
///    response matching `id=1`.
/// 8. Assert the response contains `result.capabilities` as an object.
/// 9. Send `notifications/initialized` (completes the MCP handshake per spec).
/// 10. Verify the socket is connectable (daemon is still running).
/// 11. Poll pidfile with retry until a valid PID is available (eliminates the
///    TOCTOU race between file creation and content completion).
/// 12. Cleanup: SIGTERM+poll grandchild via `kill(pid, 0)` / ESRCH, then drop
///    sqry-mcp child guard.
///
/// # Timeout rationale
///
/// 30 s: auto-start spawns `sqryd start --detach` (double-fork + IpcServer
/// bind), the MCP `initialize` round-trip then goes through the daemon's
/// hosted rmcp server. This is conservative but deterministic on CI.
///
/// # Why sqry-mcp (not sqry-lsp)
///
/// MCP uses newline-delimited JSON-RPC (no Content-Length framing), making
/// it simpler to drive from a test. The auto-start code paths in sqry-lsp
/// and sqry-mcp are structurally identical (Phase 8c design §2.3).
///
/// # Design reference
///
/// `docs/reviews/sqryd-daemon/2026-04-20/task-13-design_iter1_request.md`
/// §2.3, §3.2, §3.3.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_start_from_mcp_shim() {
    // ── 1. Binary discovery ────────────────────────────────────────────────────
    let sqry_mcp_bin = match find_binary("sqry-mcp") {
        Some(p) => p,
        None => {
            eprintln!(
                "SKIP auto_start_from_mcp_shim: sqry-mcp binary not found \
                 (run `cargo build --workspace` first)"
            );
            return;
        }
    };
    let sqryd_bin = match find_binary("sqryd") {
        Some(p) => p,
        None => {
            eprintln!(
                "SKIP auto_start_from_mcp_shim: sqryd binary not found \
                 (run `cargo build --workspace` first)"
            );
            return;
        }
    };

    // ── 2. Isolated TempDir ────────────────────────────────────────────────────
    let tmp = tempfile::TempDir::new().expect("create tempdir for auto-start e2e test");
    // The sqry subdirectory is where sqryd writes its socket, pidfile, and
    // ready-sentinel by default when XDG_RUNTIME_DIR points to tmp.
    let sqry_dir = tmp.path().join("sqry");
    std::fs::create_dir_all(&sqry_dir).expect("create sqry subdirectory inside tempdir");
    let socket_path = sqry_dir.join("sqryd.sock");
    let pidfile_path = sqry_dir.join("sqryd.pid");

    // ── 3. Empty daemon config ─────────────────────────────────────────────────
    // An empty TOML file is valid — all fields get their #[serde(default)]
    // values.  The `{}` literal is NOT a valid top-level TOML document and
    // would fail the `toml` crate's deserialiser.
    let config_path = tmp.path().join("daemon.toml");
    std::fs::write(&config_path, b"").expect("write empty daemon config");

    // ── 4. Assert no pre-existing daemon ──────────────────────────────────────
    assert!(
        !socket_path.exists(),
        "test precondition: no socket at {} before test starts",
        socket_path.display()
    );

    // ── 5. Spawn sqry-mcp --daemon ─────────────────────────────────────────────
    let mut cmd = Command::new(&sqry_mcp_bin);
    cmd.args(["--daemon", "--daemon-socket"])
        .arg(&socket_path)
        // XDG_RUNTIME_DIR: sqryd writes pidfile + ready-sentinel under
        // $XDG_RUNTIME_DIR/sqry/ — point it to our tempdir.
        .env("XDG_RUNTIME_DIR", tmp.path())
        // SQRYD_PATH: the auto-start logic in sqry-mcp reads this env var
        // (resolution tier 1) to locate the sqryd binary.
        .env("SQRYD_PATH", &sqryd_bin)
        // SQRY_DAEMON_CONFIG: the spawned sqryd child reads this to find
        // its config file.  Point it at our empty-but-valid config so the
        // grandchild never loads ~/.config/sqry/daemon.toml.
        .env("SQRY_DAEMON_CONFIG", &config_path)
        // SQRY_DAEMON_SOCKET: sqry-mcp's auto_start_daemon propagates the
        // resolved socket path to the spawned sqryd via this env var.  Set
        // it explicitly so the grandchild binds on the same path we poll.
        .env("SQRY_DAEMON_SOCKET", &socket_path)
        // Ensure auto-start is not suppressed. Remove the current env var
        // used by sqry-mcp (SQRY_DAEMON_NO_AUTO_START) so a developer
        // override in the ambient environment cannot inadvertently suppress
        // auto-start and cause the test to behave differently.
        .env_remove("SQRY_DAEMON_NO_AUTO_START")
        // Silence sqryd log noise in CI; sqry-mcp logs at info level to stderr.
        .env("SQRY_DAEMON_LOG_LEVEL", "warn")
        // Capture stdin + stdout for the MCP JSON-RPC exchange.
        // Inherit stderr so CI captures diagnostic output on failure.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn sqry-mcp --daemon: {e}"));

    let child_stdin = child.stdin.take().expect("sqry-mcp stdin is piped");
    let child_stdout = child.stdout.take().expect("sqry-mcp stdout is piped");

    // Arm cleanup guards before any blocking I/O that could panic.
    // The grandchild guard is unarmed until we read the pidfile.
    let mut grandchild_guard = DetachedDaemonGuard::unarmed();
    let mut child_guard = ChildGuard::new(child).with_stdin(child_stdin);

    // ── 6. Write MCP initialize request ───────────────────────────────────────
    // MCP uses newline-delimited JSON-RPC: each message is one JSON object
    // terminated by a newline character.  The transport layer (stdio) does NOT
    // use the `Content-Length:` framing that LSP uses.
    //
    // MCP `initialize` is the mandatory first round-trip that establishes the
    // protocol version and capability negotiation.
    let initialize_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "sqry-daemon-e2e-test",
                "version": "0.0.1"
            }
        }
    });
    let mut req_bytes =
        serde_json::to_vec(&initialize_request).expect("serialize initialize request");
    req_bytes.push(b'\n');

    // Send the request. Writing to the child's stdin may block briefly if the
    // pipe buffer is full, but for a single small JSON frame it is always
    // non-blocking in practice.  `spawn_blocking` keeps the tokio runtime
    // healthy.
    {
        // Take stdin out of the guard so we can write to it from a blocking
        // task. We put it back after writing.
        let stdin = child_guard._stdin.take().expect("stdin is present");
        let req_clone = req_bytes.clone();
        let mut stdin_moved = stdin;
        let written = tokio::task::spawn_blocking(move || {
            stdin_moved.write_all(&req_clone).map(|_| stdin_moved)
        })
        .await
        .expect("spawn_blocking did not panic");

        match written {
            Ok(stdin_back) => {
                child_guard._stdin = Some(stdin_back);
            }
            Err(e) => {
                panic!(
                    "failed to write MCP initialize request to sqry-mcp stdin: {e}; \
                     socket={}",
                    socket_path.display()
                );
            }
        }
    }

    // ── 7. Read stdout lines within timeout ────────────────────────────────────
    // auto-start → sqryd start --detach → IpcServer ready → MCP initialize
    // all happen within this 30-second window.
    //
    // We read lines on a background blocking thread and communicate via a
    // channel with timeout. This prevents an un-terminated stdout line from
    // blocking the test indefinitely.
    let response_timeout = Duration::from_secs(30);

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let child_stdout_for_thread = child_stdout;
    std::thread::spawn(move || {
        let reader = BufReader::new(child_stdout_for_thread);
        for line in reader.lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    if tx.send(l).is_err() {
                        break; // Receiver dropped; test is done.
                    }
                }
                Ok(_) => {} // Empty lines: skip.
                Err(e) => {
                    eprintln!("sqry-mcp stdout read error: {e}");
                    break;
                }
            }
        }
    });

    // Collect lines until we find one with `"id":1` or the deadline fires.
    let deadline = Instant::now() + response_timeout;
    let mut initialize_response: Option<serde_json::Value> = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(500))) {
            Ok(line) => {
                // Try to parse as JSON-RPC response.
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line)
                    && v.get("id") == Some(&serde_json::json!(1))
                {
                    initialize_response = Some(v);
                    break;
                }
                // Non-matching id or notification: keep reading.
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // No line in this window; check deadline again in the loop.
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // sqry-mcp closed stdout (it exited).
                break;
            }
        }
    }

    // ── 8. Assert response.result.capabilities is an object ───────────────────
    let response = initialize_response.unwrap_or_else(|| {
        panic!(
            "auto_start_from_mcp_shim: did not receive MCP initialize response \
             with id=1 within {}s;\n\
             sqry-mcp binary: {}\n\
             sqryd binary: {}\n\
             socket: {}",
            response_timeout.as_secs(),
            sqry_mcp_bin.display(),
            sqryd_bin.display(),
            socket_path.display(),
        )
    });

    // JSON-RPC success: response must have `result` (not `error`).
    assert!(
        response.get("error").is_none(),
        "MCP initialize returned an error: {:?}",
        response.get("error")
    );
    let result = response
        .get("result")
        .unwrap_or_else(|| panic!("MCP initialize response has no `result` field: {response:?}"));
    let capabilities = result
        .get("capabilities")
        .unwrap_or_else(|| panic!("MCP initialize result has no `capabilities` field: {result:?}"));
    assert!(
        capabilities.is_object(),
        "MCP initialize result.capabilities must be a JSON object; got: {capabilities:?}"
    );

    // ── 9. Send notifications/initialized (complete the MCP handshake) ─────────
    // Per the MCP 2024-11-05 spec and this repo's own McpTestClient pattern
    // (sqry-mcp/tests/common/mod.rs), the client MUST send
    // `notifications/initialized` after receiving the initialize result.
    // Sending this notification transitions the server to the "initialized"
    // state and completes the handshake — without it, `tools/call` would
    // not be accepted by the server.
    let initialized_notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {}
    });
    let mut notif_bytes =
        serde_json::to_vec(&initialized_notification).expect("serialize notifications/initialized");
    notif_bytes.push(b'\n');
    {
        let stdin = child_guard
            ._stdin
            .take()
            .expect("stdin is present for initialized notif");
        let bytes = notif_bytes;
        let mut stdin_moved = stdin;
        let written =
            tokio::task::spawn_blocking(move || stdin_moved.write_all(&bytes).map(|_| stdin_moved))
                .await
                .expect("spawn_blocking for notifications/initialized did not panic");

        match written {
            Ok(stdin_back) => {
                child_guard._stdin = Some(stdin_back);
            }
            Err(e) => {
                panic!("failed to write notifications/initialized: {e}");
            }
        }
    }

    // ── 10. Verify daemon is running: socket connectable ──────────────────────
    // The daemon is still running (sqry-mcp is still pumping bytes). Verify
    // the infrastructure is in the expected state.
    assert!(
        socket_path.exists(),
        "daemon socket must exist after MCP initialize: {}",
        socket_path.display()
    );
    assert!(
        wait_for_socket_sync(&socket_path, Duration::from_secs(2)),
        "daemon socket must be connectable after MCP initialize: {}",
        socket_path.display()
    );

    // ── 11. Poll pidfile until a valid PID is available ────────────────────────
    // The pidfile may exist but be empty or partially written immediately after
    // the daemon creates the file. We retry until a valid PID (> 1) can be
    // read and parsed, eliminating the TOCTOU race.
    let grandchild_pid = poll_pidfile_for_valid_pid(&pidfile_path, Duration::from_secs(5));
    assert!(
        grandchild_pid.is_some(),
        "daemon pidfile must contain a valid PID within 5s after successful auto-start: {}",
        pidfile_path.display()
    );
    if let Some(pid) = grandchild_pid {
        grandchild_guard.arm(pid);
    }

    // ── 12. Cleanup ─────────────────────────────────────────────────────────────
    // SIGTERM the grandchild (daemon) first. `kill_and_wait` uses
    // `kill(pid, 0)` / ESRCH to confirm the process has actually exited
    // (not merely closed its listener socket) before returning.
    grandchild_guard.kill_and_wait(Duration::from_secs(7));
    grandchild_guard.disarm();
    // child_guard drops here (at end of scope), killing and reaping sqry-mcp.
    drop(child_guard);
}

// ---------------------------------------------------------------------------
// U3: file_change_triggers_rebuild
// U5: concurrent_lsp_mcp_cli_against_one_daemon
// (imports shared by both test functions below)
// ---------------------------------------------------------------------------

mod support;

use std::pin::Pin;

use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::{DaemonConfig, JSONRPC_WORKSPACE_EVICTED, RealWorkspaceBuilder, WorkspaceKey};
use sqry_daemon_client::{AsyncReadWrite, DaemonClient, ShimProtocol, connect_shim_with_timeouts};
use support::init_git_repo;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// End-to-end rebuild-on-file-change test.
///
/// Load a workspace with a real `RealWorkspaceBuilder`, start the
/// file watcher via `dispatcher.ensure_watching`, overwrite a source
/// file, and verify that the next `semantic_search` query observes the
/// updated graph (new symbol found, old symbol absent).
///
/// # Scenario
///
/// 1. Create `TempDir` workspace with `src/lib.rs` containing
///    `pub fn original_function() {}`.  Initialize as a git repo
///    (required by `SourceTreeWatcher::new`).
/// 2. Spawn `TestServer` with `RealWorkspaceBuilder` and
///    `debounce_ms: 200` (fast debounce for test speed).
/// 3. Connect `TestIpcClient`, send `daemon/load`.
/// 4. Look up the `WorkspaceKey` + `Arc<LoadedWorkspace>` via
///    `server.manager`.
/// 5. Call `server.dispatcher.ensure_watching(&key, ws, root)`.
/// 6. Send `semantic_search` for `"original_function"` — assert found.
/// 7. Overwrite `src/lib.rs` with `pub fn renamed_function() {}`.
/// 8. Poll `semantic_search` for `"renamed_function"` at 500 ms
///    intervals, up to 20 s.
/// 9. Assert `"renamed_function"` is found.
/// 10. Assert `"original_function"` is NOT found (stale symbol evicted).
/// 11. Stop TestServer.
///
/// # Timing notes
///
/// The 200 ms debounce window + 500 ms poll interval gives headroom for
/// the `notify::RecommendedWatcher` to deliver the `Write` event, the
/// debouncer to fire, the dispatcher to launch a rebuild, and the
/// `RealWorkspaceBuilder` to re-index the single-file workspace.  The
/// 20 s upper bound is generous on slow CI machines.
///
/// # Design reference
///
/// `docs/reviews/sqryd-daemon/2026-04-20/task-13-design_iter1_request.md`
/// §2.4, §3.2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_change_triggers_rebuild() {
    // ── 1. Create workspace TempDir + git repo ─────────────────────────────────
    let workspace_tmp =
        tempfile::TempDir::new().expect("create workspace tempdir for file-change e2e test");
    let workspace_root = workspace_tmp.path();

    // Write the initial source file.
    std::fs::create_dir_all(workspace_root.join("src"))
        .expect("create src/ directory in workspace tempdir");
    std::fs::write(
        workspace_root.join("src/lib.rs"),
        b"pub fn original_function() {}\n",
    )
    .expect("write initial src/lib.rs");

    // Initialise as a git repo. `SourceTreeWatcher::new` requires a valid
    // `.git/` directory to set up the gitignore-aware file filter. We follow
    // the `WatcherHarness::init_git_repo` pattern from `support/mod.rs`.
    init_git_repo(workspace_root);

    // Canonicalize the root before building the WorkspaceKey. The
    // `daemon/load` handler also canonicalizes via `resolve_index_root`, so
    // both sides agree on the canonical path.
    let canon_root = canonicalize_path(workspace_root).expect("canonicalize workspace root");

    // ── 2. Spawn TestServer with RealWorkspaceBuilder ──────────────────────────
    // Use a short debounce (200 ms) for fast test execution. The socket path
    // override inside `with_builder_and_config` ensures we never touch the
    // developer's real daemon socket.
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let builder = Arc::new(RealWorkspaceBuilder::new(Arc::clone(&plugins)))
        as Arc<dyn sqry_daemon::WorkspaceBuilder>;

    let server = TestServer::with_builder_and_config(
        builder,
        DaemonConfig {
            debounce_ms: 200,
            ..DaemonConfig::default()
        },
    )
    .await;

    // ── 3. Connect TestIpcClient, send daemon/load ─────────────────────────────
    let mut mgmt = TestIpcClient::connect(&server.path).await;
    let hello = mgmt.hello(1).await;
    assert!(
        hello.compatible,
        "management hello must be compatible: {hello:?}"
    );

    let load_resp = mgmt
        .request(
            "daemon/load",
            serde_json::json!({ "index_root": canon_root.to_string_lossy() }),
        )
        .await;
    expect_success(&load_resp);

    // ── 4. Build WorkspaceKey + get LoadedWorkspace Arc ────────────────────────
    // The `daemon/load` handler canonicalizes the root and uses the default
    // `ProjectRootMode::GitRoot` when `root_mode` is omitted.
    let key = WorkspaceKey::new(canon_root.clone(), ProjectRootMode::GitRoot, 0);
    let ws = server.manager.lookup(&key).unwrap_or_else(|| {
        panic!(
            "workspace must be registered after daemon/load; \
                 key.index_root={}",
            key.index_root.display()
        )
    });

    // ── 5. Start file watcher via ensure_watching ──────────────────────────────
    // Pass the cloned root path (not the TempDir itself) so the watcher does
    // not hold a reference that would interfere with tempdir cleanup.
    server
        .dispatcher
        .ensure_watching(&key, ws, canon_root.clone())
        .await
        .expect("ensure_watching must succeed on a freshly-loaded workspace");

    // ── 6. Verify original_function is found before file change ───────────────
    {
        let search_resp = mgmt
            .request(
                "semantic_search",
                serde_json::json!({
                    "query": "name:original_function",
                    "path": canon_root.to_string_lossy(),
                    "max_results": 10,
                    "context_lines": 0,
                    "include_classpath": false,
                }),
            )
            .await;
        let result = expect_success(&search_resp);
        // The IPC `ResponseEnvelope` serialises as:
        //   { "meta": {...}, "result": { "data": { "results": [...], ... }, ... } }
        // The actual SearchHit array is at result["result"]["data"]["results"].
        assert!(
            result["result"]["data"]["results"]
                .as_array()
                .is_some_and(|items| {
                    items
                        .iter()
                        .any(|hit| hit["name"].as_str() == Some("original_function"))
                }),
            "original_function must be found before file change; result={result}"
        );
    }

    // ── 7. Overwrite src/lib.rs with renamed_function ─────────────────────────
    std::fs::write(
        workspace_root.join("src/lib.rs"),
        b"pub fn renamed_function() {}\n",
    )
    .expect("overwrite src/lib.rs with renamed_function");

    // ── 8–9. Poll for renamed_function (up to 20 s, 500 ms intervals) ─────────
    // The file watcher delivers a `Write` event, the 200 ms debouncer fires,
    // the dispatcher re-indexes the workspace, and the new graph is published.
    // We poll until `renamed_function` appears in the query results.
    let poll_timeout = Duration::from_secs(20);
    let poll_interval = Duration::from_millis(500);
    let deadline = Instant::now() + poll_timeout;

    let mut renamed_found = false;
    while Instant::now() < deadline {
        let search_resp = mgmt
            .request(
                "semantic_search",
                serde_json::json!({
                    "query": "name:renamed_function",
                    "path": canon_root.to_string_lossy(),
                    "max_results": 10,
                    "context_lines": 0,
                    "include_classpath": false,
                }),
            )
            .await;

        // Ignore transport/workspace errors during rebuild (the workspace may
        // briefly transition through `Rebuilding` or return a stale result).
        if let sqry_daemon::ipc::protocol::JsonRpcPayload::Success { result } = &search_resp.payload
        {
            // result["result"]["data"]["results"] is the SearchHit array.
            if result["result"]["data"]["results"]
                .as_array()
                .is_some_and(|items| {
                    items
                        .iter()
                        .any(|hit| hit["name"].as_str() == Some("renamed_function"))
                })
            {
                renamed_found = true;
                break;
            }
        }

        tokio::time::sleep(poll_interval).await;
    }

    assert!(
        renamed_found,
        "renamed_function must appear in semantic_search within {}s after file change; \
         workspace={}",
        poll_timeout.as_secs(),
        canon_root.display()
    );

    // ── 10. Assert original_function is no longer found ────────────────────────
    // After a successful rebuild with the renamed content, `original_function`
    // must have been evicted from the graph.
    {
        let search_resp = mgmt
            .request(
                "semantic_search",
                serde_json::json!({
                    "query": "name:original_function",
                    "path": canon_root.to_string_lossy(),
                    "max_results": 10,
                    "context_lines": 0,
                    "include_classpath": false,
                }),
            )
            .await;
        let result = expect_success(&search_resp);
        // Again check the nested data path for consistency.
        assert!(
            !result["result"]["data"]["results"]
                .as_array()
                .is_some_and(|items| {
                    items
                        .iter()
                        .any(|hit| hit["name"].as_str() == Some("original_function"))
                }),
            "original_function must NOT be found after file rename; result={result}"
        );
    }

    // ── 11. Stop TestServer ────────────────────────────────────────────────────
    drop(mgmt);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// U4: lru_eviction_under_memory_pressure
// ---------------------------------------------------------------------------

/// End-to-end LRU eviction test: load two workspaces under a 3-MiB memory
/// budget, verify workspace A is synchronously evicted when workspace B is
/// admitted, verify the -32004 `WorkspaceEvicted` error for stale queries,
/// then reload workspace A and confirm the symbol is accessible again.
///
/// # Eviction mechanism
///
/// Eviction is **synchronous** inside `reserve_rebuild()` (Phase 1 of the
/// §G.1 three-phase admission protocol). It is NOT periodic. When
/// `daemon/load` for workspace B calls `reserve_rebuild`, Phase 1 computes:
///
///   `projected = total_committed_bytes + INITIAL_WORKING_SET_BYTES * 1.5`
///              ≈ `loaded_bytes_A + 3,145,728`
///              > `3,145,728 (3 MiB budget)`
///
/// Because workspace A's actual loaded bytes are non-zero, loading B causes:
///   projected = loaded_bytes_A + 3,145,728 > 3,145,728 = limit
/// Phase 1 calls `plan_eviction` which selects
/// workspace A as the LRU victim. Phase 2 calls `execute_eviction`, which:
/// - Swaps workspace A's `ArcSwap` to an empty graph placeholder
/// - Moves bytes from `loaded_bytes` to `retained_old`
/// - Sets `rebuild_cancelled = true`
/// - Marks state `Evicted`
/// - Removes workspace A from the manager map
///
/// After eviction, any `semantic_search` call with workspace A's path calls
/// `classify_for_serve` which returns `DaemonError::WorkspaceEvicted`
/// (JSON-RPC code -32004, not -32002 which is `WorkspaceStaleExpired`).
/// Note: the design doc §2.5 lists -32002 — this is an error in the design
/// doc. The authoritative constant is `JSONRPC_WORKSPACE_EVICTED = -32004`.
///
/// # No periodic eviction timer
///
/// There is no `reaper_interval_secs` config knob. `WorkspaceManager::
/// new_without_reaper` (used by `TestServer`) omits the retention-reaper
/// task entirely; however eviction itself is admission-driven and does not
/// require the reaper. The reaper only frees retained `Arc<CodeGraph>` pointers
/// after slow queries release them — it does NOT evict workspaces.
///
/// # Design reference
///
/// `docs/reviews/sqryd-daemon/2026-04-20/task-13-design_iter1_request.md`
/// §2.5, §3.1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lru_eviction_under_memory_pressure() {
    // ── 1. Spawn TestServer with RealWorkspaceBuilder and 3-MiB memory cap ────
    //
    // `memory_limit_mb: 3` means the daemon's total budget is exactly
    // 3,145,728 bytes = 2 MiB × 1.5 (= `INITIAL_WORKING_SET_BYTES * WORKING_SET_MULTIPLIER`).
    // The conservative initial estimate for any cold `daemon/load` is also
    // 3,145,728 bytes (§G.6 + `INITIAL_WORKING_SET_BYTES` in `daemon_load.rs`). So:
    //
    //   Load A: projected = 0 + 3,145,728 = 3,145,728 <= 3,145,728 limit → fits exactly
    //   Load B: projected = loaded_bytes_A + 3,145,728 > 3,145,728 limit → eviction!
    //
    // Workspace A's actual graph is non-zero bytes (the Rust plugin parses
    // `func_alpha` + `seed` from the 2 source files), so after loading A,
    // `loaded_bytes_A > 0`, which makes the load-B projection strictly exceed
    // the limit and trigger synchronous LRU eviction of workspace A.
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let builder = Arc::new(RealWorkspaceBuilder::new(Arc::clone(&plugins)))
        as Arc<dyn sqry_daemon::WorkspaceBuilder>;

    let server = TestServer::with_builder_and_config(
        builder,
        DaemonConfig {
            // 3 MiB = 3,145,728 bytes = exactly INITIAL_WORKING_SET_BYTES * 1.5.
            // This makes the first load (A) just fit (projected == limit), while
            // any subsequent load (B) causes eviction because:
            //   projected = loaded_bytes_A + 3,145,728 > 3,145,728 = limit
            // (workspace A's actual graph is non-zero bytes after being built).
            memory_limit_mb: 3,
            ..DaemonConfig::default()
        },
    )
    .await;

    // ── 2. Create workspace A ─────────────────────────────────────────────────
    //
    // Must be git-initialized because the build infrastructure requires the
    // workspace root to be inside a git repository. `init_git_repo` creates
    // `seed.rs` + initial commit, matching the `WatcherHarness::init_git_repo`
    // pattern from `support/mod.rs`.
    let tmp_a = tempfile::TempDir::new().expect("create workspace A tempdir");
    let root_a = tmp_a.path();

    std::fs::create_dir_all(root_a.join("src")).expect("create src/ dir for workspace A");
    std::fs::write(root_a.join("src/lib.rs"), b"pub fn func_alpha() {}\n")
        .expect("write src/lib.rs for workspace A");
    init_git_repo(root_a);
    let canon_a = canonicalize_path(root_a).expect("canonicalize workspace A");

    // ── 3. Load workspace A via daemon/load ────────────────────────────────────
    {
        let mut client = TestIpcClient::connect(&server.path).await;
        client.hello(1).await;
        let resp = client
            .request(
                "daemon/load",
                serde_json::json!({ "index_root": canon_a.to_string_lossy() }),
            )
            .await;
        expect_success(&resp);
    }

    // ── 4. Verify semantic_search finds func_alpha in workspace A ──────────────
    {
        let mut client = TestIpcClient::connect(&server.path).await;
        client.hello(1).await;
        let resp = client
            .request(
                "semantic_search",
                serde_json::json!({
                    "query": "name:func_alpha",
                    "path": canon_a.to_string_lossy(),
                    "max_results": 5,
                    "context_lines": 0,
                    "include_classpath": false,
                }),
            )
            .await;
        let result = expect_success(&resp);
        let symbols_str = result.to_string();
        assert!(
            symbols_str.contains("func_alpha"),
            "semantic_search for 'func_alpha' in workspace A must find the symbol \
             before eviction; result={symbols_str:.500}"
        );
    }

    // ── 5. Create workspace B ─────────────────────────────────────────────────
    let tmp_b = tempfile::TempDir::new().expect("create workspace B tempdir");
    let root_b = tmp_b.path();

    std::fs::create_dir_all(root_b.join("src")).expect("create src/ dir for workspace B");
    std::fs::write(root_b.join("src/lib.rs"), b"pub fn func_beta() {}\n")
        .expect("write src/lib.rs for workspace B");
    init_git_repo(root_b);
    let canon_b = canonicalize_path(root_b).expect("canonicalize workspace B");

    // ── 6. Load workspace B → synchronously evicts workspace A ────────────────
    //
    // `reserve_rebuild` inside `get_or_load` computes:
    //   projected = (A's tiny actual bytes) + 3,145,728 > 3,145,728 limit
    // → Phase 1 calls `plan_eviction` → selects A as the LRU victim
    // → Phase 2 `execute_eviction` removes A from the manager map.
    //
    // The `daemon/load` call for workspace B succeeds; only workspace A is
    // evicted to make room.
    {
        let mut client = TestIpcClient::connect(&server.path).await;
        client.hello(1).await;
        let resp = client
            .request(
                "daemon/load",
                serde_json::json!({ "index_root": canon_b.to_string_lossy() }),
            )
            .await;
        expect_success(&resp);
    }

    // ── 7. Verify daemon/status shows workspace B present and A absent ─────────
    //
    // After eviction, workspace A is removed from the manager map entirely.
    // `daemon/status` only lists workspaces currently tracked in the map.
    {
        let mut client = TestIpcClient::connect(&server.path).await;
        client.hello(1).await;
        let resp = client.request("daemon/status", serde_json::json!({})).await;
        let result = expect_success(&resp);
        let workspaces = result["result"]["workspaces"]
            .as_array()
            .unwrap_or_else(|| {
                panic!("daemon/status result.workspaces must be an array; result={result}")
            });

        // Workspace B must be present.
        let b_path_str = canon_b.to_string_lossy().to_string();
        let b_present = workspaces
            .iter()
            .any(|ws| ws["index_root"].as_str() == Some(&b_path_str));
        assert!(
            b_present,
            "daemon/status must show workspace B loaded after LRU eviction of A; \
             workspaces={workspaces:?}"
        );

        // Workspace A must be absent (evicted workspaces are removed from the map).
        let a_path_str = canon_a.to_string_lossy().to_string();
        let a_absent = !workspaces
            .iter()
            .any(|ws| ws["index_root"].as_str() == Some(&a_path_str));
        assert!(
            a_absent,
            "daemon/status must NOT show workspace A after LRU eviction; \
             workspaces={workspaces:?}"
        );
    }

    // ── 8. Verify semantic_search for func_beta (workspace B) succeeds ─────────
    {
        let mut client = TestIpcClient::connect(&server.path).await;
        client.hello(1).await;
        let resp = client
            .request(
                "semantic_search",
                serde_json::json!({
                    "query": "name:func_beta",
                    "path": canon_b.to_string_lossy(),
                    "max_results": 5,
                    "context_lines": 0,
                    "include_classpath": false,
                }),
            )
            .await;
        let result = expect_success(&resp);
        let symbols_str = result.to_string();
        assert!(
            symbols_str.contains("func_beta"),
            "semantic_search for 'func_beta' in workspace B must find the symbol; \
             result={symbols_str:.500}"
        );
    }

    // ── 9. Verify semantic_search for func_alpha (evicted workspace A) → -32004 ─
    //
    // Workspace A was evicted (removed from the manager map). `classify_for_serve`
    // returns `DaemonError::WorkspaceEvicted` → JSON-RPC code -32004.
    //
    // Important: the design doc §2.5 incorrectly lists -32002 here; that code
    // is `JSONRPC_WORKSPACE_STALE_EXPIRED`. The correct code for a workspace
    // removed from the map is -32004 (`JSONRPC_WORKSPACE_EVICTED`), confirmed
    // by `src/lib.rs` constants and the `classify_for_serve.rs` unit tests.
    {
        let mut client = TestIpcClient::connect(&server.path).await;
        client.hello(1).await;
        let resp = client
            .request(
                "semantic_search",
                serde_json::json!({
                    "query": "name:func_alpha",
                    "path": canon_a.to_string_lossy(),
                    "max_results": 5,
                    "context_lines": 0,
                    "include_classpath": false,
                }),
            )
            .await;
        let err = expect_error(&resp);
        assert_eq!(
            err.code, JSONRPC_WORKSPACE_EVICTED,
            "semantic_search against evicted workspace A must return \
             JSONRPC_WORKSPACE_EVICTED ({}); got code={}: message={}",
            JSONRPC_WORKSPACE_EVICTED, err.code, err.message,
        );
    }

    // ── 10. Reload workspace A via daemon/load ────────────────────────────────
    //
    // Explicitly re-loading an evicted workspace triggers a new `get_or_load`
    // cycle: workspace A is re-inserted as a fresh `LoadedWorkspace` in
    // `Unloaded` state, then loaded. Under the 3-MiB budget this may evict
    // workspace B to make room — expected and correct behaviour.
    {
        let mut client = TestIpcClient::connect(&server.path).await;
        client.hello(1).await;
        let resp = client
            .request(
                "daemon/load",
                serde_json::json!({ "index_root": canon_a.to_string_lossy() }),
            )
            .await;
        expect_success(&resp);
    }

    // ── 11. Verify semantic_search for func_alpha succeeds after reload ────────
    {
        let mut client = TestIpcClient::connect(&server.path).await;
        client.hello(1).await;
        let resp = client
            .request(
                "semantic_search",
                serde_json::json!({
                    "query": "name:func_alpha",
                    "path": canon_a.to_string_lossy(),
                    "max_results": 5,
                    "context_lines": 0,
                    "include_classpath": false,
                }),
            )
            .await;
        let result = expect_success(&resp);
        let symbols_str = result.to_string();
        assert!(
            symbols_str.contains("func_alpha"),
            "semantic_search for 'func_alpha' must find the symbol after \
             workspace A is reloaded; result={symbols_str:.500}"
        );
    }

    // ── 12. Stop TestServer ───────────────────────────────────────────────────
    server.stop().await;
}

// ---------------------------------------------------------------------------
// U5: concurrent_lsp_mcp_cli_against_one_daemon
// ---------------------------------------------------------------------------

/// Write an LSP `Content-Length` framed message to a writer.
async fn write_lsp_frame_bytes<W: AsyncWriteExt + Unpin>(writer: &mut W, body: &str) {
    let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    writer.write_all(frame.as_bytes()).await.unwrap();
}

/// Read raw LSP bytes from a reader (up to 65536 bytes in one recv).
async fn read_lsp_bytes<R: AsyncReadExt + Unpin>(reader: &mut R) -> Vec<u8> {
    let mut buf = vec![0u8; 65536];
    let n = tokio::time::timeout(Duration::from_secs(10), reader.read(&mut buf))
        .await
        .expect("LSP read timeout within 10s")
        .expect("LSP read I/O error");
    assert!(n > 0, "expected LSP response bytes, got EOF");
    buf[..n].to_vec()
}

/// End-to-end concurrent protocol test.
///
/// Connect an LSP shim, an MCP shim, and a CLI management client
/// simultaneously against one `TestServer` backed by the real
/// `RealWorkspaceBuilder`. All three succeed concurrently, and
/// the MCP shim verifies `concurrent_target` is found via
/// `tools/call semantic_search`.
///
/// # Scenario
///
/// 1. Create workspace `TempDir` with `src/lib.rs` containing
///    `pub fn concurrent_target() {}`, git-initialized.
/// 2. Spawn `TestServer` with `RealWorkspaceBuilder`.
/// 3. Load workspace via management `TestIpcClient`.
/// 4. Spawn three concurrent tokio tasks:
///    - **LSP shim**: shim handshake (Lsp) → Content-Length-framed
///      `initialize` → verify `result.capabilities` present.
///    - **MCP shim**: shim handshake (Mcp) → newline-delimited
///      `initialize` → verify `result.capabilities` present.
///    - **CLI**: `DaemonClient::connect` → `daemon/status` → verify
///      `daemon_version` field is non-empty.
/// 5. `tokio::try_join!` all three; all must succeed.
/// 6. Through the MCP shim connection (still open after initialize),
///    send `tools/call semantic_search` for `"concurrent_target"`.
///    Assert the symbol is found.
/// 7. Drop all connections. Poll until `server.shim_registry.len() == 0`.
/// 8. Stop TestServer.
///
/// # Design reference
///
/// `docs/reviews/sqryd-daemon/2026-04-20/task-13-design_iter1_request.md`
/// §2.6, §3.1.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_lsp_mcp_cli_against_one_daemon() {
    // ── 1. Create workspace ────────────────────────────────────────────────────
    let workspace_tmp =
        tempfile::TempDir::new().expect("create workspace tempdir for concurrent e2e test");
    let workspace_root = workspace_tmp.path();

    // Write a minimal source file with the target symbol.
    std::fs::create_dir_all(workspace_root.join("src")).expect("create src/ directory");
    std::fs::write(
        workspace_root.join("src/lib.rs"),
        b"pub fn concurrent_target() {}\n",
    )
    .expect("write src/lib.rs");

    // Initialize as a git repo so the workspace can be loaded and
    // (if needed) watched. `SourceTreeWatcher::new` requires a git root.
    init_git_repo(workspace_root);

    // Canonicalize the workspace root for use as a WorkspaceKey.
    let canon_root = canonicalize_path(workspace_root).expect("canonicalize workspace root");

    // ── 2. Spawn TestServer with RealWorkspaceBuilder ──────────────────────────
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let builder = Arc::new(RealWorkspaceBuilder::new(Arc::clone(&plugins)))
        as Arc<dyn sqry_daemon::WorkspaceBuilder>;

    let server = TestServer::with_builder_and_config(builder, DaemonConfig::default()).await;
    let socket_path = server.path.clone();
    let shim_registry = server.shim_registry();

    // ── 3. Load workspace via management client ────────────────────────────────
    // The management client performs the DaemonHello handshake and then
    // speaks JSON-RPC. We send `daemon/load` to trigger a real graph build.
    {
        let mut mgmt = TestIpcClient::connect(&socket_path).await;
        let hello_resp = mgmt.hello(1).await;
        assert!(
            hello_resp.compatible,
            "management hello must be compatible: {hello_resp:?}"
        );

        let load_resp = mgmt
            .request(
                "daemon/load",
                serde_json::json!({ "index_root": canon_root.to_string_lossy() }),
            )
            .await;

        match &load_resp.payload {
            sqry_daemon::ipc::protocol::JsonRpcPayload::Success { result } => {
                // Successful load; the result may contain workspace state info.
                let _ = result;
            }
            sqry_daemon::ipc::protocol::JsonRpcPayload::Error { error } => {
                panic!(
                    "daemon/load failed for {}: code={} message={}",
                    canon_root.display(),
                    error.code,
                    error.message
                );
            }
        }
        // mgmt drops here; connection closes.
    }

    // ── 4. Spawn 3 concurrent tokio tasks ─────────────────────────────────────
    //
    // All three tasks connect to the same TestServer socket simultaneously.
    // The IpcServer's shape-discriminating first-frame dispatcher routes
    // each connection to the correct handler independently.

    // --- LSP task ---
    // Shim handshake → LSP Content-Length-framed `initialize` → verify response.
    let socket_path_lsp = socket_path.clone();
    let lsp_task = tokio::spawn(async move {
        // Shim handshake: sends ShimRegister{protocol:Lsp, pid} and reads
        // ShimRegisterAck. After this the stream carries raw LSP bytes.
        let conn = connect_shim_with_timeouts(
            &socket_path_lsp,
            ShimProtocol::Lsp,
            std::process::id(),
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .await
        .expect("LSP shim connect+handshake must succeed");

        // The stream is now in LSP byte-pump mode.
        let mut stream = conn.into_stream();

        // Send LSP `initialize` request (Content-Length framed).
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "rootUri": null,
                "capabilities": {}
            }
        })
        .to_string();
        write_lsp_frame_bytes(&mut stream, &body).await;

        // Read the response.
        let response_bytes = read_lsp_bytes(&mut stream).await;
        let response_str = String::from_utf8_lossy(&response_bytes);

        // Assert LSP Content-Length framing.
        assert!(
            response_str.contains("Content-Length:"),
            "LSP response must be Content-Length framed; got: {response_str:.200}"
        );

        // Parse the JSON body by splitting on the CRLFCRLF header separator.
        // The LSP wire format is: "Content-Length: N\r\n\r\n<JSON>".
        let json_body = response_str
            .find("\r\n\r\n")
            .map(|idx| &response_str[idx + 4..])
            .unwrap_or_else(|| {
                panic!(
                    "LSP response missing CRLFCRLF header/body separator; \
                     got: {response_str:.300}"
                )
            });

        let response: serde_json::Value = serde_json::from_str(json_body).unwrap_or_else(|e| {
            panic!(
                "LSP initialize response JSON body failed to parse: {e}; \
                     body: {json_body:.300}"
            )
        });

        // Must not be an error response.
        assert!(
            response.get("error").is_none(),
            "LSP initialize must not return error; got: {response:?}"
        );
        // Must echo id=1.
        assert!(
            response.get("id") == Some(&serde_json::json!(1)),
            "LSP initialize response must echo id=1; got: {response:?}"
        );
        // Must have `result.capabilities`.
        let result = response.get("result").unwrap_or_else(|| {
            panic!("LSP initialize response has no `result`; got: {response:?}")
        });
        assert!(
            result.get("capabilities").is_some(),
            "LSP initialize result must have `capabilities`; got: {result:?}"
        );

        // Return the stream so the caller can drop it after the join.
        stream
    });

    // --- MCP task ---
    // Shim handshake → newline-delimited `initialize` → verify response.
    // We hold the stream open after initialize so we can send tools/call later.
    let socket_path_mcp = socket_path.clone();
    let mcp_task: tokio::task::JoinHandle<Pin<Box<dyn AsyncReadWrite + Send>>> =
        tokio::spawn(async move {
            let conn = connect_shim_with_timeouts(
                &socket_path_mcp,
                ShimProtocol::Mcp,
                std::process::id(),
                Duration::from_secs(5),
                Duration::from_secs(5),
            )
            .await
            .expect("MCP shim connect+handshake must succeed");

            let mut stream = conn.into_stream();

            // Send MCP `initialize` (newline-delimited JSON-RPC).
            let initialize = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "sqry-daemon-e2e-concurrent-test",
                        "version": "0.0.1"
                    }
                }
            });
            let mut req_bytes = serde_json::to_vec(&initialize).expect("serialize MCP initialize");
            req_bytes.push(b'\n');
            stream
                .write_all(&req_bytes)
                .await
                .expect("write MCP initialize");

            // Read MCP initialize response (newline-delimited).
            // Use a line-reader to find the JSON response line.
            let mut buf = Vec::with_capacity(4096);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                let mut byte = [0u8; 1];
                let n = tokio::time::timeout(
                    deadline
                        .saturating_duration_since(tokio::time::Instant::now())
                        .max(Duration::from_millis(1)),
                    stream.read(&mut byte),
                )
                .await
                .expect("MCP initialize response read timeout")
                .expect("MCP initialize response read I/O error");
                assert!(n > 0, "MCP stream closed before initialize response");
                buf.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out reading MCP initialize response"
                );
            }

            let response_str = String::from_utf8_lossy(&buf);
            let response: serde_json::Value = serde_json::from_slice(&buf).unwrap_or_else(|e| {
                panic!(
                    "MCP initialize response is not valid JSON: {e}; \
                         got: {response_str:.200}"
                )
            });

            // Must have `result.capabilities` (no `error` key).
            assert!(
                response.get("error").is_none(),
                "MCP initialize must not return error; got: {response:?}"
            );
            let result = response.get("result").unwrap_or_else(|| {
                panic!("MCP initialize response has no `result`; got: {response:?}")
            });
            assert!(
                result.get("capabilities").is_some(),
                "MCP initialize result must have `capabilities`; got: {result:?}"
            );

            // Send `notifications/initialized` to complete the MCP handshake.
            // Without this the server stays in the initializing state and
            // rejects `tools/call` requests.
            let initialized_notif = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            });
            let mut notif_bytes = serde_json::to_vec(&initialized_notif)
                .expect("serialize notifications/initialized");
            notif_bytes.push(b'\n');
            stream
                .write_all(&notif_bytes)
                .await
                .expect("write notifications/initialized");

            // Return stream open for the tools/call phase.
            stream
        });

    // --- CLI task ---
    // Management connection → daemon/status → verify daemon_version.
    let socket_path_cli = socket_path.clone();
    let cli_task = tokio::spawn(async move {
        let mut client = DaemonClient::connect(&socket_path_cli)
            .await
            .expect("DaemonClient::connect must succeed");

        let status = client.status().await.expect("daemon/status must succeed");

        // `DaemonClient::status()` returns the JSON-RPC `result` field from the
        // daemon's response envelope. The ResponseEnvelope wraps the inner
        // `DaemonStatus` payload under a `result` sub-key, so the returned
        // value has the shape:
        //   `{"meta": {"daemon_version": "8.x.y", "stale": false}, "result": {...}}`
        //
        // We assert against the canonical documented path `meta.daemon_version`.
        // Probing multiple fallback paths would mask regressions where the
        // primary field is accidentally omitted.
        let version_str = status
            .get("meta")
            .and_then(|m| m.get("daemon_version"))
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "daemon/status response must include `meta.daemon_version`; \
                     got: {status:?}"
                )
            });
        assert!(
            !version_str.is_empty(),
            "daemon/status `meta.daemon_version` must be a non-empty string; \
             got: {status:?}"
        );
    });

    // ── 5. Join all three tasks — all must succeed ─────────────────────────────
    // `try_join!` short-circuits on the first JoinError (panic or cancellation),
    // matching the DAG §U5 step 5 spec requirement.
    let (lsp_stream, mut mcp_stream, ()) =
        tokio::try_join!(lsp_task, mcp_task, cli_task).expect("all concurrent tasks must succeed");

    // ── 6. Send tools/call semantic_search via MCP shim ────────────────────────
    // The MCP shim is still open and in the initialized state (we sent
    // notifications/initialized before returning from the task). We send a
    // `tools/call` to verify the real workspace data is accessible.
    let search_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "semantic_search",
            "arguments": {
                "query": "concurrent_target",
                "path": canon_root.to_string_lossy(),
                "max_results": 5,
                "context_lines": 0,
                "include_classpath": false
            }
        }
    });
    let mut search_bytes =
        serde_json::to_vec(&search_request).expect("serialize semantic_search tools/call");
    search_bytes.push(b'\n');
    mcp_stream
        .write_all(&search_bytes)
        .await
        .expect("write tools/call semantic_search");

    // Read newline-delimited response(s) until we find id=2.
    let search_response = 'find_response: {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            // Read one newline-delimited line.
            let mut line_buf = Vec::with_capacity(4096);
            loop {
                let remaining = deadline
                    .saturating_duration_since(tokio::time::Instant::now())
                    .max(Duration::from_millis(1));
                let mut byte = [0u8; 1];
                let n = tokio::time::timeout(remaining, mcp_stream.read(&mut byte))
                    .await
                    .expect("tools/call response read timeout")
                    .expect("tools/call response read I/O error");
                assert!(n > 0, "MCP stream closed before tools/call response");
                line_buf.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out reading tools/call response line"
                );
            }

            if line_buf.iter().all(|&b| b == b'\n' || b == b'\r') {
                // Empty / whitespace-only line: skip.
                continue;
            }
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&line_buf)
                && v.get("id") == Some(&serde_json::json!(2))
            {
                break 'find_response v;
            }
            // Notification, different id, or parse failure: keep reading.
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for tools/call id=2 response"
            );
        }
    };

    // Assert tools/call returned a result (not error).
    assert!(
        search_response.get("error").is_none(),
        "tools/call semantic_search must not return JSON-RPC error; got: {search_response:?}"
    );
    let tool_result = search_response.get("result").unwrap_or_else(|| {
        panic!("tools/call response must have `result`; got: {search_response:?}")
    });

    // The MCP tools/call result for `semantic_search` returns the JSON body in
    // `content[0].text` as `serde_json::to_string_pretty(payload)`, where
    // `payload` is the output of `build_tool_response` — a JSON object whose
    // `"data"` key holds the `SemanticSearchData { results: Vec<SearchHit> }`.
    //
    // Parse `content[0].text` and navigate `payload["data"]["results"]` to
    // assert a structural hit for `concurrent_target`, not a substring match.
    let content_json_str = tool_result
        .get("content")
        .and_then(|c| c.get(0))
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or_else(|| {
            panic!("tools/call result must have content[0].text; got: {tool_result:?}")
        });

    let content_val: serde_json::Value =
        serde_json::from_str(content_json_str).unwrap_or_else(|e| {
            panic!(
                "tools/call content[0].text is not valid JSON: {e}; \
                 got: {content_json_str:.300}"
            )
        });

    // Navigate the SearchHit array at payload["data"]["results"].
    // `build_tool_response` serialises `SemanticSearchData` under `"data"`.
    let found = content_val["data"]["results"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .any(|hit| hit["name"].as_str() == Some("concurrent_target"))
        })
        .unwrap_or(false);

    assert!(
        found,
        "semantic_search for 'concurrent_target' must return a SearchHit with \
         name == 'concurrent_target'; \
         data.results: {:?}",
        content_val["data"]["results"]
    );

    // ── 7. Drop connections; verify shim_registry drains to 0 ─────────────────
    drop(lsp_stream);
    drop(mcp_stream);

    // Poll until the registry empties (RAII deregistration is async).
    let drain_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if shim_registry.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < drain_deadline,
            "shim_registry must drain to 0 within 5s after all clients disconnect; \
             len={}",
            shim_registry.len()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        shim_registry.is_empty(),
        "shim_registry must be empty after all client connections drop"
    );

    // ── 8. Stop TestServer ─────────────────────────────────────────────────────
    server.stop().await;
}
