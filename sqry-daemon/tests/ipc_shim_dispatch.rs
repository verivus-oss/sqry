//! Task 8 Phase 8c U14 — first-frame dispatcher integration tests.
//!
//! Covers the shape-discriminating dispatcher introduced in U10
//! (`sqry-daemon/src/ipc/router.rs::run_connection`): hello-shaped
//! frames route to the JSON-RPC request loop; shim-shaped frames route
//! to the byte-pump host; malformed, ambiguous, or rejected frames are
//! rejected with appropriate error frames.

mod support;

use serde_json::json;
use sqry_daemon::{
    DaemonConfig,
    ipc::protocol::{JsonRpcPayload, ShimProtocol, ShimRegister, ShimRegisterAck},
};
use support::ipc::{TestIpcClient, TestServer};

// ---------------------------------------------------------------------------
// Helper: write a ShimRegister frame and read the ShimRegisterAck.
// ---------------------------------------------------------------------------

async fn send_shim_register_and_read_ack(
    server: &TestServer,
    protocol: ShimProtocol,
    pid: u32,
) -> ShimRegisterAck {
    let mut client = TestIpcClient::connect(&server.path).await;
    let req = ShimRegister { protocol, pid };
    client.send_raw(&req).await;
    client.read_typed::<ShimRegisterAck>().await
}

// ---------------------------------------------------------------------------
// Test 1: hello_first_frame_routed_to_jsonrpc
//
// A DaemonHello first frame is correctly identified as NOT a ShimRegister
// and the connection enters the JSON-RPC request loop. After the
// successful hello handshake, a JSON-RPC `daemon/status` request gets a
// valid success response — proving the JSON-RPC loop is live.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_first_frame_routed_to_jsonrpc() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;

    // Hello handshake should succeed.
    let hello_resp = client.hello(1).await;
    assert!(hello_resp.compatible, "hello must be compatible");

    // A valid JSON-RPC request on the same connection must get a response —
    // proving we are in the JSON-RPC loop, not in the shim path.
    let resp = client.request("daemon/status", json!({})).await;
    match &resp.payload {
        JsonRpcPayload::Success { .. } => {}
        JsonRpcPayload::Error { error } => {
            panic!("expected success from daemon/status, got error: {error:?}");
        }
    }

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 2: shim_register_first_frame_routed_to_shim_path
//
// A ShimRegister first frame is correctly identified and the router
// enters the shim path. The server writes ShimRegisterAck { accepted: true }
// and then starts the byte-pump host, which waits for the client to send
// protocol-native bytes. The client disconnects immediately (EOF); the
// server must handle this gracefully and the test server must shut down
// cleanly.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shim_register_first_frame_routed_to_shim_path() {
    let server = TestServer::new().await;
    let ack = send_shim_register_and_read_ack(&server, ShimProtocol::Mcp, 12345).await;

    assert!(ack.accepted, "shim registration should be accepted");
    assert!(
        !ack.daemon_version.is_empty(),
        "daemon_version must be populated"
    );
    assert_eq!(
        ack.envelope_version,
        sqry_daemon::ENVELOPE_VERSION,
        "envelope_version mismatch"
    );
    assert!(ack.reason.is_none(), "accepted ack must have no reason");

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 3: malformed_first_frame_rejected_with_invalid_request
//
// A first frame that is valid JSON but does not match either DaemonHello
// or ShimRegister shapes is rejected with a -32600 Invalid Request response.
// The connection is closed after the error response.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_first_frame_rejected_with_invalid_request() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;

    // An object that is neither DaemonHello nor ShimRegister — it has
    // no known top-level keys. The router parses it as JSON (no parse
    // error), fails DaemonHello strict deserialisation, and returns -32600.
    let bogus = json!({"completely": "unknown", "shape": true});
    client.send_raw(&bogus).await;

    let resp = client.read_response().await;
    match &resp.payload {
        JsonRpcPayload::Error { error } => {
            assert_eq!(
                error.code, -32600,
                "expected -32600 Invalid Request, got: {}",
                error.code
            );
            assert!(resp.id.is_none(), "invalid-request id must be null");
        }
        JsonRpcPayload::Success { .. } => panic!("expected error, got success"),
    }

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 4: first_frame_mixed_keys_rejected
//
// An object carrying BOTH DaemonHello keys (client_version, protocol_version)
// AND ShimRegister keys (protocol, pid) must be rejected. The
// `deny_unknown_fields` attribute on both structs ensures that neither
// strict deserialisation succeeds: the frame has keys the other type
// doesn't recognise.
//
// The router's shape check sees `protocol` + `pid` → routes to shim path →
// strict `ShimRegister` deserialisation fails (extra `client_version` /
// `protocol_version` fields with `deny_unknown_fields`) → writes
// ShimRegisterAck { accepted: false, reason: Some(...) } and closes.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_frame_mixed_keys_rejected() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;

    // Frame with both hello keys AND shim keys.
    let mixed = json!({
        "client_version": "test/0.0.1",
        "protocol_version": 1,
        "protocol": "lsp",
        "pid": 42,
    });
    client.send_raw(&mixed).await;

    // Because `protocol` + `pid` are present, the shape-discriminator routes
    // this to the shim path. Strict ShimRegister deserialisation fails because
    // `deny_unknown_fields` rejects the extra `client_version` and
    // `protocol_version` fields. The server responds with a rejected
    // ShimRegisterAck.
    let ack = client.read_typed::<ShimRegisterAck>().await;
    assert!(!ack.accepted, "mixed-keys frame must be rejected");
    assert!(ack.reason.is_some(), "rejected ack must carry a reason");

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 5: unknown_protocol_variant_rejected_on_shim_register
//
// A ShimRegister frame carrying an unknown protocol variant (not "lsp" or
// "mcp") fails serde deserialisation (unknown enum variant) and the
// router returns ShimRegisterAck { accepted: false, reason: Some(...) }.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_protocol_variant_rejected_on_shim_register() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;

    // Raw JSON with an unrecognised protocol variant.
    let bad = json!({"protocol": "grpc", "pid": 99});
    client.send_raw(&bad).await;

    let ack = client.read_typed::<ShimRegisterAck>().await;
    assert!(!ack.accepted, "unknown protocol variant must be rejected");
    assert!(ack.reason.is_some(), "rejected ack must carry a reason");

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 6: concurrent_hello_and_shim_connections_isolated
//
// Multiple simultaneous connections — some hello-path, some shim-path —
// are handled independently: hello connections enter the JSON-RPC loop,
// shim connections get their ShimRegisterAck. Neither path interferes
// with the other.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_hello_and_shim_connections_isolated() {
    let server = TestServer::new().await;

    // Spawn 3 hello connections and 3 shim connections concurrently.
    let hello_tasks: Vec<_> = (0u32..3)
        .map(|_| {
            let path = server.path.clone();
            tokio::spawn(async move {
                let mut client = TestIpcClient::connect(&path).await;
                let hello = client.hello(1).await;
                assert!(hello.compatible);
                // Verify JSON-RPC loop is live.
                let resp = client.request("daemon/status", json!({})).await;
                match resp.payload {
                    JsonRpcPayload::Success { .. } => {}
                    JsonRpcPayload::Error { error } => {
                        panic!("hello path got error: {error:?}");
                    }
                }
            })
        })
        .collect();

    let shim_tasks: Vec<_> = (0u32..3)
        .map(|i| {
            let path = server.path.clone();
            tokio::spawn(async move {
                let mut client = TestIpcClient::connect(&path).await;
                let req = ShimRegister {
                    protocol: ShimProtocol::Mcp,
                    pid: 2000 + i,
                };
                client.send_raw(&req).await;
                let ack = client.read_typed::<ShimRegisterAck>().await;
                assert!(ack.accepted, "shim conn {i} must be accepted");
            })
        })
        .collect();

    for task in hello_tasks {
        task.await.expect("hello task panicked");
    }
    for task in shim_tasks {
        task.await.expect("shim task panicked");
    }

    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 7: shim_register_admission_denied_when_cap_reached
