//! Recursive source-tree watcher with `.gitignore` filtering and git state
//! composition.
//!
//! [`SourceTreeWatcher`] watches the entire project working tree for file
//! modifications and reports debounced [`ChangeSet`]s. It composes two
//! internal watchers:
//!
//! 1. A **source-tree watcher** (notify, recursive) that monitors non-ignored
//!    source files and filters out `.git/` internals, `.sqry/` artifacts,
//!    editor temporaries, and `.gitignore`-excluded paths.
//! 2. A [`GitStateWatcher`] that monitors `.git/` internals and classifies
//!    changes into [`GitChangeClass`] categories so the daemon can decide
//!    whether a full rebuild is needed.
//!
//! # Debounce strategy — sliding window
//!
//! After the first event arrives, the watcher waits for a *quiet period*: a
//! duration of silence after the most-recently-received event. If new events
//! keep arriving, the window slides forward. Once the quiet period elapses
//! with no new events, all collected events are merged and returned as a
//! single [`ChangeSet`].
//!
//! # Windows rename coalescing
//!
//! On Windows, `ReadDirectoryChangesW` reports atomic renames (used by Vim,
//! `JetBrains`, VS Code) as separate Remove + Create pairs. The debounce
//! loop includes a coalescing pass that detects a Remove immediately followed
//! by a Create for the **same canonical path** and collapses them into a
//! single logical Modify. This ensures editor save patterns normalize to
//! "exactly one changed file" across all platforms.
//!
//! # Editor temporary file filtering
//!
//! In addition to `.gitignore` rules, the watcher applies hard-coded filters
//! for common editor temporaries that may not appear in `.gitignore`:
//!
//! - Vim: `.*.swp`, `.*.swo`, `*~`
//! - Emacs: `*~`, `#*#`, `.#*`
//! - VS Code: `.bak` suffix (from safe-save rename dance)
//! - `JetBrains`: `___jb_tmp___`, `___jb_old___` suffixes
//!
//! These filters run *after* `.gitignore` matching so that a deliberate
//! `.gitignore` override (`!*.swp`) is respected.

use crate::watch::git_state::{GitChangeClass, GitStateWatcher, LastIndexedGitState};
use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

/// Result of waiting for source-tree and git-state changes.
///
/// A `ChangeSet` aggregates all non-ignored file paths that changed during a
/// single debounce window, plus the result of a git-state classification
/// against the caller's last-indexed snapshot.
#[derive(Debug, Clone)]
pub struct ChangeSet {
    /// Deduplicated, canonicalized source files that were created, modified,
    /// or deleted during the debounce window. Paths are relative to the
    /// repository root when possible, absolute otherwise.
    pub changed_files: Vec<PathBuf>,
    /// `true` if the [`GitStateWatcher`] observed at least one event in
    /// `.git/` during this window. Callers should inspect
    /// [`git_change_class`](Self::git_change_class) to decide whether a full
    /// rebuild is needed.
    pub git_state_changed: bool,
    /// Classification of the git state change (if any). `None` when
    /// `git_state_changed` is `false` or when no `LastIndexedGitState`
    /// was provided for comparison.
    pub git_change_class: Option<GitChangeClass>,
}

impl ChangeSet {
    /// Returns `true` if neither source files nor git state changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changed_files.is_empty() && !self.git_state_changed
    }

    /// Returns `true` if the git state change requires a full rebuild.
    #[must_use]
    pub fn requires_full_rebuild(&self) -> bool {
        self.git_change_class
            .is_some_and(GitChangeClass::requires_full_rebuild)
    }
}

/// Raw event collected during the debounce window before deduplication.
#[derive(Debug, Clone)]
enum RawChange {
    Create(PathBuf),
    Modify(PathBuf),
    Remove(PathBuf),
}

impl RawChange {
    fn path(&self) -> &Path {
        match self {
            Self::Create(p) | Self::Modify(p) | Self::Remove(p) => p,
        }
    }
}

/// Recursive source-tree watcher with `.gitignore` filtering and git state
/// detection.
///
/// Unlike [`super::FileWatcher`] (which watches a single directory for index
/// file invalidation), `SourceTreeWatcher` monitors the full project tree and
/// is designed for the `sqryd` daemon's rebuild loop.
pub struct SourceTreeWatcher {
    /// Underlying notify watcher for source files.
    _watcher: RecommendedWatcher,
    /// Channel for receiving source-tree file system events.
    receiver: Receiver<Result<Event, notify::Error>>,
    /// Absolute path to the repository root.
    root: PathBuf,
    /// Compiled `.gitignore` matcher.
    ignore_matcher: Gitignore,
    /// Git-state watcher (`.git/` internals).
    git_state: GitStateWatcher,
}

impl SourceTreeWatcher {
    /// Creates a new source-tree watcher rooted at `root`.
    ///
    /// The watcher recursively monitors all files under `root`, filtering out
    /// `.gitignore`-excluded paths, `.git/` internals (delegated to the
    /// internal [`GitStateWatcher`]), and common editor temporaries.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The notify watcher cannot be created or attached.
    /// - The git-state watcher cannot be created (missing `.git`).
    /// - The `.gitignore` file exists but is malformed (logged as warning,
    ///   not fatal — an empty matcher is used as fallback).
    pub fn new(root: &Path) -> Result<Self> {
        let root = std::fs::canonicalize(root)
            .with_context(|| format!("Failed to canonicalize root: {}", root.display()))?;

        // Build gitignore matcher from all .gitignore files in the tree.
        let ignore_matcher = build_gitignore_matcher(&root);

        // Source-tree notify watcher.
        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .context("Failed to create source-tree watcher")?;

        watcher
            .watch(&root, RecursiveMode::Recursive)
            .with_context(|| format!("Failed to watch source tree: {}", root.display()))?;

        // Git-state watcher.
        let git_state = GitStateWatcher::new(&root)
            .with_context(|| format!("Failed to create git-state watcher at {}", root.display()))?;

        log::info!("SourceTreeWatcher started for: {}", root.display());

        Ok(Self {
            _watcher: watcher,
            receiver: rx,
            root,
            ignore_matcher,
            git_state,
        })
    }

