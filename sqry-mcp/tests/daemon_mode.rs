//! Phase 8c U12 + U16 — integration tests for the `--daemon` / `--daemon-socket`
//! flag pair, socket-resolution logic of the `sqry-mcp` binary, and end-to-end
//! shim-client tests against a live sqryd daemon fixture.
//!
//! # Scope
//!
//! Tests the daemon-argument parser (`sqry_mcp::daemon_shim::parse_daemon_args`)
//! and socket resolver (`sqry_mcp::daemon_shim::resolve_daemon_socket`) that
//! back the `--daemon` / `--daemon-socket` flags (Phase 8c U12).
//!
//! - Verifies that valid flag combinations produce the expected
//!   [`DaemonParseResult`] variants.
//! - Verifies that invalid combinations (e.g. `--daemon-socket` without
//!   `--daemon`) are rejected with the `MissingDaemon` variant.
//! - Verifies `resolve_daemon_socket` precedence: explicit override → env var
//!   → platform default.
//!
//! # Mirror-symmetry with U11 (`sqry-lsp`)
//!
//! U11 (`sqry-lsp/tests/daemon_mode.rs`) provides 5 tests via clap's
//! `try_parse_from`. This file provides 6 integration tests via
//! `parse_daemon_args` (the manual-parser equivalent). The `daemon_shim`
//! module also provides 10 unit tests covering the full arg-parsing and
//! socket-resolution surface.
//!
//! # Divergences from U11 (all justified by the manual-parser design)
//!
//! - U11 uses clap `ErrorKind::ArgumentConflict` / `MissingRequiredArgument`;
//!   U12 uses `DaemonParseResult::MissingDaemon` / `MissingSocketPath`.
//! - U11 asserts `LspOptions.daemon_socket`; U12 asserts
//!   `DaemonParseResult::Daemon { socket }`.
//! - U12 exposes `parse_daemon_args` + `resolve_daemon_socket` through the
//!   lib crate target for direct integration-test access; U11 uses clap's
//!   built-in introspection on the struct fields.
//!
//! # Safety
//!
//! Tests that mutate `SQRYD_SOCKET` / `XDG_RUNTIME_DIR` call
//! `std::env::set_var` / `remove_var` inside `unsafe` blocks (required by
//! Edition 2024). Each mutation site is guarded by an `EnvGuard` RAII wrapper
//! that restores the prior value on drop. These tests should be run with
//! `RUST_TEST_THREADS=1` to avoid cross-test races when run in parallel.

mod common;

use serial_test::serial;
use sqry_mcp::daemon_shim::{DaemonParseResult, parse_daemon_args, resolve_daemon_socket};
use std::path::PathBuf;

