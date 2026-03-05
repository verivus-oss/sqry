//! Git integration for change-aware index updates
//!
//! This module provides git-based change detection to enable 10-100x faster
//! incremental index builds by processing only files that have changed since
//! the last index build.
//!
//! # Architecture
//!
//! The module uses a trait-based design to support multiple backends:
//! - `SubprocessGit`: Subprocess-based git command execution (current)
//! - `NoGit`: Fallback when git is unavailable (always returns empty changes)
//! - Future: `Git2Backend` for libgit2-based implementation (enterprise)
//!
//! # Security
//!
//! - All git commands use `Command::new("git")` with array arguments (no shell)
//! - File paths are canonicalized and validated to remain under workspace root
//! - Environment variables are validated and clamped to safe ranges
//! - Git command output is limited to 10MB to prevent memory exhaustion
//! - Timeouts enforce SIGTERM then SIGKILL process cleanup
//!
//! # Example
//!
//! ```no_run
//! use sqry_core::git::{GitChangeTracker, ChangeSet};
//! use std::path::Path;
//!
//! let workspace = Path::new("/path/to/repo");
//! let mut tracker = GitChangeTracker::new(workspace)?;
//!
//! // Detect changes since last indexed commit
//! let baseline = Some("abc123");
//! let (changes, new_head) = tracker.detect_changes(baseline)?;
//!
//! println!("Changed files: {}", changes.total());
//! println!("New HEAD: {:?}", new_head);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::path::{Path, PathBuf};
use std::result::Result as StdResult;

mod nogit;
mod parser;
pub mod recency;
mod subprocess;
mod worktree;

pub use nogit::NoGit;
pub use parser::{parse_diff_name_status, parse_porcelain};
pub use recency::RecencyIndex;
pub use subprocess::{SubprocessGit, max_git_output_size};
pub use worktree::WorktreeManager;

/// Result type for git operations
pub type Result<T> = StdResult<T, GitError>;

/// Errors that can occur during git operations
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// Git binary not found in PATH
    #[error("Git binary not found in PATH")]
    NotFound,

    /// Directory is not a git repository
    #[error("Not a git repository: {0}")]
    NotARepo(PathBuf),

    /// Git command timed out
    #[error("Git command timed out after {0}ms")]
    Timeout(u64),

    /// Git command failed with non-zero exit
    #[error("Git command failed: {message}\nstdout: {stdout}\nstderr: {stderr}")]
    CommandFailed {
        /// Error message describing the failure
        message: String,
        /// Standard output from the git command
        stdout: String,
        /// Standard error from the git command
        stderr: String,
    },

    /// Failed to parse git output
    #[error("Failed to parse git output: {0}")]
    InvalidOutput(String),

    /// Git output exceeded configured limit (P1-17)
    ///
    /// This error occurs when a git command produces more output than the
    /// configured limit (default 10MB, range 1MB-100MB via `SQRY_GIT_MAX_OUTPUT_SIZE`).
    ///
    /// # Security
    ///
    /// This protects against `DoS` attacks from malicious repositories with
    /// arbitrarily large git diffs or status output.
    ///
    /// # Resolution
    ///
    /// 1. Investigate the large output: `git diff --stat`
    /// 2. Check for accidentally committed binaries or vendored dependencies
    /// 3. If legitimate, increase the limit: `export SQRY_GIT_MAX_OUTPUT_SIZE=<bytes>`
    #[error("Git output exceeded configured limit")]
    OutputExceededLimit {
        /// Configured limit in bytes
        limit_bytes: usize,
        /// Actual output size in bytes (conservative estimate if truncated)
        actual_bytes: usize,
    },

    /// IO error occurred
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Feature not supported by this backend
    #[error("Feature not supported: {0}")]
    NotSupported(String),
}

