//! Task 7 Phase 7b2 — watcher lifecycle shutdown test.
//!
//! Eviction sets `ws.rebuild_cancelled = true`. The cancellable
//! watcher returns `Ok(None)`, the blocking watcher thread exits,
//! the tokio mpsc sender drops, the async dispatcher task's
//! `rx.recv()` returns `None`, and the async task exits. Before
//! exiting, the async task marks its `live` flag `false` and calls
//! `reap_watcher`, which removes the entry from the dispatcher's
//! `watchers` map via compare-and-remove.
//!
//! This test asserts the full cascade reaches quiescence: after
//! eviction, `dispatcher.watchers_len() == 0` within a bounded
//! poll window.

use std::{sync::atomic::Ordering, time::Duration};

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

    // Simulate eviction of the workspace by setting rebuild_cancelled.
    // (The real `execute_eviction` path additionally removes the
    // workspace from the manager map; for this test we only need
    // the cancellation signal — the watcher bridge does not lookup
    // the manager, only the workspace Arc, which we hold via the
    // harness.)
    let ws = h.manager.lookup(&h.key).expect("workspace present");
    ws.rebuild_cancelled.store(true, Ordering::Release);

    // The cancellable watcher polls rebuild_cancelled on its
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
