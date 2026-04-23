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
