//! Task 8 Phase 8a — JSON-RPC request validator integration tests.

mod support;

use serde_json::json;
use support::ipc::{TestIpcClient, TestServer, expect_error};

async fn hello_and_validate(
    server: &TestServer,
    raw_body: &[u8],
    expected_code: i32,
) -> sqry_daemon::JsonRpcResponse {
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    client.send_raw_bytes(raw_body).await;
    let resp = client.read_response().await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, expected_code,
        "expected code {expected_code}, got {}: {err:?}",
        err.code
    );
    resp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parse_error_32700_id_null() {
    let server = TestServer::new().await;
    // Malformed JSON — not even a complete value.
    let resp = hello_and_validate(&server, b"{not json", -32700).await;
    assert!(resp.id.is_none(), "parse-error id must be null");
    // Server closes after parse error; `stop()` will still drain OK.
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parse_error_serializes_id_as_null() {
    use sqry_daemon::ipc::framing::{read_frame, write_frame_json};
    use tokio::io::AsyncWriteExt;

    let server = TestServer::new().await;
    let mut stream = tokio::net::UnixStream::connect(&server.path).await.unwrap();
    write_frame_json(
        &mut stream,
        &sqry_daemon::DaemonHello {
            client_version: "test/0".into(),
            protocol_version: 1,
            logical_workspace: None,
        },
    )
    .await
    .unwrap();
    // Consume hello response frame.
    let _ = read_frame(&mut stream).await.unwrap().unwrap();
    // Send garbage JSON.
    let body = b"garbage-json";
    let len = (body.len() as u32).to_le_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(body).await.unwrap();
    stream.flush().await.unwrap();
    let resp_bytes = read_frame(&mut stream).await.unwrap().unwrap();
    let text = std::str::from_utf8(&resp_bytes).unwrap();
    assert!(
        text.contains(r#""id":null"#),
        "wire must emit id:null: {text}"
    );
    assert!(
        text.contains(r#""code":-32700"#),
        "wire must have -32700: {text}"
    );
    drop(stream);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_jsonrpc_field_emits_32600() {
    let server = TestServer::new().await;
    let body = json!({ "id": 1, "method": "daemon/status" }).to_string();
    hello_and_validate(&server, body.as_bytes(), -32600).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_jsonrpc_version_emits_32600() {
    let server = TestServer::new().await;
    let body = json!({"jsonrpc":"1.0","id":1,"method":"daemon/status"}).to_string();
    hello_and_validate(&server, body.as_bytes(), -32600).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_method_emits_32600() {
    let server = TestServer::new().await;
    let body = json!({"jsonrpc":"2.0","id":1}).to_string();
    hello_and_validate(&server, body.as_bytes(), -32600).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn numeric_id_shape_matrix() {
    let server = TestServer::new().await;

    // Happy cases: validator accepts → dispatch returns -32601 for
    // the unknown-method branch, which proves the id passed validation.
    //
    // Note: JSON-RPC 2.0 §4 discourages `id: null` on requests; we
    // deserialise both missing `id` and present-null `id` into
    // `Option<JsonRpcId>::None`, which dispatch treats as a
    // notification (no response expected). Present-null is therefore
    // not covered here — see the batch/notification tests for the
    // no-response path.
    let accepted = [
        json!(0i64),
        json!(1i64),
        json!(-1i64),
        json!(i64::MAX),
        json!(u64::MAX),
        json!("abc"),
    ];
    for id in accepted {
        let mut client = TestIpcClient::connect(&server.path).await;
        client.hello(1).await;
        let req = json!({
            "jsonrpc":"2.0",
            "id": id,
            "method":"not-a-real-method",
        });
        let body = req.to_string();
        client.send_raw_bytes(body.as_bytes()).await;
        let resp = client.read_response().await;
        let err = expect_error(&resp);
        assert_eq!(
            err.code, -32601,
            "id {id:?} should have been accepted by validator"
        );
        drop(client);
    }

    // Rejected cases: validator rejects → -32600.
    let rejected = [
        json!(1.5_f64),
        serde_json::from_str::<serde_json::Value>("1e3").unwrap(),
        serde_json::from_str::<serde_json::Value>("42.0E0").unwrap(),
        json!(true),
        json!({}),
        json!([]),
    ];
    for id in rejected {
        let mut client = TestIpcClient::connect(&server.path).await;
        client.hello(1).await;
        let req = json!({
            "jsonrpc":"2.0",
            "id": id,
            "method":"daemon/status",
        });
        let body = req.to_string();
        client.send_raw_bytes(body.as_bytes()).await;
        let resp = client.read_response().await;
        let err = expect_error(&resp);
        assert_eq!(err.code, -32600, "id {id:?} should be rejected");
        drop(client);
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn params_must_be_object_array_or_null() {
    let server = TestServer::new().await;
    let body = json!({
        "jsonrpc":"2.0",
        "id":1,
        "method":"daemon/status",
        "params": "not-an-object",
    })
    .to_string();
    hello_and_validate(&server, body.as_bytes(), -32600).await;
    server.stop().await;
}
