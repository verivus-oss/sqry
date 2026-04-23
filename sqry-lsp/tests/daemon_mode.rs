//! Phase 8c U11 + U16 — integration tests for the `--daemon` / `--daemon-socket`
//! flag pair on the standalone `sqry-lsp` binary.
//!
//! # U11 tests (CLI flag parsing)
//!
//! Proves clap's `conflicts_with_all` + `requires` wiring on `LspCli` rejects
//! invalid combinations at parse time, BEFORE `sqry_lsp::run` is invoked. Uses
//! `<LspCli as Parser>::try_parse_from` (not `parse_from`) so the harness does
//! not call `std::process::exit` when clap reports an error.
//!
//! # U16 tests (end-to-end shim-client)
//!
//! Verifies the full wire path: `sqry_daemon_client::connect_shim_with_timeouts`
//! against a live `sqryd-test-server` child process. Tests the connect,
//! `ShimRegisterAck`, and initial LSP protocol exchange through the daemon's
//! `daemon_host::host_on_streams` byte-pump.
//!
//! The `sqryd-test-server` binary must be built before these tests run.
//! `cargo test --workspace` builds all workspace binaries automatically.
//! If the binary is absent (e.g. incremental `cargo test -p sqry-lsp` without
//! a prior `cargo build -p sqry-daemon`), the U16 tests skip gracefully via
//! the `DaemonFixture::start` returning `None`.

mod common;

use clap::Parser;
use sqry_lsp::LspCli;

/// `--daemon` must be mutually exclusive with `--stdio`: they both
/// claim ownership of process stdio, so allowing both would race for
/// the same file descriptors at runtime. Clap's
/// `conflicts_with_all = ["stdio", "socket"]` surfaces the conflict at
/// parse time with a `ErrorKind::ArgumentConflict`.
#[test]
fn daemon_flag_conflicts_with_stdio() {
    let result = LspCli::try_parse_from(["sqry-lsp", "--daemon", "--stdio"]);
    let err = result.expect_err("--daemon + --stdio must be rejected");
    assert_eq!(
        err.kind(),
        clap::error::ErrorKind::ArgumentConflict,
        "clap should raise ArgumentConflict, got: {err}"
    );
}

/// `--daemon` must be mutually exclusive with `--socket <ADDR>` (the
/// TCP-bind flag): the daemon-client path does not bind any local
/// socket, and attempting both would surface as a confusing runtime
/// error deep inside tokio.
#[test]
fn daemon_flag_conflicts_with_socket() {
    let result = LspCli::try_parse_from(["sqry-lsp", "--daemon", "--socket", "127.0.0.1:9999"]);
    let err = result.expect_err("--daemon + --socket must be rejected");
    assert_eq!(
        err.kind(),
        clap::error::ErrorKind::ArgumentConflict,
        "clap should raise ArgumentConflict, got: {err}"
    );
}

/// `--daemon-socket <PATH>` is pointless without `--daemon` — clap's
/// `requires = "daemon"` attribute surfaces this at parse time with
/// `ErrorKind::MissingRequiredArgument`, so the user gets a crisp
/// "--daemon-socket requires --daemon" message rather than a silent
/// no-op where the path is parsed but never consumed.
#[test]
fn daemon_socket_requires_daemon() {
    let result = LspCli::try_parse_from(["sqry-lsp", "--daemon-socket", "/tmp/ignored.sock"]);
    let err = result.expect_err("--daemon-socket alone must be rejected");
    assert_eq!(
        err.kind(),
        clap::error::ErrorKind::MissingRequiredArgument,
        "clap should raise MissingRequiredArgument, got: {err}"
    );
}

/// Positive case — `--daemon` alone parses successfully and the
/// resulting `LspOptions` signals client mode with no override path
/// (so `resolve_daemon_socket` will fall through to SQRYD_SOCKET or
/// the platform default at runtime).
#[test]
fn daemon_flag_parses_cleanly_without_socket_override() {
    let cli = LspCli::try_parse_from(["sqry-lsp", "--daemon"]).expect("--daemon alone is valid");
    let options = cli.into_options();
    assert!(options.daemon, "daemon flag should be set");
    assert!(options.daemon_socket.is_none(), "no override path supplied");
}

/// Positive case — `--daemon --daemon-socket <PATH>` parses and
/// threads the path through `LspCli::into_options` unchanged.
#[test]
fn daemon_flag_with_explicit_socket_path_parses() {
    let cli = LspCli::try_parse_from([
        "sqry-lsp",
        "--daemon",
        "--daemon-socket",
        "/custom/sqryd.sock",
    ])
    .expect("--daemon + --daemon-socket is valid");
    let options = cli.into_options();
    assert!(options.daemon);
    assert_eq!(
        options.daemon_socket.as_deref(),
        Some(std::path::Path::new("/custom/sqryd.sock"))
    );
}

