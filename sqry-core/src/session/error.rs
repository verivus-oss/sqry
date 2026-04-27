//! Error types for session management operations.

use std::io;
use std::path::PathBuf;

use notify::Error as NotifyError;
use thiserror::Error;

/// Result alias for session operations.
pub type SessionResult<T> = Result<T, SessionError>;

/// Errors that can occur while managing session state.
#[derive(Debug, Error)]
pub enum SessionError {
    /// Failure to create the underlying filesystem watcher.
    #[error("failed to create session file watcher: {0}")]
    WatcherInit(#[from] NotifyError),

    /// Failure to register a workspace index for watching.
    #[error("failed to watch index file at {path}: {source}")]
    WatchIndex {
        /// Workspace path being watched.
        path: PathBuf,
        /// Underlying filesystem watcher error.
        #[source]
        source: NotifyError,
    },

    /// Failure to unregister a workspace index path.
    #[error("failed to unwatch index file at {path}: {source}")]
    UnwatchIndex {
        /// Workspace path being un-watched.
        path: PathBuf,
        /// Underlying filesystem watcher error.
        #[source]
        source: NotifyError,
    },

    /// Failure to read filesystem metadata for the cached index.
    #[error("failed to read metadata for {path}: {source}")]
    IndexMetadata {
        /// Path to the affected file inside `<workspace>/.sqry/graph/`
        /// (typically `snapshot.sqry` or `manifest.json`).
        path: PathBuf,
        /// Filesystem error that occurred while reading metadata.
        #[source]
        source: io::Error,
    },

    /// Failure to deserialize a symbol index from disk.
    #[error("failed to load symbol index from {path}: {source}")]
    IndexLoad {
        /// Path to the snapshot file
        /// (`<workspace>/.sqry/graph/snapshot.sqry`) that failed to load.
        path: PathBuf,
        /// Deserialization error emitted by the symbol index loader.
        #[source]
        source: crate::Error,
    },

    /// Failure to execute a query against a cached index.
    #[error("failed to execute query: {0}")]
    QueryExecution(#[source] crate::Error),

    /// Failure to parse a query string.
    #[error("failed to parse query: {0}")]
    QueryParse(String),

    /// Failure to spawn the background session maintenance thread.
    #[error("failed to spawn session maintenance thread: {0}")]
    SpawnThread(#[source] io::Error),
}
