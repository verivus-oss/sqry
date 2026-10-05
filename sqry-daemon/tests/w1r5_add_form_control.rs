//! ADD-form negative control for surface parity W1, round 5 (designs D24,
//! D25). The guard governs plugin ids recorded in a workspace manifest; the
//! control plants an id no build of sqry compiles, `w1-r5-planted-plugin`,
//! into an otherwise valid fast-path manifest beside a valid snapshot and
//! requires refusal by name with nothing written on every daemon path round
//! 5 touches, so that observing a workspace's state and generation under one
//! guard relabels no refusal:
//!
//! 1. `daemon/load` over the planted manifest, twice (S45): the first call
//!    is `-32005` naming the id and the manifest (the resolver refuses before
//!    the build; the `LoadingGuard` leaves the row `Failed`), the second call
//!    takes the compare-exchange from `Failed` and is `-32005` again, and at
//!    no point is the row `Loaded` (the gate's helper never sees a `Loaded`
//!    slot: the refusal is upstream of it);
//! 2. daemon-hosted `rebuild_index`, `force: false`, over the planted
//!    manifest with the workspace not resident (S46): `-32005` from
//!    `roster_for`, and nothing is resident for the key afterwards (the
//!    resolution `resident_snapshot` performs, observed through `lookup`,
//!    which shares it), no `plugin_selection_warning` key in any output;
//! 3. the CLI hook directly over the planted manifest (S49): T49 in
//!    `sqry-cli/src/commands/query.rs`;
//! 4. the manifest bytes are unchanged after 1 and 2.
//!
//! Every leg is green on the pre-round-5 head (the refusals are round 1 to
//! round 3 code) and is a declared control: its purpose is that S45, S46 and
//! S49 change none of them.
//!
//! Round 6 adds one leg under its own test name (plan "The ADD-form negative
//! control (round 6)"): after a refused `daemon/load` leaves the row
//! `Failed`, `try_evict_for_test` answers `Evicted` (a `Failed` row is an
//! entry; the tombstone afterwards is `Evicted` with no record) and a second
//! call answers `Evicted` again (the row is not removed, so
//! `evict_to_tombstone_locked` takes its early return on the `Evicted` slot;
//! never `Absent`); the refusal path and the eviction hook compose without a
//! record appearing anywhere and without a byte written. Green on both
//! heads: a declared control whose purpose is that S54 and S55 change none
//! of it. A seat substitutes its own id for `R6_PLANTED_ID`.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use sqry_core::graph::unified::build::{BuildConfig, build_and_persist_graph_with_progress};
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_core::progress::no_op_reporter;
use sqry_core::project::ProjectRootMode;
use sqry_daemon::ipc::framing::{read_frame_json, write_frame_json};
use sqry_daemon::workspace::manager::TryEvictOutcome;
use sqry_daemon::{
    DaemonConfig, RealWorkspaceBuilder, WorkspaceBuilder, WorkspaceKey, WorkspaceRosterResolver,
    WorkspaceState,
};
use sqry_daemon_protocol::{ShimProtocol, ShimRegister, ShimRegisterAck};
use sqry_plugin_registry::{create_plugin_manager, create_plugin_manager_all};
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;

const PLANTED_ID: &str = "w1-r5-planted-plugin";
/// The round 6 leg's id (the implementer's seat; a reviewing seat plants its
/// own `w1-r6-<seat>-added-plugin`).
const R6_PLANTED_ID: &str = "w1-r6-impl-added-plugin";
/// The round 7 leg's id (the implementer's seat; a reviewing seat plants its
/// own `w1-r7-<seat>-added-plugin`).
const R7_PLANTED_ID: &str = "w1-r7-impl-added-plugin";

fn write_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
    )
    .expect("write lib.rs");
}

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
        "test:w1r5_add_form",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("fast_path_default".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("fast-path index persists");
}

/// Plant `id` into the recorded selection; returns the manifest bytes
/// after planting.
fn plant_id(root: &Path, id: &str) -> Vec<u8> {
    let storage = GraphStorage::new(root);
    let mut manifest = storage.load_manifest().expect("manifest");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push(id.to_string());
    manifest
        .save(storage.manifest_path())
        .expect("manifest rewritten");
    std::fs::read(storage.manifest_path()).expect("manifest bytes")
}

