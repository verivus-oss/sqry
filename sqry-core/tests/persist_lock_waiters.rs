//! Cancelled waits for an index's persist lock do not pile up helper
//! threads (decision D-i8-6, third audit item 3).
//!
//! While another holder keeps the lock, 200 cancellable waits each give up
//! after their first step. They must share one helper thread per lock file
//! (later waits reuse the one already waiting), so the process gains at
//! most one thread, and that helper must release and exit promptly once
//! the holder lets go. Its own test binary, so no other test's threads
//! move the count; Linux-only, as the count is read from `/proc/self/task`.

#![cfg(target_os = "linux")]

use std::cell::Cell;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sqry_core::graph::unified::persistence::{IndexWriteLock, LockWait};

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").unwrap().count()
}

#[test]
fn cancelled_waits_share_one_helper_and_it_releases_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let graph_dir = dir.path().join(".sqry/graph");
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = {
        let graph_dir = graph_dir.clone();
        std::thread::spawn(move || {
            let _held = IndexWriteLock::acquire(&graph_dir).unwrap();
            // Index content, so the `try_acquire` probe at the end can take
            // the lock at all (a lock file alone is no index).
            std::fs::write(graph_dir.join("manifest.json"), b"{}").unwrap();
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
    };
    held_rx.recv().unwrap();
    let baseline = threads();
    for _ in 0..200 {
        let cancelled = Cell::new(false);
        let outcome =
            IndexWriteLock::acquire_unless_cancelled(&graph_dir, &|| cancelled.get(), &mut || {
                cancelled.set(true)
            })
            .unwrap();
        assert!(matches!(outcome, LockWait::Cancelled), "{outcome:?}");
    }
    let during = threads();
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    // The helper takes the lock on release, finds no waiter, releases and
    // exits: the count returns to the baseline and the lock is free.
    let deadline = Instant::now() + Duration::from_secs(5);
    while threads() > baseline {
        assert!(
            Instant::now() < deadline,
            "the helper threads did not exit after the holder released ({} over the baseline)",
            threads() - baseline
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let free = {
        let graph_dir = graph_dir.clone();
        std::thread::spawn(move || IndexWriteLock::try_acquire(&graph_dir).unwrap().is_some())
            .join()
            .unwrap()
    };
    println!("helper threads: baseline {baseline}, after 200 cancelled waits {during}");
    assert!(
        during <= baseline + 1,
        "200 cancelled waits left {} helper threads (bound: 1)",
        during - baseline
    );
    assert!(free, "the lock is free once the helper released it");
}
