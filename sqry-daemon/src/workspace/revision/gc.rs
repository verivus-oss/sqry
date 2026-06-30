//! Revision artifact garbage collection and prune planning.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use sqry_daemon_protocol::{
    ArtifactId, PruneRefusal, PruneRevisionCandidate, PruneWorktreeCandidate, ResidentHandleKind,
    RevisionStatus,
};

use crate::config::RevisionArtifactConfig;
use crate::error::DaemonError;

use super::{
    ArtifactInventoryEntry, GitWorktreeEntry, ManagedWorktreeRegistry, ResidentRevisionLoad,
    RevisionArtifactStore,
};

/// Revision artifact disk budget policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisionDiskBudgetPolicy {
    /// Global budget across all repository identity directories.
    pub global_limit_bytes: u64,
    /// Per-repository identity budget.
    pub repo_limit_bytes: u64,
}

impl RevisionDiskBudgetPolicy {
    /// Build a policy from daemon config.
    #[must_use]
    pub const fn from_config(config: &RevisionArtifactConfig) -> Self {
        Self {
            global_limit_bytes: config.max_disk_bytes,
            repo_limit_bytes: config.max_repo_disk_bytes,
        }
    }
}

/// Dry-run prune plan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevisionPrunePlan {
    /// Artifact candidates.
    pub artifact_candidates: Vec<PruneRevisionCandidate>,
    /// Managed worktree candidates.
    pub worktree_candidates: Vec<PruneWorktreeCandidate>,
    /// Protected artifacts or worktrees that were refused.
    pub refusals: Vec<PruneRefusal>,
}

/// Applied GC summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevisionGcApplySummary {
    /// Bytes reclaimed from artifact directories.
    pub reclaimed_bytes: u64,
    /// Artifact ids removed.
    pub removed_artifacts: Vec<ArtifactId>,
}

/// Plan artifact and worktree pruning without deleting anything.
///
/// # Errors
///
/// Returns [`DaemonError`] if artifact or worktree inventory fails.
pub fn plan_prune(
    store: &RevisionArtifactStore,
    statuses: &[RevisionStatus],
    protected_artifact_ids: &[ArtifactId],
    worktree_registry: &ManagedWorktreeRegistry,
    repo_roots: &[PathBuf],
) -> Result<RevisionPrunePlan, DaemonError> {
    let protected: HashSet<_> = protected_artifact_ids.iter().cloned().collect();
    let status_by_artifact: HashMap<_, _> = statuses
        .iter()
        .map(|status| (status.artifact_id.clone(), status.revision_id.clone()))
        .collect();
    let mut plan = RevisionPrunePlan::default();

    for entry in store.inventory()? {
        let is_partial = entry.is_partial();
        if protected.contains(&entry.artifact_id) {
            plan.refusals.push(PruneRefusal {
                artifact_id: Some(entry.artifact_id),
                worktree_path: None,
                reason: "artifact is pinned by an active or pinned resident handle".to_owned(),
            });
            continue;
        }
        plan.artifact_candidates.push(PruneRevisionCandidate {
            revision_id: status_by_artifact
                .get(&entry.artifact_id)
                .cloned()
                .unwrap_or_else(|| {
                    ResidentRevisionLoad::deterministic_revision_id(
                        ResidentHandleKind::ImmutableRevision,
                        &entry.artifact_id,
                    )
                }),
            artifact_id: entry.artifact_id,
            reclaimable_bytes: entry.size_bytes,
            reason: if is_partial {
                "artifact directory is partial and safe to remove".to_owned()
            } else {
                "artifact is inactive and unpinned".to_owned()
            },
        });
    }

    for repo_root in repo_roots {
        plan_managed_worktrees(worktree_registry, repo_root, &mut plan)?;
    }

    plan.artifact_candidates
        .sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));
    plan.worktree_candidates
        .sort_by(|left, right| left.path.cmp(&right.path));
    plan.refusals.sort_by(|left, right| {
        left.artifact_id
            .cmp(&right.artifact_id)
            .then_with(|| left.worktree_path.cmp(&right.worktree_path))
    });
    Ok(plan)
}