// ─── Helper ──────────────────────────────────────────────────────────────────

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// RAII guard that restores an environment variable on drop.
///
/// `std::env::set_var` / `remove_var` require `unsafe` in Edition 2024 because
/// they are inherently thread-unsafe in multi-threaded programs. The `unsafe`
/// blocks below are intentional; their safety invariant is that these tests
/// run sequentially (`RUST_TEST_THREADS=1`) or that the mutated variables are
/// not read by other tests concurrently.
struct EnvGuard {
    key: &'static str,
    prior: Option<std::ffi::OsString>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: restoring a previously-read env var; intentional env mutation
        // in a test-only context that is guarded by the calling test's scope.
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

// ─── parse_daemon_args ────────────────────────────────────────────────────────

/// `--daemon` alone produces `Daemon { socket: None }`.
///
/// Mirror of U11 `daemon_flag_parses_cleanly_without_socket_override`.
#[test]
fn daemon_flag_alone_produces_daemon_no_socket() {
    let r = parse_daemon_args(&args(&["sqry-mcp", "--daemon"]));
    assert_eq!(r, DaemonParseResult::Daemon { socket: None });
}

/// `--daemon --daemon-socket <PATH>` produces `Daemon { socket: Some(..) }`.
///
/// Mirror of U11 `daemon_flag_with_explicit_socket_path_parses`.
#[test]
fn daemon_with_socket_path_parses() {
    let r = parse_daemon_args(&args(&[
        "sqry-mcp",
        "--daemon",
        "--daemon-socket",
        "/custom/sqryd.sock",
    ]));
    assert_eq!(
        r,
        DaemonParseResult::Daemon {
            socket: Some(PathBuf::from("/custom/sqryd.sock"))
        }
    );
}

/// `--daemon-socket <PATH>` without `--daemon` must be rejected.
///
/// Mirror of U11 `daemon_socket_requires_daemon`
/// (clap `ErrorKind::MissingRequiredArgument`).
#[test]
fn daemon_socket_without_daemon_is_rejected() {
    let r = parse_daemon_args(&args(&["sqry-mcp", "--daemon-socket", "/tmp/ignored.sock"]));
    assert_eq!(r, DaemonParseResult::MissingDaemon);
}

/// `--daemon-socket` with no following PATH is a parse error.
///
/// Mirror of U11's clap implicit handling; the manual parser must handle
/// gracefully rather than panicking on a missing array element.
#[test]
fn daemon_socket_flag_with_no_path_is_rejected() {
    let r = parse_daemon_args(&args(&["sqry-mcp", "--daemon", "--daemon-socket"]));
    assert_eq!(r, DaemonParseResult::MissingSocketPath);
}

/// `--daemon --daemon-socket` and `--daemon-socket --daemon` are both valid
/// (order-independent). Mirrors the ordering flexibility clap provides for U11.
#[test]
fn daemon_flags_are_order_independent() {
    let r = parse_daemon_args(&args(&[
        "sqry-mcp",
        "--daemon-socket",
        "/reorder.sock",
        "--daemon",
    ]));
    assert_eq!(
        r,
        DaemonParseResult::Daemon {
            socket: Some(PathBuf::from("/reorder.sock"))
        }
    );
}

/// `--daemon-socket --daemon` (next token is a flag, not a PATH) must return
/// `MissingSocketPath`, not `MissingDaemon` or `Daemon { socket: Some("--daemon") }`.
///
/// This is the regression test for the Codex iter-0 MAJOR finding: the parser
/// must reject flag tokens as PATH values for `--daemon-socket`.
#[test]
fn daemon_socket_with_flag_as_next_token_is_missing_path() {
    // `--daemon-socket --daemon` — next token is a flag, not a path.
    // Before the fix, this would consume `--daemon` as the socket path and
    // then return MissingDaemon because has_daemon remained false.
    let r = parse_daemon_args(&args(&["sqry-mcp", "--daemon-socket", "--daemon"]));
    assert_eq!(
        r,
        DaemonParseResult::MissingSocketPath,
        "--daemon-socket followed by a flag token must be MissingSocketPath"
    );

    // `--daemon --daemon-socket --help` — socket path looks like a help flag.
    let r2 = parse_daemon_args(&args(&[
        "sqry-mcp",
        "--daemon",
        "--daemon-socket",
        "--help",
    ]));
    assert_eq!(
        r2,
        DaemonParseResult::MissingSocketPath,
        "--daemon-socket followed by --help must be MissingSocketPath"
    );
}

/// No daemon flags → `NotDaemonMode` (normal server or other CLI path).
///
/// Mirror of the implicit negative case in U11 where the default parse
/// is not `--daemon`.
#[test]
fn no_daemon_flags_returns_not_daemon_mode() {
    assert_eq!(
        parse_daemon_args(&args(&["sqry-mcp"])),
        DaemonParseResult::NotDaemonMode
    );
    assert_eq!(
        parse_daemon_args(&args(&["sqry-mcp", "--help"])),
        DaemonParseResult::NotDaemonMode
    );
    assert_eq!(
        parse_daemon_args(&args(&["sqry-mcp", "--list-tools"])),
        DaemonParseResult::NotDaemonMode
    );
}

// ─── resolve_daemon_socket ────────────────────────────────────────────────────

/// Explicit `--daemon-socket <PATH>` override must always win.
///
/// Mirror of U11's implicit precedence: when `LspOptions.daemon_socket` is
/// `Some`, that path is passed directly to `resolve_daemon_socket`.
#[test]
fn resolve_explicit_override_wins_over_everything() {
    let explicit = std::path::Path::new("/explicit/sqryd.sock");
    let result = resolve_daemon_socket(Some(explicit));
    assert_eq!(result, PathBuf::from("/explicit/sqryd.sock"));
}

/// `$SQRYD_SOCKET` env var wins over the platform default when no explicit
/// override is supplied.
#[serial]
#[test]
fn resolve_env_var_wins_over_platform_default() {
    let prior = std::env::var_os("SQRYD_SOCKET");
    let _guard = EnvGuard {
        key: "SQRYD_SOCKET",
        prior,
    };
    // SAFETY: test-only env mutation; restoring on drop via EnvGuard.
    unsafe { std::env::set_var("SQRYD_SOCKET", "/env/sqryd.sock") };

    let result = resolve_daemon_socket(None);
    assert_eq!(
        result,
        PathBuf::from("/env/sqryd.sock"),
        "SQRYD_SOCKET env var should take precedence over platform default"
    );
}

/// `$XDG_RUNTIME_DIR/sqry/sqryd.sock` is used on Unix when set and no
/// explicit override or `SQRYD_SOCKET` is present.
///
/// Mirrors `sqry-daemon::DaemonConfig::socket_path()` XDG branch.
#[cfg(unix)]
#[serial]
#[test]
fn resolve_xdg_runtime_dir_used_on_unix() {
    let prior_socket = std::env::var_os("SQRYD_SOCKET");
    let prior_xdg = std::env::var_os("XDG_RUNTIME_DIR");
    let _g1 = EnvGuard {
        key: "SQRYD_SOCKET",
        prior: prior_socket,
    };
    let _g2 = EnvGuard {
        key: "XDG_RUNTIME_DIR",
        prior: prior_xdg,
    };
    // SAFETY: test-only env mutation; restoring on drop via EnvGuards.
    unsafe {
        std::env::remove_var("SQRYD_SOCKET");
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1234");
    }

    let result = resolve_daemon_socket(None);
    assert_eq!(
        result,
        PathBuf::from("/run/user/1234/sqry/sqryd.sock"),
        "XDG_RUNTIME_DIR should be used as the socket base on unix"
    );
}

// ─── U16: end-to-end shim-client tests (sqry-mcp) ────────────────────────────
//
// These tests exercise the full wire path for MCP shim connections:
//   sqry_daemon_client::connect_shim → ShimProtocol::Mcp →
//   ShimRegisterAck → byte-pump → rmcp MCP protocol exchange inside the daemon.
//
// The `sqryd-test-server` binary must be built before these tests run. If
// absent, each test skips gracefully by returning early when
// `DaemonFixture::start()` returns `None`.

/// U16 test 1 (MCP): `connects_and_initializes`
///
/// `connect_shim_with_timeouts` with `ShimProtocol::Mcp` succeeds against a
/// live daemon, returns an owned `ShimConnection`, and the MCP `initialize`
/// request round-trips through the daemon's `mcp_host::host_mcp_on_streams`
/// byte-pump.
///
/// Wire path verified:
///   1. UDS connect.
///   2. ShimRegister { protocol: Mcp } → ShimRegisterAck (accepted=true).
///   3. ShimConnection::into_stream → rmcp newline-delimited JSON-RPC MCP
///      `initialize` request → response parsed as JSON, `id` == 1,
///      `result.capabilities` is an object.
///
/// The MCP response is read by accumulating lines until we find one whose
/// parsed `id` field equals 1 (the server may emit notifications before the
/// response, so we scan rather than assuming the first line is ours).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connects_and_initializes() {
    use std::time::Duration;
    use tokio::io::{AsyncWriteExt, BufReader};

