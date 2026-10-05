//! STEP_6 (workspace-aware-cross-repo, 2026-04-26) — direct
//! `WorkspaceManager::workspace_index_status` aggregate-rollup tests.
//!
//! Rather than spinning up the full IPC server (which is expensive
//! and would duplicate `ipc_daemon_load_unload`'s coverage), these
//! tests drive the manager-level aggregate that
//! `daemon/workspaceStatus` serves over JSON-RPC. The wire-form
//! envelope shape is itself covered by the round-trip tests in
//! `sqry-daemon-protocol::protocol::tests`.

// Only `roster_status` uses the shared support module, whose IPC client is a
// Unix socket client: the gate keeps the other tests in this file compiling
// on Windows, as they did before `roster_status` arrived.
#[cfg(all(unix, feature = "test-hooks"))]
mod support;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use sqry_core::project::ProjectRootMode;
use sqry_daemon::{
    DaemonConfig, WorkspaceKey, WorkspaceManager, WorkspaceState, workspace::LoadedWorkspace,
};
use sqry_daemon_protocol::WorkspaceId;

fn make_manager() -> Arc<WorkspaceManager> {
    let config = Arc::new(DaemonConfig::default());
    WorkspaceManager::new_without_reaper(config)
}

fn insert_workspace(
    mgr: &WorkspaceManager,
    key: &WorkspaceKey,
    state: WorkspaceState,
    bytes: usize,
) -> Arc<LoadedWorkspace> {
    mgr.insert_workspace_for_test_with_bytes(key.clone(), state, false, bytes)
}

#[test]
fn daemon_workspace_status_returns_aggregate_for_loaded_workspace() {
    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0x01; 32]);
    let key_a =
        WorkspaceKey::with_workspace_id(id, PathBuf::from("/repos/a"), ProjectRootMode::GitRoot, 0);
    let key_b =
        WorkspaceKey::with_workspace_id(id, PathBuf::from("/repos/b"), ProjectRootMode::GitRoot, 0);

    let _ = insert_workspace(&mgr, &key_a, WorkspaceState::Loaded, 1_234);
    let _ = insert_workspace(&mgr, &key_b, WorkspaceState::Loaded, 5_678);

    let aggregate = mgr.workspace_index_status(&id).expect("aggregate present");
    assert_eq!(aggregate.workspace_id, id);
    assert_eq!(aggregate.source_roots.len(), 2);

    // Sorted lexically by source_root.
    assert_eq!(
        aggregate.source_roots[0].source_root,
        PathBuf::from("/repos/a")
    );
    assert_eq!(aggregate.source_roots[0].state, WorkspaceState::Loaded);
    assert_eq!(aggregate.source_roots[0].current_bytes, 1_234);
    assert_eq!(
        aggregate.source_roots[1].source_root,
        PathBuf::from("/repos/b")
    );
    assert_eq!(aggregate.source_roots[1].state, WorkspaceState::Loaded);
    assert_eq!(aggregate.source_roots[1].current_bytes, 5_678);

    assert!(
        !aggregate.partially_evicted(),
        "fully-loaded aggregate must report partially_evicted == false"
    );
}

#[test]
fn daemon_workspace_status_marks_evicted_per_source_root() {
    // Acceptance criterion #5 in the STEP_6 brief —
    // `daemon/workspaceStatus { workspace_id }` returns the aggregate
    // even when 1+ source roots are Evicted.
    //
    // STEP_6 iter-2 BLOCK fix: drive REAL eviction through
    // `mgr.evict_lru()` (which routes through the production
    // `execute_eviction` code path) instead of synthesising an
    // Evicted state via the test helper. Production
    // `execute_eviction` keeps the Evicted tombstone in the map —
    // this test asserts that contract end-to-end.
    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0x02; 32]);
    let key_a =
        WorkspaceKey::with_workspace_id(id, PathBuf::from("/repos/a"), ProjectRootMode::GitRoot, 0);
    let key_b =
        WorkspaceKey::with_workspace_id(id, PathBuf::from("/repos/b"), ProjectRootMode::GitRoot, 0);

    let _ws_a = insert_workspace(&mgr, &key_a, WorkspaceState::Loaded, 1_000);
    let ws_b = insert_workspace(&mgr, &key_b, WorkspaceState::Loaded, 1_000);

    // Make `key_a` strictly older than `key_b` so the LRU picks it.
    std::thread::sleep(std::time::Duration::from_millis(2));
    ws_b.touch();

    // Drive real per-source-root LRU eviction through the
    // production code path.
    let evicted = mgr.evict_lru().expect("LRU candidate present");
    assert_eq!(evicted, key_a, "LRU victim must be the older source root",);

    let aggregate = mgr.workspace_index_status(&id).expect("aggregate present");
    assert_eq!(
        aggregate.source_roots.len(),
        2,
        "evicted source root must remain in the aggregate as a tombstone",
    );

    // The Evicted source root is still surfaced; aggregate flags
    // partial-eviction.
    let a_status = aggregate
        .source_roots
        .iter()
        .find(|s| s.source_root.as_path() == Path::new("/repos/a"))
        .expect("a present");
    assert_eq!(
        a_status.state,
        WorkspaceState::Evicted,
        "real LRU eviction must transition the entry to Evicted",
    );
    assert_eq!(
        a_status.current_bytes, 0,
        "evicted tombstone must report zero resident bytes",
    );

    let b_status = aggregate
        .source_roots
        .iter()
        .find(|s| s.source_root.as_path() == Path::new("/repos/b"))
        .expect("b present");
    assert_eq!(b_status.state, WorkspaceState::Loaded);

    assert!(
        aggregate.partially_evicted(),
        "Evicted + Loaded aggregate must flag partially_evicted"
    );
}