impl GitError {
    /// Calculate suggested new limit (2× actual, rounded up to nearest MB)
    #[must_use]
    pub fn suggested_limit(&self) -> usize {
        match self {
            GitError::OutputExceededLimit { actual_bytes, .. } => {
                let suggested = actual_bytes * 2;
                // Round up to nearest MB
                ((suggested / (1024 * 1024)) + 1) * (1024 * 1024)
            }
            _ => 0,
        }
    }

    /// Get detailed error message with suggestions (P1-17)
    ///
    /// For `OutputExceededLimit`, returns a detailed message with:
    /// - Current limit in MB
    /// - Actual output size in MB
    /// - Suggested new limit (2× actual, rounded up)
    /// - Investigation steps
    #[must_use]
    pub fn detailed_message(&self) -> String {
        match self {
            GitError::OutputExceededLimit {
                limit_bytes,
                actual_bytes,
            } => {
                let limit_mb = bytes_to_mb(*limit_bytes);
                let actual_mb = bytes_to_mb(*actual_bytes);
                let suggested = actual_bytes * 2;
                let suggested_limit = ((suggested / (1024 * 1024)) + 1) * (1024 * 1024);
                let suggested_mb = bytes_to_mb(suggested_limit);

                format!(
                    "Git output exceeded configured limit\n  \
                     Limit: {limit_mb:.1} MB (set via SQRY_GIT_MAX_OUTPUT_SIZE)\n  \
                     Actual: >{actual_mb:.1} MB\n\n  \
                     Suggestions:\n  \
                     - Increase limit: export SQRY_GIT_MAX_OUTPUT_SIZE={suggested_limit}  # {suggested_mb:.0} MB\n  \
                     - Investigate large diffs: git diff --stat\n  \
                     - Check for accidentally committed binaries"
                )
            }
            other => format!("{other}"),
        }
    }
}

#[inline]
#[allow(clippy::cast_precision_loss)] // MB conversion uses human-readable floats; loss is acceptable
fn bytes_to_mb(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Represents a set of file changes detected by git
///
/// **Path Semantics**: All paths are repo-root-relative (not absolute).
/// Example: `src/main.rs` not `/home/user/project/src/main.rs`
///
/// **Security**: Before use, paths must be canonicalized and validated
/// to remain under the workspace root to prevent path traversal attacks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeSet {
    /// Files added (new files)
    pub added: Vec<PathBuf>,

    /// Files modified (content changed)
    pub modified: Vec<PathBuf>,

    /// Files deleted (removed)
    pub deleted: Vec<PathBuf>,

    /// Files renamed (`old_path`, `new_path`)
    pub renamed: Vec<(PathBuf, PathBuf)>,
}

impl ChangeSet {
    /// Create an empty change set
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the total number of changes
    #[must_use]
    pub fn total(&self) -> usize {
        self.added.len() + self.modified.len() + self.deleted.len() + self.renamed.len()
    }

    /// Returns true if there are no changes
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// Git backend abstraction for change detection
///
/// This trait provides a consistent interface for different git
/// backend implementations (subprocess, libgit2, etc.).
pub trait GitBackend: Send + Sync {
    /// Check if the path is a git repository
    ///
    /// Returns `Ok(true)` if path is a git repo, `Ok(false)` otherwise.
    /// Returns `Err` for permission errors or IO failures to surface
    /// them instead of silently falling back.
    ///
    /// # Errors
    ///
    /// Propagates [`GitError`] when repository detection fails (for example,
    /// when the directory cannot be accessed or git is unavailable).
    fn is_repo(&self, root: &Path) -> Result<bool>;

    /// Get the repository root path
    ///
    /// This handles git worktrees correctly (where `.git` is a file
    /// pointing to the actual git directory).
    ///
    /// Returns the canonicalized absolute path to the repository root.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] when git metadata cannot be inspected or when the
    /// path cannot be canonicalized.
    fn repo_root(&self, root: &Path) -> Result<PathBuf>;

    /// Get the current HEAD commit SHA
    ///
    /// Returns `Ok(Some(sha))` for repos with commits.
    /// Returns `Ok(None)` for newly initialized repos without commits.
    /// Returns `Err` for permission errors or command failures.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] when invoking git fails or when the output cannot
    /// be parsed.
    fn head(&self, root: &Path) -> Result<Option<String>>;

