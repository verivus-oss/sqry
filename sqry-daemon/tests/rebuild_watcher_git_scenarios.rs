//! Task 7 Phase 7b2 — bulk git scenario matrix (A2 §I).
//!
//! Four scenarios covering the dispatcher's behaviour under bulk git
//! operations:
//!
//! - `git checkout` across a 100-file diff → exactly 1 full rebuild.
//! - `git stash` + `git stash pop` → exactly 2 rebuilds (one per
//!   debounce window).
//! - `git gc` → 0 rebuilds (Noise-classed; filtered by the noise
//!   guard in `watch_loop_blocking`).
//! - `git commit` of previously-edited file → 0 *additional* rebuilds
//!   beyond the original edit.
//!
//! # Cross-platform note
//!
//! These tests shell out to `git` via `std::process::Command`. `git`
//! is assumed to be on `PATH` on every CI host (Linux, macOS, Windows).
//! No `git2` crate dependency is introduced.

use std::{fs, path::Path, time::Duration};

mod support;
use support::{WatcherHarness, assert_exactly_one_rebuild, assert_zero_rebuilds, run_git};

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Seed `count` files under `root` and commit them on the current branch.
/// Files are named `file_{i:03}.rs` with minimal Rust content so the
/// sqry-core parser accepts them.
fn seed_many_files(root: &Path, count: usize, message: &str) {
    for i in 0..count {
        let path = root.join(format!("file_{i:03}.rs"));
        fs::write(&path, format!("pub fn item_{i}() {{}}\n")).expect("seed file write");
    }
    run_git(root, &["add", "-A"]);
    run_git(root, &["commit", "-q", "-m", message]);
}

/// Create a new branch with a 100-file diff against the current HEAD.
/// The new branch is checked out at the end.
fn make_divergent_branch(root: &Path, branch: &str, count: usize) {
    run_git(root, &["checkout", "-q", "-b", branch]);
    seed_many_files(root, count, &format!("{branch}: 100-file seed"));
    run_git(root, &["checkout", "-q", "main"]);
}

