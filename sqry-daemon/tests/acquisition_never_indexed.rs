//! A read-only query the daemon cannot serve from a resident graph names
//! the real reason on every daemon surface: the IPC tool methods, the
//! daemon-hosted MCP over the shim, and `daemon/status`.
//!
//! The defect (round 7 integration review, dogfooding the daemon-hosted
//! MCP): a tool pointed at a workspace that had never been indexed answered
//! "workspace <root> evicted mid-rebuild" (`-32004` on IPC, the catch-all
//! `internal` kind on MCP) while `daemon/status` said "no persisted graph
//! artifact". Nothing had been evicted. `classify_for_serve` answers
//! `WorkspaceEvicted` for a key with no entry, the acquirer ran its one
//! read-only reload, `load_persisted` refused (no manifest), and the
//! acquirer's catch-all reload arm turned every refusal into
//! `GraphAcquisitionError::Evicted`, which the wire showed as the bare
//! eviction with the reload's text dropped. The reload left the slot
//! `Failed` over the placeholder, so the next query answered a flattened
//! `-32001` build failure, and indexing the root changed nothing until
//! `daemon/load`; after a genuine eviction the next query answered an
//! internal "stale-servable but carries no roster record" error.
//!
//! Every assertion here reads the wire (codes, messages, `error.data`),
//! never a Rust type the repair introduced, so the file compiles against
//! the tree before the repair and its failures there are the evidence.
//!
//! The eviction tests drive `WorkspaceManager::evict_for_test` and need
//! `--features test-hooks`; the rest run in every build on Unix.

#![cfg(unix)]
#![allow(clippy::too_many_lines)]

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use sqry_core::graph::unified::build::{BuildConfig, build_and_persist_graph_with_progress};
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_core::progress::no_op_reporter;
use sqry_core::project::ProjectRootMode;
use sqry_daemon::ipc::framing::{read_frame_json, write_frame_json};
use sqry_daemon::{
    DaemonConfig, DaemonError, FailingGraphBuilder, RealWorkspaceBuilder, WorkspaceBuilder,
    WorkspaceKey, WorkspaceRosterResolver, WorkspaceState,
};
use sqry_daemon_protocol::{ENVELOPE_VERSION, ShimProtocol, ShimRegister, ShimRegisterAck};
use sqry_plugin_registry::create_plugin_manager;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

fn write_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
    )
    .expect("write lib.rs");
}

