//! Task 8 Phase 8b — EXPLICIT NEGATIVE tests pinning the expected
//! `-32602 Invalid params` wire contract for tool methods whose params
//! carry custom `validate()` constraints (non-empty query/symbol).
//!
//! These tests specifically guard against the regression flagged by
//! Codex gpt-5.4's feat-iter-0 review: a daemon-side validation bug
//! could previously slip through the `ipc_tool_method_surface.rs` smoke
//! tests because `assert_method_reachable` accepts `-32603` as a pass.
//! Here we load a workspace + issue a call with a provably-invalid
//! payload + pin the EXACT `-32602` classification so the daemon's
//! `params_to_*_args` converters keep parity with the rmcp-side
//! `convert_*_params` + `params.validate()` guards in `server.rs`.

#![allow(clippy::too_many_lines)]

mod support;

use serde_json::json;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_search_blank_query_emits_32602() {
    // Whitespace-only `query` must trigger the non-empty guard in
    // `params_to_semantic_search_args` and surface as `-32602 Invalid
    // params`, mirroring the rmcp-side `server.rs` check.
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "semantic_search",
            json!({
                "query": "   ",
                "path": &path,
                "max_results": 10,
                "context_lines": 0,
                "include_classpath": false,
            }),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32602,
        "blank semantic_search query must surface -32602 Invalid params, got {err:?}"
    );
    assert_eq!(
        err.message, "query cannot be empty",
        "message must mirror rmcp `SqryServer::semantic_search` inline check verbatim, got {err:?}"
    );
    let data = err
        .data
        .as_ref()
        .expect("error.data must carry the rpc_error_to_mcp envelope");
    assert_eq!(data["kind"], json!("validation_error"), "data.kind");
    assert_eq!(data["retryable"], json!(false), "data.retryable");
    assert_eq!(data["details"]["kind"], json!("validation"), "details.kind");
    assert_eq!(
        data["details"]["constraint"],
        json!("non_empty"),
        "details.constraint"
    );
    assert_eq!(data["details"]["field"], json!("query"), "details.field");
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_callers_empty_symbol_emits_32602() {
    // Empty `symbol` must trigger `DirectCallersParams::validate()` via
    // the daemon converter and surface as `-32602 Invalid params`,
    // mirroring the rmcp-side `params.validate()` guard.
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "direct_callers",
            json!({
                "symbol": "",
                "path": &path,
                "max_results": 10,
            }),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32602,
        "empty direct_callers symbol must surface -32602 Invalid params, got {err:?}"
    );
    assert_eq!(
        err.message, "symbol cannot be empty",
        "message must mirror `DirectCallersParams::validate()` verbatim, got {err:?}"
    );
    let data = err
        .data
        .as_ref()
        .expect("error.data must carry the rpc_error_to_mcp envelope");
    assert_eq!(data["kind"], json!("validation_error"));
    assert_eq!(data["retryable"], json!(false));
    assert_eq!(data["details"]["kind"], json!("validation"));
    assert_eq!(data["details"]["constraint"], json!("non_empty"));
    assert_eq!(data["details"]["field"], json!("symbol"));
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_callees_empty_symbol_emits_32602() {
    // Empty `symbol` must trigger `DirectCalleesParams::validate()` via
    // the daemon converter and surface as `-32602 Invalid params`,
    // mirroring the rmcp-side `params.validate()` guard.
    let (server, _dir, mut client, path) = fresh_loaded().await;
    let resp = client
        .request(
            "direct_callees",
            json!({
                "symbol": "",
                "path": &path,
                "max_results": 10,
            }),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32602,
        "empty direct_callees symbol must surface -32602 Invalid params, got {err:?}"
    );
    assert_eq!(
        err.message, "symbol cannot be empty",
        "message must mirror `DirectCalleesParams::validate()` verbatim, got {err:?}"
    );
    let data = err
        .data
        .as_ref()
        .expect("error.data must carry the rpc_error_to_mcp envelope");
    assert_eq!(data["kind"], json!("validation_error"));
    assert_eq!(data["retryable"], json!(false));
    assert_eq!(data["details"]["kind"], json!("validation"));
    assert_eq!(data["details"]["constraint"], json!("non_empty"));
    assert_eq!(data["details"]["field"], json!("symbol"));
    drop(client);
    server.stop().await;
}
