//! Revision-aware workspace wire types.
//!
//! The protocol crate is intentionally leaf-only, so these types describe
//! selectors, resolved identities, handles, and daemon request/response
//! payloads without depending on daemon internals or graph crates.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Daemon-scoped stable identifier for a revision handle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct RevisionId(pub String);

/// Opaque stable artifact identifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ArtifactId(pub String);

/// Digest of the manifest inputs that produced an [`ArtifactId`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArtifactInputDigest {
    /// Manifest key schema version used to compute the digest.
    pub schema_version: u32,
    /// Hex-encoded digest over the canonical artifact manifest inputs.
    pub digest: String,
}

/// Git object identifier format for the repository.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectFormat {
    /// SHA-1 Git object ids.
    Sha1,
    /// SHA-256 Git object ids.
    Sha256,
}

/// Source bytes used while constructing a revision graph.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceByteMode {
    /// Bytes are read directly from Git tree/blob objects.
    RawGitObjects,
    /// Bytes are read from a checkout and are keyed by checkout filters.
    CheckoutBytes,
    /// Bytes are captured from the dirty live workspace.
    DirtySnapshot,
}

/// Caller-supplied revision selector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RevisionSelector {
    /// The currently loaded live workspace.
    Live,
    /// Point-in-time exact-byte dirty snapshot of the live workspace.
    Dirty {
        /// Include untracked files that are not ignored by Git.
        include_untracked: bool,
        /// Include ignored files. Defaults should keep this false.
        include_ignored: bool,
    },
    /// Mutable Git ref, pinned to commit/tree at load time.
    Ref {
        /// Ref name or ref-like input accepted by the resolver.
        name: String,
    },
    /// Immutable commit object id.
    Commit {
        /// Full object id in the repository object format.
        oid: String,
    },
    /// Immutable tree object id.
    Tree {
        /// Full object id in the repository object format.
        oid: String,
    },
    /// Explicit managed or operator-provided worktree.
    Worktree {
        /// Optional worktree root path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
        /// Optional daemon-managed worktree id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_id: Option<String>,
    },
}

/// Redacted local repository identity carried on revision responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryIdentity {
    /// Stable daemon-local hash over repository identity inputs.
    pub repo_identity_hash: String,
    /// Git object id format for this repository.
    pub object_format: ObjectFormat,
    /// Optional redacted remote diagnostic fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_fingerprint: Option<String>,
}

/// Resolved immutable revision identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRevision {
    /// Original caller selector.
    pub selector: RevisionSelector,
    /// Daemon-local repository identity.
    pub repository: RepositoryIdentity,
    /// Resolved commit object id when selector resolution has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_oid: Option<String>,
    /// Resolved tree object id.
    pub tree_oid: String,
    /// Git object id format.
    pub object_format: ObjectFormat,
    /// Source bytes used for graph construction.
    pub source_byte_mode: SourceByteMode,
    /// RFC3339 UTC timestamp when mutable selectors were pinned.
    pub resolved_at: String,
}

/// Explicit query target for revision-aware daemon searches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RevisionQueryTarget {
    /// Query the currently loaded live workspace.
    Live,
    /// Query by resident revision handle.
    RevisionId {
        /// Loaded revision id.
        revision_id: RevisionId,
    },
    /// Resolve and query this selector if a matching resident handle exists.
    Selector {
        /// Caller selector.
        selector: RevisionSelector,
    },
}

/// Revision metadata attached to revision-aware query responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RevisionQueryMetadata {
    /// Revision handle that served the query, absent for live legacy searches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<RevisionId>,
    /// Artifact that served the query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<ArtifactId>,
    /// Fully resolved revision identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved: Option<ResolvedRevision>,
}

/// Kind of resident revision handle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResidentHandleKind {
    /// Existing live workspace handle.
    LiveWorkspace,
    /// Immutable Git tree/commit/ref graph.
    ImmutableRevision,
    /// Exact-byte dirty snapshot graph.
    DirtySnapshot,
    /// Explicit managed or operator worktree graph.
    ManagedWorktree,
}

/// Resident revision lifecycle state.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RevisionLoadState {
    /// Load has been accepted and is building.
    Loading,
    /// Graph is resident and queryable.
    Loaded,
    /// Load failed with a recorded diagnostic.
    Failed,
    /// Handle is currently being evicted.
    Evicting,
    /// Handle is no longer resident.
    Unloaded,
}