/// What `sqry index <root>` writes: a fast-path manifest beside a snapshot.
fn index_fast_path(root: &Path) {
    let plugins = create_plugin_manager();
    let ids: Vec<String> = plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    build_and_persist_graph_with_progress(
        root,
        &plugins,
        &BuildConfig::default(),
        "test:acquisition_never_indexed",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("fast_path_default".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("fast-path index persists");
}

/// A fixture root, canonical, not indexed.
fn fixture() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    (tmp, root)
}

fn manifest_path(root: &Path) -> PathBuf {
    GraphStorage::new(root).manifest_path().to_path_buf()
}

fn snapshot_path(root: &Path) -> PathBuf {
    GraphStorage::new(root).snapshot_path().to_path_buf()
}

fn key(root: &Path) -> WorkspaceKey {
    WorkspaceKey::new(root.to_path_buf(), ProjectRootMode::GitRoot, 0)
}

/// The production daemon's builder over a production resolver.
async fn production_server() -> TestServer {
    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver).await
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// How long one MCP call may take before the test fails.
const MCP_CALL_DEADLINE: Duration = Duration::from_secs(60);

/// One MCP `tools/call` over a fresh shim connection.
async fn mcp_call(
    server: &TestServer,
    tool: &'static str,
    arguments: Value,
) -> Result<rmcp::model::CallToolResult, rmcp::model::ErrorData> {
    let stream = tokio::net::UnixStream::connect(&server.path)
        .await
        .expect("connect");
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
    assert!(
        ack.accepted,
        "ack must be accepted; reason={:?}",
        ack.reason
    );
    let running = rmcp::serve_client((), (rh, wh))
        .await
        .expect("rmcp initialize");
    let Value::Object(arguments) = arguments else {
        panic!("tool arguments must be an object");
    };
    // Bounded: a handler that panics answers nothing, and an unbounded
    // call would hang the test instead of failing it.
    let outcome = tokio::time::timeout(
        MCP_CALL_DEADLINE,
        running
            .peer()
            .call_tool(rmcp::model::CallToolRequestParams::new(tool).with_arguments(arguments)),
    )
    .await
    .unwrap_or_else(|_| panic!("{tool} got no answer within {MCP_CALL_DEADLINE:?}"));
    drop(running);
    match outcome {
        Ok(result) => Ok(result),
        Err(rmcp::ServiceError::McpError(err)) => Err(err),
        Err(other) => panic!("expected an MCP result or error envelope, got {other:?}"),
    }
}

fn search_args(root: &Path) -> Value {
    json!({
        "query": "kind:function",
        "path": root.to_string_lossy(),
        "max_results": 5,
        "context_lines": 0,
        "include_classpath": false,
    })
}

async fn mcp_search(
    server: &TestServer,
    root: &Path,
) -> Result<rmcp::model::CallToolResult, rmcp::model::ErrorData> {
    mcp_call(server, "semantic_search", search_args(root)).await
}

/// The MCP refusal of a `semantic_search`, with the canonical four keys.
async fn mcp_refusal(server: &TestServer, root: &Path) -> rmcp::model::ErrorData {
    let err = mcp_search(server, root)
        .await
        .expect_err("the query must be refused");
    let data = err.data.as_ref().expect("an MCP refusal carries data");
    let keys: Vec<&str> = data
        .as_object()
        .expect("data is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys.len(),
        4,
        "the canonical {{kind, retryable, retry_after_ms, details}} envelope, got {keys:?}"
    );
    err
}

/// The served `semantic_search` names `alpha`.
async fn assert_mcp_serves(server: &TestServer, root: &Path) {
    let result = mcp_search(server, root)
        .await
        .unwrap_or_else(|err| panic!("the query must be served, got {err:?}"));
    let payload = result
        .structured_content
        .as_ref()
        .expect("structured content")
        .to_string();
    assert!(payload.contains("alpha"), "served result: {payload:.400}");
}

async fn ipc(
    server: &TestServer,
    method: &str,
    params: Value,
) -> sqry_daemon::ipc::protocol::JsonRpcResponse {
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    client.request(method, params).await
}

async fn ipc_search(
    server: &TestServer,
    root: &Path,
) -> sqry_daemon::ipc::protocol::JsonRpcResponse {
    ipc(server, "semantic_search", search_args(root)).await
}

async fn daemon_search(
    server: &TestServer,
    root: &Path,
) -> sqry_daemon::ipc::protocol::JsonRpcResponse {
    ipc(
        server,
        "daemon/search",
        json!({
            "envelope_version": ENVELOPE_VERSION,
            "pattern": "alpha",
            "search_path": root.to_string_lossy(),
            "mode": "exact",
            "include_generated": false,
        }),
    )
    .await
}

async fn status_row(server: &TestServer, root: &Path) -> Value {
    let resp = ipc(server, "daemon/status", Value::Null).await;
    let status = expect_success(&resp);
    status["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"].as_str() == Some(&*root.to_string_lossy()))
        })
        .cloned()
        .unwrap_or_else(|| panic!("no status row for {}: {status}", root.display()))
}

// ---------------------------------------------------------------------------
// Assertions on the absent-index refusal
// ---------------------------------------------------------------------------

