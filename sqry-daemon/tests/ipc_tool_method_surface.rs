//! Task 8 Phase 8b — smoke tests for the 12 tool methods not covered by
//! [`ipc_tool_semantic_search_matrix`] or [`ipc_tool_find_cycles_matrix`].
//!
//! Each test drives a Loaded workspace + issues one call for the method
//! under test. The goal is **envelope routing correctness**: the method
//! is reachable, params deserialise, and the wrapper invokes the inner
//! handler against the Loaded graph.
//!
//! ## Success vs. -32603 branches
//!
//! The `EmptyGraphBuilder`-backed workspace has zero nodes, so most
//! handlers that resolve a symbol (`main`) cannot produce a successful
//! result. Per the Task 8 Phase 8b smoke-test policy and the
//! `semantic_diff` precedent, we accept the following two outcomes as
//! equivalent evidence of "method reachable + envelope routing works":
//!
//! 1. `expect_success(&resp)` + `meta.workspace_state == "Loaded"` +
//!    `meta.stale == false` (for methods tolerant of an empty graph:
//!    `find_unused`, `complexity_metrics`).
//!
//! 2. `expect_error(&resp)` with `code == -32603` (for methods that
//!    require at least one resolvable symbol or side-effect data: the
//!    remaining 10 methods). The -32603 classification — not
//!    -32601/-32602/-32001 — confirms the method reached the inner
//!    handler and failed inside its legitimate "no data" logic, not at
//!    the dispatcher / params-validation / classify_for_serve layer.
//!    This mirrors the explicit escalation in the Task 8 spec for
//!    `semantic_diff`.
//!
//! Alternative (`git init` + populate the workspace with a source file +
//! use `RealGraphBuilder`) was considered and rejected as significantly
//! higher complexity for no routing-coverage gain.

#![allow(clippy::too_many_lines)]

mod support;

use serde_json::json;
use sqry_daemon::ipc::protocol::JsonRpcPayload;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};

async fn fresh_loaded() -> (TestServer, tempfile::TempDir, TestIpcClient, String) {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().to_string();
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    (server, dir, client, path)
}

fn assert_loaded_fresh_envelope(result: &serde_json::Value) {
    assert_eq!(
        result["meta"]["workspace_state"],
        json!("Loaded"),
        "workspace_state must be Loaded: {result}"
    );
    assert_eq!(
        result["meta"]["stale"],
        json!(false),
        "stale must be false on Fresh Loaded arm: {result}"
    );
}

/// Assert that a response either (a) succeeded with the expected
/// `Loaded`/`stale=false` envelope, or (b) failed with a legitimate
/// `-32603 Internal error` from the inner handler (empty-graph "no data"
/// response on a method that can't serve an empty graph). Anything else
/// — -32601 Method not found, -32602 Invalid params, -32001 workspace
/// not ready — indicates an envelope-routing regression.
fn assert_method_reachable(resp: &sqry_daemon::ipc::protocol::JsonRpcResponse) {
    match &resp.payload {
        JsonRpcPayload::Success { result } => {
            assert_loaded_fresh_envelope(result);
        }
        JsonRpcPayload::Error { error } => {
            assert_eq!(
                error.code, -32603,
                "method reachable must surface success or -32603, got {error:?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relation_query() {
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "relation_query",
            json!({
                "symbol": "main",
                "relation_type": "callers",
                "path": &path,
                "max_results": 10,
                "max_depth": 1,
                "page_size": 50,
            }),
        )
        .await;
    assert_method_reachable(&resp);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn structural_similar() {
    // WS6 reachability gate (U07): the daemon JSON-RPC method must ROUTE to the
    // tool dispatcher. Whether the probe resolves to a descriptor-bearing node
    // depends on the fixture; either a fresh-loaded success envelope or a
    // -32603 (no descriptor) proves the method is reachable — what must NOT
    // happen is a -32601 MethodNotFound, which would mean the method gate or
    // dispatch arm is unwired.
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "structural_similar",
            json!({
                "symbol_name": "main",
                "path": &path,
                "similarity_threshold": 0.6,
                "max_results": 10,
            }),
        )
        .await;
    assert_method_reachable(&resp);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_callers() {
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "direct_callers",
            json!({
                "symbol": "main",
                "path": &path,
                "max_results": 10,
            }),
        )
        .await;
    assert_method_reachable(&resp);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_callees() {
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "direct_callees",
            json!({
                "symbol": "main",
                "path": &path,
                "max_results": 10,
            }),
        )
        .await;
    assert_method_reachable(&resp);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn find_unused() {
    // `find_unused` tolerates an empty graph (returns an empty unused
    // list), so a successful envelope is the expected path.
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "find_unused",
            json!({
                "path": &path,
                "scope": "all",
                "language": [],
                "symbol_kind": [],
                "max_results": 10,
            }),
        )
        .await;
    let result = expect_success(&resp);
    assert_loaded_fresh_envelope(result);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn is_node_in_cycle() {
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "is_node_in_cycle",
            json!({
                "symbol": "main",
                "path": &path,
                "cycle_type": "calls",
                "min_depth": 2,
            }),
        )
        .await;
    assert_method_reachable(&resp);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trace_path() {
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "trace_path",
            json!({
                "from_symbol": "main",
                "to_symbol": "main",
                "path": &path,
                "max_hops": 5,
                "max_paths": 5,
            }),
        )
        .await;
    assert_method_reachable(&resp);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subgraph() {
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "subgraph",
            json!({
                "symbols": ["main"],
                "path": &path,
                "max_depth": 2,
                "max_nodes": 10,
                "page_size": 50,
            }),
        )
        .await;
    assert_method_reachable(&resp);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn export_graph() {
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "export_graph",
            json!({
                "path": &path,
                "symbol_name": "main",
                "format": "json",
                "max_depth": 2,
                "max_results": 10,
                "page_size": 200,
            }),
        )
        .await;
    assert_method_reachable(&resp);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complexity_metrics() {
    // `complexity_metrics` tolerates an empty graph (returns empty
    // metrics list), so a successful envelope is the expected path.
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "complexity_metrics",
            json!({
                "path": &path,
                "max_results": 10,
            }),
        )
        .await;
    let result = expect_success(&resp);
    assert_loaded_fresh_envelope(result);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_diff() {
    // `semantic_diff` requires a real git worktree with at least two
    // refs. The EmptyGraphBuilder-backed tempdir is not a git repo, so
    // the inner body fails with `-32603 Internal error`. Per the Task 8
    // escalation: confirms envelope routing works, inner logic fails
    // as expected.
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "semantic_diff",
            json!({
                "path": &path,
                "base": {"ref": "HEAD~1"},
                "target": {"ref": "HEAD"},
                "max_results": 10,
                "page_size": 100,
            }),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(err.code, -32603, "semantic_diff: {err:?}");
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dependency_impact() {
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "dependency_impact",
            json!({
                "symbol": "main",
                "path": &path,
                "max_depth": 3,
                "max_results": 10,
                "page_size": 100,
            }),
        )
        .await;
    assert_method_reachable(&resp);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn show_dependencies() {
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "show_dependencies",
            json!({
                "symbol_name": "main",
                "path": &path,
                "max_depth": 2,
                "max_results": 10,
                "page_size": 100,
            }),
        )
        .await;
    assert_method_reachable(&resp);
    drop(client);
    server.stop().await;
}
