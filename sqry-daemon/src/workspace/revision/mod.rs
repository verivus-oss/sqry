//! Revision-aware workspace identity and artifact-key support.
//!
//! This module contains the deterministic inputs and raw source readers needed
//! before any revision graph is built: local Git repository identity, manifest
//! serialization, artifact id construction, and raw Git object traversal.

pub mod artifact_key;
pub mod artifact_store;
pub mod dirty_snapshot;
pub mod gc;
pub mod git_source;
pub mod identity;
pub mod manifest;
pub mod recovery;
pub mod resident;
pub mod virtual_source;
pub mod worktree_registry;

pub use artifact_key::{
    ArtifactKeyInputs, CheckoutByteFingerprint, CheckoutFilterFingerprint, GraphSchemaFingerprint,
    PathScope, SourceDigest,
};
pub use artifact_store::{
    ArtifactInventoryEntry, ArtifactPublishResult, ArtifactPublishStatus, RevisionArtifactStore,
};
pub use dirty_snapshot::{
    DirtyPathStatus, DirtySnapshotEntryDigest, DirtySnapshotFingerprint, DirtySnapshotOptions,
    DirtySnapshotSource,
};
pub use gc::{
    RevisionDiskBudgetPolicy, RevisionGcApplySummary, RevisionPrunePlan, enforce_disk_budgets,
    plan_prune,
};
pub use git_source::{RawGitSource, RawGitSourceOptions};
pub use identity::{LocalRepositoryIdentity, RepositoryIdentityError};
pub use manifest::{ManifestHashError, RevisionArtifactManifest, canonical_json_sha256};
pub use recovery::{RevisionRecoverySummary, recover_managed_worktrees, recover_startup};
pub use resident::{
    ResidentQueryGuard, ResidentRevisionHandle, ResidentRevisionLoad, ResidentRevisionRegistry,
};
pub use virtual_source::{
    DEFAULT_RAW_SOURCE_MAX_FILE_BYTES, MaterializedVirtualSource, VirtualPath, VirtualSourceEntry,
    VirtualSourceKind, VirtualSourceReader, materialize_virtual_source,
};
pub use worktree_registry::{
    GitWorktreeEntry, ManagedWorktreeCreateOptions, ManagedWorktreeKind,
    ManagedWorktreeReconciliation, ManagedWorktreeRecord, ManagedWorktreeRegistry,
};
