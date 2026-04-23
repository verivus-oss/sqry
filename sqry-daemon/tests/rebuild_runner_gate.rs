//! Task 7 Phase 7b1 — runner-role gate + drain-loop test binary.
//!
//! Covers the Amendment 2 §J.2 serial-consumer invariants that
//! `RebuildDispatcher::handle_changes` must honour:
//!
//! 1. At most one runner per workspace executes the pipeline at a
//!    time. Concurrent callers park their coalesced `PendingRebuild`
//!    in `rebuild_lane` and return `Ok(())` without running the
//!    pipeline.
//! 2. The runner's drain loop picks up the parked `PendingRebuild` at
//!    the end of each iteration and runs another iteration until the
//!    lane is empty.
//! 3. Workspace eviction (`rebuild_cancelled == true`) terminates the
//!    drain loop at its next iteration's top-of-loop gate, surfaces
//!    `DaemonError::WorkspaceEvicted`, and drops any parked pending.
//! 4. A workspace unloaded between `handle_changes`' initial
//!    `manager.lookup` and the test's await point surfaces
//!    `DaemonError::WorkspaceEvicted` via the lookup-None path.
//! 5. `execute_one_rebuild` records `record_success` on publish so
//!    `ws.last_good_at` is stamped and `ws.retry_count` is reset.
//!
//! Separately, `sqry-daemon/src/workspace/manager.rs` inline tests
//! (`reserve_rebuild_rejects_unknown_key`,
//! `reserve_rebuild_rejects_cancelled_workspace`) cover the
//! `reserve_rebuild` Phase-1 membership + cancellation checks
//! directly at the admission layer.
//!
//! # Harness design
//!
//! Each test boots a real `RebuildDispatcher` against a tempdir
//! fixture. Good paths use an empty tempdir — the unified-graph build
//! pipeline produces an empty graph in ~ms, fast enough for the
//! serialization + eviction stress tests.
//!
//! Per Codex iter-2 MINOR 2: tests that synchronise across tokio
//! tasks use the `Notify` "obtain `notified()` future first, then
//! trigger, then await" handshake to avoid lost wakeups.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use sqry_core::{
    graph::unified::build::BuildConfig,
    project::ProjectRootMode,
    watch::{ChangeSet, GitChangeClass},
};
use sqry_daemon::{
    DaemonConfig, DaemonError, PendingRebuild, RebuildDispatcher, WorkspaceKey, WorkspaceManager,
    workspace::{WorkingSetInputs, WorkspaceBuilder, working_set_estimate},
};
use tempfile::TempDir;
use tokio::sync::Notify;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct RealGraphBuilder {
    plugins: Arc<sqry_core::plugin::PluginManager>,
    cfg: BuildConfig,
}

impl std::fmt::Debug for RealGraphBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealGraphBuilder").finish_non_exhaustive()
    }
}

impl WorkspaceBuilder for RealGraphBuilder {
    fn build(&self, root: &Path) -> Result<sqry_core::graph::CodeGraph, DaemonError> {
        sqry_core::graph::unified::build::build_unified_graph(root, &self.plugins, &self.cfg)
            .map_err(|e| DaemonError::WorkspaceBuildFailed {
                root: root.to_path_buf(),
                reason: format!("test build: {e}"),
            })
    }
}

struct Harness {
    _tmp: TempDir,
    key: WorkspaceKey,
    manager: Arc<WorkspaceManager>,
    dispatcher: Arc<RebuildDispatcher>,
}

fn make_harness() -> Harness {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().to_path_buf();

    let config = Arc::new(DaemonConfig::default());
    let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
    let plugins = Arc::new(sqry_plugin_registry::create_plugin_manager());
    let dispatcher = RebuildDispatcher::new(
        Arc::clone(&manager),
        Arc::clone(&config),
        Arc::clone(&plugins),
    );

    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);

    let builder = RealGraphBuilder {
        plugins: Arc::clone(&plugins),
        cfg: BuildConfig::default(),
    };
    let estimate = working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: 64 * 1024,
        staging_overhead: 32 * 1024,
        interner_snapshot_bytes: 16 * 1024,
    });
    manager
        .get_or_load(&key, &builder, estimate)
        .expect("initial load of empty fixture must succeed");

    Harness {
        _tmp: tmp,
        key,
        manager,
        dispatcher,
    }
}

fn empty_changes() -> ChangeSet {
    ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: false,
        git_change_class: None,
    }
}

