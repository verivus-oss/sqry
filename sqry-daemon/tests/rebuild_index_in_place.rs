//! The daemon-hosted `rebuild_index` over a resident workspace rebuilds it
//! in place through the rebuild dispatcher, as `daemon/rebuild` does
//! (F2, F6). These are the refusals and the failure that need their own
//! fixtures: a narrowing roster (a resolver pinned to the fast path beside
//! a manifest recording every compiled plugin), a memory budget the
//! rebuild cannot fit, and a durable persist that fails after every input
//! was accepted. On each surface the workspace stays resident and watched;
//! a refusal leaves it `Loaded` with no recorded failure, the failure
//! leaves it `Failed` and still serving, and both surfaces give the same
//! answer. Before the repair the daemon-hosted force path unloaded the
//! workspace first, so the budget refusal discarded it, the narrowing
//! rebuild was not refused at all, and the watcher stopped.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use sqry_core::graph::unified::memory::GraphMemorySize;
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::workspace::builder::{FunctionGraphBuilder, graph_with_function_nodes};
use sqry_daemon::{
    DaemonConfig, ESTIMATE_STAGING_PER_FILE_BYTES, RealWorkspaceBuilder, RosterRecord,
    WorkingSetInputs, WorkspaceBuilder, WorkspaceKey, WorkspaceRosterResolver, WorkspaceState,
    ipc::framing::{read_frame_json, write_frame_json},
    working_set_estimate,
};
use sqry_daemon_protocol::{ShimProtocol, ShimRegister, ShimRegisterAck};
use sqry_plugin_registry::RosterSource;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tokio::net::UnixStream;

/// An MCP client session on the daemon's MCP shim.
async fn mcp_session(server: &TestServer) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let stream = UnixStream::connect(&server.path).await.expect("connect");
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

async fn ipc_client(server: &TestServer) -> TestIpcClient {
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    client
}

/// `daemon/rebuild` of `root` with `force: true`.
async fn ipc_rebuild(
    client: &mut TestIpcClient,
    root: &Path,
) -> sqry_daemon::ipc::protocol::JsonRpcResponse {
    client
        .request(
            "daemon/rebuild",
            json!({ "path": root.to_string_lossy().as_ref(), "force": true }),
        )
        .await
}

/// `rebuild_index` of `root` with `force: true`.
async fn mcp_rebuild(
    peer: &rmcp::Peer<rmcp::RoleClient>,
    root: &Path,
) -> Result<rmcp::model::CallToolResult, rmcp::ServiceError> {
    peer.call_tool(
        rmcp::model::CallToolRequestParams::new("rebuild_index").with_arguments(
            serde_json::Map::from_iter([
                ("path".to_string(), json!(root.to_string_lossy().as_ref())),
                ("force".to_string(), json!(true)),
            ]),
        ),
    )
    .await
}

fn mcp_error(
    label: &str,
    outcome: Result<rmcp::model::CallToolResult, rmcp::ServiceError>,
) -> rmcp::ErrorData {
    match outcome {
        Ok(result) => panic!("{label}: the call must fail, got {result:?}"),
        Err(rmcp::ServiceError::McpError(err)) => err,
        Err(other) => panic!("{label}: expected an MCP error envelope, got {other:?}"),
    }
}

async fn status_row(client: &mut TestIpcClient, root: &Path) -> Value {
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
        .unwrap_or_else(|| panic!("a status row for {wanted}: {status}"))
}

/// What a rebuild that did not complete must leave behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// A refusal: `Loaded`, no recorded failure, no backoff.
    Refused,
    /// A failure after every input was accepted: `Failed`, the failure
    /// recorded, the old generation still serving.
    Failed,
}

/// Assert the workspace at `key` is still resident with `graph_before`,
/// still watched, and in the state `expect` names.
async fn assert_kept(
    label: &str,
    server: &TestServer,
    client: &mut TestIpcClient,
    key: &WorkspaceKey,
    graph_before: &Arc<sqry_core::graph::CodeGraph>,
    expect: Expect,
) -> Value {
    let ws = server
        .manager
        .lookup(key)
        .unwrap_or_else(|| panic!("{label}: the workspace must stay resident"));
    assert!(
        Arc::ptr_eq(graph_before, &ws.graph()),
        "{label}: the resident graph must be the one published before"
    );
    let row = status_row(client, &key.source_root).await;
    assert_eq!(
        row["watching"],
        json!(true),
        "{label}: still watched: {row}"
    );
    match expect {
        Expect::Refused => {
            assert_eq!(ws.load_state(), WorkspaceState::Loaded, "{label}");
            assert_eq!(row["state"], json!("Loaded"), "{label}: {row}");
            assert!(row["last_error"].is_null(), "{label}: {row}");
            assert_eq!(row["retry_count"], json!(0), "{label}: {row}");
        }
        Expect::Failed => {
            assert_eq!(ws.load_state(), WorkspaceState::Failed, "{label}");
            assert_eq!(row["state"], json!("Failed"), "{label}: {row}");
            assert!(
                row["last_error"].is_string(),
                "{label}: the failure is recorded: {row}"
            );
        }
    }
    row
}

