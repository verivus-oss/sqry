//! A rebuild request the request check refuses is refused at the door
//! (integration round 7, S5 and S6 on the daemon side).
//!
//! `MacroOptionsRequest::validate` refuses an empty, blank or padded cfg
//! flag and an empty expand cache. The daemon ran it nowhere: an IPC
//! `daemon/rebuild` or a daemon-hosted `rebuild_index` carrying such a flag
//! was enqueued, parked behind a running rebuild, and (on the load route)
//! registered the workspace before any refusal. Both handlers now check the
//! request before they look anything up, enqueue or load, and the
//! daemon-hosted `rebuild_index` refuses a nested index without `force`
//! before the loader registers anything. The checks inside the build
//! preparation stay as the defence for any other caller.
//!
//! Each refusal here is answered while another rebuild of the same
//! workspace is held at its reservation (a request that reached the lane
//! would wait behind it), or leaves the manager with no entry for a
//! workspace that was not resident.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_core::project::ProjectRootMode;
use sqry_daemon::{DaemonConfig, TestCapture, WorkspaceKey, WorkspaceState};
use support::ipc::{TestIpcClient, expect_success};
use support::rebuild_fixtures::{
    err_of, file_bytes, index_fast_path, ipc_client, ipc_rebuild, mcp_error, mcp_rebuild_index,
    mcp_session, mixed_workspace, real_server, status_row,
};

/// The requests the check refuses, as `(label, extra fields)`.
fn refused_requests() -> Vec<(&'static str, Vec<(&'static str, serde_json::Value)>)> {
    vec![
        ("empty flag", vec![("cfg_flags", json!([""]))]),
        ("blank flag", vec![("cfg_flags", json!(["  "]))]),
        ("padded flag", vec![("cfg_flags", json!([" test"]))]),
        ("empty expand cache", vec![("expand_cache", json!(""))]),
    ]
}

/// A resident workspace whose rebuild is held at its reservation: each
/// refused request, on IPC and on MCP, is answered `-32602` naming the
/// root while the hold is still in place, and none of them runs an
/// iteration. Before the repair each one parked behind the held rebuild
/// and would have been answered only after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_request_is_answered_while_another_rebuild_runs() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let capture = Arc::new(TestCapture::new());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("capture installs");
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
    capture.arm_post_reservation_hold();
    let sock = server.path.clone();
    let held_root = root.clone();
    let held = tokio::spawn(async move {
        let mut c = TestIpcClient::connect(&sock).await;
        c.hello(1).await;
        ipc_rebuild(&mut c, &held_root, true, &[]).await
    });
    tokio::time::timeout(
        Duration::from_secs(60),
        capture.wait_until_post_reservation(),
    )
    .await
    .expect("the held rebuild reaches its reservation");

    let running = mcp_session(&server).await;
    let mut answered = 0;
    for (label, extra) in refused_requests() {
        let ipc = tokio::time::timeout(
            Duration::from_secs(10),
            ipc_rebuild(&mut client, &root, true, &extra),
        )
        .await
        .unwrap_or_else(|_| panic!("{label}: daemon/rebuild waited behind the held rebuild"));
        let ipc = err_of(&ipc).cloned();
        let mcp = tokio::time::timeout(
            Duration::from_secs(10),
            mcp_rebuild_index(running.peer(), &root, true, &extra),
        )
        .await
        .unwrap_or_else(|_| panic!("{label}: rebuild_index waited behind the held rebuild"));
        let mcp = mcp_error(label, mcp);
        println!(
            "{label}: ipc={:?} mcp={} {}",
            ipc.as_ref().map(|e| (e.code, e.message.clone())),
            mcp.code.0,
            mcp.message
        );
        let ipc = ipc.expect("refused on IPC");
        assert_eq!(ipc.code, -32602, "{label}");
        assert_eq!(mcp.code.0, -32602, "{label}");
        let named = format!("rebuild of {} refused", root.display());
        assert!(ipc.message.contains(&named), "{label}: {}", ipc.message);
        assert!(mcp.message.contains(&named), "{label}: {}", mcp.message);
        answered += 1;
    }
    let iterations_while_held = capture.iterations.lock().len();
    capture.release_post_reservation();
    let held = held.await.expect("join");
    assert!(err_of(&held).is_none(), "{:?}", err_of(&held));
    let row = status_row(&mut client, &root).await;
    println!(
        "answered while held: {answered}; iterations while held={iterations_while_held} after={} state={}",
        capture.iterations.lock().len(),
        row["state"]
    );
    assert_eq!(answered, 4, "every refused request was answered");
    assert_eq!(
        capture.iterations.lock().len(),
        1,
        "only the held rebuild ran an iteration"
    );
    assert_eq!(row["state"], json!("Loaded"));
    drop(running);
    drop(client);
    server.stop().await;
}