fn single_file_changes(path: PathBuf) -> ChangeSet {
    ChangeSet {
        changed_files: vec![path],
        git_state_changed: false,
        git_change_class: None,
    }
}

fn full_rebuild_changes() -> ChangeSet {
    ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: true,
        git_change_class: Some(GitChangeClass::TreeDiverged),
    }
}

// ---------------------------------------------------------------------------
// §J.2 — single serial caller
// ---------------------------------------------------------------------------

#[tokio::test]
async fn serial_caller_runs_exactly_once() {
    let h = make_harness();
    let before = h.dispatcher.dispatched_count();
    h.dispatcher
        .handle_changes(&h.key, empty_changes())
        .await
        .expect("empty changes must succeed");
    assert_eq!(
        h.dispatcher.dispatched_count(),
        before + 1,
        "single serial call must advance dispatched_count by exactly 1"
    );
    let ws = h.manager.lookup(&h.key).expect("workspace registered");
    assert!(
        !ws.rebuild_in_flight.load(Ordering::Acquire),
        "in_flight must be false after drain-loop exit"
    );
}

#[tokio::test]
async fn drain_loop_exits_with_empty_lane() {
    let h = make_harness();
    h.dispatcher
        .handle_changes(&h.key, empty_changes())
        .await
        .expect("ok");
    let ws = h.manager.lookup(&h.key).expect("present");
    let lane_guard = ws.rebuild_lane.lock().await;
    assert!(
        lane_guard.is_none(),
        "lane must be empty after a single serial caller completes"
    );
    assert!(
        !ws.rebuild_in_flight.load(Ordering::Acquire),
        "in_flight must be false"
    );
}

#[tokio::test]
async fn successful_rebuild_stamps_last_good_at_and_resets_retry_count() {
    let h = make_harness();
    let ws = h.manager.lookup(&h.key).expect("present");

    // Pre-condition: initial load via `get_or_load` already stamped
    // last_good_at. Clear it + set retry_count=5 to prove the rebuild
    // path performs its OWN bookkeeping via record_success.
    *ws.last_good_at.write() = None;
    ws.retry_count.store(5, Ordering::Release);
    *ws.last_error.write() = Some(DaemonError::WorkspaceBuildFailed {
        root: h.key.index_root.clone(),
        reason: "seeded prior-error".into(),
    });

    h.dispatcher
        .handle_changes(&h.key, empty_changes())
        .await
        .expect("empty-ChangeSet rebuild must succeed");

    assert!(
        ws.last_good_at.read().is_some(),
        "successful handle_changes must stamp last_good_at via record_success"
    );
    assert_eq!(
        ws.retry_count.load(Ordering::Acquire),
        0,
        "successful handle_changes must reset retry_count via record_success"
    );
    assert!(
        ws.last_error.read().is_none(),
        "successful handle_changes must clear last_error via record_success"
    );
}

// ---------------------------------------------------------------------------
// §J.2 — parked-caller semantics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn second_caller_while_runner_active_parks_in_lane() {
    // Prime `in_flight = true` directly so the call hits the park
    // branch deterministically without racing a real runner.
    let h = make_harness();
    let ws = h.manager.lookup(&h.key).expect("present");
    ws.rebuild_in_flight.store(true, Ordering::Release);

    let before_count = h.dispatcher.dispatched_count();
    let file = PathBuf::from("main.rs");
    let result = h
        .dispatcher
        .handle_changes(&h.key, single_file_changes(file.clone()))
        .await;
    assert!(
        result.is_ok(),
        "parked caller must return Ok(()) promptly, got {result:?}"
    );
    assert_eq!(
        h.dispatcher.dispatched_count(),
        before_count,
        "parked caller must NOT increment dispatched_count"
    );

    let lane_guard = ws.rebuild_lane.lock().await;
    let parked = lane_guard.as_ref().expect("parked pending present");
    assert_eq!(
        parked.changes.changed_files,
        vec![file],
        "parked pending must contain the caller's ChangeSet"
    );
    drop(lane_guard);

    ws.rebuild_in_flight.store(false, Ordering::Release);
}

