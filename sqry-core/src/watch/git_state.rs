//! Git-state watcher for the `sqryd` daemon.
//!
//! Watches `.git/` internals (`HEAD`, `refs/heads/`, `packed-refs`, `index`)
//! and classifies observed changes into one of four categories so the daemon
//! can make correct rebuild decisions:
//!
//! - [`GitChangeClass::BranchSwitch`] — the current symbolic ref changed
//!   (checkout to a different branch or detached HEAD).
//! - [`GitChangeClass::TreeDiverged`] — the current ref still points to the
//!   same branch name, but `HEAD^{tree}` no longer matches the last indexed
//!   tree OID (pull, reset, fast-forward to a remote head, etc.).
//! - [`GitChangeClass::LocalCommit`] — the current ref advanced but the tree
//!   is unchanged (e.g. `git commit --amend` with no content change, or a
//!   commit whose working-tree edits were already observed by the source-tree
//!   watcher and accounted for).
//! - [`GitChangeClass::Noise`] — packed-refs rewrite, index bookkeeping, gc
//!   loose-object churn, reflog appends, or any other change that leaves both
//!   the current ref name and the tree OID unchanged.
//!
//! Only `BranchSwitch` and `TreeDiverged` signal a full rebuild. `LocalCommit`
//! and `Noise` are reported for telemetry / tracing but do not trigger any
//! rebuild by themselves — a commit that modified the working tree was
//! already handled by the source-tree watcher when the edit occurred.
//!
//! # Gitdir resolution
//!
//! Worktrees and submodules use a plain file named `.git` containing a
//! single `gitdir: <path>` line pointing at the real gitdir. [`GitStateWatcher::new`]
//! resolves this transparently before attaching the underlying notify watcher.
//!
//! # Fallback
//!
//! On any uncertainty (failed `git rev-parse`, unreadable `HEAD`, missing
//! refs, etc.) [`GitStateWatcher::classify`] returns [`GitChangeClass::BranchSwitch`]
//! — the "fall back to full rebuild" rule from the sqryd daemon design
//! amendment (2026-04-09 §B). Correctness over optimization.

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// Snapshot of the git state at the moment the workspace was last indexed.
///
/// This is the authoritative reference point for [`GitStateWatcher::classify`].
/// It stores the symbolic ref name (or `None` for detached HEAD), the commit
/// OID the ref resolved to, and the tree OID that commit pointed at.
///
/// Tracking the commit OID in addition to the tree OID lets the classifier
/// distinguish [`GitChangeClass::LocalCommit`] (ref moved, tree identical)
/// from [`GitChangeClass::Noise`] (nothing substantive changed).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LastIndexedGitState {
    /// Symbolic reference name (e.g. `refs/heads/main`), or `None` if HEAD
    /// is detached.
    pub head_ref: Option<String>,
    /// Commit OID that `HEAD` resolved to at indexing time.
    pub head_commit_oid: Option<String>,
    /// Tree OID corresponding to `HEAD^{tree}` at indexing time.
    pub head_tree_oid: Option<String>,
}

/// Classification of a detected `.git/` change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitChangeClass {
    /// The current symbolic ref changed (checkout to another branch or
    /// detached HEAD). Signals a full rebuild.
    BranchSwitch,
    /// The current ref name is unchanged but the tree OID changed
    /// (pull, reset, fast-forward, force-push landing, etc.). Signals a
    /// full rebuild.
    TreeDiverged,
    /// The current ref advanced (commit OID changed) but the tree OID is
    /// identical to the last indexed state. No rebuild.
    LocalCommit,
    /// Neither the ref name, nor the commit OID, nor the tree OID changed.
    /// Packed-refs rewrite, index bookkeeping, reflog append, gc churn,
    /// staging operations, etc. No rebuild.
    Noise,
}

impl GitChangeClass {
    /// Returns `true` if this classification requires a full rebuild.
    #[must_use]
    pub fn requires_full_rebuild(self) -> bool {
        matches!(self, Self::BranchSwitch | Self::TreeDiverged)
    }
}

