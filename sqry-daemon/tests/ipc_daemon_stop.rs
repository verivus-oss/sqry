//! Task 8 Phase 8a — `daemon/stop` integration tests.

mod support;

use serde_json::json;
use support::ipc::{TestIpcClient, TestServer, expect_success};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_stop_cancels_shutdown_token() {
    let server = TestServer::new().await;
    let shutdown = server.shutdown.clone();
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let resp = client.request("daemon/stop", json!({})).await;
    let result = expect_success(&resp);
    assert_eq!(result["result"]["scheduled"], json!(true));
    assert!(shutdown.is_cancelled(), "stop must flip the shutdown token");
    drop(client);
    // Server future should now drain and return Ok.
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_stop_responds_before_drain() {
    // Verify the response lands on the wire before the server
    // completes shutdown — clients must not block waiting for drain.
    let server = TestServer::new().await;
    let path = server.path.clone();
    let mut client = TestIpcClient::connect(&path).await;
    client.hello(1).await;
    let resp = client.request("daemon/stop", json!({})).await;
    let _ = expect_success(&resp);
    // At this point the response was read successfully; the drain
    // phase may still be in progress. Subsequent stop() awaits full
    // drain without error.
    drop(client);
    server.stop().await;
}
