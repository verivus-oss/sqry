//! T6 and T7 (surface parity, unit W1): a daemon rebuild preserves the
//! plugin selection the manifest records, and a rebuild that would narrow
//! it is refused before anything is written.
//!
//! Both tests plant an `include_all` index (one `.rs`, one `.json`) and
//! drive a rebuild twice: once through a resolver pinned to the fast path
//! (the roster the daemon used for every workspace before W1), which must
//! be refused with `-32021` and leave the manifest bytes unchanged; and
//! once through the production resolver, which must succeed and keep
//! `json` and `high_cost_mode: include_all` in the manifest.
//!
//! T6 drives `daemon/rebuild { force: true }` over IPC (a `Full` rebuild).
//! T7 drives `RebuildDispatcher::handle_changes` with an empty change set,
//! which `decide_mode` classifies `Incremental` (asserted through
//! `last_mode()`), because the persist, and therefore the guard, runs for
//! both modes.
//!
//! On the pre-change head both runs overwrote the manifest: ids without
//! `json`, `high_cost_mode` absent, and the RPC returned success.
//!
//! Round 2 adds T23 (design D9): a `daemon/rebuild` over a resident
//! workspace whose manifest has become unreadable is refused with `-32001`
//! naming the file and `sqry index --force <root>`, the manifest bytes are
//! unchanged, and the workspace state is the state before the call. On
//! `abefdd8e3` the resolver fell back, the guard treated the unreadable
//! file as no prior selection, and the rebuild rewrote the manifest.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use sha2::{Digest, Sha256};
use sqry_core::graph::unified::build::{BuildConfig, build_and_persist_graph_with_progress};
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_core::progress::no_op_reporter;
use sqry_core::project::ProjectRootMode;
use sqry_core::watch::ChangeSet;
use sqry_daemon::workspace::{WorkingSetInputs, working_set_estimate};
use sqry_daemon::{
    DaemonConfig, DaemonError, JSONRPC_REBUILD_WOULD_NARROW_SELECTION, RealWorkspaceBuilder,
    RebuildDispatcher, RebuildMode, RosterRecord, WorkspaceBuilder, WorkspaceKey, WorkspaceManager,
    WorkspaceRosterResolver, WorkspaceState,
};
use sqry_plugin_registry::{RosterSource, create_plugin_manager, create_plugin_manager_all};
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;

fn write_mixed_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
    )
    .expect("write lib.rs");
    std::fs::write(
        root.join("config.json"),
        br#"{"name": "fixture", "nested": {"enabled": true, "count": 3}, "items": [1, 2]}"#,
    )
    .expect("write config.json");
}

