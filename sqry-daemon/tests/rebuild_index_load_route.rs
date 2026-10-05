//! The daemon-hosted `rebuild_index` load route answers only for what it
//! built, and every route that makes a workspace resident watches it
//! (integration round 7: S2, and DAEMON_FOLLOWUP's "watch a workspace
//! loaded through `rebuild_index`").
//!
//! S2: `rebuild_index force=true` with macro options that found no
//! resident graph took the load route; when another load published first,
//! the route's gate handed back that load's generation and the call
//! answered "Index rebuilt successfully." with nothing built, persisted or
//! recorded (the auditor's E10, 50 of 50 runs; made deterministic here with
//! the `SnapshotObserved` observation plant). The route now answers a found
//! generation as it answers a resident one.
//!
//! Watching: a workspace the load route made resident was not watched, and
//! one an LRU eviction stopped stayed unwatched after the read-only reload
//! that made it resident again.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::project::ProjectRootMode;
use sqry_daemon::workspace::builder::FunctionGraphBuilder;
use sqry_daemon::workspace::manager::ObservationPhase;
use sqry_daemon::{DaemonConfig, DaemonError, WorkspaceBuilder, WorkspaceKey, WorkspaceState};
use support::ipc::expect_success;
use support::rebuild_fixtures::{
    index_fast_path, ipc_client, manifest_json, mcp_call, mcp_error, mcp_payload,
    mcp_rebuild_index, mcp_session, mixed_workspace, real_server, status_row, wait_until_blocking,
};

/// A builder whose build waits for a signal, standing in for another load
/// in flight.
#[derive(Debug)]
struct Blocking {
    gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl WorkspaceBuilder for Blocking {
    fn build(&self, root: &Path) -> Result<sqry_daemon::workspace::BuiltGraph, DaemonError> {
        let _ = self.gate.lock().expect("gate").recv();
        FunctionGraphBuilder::with_fast_path_record(5).build(root)
    }
}

/// Another load of `key` holds the workspace `Loading`; once `rebuild_index`
/// has observed no resident graph, the plant releases it and waits until it
/// has published, so the call's load gate finds the workspace `Loaded`.
/// Returns the other load's thread and whether the plant fired.
fn race_another_load(
    server: &support::ipc::TestServer,
    key: &WorkspaceKey,
) -> (
    std::thread::JoinHandle<Result<Arc<CodeGraph>, DaemonError>>,
    Arc<AtomicBool>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let blocking = Arc::new(Blocking {
        gate: std::sync::Mutex::new(rx),
    });
    let manager = Arc::clone(&server.manager);
    let k = key.clone();
    let other = std::thread::spawn(move || manager.get_or_load(&k, &*blocking, 1));
    assert!(
        wait_until_blocking(Duration::from_secs(10), || {
            server
                .manager
                .lookup(key)
                .is_some_and(|ws| ws.load_state() == WorkspaceState::Loading)
        }),
        "the other load holds the workspace Loading"
    );
    let fired = Arc::new(AtomicBool::new(false));
    let tx = std::sync::Mutex::new(Some(tx));
    let manager = Arc::clone(&server.manager);
    let k = key.clone();
    let fired_in_plant = Arc::clone(&fired);
    server
        .manager
        .install_observation_plant_for_test(Arc::new(move |phase| {
            if phase != ObservationPhase::SnapshotObserved
                || fired_in_plant.swap(true, Ordering::AcqRel)
            {
                return;
            }
            if let Some(tx) = tx.lock().expect("tx").take() {
                let _ = tx.send(());
            }
            // The snapshot's read guard is shared with the other load's
            // publish, so it can complete while the plant waits.
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if manager
                    .lookup(&k)
                    .is_some_and(|ws| ws.load_state() == WorkspaceState::Loaded)
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }));
    (other, fired)
}

/// S2 (E10): `force=true` with `cfg_flags` racing another load is answered
/// with a generation built with its own options: the manifest records the
/// flag and was rewritten, and the resident generation is no longer the
/// other load's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_forced_rebuild_index_racing_a_load_builds_with_its_own_options() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    index_fast_path(&root);
    let before = manifest_json(&root).expect("manifest");
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::default(), 0);
    let (other, fired) = race_another_load(&server, &key);
    let running = mcp_session(&server).await;
    let answered = mcp_rebuild_index(
        running.peer(),
        &root,
        true,
        &[("cfg_flags", json!(["test"]))],
    )
    .await;
    let other = other
        .join()
        .expect("join")
        .expect("the other load publishes");
    let resident = server.manager.lookup(&key).expect("resident").graph();
    let after = manifest_json(&root).expect("manifest");
    let payload = mcp_payload("forced rebuild_index", &answered);
    println!(
        "S2 forced: plant fired={} resident is the other load's={} message={} nodeCount={} \
         record before={} after={} built_at changed={}",
        fired.load(Ordering::Acquire),
        Arc::ptr_eq(&resident, &other),
        payload["data"]["message"],
        payload["data"]["nodeCount"],
        before["macro_options"],
        after["macro_options"],
        before["built_at"] != after["built_at"]
    );
    assert!(fired.load(Ordering::Acquire), "the race was staged");
    assert_eq!(
        payload["data"]["message"],
        json!("Index rebuilt successfully.")
    );
    assert!(
        !Arc::ptr_eq(&resident, &other),
        "a generation was built after the other load's"
    );
    assert_eq!(
        after["macro_options"]["cfg_flags"],
        json!(["test"]),
        "the call's options were recorded"
    );
    assert_ne!(
        before["built_at"], after["built_at"],
        "the index was rebuilt"
    );
    drop(running);
    server.stop().await;
}

