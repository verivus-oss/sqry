//! Task 7 Phase 7b2: bulk git scenario matrix (A2 §I).
//!
//! Four scenarios covering the dispatcher's behaviour under bulk git
//! operations:
//!
//! - `git checkout` across a 100-file diff: exactly 1 full rebuild.
//! - `git stash`, then `git stash pop`: 1 rebuild each.
//! - `git gc`: 0 rebuilds of its own (it changes no source file, and a
//!   window of `.git` events alone is filtered by the noise guard in
//!   `watch_loop_blocking`).
//! - `git commit` of a previously edited file: 0 *additional* rebuilds,
//!   for the same reason.
//!
//! # Determinism (integration round 7, DAEMON_FOLLOWUP)
//!
//! The scenarios used to count rebuilds against wall-clock windows (a
//! fixed sleep to let setup churn settle, then a fixed settle after the
//! operation), so under load a slow setup rebuild landed after the
//! baseline, or a burst straddled two debounce windows, and the counts
//! were off. They now synchronise on the watcher itself:
//!
//! - **Drain.** A sentinel file is written and the test waits until a
//!   rebuild consumed it and the runner is idle. inotify delivers events
//!   in order, so every event the earlier activity caused was consumed
//!   with or before the sentinel.
//! - **Hold.** Before the operation the runner is held at the test gate
//!   inside a rebuild a direct caller asked for (not the watcher's own:
//!   the watcher's task runs a rebuild it starts inline, so it would stop
//!   receiving), so every change set the watcher sends while the operation
//!   runs parks in the rebuild lane, where watcher enqueues merge into one
//!   entry. A second sentinel written
//!   after the operation (whose git process has exited, so all its events
//!   are queued before it) proves the entry holds everything once it
//!   appears in the lane. Released, the runner runs exactly one iteration
//!   for that entry, and the scenario asserts on its change set: what the
//!   operation contributed, and how the iteration ran.
//!
//! Every wait is a bounded poll on that state (60 s), never a sleep.
//!
//! # Cross-platform note
//!
//! These tests shell out to `git` via `std::process::Command`. `git`
//! is assumed to be on `PATH` on every CI host (Linux, macOS, Windows).
//! No `git2` crate dependency is introduced.

#![cfg(feature = "test-hooks")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

mod support;
use sqry_daemon::{CapturedIteration, RebuildMode, TestCapture, TestGate};
use support::{WatcherHarness, run_git, wait_until};

const WAIT: Duration = Duration::from_secs(60);

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

/// The watcher harness with a capture and a gate installed.
struct Scenario {
    h: WatcherHarness,
    capture: Arc<TestCapture>,
    gate: Arc<TestGate>,
    sentinels: usize,
}

