//! Daemon-owned managed Git worktree registry.
//!
//! Managed worktrees are checkout-byte fallback and multi-agent infrastructure.
//! They are deliberately separate from raw Git object indexing, which remains
//! the preferred immutable source path.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    config::ManagedWorktreeConfig,
    error::{DaemonError, DaemonResult},
};

use super::manifest::hex_sha256;

/// Managed worktree purpose.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ManagedWorktreeKind {
    /// Detached checkout for explicit checkout-byte fallback.
    DetachedImmutable,
    /// Branch-backed worktree assigned to one agent task.
    AgentBranch,
}

/// Request to create a managed worktree.
#[derive(Debug, Clone)]
pub struct ManagedWorktreeCreateOptions {
    /// Git repository root.
    pub repo_root: PathBuf,
    /// Ref or commit to check out.
    pub git_ref: String,
    /// Managed worktree purpose.
    pub kind: ManagedWorktreeKind,
    /// Explicit branch name for an agent worktree. If absent, one is generated.
    pub branch_name: Option<String>,
    /// Agent identifier used when generating a branch name.
    pub agent_id: Option<String>,
    /// Task identifier used when generating a branch name.
    pub task_id: Option<String>,
    /// Permit detached fallback if the requested branch is already checked out.
    pub allow_detached_fallback: bool,
    /// Human-readable lock reason.
    pub lock_reason: Option<String>,
}

impl ManagedWorktreeCreateOptions {
    /// Create detached immutable fallback options.
    #[must_use]
    pub fn detached(repo_root: impl Into<PathBuf>, git_ref: impl Into<String>) -> Self {
        Self {
            repo_root: repo_root.into(),
            git_ref: git_ref.into(),
            kind: ManagedWorktreeKind::DetachedImmutable,
            branch_name: None,
            agent_id: None,
            task_id: None,
            allow_detached_fallback: false,
            lock_reason: None,
        }
    }

    /// Create branch-backed agent options.
    #[must_use]
    pub fn agent_branch(
        repo_root: impl Into<PathBuf>,
        git_ref: impl Into<String>,
        branch_name: Option<String>,
    ) -> Self {
        Self {
            repo_root: repo_root.into(),
            git_ref: git_ref.into(),
            kind: ManagedWorktreeKind::AgentBranch,
            branch_name,
            agent_id: None,
            task_id: None,
            allow_detached_fallback: false,
            lock_reason: None,
        }
    }
}

/// Registry record for one daemon-managed worktree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManagedWorktreeRecord {
    /// Daemon-local worktree id.
    pub worktree_id: String,
    /// Canonical source repository root.
    pub repo_root: PathBuf,
    /// Managed worktree path.
    pub path: PathBuf,
    /// Worktree purpose.
    pub kind: ManagedWorktreeKind,
    /// Ref requested by the caller.
    pub git_ref: String,
    /// Branch checked out for agent worktrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_name: Option<String>,
    /// Whether `git worktree lock` succeeded for this record.
    pub locked: bool,
    /// Lock reason passed to Git.
    pub lock_reason: String,
}

/// Parsed `git worktree list --porcelain -z` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeEntry {
    /// Worktree path.
    pub path: PathBuf,
    /// HEAD object id, if Git reported one.
    pub head: Option<String>,
    /// Full ref name such as `refs/heads/main`, if attached.
    pub branch: Option<String>,
    /// Whether the worktree is detached.
    pub detached: bool,
    /// Whether Git marks this entry as bare.
    pub bare: bool,
    /// Whether Git marks this worktree as locked.
    pub locked: bool,
    /// Optional lock reason.
    pub lock_reason: Option<String>,
    /// Whether Git marks this worktree as prunable.
    pub prunable: bool,
}

impl GitWorktreeEntry {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            head: None,
            branch: None,
            detached: false,
            bare: false,
            locked: false,
            lock_reason: None,
            prunable: false,
        }
    }
}

/// Startup reconciliation summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagedWorktreeReconciliation {
    /// Managed worktrees found under the daemon-managed root.
    pub managed_entries: Vec<GitWorktreeEntry>,
    /// Other Git worktrees for the repository.
    pub external_entries: Vec<GitWorktreeEntry>,
}

