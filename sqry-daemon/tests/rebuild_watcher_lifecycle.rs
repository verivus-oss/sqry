//! The file watcher's stop signal across one `LoadedWorkspace`'s reuse
//! (integration round 7, S1, and plants P01 and P02).
//!
//! A watcher stops on its own signal, which the tombstone writers set
//! (LRU eviction, `daemon/unload`, `daemon/reset`) and which the next
//! watcher armed for the same workspace sets for the one before it. The
//! workspace object outlives the watcher: an eviction keeps the tombstone
//! in the map and the next load reuses it. Before the repair a watcher
//! whose signal was set but whose blocking loop had not yet observed it
//! (up to one 100 ms poll) still counted as live, so a reload inside that
//! window started no watcher and the reloaded workspace was left
//! unwatched once the old one exited (E4b: 44 of 50 runs).

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::workspace::builder::FunctionGraphBuilder;
use sqry_daemon::{DaemonConfig, RebuildDispatcher, WorkspaceKey, WorkspaceRosterResolver};
use support::ipc::TestServer;
use support::rebuild_fixtures::{ipc_client, status_row, wait_until_blocking};

/// A git workspace loaded through the in-memory builder and watched.
async fn watched_workspace(server: &TestServer) -> (tempfile::TempDir, PathBuf, WorkspaceKey) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = canonicalize_path(dir.path()).expect("canonical root");
    support::init_git_repo(&root);
    let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
    server
        .manager
        .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(10), 1)
        .expect("loads");
    server.dispatcher.start_watching(&key);
    assert!(
        server.dispatcher.live_watcher_keys().contains(&key),
        "the loaded workspace is watched"
    );
    (dir, root, key)
}

/// Wait on a blocking thread until `dispatcher` holds no watcher at all
/// (every task exited and was reaped), or `within` elapses.
async fn watchers_exit(dispatcher: &Arc<RebuildDispatcher>, within: Duration) -> bool {
    let dispatcher = Arc::clone(dispatcher);
    tokio::task::spawn_blocking(move || {
        wait_until_blocking(within, || dispatcher.watchers_len() == 0)
    })
    .await
    .expect("join")
}

/// E4b, deterministic form: an eviction makes the watcher stop counting as
/// watching at once (not one poll later), and a reload inside the old
/// watcher's last poll starts a new watcher, which is still watching once
/// the old one has exited. Before the repair the stopped watcher counted
/// as live until its loop noticed, so `daemon/status` reported an evicted
/// workspace as watched and a quick reload started nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reload_inside_the_stopped_watchers_last_poll_is_watched() {
    let server = TestServer::new().await;
    let (_dir, root, key) = watched_workspace(&server).await;
    let mut client = ipc_client(&server).await;

    assert!(server.manager.evict_for_test(&key), "evicts");
    let watched_after_eviction = server.dispatcher.live_watcher_keys().contains(&key);
    let row_after_eviction = status_row(&mut client, &root).await;
    server
        .manager
        .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(10), 1)
        .expect("reloads");
    server.dispatcher.start_watching(&key);
    // Longer than one poll of the old watcher (100 ms) by a wide margin,
    // so the old one has exited and been reaped (or not) by now.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let watched_later = server.dispatcher.live_watcher_keys().contains(&key);
    let row_later = status_row(&mut client, &root).await;
    println!(
        "S1 reload inside the old watcher's poll: watched right after the eviction={watched_after_eviction} \
         (status {}), 1 s after the reload={watched_later} (status {}), watchers={}",
        row_after_eviction["watching"],
        row_later["watching"],
        server.dispatcher.watchers_len()
    );
    assert!(
        !watched_after_eviction,
        "a watcher whose stop signal is set is not watching"
    );
    assert_eq!(row_after_eviction["watching"], json!(false));
    assert!(watched_later, "the reloaded workspace must be watched");
    assert_eq!(row_later["watching"], json!(true));
    assert_eq!(
        server.dispatcher.watchers_len(),
        1,
        "exactly one watcher: the old one exited, the new one runs"
    );
    drop(client);
    server.stop().await;
}