/// The IPC form: `-32001`, the message is the data's `reason`, and the data
/// names the root, the absent file and both repairs.
fn assert_ipc_not_indexed(
    resp: &sqry_daemon::ipc::protocol::JsonRpcResponse,
    root: &Path,
    missing: &Path,
    repair_command: &str,
) {
    let err = expect_error(resp);
    assert_eq!(
        err.code, -32001,
        "the absent index is -32001, not -32004: {err:?}"
    );
    assert!(
        !err.message.contains("evicted"),
        "nothing was evicted: {}",
        err.message
    );
    let data = err.data.as_ref().expect("error.data");
    assert_eq!(data["root"], json!(root), "{data}");
    assert_eq!(data["missing_path"], json!(missing), "{data}");
    assert_eq!(data["repair_command"], repair_command, "{data}");
    assert_eq!(data["repair_tool"], "rebuild_index", "{data}");
    assert_eq!(data["reason"], err.message.as_str(), "{data}");
    assert!(
        err.message.contains("is not indexed") && err.message.contains(repair_command),
        "the message names the cause and the repair: {}",
        err.message
    );
}

/// The MCP form: not retryable, `workspace_not_ready`, details naming the
/// absent file and both repairs.
fn assert_mcp_not_indexed(
    err: &rmcp::model::ErrorData,
    root: &Path,
    missing: &Path,
    repair_command: &str,
) {
    let data = err.data.as_ref().expect("data");
    assert!(
        !err.message.contains("evicted"),
        "nothing was evicted: {}",
        err.message
    );
    assert_eq!(data["kind"], "workspace_not_ready", "{data}");
    assert_eq!(data["retryable"], false, "{data}");
    let details = &data["details"];
    assert_eq!(details["root"], root.display().to_string(), "{data}");
    assert_eq!(
        details["missing_path"],
        missing.display().to_string(),
        "{data}"
    );
    assert_eq!(details["repair_command"], repair_command, "{data}");
    assert_eq!(details["repair_tool"], "rebuild_index", "{data}");
    assert!(
        err.message.contains("is not indexed") && err.message.contains(repair_command),
        "the message names the cause and the repair: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// A root that was never indexed
// ---------------------------------------------------------------------------

/// The reviewer's reproduction, over every surface: a never-indexed root is
/// refused as not indexed on the first query and on every later one (the
/// reload's `Failed` slot is reloaded, not answered from its stored text),
/// `daemon/status` agrees, and once the root is indexed the next query is
/// served without `daemon/load`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_indexed_root_is_refused_as_not_indexed_until_it_is_indexed() {
    let (_tmp, root) = fixture();
    let server = production_server().await;
    let manifest = manifest_path(&root);
    let repair = format!("sqry index {}", root.display());

    assert!(
        server.manager.lookup(&key(&root)).is_none(),
        "the root has no slot before the first query"
    );
    let first = mcp_refusal(&server, &root).await;
    assert_mcp_not_indexed(&first, &root, &manifest, &repair);
    let second = mcp_refusal(&server, &root).await;
    assert_mcp_not_indexed(&second, &root, &manifest, &repair);
    assert_ipc_not_indexed(&ipc_search(&server, &root).await, &root, &manifest, &repair);
    assert_ipc_not_indexed(
        &daemon_search(&server, &root).await,
        &root,
        &manifest,
        &repair,
    );

    let row = status_row(&server, &root).await;
    assert_eq!(row["state"], "Failed", "{row}");
    let last_error = row["last_error"].as_str().expect("last_error");
    assert!(
        last_error.contains("is not indexed") && last_error.contains(&repair),
        "daemon/status names the same cause: {last_error}"
    );
    // One load attempt for four queries: while the manifest is absent the
    // later queries answer from the record instead of reserving admission
    // for a reload the builder would refuse again.
    assert_eq!(row["retry_count"], 1, "{row}");

    index_fast_path(&root);
    assert_mcp_serves(&server, &root).await;
    expect_success(&ipc_search(&server, &root).await);
    assert_eq!(
        server
            .manager
            .lookup(&key(&root))
            .expect("slot")
            .load_state(),
        WorkspaceState::Loaded
    );
    server.stop().await;
}

/// The MCP caller's repair named in the refusal works: `rebuild_index` over
/// the refused root builds it, and the next query is served.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_indexed_root_is_served_after_rebuild_index() {
    let (_tmp, root) = fixture();
    let server = production_server().await;
    let refused = mcp_refusal(&server, &root).await;
    assert_mcp_not_indexed(
        &refused,
        &root,
        &manifest_path(&root),
        &format!("sqry index {}", root.display()),
    );

    mcp_call(
        &server,
        "rebuild_index",
        json!({ "path": root.to_string_lossy() }),
    )
    .await
    .unwrap_or_else(|err| panic!("rebuild_index must build the root, got {err:?}"));
    assert_mcp_serves(&server, &root).await;
    server.stop().await;
}