/// Durable managed worktree registry.
#[derive(Debug)]
pub struct ManagedWorktreeRegistry {
    config: ManagedWorktreeConfig,
    repo_locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl ManagedWorktreeRegistry {
    /// Create a registry from daemon config.
    #[must_use]
    pub fn new(config: ManagedWorktreeConfig) -> Self {
        Self {
            config,
            repo_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Default daemon cache root for managed worktrees.
    #[must_use]
    pub fn default_root() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("sqry")
            .join("managed-worktrees")
    }

    /// Effective root for daemon-managed worktrees.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.config.root.clone().unwrap_or_else(Self::default_root)
    }

    /// Generate a unique safe agent branch name using the first configured safe
    /// prefix.
    #[must_use]
    pub fn generate_agent_branch(&self, agent_id: Option<&str>, task_id: Option<&str>) -> String {
        let prefix = self
            .config
            .safe_branch_prefixes
            .first()
            .cloned()
            .unwrap_or_else(|| "sqry/agent/".to_owned());
        let agent = sanitize_component(agent_id.unwrap_or("agent"));
        let task = sanitize_component(task_id.unwrap_or("task"));
        format!("{prefix}{agent}-{task}-{}", entropy_suffix())
    }

    /// Create a managed worktree and lock it with Git.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] when repository validation, branch policy,
    /// worktree creation, or worktree locking fails.
    pub fn create(
        &self,
        options: &ManagedWorktreeCreateOptions,
    ) -> DaemonResult<ManagedWorktreeRecord> {
        let repo_root = canonical_repo_root(&options.repo_root)?;
        let repo_lock = self.repo_lock(&repo_root);
        let _guard = repo_lock.lock().map_err(|_| {
            DaemonError::Internal(anyhow::anyhow!("managed worktree repo lock poisoned"))
        })?;

        let branch_name = match options.kind {
            ManagedWorktreeKind::DetachedImmutable => None,
            ManagedWorktreeKind::AgentBranch => {
                Some(options.branch_name.clone().unwrap_or_else(|| {
                    self.generate_agent_branch(
                        options.agent_id.as_deref(),
                        options.task_id.as_deref(),
                    )
                }))
            }
        };
        let mut effective_kind = options.kind;
        let mut effective_branch = branch_name.clone();

        if let Some(branch) = &branch_name {
            match self.validate_agent_branch(&repo_root, branch) {
                Ok(()) => {}
                Err(err) if options.allow_detached_fallback => {
                    if matches!(err, DaemonError::ManagedWorktreeInUse { .. }) {
                        effective_kind = ManagedWorktreeKind::DetachedImmutable;
                        effective_branch = None;
                    } else {
                        return Err(err);
                    }
                }
                Err(err) => return Err(err),
            }
        }

        let worktree_id = build_worktree_id(
            &repo_root,
            &options.git_ref,
            effective_kind,
            effective_branch.as_deref(),
        );
        let path = self.repo_managed_dir(&repo_root)?.join(&worktree_id);
        if path.exists() {
            return Err(DaemonError::ManagedWorktreeInUse {
                worktree: path,
                reason: "managed worktree path already exists".to_owned(),
            });
        }
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root());
        fs::create_dir_all(&parent).map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to create managed worktree parent: {err}"),
            path: Some(parent),
        })?;

        if let Some(branch) = &effective_branch {
            git_status(
                &repo_root,
                &[
                    "worktree",
                    "add",
                    "-b",
                    branch,
                    path_to_git_arg(&path)?.as_str(),
                    &options.git_ref,
                ],
            )?;
        } else {
            git_status(
                &repo_root,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    path_to_git_arg(&path)?.as_str(),
                    &options.git_ref,
                ],
            )?;
        }

        let lock_reason = options
            .lock_reason
            .clone()
            .unwrap_or_else(|| match effective_kind {
                ManagedWorktreeKind::DetachedImmutable => {
                    "sqry managed detached revision worktree".to_owned()
                }
                ManagedWorktreeKind::AgentBranch => "sqry managed agent worktree".to_owned(),
            });
        self.lock_worktree(&repo_root, &path, &lock_reason)?;

        Ok(ManagedWorktreeRecord {
            worktree_id,
            repo_root,
            path,
            kind: effective_kind,
            git_ref: options.git_ref.clone(),
            branch_name: effective_branch,
            locked: true,
            lock_reason,
        })
    }

    /// Enumerate Git worktrees using `git worktree list --porcelain -z`.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if Git cannot enumerate local worktree metadata.
    pub fn list(&self, repo_root: &Path) -> DaemonResult<Vec<GitWorktreeEntry>> {
        let repo_root = canonical_repo_root(repo_root)?;
        let output = git_output(&repo_root, &["worktree", "list", "--porcelain", "-z"])?;
        Ok(parse_worktree_porcelain_z(&output))
    }

    /// Reconcile current Git worktree state against the daemon-managed root.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if Git metadata cannot be enumerated.
    pub fn reconcile(&self, repo_root: &Path) -> DaemonResult<ManagedWorktreeReconciliation> {
        let repo_root = canonical_repo_root(repo_root)?;
        let managed_root = self.repo_managed_dir(&repo_root)?;
        let entries = self.list(&repo_root)?;
        let mut summary = ManagedWorktreeReconciliation::default();
        for entry in entries {
            if entry.path.starts_with(&managed_root) {
                summary.managed_entries.push(entry);
            } else {
                summary.external_entries.push(entry);
            }
        }
        Ok(summary)
    }

    /// Managed worktree directory for a repository identity.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the repository root cannot be canonicalized
    /// or the default managed root would resolve inside the repository.
    pub fn managed_repo_dir(&self, repo_root: &Path) -> DaemonResult<PathBuf> {
        self.repo_managed_dir(repo_root)
    }

    /// Lock an active worktree with a visible reason.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if Git cannot lock the worktree.
    pub fn lock_worktree(&self, repo_root: &Path, path: &Path, reason: &str) -> DaemonResult<()> {
        git_status(
            repo_root,
            &[
                "worktree",
                "lock",
                "--reason",
                reason,
                path_to_git_arg(path)?.as_str(),
            ],
        )
    }

    /// Unlock a managed worktree if Git still knows about it.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if Git rejects the unlock command.
    pub fn unlock_worktree(&self, repo_root: &Path, path: &Path) -> DaemonResult<()> {
        git_status(
            repo_root,
            &["worktree", "unlock", path_to_git_arg(path)?.as_str()],
        )
    }

    /// Remove a managed worktree idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if the path is outside the managed root or Git
    /// cannot remove a known worktree.
    pub fn remove(&self, repo_root: &Path, path: &Path) -> DaemonResult<()> {
        let repo_root = canonical_repo_root(repo_root)?;
        let managed_root = self.repo_managed_dir(&repo_root)?;
        if !path.starts_with(&managed_root) {
            return Err(DaemonError::ManagedWorktreeInUse {
                worktree: path.to_path_buf(),
                reason: "refusing to remove worktree outside managed root".to_owned(),
            });
        }

        let repo_lock = self.repo_lock(&repo_root);
        let _guard = repo_lock.lock().map_err(|_| {
            DaemonError::Internal(anyhow::anyhow!("managed worktree repo lock poisoned"))
        })?;

        let _ = self.unlock_worktree(&repo_root, path);
        if path.exists() {
            git_status(
                &repo_root,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    path_to_git_arg(path)?.as_str(),
                ],
            )?;
        }
        Ok(())
    }

    /// Run `git worktree prune` for this repository.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if Git rejects the prune operation.
    pub fn prune(&self, repo_root: &Path) -> DaemonResult<()> {
        let repo_root = canonical_repo_root(repo_root)?;
        let repo_lock = self.repo_lock(&repo_root);
        let _guard = repo_lock.lock().map_err(|_| {
            DaemonError::Internal(anyhow::anyhow!("managed worktree repo lock poisoned"))
        })?;
        git_status(&repo_root, &["worktree", "prune"])
    }

    /// Run `git worktree repair` for this repository.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError`] if Git rejects the repair operation.
    pub fn repair(&self, repo_root: &Path, paths: &[PathBuf]) -> DaemonResult<()> {
        let repo_root = canonical_repo_root(repo_root)?;
        let repo_lock = self.repo_lock(&repo_root);
        let _guard = repo_lock.lock().map_err(|_| {
            DaemonError::Internal(anyhow::anyhow!("managed worktree repo lock poisoned"))
        })?;
        let path_args: Result<Vec<String>, DaemonError> =
            paths.iter().map(|path| path_to_git_arg(path)).collect();
        let path_args = path_args?;
        let mut args = vec!["worktree", "repair"];
        args.extend(path_args.iter().map(String::as_str));
        git_status(&repo_root, &args)
    }

    fn validate_agent_branch(&self, repo_root: &Path, branch: &str) -> DaemonResult<()> {
        validate_branch_component(branch)?;
        if self
            .config
            .protected_branches
            .iter()
            .any(|protected| branch == protected)
            || self
                .config
                .protected_branch_prefixes
                .iter()
                .any(|prefix| branch.starts_with(prefix))
        {
            return Err(DaemonError::ManagedWorktreeInUse {
                worktree: repo_root.to_path_buf(),
                reason: format!("branch {branch} is protected"),
            });
        }
        if !self
            .config
            .safe_branch_prefixes
            .iter()
            .any(|prefix| branch.starts_with(prefix))
        {
            return Err(DaemonError::ManagedWorktreeInUse {
                worktree: repo_root.to_path_buf(),
                reason: format!("branch {branch} does not use an approved automation prefix"),
            });
        }
        if branch_exists(repo_root, branch)? {
            return Err(DaemonError::ManagedWorktreeInUse {
                worktree: repo_root.to_path_buf(),
                reason: format!("branch {branch} already exists"),
            });
        }
        let checked_out_ref = format!("refs/heads/{branch}");
        if self
            .list(repo_root)?
            .iter()
            .any(|entry| entry.branch.as_deref() == Some(checked_out_ref.as_str()))
        {
            return Err(DaemonError::ManagedWorktreeInUse {
                worktree: repo_root.to_path_buf(),
                reason: format!("branch {branch} is already checked out in another worktree"),
            });
        }
        Ok(())
    }

    fn repo_managed_dir(&self, repo_root: &Path) -> DaemonResult<PathBuf> {
        let root = self.root();
        let canonical = canonical_repo_root(repo_root)?;
        if self.config.root.is_none() && root.starts_with(&canonical) {
            return Err(DaemonError::ManagedWorktreeInUse {
                worktree: root,
                reason: "default managed worktree root resolved inside repository".to_owned(),
            });
        }
        Ok(root.join(hex_sha256(canonical.to_string_lossy().as_bytes())))
    }

    fn repo_lock(&self, repo_root: &Path) -> Arc<Mutex<()>> {
        let mut locks = self
            .repo_locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locks
            .entry(repo_root.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

fn canonical_repo_root(repo_root: &Path) -> DaemonResult<PathBuf> {
    repo_root
        .canonicalize()
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to canonicalize repository root: {err}"),
            path: Some(repo_root.to_path_buf()),
        })
}