#[test]
fn daemon_workspace_status_returns_none_for_unknown_workspace_id() {
    // No entry in the manager carries this id ⇒ aggregate is None.
    let mgr = make_manager();
    let unknown = WorkspaceId::from_bytes([0xff; 32]);
    assert!(
        mgr.workspace_index_status(&unknown).is_none(),
        "unknown workspace_id must yield None"
    );
}

#[test]
fn daemon_workspace_status_isolates_by_workspace_id() {
    // Two logical workspaces, two source roots each. Aggregate for
    // one MUST NOT include the other's source roots.
    let mgr = make_manager();
    let id1 = WorkspaceId::from_bytes([0xa1; 32]);
    let id2 = WorkspaceId::from_bytes([0xa2; 32]);

    let k1a = WorkspaceKey::with_workspace_id(
        id1,
        PathBuf::from("/repos/1a"),
        ProjectRootMode::GitRoot,
        0,
    );
    let k1b = WorkspaceKey::with_workspace_id(
        id1,
        PathBuf::from("/repos/1b"),
        ProjectRootMode::GitRoot,
        0,
    );
    let k2a = WorkspaceKey::with_workspace_id(
        id2,
        PathBuf::from("/repos/2a"),
        ProjectRootMode::GitRoot,
        0,
    );
    let k2b = WorkspaceKey::with_workspace_id(
        id2,
        PathBuf::from("/repos/2b"),
        ProjectRootMode::GitRoot,
        0,
    );

    for k in [&k1a, &k1b, &k2a, &k2b] {
        insert_workspace(&mgr, k, WorkspaceState::Loaded, 100);
    }

    let agg1 = mgr.workspace_index_status(&id1).expect("id1 aggregate");
    assert_eq!(
        agg1.source_roots.len(),
        2,
        "id1 has exactly two source roots"
    );
    for sr in &agg1.source_roots {
        let s = sr.source_root.to_string_lossy();
        assert!(
            s.starts_with("/repos/1"),
            "id1 must isolate to /repos/1*; got {s}"
        );
    }

    let agg2 = mgr.workspace_index_status(&id2).expect("id2 aggregate");
    assert_eq!(
        agg2.source_roots.len(),
        2,
        "id2 has exactly two source roots"
    );
    for sr in &agg2.source_roots {
        let s = sr.source_root.to_string_lossy();
        assert!(
            s.starts_with("/repos/2"),
            "id2 must isolate to /repos/2*; got {s}"
        );
    }
}

// ============================================================================
// STEP_12 — daemon/status + daemon/workspaceStatus expose both
// `workspace_id_short` (display) and `workspace_id_full` (machine identity).
//
// Scripts consuming the JSON should key on `_full` to avoid the remote
// possibility of short-hex collisions across hundreds of thousands of
// workspaces. The acceptance contract from the DAG (STEP_12_TELEMETRY):
//
//   "Daemon daemon/status and daemon/workspaceStatus include both
//    workspace_id_short (display) and workspace_id_full (machine
//    identity); scripts consuming JSON should key on workspace_id_full
//    to avoid the remote possibility of short-hex collisions across
//    hundreds of thousands of workspaces."
// ============================================================================