#[tokio::test]
async fn parked_callers_coalesce_file_union() {
    let h = make_harness();
    let ws = h.manager.lookup(&h.key).expect("present");
    ws.rebuild_in_flight.store(true, Ordering::Release);

    h.dispatcher
        .handle_changes(&h.key, single_file_changes(PathBuf::from("a.rs")))
        .await
        .expect("first park ok");
    h.dispatcher
        .handle_changes(&h.key, single_file_changes(PathBuf::from("b.rs")))
        .await
        .expect("second park ok");

    let lane_guard = ws.rebuild_lane.lock().await;
    let parked = lane_guard.as_ref().expect("parked pending present");
    let mut files = parked.changes.changed_files.clone();
    files.sort();
    assert_eq!(
        files,
        vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")],
        "coalesced parked pending must union both callers' files"
    );
    drop(lane_guard);

    ws.rebuild_in_flight.store(false, Ordering::Release);
}

#[tokio::test]
async fn parked_caller_full_rebuild_class_propagates_through_coalesce() {
    let h = make_harness();
    let ws = h.manager.lookup(&h.key).expect("present");
    ws.rebuild_in_flight.store(true, Ordering::Release);

    h.dispatcher
        .handle_changes(&h.key, single_file_changes(PathBuf::from("a.rs")))
        .await
        .expect("first park ok");
    h.dispatcher
        .handle_changes(&h.key, full_rebuild_changes())
        .await
        .expect("second park ok");

    let lane_guard = ws.rebuild_lane.lock().await;
    let parked = lane_guard.as_ref().expect("parked pending present");
    assert!(
        parked.changes.requires_full_rebuild(),
        "coalesced pending must carry the full-rebuild signal from the second park"
    );
    drop(lane_guard);

    ws.rebuild_in_flight.store(false, Ordering::Release);
}

#[tokio::test]
async fn parked_caller_does_not_increment_dispatched_count() {
    let h = make_harness();
    let ws = h.manager.lookup(&h.key).expect("present");
    ws.rebuild_in_flight.store(true, Ordering::Release);

    let before = h.dispatcher.dispatched_count();
    for i in 0..5 {
        h.dispatcher
            .handle_changes(
                &h.key,
                single_file_changes(PathBuf::from(format!("f{i}.rs"))),
            )
            .await
            .expect("park ok");
    }
    assert_eq!(
        h.dispatcher.dispatched_count(),
        before,
        "parked callers must never increment dispatched_count"
    );

    ws.rebuild_in_flight.store(false, Ordering::Release);
}

// ---------------------------------------------------------------------------
// §J.2 — drain loop runs parked pending after runner finishes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn runner_drains_parked_pending_after_first_iteration() {
    // C1 runs an iteration; C2 parks mid-flight. After C1 finishes,
    // the drain loop must re-lock the lane, take C2's parked pending,
    // and run a second iteration. Total dispatched_count = 2.
    let h = make_harness();
    let dispatcher_c1 = Arc::clone(&h.dispatcher);
    let key_c1 = h.key.clone();
    let dispatcher_c2 = Arc::clone(&h.dispatcher);
    let key_c2 = h.key.clone();

    let c1_handle = tokio::spawn(async move {
        dispatcher_c1
            .handle_changes(&key_c1, full_rebuild_changes())
            .await
    });

    // Give C1 a head start to take the runner role.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // C2 parks.
    let c2_result = dispatcher_c2
        .handle_changes(&key_c2, single_file_changes(PathBuf::from("parked.rs")))
        .await;
    assert!(
        c2_result.is_ok(),
        "C2 must park successfully (or race-win the runner role)"
    );

    let c1_result = c1_handle.await.expect("c1 task panicked");
    assert!(c1_result.is_ok(), "c1 rebuild ok");

    // Final state: at most one runner remaining, lane empty,
    // dispatched_count advanced by at least 2 (C1's iteration + the
    // drain of C2's parked, IF C2 parked). If C2 race-won and ran
    // independently, count could also be 2. Either way: >= 1 and
    // <= 2, with final in_flight cleared.
    let count = h.dispatcher.dispatched_count();
    assert!(
        count == 1 || count == 2,
        "expected 1 or 2 pipeline runs, got {count}"
    );

    let ws = h.manager.lookup(&h.key).expect("present");
    assert!(
        !ws.rebuild_in_flight.load(Ordering::Acquire),
        "in_flight must be released after drain loop exits"
    );
    let lane = ws.rebuild_lane.lock().await;
    assert!(lane.is_none(), "lane must be empty after full drain");
}

