//! Task 7 Phase 7c — eviction-during-rebuild abort tests.
//!
//! Proves that BOTH cancellation surfaces work independently:
//!
//! 1. The §5e `workspaces.read()` publish-recheck path fires when
//!    eviction happens while the post-reservation gate is held and
//!    the pipeline completes fast enough that the forwarder has not
//!    yet flipped the token. Assertion: `publish_path_evictions == 1`.
//! 2. The sqry-core pass-boundary cancellation path fires when the
//!    forwarder has time to flip the token before the pipeline
//!    completes. Assertion: `pass_boundary_cancellations == 1`.
//!
//! Both paths surface `DaemonError::WorkspaceEvicted` (JSON-RPC
//! -32004), refund the admission reservation (RAII drop), leave
//! `dispatched_count` unchanged, and result in the workspace being
//! removed from the manager map.
//!
//! Iter-1 Codex MAJOR 2 fix: the single combined test from iter-0
//! accepted "either path" which made it impossible to prove the
//! mechanisms independently. These two focused tests + the counter
//! instrumentation in `TestCapture` close that gap.

// Every test here installs a `TestCapture`, which only the `test-hooks`
// feature compiles (fifth audit, item 2).
#![cfg(feature = "test-hooks")]

mod support;

use std::{path::PathBuf, sync::Arc, time::Duration};

use sqry_core::watch::ChangeSet;
use sqry_daemon::{DaemonError, RebuildDispatcher, TestCapture};

fn trivial_changes() -> ChangeSet {
    ChangeSet {
        changed_files: vec![PathBuf::from("seed.rs")],
        git_state_changed: false,
        git_change_class: None,
    }
}

/// Force §5e publish-recheck path: suppress the forwarder so the
/// pipeline cannot have its token flipped during execution. Evict
/// while the post-reservation gate is held. When released, the
/// pipeline runs Ok(graph), returns to the §5e `workspaces.read()`
/// block, which observes `rebuild_cancelled = true` and returns
/// WorkspaceEvicted without publishing.
///
/// Without forwarder suppression the test would race — seed.rs
/// rebuilds faster than the forwarder spawns, but the forwarder
/// reliably beats it on contended hosts. Suppression makes the §5e
/// path deterministic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eviction_during_rebuild_hits_publish_path_recheck() {
    use std::sync::atomic::Ordering;

    let harness = support::WatcherHarness::new().await;

    let capture = Arc::new(TestCapture::new());
    harness
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("first install");

    // Iter-1 Codex MAJOR 2: suppress the cancellation forwarder so
    // the pipeline cannot cancel via the pass-boundary path. This
    // forces the §5e recheck to handle eviction.
    capture.suppress_forwarder.store(true, Ordering::Release);

    capture.arm_post_reservation_hold();

    let dispatched_before = harness.dispatcher.dispatched_count();
    assert_eq!(
        harness.manager.status().memory.reserved_bytes,
        0,
        "baseline: no reservation before the rebuild"
    );

    let dispatcher_clone: Arc<RebuildDispatcher> = Arc::clone(&harness.dispatcher);
    let key_clone = harness.key.clone();
    let rebuild_task = tokio::spawn(async move {
        dispatcher_clone
            .handle_changes(&key_clone, trivial_changes())
            .await
    });

    capture.wait_until_post_reservation().await;

    // Reservation is live at this point.
    assert!(
        harness.manager.status().memory.reserved_bytes > 0,
        "post-reservation hook must fire with a live reservation"
    );

    // Evict — rebuild_cancelled=true + removed from map.
    harness.manager.unload(&harness.key);

    // Release immediately. Pipeline runs fast (<50ms for seed.rs),
    // completes before the forwarder's first poll. §5e recheck
    // catches rebuild_cancelled.
    capture.release_post_reservation();

    let result = rebuild_task.await.expect("join").expect_err("must error");
    assert!(
        matches!(result, DaemonError::WorkspaceEvicted { .. }),
        "expected WorkspaceEvicted, got: {result:?}"
    );

    // Settle for forwarder abort + drain loop.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // COUNTER assertion: §5e path fired.
    assert_eq!(
        capture.publish_path_evictions(),
        1,
        "publish-path recheck must fire exactly once"
    );
    assert_eq!(
        capture.pass_boundary_cancellations(),
        0,
        "pass-boundary path must NOT fire when pipeline beats forwarder poll"
    );

    assert_eq!(
        harness.manager.status().memory.reserved_bytes,
        0,
        "reservation must refund"
    );
    assert_eq!(
        harness.dispatcher.dispatched_count(),
        dispatched_before,
        "dispatched_count must not advance"
    );
    assert!(
        harness.manager.lookup(&harness.key).is_none(),
        "evicted workspace must be removed from manager map"
    );
}