fn index_with_include_all(root: &Path) {
    let plugins = create_plugin_manager_all();
    let ids: Vec<String> = plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    build_and_persist_graph_with_progress(
        root,
        &plugins,
        &BuildConfig::default(),
        "test:include_all",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("include_all".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("include_all index persists");
}

fn manifest_sha256(root: &Path) -> String {
    let bytes = std::fs::read(GraphStorage::new(root).manifest_path()).expect("manifest bytes");
    Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn recorded_selection(root: &Path) -> PluginSelectionManifest {
    GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable")
        .plugin_selection
        .expect("plugin_selection recorded")
}

fn assert_include_all_recorded(root: &Path, what: &str) {
    let selection = recorded_selection(root);
    assert!(
        selection.active_plugin_ids.iter().any(|id| id == "json"),
        "{what}: the manifest must still record json, got {:?}",
        selection.active_plugin_ids
    );
    assert_eq!(
        selection.high_cost_mode.as_deref(),
        Some("include_all"),
        "{what}: high_cost_mode must be carried through, not dropped to None"
    );
}

/// A resolver that returns the fast-path roster for every root, planting
/// the pre-W1 daemon behaviour so the guard has something to refuse.
fn pinned_fast_path_resolver() -> Arc<WorkspaceRosterResolver> {
    let fast = Arc::new(create_plugin_manager());
    let record = RosterRecord::from_manager(&fast, RosterSource::Fallback);
    assert!(
        !record.contains("json"),
        "fixture precondition: the pinned roster must lack json"
    );
    Arc::new(WorkspaceRosterResolver::pinned(fast, record))
}

fn estimate() -> u64 {
    working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: 256 * 1024,
        staging_overhead: 64 * 1024,
        interner_snapshot_bytes: 32 * 1024,
    })
}

// ---------------------------------------------------------------------------
// T6: daemon/rebuild over IPC
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_rebuild_refuses_to_narrow_the_recorded_selection() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    index_with_include_all(&root);
    let sha_before = manifest_sha256(&root);

    let resolver = pinned_fast_path_resolver();
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    let resp = client
        .request("daemon/rebuild", json!({ "path": &path, "force": true }))
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, JSONRPC_REBUILD_WOULD_NARROW_SELECTION,
        "a narrowing rebuild must be refused with the dedicated code: {err:?}"
    );
    assert_eq!(err.code, -32021);
    let data = err.data.clone().expect("refusal carries error.data");
    assert_eq!(data["kind"], "rebuild_would_narrow_selection");
    assert_eq!(data["missing_plugin_ids"], json!(["json"]));
    let restore = data["restore_command"]
        .as_str()
        .expect("restore_command is a string");
    assert_eq!(
        restore,
        format!("sqry index --force --include-high-cost {}", root.display())
    );

    assert_eq!(
        manifest_sha256(&root),
        sha_before,
        "the refusal must leave the manifest bytes unchanged"
    );
    assert_include_all_recorded(&root, "after refusal");

    // The prior graph is intact and still served: the workspace is
    // Loaded, not Failed, and the tool path answers.
    let status_resp = client.request("daemon/status", json!({})).await;
    let status = expect_success(&status_resp);
    let row = status["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"].as_str() == Some(path.as_str()))
        })
        .cloned()
        .expect("workspace row present");
    assert_eq!(row["state"], json!("Loaded"), "row: {row}");

    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_rebuild_with_the_resolver_preserves_the_recorded_selection() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    index_with_include_all(&root);
    let sha_before = manifest_sha256(&root);

    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    let resp = client
        .request("daemon/rebuild", json!({ "path": &path, "force": true }))
        .await;
    let result = expect_success(&resp);
    assert_eq!(result["result"]["status"], json!("completed"), "{result}");
    let nodes = result["result"]["nodes"].as_u64().expect("nodes reported");
    assert!(nodes > 0, "a rebuilt graph must have nodes, got {nodes}");

    assert_ne!(
        manifest_sha256(&root),
        sha_before,
        "a successful rebuild rewrites the manifest (new built_at and snapshot sha)"
    );
    assert_include_all_recorded(&root, "after resolver rebuild");
    // Invariant I7 (surface parity W4): a `{path, force}` request over a
    // workspace with no macro record writes no `macro_options` key.
    assert!(
        GraphStorage::new(&root)
            .load_manifest()
            .expect("manifest readable")
            .macro_options
            .is_none(),
        "an old client's request must not invent a macro record"
    );
    let selection = recorded_selection(&root);
    let all_ids: Vec<String> = create_plugin_manager_all()
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    assert_eq!(
        selection.active_plugin_ids, all_ids,
        "the rewritten selection must be exactly the recorded include_all roster"
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// T7: RebuildDispatcher::handle_changes with an Incremental change set
// ---------------------------------------------------------------------------

fn empty_change_set() -> ChangeSet {
    ChangeSet {
        changed_files: vec![],
        git_state_changed: false,
        git_change_class: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_rebuild_refuses_to_narrow_the_recorded_selection() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    index_with_include_all(&root);
    let sha_before = manifest_sha256(&root);

    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    let resolver = pinned_fast_path_resolver();
    let dispatcher = RebuildDispatcher::new(
        Arc::clone(&manager),
        Arc::clone(&config),
        Arc::clone(&resolver),
    );
    let builder = RealWorkspaceBuilder::new(Arc::clone(&resolver));
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    manager
        .get_or_load(&key, &builder, estimate())
        .expect("initial load");

    let err = dispatcher
        .handle_changes(&key, empty_change_set())
        .await
        .expect_err("a narrowing incremental rebuild must be refused");
    assert_eq!(
        dispatcher.last_mode(),
        Some(RebuildMode::Incremental),
        "an empty change set must be classified Incremental"
    );
    match &err {
        DaemonError::RebuildWouldNarrowSelection {
            missing_plugin_ids, ..
        } => assert_eq!(missing_plugin_ids, &vec!["json".to_string()]),
        other => panic!("expected RebuildWouldNarrowSelection, got {other:?}"),
    }
    assert_eq!(err.jsonrpc_code(), Some(-32021));
    assert_eq!(
        manifest_sha256(&root),
        sha_before,
        "the refusal must leave the manifest bytes unchanged"
    );
    assert_include_all_recorded(&root, "after incremental refusal");
    let ws = manager.lookup(&key).expect("workspace registered");
    assert_eq!(
        ws.load_state(),
        WorkspaceState::Loaded,
        "a refused rebuild leaves the prior graph served"
    );
    assert!(
        ws.last_error.read().is_none(),
        "a refusal that wrote nothing is not a recorded failure"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn incremental_rebuild_with_the_resolver_preserves_the_recorded_selection() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    index_with_include_all(&root);

    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let dispatcher = RebuildDispatcher::new(
        Arc::clone(&manager),
        Arc::clone(&config),
        Arc::clone(&resolver),
    );
    let builder = RealWorkspaceBuilder::new(Arc::clone(&resolver));
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    manager
        .get_or_load(&key, &builder, estimate())
        .expect("initial load");

    dispatcher
        .handle_changes(&key, empty_change_set())
        .await
        .expect("an incremental rebuild with the manifest roster succeeds");
    assert_eq!(dispatcher.last_mode(), Some(RebuildMode::Incremental));
    assert_include_all_recorded(&root, "after incremental resolver rebuild");
    let ws = manager.lookup(&key).expect("workspace registered");
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
    let record = ws.roster().expect("record published with the graph");
    assert!(record.contains("json"));
    assert_eq!(record.source, RosterSource::PersistedManifest);
}

// ---------------------------------------------------------------------------
// T23 (round 2): daemon/rebuild over an unreadable manifest
// ---------------------------------------------------------------------------

fn workspace_state_row(status: &serde_json::Value, path: &str) -> serde_json::Value {
    status["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"].as_str() == Some(path))
        })
        .cloned()
        .expect("workspace row present")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_rebuild_refuses_an_unreadable_manifest_and_writes_nothing() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_mixed_fixture(&root);
    index_with_include_all(&root);

    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    let state_before = workspace_state_row(
        expect_success(&client.request("daemon/status", json!({})).await),
        &path,
    )["state"]
        .clone();
    assert_eq!(state_before, json!("Loaded"), "fixture precondition");

    // The manifest becomes unreadable under the resident graph.
    let storage = GraphStorage::new(&root);
    std::fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");
    let sha_before = manifest_sha256(&root);
    let manifest_path = storage.manifest_path().display().to_string();
    let repair = format!("sqry index --force {}", root.display());

    let resp = client
        .request("daemon/rebuild", json!({ "path": &path, "force": true }))
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32001,
        "a rebuild over an unreadable manifest must be refused with -32001: {err:?}"
    );
    assert!(
        err.message.contains(&manifest_path) && err.message.contains(&repair),
        "the refusal must name the manifest and the repair: {}",
        err.message
    );
    let data = err.data.clone().expect("refusal carries error.data");
    assert_eq!(data["manifest_path"], manifest_path);
    assert_eq!(data["repair_command"], repair);

    let sha_after = manifest_sha256(&root);
    assert_eq!(
        sha_after, sha_before,
        "the refusal must leave the manifest bytes unchanged (before {sha_before}, after {sha_after})"
    );
    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        b"{}".to_vec(),
        "the unreadable bytes are still the unreadable bytes"
    );

    // The refusal wrote nothing and the prior graph is intact, so the
    // workspace state is the state before the call.
    let state_after = workspace_state_row(
        expect_success(&client.request("daemon/status", json!({})).await),
        &path,
    )["state"]
        .clone();
    assert_eq!(
        state_after, state_before,
        "a refusal before any write must leave the workspace state unchanged"
    );

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Surface parity W4 (design W4-D6, W4-D7, W4-D8): `daemon/rebuild` records,
// reuses and resets the macro build options, and refuses a recorded expand
// cache directory that no longer exists before anything is written.
// ---------------------------------------------------------------------------

