//! Revision-aware daemon load/unload methods.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
};

use serde_json::Value;
use sqry_daemon_protocol::{
    LoadRevisionRequest, LoadRevisionResult, ResidentHandleKind, ResolvedRevision, RevisionId,
    RevisionQueryMetadata, RevisionQueryTarget, RevisionSelector, SourceByteMode,
    UnloadRevisionRequest, UnloadRevisionResult,
};

use crate::{
    error::DaemonError,
    workspace::{
        ResidentRevisionLoad, WorkspaceBuilder,
        revision::{
            ArtifactKeyInputs, DirtySnapshotOptions, DirtySnapshotSource, GraphSchemaFingerprint,
            LocalRepositoryIdentity, PathScope, RawGitSource, RawGitSourceOptions,
            RevisionArtifactManifest, RevisionArtifactStore, RevisionDiskBudgetPolicy,
            SourceDigest, canonical_json_sha256, enforce_disk_budgets,
        },
    },
};

use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError};

/// Handle `daemon/loadRevision`.
pub(crate) async fn handle_load(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let req: LoadRevisionRequest = parse_params(params, "daemon/loadRevision")?;
    let root = canonical_git_root(&req.root).map_err(MethodError::Daemon)?;
    let config = Arc::clone(&ctx.config);
    let manager = Arc::clone(&ctx.manager);
    let builder = Arc::clone(&ctx.workspace_builder);

    let handle = tokio::task::spawn_blocking(move || {
        load_revision_sync(&manager, &builder, &config, root, req)
    })
    .await
    .map_err(MethodError::JoinError)??;

    let status = handle.status();
    let result = LoadRevisionResult {
        revision_id: status.revision_id.clone(),
        artifact_id: status.artifact_id.clone(),
        artifact_inputs: status.artifact_inputs.clone(),
        resolved: status.resolved.clone(),
        status,
    };
    let envelope = ResponseEnvelope {
        result,
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope).map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
}

/// Handle `daemon/unloadRevision`.
pub(crate) fn handle_unload(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let req: UnloadRevisionRequest = parse_params(params, "daemon/unloadRevision")?;
    let unloaded = ctx
        .manager
        .unload_resident_revision(&req.revision_id, req.force)?;
    let result = UnloadRevisionResult {
        revision_id: req.revision_id,
        unloaded,
    };
    let envelope = ResponseEnvelope {
        result,
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope).map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
}

pub(crate) fn resolve_query_target(
    ctx: &HandlerContext,
    root: &Path,
    target: &RevisionQueryTarget,
) -> Result<(RevisionId, RevisionQueryMetadata), DaemonError> {
    match target {
        RevisionQueryTarget::Live => Err(DaemonError::RevisionQueryRequiresExplicitSelector {
            reason: "explicit revision query target was live; omit the revision field to query the live workspace".to_owned(),
        }),
        RevisionQueryTarget::RevisionId { revision_id } => {
            let handle = ctx.manager.resident_revisions().get(revision_id).ok_or_else(|| {
                DaemonError::RevisionSourceUnavailable {
                    reason: format!("resident revision {} is not loaded", revision_id.0),
                    path: None,
                }
            })?;
            if handle.source_root() != root {
                return Err(DaemonError::RevisionSourceUnavailable {
                    reason: format!(
                        "resident revision {} belongs to {}, not requested root {}",
                        revision_id.0,
                        handle.source_root().display(),
                        root.display()
                    ),
                    path: Some(root.to_path_buf()),
                });
            }
            let status = handle.status();
            Ok((
                revision_id.clone(),
                RevisionQueryMetadata {
                    revision_id: Some(revision_id.clone()),
                    artifact_id: Some(status.artifact_id),
                    resolved: Some(status.resolved),
                },
            ))
        }
        RevisionQueryTarget::Selector { selector } => {
            let statuses = ctx.manager.resident_revision_statuses(Some(root), false);
            let matches: Vec<_> = statuses
                .into_iter()
                .filter(|status| selector_matches(selector, &status.resolved))
                .collect();
            match matches.len() {
                0 => Err(DaemonError::RevisionSourceUnavailable {
                    reason: format!(
                        "no loaded resident revision matches selector {} for {}",
                        selector_label(selector),
                        root.display()
                    ),
                    path: Some(root.to_path_buf()),
                }),
                1 => {
                    let status = matches.into_iter().next().expect("one match");
                    Ok((
                        status.revision_id.clone(),
                        RevisionQueryMetadata {
                            revision_id: Some(status.revision_id),
                            artifact_id: Some(status.artifact_id),
                            resolved: Some(status.resolved),
                        },
                    ))
                }
                _ => Err(DaemonError::RevisionSelectorAmbiguous {
                    selector: selector_label(selector),
                    matches: matches
                        .into_iter()
                        .map(|status| status.revision_id.0)
                        .collect(),
                }),
            }
        }
    }
}

