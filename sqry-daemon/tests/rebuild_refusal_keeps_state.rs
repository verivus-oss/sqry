//! A refused rebuild leaves the workspace in the state it found it
//! (integration round 7, S3 and plant P22).
//!
//! A refusal is raised before anything is written, so the workspace is
//! exactly as the iteration found it. Before the repair every refusal arm
//! moved it to `Loaded`, so a `Failed` workspace (a persist failure, its
//! stale graph still served) came out of a refused `daemon/rebuild` as
//! `Loaded` beside its old `last_error`, and was then served as fresh with
//! no stale-serve expiry (E2). A refusal at the persist (under its lock)
//! must be recorded as a refusal too: routed to the failure recorder (P22)
//! it would mark a workspace `Failed` for a rebuild that wrote nothing.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_core::project::ProjectRootMode;
use sqry_daemon::{DaemonConfig, TestCapture, WorkspaceKey, WorkspaceState};
use support::ipc::{TestIpcClient, expect_success};
use support::rebuild_fixtures::{
    err_of, file_bytes, index_fast_path, ipc_client, ipc_rebuild, mcp_error, mcp_rebuild_index,
    mcp_session, mixed_workspace, real_server, status_row,
};

/// Make the next durable persist fail: the snapshot path becomes a
/// directory, which the atomic rename cannot replace.
fn break_the_snapshot(root: &std::path::Path) {
    let storage = GraphStorage::new(root);
    std::fs::remove_file(storage.snapshot_path()).expect("remove snapshot");
    std::fs::create_dir_all(storage.snapshot_path()).expect("snapshot as a directory");
}

/// E2: a persist failure leaves the workspace `Failed`; a refused rebuild
/// afterwards (a missing expand cache, on IPC and on MCP) answers its
/// refusal and leaves the workspace `Failed`, its earlier failure still
/// recorded and its retry count unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_of_a_failed_workspace_leaves_it_failed_on_both_surfaces() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    index_fast_path(&root);
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    let mut client = ipc_client(&server).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": root.to_string_lossy() }),
            )
            .await,
    );
    break_the_snapshot(&root);
    let failed = ipc_rebuild(&mut client, &root, true, &[]).await;
    let ws = server.manager.lookup(&key).expect("resident");
    assert_eq!(err_of(&failed).map(|e| e.code), Some(-32001));
    assert_eq!(ws.load_state(), WorkspaceState::Failed);
    let failure_before = ws.last_error.read().as_ref().map(ToString::to_string);
    let retries_before = ws.retry_count.load(std::sync::atomic::Ordering::Acquire);
    assert!(failure_before.is_some());

    let missing = root.join("no-such-cache");
    let refused = ipc_rebuild(
        &mut client,
        &root,
        true,
        &[("expand_cache", json!(missing.to_string_lossy()))],
    )
    .await;
    let row_after_ipc = status_row(&mut client, &root).await;
    let running = mcp_session(&server).await;
    let mcp = mcp_error(
        "MCP refusal",
        mcp_rebuild_index(
            running.peer(),
            &root,
            true,
            &[("expand_cache", json!(missing.to_string_lossy()))],
        )
        .await,
    );
    let row_after_mcp = status_row(&mut client, &root).await;
    println!(
        "E2 refusal of a Failed workspace: ipc={:?} state={} | mcp={} state={} | last_error kept={}",
        err_of(&refused).map(|e| e.code),
        row_after_ipc["state"],
        mcp.code.0,
        row_after_mcp["state"],
        row_after_mcp["last_error"].as_str() == failure_before.as_deref()
    );
    assert_eq!(err_of(&refused).map(|e| e.code), Some(-32022));
    assert_eq!(row_after_ipc["state"], json!("Failed"), "{row_after_ipc}");
    assert_eq!(mcp.code.0, -32602);
    assert_eq!(row_after_mcp["state"], json!("Failed"), "{row_after_mcp}");
    assert_eq!(
        row_after_mcp["last_error"].as_str(),
        failure_before.as_deref(),
        "the earlier failure stays recorded"
    );
    assert_eq!(
        ws.retry_count.load(std::sync::atomic::Ordering::Acquire),
        retries_before,
        "a refusal counts no attempt"
    );
    drop(running);
    drop(client);
    server.stop().await;
}

