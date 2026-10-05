//! The cancellable wait for an index's persist lock is not starved by
//! blocking acquirers in other processes (decision D-i8-6, audit N5).
//!
//! Two shell loops, each a separate process tree, take the lock in turn
//! with util-linux `flock(1)` (a blocking `flock(2)`, the call every other
//! sqry writer makes) and hold it 50 ms each time, as a busy user's
//! `sqry index` runs would. The daemon's cancellable wait must win a
//! handoff within 10 s. Linux-only: it needs `flock(1)` and the kernel's
//! `flock` semantics; the test fails, rather than skips, when `flock(1)` is
//! missing, so it never passes without running.

#![cfg(target_os = "linux")]

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use sqry_core::graph::unified::persistence::{IndexWriteLock, LockWait, PERSIST_LOCK_FILE_NAME};

/// The lock loops; dropping it asks them to stop (the stop file) and waits
/// for each to finish its current hold and exit.
struct Loops {
    children: Vec<Child>,
    stop: std::path::PathBuf,
}

impl Drop for Loops {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.stop, b"");
        for child in &mut self.children {
            let _ = child.wait();
        }
    }
}

#[test]
fn a_cancellable_wait_is_not_starved_by_blocking_waiters_in_other_processes() {
    assert!(
        Command::new("flock")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success()),
        "util-linux flock(1) is required"
    );
    let dir = tempfile::tempdir().unwrap();
    let graph_dir = dir.path().join(".sqry/graph");
    std::fs::create_dir_all(&graph_dir).unwrap();
    let lock = graph_dir.join(PERSIST_LOCK_FILE_NAME);
    let count = dir.path().join("holds");
    std::fs::write(&count, b"").unwrap();
    let stop = dir.path().join("stop");
    let script = format!(
        "while [ ! -e '{stop}' ]; do flock '{lock}' sh -c 'echo x >> \"{count}\"; sleep 0.05'; done",
        stop = stop.display(),
        lock = lock.display(),
        count = count.display()
    );
    let loops = Loops {
        children: (0..2)
            .map(|_| Command::new("sh").arg("-c").arg(&script).spawn().unwrap())
            .collect(),
        stop,
    };
    // Start once the two processes are trading the lock.
    let started = Instant::now() + Duration::from_secs(30);
    while std::fs::read(&count).unwrap().len() < 4 {
        assert!(Instant::now() < started, "the lock loops never ran");
        std::thread::sleep(Duration::from_millis(5));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let outcome = IndexWriteLock::acquire_unless_cancelled(
        &graph_dir,
        &|| Instant::now() > deadline,
        &mut || {},
    )
    .unwrap();
    let won = matches!(outcome, LockWait::Held(_));
    drop(outcome);
    let holds = std::fs::read(&count).unwrap().len() / 2;
    drop(loops);
    assert!(
        won,
        "the cancellable wait never won a handoff from two blocking processes in 10 s ({holds} holds)"
    );
}