// ---------------------------------------------------------------------------
// CAS-under-lane race-freedom stress
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_callers_terminate_cleanly_with_empty_lane() {
    // 20 concurrent callers — at any instant at most one is the
    // runner. No panic, no deadlock, and after all join the
    // post-storm state is: in_flight == false, lane empty.
    let h = make_harness();
    let callers = 20;
    let mut handles = Vec::with_capacity(callers);
    for i in 0..callers {
        let d = Arc::clone(&h.dispatcher);
        let k = h.key.clone();
        let changes = single_file_changes(PathBuf::from(format!("f{i}.rs")));
        handles.push(tokio::spawn(
            async move { d.handle_changes(&k, changes).await },
        ));
    }
    for h_ in handles {
        h_.await.expect("task join").expect("handle_changes ok");
    }

    let ws = h.manager.lookup(&h.key).expect("present");
    assert!(
        !ws.rebuild_in_flight.load(Ordering::Acquire),
        "post-storm: no runner should be active"
    );
    let lane = ws.rebuild_lane.lock().await;
    assert!(lane.is_none(), "post-storm: lane must be empty");

    let count = h.dispatcher.dispatched_count();
    assert!(count >= 1, "at least one pipeline must have run");
    assert!(
        count <= callers as u64,
        "pipeline runs cannot exceed caller count (got {count})"
    );
}

// ---------------------------------------------------------------------------
// Eviction cooperation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cancelled_workspace_gate_returns_evicted_and_drops_parked() {
    // Top-of-loop gate path: pre-park a pending, flip cancelled,
    // then call handle_changes. The gate fires, returns
    // WorkspaceEvicted, and drops the parked pending.
    let h = make_harness();
    let ws = h.manager.lookup(&h.key).expect("present");

    *ws.rebuild_lane.lock().await = Some(PendingRebuild {
        changes: single_file_changes(PathBuf::from("stranded.rs")),
        enqueued_at: std::time::Instant::now(),
        git_state_at_enqueue: None,
    });
    ws.rebuild_cancelled.store(true, Ordering::Release);

    let before_count = h.dispatcher.dispatched_count();
    let result = h.dispatcher.handle_changes(&h.key, empty_changes()).await;
    assert!(
        matches!(result, Err(DaemonError::WorkspaceEvicted { .. })),
        "cancelled workspace must surface WorkspaceEvicted, got {result:?}"
    );
    assert_eq!(
        h.dispatcher.dispatched_count(),
        before_count,
        "no pipeline must run when top-of-loop gate fires"
    );
    let lane = ws.rebuild_lane.lock().await;
    assert!(
        lane.is_none(),
        "gate must take + drop parked pending on eviction"
    );
    drop(lane);
    assert!(
        !ws.rebuild_in_flight.load(Ordering::Acquire),
        "in_flight must be released by the gate path"
    );

    ws.rebuild_cancelled.store(false, Ordering::Release);
}

#[tokio::test]
async fn unloaded_workspace_surfaces_evicted_from_lookup() {
    // `handle_changes`' initial `manager.lookup` path: unload the
    // workspace, then call handle_changes — lookup returns None and
    // handle_changes returns WorkspaceEvicted without running any
    // pipeline step.
    //
    // Per Codex iter-2 MINOR 2 on Notify handshake: obtain the
    // `notified()` future BEFORE signalling so we cannot miss the
    // wakeup.
    let h = make_harness();

    let ready = Arc::new(Notify::new());
    let go = Arc::new(Notify::new());

    let dispatcher = Arc::clone(&h.dispatcher);
    let key = h.key.clone();
    let ready_clone = Arc::clone(&ready);
    let go_clone = Arc::clone(&go);
    let call_handle = tokio::spawn(async move {
        // Arm the waiter BEFORE signalling ready, so we cannot miss
        // the signal from the test side.
        let wait = go_clone.notified();
        tokio::pin!(wait);
        ready_clone.notify_one();
        wait.await;
        dispatcher.handle_changes(&key, empty_changes()).await
    });

    // Wait for the task to arm its waiter.
    ready.notified().await;

    // Trigger eviction. This removes the workspace from the manager
    // map AND sets rebuild_cancelled = true (atomic under
    // workspaces.write()).
    let unloaded = h.manager.unload(&h.key);
    assert!(unloaded, "workspace must have been present to unload");

    // Release the task.
    go.notify_one();

    let result = call_handle.await.expect("task join");
    assert!(
        matches!(result, Err(DaemonError::WorkspaceEvicted { .. })),
        "handle_changes on an unloaded workspace must surface WorkspaceEvicted, got {result:?}"
    );
    assert_eq!(
        h.dispatcher.dispatched_count(),
        0,
        "evicted-workspace path must not run any pipeline iteration"
    );
}