fn branch_exists(repo_root: &Path, branch: &str) -> DaemonResult<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to run git show-ref: {err}"),
            path: Some(repo_root.to_path_buf()),
        })?;
    Ok(output.status.success())
}

fn git_output(repo_root: &Path, args: &[&str]) -> DaemonResult<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|err| DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to run git {}: {err}", args.join(" ")),
            path: Some(repo_root.to_path_buf()),
        })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(DaemonError::RevisionSourceUnavailable {
            reason: format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            path: Some(repo_root.to_path_buf()),
        })
    }
}

fn git_status(repo_root: &Path, args: &[&str]) -> DaemonResult<()> {
    git_output(repo_root, args).map(|_| ())
}

fn path_to_git_arg(path: &Path) -> DaemonResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| DaemonError::RevisionSourceUnavailable {
            reason: "worktree path is not valid UTF-8 for git CLI".to_owned(),
            path: Some(path.to_path_buf()),
        })
}

fn validate_branch_component(branch: &str) -> DaemonResult<()> {
    if branch.trim().is_empty()
        || branch.starts_with('-')
        || branch.contains("..")
        || branch.contains('\\')
        || branch.split('/').any(str::is_empty)
    {
        return Err(DaemonError::ManagedWorktreeInUse {
            worktree: PathBuf::from(branch),
            reason: "branch name is not safe for managed worktree creation".to_owned(),
        });
    }
    Ok(())
}