fn load_revision_sync(
    manager: &crate::workspace::WorkspaceManager,
    builder: &Arc<dyn WorkspaceBuilder>,
    config: &crate::config::DaemonConfig,
    root: PathBuf,
    req: LoadRevisionRequest,
) -> Result<Arc<crate::workspace::ResidentRevisionHandle>, DaemonError> {
    match req.selector.clone() {
        RevisionSelector::Live => Err(DaemonError::RevisionQueryRequiresExplicitSelector {
            reason: "daemon/loadRevision does not load the live workspace; use daemon/load"
                .to_owned(),
        }),
        RevisionSelector::Ref { name } => load_immutable_revision(
            manager,
            builder,
            config,
            root,
            req.selector,
            resolve_ref(&req.root, &name)?,
            req.source_byte_mode,
            req.pin,
        ),
        RevisionSelector::Commit { oid } => load_immutable_revision(
            manager,
            builder,
            config,
            root,
            req.selector,
            resolve_commit(&req.root, &oid)?,
            req.source_byte_mode,
            req.pin,
        ),
        RevisionSelector::Tree { oid } => load_immutable_revision(
            manager,
            builder,
            config,
            root,
            req.selector,
            ResolvedGitObject {
                commit_oid: None,
                tree_oid: resolve_tree(&req.root, &oid)?,
            },
            req.source_byte_mode,
            req.pin,
        ),
        RevisionSelector::Dirty {
            include_untracked,
            include_ignored,
        } => load_snapshot_revision(
            manager,
            builder,
            config,
            root,
            req.selector,
            &SnapshotKind::Dirty {
                include_untracked,
                include_ignored,
            },
            req.source_byte_mode,
            req.pin,
        ),
        RevisionSelector::Worktree { path, worktree_id } => {
            let worktree_root = path
                .as_deref()
                .map(canonical_git_root)
                .transpose()?
                .unwrap_or_else(|| root.clone());
            load_snapshot_revision(
                manager,
                builder,
                config,
                worktree_root,
                req.selector,
                &SnapshotKind::Worktree { worktree_id },
                req.source_byte_mode,
                req.pin,
            )
        }
    }
}