/// Daemon status for one resident revision handle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RevisionStatus {
    /// Resident revision id.
    pub revision_id: RevisionId,
    /// Handle kind.
    pub handle_kind: ResidentHandleKind,
    /// Resolved immutable identity.
    pub resolved: ResolvedRevision,
    /// Artifact backing this handle.
    pub artifact_id: ArtifactId,
    /// Manifest input digest for artifact verification.
    pub artifact_inputs: ArtifactInputDigest,
    /// Current lifecycle state.
    pub state: RevisionLoadState,
    /// Whether this handle is pinned against pruning/eviction.
    pub pinned: bool,
    /// Number of active queries using this handle.
    pub active_queries: u64,
    /// Estimated resident memory bytes.
    pub memory_bytes: u64,
    /// Last error associated with this handle, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Request payload for `daemon/loadRevision`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LoadRevisionRequest {
    /// Source root or repository root used to resolve the selector.
    pub root: PathBuf,
    /// Revision selector to load.
    pub selector: RevisionSelector,
    /// Source byte mode requested by the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_byte_mode: Option<SourceByteMode>,
    /// Pin the loaded handle against eviction/pruning.
    #[serde(default)]
    pub pin: bool,
}

/// Result payload for `daemon/loadRevision`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LoadRevisionResult {
    /// Loaded revision id.
    pub revision_id: RevisionId,
    /// Artifact backing the loaded handle.
    pub artifact_id: ArtifactId,
    /// Manifest input digest used to verify the artifact id.
    pub artifact_inputs: ArtifactInputDigest,
    /// Resolved revision identity.
    pub resolved: ResolvedRevision,
    /// Handle status after load.
    pub status: RevisionStatus,
}

/// Request payload for `daemon/unloadRevision`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UnloadRevisionRequest {
    /// Resident revision id.
    pub revision_id: RevisionId,
    /// Force unload of a pinned handle.
    #[serde(default)]
    pub force: bool,
}

/// Result payload for `daemon/unloadRevision`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UnloadRevisionResult {
    /// Resident revision id.
    pub revision_id: RevisionId,
    /// True when a resident handle was unloaded.
    pub unloaded: bool,
}

/// Request payload for `daemon/listRevisions`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListRevisionsRequest {
    /// Optional repository root filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    /// Include unloaded handles when the daemon retains status history.
    #[serde(default)]
    pub include_unloaded: bool,
}

/// Result payload for `daemon/listRevisions`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ListRevisionsResult {
    /// Matching revision statuses.
    pub revisions: Vec<RevisionStatus>,
}

/// Request payload for `daemon/revisionStatus`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RevisionStatusRequest {
    /// Resident revision id.
    pub revision_id: RevisionId,
}

/// Request payload for `daemon/pruneRevisions`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PruneRevisionsRequest {
    /// Optional repository root filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    /// Actually remove prunable handles/artifacts.
    #[serde(default)]
    pub apply: bool,
}

/// One candidate returned by `daemon/pruneRevisions`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PruneRevisionCandidate {
    /// Resident revision id.
    pub revision_id: RevisionId,
    /// Artifact that can be removed.
    pub artifact_id: ArtifactId,
    /// Bytes reclaimable if this candidate is pruned.
    pub reclaimable_bytes: u64,
    /// Human-readable reason.
    pub reason: String,
}

/// One managed worktree candidate returned by `daemon/pruneRevisions`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PruneWorktreeCandidate {
    /// Managed worktree path.
    pub path: PathBuf,
    /// Bytes reclaimable if this worktree directory is removed.
    pub reclaimable_bytes: u64,
    /// Whether Git currently marks this worktree as locked.
    pub locked: bool,
    /// Human-readable reason.
    pub reason: String,
}

/// One artifact or worktree that was considered but refused for pruning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PruneRefusal {
    /// Artifact protected from deletion, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<ArtifactId>,
    /// Worktree protected from deletion, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<PathBuf>,
    /// Human-readable refusal reason.
    pub reason: String,
}