// ─── U16: end-to-end shim-client tests ───────────────────────────────────────
//
// The tests below require a running sqryd-test-server child process. They use
// `DaemonFixture::start()` which spawns the binary and waits for it to bind
// the socket. If the binary is absent (not yet built) the fixture returns
// `None` and the test is skipped with a descriptive message — this preserves
// CI green when running crate-scoped tests before a full workspace build.
//
// The end-to-end flow exercised:
//   sqry_daemon_client::connect_shim → ShimRegister frame → ShimRegisterAck
//   → raw LSP byte-pump → tower_lsp `initialize` round-trip inside the daemon

/// U16 test 1: `connects_and_initializes`
///
/// `connect_shim_with_timeouts` succeeds against a live daemon, returns an
/// owned `ShimConnection`, and the shim LSP protocol can exchange an
/// `initialize` request through the daemon's `host_on_streams` byte-pump.
///
/// Wire path verified:
///   1. UDS connect.
///   2. ShimRegister → ShimRegisterAck (accepted=true).
///   3. ShimConnection::into_stream → LSP Content-Length frame → initialize
///      response: header parsed for exact content-length, body parsed as JSON,
///      `id` field matches 1, `result.capabilities` is an object.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connects_and_initializes() {
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    let Some(fixture) = common::DaemonFixture::start() else {
        eprintln!(
            "SKIP connects_and_initializes: sqryd-test-server binary not found \
             (run `cargo build -p sqry-daemon` first)"
        );
        return;
    };

    // Step 1: connect via sqry_daemon_client.
    let conn = sqry_daemon_client::connect_shim_with_timeouts(
        &fixture.socket_path,
        sqry_daemon_client::ShimProtocol::Lsp,
        std::process::id(),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await
    .expect("connect_shim must succeed against live sqryd-test-server");

    assert!(
        !conn.daemon_version().is_empty(),
        "daemon_version must be non-empty in ShimRegisterAck"
    );

    // Step 2: extract the raw stream and send an LSP `initialize` frame.
    let mut stream = conn.into_stream();
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
    let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stream
        .write_all(frame.as_bytes())
        .await
        .expect("write LSP initialize frame");
    stream.flush().await.expect("flush");

    // Step 3: read the complete LSP response.
    //
    // LSP uses HTTP-style Content-Length framing:
    //   "Content-Length: <N>\r\n\r\n<JSON body of exactly N bytes>"
    //
    // We accumulate bytes until we have the full header block + body,
    // then parse the header to extract N and parse the JSON body.
    let raw = read_lsp_message(&mut stream, Duration::from_secs(5)).await;

    // Parse the Content-Length header from the raw bytes.
    let header_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("LSP response must contain \\r\\n\\r\\n header separator");
    let header_block =
        std::str::from_utf8(&raw[..header_end]).expect("LSP header must be valid UTF-8");
    let content_length: usize = header_block
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split_once(':').map(|x| x.1))
        .and_then(|v| v.trim().parse().ok())
        .expect("LSP response must contain a valid Content-Length header");

    let body_start = header_end + 4;
    let body_bytes = &raw[body_start..];
    assert_eq!(
        body_bytes.len(),
        content_length,
        "LSP response body length must match Content-Length header: \
         header says {content_length}, got {} bytes",
        body_bytes.len()
    );

    // Parse the JSON body.
    let resp: serde_json::Value =
        serde_json::from_slice(body_bytes).expect("LSP response body must be valid JSON");

    assert_eq!(
        resp["id"], 1,
        "response must echo request id=1; got: {resp}"
    );
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

/// Read a complete LSP message (Content-Length framed) from `stream` within
/// `timeout`. Accumulates bytes until the full header + body is present, then
/// returns the raw bytes.
#[cfg(unix)]
async fn read_lsp_message(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    timeout: std::time::Duration,
) -> Vec<u8> {
    use tokio::io::AsyncReadExt;

    let mut buf = Vec::with_capacity(8192);
    let mut tmp = vec![0u8; 4096];

    let read_fut = async {
        loop {
            let n = stream.read(&mut tmp).await.expect("read LSP bytes");
            assert!(n > 0, "LSP stream closed before complete message");
            buf.extend_from_slice(&tmp[..n]);

            // Look for the header/body separator.
            if let Some(sep) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                // Parse Content-Length from the accumulated header.
                let header = std::str::from_utf8(&buf[..sep]).unwrap_or("");
                let cl: Option<usize> = header
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
                    .and_then(|l| l.split_once(':').map(|x| x.1))
                    .and_then(|v| v.trim().parse().ok());

                if let Some(content_length) = cl {
                    let body_start = sep + 4;
                    if buf.len() >= body_start + content_length {
                        // Full message received.
                        buf.truncate(body_start + content_length);
                        return buf;
                    }
                    // Keep reading until full body arrives.
                }
            }
        }
    };

    tokio::time::timeout(timeout, read_fut)
        .await
        .expect("LSP initialize response within timeout")
}