/// Run the same rebuild on `daemon/rebuild` and on `rebuild_index`, assert
/// each answers with the expected codes and the same message, and that
/// each left the workspace as `expect` says. Returns the message.
#[allow(clippy::too_many_arguments)]
async fn on_both_surfaces(
    label: &str,
    server: &TestServer,
    client: &mut TestIpcClient,
    peer: &rmcp::Peer<rmcp::RoleClient>,
    key: &WorkspaceKey,
    graph_before: &Arc<sqry_core::graph::CodeGraph>,
    ipc_code: i32,
    mcp_code: i32,
    expect: Expect,
) -> String {
    let resp = ipc_rebuild(client, &key.source_root).await;
    let ipc_err = expect_error(&resp).clone();
    assert_eq!(ipc_err.code, ipc_code, "{label}: {ipc_err:?}");
    assert_kept(
        &format!("{label} (daemon/rebuild)"),
        server,
        client,
        key,
        graph_before,
        expect,
    )
    .await;
    let mcp_err = mcp_error(label, mcp_rebuild(peer, &key.source_root).await);
    assert_eq!(mcp_err.code.0, mcp_code, "{label}: {mcp_err:?}");
    assert_kept(
        &format!("{label} (rebuild_index)"),
        server,
        client,
        key,
        graph_before,
        expect,
    )
    .await;
    // `WorkspaceBuildFailed` names the root in the IPC message and not in
    // the MCP one (the MCP error map renders it without); the reason after
    // "build failed: " is the same on both. Every other answer here is
    // rendered identically.
    let comparable = |message: &str| {
        message
            .split_once("build failed: ")
            .map_or_else(|| message.to_string(), |(_, reason)| reason.to_string())
    };
    assert_eq!(
        comparable(&mcp_err.message),
        comparable(&ipc_err.message),
        "{label}: both surfaces give the same answer"
    );
    println!("{label}: {}", ipc_err.message);
    ipc_err.message
}

fn plugin_ids_of(plugins: &sqry_core::plugin::PluginManager) -> Vec<String> {
    plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect()
}

/// A Rust file beside a JSON file, so a build with every compiled plugin
/// differs from a fast-path build.
fn write_mixed_fixture(root: &Path) {
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
}

fn index_with(root: &Path, plugins: &sqry_core::plugin::PluginManager, high_cost_mode: &str) {
    sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
        root,
        plugins,
        &sqry_core::graph::unified::build::BuildConfig::default(),
        "test:rebuild_index_in_place",
        Some(PluginSelectionManifest {
            active_plugin_ids: plugin_ids_of(plugins),
            high_cost_mode: Some(high_cost_mode.to_string()),
        }),
        sqry_core::progress::no_op_reporter(),
    )
    .expect("index persists");
}

async fn load(client: &mut TestIpcClient, root: &Path) {
    let resp = client
        .request(
            "daemon/load",
            json!({ "index_root": root.to_string_lossy().as_ref() }),
        )
        .await;
    expect_success(&resp);
}

/// A rebuild whose roster narrows the recorded selection is refused before
/// anything is reserved, with the same answer on both surfaces, and the
/// workspace stays `Loaded` and watched. The resolver is pinned to the fast
/// path; the manifest records every compiled plugin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_narrowing_rebuild_is_refused_the_same_way_on_both_surfaces() {
    let fast = sqry_plugin_registry::create_plugin_manager();
    let full = sqry_plugin_registry::create_plugin_manager_all();
    let pinned = Arc::new(WorkspaceRosterResolver::pinned(
        Arc::new(sqry_plugin_registry::create_plugin_manager()),
        RosterRecord::from_manager(&fast, RosterSource::Fallback),
    ));
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&pinned)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), pinned).await;
    let dir = tempfile::tempdir().unwrap();
    let root = canonicalize_path(dir.path()).unwrap();
    // A git repository, which the file watcher requires.
    support::init_git_repo(&root);
    write_mixed_fixture(&root);
    index_with(&root, &full, "include_all");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::default(), 0);

    let mut client = ipc_client(&server).await;
    let running = mcp_session(&server).await;
    load(&mut client, &root).await;
    let graph_before = server.manager.lookup(&key).expect("resident").graph();
    assert_kept(
        "loaded",
        &server,
        &mut client,
        &key,
        &graph_before,
        Expect::Refused,
    )
    .await;

    let message = on_both_surfaces(
        "narrowing",
        &server,
        &mut client,
        running.peer(),
        &key,
        &graph_before,
        sqry_daemon::JSONRPC_REBUILD_WOULD_NARROW_SELECTION,
        -32602,
        Expect::Refused,
    )
    .await;
    assert!(
        message.contains("json"),
        "the refusal names json: {message}"
    );

    drop(running);
    drop(client);
    server.stop().await;
}