const W4_CFG_LIB_RS: &str =
    "#[cfg(test)]\npub fn gated_by_test() -> u32 { 1 }\n\npub fn always_present() -> u32 { 2 }\n";

fn write_cfg_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(root.join("src").join("lib.rs"), W4_CFG_LIB_RS).expect("write lib.rs");
}

/// Index `root` with the full roster (as `index_with_include_all`) and the
/// given macro options, so the manifest records both.
fn index_with_include_all_and_macro_options(
    root: &Path,
    macro_options: sqry_core::graph::unified::build::MacroBuildOptions,
) {
    let plugins = create_plugin_manager_all();
    let ids: Vec<String> = plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect();
    let config = BuildConfig {
        macro_options,
        ..BuildConfig::default()
    };
    build_and_persist_graph_with_progress(
        root,
        &plugins,
        &config,
        "test:include_all_macro",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("include_all".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("index persists");
}

fn recorded_macro_options(
    root: &Path,
) -> Option<sqry_core::graph::unified::persistence::MacroOptionsManifest> {
    GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable")
        .macro_options
}

/// The activation the persisted snapshot records for the fixture's
/// `cfg(test)` item: `Some(true)` only when the build ran with `--cfg test`.
fn persisted_cfg_test_activation(root: &Path) -> Option<bool> {
    let storage = GraphStorage::new(root);
    let graph =
        sqry_core::graph::unified::persistence::load_from_path(storage.snapshot_path(), None)
            .expect("snapshot loads");
    let pairs: Vec<(String, Option<bool>)> = graph
        .macro_metadata()
        .iter()
        .filter_map(|(_, meta)| meta.cfg_condition.clone().map(|c| (c, meta.cfg_active)))
        .collect();
    println!("recorded cfg pairs: {pairs:?}");
    pairs
        .iter()
        .find(|(condition, _)| condition.contains("test"))
        .unwrap_or_else(|| {
            panic!("fixture precondition: the cfg(test) item is recorded: {pairs:?}")
        })
        .1
}

fn sha256_of(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read file");
    Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn daemon_search_total(client: &mut TestIpcClient, root: &Path, pattern: &str) -> u64 {
    let resp = client
        .request(
            "daemon/search",
            json!({
                "envelope_version": sqry_daemon_protocol::ENVELOPE_VERSION,
                "pattern": pattern,
                "search_path": root.to_string_lossy(),
                "mode": "exact",
                "include_generated": false,
            }),
        )
        .await;
    let envelope = expect_success(&resp);
    envelope["result"]["total"]
        .as_u64()
        .unwrap_or_else(|| panic!("daemon/search total: {envelope}"))
}

/// T12: `daemon/rebuild` with `cfg_flags` records them and builds with
/// them; a following `{path, force}` request reuses the record; a
/// `reset_macro_options` request drops it. On the pre-change head the
/// first request was refused as invalid params (`deny_unknown_fields`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_rebuild_records_reuses_and_resets_the_macro_options() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_cfg_fixture(&root);
    index_with_include_all_and_macro_options(
        &root,
        sqry_core::graph::unified::build::MacroBuildOptions::default(),
    );
    assert!(
        recorded_macro_options(&root).is_none(),
        "fixture precondition: no record"
    );
    assert_eq!(
        persisted_cfg_test_activation(&root),
        None,
        "fixture precondition"
    );

    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // Explicit flags: recorded and built with.
    let resp = client
        .request(
            "daemon/rebuild",
            json!({ "path": &path, "force": true, "cfg_flags": ["test"] }),
        )
        .await;
    let result = expect_success(&resp);
    assert_eq!(result["result"]["status"], json!("completed"), "{result}");
    let record = recorded_macro_options(&root).expect("the rebuild records the flags");
    assert_eq!(record.cfg_flags, vec!["test".to_string()]);
    assert_eq!(record.expand_cache_dir, None);
    assert_eq!(
        persisted_cfg_test_activation(&root),
        Some(true),
        "the rebuilt graph must carry the activation"
    );
    let record_bytes = serde_json::to_vec(&record).expect("record serialises");

    // An old client's request reuses the record (invariant I7 in the
    // presence of a record).
    let resp = client
        .request("daemon/rebuild", json!({ "path": &path, "force": true }))
        .await;
    expect_success(&resp);
    let reused = recorded_macro_options(&root).expect("the record survives");
    assert_eq!(
        serde_json::to_vec(&reused).expect("record serialises"),
        record_bytes,
        "a request without macro fields keeps the record byte for byte"
    );
    assert_eq!(persisted_cfg_test_activation(&root), Some(true));
    assert_include_all_recorded(&root, "after the reuse rebuild");

    // Reset drops the record.
    let resp = client
        .request(
            "daemon/rebuild",
            json!({ "path": &path, "force": true, "reset_macro_options": true }),
        )
        .await;
    expect_success(&resp);
    assert!(
        recorded_macro_options(&root).is_none(),
        "the reset drops the record"
    );
    assert_eq!(persisted_cfg_test_activation(&root), None);

    drop(client);
    server.stop().await;
}

/// T14 (daemon legs 2, 5 and 6): the manifest names an expand cache
/// directory that is then removed. `daemon/rebuild` is refused with
/// `-32022` naming the directory and the reset command, the workspace stays
/// `Loaded` and still serves the prior graph, the index bytes are
/// untouched; `reset_macro_options` is the way out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w4_add_form_control_daemon_rebuild_refuses_a_missing_recorded_expand_cache() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_cfg_fixture(&root);
    let cache = root.join("expand-cache");
    std::fs::create_dir_all(&cache).expect("cache dir");
    index_with_include_all_and_macro_options(
        &root,
        sqry_core::graph::unified::build::MacroBuildOptions {
            cfg_flags: vec!["test".to_string()],
            expand_cache_dir: Some(cache.clone()),
        },
    );
    let recorded_dir = recorded_macro_options(&root)
        .and_then(|record| record.expand_cache_dir)
        .expect("the manifest names the expand cache directory");
    assert_eq!(Path::new(&recorded_dir), cache.as_path());

    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    let total_before = daemon_search_total(&mut client, &root, "always_present").await;
    assert_eq!(total_before, 1, "fixture precondition: the graph is served");

    std::fs::remove_dir_all(&cache).expect("remove the cache");
    let storage = GraphStorage::new(&root);
    let manifest_sha = sha256_of(storage.manifest_path());
    let snapshot_sha = sha256_of(storage.snapshot_path());

    let resp = client
        .request("daemon/rebuild", json!({ "path": &path, "force": true }))
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code,
        sqry_daemon::JSONRPC_REBUILD_MACRO_OPTIONS_UNAVAILABLE,
        "a missing recorded expand cache must be refused with the dedicated code: {err:?}"
    );
    assert_eq!(err.code, -32022);
    assert!(
        err.message.contains(&recorded_dir) && err.message.contains("--no-macro-options"),
        "the refusal names the directory and the way out: {}",
        err.message
    );
    let data = err.data.clone().expect("refusal carries error.data");
    assert_eq!(data["kind"], "rebuild_macro_options_unavailable");
    assert_eq!(data["expand_cache_dir"], json!(recorded_dir));
    assert_eq!(
        data["reset_command"],
        json!(format!(
            "sqry daemon rebuild --no-macro-options {}",
            root.display()
        ))
    );

    assert_eq!(
        sha256_of(storage.manifest_path()),
        manifest_sha,
        "manifest untouched"
    );
    assert_eq!(
        sha256_of(storage.snapshot_path()),
        snapshot_sha,
        "snapshot untouched"
    );
    let row = workspace_state_row(
        expect_success(&client.request("daemon/status", json!({})).await),
        &path,
    );
    assert_eq!(row["state"], json!("Loaded"), "row: {row}");
    assert_eq!(
        daemon_search_total(&mut client, &root, "always_present").await,
        total_before,
        "the prior graph is still served"
    );

    // The way out: reset on the same surface.
    let resp = client
        .request(
            "daemon/rebuild",
            json!({ "path": &path, "force": true, "reset_macro_options": true }),
        )
        .await;
    expect_success(&resp);
    assert!(
        recorded_macro_options(&root).is_none(),
        "the reset drops the record"
    );
    assert_ne!(
        sha256_of(storage.manifest_path()),
        manifest_sha,
        "the reset rebuilt"
    );

    drop(client);
    server.stop().await;
}

