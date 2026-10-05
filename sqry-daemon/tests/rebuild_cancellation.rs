//! Cancellations never leave a workspace stuck (the rebuild dispatcher).
//!
//! A `daemon/cancel_rebuild` (here `RebuildDispatcher::cancel_rebuild`, the
//! call the handler makes) sets `rebuild_cancelled` while a rebuild is in
//! flight. Where it lands decides which exit the iteration takes:
//!
//! - mid-pipeline: a pass boundary observes it and the pipeline returns
//!   `Cancelled`. Before the repair the iteration left the workspace
//!   `Unloaded` with the flag still set, and every later load failed
//!   "workspace evicted mid-load";
//! - during the durable persist (no pass boundary left): only the publish
//!   recheck observes it. Before the repair the iteration returned without a
//!   transition, leaving the workspace `Rebuilding` with no runner, and
//!   `daemon/reset` answered `ResetCancellationDispatched` on every retry;
//! - after the iteration published, before the runner released its role:
//!   before the repair the flag outlived the runner and aborted the next
//!   rebuild.
//!
//! Each test then checks that a later `daemon/load` (`get_or_load`) and
//! `daemon/reset` (`WorkspaceManager::reset`) behave. An eviction during the
//! pipeline is the control: it owns the state, so the workspace is left
//! `Evicted` and the flag is left for the next load to consume.

#![cfg(feature = "test-hooks")]

mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use sqry_core::graph::unified::build::BuildConfig;
use sqry_core::watch::{ChangeSet, GitChangeClass};
use sqry_daemon::{DaemonError, LoadedWorkspace, TestCapture, WorkspaceState};

use support::{DispatchHarness, RealGraphBuilder};

fn full_changes() -> ChangeSet {
    ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: true,
        git_change_class: Some(GitChangeClass::TreeDiverged),
    }
}

fn real_builder() -> RealGraphBuilder {
    RealGraphBuilder {
        plugins: Arc::new(sqry_plugin_registry::create_plugin_manager()),
        cfg: BuildConfig::default(),
    }
}

/// A harness with a source file and an installed capture.
fn harness() -> (DispatchHarness, Arc<TestCapture>) {
    let h = DispatchHarness::new();
    std::fs::write(h.root.join("lib.rs"), b"pub fn a() {}\n").expect("source");
    let cap = Arc::new(TestCapture::default());
    h.dispatcher
        .install_test_capture(Arc::clone(&cap))
        .expect("capture installs once");
    (h, cap)
}

/// A later `daemon/reset` and `daemon/load` both succeed on the first try.
fn assert_reset_and_load_behave(h: &DispatchHarness, label: &str) {
    let ws = h.manager.lookup(&h.key).expect("resident");
    let reset = h.manager.reset(&h.key, false);
    println!("{label}: reset={reset:?} state={}", ws.load_state());
    assert!(
        matches!(reset, Ok(true)),
        "{label}: daemon/reset must reset on the first try: {reset:?}"
    );
    let loaded = h.manager.get_or_load(&h.key, &real_builder(), 1);
    assert!(
        loaded.is_ok(),
        "{label}: daemon/load must bring the workspace back: {loaded:?}"
    );
    assert_eq!(ws.load_state(), WorkspaceState::Loaded, "{label}");
    assert!(!ws.rebuild_cancelled.load(Ordering::Acquire), "{label}");
}

/// The auditor's experiment: a cancel that lands mid-pipeline. The
/// iteration ends `Unloaded`, the runner consumes the flag, and three
/// `daemon/load` attempts succeed from the first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_mid_pipeline_does_not_leave_the_flag_set() {
    let (h, cap) = harness();
    cap.arm_post_reservation_hold();
    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_reservation())
        .await
        .expect("the rebuild reaches its reservation");
    let ws = h.manager.lookup(&h.key).expect("resident");
    assert!(
        h.dispatcher.cancel_rebuild(&ws).await,
        "a rebuild is in flight, so the cancel is dispatched"
    );
    // Make the first pass boundary observe it, deterministically.
    cap.precancel_token_for_pass_boundary
        .store(true, Ordering::Release);
    cap.release_post_reservation();
    let result = run.await.expect("runner task");
    cap.precancel_token_for_pass_boundary
        .store(false, Ordering::Release);
    println!(
        "mid-pipeline cancel: result={result:?} state={} flag={} in_flight={}",
        ws.load_state(),
        ws.rebuild_cancelled.load(Ordering::Acquire),
        ws.rebuild_in_flight.load(Ordering::Acquire)
    );
    assert!(
        matches!(result, Err(DaemonError::WorkspaceEvicted { .. })),
        "{result:?}"
    );
    assert_eq!(cap.pass_boundary_cancellations(), 1);
    assert_eq!(ws.load_state(), WorkspaceState::Unloaded);
    assert!(
        !ws.rebuild_cancelled.load(Ordering::Acquire),
        "a cancellation must not outlive the runner it was aimed at"
    );
    assert!(!ws.rebuild_in_flight.load(Ordering::Acquire));

    let mut loads = Vec::new();
    for _ in 0..3 {
        let load = h.manager.get_or_load(&h.key, &real_builder(), 1);
        loads.push((load.is_ok(), ws.load_state()));
    }
    println!("three daemon/load attempts: {loads:?}");
    assert!(loads[0].0, "the first daemon/load succeeds: {loads:?}");
    assert!(loads.iter().all(|(ok, _)| *ok), "{loads:?}");
    assert_reset_and_load_behave(&h, "mid-pipeline cancel");
}

/// The auditor's experiment: a cancel that lands during the durable
/// persist (no forwarder, so only the publish recheck observes it). The
/// iteration must leave `Rebuilding`, and `daemon/reset` must reset on the
/// first try.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_during_the_persist_does_not_leave_the_workspace_rebuilding() {
    let (h, cap) = harness();
    cap.suppress_forwarder.store(true, Ordering::Release);
    cap.arm_post_reservation_hold();
    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_reservation())
        .await
        .expect("the rebuild reaches its reservation");
    let ws = h.manager.lookup(&h.key).expect("resident");
    assert!(h.dispatcher.cancel_rebuild(&ws).await);
    cap.release_post_reservation();
    let result = run.await.expect("runner task");
    println!(
        "persist-time cancel: result={result:?} state={} flag={} in_flight={} recheck={}",
        ws.load_state(),
        ws.rebuild_cancelled.load(Ordering::Acquire),
        ws.rebuild_in_flight.load(Ordering::Acquire),
        cap.publish_path_evictions()
    );
    assert!(
        matches!(result, Err(DaemonError::WorkspaceEvicted { .. })),
        "{result:?}"
    );
    assert_eq!(
        cap.publish_path_evictions(),
        1,
        "the publish recheck observed it"
    );
    assert_ne!(
        ws.load_state(),
        WorkspaceState::Rebuilding,
        "no rebuild is running, so the workspace must not stay Rebuilding"
    );
    assert_eq!(ws.load_state(), WorkspaceState::Unloaded);
    assert!(!ws.rebuild_cancelled.load(Ordering::Acquire));
    assert!(!ws.rebuild_in_flight.load(Ordering::Acquire));
    assert_reset_and_load_behave(&h, "persist-time cancel");
}

