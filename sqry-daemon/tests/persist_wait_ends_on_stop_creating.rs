//! A daemon stop ends a durable persist that is waiting for another
//! writer's persist lock in the creating wait (decision D-i8-6; round-nine
//! verification): the index directory holds no index content (only the
//! other writer's lock file), so the iteration resolved no index and its
//! persist waits through `acquire_unless_cancelled`, not the existing-index
//! wait `persist_wait_ends_on_stop.rs` covers. The stop closes the
//! process's persist gate, which the creating wait observes as the other
//! does. Its own test binary, because closing the gate is process-wide.

#![cfg(feature = "test-hooks")]

mod support;

use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use sqry_core::graph::unified::persistence::{
    GraphStorage, IndexWriteLock, close_persists_and_wait,
};
use sqry_core::watch::{ChangeSet, GitChangeClass};
use sqry_daemon::{DaemonError, TestCapture};

use support::DispatchHarness;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_ends_a_persist_waiting_in_the_creating_wait() {
    let h = DispatchHarness::new();
    std::fs::write(h.root.join("lib.rs"), b"pub fn a() {}\n").expect("source");
    let cap = Arc::new(TestCapture::default());
    h.dispatcher
        .install_test_capture(Arc::clone(&cap))
        .expect("capture installs once");
    // Nothing cancels the iteration's token: only the gate can end the wait.
    cap.suppress_forwarder.store(true, Ordering::Release);

    let graph_dir = GraphStorage::new(&h.root).graph_dir().to_path_buf();
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let other_writer = {
        let graph_dir = graph_dir.clone();
        std::thread::spawn(move || {
            let _held = IndexWriteLock::acquire(&graph_dir).expect("the other writer's lock");
            held_tx.send(()).expect("report the hold");
            release_rx.recv().expect("wait for the release");
        })
    };
    held_rx.recv().expect("the other writer holds the lock");

    let d = Arc::clone(&h.dispatcher);
    let k = h.key.clone();
    let changes = ChangeSet {
        changed_files: Vec::new(),
        git_state_changed: true,
        git_change_class: Some(GitChangeClass::TreeDiverged),
    };
    let run = tokio::spawn(async move { d.handle_changes(&k, changes).await });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !cap.persist_lock_contended.load(Ordering::Acquire) {
        assert!(
            !run.is_finished(),
            "the iteration ended before it waited for the other writer"
        );
        assert!(
            tokio::time::Instant::now() < deadline,
            "the rebuild's persist never reached the held lock"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // The stop: the waiting persist holds no gate ticket, so this returns.
    tokio::task::spawn_blocking(close_persists_and_wait)
        .await
        .expect("close the gate");
    let ended = tokio::time::timeout(Duration::from_secs(10), run).await;
    release_tx.send(()).expect("release the other writer");
    other_writer.join().expect("the other writer");
    let result = ended
        .expect("the persist kept waiting for the other writer's lock after the stop")
        .expect("runner task");
    assert!(
        matches!(result, Err(DaemonError::WorkspaceEvicted { .. })),
        "{result:?}"
    );
    assert!(
        !GraphStorage::new(&h.root).manifest_path().exists(),
        "the stopped rebuild published nothing"
    );
}
