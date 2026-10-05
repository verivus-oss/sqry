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

// Every test here installs a `TestCapture`, which only the `test-hooks`
// feature compiles (fifth audit, item 2).
#![cfg(feature = "test-hooks")]

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use sqry_core::graph::unified::build::MacroOptionsRequest;
use sqry_core::watch::{ChangeSet, GitChangeClass};
use sqry_daemon::{DaemonError, RebuildMode, TestCapture, TestGate, WorkspaceState};
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

// ---------------------------------------------------------------------------
// Integration of W1 and W4: each daemon/rebuild request receives the outcome
// of the iteration that consumed it. A request parked behind a running
// rebuild used to be told `Completed` whatever its own iteration did.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_rebuild_reports_its_own_refusal_and_the_runner_its_own_success() {
    let h = DispatchHarness::new();
    let gate = Arc::new(TestGate {
        hold: AtomicUsize::new(2),
        release: Notify::new(),
    });
    let cap = Arc::new(TestCapture::default());
    h.dispatcher.install_test_gate(Arc::clone(&gate)).unwrap();
    h.dispatcher.install_test_capture(Arc::clone(&cap)).unwrap();

    // The runner: a request with no macro options, stalled in the gate.
    let d1 = Arc::clone(&h.dispatcher);
    let k1 = h.key.clone();
    let runner = tokio::spawn(async move {
        d1.handle_changes_with_macro_options(
            &k1,
            changes_for(vec!["a.rs"]),
            MacroOptionsRequest::empty(),
        )
        .await
        .wait()
        .await
    });
    assert!(
        wait_until(|| cap.iterations.lock().len() == 1, Duration::from_secs(10)).await,
        "the runner's iteration must reach the gate"
    );

    // Queued behind it: a request naming an expand cache that does not exist.
    let missing = h.root.join("no-such-expand-cache");
    let queued = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec![]),
            MacroOptionsRequest::from_flags(&[], Some(&missing), false),
        )
        .await;

    // Release the runner's iteration; the drain loop takes the queued request
    // and stalls it in the gate, so its before-state can be read.
    gate.release.notify_one();
    assert!(
        wait_until(|| cap.iterations.lock().len() == 2, Duration::from_secs(30)).await,
        "the drain loop must take the queued request"
    );
    let ws = h.manager.lookup(&h.key).expect("workspace present");
    let graph_before = ws.graph();
    let manifest = h.root.join(".sqry/graph/manifest.json");
    let manifest_before = std::fs::read(&manifest).ok();
    gate.release.notify_one();

    let own = tokio::time::timeout(Duration::from_secs(30), queued.wait())
        .await
        .expect("the queued request's outcome arrives");
    assert!(
        matches!(own, Err(DaemonError::RebuildMacroOptionsUnavailable { .. })),
        "the queued request must receive its own refusal, got {own:?}"
    );
    let runner_own = runner.await.expect("runner task did not panic");
    assert!(
        runner_own.is_ok(),
        "the runner's own iteration succeeded and must be reported so, got {runner_own:?}"
    );
    assert!(
        Arc::ptr_eq(&graph_before, &ws.graph()),
        "the refused iteration must publish nothing"
    );
    assert_eq!(
        std::fs::read(&manifest).ok(),
        manifest_before,
        "the refused iteration must write no manifest"
    );
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_rebuild_with_valid_options_reports_success() {
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
    let runner =
        tokio::spawn(async move { d1.handle_changes(&k1, changes_for(vec!["a.rs"])).await });
    assert!(
        wait_until(|| cap.iterations.lock().len() == 1, Duration::from_secs(10)).await,
        "the runner's iteration must reach the gate"
    );

    let queued = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec![]),
            MacroOptionsRequest::from_flags(&["unix".to_string()], None, false),
        )
        .await;
    gate.release.notify_one();

    let own = tokio::time::timeout(Duration::from_secs(30), queued.wait())
        .await
        .expect("the queued request's outcome arrives");
    assert!(
        own.is_ok(),
        "a valid queued request must succeed, got {own:?}"
    );
    runner
        .await
        .expect("runner task did not panic")
        .expect("the drain loop's last iteration succeeded");
    assert_eq!(
        cap.iterations.lock().len(),
        2,
        "the queued request ran its own iteration"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_differing_explicit_request_is_refused_while_another_is_queued() {
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
    let runner =
        tokio::spawn(async move { d1.handle_changes(&k1, changes_for(vec!["a.rs"])).await });
    assert!(
        wait_until(|| cap.iterations.lock().len() == 1, Duration::from_secs(10)).await,
        "the runner's iteration must reach the gate"
    );

    let first = MacroOptionsRequest::from_flags(&["unix".to_string()], None, false);
    let other = MacroOptionsRequest::from_flags(&["windows".to_string()], None, false);
    let queued = h
        .dispatcher
        .handle_changes_with_macro_options(&h.key, changes_for(vec![]), first.clone())
        .await;
    let same = h
        .dispatcher
        .handle_changes_with_macro_options(&h.key, changes_for(vec![]), first.clone())
        .await;
    let differing = h
        .dispatcher
        .handle_changes_with_macro_options(&h.key, changes_for(vec![]), other)
        .await;

    let refused = tokio::time::timeout(Duration::from_secs(5), differing.wait())
        .await
        .expect("the refusal is immediate");
    assert!(
        matches!(refused, Err(DaemonError::InvalidArgument { .. })),
        "a differing explicit request must be refused, got {refused:?}"
    );
    let ws = h.manager.lookup(&h.key).expect("workspace present");
    assert_eq!(
        ws.rebuild_lane
            .lock()
            .await
            .as_ref()
            .map(|parked| parked.macro_request.clone()),
        Some(first),
        "the parked request keeps its own options"
    );

    gate.release.notify_one();
    for (name, outcome) in [("queued", queued), ("same", same)] {
        let own = tokio::time::timeout(Duration::from_secs(30), outcome.wait())
            .await
            .expect("the outcome arrives");
        assert!(
            own.is_ok(),
            "{name}: an equal request shares the iteration, got {own:?}"
        );
    }
    runner
        .await
        .expect("runner task did not panic")
        .expect("runner ok");
    assert_eq!(
        cap.iterations.lock().len(),
        2,
        "the two equal requests coalesce into one iteration"
    );
}

// ---------------------------------------------------------------------------
// The merge rule (decision D-i7-1): two entries share one iteration only when
// their macro requests mean the same, or when one side is a watcher-driven
// enqueue (no waiters, an empty request). Each pair is driven in both orders
// behind a runner stalled in the gate, and the request each iteration ran
// with is read from the capture.
// ---------------------------------------------------------------------------

/// A harness whose first rebuild (a watcher-shaped runner) is stalled in the
/// gate, so the requests a test sends next park behind it.
async fn stalled_runner() -> (
    DispatchHarness,
    Arc<TestGate>,
    Arc<TestCapture>,
    tokio::task::JoinHandle<Result<(), DaemonError>>,
) {
    let h = DispatchHarness::new();
    let gate = Arc::new(TestGate {
        hold: AtomicUsize::new(1),
        release: Notify::new(),
    });
    let cap = Arc::new(TestCapture::default());
    h.dispatcher.install_test_gate(Arc::clone(&gate)).unwrap();
    h.dispatcher.install_test_capture(Arc::clone(&cap)).unwrap();
    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let runner = tokio::spawn(async move { d.handle_changes(&k, changes_for(vec!["a.rs"])).await });
    assert!(
        wait_until(|| cap.iterations.lock().len() == 1, Duration::from_secs(10)).await,
        "the runner's iteration must reach the gate"
    );
    (h, gate, cap, runner)
}

/// Round 7, plant P23: a request that parks after the drain loop found its
/// lane empty, but before the loop released the runner role, is run by
/// that loop. The release path takes the lane again and finds it; without
/// that re-check the role is released with the request parked, and it
/// waits for the next dispatch. The loop is held at the release hook while
/// the request parks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_parked_before_the_release_is_run_by_the_runner() {
    let (h, gate, cap, runner) = stalled_runner().await;
    cap.arm_pre_release_hold();
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(30), cap.wait_until_pre_release())
        .await
        .expect("the runner's loop reaches its release");
    let ws = h.manager.lookup(&h.key).expect("resident");
    assert!(
        ws.rebuild_in_flight.load(Ordering::Acquire),
        "precondition: the runner still holds the role"
    );
    let late = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec!["late.rs"]),
            MacroOptionsRequest::empty(),
        )
        .await;
    assert!(
        wait_until(
            || ws.rebuild_lane.try_lock().is_ok_and(|lane| lane.is_some()),
            Duration::from_secs(10)
        )
        .await,
        "the late request parks behind the runner"
    );
    cap.release_pre_release();
    let answered = tokio::time::timeout(Duration::from_secs(30), late.wait()).await;
    println!(
        "late request: answered={} iterations={}",
        answered.is_ok(),
        cap.iterations.lock().len()
    );
    let answered = answered.expect("the late request is run, not stranded until the next dispatch");
    assert!(answered.is_ok(), "{answered:?}");
    assert_eq!(cap.iterations.lock().len(), 2, "the runner ran it");
    runner
        .await
        .expect("runner task did not panic")
        .expect("the drain loop's last iteration succeeded");
    assert!(!ws.rebuild_in_flight.load(Ordering::Acquire));
}

