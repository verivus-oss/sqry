//! Phase 8c U15 — daemon tools/list subset assertion.
//!
//! Verifies that the tools advertised by the daemon MCP host via
//! `tools/list` are exactly the 15 names in
//! `sqry_mcp::tools_schema::DAEMON_SUPPORTED_TOOL_NAMES` (the
//! natural-language sqry_ask tool was removed).
//!
//! This test drives the full IPC path: ShimRegister → ShimRegisterAck
//! → rmcp client `list_tools`. It exercises the actual `list_tools`
//! code path in `DaemonMcpHandler` rather than the unit-level assertion
//! in `mcp_host/mod.rs`, giving end-to-end coverage of the 15-name
//! contract through the wire.

#![allow(clippy::too_many_lines)]

mod support;

use std::collections::HashSet;

use sqry_daemon::ipc::framing::{read_frame_json, write_frame_json};
use sqry_daemon_protocol::{ShimProtocol, ShimRegister, ShimRegisterAck};
use sqry_mcp::tools_schema::DAEMON_SUPPORTED_TOOL_NAMES;
use support::ipc::TestServer;
use tokio::net::UnixStream;

/// Connect to the server socket, do the shim handshake, and return
/// the two halves ready for use as an rmcp transport.
async fn connect_mcp_shim(
    server: &TestServer,
) -> (
    tokio::io::ReadHalf<UnixStream>,
    tokio::io::WriteHalf<UnixStream>,
) {
    let stream = UnixStream::connect(&server.path).await.expect("connect");
    let (mut read_half, mut write_half) = tokio::io::split(stream);

    // Send ShimRegister for MCP.
    let shim_reg = ShimRegister {
        protocol: ShimProtocol::Mcp,
        pid: std::process::id(),
    };
    write_frame_json(&mut write_half, &shim_reg)
        .await
        .expect("write ShimRegister");

    // Read and verify ShimRegisterAck.
    let ack = read_frame_json::<_, ShimRegisterAck>(&mut read_half)
        .await
        .expect("read ack")
        .expect("ack frame");
    assert!(
        ack.accepted,
        "ShimRegisterAck must be accepted; reason={:?}",
        ack.reason
    );

    (read_half, write_half)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_tools_list_exactly_15_names_matches_daemon_supported_tool_names() {
    // Spawn a daemon server with default config.
    let server = TestServer::new().await;

    // Connect a shim (MCP) and get the rmcp transport halves.
    let (read_half, write_half) = connect_mcp_shim(&server).await;

    // Spin up an rmcp client on those halves. `()` implements
    // `ClientHandler` (the minimal no-op implementation).
    let running = rmcp::serve_client((), (read_half, write_half))
        .await
        .expect("rmcp client initialize");

    // Call tools/list and collect the name set.
    let list_result = running
        .peer()
        .list_tools(None)
        .await
        .expect("list_tools must succeed");

    let got_names: HashSet<&str> = list_result.tools.iter().map(|t| t.name.as_ref()).collect();
    let expected_names: HashSet<&str> = DAEMON_SUPPORTED_TOOL_NAMES.iter().copied().collect();

    assert_eq!(
        got_names, expected_names,
        "daemon tools/list must return exactly DAEMON_SUPPORTED_TOOL_NAMES \
         (17 tools: 15 + body-shape structural_similar U07 + generate_overview). \
         Got {got_names:?}, expected {expected_names:?}"
    );
    assert_eq!(
        list_result.tools.len(),
        17,
        "tools list length must be exactly 17 under default feature flags \
         (15 + body-shape structural_similar + generate_overview)"
    );

    // Clean up: cancel the rmcp client and stop the server.
    drop(running);
    server.stop().await;
}
