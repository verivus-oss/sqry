//! Task 8 Phase 8a — `daemon/status` integration tests.

mod support;

use serde_json::json;
use support::ipc::{TestIpcClient, TestServer, expect_success};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_status_round_trips_for_empty_manager() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let resp = client.request("daemon/status", json!({})).await;
    let result = expect_success(&resp);
    // Envelope shape.
    assert!(
        result.get("result").is_some(),
        "envelope result: {result:?}"
    );
    let meta = result.get("meta").expect("meta present");
    assert_eq!(meta.get("stale").and_then(|v| v.as_bool()), Some(false));
    assert!(
        meta.get("workspace_state").is_none(),
        "management meta omits workspace_state"
    );
    // Inner DaemonStatus has zero workspaces.
    let inner = &result["result"];
    assert_eq!(
        inner["workspaces"].as_array().map(std::vec::Vec::len),
        Some(0)
    );
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_status_reports_loaded_workspaces() {
    let server = TestServer::new().await;
    // Preload a workspace directly via manager (not via daemon/load).
    let dir = tempfile::tempdir().unwrap();
    let key = sqry_daemon::WorkspaceKey::new(
        sqry_core::project::canonicalize_path(dir.path()).unwrap(),
        sqry_core::project::ProjectRootMode::GitRoot,
        0,
    );
    server
        .manager
        .get_or_load(&key, &sqry_daemon::EmptyGraphBuilder, 2 * 1024 * 1024)
        .expect("preload");
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let resp = client.request("daemon/status", json!({})).await;
    let result = expect_success(&resp);
    let workspaces = result["result"]["workspaces"].as_array().expect("array");
    assert_eq!(workspaces.len(), 1);
    assert_eq!(workspaces[0]["state"].as_str(), Some("Loaded"));
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_status_envelope_has_management_meta() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let resp = client.request("daemon/status", json!({})).await;
    let result = expect_success(&resp);
    let meta = &result["meta"];
    assert_eq!(meta["stale"], json!(false));
    assert!(meta.get("last_good_at").is_none());
    assert!(meta.get("last_error").is_none());
    assert!(meta.get("workspace_state").is_none());
    assert!(meta["daemon_version"].is_string());
    drop(client);
    server.stop().await;
}