    /// Get uncommitted changes (index + working tree)
    ///
    /// Returns a tuple of `(ChangeSet, current_head)` to avoid race
    /// conditions between querying changes and querying HEAD separately.
    ///
    /// # Arguments
    ///
    /// * `root` - Repository root path
    /// * `include_untracked` - Whether to include untracked files
    ///
    /// # Returns
    ///
    /// * `ChangeSet` - Files changed in index and working tree
    /// * `Option<String>` - Current HEAD commit SHA (None if no commits)
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] when git commands fail, time out, or output parsing
    /// detects malformed entries.
    fn uncommitted(
        &self,
        root: &Path,
        include_untracked: bool,
    ) -> Result<(ChangeSet, Option<String>)>;

    /// Get changes since a baseline commit
    ///
    /// Returns a tuple of `(ChangeSet, current_head)` to avoid race
    /// conditions between diff and HEAD query.
    ///
    /// # Arguments
    ///
    /// * `root` - Repository root path
    /// * `baseline` - Baseline commit SHA to compare against
    /// * `rename_similarity` - Rename detection threshold (0-100)
    ///
    /// # Returns
    ///
    /// * `ChangeSet` - Files changed between baseline and HEAD
    /// * `Option<String>` - Current HEAD commit SHA (None if no commits)
    ///
    /// # Errors
    ///
    /// Returns `Err` if baseline commit doesn't exist (e.g., shallow clone
    /// where baseline was pruned). Caller should fall back to hash-based.
    fn since(
        &self,
        root: &Path,
        baseline: &str,
        rename_similarity: u8,
    ) -> Result<(ChangeSet, Option<String>)>;

    /// Get backend-specific capabilities
    ///
    /// This allows backends to advertise optional features like
    /// blame, time-travel indexing, etc. Used for enterprise features.
    fn capabilities(&self) -> GitCapabilities {
        GitCapabilities::default()
    }
}

/// Backend-specific capabilities
///
/// Allows different git backends to advertise their supported features.
/// For example, git2 backend can provide blame info, subprocess cannot.
#[derive(Debug, Clone, Default)]
pub struct GitCapabilities {
    /// Whether this backend supports blame overlays
    pub supports_blame: bool,

    /// Whether this backend supports time-travel indexing
    pub supports_time_travel: bool,

    /// Whether this backend supports historical indexing
    pub supports_history_index: bool,
}

/// High-level facade for git change tracking
///
/// This provides a convenient API over the `GitBackend` trait with
/// caching and error handling.
pub struct GitChangeTracker {
    backend: Box<dyn GitBackend>,
    root: PathBuf,
    cached_head: Option<String>,
}

impl GitChangeTracker {
    /// Create a new git change tracker
    ///
    /// Automatically selects the appropriate backend based on the
    /// `SQRY_GIT_BACKEND` environment variable:
    /// - `auto` (default): Use subprocess git, fall back to `NoGit`
    /// - `subprocess`: Force subprocess git (fail if git not found)
    /// - `none`: Always use `NoGit` (disable git integration)
    ///
    /// # Errors
    ///
    /// Returns `GitError::NotFound` if git binary is not in PATH and
    /// backend is set to `subprocess`.
    ///
    /// Returns `GitError::NotARepo` if path is not a git repository
    /// (only when backend is `subprocess` or `auto` with git available).
    pub fn new(root: &Path) -> Result<Self> {
        let backend_type = std::env::var("SQRY_GIT_BACKEND").unwrap_or_else(|_| "auto".to_string());

        let backend: Box<dyn GitBackend> = match backend_type.as_str() {
            "subprocess" => {
                let subprocess = SubprocessGit::new();
                if !subprocess.is_repo(root)? {
                    return Err(GitError::NotARepo(root.to_path_buf()));
                }
                Box::new(subprocess)
            }
            "none" => Box::new(NoGit),
            _ => {
                let subprocess = SubprocessGit::new();
                match subprocess.is_repo(root) {
                    Ok(true) => Box::new(subprocess),
                    Ok(false) => return Err(GitError::NotARepo(root.to_path_buf())),
                    Err(GitError::NotFound) => Box::new(NoGit),
                    Err(e) => return Err(e),
                }
            }
        };

        Ok(Self {
            backend,
            root: root.to_path_buf(),
            cached_head: None,
        })
    }

