//! ADD-form negative control for surface parity W1, verifier round 2.
//!
//! The unit's guard governs plugin ids recorded in a workspace manifest.
//! Round 2 added two paths that read that record (the daemon reload after
//! eviction through `load_persisted`, and the registry build helper every
//! non-durable persisting site calls). This control plants a NEW id,
//! `w1-r2-verifier-added-plugin`, that no build of sqry compiles, into an
//! otherwise valid fast-path manifest beside a valid snapshot, and requires
//! each of those paths to refuse it by name with nothing written:
//!
//! 1. the registry helper under both `UnreadableManifestPolicy` values
//!    returns `Selection(UnknownPluginIdsCtx)` naming the id and the
//!    manifest path, and the `.sqry` listing is unchanged;
//! 2. `RealWorkspaceBuilder::load_persisted` returns
//!    `WorkspaceIncompatibleGraph` (`-32005`) naming the id and the manifest
//!    path, and the `.sqry` listing is unchanged;
//! 3. `daemon/load` through the production resolver returns `-32005`
//!    naming the id;
//! 4. after a successful load of the clean manifest, the id is planted and
//!    the workspace evicted: the next tool call is refused with `-32005`
//!    naming the id and `daemon/status` does not list the workspace as
//!    `Loaded` (the reload refused before publishing);
//! 5. the manifest bytes are unchanged after 1 to 4.
//!
//! The CLI leg (`sqry index --force` over the same manifest) drives the
//! binary and lives in `sqry-cli/tests/w1r2_verifier_add_control.rs`.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use sqry_core::graph::unified::build::{BuildConfig, build_and_persist_graph_with_progress};
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_core::progress::no_op_reporter;
use sqry_core::project::ProjectRootMode;
use sqry_daemon::{
    DaemonConfig, DaemonError, RealWorkspaceBuilder, WorkspaceBuilder, WorkspaceKey,
    WorkspaceRosterResolver,
};
use sqry_plugin_registry::{
    BuildWithRosterError, PluginSelectionConfig, PluginSelectionError, UnreadableManifestPolicy,
    build_and_persist_with_workspace_roster, create_plugin_manager, create_plugin_manager_all,
};
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;

const ADDED_ID: &str = "w1-r2-verifier-added-plugin";

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
        "test:w1r2_add_form",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("fast_path_default".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("fast-path index persists");
}