/// The macro request the `n`th iteration (0-based) ran with.
fn ran_with(cap: &TestCapture, n: usize) -> MacroOptionsRequest {
    cap.iterations.lock()[n].macro_request.clone()
}

/// Release the stalled runner and wait for the drain loop to finish.
async fn finish(gate: &TestGate, runner: tokio::task::JoinHandle<Result<(), DaemonError>>) {
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(60), runner)
        .await
        .expect("the drain loop finishes")
        .expect("runner task did not panic")
        .expect("the drain loop's last iteration succeeded");
}

fn explicit_cfg(flags: &[&str]) -> MacroOptionsRequest {
    MacroOptionsRequest {
        cfg_flags: Some(flags.iter().map(|flag| (*flag).to_string()).collect()),
        ..MacroOptionsRequest::empty()
    }
}

/// A watcher enqueue parked first, an explicit `daemon/rebuild` second: they
/// merge, and the one iteration runs with the explicit options (rule 6, a
/// later non-empty request replaces an empty one). The caller is answered
/// with that iteration's success.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_request_merges_over_a_parked_watcher_enqueue_and_runs_with_its_options() {
    let (h, gate, cap, runner) = stalled_runner().await;
    h.dispatcher
        .handle_changes(&h.key, changes_for(vec!["b.rs"]))
        .await
        .expect("the watcher enqueue parks");
    let explicit = explicit_cfg(&["unix"]);
    let caller = h
        .dispatcher
        .handle_changes_with_macro_options(&h.key, changes_for(vec![]), explicit.clone())
        .await;
    finish(&gate, runner).await;
    let own = tokio::time::timeout(Duration::from_secs(30), caller.wait())
        .await
        .expect("the outcome arrives");
    assert!(own.is_ok(), "{own:?}");
    assert_eq!(cap.iterations.lock().len(), 2, "one merged iteration");
    assert_eq!(
        ran_with(&cap, 1),
        explicit,
        "the merged iteration runs with the explicit request's options"
    );
    assert_eq!(
        cap.iterations.lock()[1].changeset.changed_files,
        vec![PathBuf::from("b.rs")],
        "the watcher's changes ride along"
    );
}

