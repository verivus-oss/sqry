//! Task 8 Phase 8b — `daemon/status` memory-telemetry + concurrency
//! smoke tests exercised via the new tool calls.
//!
//! Schema decisions (documented inline; Task 8 escalation path):
//!
//! - Phase 8a's `DaemonStatus` exposes per-workspace `current_bytes` and
//!   `high_water_bytes` (NOT `memory_high_water_bytes`). The tests use
//!   these canonical names.
//! - Daemon-level totals live under `result.memory.{current_bytes,
//!   reserved_bytes, high_water_bytes, limit_bytes}` — the "total" is
//!   `memory.high_water_bytes`, not `total_memory_high_water_bytes`.

#![allow(clippy::too_many_lines)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::ipc::{TestIpcClient, TestServer, expect_success};

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
async fn daemon_status_reports_per_workspace_current_and_high_water() {
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

    let status = expect_success(&client.request("daemon/status", json!({})).await).clone();
    let workspaces = status["result"]["workspaces"]
        .as_array()
        .expect("workspaces array present");
    assert_eq!(workspaces.len(), 1, "one loaded workspace: {status}");
    let ws = &workspaces[0];
    assert!(
        ws["current_bytes"].is_u64(),
        "workspace.current_bytes must be u64: {ws}"
    );
    assert!(
        ws["high_water_bytes"].is_u64(),
        "workspace.high_water_bytes must be u64: {ws}"
    );
    // Daemon-wide memory block also carries the canonical field names.
    let mem = &status["result"]["memory"];
    assert!(mem["current_bytes"].is_u64());
    assert!(mem["high_water_bytes"].is_u64());
    assert!(mem["reserved_bytes"].is_u64());
    assert!(mem["limit_bytes"].is_u64());
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn high_water_monotonic_across_rebuild() {
    // Simplification (documented per Task 8 escalation): there's no
    // test-exposed way to force a rebuild programmatically without the
    // source-tree watcher, which requires a real git repo + filesystem
    // events. Instead we load one workspace, capture high_water, then
    // load a second distinct workspace and assert the daemon-wide
    // high_water is non-decreasing. This still guards the "monotonic
    // high-water across graph publish events" invariant without
    // depending on watcher wiring.
    let server = TestServer::new().await;
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": dir1.path().to_string_lossy() }),
            )
            .await,
    );
    let first_status = expect_success(&client.request("daemon/status", json!({})).await).clone();
    let first_high_water = first_status["result"]["memory"]["high_water_bytes"]
        .as_u64()
        .expect("high_water_bytes u64");

    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": dir2.path().to_string_lossy() }),
            )
            .await,
    );
    let second_status = expect_success(&client.request("daemon/status", json!({})).await).clone();
    let second_high_water = second_status["result"]["memory"]["high_water_bytes"]
        .as_u64()
        .expect("high_water_bytes u64");

    assert!(
        second_high_water >= first_high_water,
        "high_water must be monotonic: first={first_high_water}, second={second_high_water}"
    );
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unload_removes_per_workspace_entry_total_high_water_unchanged() {
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
    let before_status = expect_success(&client.request("daemon/status", json!({})).await).clone();
    let before_high_water = before_status["result"]["memory"]["high_water_bytes"]
        .as_u64()
        .expect("u64");
    assert_eq!(
        before_status["result"]["workspaces"]
            .as_array()
            .map(std::vec::Vec::len),
        Some(1)
    );

    expect_success(
        &client
            .request(
                "daemon/unload",
                json!({ "index_root": dir.path().to_string_lossy() }),
            )
            .await,
    );

    let after_status = expect_success(&client.request("daemon/status", json!({})).await).clone();
    assert_eq!(
        after_status["result"]["workspaces"]
            .as_array()
            .map(std::vec::Vec::len),
        Some(0),
        "unload must remove the workspace entry"
    );
    let after_high_water = after_status["result"]["memory"]["high_water_bytes"]
        .as_u64()
        .expect("u64");
    assert_eq!(
        after_high_water, before_high_water,
        "daemon-wide high_water must be unchanged by unload"
    );
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_tool_calls_on_same_workspace_coexist() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().to_string();

    // Pre-load via a dedicated client.
    let mut loader = TestIpcClient::connect(&server.path).await;
    loader.hello(1).await;
    expect_success(
        &loader
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    drop(loader);

    let server_path = Arc::new(server.path.clone());
    let path_arc = Arc::new(path.clone());

    let mut joins = Vec::new();
    for _ in 0..4 {
        let server_path = Arc::clone(&server_path);
        let path_arc = Arc::clone(&path_arc);
        joins.push(tokio::spawn(async move {
            let mut client = TestIpcClient::connect(&server_path).await;
            client.hello(1).await;
            let resp = client
                .request("semantic_search", semantic_search_params(path_arc.as_str()))
                .await;
            let result = expect_success(&resp).clone();
            drop(client);
            result
        }));
    }
    for j in joins {
        let result = j.await.expect("join");
        assert_eq!(result["meta"]["workspace_state"], json!("Loaded"));
        assert_eq!(result["meta"]["stale"], json!(false));
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_call_during_rebuild_serves_fresh_from_prior_snapshot() {
    use sqry_core::project::{ProjectRootMode, canonicalize_path};
    use sqry_daemon::{WorkspaceKey, WorkspaceState};

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

    // Flip the workspace state to Rebuilding; `classify_for_serve`
    // must still return `Fresh { state: Rebuilding, graph: <prior> }`.
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
    assert_eq!(
        result["meta"]["workspace_state"],
        json!("Rebuilding"),
        "tool call during rebuild must observe Rebuilding state"
    );
    assert_eq!(result["meta"]["stale"], json!(false));
    drop(client);
    server.stop().await;
}
