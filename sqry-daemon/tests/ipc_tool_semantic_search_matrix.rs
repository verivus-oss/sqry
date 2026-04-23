//! Task 8 Phase 8b — `semantic_search` tool method verdict matrix.
//!
//! Exercises all four `ServeVerdict` arms (Fresh[Loaded],
//! Fresh[Rebuilding], Stale, NotReady) through the IPC `semantic_search`
//! entry point, asserting the `ResponseEnvelope.meta` shape + optional
//! `_stale_warning` splice.

#![allow(clippy::too_many_lines)]

mod support;

use std::time::{Duration, SystemTime};

use serde_json::json;
use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::{EmptyGraphBuilder, WorkspaceKey, WorkspaceState};
use support::insert_workspace_in_state;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};

fn semantic_search_params(path: &str) -> serde_json::Value {
    json!({
        "query": "kind:function",
        "path": path,
        "max_results": 10,
        "context_lines": 0,
        "include_classpath": false,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_loaded_arm() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    // Load the workspace via daemon/load (canonicalised, Loaded state).
    let load_resp = client
        .request(
            "daemon/load",
            json!({ "index_root": dir.path().to_string_lossy() }),
        )
        .await;
    expect_success(&load_resp);

    let resp = client
        .request(
            "semantic_search",
            semantic_search_params(&dir.path().to_string_lossy()),
        )
        .await;
    let result = expect_success(&resp);
    assert_eq!(
        result["meta"]["workspace_state"],
        json!("Loaded"),
        "workspace_state must be Loaded; result={result}"
    );
    assert_eq!(result["meta"]["stale"], json!(false));
    assert!(
        result["result"].get("_stale_warning").is_none(),
        "Fresh verdicts must NOT splice _stale_warning"
    );
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_rebuilding_arm() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let load_resp = client
        .request(
            "daemon/load",
            json!({ "index_root": dir.path().to_string_lossy() }),
        )
        .await;
    expect_success(&load_resp);

    // Flip the workspace state to Rebuilding synthetically.
    let canon = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canon, ProjectRootMode::GitRoot, 0);
    let ws = server.manager.lookup(&key).expect("registered");
    ws.store_state(WorkspaceState::Rebuilding);

    let resp = client
        .request(
            "semantic_search",
            semantic_search_params(&dir.path().to_string_lossy()),
        )
        .await;
    let result = expect_success(&resp);
    assert_eq!(result["meta"]["workspace_state"], json!("Rebuilding"));
    assert_eq!(result["meta"]["stale"], json!(false));
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_arm() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let load_resp = client
        .request(
            "daemon/load",
            json!({ "index_root": dir.path().to_string_lossy() }),
        )
        .await;
    expect_success(&load_resp);

    let canon = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canon, ProjectRootMode::GitRoot, 0);
    let ws = server.manager.lookup(&key).expect("registered");
    // Drive into Failed state with a 12-hour-old last-good snapshot
    // (well within the default 24h stale cap).
    ws.store_state(WorkspaceState::Failed);
    let now = SystemTime::now();
    ws.set_last_good_at_for_test(Some(now - Duration::from_secs(12 * 3600)));

    let resp = client
        .request(
            "semantic_search",
            semantic_search_params(&dir.path().to_string_lossy()),
        )
        .await;
    let result = expect_success(&resp);
    assert_eq!(result["meta"]["stale"], json!(true));
    assert_eq!(result["meta"]["workspace_state"], json!("Failed"));
    let last_good_at = result["meta"]["last_good_at"]
        .as_str()
        .expect("last_good_at is a string on stale responses");
    assert!(
        last_good_at.ends_with('Z'),
        "last_good_at must be RFC3339 UTC-Zulu: {last_good_at}"
    );
    let warning = result["result"]["_stale_warning"]
        .as_str()
        .expect("_stale_warning spliced into result object");
    assert!(
        warning.contains("12h stale"),
        "_stale_warning must describe the age: {warning}"
    );
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notready_arm() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();

    // Insert the workspace in Loading state directly — bypassing
    // daemon/load so it never reaches the Loaded arm.
    let canon = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canon, ProjectRootMode::GitRoot, 0);
    insert_workspace_in_state(&server.manager, &key, WorkspaceState::Loading);
    // Keep the EmptyGraphBuilder unused — we never let the builder run.
    let _ = EmptyGraphBuilder;

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let resp = client
        .request(
            "semantic_search",
            semantic_search_params(&dir.path().to_string_lossy()),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(err.code, -32001, "NotReady must surface -32001: {err:?}");
    let data = err.data.as_ref().expect("error.data populated");
    let reason = data["reason"].as_str().expect("reason field is a string");
    assert!(
        reason.contains("workspace not ready"),
        "reason must mention 'workspace not ready': {reason}"
    );
    drop(client);
    server.stop().await;
}