/// Enforce configured disk budgets by deleting unprotected artifacts in LRU
/// order.
///
/// # Errors
///
/// Returns [`DaemonError::RevisionDiskBudgetExceeded`] if protected artifacts
/// alone exceed a budget or if filesystem removal fails.
pub fn enforce_disk_budgets(
    store: &RevisionArtifactStore,
    policy: RevisionDiskBudgetPolicy,
    protected_artifact_ids: &[ArtifactId],
) -> Result<RevisionGcApplySummary, DaemonError> {
    let protected: HashSet<_> = protected_artifact_ids.iter().cloned().collect();
    let mut entries: Vec<_> = store
        .inventory()?
        .into_iter()
        .filter(|entry| !entry.is_partial())
        .collect();
    entries.sort_by(|left, right| {
        left.modified_at
            .cmp(&right.modified_at)
            .then_with(|| left.artifact_id.cmp(&right.artifact_id))
    });

    let mut global_bytes = total_bytes(entries.iter());
    let mut repo_bytes = repo_totals(entries.iter());
    let mut summary = RevisionGcApplySummary::default();

    for entry in entries {
        if global_bytes <= policy.global_limit_bytes
            && repo_bytes
                .values()
                .all(|bytes| *bytes <= policy.repo_limit_bytes)
        {
            break;
        }
        let repo_over = repo_bytes
            .get(&entry.repo_identity_hash)
            .is_some_and(|bytes| *bytes > policy.repo_limit_bytes);
        if global_bytes <= policy.global_limit_bytes && !repo_over {
            continue;
        }
        if protected.contains(&entry.artifact_id) {
            continue;
        }
        let removed = store.remove_artifact(&entry.repo_identity_hash, &entry.artifact_id)?;
        global_bytes = global_bytes.saturating_sub(removed);
        if let Some(repo_total) = repo_bytes.get_mut(&entry.repo_identity_hash) {
            *repo_total = repo_total.saturating_sub(removed);
        }
        summary.reclaimed_bytes = summary.reclaimed_bytes.saturating_add(removed);
        summary.removed_artifacts.push(entry.artifact_id);
    }

    if global_bytes > policy.global_limit_bytes {
        return Err(DaemonError::RevisionDiskBudgetExceeded {
            limit_bytes: policy.global_limit_bytes,
            requested_bytes: 0,
            current_bytes: global_bytes,
        });
    }
    if let Some(current_bytes) = repo_bytes
        .values()
        .copied()
        .find(|bytes| *bytes > policy.repo_limit_bytes)
    {
        return Err(DaemonError::RevisionDiskBudgetExceeded {
            limit_bytes: policy.repo_limit_bytes,
            requested_bytes: 0,
            current_bytes,
        });
    }
    Ok(summary)
}

fn plan_managed_worktrees(
    registry: &ManagedWorktreeRegistry,
    repo_root: &Path,
    plan: &mut RevisionPrunePlan,
) -> Result<(), DaemonError> {
    let reconciliation = registry.reconcile(repo_root)?;
    let managed_root = registry.managed_repo_dir(repo_root)?;
    let known_paths: HashSet<_> = reconciliation
        .managed_entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect();

    for entry in reconciliation.managed_entries {
        if entry.is_prunable() {
            plan.worktree_candidates.push(worktree_candidate(
                &entry,
                "Git marks managed worktree prunable",
            ));
        } else {
            let is_locked = entry.is_locked();
            plan.refusals.push(PruneRefusal {
                artifact_id: None,
                worktree_path: Some(entry.path),
                reason: if is_locked {
                    "managed worktree is locked and considered active".to_owned()
                } else {
                    "managed worktree is registered and not prunable".to_owned()
                },
            });
        }
    }

    if managed_root.is_dir() {
        for entry in std::fs::read_dir(&managed_root).map_err(|err| {
            DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to list managed worktree root: {err}"),
                path: Some(managed_root.clone()),
            }
        })? {
            let entry = entry.map_err(|err| DaemonError::RevisionSourceUnavailable {
                reason: format!("failed to read managed worktree root entry: {err}"),
                path: Some(managed_root.clone()),
            })?;
            let path = entry.path();
            if path.is_dir() && !known_paths.contains(&path) {
                plan.worktree_candidates.push(PruneWorktreeCandidate {
                    reclaimable_bytes: dir_size(&path),
                    locked: false,
                    reason: "managed worktree directory is not listed by Git".to_owned(),
                    path,
                });
            }
        }
    }
    Ok(())
}

