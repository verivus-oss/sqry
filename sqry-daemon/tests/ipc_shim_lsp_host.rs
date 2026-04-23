//! Phase 8c U15 — LSP host integration tests (§K, design iter-2).
//!
//! Each test exercises `sqry_lsp::daemon_host::host_on_streams` via the
//! full IPC path: `TestServer` accept loop → `run_connection` →
//! `run_shim_connection` → `host_on_streams`. Streams are `UnixStream`
//! halves handed over after the shim handshake.
//!
//! ## Test coverage
//!
//! 1. `lsp_host_serves_initialize_via_bytepump` — LSP `initialize`
//!    round-trips through the byte-pump; checks a valid LSP response.
//! 2. `lsp_host_raii_deregisters_on_client_disconnect` — after the
//!    client drops the connection, the `ShimRegistry` entry is gone.
//! 3. `lsp_host_cancellation_token_cuts_mid_request` — the server's
//!    cancellation token terminates the host; the registry drains.
//! 4. `two_concurrent_lsp_shims_isolated` — two simultaneous LSP shim
//!    connections both get independent `initialize` responses.

#![allow(clippy::too_many_lines)]

mod support;

use std::time::Duration;

use sqry_daemon::ipc::framing::{read_frame_json, write_frame_json};
use sqry_daemon_protocol::{ShimProtocol, ShimRegister, ShimRegisterAck};
use support::ipc::TestServer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

// ---------------------------------------------------------------------------
// Helper: LSP content-length framing
// ---------------------------------------------------------------------------

/// Write an LSP `Content-Length` framed message to a writer.
async fn write_lsp_frame<W: AsyncWriteExt + Unpin>(writer: &mut W, body: &str) {
    let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    writer.write_all(frame.as_bytes()).await.unwrap();
}

