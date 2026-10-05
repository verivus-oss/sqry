//! Task 7 Phase 7b2 — watcher lifecycle shutdown test.
//!
//! Eviction stops the workspace's file watcher
//! (`LoadedWorkspace::stop_watcher`, which the tombstone writers call).
//! The cancellable watcher returns `Ok(None)`, the blocking watcher
//! thread exits,
//! the tokio mpsc sender drops, the async dispatcher task's
//! `rx.recv()` returns `None`, and the async task exits. Before
//! exiting, the async task marks its `live` flag `false` and calls
//! `reap_watcher`, which removes the entry from the dispatcher's
//! `watchers` map via compare-and-remove.
//!
//! This test asserts the full cascade reaches quiescence: after
//! eviction, `dispatcher.watchers_len() == 0` within a bounded
//! poll window.

use std::time::Duration;

mod support;
use support::{WatcherHarness, wait_until};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn eviction_terminates_watcher_tasks_and_reaps_entry() {
    let h = WatcherHarness::new().await;

    assert_eq!(
        h.dispatcher.watchers_len(),
        1,
        "ensure_watching must have inserted exactly one entry"
    );

    // Simulate eviction of the workspace by sending the watcher's stop
    // signal, which every tombstone writer sends. (The real eviction also
    // swaps in the placeholder and stores `Evicted`; for this test we only
    // need the stop signal: the watcher bridge does not look up the
    // manager, only the workspace Arc, which we hold via the harness.)
    let ws = h.manager.lookup(&h.key).expect("workspace present");
    ws.stop_watcher();

    // The cancellable watcher polls its stop signal on its
    // cancel_poll_period (100 ms). Shutdown cascade:
    //   watcher thread exits → sender drops → async task's rx
    //   returns None → async task exits → live=false + reap_watcher.
    // Total bounded by a couple of cancel_poll_period cycles plus
    // task scheduling.
    let reaped = wait_until(|| h.dispatcher.watchers_len() == 0, Duration::from_secs(3)).await;

    assert!(
        reaped,
        "dispatcher.watchers map must reach size 0 after eviction; current len={}",
        h.dispatcher.watchers_len()
    );
}

/// Audit S7 (plants L03 and L05): once the dispatcher is shutting down it
/// starts no new watcher. `RebuildDispatcher::shutdown` sets the flag and
/// `ensure_watching`'s step 0 reads it under the watcher map's lock, so a
/// load that lands during the shutdown (its watcher started after the
/// shutdown stopped the others) cannot leave a blocking loop the runtime
/// then waits on at exit. Without either half the call starts a watcher.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_watcher_starts_once_the_dispatcher_is_shutting_down() {
    let h = WatcherHarness::new().await;
    let ws = h.manager.lookup(&h.key).expect("workspace present");
    h.dispatcher.shutdown().await;
    let exited = wait_until(|| h.dispatcher.watchers_len() == 0, Duration::from_secs(3)).await;
    assert!(exited, "the shutdown stops the existing watcher");
    let refused = h.dispatcher.ensure_watching(&h.key, &ws, &h.root);
    let started = h.dispatcher.watchers_len();
    println!("after shutdown: ensure_watching={refused:?} watchers={started}");
    // Do not let a leaked watcher outlive the test.
    ws.stop_watcher();
    assert!(
        refused.is_err(),
        "a watcher must not start once the dispatcher is shutting down"
    );
    assert_eq!(started, 0, "no watcher entry was inserted");
}
