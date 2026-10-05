//! ADD-form negative control for surface parity W1 (verifier round 1).
//!
//! The unit's guard governs plugin ids recorded in a workspace manifest.
//! This control plants a NEW id, `w1-verifier-added-plugin`, that no build
//! of sqry compiles, into an otherwise valid fast-path manifest, and
//! requires every surface to refuse it by name without touching the file:
//!
//! 1. the registry resolver (`resolve_workspace_roster`) refuses with
//!    `UnknownPluginIdsCtx` naming the id and the manifest path;
//! 2. `daemon/load` through the production resolver returns `-32005`
//!    naming the id;
//! 3. `daemon/rebuild` on the same root returns `-32005` naming the id;
//! 4. a resident graph planted through a pinned (manifest-blind) resolver
//!    is refused at query time by the acquirer with `-32005` naming the id,
//!    so the acquirer's own unknown-id check is exercised, not only the
//!    resolver's;
//! 5. the manifest bytes are unchanged after 1 to 4.
//!
//! The pinned leg (4) is the site the resolver-only tests cannot reach: a
//! builder that ignores the manifest loads the workspace, and the manifest
//! naming an uncompiled id is only seen when the served graph is classified.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use sqry_core::graph::unified::build::{BuildConfig, build_and_persist_graph_with_progress};
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_core::progress::no_op_reporter;
use sqry_daemon::{
    DaemonConfig, RealWorkspaceBuilder, RosterRecord, WorkspaceBuilder, WorkspaceRosterResolver,
};
use sqry_plugin_registry::{
    PluginSelectionConfig, PluginSelectionError, RosterSource, create_plugin_manager,
    create_plugin_manager_all, resolve_workspace_roster,
};
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;

const ADDED_ID: &str = "w1-verifier-added-plugin";

fn write_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn alpha() -> u32 { 1 }\n",
    )
    .expect("write lib.rs");
}

fn index_fast_path_then_add_id(root: &Path) -> Vec<u8> {
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
        "test:add_form",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("fast_path_default".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("fast-path index persists");
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

async fn server_with(resolver: Arc<WorkspaceRosterResolver>) -> TestServer {
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_form_control_every_surface_refuses_the_added_plugin_id_by_name() {
    assert!(
        create_plugin_manager_all().plugin_by_id(ADDED_ID).is_none(),
        "the control needs an id no build compiles"
    );
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    let manifest_before = index_fast_path_then_add_id(&root);
    let manifest_path = GraphStorage::new(&root).manifest_path().to_path_buf();

    // 1. Registry resolver.
    let err = resolve_workspace_roster(&root, &PluginSelectionConfig::default())
        .expect_err("registry must refuse the added id");
    match &err {
        PluginSelectionError::UnknownPluginIdsCtx {
            ids,
            manifest_path: named,
            ..
        } => {
            assert_eq!(ids, &vec![ADDED_ID.to_string()], "ids: {ids:?}");
            assert_eq!(named.as_deref(), Some(manifest_path.as_path()));
        }
        other => panic!("expected UnknownPluginIdsCtx, got {other:?}"),
    }
    println!("ADD-FORM registry: {err}");

    // 2 and 3. daemon/load and daemon/rebuild through the production resolver.
    let server = server_with(Arc::new(WorkspaceRosterResolver::new())).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();
    let load_err = expect_error(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    )
    .clone();
    assert_eq!(load_err.code, -32005, "daemon/load: {load_err:?}");
    assert!(
        load_err.message.contains(ADDED_ID),
        "daemon/load must name the id: {}",
        load_err.message
    );
    println!(
        "ADD-FORM daemon/load: code={} message={}",
        load_err.code, load_err.message
    );
    let rebuild_err = expect_error(
        &client
            .request("daemon/rebuild", json!({ "path": &path, "force": true }))
            .await,
    )
    .clone();
    assert_eq!(rebuild_err.code, -32005, "daemon/rebuild: {rebuild_err:?}");
    assert!(
        rebuild_err.message.contains(ADDED_ID),
        "daemon/rebuild must name the id: {}",
        rebuild_err.message
    );
    println!(
        "ADD-FORM daemon/rebuild: code={} message={}",
        rebuild_err.code, rebuild_err.message
    );
    drop(client);
    server.stop().await;

    // 4. A resident graph planted past the resolver: the acquirer refuses.
    let fast = Arc::new(create_plugin_manager());
    let record = RosterRecord::from_manager(&fast, RosterSource::Fallback);
    let server = server_with(Arc::new(WorkspaceRosterResolver::pinned(fast, record))).await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
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
    assert_eq!(
        search_err.code, -32005,
        "a served graph whose manifest names an uncompiled id must be refused at query time: {search_err:?}"
    );
    assert!(
        search_err.message.contains(ADDED_ID),
        "the acquirer refusal must name the id: {}",
        search_err.message
    );
    println!(
        "ADD-FORM acquirer (pinned resident graph): code={} message={}",
        search_err.code, search_err.message
    );
    drop(client);
    server.stop().await;

    // 5. Nothing rewrote the manifest.
    assert_eq!(
        std::fs::read(&manifest_path).expect("manifest bytes"),
        manifest_before,
        "no surface may rewrite the manifest while refusing it"
    );
}
