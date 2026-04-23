//! Task 8 Phase 8a — wire framing integration tests.
//!
//! Most framing coverage lives in `src/ipc/framing.rs`'s internal unit
//! tests (the `duplex` harness is unit-test local). The integration
//! tests here exercise framing behaviour through a real IPC server +
//! UDS connection so we see the failure path the way a client would.

mod support;

use std::time::Duration;

use sqry_daemon::ipc::framing::{MAX_FRAME_BYTES, read_frame, write_frame_json};
use support::ipc::TestServer;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

async fn hello_raw(path: &std::path::Path) -> UnixStream {
    let mut stream = UnixStream::connect(path).await.unwrap();
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
    stream
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn round_trip_daemon_status_over_real_socket() {
    let server = TestServer::new().await;
    let mut stream = hello_raw(&server.path).await;
    let req = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"daemon/status",
    });
    let body = req.to_string();
    let len = (body.len() as u32).to_le_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(body.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let resp_bytes = read_frame(&mut stream).await.unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    assert_eq!(resp["id"], serde_json::json!(1));
    assert!(resp["result"].is_object());
    drop(stream);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversize_frame_claim_closes_connection() {
    let server = TestServer::new().await;
    let mut stream = hello_raw(&server.path).await;
    // Claim a length of MAX + 1 but never deliver the body. Server
    // must drop the connection.
    let bad_len = (MAX_FRAME_BYTES as u32 + 1).to_le_bytes();
    stream.write_all(&bad_len).await.unwrap();
    stream.flush().await.unwrap();
    // A well-behaved server closes; read_frame on our side should see
    // either a response frame (unlikely) or clean EOF.
    let result = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut stream)).await;
    // Either Ok(None) (clean EOF after drop) or a UnexpectedEof error.
    match result {
        Ok(Ok(None)) => {}
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        other => panic!("expected EOF or UnexpectedEof, got {other:?}"),
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncated_prefix_closes_connection() {
    let server = TestServer::new().await;
    let mut stream = hello_raw(&server.path).await;
    // Send 2 of 4 length-prefix bytes, then close.
    stream.write_all(&[0x10, 0x00]).await.unwrap();
    stream.flush().await.unwrap();
    drop(stream);
    // The server's per-connection task should log + drop without
    // impacting the accept loop.
    tokio::time::sleep(Duration::from_millis(100)).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn truncated_body_closes_connection() {
    let server = TestServer::new().await;
    let mut stream = hello_raw(&server.path).await;
    let len = 16u32.to_le_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(b"short").await.unwrap();
    stream.flush().await.unwrap();
    drop(stream);
    tokio::time::sleep(Duration::from_millis(100)).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clean_eof_at_frame_boundary_shuts_connection_gracefully() {
    let server = TestServer::new().await;
    let stream = hello_raw(&server.path).await;
    drop(stream);
    // Give the server a moment to process the close before asserting
    // overall server shutdown works.
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mid_sized_frame_round_trips() {
    let server = TestServer::new().await;
    let mut stream = hello_raw(&server.path).await;
    // A 1 MiB request payload with a big `params.pad` string — we
    // expect a -32601 Method not found (the method name is fake) but
    // the frame itself must round-trip.
    let pad = "x".repeat(1_000_000);
    let req = serde_json::json!({
        "jsonrpc":"2.0","id":42,"method":"not-a-real-method",
        "params": { "pad": pad },
    });
    let body = serde_json::to_vec(&req).unwrap();
    let len = (body.len() as u32).to_le_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(&body).await.unwrap();
    stream.flush().await.unwrap();
    let resp_bytes = read_frame(&mut stream).await.unwrap().unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    assert_eq!(resp["id"], serde_json::json!(42));
    assert_eq!(resp["error"]["code"], serde_json::json!(-32601));
    drop(stream);
    server.stop().await;
}