    /// Returns the repository root this watcher is monitoring.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns a reference to the internal [`GitStateWatcher`].
    #[must_use]
    pub fn git_state(&self) -> &GitStateWatcher {
        &self.git_state
    }

    /// Blocking wait for changes with sliding-window debounce.
    ///
    /// Blocks until at least one relevant event arrives, then continues
    /// collecting events until `debounce` elapses with no new events.
    /// Returns a [`ChangeSet`] with all changes observed during the window.
    ///
    /// If `last_git_state` is provided, the git-state classifier runs against
    /// it and the result is stored in [`ChangeSet::git_change_class`].
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying watcher channel disconnects.
    pub fn wait_for_changes(
        &self,
        debounce: Duration,
        last_git_state: Option<&LastIndexedGitState>,
    ) -> Result<ChangeSet> {
        let mut raw_changes: Vec<RawChange> = Vec::new();

        // Block until first event.
        let first_event = self
            .receiver
            .recv()
            .context("Source-tree watcher channel disconnected")?;
        if let Ok(event) = first_event {
            collect_raw_changes(&event, &mut raw_changes);
        }

        // Sliding-window: keep draining until `debounce` of silence.
        let mut deadline = Instant::now() + debounce;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self
                .receiver
                .recv_timeout(remaining.min(Duration::from_millis(10)))
            {
                Ok(Ok(event)) => {
                    collect_raw_changes(&event, &mut raw_changes);
                    // Slide the window forward.
                    deadline = Instant::now() + debounce;
                }
                Ok(Err(e)) => {
                    log::warn!("Source-tree watcher error: {e}");
                    deadline = Instant::now() + debounce;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Check if we've passed the deadline.
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    log::error!("Source-tree watcher channel disconnected during debounce");
                    break;
                }
            }
        }

        let git_state_changed = self.git_state.poll_changed();
        Ok(self.build_changeset(raw_changes, git_state_changed, last_git_state))
    }

    /// Debounced wait for changes with cooperative cancellation.
    ///
    /// Behaves like [`Self::wait_for_changes`] but polls `cancelled` on a
    /// `cancel_poll_period` cadence so callers can terminate a blocking
    /// watcher thread without waiting for a real filesystem event. If
    /// `cancelled.load(Ordering::Acquire) == true` is observed at any
    /// checkpoint — including before the first event arrives, or during
    /// the sliding debounce window — this returns `Ok(None)` promptly,
    /// discarding any raw changes accumulated so far (the workspace is
    /// assumed to be terminating).
    ///
    /// On a non-empty debounce window completing without cancellation,
    /// returns `Ok(Some(cs))`.
    ///
    /// # Parameters
    ///
    /// - `debounce`: sliding quiet-period length (same semantics as
    ///   [`Self::wait_for_changes`]).
    /// - `last_git_state`: optional baseline for git-state
    ///   classification; `None` produces `git_change_class = None`.
    /// - `cancelled`: shared cancellation flag. Read with
    ///   `Ordering::Acquire` so writes on the evicting thread (typically
    ///   under the workspace-manager write lock) synchronise with this
    ///   reader.
    /// - `cancel_poll_period`: how often to check `cancelled` while
    ///   waiting for the first event and during the sliding window.
    ///   Production callers use ~100 ms; tests can use ~10 ms for fast
    ///   termination.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the underlying notify channel disconnects (the
    /// watcher is unrecoverable).
    pub fn wait_for_changes_cancellable(
        &self,
        debounce: Duration,
        last_git_state: Option<&LastIndexedGitState>,
        cancelled: &AtomicBool,
        cancel_poll_period: Duration,
    ) -> Result<Option<ChangeSet>> {
        let mut raw_changes: Vec<RawChange> = Vec::new();

        // Phase 1 — wait for first event while polling cancellation.
        //
        // Replace the unconditional `recv()` from `wait_for_changes`
        // with a bounded `recv_timeout` loop that checks `cancelled`
        // on every tick. Without this an evicted workspace's watcher
        // thread would sit forever in `recv()` on a quiet repo.
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Ok(None);
            }
            match self.receiver.recv_timeout(cancel_poll_period) {
                Ok(Ok(event)) => {
                    collect_raw_changes(&event, &mut raw_changes);
                    break;
                }
                Ok(Err(e)) => {
                    // Notify reported an error event. Per `wait_for_changes`
                    // semantics we log and treat as having observed
                    // something — break out of the first-event wait.
                    log::warn!("Source-tree watcher error: {e}");
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Fall through — loop iterates and checks `cancelled`.
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("Source-tree watcher channel disconnected before first event");
                }
            }
        }

        // Phase 2 — sliding-window debounce with cancellation polling.
        //
        // The existing `wait_for_changes` already uses a 10 ms inner
        // `recv_timeout` slice; we additionally check `cancelled` on
        // every slice so a late eviction still terminates quickly.
        let mut deadline = Instant::now() + debounce;
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Ok(None);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let slice = remaining
                .min(Duration::from_millis(10))
                .min(cancel_poll_period);
            match self.receiver.recv_timeout(slice) {
                Ok(Ok(event)) => {
                    collect_raw_changes(&event, &mut raw_changes);
                    deadline = Instant::now() + debounce;
                }
                Ok(Err(e)) => {
                    log::warn!("Source-tree watcher error: {e}");
                    deadline = Instant::now() + debounce;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    log::error!("Source-tree watcher channel disconnected during debounce");
                    break;
                }
            }
        }

        let git_state_changed = self.git_state.poll_changed();
        Ok(Some(self.build_changeset(
            raw_changes,
            git_state_changed,
            last_git_state,
        )))
    }

    /// Non-blocking poll for changes.
    ///
    /// Drains all pending events from the channel, applies debounce
    /// coalescing, and returns a [`ChangeSet`]. Returns `Ok(None)` if no
    /// events are pending.
    ///
    /// If `last_git_state` is provided, the git-state classifier runs against
    /// it when git events were observed.
    ///
    /// # Errors
    ///
    /// Returns an error if the watcher channel is in an unrecoverable state.
    pub fn poll_changes(
        &self,
        last_git_state: Option<&LastIndexedGitState>,
    ) -> Result<Option<ChangeSet>> {
        let mut raw_changes: Vec<RawChange> = Vec::new();

        loop {
            match self.receiver.try_recv() {
                Ok(Ok(event)) => {
                    collect_raw_changes(&event, &mut raw_changes);
                }
                Ok(Err(e)) => {
                    log::warn!("Source-tree watcher error: {e}");
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    anyhow::bail!("Source-tree watcher channel disconnected");
                }
            }
        }

        let git_state_changed = self.git_state.poll_changed();

        if raw_changes.is_empty() && !git_state_changed {
            return Ok(None);
        }

        Ok(Some(self.build_changeset(
            raw_changes,
            git_state_changed,
            last_git_state,
        )))
    }

    /// Builds a [`ChangeSet`] from raw changes, applying gitignore filtering,
    /// editor temp filtering, `.git/` exclusion, and rename coalescing.
    ///
    /// `git_state_changed` is passed in from the caller to avoid double-draining
    /// the git-state channel (`poll_changed` drains on first call; a second call
    /// would return `false` and lose the signal).
    fn build_changeset(
        &self,
        raw_changes: Vec<RawChange>,
        git_state_changed: bool,
        last_git_state: Option<&LastIndexedGitState>,
    ) -> ChangeSet {
        // 1. Filter out .git/ paths, sqry internal artifacts, gitignored
        // paths, and editor temps.
        let filtered: Vec<RawChange> = raw_changes
            .into_iter()
            .filter(|change| {
                let path = change.path();
                !is_under_git_dir(path, &self.root)
                    && !is_under_sqry_dir(path, &self.root)
                    && !self.is_gitignored(path)
                    && !is_editor_temporary(path)
            })
            .collect();

        // 2. Coalesce Remove+Create pairs into Modify (Windows rename pattern).
        let coalesced = coalesce_rename_pairs(filtered);

        // 3. Deduplicate: keep last change per path.
        let mut deduped: HashMap<PathBuf, &RawChange> = HashMap::new();
        for change in &coalesced {
            deduped.insert(change.path().to_path_buf(), change);
        }

        let changed_files: Vec<PathBuf> = deduped.into_keys().collect();

        // 4. Classify git state if events were observed.
        let git_change_class = if git_state_changed {
            last_git_state.map(|last| self.git_state.classify(last))
        } else {
            None
        };

        ChangeSet {
            changed_files,
            git_state_changed,
            git_change_class,
        }
    }

    /// Returns `true` if the path is excluded by the `.gitignore` matcher.
    fn is_gitignored(&self, path: &Path) -> bool {
        let is_dir = path.is_dir();
        // Try to make the path relative to root for matching.
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        self.ignore_matcher
            .matched_path_or_any_parents(rel, is_dir)
            .is_ignore()
    }
}

