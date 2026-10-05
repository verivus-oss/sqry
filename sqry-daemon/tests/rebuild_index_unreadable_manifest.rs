//! A forced daemon-hosted `rebuild_index` over a manifest that cannot be
//! read falls back, warns and records the fallback, as the standalone
//! `rebuild_index` and `sqry index --force` do (surface parity W1 round 2,
//! design D9; DAEMON_FOLLOWUP).
//!
//! D9's daemon row deferred this to the daemon-hosted `rebuild_index`
//! persistence path, which now exists: before this round the call refused
//! with `-32001` `WorkspaceManifestUnreadable`, so the daemon had no path
//! that repaired a corrupt manifest. The other daemon rebuilds keep
//! refusing (the watcher, and `daemon/rebuild`, whose `force` the
//! dispatcher cannot tell from a watcher full rebuild). A resident
//! generation built from a manifest still protects its selection: the
//! fallback is refused with `-32021` when it would drop recorded ids
//! (decision D-i7-4's rule for a missing manifest).

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use serde_json::json;
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_daemon::DaemonConfig;
use support::ipc::expect_success;
use support::rebuild_fixtures::{
    err_of, file_bytes, index_fast_path, index_include_all, ipc_client, ipc_rebuild, manifest_json,
    mcp_error, mcp_payload, mcp_rebuild_index, mcp_session, mixed_workspace, real_server,
};

const CORRUPT: &[u8] = b"{";

fn corrupt_the_manifest(root: &std::path::Path) {
    std::fs::write(GraphStorage::new(root).manifest_path(), CORRUPT).expect("corrupt manifest");
}

/// Not resident, and resident from a fast-path index: `rebuild_index
/// force=true` rebuilds, and the manifest is readable again, recording the
/// fallback selection and the call's provenance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forced_rebuild_index_repairs_an_unreadable_manifest() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let running = mcp_session(&server).await;
    let mut client = ipc_client(&server).await;
    let mut checked = 0;
    for resident in [false, true] {
        let (_dir, root) = mixed_workspace();
        index_fast_path(&root);
        if resident {
            expect_success(
                &client
                    .request(
                        "daemon/load",
                        json!({ "index_root": root.to_string_lossy() }),
                    )
                    .await,
            );
        }
        corrupt_the_manifest(&root);
        let answered = mcp_rebuild_index(running.peer(), &root, true, &[]).await;
        let manifest = manifest_json(&root);
        println!(
            "resident={resident}: answered={} manifest readable={} high_cost_mode={:?} \
             build_command={:?}",
            answered.is_ok(),
            manifest.is_some(),
            manifest
                .as_ref()
                .map(|m| m["plugin_selection"]["high_cost_mode"].clone()),
            manifest
                .as_ref()
                .map(|m| m["build_provenance"]["build_command"].clone()),
        );
        mcp_payload("forced rebuild_index", &answered);
        let manifest = manifest.expect("the manifest is readable again");
        assert_eq!(
            manifest["plugin_selection"]["high_cost_mode"],
            json!("fast_path_default"),
            "resident={resident}: the fallback selection is recorded"
        );
        assert_eq!(
            manifest["build_provenance"]["build_command"],
            json!("daemon:rebuild_index"),
            "resident={resident}"
        );
        checked += 1;
    }
    assert_eq!(checked, 2);
    drop(running);
    drop(client);
    server.stop().await;
}

/// A resident generation built from an `include_all` manifest: the
/// fallback would drop `json`, so the forced rebuild is refused with
/// `-32021` naming it and the restore command, and the corrupt file is
/// left as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fallback_does_not_narrow_a_resident_selection() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    index_include_all(&root);
    let mut client = ipc_client(&server).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": root.to_string_lossy() }),
            )
            .await,
    );
    corrupt_the_manifest(&root);
    let running = mcp_session(&server).await;
    let refused = mcp_error(
        "narrowing fallback",
        mcp_rebuild_index(running.peer(), &root, true, &[]).await,
    );
    println!(
        "include_all resident: {} {}",
        refused.code.0, refused.message
    );
    assert_eq!(refused.code.0, -32602);
    let data = refused.data.expect("data");
    assert_eq!(data["kind"], json!("rebuild_would_narrow_selection"));
    assert_eq!(data["details"]["missing_plugin_ids"], json!(["json"]));
    assert_eq!(
        file_bytes(GraphStorage::new(&root).manifest_path()),
        Some(CORRUPT.to_vec()),
        "nothing was written"
    );
    drop(running);
    drop(client);
    server.stop().await;
}

