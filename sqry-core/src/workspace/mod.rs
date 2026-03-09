//! Workspace registry and discovery utilities for multi-repo indexing.

pub mod discovery;
pub mod error;
pub mod index;
pub mod registry;
mod serde_time;
pub mod stats;

pub use discovery::{DiscoveryMode, discover_repositories};
pub use error::{WorkspaceError, WorkspaceResult};
pub use index::{MatchInfo, NodeWithRepo, WorkspaceIndex, WorkspaceStats};
pub use registry::{WorkspaceMetadata, WorkspaceRegistry, WorkspaceRepoId, WorkspaceRepository};
pub use stats::{DetailedWorkspaceStats, FreshnessBuckets};

#[cfg(test)]
mod tests;
