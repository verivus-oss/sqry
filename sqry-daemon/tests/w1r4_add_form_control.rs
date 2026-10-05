//! ADD-form negative control for surface parity W1, round 4 (design D20).
//! The guard governs plugin ids recorded in a workspace manifest; the
//! control plants an id no build of sqry compiles, `w1-r4-planted-plugin`,
//! into an otherwise valid fast-path manifest beside a valid snapshot and
//! requires refusal by name with nothing written on every daemon path round
//! 4 touches, so that handing readers the published pair relabels no
//! refusal:
//!
//! 1. the evicted reload through the pair (S34): after `daemon/load`, the
//!    planted id written into the manifest, `evict_for_test`, one
//!    `semantic_search`: `-32005` with `unknown plugin ids: [..]`, state
//!    not `Loaded` (the round 2 leg 6, re-run through the new arm);
//! 2. daemon-hosted `rebuild_index`, `force: false`, after the eviction
//!    over the planted manifest (S35): the incompatible-graph refusal
//!    naming the id and the manifest from `roster_for`, before any
//!    envelope, no `plugin_selection_warning` key in any output;
//! 3. daemon-hosted `rebuild_index`, `force: true`, over the planted
//!    manifest (S35): the same refusal from the builder's resolver, nothing
//!    published, the `daemon/status` row not `Loaded`;
//! 4. the manifest bytes are unchanged after 1 to 3.
//!
//! Every leg is green on the pre-round-4 head (the refusals are round 1
//! and round 3 code) and is a declared control: its purpose is that S34
//! and S35 change none of them.

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
use sqry_daemon::{
    DaemonConfig, RealWorkspaceBuilder, WorkspaceBuilder, WorkspaceKey, WorkspaceRosterResolver,
};
use sqry_daemon_protocol::{ShimProtocol, ShimRegister, ShimRegisterAck};
use sqry_plugin_registry::{create_plugin_manager, create_plugin_manager_all};
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;

const PLANTED_ID: &str = "w1-r4-planted-plugin";

fn write_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
    )
    .expect("write lib.rs");
    std::fs::write(
        root.join("config.json"),
        br#"{"name": "fixture", "nested": {"enabled": true}}"#,
    )
    .expect("write config.json");
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
        "test:w1r4_add_form",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("fast_path_default".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("fast-path index persists");
}

/// Plant `PLANTED_ID` into the recorded selection; returns the manifest
/// bytes after planting.
fn plant_id(root: &Path) -> Vec<u8> {
    let storage = GraphStorage::new(root);
    let mut manifest = storage.load_manifest().expect("manifest");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push(PLANTED_ID.to_string());
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

/// Assert the MCP form of the daemon's `-32005` refusal: `kind` is
/// `workspace_incompatible_graph`, the reason names the id and the
/// manifest, and nothing in the envelope is a `plugin_selection_warning`.
fn assert_incompatible_refusal(
    leg: &str,
    err: &rmcp::model::ErrorData,
    manifest_path: &str,
) -> String {
    let data = err.data.clone().expect("the refusal carries data");
    assert_eq!(
        data["kind"],
        json!("workspace_incompatible_graph"),
        "{leg}: {err:?}"
    );
    let reason = data["details"]["reason"]
        .as_str()
        .expect("details.reason is a string")
        .to_string();
    assert!(
        reason.contains(PLANTED_ID),
        "{leg} must name the id: {reason}"
    );
    assert!(
        reason.contains(manifest_path),
        "{leg} must name the manifest: {reason}"
    );
    let rendered = serde_json::to_string(&json!({
        "code": err.code.0,
        "message": err.message,
        "data": data,
    }))
    .expect("json");
    assert!(
        !rendered.contains("plugin_selection_warning"),
        "{leg}: a refusal carries no plugin_selection_warning; envelope: {rendered}"
    );
    reason
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w1r4_add_form_control_daemon_paths_refuse_the_planted_id_by_name() {
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
    let storage = GraphStorage::new(&root);
    let manifest_path = storage.manifest_path().display().to_string();
    let path = root.to_string_lossy().to_string();

    // Leg 1: the evicted reload through the pair. Clean manifest loaded,
    // id planted, workspace evicted, the next tool call is the -32005
    // refusal with the unknown-id label and without the snapshot-format
    // label; the reload publishes nothing.
    let server = server_with_production_resolver().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    let planted = plant_id(&root);
    let listing_planted = index_dir_listing(&root);
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    assert!(
        server.manager.evict_for_test(&key),
        "workspace was resident"
    );
    let search_err = expect_error(
        &client
            .request(
                "semantic_search",
                json!({
                    "query": "kind:function",
                    "path": &path,
                    "max_results": 10,
                    "context_lines": 0,
                    "include_classpath": false,
                }),
            )
            .await,
    )
    .clone();
    assert_eq!(search_err.code, -32005, "leg 1: {search_err:?}");
    assert!(
        search_err
            .message
            .contains(&format!("unknown plugin ids: [{PLANTED_ID}]")),
        "leg 1 must carry the unknown-id label: {}",
        search_err.message
    );
    assert!(
        !search_err.message.contains("incompatible snapshot format"),
        "leg 1 must not be relabelled as a snapshot-format mismatch: {}",
        search_err.message
    );
    let status = client.request("daemon/status", json!({})).await;
    let row = status_row(expect_success(&status), &path);
    assert_ne!(
        row["state"],
        json!("Loaded"),
        "leg 1: a refused reload must not republish; row: {row}"
    );
    assert!(
        row["plugin_roster"].is_null(),
        "leg 1: a refused reload publishes no record; row: {row}"
    );
    println!(
        "ADD-FORM r4 evicted reload: code={} message={} state={}",
        search_err.code, search_err.message, row["state"]
    );

    // Leg 2: daemon-hosted rebuild_index, force: false, after the
    // eviction, over the planted manifest: refused by roster_for before
    // any envelope.
    let err = mcp_rebuild_index_error(&server, &path, false).await;
    let reason = assert_incompatible_refusal("leg 2", &err, &manifest_path);
    assert_eq!(
        index_dir_listing(&root),
        listing_planted,
        "leg 2 wrote nothing"
    );
    let status = client.request("daemon/status", json!({})).await;
    let row = status_row(expect_success(&status), &path);
    assert_ne!(
        row["state"],
        json!("Loaded"),
        "leg 2: the cache-hit leg builds nothing; row: {row}"
    );
    println!("ADD-FORM r4 daemon rebuild_index(force=false): reason={reason}");

    // Leg 3: daemon-hosted rebuild_index, force: true, over the planted
    // manifest: the builder's resolver refuses, nothing is published.
    let err = mcp_rebuild_index_error(&server, &path, true).await;
    let reason = assert_incompatible_refusal("leg 3", &err, &manifest_path);
    assert_eq!(
        index_dir_listing(&root),
        listing_planted,
        "leg 3 wrote nothing"
    );
    let status = client.request("daemon/status", json!({})).await;
    let row = status_row(expect_success(&status), &path);
    assert_ne!(
        row["state"],
        json!("Loaded"),
        "leg 3: a refused build must not publish; row: {row}"
    );
    assert!(
        row["plugin_roster"].is_null(),
        "leg 3: a refused build publishes no record; row: {row}"
    );
    println!(
        "ADD-FORM r4 daemon rebuild_index(force=true): reason={reason} state={}",
        row["state"]
    );
    drop(client);
    server.stop().await;

    // Leg 4: nothing rewrote the manifest.
    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        planted,
        "no round 4 daemon path may rewrite the manifest while refusing it"
    );
}
