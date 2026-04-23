//! Task 8 Phase 8a — `daemon/load` + `daemon/unload` integration.

mod support;

use std::sync::Arc;

use serde_json::json;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_load_registers_workspace() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let canon = sqry_core::project::canonicalize_path(dir.path()).unwrap();

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let resp = client
        .request(
            "daemon/load",
            json!({ "index_root": dir.path().to_string_lossy() }),
        )
        .await;
    let result = expect_success(&resp);
    assert_eq!(result["meta"]["workspace_state"], json!("Loaded"));
    let payload = &result["result"];
    assert_eq!(payload["state"], json!("Loaded"));
    assert_eq!(
        payload["root"].as_str().map(std::path::PathBuf::from),
        Some(canon.clone())
    );
    // Manager should have exactly one workspace.
    let status = server.manager.status();
    assert_eq!(status.workspaces.len(), 1);
    assert_eq!(status.workspaces[0].index_root, canon);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_unload_reports_was_loaded_true_then_false() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    client
        .request(
            "daemon/load",
            json!({ "index_root": dir.path().to_string_lossy() }),
        )
        .await;

    let first = client
        .request(
            "daemon/unload",
            json!({ "index_root": dir.path().to_string_lossy() }),
        )
        .await;
    let first_result = expect_success(&first);
    assert_eq!(first_result["result"]["was_loaded"], json!(true));

    let second = client
        .request(
            "daemon/unload",
            json!({ "index_root": dir.path().to_string_lossy() }),
        )
        .await;
    let second_result = expect_success(&second);
    assert_eq!(second_result["result"]["was_loaded"], json!(false));
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_load_invalid_path_returns_32602() {
    let server = TestServer::new().await;
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    // Non-existent root — uniform rejection per Phase 8a iter-3.
    let ghost = std::path::PathBuf::from("/this/path/should/not/exist/sqryd-test-8a");
    let resp = client
        .request(
            "daemon/load",
            json!({ "index_root": ghost.to_string_lossy() }),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(err.code, -32602);
    drop(client);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_same_key_load_runs_builder_once() {
    use sqry_core::graph::CodeGraph;
    use sqry_daemon::DaemonError;
    use std::sync::atomic::{AtomicU64, Ordering};

    // Counting builder so we can assert exactly one underlying build
    // even when multiple concurrent loads race. The racing callers
    // either win the CAS (get a success response) or lose it and
    // receive `-32001 workspace load already in progress` — the daemon
    // intentionally does NOT block them on the in-flight build. What
    // we MUST observe: exactly one builder invocation.
    #[derive(Debug, Default)]
    struct CountingBuilder {
        hits: Arc<AtomicU64>,
    }
    impl sqry_daemon::WorkspaceBuilder for CountingBuilder {
        fn build(&self, _workspace_root: &std::path::Path) -> Result<CodeGraph, DaemonError> {
            self.hits.fetch_add(1, Ordering::AcqRel);
            std::thread::sleep(std::time::Duration::from_millis(50));
            Ok(CodeGraph::new())
        }
    }
    let hits = Arc::new(AtomicU64::new(0));
    let builder = Arc::new(CountingBuilder {
        hits: Arc::clone(&hits),
    }) as Arc<dyn sqry_daemon::WorkspaceBuilder>;
    let server = TestServer::with_builder(builder).await;
    let dir = tempfile::tempdir().unwrap();
    let index_root = dir.path().to_string_lossy().to_string();

    let mut handles = Vec::new();
    for _ in 0..4 {
        let path = server.path.clone();
        let root = index_root.clone();
        handles.push(tokio::spawn(async move {
            let mut client = TestIpcClient::connect(&path).await;
            client.hello(1).await;
            client
                .request("daemon/load", json!({ "index_root": root }))
                .await
        }));
    }
    let mut successes = 0_usize;
    let mut transient_conflicts = 0_usize;
    for h in handles {
        let resp = h.await.expect("join");
        match &resp.payload {
            sqry_daemon::ipc::protocol::JsonRpcPayload::Success { .. } => successes += 1,
            sqry_daemon::ipc::protocol::JsonRpcPayload::Error { error } => {
                // -32001 "load already in progress" is the race loser.
                if error.code == -32001 {
                    transient_conflicts += 1;
                } else {
                    panic!("unexpected error: {error:?}");
                }
            }
        }
    }
    let observed_builds = hits.load(Ordering::Acquire);
    assert_eq!(
        observed_builds, 1,
        "expected exactly 1 builder invocation, got {observed_builds}"
    );
    assert!(successes >= 1, "at least one caller must succeed");
    assert_eq!(
        successes + transient_conflicts,
        4,
        "all callers must produce either Success or -32001 Conflict"
    );
    server.stop().await;
}