#[test]
fn workspace_index_status_carries_short_and_full_hex() {
    let mgr = make_manager();
    let id_bytes = [0xab; 32];
    let id = WorkspaceId::from_bytes(id_bytes);
    let key = WorkspaceKey::with_workspace_id(
        id,
        PathBuf::from("/repos/short-and-full"),
        ProjectRootMode::GitRoot,
        0,
    );
    insert_workspace(&mgr, &key, WorkspaceState::Loaded, 42);

    let aggregate = mgr.workspace_index_status(&id).expect("aggregate present");

    // Wire-form must surface both fields verbatim.
    assert_eq!(
        aggregate.workspace_id_short,
        id.as_short_hex(),
        "workspace_id_short must be the first 16 hex chars of the digest",
    );
    assert_eq!(
        aggregate.workspace_id_short.len(),
        16,
        "workspace_id_short is exactly 16 hex chars",
    );
    assert_eq!(
        aggregate.workspace_id_full,
        id.as_full_hex(),
        "workspace_id_full must be the full 64-char hex digest",
    );
    assert_eq!(
        aggregate.workspace_id_full.len(),
        64,
        "workspace_id_full is exactly 64 hex chars",
    );
    assert!(
        aggregate
            .workspace_id_full
            .starts_with(&aggregate.workspace_id_short),
        "workspace_id_short must be the prefix of workspace_id_full",
    );

    // Machine identity round-trips bytewise — JSON consumers can rely
    // on the `_full` field for cross-process equality without ever
    // re-deriving from the 32-byte digest.
    let recovered = (0..32)
        .map(|i| {
            u8::from_str_radix(&aggregate.workspace_id_full[i * 2..i * 2 + 2], 16)
                .expect("hex digit")
        })
        .collect::<Vec<_>>();
    assert_eq!(recovered.as_slice(), &id_bytes);
}

#[test]
fn daemon_status_workspace_rows_carry_short_and_full_hex() {
    // The DaemonStatus aggregate (the `daemon/status` payload) must
    // surface the same `_short` + `_full` pair on every workspace row
    // when the underlying WorkspaceKey carries a workspace_id. Rows
    // for anonymous (workspace_id = None) keys carry both fields as
    // `None` so the wire shape is uniform.
    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0xcd; 32]);
    let labelled = WorkspaceKey::with_workspace_id(
        id,
        PathBuf::from("/repos/labelled"),
        ProjectRootMode::GitRoot,
        0,
    );
    let anonymous = WorkspaceKey::new(
        PathBuf::from("/repos/anonymous"),
        ProjectRootMode::GitRoot,
        0,
    );
    insert_workspace(&mgr, &labelled, WorkspaceState::Loaded, 100);
    insert_workspace(&mgr, &anonymous, WorkspaceState::Loaded, 100);

    let status = mgr.status();
    assert_eq!(status.workspaces.len(), 2);

    let labelled_row = status
        .workspaces
        .iter()
        .find(|w| w.index_root.as_path() == Path::new("/repos/labelled"))
        .expect("labelled present");
    assert_eq!(
        labelled_row.workspace_id_short.as_deref(),
        Some(id.as_short_hex().as_str()),
        "labelled row must surface workspace_id_short",
    );
    assert_eq!(
        labelled_row.workspace_id_full.as_deref(),
        Some(id.as_full_hex().as_str()),
        "labelled row must surface workspace_id_full",
    );

    let anonymous_row = status
        .workspaces
        .iter()
        .find(|w| w.index_root.as_path() == Path::new("/repos/anonymous"))
        .expect("anonymous present");
    assert!(
        anonymous_row.workspace_id_short.is_none(),
        "anonymous row carries no workspace_id_short",
    );
    assert!(
        anonymous_row.workspace_id_full.is_none(),
        "anonymous row carries no workspace_id_full",
    );
}

#[test]
fn daemon_status_serializes_short_and_full_into_json() {
    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0xef; 32]);
    let key = WorkspaceKey::with_workspace_id(
        id,
        PathBuf::from("/repos/json-shape"),
        ProjectRootMode::GitRoot,
        0,
    );
    insert_workspace(&mgr, &key, WorkspaceState::Loaded, 200);

    let status = mgr.status();
    let json = serde_json::to_value(&status).expect("serialise daemon/status");
    let row = &json["workspaces"][0];
    assert_eq!(
        row["workspace_id_short"].as_str(),
        Some(id.as_short_hex().as_str()),
    );
    assert_eq!(
        row["workspace_id_full"].as_str(),
        Some(id.as_full_hex().as_str()),
    );
}

