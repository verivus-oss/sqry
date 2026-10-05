//! Fixtures shared by the rebuild-path integration tests of integration
//! round 7 (the registered key, the watcher lifecycle, `daemon/reset`,
//! refusals from every state, the durable persist and the daemon-hosted
//! `rebuild_index`): a real-builder test server, a git workspace indexed
//! with a chosen plugin selection, an MCP session on the daemon's shim,
//! and the reads those tests make of the status rows and the manifest.

#![allow(dead_code, unused_imports)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_core::project::canonicalize_path;
use sqry_daemon::{
    DaemonConfig, RealWorkspaceBuilder, WorkspaceBuilder, WorkspaceRosterResolver,
    ipc::framing::{read_frame_json, write_frame_json},
    ipc::protocol::{JsonRpcError, JsonRpcPayload, JsonRpcResponse},
};
use sqry_daemon_protocol::{ShimProtocol, ShimRegister, ShimRegisterAck};
use tempfile::TempDir;
use tokio::net::UnixStream;

use super::ipc::{TestIpcClient, TestServer, expect_success};

/// A test server whose builder is the production [`RealWorkspaceBuilder`],
/// sharing its roster resolver with the server's rebuild dispatcher, and
/// the builder itself for direct `get_or_load` calls.
pub async fn real_server(config: DaemonConfig) -> (TestServer, Arc<dyn WorkspaceBuilder>) {
    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(Arc::clone(&builder), config, resolver).await;
    (server, builder)
}

/// A git repository holding one Rust file and one JSON file, so an index
/// built with every compiled plugin records `json` and one built with the
/// fast path does not. Returns the directory and its canonical path.
pub fn mixed_workspace() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = canonicalize_path(dir.path()).expect("canonical root");
    super::init_git_repo(&root);
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn func_alpha() -> u32 { 1 }\n",
    )
    .expect("write lib.rs");
    std::fs::write(
        root.join("config.json"),
        br#"{"name": "fixture", "count": 3}"#,
    )
    .expect("write config.json");
    (dir, root)
}

/// The ids `plugins` registers, in its order.
pub fn plugin_ids_of(plugins: &sqry_core::plugin::PluginManager) -> Vec<String> {
    plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect()
}

/// Index `root` with `plugins` and record `high_cost_mode`, as
/// `sqry index` does.
pub fn index_with(root: &Path, plugins: &sqry_core::plugin::PluginManager, high_cost_mode: &str) {
    sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
        root,
        plugins,
        &sqry_core::graph::unified::build::BuildConfig::default(),
        "test:rebuild-fixtures",
        Some(PluginSelectionManifest {
            active_plugin_ids: plugin_ids_of(plugins),
            high_cost_mode: Some(high_cost_mode.to_string()),
        }),
        sqry_core::progress::no_op_reporter(),
    )
    .expect("index persists");
}

/// Index `root` with the fast-path default roster.
pub fn index_fast_path(root: &Path) {
    index_with(
        root,
        &sqry_plugin_registry::create_plugin_manager(),
        "fast_path_default",
    );
}

/// Index `root` with every compiled plugin (`include_all`).
pub fn index_include_all(root: &Path) {
    index_with(
        root,
        &sqry_plugin_registry::create_plugin_manager_all(),
        "include_all",
    );
}