/// A cancel that lands after the iteration published but before the runner
/// released its role cancels nothing that is still running; the runner
/// consumes it, so the next rebuild is not aborted by it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_after_the_publish_is_consumed_by_its_runner() {
    let (h, cap) = harness();
    cap.arm_post_publish_hold();
    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_publish())
        .await
        .expect("the rebuild publishes");
    let ws = h.manager.lookup(&h.key).expect("resident");
    assert!(h.dispatcher.cancel_rebuild(&ws).await, "still in flight");
    cap.release_post_publish();
    let result = run.await.expect("runner task");
    println!(
        "late cancel: result={result:?} state={} flag={}",
        ws.load_state(),
        ws.rebuild_cancelled.load(Ordering::Acquire)
    );
    assert_eq!(
        ws.load_state(),
        WorkspaceState::Loaded,
        "the publish stands"
    );
    assert!(
        !ws.rebuild_cancelled.load(Ordering::Acquire),
        "the runner consumed the cancellation"
    );
    assert!(!ws.rebuild_in_flight.load(Ordering::Acquire));
    let next = h.dispatcher.handle_changes(&h.key, full_changes()).await;
    assert!(
        next.is_ok(),
        "the next rebuild is not aborted by a stale cancellation: {next:?}"
    );
}

/// The control: an eviction during the pipeline owns the state. The
/// workspace stays `Evicted` and the flag is left for the next load, which
/// consumes it and loads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_eviction_mid_pipeline_is_left_to_the_next_load_control() {
    let (h, cap) = harness();
    cap.arm_post_reservation_hold();
    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_reservation())
        .await
        .expect("the rebuild reaches its reservation");
    let ws = h.manager.lookup(&h.key).expect("resident");
    assert!(h.manager.evict_for_test(&h.key), "evicted");
    cap.release_post_reservation();
    let result = run.await.expect("runner task");
    println!(
        "eviction: result={result:?} state={} flag={}",
        ws.load_state(),
        ws.rebuild_cancelled.load(Ordering::Acquire)
    );
    assert!(
        matches!(result, Err(DaemonError::WorkspaceEvicted { .. })),
        "{result:?}"
    );
    assert_eq!(ws.load_state(), WorkspaceState::Evicted);
    assert!(
        ws.rebuild_cancelled.load(Ordering::Acquire),
        "a completed eviction's flag is left for the next load"
    );
    assert!(!ws.rebuild_in_flight.load(Ordering::Acquire));
    h.manager
        .get_or_load(&h.key, &real_builder(), 1)
        .expect("the next load consumes the flag and loads");
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
    assert!(!ws.rebuild_cancelled.load(Ordering::Acquire));
}

/// `daemon/cancel_rebuild` over IPC no longer stops the file watcher. The
/// watcher used to poll `rebuild_cancelled` and exit on it, so cancelling a
/// rebuild left a loaded workspace unwatched; its dispatcher task also exited
/// on the cancelled rebuild's `WorkspaceEvicted`. After the cancel the
/// workspace is `Unloaded`, `daemon/load` brings it back, it is still
/// watched, and an edit still triggers a rebuild.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_cancel_rebuild_keeps_the_workspace_watched() {
    use serde_json::json;
    use support::ipc::{TestIpcClient, TestServer, expect_success};

    let builder: Arc<dyn sqry_daemon::WorkspaceBuilder> =
        Arc::new(sqry_daemon::RealWorkspaceBuilder::new(Arc::new(
            sqry_daemon::WorkspaceRosterResolver::new(),
        )));
    let server = TestServer::with_builder_and_config(
        builder,
        sqry_daemon::DaemonConfig {
            debounce_ms: 100,
            ..sqry_daemon::DaemonConfig::default()
        },
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    support::init_git_repo(&root);
    let path = root.to_string_lossy().to_string();
    let mut client = TestIpcClient::connect(&server.path).await;
    client.hello(1).await;
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    let watching = |server: &TestServer| {
        server
            .dispatcher
            .live_watcher_keys()
            .iter()
            .any(|key| key.source_root == root)
    };
    assert!(
        watching(&server),
        "precondition: daemon/load starts a watcher"
    );

    let cap = Arc::new(TestCapture::default());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&cap))
        .unwrap();
    cap.arm_post_reservation_hold();
    let socket = server.path.clone();
    let rebuild_path = path.clone();
    let rebuild = tokio::spawn(async move {
        let mut c = TestIpcClient::connect(&socket).await;
        c.hello(1).await;
        c.request(
            "daemon/rebuild",
            json!({ "path": rebuild_path, "force": true }),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_reservation())
        .await
        .expect("the rebuild reaches its reservation");
    let cancel = client
        .request("daemon/cancel_rebuild", json!({ "path": &path }))
        .await;
    assert_eq!(
        expect_success(&cancel)["result"]["cancelled"],
        json!(true),
        "a rebuild was in flight"
    );
    // Longer than the watcher's stop poll (100 ms), so a watcher that
    // polled the rebuild flag would have exited by now.
    tokio::time::sleep(Duration::from_millis(400)).await;
    cap.precancel_token_for_pass_boundary
        .store(true, Ordering::Release);
    cap.release_post_reservation();
    let answered = rebuild.await.expect("rebuild task");
    cap.precancel_token_for_pass_boundary
        .store(false, Ordering::Release);
    let err = support::ipc::expect_error(&answered);
    assert_eq!(err.code, -32004, "the cancelled rebuild: {err:?}");
    tokio::time::sleep(Duration::from_millis(400)).await;

    let status = expect_success(&client.request("daemon/status", json!({})).await).clone();
    println!("status after cancel: {}", status["result"]["workspaces"]);
    let row = status["result"]["workspaces"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["index_root"] == json!(path))
                .cloned()
        })
        .expect("the workspace row");
    assert_eq!(row["watching"], json!(true), "row: {row}");
    assert_eq!(row["state"], json!("Unloaded"), "row: {row}");

    // daemon/load brings it back and it is still watched.
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    assert!(watching(&server), "still watched after the reload");

    // An edit still triggers a rebuild.
    let before = server.dispatcher.dispatched_count();
    std::fs::write(root.join("edited.rs"), b"pub fn edited() {}\n").expect("edit");
    let rebuilt = support::wait_until(
        || server.dispatcher.dispatched_count() > before,
        Duration::from_secs(20),
    )
    .await;
    assert!(
        rebuilt,
        "an edit after the cancel triggers a watcher rebuild"
    );

    // A cancelled watcher-driven rebuild: the watcher's own dispatch returns
    // `WorkspaceEvicted`, and its dispatcher task must keep running.
    cap.reset_post_reservation_reached();
    cap.arm_post_reservation_hold();
    std::fs::write(root.join("edited_again.rs"), b"pub fn again() {}\n").expect("edit");
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_reservation())
        .await
        .expect("the watcher's rebuild reaches its reservation");
    let cancel = client
        .request("daemon/cancel_rebuild", json!({ "path": &path }))
        .await;
    assert_eq!(expect_success(&cancel)["result"]["cancelled"], json!(true));
    cap.precancel_token_for_pass_boundary
        .store(true, Ordering::Release);
    cap.release_post_reservation();
    let settled = support::wait_until(
        || {
            server
                .manager
                .find_key_and_workspace_by_path(&root)
                .is_some_and(|(_, ws)| !ws.rebuild_in_flight.load(Ordering::Acquire))
        },
        Duration::from_secs(20),
    )
    .await;
    cap.precancel_token_for_pass_boundary
        .store(false, Ordering::Release);
    assert!(settled, "the cancelled watcher rebuild ends");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        watching(&server),
        "a cancelled watcher-driven rebuild leaves the watcher running"
    );
    expect_success(
        &client
            .request("daemon/load", json!({ "index_root": &path }))
            .await,
    );
    let before = server.dispatcher.dispatched_count();
    std::fs::write(root.join("edited_third.rs"), b"pub fn third() {}\n").expect("edit");
    let rebuilt = support::wait_until(
        || server.dispatcher.dispatched_count() > before,
        Duration::from_secs(20),
    )
    .await;
    assert!(
        rebuilt,
        "the watcher's dispatcher task survived its own cancelled rebuild"
    );

    drop(client);
    server.stop().await;
}