/// Integration of W1 and W4: a `daemon/rebuild` queued behind a running
/// rebuild receives its own outcome. It names an expand cache that does not
/// exist, so it is refused with `-32022` and the index stays as the runner
/// left it, while the runner's own request is answered `completed`. Before,
/// the queued handler waited for `rebuild_in_flight` to clear and answered
/// `completed` whatever its own iteration did.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_queued_daemon_rebuild_reports_its_own_refusal() {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_cfg_fixture(&root);
    index_with_include_all_and_macro_options(
        &root,
        sqry_core::graph::unified::build::MacroBuildOptions::default(),
    );

    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // Installed after the load, so only the two rebuilds below stall.
    let gate = Arc::new(sqry_daemon::TestGate {
        hold: AtomicUsize::new(2),
        release: tokio::sync::Notify::new(),
    });
    let capture = Arc::new(sqry_daemon::TestCapture::default());
    server
        .dispatcher
        .install_test_gate(Arc::clone(&gate))
        .unwrap();
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .unwrap();
    let (_, ws) = server
        .manager
        .find_key_and_workspace_by_path(&root)
        .expect("the workspace is loaded");

    // The runner: a plain forced rebuild on its own connection, stalled.
    let socket = server.path.clone();
    let runner_path = path.clone();
    let runner = tokio::spawn(async move {
        let mut runner_client = TestIpcClient::connect(&socket).await;
        runner_client.hello(1).await;
        runner_client
            .request(
                "daemon/rebuild",
                json!({ "path": runner_path, "force": true }),
            )
            .await
    });
    let mut stalled = false;
    for _ in 0..1000 {
        if capture.iterations.lock().len() == 1 {
            stalled = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(stalled, "the runner's iteration must reach the gate");

    // Queued behind it: a request naming an expand cache that does not exist.
    let missing = root.join("no-such-expand-cache");
    let queued_path = path.clone();
    let queued = tokio::spawn(async move {
        client
            .request(
                "daemon/rebuild",
                json!({
                    "path": queued_path,
                    "force": true,
                    "expand_cache": missing.to_string_lossy(),
                }),
            )
            .await
    });
    let mut parked = false;
    for _ in 0..1000 {
        if ws.rebuild_lane.lock().await.is_some() {
            parked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(parked, "the second request must park behind the runner");

    // Release the runner's iteration; the drain loop takes the queued request
    // and stalls it, so the index can be read before it runs.
    gate.release.notify_one();
    let mut drained = false;
    for _ in 0..3000 {
        if capture.iterations.lock().len() == 2 {
            drained = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(drained, "the drain loop must take the queued request");
    let storage = GraphStorage::new(&root);
    let manifest_sha = sha256_of(storage.manifest_path());
    let snapshot_sha = sha256_of(storage.snapshot_path());
    gate.release.notify_one();

    let queued_resp = queued.await.expect("queued request task did not panic");
    let err = expect_error(&queued_resp);
    assert_eq!(
        err.code,
        sqry_daemon::JSONRPC_REBUILD_MACRO_OPTIONS_UNAVAILABLE,
        "the queued request must receive its own refusal: {err:?}"
    );
    let runner_resp = runner.await.expect("runner task did not panic");
    let result = expect_success(&runner_resp);
    assert_eq!(result["result"]["status"], json!("completed"), "{result}");
    assert_eq!(
        sha256_of(storage.manifest_path()),
        manifest_sha,
        "the refused request writes no manifest"
    );
    assert_eq!(
        sha256_of(storage.snapshot_path()),
        snapshot_sha,
        "the refused request writes no snapshot"
    );
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
}

/// Integration of W1 and W4: `daemon/rebuild` with an empty `expand_cache` is
/// refused with `-32602` and writes nothing. Joined to the workspace root, an
/// empty directory named the root itself, which the manifest then recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_rebuild_refuses_an_empty_expand_cache() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_cfg_fixture(&root);
    index_with_include_all_and_macro_options(
        &root,
        sqry_core::graph::unified::build::MacroBuildOptions::default(),
    );

    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    let storage = GraphStorage::new(&root);
    let manifest_sha = sha256_of(storage.manifest_path());
    let snapshot_sha = sha256_of(storage.snapshot_path());

    let resp = client
        .request(
            "daemon/rebuild",
            json!({ "path": &path, "force": true, "expand_cache": "" }),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32602,
        "an empty expand cache is an invalid argument: {err:?}"
    );
    assert!(
        err.message.contains("expand cache directory is empty"),
        "the refusal says why: {}",
        err.message
    );
    assert_eq!(
        sha256_of(storage.manifest_path()),
        manifest_sha,
        "manifest untouched"
    );
    assert_eq!(
        sha256_of(storage.snapshot_path()),
        snapshot_sha,
        "snapshot untouched"
    );
    assert!(
        recorded_macro_options(&root).is_none(),
        "no expand cache was recorded"
    );
}

/// F2: a `daemon/rebuild` over a resident workspace whose manifest names a
/// plugin id this binary did not compile is refused with `-32005` before
/// anything is written, and the workspace returns to `Loaded` with no
/// recorded failure, the same graph and the manifest untouched. Before the
/// repair the dispatcher recorded a failure and left the workspace `Failed`
/// (`retry_count` 1), where the daemon-hosted `rebuild_index` kept it
/// `Loaded` for the same refusal; after `stale_serve_max_age_hours` the
/// intact graph then answered `-32002`. The control removes the planted id
/// and the same request rebuilds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_rebuild_refuses_an_uncompiled_plugin_id_and_keeps_the_workspace_loaded() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_cfg_fixture(&root);
    index_with_include_all_and_macro_options(
        &root,
        sqry_core::graph::unified::build::MacroBuildOptions::default(),
    );
    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    let (_, ws) = server
        .manager
        .find_key_and_workspace_by_path(&root)
        .expect("loaded");
    let storage = GraphStorage::new(&root);
    let clean_manifest = std::fs::read(storage.manifest_path()).expect("manifest bytes");
    let mut manifest = storage.load_manifest().expect("manifest");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push("f2-uncompiled-plugin".to_string());
    manifest
        .save(storage.manifest_path())
        .expect("manifest rewritten");
    let manifest_sha = sha256_of(storage.manifest_path());
    let snapshot_sha = sha256_of(storage.snapshot_path());
    let graph_before = ws.graph();

    let resp = client
        .request("daemon/rebuild", json!({ "path": &path, "force": true }))
        .await;
    let err = expect_error(&resp);
    println!(
        "code={} state={:?} retry_count={} last_error={:?}",
        err.code,
        ws.load_state(),
        ws.retry_count.load(std::sync::atomic::Ordering::Acquire),
        ws.last_error.read().as_ref().map(ToString::to_string)
    );
    assert_eq!(
        err.code,
        sqry_daemon::JSONRPC_WORKSPACE_INCOMPATIBLE_GRAPH,
        "{err:?}"
    );
    assert!(
        err.message.contains("f2-uncompiled-plugin"),
        "{}",
        err.message
    );
    assert_eq!(
        ws.load_state(),
        WorkspaceState::Loaded,
        "a rebuild refused before anything was written leaves the workspace Loaded"
    );
    assert_eq!(
        ws.retry_count.load(std::sync::atomic::Ordering::Acquire),
        0,
        "a refusal counts no failed attempt"
    );
    assert!(
        ws.last_error.read().is_none(),
        "a refusal records no failure"
    );
    assert!(
        Arc::ptr_eq(&graph_before, &ws.graph()),
        "the resident graph is the one published before"
    );
    assert_eq!(sha256_of(storage.manifest_path()), manifest_sha);
    assert_eq!(sha256_of(storage.snapshot_path()), snapshot_sha);
    let row = workspace_state_row(
        expect_success(&client.request("daemon/status", json!({})).await),
        &path,
    );
    assert_eq!(row["state"], json!("Loaded"), "row: {row}");
    assert_eq!(row["last_error"], json!(null), "row: {row}");

    // Control: without the planted id the same request rebuilds.
    std::fs::write(storage.manifest_path(), &clean_manifest).expect("restore the manifest");
    let resp = client
        .request("daemon/rebuild", json!({ "path": &path, "force": true }))
        .await;
    expect_success(&resp);
    assert!(
        !Arc::ptr_eq(&graph_before, &ws.graph()),
        "the control publishes a new graph"
    );
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);

    drop(client);
    server.stop().await;
}

// ---------------------------------------------------------------------------
// F4 and F5: each `daemon/rebuild` caller is answered from its own
// iteration's report, as soon as that iteration ends, within a bound that
// applies to the handler's wait and never to the rebuild.
// ---------------------------------------------------------------------------

/// A production server over an `include_all` index of the cfg fixture, the
/// workspace loaded through `daemon/load`.
async fn loaded_cfg_fixture() -> (
    TempDir,
    std::path::PathBuf,
    String,
    TestServer,
    TestIpcClient,
) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_cfg_fixture(&root);
    index_with_include_all_and_macro_options(
        &root,
        sqry_core::graph::unified::build::MacroBuildOptions::default(),
    );
    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    (tmp, root, path, server, client)
}

/// Send `params` as `daemon/rebuild` on a connection of its own.
fn spawn_rebuild(
    server: &TestServer,
    params: serde_json::Value,
) -> tokio::task::JoinHandle<sqry_daemon::JsonRpcResponse> {
    let socket = server.path.clone();
    tokio::spawn(async move {
        let mut c = TestIpcClient::connect(&socket).await;
        c.hello(1).await;
        c.request("daemon/rebuild", params).await
    })
}

/// The auditor's experiment: the runner's answer used to be read from the
/// slot after the whole drain loop, so it described a later request's
/// iteration. It must describe the generation its own iteration published.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_runner_is_answered_with_its_own_iterations_counts() {
    use std::time::Duration;
    let (_tmp, root, path, server, client) = loaded_cfg_fixture().await;
    let capture = Arc::new(sqry_daemon::TestCapture::default());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .unwrap();
    capture.arm_post_publish_hold();
    let (key, ws) = server
        .manager
        .find_key_and_workspace_by_path(&root)
        .expect("loaded");

    let runner = spawn_rebuild(&server, json!({ "path": &path, "force": true }));
    tokio::time::timeout(Duration::from_secs(120), capture.wait_until_post_publish())
        .await
        .expect("the runner's own iteration publishes");
    let own = ws.graph();
    let (own_files, own_nodes) = (own.files().len() as u64, own.node_count() as u64);

    // After the runner's own publish, a second request with more files
    // parks behind it.
    std::fs::write(
        root.join("src").join("extra.rs"),
        "pub fn extra_one() {}\npub fn extra_two() {}\npub fn extra_three() {}\n",
    )
    .unwrap();
    server
        .dispatcher
        .handle_changes(
            &key,
            ChangeSet {
                changed_files: vec![root.join("src").join("extra.rs")],
                git_state_changed: false,
                git_change_class: None,
            },
        )
        .await
        .expect("the second request parks");
    assert!(ws.rebuild_lane.lock().await.is_some(), "parked");
    capture.release_post_publish();

    let resp = runner.await.expect("runner task");
    let result = expect_success(&resp);
    println!("own iteration: files={own_files} nodes={own_nodes}; runner answered {result}");
    assert_eq!(
        result["result"]["files_indexed"].as_u64(),
        Some(own_files),
        "the runner's answer must describe its own iteration"
    );
    assert_eq!(result["result"]["nodes"].as_u64(), Some(own_nodes));
    // Control: the second iteration did run and published more.
    assert!(
        support::wait_until(
            || ws.graph().files().len() as u64 > own_files,
            Duration::from_secs(60)
        )
        .await,
        "the parked request's iteration publishes after the runner's answer"
    );
    drop(client);
    server.stop().await;
}

