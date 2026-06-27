//! Startup recovery for revision artifacts and managed worktrees.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::DaemonConfig;
use crate::error::DaemonError;

use super::{ManagedWorktreeRegistry, RevisionArtifactStore};

/// Startup recovery summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevisionRecoverySummary {
    /// Partial artifact directories removed from the revision cache.
    pub partial_artifacts_removed: Vec<PathBuf>,
    /// Repository roots whose managed worktree state was reconciled.
    pub worktree_repos_reconciled: Vec<PathBuf>,
    /// Daemon-owned worktree directories removed because Git no longer listed
    /// them.
    pub orphaned_worktree_dirs_removed: Vec<PathBuf>,
}

/// Run best-effort startup recovery using the daemon's configured roots.
///
/// # Errors
///
/// Returns [`DaemonError`] when artifact cleanup or managed-worktree
/// reconciliation fails.
pub fn recover_startup(config: &DaemonConfig) -> Result<RevisionRecoverySummary, DaemonError> {
    let store = RevisionArtifactStore::new(RevisionArtifactStore::default_cache_root());
    let mut summary = RevisionRecoverySummary {
        partial_artifacts_removed: store
            .remove_partial_artifacts()?
            .into_iter()
            .map(|entry| entry.artifact_dir)
            .collect(),
        ..RevisionRecoverySummary::default()
    };

    let registry = ManagedWorktreeRegistry::new(config.managed_worktrees.clone());
    for workspace in config
        .workspaces
        .iter()
        .filter(|workspace| !workspace.exclude)
    {
        if !workspace.path.exists() {
            continue;
        }
        let recovered = recover_managed_worktrees(&registry, &workspace.path)?;
        summary
            .worktree_repos_reconciled
            .extend(recovered.worktree_repos_reconciled);
        summary
            .orphaned_worktree_dirs_removed
            .extend(recovered.orphaned_worktree_dirs_removed);
    }
    Ok(summary)
}

/// Reconcile managed worktrees for one repository root.
///
/// # Errors
///
/// Returns [`DaemonError`] if Git worktree metadata operations or daemon-owned
/// directory cleanup fails.
pub fn recover_managed_worktrees(
    registry: &ManagedWorktreeRegistry,
    repo_root: &Path,
) -> Result<RevisionRecoverySummary, DaemonError> {
    let reconciliation = registry.reconcile(repo_root)?;
    let managed_root = registry.managed_repo_dir(repo_root)?;
    let known_paths: HashSet<_> = reconciliation
        .managed_entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect();

    registry.repair(repo_root, &[])?;
    registry.prune(repo_root)?;

    let mut summary = RevisionRecoverySummary {
        worktree_repos_reconciled: vec![repo_root.to_path_buf()],
        ..RevisionRecoverySummary::default()
    };
    if managed_root.is_dir() {
        for entry in
            fs::read_dir(&managed_root).map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to list managed worktree root during recovery: {err}"),
                path: Some(managed_root.clone()),
            })?
        {
            let entry = entry.map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to read managed worktree root during recovery: {err}"),
                path: Some(managed_root.clone()),
            })?;
            let path = entry.path();
            if path.is_dir() && !known_paths.contains(&path) {
                fs::remove_dir_all(&path).map_err(|err| {
                    DaemonError::RevisionSourceUnavailable {
                        reason: format!(
                            "failed to remove orphaned managed worktree directory: {err}"
                        ),
                        path: Some(path.clone()),
                    }
                })?;
                summary.orphaned_worktree_dirs_removed.push(path);
            }
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::TempDir;

    use crate::config::ManagedWorktreeConfig;
    use crate::workspace::revision::ManagedWorktreeRegistry;

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

    #[test]
    fn startup_artifact_recovery_removes_partial_directories() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        let partial = store
            .artifact_dir(
                "repo",
                &sqry_daemon_protocol::ArtifactId("artifact".to_owned()),
            )
            .unwrap();
        fs::create_dir_all(&partial).unwrap();
        fs::write(partial.join("graph.bin"), b"partial").unwrap();

        let removed = store.remove_partial_artifacts().unwrap();

        assert_eq!(removed.len(), 1);
        assert!(!partial.exists());
    }

    #[test]
    fn managed_worktree_recovery_removes_orphaned_managed_dirs_only() {
        let repo = repo();
        let managed = TempDir::new().unwrap();
        let registry = ManagedWorktreeRegistry::new(ManagedWorktreeConfig {
            root: Some(managed.path().to_path_buf()),
            ..ManagedWorktreeConfig::default()
        });
        let managed_root = registry.managed_repo_dir(repo.path()).unwrap();
        let orphan = managed_root.join("orphan");
        fs::create_dir_all(&orphan).unwrap();

        let summary = recover_managed_worktrees(&registry, repo.path()).unwrap();

        assert_eq!(summary.orphaned_worktree_dirs_removed, vec![orphan.clone()]);
        assert!(!orphan.exists());
    }
}
