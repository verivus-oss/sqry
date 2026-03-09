//! Error types for workspace registry operations.

use std::path::PathBuf;

use thiserror::Error;

/// Specialized result type for workspace operations.
pub type WorkspaceResult<T> = Result<T, WorkspaceError>;

/// Errors emitted by workspace registry and discovery routines.
#[derive(Debug, Error)]
pub enum WorkspaceError {
    /// Failure while reading or writing registry files.
    #[error("workspace registry I/O error at {path}: {source}")]
    Io {
        /// Path associated with the I/O failure.
        path: PathBuf,
        /// Source error.
        #[source]
        source: std::io::Error,
    },

    /// Registry JSON failed to serialize or deserialize.
    #[error("workspace registry serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// The registry version stored on disk is unsupported.
    #[error(
        "unsupported workspace registry version {found}; expected {expected}. \
         Run `sqry workspace init` to rebuild the registry."
    )]
    UnsupportedVersion {
        /// Version read from disk.
        found: u32,
        /// Version supported by this build.
        expected: u32,
    },

    /// Attempted to insert a repository that already exists.
    #[error("workspace repository with id '{id}' already exists")]
    DuplicateRepository {
        /// Conflicting repository identifier.
        id: String,
    },

    /// Attempted to remove or fetch a repository that does not exist.
    #[error("workspace repository with id '{id}' not found")]
    RepositoryNotFound {
        /// Missing repository identifier.
        id: String,
    },

    /// Too many repositories discovered during workspace scanning (`DoS` prevention).
    ///
    /// This limit prevents denial-of-service attacks via workspaces containing
    /// thousands of `.sqry-index` files. See RR-10 Gap #2.
    #[error(
        "Too many repositories found in workspace: {found} exceeds limit of {limit}. \
         Adjust SQRY_MAX_REPOSITORIES environment variable if needed."
    )]
    TooManyRepositories {
        /// Number of repositories found before limit was hit.
        found: usize,
        /// Maximum allowed repositories (configurable via `SQRY_MAX_REPOSITORIES`).
        limit: usize,
    },

    /// Errors produced during repository discovery.
    #[error("failed to discover repositories in {root}: {source}")]
    Discovery {
        /// Root scanned during discovery.
        root: PathBuf,
        /// Source error.
        #[source]
        source: std::io::Error,
    },

    /// Query parsing error during workspace operations.
    #[error("query parsing error: {message}")]
    QueryParsing {
        /// Error message.
        message: String,
    },
}

impl WorkspaceError {
    /// Helper for wrapping I/O failures with path context.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        WorkspaceError::Io {
            path: path.into(),
            source,
        }
    }
}