/// The auditor's experiment: the runner's answer used to wait for every
/// request parked behind it, although its own outcome was delivered when its
/// own iteration ended. It must arrive while another request's iteration is
/// still held.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_runner_is_answered_before_the_iterations_parked_behind_it() {
    use std::time::Duration;
    let (_tmp, root, path, server, client) = loaded_cfg_fixture().await;
    let capture = Arc::new(sqry_daemon::TestCapture::default());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .unwrap();
    capture.arm_post_publish_hold();
    let (key, ws) = server
        .manager
        .find_key_and_workspace_by_path(&root)
        .expect("loaded");
    let mut runner = spawn_rebuild(&server, json!({ "path": &path, "force": true }));
    tokio::time::timeout(Duration::from_secs(120), capture.wait_until_post_publish())
        .await
        .expect("the runner's own iteration publishes");
    server
        .dispatcher
        .handle_changes(
            &key,
            ChangeSet {
                changed_files: Vec::new(),
                git_state_changed: false,
                git_change_class: None,
            },
        )
        .await
        .expect("the second request parks");
    assert!(ws.rebuild_lane.lock().await.is_some(), "parked");
    capture.reset_post_reservation_reached();
    capture.arm_post_reservation_hold();
    capture.release_post_publish();
    tokio::time::timeout(
        Duration::from_secs(120),
        capture.wait_until_post_reservation(),
    )
    .await
    .expect("the second iteration reaches its reservation");
    let early = tokio::time::timeout(Duration::from_secs(10), &mut runner).await;
    println!(
        "runner answered while another request's iteration was held: {}",
        early.is_ok()
    );
    let answered_early = early.is_ok();
    capture.release_post_reservation();
    let resp = match early {
        Ok(joined) => joined.expect("runner task"),
        Err(_) => runner.await.expect("runner task"),
    };
    expect_success(&resp);
    assert!(
        answered_early,
        "the runner's answer must not wait on another request's iteration"
    );
    drop(client);
    server.stop().await;
}

