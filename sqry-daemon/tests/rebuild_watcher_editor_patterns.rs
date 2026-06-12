//! Task 7 Phase 7b2 — editor-save pattern matrix + ensure_watching
//! idempotence (A2 §I).
//!
//! Each editor family saves files differently. The watcher must
//! normalise every pattern to "exactly one logical changed file in
//! the debounced `ChangeSet`", which in turn must drive exactly one
//! rebuild dispatch. This test binary validates the end-to-end
//! watcher+dispatcher pipeline against all 5 patterns plus two
//! idempotence tests on `ensure_watching`.

use std::{fs, time::Duration};

mod support;
use support::{
    WatcherHarness, assert_exactly_one_rebuild,
    editor_patterns::{EditorSavePattern, simulate_save},
};

/// Shared test body — seed a file, invoke `simulate_save` with the
/// pattern, and assert exactly one rebuild fires within the settle
/// window.
async fn run_pattern(pattern: EditorSavePattern) {
    let h = WatcherHarness::new().await;
    let target = h.root.join("touched.rs");

    // Seed the target file (all patterns except DirectWrite require
    // an existing file to rename/back up).
    fs::write(&target, b"pub fn original() {}\n").expect("seed write");
    // Let the seed write settle beyond the debounce window so the
    // test's `simulate_save` triggers a fresh, isolated rebuild.
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_exactly_one_rebuild(
        &h.dispatcher,
        // Timeout: generous enough that a slow CI host's watcher
        // + pipeline still completes (500 ms debounce margin × 6).
        Duration::from_secs(3),
        // Post-settle: 2× debounce (200 ms) — proves no second
        // rebuild fires from an over-split atomic-save sequence.
        Duration::from_millis(400),
        || {
            simulate_save(&target, b"pub fn modified() {}\n", pattern);
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// Five patterns × 1 rebuild each
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_write_triggers_exactly_one_rebuild() {
    run_pattern(EditorSavePattern::DirectWrite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vim_atomic_rename_triggers_exactly_one_rebuild() {
    run_pattern(EditorSavePattern::VimAtomicRename).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jetbrains_atomic_save_triggers_exactly_one_rebuild() {
    run_pattern(EditorSavePattern::JetBrainsAtomicSave).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vscode_safe_save_triggers_exactly_one_rebuild() {
    run_pattern(EditorSavePattern::VscodeSafeSave).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn emacs_backup_triggers_exactly_one_rebuild() {
    run_pattern(EditorSavePattern::EmacsBackup).await;
}

// ---------------------------------------------------------------------------
// ensure_watching idempotence
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_watching_is_idempotent_for_same_key() {
    let h = WatcherHarness::new().await;
    assert_eq!(
        h.dispatcher.watchers_len(),
        1,
        "initial ensure_watching must have inserted 1 entry"
    );

    // Re-call with the same key — must be a no-op fast path.
    let ws = h.manager.lookup(&h.key).expect("workspace present");
    h.dispatcher
        .ensure_watching(&h.key, &ws, &h.root)
        .expect("second ensure_watching must succeed");

    assert_eq!(
        h.dispatcher.watchers_len(),
        1,
        "idempotent second call must NOT spawn a duplicate entry"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ensure_watching_is_race_free_under_concurrent_callers() {
    // Regression test for the 7b2 iter-0 feat review MAJOR: two
    // concurrent `ensure_watching` calls for the same WorkspaceKey
    // must produce exactly one stored entry, not two. The fix holds
    // `self.watchers` across the entire check → spawn → insert
    // sequence, serialising concurrent callers through the map's
    // parking_lot::Mutex.
    use std::sync::Arc;

    let h = WatcherHarness::new().await;

    // The harness already inserted one entry via its own
    // ensure_watching at construction time. Tear that down so we
    // exercise the "fresh spawn under contention" path.
    use std::sync::atomic::Ordering;
    use support::wait_until;
    let ws = h.manager.lookup(&h.key).expect("workspace present");
    ws.rebuild_cancelled.store(true, Ordering::Release);
    assert!(
        wait_until(|| h.dispatcher.watchers_len() == 0, Duration::from_secs(3)).await,
        "harness's initial watcher must drain before the race test"
    );
    ws.rebuild_cancelled.store(false, Ordering::Release);

    // Fire N concurrent ensure_watching calls for the same key.
    // Each task holds the workspace Arc + root PathBuf it needs.
    const N: usize = 8;
    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let dispatcher = Arc::clone(&h.dispatcher);
        let key = h.key.clone();
        let ws = Arc::clone(&ws);
        let root = h.root.clone();
        handles.push(tokio::spawn(async move {
            dispatcher.ensure_watching(&key, &ws, &root)
        }));
    }

    for h in handles {
        h.await
            .expect("task panicked")
            .expect("ensure_watching must succeed under contention");
    }

    assert_eq!(
        h.dispatcher.watchers_len(),
        1,
        "N concurrent ensure_watching calls must produce exactly 1 stored entry, \
         not N; got {} entries",
        h.dispatcher.watchers_len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_watching_prunes_finished_entry_and_respawns() {
    use std::sync::atomic::Ordering;
    use support::wait_until;

    let h = WatcherHarness::new().await;
    assert_eq!(h.dispatcher.watchers_len(), 1);

    // Force the watcher to shut down via rebuild_cancelled. Once the
    // async task drains, live=false and reap_watcher removes the
    // entry — so the map reaches size 0 after shutdown.
    let ws = h.manager.lookup(&h.key).expect("workspace present");
    ws.rebuild_cancelled.store(true, Ordering::Release);

    let reaped = wait_until(|| h.dispatcher.watchers_len() == 0, Duration::from_secs(3)).await;
    assert!(reaped, "watcher map must reach 0 after cancellation");

    // Reset the cancellation flag and re-call ensure_watching.
    // Since the old entry was reaped, this is effectively a fresh
    // spawn (not a prune-then-respawn through a live=false zombie).
    // The prune-then-respawn flow is separately exercised if the
    // reap hasn't happened yet at the time of the second call —
    // here we verify the END STATE after a complete shutdown: a
    // subsequent ensure_watching spawns a fresh pair.
    ws.rebuild_cancelled.store(false, Ordering::Release);
    h.dispatcher
        .ensure_watching(&h.key, &ws, &h.root)
        .expect("respawn after shutdown must succeed");

    assert_eq!(
        h.dispatcher.watchers_len(),
        1,
        "respawn must populate the map with a fresh entry"
    );
}