    let Some(fixture) = common::DaemonFixture::start() else {
        eprintln!("SKIP mcp::connects_and_initializes: sqryd-test-server binary not found");
        return;
    };

    // Step 1: connect with MCP protocol.
    let conn = sqry_daemon_client::connect_shim_with_timeouts(
        &fixture.socket_path,
        sqry_daemon_client::ShimProtocol::Mcp,
        std::process::id(),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await
    .expect("MCP connect_shim must succeed against live sqryd-test-server");

    assert!(
        !conn.daemon_version().is_empty(),
        "daemon_version must be non-empty in ShimRegisterAck"
    );

    // Step 2: send an MCP `initialize` request (newline-delimited JSON-RPC).
    let mut stream = BufReader::new(conn.into_stream());
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "u16-test", "version": "0.0" }
        }
    })
    .to_string();
    let msg = format!("{init_req}\n");
    stream
        .get_mut()
        .write_all(msg.as_bytes())
        .await
        .expect("write MCP initialize request");
    stream.get_mut().flush().await.expect("flush");

    // Step 3: read newline-delimited JSON lines until we find the response
    // with id=1. The server may emit a notification (e.g. progress) before
    // the initialize response, so we scan all lines rather than assuming the
    // first line is ours.
    let resp = read_mcp_response_with_id(&mut stream, 1, Duration::from_secs(5)).await;

    let result = &resp["result"];
    assert!(
        !result.is_null(),
        "initialize response must have a result object; got: {resp}"
    );
    assert!(
        result["capabilities"].is_object(),
        "initialize result.capabilities must be an object; got result: {result}"
    );
}