/// `was_full` reports the mode the caller's own iteration ran. A
/// `force: false` request merged with a `force: true` one shares a full
/// iteration and says so; a lone `force: false` request reports the
/// incremental decision. Before, `was_full` echoed the request's own flag.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn was_full_reports_the_mode_the_callers_own_iteration_ran() {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    let (_tmp, root, path, server, mut client) = loaded_cfg_fixture().await;
    let gate = Arc::new(sqry_daemon::TestGate {
        hold: AtomicUsize::new(1),
        release: tokio::sync::Notify::new(),
    });
    let capture = Arc::new(sqry_daemon::TestCapture::default());
    server
        .dispatcher
        .install_test_gate(Arc::clone(&gate))
        .unwrap();
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .unwrap();
    let (key, ws) = server
        .manager
        .find_key_and_workspace_by_path(&root)
        .expect("loaded");
    // A watcher-shaped runner, stalled in the gate.
    let dispatcher = Arc::clone(&server.dispatcher);
    let runner_key = key.clone();
    let runner = tokio::spawn(async move {
        dispatcher
            .handle_changes(
                &runner_key,
                ChangeSet {
                    changed_files: Vec::new(),
                    git_state_changed: false,
                    git_change_class: None,
                },
            )
            .await
    });
    assert!(
        support::wait_until(
            || capture.iterations.lock().len() == 1,
            Duration::from_secs(30)
        )
        .await
    );
    let forced = spawn_rebuild(&server, json!({ "path": &path, "force": true }));
    let unforced = spawn_rebuild(&server, json!({ "path": &path, "force": false }));
    let mut merged = false;
    for _ in 0..1000 {
        if ws
            .rebuild_lane
            .lock()
            .await
            .as_ref()
            .is_some_and(|parked| parked.waiters.pending() == 2)
        {
            merged = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(merged, "the two plain requests merge into one parked entry");
    gate.release.notify_one();
    let _ = runner.await;
    for (label, task) in [("forced", forced), ("unforced", unforced)] {
        let resp = task.await.expect("request task");
        let result = expect_success(&resp);
        assert_eq!(
            result["result"]["was_full"],
            json!(true),
            "{label}: the merged iteration ran full: {result}"
        );
    }
    assert_eq!(capture.iterations.lock()[1].mode, RebuildMode::Full);

    // Control: a lone unforced request reports the incremental decision.
    let resp = client
        .request("daemon/rebuild", json!({ "path": &path, "force": false }))
        .await;
    let result = expect_success(&resp);
    assert_eq!(result["result"]["was_full"], json!(false), "{result}");
    assert_eq!(
        capture.iterations.lock().last().map(|it| it.mode),
        Some(RebuildMode::Incremental)
    );
    drop(client);
    server.stop().await;
}

/// F5 (P7's class): the handler bounds its own wait. A shortened bound
/// answers both a request queued behind a stalled runner and the runner's
/// own caller with `-32000` carrying the bound; neither rebuild is
/// abandoned: once released, both iterations run and the second publishes.
/// The data says so (round 7 note): `RebuildOutcomeTimeout`, not
/// retryable (a retry queues another rebuild), the rebuild continuing,
/// and `daemon/status` as the read that gives its outcome. It said
/// `retryable: true, retry_after_ms: 500`, the tool timeout's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rebuild_wait_is_bounded_and_the_rebuild_is_never_abandoned() {
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;
    let (_tmp, root, path, server, client) = loaded_cfg_fixture().await;
    assert_eq!(
        server.dispatcher.outcome_wait(),
        sqry_daemon::rebuild::REBUILD_OUTCOME_WAIT,
        "production bound"
    );
    assert_eq!(
        sqry_daemon::rebuild::REBUILD_OUTCOME_WAIT,
        Duration::from_secs(600)
    );
    server
        .dispatcher
        .set_outcome_wait_for_test(Duration::from_millis(300));
    let gate = Arc::new(sqry_daemon::TestGate {
        hold: AtomicUsize::new(1),
        release: tokio::sync::Notify::new(),
    });
    let capture = Arc::new(sqry_daemon::TestCapture::default());
    server
        .dispatcher
        .install_test_gate(Arc::clone(&gate))
        .unwrap();
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .unwrap();
    let (_, ws) = server
        .manager
        .find_key_and_workspace_by_path(&root)
        .expect("loaded");
    let before = server.dispatcher.dispatched_count();

    // The runner's own caller: its iteration is stalled in the gate.
    let runner = spawn_rebuild(&server, json!({ "path": &path, "force": true }));
    // A request queued behind it.
    assert!(
        support::wait_until(
            || capture.iterations.lock().len() == 1,
            Duration::from_secs(30)
        )
        .await
    );
    let queued = spawn_rebuild(&server, json!({ "path": &path, "force": false }));
    // The daemon-hosted `rebuild_index` over the resident workspace waits
    // behind the stalled runner the same way, and its envelope names the
    // read an MCP caller makes instead.
    let running = support::rebuild_fixtures::mcp_session(&server).await;
    let mcp_root = root.clone();
    let mcp_peer = running.peer().clone();
    let mcp = tokio::spawn(async move {
        support::rebuild_fixtures::mcp_rebuild_index(&mcp_peer, &mcp_root, true, &[]).await
    });
    let mcp_err = support::rebuild_fixtures::mcp_error(
        "rebuild_index",
        tokio::time::timeout(Duration::from_secs(20), mcp)
            .await
            .expect("rebuild_index: the wait must be bounded")
            .expect("rebuild_index task"),
    );
    println!("rebuild_index: {} {:?}", mcp_err.message, mcp_err.data);
    let mcp_data = mcp_err.data.clone().expect("data");
    assert_eq!(mcp_data["retryable"], json!(false), "{mcp_data}");
    assert_eq!(
        mcp_data["details"]["tool"],
        json!("rebuild_index"),
        "{mcp_data}"
    );
    assert_eq!(
        mcp_data["details"]["rebuild_continues"],
        json!(true),
        "{mcp_data}"
    );
    assert!(
        mcp_data["details"]["follow_with"]
            .as_str()
            .is_some_and(|read| read.starts_with("rebuild_index with force: false")),
        "{mcp_data}"
    );
    for (label, task) in [("runner", runner), ("queued", queued)] {
        let resp = tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap_or_else(|_| panic!("{label}: the wait must be bounded"))
            .expect("request task");
        let err = expect_error(&resp);
        assert_eq!(err.code, -32000, "{label}: {err:?}");
        let data = err.data.clone().expect("error.data");
        assert_eq!(
            data["details"]["deadline_ms"],
            json!(300),
            "{label}: {data}"
        );
        println!("{label}: {} {data}", err.message);
        assert_eq!(data["retryable"], json!(false), "{label}: {data}");
        assert_eq!(data["retry_after_ms"], json!(null), "{label}: {data}");
        assert_eq!(
            data["details"]["rebuild_continues"],
            json!(true),
            "{label}: {data}"
        );
        assert!(
            data["details"]["follow_with"]
                .as_str()
                .is_some_and(|read| read.starts_with("daemon/status")),
            "{label}: {data}"
        );
        assert!(
            err.message.contains("the rebuild continues"),
            "{label}: {err:?}"
        );
    }
    assert!(
        ws.rebuild_in_flight
            .load(std::sync::atomic::Ordering::Acquire)
    );
    gate.release.notify_one();
    assert!(
        support::wait_until(
            || server.dispatcher.dispatched_count() >= before + 2,
            Duration::from_secs(120)
        )
        .await,
        "both iterations run and publish although nobody waits"
    );
    assert_eq!(capture.iterations.lock().len(), 2);
    drop(running);
    drop(client);
    server.stop().await;
}

