//! Workspace registry and discovery utilities for multi-repo indexing.

pub mod cache;
pub mod discovery;
pub mod error;
pub mod index;
pub mod logical;
pub mod registry;
mod serde_time;
pub mod stats;

pub use cache::{
    CACHE_TTL, SourceRootIndexState, SourceRootStatus, WORKSPACE_CACHE_DIRNAME,
    WORKSPACE_STATUS_FILENAME, WorkspaceIndexStatus, WorkspaceWarning, cache_path, read_cache,
    write_cache,
};
pub use discovery::{DiscoveryMode, discover_repositories};
pub use error::{WorkspaceError, WorkspaceResult};
pub use index::{MatchInfo, NodeWithRepo, WorkspaceIndex, WorkspaceStats};
pub use logical::{
    Classification, HeuristicVerdict, LogicalWorkspace, LogicalWorkspaceError, MemberFolder,
    MemberReason, SourceRoot, WorkspaceId, WorkspaceIdentity,
};
pub use registry::{
    WORKSPACE_REGISTRY_VERSION, WorkspaceMemberFolder, WorkspaceMetadata, WorkspaceRegistry,
    WorkspaceRepoId, WorkspaceRepository,
};
pub use stats::{DetailedWorkspaceStats, FreshnessBuckets};

#[cfg(test)]
mod discovery_tests;
#[cfg(test)]
mod tests;