fn load_immutable_revision(
    manager: &crate::workspace::WorkspaceManager,
    builder: &Arc<dyn WorkspaceBuilder>,
    config: &crate::config::DaemonConfig,
    root: PathBuf,
    selector: RevisionSelector,
    object: ResolvedGitObject,
    requested_mode: Option<SourceByteMode>,
    pinned: bool,
) -> Result<Arc<crate::workspace::ResidentRevisionHandle>, DaemonError> {
    let source_byte_mode = requested_mode.unwrap_or(SourceByteMode::RawGitObjects);
    if source_byte_mode != SourceByteMode::RawGitObjects {
        return Err(DaemonError::CheckoutFilterUnsupported {
            filter: format!("{source_byte_mode:?} requested for immutable Git revision"),
            path: Some(root),
        });
    }
    let identity = LocalRepositoryIdentity::discover(&root).map_err(|err| identity_error(&err))?;
    let resolved = ResolvedRevision {
        selector,
        repository: identity.to_wire(),
        commit_oid: object.commit_oid,
        tree_oid: object.tree_oid.clone(),
        object_format: identity.object_format,
        source_byte_mode,
        resolved_at: resolved_at_now(),
    };
    let key_inputs = artifact_inputs(
        config,
        &identity,
        SourceDigest::Tree {
            tree_oid: object.tree_oid.clone(),
        },
        source_byte_mode,
    )?;
    let artifact_id = key_inputs
        .artifact_id()
        .map_err(|err| DaemonError::ArtifactKeyMismatch {
            artifact_id: "uncomputed".to_owned(),
            reason: err.to_string(),
        })?;
    let revision_id = ResidentRevisionLoad::deterministic_revision_id(
        ResidentHandleKind::ImmutableRevision,
        &artifact_id,
    );
    let artifact_inputs =
        RevisionArtifactManifest::new(artifact_id.clone(), resolved.clone(), key_inputs.clone())
            .map_err(|err| DaemonError::ArtifactKeyMismatch {
                artifact_id: artifact_id.0.clone(),
                reason: err.to_string(),
            })?
            .artifact_inputs;
    let load = ResidentRevisionLoad {
        source_root: root.clone(),
        revision_id,
        handle_kind: ResidentHandleKind::ImmutableRevision,
        artifact_id: artifact_id.clone(),
        artifact_inputs,
        resolved: resolved.clone(),
        pinned,
    };
    let store = RevisionArtifactStore::new(RevisionArtifactStore::default_cache_root());
    manager.load_resident_revision(&load, || {
        if let Ok((graph, _manifest)) =
            store.load_graph_for_inputs(&identity.repo_identity_hash, &artifact_id, &key_inputs)
        {
            return Ok(graph);
        }
        let source = RawGitSource::open(RawGitSourceOptions::new(&root, &object.tree_oid))?;
        let graph = builder.build_virtual_source(&source)?;
        store.publish_graph(&graph, &artifact_id, resolved, key_inputs, None)?;
        let mut protected = manager.pinned_revision_artifact_ids();
        protected.push(artifact_id.clone());
        if let Err(err) = enforce_disk_budgets(
            &store,
            RevisionDiskBudgetPolicy::from_config(&config.revision_artifacts),
            &protected,
        ) {
            let _ = store.remove_artifact(&identity.repo_identity_hash, &artifact_id);
            return Err(err);
        }
        Ok(graph)
    })
}