/// A manifest with no snapshot is an index the daemon cannot load: refused
/// naming the snapshot and `sqry index --force` (plain `sqry index` reports
/// the manifest as an existing index and writes nothing).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manifest_without_snapshot_is_refused_naming_the_snapshot_and_force() {
    let (_tmp, root) = fixture();
    index_fast_path(&root);
    std::fs::remove_file(snapshot_path(&root)).expect("remove snapshot");
    let server = production_server().await;
    let repair = format!("sqry index --force {}", root.display());

    let refused = mcp_refusal(&server, &root).await;
    assert_mcp_not_indexed(&refused, &root, &snapshot_path(&root), &repair);
    assert_ipc_not_indexed(
        &ipc_search(&server, &root).await,
        &root,
        &snapshot_path(&root),
        &repair,
    );

    index_fast_path(&root);
    assert_mcp_serves(&server, &root).await;
    server.stop().await;
}

/// Control, the reject side of the reload on a `Failed` slot: a never-loaded
/// root whose `daemon/load` build failed keeps that build failure on the
/// read-only path. No reload runs over it (the builder's `load_persisted`
/// would answer "not implemented"), and `daemon/status` keeps the reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_build_of_a_never_loaded_root_keeps_its_build_failure() {
    let (_tmp, root) = fixture();
    let builder: Arc<dyn WorkspaceBuilder> = Arc::new(FailingGraphBuilder::new("plugin panic"));
    let server = TestServer::with_builder(builder).await;

    let load = ipc(
        &server,
        "daemon/load",
        json!({ "index_root": root.to_string_lossy() }),
    )
    .await;
    let load_err = expect_error(&load);
    assert_eq!(load_err.code, -32001, "{load_err:?}");

    for _ in 0..2 {
        let resp = ipc_search(&server, &root).await;
        let err = expect_error(&resp);
        assert_eq!(err.code, -32001, "{err:?}");
        assert!(
            err.message.contains("plugin panic") && !err.message.contains("not implemented"),
            "the recorded build failure is the answer: {}",
            err.message
        );
    }
    let row = status_row(&server, &root).await;
    assert!(
        row["last_error"]
            .as_str()
            .is_some_and(|e| e.contains("plugin panic")),
        "{row}"
    );
    server.stop().await;
}

/// The unreadable-manifest refusal (design D15) is answered with its keys on
/// every query, not only the first (the second used to be the flattened
/// `-32001` without `manifest_path`), and the repair it names is served
/// without `daemon/load`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreadable_manifest_is_refused_with_its_keys_until_repaired() {
    let (_tmp, root) = fixture();
    index_fast_path(&root);
    std::fs::write(manifest_path(&root), b"{}").expect("corrupt manifest");
    let server = production_server().await;

    for _ in 0..2 {
        let resp = ipc_search(&server, &root).await;
        let err = expect_error(&resp);
        assert_eq!(err.code, -32001, "{err:?}");
        let data = err.data.as_ref().expect("data");
        assert_eq!(data["manifest_path"], json!(manifest_path(&root)), "{data}");
        assert_eq!(
            data["repair_command"],
            format!("sqry index --force {}", root.display()),
            "{data}"
        );
    }

    index_fast_path(&root);
    assert_mcp_serves(&server, &root).await;
    server.stop().await;
}