/// Force sqry-core pass-boundary cancellation path deterministically.
///
/// Uses `TestCapture::precancel_token_for_pass_boundary = true` so
/// `execute_rebuild` synchronously calls `token.cancel()` BEFORE
/// dispatching the blocking pipeline. The pipeline's first
/// `cancellation.check()?` fires immediately, returning
/// `GraphBuilderError::Cancelled`, which `map_graph_builder_err`
/// translates to `DaemonError::WorkspaceEvicted`. The `execute_rebuild`
/// error arm increments `pass_boundary_cancellations` and returns
/// before the §5e recheck block.
///
/// Iter-2 Codex MAJOR 1 fix: the iter-1 version used a 120ms sleep
/// which fired BEFORE `execute_rebuild` was entered — timing rationale
/// was wrong because the forwarder isn't spawned until the gate
/// releases. The pre-cancel switch closes the determinism gap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eviction_during_rebuild_hits_pass_boundary_cancellation() {
    use std::sync::atomic::Ordering;

    let harness = support::WatcherHarness::new().await;

    let capture = Arc::new(TestCapture::new());
    harness
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("first install");

    // Iter-2 Codex MAJOR 1 fix: arm the pre-cancel switch so the
    // pipeline deterministically sees a cancelled token on its first
    // check.
    capture
        .precancel_token_for_pass_boundary
        .store(true, Ordering::Release);

    capture.arm_post_reservation_hold();

    let dispatched_before = harness.dispatcher.dispatched_count();

    let dispatcher_clone: Arc<RebuildDispatcher> = Arc::clone(&harness.dispatcher);
    let key_clone = harness.key.clone();
    let rebuild_task = tokio::spawn(async move {
        dispatcher_clone
            .handle_changes(&key_clone, trivial_changes())
            .await
    });

    capture.wait_until_post_reservation().await;
    assert!(
        harness.manager.status().memory.reserved_bytes > 0,
        "reservation must be live at hook"
    );

    harness.manager.unload(&harness.key);
    capture.release_post_reservation();

    let result = rebuild_task.await.expect("join").expect_err("must error");
    assert!(
        matches!(result, DaemonError::WorkspaceEvicted { .. }),
        "expected WorkspaceEvicted, got: {result:?}"
    );

    tokio::time::sleep(Duration::from_millis(150)).await;

    // Iter-2 Codex MAJOR 1 fix: with the pre-cancel switch armed,
    // the pass-boundary path fires deterministically. The §5e
    // recheck path is unreachable because execute_rebuild returns
    // Err before reaching §5e.
    assert_eq!(
        capture.pass_boundary_cancellations(),
        1,
        "pass-boundary cancellation must fire exactly once with pre-cancel armed"
    );
    assert_eq!(
        capture.publish_path_evictions(),
        0,
        "publish-path recheck must NOT fire when pipeline is pre-cancelled"
    );

    assert_eq!(
        harness.manager.status().memory.reserved_bytes,
        0,
        "reservation must refund"
    );
    assert_eq!(
        harness.dispatcher.dispatched_count(),
        dispatched_before,
        "dispatched_count must not advance"
    );
    assert!(
        harness.manager.lookup(&harness.key).is_none(),
        "evicted workspace must be removed from manager map"
    );
}