/// The manifest at `root`, parsed, or `None` when it is absent or
/// unparseable.
pub fn manifest_json(root: &Path) -> Option<Value> {
    let storage = GraphStorage::new(root);
    std::fs::read(storage.manifest_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// The bytes of a file, or `None` when it cannot be read.
pub fn file_bytes(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

/// An IPC client that has completed the handshake.
pub async fn ipc_client(server: &TestServer) -> TestIpcClient {
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    client
}

/// An MCP client session on the daemon's MCP shim.
pub async fn mcp_session(
    server: &TestServer,
) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    mcp_session_at(&server.path).await
}

/// An MCP client session on the MCP shim of the daemon listening at
/// `socket` (a test server, or a `sqryd` process).
pub async fn mcp_session_at(socket: &Path) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let stream = UnixStream::connect(socket).await.expect("connect");
    let (mut rh, mut wh) = tokio::io::split(stream);
    write_frame_json(
        &mut wh,
        &ShimRegister {
            protocol: ShimProtocol::Mcp,
            pid: std::process::id(),
        },
    )
    .await
    .expect("write ShimRegister");
    let ack = read_frame_json::<_, ShimRegisterAck>(&mut rh)
        .await
        .expect("read ack")
        .expect("ack frame");
    assert!(ack.accepted, "ack must be accepted: {:?}", ack.reason);
    rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize")
}

/// `tools/call` of `tool` with `args`.
pub async fn mcp_call(
    peer: &rmcp::Peer<rmcp::RoleClient>,
    tool: &'static str,
    args: Value,
) -> Result<rmcp::model::CallToolResult, rmcp::ServiceError> {
    let map = match args {
        Value::Object(map) => map,
        other => panic!("tool arguments must be an object, got {other}"),
    };
    peer.call_tool(rmcp::model::CallToolRequestParams::new(tool).with_arguments(map))
        .await
}

/// `rebuild_index` of `root` with `force` and the extra fields in `extra`.
pub async fn mcp_rebuild_index(
    peer: &rmcp::Peer<rmcp::RoleClient>,
    root: &Path,
    force: bool,
    extra: &[(&str, Value)],
) -> Result<rmcp::model::CallToolResult, rmcp::ServiceError> {
    let mut args = serde_json::Map::from_iter([
        ("path".to_string(), json!(root.to_string_lossy().as_ref())),
        ("force".to_string(), json!(force)),
    ]);
    for (name, value) in extra {
        args.insert((*name).to_string(), value.clone());
    }
    mcp_call(peer, "rebuild_index", Value::Object(args)).await
}

/// The structured payload of a successful tool call.
pub fn mcp_payload(
    label: &str,
    outcome: &Result<rmcp::model::CallToolResult, rmcp::ServiceError>,
) -> Value {
    match outcome {
        Ok(result) => result
            .structured_content
            .clone()
            .unwrap_or_else(|| panic!("{label}: a structured payload: {result:?}")),
        Err(err) => panic!("{label}: the call must succeed, got {err:?}"),
    }
}

/// The MCP error envelope of a refused tool call.
pub fn mcp_error(
    label: &str,
    outcome: Result<rmcp::model::CallToolResult, rmcp::ServiceError>,
) -> rmcp::ErrorData {
    match outcome {
        Ok(result) => panic!("{label}: the call must fail, got {result:?}"),
        Err(rmcp::ServiceError::McpError(err)) => err,
        Err(other) => panic!("{label}: expected an MCP error envelope, got {other:?}"),
    }
}

/// The error of a JSON-RPC response, `None` for a success.
pub fn err_of(resp: &JsonRpcResponse) -> Option<&JsonRpcError> {
    match &resp.payload {
        JsonRpcPayload::Error { error } => Some(error),
        JsonRpcPayload::Success { .. } => None,
    }
}

/// The `daemon/status` row for `root`, or `Value::Null` when none.
pub async fn status_row(client: &mut TestIpcClient, root: &Path) -> Value {
    let resp = client.request("daemon/status", json!({})).await;
    let status = expect_success(&resp);
    let wanted = root.to_string_lossy().to_string();
    status["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"].as_str() == Some(wanted.as_str()))
        })
        .cloned()
        .unwrap_or(Value::Null)
}

/// `daemon/rebuild` of `root` with `force` and the extra fields in `extra`.
pub async fn ipc_rebuild(
    client: &mut TestIpcClient,
    root: &Path,
    force: bool,
    extra: &[(&str, Value)],
) -> JsonRpcResponse {
    let mut params = serde_json::Map::from_iter([
        ("path".to_string(), json!(root.to_string_lossy().as_ref())),
        ("force".to_string(), json!(force)),
    ]);
    for (name, value) in extra {
        params.insert((*name).to_string(), value.clone());
    }
    client
        .request("daemon/rebuild", Value::Object(params))
        .await
}

/// Poll `predicate` on this thread every 10 ms until it holds or `within`
/// elapses; answers whether it held. For conditions a blocking thread
/// waits on (no runtime needed).
pub fn wait_until_blocking(within: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