/// P01: each tombstone writer (LRU eviction, `daemon/unload`,
/// `daemon/reset`) stops the workspace's watcher, which then exits. A
/// tombstone writer that left the watcher running would leave its
/// blocking loop polling a signal nothing else sets.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_tombstone_writer_stops_the_watcher() {
    for writer in ["eviction", "unload", "reset"] {
        let server = TestServer::new().await;
        let (_dir, _root, key) = watched_workspace(&server).await;
        let ws = server.manager.lookup(&key).expect("resident");
        match writer {
            "eviction" => assert!(server.manager.evict_for_test(&key)),
            "unload" => assert!(server.manager.unload(&key)),
            _ => assert!(server.manager.reset(&key, false).expect("resets")),
        }
        let exited = watchers_exit(&server.dispatcher, Duration::from_secs(3)).await;
        println!("P01 {writer}: the watcher exited={exited}");
        // On a tree where the writer left it running, stop it so the
        // server's own shutdown is not what the assertion measures.
        ws.stop_watcher();
        assert!(exited, "{writer} must stop the workspace's watcher");
        server.stop().await;
    }
}

/// P02: arming a new watcher for a workspace stops the one attached before
/// it, so a workspace never has two watchers dispatching rebuilds. Two
/// dispatchers over one manager attach in turn; the first one's watcher
/// exits once the second attaches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_watcher_stops_the_one_attached_before_it() {
    let server = TestServer::new().await;
    let (_dir, _root, key) = watched_workspace(&server).await;
    let ws = server.manager.lookup(&key).expect("resident");
    let second = RebuildDispatcher::new(
        Arc::clone(&server.manager),
        Arc::new(DaemonConfig::default()),
        Arc::new(WorkspaceRosterResolver::new()),
    );
    second.start_watching(&key);
    assert!(
        second.live_watcher_keys().contains(&key),
        "the second dispatcher's watcher is attached"
    );
    let first_exited = watchers_exit(&server.dispatcher, Duration::from_secs(3)).await;
    let second_live = second.live_watcher_keys().contains(&key);
    println!("P02 the first watcher exited={first_exited}, the second is live={second_live}");
    assert!(
        first_exited,
        "the watcher attached first must exit once another is armed"
    );
    assert!(second_live, "the watcher armed last keeps watching");
    ws.stop_watcher();
    assert!(watchers_exit(&second, Duration::from_secs(3)).await);
    server.stop().await;
}

/// Audit S1 (D2A): an eviction that lands between a loader's publish and
/// its `start_watching` leaves a tombstone. The watcher must not start on
/// it with a clear signal, and a later `daemon/unload` or `daemon/reset`
/// of the tombstone must leave no watcher running. Before the repair the
/// watcher ran on the tombstone (`live=true`) and survived both writers,
/// because `evict_to_tombstone_locked` returned early for `Evicted`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watcher_started_after_an_eviction_does_not_run_on_the_tombstone() {
    for then in ["unload", "reset"] {
        let server = TestServer::new().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = canonicalize_path(dir.path()).expect("canonical root");
        support::init_git_repo(&root);
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        server
            .manager
            .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(10), 1)
            .expect("loads");
        // The eviction lands before the loader starts the watcher.
        assert!(server.manager.evict_for_test(&key));
        let ws = server.manager.lookup(&key).expect("tombstone");
        assert_eq!(ws.load_state(), sqry_daemon::WorkspaceState::Evicted);
        server.dispatcher.start_watching(&key);
        let live_on_tombstone = server.dispatcher.live_watcher_keys().contains(&key);
        match then {
            "unload" => assert!(server.manager.unload(&key)),
            _ => assert!(server.manager.reset(&key, true).expect("reset")),
        }
        let exited = watchers_exit(&server.dispatcher, Duration::from_secs(3)).await;
        println!(
            "D2A[{then}] live on the tombstone={live_on_tombstone} exited after {then}={exited}"
        );
        // Do not let the server's shutdown be what stops a leaked watcher.
        ws.stop_watcher();
        assert!(
            !live_on_tombstone,
            "no watcher may run on an Evicted tombstone"
        );
        assert!(exited, "{then} must leave no watcher for the root");
        assert!(
            ws.watch_wanted.load(std::sync::atomic::Ordering::Acquire),
            "the refused watcher keeps the intent for the next load"
        );
        server.stop().await;
    }
}