/// `cancel_rebuild` reads `rebuild_in_flight` under the rebuild lane, where
/// the runner releases its role. A cancel that races the release finds the
/// role released and sets nothing; before the repair it read the role taken,
/// the runner released it, and the cancel then set a flag no runner would
/// consume, which aborted the next rebuild and failed the next load.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_racing_the_runner_release_sets_nothing() {
    let (h, _cap) = harness();
    let ws = h.manager.lookup(&h.key).expect("resident");
    // A runner holds the role and is releasing it under the lane.
    ws.rebuild_in_flight.store(true, Ordering::Release);
    let lane = ws.rebuild_lane.lock().await;
    let d = Arc::clone(&h.dispatcher);
    let ws_for_cancel = Arc::clone(&ws);
    let cancel = tokio::spawn(async move { d.cancel_rebuild(&ws_for_cancel).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !cancel.is_finished(),
        "the cancel must wait for the lane the runner holds"
    );
    ws.rebuild_in_flight.store(false, Ordering::Release);
    drop(lane);
    let cancelled = cancel.await.expect("cancel task");
    assert!(!cancelled, "the cancel found the role released");
    assert!(
        !ws.rebuild_cancelled.load(Ordering::Acquire),
        "a cancel that races the release sets nothing"
    );
    // Control: with the role held and the lane free, the cancel is set.
    ws.rebuild_in_flight.store(true, Ordering::Release);
    assert!(h.dispatcher.cancel_rebuild(&ws).await);
    assert!(ws.rebuild_cancelled.load(Ordering::Acquire));
    ws.rebuild_cancelled.store(false, Ordering::Release);
    ws.rebuild_in_flight.store(false, Ordering::Release);
}

/// Cancel a rebuild mid-pipeline, as `a_cancel_mid_pipeline_does_not_leave_the_flag_set`
/// does, and return the workspace the cancelled iteration left `Unloaded`.
async fn cancel_mid_pipeline(h: &DispatchHarness, cap: &TestCapture) -> Arc<LoadedWorkspace> {
    let ws = h.manager.lookup(&h.key).expect("resident");
    cap.arm_post_reservation_hold();
    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_reservation())
        .await
        .expect("the rebuild reaches its reservation");
    assert!(
        h.dispatcher.cancel_rebuild(&ws).await,
        "a rebuild is in flight"
    );
    cap.precancel_token_for_pass_boundary
        .store(true, Ordering::Release);
    cap.release_post_reservation();
    let result = run.await.expect("runner task");
    cap.precancel_token_for_pass_boundary
        .store(false, Ordering::Release);
    assert!(
        matches!(result, Err(DaemonError::WorkspaceEvicted { .. })),
        "{result:?}"
    );
    assert_eq!(
        ws.load_state(),
        WorkspaceState::Unloaded,
        "the documented end state"
    );
    ws
}

/// Round 8 review, note (d), decision D-i8-44: a `daemon/cancel_rebuild`
/// that lands mid-pipeline leaves the workspace `Unloaded` still holding
/// its graph, which its watcher's next rebuild reads (the control below).
/// Before the repair both eviction paths (`evict_lru` and the reservation's
/// eviction plan) skipped every `Unloaded` workspace as having no bytes, so
/// those bytes stayed counted with nothing able to reclaim them until a
/// later load. Measured here: the bytes the slot counts, whether LRU
/// eviction reclaims them, and the bytes left after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_rebuild_leaves_no_graph_counted_in_an_unloaded_workspace() {
    let (h, cap) = harness();
    let bytes_before = h
        .manager
        .lookup(&h.key)
        .expect("resident")
        .memory_bytes
        .load(Ordering::Acquire);
    let ws = cancel_mid_pipeline(&h, &cap).await;
    let bytes_unloaded = ws.memory_bytes.load(Ordering::Acquire);
    let generation_kept = ws.published().roster.is_some();
    let live_unloaded = h.manager.status().memory.live_workspace_bytes;

    let reclaimed = h.manager.evict_lru();
    let bytes_after = ws.memory_bytes.load(Ordering::Acquire);
    // Eviction moves the bytes to the retained tier; the reaper frees them
    // once no reader holds the old graph (none does here).
    h.manager.reap_once();
    let memory = h.manager.status().memory;
    println!(
        "R8 note (d): bytes_before={bytes_before} bytes_unloaded={bytes_unloaded} \
         generation_kept={generation_kept} live_unloaded={live_unloaded} \
         evict_lru={reclaimed:?} state={} bytes_after={bytes_after} \
         live_workspace_bytes={}",
        ws.load_state(),
        memory.live_workspace_bytes
    );
    assert!(
        bytes_before > 0,
        "instrument: the loaded graph counts bytes"
    );
    assert_eq!(
        bytes_unloaded, bytes_before,
        "instrument: the cancelled slot still holds and counts its graph"
    );
    assert!(generation_kept, "instrument: the slot keeps its generation");
    assert_eq!(
        live_unloaded, bytes_before as u64,
        "instrument: counted as live"
    );
    assert_eq!(
        reclaimed.as_ref(),
        Some(&ws.key),
        "LRU eviction reclaims an Unloaded workspace that holds a graph"
    );
    assert_eq!(ws.load_state(), WorkspaceState::Evicted);
    assert_eq!(bytes_after, 0, "the evicted slot counts no graph bytes");
    assert_eq!(
        memory.live_workspace_bytes, 0,
        "no live workspace bytes once the reaper ran"
    );
    assert!(
        h.manager.evict_lru().is_none(),
        "the tombstone is not a candidate twice"
    );

    // The workspace still comes back on a load.
    let loaded = h.manager.get_or_load(&h.key, &real_builder(), 1);
    assert!(loaded.is_ok(), "{loaded:?}");
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
}