/// The reverse order: an explicit `daemon/rebuild` parked first, a watcher
/// enqueue second. They merge and the explicit options are kept (rule 6, an
/// empty later request keeps the earlier one); a watcher enqueue never erases
/// a parked caller's options.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watcher_enqueue_merges_into_a_parked_explicit_request_and_keeps_its_options() {
    let (h, gate, cap, runner) = stalled_runner().await;
    let explicit = explicit_cfg(&["unix"]);
    let caller = h
        .dispatcher
        .handle_changes_with_macro_options(&h.key, changes_for(vec![]), explicit.clone())
        .await;
    h.dispatcher
        .handle_changes(&h.key, changes_for(vec!["b.rs"]))
        .await
        .expect("the watcher enqueue merges, it is not refused");
    finish(&gate, runner).await;
    let own = tokio::time::timeout(Duration::from_secs(30), caller.wait())
        .await
        .expect("the outcome arrives");
    assert!(own.is_ok(), "{own:?}");
    assert_eq!(cap.iterations.lock().len(), 2, "one merged iteration");
    assert_eq!(
        ran_with(&cap, 1),
        explicit,
        "the watcher enqueue must not erase the parked explicit options"
    );
}

/// O1: a plain `daemon/rebuild` (a waiter with an empty request) parked
/// first, an explicit request second. Merging would answer the plain caller
/// with the outcome of an iteration that ran with options it never asked for,
/// so the explicit request is refused (`-32602`) and the parked one runs as
/// it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_explicit_request_is_refused_while_a_plain_request_is_queued() {
    let (h, gate, cap, runner) = stalled_runner().await;
    let plain = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec![]),
            MacroOptionsRequest::empty(),
        )
        .await;
    let missing = h.root.join("no-such-expand-cache");
    let explicit = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec![]),
            MacroOptionsRequest::from_flags(&[], Some(&missing), false),
        )
        .await;
    let refused = tokio::time::timeout(Duration::from_secs(5), explicit.wait())
        .await
        .expect("the refusal is immediate");
    match &refused {
        Err(err @ DaemonError::InvalidArgument { reason }) => {
            assert_eq!(err.jsonrpc_code(), Some(-32602));
            assert!(reason.contains("already queued"), "{reason}");
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
    finish(&gate, runner).await;
    let own = tokio::time::timeout(Duration::from_secs(30), plain.wait())
        .await
        .expect("the outcome arrives");
    assert!(
        own.is_ok(),
        "the plain caller is answered for its own request, not -32022: {own:?}"
    );
    assert_eq!(cap.iterations.lock().len(), 2);
    assert!(
        ran_with(&cap, 1).is_empty(),
        "the parked plain request ran as it was"
    );
}

/// O1, the reverse order: an explicit request parked first, a plain
/// `daemon/rebuild` second. The plain request is refused (`-32602`); the
/// explicit one runs with its options.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plain_request_is_refused_while_an_explicit_request_is_queued() {
    let (h, gate, cap, runner) = stalled_runner().await;
    let explicit_request = explicit_cfg(&["unix"]);
    let explicit = h
        .dispatcher
        .handle_changes_with_macro_options(&h.key, changes_for(vec![]), explicit_request.clone())
        .await;
    let plain = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec![]),
            MacroOptionsRequest::empty(),
        )
        .await;
    let refused = tokio::time::timeout(Duration::from_secs(5), plain.wait())
        .await
        .expect("the refusal is immediate");
    assert!(
        matches!(refused, Err(DaemonError::InvalidArgument { .. })),
        "a plain request must not be answered for another request's options: {refused:?}"
    );
    finish(&gate, runner).await;
    let own = tokio::time::timeout(Duration::from_secs(30), explicit.wait())
        .await
        .expect("the outcome arrives");
    assert!(own.is_ok(), "{own:?}");
    assert_eq!(cap.iterations.lock().len(), 2);
    assert_eq!(ran_with(&cap, 1), explicit_request);
}