/// The answer comes from the report of the generation the caller's own
/// iteration published, never from a read of the slot: held after its
/// publish, the runner's iteration finds the slot republished with a
/// different generation (an eviction and a reload, T47's plant), and its
/// answer still carries its own counts.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_runner_is_answered_from_its_report_even_when_the_slot_moved_on() {
    use std::time::Duration;
    const DOUBLE_NODES: u32 = 4321;
    let (_tmp, root, path, server, client) = loaded_cfg_fixture().await;
    let capture = Arc::new(sqry_daemon::TestCapture::default());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .unwrap();
    capture.arm_post_publish_hold();
    let (key, ws) = server
        .manager
        .find_key_and_workspace_by_path(&root)
        .expect("loaded");
    let runner = spawn_rebuild(&server, json!({ "path": &path, "force": true }));
    tokio::time::timeout(Duration::from_secs(120), capture.wait_until_post_publish())
        .await
        .expect("the runner's own iteration publishes");
    let own_nodes = capture.published_generations.lock()[0].graph.node_count() as u64;
    assert!(server.manager.evict_for_test(&key), "evicted");
    server
        .manager
        .get_or_load(
            &key,
            &sqry_daemon::workspace::builder::FunctionGraphBuilder::with_fast_path_record(
                DOUBLE_NODES,
            ),
            0,
        )
        .expect("a second generation publishes");
    assert_eq!(
        ws.graph().node_count(),
        DOUBLE_NODES as usize,
        "the slot moved on"
    );
    assert_ne!(own_nodes, u64::from(DOUBLE_NODES));
    capture.release_post_publish();
    let resp = runner.await.expect("runner task");
    let result = expect_success(&resp);
    assert_eq!(
        result["result"]["nodes"].as_u64(),
        Some(own_nodes),
        "the answer describes the generation its own iteration published: {result}"
    );
    drop(client);
    server.stop().await;
}
