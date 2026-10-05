//! The rebuild path names a workspace by the key it is registered under
//! (integration round 7, B1).
//!
//! An anonymous `WorkspaceKey` resolves to the registered workspace by its
//! source root, so a caller may hold a key that differs from the
//! registered one in its root mode: the pinned preload registers
//! `ProjectRootMode::WorkspaceFolder`, while the CLI's `daemon/load`, the
//! daemon-hosted `rebuild_index` and the tool-call acquirer all send
//! `GitRoot`. Before the repair the rebuild iteration's publish recheck,
//! the watcher map and the serve classification looked the caller's key up
//! exactly, so a `rebuild_index force=true` of a pinned workspace rewrote
//! the index and then answered `-32603` "evicted mid-rebuild" with the
//! workspace stuck in `Rebuilding`; an edit after `sqry daemon load` of a
//! pinned workspace was persisted but never published; and a tool call
//! during a pinned workspace's rebuild was refused `-32004`. Every test
//! here runs once per registered root mode, so the `GitRoot` legs are the
//! controls the repair had to keep passing.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use sqry_core::project::ProjectRootMode;
use sqry_daemon::{DaemonConfig, TestCapture, TestGate, WorkspaceKey, WorkspaceState};
use support::ipc::expect_success;
use support::rebuild_fixtures::{
    err_of, index_fast_path, ipc_client, ipc_rebuild, manifest_json, mcp_payload,
    mcp_rebuild_index, mcp_session, mixed_workspace, real_server, status_row, wait_until_blocking,
};

/// The two anonymous root modes a workspace is registered under in
/// production: the pinned preload's and the CLI's.
const MODES: [ProjectRootMode; 2] = [ProjectRootMode::WorkspaceFolder, ProjectRootMode::GitRoot];

/// The root mode a caller other than the registering one sends.
fn other_mode(mode: ProjectRootMode) -> ProjectRootMode {
    match mode {
        ProjectRootMode::WorkspaceFolder => ProjectRootMode::GitRoot,
        _ => ProjectRootMode::WorkspaceFolder,
    }
}

/// Wait until the runner has released the role (the caller is answered as
/// soon as its own iteration ends, before the drain loop's last gate).
fn runner_released(ws: &sqry_daemon::LoadedWorkspace) -> bool {
    wait_until_blocking(Duration::from_secs(10), || {
        !ws.rebuild_in_flight.load(Ordering::Acquire)
    })
}

/// B1 on the MCP route: `rebuild_index force=true` of a workspace
/// registered under either mode rebuilds it in place and answers success,
/// leaving it `Loaded`, published, with no runner and no cancellation
/// left, and the manifest rewritten.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebuild_index_force_rebuilds_a_workspace_registered_under_either_mode() {
    for mode in MODES {
        let label = format!("registered {}", mode.as_str());
        let (server, builder) = real_server(DaemonConfig::default()).await;
        let (_dir, root) = mixed_workspace();
        index_fast_path(&root);
        let key = WorkspaceKey::new(root.clone(), mode, 0);
        let graph_before = server
            .manager
            .get_or_load(&key, builder.as_ref(), 1)
            .expect("the workspace loads");
        server.dispatcher.start_watching(&key);
        let built_before = manifest_json(&root).expect("manifest")["built_at"].clone();

        let running = mcp_session(&server).await;
        let outcome = mcp_rebuild_index(running.peer(), &root, true, &[]).await;
        let ws = server.manager.lookup(&key).expect("resident");
        let released = runner_released(&ws);
        let built_after = manifest_json(&root).map(|m| m["built_at"].clone());
        println!(
            "B1 rebuild_index {label}: ok={} state={:?} released={released} cancelled={} \
             same_graph={} built_at before={built_before} after={built_after:?}",
            outcome.is_ok(),
            ws.load_state(),
            ws.rebuild_cancelled.load(Ordering::Acquire),
            Arc::ptr_eq(&graph_before, &ws.graph()),
        );
        let payload = mcp_payload(&label, &outcome);
        assert_eq!(
            payload["data"]["message"],
            json!("Index rebuilt successfully."),
            "{label}: {payload}"
        );
        assert_eq!(ws.load_state(), WorkspaceState::Loaded, "{label}");
        assert!(released, "{label}: the runner role must be released");
        assert!(
            !ws.rebuild_cancelled.load(Ordering::Acquire),
            "{label}: no cancellation may be left behind"
        );
        assert!(
            !Arc::ptr_eq(&graph_before, &ws.graph()),
            "{label}: the rebuilt generation must be published"
        );
        assert_ne!(
            built_after,
            Some(built_before.clone()),
            "{label}: the index must be rewritten"
        );
        drop(running);
        server.stop().await;
    }
}