/// A manifest naming a plugin this binary did not compile (design D-13) is
/// refused `-32005` on every query, not only the first (the second used to
/// be `-32001`), and the reindex is served without `daemon/load`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_plugin_id_is_refused_as_incompatible_until_reindexed() {
    const PLANTED: &str = "acquisition-never-indexed-planted-plugin";
    let (_tmp, root) = fixture();
    index_fast_path(&root);
    let storage = GraphStorage::new(&root);
    let mut manifest = storage.load_manifest().expect("manifest");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push(PLANTED.to_string());
    manifest
        .save(storage.manifest_path())
        .expect("rewrite manifest");
    let server = production_server().await;

    for _ in 0..2 {
        let resp = ipc_search(&server, &root).await;
        let err = expect_error(&resp);
        assert_eq!(err.code, -32005, "{err:?}");
        assert!(err.message.contains(PLANTED), "{}", err.message);
    }

    index_fast_path(&root);
    assert_mcp_serves(&server, &root).await;
    server.stop().await;
}

/// `-32002` carries the configured cap, the last good time and the last
/// error; routed through the shared taxonomy it read "<age>h >= 0h cap" with
/// both left null.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_expired_names_the_configured_cap_and_the_last_error() {
    let (_tmp, root) = fixture();
    let server = TestServer::new().await;
    server
        .manager
        .insert_workspace_in_state_for_test(key(&root), WorkspaceState::Failed);
    let ws = server.manager.lookup(&key(&root)).expect("inserted");
    ws.set_last_good_at_for_test(Some(SystemTime::now() - Duration::from_secs(48 * 3600)));
    ws.record_failure(DaemonError::WorkspaceBuildFailed {
        root: root.clone(),
        reason: "index corrupt".to_string(),
    });
    let cap = DaemonConfig::default().stale_serve_max_age_hours;

    let resp = ipc_search(&server, &root).await;
    let err = expect_error(&resp);
    assert_eq!(err.code, -32002, "{err:?}");
    let data = err.data.as_ref().expect("data");
    assert_eq!(data["cap_hours"], cap, "{data}");
    assert!(
        data["last_error"]
            .as_str()
            .is_some_and(|e| e.contains("index corrupt")),
        "{data}"
    );
    assert!(data["last_good_at"].as_str().is_some(), "{data}");
    assert!(
        err.message.contains(&format!("{cap}h cap")),
        "the message names the cap: {}",
        err.message
    );

    let mcp = mcp_refusal(&server, &root).await;
    let details = &mcp.data.as_ref().expect("data")["details"];
    assert_eq!(details["cap_hours"], cap, "{details}");
    assert!(details["last_good_at"].as_str().is_some(), "{details}");
    server.stop().await;
}

// ---------------------------------------------------------------------------
// A workspace that was loaded and then evicted
// ---------------------------------------------------------------------------

/// Load the indexed root through a first query, then evict it.
#[cfg(feature = "test-hooks")]
async fn loaded_then_evicted(server: &TestServer, root: &Path) {
    assert_mcp_serves(server, root).await;
    assert!(
        server.manager.evict_for_test(&key(root)),
        "the first query loaded the root"
    );
}

/// The eviction refusal: IPC `-32004` and MCP `workspace_not_ready`, both
/// carrying the reload's failure, which must contain `needle`.
#[cfg(feature = "test-hooks")]
async fn assert_evicted_reload_failed(server: &TestServer, root: &Path, needle: &str) {
    let resp = ipc_search(server, root).await;
    let err = expect_error(&resp);
    assert_eq!(err.code, -32004, "{err:?}");
    let data = err.data.as_ref().expect("data");
    assert_eq!(data["root"], json!(root), "{data}");
    let reload = data["reload_failure"]
        .as_str()
        .unwrap_or_else(|| panic!("error.data.reload_failure: {data}"));
    assert!(reload.contains(needle), "reload_failure: {reload}");
    assert!(
        err.message.contains("was evicted") && err.message.contains(needle),
        "the message names the eviction and the failure: {}",
        err.message
    );

    let mcp = mcp_refusal(server, root).await;
    let data = mcp.data.as_ref().expect("data");
    assert_eq!(data["kind"], "workspace_not_ready", "{data}");
    assert!(
        data["details"]["reload_failure"]
            .as_str()
            .is_some_and(|r| r.contains(needle)),
        "{data}"
    );
}

