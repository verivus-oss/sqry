//! A refused or failed load of a workspace that is not resident leaves no
//! `Failed` slot that breaks later queries of a healthy index (B2 of the
//! round 7 daemon audit).
//!
//! The daemon-hosted `rebuild_index` takes its load route when no graph is
//! resident, and `daemon/load` loads through the same manager gate. Both
//! registered the workspace before the builder's preparation ran, so a
//! refusal raised there (a requested expand cache that does not exist, a
//! recorded cfg flag no surface would record, a recorded expand cache that
//! is gone) left a `Failed` slot carrying the refusal. Every later query of
//! the root read that slot and was answered `-32603` "classify_for_serve
//! returned unexpected error", although the index on disk was intact and
//! the standalone server served it; only `daemon/unload` recovered. A
//! failure of the load route's durable persist (E4) left the same slot,
//! answered `-32001` on every query.
//!
//! Each case below is refused (or fails) as before, leaves no row in
//! `daemon/status`, and is followed by a query of the index on both
//! daemon surfaces (MCP and IPC), which must be served.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::Path;

use serde_json::{Value, json};
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_daemon::DaemonConfig;
use support::ipc::{TestIpcClient, expect_success};
use support::rebuild_fixtures::{
    err_of, index_fast_path, ipc_client, mcp_call, mcp_error, mcp_payload, mcp_rebuild_index,
    mcp_session, mixed_workspace, real_server, status_row,
};

/// Rewrite the manifest at `root` with `macro_options` as its record, the
/// hand edit the audit made to plant a record no surface would write.
fn plant_macro_record(root: &Path, record: Value) {
    let storage = GraphStorage::new(root);
    let bytes = std::fs::read(storage.manifest_path()).expect("read manifest");
    let mut manifest: Value = serde_json::from_slice(&bytes).expect("parse manifest");
    manifest["macro_options"] = record;
    std::fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("render manifest"),
    )
    .expect("write manifest");
}

/// A query of `root` on the daemon-hosted MCP and on IPC, each of which
/// must be served; then the root's `daemon/status` row, which must not be
/// `Failed` (the queries made it resident again).
async fn assert_served_after(
    label: &str,
    client: &mut TestIpcClient,
    peer: &rmcp::Peer<rmcp::RoleClient>,
    root: &Path,
) {
    let args = json!({ "path": root.to_string_lossy(), "query": "func_alpha" });
    let mcp = mcp_call(peer, "semantic_search", args.clone()).await;
    if let Err(err) = &mcp {
        panic!("{label}: the daemon MCP query of the healthy index must be served, got {err:?}");
    }
    let payload = mcp_payload(label, &mcp);
    println!("{label}: MCP query served, total={}", payload["total"]);
    let ipc = client.request("semantic_search", args).await;
    assert!(
        err_of(&ipc).is_none(),
        "{label}: the IPC query of the healthy index must be served, got {:?}",
        err_of(&ipc)
    );
    let served = expect_success(&ipc);
    println!(
        "{label}: IPC query served, workspace_state={}",
        served["meta"]["workspace_state"]
    );
    let row = status_row(client, root).await;
    println!("{label}: status row after the queries: {row}");
    assert_ne!(
        row["state"],
        json!("Failed"),
        "{label}: a refused load must leave no Failed slot; row: {row}"
    );
}

/// One refused `rebuild_index` case: how the indexed root is prepared, and
/// the request's extra fields for it.
struct Case {
    label: &'static str,
    prepare: fn(&Path),
    extra: fn(&Path) -> Vec<(&'static str, Value)>,
}

/// `rebuild_index force=true` on the load route, refused or failed for each
/// of the audit's cases, then a query of the healthy index on both
/// surfaces.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_rebuild_index_load_route_leaves_the_index_servable() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let mut client = ipc_client(&server).await;
    let running = mcp_session(&server).await;