fn names(changed: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = changed
        .iter()
        .filter_map(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names
}

impl Scenario {
    async fn new(debounce_ms: u64) -> Self {
        let h = WatcherHarness::new_with_debounce(debounce_ms).await;
        let capture = Arc::new(TestCapture::new());
        let gate = Arc::new(TestGate {
            hold: AtomicUsize::new(0),
            release: tokio::sync::Notify::new(),
        });
        h.dispatcher
            .install_test_capture(Arc::clone(&capture))
            .expect("capture installs");
        h.dispatcher
            .install_test_gate(Arc::clone(&gate))
            .expect("gate installs");
        Self {
            h,
            capture,
            gate,
            sentinels: 0,
        }
    }

    fn root(&self) -> &Path {
        &self.h.root
    }

    /// Write a fresh sentinel source file and return its name.
    fn sentinel(&mut self) -> String {
        self.sentinels += 1;
        let name = format!("zz_sentinel_{}.rs", self.sentinels);
        fs::write(
            self.root().join(&name),
            format!("pub fn sentinel_{}() {{}}\n", self.sentinels),
        )
        .expect("sentinel write");
        name
    }

    fn iterations(&self) -> Vec<CapturedIteration> {
        self.capture.iterations.lock().clone()
    }

    fn runner_idle(&self) -> bool {
        let ws = self.h.manager.lookup(&self.h.key).expect("resident");
        !ws.rebuild_in_flight.load(Ordering::Acquire)
            && ws.rebuild_lane.try_lock().is_ok_and(|lane| lane.is_none())
    }

    /// Wait until a rebuild consumed a fresh sentinel and the runner is
    /// idle: every event caused before it has been dispatched.
    async fn drain(&mut self) {
        let sentinel = self.sentinel();
        let drained = wait_until(
            || {
                self.capture
                    .iterations
                    .lock()
                    .iter()
                    .any(|it| names(&it.changeset.changed_files).contains(&sentinel))
                    && self.runner_idle()
            },
            WAIT,
        )
        .await;
        assert!(drained, "the watcher drains up to {sentinel}");
    }

    /// Run `op` while the runner is held, and return the one iteration
    /// that consumed everything the watcher sent for it (and the closing
    /// sentinel), with that sentinel's name.
    async fn merged_iteration_for(
        &mut self,
        op: impl FnOnce(&Path),
    ) -> (CapturedIteration, String) {
        self.drain().await;
        let before = self.iterations().len();
        // A direct caller's rebuild, held at the gate: the runner role is
        // taken by a task of its own, so the watcher keeps receiving and
        // its change sets park behind it.
        self.gate.hold.store(1, Ordering::Release);
        let held_rebuild = self
            .h
            .dispatcher
            .handle_changes_with_macro_options(
                &self.h.key,
                sqry_core::watch::ChangeSet {
                    changed_files: Vec::new(),
                    git_state_changed: false,
                    git_change_class: None,
                },
                sqry_core::graph::unified::build::MacroOptionsRequest::empty(),
            )
            .await;
        let held = wait_until(|| self.capture.iterations.lock().len() > before, WAIT).await;
        assert!(held, "the runner is held in the direct caller's rebuild");

        op(self.root());
        let closing = self.sentinel();
        let ws = self.h.manager.lookup(&self.h.key).expect("resident");
        let parked = wait_until(
            || {
                ws.rebuild_lane.try_lock().is_ok_and(|lane| {
                    lane.as_ref()
                        .is_some_and(|entry| names(&entry.changes.changed_files).contains(&closing))
                })
            },
            WAIT,
        )
        .await;
        assert!(
            parked,
            "everything up to {closing} parked behind the held rebuild"
        );

        self.gate.release.notify_one();
        tokio::time::timeout(WAIT, held_rebuild.wait())
            .await
            .expect("the held rebuild answers")
            .expect("the held rebuild succeeds");
        let done = wait_until(
            || self.iterations().len() >= before + 2 && self.runner_idle(),
            WAIT,
        )
        .await;
        assert!(done, "the parked rebuild ran");
        let iterations = self.iterations();
        assert_eq!(
            iterations.len(),
            before + 2,
            "exactly one iteration consumed the parked entry: {:?}",
            iterations[before..]
                .iter()
                .map(|it| names(&it.changeset.changed_files))
                .collect::<Vec<_>>()
        );
        (iterations[before + 1].clone(), closing)
    }
}

// ---------------------------------------------------------------------------
// Scenario 1: git checkout across a 100-file diff
// ---------------------------------------------------------------------------

/// The checkout's 100 changed files and its branch switch are consumed by
/// one rebuild, which runs Full.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_checkout_100_file_diff_triggers_one_full_rebuild() {
    let mut s = Scenario::new(200).await;
    make_divergent_branch(s.root(), "feature-x", 100);

    let (merged, closing) = s
        .merged_iteration_for(|root| run_git(root, &["checkout", "-q", "feature-x"]))
        .await;
    let files = names(&merged.changeset.changed_files);
    let checked_out = files.iter().filter(|n| n.starts_with("file_")).count();
    println!(
        "checkout: one iteration, mode={:?} class={:?} checkout files={checked_out} total files={}",
        merged.mode,
        merged.changeset.git_change_class,
        files.len()
    );
    assert_eq!(
        checked_out, 100,
        "every checked-out file is in the one rebuild"
    );
    assert!(files.contains(&closing));
    assert!(
        merged.changeset.requires_full_rebuild(),
        "the branch switch is seen: {:?}",
        merged.changeset.git_change_class
    );
    assert_eq!(merged.mode, RebuildMode::Full);
}

// ---------------------------------------------------------------------------
// Scenario 2: git stash, then git stash pop
// ---------------------------------------------------------------------------