fn load_snapshot_revision(
    manager: &crate::workspace::WorkspaceManager,
    builder: &Arc<dyn WorkspaceBuilder>,
    config: &crate::config::DaemonConfig,
    root: PathBuf,
    selector: RevisionSelector,
    snapshot_kind: &SnapshotKind,
    requested_mode: Option<SourceByteMode>,
    pinned: bool,
) -> Result<Arc<crate::workspace::ResidentRevisionHandle>, DaemonError> {
    let source_byte_mode = requested_mode.unwrap_or(SourceByteMode::DirtySnapshot);
    if source_byte_mode != SourceByteMode::DirtySnapshot {
        return Err(DaemonError::CheckoutFilterUnsupported {
            filter: format!("{source_byte_mode:?} requested for snapshot revision"),
            path: Some(root),
        });
    }
    let identity = LocalRepositoryIdentity::discover(&root).map_err(|err| identity_error(&err))?;
    let mut options = DirtySnapshotOptions::new(&root);
    if let SnapshotKind::Dirty {
        include_untracked,
        include_ignored,
    } = snapshot_kind
    {
        options.include_untracked = *include_untracked;
        options.include_ignored = *include_ignored;
    }
    let source = DirtySnapshotSource::capture(&options)?;
    let fingerprint = source.fingerprint().clone();
    let source_digest = match snapshot_kind {
        SnapshotKind::Dirty { .. } => fingerprint.source_digest(),
        SnapshotKind::Worktree { worktree_id } => SourceDigest::Worktree {
            worktree_id: worktree_id.clone().unwrap_or_else(|| {
                fingerprint
                    .base_head_commit_oid
                    .clone()
                    .unwrap_or_else(|| fingerprint.snapshot_digest.clone())
            }),
            source_digest: fingerprint.snapshot_digest.clone(),
        },
    };
    let key_inputs = artifact_inputs(config, &identity, source_digest, source_byte_mode)?;
    let artifact_id = key_inputs
        .artifact_id()
        .map_err(|err| DaemonError::ArtifactKeyMismatch {
            artifact_id: "uncomputed".to_owned(),
            reason: err.to_string(),
        })?;
    let handle_kind = match snapshot_kind {
        SnapshotKind::Dirty { .. } => ResidentHandleKind::DirtySnapshot,
        SnapshotKind::Worktree { .. } => ResidentHandleKind::ManagedWorktree,
    };
    let resolved = ResolvedRevision {
        selector,
        repository: identity.to_wire(),
        commit_oid: fingerprint.base_head_commit_oid.clone(),
        tree_oid: fingerprint
            .index_tree_oid
            .clone()
            .or(fingerprint.base_head_tree_oid.clone())
            .unwrap_or_else(|| fingerprint.snapshot_digest.clone()),
        object_format: identity.object_format,
        source_byte_mode,
        resolved_at: resolved_at_now(),
    };
    let revision_id = ResidentRevisionLoad::deterministic_revision_id(handle_kind, &artifact_id);
    let artifact_inputs =
        RevisionArtifactManifest::new(artifact_id.clone(), resolved.clone(), key_inputs)
            .map_err(|err| DaemonError::ArtifactKeyMismatch {
                artifact_id: artifact_id.0.clone(),
                reason: err.to_string(),
            })?
            .artifact_inputs;
    let graph = builder.build_virtual_source(&source)?;
    let load = ResidentRevisionLoad {
        source_root: root,
        revision_id,
        handle_kind,
        artifact_id,
        artifact_inputs,
        resolved,
        pinned,
    };
    manager.load_resident_revision(&load, || Ok(graph))
}

fn artifact_inputs(
    config: &crate::config::DaemonConfig,
    identity: &LocalRepositoryIdentity,
    source_digest: SourceDigest,
    source_byte_mode: SourceByteMode,
) -> Result<ArtifactKeyInputs, DaemonError> {
    let plugin_ids = sqry_plugin_registry::resolve_plugin_selection(
        &sqry_plugin_registry::PluginSelectionConfig::default(),
    )
    .map_err(|err| DaemonError::RevisionSourceUnavailable {
        reason: format!("failed to resolve plugin roster: {err}"),
        path: None,
    })?
    .active_plugin_ids;
    let graph_config_hash = canonical_json_sha256(&config.revision_artifacts).map_err(|err| {
        DaemonError::ArtifactKeyMismatch {
            artifact_id: "uncomputed".to_owned(),
            reason: err.to_string(),
        }
    })?;
    Ok(ArtifactKeyInputs {
        repo_identity_hash: identity.repo_identity_hash.clone(),
        source_digest,
        object_format: identity.object_format,
        path_scope: PathScope::Repository,
        source_byte_mode,
        checkout_fingerprint: None,
        graph_schema: GraphSchemaFingerprint {
            graph_schema_version: config.revision_artifacts.graph_schema_version,
            derived_schema_version: config.revision_artifacts.derived_schema_version,
            sqry_build_version: env!("CARGO_PKG_VERSION").to_owned(),
            plugin_roster_digest: plugin_ids.join(","),
            graph_config_hash,
        },
    })
}

fn selector_matches(selector: &RevisionSelector, resolved: &ResolvedRevision) -> bool {
    if selector == &resolved.selector {
        return true;
    }
    match selector {
        RevisionSelector::Live => false,
        RevisionSelector::Dirty { .. } => {
            matches!(resolved.selector, RevisionSelector::Dirty { .. })
        }
        RevisionSelector::Ref { name } => {
            matches!(&resolved.selector, RevisionSelector::Ref { name: loaded } if loaded == name)
        }
        RevisionSelector::Commit { oid } => resolved.commit_oid.as_ref() == Some(oid),
        RevisionSelector::Tree { oid } => &resolved.tree_oid == oid,
        RevisionSelector::Worktree { path, worktree_id } => match &resolved.selector {
            RevisionSelector::Worktree {
                path: loaded_path,
                worktree_id: loaded_id,
            } => path == loaded_path && worktree_id == loaded_id,
            _ => false,
        },
    }
}