/// D-i8-44's control: the reader that keeps the graph in a cancelled
/// slot. The watcher keeps watching after a cancel, and the next rebuild it
/// (or `daemon/rebuild`) dispatches enters from that `Unloaded` slot and
/// publishes, because the slot still holds its generation; over the
/// placeholder (the shape `daemon/reset` leaves) the same request is
/// refused `WorkspaceNotLoaded`. Releasing the graph at the cancel would
/// turn every such rebuild into that refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rebuild_after_a_cancel_republishes_from_the_kept_graph() {
    let (h, cap) = harness();
    let ws = cancel_mid_pipeline(&h, &cap).await;
    let next = h.dispatcher.handle_changes(&h.key, full_changes()).await;
    println!(
        "R8 note (d) control: next={next:?} state={}",
        ws.load_state()
    );
    assert!(next.is_ok(), "{next:?}");
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
    assert!(ws.memory_bytes.load(Ordering::Acquire) > 0);
}

/// Decision D-i8-5: a cancel that lands while the durable persist builds
/// again under the persist lock (another writer recorded different inputs
/// during the first build) ends that build at its next pass boundary. It
/// publishes nothing, releases the lock, and leaves the workspace as a
/// mid-pipeline cancel does: `Unloaded`, its graph's bytes still counted
/// (the shape decision D-i8-44 makes evictable on the merged tree), the
/// flag consumed. Before the fix that build ran on a fresh token, ignored
/// the cancel, and published its manifest while holding the lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_during_the_rebuild_under_the_persist_lock_publishes_nothing() {
    use sqry_core::graph::unified::build::MacroOptionsRequest;
    use sqry_core::graph::unified::persistence::{GraphStorage, IndexWriteLock};
    use sqry_plugin_registry::{
        PluginSelectionConfig, UnreadableManifestPolicy, build_and_persist_with_workspace_roster,
    };

    let (h, cap) = harness();
    // Only the persist's own switch cancels the token, so the first build
    // completes and the rebuild under the lock is the stage that sees it.
    cap.suppress_forwarder.store(true, Ordering::Release);
    cap.cancel_token_at_rebuild_under_lock
        .store(true, Ordering::Release);
    cap.arm_post_reservation_hold();
    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_reservation())
        .await
        .expect("the rebuild reaches its reservation");
    let ws = h.manager.lookup(&h.key).expect("resident");
    let bytes_before = ws.memory_bytes.load(Ordering::Acquire);
    assert!(
        bytes_before > 0,
        "precondition: the loaded graph is counted"
    );

    // Another writer (a `sqry index --cfg test`) records new inputs after
    // the iteration resolved its own, so the persist must build again.
    let other_root = h.root.clone();
    tokio::task::spawn_blocking(move || {
        build_and_persist_with_workspace_roster(
            &other_root,
            &PluginSelectionConfig::default(),
            UnreadableManifestPolicy::Refuse,
            "t:other-writer",
            &BuildConfig::default(),
            &MacroOptionsRequest {
                cfg_flags: Some(vec!["test".to_string()]),
                ..MacroOptionsRequest::empty()
            },
            sqry_core::progress::no_op_reporter(),
        )
        .expect("the other writer publishes");
    })
    .await
    .expect("other writer task");
    let storage = GraphStorage::new(&h.root);
    let published_before = std::fs::read(storage.manifest_path()).expect("manifest");

    assert!(
        h.dispatcher.cancel_rebuild(&ws).await,
        "a rebuild is in flight"
    );
    cap.release_post_reservation();
    let result = run.await.expect("runner task");
    cap.cancel_token_at_rebuild_under_lock
        .store(false, Ordering::Release);
    println!(
        "cancel under the lock: result={result:?} state={} flag={} bytes={} pass_boundary={} recheck={}",
        ws.load_state(),
        ws.rebuild_cancelled.load(Ordering::Acquire),
        ws.memory_bytes.load(Ordering::Acquire),
        cap.pass_boundary_cancellations(),
        cap.publish_path_evictions()
    );
    assert_eq!(
        storage
            .load_manifest()
            .expect("manifest")
            .build_provenance
            .build_command,
        "t:other-writer",
        "the cancelled rebuild under the lock published its manifest"
    );
    assert!(
        std::fs::read(storage.manifest_path()).expect("manifest") == published_before,
        "the manifest changed"
    );
    assert!(
        matches!(result, Err(DaemonError::WorkspaceEvicted { .. })),
        "{result:?}"
    );
    assert_eq!(
        cap.pass_boundary_cancellations(),
        1,
        "the rebuild's pass boundary observed the cancel"
    );
    assert_eq!(cap.publish_path_evictions(), 0);
    assert_eq!(ws.load_state(), WorkspaceState::Unloaded);
    assert_eq!(
        ws.memory_bytes.load(Ordering::Acquire),
        bytes_before,
        "the cancelled iteration keeps its graph's bytes, as a mid-pipeline cancel does"
    );
    assert!(!ws.rebuild_cancelled.load(Ordering::Acquire));
    assert!(!ws.rebuild_in_flight.load(Ordering::Acquire));
    let graph_dir = storage.graph_dir().to_path_buf();
    let free = std::thread::spawn(move || {
        IndexWriteLock::try_acquire(&graph_dir)
            .expect("try the lock")
            .is_some()
    })
    .join()
    .expect("lock probe");
    assert!(free, "the cancelled persist released the persist lock");
    assert_reset_and_load_behave(&h, "cancel under the persist lock");
}