/// Control: an eviction whose reload succeeds is still served, twice.
#[cfg(feature = "test-hooks")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn evicted_workspace_whose_reload_succeeds_is_served() {
    let (_tmp, root) = fixture();
    index_fast_path(&root);
    let server = production_server().await;
    loaded_then_evicted(&server, &root).await;
    assert_mcp_serves(&server, &root).await;
    assert!(server.manager.evict_for_test(&key(&root)));
    expect_success(&ipc_search(&server, &root).await);
    server.stop().await;
}

/// An evicted workspace whose snapshot is gone: `-32004` carrying the
/// absent-index refusal (naming the snapshot and `--force`), the same on the
/// next query (not the internal "stale-servable" error), and served once the
/// root is indexed again.
#[cfg(feature = "test-hooks")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn evicted_workspace_without_a_snapshot_names_the_reload_failure() {
    let (_tmp, root) = fixture();
    index_fast_path(&root);
    let server = production_server().await;
    loaded_then_evicted(&server, &root).await;
    std::fs::remove_file(snapshot_path(&root)).expect("remove snapshot");
    let needle = format!("sqry index --force {}", root.display());

    assert_evicted_reload_failed(&server, &root, &needle).await;
    assert_evicted_reload_failed(&server, &root, &needle).await;
    let row = status_row(&server, &root).await;
    assert_eq!(row["state"], "Failed", "{row}");
    assert_eq!(
        row["retry_count"], 1,
        "one reload for four answers while the snapshot is absent: {row}"
    );

    index_fast_path(&root);
    assert_mcp_serves(&server, &root).await;
    server.stop().await;
}

/// An evicted workspace whose snapshot no longer loads: `-32004` carrying
/// the load error, which names the snapshot and the repair (round 7: the
/// typed `WorkspaceSnapshotUnreadable`), on this query and the next; once
/// the index is rewritten the next query is served.
#[cfg(feature = "test-hooks")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn evicted_workspace_with_a_corrupt_snapshot_names_the_reload_failure() {
    let (_tmp, root) = fixture();
    index_fast_path(&root);
    let server = production_server().await;
    loaded_then_evicted(&server, &root).await;
    std::fs::write(snapshot_path(&root), b"not a snapshot").expect("corrupt snapshot");

    let needle = "cannot be loaded";
    assert_evicted_reload_failed(&server, &root, needle).await;
    assert_evicted_reload_failed(&server, &root, needle).await;
    index_fast_path(&root);
    assert_mcp_serves(&server, &root).await;
    server.stop().await;
}

/// A never-loaded root whose snapshot does not load (DAEMON_FOLLOWUP):
/// every query is refused with `-32001` naming the snapshot and
/// `sqry index --force` (the typed refusal, once, with no "workspace build
/// failed" prefix repeated on later queries), and once the index is
/// rewritten the next query is served without `daemon/load`. Before round
/// 7 the failure was recorded as a build failure, which no reload retried,
/// so the repaired root stayed refused until `daemon/load`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn never_loaded_root_with_a_corrupt_snapshot_is_served_once_repaired() {
    let (_tmp, root) = fixture();
    index_fast_path(&root);
    std::fs::write(snapshot_path(&root), b"not a snapshot").expect("corrupt snapshot");
    let server = production_server().await;
    for attempt in 0..2 {
        let resp = ipc_search(&server, &root).await;
        let err = expect_error(&resp);
        println!(
            "corrupt snapshot, query {attempt}: {} {}",
            err.code, err.message
        );
        assert_eq!(err.code, -32001, "{err:?}");
        assert!(
            err.message.contains("cannot be loaded")
                && err.message.contains("sqry index --force")
                && !err.message.contains("workspace build failed"),
            "{}",
            err.message
        );
        let data = err.data.as_ref().expect("data");
        assert_eq!(data["snapshot_path"], json!(snapshot_path(&root)), "{data}");
    }
    index_fast_path(&root);
    assert_mcp_serves(&server, &root).await;
    server.stop().await;
}
