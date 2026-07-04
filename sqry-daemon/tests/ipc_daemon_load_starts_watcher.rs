//! Regression coverage for verivus-oss/sqry#461.
//!
//! `daemon/load` must wire the file watcher into the production bootstrap:
//! after a successful `get_or_load`, the handler calls
//! [`RebuildDispatcher::start_watching`], so a loaded workspace has a live
//! `SourceTreeWatcher` and edits trigger a debounced incremental rebuild
//! without a manual `sqry daemon rebuild`.
//!
//! Before the fix, `ensure_watching` was reachable only from test harnesses,
//! so `daemon/load` left zero watchers resident and graphs drifted silently.

mod support;

use serde_json::json;
use support::init_git_repo;
use support::ipc::{TestIpcClient, TestServer, expect_success};

/// A `daemon/load` of a git workspace leaves exactly one live watcher.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_load_starts_file_watcher_for_git_workspace() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    // `SourceTreeWatcher::new` requires a `.git` directory; seed one so the
    // watcher can actually start.
    init_git_repo(dir.path());

    // No watcher before the load.
    assert_eq!(
        server.dispatcher.watchers_len(),
        0,
        "no watcher should exist before any load"
    );

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let resp = client
        .request(
            "daemon/load",
            json!({ "index_root": dir.path().to_string_lossy() }),
        )
        .await;
    let result = expect_success(&resp);
    assert_eq!(result["result"]["state"], json!("Loaded"));

    // The handler starts the watcher synchronously before returning the
    // load response, so the count is deterministic by the time the client
    // observes success.
    assert_eq!(
        server.dispatcher.watchers_len(),
        1,
        "daemon/load must start exactly one file watcher for a git workspace (sqry#461)"
    );
    let status_resp = client.request("daemon/status", json!({})).await;
    let status = expect_success(&status_resp);
    assert_eq!(
        status["result"]["workspaces"][0]["watching"],
        json!(true),
        "daemon/status must expose the live watcher for a loaded git workspace"
    );

    // Idempotent: a second load of the same key does not spawn a duplicate.
    let resp2 = client
        .request(
            "daemon/load",
            json!({ "index_root": dir.path().to_string_lossy() }),
        )
        .await;
    expect_success(&resp2);
    assert_eq!(
        server.dispatcher.watchers_len(),
        1,
        "re-loading an already-watched workspace must not spawn a duplicate watcher"
    );

    drop(client);
    server.stop().await;
}

/// Watcher setup is best-effort: a non-git workspace still loads
/// successfully (no `.git` means `SourceTreeWatcher::new` fails, which is
/// logged and swallowed, not surfaced as a load error).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_load_non_git_workspace_still_succeeds_without_watcher() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let resp = client
        .request(
            "daemon/load",
            json!({ "index_root": dir.path().to_string_lossy() }),
        )
        .await;
    // Load succeeds even though the watcher could not be started.
    let result = expect_success(&resp);
    assert_eq!(result["result"]["state"], json!("Loaded"));

    assert_eq!(
        server.dispatcher.watchers_len(),
        0,
        "a non-git workspace loads but starts no watcher (best-effort, non-fatal)"
    );
    let status_resp = client.request("daemon/status", json!({})).await;
    let status = expect_success(&status_resp);
    assert_eq!(
        status["result"]["workspaces"][0]["watching"],
        json!(false),
        "daemon/status must expose the missing watcher for a loaded non-git workspace"
    );

    drop(client);
    server.stop().await;
}
