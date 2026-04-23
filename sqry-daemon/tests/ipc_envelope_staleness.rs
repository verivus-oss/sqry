//! Task 8 Phase 8b — envelope meta + error-data shape assertions,
//! tool-method independent.
//!
//! Each test drives a workspace into a specific verdict/error state and
//! verifies the **envelope** (wire contract) is shaped as Phase 8b
//! demands, regardless of the tool method used to probe it.

#![allow(clippy::too_many_lines)]

mod support;

use std::time::{Duration, SystemTime};

use serde_json::json;
use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::{WorkspaceKey, WorkspaceState};
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
async fn stale_warning_omits_last_error_when_none() {
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
    // Intentionally do NOT call `record_failure`: last_error stays None.
    ws.set_last_good_at_for_test(Some(SystemTime::now() - Duration::from_secs(12 * 3600)));

    let resp = client
        .request(
            "semantic_search",
            semantic_search_params(&dir.path().to_string_lossy()),
        )
        .await;
    let result = expect_success(&resp);
    let warning = result["result"]["_stale_warning"]
        .as_str()
        .expect("_stale_warning present for stale verdict");
    assert!(
        !warning.contains("last error:"),
        "omit 'last error:' clause when record_failure never fired: {warning}"
    );
    // And the meta should not carry a last_error field either.
    assert!(
        result["meta"].get("last_error").is_none(),
        "meta.last_error must be skipped when None: meta={}",
        result["meta"]
    );
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_warning_last_good_at_is_rfc3339_utc_zulu() {
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
    ws.set_last_good_at_for_test(Some(SystemTime::now() - Duration::from_secs(6 * 3600)));

    let resp = client
        .request(
            "semantic_search",
            semantic_search_params(&dir.path().to_string_lossy()),
        )
        .await;
    let result = expect_success(&resp);
    let rfc = result["meta"]["last_good_at"]
        .as_str()
        .expect("last_good_at string present");
    assert!(
        rfc.starts_with("20"),
        "RFC3339 must start with a 20YY year: {rfc}"
    );
    assert!(rfc.contains('T'), "RFC3339 must contain T separator: {rfc}");
    assert!(rfc.ends_with('Z'), "UTC-Zulu must end in Z: {rfc}");
    assert!(
        rfc.len() >= 20,
        "RFC3339 with seconds must be >= 20 chars: {rfc}"
    );
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notready_error_data_contains_state_field() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();

    let canon = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canon, ProjectRootMode::GitRoot, 0);
    insert_workspace_in_state(&server.manager, &key, WorkspaceState::Loading);

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let resp = client
        .request(
            "semantic_search",
            semantic_search_params(&dir.path().to_string_lossy()),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(err.code, -32001, "NotReady → -32001 WorkspaceBuildFailed");
    // `DaemonError::WorkspaceBuildFailed::error_data()` renders
    // `{ "root": ..., "reason": "workspace not ready (Loading); ..." }`.
    let data = err.data.as_ref().expect("error.data populated");
    let reason = data["reason"].as_str().expect("reason is a string");
    assert!(
        reason.contains("Loading"),
        "NotReady reason must carry observed state: {reason}"
    );
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_expired_error_data_contains_plan_fields() {
    // Default config sets `stale_serve_max_age_hours = 24`; use a
    // 48-hour-old last-good timestamp to force the Expired arm.
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
    ws.set_last_good_at_for_test(Some(SystemTime::now() - Duration::from_secs(48 * 3600)));

    let resp = client
        .request(
            "semantic_search",
            semantic_search_params(&dir.path().to_string_lossy()),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(err.code, -32002, "Expired → -32002 WorkspaceStaleExpired");
    let data = err.data.as_ref().expect("error.data populated");
    for key in [
        "root",
        "age_hours",
        "cap_hours",
        "last_good_at",
        "last_error",
    ] {
        assert!(
            data.get(key).is_some(),
            "error.data must carry `{key}`: data={data}"
        );
    }
    // Sanity-check numeric ordering.
    let age = data["age_hours"].as_u64().expect("age_hours number");
    let cap = data["cap_hours"].as_u64().expect("cap_hours number");
    assert!(age >= cap, "Expired implies age >= cap; {age} vs {cap}");
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_no_prior_good_emits_32001() {
    // Deterministically stage a workspace in `Failed` state with
    // `last_good_at = None` and a live map entry. Then verify that
    // `semantic_search` routes through `classify_for_serve`'s
    // `NoPriorGood` arm and EXACTLY emits -32001 — NOT -32004
    // (Evicted). The iter-0 review flagged the old `-32001 | -32004`
    // tolerance as masking validation drift; we force the map entry
    // up-front so the NoPriorGood code path is actually exercised.
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = canonicalize_path(dir.path()).unwrap();
    let key = WorkspaceKey::new(canon, ProjectRootMode::GitRoot, 0);

    // Synthesise a live map entry in `Failed` state. Bypassing
    // `get_or_load` guarantees we can assert the *exact* code without
    // racing on `daemon/load`'s placeholder-entry behaviour.
    insert_workspace_in_state(&server.manager, &key, WorkspaceState::Failed);
    let ws = server
        .manager
        .lookup(&key)
        .expect("inserted workspace must be present");
    ws.set_last_good_at_for_test(None);

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    let resp = client
        .request(
            "semantic_search",
            semantic_search_params(&dir.path().to_string_lossy()),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(
        err.code, -32001,
        "NoPriorGood must surface -32001 WorkspaceBuildFailed, got {err:?}"
    );
    drop(client);
    server.stop().await;
}