/// Read newline-delimited MCP JSON-RPC lines until one has `id == expected_id`.
/// Returns the parsed JSON of that line. Panics on timeout or EOF.
#[cfg(unix)]
async fn read_mcp_response_with_id(
    stream: &mut tokio::io::BufReader<impl tokio::io::AsyncRead + Unpin>,
    expected_id: i64,
    timeout: std::time::Duration,
) -> serde_json::Value {
    use tokio::io::AsyncBufReadExt;

    let read_fut = async {
        let mut line = String::new();
        loop {
            line.clear();
            let n = stream
                .read_line(&mut line)
                .await
                .expect("read MCP response line");
            assert!(
                n > 0,
                "MCP stream closed before receiving id={expected_id} response"
            );
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let msg: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
                panic!("MCP response must be valid JSON: {e}; got: {trimmed:.300}")
            });
            if msg["id"] == expected_id {
                return msg;
            }
            // Notification or response for a different id — skip and keep reading.
        }
    };

    tokio::time::timeout(timeout, read_fut)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "MCP initialize response (id={expected_id}) not received within {}s",
                timeout.as_secs()
            )
        })
}

/// U16 test 2 (MCP): `daemon_unreachable_bails`
///
/// When the socket path does not exist, `connect_shim_with_timeouts` with
/// `ShimProtocol::Mcp` returns `ClientError::Connect`.
#[cfg(unix)]
#[tokio::test]
async fn daemon_unreachable_bails() {
    use std::time::Duration;

    let nonexistent = std::path::Path::new("/tmp/sqry-u16-mcp-nonexistent-99999999.sock");
    let err = sqry_daemon_client::connect_shim_with_timeouts(
        nonexistent,
        sqry_daemon_client::ShimProtocol::Mcp,
        std::process::id(),
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .await
    .expect_err("connect to non-existent socket must fail");

    match &err {
        sqry_daemon_client::ClientError::Connect { path, .. } => {
            assert_eq!(
                path, nonexistent,
                "error must cite the socket path we attempted"
            );
        }
        sqry_daemon_client::ClientError::ConnectTimeout { .. } => {
            // Also acceptable.
        }
        other => panic!("expected Connect or ConnectTimeout, got: {other:?}"),
    }
}

/// U16 test 3 (MCP): `shim_rejected_on_cap_exceeded_bails`
///
/// When the daemon has reached its `max_shim_connections` cap, the second MCP
/// shim connection is rejected with `ClientError::ShimRejected`.
///
/// The admission counter is incremented atomically before the ack is sent, so
/// once the first connection is confirmed accepted, the second connection must
/// deterministically receive `ShimRejected`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shim_rejected_on_cap_exceeded_bails() {
    use std::time::Duration;

    let Some(binary) = common::daemon_fixture::find_sqryd_test_server_binary() else {
        eprintln!("SKIP shim_rejected_on_cap_exceeded_bails: sqryd-test-server binary not found");
        return;
    };

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket_path = tmp.path().join("sqryd-mcp-cap1.sock");

    let mut child = std::process::Command::new(&binary)
        .env("SQRYD_TEST_SOCKET", &socket_path)
        .env("SQRY_DAEMON_MAX_SHIM_CONNECTIONS", "1")
        .env("SQRYD_TEST_LOG", "warn")
        // Inherit stderr for CI diagnostics; capture stdout for READY handshake.
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn sqryd-test-server (mcp cap=1): {e}"));

    // Use the bounded READY wait (background thread + channel).
    let stdout = child.stdout.take().expect("child stdout");
    let _reader =
        common::daemon_fixture::wait_for_ready_pub(stdout, Duration::from_secs(5), &socket_path);

    {
        let sock_deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if socket_path.exists() {
                break;
            }
            if std::time::Instant::now() >= sock_deadline {
                panic!("socket never appeared (mcp cap=1 fixture)");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // First connection: must be accepted (cap=1, slot free).
    let first = sqry_daemon_client::connect_shim_with_timeouts(
        &socket_path,
        sqry_daemon_client::ShimProtocol::Mcp,
        std::process::id(),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await
    .expect("first MCP shim connection must be accepted (cap=1, slot free)");

    // Admission counter is incremented atomically before the ack is sent.
    // A short settle ensures the first slot is fully recorded.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second connection: must be rejected (cap exhausted).
    let second_result = sqry_daemon_client::connect_shim_with_timeouts(
        &socket_path,
        sqry_daemon_client::ShimProtocol::Mcp,
        std::process::id(),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await;

    drop(first);
    let _ = child.kill();
    let _ = child.wait();

    match second_result {
        Err(sqry_daemon_client::ClientError::ShimRejected(reason)) => {
            assert!(
                !reason.is_empty(),
                "rejection reason must be non-empty: got {reason:?}"
            );
        }
        Ok(_conn) => {
            panic!(
                "second MCP connection was accepted despite cap=1. \
                 The server-side admission counter must be incremented before \
                 the ack is sent, so the second connection must be rejected."
            );
        }
        Err(other) => {
            panic!("expected ShimRejected (cap exceeded), got: {other:?}");
        }
    }
}
