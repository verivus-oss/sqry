//! Revision prune diagnostics and apply path.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;
use sqry_daemon_protocol::{ArtifactId, PruneRevisionsRequest, PruneRevisionsResult};

use crate::workspace::revision::{ManagedWorktreeRegistry, RevisionArtifactStore, plan_prune};

use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError};

/// Handle `daemon/pruneRevisions`.
pub(crate) fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let req: PruneRevisionsRequest = match params {
        Value::Null => PruneRevisionsRequest::default(),
        other => serde_json::from_value(other).map_err(MethodError::InvalidParams)?,
    };
    let root = req.root.as_deref().map(canonical_or_original);
    let store = RevisionArtifactStore::new(RevisionArtifactStore::default_cache_root());
    let statuses = ctx
        .manager
        .resident_revision_statuses(root.as_deref(), false);
    let pinned = ctx.manager.pinned_revision_artifact_ids();
    let worktree_registry = ManagedWorktreeRegistry::new(ctx.config.managed_worktrees.clone());
    let repo_roots = prune_repo_roots(root.as_deref(), ctx);
    let plan = plan_prune(&store, &statuses, &pinned, &worktree_registry, &repo_roots)?;
    let mut reclaimed_bytes = 0;
    if req.apply {
        let artifact_repos = artifact_repo_map(&store)?;
        for candidate in &plan.artifact_candidates {
            let _ = ctx
                .manager
                .unload_resident_revision(&candidate.revision_id, false)?;
            if let Some(repo_identity_hash) = artifact_repos.get(&candidate.artifact_id) {
                reclaimed_bytes +=
                    store.remove_artifact(repo_identity_hash, &candidate.artifact_id)?;
            }
        }
        for candidate in &plan.worktree_candidates {
            apply_worktree_candidate(&worktree_registry, &repo_roots, &candidate.path)?;
            reclaimed_bytes += candidate.reclaimable_bytes;
        }
    }

    let result = PruneRevisionsResult {
        candidates: plan.artifact_candidates,
        worktree_candidates: plan.worktree_candidates,
        refusals: plan.refusals,
        applied: req.apply,
        reclaimed_bytes,
    };
    let envelope = ResponseEnvelope {
        result,
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope).map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
}

fn canonical_or_original(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn prune_repo_roots(root: Option<&Path>, ctx: &HandlerContext) -> Vec<PathBuf> {
    if let Some(root) = root {
        return vec![root.to_path_buf()];
    }
    ctx.config
        .workspaces
        .iter()
        .filter(|workspace| !workspace.exclude && workspace.path.exists())
        .map(|workspace| canonical_or_original(&workspace.path))
        .collect()
}

fn artifact_repo_map(
    store: &RevisionArtifactStore,
) -> Result<HashMap<ArtifactId, String>, crate::error::DaemonError> {
    Ok(store
        .inventory()?
        .into_iter()
        .map(|entry| (entry.artifact_id, entry.repo_identity_hash))
        .collect())
}

fn apply_worktree_candidate(
    registry: &ManagedWorktreeRegistry,
    repo_roots: &[PathBuf],
    path: &Path,
) -> Result<(), crate::error::DaemonError> {
    for repo_root in repo_roots {
        let managed_root = registry.managed_repo_dir(repo_root)?;
        if path.starts_with(&managed_root) {
            if registry
                .list(repo_root)?
                .iter()
                .any(|entry| entry.path == path)
            {
                registry.remove(repo_root, path)?;
                registry.prune(repo_root)?;
            } else if path.exists() {
                fs::remove_dir_all(path).map_err(|err| {
                    crate::error::DaemonError::RevisionSourceUnavailable {
                        reason: format!("failed to remove orphaned managed worktree: {err}"),
                        path: Some(path.to_path_buf()),
                    }
                })?;
            }
            return Ok(());
        }
    }
    Err(crate::error::DaemonError::ManagedWorktreeInUse {
        worktree: path.to_path_buf(),
        reason: "candidate is not under a configured managed worktree root".to_owned(),
    })
}
