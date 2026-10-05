//! T55 (surface parity W1 round 7, design D36, codex R7-0): a rebuild's
//! success bookkeeping runs under the publish block's own
//! `workspaces.read()` guard, so an eviction cannot complete between the
//! publish and the state that reports it.
//!
//! Ahead of round 7 the three operations that report the generation an
//! iteration published (the `last_indexed_git_state` write,
//! `LoadedWorkspace::record_success` and the store of
//! `WorkspaceState::Loaded`) ran after that block ended, with no manager
//! lock held and with the state store comparing nothing. An eviction
//! that completed in that window had already swapped
//! `PublishedGraph::placeholder()` in and stored `Evicted`; the store
//! then made the slot `(Loaded, placeholder)`, which
//! `WorkspaceManager::classify_for_serve` and
//! `WorkspaceManager::resident_generation` answer with
//! `DaemonError::Internal` naming a workspace that "is servable but
//! carries no roster record", instead of the `WorkspaceEvicted` that
//! makes the shared acquirer reload from the snapshot.
//!
//! How this test reaches that window without a test-only seam inside
//! the dispatcher: it takes `ws.last_good_at.write()`, a production
//! field on `LoadedWorkspace` that `record_success` writes, and holds it
//! while a real `handle_changes` runs. The rebuild publishes, then
//! blocks inside `record_success`. With D36 in place it is blocked with
//! the publish block's read guard still held, so
//! `WorkspaceManager::try_evict_for_test`, which takes
//! `workspaces.try_write()`, answers `TryEvictOutcome::WouldBlock` at
//! every sample. That is round 7's addition to what D38 narrowed the
//! exclusivity sentence to: the guard the reader observes here is held
//! by a production path, not by the test that asks.
//!
//! Row: C64 (the success bookkeeping moved back outside the publish
//! block with the `Loaded` store written as a bare `store_state`, which
//! is the pre-change shape), killed on the first assertion. Declared
//! must-survive beside it: S28 (the three operations reordered among
//! themselves inside the guard), S29 (the guard dropped and retaken
//! around them) and S32 (the store written as `store_state(Loaded)` with
//! D36's guard kept).

#![cfg(feature = "test-hooks")]

mod support;

use std::{path::PathBuf, sync::Arc, time::Duration};

use sqry_core::watch::ChangeSet;
use sqry_daemon::workspace::manager::TryEvictOutcome;
use sqry_daemon::{RebuildDispatcher, WorkspaceState};

/// How many times the eviction hook is asked while the bookkeeping is
/// blocked. Every answer must be `WouldBlock`.
const SAMPLES: usize = 64;

/// Upper bound on the polls that wait for the publish to land. Asserted
/// non-zero (the publish is not instantaneous from here) and under the
/// bound (the rebuild did not hang).
const POLL_BOUND: usize = 2_500;

/// Wall time between polls, and between every eighth sample, so the
/// sampling window is real time on another thread and not only a run of
/// scheduler yields.
const TICK: Duration = Duration::from_millis(2);

fn trivial_changes() -> ChangeSet {
    ChangeSet {
        changed_files: vec![PathBuf::from("seed.rs")],
        git_state_changed: false,
        git_change_class: None,
    }
}