/// Sorted listing of every file under `<root>/.sqry` with its bytes.
fn index_dir_listing(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, prefix: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("dir entry").path();
            let rel = path.strip_prefix(prefix).expect("under prefix");
            if path.is_dir() {
                walk(&path, prefix, out);
            } else {
                out.push((
                    rel.display().to_string(),
                    std::fs::read(&path).expect("file bytes"),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join(".sqry"), root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

async fn server_with_production_resolver() -> TestServer {
    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver).await
}

/// Call the daemon-hosted `rebuild_index` tool over the MCP shim (it is not
/// an IPC method) and return the MCP error envelope.
async fn mcp_rebuild_index_error(
    server: &TestServer,
    path: &str,
    force: bool,
) -> rmcp::model::ErrorData {
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
    let outcome = running
        .peer()
        .call_tool(
            rmcp::model::CallToolRequestParams::new("rebuild_index").with_arguments(
                serde_json::Map::from_iter([
                    ("path".to_string(), json!(path)),
                    ("force".to_string(), json!(force)),
                ]),
            ),
        )
        .await;
    drop(running);
    match outcome {
        Ok(result) => panic!("rebuild_index must refuse the planted id; survived {result:?}"),
        Err(rmcp::ServiceError::McpError(err)) => err,
        Err(other) => panic!("expected an MCP error envelope, got {other:?}"),
    }
}

fn status_row(status: &serde_json::Value, path: &str) -> serde_json::Value {
    status["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"].as_str() == Some(path))
        })
        .cloned()
        .expect("workspace row present")
}

/// Assert one `daemon/load` refusal: `-32005`, the unknown-id label with
/// the planted id, the manifest named, no snapshot-format relabelling, no
/// `plugin_selection_warning` anywhere in the rendered error.
fn assert_load_refusal(
    leg: &str,
    err: &sqry_daemon::ipc::protocol::JsonRpcError,
    manifest_path: &str,
    id: &str,
) {
    assert_eq!(err.code, -32005, "{leg}: {err:?}");
    // The resolver's refusal (before any build) reads `unknown plugin ids:
    // <id> (this binary supports: ..)`; the bracketed form belongs to
    // `load_persisted`'s refusal on the evicted reload.
    assert!(
        err.message.contains("unknown plugin ids") && err.message.contains(id),
        "{leg} must carry the unknown-id label and the id: {}",
        err.message
    );
    assert!(
        err.message.contains(manifest_path),
        "{leg} must name the manifest: {}",
        err.message
    );
    assert!(
        !err.message.contains("incompatible snapshot format"),
        "{leg} must not be relabelled as a snapshot-format mismatch: {}",
        err.message
    );
    let rendered = serde_json::to_string(err).expect("json");
    assert!(
        !rendered.contains("plugin_selection_warning"),
        "{leg}: a refusal carries no plugin_selection_warning; envelope: {rendered}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w1r5_add_form_control_daemon_paths_refuse_the_planted_id_by_name() {
    assert!(
        create_plugin_manager_all()
            .plugin_by_id(PLANTED_ID)
            .is_none(),
        "the control needs an id no build compiles"
    );
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    index_fast_path(&root);
    let planted = plant_id(&root, PLANTED_ID);
    let listing_planted = index_dir_listing(&root);
    let storage = GraphStorage::new(&root);
    let manifest_path = storage.manifest_path().display().to_string();
    let path = root.to_string_lossy().to_string();
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);

    // Leg 1: daemon/load over the planted manifest, twice. The first call
    // is refused by the resolver before any build and the row is left
    // Failed; the second call takes the CAS from Failed and is refused the
    // same way. The row is never Loaded, so the gate's helper never sees
    // a Loaded slot for this key.
    let server = server_with_production_resolver().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let mut states = Vec::new();
    for call in 1..=2 {
        let leg = format!("leg 1 call {call}");
        let err = expect_error(
            &client
                .request("daemon/load", json!({ "index_root": &path }))
                .await,
        )
        .clone();
        assert_load_refusal(&leg, &err, &manifest_path, PLANTED_ID);
        let status = client.request("daemon/status", json!({})).await;
        let row = status_row(expect_success(&status), &path);
        assert_ne!(
            row["state"],
            json!("Loaded"),
            "{leg}: a refused load must not publish; row: {row}"
        );
        assert_eq!(
            row["state"],
            json!("Failed"),
            "{leg}: the LoadingGuard leaves the row Failed; row: {row}"
        );
        assert!(
            row["plugin_roster"].is_null(),
            "{leg}: a refused load publishes no record; row: {row}"
        );
        assert_eq!(
            index_dir_listing(&root),
            listing_planted,
            "{leg} wrote nothing"
        );
        states.push(row["state"].clone());
        println!(
            "ADD-FORM r5 daemon/load call {call}: code={} message={} state={}",
            err.code, err.message, row["state"]
        );
    }
    assert_eq!(states, vec![json!("Failed"), json!("Failed")]);
    let resident = server
        .manager
        .lookup(&key)
        .expect("the Failed row is resident");
    assert_ne!(
        resident.load_state(),
        sqry_daemon::WorkspaceState::Loaded,
        "leg 1: the slot is never Loaded"
    );
    drop(client);
    server.stop().await;

    // Leg 2: daemon-hosted rebuild_index, force: false, over the planted
    // manifest with the workspace not resident (a fresh server): refused
    // by roster_for before any envelope; nothing is resident for the key
    // afterwards, so the cache-hit leg had nothing to observe.
    let server = server_with_production_resolver().await;
    assert!(
        server.manager.lookup(&key).is_none(),
        "leg 2 precondition: the workspace is not resident"
    );
    let err = mcp_rebuild_index_error(&server, &path, false).await;
    let data = err.data.clone().expect("the refusal carries data");
    assert_eq!(
        data["kind"],
        json!("workspace_incompatible_graph"),
        "leg 2: {err:?}"
    );
    let reason = data["details"]["reason"]
        .as_str()
        .expect("details.reason is a string")
        .to_string();
    assert!(
        reason.contains(PLANTED_ID),
        "leg 2 must name the id: {reason}"
    );
    assert!(
        reason.contains(&manifest_path),
        "leg 2 must name the manifest: {reason}"
    );
    let rendered = serde_json::to_string(&json!({
        "code": err.code.0,
        "message": err.message,
        "data": data,
    }))
    .expect("json");
    assert!(
        !rendered.contains("plugin_selection_warning"),
        "leg 2: a refusal carries no plugin_selection_warning; envelope: {rendered}"
    );
    assert!(
        server.manager.lookup(&key).is_none(),
        "leg 2: nothing is resident for the key after the refusal"
    );
    assert_eq!(
        index_dir_listing(&root),
        listing_planted,
        "leg 2 wrote nothing"
    );
    println!(
        "ADD-FORM r5 daemon rebuild_index(force=false, not resident): reason={reason} resident={}",
        server.manager.lookup(&key).is_some()
    );
    server.stop().await;

    // Leg 4: nothing rewrote the manifest (leg 3 is T49, a unit test of
    // the CLI crate).
    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        planted,
        "no round 5 daemon path may rewrite the manifest while refusing it"
    );
}

