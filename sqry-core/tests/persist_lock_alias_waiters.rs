//! Cancelled waits through different spellings of one graph directory
//! share one helper thread (decision D-i8-6, Codex round-eight review item
//! 2). Its own test binary, so no other test's threads move the count;
//! Linux-only, as the count is read from `/proc/self/task`.

#![cfg(target_os = "linux")]

use std::cell::Cell;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sqry_core::graph::unified::persistence::{IndexWriteLock, LockWait};

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").unwrap().count()
}

/// Codex round-eight review, item 2: cancelled waits through different
/// spellings of one graph directory (16 symlink aliases) still share one
/// helper, since they wait for one lock file. Before, the registry was
/// keyed by path and identity, so each alias left a helper parked: 3
/// threads became 19.
#[test]
fn cancelled_waits_through_aliases_share_one_helper() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real");
    let graph_dir = real.join(".sqry/graph");
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = {
        let graph_dir = graph_dir.clone();
        std::thread::spawn(move || {
            let _held = IndexWriteLock::acquire(&graph_dir).unwrap();
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        })
    };
    held_rx.recv().unwrap();
    let aliases: Vec<_> = (0..16)
        .map(|i| {
            let alias = dir.path().join(format!("alias{i}"));
            std::os::unix::fs::symlink(&real, &alias).unwrap();
            alias.join(".sqry/graph")
        })
        .collect();
    let baseline = threads();
    for alias in &aliases {
        let cancelled = Cell::new(false);
        let outcome =
            IndexWriteLock::acquire_unless_cancelled(alias, &|| cancelled.get(), &mut || {
                cancelled.set(true)
            })
            .unwrap();
        assert!(matches!(outcome, LockWait::Cancelled), "{outcome:?}");
    }
    let during = threads();
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while threads() > baseline {
        assert!(
            Instant::now() < deadline,
            "the helper threads did not exit after the holder released"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("alias helper threads: baseline {baseline}, after 16 alias waits {during}");
    assert!(
        during <= baseline + 1,
        "16 cancelled waits through aliases left {} helper threads (bound: 1)",
        during - baseline
    );
}
