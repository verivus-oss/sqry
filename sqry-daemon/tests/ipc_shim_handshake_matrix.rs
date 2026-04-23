//! Task 8 Phase 8c U14 — shim handshake matrix integration tests.
//!
//! Tests the two mutually-exclusive first-frame paths introduced by U10:
//!
//! 1. **Shim-first flow**: `ShimRegister` as the FIRST frame → server
//!    writes `ShimRegisterAck { accepted: true }` → raw bytes flow through
//!    the byte-pump host.
//!
//! 2. **Hello-then-shim flow (INVALID)**: after a successful `DaemonHello`
//!    handshake the client tries to send a raw `ShimRegister` frame on the
//!    SAME connection. At this point the router is inside the JSON-RPC
//!    request loop; `validate_request_value` rejects the frame because
//!    the `ShimRegister` object lacks `jsonrpc` and `method` fields →
//!    `-32600 Invalid Request`.

mod support;

use serde_json::Value;
use sqry_daemon::ipc::protocol::{JsonRpcPayload, ShimProtocol, ShimRegister, ShimRegisterAck};
use support::ipc::{TestIpcClient, TestServer};

// ---------------------------------------------------------------------------
// Test 1: shim_first_flow_admits_and_transitions_to_raw_bytes
//
// Client sends ShimRegister as the very first frame (before any
// DaemonHello). Server must:
//   a. Detect the shim shape.
//   b. Admit the connection (registry not full).
//   c. Write ShimRegisterAck { accepted: true }.
//   d. Transition to the raw byte-pump host.
//
// The test verifies (a)-(c) directly. (d) is exercised implicitly: after
// the ack the client sends EOF; the byte-pump host sees EOF, cleans up,
// and the server shuts down without panicking.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shim_first_flow_admits_and_transitions_to_raw_bytes() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;

    // ShimRegister is the first and only frame sent.
    let req = ShimRegister {
        protocol: ShimProtocol::Mcp,
        pid: std::process::id(),
    };
    client.send_raw(&req).await;

    // Server must respond with ShimRegisterAck before entering the byte-pump.
    let ack = client.read_typed::<ShimRegisterAck>().await;
    assert!(ack.accepted, "shim-first flow must be accepted");
    assert!(
        !ack.daemon_version.is_empty(),
        "ack must carry daemon_version"
    );
    assert_eq!(
        ack.envelope_version,
        sqry_daemon::ENVELOPE_VERSION,
        "ack envelope_version must match ENVELOPE_VERSION constant"
    );
    assert!(
        ack.reason.is_none(),
        "accepted ack must not carry a reason, got: {:?}",
        ack.reason
    );

    // Drop the client (EOF) — byte-pump host cleans up; server handles
    // this without panic.
    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Test 2: hello_then_shim_fails_invalid_request
//
// After a successful DaemonHello handshake, the router is in JSON-RPC
// request-loop mode. A raw ShimRegister frame ({protocol, pid}) lacks
// `jsonrpc` and `method` fields, so `validate_request_value` at
// `sqry-daemon/src/ipc/validation.rs:79-116` fails BEFORE any method
// dispatch — yielding `-32600 Invalid Request`.
//
// Assertion contract:
//   - `error.code == -32600` (stable; do NOT assert on message substrings
//     per design iter-2 MINOR-1 fix).
//   - If `error.data.reason` is present, it must mention "jsonrpc" or
//     "method" — the authoritative validator failure reason.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hello_then_shim_fails_invalid_request() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;

    // Step 1: normal DaemonHello handshake — must succeed.
    let hello_resp = client.hello(1).await;
    assert!(hello_resp.compatible, "hello must be compatible");

    // Step 2: send a raw ShimRegister frame on the same connection.
    // This object has no `jsonrpc` or `method` fields, so the JSON-RPC
    // request validator must reject it with -32600.
    let shim = ShimRegister {
        protocol: ShimProtocol::Lsp,
        pid: 42,
    };
    client.send_raw(&shim).await;

    // Step 3: read the error response.
    let resp = client.read_response().await;
    match &resp.payload {
        JsonRpcPayload::Error { error } => {
            // Stable contract: assert on code only (iter-2 MINOR-1 fix —
            // do not assert on message substrings).
            assert_eq!(
                error.code, -32600,
                "ShimRegister-after-hello must produce -32600 Invalid Request, got: {}",
                error.code
            );

            // Optional payload validation: if error.data.reason is present,
            // it must reference the missing jsonrpc or method field.
            if let Some(data) = &error.data {
                let reason = data.get("reason").and_then(Value::as_str).unwrap_or("");
                assert!(
                    reason.contains("jsonrpc") || reason.contains("method"),
                    "error.data.reason should mention the missing field: {reason:?}"
                );
            }
        }
        JsonRpcPayload::Success { .. } => {
            panic!("expected -32600 error for ShimRegister-after-hello, got success");
        }
    }

    drop(client);
    server.stop().await;
}