/// Decision D-i8-6: a rebuild whose durable persist waits for another
/// writer's persist lock (a `sqry index` holds it for its whole build,
/// D-i8-4) is ended by an eviction that lands during the wait, without
/// that writer releasing the lock: the wait observes the iteration's
/// cancellation, nothing is published, and a load of the evicted workspace
/// is answered while the other writer still holds the lock. Before the fix
/// the wait was a blocking `flock` the cancellation could not reach, so the
/// runner kept its role for the length of the other writer's build and
/// D-i8-43 refused every load with `EVICTED_REBUILD_STILL_RUNNING`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_eviction_ends_a_persist_waiting_on_another_writers_lock() {
    use sqry_core::graph::unified::persistence::{GraphStorage, IndexWriteLock};
    use std::sync::mpsc;

    // An indexed root, so the persist waits through the existing-index
    // wait (a directory with no manifest, snapshot or rollback set takes
    // the creating wait instead).
    let (h, cap) = indexed_harness().await;
    let graph_dir = GraphStorage::new(&h.root).graph_dir().to_path_buf();
    let published_before =
        std::fs::read(GraphStorage::new(&h.root).manifest_path()).expect("manifest");
    // Another writer holds the persist lock (on a thread of its own: the
    // guard belongs to the thread that took it).
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let other_writer = {
        let graph_dir = graph_dir.clone();
        std::thread::spawn(move || {
            let _held = IndexWriteLock::acquire(&graph_dir).expect("the other writer's lock");
            held_tx.send(()).expect("report the hold");
            release_rx.recv().expect("wait for the release");
        })
    };
    held_rx.recv().expect("the other writer holds the lock");

    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !cap.persist_lock_contended.load(Ordering::Acquire) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the rebuild's persist never reached the held lock"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let ws = h.manager.lookup(&h.key).expect("resident");
    assert_eq!(h.manager.evict_lru(), Some(h.key.clone()), "the eviction");
    assert_eq!(ws.load_state(), WorkspaceState::Evicted);

    // The rebuild ends while the other writer still holds the lock.
    let ended = tokio::time::timeout(Duration::from_secs(10), run).await;
    let load_while_held = h.manager.get_or_load(&h.key, &real_builder(), 1);
    release_tx.send(()).expect("release the other writer");
    other_writer.join().expect("the other writer");
    let result = match ended {
        Ok(joined) => joined.expect("runner task"),
        Err(_) => panic!(
            "the persist kept waiting for the other writer's lock after the eviction \
             (load meanwhile: {load_while_held:?})"
        ),
    };
    println!(
        "eviction during the lock wait: result={result:?} load={:?} state={}",
        load_while_held.as_ref().map(|_| ()),
        ws.load_state()
    );
    assert!(
        matches!(result, Err(DaemonError::WorkspaceEvicted { .. })),
        "{result:?}"
    );
    assert!(
        load_while_held.is_ok(),
        "a load during the other writer's hold was refused: {load_while_held:?}"
    );
    assert_eq!(
        std::fs::read(GraphStorage::new(&h.root).manifest_path()).expect("manifest"),
        published_before,
        "the evicted rebuild published nothing"
    );
}

/// Round-nine verification: the eviction case above, in the creating wait.
/// The index directory holds only the other writer's lock file (a first
/// `sqry index` in progress), so the iteration resolved no index and its
/// persist waits through `acquire_unless_cancelled`. An eviction during
/// that wait ends it the same way: nothing published, the eviction's
/// `WorkspaceEvicted`, and a load answered while the writer still holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_eviction_ends_a_persist_waiting_in_the_creating_wait() {
    use sqry_core::graph::unified::persistence::{GraphStorage, IndexWriteLock};
    use std::sync::mpsc;

    // No index: the other writer's lock file will be the directory's only
    // entry, so the persist takes the creating wait.
    let (h, cap) = harness();
    let graph_dir = GraphStorage::new(&h.root).graph_dir().to_path_buf();
    // Another writer holds the persist lock (on a thread of its own: the
    // guard belongs to the thread that took it).
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let other_writer = {
        let graph_dir = graph_dir.clone();
        std::thread::spawn(move || {
            let _held = IndexWriteLock::acquire(&graph_dir).expect("the other writer's lock");
            held_tx.send(()).expect("report the hold");
            release_rx.recv().expect("wait for the release");
        })
    };
    held_rx.recv().expect("the other writer holds the lock");

    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !cap.persist_lock_contended.load(Ordering::Acquire) {
        assert!(
            !run.is_finished(),
            "the iteration ended before it waited for the other writer"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "the rebuild's persist never reached the held lock"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let ws = h.manager.lookup(&h.key).expect("resident");
    assert_eq!(h.manager.evict_lru(), Some(h.key.clone()), "the eviction");
    assert_eq!(ws.load_state(), WorkspaceState::Evicted);

    // The rebuild ends while the other writer still holds the lock.
    let ended = tokio::time::timeout(Duration::from_secs(10), run).await;
    let load_while_held = h.manager.get_or_load(&h.key, &real_builder(), 1);
    release_tx.send(()).expect("release the other writer");
    other_writer.join().expect("the other writer");
    let result = match ended {
        Ok(joined) => joined.expect("runner task"),
        Err(_) => panic!(
            "the persist kept waiting for the other writer's lock after the eviction \
             (load meanwhile: {load_while_held:?})"
        ),
    };
    println!(
        "eviction during the lock wait: result={result:?} load={:?} state={}",
        load_while_held.as_ref().map(|_| ()),
        ws.load_state()
    );
    assert!(
        matches!(result, Err(DaemonError::WorkspaceEvicted { .. })),
        "{result:?}"
    );
    assert!(
        load_while_held.is_ok(),
        "a load during the other writer's hold was refused: {load_while_held:?}"
    );
    assert!(
        !GraphStorage::new(&h.root).manifest_path().exists(),
        "the evicted rebuild published nothing"
    );
}

/// Audit item E (round 8): the build again under the persist lock (D-i8-5)
/// is a full build, so an incremental iteration that needs it reserves a
/// full build's working set first, through the manager. Measured from the
/// manager's own admission figures: the bytes reserved while that build is
/// held exceed the incremental iteration's reservation, and every byte is
/// released afterwards. Before the fix the two figures were equal: the
/// full build ran inside a reservation sized for one changed file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_incremental_iteration_reserves_a_full_build_before_the_rebuild_under_the_lock() {
    use sqry_core::graph::unified::build::MacroOptionsRequest;
    use sqry_core::graph::unified::persistence::GraphStorage;
    use sqry_daemon::RebuildMode;
    use sqry_plugin_registry::{
        PluginSelectionConfig, UnreadableManifestPolicy, build_and_persist_with_workspace_roster,
    };

    let (h, cap) = harness();
    for i in 0..10 {
        std::fs::write(
            h.root.join(format!("f{i}.rs")),
            format!("pub fn f{i}() -> u32 {{ {i} }}\n"),
        )
        .expect("source");
    }
    // A full iteration first, so the prior graph counts every file.
    h.dispatcher
        .handle_changes(&h.key, full_changes())
        .await
        .expect("the full rebuild");
    let edited = h.root.join("f3.rs");
    std::fs::write(&edited, b"pub fn f3() -> u32 { 33 }\n").expect("edit");

    // The full iteration passed the reservation hook too.
    cap.reset_post_reservation_reached();
    cap.arm_post_reservation_hold();
    cap.rebuild_under_lock_hold.store(true, Ordering::Release);
    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let changes = ChangeSet {
        changed_files: vec![edited.clone()],
        git_state_changed: false,
        git_change_class: None,
    };
    let run = tokio::spawn(async move { d.handle_changes(&k, changes).await });
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_reservation())
        .await
        .expect("the rebuild reaches its reservation");
    let reserved_by_iteration = h.manager.status().memory.reserved_bytes;
    assert!(reserved_by_iteration > 0, "the iteration reserved");

    // Another writer records new inputs, so the persist builds again.
    let other_root = h.root.clone();
    tokio::task::spawn_blocking(move || {
        build_and_persist_with_workspace_roster(
            &other_root,
            &PluginSelectionConfig::default(),
            UnreadableManifestPolicy::Refuse,
            "t:other-writer",
            &BuildConfig::default(),
            &MacroOptionsRequest {
                cfg_flags: Some(vec!["test".to_string()]),
                ..MacroOptionsRequest::empty()
            },
            sqry_core::progress::no_op_reporter(),
        )
        .expect("the other writer publishes");
    })
    .await
    .expect("other writer task");
    cap.release_post_reservation();

    let waiter = Arc::clone(&cap);
    let reached = tokio::task::spawn_blocking(move || {
        waiter.wait_until_rebuild_under_lock(Duration::from_secs(60))
    })
    .await
    .expect("hold waiter");
    assert!(reached, "the persist reached the build under the lock");
    let reserved_for_rebuild = h.manager.status().memory.reserved_bytes;
    cap.release_rebuild_under_lock();
    let result = run.await.expect("runner task");
    println!(
        "incremental rebuild under the lock: mode={:?} iteration={reserved_by_iteration} \
         under_lock={reserved_for_rebuild} after={} result={:?}",
        h.dispatcher.last_mode(),
        h.manager.status().memory.reserved_bytes,
        result.as_ref().map(|_| ())
    );
    result.expect("the rebuild publishes");
    assert_eq!(h.dispatcher.last_mode(), Some(RebuildMode::Incremental));
    assert!(
        reserved_for_rebuild > reserved_by_iteration,
        "the full build under the lock ran inside the incremental reservation \
         ({reserved_for_rebuild} reserved, {reserved_by_iteration} by the iteration)"
    );
    assert_eq!(
        h.manager.status().memory.reserved_bytes,
        0,
        "every reservation is released"
    );
    let manifest = GraphStorage::new(&h.root)
        .load_manifest()
        .expect("manifest");
    assert_eq!(
        manifest.macro_options.map(|m| m.cfg_flags),
        Some(vec!["test".to_string()])
    );
}