    let cases = [
        Case {
            label: "E1 requested expand cache missing (absolute)",
            prepare: |_| {},
            extra: |root| {
                vec![(
                    "expand_cache",
                    json!(root.join("no-such-cache").to_string_lossy()),
                )]
            },
        },
        Case {
            label: "E1b requested expand cache missing (relative)",
            prepare: |_| {},
            extra: |_| vec![("expand_cache", json!("nope"))],
        },
        Case {
            label: "E1c requested expand cache blank",
            prepare: |_| {},
            extra: |_| vec![("expand_cache", json!("  "))],
        },
        Case {
            label: "E2 recorded padded cfg flag",
            prepare: |root| plant_macro_record(root, json!({ "cfg_flags": [" a"] })),
            extra: |_| vec![],
        },
        Case {
            label: "E2b recorded blank cfg flag",
            prepare: |root| plant_macro_record(root, json!({ "cfg_flags": ["  "] })),
            extra: |_| vec![],
        },
        Case {
            label: "E3 recorded expand cache missing",
            prepare: |root| {
                let gone = root.join("cache-that-was-removed");
                plant_macro_record(root, json!({ "expand_cache_dir": gone.to_string_lossy() }));
            },
            extra: |_| vec![],
        },
    ];

    let mut dirs = Vec::new();
    for Case {
        label,
        prepare,
        extra,
    } in cases
    {
        let (dir, root) = mixed_workspace();
        index_fast_path(&root);
        prepare(&root);
        let extra = extra(&root);
        let refused = mcp_error(
            label,
            mcp_rebuild_index(running.peer(), &root, true, &extra).await,
        );
        println!(
            "{label}: refused code={} message={}",
            refused.code.0, refused.message
        );
        assert_eq!(refused.code.0, -32602, "{label}: {}", refused.message);
        assert!(
            refused.message.contains("refused"),
            "{label}: a refusal: {}",
            refused.message
        );
        assert_served_after(label, &mut client, running.peer(), &root).await;
        dirs.push(dir);
    }

    // E4: the durable persist fails on the load route (the graph directory
    // is read-only, so the old manifest cannot be set aside). The persist
    // left the index as it found it, so a query of it must be served.
    let (dir, root) = mixed_workspace();
    index_fast_path(&root);
    let graph_dir = GraphStorage::new(&root).graph_dir().to_path_buf();
    set_mode(&graph_dir, 0o555);
    let failed = mcp_rebuild_index(running.peer(), &root, true, &[]).await;
    set_mode(&graph_dir, 0o755);
    let failed = mcp_error("E4 persist failure", failed);
    println!(
        "E4 persist failure: code={} message={}",
        failed.code.0, failed.message
    );
    assert!(
        failed
            .message
            .contains("durable graph persistence transaction failed"),
        "E4: the persist failed: {}",
        failed.message
    );
    assert_served_after("E4 persist failure", &mut client, running.peer(), &root).await;
    dirs.push(dir);

    drop(running);
    drop(client);
    server.stop().await;
}

/// `daemon/load` refused for a recorded cfg flag (E5 and F2): refused as
/// before, and again on a second call (the record is still unusable), but a
/// query of the index is served in between and after, where before every
/// query was answered `-32603` until `daemon/unload`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_daemon_load_leaves_the_index_servable() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let mut client = ipc_client(&server).await;
    let running = mcp_session(&server).await;
    let (_dir, root) = mixed_workspace();
    index_fast_path(&root);
    plant_macro_record(&root, json!({ "cfg_flags": [" a"] }));
    for call in 1..=2 {
        let label = format!("E5 daemon/load call {call}");
        let resp = client
            .request(
                "daemon/load",
                json!({ "index_root": root.to_string_lossy() }),
            )
            .await;
        let err = err_of(&resp)
            .unwrap_or_else(|| panic!("{label}: refused for the recorded flag"))
            .clone();
        println!("{label}: refused code={} message={}", err.code, err.message);
        assert_eq!(err.code, -32602, "{label}: {}", err.message);
        assert!(
            err.message.contains("records cfg flag"),
            "{label}: {}",
            err.message
        );
        // The query evicts nothing to serve: unload the reloaded
        // generation so the second call takes the load route again.
        assert_served_after(&label, &mut client, running.peer(), &root).await;
        expect_success(
            &client
                .request(
                    "daemon/unload",
                    json!({ "index_root": root.to_string_lossy() }),
                )
                .await,
        );
    }
    drop(running);
    drop(client);
    server.stop().await;
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}