/// Round 6 leg: the refused `daemon/load` row and the eviction hook compose
/// (module doc). Green on both heads; a declared control.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w1r6_add_form_control_refused_row_composes_with_the_eviction_hook() {
    assert!(
        create_plugin_manager_all()
            .plugin_by_id(R6_PLANTED_ID)
            .is_none(),
        "the control needs an id no build compiles"
    );
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    index_fast_path(&root);
    let planted = plant_id(&root, R6_PLANTED_ID);
    let listing_planted = index_dir_listing(&root);
    let storage = GraphStorage::new(&root);
    let manifest_path = storage.manifest_path().display().to_string();
    let path = root.to_string_lossy().to_string();
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);

    let server = server_with_production_resolver().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let err = expect_error(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    )
    .clone();
    assert_load_refusal("r6 leg daemon/load", &err, &manifest_path, R6_PLANTED_ID);
    let status = client.request("daemon/status", json!({})).await;
    let row = status_row(expect_success(&status), &path);
    assert_eq!(
        row["state"],
        json!("Failed"),
        "r6 leg: the LoadingGuard leaves the row Failed; row: {row}"
    );

    let resident = server
        .manager
        .lookup(&key)
        .expect("the Failed row is resident");
    let before = (resident.load_state(), resident.roster().is_some());
    let first = server.manager.try_evict_for_test(&key);
    let after = server
        .manager
        .lookup(&key)
        .expect("the tombstone stays in the map after the first call");
    let after_state = after.load_state();
    let after_record = after.roster().is_some();
    let second = server.manager.try_evict_for_test(&key);
    let again = server
        .manager
        .lookup(&key)
        .expect("the tombstone stays in the map after the second call");
    let again_state = again.load_state();
    let again_record = again.roster().is_some();
    println!(
        "ADD-FORM r6 refused row + eviction hook: code={} message={} before={before:?} \
         first={first:?} after_state={after_state:?} after_record={after_record} \
         second={second:?} again_state={again_state:?} again_record={again_record}",
        err.code, err.message
    );
    assert_eq!(
        before,
        (WorkspaceState::Failed, false),
        "a refused load leaves a Failed row with no record"
    );
    assert_eq!(
        first,
        TryEvictOutcome::Evicted,
        "a Failed row is an entry: the hook evicts it to the tombstone"
    );
    assert_eq!(
        (after_state, after_record),
        (WorkspaceState::Evicted, false),
        "the tombstone: Evicted, holding the placeholder with no record"
    );
    assert_eq!(
        second,
        TryEvictOutcome::Evicted,
        "the row is not removed: the second call answers Evicted through the early return \
         on an already-Evicted slot, never Absent"
    );
    assert_eq!(
        (again_state, again_record),
        (WorkspaceState::Evicted, false),
        "the second call leaves the tombstone as it was"
    );
    assert_eq!(
        index_dir_listing(&root),
        listing_planted,
        "r6 leg wrote nothing"
    );
    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        planted,
        "the refusal and the eviction hook rewrite nothing"
    );
    drop(client);
    server.stop().await;
}

