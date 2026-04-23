//! Task 8 Phase 8a — JSON-RPC 2.0 §6 batch dispatch tests.

mod support;

use serde_json::{Value, json};
use support::ipc::{TestIpcClient, TestServer};

async fn send_batch(server: &TestServer, batch: &Value) -> Value {
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let body = batch.to_string();
    client.send_raw_bytes(body.as_bytes()).await;
    // Read the next framed value as raw Value so we can distinguish
    // a single response object from a batch array.
    client.read_typed::<Value>().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_of_three_requests_returns_three_responses() {
    let server = TestServer::new().await;
    let batch = json!([
        {"jsonrpc":"2.0","id":1,"method":"daemon/status"},
        {"jsonrpc":"2.0","id":2,"method":"daemon/status"},
        {"jsonrpc":"2.0","id":3,"method":"daemon/status"},
    ]);
    let resp = send_batch(&server, &batch).await;
    let arr = resp.as_array().expect("batch response array");
    assert_eq!(arr.len(), 3);
    let ids: Vec<_> = arr.iter().filter_map(|r| r.get("id").cloned()).collect();
    assert!(ids.contains(&json!(1)));
    assert!(ids.contains(&json!(2)));
    assert!(ids.contains(&json!(3)));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_with_two_calls_one_notification_returns_two_responses() {
    let server = TestServer::new().await;
    let batch = json!([
        {"jsonrpc":"2.0","id":1,"method":"daemon/status"},
        {"jsonrpc":"2.0","method":"notif/ignored","params":{}},
        {"jsonrpc":"2.0","id":3,"method":"daemon/status"},
    ]);
    let resp = send_batch(&server, &batch).await;
    let arr = resp.as_array().expect("batch response array");
    assert_eq!(arr.len(), 2, "notification must be excluded");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_of_only_notifications_returns_no_frame() {
    use sqry_daemon::ipc::framing::{read_frame, write_frame_json};
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    let server = TestServer::new().await;
    let mut stream = tokio::net::UnixStream::connect(&server.path).await.unwrap();
    write_frame_json(
        &mut stream,
        &sqry_daemon::DaemonHello {
            client_version: "test/0".into(),
            protocol_version: 1,
        },
    )
    .await
    .unwrap();
    let _ = read_frame(&mut stream).await.unwrap().unwrap();

    let batch = json!([
        {"jsonrpc":"2.0","method":"x","params":{}},
        {"jsonrpc":"2.0","method":"y","params":{}},
    ]);
    let body = batch.to_string();
    let len = (body.len() as u32).to_le_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(body.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();

    // No response expected. Poll briefly and confirm.
    let maybe = tokio::time::timeout(Duration::from_millis(200), read_frame(&mut stream)).await;
    match maybe {
        Err(_elapsed) => { /* expected */ }
        Ok(Ok(Some(bytes))) => panic!(
            "notification-only batch must produce no frame; got {}",
            String::from_utf8_lossy(&bytes)
        ),
        Ok(Ok(None)) => panic!("server closed unexpectedly"),
        Ok(Err(e)) => panic!("unexpected error: {e}"),
    }
    drop(stream);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_batch_returns_32600() {
    let server = TestServer::new().await;
    let batch = json!([]);
    let resp = send_batch(&server, &batch).await;
    // Expect a single response object (not an array) per spec.
    let err = resp.get("error").expect("error object");
    assert_eq!(err["code"], json!(-32600));
    assert!(
        resp.get("id").and_then(|v| v.as_null()).is_some() || resp["id"].is_null(),
        "id must be null"
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_with_nested_array_element_returns_32600_for_that_slot() {
    let server = TestServer::new().await;
    let batch = json!([
        {"jsonrpc":"2.0","id":1,"method":"daemon/status"},
        [],
        {"jsonrpc":"2.0","id":3,"method":"daemon/status"},
    ]);
    let resp = send_batch(&server, &batch).await;
    let arr = resp.as_array().expect("batch response array");
    assert_eq!(arr.len(), 3);
    // Find the -32600 slot — its id must be null.
    let invalid = arr
        .iter()
        .find(|r| r.get("error").and_then(|e| e.get("code")) == Some(&json!(-32600)))
        .expect("must include a -32600 slot");
    assert!(invalid["id"].is_null(), "nested-array slot id must be null");
    server.stop().await;
}
