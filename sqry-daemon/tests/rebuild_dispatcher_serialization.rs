//! Task 7 Phase 7b2 — A2 §J.2 serialization stress tests.
//!
//! Asserts the "3× rapid-fire `handle_changes` → exactly 2 rebuilds
//! execute" contract documented in plan §Task 7 Step 4b and A2
//! §J.2 Test Requirements.
//!
//! # Harness approach
//!
//! These tests drive `RebuildDispatcher::handle_changes` directly
//! (bypassing the watcher bridge). They use the dispatcher's
//! `TestGate` hook to stall the first iteration inside
//! `execute_one_rebuild`, fire two more dispatches that park+coalesce
//! in the lane, release the gate, and then inspect the `TestCapture`
//! recorder to prove the drain loop consumed exactly two iterations
//! with the correct per-iteration ChangeSet.
//!
//! # Multi-threaded tokio runtime
//!
//! Each test uses `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`
//! so the test-driver task and the spawned handle_changes task can
//! make independent progress. With the default single-thread runtime
//! the spawned #1 would block the driver.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use sqry_core::watch::{ChangeSet, GitChangeClass};
use sqry_daemon::{RebuildMode, TestCapture, TestGate};
use tokio::sync::Notify;

mod support;
use support::{DispatchHarness, wait_until};