/// The same on the IPC route, which always found the registered key
/// (`find_key_and_workspace_by_path`): the control for the MCP route.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_rebuild_rebuilds_a_workspace_registered_under_either_mode() {
    for mode in MODES {
        let label = format!("registered {}", mode.as_str());
        let (server, builder) = real_server(DaemonConfig::default()).await;
        let (_dir, root) = mixed_workspace();
        index_fast_path(&root);
        let key = WorkspaceKey::new(root.clone(), mode, 0);
        let graph_before = server
            .manager
            .get_or_load(&key, builder.as_ref(), 1)
            .expect("the workspace loads");
        let mut client = ipc_client(&server).await;
        let resp = ipc_rebuild(&mut client, &root, true, &[]).await;
        let ws = server.manager.lookup(&key).expect("resident");
        assert!(err_of(&resp).is_none(), "{label}: {:?}", err_of(&resp));
        assert!(runner_released(&ws), "{label}");
        assert_eq!(ws.load_state(), WorkspaceState::Loaded, "{label}");
        assert!(!Arc::ptr_eq(&graph_before, &ws.graph()), "{label}");
        drop(client);
        server.stop().await;
    }
}

/// B1 on the watcher route (E5): a workspace registered under one mode and
/// then loaded by a client sending the other keeps exactly one watcher,
/// under the registered key, reports `watching: true`, and an edit
/// afterwards is rebuilt and published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_after_a_load_under_another_mode_is_rebuilt_and_published() {
    for mode in MODES {
        let label = format!("registered {}", mode.as_str());
        let (server, builder) = real_server(DaemonConfig {
            debounce_ms: 200,
            ..DaemonConfig::default()
        })
        .await;
        let (_dir, root) = mixed_workspace();
        index_fast_path(&root);
        let key = WorkspaceKey::new(root.clone(), mode, 0);
        server
            .manager
            .get_or_load(&key, builder.as_ref(), 1)
            .expect("the workspace loads");
        server.dispatcher.start_watching(&key);

        let mut client = ipc_client(&server).await;
        expect_success(
            &client
                .request(
                    "daemon/load",
                    json!({
                        "index_root": root.to_string_lossy(),
                        "root_mode": other_mode(mode).as_str(),
                    }),
                )
                .await,
        );
        let live = server.dispatcher.live_watcher_keys();
        let row = status_row(&mut client, &root).await;
        assert_eq!(
            live.into_iter().collect::<Vec<_>>(),
            vec![key.clone()],
            "{label}: one watcher, under the registered key"
        );
        assert_eq!(row["watching"], json!(true), "{label}: {row}");

        let ws = server.manager.lookup(&key).expect("resident");
        let graph_before = ws.graph();
        std::fs::write(
            root.join("src").join("lib.rs"),
            b"pub fn func_after_the_load() -> u32 { 2 }\n",
        )
        .expect("edit");
        let ws_wait = Arc::clone(&ws);
        let published = tokio::task::spawn_blocking(move || {
            wait_until_blocking(Duration::from_secs(30), || {
                !Arc::ptr_eq(&graph_before, &ws_wait.graph())
                    && !ws_wait.rebuild_in_flight.load(Ordering::Acquire)
            })
        })
        .await
        .expect("join");
        println!(
            "B1 edit after a {} load of a {label} workspace: published={published} state={:?}",
            other_mode(mode).as_str(),
            ws.load_state()
        );
        assert!(published, "{label}: the edit must be rebuilt and published");
        assert_eq!(ws.load_state(), WorkspaceState::Loaded, "{label}");
        drop(client);
        server.stop().await;
    }
}