/// Audit N6 (round 8): a cancel that lands while the persist lock is being
/// handed to the persist, past the wait's own last check, still writes
/// nothing. The persist checks the cancellation once it holds the lock and
/// again just before the transaction. Before, the unchanged-inputs path
/// went straight to the transaction and wrote the index.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_during_the_lock_hand_over_writes_nothing() {
    use sqry_core::graph::unified::persistence::{GraphStorage, PERSIST_LOCK_FILE_NAME};

    let (h, cap) = harness();
    // Only the persist's switch cancels the token.
    cap.suppress_forwarder.store(true, Ordering::Release);
    cap.cancel_token_after_lock_acquired
        .store(true, Ordering::Release);
    let result = h.dispatcher.handle_changes(&h.key, full_changes()).await;
    cap.cancel_token_after_lock_acquired
        .store(false, Ordering::Release);
    let storage = GraphStorage::new(&h.root);
    println!(
        "cancel during the hand-over: result={result:?} manifest={}",
        storage.manifest_path().exists()
    );
    assert!(
        !storage.manifest_path().exists(),
        "a persist cancelled after it took the lock wrote the index"
    );
    assert!(
        matches!(result, Err(DaemonError::WorkspaceEvicted { .. })),
        "{result:?}"
    );
    // No manifest was written, so `try_acquire` would answer "no index"
    // whatever the lock's state: probe the lock file itself.
    let lock_path = storage.graph_dir().join(PERSIST_LOCK_FILE_NAME);
    let free = std::thread::spawn(move || {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("the persist created its lock file");
        file.try_lock().is_ok()
    })
    .join()
    .expect("lock probe");
    assert!(free, "the cancelled persist released the persist lock");
}

/// Audit item E follow-up (round 8): the reservation for the build again
/// under the persist lock is sized from the inputs that build uses. A full
/// iteration reserved for the prior graph's files; another writer widens
/// the recorded roster to include `json` during the build, so the rebuild
/// under the lock also parses the 40 JSON files. The reservation it holds
/// must exceed the iteration's. Before, a full iteration took no top-up
/// (and an incremental one was topped up to the prior graph's full build
/// only), so the widened build ran inside the prior's reservation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_widened_roster_is_reserved_for_before_the_rebuild_under_the_lock() {
    use sqry_core::graph::unified::persistence::GraphStorage;
    use sqry_daemon::RebuildMode;

    let (h, cap) = harness();
    for i in 0..40 {
        std::fs::write(
            h.root.join(format!("c{i}.json")),
            format!("{{\"k{i}\": {i}}}\n"),
        )
        .expect("json source");
    }
    h.dispatcher
        .handle_changes(&h.key, full_changes())
        .await
        .expect("the first full rebuild");
    cap.reset_post_reservation_reached();
    cap.arm_post_reservation_hold();
    cap.rebuild_under_lock_hold.store(true, Ordering::Release);
    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_reservation())
        .await
        .expect("the rebuild reaches its reservation");
    let reserved_by_iteration = h.manager.status().memory.reserved_bytes;

    // Another writer (`sqry index --include-high-cost`) widens the record.
    let storage = GraphStorage::new(&h.root);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(storage.manifest_path()).expect("manifest"))
            .expect("manifest parses");
    manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("recorded ids")
        .push(serde_json::json!("json"));
    manifest["plugin_selection"]["high_cost_mode"] = serde_json::json!("include_all");
    std::fs::write(
        storage.manifest_path(),
        serde_json::to_vec_pretty(&manifest).expect("serialise"),
    )
    .expect("rewrite manifest");
    cap.release_post_reservation();

    let waiter = Arc::clone(&cap);
    let reached = tokio::task::spawn_blocking(move || {
        waiter.wait_until_rebuild_under_lock(Duration::from_secs(60))
    })
    .await
    .expect("hold waiter");
    assert!(reached, "the persist reached the build under the lock");
    let reserved_for_rebuild = h.manager.status().memory.reserved_bytes;
    cap.release_rebuild_under_lock();
    let result = run.await.expect("runner task");
    println!(
        "widened roster: mode={:?} iteration={reserved_by_iteration} under_lock={reserved_for_rebuild} result={:?}",
        h.dispatcher.last_mode(),
        result.as_ref().map(|_| ())
    );
    result.expect("the rebuild publishes");
    assert_eq!(h.dispatcher.last_mode(), Some(RebuildMode::Full));
    assert!(
        reserved_for_rebuild > reserved_by_iteration,
        "the widened build under the lock ran inside the prior graph's reservation \
         ({reserved_for_rebuild} reserved, {reserved_by_iteration} by the iteration)"
    );
    assert_eq!(h.manager.status().memory.reserved_bytes, 0);
    let recorded = storage
        .load_manifest()
        .expect("manifest")
        .plugin_selection
        .expect("selection");
    assert!(recorded.active_plugin_ids.iter().any(|id| id == "json"));
}