/// Read the next LSP response frame (reads until the trailing newline of the
/// Content-Length header + body). Panics on EOF or timeout.
async fn read_lsp_response<R: AsyncReadExt + Unpin>(reader: &mut R) -> String {
    let mut buf = vec![0u8; 65536];
    let n = tokio::time::timeout(Duration::from_secs(5), reader.read(&mut buf))
        .await
        .expect("LSP read timeout")
        .expect("LSP read error");
    assert!(n > 0, "expected LSP response bytes, got EOF");
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

// ---------------------------------------------------------------------------
// Helper: connect a shim of the given protocol and return the raw stream
// halves (AFTER the ShimRegisterAck has been consumed).
// ---------------------------------------------------------------------------

async fn connect_lsp_shim(
    server: &TestServer,
) -> (
    tokio::io::ReadHalf<UnixStream>,
    tokio::io::WriteHalf<UnixStream>,
) {
    let stream = UnixStream::connect(&server.path).await.expect("connect");
    let (mut rh, mut wh) = tokio::io::split(stream);

    let shim_reg = ShimRegister {
        protocol: ShimProtocol::Lsp,
        pid: std::process::id(),
    };
    write_frame_json(&mut wh, &shim_reg)
        .await
        .expect("write ShimRegister");

    let ack = read_frame_json::<_, ShimRegisterAck>(&mut rh)
        .await
        .expect("read ack")
        .expect("ack frame");
    assert!(
        ack.accepted,
        "ack must be accepted; reason={:?}",
        ack.reason
    );

    (rh, wh)
}

// ---------------------------------------------------------------------------
// Test 1: lsp_host_serves_initialize_via_bytepump
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lsp_host_serves_initialize_via_bytepump() {
    let server = TestServer::new().await;

    let (mut rh, mut wh) = connect_lsp_shim(&server).await;

    // Send LSP `initialize` request.
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
    write_lsp_frame(&mut wh, &body).await;

    // Read the response from the server.
    let response_raw = read_lsp_response(&mut rh).await;

    // Assert LSP framing is present.
    assert!(
        response_raw.contains("Content-Length:"),
        "response must be LSP Content-Length framed; got: {response_raw:.200}"
    );
    assert!(
        response_raw.contains("\"jsonrpc\":\"2.0\""),
        "response must be JSON-RPC 2.0; got: {response_raw:.200}"
    );
    assert!(
        response_raw.contains("\"id\":1"),
        "response must echo request id=1; got: {response_raw:.200}"
    );
    // tower_lsp responds to `initialize` with `result.capabilities`.
    assert!(
        response_raw.contains("capabilities") || response_raw.contains("result"),
        "initialize response must contain capabilities or result; got: {response_raw:.200}"
    );

    drop(wh); // client disconnect
    // Allow registry drain.
    tokio::time::sleep(Duration::from_millis(100)).await;
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 2: lsp_host_raii_deregisters_on_client_disconnect
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lsp_host_raii_deregisters_on_client_disconnect() {
    let server = TestServer::new().await;
    let registry = server.shim_registry();

    // Confirm empty before connection.
    assert!(
        registry.is_empty(),
        "registry must be empty before connecting"
    );

    let (rh, wh) = connect_lsp_shim(&server).await;

    // After handshake, registry has 1 entry.
    // Give the server task a moment to register.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        registry.len(),
        1,
        "registry must have 1 entry after LSP shim connected"
    );

    // Drop both halves — this closes the connection.
    drop(rh);
    drop(wh);

    // Wait for the RAII drop to deregister.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if registry.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "registry must deregister within 3s of client disconnect; len={}",
            registry.len()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert!(
        registry.is_empty(),
        "registry must be empty after client disconnects"
    );

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 3: lsp_host_cancellation_token_cuts_mid_request
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lsp_host_cancellation_token_cuts_mid_request() {
    let server = TestServer::new().await;
    let registry = server.shim_registry();

    let (rh, wh) = connect_lsp_shim(&server).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Registry should have 1 entry.
    assert_eq!(
        registry.len(),
        1,
        "registry must have 1 entry after connect"
    );

    // Fire the server shutdown token.
    server.shutdown.cancel();

    // The host should drain; the registry should empty within
    // `ipc_shutdown_drain_secs` (default 30s, but the test observes
    // host termination which is faster).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if registry.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "registry must deregister within 5s of cancellation; len={}",
            registry.len()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert!(
        registry.is_empty(),
        "registry must be empty after shutdown cancellation"
    );

    // Drop the client halves (server may have already closed the stream).
    drop(rh);
    drop(wh);

    // Wait for server to finish — it already received the cancel.
    let _ = tokio::time::timeout(Duration::from_secs(5), server.handle).await;
}

// ---------------------------------------------------------------------------
// Test 4: two_concurrent_lsp_shims_isolated
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_concurrent_lsp_shims_isolated() {
    let server = TestServer::new().await;

    // Connect two LSP shims concurrently.
    let (mut rh1, mut wh1) = connect_lsp_shim(&server).await;
    let (mut rh2, mut wh2) = connect_lsp_shim(&server).await;

    // Send `initialize` on both connections with different IDs.
    let body1 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 101,
        "method": "initialize",
        "params": { "processId": null, "rootUri": null, "capabilities": {} }
    })
    .to_string();
    let body2 = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 202,
        "method": "initialize",
        "params": { "processId": null, "rootUri": null, "capabilities": {} }
    })
    .to_string();

    write_lsp_frame(&mut wh1, &body1).await;
    write_lsp_frame(&mut wh2, &body2).await;

    // Read both responses.
    let resp1 = read_lsp_response(&mut rh1).await;
    let resp2 = read_lsp_response(&mut rh2).await;

    // Each response must echo its own request ID.
    assert!(
        resp1.contains("\"id\":101"),
        "connection 1 response must echo id=101; got: {resp1:.200}"
    );
    assert!(
        resp2.contains("\"id\":202"),
        "connection 2 response must echo id=202; got: {resp2:.200}"
    );

    // Both are LSP-framed (Content-Length header present).
    assert!(
        resp1.contains("Content-Length:"),
        "response 1 must be LSP-framed"
    );
    assert!(
        resp2.contains("Content-Length:"),
        "response 2 must be LSP-framed"
    );

    // Responses must not cross-contaminate (connection 1 should NOT see
    // id=202 in its first response frame, and vice versa).
    // Note: we can only assert on the FIRST frame we read; later frames
    // (notifications) may come asynchronously. The ID check above is
    // the authoritative isolation test.
    assert!(
        !resp1.contains("\"id\":202"),
        "connection 1 response must not contain id=202 (cross-contamination)"
    );

    drop(wh1);
    drop(wh2);
    tokio::time::sleep(Duration::from_millis(100)).await;
    server.stop().await;
}