    /// Detect changes since last indexed commit
    ///
    /// If `baseline` is `None`, returns uncommitted changes only.
    /// If `baseline` is `Some(commit_sha)`, returns changes since that commit.
    ///
    /// # Returns
    ///
    /// * `ChangeSet` - Files changed
    /// * `Option<String>` - Current HEAD commit SHA (for updating baseline)
    ///
    /// # Configuration
    ///
    /// Respects these environment variables:
    /// - `SQRY_GIT_INCLUDE_UNTRACKED`: Include untracked files (default: 1)
    /// - `SQRY_GIT_RENAME_SIMILARITY`: Rename detection threshold 0-100 (default: 50)
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] when the underlying backend encounters IO failures,
    /// the repository is not available, or git command output is malformed.
    pub fn detect_changes(
        &mut self,
        baseline: Option<&str>,
    ) -> Result<(ChangeSet, Option<String>)> {
        let include_untracked = std::env::var("SQRY_GIT_INCLUDE_UNTRACKED")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            != Some(0);

        let rename_similarity = std::env::var("SQRY_GIT_RENAME_SIMILARITY")
            .ok()
            .and_then(|v| v.parse::<u8>().ok())
            .map_or(50, |v| v.clamp(0, 100));

        if let Some(baseline_sha) = baseline {
            let (changes, new_head) =
                self.backend
                    .since(&self.root, baseline_sha, rename_similarity)?;
            self.cached_head.clone_from(&new_head);
            Ok((changes, new_head))
        } else {
            let (changes, new_head) = self.backend.uncommitted(&self.root, include_untracked)?;
            self.cached_head.clone_from(&new_head);
            Ok((changes, new_head))
        }
    }

    /// Get the current HEAD commit SHA
    ///
    /// Uses cached value if available, otherwise queries git.
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] when requesting the HEAD from the backend fails.
    pub fn head(&mut self) -> Result<Option<String>> {
        if let Some(ref head) = self.cached_head {
            return Ok(Some(head.clone()));
        }

        let head = self.backend.head(&self.root)?;
        self.cached_head.clone_from(&head);
        Ok(head)
    }

    /// Get the repository root path
    ///
    /// # Errors
    ///
    /// Returns [`GitError`] when the backend cannot determine or canonicalize
    /// the root directory.
    pub fn repo_root(&self) -> Result<PathBuf> {
        self.backend.repo_root(&self.root)
    }

    /// Get backend capabilities
    #[must_use]
    pub fn capabilities(&self) -> GitCapabilities {
        self.backend.capabilities()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_changeset_total() {
        let mut changes = ChangeSet::new();
        assert_eq!(changes.total(), 0);
        assert!(changes.is_empty());

        changes.added.push(PathBuf::from("a.rs"));
        changes.modified.push(PathBuf::from("b.rs"));
        changes.deleted.push(PathBuf::from("c.rs"));
        changes
            .renamed
            .push((PathBuf::from("d.rs"), PathBuf::from("e.rs")));

        assert_eq!(changes.total(), 4);
        assert!(!changes.is_empty());
    }

    #[test]
    fn test_changeset_equality() {
        let mut changes1 = ChangeSet::new();
        changes1.added.push(PathBuf::from("a.rs"));

        let mut changes2 = ChangeSet::new();
        changes2.added.push(PathBuf::from("a.rs"));

        assert_eq!(changes1, changes2);

        changes2.modified.push(PathBuf::from("b.rs"));
        assert_ne!(changes1, changes2);
    }
}