/// U16 test 2: `daemon_unreachable_bails`
///
/// When the socket path does not exist, `connect_shim_with_timeouts` returns
/// `ClientError::Connect` (not a timeout — the OS `connect(2)` fails
/// immediately with ENOENT / ECONNREFUSED when the file is absent).
#[cfg(unix)]
#[tokio::test]
async fn daemon_unreachable_bails() {
    use std::time::Duration;

    let nonexistent = std::path::Path::new("/tmp/sqry-u16-lsp-nonexistent-99999999.sock");
    let err = sqry_daemon_client::connect_shim_with_timeouts(
        nonexistent,
        sqry_daemon_client::ShimProtocol::Lsp,
        std::process::id(),
        Duration::from_secs(2),
        Duration::from_secs(2),
    )
    .await
    .expect_err("connect to non-existent socket must fail");

    // Should be a Connect error (ENOENT / ECONNREFUSED), not a timeout.
    match &err {
        sqry_daemon_client::ClientError::Connect { path, .. } => {
            assert_eq!(
                path, nonexistent,
                "error must cite the socket path we attempted"
            );
        }
        sqry_daemon_client::ClientError::ConnectTimeout { .. } => {
            // Also acceptable: some platforms may surface this as a timeout
            // if the OS is slow to reply. Both variants indicate unreachable.
        }
        other => panic!(
            "expected Connect or ConnectTimeout, got: {other:?}. \
             A non-existent socket path must produce a connect-level error."
        ),
    }
}

/// U16 test 3: `daemon_rejects_shim_register_bails`
///
/// When the daemon has reached its `max_shim_connections` cap, it sends
/// `ShimRegisterAck { accepted: false, reason: Some("...") }`, and
/// `connect_shim_with_timeouts` returns `ClientError::ShimRejected`.
///
/// The admission counter is incremented atomically under lock before the ack
/// is sent, so once the first connection has been confirmed accepted (the
/// `connect_shim_with_timeouts` future resolved with `Ok`), the cap slot is
/// taken and the second connection must deterministically receive `ShimRejected`.
///
/// Implementation: set `max_shim_connections = 1` via
/// `SQRY_DAEMON_MAX_SHIM_CONNECTIONS=1`. Open the first connection, wait 100ms
/// for the admission record to be written, then attempt a second.
///
/// The READY wait is done via the `wait_for_ready` helper from the fixture
/// module (background thread + channel) so a stalling child cannot hang forever.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_rejects_shim_register_bails() {
    use std::time::Duration;

    let Some(binary) = common::daemon_fixture::find_sqryd_test_server_binary() else {
        eprintln!("SKIP daemon_rejects_shim_register_bails: sqryd-test-server binary not found");
        return;
    };

    let tmp = tempfile::TempDir::new().expect("tempdir");
    let socket_path = tmp.path().join("sqryd-cap1.sock");

    // Spawn the test-server with cap=1 so the second connection is rejected.
    let mut child = std::process::Command::new(&binary)
        .env("SQRYD_TEST_SOCKET", &socket_path)
        .env("SQRY_DAEMON_MAX_SHIM_CONNECTIONS", "1")
        .env("SQRYD_TEST_LOG", "warn")
        // Inherit stderr for CI diagnostics; capture stdout for READY handshake.
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn sqryd-test-server (cap=1): {e}"));

    // Use the bounded READY wait (background thread + channel) so a stalling
    // child process panics within the timeout rather than hanging forever.
    let stdout = child.stdout.take().expect("child stdout");
    let _reader =
        common::daemon_fixture::wait_for_ready_pub(stdout, Duration::from_secs(5), &socket_path);

    // Wait for socket to appear.
    {
        let sock_deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if socket_path.exists() {
                break;
            }
            if std::time::Instant::now() >= sock_deadline {
                panic!("socket never appeared (cap=1 fixture)");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    // First connection: must be accepted (cap=1, slot free).
    let first = sqry_daemon_client::connect_shim_with_timeouts(
        &socket_path,
        sqry_daemon_client::ShimProtocol::Lsp,
        std::process::id(),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await
    .expect("first connection must be accepted (cap=1, slot free)");

    // The admission counter is incremented atomically before the ack is sent.
    // A short settle ensures the first slot is fully recorded before we knock.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Second connection: must be rejected — cap is exhausted.
    let second_result = sqry_daemon_client::connect_shim_with_timeouts(
        &socket_path,
        sqry_daemon_client::ShimProtocol::Lsp,
        std::process::id(),
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .await;

    // Release the first connection so the child can drain cleanly.
    drop(first);
    let _ = child.kill();
    let _ = child.wait();

    match second_result {
        Err(sqry_daemon_client::ClientError::ShimRejected(reason)) => {
            // Expected path: cap=1 is exhausted, daemon sends
            // ShimRegisterAck { accepted: false, reason: "..." }.
            assert!(
                !reason.is_empty(),
                "rejection reason must be non-empty: got {reason:?}"
            );
        }
        Ok(_conn) => {
            panic!(
                "second connection was accepted despite cap=1. \
                 The server-side admission counter must be incremented before \
                 the ack is sent, so the second connection must be rejected."
            );
        }
        Err(other) => {
            panic!("expected ShimRejected (cap exceeded), got unexpected error: {other:?}");
        }
    }
}