// ---------------------------------------------------------------------------
// Scenario 1 — git checkout across 100-file diff
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_checkout_100_file_diff_triggers_one_full_rebuild() {
    // Wider debounce so a 100-file checkout + .git/HEAD switch is
    // captured in a single debounce window on slow CI hosts. With a
    // 300 ms window inotify can split the burst across two windows,
    // producing 2 rebuilds — this is a debounce-calibration issue,
    // not a correctness issue.
    let h = WatcherHarness::new_with_debounce(800).await;

    // Prepare a 100-file branch diff before the assertion window.
    make_divergent_branch(&h.root, "feature-x", 100);
    // Wait for the setup churn to settle past the debounce window.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let baseline = h.dispatcher.dispatched_count();

    assert_exactly_one_rebuild(
        &h.dispatcher,
        Duration::from_secs(15),
        // Post-settle longer than 2× debounce so a straggler
        // debounce window would definitely have fired by now.
        Duration::from_millis(2000),
        || {
            // Run git checkout SYNCHRONOUSLY (blocks the test thread,
            // not the tokio runtime worker). The watcher picks up
            // all 100 file changes + .git/HEAD change within the
            // debounce window.
            run_git(&h.root, &["checkout", "-q", "feature-x"]);
        },
    )
    .await;

    // After the rebuild fires, `last_mode` must be `Full` —
    // `BranchSwitch` signal from the classifier forces full rebuild
    // via requires_full_rebuild() path in decide_mode, OR the
    // 100-file threshold triggers the file-count path. Either way
    // the answer is Full.
    assert_eq!(
        h.dispatcher.last_mode(),
        Some(sqry_daemon::RebuildMode::Full),
        "branch switch across 100 files must select Full mode"
    );

    // Exactly 1 rebuild delta from baseline.
    assert_eq!(
        h.dispatcher.dispatched_count(),
        baseline + 1,
        "git checkout must produce exactly 1 rebuild"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2 — git stash + git stash pop
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_stash_then_pop_triggers_two_rebuilds() {
    let h = WatcherHarness::new_with_debounce(200).await;

    // Seed a modifiable file + commit it so there's something to stash.
    fs::write(h.root.join("modifiable.rs"), b"pub fn orig() {}\n").unwrap();
    run_git(&h.root, &["add", "modifiable.rs"]);
    run_git(&h.root, &["commit", "-q", "-m", "add modifiable"]);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let baseline = h.dispatcher.dispatched_count();

    // Modify the file (creates working-tree delta + triggers rebuild).
    fs::write(h.root.join("modifiable.rs"), b"pub fn changed() {}\n").unwrap();
    // Wait for the modification's debounce + dispatch to complete.
    let after_modify_deadline = std::time::Instant::now() + Duration::from_secs(3);
    while h.dispatcher.dispatched_count() == baseline {
        if std::time::Instant::now() > after_modify_deadline {
            panic!("initial modify must dispatch within 3 s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let after_modify = h.dispatcher.dispatched_count();

    // git stash — removes the working-tree delta. 1 rebuild fires.
    run_git(&h.root, &["stash"]);
    let after_stash_deadline = std::time::Instant::now() + Duration::from_secs(3);
    while h.dispatcher.dispatched_count() == after_modify {
        if std::time::Instant::now() > after_stash_deadline {
            panic!("git stash must dispatch within 3 s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let after_stash = h.dispatcher.dispatched_count();

    // Wait for the stash's debounce to fully settle so the pop is a
    // fresh debounce window.
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // git stash pop — restores the working-tree delta. Another rebuild.
    run_git(&h.root, &["stash", "pop"]);
    let after_pop_deadline = std::time::Instant::now() + Duration::from_secs(3);
    while h.dispatcher.dispatched_count() == after_stash {
        if std::time::Instant::now() > after_pop_deadline {
            panic!("git stash pop must dispatch within 3 s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let after_pop = h.dispatcher.dispatched_count();

    // Stash + pop → exactly 2 additional rebuilds beyond the
    // post-modify baseline.
    assert_eq!(
        after_pop - after_modify,
        2,
        "git stash + git stash pop must produce exactly 2 rebuilds \
         (after_modify={after_modify}, after_stash={after_stash}, after_pop={after_pop})"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3 — git gc
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_gc_triggers_zero_rebuilds() {
    let h = WatcherHarness::new_with_debounce(200).await;

    // Seed a few commits so git gc has something to repack.
    for i in 0..3 {
        let path = h.root.join(format!("gc_seed_{i}.rs"));
        fs::write(&path, format!("pub fn gc_{i}() {{}}\n")).unwrap();
        run_git(&h.root, &["add", &format!("gc_seed_{i}.rs")]);
        run_git(&h.root, &["commit", "-q", "-m", &format!("gc seed {i}")]);
    }
    // Let all setup churn settle.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    assert_zero_rebuilds(
        &h.dispatcher,
        // 3× debounce settle window for noise verification.
        Duration::from_millis(800),
        || {
            run_git(&h.root, &["gc", "--quiet"]);
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// Scenario 4 — git commit of previously-edited file
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_commit_of_previously_edited_file_triggers_zero_additional_rebuilds() {
    let h = WatcherHarness::new_with_debounce(200).await;

    // Seed a file + commit it so we can edit+commit without creating
    // a brand new file (which would be a legitimate new source).
    fs::write(h.root.join("edited.rs"), b"pub fn a() {}\n").unwrap();
    run_git(&h.root, &["add", "edited.rs"]);
    run_git(&h.root, &["commit", "-q", "-m", "edited.rs seed"]);
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Edit the file (triggers rebuild). Wait for the rebuild to
    // complete so the "original edit" has fully processed.
    let pre_edit_count = h.dispatcher.dispatched_count();
    fs::write(h.root.join("edited.rs"), b"pub fn b() {}\n").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while h.dispatcher.dispatched_count() == pre_edit_count {
        if std::time::Instant::now() > deadline {
            panic!("initial edit must dispatch within 3 s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let after_edit = h.dispatcher.dispatched_count();

    // Wait for the post-edit debounce to fully settle.
    tokio::time::sleep(Duration::from_millis(1500)).await;

    assert_zero_rebuilds(&h.dispatcher, Duration::from_millis(800), || {
        run_git(&h.root, &["add", "edited.rs"]);
        run_git(&h.root, &["commit", "-q", "-m", "commit the edit"]);
    })
    .await;

    // Total delta must be exactly 1 (the initial edit), with ZERO
    // additional from the commit. after_edit must equal current.
    let final_count = h.dispatcher.dispatched_count();
    assert_eq!(
        final_count, after_edit,
        "git commit of a previously-edited file must NOT add rebuilds \
         (after_edit={after_edit}, final={final_count})"
    );
}