/// S2's `force=false` side: racing another load, macro options are refused
/// with the shared need-force refusal (a graph exists, so no build would
/// honour them) and nothing is written; without them the answer is the
/// existing graph.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rebuild_index_without_force_racing_a_load_builds_nothing() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::default(), 0);
    let running = mcp_session(&server).await;

    let (other, fired) = race_another_load(&server, &key);
    let refused = mcp_error(
        "options without force",
        mcp_rebuild_index(
            running.peer(),
            &root,
            false,
            &[("cfg_flags", json!(["test"]))],
        )
        .await,
    );
    other
        .join()
        .expect("join")
        .expect("the other load publishes");
    println!(
        "S2 force=false with options: plant fired={} code={} message={}",
        fired.load(Ordering::Acquire),
        refused.code.0,
        refused.message
    );
    assert!(fired.load(Ordering::Acquire), "the race was staged");
    assert_eq!((refused.code, refused.message.to_string()), {
        let shared = sqry_mcp::error::rpc_error_to_mcp(
            sqry_mcp::error::RpcError::macro_options_need_force(&root),
        );
        (shared.code, shared.message.to_string())
    });
    assert!(
        manifest_json(&root).is_none(),
        "nothing was written: {:?}",
        manifest_json(&root)
    );
    drop(running);
    server.stop().await;
}

/// A workspace `rebuild_index` made resident is watched, as one
/// `daemon/load` made resident is. Before the repair it was not.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workspace_rebuild_index_loads_is_watched() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    let running = mcp_session(&server).await;
    mcp_payload(
        "first build",
        &mcp_rebuild_index(running.peer(), &root, true, &[]).await,
    );
    let mut client = ipc_client(&server).await;
    let row = status_row(&mut client, &root).await;
    println!(
        "rebuild_index load route: state={} watching={}",
        row["state"], row["watching"]
    );
    assert_eq!(row["state"], json!("Loaded"));
    assert_eq!(row["watching"], json!(true), "{row}");
    drop(running);
    drop(client);
    server.stop().await;
}

/// A watched workspace an LRU eviction stopped is watched again once a
/// query reloads it; before the repair it stayed unwatched. The control
/// from the other side: a root nobody loaded, made resident only by a
/// query, is not watched, before or after an eviction and a reload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reloaded_workspace_is_watched_again_if_a_load_watched_it() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_loaded_dir, loaded) = mixed_workspace();
    let (_queried_dir, queried) = mixed_workspace();
    index_fast_path(&loaded);
    index_fast_path(&queried);
    let mut client = ipc_client(&server).await;
    let running = mcp_session(&server).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": loaded.to_string_lossy() }),
            )
            .await,
    );
    let query = |root: &Path| json!({ "path": root.to_string_lossy(), "query": "func_alpha" });
    mcp_payload(
        "first query",
        &mcp_call(running.peer(), "semantic_search", query(&queried)).await,
    );
    let mut seen: Vec<(String, Value)> = Vec::new();
    for root in [&loaded, &queried] {
        let label = if root == &loaded { "loaded" } else { "queried" };
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        let resident = status_row(&mut client, root).await;
        assert!(server.manager.evict_for_test(&key), "{label}: evicted");
        let evicted = status_row(&mut client, root).await;
        assert_eq!(evicted["watching"], json!(false), "{label}: {evicted}");
        mcp_payload(
            "query after the eviction",
            &mcp_call(running.peer(), "semantic_search", query(root)).await,
        );
        let reloaded = status_row(&mut client, root).await;
        println!(
            "{label}: resident watching={} -> evicted -> query: state={} watching={}",
            resident["watching"], reloaded["state"], reloaded["watching"]
        );
        assert_eq!(reloaded["state"], json!("Loaded"), "{label}: reloaded");
        seen.push((format!("{label} before"), resident["watching"].clone()));
        seen.push((format!("{label} after"), reloaded["watching"].clone()));
    }
    assert_eq!(
        seen,
        vec![
            ("loaded before".to_string(), json!(true)),
            ("loaded after".to_string(), json!(true)),
            ("queried before".to_string(), json!(false)),
            ("queried after".to_string(), json!(false)),
        ]
    );
    drop(running);
    drop(client);
    server.stop().await;
}