fn changes_for(files: Vec<&str>) -> ChangeSet {
    ChangeSet {
        changed_files: files.into_iter().map(PathBuf::from).collect(),
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
// Test 1 — dispatch count: 3 rapid-fire → exactly 2 rebuilds
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rapid_fire_three_dispatches_produces_exactly_two_rebuilds() {
    let h = DispatchHarness::new();
    let gate = Arc::new(TestGate {
        hold: AtomicUsize::new(1),
        release: Notify::new(),
    });
    let cap = Arc::new(TestCapture::default());

    h.dispatcher.install_test_gate(Arc::clone(&gate)).unwrap();
    h.dispatcher.install_test_capture(Arc::clone(&cap)).unwrap();

    // Dispatch #1 spawned — blocks inside gate_check.
    let d1 = Arc::clone(&h.dispatcher);
    let k1 = h.key.clone();
    let h1 = tokio::spawn(async move { d1.handle_changes(&k1, changes_for(vec!["a.rs"])).await });

    // Wait until #1 has acquired the runner role (in_flight=true).
    let acquired = wait_until(
        || {
            h.manager
                .lookup(&h.key)
                .is_some_and(|ws| ws.rebuild_in_flight.load(Ordering::Acquire))
        },
        Duration::from_millis(500),
    )
    .await;
    assert!(
        acquired,
        "dispatch #1 must acquire runner role before firing #2/#3"
    );

    // Dispatch #2 — parks b.rs in lane.
    h.dispatcher
        .handle_changes(&h.key, changes_for(vec!["b.rs"]))
        .await
        .expect("dispatch #2 must return Ok(()) promptly");

    // Dispatch #3 — coalesces c.rs into the parked entry.
    h.dispatcher
        .handle_changes(&h.key, changes_for(vec!["c.rs"]))
        .await
        .expect("dispatch #3 must return Ok(()) promptly");

    // dispatched_count must still be 0 — no iteration has completed
    // yet because #1 is blocked in the gate.
    assert_eq!(
        h.dispatcher.dispatched_count(),
        0,
        "no dispatch should have completed while #1 is gated"
    );

    // Release #1. Its pipeline runs, drain loop picks up the
    // coalesced (b.rs, c.rs), runs iteration 2, exits.
    gate.release.notify_one();
    h1.await
        .expect("dispatch #1 task did not panic")
        .expect("dispatch #1 must succeed after gate release");

    assert_eq!(
        h.dispatcher.dispatched_count(),
        2,
        "3 rapid-fire dispatches must produce exactly 2 rebuilds \
         (first + coalesced second/third)"
    );

    let ws = h.manager.lookup(&h.key).expect("workspace present");
    assert!(
        !ws.rebuild_in_flight.load(Ordering::Acquire),
        "in_flight must be false after drain-loop exit"
    );
    assert!(ws.rebuild_lane.lock().await.is_none(), "lane must be empty");
}

// ---------------------------------------------------------------------------
// Test 2 — file-set union: iteration 2's ChangeSet == union of #2 + #3
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rapid_fire_coalesced_changeset_is_file_union() {
    let h = DispatchHarness::new();
    let gate = Arc::new(TestGate {
        hold: AtomicUsize::new(1),
        release: Notify::new(),
    });
    let cap = Arc::new(TestCapture::default());

    h.dispatcher.install_test_gate(Arc::clone(&gate)).unwrap();
    h.dispatcher.install_test_capture(Arc::clone(&cap)).unwrap();

    let d1 = Arc::clone(&h.dispatcher);
    let k1 = h.key.clone();
    let h1 =
        tokio::spawn(async move { d1.handle_changes(&k1, changes_for(vec!["alpha.rs"])).await });

    wait_until(
        || {
            h.manager
                .lookup(&h.key)
                .is_some_and(|ws| ws.rebuild_in_flight.load(Ordering::Acquire))
        },
        Duration::from_millis(500),
    )
    .await;

    // #2 parks bravo.rs.
    h.dispatcher
        .handle_changes(&h.key, changes_for(vec!["bravo.rs"]))
        .await
        .unwrap();

    // #3 coalesces charlie.rs + delta.rs (file union exercise).
    h.dispatcher
        .handle_changes(&h.key, changes_for(vec!["charlie.rs", "delta.rs"]))
        .await
        .unwrap();

    gate.release.notify_one();
    h1.await.unwrap().unwrap();

    let iters = cap.iterations.lock();
    assert_eq!(
        iters.len(),
        2,
        "expected exactly 2 captured iterations, got {}",
        iters.len()
    );

    // Iteration 1: exactly alpha.rs.
    assert_eq!(
        iters[0].changeset.changed_files,
        vec![PathBuf::from("alpha.rs")],
        "iteration 1 must see exactly the #1 ChangeSet"
    );

    // Iteration 2: coalesced union of #2 + #3, sorted (BTreeSet order
    // from coalesce_with).
    assert_eq!(
        iters[1].changeset.changed_files,
        vec![
            PathBuf::from("bravo.rs"),
            PathBuf::from("charlie.rs"),
            PathBuf::from("delta.rs"),
        ],
        "iteration 2 must see the union of #2 and #3's files in \
         lexicographic order"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — git_state_changed propagation via OR + full-rebuild dominance
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rapid_fire_git_state_or_forces_full_rebuild() {
    let h = DispatchHarness::new();
    let gate = Arc::new(TestGate {
        hold: AtomicUsize::new(1),
        release: Notify::new(),
    });
    let cap = Arc::new(TestCapture::default());

    h.dispatcher.install_test_gate(Arc::clone(&gate)).unwrap();
    h.dispatcher.install_test_capture(Arc::clone(&cap)).unwrap();

    // #1: no git state.
    let d1 = Arc::clone(&h.dispatcher);
    let k1 = h.key.clone();
    let h1 = tokio::spawn(async move { d1.handle_changes(&k1, changes_for(vec!["inc.rs"])).await });

    wait_until(
        || {
            h.manager
                .lookup(&h.key)
                .is_some_and(|ws| ws.rebuild_in_flight.load(Ordering::Acquire))
        },
        Duration::from_millis(500),
    )
    .await;

    // #2: TreeDiverged — sets git_change_class requiring full rebuild.
    h.dispatcher
        .handle_changes(&h.key, full_rebuild_changes())
        .await
        .unwrap();

    // #3: no git state — coalesce-merge preserves TreeDiverged from #2.
    h.dispatcher
        .handle_changes(&h.key, changes_for(vec!["extra.rs"]))
        .await
        .unwrap();

    gate.release.notify_one();
    h1.await.unwrap().unwrap();

    let iters = cap.iterations.lock();
    assert_eq!(iters.len(), 2);

    // Iteration 1: Incremental (single-file edit, no git_state).
    assert_eq!(
        iters[0].mode,
        RebuildMode::Incremental,
        "iteration 1 (single-file edit) must be Incremental"
    );
    assert!(
        !iters[0].changeset.git_state_changed,
        "iteration 1 must NOT have git_state_changed"
    );

    // Iteration 2: Full (git_state_changed OR from #2 AND
    // full-rebuild-dominance merge on git_change_class).
    assert!(
        iters[1].changeset.git_state_changed,
        "iteration 2 must have git_state_changed=true (OR merge of #2|#3)"
    );
    assert!(
        iters[1]
            .changeset
            .git_change_class
            .is_some_and(|c| c.requires_full_rebuild()),
        "iteration 2's git_change_class must require full rebuild \
         (full-rebuild-dominance merge from #2's TreeDiverged)"
    );
    assert_eq!(
        iters[1].mode,
        RebuildMode::Full,
        "iteration 2 must select Full mode"
    );
}
