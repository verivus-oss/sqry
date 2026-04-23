//! Task 8 Phase 8b — `find_cycles` tool method verdict matrix.
//!
//! Mirrors `ipc_tool_semantic_search_matrix` for a DB-backed tool
//! (`find_cycles` routes through `sqry_db::make_query_db_cold`). Asserts
//! the envelope shape across all four `ServeVerdict` arms plus that the
//! DB-backed wiring is reachable from every arm.

#![allow(clippy::too_many_lines)]

mod support;

use std::time::{Duration, SystemTime};

use serde_json::json;
use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::{WorkspaceKey, WorkspaceState};
use support::insert_workspace_in_state;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};

fn find_cycles_params(path: &str) -> serde_json::Value {
    json!({
        "path": path,
        "cycle_type": "calls",
        "max_results": 10,
        "min_depth": 2,
        "include_self_loops": false,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_loaded_arm() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": dir.path().to_string_lossy() }),
            )
            .await,
    );

    let resp = client
        .request(
            "find_cycles",
            find_cycles_params(&dir.path().to_string_lossy()),
        )
        .await;
    let result = expect_success(&resp);
    assert_eq!(result["meta"]["workspace_state"], json!("Loaded"));
    assert_eq!(result["meta"]["stale"], json!(false));
    assert!(result["result"].get("_stale_warning").is_none());
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fresh_rebuilding_arm() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": dir.path().to_string_lossy() }),
            )
            .await,
    );

    let canon = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canon, ProjectRootMode::GitRoot, 0);
    let ws = server.manager.lookup(&key).expect("registered");
    ws.store_state(WorkspaceState::Rebuilding);

    let resp = client
        .request(
            "find_cycles",
            find_cycles_params(&dir.path().to_string_lossy()),
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
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": dir.path().to_string_lossy() }),
            )
            .await,
    );

    let canon = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canon, ProjectRootMode::GitRoot, 0);
    let ws = server.manager.lookup(&key).expect("registered");
    ws.store_state(WorkspaceState::Failed);
    let now = SystemTime::now();
    ws.set_last_good_at_for_test(Some(now - Duration::from_secs(12 * 3600)));

    let resp = client
        .request(
            "find_cycles",
            find_cycles_params(&dir.path().to_string_lossy()),
        )
        .await;
    let result = expect_success(&resp);
    assert_eq!(result["meta"]["stale"], json!(true));
    assert_eq!(result["meta"]["workspace_state"], json!("Failed"));
    let last_good_at = result["meta"]["last_good_at"]
        .as_str()
        .expect("last_good_at present");
    assert!(last_good_at.ends_with('Z'), "{last_good_at}");
    let warning = result["result"]["_stale_warning"]
        .as_str()
        .expect("_stale_warning spliced");
    assert!(warning.contains("12h stale"), "{warning}");
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notready_arm() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();

    let canon = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canon, ProjectRootMode::GitRoot, 0);
    insert_workspace_in_state(&server.manager, &key, WorkspaceState::Loading);

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let resp = client
        .request(
            "find_cycles",
            find_cycles_params(&dir.path().to_string_lossy()),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(err.code, -32001);
    let data = err.data.as_ref().expect("error.data populated");
    let reason = data["reason"].as_str().expect("reason string");
    assert!(reason.contains("workspace not ready"), "reason: {reason}");
    drop(client);
    server.stop().await;
}