fn plant_added_id(root: &Path) -> Vec<u8> {
    let storage = GraphStorage::new(root);
    let mut manifest = storage.load_manifest().expect("manifest");
    manifest
        .plugin_selection
        .as_mut()
        .expect("selection recorded")
        .active_plugin_ids
        .push(ADDED_ID.to_string());
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w1r2_add_form_control_round_2_paths_refuse_the_added_plugin_id_by_name() {
    assert!(
        create_plugin_manager_all().plugin_by_id(ADDED_ID).is_none(),
        "the control needs an id no build compiles"
    );
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    index_fast_path(&root);
    let manifest_before = plant_added_id(&root);
    let storage = GraphStorage::new(&root);
    let manifest_path = storage.manifest_path().display().to_string();
    let listing_before = index_dir_listing(&root);
    assert!(listing_before.len() >= 2, "manifest and snapshot exist");

    // 1. The registry helper, both policies.
    for policy in [
        UnreadableManifestPolicy::Refuse,
        UnreadableManifestPolicy::FallBack,
    ] {
        let err = build_and_persist_with_workspace_roster(
            &root,
            &PluginSelectionConfig::default(),
            policy,
            "test:w1r2_add_form",
            &BuildConfig::default(),
            &sqry_core::graph::unified::build::MacroOptionsRequest::empty(),
            no_op_reporter(),
        )
        .expect_err("the helper must refuse the added id");
        match &err {
            BuildWithRosterError::Selection(PluginSelectionError::UnknownPluginIdsCtx {
                ids,
                manifest_path: named,
                ..
            }) => {
                assert_eq!(ids, &vec![ADDED_ID.to_string()], "policy {policy:?}");
                assert_eq!(
                    named.as_deref(),
                    Some(storage.manifest_path()),
                    "policy {policy:?}"
                );
            }
            other => panic!("expected Selection(UnknownPluginIdsCtx), got {other:?}"),
        }
        assert!(err.to_string().contains(ADDED_ID), "{err}");
        assert_eq!(
            index_dir_listing(&root),
            listing_before,
            "policy {policy:?}"
        );
        println!("ADD-FORM r2 registry helper [{policy:?}]: {err}");
    }

    // 2. The daemon builder's load path.
    let builder = RealWorkspaceBuilder::new(Arc::new(WorkspaceRosterResolver::new()));
    let load_err = builder
        .load_persisted(&root)
        .expect_err("load_persisted must refuse the added id");
    match &load_err {
        DaemonError::WorkspaceIncompatibleGraph {
            root: named,
            reason,
        } => {
            assert_eq!(named, &root);
            assert!(reason.contains(ADDED_ID), "{reason}");
            assert!(reason.contains(&manifest_path), "{reason}");
        }
        other => panic!("expected WorkspaceIncompatibleGraph, got {other:?}"),
    }
    assert_eq!(load_err.jsonrpc_code(), Some(-32005));
    assert_eq!(index_dir_listing(&root), listing_before);
    println!("ADD-FORM r2 load_persisted: {load_err}");

    // 3. daemon/load through the production resolver.
    let server = server_with_production_resolver().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    let err = expect_error(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    )
    .clone();
    assert_eq!(err.code, -32005, "daemon/load: {err:?}");
    assert!(err.message.contains(ADDED_ID), "{}", err.message);
    println!(
        "ADD-FORM r2 daemon/load: code={} message={}",
        err.code, err.message
    );
    drop(client);
    server.stop().await;

    // 4. Loaded clean, then the id is planted and the workspace evicted:
    //    the reload refuses and the workspace is not republished.
    std::fs::write(storage.manifest_path(), {
        let mut m = storage.load_manifest().expect("manifest");
        m.plugin_selection
            .as_mut()
            .expect("selection")
            .active_plugin_ids
            .retain(|id| id != ADDED_ID);
        serde_json::to_vec_pretty(&m).expect("json")
    })
    .expect("clean manifest");
    let server = server_with_production_resolver().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    let replanted = plant_added_id(&root);
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
    assert_eq!(search_err.code, -32005, "reload: {search_err:?}");
    assert!(
        search_err.message.contains(ADDED_ID),
        "{}",
        search_err.message
    );
    // Battery row V2 (design D-13): the acquirer re-derives the unknown
    // ids for a refused reload and keeps the unknown-id label; a refusal
    // relabelled as a snapshot-format mismatch reads
    // `incompatible snapshot format: unknown plugin ids: [..]`, which is the
    // wrong repair hint (reindex) for a manifest naming a plugin this
    // binary did not compile. Both halves are asserted.
    assert!(
        search_err
            .message
            .contains(&format!("unknown plugin ids: [{ADDED_ID}]")),
        "the reload refusal must carry the unknown-id label: {}",
        search_err.message
    );
    assert!(
        !search_err.message.contains("incompatible snapshot format"),
        "the reload refusal must not be relabelled as a snapshot-format mismatch: {}",
        search_err.message
    );
    let status = client.request("daemon/status", json!({})).await;
    let row = status_row(expect_success(&status), &path);
    assert_ne!(
        row["state"],
        json!("Loaded"),
        "a refused reload must not republish the workspace; row: {row}"
    );
    println!(
        "ADD-FORM r2 evicted reload: code={} message={} state={}",
        search_err.code, search_err.message, row["state"]
    );
    drop(client);
    server.stop().await;

    // 5. Nothing rewrote the manifest.
    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        replanted,
        "no round 2 path may rewrite the manifest while refusing it"
    );
    assert_eq!(
        manifest_before.len(),
        replanted.len(),
        "the two plantings produce the same manifest shape"
    );
}