/// A workspace that is not resident: `rebuild_index` with a refused
/// request, and `rebuild_index force=false` of a subdirectory of an indexed
/// project, are refused with nothing built and no workspace registered.
/// Before the repair the first registered a `Failed` workspace (refused in
/// the loader's preparation) and the second built and persisted a nested
/// index. The control: `force=true` builds the nested index.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_rebuild_index_registers_no_workspace() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    index_fast_path(&root);
    let snapshot_before = file_bytes(GraphStorage::new(&root).snapshot_path());
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    let running = mcp_session(&server).await;

    for (label, extra) in refused_requests() {
        let refused = mcp_error(
            label,
            mcp_rebuild_index(running.peer(), &root, true, &extra).await,
        );
        let registered = server.manager.lookup(&key).map(|ws| ws.load_state());
        println!("{label}: code={} registered={registered:?}", refused.code.0);
        assert_eq!(refused.code.0, -32602, "{label}");
        assert_eq!(registered, None, "{label}: no workspace was registered");
    }
    assert_eq!(
        file_bytes(GraphStorage::new(&root).snapshot_path()),
        snapshot_before,
        "nothing was built"
    );

    let sub = root.join("nested");
    std::fs::create_dir_all(&sub).expect("nested dir");
    std::fs::write(sub.join("m.rs"), "pub fn in_nested() {}\n").expect("m.rs");
    let sub_key = WorkspaceKey::new(sub.clone(), ProjectRootMode::GitRoot, 0);
    let nested = mcp_error(
        "nested",
        mcp_rebuild_index(running.peer(), &sub, false, &[]).await,
    );
    let registered = server.manager.lookup(&sub_key).map(|ws| ws.load_state());
    println!(
        "nested without force: code={} registered={registered:?} built={}",
        nested.code.0,
        GraphStorage::new(&sub).exists()
    );
    assert_eq!(nested.code.0, -32602);
    assert_eq!(registered, None, "no workspace was registered");
    assert!(!GraphStorage::new(&sub).exists(), "nothing was built");

    let built = mcp_rebuild_index(running.peer(), &sub, true, &[]).await;
    assert!(built.is_ok(), "force builds the nested index: {built:?}");
    assert!(GraphStorage::new(&sub).exists());
    assert_eq!(
        server.manager.lookup(&sub_key).map(|ws| ws.load_state()),
        Some(WorkspaceState::Loaded)
    );
    drop(running);
    server.stop().await;
}

/// A hand-edited manifest recording a cfg flag no surface would record (an
/// empty one here): `daemon/load`, which builds with the record, and
/// `rebuild_index`, which keeps it, are refused (`-32602`, naming the flag
/// and both ways out) and write nothing; `rebuild_index` replacing the
/// recorded flags builds and records its own. Before the repair the flag
/// was reused and recorded again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recorded_cfg_flag_that_names_no_predicate_is_refused() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    index_fast_path(&root);
    let storage = GraphStorage::new(&root);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    manifest["macro_options"] = json!({ "cfg_flags": ["unix", ""], "expand_cache_dir": null });
    std::fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("hand edit");
    let edited = file_bytes(storage.manifest_path());
    let mut client = ipc_client(&server).await;
    let load = client
        .request(
            "daemon/load",
            json!({ "index_root": root.to_string_lossy() }),
        )
        .await;
    let load = err_of(&load).cloned().expect("daemon/load is refused");
    let running = mcp_session(&server).await;
    let mcp = mcp_error(
        "rebuild_index",
        mcp_rebuild_index(running.peer(), &root, true, &[]).await,
    );
    println!(
        "recorded \"\": daemon/load={} {} | rebuild_index={} {}",
        load.code, load.message, mcp.code.0, mcp.message
    );
    for (surface, code, message) in [
        ("daemon/load", load.code, load.message.clone()),
        ("rebuild_index", mcp.code.0, mcp.message.to_string()),
    ] {
        assert_eq!(code, -32602, "{surface}");
        assert!(
            message.contains("records cfg flag \"\"") && message.contains("--no-macro-options"),
            "{surface}: {message}"
        );
    }
    assert_eq!(
        file_bytes(storage.manifest_path()),
        edited,
        "nothing was written"
    );

    let replaced = mcp_rebuild_index(
        running.peer(),
        &root,
        true,
        &[("cfg_flags", json!(["test"]))],
    )
    .await;
    assert!(replaced.is_ok(), "{replaced:?}");
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(storage.manifest_path()).expect("manifest"))
            .expect("json");
    assert_eq!(after["macro_options"]["cfg_flags"], json!(["test"]));
    drop(running);
    drop(client);
    server.stop().await;
}