/// Builds a [`Gitignore`] matcher by walking up from `root` and loading all
/// `.gitignore` files found. Falls back to an empty matcher on error.
fn build_gitignore_matcher(root: &Path) -> Gitignore {
    const MAX_DEPTH: usize = 20;

    let mut builder = GitignoreBuilder::new(root);

    // Load root .gitignore.
    let gitignore_path = root.join(".gitignore");
    if gitignore_path.is_file()
        && let Some(err) = builder.add(&gitignore_path)
    {
        log::warn!("Error parsing {}: {err}", gitignore_path.display());
    }

    // Walk subdirectories for nested .gitignore files (breadth-first, bounded).
    // We use a simple iterative walk to avoid pulling in another dependency.
    let mut dirs_to_scan = vec![root.to_path_buf()];
    let mut depth = 0;

    while !dirs_to_scan.is_empty() && depth < MAX_DEPTH {
        let mut next_dirs = Vec::new();
        for dir in &dirs_to_scan {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Skip .git directory itself.
                    if path.file_name().is_some_and(|n| n == ".git") {
                        continue;
                    }
                    // Check for .gitignore in this subdirectory.
                    let sub_gitignore = path.join(".gitignore");
                    if sub_gitignore.is_file()
                        && let Some(err) = builder.add(&sub_gitignore)
                    {
                        log::warn!("Error parsing {}: {err}", sub_gitignore.display());
                    }
                    next_dirs.push(path);
                }
            }
        }
        dirs_to_scan = next_dirs;
        depth += 1;
    }

    match builder.build() {
        Ok(matcher) => matcher,
        Err(e) => {
            log::warn!("Failed to build gitignore matcher: {e}; using empty matcher");
            Gitignore::empty()
        }
    }
}

/// Returns `true` if `path` is under the `.git/` directory of `root`.
fn is_under_git_dir(path: &Path, root: &Path) -> bool {
    let git_dir = root.join(".git");
    path.starts_with(&git_dir)
}

/// Returns `true` if `path` is under sqry's internal `.sqry/` directory.
fn is_under_sqry_dir(path: &Path, root: &Path) -> bool {
    let sqry_dir = root.join(".sqry");
    path.starts_with(&sqry_dir)
}