fn full_rebuild_estimate(graph: &sqry_core::graph::CodeGraph) -> u64 {
    working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: graph.heap_bytes() as u64,
        staging_overhead: (graph.files().len() as u64)
            .saturating_mul(ESTIMATE_STAGING_PER_FILE_BYTES),
        interner_snapshot_bytes: graph.strings().heap_bytes() as u64,
    })
}

/// A budget that holds the resident workspace but not its rebuild working
/// set on top, with nothing else to evict: both surfaces refuse with
/// `MemoryBudgetExceeded` and the same message, and the workspace stays
/// `Loaded` and watched with no recorded failure. Before the repair the
/// daemon-hosted force path unloaded the workspace before the reservation
/// refused, so the refusal discarded it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rebuild_the_budget_cannot_admit_keeps_the_workspace_on_both_surfaces() {
    let config = DaemonConfig {
        memory_limit_mb: 1,
        ..DaemonConfig::default()
    };
    let limit = config.memory_limit_bytes();
    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server = TestServer::with_builder_config_and_roster(builder, config, resolver).await;
    let mut nodes = 200_u32;
    loop {
        let graph = graph_with_function_nodes(nodes);
        let size = graph.heap_bytes() as u64;
        assert!(size <= limit, "the fixture overshot at {nodes} nodes");
        if size + full_rebuild_estimate(&graph) > limit {
            break;
        }
        nodes += 200;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = canonicalize_path(dir.path()).unwrap();
    // A git repository, which the file watcher requires.
    support::init_git_repo(&root);
    std::fs::write(root.join("lib.rs"), b"pub fn a() {}\n").expect("source");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::default(), 0);
    let graph_before = server
        .manager
        .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(nodes), 1)
        .expect("the workspace loads");
    // Watched, as `daemon/load` leaves it.
    server.dispatcher.start_watching(&key);

    let mut client = ipc_client(&server).await;
    let running = mcp_session(&server).await;
    assert_kept(
        "loaded",
        &server,
        &mut client,
        &key,
        &graph_before,
        Expect::Refused,
    )
    .await;
    let message = on_both_surfaces(
        "memory budget",
        &server,
        &mut client,
        running.peer(),
        &key,
        &graph_before,
        sqry_daemon::JSONRPC_MEMORY_BUDGET_EXCEEDED,
        -32603,
        Expect::Refused,
    )
    .await;
    assert!(message.contains("memory"), "{message}");

    drop(running);
    drop(client);
    server.stop().await;
}

/// A rebuild that fails after every input was accepted (the durable
/// persist cannot write the snapshot, because a directory stands where the
/// snapshot file goes) leaves the workspace resident and watched, `Failed`
/// with the failure recorded and the old generation still serving, with
/// the same answer on both surfaces. Once the obstacle is gone, a forced
/// rebuild publishes a new generation and the workspace is `Loaded` again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rebuild_that_fails_to_persist_keeps_the_workspace_on_both_surfaces() {
    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let dir = tempfile::tempdir().unwrap();
    let root = canonicalize_path(dir.path()).unwrap();
    // A git repository, which the file watcher requires.
    support::init_git_repo(&root);
    write_mixed_fixture(&root);
    index_with(
        &root,
        &sqry_plugin_registry::create_plugin_manager(),
        "fast_path_default",
    );
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::default(), 0);

    let mut client = ipc_client(&server).await;
    let running = mcp_session(&server).await;
    load(&mut client, &root).await;
    let graph_before = server.manager.lookup(&key).expect("resident").graph();

    let storage = GraphStorage::new(&root);
    std::fs::remove_file(storage.snapshot_path()).expect("remove the snapshot");
    std::fs::create_dir_all(storage.snapshot_path()).expect("a directory in its place");
    let message = on_both_surfaces(
        "persist failure",
        &server,
        &mut client,
        running.peer(),
        &key,
        &graph_before,
        sqry_daemon::JSONRPC_WORKSPACE_BUILD_FAILED,
        -32603,
        Expect::Failed,
    )
    .await;
    assert!(
        message.contains(storage.snapshot_path().to_string_lossy().as_ref()),
        "the failure names the snapshot: {message}"
    );

    std::fs::remove_dir(storage.snapshot_path()).expect("remove the obstacle");
    let result = mcp_rebuild(running.peer(), &root)
        .await
        .expect("the rebuild succeeds once the snapshot can be written");
    assert!(result.is_error != Some(true), "{result:?}");
    let ws = server.manager.lookup(&key).expect("resident");
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
    assert!(!Arc::ptr_eq(&graph_before, &ws.graph()), "a new generation");
    let row = status_row(&mut client, &root).await;
    assert_eq!(row["watching"], json!(true), "{row}");
    assert!(storage.exists(), "the index was persisted");

    drop(running);
    drop(client);
    server.stop().await;
}
