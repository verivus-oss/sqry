//! Task 8 Phase 8a — JSON-RPC notification tests.

mod support;

use std::time::Duration;

use serde_json::json;
use support::ipc::{TestIpcClient, TestServer};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notification_without_id_emits_no_response() {
    use sqry_daemon::ipc::framing::{read_frame, write_frame_json};
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

    let notif = json!({
        "jsonrpc":"2.0",
        "method":"daemon/status",
        "params":{},
    })
    .to_string();
    let body = notif.as_bytes();
    let len = (body.len() as u32).to_le_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(body).await.unwrap();
    stream.flush().await.unwrap();

    let result = tokio::time::timeout(Duration::from_millis(200), read_frame(&mut stream)).await;
    match result {
        Err(_elapsed) => { /* expected: timeout = no response */ }
        Ok(Ok(Some(bytes))) => panic!(
            "expected no response for notification; got {}",
            String::from_utf8_lossy(&bytes)
        ),
        Ok(Ok(None)) => panic!("server closed unexpectedly after notification"),
        Ok(Err(e)) => panic!("unexpected error: {e}"),
    }
    drop(stream);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notification_before_shutdown_is_dropped() {
    // Issue a notification, then a regular request. The regular
    // request's response proves the notification did not disrupt the
    // request loop or emit a stray response frame.
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    client
        .notify("some/notification", json!({"marker":"test"}))
        .await;
    let resp = client.request("daemon/status", json!({})).await;
    assert!(resp.id.is_some(), "request must get a response");
    drop(client);
    server.stop().await;
}