/// Round 7 leg (design D37, plan "The ADD-form negative control (round 7)"):
/// the tombstone a refused row leaves is not rebuildable. This is the one
/// place where round 7's repair and the ADD-form guard meet: the refusal
/// leaves the row `Failed`, the eviction hook turns it into a tombstone, and
/// a rebuild iteration for the same key must answer `WorkspaceEvicted` and
/// leave the row exactly as the eviction left it.
///
/// The first `handle_changes` is the one that consumes the cancellation flag
/// eviction set, at the top-of-loop gate; the second is the measurement of
/// the iteration's entry store, which is battery row C72.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w1r7_add_form_control_tombstoned_row_refuses_a_rebuild_iteration() {
    assert!(
        create_plugin_manager_all()
            .plugin_by_id(R7_PLANTED_ID)
            .is_none(),
        "the control needs an id no build compiles"
    );
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    index_fast_path(&root);
    let planted = plant_id(&root, R7_PLANTED_ID);
    let storage = GraphStorage::new(&root);
    let manifest_path = storage.manifest_path().display().to_string();
    let path = root.to_string_lossy().to_string();
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);

    let server = server_with_production_resolver().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let err = expect_error(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    )
    .clone();
    assert_load_refusal("r7 leg daemon/load", &err, &manifest_path, R7_PLANTED_ID);

    let resident = server
        .manager
        .lookup(&key)
        .expect("the Failed row is resident");
    let before = (resident.load_state(), resident.roster().is_some());
    let evicted = server.manager.try_evict_for_test(&key);
    let tombstone = (resident.load_state(), resident.roster().is_some());

    let changes = || sqry_core::watch::ChangeSet {
        changed_files: vec![root.join("src").join("lib.rs")],
        git_state_changed: false,
        git_change_class: None,
    };
    let first = server.dispatcher.handle_changes(&key, changes()).await;
    let between = (resident.load_state(), resident.roster().is_some());
    let second = server.dispatcher.handle_changes(&key, changes()).await;
    let after = (
        resident.load_state(),
        resident.roster().is_some(),
        resident.graph().node_count(),
    );

    println!(
        "ADD-FORM r7 tombstoned row + rebuild iteration: code={} message={} before={before:?} \
         evicted={evicted:?} tombstone={tombstone:?} first_is_evicted={} between={between:?} \
         second_is_evicted={} after={after:?}",
        err.code,
        err.message,
        matches!(
            first,
            Err(sqry_daemon::DaemonError::WorkspaceEvicted { .. })
        ),
        matches!(
            second,
            Err(sqry_daemon::DaemonError::WorkspaceEvicted { .. })
        ),
    );

    assert_eq!(
        before,
        (WorkspaceState::Failed, false),
        "a refused load leaves a Failed row with no record"
    );
    assert_eq!(
        evicted,
        TryEvictOutcome::Evicted,
        "a Failed row is an entry: the hook evicts it to the tombstone"
    );
    assert_eq!(
        tombstone,
        (WorkspaceState::Evicted, false),
        "the tombstone: Evicted, holding the placeholder with no record"
    );
    assert!(
        matches!(
            first,
            Err(sqry_daemon::DaemonError::WorkspaceEvicted { .. })
        ),
        "the top-of-loop gate must answer WorkspaceEvicted: {first:?}"
    );
    assert_eq!(
        between,
        (WorkspaceState::Evicted, false),
        "the gate must not rewrite the tombstone"
    );
    assert!(
        matches!(
            second,
            Err(sqry_daemon::DaemonError::WorkspaceEvicted { .. })
        ),
        "a rebuild iteration that begins on the tombstone must answer WorkspaceEvicted: {second:?}"
    );
    assert_eq!(
        after,
        (WorkspaceState::Evicted, false, 0),
        "the iteration must leave the tombstone exactly as the eviction left it"
    );
    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        planted,
        "no leg of this control rewrites the manifest"
    );
}