/// Returns `true` if the path looks like a common editor temporary file.
///
/// Checks file name patterns for Vim, Emacs, VS Code, and `JetBrains` editors.
fn is_editor_temporary(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    // Vim: .foo.swp, .foo.swo
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("swp") || ext.eq_ignore_ascii_case("swo"))
        && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        && stem.starts_with('.')
    {
        return true;
    }

    // Emacs backup: foo~
    if file_name.ends_with('~') {
        return true;
    }

    // Emacs auto-save: #foo#
    if file_name.starts_with('#') && file_name.ends_with('#') {
        return true;
    }

    // Emacs lock: .#foo
    if file_name.starts_with(".#") {
        return true;
    }

    // VS Code safe-save: .bak suffix on the renamed-away original
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("bak"))
    {
        return true;
    }

    // JetBrains atomic save temporaries
    if file_name.ends_with("___jb_tmp___") || file_name.ends_with("___jb_old___") {
        return true;
    }

    false
}

/// Extracts [`RawChange`] entries from a notify [`Event`].
fn collect_raw_changes(event: &Event, out: &mut Vec<RawChange>) {
    match event.kind {
        EventKind::Create(_) => {
            for path in &event.paths {
                if path.is_file() {
                    out.push(RawChange::Create(path.clone()));
                }
            }
        }
        EventKind::Modify(_) => {
            for path in &event.paths {
                // For modify events, accept even if the file doesn't exist
                // anymore (race with deletion).
                out.push(RawChange::Modify(path.clone()));
            }
        }
        EventKind::Remove(_) => {
            for path in &event.paths {
                out.push(RawChange::Remove(path.clone()));
            }
        }
        _ => {
            // Access, metadata, other — not relevant for rebuild decisions.
        }
    }
}

