//! STEP_6 (workspace-aware-cross-repo, 2026-04-26) iter-2 BLOCK fix —
//! `DaemonHello.logical_workspace` connection-level binding inherited
//! by subsequent `daemon/load` requests that omit the field.
//!
//! Codex iter-1 BLOCK item: the wire docs on `DaemonHello.logical_workspace`
//! promised every later `daemon/load` on the same connection that did
//! not itself supply a `logical_workspace` would inherit the binding.
//! The pre-fix router parsed the field and silently dropped it. These
//! tests drive the real handshake → load flow over a live IPC socket
//! and assert the binding is honoured end-to-end.

mod support;

use std::path::PathBuf;

use serde_json::json;
use sqry_daemon_protocol::{LogicalWorkspaceWire, WorkspaceId};
use support::ipc::{TestIpcClient, TestServer, expect_success};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_logical_workspace_inherited_by_subsequent_load() {
    // Setup: hello with `logical_workspace = Some(id)`, then issue a
    // `daemon/load` whose params OMIT `logical_workspace`. The daemon
    // must inherit the connection-level binding and register the
    // workspace under that `workspace_id`. We verify by querying
    // `daemon/workspaceStatus { workspace_id: id }` — the loaded
    // source root must surface in the aggregate.
    let server = TestServer::new().await;

    // Two physical source roots — one we'll load, the other declared
    // up-front on the hello binding so the connection-level binding
    // has a non-empty `source_roots` list (mirrors how a real client
    // declares the logical workspace before issuing per-root loads).
    let dir = tempfile::tempdir().unwrap();
    let primary = dir.path().join("primary");
    std::fs::create_dir(&primary).unwrap();
    let primary_canon = sqry_core::project::canonicalize_path(&primary).unwrap();

    let id = WorkspaceId::from_bytes([0x77; 32]);
    let binding = LogicalWorkspaceWire {
        workspace_id: id,
        source_roots: vec![primary_canon.clone()],
        source_root_bindings: Vec::new(),
        workspace_config_fingerprint: 0,
    };

    let mut client = TestIpcClient::connect(&server.path).await;
    let hello_resp = client.hello_with_binding(1, Some(binding)).await;
    assert!(hello_resp.compatible, "protocol version 1 must be accepted");

    // `daemon/load` WITHOUT `logical_workspace` in its params —
    // inheritance must kick in and route the load under the
    // connection's workspace_id.
    let load_resp = client
        .request(
            "daemon/load",
            json!({ "index_root": primary_canon.to_string_lossy() }),
        )
        .await;
    let load_result = expect_success(&load_resp);
    assert_eq!(
        load_result["result"]["root"]
            .as_str()
            .map(PathBuf::from)
            .unwrap(),
        primary_canon,
        "load returns the canonical primary source root"
    );

    // Verify inheritance: the workspace should be reachable via
    // `daemon/workspaceStatus { workspace_id: id }`. If the inheritance
    // were broken, the workspace would have been registered with
    // `workspace_id = None` and the status query would return
    // `-32004 WorkspaceNotLoaded` (since no entry carries this id).
    let status_resp = client
        .request(
            "daemon/workspaceStatus",
            json!({ "workspace_id": id.as_bytes() }),
        )
        .await;
    let status_result = expect_success(&status_resp);
    let source_roots = status_result["result"]["source_roots"]
        .as_array()
        .expect("source_roots is an array");
    assert!(
        source_roots.iter().any(|r| {
            r["source_root"].as_str().map(PathBuf::from) == Some(primary_canon.clone())
        }),
        "primary source root must surface in the aggregate keyed by the \
         connection-level workspace_id — got source_roots = {source_roots:?}",
    );

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn per_request_logical_workspace_overrides_connection_binding() {
    // Precedence rule: per-request `logical_workspace` always wins
    // over the connection-level binding. Hello with id_a; load with
    // params declaring id_b. The workspace must be registered under
    // id_b (not id_a).
    let server = TestServer::new().await;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let canon = sqry_core::project::canonicalize_path(&root).unwrap();

    let id_a = WorkspaceId::from_bytes([0xaa; 32]);
    let id_b = WorkspaceId::from_bytes([0xbb; 32]);
    let binding_a = LogicalWorkspaceWire {
        workspace_id: id_a,
        source_roots: vec![canon.clone()],
        source_root_bindings: Vec::new(),
        workspace_config_fingerprint: 0,
    };

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello_with_binding(1, Some(binding_a)).await;

    // Per-request override: declare id_b in params.
    let load_resp = client
        .request(
            "daemon/load",
            json!({
                "index_root": canon.to_string_lossy(),
                "logical_workspace": {
                    "workspace_id": id_b.as_bytes(),
                    "source_roots": [canon.to_string_lossy()],
                },
            }),
        )
        .await;
    expect_success(&load_resp);

    // The workspace must surface under id_b, NOT id_a. We assert both
    // halves to make the precedence rule explicit:
    //   - status query for id_b succeeds and contains the source root.
    //   - status query for id_a returns -32004 (no entry carries id_a).
    let status_b = client
        .request(
            "daemon/workspaceStatus",
            json!({ "workspace_id": id_b.as_bytes() }),
        )
        .await;
    let status_b_result = expect_success(&status_b);
    let source_roots = status_b_result["result"]["source_roots"]
        .as_array()
        .expect("id_b aggregate has source_roots");
    assert!(
        source_roots
            .iter()
            .any(|r| r["source_root"].as_str().map(PathBuf::from) == Some(canon.clone())),
        "per-request workspace_id must win over the connection-level binding",
    );

    let status_a = client
        .request(
            "daemon/workspaceStatus",
            json!({ "workspace_id": id_a.as_bytes() }),
        )
        .await;
    // id_a is unbound: the daemon surfaces -32004 WorkspaceNotLoaded.
    match &status_a.payload {
        sqry_daemon::ipc::protocol::JsonRpcPayload::Error { error } => {
            assert_eq!(
                error.code, -32004,
                "id_a must be absent — per-request override beat the connection-level binding",
            );
        }
        sqry_daemon::ipc::protocol::JsonRpcPayload::Success { result } => {
            panic!("id_a must NOT carry the loaded workspace; got success: {result:?}");
        }
    }

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_connection_binding_keeps_anonymous_load_semantics() {
    // Backwards compat: hello WITHOUT `logical_workspace` + load
    // WITHOUT `logical_workspace` reproduces the pre-STEP_6 anonymous
    // (per-source-root) shape exactly. The workspace is registered
    // with `workspace_id = None` and must NOT appear under any
    // logical-workspace aggregate.
    let server = TestServer::new().await;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    let canon = sqry_core::project::canonicalize_path(&root).unwrap();

    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await; // no binding

    let load_resp = client
        .request(
            "daemon/load",
            json!({ "index_root": canon.to_string_lossy() }),
        )
        .await;
    expect_success(&load_resp);

    // Any random workspace_id query must surface -32004 — no entry in
    // the manager carries one.
    let probe_id = WorkspaceId::from_bytes([0x12; 32]);
    let status = client
        .request(
            "daemon/workspaceStatus",
            json!({ "workspace_id": probe_id.as_bytes() }),
        )
        .await;
    match &status.payload {
        sqry_daemon::ipc::protocol::JsonRpcPayload::Error { error } => {
            assert_eq!(
                error.code, -32004,
                "anonymous load must not register under any workspace_id",
            );
        }
        sqry_daemon::ipc::protocol::JsonRpcPayload::Success { result } => {
            panic!("anonymous load should not surface an aggregate: {result:?}");
        }
    }

    server.stop().await;
}