#[test]
fn workspace_index_status_serializes_short_and_full_into_json() {
    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0x7a; 32]);
    let key = WorkspaceKey::with_workspace_id(
        id,
        PathBuf::from("/repos/wsstatus-json"),
        ProjectRootMode::GitRoot,
        0,
    );
    insert_workspace(&mgr, &key, WorkspaceState::Loaded, 250);

    let aggregate = mgr.workspace_index_status(&id).expect("present");
    let json = serde_json::to_value(&aggregate).expect("serialise");
    assert_eq!(
        json["workspace_id_short"].as_str(),
        Some(id.as_short_hex().as_str()),
    );
    assert_eq!(
        json["workspace_id_full"].as_str(),
        Some(id.as_full_hex().as_str()),
    );
}

// ---------------------------------------------------------------------------
// T5b (surface parity W1): `daemon/status` reports the resident roster and
// whether it diverges from the workspace manifest.
// ---------------------------------------------------------------------------

// `WorkspaceRosterResolver::pinned` is compiled only under `test-hooks`, so
// this module is gated the same way `rebuild_guard.rs` is; the pre-existing
// tests above stay in the default-feature run.
#[cfg(all(unix, feature = "test-hooks"))]
mod roster_status {
    use std::path::Path;
    use std::sync::Arc;

    use serde_json::json;
    use sqry_core::graph::unified::build::{BuildConfig, build_and_persist_graph_with_progress};
    use sqry_core::graph::unified::persistence::PluginSelectionManifest;
    use sqry_core::progress::no_op_reporter;
    use sqry_daemon::{
        DaemonConfig, RealWorkspaceBuilder, RosterRecord, WorkspaceBuilder, WorkspaceRosterResolver,
    };
    use sqry_plugin_registry::{RosterSource, create_plugin_manager, create_plugin_manager_all};
    use tempfile::TempDir;

    use super::support::ipc::{TestIpcClient, TestServer, expect_success};

    fn write_mixed_fixture(root: &Path) {
        std::fs::create_dir_all(root.join("src")).expect("src dir");
        std::fs::write(root.join("src").join("lib.rs"), b"pub fn alpha() {}\n")
            .expect("write lib.rs");
        std::fs::write(
            root.join("config.json"),
            br#"{"name": "fixture", "on": true}"#,
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

    async fn status_row_for(
        resolver: Arc<WorkspaceRosterResolver>,
        root: &Path,
    ) -> serde_json::Value {
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
        drop(client);
        server.stop().await;
        row
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn daemon_status_reports_a_resident_roster_narrower_than_the_manifest() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        write_mixed_fixture(&root);
        index_with_include_all(&root);

        let fast = Arc::new(create_plugin_manager());
        let record = RosterRecord::from_manager(&fast, RosterSource::Fallback);
        let resolver = Arc::new(WorkspaceRosterResolver::pinned(fast, record));
        let row = status_row_for(resolver, &root).await;

        let roster = &row["plugin_roster"];
        assert!(!roster.is_null(), "plugin_roster must be present: {row}");
        assert_eq!(roster["source"], json!("fallback"));
        assert!(
            !roster["active_plugin_ids"]
                .as_array()
                .expect("ids array")
                .iter()
                .any(|id| id == "json"),
            "the pinned fast-path roster lacks json: {roster}"
        );
        assert_eq!(
            roster["diverges_from_manifest"]["missing_plugin_ids"],
            json!(["json"]),
            "row: {row}"
        );
        assert_eq!(
            roster["diverges_from_manifest"]["extra_plugin_ids"],
            json!([]),
            "row: {row}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn daemon_status_reports_no_divergence_when_the_roster_matches() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical root");
        write_mixed_fixture(&root);
        index_with_include_all(&root);

        let row = status_row_for(Arc::new(WorkspaceRosterResolver::new()), &root).await;
        let roster = &row["plugin_roster"];
        assert!(!roster.is_null(), "plugin_roster must be present: {row}");
        assert_eq!(roster["source"], json!("persisted_manifest"));
        assert_eq!(roster["high_cost_mode"], json!("include_all"));
        assert!(
            roster["active_plugin_ids"]
                .as_array()
                .expect("ids array")
                .iter()
                .any(|id| id == "json"),
            "the resolved roster carries json: {roster}"
        );
        assert!(
            roster.get("diverges_from_manifest").is_none(),
            "a matching roster must carry no divergence block: {roster}"
        );
    }
}