/// The edit, the stash that reverts it and the pop that restores it are
/// each consumed by one rebuild carrying the file.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_stash_then_pop_triggers_two_rebuilds() {
    let mut s = Scenario::new(200).await;
    fs::write(s.root().join("modifiable.rs"), b"pub fn orig() {}\n").unwrap();
    run_git(s.root(), &["add", "modifiable.rs"]);
    run_git(s.root(), &["commit", "-q", "-m", "add modifiable"]);

    let mut seen = Vec::new();
    for (label, op) in [
        (
            "modify",
            Box::new(|root: &Path| {
                fs::write(root.join("modifiable.rs"), b"pub fn changed() {}\n").unwrap();
            }) as Box<dyn FnOnce(&Path)>,
        ),
        ("stash", Box::new(|root: &Path| run_git(root, &["stash"]))),
        (
            "stash pop",
            Box::new(|root: &Path| run_git(root, &["stash", "pop"])),
        ),
    ] {
        let (merged, _closing) = s.merged_iteration_for(op).await;
        let files = names(&merged.changeset.changed_files);
        println!("{label}: one iteration with {files:?}");
        assert!(
            files.contains(&"modifiable.rs".to_string()),
            "{label}: the change is rebuilt: {files:?}"
        );
        seen.push(label);
    }
    assert_eq!(seen, ["modify", "stash", "stash pop"]);
}

// ---------------------------------------------------------------------------
// Scenario 3: git gc
// ---------------------------------------------------------------------------

/// `git gc` contributes no source file: the one iteration after it carries
/// the closing sentinel alone. On its own, a window with no source file is
/// never dispatched (the noise guard in `watch_loop_blocking`), so gc
/// reaches no rebuild of its own. The window the closing sentinel shares
/// with gc's last `.git` events may carry their git observation; that is
/// the sentinel's rebuild, not one gc caused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_gc_triggers_zero_rebuilds() {
    let mut s = Scenario::new(200).await;
    for i in 0..3 {
        let path = s.root().join(format!("gc_seed_{i}.rs"));
        fs::write(&path, format!("pub fn gc_{i}() {{}}\n")).unwrap();
        run_git(s.root(), &["add", &format!("gc_seed_{i}.rs")]);
        run_git(s.root(), &["commit", "-q", "-m", &format!("gc seed {i}")]);
    }

    let (merged, closing) = s
        .merged_iteration_for(|root| run_git(root, &["gc", "--quiet"]))
        .await;
    let files = names(&merged.changeset.changed_files);
    println!(
        "gc: the iteration after it carries {files:?} git_state_changed={} class={:?}",
        merged.changeset.git_state_changed, merged.changeset.git_change_class
    );
    assert_eq!(files, vec![closing], "gc contributed no source file");
}

// ---------------------------------------------------------------------------
// Scenario 4: git commit of a previously edited file
// ---------------------------------------------------------------------------

/// Committing an edit the watcher already rebuilt contributes no source
/// file: the edit is consumed by its own rebuild, and the one iteration
/// after the commit carries the closing sentinel alone. As for gc, a
/// window of `.git` events alone is never dispatched, so the commit
/// reaches no additional rebuild of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_commit_of_previously_edited_file_triggers_zero_additional_rebuilds() {
    let mut s = Scenario::new(200).await;
    fs::write(s.root().join("edited.rs"), b"pub fn a() {}\n").unwrap();
    run_git(s.root(), &["add", "edited.rs"]);
    run_git(s.root(), &["commit", "-q", "-m", "edited.rs seed"]);

    let (edit, _) = s
        .merged_iteration_for(|root| fs::write(root.join("edited.rs"), b"pub fn b() {}\n").unwrap())
        .await;
    assert!(
        names(&edit.changeset.changed_files).contains(&"edited.rs".to_string()),
        "the edit is rebuilt"
    );

    let (merged, closing) = s
        .merged_iteration_for(|root| {
            run_git(root, &["add", "edited.rs"]);
            run_git(root, &["commit", "-q", "-m", "commit the edit"]);
        })
        .await;
    let files = names(&merged.changeset.changed_files);
    println!(
        "commit: the iteration after it carries {files:?} git_state_changed={} class={:?}",
        merged.changeset.git_state_changed, merged.changeset.git_change_class
    );
    assert_eq!(
        files,
        vec![closing],
        "the commit contributed no source file"
    );
}
