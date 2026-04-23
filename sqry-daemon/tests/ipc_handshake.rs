//! Task 8 Phase 8a — `daemon/hello` handshake integration tests.

mod support;

use support::ipc::{TestIpcClient, TestServer};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_hello_compatible_returns_envelope_version_1() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    let resp = client.hello(1).await;
    assert!(resp.compatible);
    assert_eq!(resp.envelope_version, sqry_daemon::ENVELOPE_VERSION);
    assert!(!resp.daemon_version.is_empty());
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_hello_incompatible_version_closes() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    let resp = client.hello(99).await;
    assert!(!resp.compatible);
    // Server closes immediately after reply — subsequent reads return
    // clean EOF via an empty buffer.
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_frame_not_daemon_hello_returns_invalid_request_and_closes() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    // Send a syntactically-valid JSON-RPC request as the first frame —
    // the server rejects it because the first frame MUST be a
    // DaemonHello handshake.
    let bogus = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "daemon/status",
    });
    client.send_raw(&bogus).await;
    let resp = client.read_response().await;
    match &resp.payload {
        sqry_daemon::ipc::protocol::JsonRpcPayload::Error { error } => {
            assert_eq!(error.code, -32600);
            assert!(resp.id.is_none(), "invalid-request id must be null");
        }
        other => panic!("expected -32600 error, got: {other:?}"),
    }
    drop(client);
    server.stop().await;
}