/// Watches `.git/` internals for state changes relevant to indexing.
///
/// The watcher attaches to the resolved gitdir (supporting the `.git`-file
/// worktree layout) and buffers notify events in a channel. Callers drain
/// the channel with [`Self::poll_changed`] and then re-classify the current
/// state against their last-indexed snapshot via [`Self::classify`].
pub struct GitStateWatcher {
    /// Underlying notify watcher (kept alive for the lifetime of `Self`).
    _watcher: RecommendedWatcher,
    /// Channel for receiving raw notify events.
    receiver: Receiver<Result<Event, notify::Error>>,
    /// Absolute path to the repository working tree root.
    repo_root: PathBuf,
    /// Absolute path to the resolved gitdir (may be outside `repo_root`
    /// for worktrees / submodules).
    gitdir: PathBuf,
}

impl GitStateWatcher {
    /// Creates a new git-state watcher attached to the resolved gitdir for
    /// the repository at `repo_root`.
    ///
    /// Supports both the conventional `.git/` directory layout and the
    /// `.git`-file worktree/submodule layout (`gitdir: <path>`).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - `repo_root` does not contain a `.git` entry (file or directory).
    /// - The `.git` file is malformed or its target gitdir cannot be resolved.
    /// - The underlying notify watcher cannot be created or attached.
    pub fn new(repo_root: &Path) -> Result<Self> {
        let repo_root = repo_root.to_path_buf();
        let gitdir = resolve_gitdir(&repo_root)
            .with_context(|| format!("Failed to resolve gitdir at {}", repo_root.display()))?;

        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |res| {
            // Ignore send errors if the receiver has been dropped.
            let _ = tx.send(res);
        })
        .context("Failed to create git-state watcher")?;

        // Watch the gitdir recursively. This covers HEAD, refs/heads/,
        // refs/remotes/, packed-refs, index, logs/, objects/ churn, etc.
        // The classifier is responsible for separating signal from noise;
        // we intentionally over-capture at the watch layer.
        watcher
            .watch(&gitdir, RecursiveMode::Recursive)
            .with_context(|| format!("Failed to watch gitdir: {}", gitdir.display()))?;

        log::info!(
            "Git-state watcher started for repo {} (gitdir {})",
            repo_root.display(),
            gitdir.display(),
        );

        Ok(Self {
            _watcher: watcher,
            receiver: rx,
            repo_root,
            gitdir,
        })
    }

    /// Returns the resolved gitdir this watcher is attached to.
    #[must_use]
    pub fn gitdir(&self) -> &Path {
        &self.gitdir
    }

    /// Returns the repository working-tree root this watcher was constructed
    /// for.
    #[must_use]
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Drains all pending events from the underlying notify channel.
    ///
    /// Returns `true` if at least one event was observed since the last call
    /// (including error events), `false` if the channel was empty. Callers
    /// use this as a "something happened in `.git/`" trigger and then call
    /// [`Self::classify`] to interpret the change.
    #[must_use]
    pub fn poll_changed(&self) -> bool {
        let mut observed = false;
        loop {
            match self.receiver.try_recv() {
                Ok(Ok(_event)) => {
                    observed = true;
                }
                Ok(Err(e)) => {
                    log::warn!("Git-state watcher error: {e}");
                    observed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    log::error!("Git-state watcher channel disconnected");
                    break;
                }
            }
        }
        observed
    }

    /// Captures the current git state as a [`LastIndexedGitState`] snapshot.
    ///
    /// This is the snapshot callers should persist alongside an index build
    /// and pass back into [`Self::classify`] on subsequent polls.
    ///
    /// The snapshot is acquired with minimal race exposure:
    ///
    /// 1. `head_ref` is read from the `.git/HEAD` file directly (file I/O,
    ///    no subprocess fork) — this completes in microseconds.
    /// 2. `head_commit_oid` and `head_tree_oid` are obtained from a **single**
    ///    `git rev-parse HEAD HEAD^{tree}` subprocess, which resolves both
    ///    values against the same repository state.
    ///
    /// The only remaining TOCTOU window is between the file read (step 1)
    /// and the subprocess (step 2). If `HEAD` moves in that window the
    /// classifier will see a ref/commit mismatch and fall back to
    /// [`GitChangeClass::BranchSwitch`] (full rebuild), which is correct by
    /// the daemon's conservative fallback rule.
    ///
    /// On any git command failure this returns a snapshot with `None`
    /// fields, which guarantees the next [`Self::classify`] call falls back
    /// to [`GitChangeClass::BranchSwitch`] (full rebuild).
    #[must_use]
    pub fn current_state(&self) -> LastIndexedGitState {
        let head_ref = read_head_ref_from_file(&self.gitdir);
        let (head_commit_oid, head_tree_oid) = read_commit_and_tree(&self.repo_root);
        LastIndexedGitState {
            head_ref,
            head_commit_oid,
            head_tree_oid,
        }
    }

    /// Classifies the current git state against the last-indexed snapshot.
    ///
    /// Returns one of the four [`GitChangeClass`] variants according to the
    /// rules documented on the module.
    ///
    /// On any uncertainty (failed `git rev-parse`, missing fields in either
    /// the current state or the stored snapshot) this returns
    /// [`GitChangeClass::BranchSwitch`] — the conservative "fall back to a
    /// full rebuild" rule.
    #[must_use]
    pub fn classify(&self, last: &LastIndexedGitState) -> GitChangeClass {
        let current = self.current_state();

        // Uncertainty guard: if either side is missing any field we can't
        // compare against, bail out to the full-rebuild fallback.
        let (Some(current_ref), Some(current_commit), Some(current_tree)) = (
            current.head_ref.as_deref(),
            current.head_commit_oid.as_deref(),
            current.head_tree_oid.as_deref(),
        ) else {
            return GitChangeClass::BranchSwitch;
        };
        let (Some(last_ref), Some(last_commit), Some(last_tree)) = (
            last.head_ref.as_deref(),
            last.head_commit_oid.as_deref(),
            last.head_tree_oid.as_deref(),
        ) else {
            return GitChangeClass::BranchSwitch;
        };

        if current_ref != last_ref {
            return GitChangeClass::BranchSwitch;
        }
        if current_tree != last_tree {
            return GitChangeClass::TreeDiverged;
        }
        if current_commit != last_commit {
            return GitChangeClass::LocalCommit;
        }
        GitChangeClass::Noise
    }
}