/// Provenance (DAEMON_FOLLOWUP): the in-place `rebuild_index` records
/// `daemon:rebuild_index` as the index's `build_command`, as its load route
/// does; before the repair it recorded `daemon:rebuild:full`. The control:
/// `daemon/rebuild` of the same workspace records `daemon:rebuild:full`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_rebuild_route_records_its_own_provenance() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    let running = mcp_session(&server).await;
    let mut client = ipc_client(&server).await;
    let mut seen = Vec::new();
    mcp_payload(
        "load route",
        &mcp_rebuild_index(running.peer(), &root, true, &[]).await,
    );
    seen.push((
        "load route",
        manifest_json(&root).expect("manifest")["build_provenance"]["build_command"].clone(),
    ));
    mcp_payload(
        "in place",
        &mcp_rebuild_index(running.peer(), &root, true, &[]).await,
    );
    seen.push((
        "in place",
        manifest_json(&root).expect("manifest")["build_provenance"]["build_command"].clone(),
    ));
    expect_success(
        &client
            .request(
                "daemon/rebuild",
                json!({ "path": root.to_string_lossy(), "force": true }),
            )
            .await,
    );
    seen.push((
        "daemon/rebuild",
        manifest_json(&root).expect("manifest")["build_provenance"]["build_command"].clone(),
    ));
    println!("provenance: {seen:?}");
    assert_eq!(
        seen,
        vec![
            ("load route", json!("daemon:rebuild_index")),
            ("in place", json!("daemon:rebuild_index")),
            ("daemon/rebuild", json!("daemon:rebuild:full")),
        ]
    );
    drop(running);
    drop(client);
    server.stop().await;
}

/// Round 7, plant P21: a slot that is not serving (`Loading`: a load in
/// flight over the generation a failed workspace still held) is not taken
/// for a resident graph. `rebuild_index force=false` is refused as a load
/// already in progress rather than answered "Index already exists" from a
/// generation the load is about to replace; once the slot is `Loaded`
/// again the same call is answered from it (the control).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slot_that_is_loading_is_not_taken_for_a_resident_graph() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let (_dir, root) = mixed_workspace();
    let mut client = ipc_client(&server).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": root.to_string_lossy() }),
            )
            .await,
    );
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    let ws = server.manager.lookup(&key).expect("resident");
    assert!(
        ws.roster().is_some(),
        "precondition: a generation with its record"
    );
    ws.store_state(WorkspaceState::Loading);
    let running = mcp_session(&server).await;
    let refused = mcp_error(
        "loading slot",
        mcp_rebuild_index(running.peer(), &root, false, &[]).await,
    );
    println!("loading slot: {} {}", refused.code.0, refused.message);
    assert!(
        refused.message.contains("already in progress"),
        "{}",
        refused.message
    );
    ws.store_state(WorkspaceState::Loaded);
    let answered = mcp_payload(
        "loaded slot",
        &mcp_rebuild_index(running.peer(), &root, false, &[]).await,
    );
    assert_eq!(
        answered["data"]["message"],
        json!("Index already exists. Use force=true to rebuild.")
    );
    drop(running);
    drop(client);
    server.stop().await;
}

/// DAEMON_FOLLOWUP (round 7): a manifest whose snapshot is gone is not an
/// index. `rebuild_index force=false` over it builds the index (keeping the
/// recorded selection) and answers "Index built successfully.", where it
/// answered the cache hit "Index already exists" with nothing loadable on
/// disk. The control: with the snapshot present it is the cache hit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_manifest_without_its_snapshot_is_built_not_reported() {
    let (server, _builder) = real_server(DaemonConfig::default()).await;
    let running = mcp_session(&server).await;
    let mut seen = Vec::new();
    for snapshot_present in [true, false] {
        let (_dir, root) = mixed_workspace();
        index_fast_path(&root);
        let storage = sqry_core::graph::unified::persistence::GraphStorage::new(&root);
        if !snapshot_present {
            std::fs::remove_file(storage.snapshot_path()).expect("remove snapshot");
        }
        let payload = mcp_payload(
            "rebuild_index force=false",
            &mcp_rebuild_index(running.peer(), &root, false, &[]).await,
        );
        println!(
            "snapshot present={snapshot_present}: {} snapshot after={}",
            payload["data"]["message"],
            storage.snapshot_exists()
        );
        seen.push((
            snapshot_present,
            payload["data"]["message"].clone(),
            storage.snapshot_exists(),
        ));
    }
    assert_eq!(
        seen,
        vec![
            (
                true,
                json!("Index already exists. Use force=true to rebuild."),
                true
            ),
            (false, json!("Index built successfully."), true),
        ]
    );
    drop(running);
    server.stop().await;
}
