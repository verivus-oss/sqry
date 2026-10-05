//! Verifier pins for surface parity W1 round 2 (battery row V8, deviation
//! D-12): the refusal of an unreadable manifest is recorded on the failed
//! workspace entry as the variant's own Display, so `daemon/status` shows
//! `last_error` equal to the refusal the client received. A `clone_err`
//! that collapsed the variant into `WorkspaceBuildFailed` would prefix it
//! with `workspace <root> build failed:`; the battery showed no shipped
//! test observing the recorded error, so this one does.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use sqry_core::graph::unified::build::{BuildConfig, build_and_persist_graph_with_progress};
use sqry_core::graph::unified::persistence::{GraphStorage, PluginSelectionManifest};
use sqry_core::progress::no_op_reporter;
use sqry_daemon::{DaemonConfig, RealWorkspaceBuilder, WorkspaceBuilder, WorkspaceRosterResolver};
use sqry_plugin_registry::create_plugin_manager;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};
use tempfile::TempDir;

fn write_fixture(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn alpha() -> u32 { 1 }\n",
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
        "test:w1r2_pins",
        Some(PluginSelectionManifest {
            active_plugin_ids: ids,
            high_cost_mode: Some("fast_path_default".to_string()),
        }),
        no_op_reporter(),
    )
    .expect("fast-path index persists");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refused_load_over_an_unreadable_manifest_records_the_refusal_verbatim() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    index_fast_path(&root);
    let storage = GraphStorage::new(&root);
    std::fs::write(storage.manifest_path(), b"{}").expect("unparseable manifest");

    let resolver = Arc::new(WorkspaceRosterResolver::new());
    let builder: Arc<dyn WorkspaceBuilder> =
        Arc::new(RealWorkspaceBuilder::new(Arc::clone(&resolver)));
    let server =
        TestServer::with_builder_config_and_roster(builder, DaemonConfig::default(), resolver)
            .await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let path = root.to_string_lossy().to_string();

    let err = expect_error(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    )
    .clone();
    assert_eq!(err.code, -32001, "{err:?}");
    assert!(
        err.message.starts_with("manifest at "),
        "the refusal is the variant's own Display: {}",
        err.message
    );

    let status = client.request("daemon/status", json!({})).await;
    let status = expect_success(&status);
    let row = status["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"].as_str() == Some(path.as_str()))
        })
        .cloned()
        .expect("a failed load registers the root in the status table");
    assert_eq!(row["state"], json!("Failed"), "row: {row}");
    assert_eq!(
        row["last_error"],
        json!(err.message),
        "the recorded error must be the refusal verbatim, not a re-labelled build failure; row: {row}"
    );

    drop(client);
    server.stop().await;
}