/// Audit S1 (D2C): the consequences of a watcher started on a tombstone.
/// (a) After the tombstone is reset, an edit must not rebuild the reset
/// workspace back to `Loaded`. (b) After an unload and a fresh load of the
/// same root, an eviction of the new workspace stops the watcher that
/// serves the root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_watcher_started_on_a_tombstone_neither_undoes_a_reset_nor_outlives_a_reload() {
    // (a) reset, then an edit.
    {
        let server = TestServer::new().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = canonicalize_path(dir.path()).expect("canonical root");
        support::init_git_repo(&root);
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        server
            .manager
            .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(10), 1)
            .expect("loads");
        assert!(server.manager.evict_for_test(&key));
        server.dispatcher.start_watching(&key);
        assert!(server.manager.reset(&key, true).expect("reset"));
        let ws = server.manager.lookup(&key).expect("entry");
        let before = server.dispatcher.dispatched_count();
        std::fs::write(root.join("lib.rs"), b"pub fn after_reset() {}\n").expect("edit");
        let dispatcher = Arc::clone(&server.dispatcher);
        let watched_ws = Arc::clone(&ws);
        let rebuilt = tokio::task::spawn_blocking(move || {
            wait_until_blocking(Duration::from_secs(4), || {
                dispatcher.dispatched_count() > before
                    || watched_ws.load_state() != sqry_daemon::WorkspaceState::Unloaded
            })
        })
        .await
        .expect("join");
        println!(
            "D2C(a) rebuilt={rebuilt} state={:?} dispatched {before} -> {}",
            ws.load_state(),
            server.dispatcher.dispatched_count()
        );
        ws.stop_watcher();
        assert!(
            !rebuilt,
            "an edit after a reset must not rebuild the reset workspace"
        );
        assert_eq!(ws.load_state(), sqry_daemon::WorkspaceState::Unloaded);
        server.stop().await;
    }
    // (b) unload, then a fresh load of the same root.
    {
        let server = TestServer::new().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let root = canonicalize_path(dir.path()).expect("canonical root");
        support::init_git_repo(&root);
        let key = WorkspaceKey::new(root.clone(), ProjectRootMode::GitRoot, 0);
        server
            .manager
            .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(10), 1)
            .expect("loads");
        assert!(server.manager.evict_for_test(&key));
        let old_ws = server.manager.lookup(&key).expect("tombstone");
        server.dispatcher.start_watching(&key);
        assert!(server.manager.unload(&key));
        server
            .manager
            .get_or_load(&key, &FunctionGraphBuilder::with_fast_path_record(10), 1)
            .expect("reloads");
        server.dispatcher.start_watching(&key);
        let new_ws = server.manager.lookup(&key).expect("new entry");
        assert!(
            !Arc::ptr_eq(&old_ws, &new_ws),
            "the reload made a new workspace"
        );
        assert!(
            server.dispatcher.live_watcher_keys().contains(&key),
            "the reloaded workspace is watched"
        );
        assert!(server.manager.evict_for_test(&key));
        let exited = watchers_exit(&server.dispatcher, Duration::from_secs(3)).await;
        println!("D2C(b) after evicting the new workspace: exited={exited}");
        old_ws.stop_watcher();
        assert!(
            exited,
            "evicting the reloaded workspace must stop the root's watcher"
        );
        server.stop().await;
    }
}
