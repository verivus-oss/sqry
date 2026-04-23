//! Task 8 Phase 8a — `daemon/load` canonicalization policy tests.

mod support;

use serde_json::json;
use support::ipc::{TestIpcClient, TestServer, expect_error, expect_success};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relative_index_root_canonicalised_against_cwd() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    // The server resolves relative paths against its own CWD (not the
    // client's). Pass an absolute path instead — the test verifies
    // the canonicalisation still occurs.
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let resp = client
        .request(
            "daemon/load",
            json!({ "index_root": sub.to_string_lossy() }),
        )
        .await;
    let result = expect_success(&resp);
    let canon = sqry_core::project::canonicalize_path(&sub).unwrap();
    assert_eq!(
        result["result"]["root"]
            .as_str()
            .map(std::path::PathBuf::from),
        Some(canon)
    );
    server.stop().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symlink_to_directory_resolves_to_canonical_target() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;

    // Load via the symlink.
    let via_link = client
        .request(
            "daemon/load",
            json!({ "index_root": link.to_string_lossy() }),
        )
        .await;
    let via_link = expect_success(&via_link);
    // Unload so the second request hits the canonicalisation path fresh.
    let _ = client
        .request(
            "daemon/unload",
            json!({ "index_root": link.to_string_lossy() }),
        )
        .await;
    // Load via the target.
    let via_target = client
        .request(
            "daemon/load",
            json!({ "index_root": real.to_string_lossy() }),
        )
        .await;
    let via_target = expect_success(&via_target);

    assert_eq!(via_link["result"]["root"], via_target["result"]["root"]);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_as_index_root_emits_32602() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a.txt");
    std::fs::write(&f, b"x").unwrap();
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let resp = client
        .request("daemon/load", json!({ "index_root": f.to_string_lossy() }))
        .await;
    let err = expect_error(&resp);
    assert_eq!(err.code, -32602);
    assert!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("not a directory")),
        "expected not-a-directory reason, got: {err:?}",
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nonexistent_index_root_emits_32602() {
    let server = TestServer::new().await;
    let dir = tempfile::tempdir().unwrap();
    let ghost = dir.path().join("ghost");
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    let resp = client
        .request(
            "daemon/load",
            json!({ "index_root": ghost.to_string_lossy() }),
        )
        .await;
    let err = expect_error(&resp);
    assert_eq!(err.code, -32602);
    assert!(
        err.data
            .as_ref()
            .and_then(|d| d.get("reason"))
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("does not exist")),
        "expected does-not-exist reason, got: {err:?}",
    );
    server.stop().await;
}