fn build_worktree_id(
    repo_root: &Path,
    git_ref: &str,
    kind: ManagedWorktreeKind,
    branch_name: Option<&str>,
) -> String {
    let purpose = match kind {
        ManagedWorktreeKind::DetachedImmutable => "detached",
        ManagedWorktreeKind::AgentBranch => "agent",
    };
    let visible = branch_name.unwrap_or(git_ref);
    let entropy = entropy_suffix();
    let digest = hex_sha256(
        format!(
            "{}\n{git_ref}\n{purpose}\n{}\n{entropy}",
            repo_root.display(),
            branch_name.unwrap_or("")
        )
        .as_bytes(),
    );
    format!(
        "{purpose}-{}-{}",
        sanitize_component(visible),
        &digest[..12]
    )
}

fn entropy_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

fn sanitize_component(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    sanitized
        .trim_matches('-')
        .chars()
        .take(64)
        .collect::<String>()
}

fn parse_worktree_porcelain_z(bytes: &[u8]) -> Vec<GitWorktreeEntry> {
    let mut entries = Vec::new();
    let mut current: Option<GitWorktreeEntry> = None;
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            continue;
        }
        let line = String::from_utf8_lossy(field);
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(GitWorktreeEntry::new(PathBuf::from(path)));
        } else if let Some(entry) = current.as_mut() {
            if let Some(head) = line.strip_prefix("HEAD ") {
                entry.head = Some(head.to_owned());
            } else if let Some(branch) = line.strip_prefix("branch ") {
                entry.branch = Some(branch.to_owned());
            } else if line == "detached" {
                entry.detached = true;
            } else if line == "bare" {
                entry.bare = true;
            } else if let Some(reason) = line.strip_prefix("locked ") {
                entry.locked = true;
                entry.lock_reason = Some(reason.to_owned());
            } else if line == "locked" {
                entry.locked = true;
            } else if line.starts_with("prunable") {
                entry.prunable = true;
            }
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tempfile::TempDir;

    use crate::config::DEFAULT_MANAGED_WORKTREE_BRANCH_PREFIX;

    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        git(tmp.path(), &["init"]);
        fs::write(tmp.path().join("tracked.rs"), b"fn tracked() {}\n").unwrap();
        git(tmp.path(), &["add", "tracked.rs"]);
        git(tmp.path(), &["commit", "-m", "initial"]);
        tmp
    }

    fn registry(root: &Path) -> ManagedWorktreeRegistry {
        ManagedWorktreeRegistry::new(ManagedWorktreeConfig {
            root: Some(root.to_path_buf()),
            ..ManagedWorktreeConfig::default()
        })
    }

    #[test]
    fn parses_porcelain_z_with_lock_reason_and_branch() {
        let bytes = b"worktree /repo\0HEAD abc123\0branch refs/heads/main\0\0worktree /wt\0HEAD def456\0detached\0locked sqry reason\0prunable gitdir file points to non-existent location\0\0";
        let entries = parse_worktree_porcelain_z(bytes);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, PathBuf::from("/repo"));
        assert_eq!(entries[0].branch.as_deref(), Some("refs/heads/main"));
        assert!(entries[1].detached);
        assert!(entries[1].locked);
        assert_eq!(entries[1].lock_reason.as_deref(), Some("sqry reason"));
        assert!(entries[1].prunable);
    }

    #[test]
    fn generated_agent_branches_are_unique_and_prefixed() {
        let registry = ManagedWorktreeRegistry::new(ManagedWorktreeConfig::default());
        let first = registry.generate_agent_branch(Some("codex"), Some("task/one"));
        let second = registry.generate_agent_branch(Some("codex"), Some("task/one"));

        assert!(first.starts_with(DEFAULT_MANAGED_WORKTREE_BRANCH_PREFIX));
        assert!(second.starts_with(DEFAULT_MANAGED_WORKTREE_BRANCH_PREFIX));
        assert_ne!(first, second);
        assert!(!first.contains("task/one"));
    }

    #[test]
    fn rejects_protected_and_unapproved_agent_branches() {
        let tmp = repo();
        let managed_root = TempDir::new().unwrap();
        let registry = registry(managed_root.path());

        let protected =
            ManagedWorktreeCreateOptions::agent_branch(tmp.path(), "HEAD", Some("main".to_owned()));
        assert!(matches!(
            registry.create(&protected),
            Err(DaemonError::ManagedWorktreeInUse { .. })
        ));

        let unapproved = ManagedWorktreeCreateOptions::agent_branch(
            tmp.path(),
            "HEAD",
            Some("feature/not-approved".to_owned()),
        );
        assert!(matches!(
            registry.create(&unapproved),
            Err(DaemonError::ManagedWorktreeInUse { .. })
        ));
    }

    #[test]
    fn creates_locks_lists_reconciles_and_removes_detached_worktree() {
        let tmp = repo();
        let managed_root = TempDir::new().unwrap();
        let registry = registry(managed_root.path());
        let record = registry
            .create(&ManagedWorktreeCreateOptions::detached(tmp.path(), "HEAD"))
            .unwrap();

        assert!(record.path.exists());
        assert_eq!(record.kind, ManagedWorktreeKind::DetachedImmutable);
        assert!(record.locked);
        let entries = registry.list(tmp.path()).unwrap();
        assert!(
            entries
                .iter()
                .any(|entry| entry.path == record.path && entry.locked)
        );
        let reconciliation = registry.reconcile(tmp.path()).unwrap();
        assert!(
            reconciliation
                .managed_entries
                .iter()
                .any(|entry| entry.path == record.path)
        );

        registry.remove(tmp.path(), &record.path).unwrap();
        assert!(!record.path.exists());
    }

    #[test]
    fn branch_backed_agent_worktree_uses_unique_branch_and_refuses_reuse() {
        let tmp = repo();
        let managed_root = TempDir::new().unwrap();
        let registry = registry(managed_root.path());
        let branch = format!("{DEFAULT_MANAGED_WORKTREE_BRANCH_PREFIX}task-a");
        let record = registry
            .create(&ManagedWorktreeCreateOptions::agent_branch(
                tmp.path(),
                "HEAD",
                Some(branch.clone()),
            ))
            .unwrap();

        assert_eq!(record.kind, ManagedWorktreeKind::AgentBranch);
        assert_eq!(record.branch_name.as_deref(), Some(branch.as_str()));

        let reuse = registry.create(&ManagedWorktreeCreateOptions::agent_branch(
            tmp.path(),
            "HEAD",
            Some(branch),
        ));
        assert!(matches!(
            reuse,
            Err(DaemonError::ManagedWorktreeInUse { .. })
        ));

        registry.remove(tmp.path(), &record.path).unwrap();
    }

    #[test]
    fn detached_fallback_is_explicit_when_agent_branch_is_unavailable() {
        let tmp = repo();
        let managed_root = TempDir::new().unwrap();
        let registry = registry(managed_root.path());
        let branch = format!("{DEFAULT_MANAGED_WORKTREE_BRANCH_PREFIX}task-b");
        let first = registry
            .create(&ManagedWorktreeCreateOptions::agent_branch(
                tmp.path(),
                "HEAD",
                Some(branch.clone()),
            ))
            .unwrap();
        let mut fallback =
            ManagedWorktreeCreateOptions::agent_branch(tmp.path(), "HEAD", Some(branch));
        fallback.allow_detached_fallback = true;
        let second = registry.create(&fallback).unwrap();

        assert_eq!(second.kind, ManagedWorktreeKind::DetachedImmutable);
        assert_eq!(second.branch_name, None);

        registry.remove(tmp.path(), &second.path).unwrap();
        registry.remove(tmp.path(), &first.path).unwrap();
    }

    #[test]
    fn remove_refuses_paths_outside_managed_root() {
        let tmp = repo();
        let managed_root = TempDir::new().unwrap();
        let registry = registry(managed_root.path());
        let err = registry.remove(tmp.path(), tmp.path()).unwrap_err();

        assert!(matches!(err, DaemonError::ManagedWorktreeInUse { .. }));
    }

    #[test]
    fn git_command_source_does_not_use_stash() {
        let source = include_str!("worktree_registry.rs");
        assert!(!source.contains("\"stash\""));
    }

    #[test]
    fn branch_names_reject_escape_shapes() {
        assert!(validate_branch_component("sqry/agent/good").is_ok());
        assert!(validate_branch_component("-bad").is_err());
        assert!(validate_branch_component("bad..branch").is_err());
        assert!(validate_branch_component("bad//branch").is_err());
        assert!(validate_branch_component("bad\\branch").is_err());
    }

    #[test]
    fn file_writes_in_agent_worktrees_are_isolated() {
        let tmp = repo();
        let managed_root = TempDir::new().unwrap();
        let registry = registry(managed_root.path());
        let mut first = ManagedWorktreeCreateOptions::agent_branch(tmp.path(), "HEAD", None);
        first.agent_id = Some("codex".to_owned());
        first.task_id = Some("one".to_owned());
        let first = registry.create(&first).unwrap();
        let mut second = ManagedWorktreeCreateOptions::agent_branch(tmp.path(), "HEAD", None);
        second.agent_id = Some("codex".to_owned());
        second.task_id = Some("two".to_owned());
        let second = registry.create(&second).unwrap();

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(first.path.join("tracked.rs"))
            .unwrap();
        file.write_all(b"// first only\n").unwrap();

        let first_bytes = fs::read(first.path.join("tracked.rs")).unwrap();
        let second_bytes = fs::read(second.path.join("tracked.rs")).unwrap();
        assert_ne!(first_bytes, second_bytes);

        registry.remove(tmp.path(), &second.path).unwrap();
        registry.remove(tmp.path(), &first.path).unwrap();
    }
}