/// Resolves the absolute gitdir for a working-tree root.
///
/// Handles both the conventional `<repo>/.git/` directory layout and the
/// `.git`-file layout used by worktrees and submodules (the file contains
/// a single line `gitdir: <path>` pointing at the real gitdir).
fn resolve_gitdir(repo_root: &Path) -> Result<PathBuf> {
    let dot_git = repo_root.join(".git");
    let metadata = std::fs::metadata(&dot_git)
        .with_context(|| format!("No .git entry at {}", dot_git.display()))?;
    if metadata.is_dir() {
        return Ok(dot_git);
    }
    // `.git` is a file (worktree/submodule). Parse `gitdir: <path>` line.
    let contents = std::fs::read_to_string(&dot_git)
        .with_context(|| format!("Failed to read .git file at {}", dot_git.display()))?;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("gitdir:") {
            let raw = rest.trim();
            let candidate = PathBuf::from(raw);
            let resolved = if candidate.is_absolute() {
                candidate
            } else {
                repo_root.join(candidate)
            };
            let canonical = std::fs::canonicalize(&resolved).with_context(|| {
                format!(
                    "Failed to canonicalize worktree gitdir {}",
                    resolved.display()
                )
            })?;
            return Ok(canonical);
        }
    }
    anyhow::bail!(
        "Malformed .git file at {}: missing `gitdir:` line",
        dot_git.display()
    )
}

/// Reads the `.git/HEAD` file and returns the symbolic ref it points at
/// (e.g. `refs/heads/main`), or `None` for detached HEAD / unreadable HEAD.
///
/// This is a file read, not a subprocess — intentionally fast and free of
/// the fork/exec overhead and race windows that come with shelling out.
fn read_head_ref_from_file(gitdir: &Path) -> Option<String> {
    let head_path = gitdir.join("HEAD");
    let contents = match std::fs::read_to_string(&head_path) {
        Ok(c) => c,
        Err(e) => {
            log::error!(
                "failed to read git HEAD file at {}: {e}",
                head_path.display()
            );
            return None;
        }
    };
    let trimmed = contents.trim();
    // `ref: refs/heads/main` → symbolic ref. Bare OID → detached HEAD.
    trimmed
        .strip_prefix("ref: ")
        .map(std::borrow::ToOwned::to_owned)
}