/// The other side: `daemon/rebuild --force` over the same corrupt manifest
/// still refuses with `-32001` naming the file and the repair, and writes
/// nothing (D9's daemon rebuild row is unchanged).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_rebuild_still_refuses_an_unreadable_manifest() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    index_fast_path(&root);
    let mut client = ipc_client(&server).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": root.to_string_lossy() }),
            )
            .await,
    );
    corrupt_the_manifest(&root);
    let refused = ipc_rebuild(&mut client, &root, true, &[]).await;
    let error = err_of(&refused).cloned().expect("refused");
    println!(
        "daemon/rebuild over a corrupt manifest: {} {}",
        error.code, error.message
    );
    assert_eq!(error.code, -32001);
    assert!(
        error
            .data
            .as_ref()
            .is_some_and(|data| data["repair_command"].as_str().is_some()),
        "{error:?}"
    );
    assert_eq!(
        file_bytes(GraphStorage::new(&root).manifest_path()),
        Some(CORRUPT.to_vec())
    );
    drop(client);
    server.stop().await;
}

/// Audit S3 (D2G): a parked `daemon/rebuild` merged with a parked
/// `rebuild_index force=true` (both with empty macro requests, so they
/// merge) runs with `daemon/rebuild`'s refusal of an unreadable manifest
/// (decision D-i8-40), not `rebuild_index`'s fall-back. Both callers are
/// told the refusal and the corrupt manifest is left as it is. Before the
/// repair the merged entry took `rebuild_index`'s policy, so the
/// `daemon/rebuild` caller was answered with a fall-back rebuild it was
/// owed a refusal for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_daemon_rebuild_merged_with_a_rebuild_index_keeps_its_refusal() {
    use sqry_core::project::ProjectRootMode;
    use sqry_daemon::{TestCapture, WorkspaceKey};
    use std::sync::Arc;
    use std::time::Duration;

    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    index_fast_path(&root);
    let mut client = ipc_client(&server).await;
    let load = client
        .request(
            "daemon/load",
            json!({ "index_root": root.to_string_lossy() }),
        )
        .await;
    assert!(err_of(&load).is_none(), "{load:?}");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    let ws = server.manager.lookup(&key).expect("resident");
    let capture = Arc::new(TestCapture::new());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("capture installs");
    // R0 takes the runner role and holds after its reservation.
    capture.arm_post_reservation_hold();
    let r0 = {
        let socket = server.path.clone();
        let root = root.clone();
        tokio::spawn(async move {
            let mut c = support::ipc::TestIpcClient::connect(&socket).await;
            c.hello(1).await;
            ipc_rebuild(&mut c, &root, true, &[]).await
        })
    };
    tokio::time::timeout(
        Duration::from_secs(60),
        capture.wait_until_post_reservation(),
    )
    .await
    .expect("R0 reserved");
    let parked = |ws: &sqry_daemon::workspace::LoadedWorkspace| {
        ws.rebuild_lane
            .try_lock()
            .ok()
            .and_then(|lane| lane.as_ref().map(|p| p.waiters.pending()))
    };
    // R1, a daemon/rebuild, parks behind it.
    let r1 = {
        let socket = server.path.clone();
        let root = root.clone();
        tokio::spawn(async move {
            let mut c = support::ipc::TestIpcClient::connect(&socket).await;
            c.hello(1).await;
            ipc_rebuild(&mut c, &root, true, &[]).await
        })
    };
    for _ in 0..1000 {
        if parked(&ws) == Some(1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // R2, a forced rebuild_index, merges into R1's entry.
    let running = mcp_session(&server).await;
    let r2 = {
        let peer = running.peer().clone();
        let root = root.clone();
        tokio::spawn(async move { mcp_rebuild_index(&peer, &root, true, &[]).await })
    };
    for _ in 0..1000 {
        if parked(&ws) == Some(2) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(parked(&ws), Some(2), "R1 and R2 share one parked entry");
    // The manifest becomes unreadable while they wait.
    corrupt_the_manifest(&root);
    capture.release_post_reservation();
    let a0 = r0.await.expect("join");
    let a1 = r1.await.expect("join");
    let a2 = r2.await.expect("join");
    let r1_err = err_of(&a1).map(|e| (e.code, e.message.clone()));
    println!(
        "D2G R0={:?} R1={r1_err:?} R2 ok={} manifest={:?}",
        err_of(&a0).map(|e| e.code),
        a2.is_ok(),
        file_bytes(GraphStorage::new(&root).manifest_path()).map(|b| b.len())
    );
    assert_eq!(
        r1_err.as_ref().map(|(code, _)| *code),
        Some(-32001),
        "the merged daemon/rebuild keeps its refusal of an unreadable manifest: {a1:?}"
    );
    assert!(
        a2.is_err(),
        "the rebuild_index merged with it shares the refusal: {a2:?}"
    );
    assert_eq!(
        file_bytes(GraphStorage::new(&root).manifest_path()).as_deref(),
        Some(CORRUPT),
        "nothing was written over the corrupt manifest"
    );
    drop(running);
    drop(client);
    server.stop().await;
}