/// Rewrite the manifest at `root` while a rebuild is held after its
/// reservation, as another writer would, then release the rebuild and
/// return its answer and the rewritten manifest's bytes.
async fn rebuild_across_a_rewrite(
    server: &support::ipc::TestServer,
    capture: &Arc<TestCapture>,
    root: &std::path::Path,
    rewrite: impl FnOnce(&mut Value),
) -> (sqry_daemon::JsonRpcResponse, Option<Vec<u8>>) {
    capture.arm_post_reservation_hold();
    let sock = server.path.clone();
    let rebuild_root = root.to_path_buf();
    let rebuild = tokio::spawn(async move {
        let mut c = TestIpcClient::connect(&sock).await;
        c.hello(1).await;
        ipc_rebuild(&mut c, &rebuild_root, true, &[]).await
    });
    tokio::time::timeout(
        Duration::from_secs(60),
        capture.wait_until_post_reservation(),
    )
    .await
    .expect("the rebuild reaches its reservation");
    let storage = GraphStorage::new(root);
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(storage.manifest_path()).expect("manifest"))
            .expect("manifest parses");
    rewrite(&mut manifest);
    std::fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("serialise"),
    )
    .expect("rewrite manifest");
    let rewritten = file_bytes(storage.manifest_path());
    capture.release_post_reservation();
    (rebuild.await.expect("join"), rewritten)
}

/// P22: a refusal of the resolution under the persist lock (the record was
/// rewritten during the build to name a plugin this binary did not
/// compile) is a refusal. That refusal wrote nothing, so the workspace
/// stays `Loaded` with no recorded failure and the manifest is the one
/// rewritten during the build, byte for byte. (Until round 8 this plant
/// widened the record instead; a widened record is now built again and
/// published, decision D-i8-5, in the next test.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_under_the_persist_lock_is_a_refusal() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let capture = Arc::new(TestCapture::new());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("capture installs");
    let (_dir, root) = mixed_workspace();
    index_fast_path(&root);
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    let mut client = ipc_client(&server).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": root.to_string_lossy() }),
            )
            .await,
    );
    let (answered, rewritten) = rebuild_across_a_rewrite(&server, &capture, &root, |manifest| {
        manifest["plugin_selection"]["active_plugin_ids"]
            .as_array_mut()
            .expect("recorded ids")
            .push(json!("no-such-plugin"));
    })
    .await;
    let ws = server.manager.lookup(&key).expect("resident");
    let row = status_row(&mut client, &root).await;
    println!(
        "P22 persist-time refusal: answer={:?} state={} last_error={} retry_count={}",
        err_of(&answered).map(|e| e.code),
        row["state"],
        row["last_error"],
        row["retry_count"]
    );
    assert_eq!(err_of(&answered).map(|e| e.code), Some(-32005));
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
    assert!(
        ws.last_error.read().is_none(),
        "a refusal records no failure"
    );
    assert_eq!(row["retry_count"], json!(0));
    assert_eq!(
        file_bytes(GraphStorage::new(&root).manifest_path()),
        rewritten,
        "the refusal wrote nothing"
    );
    drop(client);
    server.stop().await;
}

/// D-i8-5: a record widened during the build (a concurrent `sqry index
/// --include-high-cost` adds `json`) is the record the rebuild publishes:
/// the persist resolves the inputs again under the lock, builds again with
/// them, and records them. The workspace ends `Loaded` with no failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_record_widened_during_the_build_is_the_one_published() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let capture = Arc::new(TestCapture::new());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("capture installs");
    let (_dir, root) = mixed_workspace();
    index_fast_path(&root);
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    let mut client = ipc_client(&server).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": root.to_string_lossy() }),
            )
            .await,
    );
    let (answered, _rewritten) = rebuild_across_a_rewrite(&server, &capture, &root, |manifest| {
        manifest["plugin_selection"]["active_plugin_ids"]
            .as_array_mut()
            .expect("recorded ids")
            .push(json!("json"));
        manifest["plugin_selection"]["high_cost_mode"] = json!("include_all");
    })
    .await;
    expect_success(&answered);
    let ws = server.manager.lookup(&key).expect("resident");
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
    assert!(ws.last_error.read().is_none(), "no failure recorded");
    let manifest = GraphStorage::new(&root)
        .load_manifest()
        .expect("the rebuild published a manifest");
    let selection = manifest.plugin_selection.expect("a recorded selection");
    assert!(
        selection.active_plugin_ids.iter().any(|id| id == "json"),
        "the rebuild dropped the json plugin recorded during its build: {:?}",
        selection.active_plugin_ids
    );
    assert_eq!(selection.high_cost_mode.as_deref(), Some("include_all"));
    drop(client);
    server.stop().await;
}
