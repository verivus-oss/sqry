//! A failed durable persist leaves the index it started from, and the next
//! rebuild cannot silently narrow the selection (integration round 7, S4).
//!
//! Before the repair a persist that failed after its first step (here, the
//! analyses directory refuses a write) left no manifest and a new snapshot
//! (E11), and the next plain `daemon/rebuild` then found no recorded
//! selection and recorded the fast path in place of `include_all`, with no
//! `-32021` (E3). The core transaction now puts the old pair back; and with
//! no manifest at all (a crash, a hand removal), the daemon compares the
//! roster it is about to record against the resident generation's record.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serde_json::{Value, json};
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_core::project::ProjectRootMode;
use sqry_daemon::{DaemonConfig, WorkspaceKey, WorkspaceState};
use support::ipc::expect_success;
use support::rebuild_fixtures::{
    err_of, file_bytes, index_include_all, ipc_client, ipc_rebuild, manifest_json, mcp_error,
    mcp_rebuild_index, mcp_session, mixed_workspace, real_server,
};

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Make the analyses directory refuse writes; returns what to undo.
fn lock_the_analyses(root: &Path) -> Vec<std::path::PathBuf> {
    let analysis = root.join(".sqry").join("analysis");
    let files: Vec<_> = std::fs::read_dir(&analysis)
        .expect("analysis dir")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    for file in &files {
        set_mode(file, 0o444);
    }
    set_mode(&analysis, 0o555);
    let probe = analysis.join(".probe");
    assert!(
        std::fs::write(&probe, b"x").is_err(),
        "precondition: the analyses directory refuses a write"
    );
    files
}

fn unlock_the_analyses(root: &Path, files: &[std::path::PathBuf]) {
    set_mode(&root.join(".sqry").join("analysis"), 0o755);
    for file in files {
        set_mode(file, 0o644);
    }
}

fn records_json(manifest: Option<&Value>) -> bool {
    manifest
        .and_then(|m| m["plugin_selection"]["active_plugin_ids"].as_array())
        .is_some_and(|ids| ids.iter().any(|id| id == "json"))
}

/// E11 and E3: `rebuild_index force=true` fails at the analyses; the
/// manifest and the snapshot are the old ones, byte for byte, the
/// workspace is `Failed` and still serves its graph; and once the
/// directory is writable again a plain `daemon/rebuild` keeps the
/// recorded `include_all` selection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_persist_leaves_the_old_index_and_the_next_rebuild_keeps_its_selection() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    index_include_all(&root);
    let storage = GraphStorage::new(&root);
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
    let manifest_before = file_bytes(storage.manifest_path());
    let snapshot_before = file_bytes(storage.snapshot_path());

    let locked = lock_the_analyses(&root);
    let running = mcp_session(&server).await;
    let failed = mcp_error(
        "the persist fails",
        mcp_rebuild_index(running.peer(), &root, true, &[]).await,
    );
    unlock_the_analyses(&root, &locked);
    let ws = server.manager.lookup(&key).expect("resident");
    let manifest_after = file_bytes(storage.manifest_path());
    let snapshot_after = file_bytes(storage.snapshot_path());
    println!(
        "E11 failed persist: code={} state={:?} manifest unchanged={} snapshot unchanged={}",
        failed.code.0,
        ws.load_state(),
        manifest_after == manifest_before,
        snapshot_after == snapshot_before
    );
    assert_eq!(ws.load_state(), WorkspaceState::Failed);
    assert!(manifest_after.is_some(), "the manifest is not missing");
    assert!(
        manifest_after == manifest_before,
        "the manifest is the old one"
    );
    assert!(
        snapshot_after == snapshot_before,
        "the snapshot is the old one, so the pair is not torn"
    );

    let plain = ipc_rebuild(&mut client, &root, false, &[]).await;
    let after = manifest_json(&root);
    println!(
        "E3 plain rebuild after the failure: error={:?} records json={} high_cost={:?}",
        err_of(&plain).map(|e| e.code),
        records_json(after.as_ref()),
        after
            .as_ref()
            .map(|m| m["plugin_selection"]["high_cost_mode"].clone())
    );
    assert!(err_of(&plain).is_none(), "{:?}", err_of(&plain));
    assert!(
        records_json(after.as_ref()),
        "the recorded selection survives the failure and the next rebuild"
    );
    assert_eq!(
        after
            .as_ref()
            .map(|m| m["plugin_selection"]["high_cost_mode"].clone()),
        Some(json!("include_all"))
    );
    drop(running);
    drop(client);
    server.stop().await;
}

/// With no manifest at all, a rebuild of a resident workspace built from
/// an `include_all` manifest is refused (`-32021`, naming `json` and the
/// restore command) instead of recording the fast path; nothing is
/// written and the workspace keeps its state and its graph.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rebuild_with_the_manifest_gone_does_not_narrow_the_resident_selection() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    index_include_all(&root);
    let storage = GraphStorage::new(&root);
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
    let ws = server.manager.lookup(&key).expect("resident");
    let graph_before = ws.graph();
    std::fs::remove_file(storage.manifest_path()).expect("the manifest goes");
    let snapshot_before = file_bytes(storage.snapshot_path());

    let refused = ipc_rebuild(&mut client, &root, true, &[]).await;
    let error = err_of(&refused).cloned();
    println!(
        "S4 rebuild with the manifest gone: error={:?} state={:?} manifest present={}",
        error.as_ref().map(|e| (e.code, e.message.clone())),
        ws.load_state(),
        storage.manifest_path().exists()
    );
    let error = error.expect("the rebuild is refused");
    assert_eq!(error.code, -32021);
    let data = error.data.expect("refusal data");
    assert_eq!(data["missing_plugin_ids"], json!(["json"]));
    assert!(
        data["restore_command"]
            .as_str()
            .is_some_and(|command| command.contains("--include-high-cost")),
        "{data}"
    );
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
    assert!(std::sync::Arc::ptr_eq(&graph_before, &ws.graph()));
    assert!(!storage.manifest_path().exists(), "nothing was written");
    assert_eq!(file_bytes(storage.snapshot_path()), snapshot_before);
    drop(client);
    server.stop().await;
}