/// Requests are compared by meaning, not text (the macro audit's queued-request
/// note): with `cachedir` parked, `./cachedir` and `<root>/cachedir` name the
/// same directory and merge; cfg flags in another order are the same set and
/// merge. The controls: a different directory and a different flag set are
/// refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_naming_one_directory_or_one_flag_set_merge_and_others_are_refused() {
    let (h, gate, cap, runner) = stalled_runner().await;
    std::fs::create_dir_all(h.root.join("cachedir")).expect("cache dir");
    std::fs::create_dir_all(h.root.join("othercache")).expect("other cache dir");
    let with_dir = |dir: PathBuf, flags: &[&str]| MacroOptionsRequest {
        expand_cache_dir: Some(dir),
        ..explicit_cfg(flags)
    };
    let first = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec![]),
            with_dir(PathBuf::from("cachedir"), &["a", "b"]),
        )
        .await;
    let dotted = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec![]),
            with_dir(PathBuf::from("./cachedir/"), &["b", "a"]),
        )
        .await;
    let absolute = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec![]),
            with_dir(h.root.join("cachedir"), &["a", "b", "a"]),
        )
        .await;
    let other_dir = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec![]),
            with_dir(PathBuf::from("othercache"), &["a", "b"]),
        )
        .await;
    let other_flags = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec![]),
            with_dir(PathBuf::from("cachedir"), &["a", "c"]),
        )
        .await;
    for (label, outcome) in [("other directory", other_dir), ("other flags", other_flags)] {
        let refused = tokio::time::timeout(Duration::from_secs(5), outcome.wait())
            .await
            .expect("the refusal is immediate");
        assert!(
            matches!(refused, Err(DaemonError::InvalidArgument { .. })),
            "{label}: must be refused, got {refused:?}"
        );
    }
    finish(&gate, runner).await;
    for (label, outcome) in [("first", first), ("dotted", dotted), ("absolute", absolute)] {
        let own = tokio::time::timeout(Duration::from_secs(30), outcome.wait())
            .await
            .expect("the outcome arrives");
        assert!(
            own.is_ok(),
            "{label}: one directory, one iteration: {own:?}"
        );
    }
    assert_eq!(
        cap.iterations.lock().len(),
        2,
        "the three spellings share one iteration"
    );
    let canonical = h.root.join("cachedir").canonicalize().expect("canonical");
    assert_eq!(
        ran_with(&cap, 1).expand_cache_dir,
        Some(canonical),
        "the iteration runs with the canonical directory"
    );
}