/// Runs a single `git rev-parse HEAD HEAD^{tree}` subprocess and returns
/// the (commit OID, tree OID) pair atomically. Both values are resolved
/// against the same repository state in a single process invocation.
///
/// On any failure (missing `git`, non-zero exit, unparseable output) the
/// corresponding fields are returned as `None`. Spawn failures and non-zero
/// exits are logged at distinct severity levels so daemon operators can
/// diagnose a missing `git` binary vs. a transient repository state issue.
fn read_commit_and_tree(repo_root: &Path) -> (Option<String>, Option<String>) {
    let output = match Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["rev-parse", "HEAD", "HEAD^{tree}"])
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            log::error!(
                "failed to spawn `git rev-parse` at {}: {e} \
                 — is `git` on the daemon's PATH?",
                repo_root.display()
            );
            return (None, None);
        }
    };
    if !output.status.success() {
        log::warn!(
            "`git rev-parse HEAD HEAD^{{tree}}` returned {} at {} (stderr: {})",
            output.status,
            repo_root.display(),
            String::from_utf8_lossy(&output.stderr).trim(),
        );
        return (None, None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let commit = lines.next().map(|s| s.trim().to_owned());
    let tree = lines.next().map(|s| s.trim().to_owned());
    (commit, tree)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    /// Initializes a minimal git repo in `dir` with one commit on `main`.
    /// Returns the absolute repo root.
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

    #[test]
    fn gitdir_resolves_conventional_directory_layout() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let gitdir = resolve_gitdir(tmp.path()).unwrap();
        assert_eq!(gitdir, tmp.path().join(".git"));
    }

    #[test]
    fn gitdir_resolves_worktree_dot_git_file_layout() {
        // Create a primary repo, then `git worktree add` into a separate
        // directory whose `.git` will be a file containing `gitdir: ...`.
        let primary = TempDir::new().unwrap();
        init_repo(primary.path());

        let work = TempDir::new().unwrap();
        let work_path = work.path().join("wt");
        run_git(
            primary.path(),
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                work_path.to_str().unwrap(),
            ],
        );

        // Sanity: .git in the worktree must be a file, not a directory.
        let dot_git = work_path.join(".git");
        let md = fs::metadata(&dot_git).unwrap();
        assert!(md.is_file(), ".git should be a file in the worktree");

        let resolved = resolve_gitdir(&work_path).unwrap();
        assert!(
            resolved.is_dir(),
            "resolved gitdir should be a directory: {}",
            resolved.display()
        );
        // The resolved gitdir must live under the primary repo's gitdir
        // (typically `<primary>/.git/worktrees/wt`).
        let primary_gitdir = primary.path().join(".git").join("worktrees").join("wt");
        let primary_canon = fs::canonicalize(&primary_gitdir).unwrap();
        assert_eq!(resolved, primary_canon);
    }

    #[test]
    fn watcher_attaches_to_conventional_repo() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let watcher = GitStateWatcher::new(tmp.path()).unwrap();
        assert_eq!(watcher.repo_root(), tmp.path());
        assert_eq!(watcher.gitdir(), tmp.path().join(".git"));
    }

    #[test]
    fn watcher_attaches_to_worktree_layout() {
        let primary = TempDir::new().unwrap();
        init_repo(primary.path());
        let work = TempDir::new().unwrap();
        let work_path = work.path().join("wt");
        run_git(
            primary.path(),
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                work_path.to_str().unwrap(),
            ],
        );

        let watcher = GitStateWatcher::new(&work_path).unwrap();
        assert_eq!(watcher.repo_root(), work_path);
        // gitdir resolved into the primary repo.
        assert!(
            watcher.gitdir().starts_with(primary.path()),
            "worktree gitdir should live under primary: got {}",
            watcher.gitdir().display()
        );
    }

    #[test]
    fn classify_commit_on_same_branch_without_tree_change_is_local_commit() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let watcher = GitStateWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.current_state();
        assert!(baseline.head_ref.as_deref() == Some("refs/heads/main"));

        // An empty commit advances the ref to a new commit OID but keeps
        // `HEAD^{tree}` identical. That is the canonical "ref moved, tree
        // unchanged" scenario the classifier calls LocalCommit. (We prefer
        // this over `git commit --amend --no-edit` because amending in the
        // same sub-second window can produce a byte-identical commit object
        // and thus the same OID, collapsing into Noise.)
        run_git(
            tmp.path(),
            &["commit", "-q", "--allow-empty", "-m", "empty commit"],
        );

        let class = watcher.classify(&baseline);
        assert_eq!(class, GitChangeClass::LocalCommit);
        assert!(!class.requires_full_rebuild());
    }

    /// Detached HEAD: head_ref becomes None, which triggers the uncertainty
    /// guard and falls back to BranchSwitch. This is intentional — the daemon
    /// treats detached HEAD as "cannot determine ref, assume the worst."
    #[test]
    fn classify_detached_head_falls_back_to_branch_switch() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let watcher = GitStateWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.current_state();
        assert!(baseline.head_ref.is_some());

        // Detach HEAD by checking out the commit OID directly.
        let oid = baseline.head_commit_oid.as_deref().unwrap();
        run_git(tmp.path(), &["checkout", "-q", "--detach", oid]);

        let class = watcher.classify(&baseline);
        assert_eq!(class, GitChangeClass::BranchSwitch);
        assert!(class.requires_full_rebuild());
    }

    #[test]
    fn classify_commit_that_changes_tree_is_tree_diverged() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let watcher = GitStateWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.current_state();

        fs::write(tmp.path().join("a.txt"), b"alpha-modified\n").unwrap();
        run_git(tmp.path(), &["commit", "-q", "-am", "edit alpha"]);

        let class = watcher.classify(&baseline);
        assert_eq!(class, GitChangeClass::TreeDiverged);
        assert!(class.requires_full_rebuild());
    }

    #[test]
    fn classify_checkout_to_other_branch_is_branch_switch() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let watcher = GitStateWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.current_state();

        run_git(tmp.path(), &["checkout", "-q", "-b", "other"]);

        let class = watcher.classify(&baseline);
        assert_eq!(class, GitChangeClass::BranchSwitch);
        assert!(class.requires_full_rebuild());
    }

    #[test]
    fn classify_gc_is_noise() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        // Generate a few dangling loose objects so `gc --prune=now` has
        // something to collect.
        fs::write(tmp.path().join("b.txt"), b"bravo\n").unwrap();
        run_git(tmp.path(), &["add", "b.txt"]);
        run_git(tmp.path(), &["reset", "--hard", "HEAD"]);
        let watcher = GitStateWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.current_state();
        run_git(tmp.path(), &["gc", "--quiet", "--prune=now"]);

        let class = watcher.classify(&baseline);
        assert_eq!(class, GitChangeClass::Noise);
        assert!(!class.requires_full_rebuild());
    }

    #[test]
    fn classify_staging_operations_are_noise() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let watcher = GitStateWatcher::new(tmp.path()).unwrap();
        let baseline = watcher.current_state();

        fs::write(tmp.path().join("c.txt"), b"charlie\n").unwrap();
        run_git(tmp.path(), &["add", "c.txt"]);
        run_git(tmp.path(), &["reset", "HEAD", "c.txt"]);

        let class = watcher.classify(&baseline);
        assert_eq!(class, GitChangeClass::Noise);
        assert!(!class.requires_full_rebuild());
    }

    #[test]
    fn classify_missing_baseline_fields_falls_back_to_branch_switch() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let watcher = GitStateWatcher::new(tmp.path()).unwrap();
        let empty = LastIndexedGitState::default();
        assert_eq!(watcher.classify(&empty), GitChangeClass::BranchSwitch);
    }

    #[test]
    fn poll_changed_reports_events_after_git_command() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let watcher = GitStateWatcher::new(tmp.path()).unwrap();
        // Drain any events left from setup.
        let _ = watcher.poll_changed();
        fs::write(tmp.path().join("a.txt"), b"alpha-modified\n").unwrap();
        run_git(tmp.path(), &["commit", "-q", "-am", "edit alpha"]);
        // notify is eventual-consistency on Linux inotify; wait briefly.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            watcher.poll_changed(),
            "watcher should report events after a commit touches .git/"
        );
    }
}