// Holding `last_good_at.write()` across the awaits below IS the
// mechanism: it parks the rebuild inside `record_success` so the
// eviction hook can be asked while the bookkeeping is in flight. The
// lint is about accidental contention, and this contention is the
// measurement.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rebuild_success_bookkeeping_holds_the_publish_guard() {
    let harness = support::WatcherHarness::new().await;

    let ws = harness.manager.lookup(&harness.key).expect("registered");
    let before = ws.published();
    let fixture_nodes = before.graph.node_count();
    assert!(
        fixture_nodes > 0,
        "the harness's own load published a non-empty generation"
    );
    assert!(
        before.roster.is_some(),
        "the harness's own load published a roster record"
    );

    // A production field, not a test hook: `record_success` takes this
    // write lock, so holding it parks the rebuild exactly where the
    // success bookkeeping begins.
    let good_at_guard = ws.last_good_at.write();

    let dispatcher_clone: Arc<RebuildDispatcher> = Arc::clone(&harness.dispatcher);
    let key_clone = harness.key.clone();
    let rebuild_task = tokio::spawn(async move {
        dispatcher_clone
            .handle_changes(&key_clone, trivial_changes())
            .await
    });

    // Wait for the publish itself: `publish_and_retain` swaps the
    // generation, so the slot's `Arc` changes. Bounded.
    let mut polls = 0usize;
    let mut published_changed = false;
    while polls < POLL_BOUND {
        polls += 1;
        let current = harness
            .manager
            .lookup(&harness.key)
            .expect("the workspace stays in the map")
            .published();
        if !Arc::ptr_eq(&current, &before) {
            published_changed = true;
            break;
        }
        tokio::time::sleep(TICK).await;
    }

    // Sample the eviction hook while the bookkeeping is parked.
    let mut outcomes: Vec<TryEvictOutcome> = Vec::with_capacity(SAMPLES);
    for i in 0..SAMPLES {
        outcomes.push(harness.manager.try_evict_for_test(&harness.key));
        if i % 8 == 7 {
            tokio::time::sleep(TICK).await;
        } else {
            tokio::task::yield_now().await;
        }
    }
    let first_non_would_block = outcomes
        .iter()
        .position(|o| *o != TryEvictOutcome::WouldBlock);

    // Release the bookkeeping and let the iteration finish.
    drop(good_at_guard);
    let rebuild = rebuild_task.await.expect("join");

    let after_state = ws.load_state();
    let after_record = ws.roster().is_some();
    let after_nodes = ws.graph().node_count();

    // The in-test positive control: the same call, with no guard held,
    // evicts. Without it a green could mean the hook never works here.
    let control = harness.manager.try_evict_for_test(&harness.key);
    let tombstone_state = ws.load_state();
    let tombstone_record = ws.roster().is_some();
    let tombstone_nodes = ws.graph().node_count();

    println!(
        "R7-0 bookkeeping guard: polls={polls} samples={SAMPLES} \
         first_non_would_block={first_non_would_block:?} published_changed={published_changed} \
         rebuild={rebuild:?} after_join=({after_state},{after_record},{after_nodes}) \
         control={control:?} tombstone=({tombstone_state},{tombstone_record},{tombstone_nodes})"
    );

    // 1. The guard. This is the assertion C64 fails.
    assert_eq!(
        first_non_would_block,
        None,
        "every one of the {SAMPLES} eviction attempts must answer WouldBlock while the \
         rebuild's success bookkeeping is parked under the publish guard; sample \
         {first_non_would_block:?} answered {:?}",
        first_non_would_block.map(|i| outcomes[i])
    );
    assert_eq!(outcomes.len(), SAMPLES, "every sample must have been taken");

    // 2. The wait was a real wait and it terminated.
    assert!(
        published_changed,
        "the rebuild must have published a new generation within {POLL_BOUND} polls"
    );
    assert!(
        polls > 0 && polls < POLL_BOUND,
        "the publish poll count must be non-zero and inside its bound: polls={polls} bound={POLL_BOUND}"
    );

    // 3. The iteration completed, and its bookkeeping describes the
    //    generation it published.
    assert!(
        rebuild.is_ok(),
        "the parked rebuild must complete once the bookkeeping lock is released: {rebuild:?}"
    );
    assert_eq!(
        (after_state, after_record),
        (WorkspaceState::Loaded, true),
        "after the join the slot must be Loaded and carry the roster record its publish wrote"
    );
    assert!(
        after_nodes > 0,
        "the published generation must be non-empty: after_nodes={after_nodes}"
    );

    // 4. The control, and the tombstone it leaves.
    assert_eq!(
        control,
        TryEvictOutcome::Evicted,
        "the same call must evict when no guard is held, which is what makes the \
         WouldBlock samples above evidence"
    );
    assert_eq!(
        (tombstone_state, tombstone_record, tombstone_nodes),
        (WorkspaceState::Evicted, false, 0),
        "the tombstone is Evicted, carries no roster record and holds the placeholder generation"
    );

    // Settle so the dispatcher's own post-publish work is done before the
    // harness drops.
    tokio::time::sleep(Duration::from_millis(50)).await;
}