//
// When `max_shim_connections` is 1 and one shim connection is already
// registered, a second ShimRegister is rejected with
// ShimRegisterAck { accepted: false, reason: Some("shim registry full ...") }.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shim_register_admission_denied_when_cap_reached() {
    let server = TestServer::with_config(DaemonConfig {
        max_shim_connections: 1,
        ..DaemonConfig::default()
    })
    .await;

    // First connection — must be accepted and we hold it open.
    let mut client1 = TestIpcClient::connect(&server.path).await;
    let req1 = ShimRegister {
        protocol: ShimProtocol::Lsp,
        pid: 100,
    };
    client1.send_raw(&req1).await;
    let ack1 = client1.read_typed::<ShimRegisterAck>().await;
    assert!(ack1.accepted, "first shim must be accepted");

    // Second connection — must be rejected because the cap is full.
    let ack2 = send_shim_register_and_read_ack(&server, ShimProtocol::Mcp, 200).await;
    assert!(!ack2.accepted, "second shim must be denied at cap 1");
    let reason = ack2.reason.expect("rejected ack must have a reason");
    assert!(
        reason.contains("full") || reason.contains("cap") || reason.contains("registry"),
        "rejection reason should mention capacity: {reason}"
    );

    // Drop the first connection; the RAII handle deregisters the entry.
    drop(client1);

    // A small settle to let the server process the EOF and deregister.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Now a new connection must succeed again (cap freed).
    let ack3 = send_shim_register_and_read_ack(&server, ShimProtocol::Mcp, 300).await;
    assert!(
        ack3.accepted,
        "after first client disconnects, new shim must be accepted"
    );

    server.stop().await;
}