fn worktree_candidate(entry: &GitWorktreeEntry, reason: &str) -> PruneWorktreeCandidate {
    PruneWorktreeCandidate {
        path: entry.path.clone(),
        reclaimable_bytes: dir_size(&entry.path),
        locked: entry.is_locked(),
        reason: reason.to_owned(),
    }
}

fn total_bytes<'a>(entries: impl Iterator<Item = &'a ArtifactInventoryEntry>) -> u64 {
    entries.map(|entry| entry.size_bytes).sum()
}

fn repo_totals<'a>(
    entries: impl Iterator<Item = &'a ArtifactInventoryEntry>,
) -> HashMap<String, u64> {
    let mut totals = HashMap::new();
    for entry in entries {
        *totals.entry(entry.repo_identity_hash.clone()).or_insert(0) += entry.size_bytes;
    }
    totals
}

fn dir_size(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    std::fs::read_dir(path).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .map(|entry| dir_size(&entry.path()))
            .sum()
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    use tempfile::TempDir;

    use crate::config::ManagedWorktreeConfig;

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

    fn artifact(store: &RevisionArtifactStore, repo: &str, artifact: &str, bytes: &[u8]) {
        let dir = store
            .artifact_dir(repo, &ArtifactId(artifact.to_owned()))
            .unwrap();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("graph.bin"), bytes).unwrap();
        fs::write(dir.join("manifest.json"), b"{}").unwrap();
    }

    #[test]
    fn budget_enforcement_deletes_oldest_unprotected_artifacts_first() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        artifact(&store, "repo", "old", b"old bytes");
        thread::sleep(Duration::from_millis(5));
        artifact(&store, "repo", "new", b"new bytes");

        let summary = enforce_disk_budgets(
            &store,
            RevisionDiskBudgetPolicy {
                global_limit_bytes: 15,
                repo_limit_bytes: 15,
            },
            &[ArtifactId("new".to_owned())],
        )
        .unwrap();

        assert_eq!(
            summary.removed_artifacts,
            vec![ArtifactId("old".to_owned())]
        );
        assert!(
            !store
                .artifact_dir("repo", &ArtifactId("old".to_owned()))
                .unwrap()
                .exists()
        );
        assert!(
            store
                .artifact_dir("repo", &ArtifactId("new".to_owned()))
                .unwrap()
                .exists()
        );
    }

    #[test]
    fn protected_artifacts_can_block_budget_enforcement() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        artifact(&store, "repo", "pinned", b"pinned bytes");

        let err = enforce_disk_budgets(
            &store,
            RevisionDiskBudgetPolicy {
                global_limit_bytes: 1,
                repo_limit_bytes: 1,
            },
            &[ArtifactId("pinned".to_owned())],
        )
        .unwrap_err();

        assert!(matches!(
            err,
            DaemonError::RevisionDiskBudgetExceeded { .. }
        ));
        assert!(
            store
                .artifact_dir("repo", &ArtifactId("pinned".to_owned()))
                .unwrap()
                .exists()
        );
    }

    #[test]
    fn prune_plan_reports_protected_artifact_refusals_and_worktree_candidates() {
        let tmp = TempDir::new().unwrap();
        let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
        artifact(&store, "repo", "pinned", b"pinned bytes");
        let repo = repo();
        let managed_root = TempDir::new().unwrap();
        let registry = ManagedWorktreeRegistry::new(ManagedWorktreeConfig {
            root: Some(managed_root.path().to_path_buf()),
            ..ManagedWorktreeConfig::default()
        });
        let orphan = registry
            .managed_repo_dir(repo.path())
            .unwrap()
            .join("orphan");
        fs::create_dir_all(&orphan).unwrap();

        let plan = plan_prune(
            &store,
            &[],
            &[ArtifactId("pinned".to_owned())],
            &registry,
            &[repo.path().to_path_buf()],
        )
        .unwrap();

        assert!(plan.artifact_candidates.is_empty());
        assert_eq!(plan.refusals.len(), 1);
        assert_eq!(
            plan.refusals[0].artifact_id,
            Some(ArtifactId("pinned".to_owned()))
        );
        assert_eq!(plan.worktree_candidates.len(), 1);
        assert_eq!(plan.worktree_candidates[0].path, orphan);
    }
}