/// B1 on the serve route: a tool call while a workspace registered under
/// either mode rebuilds is served from the graph it holds, labelled
/// `Rebuilding`. The tool-call acquirer sends `GitRoot`; before the
/// repair `classify_for_serve` missed a `WorkspaceFolder` entry, the
/// acquirer took that for an eviction, the reload's load gate found a
/// rebuild in flight, and the call was refused `-32004`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tool_call_during_a_rebuild_is_served_under_either_mode() {
    for mode in MODES {
        let label = format!("registered {}", mode.as_str());
        let (server, builder) = real_server(DaemonConfig::default()).await;
        let gate = Arc::new(TestGate {
            hold: AtomicUsize::new(1),
            release: tokio::sync::Notify::new(),
        });
        server
            .dispatcher
            .install_test_gate(Arc::clone(&gate))
            .expect("gate installs");
        let capture = Arc::new(TestCapture::new());
        server
            .dispatcher
            .install_test_capture(Arc::clone(&capture))
            .expect("capture installs");
        let (_dir, root) = mixed_workspace();
        index_fast_path(&root);
        let key = WorkspaceKey::new(root.clone(), mode, 0);
        server
            .manager
            .get_or_load(&key, builder.as_ref(), 1)
            .expect("the workspace loads");

        let sock = server.path.clone();
        let rebuild_root = root.clone();
        let rebuild = tokio::spawn(async move {
            let mut client = support::ipc::TestIpcClient::connect(&sock).await;
            client.hello(1).await;
            ipc_rebuild(&mut client, &rebuild_root, true, &[]).await
        });
        let ws = server.manager.lookup(&key).expect("resident");
        let held = {
            let capture = Arc::clone(&capture);
            let ws = Arc::clone(&ws);
            tokio::task::spawn_blocking(move || {
                wait_until_blocking(Duration::from_secs(30), || {
                    !capture.iterations.lock().is_empty()
                        && ws.load_state() == WorkspaceState::Rebuilding
                })
            })
            .await
            .expect("join")
        };
        assert!(held, "{label}: the rebuild must be held at the gate");

        let mut client = ipc_client(&server).await;
        let resp = client
            .request(
                "semantic_search",
                json!({
                    "query": "kind:function",
                    "path": root.to_string_lossy(),
                    "max_results": 10,
                    "context_lines": 0,
                    "include_classpath": false,
                }),
            )
            .await;
        let answered = err_of(&resp).map(|e| (e.code, e.message.clone()));
        gate.release.notify_one();
        let rebuilt = rebuild.await.expect("join");
        println!(
            "B1 tool call during a {label} rebuild: error={answered:?} rebuild_error={:?}",
            err_of(&rebuilt).map(|e| e.code)
        );
        let result = expect_success(&resp);
        assert_eq!(
            result["meta"]["workspace_state"],
            json!("Rebuilding"),
            "{label}: {result}"
        );
        assert!(
            err_of(&rebuilt).is_none(),
            "{label}: the held rebuild completes"
        );
        drop(client);
        server.stop().await;
    }
}