/// Decision D-i8-6, the daemon side of `LockWait::IndexRemoved`: a rebuild
/// whose persist waits for another writer's lock, while the index
/// directory is removed (`sqry workspace clean`), ends promptly with a
/// refusal (`WorkspaceBuildFailed`, JSON-RPC `-32001`, reason
/// `INDEX_REMOVED_DURING_PERSIST_WAIT`) without the other writer
/// releasing. Nothing is recreated on disk, and the refusal leaves the slot
/// in the state the iteration entered from (`Loaded`, no failure recorded).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_index_during_the_lock_wait_is_a_refusal() {
    use sqry_core::graph::unified::persistence::{GraphStorage, IndexWriteLock};
    use std::sync::mpsc;

    // An indexed root, so the persist takes the existing-index wait, the
    // one that refuses a removed index.
    let (h, cap) = indexed_harness().await;
    let sqry_dir = h.root.join(".sqry");
    let graph_dir = GraphStorage::new(&h.root).graph_dir().to_path_buf();
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let other_writer = {
        let graph_dir = graph_dir.clone();
        std::thread::spawn(move || {
            let _held = IndexWriteLock::acquire(&graph_dir).expect("the other writer's lock");
            held_tx.send(()).expect("report the hold");
            release_rx.recv().expect("wait for the release");
        })
    };
    held_rx.recv().expect("the other writer holds the lock");
    let ws = h.manager.lookup(&h.key).expect("resident");
    let entered = ws.load_state();

    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !cap.persist_lock_contended.load(Ordering::Acquire) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the rebuild's persist never reached the held lock"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    std::fs::remove_dir_all(&sqry_dir).expect("remove the index directory");

    // The iteration ends while the other writer still holds the lock.
    let ended = tokio::time::timeout(Duration::from_secs(10), run).await;
    let recreated_while_held = sqry_dir.exists();
    release_tx.send(()).expect("release the other writer");
    other_writer.join().expect("the other writer");
    let result = ended
        .expect("the persist kept waiting after the index directory was removed")
        .expect("runner task");
    println!(
        "index removed during the lock wait: result={result:?} state={} last_error={:?} sqry_dir={}",
        ws.load_state(),
        ws.last_error.read().as_ref().map(ToString::to_string),
        sqry_dir.exists()
    );
    match &result {
        Err(err @ DaemonError::WorkspaceBuildFailed { reason, .. }) => {
            assert!(
                reason.contains("the index directory was removed while the rebuild waited"),
                "{reason}"
            );
            assert_eq!(err.jsonrpc_code(), Some(-32001));
        }
        other => panic!("expected the removed-index refusal, got {other:?}"),
    }
    assert!(
        !recreated_while_held,
        "the persist recreated the index directory"
    );
    assert!(!sqry_dir.exists(), "nothing recreated the index directory");
    assert_eq!(
        ws.load_state(),
        entered,
        "a refusal leaves the entered state"
    );
    assert!(
        ws.last_error.read().is_none(),
        "a refusal records no failure"
    );
    assert!(!ws.rebuild_in_flight.load(Ordering::Acquire));
}

/// Decision D-i8-6 (third audit, item 2): an index directory removed after
/// the persist took the lock, here while it holds before its build under
/// the lock, is not recreated. The persist refuses (`WorkspaceBuildFailed`,
/// reason "removed during the persist") and the slot is left as it
/// entered. Before, the transaction ran `create_dir_all` and published:
/// `result=Ok(()) sqry_dir=true manifest=true`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_index_during_the_persist_is_not_recreated() {
    use sqry_core::graph::unified::build::MacroOptionsRequest;
    use sqry_plugin_registry::{
        PluginSelectionConfig, UnreadableManifestPolicy, build_and_persist_with_workspace_roster,
    };

    let (h, cap) = harness();
    let sqry_dir = h.root.join(".sqry");
    let ws = h.manager.lookup(&h.key).expect("resident");
    let entered = ws.load_state();
    cap.arm_post_reservation_hold();
    cap.rebuild_under_lock_hold.store(true, Ordering::Release);
    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_reservation())
        .await
        .expect("the rebuild reaches its reservation");
    // Another writer records new inputs, so the persist builds again under
    // the lock and stops at the hold first.
    let other_root = h.root.clone();
    tokio::task::spawn_blocking(move || {
        build_and_persist_with_workspace_roster(
            &other_root,
            &PluginSelectionConfig::default(),
            UnreadableManifestPolicy::Refuse,
            "t:other-writer",
            &BuildConfig::default(),
            &MacroOptionsRequest {
                cfg_flags: Some(vec!["test".to_string()]),
                ..MacroOptionsRequest::empty()
            },
            sqry_core::progress::no_op_reporter(),
        )
        .expect("the other writer publishes");
    })
    .await
    .expect("other writer task");
    cap.release_post_reservation();
    let waiter = Arc::clone(&cap);
    let reached = tokio::task::spawn_blocking(move || {
        waiter.wait_until_rebuild_under_lock(Duration::from_secs(60))
    })
    .await
    .expect("hold waiter");
    assert!(reached, "the persist reached the build under the lock");
    std::fs::remove_dir_all(&sqry_dir).expect("remove the index directory");
    cap.release_rebuild_under_lock();
    let result = run.await.expect("runner task");
    println!(
        "index removed during the persist: result={result:?} sqry_dir={} state={} last_error={:?}",
        sqry_dir.exists(),
        ws.load_state(),
        ws.last_error.read().as_ref().map(ToString::to_string)
    );
    assert!(
        !sqry_dir.exists(),
        "the persist recreated the removed index"
    );
    match &result {
        Err(DaemonError::WorkspaceBuildFailed { reason, .. }) => {
            assert!(reason.contains("removed during the persist"), "{reason}");
        }
        other => panic!("expected the removed-index refusal, got {other:?}"),
    }
    assert_eq!(
        ws.load_state(),
        entered,
        "a refusal leaves the entered state"
    );
    assert!(
        ws.last_error.read().is_none(),
        "a refusal records no failure"
    );
    // Fourth audit, item 4: refused before the build under the lock ran.
    assert_eq!(
        cap.rebuilds_under_lock.load(Ordering::Acquire),
        0,
        "the persist built again under a stale lock before refusing"
    );
}

/// Index a fresh harness root through one full iteration, so later
/// iterations rebuild an indexed workspace.
async fn indexed_harness() -> (DispatchHarness, Arc<TestCapture>) {
    let (h, cap) = harness();
    h.dispatcher
        .handle_changes(&h.key, full_changes())
        .await
        .expect("the first full rebuild indexes the root");
    cap.reset_post_reservation_reached();
    assert!(h.root.join(".sqry").is_dir(), "precondition: indexed");
    (h, cap)
}

