//! Verifier pin for surface parity W1, round 3 (battery row VR16). Design
//! D14 says eviction swaps in the placeholder generation, an empty graph
//! with no roster record. The round 3 battery found the record half
//! unobserved: a tombstone that kept the old record beside the placeholder
//! graph survived every shipped test. `daemon/status` reports each row's
//! `plugin_roster` from the published record whatever the state, so this
//! test is the oracle: after eviction the row carries no roster.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use sqry_core::graph::unified::build::{BuildConfig, build_and_persist_graph_with_progress};
use sqry_core::graph::unified::persistence::PluginSelectionManifest;
use sqry_core::progress::no_op_reporter;
use sqry_core::project::ProjectRootMode;
use sqry_daemon::{
    DaemonConfig, RealWorkspaceBuilder, WorkspaceBuilder, WorkspaceKey, WorkspaceRosterResolver,
};
use sqry_plugin_registry::create_plugin_manager;
use support::ipc::{TestIpcClient, TestServer, expect_success};
use tempfile::TempDir;

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
        "test:w1r3_verifier_pins",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("fast_path_default".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("fast-path index persists");
}

async fn status_row(client: &mut TestIpcClient, path: &str) -> serde_json::Value {
    let status = client.request("daemon/status", json!({})).await;
    expect_success(&status)["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"].as_str() == Some(path))
        })
        .cloned()
        .expect("workspace row present")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn evicted_workspace_status_row_carries_no_roster_record() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    index_fast_path(&root);
    let path = root.to_string_lossy().to_string();

    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );

    // Control leg: a loaded workspace reports the record it was built with.
    let loaded = status_row(&mut client, &path).await;
    assert_eq!(loaded["state"], json!("Loaded"), "row: {loaded}");
    assert!(
        loaded["plugin_roster"].is_object(),
        "a loaded workspace reports its roster; row: {loaded}"
    );

    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    assert!(
        server.manager.evict_for_test(&key),
        "workspace was resident"
    );

    // The pin: the tombstone is the placeholder generation, no record.
    let evicted = status_row(&mut client, &path).await;
    assert_eq!(evicted["state"], json!("Evicted"), "row: {evicted}");
    assert!(
        evicted["plugin_roster"].is_null(),
        "an evicted workspace carries no roster record; row: {evicted}"
    );
    println!(
        "VR16 pin: state={} plugin_roster={}",
        evicted["state"], evicted["plugin_roster"]
    );

    drop(client);
    server.stop().await;
}