fn selector_label(selector: &RevisionSelector) -> String {
    serde_json::to_string(selector).unwrap_or_else(|_| format!("{selector:?}"))
}

fn parse_params<T>(params: Value, method: &'static str) -> Result<T, MethodError>
where
    T: serde::de::DeserializeOwned,
{
    match params {
        Value::Null => Err(MethodError::InvalidParams(serde::de::Error::custom(
            format!("{method} requires params"),
        ))),
        other => serde_json::from_value(other).map_err(MethodError::InvalidParams),
    }
}

#[derive(Debug, Clone)]
struct ResolvedGitObject {
    commit_oid: Option<String>,
    tree_oid: String,
}

#[derive(Debug, Clone)]
enum SnapshotKind {
    Dirty {
        include_untracked: bool,
        include_ignored: bool,
    },
    Worktree {
        worktree_id: Option<String>,
    },
}

fn resolve_ref(root: &Path, name: &str) -> Result<ResolvedGitObject, DaemonError> {
    let commit_oid = git_stdout(
        root,
        &["rev-parse", "--verify", &format!("{name}^{{commit}}")],
    )?;
    let tree_oid = git_stdout(
        root,
        &["rev-parse", "--verify", &format!("{commit_oid}^{{tree}}")],
    )?;
    Ok(ResolvedGitObject {
        commit_oid: Some(commit_oid),
        tree_oid,
    })
}

fn resolve_commit(root: &Path, oid: &str) -> Result<ResolvedGitObject, DaemonError> {
    let commit_oid = git_stdout(
        root,
        &["rev-parse", "--verify", &format!("{oid}^{{commit}}")],
    )?;
    let tree_oid = git_stdout(
        root,
        &["rev-parse", "--verify", &format!("{commit_oid}^{{tree}}")],
    )?;
    Ok(ResolvedGitObject {
        commit_oid: Some(commit_oid),
        tree_oid,
    })
}

fn resolve_tree(root: &Path, oid: &str) -> Result<String, DaemonError> {
    git_stdout(root, &["rev-parse", "--verify", &format!("{oid}^{{tree}}")])
}

fn canonical_git_root(root: &Path) -> Result<PathBuf, DaemonError> {
    let top = git_stdout(root, &["rev-parse", "--show-toplevel"])?;
    PathBuf::from(top)
        .canonicalize()
        .map_err(|err| DaemonError::InvalidArgument {
            reason: format!("failed to canonicalize Git root {}: {err}", root.display()),
        })
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String, DaemonError> {
    let output = git_command(root).args(args).output().map_err(|err| {
        DaemonError::RevisionSourceUnavailable {
            reason: format!("failed to execute git {}: {err}", args.join(" ")),
            path: Some(root.to_path_buf()),
        }
    })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(git_failure_to_error(args, root, &output))
    }
}

fn git_command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    command
}

fn git_failure_to_error(args: &[&str], root: &Path, output: &Output) -> DaemonError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let label = args.last().copied().unwrap_or("unknown").to_owned();
    if args.iter().any(|arg| arg.contains("rev-parse"))
        || stderr.contains("Needed a single revision")
    {
        return DaemonError::RevisionObjectMissing {
            object: label,
            path: Some(root.to_path_buf()),
        };
    }
    DaemonError::RevisionSourceUnavailable {
        reason: format!("git {} failed: {stderr}", args.join(" ")),
        path: Some(root.to_path_buf()),
    }
}

fn identity_error(err: &crate::workspace::revision::RepositoryIdentityError) -> DaemonError {
    DaemonError::RevisionSourceUnavailable {
        reason: err.to_string(),
        path: None,
    }
}

fn resolved_at_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