/// Assert `result` is the removed-index refusal with a reason containing
/// `reason_part`, that nothing recreated `.sqry`, and that the slot is left
/// as it entered with no failure recorded.
fn assert_removed_index_refusal(
    h: &DispatchHarness,
    result: &Result<(), DaemonError>,
    reason_part: &str,
    entered: WorkspaceState,
) {
    let ws = h.manager.lookup(&h.key).expect("resident");
    println!(
        "removed index: result={result:?} sqry_dir={} state={} last_error={:?}",
        h.root.join(".sqry").exists(),
        ws.load_state(),
        ws.last_error.read().as_ref().map(ToString::to_string)
    );
    assert!(
        !h.root.join(".sqry").exists(),
        "the persist recreated the removed index"
    );
    match result {
        Err(err @ DaemonError::WorkspaceBuildFailed { reason, .. }) => {
            assert!(reason.contains(reason_part), "{reason}");
            assert_eq!(err.jsonrpc_code(), Some(-32001));
        }
        other => panic!("expected the removed-index refusal, got {other:?}"),
    }
    assert_eq!(
        ws.load_state(),
        entered,
        "a refusal leaves the entered state"
    );
    assert!(
        ws.last_error.read().is_none(),
        "a refusal records no failure"
    );
}

/// Fourth audit, item 2: an index directory removed after the persist's
/// own last check and before the transaction (the seam removes it there)
/// is refused by the transaction, and that refusal is a refusal here too:
/// nothing recreated, the slot left `Loaded` with no failure. Before, the
/// transaction's `create_dir_all` and fresh lock recreated the index and
/// published it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_index_just_before_the_transaction_is_a_refusal() {
    let (h, cap) = indexed_harness().await;
    let entered = h.manager.lookup(&h.key).expect("resident").load_state();
    cap.remove_index_before_transaction
        .store(true, Ordering::Release);
    let result = h.dispatcher.handle_changes(&h.key, full_changes()).await;
    cap.remove_index_before_transaction
        .store(false, Ordering::Release);
    assert_removed_index_refusal(&h, &result, "removed during the persist", entered);
}

/// Fourth audit, item 3: an index removed during the build, before the
/// persist's wait for the lock begins, is not recreated by the wait. The
/// iteration resolved its inputs over an existing index, so the wait
/// creates nothing and answers `IndexRemoved`, which the persist refuses.
/// Before, the wait ran `create_dir_all` and the persist published.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_index_before_the_lock_wait_is_not_recreated() {
    let (h, cap) = indexed_harness().await;
    let entered = h.manager.lookup(&h.key).expect("resident").load_state();
    cap.arm_post_reservation_hold();
    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    tokio::time::timeout(Duration::from_secs(60), cap.wait_until_post_reservation())
        .await
        .expect("the rebuild reaches its reservation");
    std::fs::remove_dir_all(h.root.join(".sqry")).expect("remove the index directory");
    cap.release_post_reservation();
    let result = run.await.expect("runner task");
    assert_removed_index_refusal(
        &h,
        &result,
        "the index directory was removed while the rebuild waited",
        entered,
    );
}

/// Round-nine verification, the daemon side of a content-less index
/// directory (absent, empty or holding only a lock file): the iteration
/// resolves it as no index (`index_present` is false, by the same content
/// predicate the lock uses), so its persist takes the creating wait, as
/// for an absent directory. A lock file left by an interrupted first
/// `sqry index`, with no holder, is persisted over by the next iteration.
/// Before, `index_present` was `graph_dir().is_dir()`, the persist took
/// the existing-index wait, and that wait refused the directory as
/// removed (`-32001`) on every iteration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_left_lock_file_without_an_index_is_persisted_over() {
    use sqry_core::graph::unified::persistence::{
        GraphStorage, IndexWriteLock, PERSIST_LOCK_FILE_NAME,
    };

    let (h, _cap) = harness();
    let storage = GraphStorage::new(&h.root);
    let graph_dir = storage.graph_dir().to_path_buf();
    // An interrupted first index: the lock file created, nothing written.
    drop(IndexWriteLock::acquire(&graph_dir).expect("the first index's lock"));
    assert!(graph_dir.join(PERSIST_LOCK_FILE_NAME).exists());
    assert!(!storage.manifest_path().exists());
    let result = h.dispatcher.handle_changes(&h.key, full_changes()).await;
    let ws = h.manager.lookup(&h.key).expect("resident");
    println!(
        "left lock file: result={result:?} state={} last_error={:?}",
        ws.load_state(),
        ws.last_error.read().as_ref().map(ToString::to_string)
    );
    assert!(
        result.is_ok(),
        "the iteration was not persisted: {result:?}"
    );
    assert!(storage.manifest_path().exists(), "no manifest was written");
    assert_ne!(ws.load_state(), WorkspaceState::Failed);
    assert!(ws.last_error.read().is_none());
}

/// Round-nine verification: with another writer (a first `sqry index`)
/// holding the lock over a content-less index directory, the iteration
/// waits for it, with no `-32001` refusal, and persists once it releases.
/// Before, the existing-index wait refused at once without waiting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_writer_holding_a_content_less_directory_is_waited_for() {
    use sqry_core::graph::unified::persistence::{GraphStorage, IndexWriteLock};
    use std::sync::mpsc;

    let (h, cap) = harness();
    let storage = GraphStorage::new(&h.root);
    let graph_dir = storage.graph_dir().to_path_buf();
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let other_writer = {
        let graph_dir = graph_dir.clone();
        std::thread::spawn(move || {
            let _held = IndexWriteLock::acquire(&graph_dir).expect("the other writer's lock");
            held_tx.send(()).expect("report the hold");
            release_rx.recv().expect("wait for the release");
        })
    };
    held_rx.recv().expect("the other writer holds the lock");

    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let run = tokio::spawn(async move { d.handle_changes(&k, full_changes()).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !cap.persist_lock_contended.load(Ordering::Acquire) {
        if run.is_finished() {
            release_tx.send(()).expect("release the other writer");
            other_writer.join().expect("the other writer");
            let result = run.await.expect("runner task");
            panic!("the iteration ended without waiting for the writer: {result:?}");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the rebuild's persist never reached the held lock"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        !run.is_finished(),
        "the persist did not wait for the writer"
    );
    release_tx.send(()).expect("release the other writer");
    other_writer.join().expect("the other writer");
    let result = tokio::time::timeout(Duration::from_secs(60), run)
        .await
        .expect("the persist kept waiting after the writer released")
        .expect("runner task");
    let ws = h.manager.lookup(&h.key).expect("resident");
    println!(
        "writer over a content-less directory: result={result:?} state={} last_error={:?}",
        ws.load_state(),
        ws.last_error.read().as_ref().map(ToString::to_string)
    );
    assert!(
        result.is_ok(),
        "the iteration was not persisted: {result:?}"
    );
    assert!(storage.manifest_path().exists(), "no manifest was written");
    assert_ne!(ws.load_state(), WorkspaceState::Failed);
    assert!(ws.last_error.read().is_none());
}