/// A runner that unwinds mid-iteration (F5, P1's class). The request it was
/// running is answered `Internal` (its sender was dropped by the unwind, which
/// `RebuildOutcome::wait` never reads as success), the request parked behind
/// it is answered `Internal` by the sentinel instead of waiting out its bound,
/// the workspace is not left `Rebuilding` with no runner, and the next
/// request runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_runner_unwind_answers_every_waiter_with_an_error() {
    let (h, gate, cap, runner) = stalled_runner().await;
    // The request whose iteration will unwind, parked behind the runner.
    let unwinding = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec!["c.rs"]),
            MacroOptionsRequest::empty(),
        )
        .await;
    // Hold the next iteration too, so a request can park behind it: the
    // release below takes the runner's hold, this one stays.
    gate.hold.store(2, Ordering::Release);
    gate.release.notify_one();
    assert!(
        wait_until(|| cap.iterations.lock().len() == 2, Duration::from_secs(30)).await,
        "the drain loop takes the parked request"
    );
    let parked = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec!["d.rs"]),
            MacroOptionsRequest::empty(),
        )
        .await;
    cap.panic_in_next_iteration.store(true, Ordering::Release);
    gate.release.notify_one();

    let own = tokio::time::timeout(Duration::from_secs(30), unwinding.wait())
        .await
        .expect("the unwinding request is answered");
    let behind = tokio::time::timeout(Duration::from_secs(30), parked.wait())
        .await
        .expect("the parked request is answered, not left to its bound");
    println!("unwound: own={own:?} parked={behind:?}");
    // The request whose iteration unwound: its sender was dropped.
    match &own {
        Err(DaemonError::Internal(reason)) => assert!(
            reason
                .to_string()
                .contains("exited before reporting this request's outcome"),
            "{reason}"
        ),
        other => panic!("own: an unwind must be an error, got {other:?}"),
    }
    // The request parked behind it: answered by the sentinel.
    match &behind {
        Err(DaemonError::Internal(reason)) => assert!(
            reason
                .to_string()
                .contains("unwound before running this request"),
            "{reason}"
        ),
        other => panic!("parked: an unwind must be an error, got {other:?}"),
    }
    assert!(
        runner.await.is_err(),
        "the drain loop ran on the runner's task, which unwound"
    );
    let ws = h.manager.lookup(&h.key).expect("resident");
    assert!(
        wait_until(
            || !ws.rebuild_in_flight.load(Ordering::Acquire),
            Duration::from_secs(10)
        )
        .await,
        "the runner role is released"
    );
    assert_eq!(
        ws.load_state(),
        WorkspaceState::Failed,
        "an unwound iteration is not left Rebuilding with no runner"
    );
    // The sentinel records why (round 7, plant P13): a `Failed` workspace
    // with no recorded failure would serve stale with nothing to say.
    assert!(
        ws.last_error
            .read()
            .as_ref()
            .is_some_and(|err| err.to_string().contains("unwound mid-iteration")),
        "the unwind is recorded: {:?}",
        ws.last_error.read().as_ref().map(ToString::to_string)
    );
    assert!(
        ws.rebuild_lane.lock().await.is_none(),
        "nothing stays parked"
    );
    let next = h
        .dispatcher
        .handle_changes_with_macro_options(
            &h.key,
            changes_for(vec![]),
            MacroOptionsRequest::empty(),
        )
        .await;
    let next = tokio::time::timeout(Duration::from_secs(60), next.wait())
        .await
        .expect("the next request is answered");
    assert!(next.is_ok(), "the next request runs: {next:?}");
    assert_eq!(ws.load_state(), WorkspaceState::Loaded);
}