/// Round 8 review, note (a): an LRU eviction while a rebuild is held after
/// its reservation, then a load of the evicted workspace that fails, then
/// the rebuild released. The load's gate consumed the eviction's
/// cancellation (a load from `Evicted` clears it), the cancellation
/// forwarder is suppressed as above, so the runner read a clear flag at
/// its publish recheck and published its graph into the slot the failed
/// load had left `Failed`: `Ok`, one published generation, a `Failed`
/// workspace carrying a graph. The gate now refuses a load while the
/// evicted generation's runner holds the runner role and leaves the flag
/// for it, so the runner answers `WorkspaceEvicted` and publishes nothing;
/// the control is the same load once the runner has stopped, which builds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_load_after_an_eviction_does_not_let_the_evicted_rebuild_publish() {
    use std::sync::atomic::Ordering;

    use sqry_core::graph::unified::build::BuildConfig;
    use sqry_daemon::FailingGraphBuilder;
    use sqry_daemon::workspace::{WorkingSetInputs, WorkspaceState, working_set_estimate};

    let harness = support::WatcherHarness::new().await;
    let capture = Arc::new(TestCapture::new());
    harness
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("first install");
    capture.suppress_forwarder.store(true, Ordering::Release);
    capture.arm_post_reservation_hold();
    let dispatched_before = harness.dispatcher.dispatched_count();
    let estimate = working_set_estimate(WorkingSetInputs {
        new_graph_final_estimate: 64 * 1024,
        staging_overhead: 32 * 1024,
        interner_snapshot_bytes: 16 * 1024,
    });

    let dispatcher_clone: Arc<RebuildDispatcher> = Arc::clone(&harness.dispatcher);
    let key_clone = harness.key.clone();
    let rebuild_task = tokio::spawn(async move {
        dispatcher_clone
            .handle_changes(&key_clone, trivial_changes())
            .await
    });
    capture.wait_until_post_reservation().await;

    let ws = harness.manager.lookup(&harness.key).expect("registered");
    let evicted = harness.manager.evict_lru();
    let evicted_state = ws.load_state();
    let load = harness.manager.get_or_load(
        &harness.key,
        &FailingGraphBuilder::new("planted load failure"),
        estimate,
    );
    let load_answer = match &load {
        Ok(_) => "Ok".to_string(),
        Err(DaemonError::WorkspaceBuildFailed { reason, .. }) => reason.clone(),
        Err(other) => format!("{other:?}"),
    };
    let state_after_load = ws.load_state();
    let flag_after_load = ws.rebuild_cancelled.load(Ordering::Acquire);

    capture.release_post_reservation();
    let rebuild = rebuild_task.await.expect("join");
    tokio::time::sleep(Duration::from_millis(150)).await;
    let published = capture.published_generations.lock().len();
    let state_after_rebuild = ws.load_state();
    let graph_after_rebuild = ws.published().roster.is_some();

    // The control: with the runner stopped, the load builds.
    let builder = support::RealGraphBuilder {
        plugins: Arc::new(sqry_plugin_registry::create_plugin_manager()),
        cfg: BuildConfig::default(),
    };
    let reload = harness
        .manager
        .get_or_load(&harness.key, &builder, estimate)
        .map(|_| ());
    let state_after_reload = ws.load_state();

    println!(
        "R8 note (a): evicted={evicted:?} ({evicted_state}); load={load_answer} \
         state={state_after_load} flag={flag_after_load}; rebuild={:?} published={published} \
         state={state_after_rebuild} graph={graph_after_rebuild}; reload={reload:?} \
         state={state_after_reload}",
        rebuild.as_ref().map(|_| "Ok")
    );
    assert_eq!(
        evicted.as_ref(),
        Some(&harness.key),
        "the eviction took the rebuilding workspace"
    );
    assert_eq!(evicted_state, WorkspaceState::Evicted);
    assert!(
        load_answer.contains("already in progress"),
        "a load while the evicted generation's rebuild runs is refused as in progress, \
         got: {load_answer}"
    );
    assert!(
        matches!(rebuild, Err(DaemonError::WorkspaceEvicted { .. })),
        "the evicted generation's rebuild must not complete: {rebuild:?}"
    );
    assert_eq!(published, 0, "nothing is published after the eviction");
    assert_eq!(
        state_after_rebuild,
        WorkspaceState::Evicted,
        "the tombstone is left to its writer"
    );
    assert!(!graph_after_rebuild, "the tombstone carries no generation");
    assert_eq!(
        harness.dispatcher.dispatched_count(),
        dispatched_before,
        "dispatched_count must not advance"
    );
    assert!(reload.is_ok(), "the control load builds: {reload:?}");
    assert_eq!(state_after_reload, WorkspaceState::Loaded);
}