/// The serve classification itself, under a caller key of the other mode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classify_for_serve_resolves_a_caller_key_of_either_mode() {
    for mode in MODES {
        let (server, builder) = real_server(DaemonConfig::default()).await;
        let (_dir, root) = mixed_workspace();
        index_fast_path(&root);
        let key = WorkspaceKey::new(root.clone(), mode, 0);
        server
            .manager
            .get_or_load(&key, builder.as_ref(), 1)
            .expect("the workspace loads");
        let caller = WorkspaceKey::new(root.clone(), other_mode(mode), 0);
        let verdict = server
            .manager
            .classify_for_serve(&caller, std::time::SystemTime::now());
        assert!(
            matches!(verdict, Ok(sqry_daemon::ServeVerdict::Fresh { .. })),
            "registered {}: a caller key of the other mode must be served fresh: {verdict:?}",
            mode.as_str()
        );
        server.stop().await;
    }
}

/// Audit S7 (plant K04): the load's publish recheck asks whether the
/// workspace it built is still the registered one (`registers`), not
/// whether the caller's key is in the map. A workspace registered under
/// one root mode and reset is loaded by a caller key of the other mode:
/// the load publishes into the registered entry. Asked by the caller's
/// key, the recheck refused it as "workspace removed mid-load".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reset_workspace_loads_under_the_other_mode() {
    for mode in MODES {
        let server = support::ipc::TestServer::new().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = sqry_core::project::canonicalize_path(dir.path()).expect("canonical");
        let registered = WorkspaceKey::new(root.clone(), mode, 0);
        let builder =
            sqry_daemon::workspace::builder::FunctionGraphBuilder::with_fast_path_record(3);
        server
            .manager
            .get_or_load(&registered, &builder, 1)
            .expect("loads");
        assert!(server.manager.reset(&registered, true).expect("resets"));
        let caller = WorkspaceKey::new(root.clone(), other_mode(mode), 0);
        let loaded = server.manager.get_or_load(&caller, &builder, 1);
        let entries = server.manager.find_all_by_source_root(&root).len();
        let ws = server.manager.lookup(&registered).expect("registered");
        let state = ws.load_state();
        println!(
            "K04 registered {}: loaded={:?} entries={entries} state={state:?}",
            mode.as_str(),
            loaded.as_ref().map(|_| ())
        );
        assert!(
            loaded.is_ok(),
            "registered {}: {:?}",
            mode.as_str(),
            loaded.err()
        );
        assert_eq!(entries, 1, "one registered entry");
        assert_eq!(state, WorkspaceState::Loaded);
        server.stop().await;
    }
}

/// Audit S7 (plant K05): the read-only reload's publish recheck asks
/// `registers` too. An evicted workspace registered under either mode is
/// reloaded for an IPC tool call (the acquirer sends the default root
/// mode) and served. Asked by the caller's key, the recheck refused the
/// `WorkspaceFolder` leg as "workspace removed mid-reload".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_evicted_workspace_reloads_for_a_tool_call_under_either_mode() {
    for mode in MODES {
        let (server, builder) = real_server(DaemonConfig::default()).await;
        let (_dir, root) = mixed_workspace();
        index_fast_path(&root);
        let key = WorkspaceKey::new(root.clone(), mode, 0);
        server
            .manager
            .get_or_load(&key, builder.as_ref(), 1)
            .expect("the workspace loads");
        assert!(server.manager.evict_for_test(&key), "evicts");
        let mut client = ipc_client(&server).await;
        let resp = client
            .request(
                "semantic_search",
                json!({
                    "query": "kind:function",
                    "path": root.to_string_lossy(),
                    "max_results": 10,
                    "context_lines": 0,
                    "include_classpath": false,
                }),
            )
            .await;
        let ws = server.manager.lookup(&key).expect("registered");
        println!(
            "K05 registered {}: error={:?} state={:?}",
            mode.as_str(),
            err_of(&resp).map(|e| (e.code, e.message.clone())),
            ws.load_state()
        );
        let _ = expect_success(&resp);
        assert_eq!(
            ws.load_state(),
            WorkspaceState::Loaded,
            "registered {}",
            mode.as_str()
        );
        drop(client);
        server.stop().await;
    }
}