/// Coalesces Remove + Create pairs on the same path into a single Modify.
///
/// This handles the Windows `ReadDirectoryChangesW` pattern where an atomic
/// rename (used by Vim, `JetBrains`, VS Code) is reported as a Remove of the
/// old file followed by a Create of the new file at the same path.
///
/// The algorithm is sequential: for each Remove, it looks ahead for a Create
/// on the same path. If found, both are replaced by a single Modify. Events
/// that don't participate in a pair pass through unchanged.
///
/// This also handles Unix rename-over patterns where notify may report
/// separate Remove/Create events for the same destination path.
fn coalesce_rename_pairs(changes: Vec<RawChange>) -> Vec<RawChange> {
    if changes.len() < 2 {
        return changes;
    }

    let mut result: Vec<RawChange> = Vec::with_capacity(changes.len());
    let mut consumed: Vec<bool> = vec![false; changes.len()];

    for i in 0..changes.len() {
        if consumed[i] {
            continue;
        }

        if let RawChange::Remove(ref remove_path) = changes[i] {
            // Look ahead for a matching Create on the same path.
            let mut found_create = false;
            for j in (i + 1)..changes.len() {
                if consumed[j] {
                    continue;
                }
                if let RawChange::Create(ref create_path) = changes[j]
                    && create_path == remove_path
                {
                    // Coalesce into Modify.
                    result.push(RawChange::Modify(remove_path.clone()));
                    consumed[i] = true;
                    consumed[j] = true;
                    found_create = true;
                    break;
                }
            }
            if !found_create {
                result.push(changes[i].clone());
                consumed[i] = true;
            }
        } else {
            result.push(changes[i].clone());
            consumed[i] = true;
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::thread;
    use tempfile::TempDir;

    /// Timeout for waiting for watcher events; generous for CI.
    fn event_timeout() -> Duration {
        let base = if cfg!(target_os = "macos") {
            Duration::from_secs(3)
        } else {
            Duration::from_secs(2)
        };
        if std::env::var("CI").is_ok() {
            base * 2
        } else {
            base
        }
    }

    fn init_repo(dir: &Path) {
        run_git(dir, &["init", "-q", "-b", "main"]);
        run_git(dir, &["config", "user.email", "test@sqry.dev"]);
        run_git(dir, &["config", "user.name", "Sqry Test"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
        fs::write(dir.join("a.txt"), b"alpha\n").unwrap();
        run_git(dir, &["add", "a.txt"]);
        run_git(dir, &["commit", "-q", "-m", "initial"]);
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .expect("git command failed to launch");
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    }

    fn wait_for_poll<F>(timeout: Duration, mut predicate: F) -> bool
    where
        F: FnMut() -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if predicate() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    // -----------------------------------------------------------------------
    // Unit tests: is_editor_temporary
    // -----------------------------------------------------------------------

    #[test]
    fn editor_temp_vim_swp() {
        assert!(is_editor_temporary(Path::new("/tmp/.foo.swp")));
        assert!(is_editor_temporary(Path::new("/tmp/.foo.swo")));
        // Regular .swp without leading dot is not a Vim swap file.
        assert!(!is_editor_temporary(Path::new("/tmp/foo.swp")));
    }

    #[test]
    fn editor_temp_emacs_backup() {
        assert!(is_editor_temporary(Path::new("/tmp/foo.rs~")));
        assert!(is_editor_temporary(Path::new("/tmp/#foo.rs#")));
        assert!(is_editor_temporary(Path::new("/tmp/.#foo.rs")));
    }

    #[test]
    fn editor_temp_vscode_bak() {
        assert!(is_editor_temporary(Path::new("/tmp/foo.rs.bak")));
    }

    #[test]
    fn editor_temp_jetbrains() {
        assert!(is_editor_temporary(Path::new("/tmp/foo.rs___jb_tmp___")));
        assert!(is_editor_temporary(Path::new("/tmp/foo.rs___jb_old___")));
    }

    #[test]
    fn non_temp_files_pass_through() {
        assert!(!is_editor_temporary(Path::new("/tmp/foo.rs")));
        assert!(!is_editor_temporary(Path::new("/tmp/Makefile")));
        assert!(!is_editor_temporary(Path::new("/tmp/README.md")));
    }

    // -----------------------------------------------------------------------
    // Unit tests: is_under_git_dir
    // -----------------------------------------------------------------------

    #[test]
    fn git_dir_detection() {
        let root = Path::new("/repo");
        assert!(is_under_git_dir(Path::new("/repo/.git/HEAD"), root));
        assert!(is_under_git_dir(
            Path::new("/repo/.git/refs/heads/main"),
            root
        ));
        assert!(!is_under_git_dir(Path::new("/repo/src/main.rs"), root));
        assert!(!is_under_git_dir(Path::new("/repo/.gitignore"), root));
    }

    // -----------------------------------------------------------------------
    // Unit tests: is_under_sqry_dir
    // -----------------------------------------------------------------------

    #[test]
    fn sqry_dir_detection() {
        let root = Path::new("/repo");
        assert!(is_under_sqry_dir(
            Path::new("/repo/.sqry/graph/snapshot.sqry"),
            root
        ));
        assert!(is_under_sqry_dir(
            Path::new("/repo/.sqry/analysis/adjacency.csr"),
            root
        ));
        assert!(!is_under_sqry_dir(Path::new("/repo/src/main.rs"), root));
        assert!(!is_under_sqry_dir(Path::new("/repo/.sqry-workspace"), root));
    }

    // -----------------------------------------------------------------------
    // Unit tests: coalesce_rename_pairs
    // -----------------------------------------------------------------------

    #[test]
    fn coalesce_empty() {
        let result = coalesce_rename_pairs(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn coalesce_single_event_passthrough() {
        let changes = vec![RawChange::Modify(PathBuf::from("foo.rs"))];
        let result = coalesce_rename_pairs(changes);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], RawChange::Modify(p) if p == Path::new("foo.rs")));
    }

    #[test]
    fn coalesce_remove_create_same_path_becomes_modify() {
        let changes = vec![
            RawChange::Remove(PathBuf::from("foo.rs")),
            RawChange::Create(PathBuf::from("foo.rs")),
        ];
        let result = coalesce_rename_pairs(changes);
        assert_eq!(result.len(), 1);
        assert!(
            matches!(&result[0], RawChange::Modify(p) if p == Path::new("foo.rs")),
            "Remove+Create should coalesce into Modify"
        );
    }

    #[test]
    fn coalesce_remove_create_different_paths_no_coalesce() {
        let changes = vec![
            RawChange::Remove(PathBuf::from("old.rs")),
            RawChange::Create(PathBuf::from("new.rs")),
        ];
        let result = coalesce_rename_pairs(changes);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn coalesce_interleaved_events() {
        // Remove(a) + Modify(b) + Create(a) → Modify(a), Modify(b)
        let changes = vec![
            RawChange::Remove(PathBuf::from("a.rs")),
            RawChange::Modify(PathBuf::from("b.rs")),
            RawChange::Create(PathBuf::from("a.rs")),
        ];
        let result = coalesce_rename_pairs(changes);
        assert_eq!(result.len(), 2);
        // a.rs should be coalesced to Modify.
        assert!(
            result
                .iter()
                .any(|c| matches!(c, RawChange::Modify(p) if p == Path::new("a.rs")))
        );
        assert!(
            result
                .iter()
                .any(|c| matches!(c, RawChange::Modify(p) if p == Path::new("b.rs")))
        );
    }

    #[test]
    fn coalesce_multiple_rename_pairs() {
        let changes = vec![
            RawChange::Remove(PathBuf::from("a.rs")),
            RawChange::Remove(PathBuf::from("b.rs")),
            RawChange::Create(PathBuf::from("a.rs")),
            RawChange::Create(PathBuf::from("b.rs")),
        ];
        let result = coalesce_rename_pairs(changes);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|c| matches!(c, RawChange::Modify(_))));
    }

    // -----------------------------------------------------------------------
    // Unit tests: gitignore matching
    // -----------------------------------------------------------------------

    #[test]
    fn gitignore_filters_target_directory() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), "target/\n*.log\n").unwrap();
        let matcher = build_gitignore_matcher(tmp.path());

        assert!(
            matcher
                .matched_path_or_any_parents("target/debug/foo", false)
                .is_ignore(),
            "target/ contents should be ignored"
        );
        assert!(
            matcher
                .matched_path_or_any_parents("build.log", false)
                .is_ignore(),
            "*.log should be ignored"
        );
        assert!(
            !matcher
                .matched_path_or_any_parents("src/main.rs", false)
                .is_ignore(),
            "src/main.rs should not be ignored"
        );
    }

    #[test]
    fn gitignore_nested_rules() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(".gitignore"), "*.o\n").unwrap();
        fs::create_dir_all(tmp.path().join("vendor")).unwrap();
        fs::write(tmp.path().join("vendor/.gitignore"), "*.vendored\n").unwrap();

        let matcher = build_gitignore_matcher(tmp.path());

        assert!(
            matcher
                .matched_path_or_any_parents("foo.o", false)
                .is_ignore()
        );
        assert!(
            matcher
                .matched_path_or_any_parents("vendor/lib.vendored", false)
                .is_ignore()
        );
    }

    // -----------------------------------------------------------------------
    // Integration tests: SourceTreeWatcher
    // -----------------------------------------------------------------------

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn watcher_detects_source_file_change() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        fs::write(tmp.path().join(".gitignore"), "*.log\ntarget/\n").unwrap();
        run_git(tmp.path(), &["add", ".gitignore"]);
        run_git(tmp.path(), &["commit", "-q", "-m", "add gitignore"]);

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();

        // Give watcher time to initialize.
        thread::sleep(Duration::from_millis(100));

        // Modify a source file.
        fs::write(tmp.path().join("a.txt"), b"modified\n").unwrap();

        let detected = wait_for_poll(event_timeout(), || {
            let cs = watcher.poll_changes(None).unwrap();
            cs.is_some_and(|cs| !cs.changed_files.is_empty())
        });

        assert!(detected, "Watcher should detect source file modification");
    }

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn watcher_filters_gitignored_files() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        fs::write(tmp.path().join(".gitignore"), "*.log\ntarget/\n").unwrap();
        run_git(tmp.path(), &["add", ".gitignore"]);
        run_git(tmp.path(), &["commit", "-q", "-m", "add gitignore"]);

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();
        thread::sleep(Duration::from_millis(100));

        // Write a .log file (gitignored).
        fs::write(tmp.path().join("build.log"), b"log output\n").unwrap();

        // Also write a source file so we know the watcher is working.
        thread::sleep(Duration::from_millis(50));
        fs::write(tmp.path().join("a.txt"), b"modified\n").unwrap();

        let mut saw_log = false;
        let saw_source = wait_for_poll(event_timeout(), || {
            if let Some(cs) = watcher.poll_changes(None).unwrap() {
                for path in &cs.changed_files {
                    if path.extension().is_some_and(|e| e == "log") {
                        saw_log = true;
                    }
                }
                cs.changed_files
                    .iter()
                    .any(|p| p.file_name().is_some_and(|n| n == "a.txt"))
            } else {
                false
            }
        });

        assert!(saw_source, "Watcher should detect a.txt change");
        assert!(!saw_log, "Watcher should filter out *.log files");
    }

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn watcher_filters_editor_temporaries() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();
        thread::sleep(Duration::from_millis(100));

        // Write editor temp files.
        fs::write(tmp.path().join(".foo.swp"), b"vim swap\n").unwrap();
        fs::write(tmp.path().join("bar.rs~"), b"emacs backup\n").unwrap();
        fs::write(tmp.path().join("baz.rs.bak"), b"vscode bak\n").unwrap();

        // Also write a real source file.
        thread::sleep(Duration::from_millis(50));
        fs::write(tmp.path().join("a.txt"), b"modified\n").unwrap();

        let mut saw_temp = false;
        let saw_source = wait_for_poll(event_timeout(), || {
            if let Some(cs) = watcher.poll_changes(None).unwrap() {
                for path in &cs.changed_files {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name.ends_with(".swp") || name.ends_with('~') || name.ends_with(".bak") {
                        saw_temp = true;
                    }
                }
                cs.changed_files
                    .iter()
                    .any(|p| p.file_name().is_some_and(|n| n == "a.txt"))
            } else {
                false
            }
        });

        assert!(saw_source, "Watcher should detect a.txt change");
        assert!(!saw_temp, "Watcher should filter out editor temporaries");
    }

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn watcher_git_state_composition() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.git_state().current_state();

        // Drain initial events.
        thread::sleep(Duration::from_millis(200));
        let _ = watcher.poll_changes(None);

        // Make a commit that changes the tree.
        fs::write(tmp.path().join("a.txt"), b"changed\n").unwrap();
        run_git(tmp.path(), &["commit", "-q", "-am", "edit"]);

        thread::sleep(Duration::from_millis(300));

        // A commit that changes the tree should produce a ChangeSet.
        // Use wait_for_poll to handle event timing.
        let found = wait_for_poll(event_timeout(), || {
            if let Some(cs) = watcher.poll_changes(Some(&baseline)).unwrap() {
                // Must detect git_state_changed=true (regression: double-drain
                // used to lose this). Classification depends on whether the
                // source-file edit or the commit events arrive first, but
                // git_change_class must be set when git_state_changed is true.
                if cs.git_state_changed {
                    assert!(
                        cs.git_change_class.is_some(),
                        "git_change_class must be set when git_state_changed is true"
                    );
                    return true;
                }
                // Source-file changes without git events are also valid here.
                return !cs.changed_files.is_empty();
            }
            false
        });

        assert!(
            found,
            "Should detect changes after commit with tree modification"
        );
    }

    // -----------------------------------------------------------------------
    // ChangeSet API tests
    // -----------------------------------------------------------------------

    #[test]
    fn changeset_is_empty_when_no_changes() {
        let cs = ChangeSet {
            changed_files: vec![],
            git_state_changed: false,
            git_change_class: None,
        };
        assert!(cs.is_empty());
        assert!(!cs.requires_full_rebuild());
    }

    #[test]
    fn changeset_requires_full_rebuild_on_branch_switch() {
        let cs = ChangeSet {
            changed_files: vec![],
            git_state_changed: true,
            git_change_class: Some(GitChangeClass::BranchSwitch),
        };
        assert!(!cs.is_empty());
        assert!(cs.requires_full_rebuild());
    }

    #[test]
    fn changeset_requires_full_rebuild_on_tree_diverged() {
        let cs = ChangeSet {
            changed_files: vec![],
            git_state_changed: true,
            git_change_class: Some(GitChangeClass::TreeDiverged),
        };
        assert!(cs.requires_full_rebuild());
    }

    #[test]
    fn changeset_no_rebuild_on_local_commit() {
        let cs = ChangeSet {
            changed_files: vec![],
            git_state_changed: true,
            git_change_class: Some(GitChangeClass::LocalCommit),
        };
        assert!(!cs.requires_full_rebuild());
    }

    #[test]
    fn changeset_no_rebuild_on_noise() {
        let cs = ChangeSet {
            changed_files: vec![],
            git_state_changed: true,
            git_change_class: Some(GitChangeClass::Noise),
        };
        assert!(!cs.requires_full_rebuild());
    }

    // -----------------------------------------------------------------------
    // Git scenario tests
    // -----------------------------------------------------------------------

    #[test]
    fn classify_gc_as_noise_through_source_tree_watcher() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        // Generate loose objects.
        fs::write(tmp.path().join("b.txt"), b"bravo\n").unwrap();
        run_git(tmp.path(), &["add", "b.txt"]);
        run_git(tmp.path(), &["reset", "--hard", "HEAD"]);

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.git_state().current_state();
        // Drain setup events.
        thread::sleep(Duration::from_millis(200));
        let _ = watcher.poll_changes(None);

        run_git(tmp.path(), &["gc", "--quiet", "--prune=now"]);
        thread::sleep(Duration::from_millis(300));

        // The git state classifier should see this as Noise.
        let class = watcher.git_state().classify(&baseline);
        assert_eq!(class, GitChangeClass::Noise);
    }

    #[test]
    fn classify_staging_as_noise_through_source_tree_watcher() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.git_state().current_state();
        thread::sleep(Duration::from_millis(200));
        let _ = watcher.poll_changes(None);

        fs::write(tmp.path().join("c.txt"), b"charlie\n").unwrap();
        run_git(tmp.path(), &["add", "c.txt"]);
        run_git(tmp.path(), &["reset", "HEAD", "c.txt"]);

        let class = watcher.git_state().classify(&baseline);
        assert_eq!(class, GitChangeClass::Noise);
    }

    #[test]
    fn classify_branch_switch_through_source_tree_watcher() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.git_state().current_state();

        run_git(tmp.path(), &["checkout", "-q", "-b", "feature"]);
        let class = watcher.git_state().classify(&baseline);
        assert_eq!(class, GitChangeClass::BranchSwitch);
        assert!(class.requires_full_rebuild());
    }

    // -----------------------------------------------------------------------
    // Bulk git scenario: checkout across 100+ file diff
    // -----------------------------------------------------------------------

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn bulk_checkout_100_files_single_changeset() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());

        // Create 120 files on a feature branch.
        run_git(tmp.path(), &["checkout", "-q", "-b", "many-files"]);
        let src_dir = tmp.path().join("src");
        fs::create_dir_all(&src_dir).unwrap();
        for i in 0..120 {
            fs::write(
                src_dir.join(format!("file_{i}.rs")),
                format!("// file {i}\n"),
            )
            .unwrap();
        }
        run_git(tmp.path(), &["add", "."]);
        run_git(tmp.path(), &["commit", "-q", "-m", "add 120 files"]);

        // Switch back to main (no 120 files).
        run_git(tmp.path(), &["checkout", "-q", "main"]);

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.git_state().current_state();
        thread::sleep(Duration::from_millis(200));
        let _ = watcher.poll_changes(None);

        // Checkout back to the branch with 120 files.
        run_git(tmp.path(), &["checkout", "-q", "many-files"]);
        thread::sleep(Duration::from_millis(500));

        // Poll should yield a single changeset.
        let cs = watcher.poll_changes(Some(&baseline)).unwrap();
        assert!(cs.is_some(), "Should detect checkout across 120 files");
        let cs = cs.unwrap();

        // Git state should classify as BranchSwitch.
        if cs.git_state_changed {
            assert!(
                cs.git_change_class
                    .is_some_and(GitChangeClass::requires_full_rebuild),
                "100+ file checkout should trigger full rebuild"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Bulk git scenario: stash + pop produces 2 changesets
    // -----------------------------------------------------------------------

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn stash_pop_produces_changesets() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();
        thread::sleep(Duration::from_millis(200));
        let _ = watcher.poll_changes(None);

        // Make a working-tree change.
        fs::write(tmp.path().join("a.txt"), b"stash-me\n").unwrap();
        thread::sleep(Duration::from_millis(300));

        // Poll: first changeset (the edit).
        let cs1 = watcher.poll_changes(None).unwrap();
        assert!(
            cs1.is_some_and(|cs| !cs.changed_files.is_empty()),
            "Edit should produce first changeset"
        );

        // Stash.
        run_git(tmp.path(), &["stash"]);
        thread::sleep(Duration::from_millis(300));

        // Poll: second changeset (stash reverts working tree).
        let cs2 = watcher.poll_changes(None).unwrap();
        assert!(cs2.is_some(), "Stash should produce changeset");

        // Pop.
        run_git(tmp.path(), &["stash", "pop"]);
        thread::sleep(Duration::from_millis(300));

        // Poll: third changeset (pop restores working tree).
        let cs3 = watcher.poll_changes(None).unwrap();
        assert!(cs3.is_some(), "Stash pop should produce changeset");
    }

    // -----------------------------------------------------------------------
    // Bulk git scenario: gc produces zero relevant events
    // -----------------------------------------------------------------------

    #[test]
    fn gc_zero_source_events() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        // Create some loose objects.
        for i in 0..10 {
            fs::write(tmp.path().join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
            run_git(tmp.path(), &["add", "."]);
            run_git(tmp.path(), &["commit", "-q", "-m", &format!("commit {i}")]);
        }

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.git_state().current_state();
        thread::sleep(Duration::from_millis(200));
        let _ = watcher.poll_changes(None);

        run_git(tmp.path(), &["gc", "--quiet", "--prune=now"]);
        thread::sleep(Duration::from_millis(300));

        // Only git-state events should arrive, and classified as Noise.
        // gc may or may not produce events depending on OS-level notify
        // batching, so None is acceptable (gc produced no observed events).
        let cs = watcher.poll_changes(Some(&baseline)).unwrap();
        if let Some(cs) = cs {
            assert!(
                cs.changed_files.is_empty(),
                "gc should not produce source-file events, got: {:?}",
                cs.changed_files
            );
            // When git events ARE observed, they must classify as Noise and
            // git_state_changed must be true (regression: double-drain bug
            // used to lose this signal).
            if cs.git_state_changed {
                assert!(
                    cs.git_state_changed,
                    "git_state_changed must be true when git events observed"
                );
                assert_eq!(
                    cs.git_change_class,
                    Some(GitChangeClass::Noise),
                    "gc git events should classify as Noise"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Bulk git scenario: commit of previously-edited file — zero additional
    // -----------------------------------------------------------------------

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn commit_no_additional_changeset() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();
        thread::sleep(Duration::from_millis(200));
        let _ = watcher.poll_changes(None);

        // Edit a file — this produces the first changeset.
        fs::write(tmp.path().join("a.txt"), b"edited\n").unwrap();
        thread::sleep(Duration::from_millis(300));
        let cs1 = watcher.poll_changes(None).unwrap();
        assert!(
            cs1.is_some_and(|cs| !cs.changed_files.is_empty()),
            "Edit should produce changeset"
        );

        // Now commit the edit. The source-tree watcher should NOT produce
        // additional source-file events (git events are classified as
        // LocalCommit or TreeDiverged depending on whether the baseline
        // already captured the tree).
        let baseline = watcher.git_state().current_state();
        run_git(tmp.path(), &["add", "a.txt"]);
        run_git(tmp.path(), &["commit", "-q", "-m", "commit edit"]);
        thread::sleep(Duration::from_millis(300));

        let cs2 = watcher.poll_changes(Some(&baseline)).unwrap();
        if let Some(cs2) = cs2 {
            // Any source-file changes should be from .git/ internals that
            // leak through (should be filtered), not from a.txt itself.
            let has_source_change = cs2
                .changed_files
                .iter()
                .any(|p| p.file_name().is_some_and(|n| n == "a.txt"));
            assert!(
                !has_source_change,
                "Commit should not re-report a.txt as changed"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Regression: poll_changes must not double-drain git_state channel
    // -----------------------------------------------------------------------

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn poll_changes_reports_git_state_changed_on_git_only_events() {
        // Regression test: poll_changes() used to call git_state.poll_changed()
        // twice — once in the early-exit guard and once in build_changeset —
        // which drained the git channel on the first call and returned
        // git_state_changed=false on the second. This test ensures that a
        // pure git-state event (branch switch with no source-file edits)
        // produces a ChangeSet with git_state_changed=true.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());

        let watcher = SourceTreeWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.git_state().current_state();
        thread::sleep(Duration::from_millis(200));
        let _ = watcher.poll_changes(None); // drain init

        // Pure git operation: create and switch to a new branch.
        run_git(tmp.path(), &["checkout", "-q", "-b", "other"]);
        thread::sleep(Duration::from_millis(300));

        // poll_changes must report git_state_changed=true.
        let found = wait_for_poll(event_timeout(), || {
            if let Some(cs) = watcher.poll_changes(Some(&baseline)).unwrap()
                && cs.git_state_changed
            {
                assert!(
                    cs.git_change_class.is_some(),
                    "git_change_class must be set when git_state_changed is true"
                );
                return true;
            }
            false
        });

        assert!(
            found,
            "poll_changes must report git_state_changed=true for branch switch"
        );
    }

    // -----------------------------------------------------------------
    // wait_for_changes_cancellable — cooperative cancellation
    // -----------------------------------------------------------------

    #[test]
    fn wait_for_changes_cancellable_returns_none_on_pre_event_cancel() {
        // Cancellation observed BEFORE any filesystem event arrives.
        // The watcher must return Ok(None) within a few poll cycles
        // rather than blocking indefinitely on an empty recv().
        let tmp = TempDir::new().expect("tempdir");
        init_repo(tmp.path());

        let watcher = SourceTreeWatcher::new(tmp.path()).expect("watcher");
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));

        let cancel_signal = std::sync::Arc::clone(&cancelled);
        let handle = thread::spawn(move || {
            // Give the watcher a moment to enter the first-event wait.
            thread::sleep(Duration::from_millis(50));
            cancel_signal.store(true, Ordering::Release);
        });

        let started = Instant::now();
        let result = watcher.wait_for_changes_cancellable(
            Duration::from_secs(60), // long debounce: must NOT be reached
            None,
            &cancelled,
            Duration::from_millis(20),
        );
        let elapsed = started.elapsed();
        handle.join().unwrap();

        assert!(
            matches!(result, Ok(None)),
            "pre-event cancellation must produce Ok(None), got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "cancellation must terminate quickly; took {elapsed:?}"
        );
    }

    #[test]
    fn wait_for_changes_cancellable_returns_none_on_mid_debounce_cancel() {
        // Event arrives → watcher enters sliding debounce. Cancellation
        // observed during the debounce must still return Ok(None),
        // discarding the partial accumulation (workspace is terminating).
        let tmp = TempDir::new().expect("tempdir");
        init_repo(tmp.path());

        let watcher = SourceTreeWatcher::new(tmp.path()).expect("watcher");
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));

        // Fire one event to put the watcher into the debounce phase.
        fs::write(tmp.path().join("a.txt"), b"modified\n").unwrap();

        let cancel_signal = std::sync::Arc::clone(&cancelled);
        let handle = thread::spawn(move || {
            // Allow the watcher to enter the debounce window before
            // cancelling. 500 ms ≫ the 20 ms poll period below but ≪
            // the 60 s debounce window.
            thread::sleep(Duration::from_millis(500));
            cancel_signal.store(true, Ordering::Release);
        });

        let started = Instant::now();
        let result = watcher.wait_for_changes_cancellable(
            Duration::from_secs(60),
            None,
            &cancelled,
            Duration::from_millis(20),
        );
        let elapsed = started.elapsed();
        handle.join().unwrap();

        assert!(
            matches!(result, Ok(None)),
            "mid-debounce cancellation must produce Ok(None), got {result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "cancellation must terminate quickly; took {elapsed:?}"
        );
    }
}