/// Result payload for `daemon/pruneRevisions`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PruneRevisionsResult {
    /// Candidate list, populated for dry-run and apply.
    pub candidates: Vec<PruneRevisionCandidate>,
    /// Managed worktree candidates, populated for dry-run and apply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worktree_candidates: Vec<PruneWorktreeCandidate>,
    /// Artifacts or worktrees refused for pruning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<PruneRefusal>,
    /// Whether removal was applied.
    pub applied: bool,
    /// Total bytes reclaimed by this invocation.
    pub reclaimed_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_repo() -> RepositoryIdentity {
        RepositoryIdentity {
            repo_identity_hash: "repo-hash".to_owned(),
            object_format: ObjectFormat::Sha1,
            remote_fingerprint: Some("remote:redacted".to_owned()),
        }
    }

    fn sample_resolved(selector: RevisionSelector) -> ResolvedRevision {
        ResolvedRevision {
            selector,
            repository: sample_repo(),
            commit_oid: Some("1111111111111111111111111111111111111111".to_owned()),
            tree_oid: "2222222222222222222222222222222222222222".to_owned(),
            object_format: ObjectFormat::Sha1,
            source_byte_mode: SourceByteMode::RawGitObjects,
            resolved_at: "2026-06-26T00:00:00Z".to_owned(),
        }
    }

    fn sample_status() -> RevisionStatus {
        let resolved = sample_resolved(RevisionSelector::Ref {
            name: "main".to_owned(),
        });
        RevisionStatus {
            revision_id: RevisionId("rev-1".to_owned()),
            handle_kind: ResidentHandleKind::ImmutableRevision,
            resolved,
            artifact_id: ArtifactId("artifact-1".to_owned()),
            artifact_inputs: ArtifactInputDigest {
                schema_version: 1,
                digest: "digest-1".to_owned(),
            },
            state: RevisionLoadState::Loaded,
            pinned: false,
            active_queries: 0,
            memory_bytes: 128,
            last_error: None,
        }
    }

    #[test]
    fn revision_selector_round_trips_all_variants() {
        let selectors = [
            RevisionSelector::Live,
            RevisionSelector::Dirty {
                include_untracked: true,
                include_ignored: false,
            },
            RevisionSelector::Ref {
                name: "refs/heads/main".to_owned(),
            },
            RevisionSelector::Commit {
                oid: "a".repeat(40),
            },
            RevisionSelector::Tree {
                oid: "b".repeat(40),
            },
            RevisionSelector::Worktree {
                path: Some(PathBuf::from("/repo-agent")),
                worktree_id: Some("agent-1".to_owned()),
            },
        ];

        for selector in selectors {
            let wire = serde_json::to_string(&selector).expect("serialize selector");
            let back: RevisionSelector = serde_json::from_str(&wire).expect("deserialize selector");
            assert_eq!(back, selector);
        }
    }

    #[test]
    fn resolved_revision_round_trips_with_identity_and_source_mode() {
        let resolved = sample_resolved(RevisionSelector::Commit {
            oid: "a".repeat(40),
        });
        let wire = serde_json::to_string(&resolved).expect("serialize");
        assert!(wire.contains(r#""source_byte_mode":"raw_git_objects""#));
        assert!(wire.contains(r#""object_format":"sha1""#));
        let back: ResolvedRevision = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back, resolved);
    }

    #[test]
    fn artifact_id_is_opaque_but_inputs_are_manifest_verifiable() {
        let id = ArtifactId("repo/artifact-key".to_owned());
        let digest = ArtifactInputDigest {
            schema_version: 1,
            digest: "canonical-manifest-sha256".to_owned(),
        };
        let wire = serde_json::json!({
            "artifact_id": id,
            "artifact_inputs": digest,
        });
        assert_eq!(wire["artifact_id"], "repo/artifact-key");
        assert_eq!(
            wire["artifact_inputs"]["digest"],
            "canonical-manifest-sha256"
        );
    }

    #[test]
    fn status_and_load_result_round_trip() {
        let status = sample_status();
        let result = LoadRevisionResult {
            revision_id: status.revision_id.clone(),
            artifact_id: status.artifact_id.clone(),
            artifact_inputs: status.artifact_inputs.clone(),
            resolved: status.resolved.clone(),
            status,
        };
        let wire = serde_json::to_string(&result).expect("serialize");
        let back: LoadRevisionResult = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back, result);
    }

    #[test]
    fn list_status_and_prune_requests_round_trip() {
        let list = ListRevisionsRequest {
            root: Some(PathBuf::from("/repo")),
            include_unloaded: true,
        };
        let wire = serde_json::to_string(&list).expect("serialize list");
        let back: ListRevisionsRequest = serde_json::from_str(&wire).expect("deserialize list");
        assert_eq!(back, list);

        let prune = PruneRevisionsRequest {
            root: Some(PathBuf::from("/repo")),
            apply: false,
        };
        let wire = serde_json::to_string(&prune).expect("serialize prune");
        let back: PruneRevisionsRequest = serde_json::from_str(&wire).expect("deserialize prune");
        assert_eq!(back, prune);
    }

    #[test]
    fn revision_query_target_round_trips_without_path_leak() {
        let target = RevisionQueryTarget::RevisionId {
            revision_id: RevisionId("rev-1".to_owned()),
        };
        let wire = serde_json::to_string(&target).expect("serialize");
        assert!(!wire.contains("/"));
        let back: RevisionQueryTarget = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back, target);
    }
}
