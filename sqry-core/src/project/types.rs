//! Foundation types for Project root lifecycle management
//!
//! Implements types from `PROJECT_ROOT_SPEC.md` and `02_DESIGN.md`:
//! - `ProjectRootMode`: How the Project root is determined
//! - `ProjectId`: Unique identifier for a Project
//! - `RepoId`: Unique identifier for a git repository within a Project
//! - `FileEntry`: Metadata for an indexed file

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// How the Project root is determined in LSP mode.
///
/// This setting only affects LSP path routing. CLI always uses explicit path.
///
/// Per `PROJECT_ROOT_SPEC.md` Section 2:
/// - **`GitRoot`** (default): Each git repository gets its own Project
/// - **`WorkspaceFolder`**: Each VS Code workspace folder gets a Project
/// - **`WorkspaceRoot`**: Single Project covering all workspace folders
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectRootMode {
    /// Each git repository (`.git` root) gets its own Project.
    ///
    /// Files without a git root fall back to workspace folder, then parent directory.
    #[default]
    GitRoot,

    /// Each workspace folder becomes a Project root, ignoring git boundaries.
    ///
    /// Useful for monorepos where you want folder-level isolation.
    WorkspaceFolder,

    /// Single Project covering all workspace folders.
    ///
    /// The first workspace folder is used as the `index_root`.
    /// Cross-repo edges are allowed; `RepoId` metadata distinguishes repositories.
    WorkspaceRoot,
}

impl ProjectRootMode {
    /// Parse from string (case-insensitive).
    ///
    /// Returns `None` for unrecognized values.
    #[must_use]
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "gitroot" | "git_root" | "git-root" => Some(Self::GitRoot),
            "workspacefolder" | "workspace_folder" | "workspace-folder" => {
                Some(Self::WorkspaceFolder)
            }
            "workspaceroot" | "workspace_root" | "workspace-root" => Some(Self::WorkspaceRoot),
            _ => None,
        }
    }

    /// Return the canonical string representation.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::GitRoot => "gitRoot",
            Self::WorkspaceFolder => "workspaceFolder",
            Self::WorkspaceRoot => "workspaceRoot",
        }
    }
}

impl std::fmt::Display for ProjectRootMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Unique identifier for a Project.
///
/// Generated from the canonical `index_root` path using xxh64 hash.
/// Stable across restarts for the same path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProjectId(u64);

impl ProjectId {
    /// Seed for xxh64 hashing - fixed for reproducibility
    /// Value chosen as ASCII: "SQRYPROJ" = `0x5351_5259_5052_4F4A`
    const HASH_SEED: u64 = 0x5351_5259_5052_4F4A;

    /// Generate `ProjectId` from an `index_root` path.
    ///
    /// Path MUST be canonicalized before calling.
    /// Uses xxh64 for stable cross-version hashing.
    #[must_use]
    pub fn from_index_root(index_root: &Path) -> Self {
        use xxhash_rust::xxh64::xxh64;

        // Use raw OS bytes to preserve non-UTF8 path components
        let path_bytes = index_root.as_os_str().as_encoded_bytes();
        let hash = xxh64(path_bytes, Self::HASH_SEED);

        ProjectId(hash)
    }

    /// Get the raw u64 value.
    #[must_use]
    pub const fn as_u64(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "proj_{:016x}", self.0)
    }
}

/// Unique identifier for a git repository within a Project.
///
/// Per `02_DESIGN.md` M2, uses xxh64 hash of canonical git root path
/// for stability across Rust versions (unlike `DefaultHasher`).
///
/// # Stability
///
/// `RepoId` values are stable across:
/// - Process restarts
/// - Rust compiler versions
/// - Operating systems (for the same path bytes)
///
/// This is critical because `RepoId` may be persisted in cache files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoId(u64);

impl RepoId {
    /// Seed for xxh64 hashing - fixed for reproducibility.
    ///
    /// Value chosen as ASCII: "SQRYREPO" = `0x5351_5259_5245_504F`
    /// Per `02_DESIGN.md` section M2 and review findings C7, C10.
    const HASH_SEED: u64 = 0x5351_5259_5245_504F;

    /// Sentinel value for files without a git root.
    pub const NONE: RepoId = RepoId(0);

    /// Generate `RepoId` from a git root path.
    ///
    /// Path MUST be canonicalized before calling.
    /// Uses xxh64 for stable cross-version hashing.
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::project::types::RepoId;
    /// use std::path::Path;
    ///
    /// let repo_id = RepoId::from_git_root(Path::new("/home/user/project"));
    /// assert_ne!(repo_id, RepoId::NONE);
    /// ```
    #[must_use]
    pub fn from_git_root(git_root: &Path) -> Self {
        use xxhash_rust::xxh64::xxh64;

        // Use raw OS bytes to preserve non-UTF8 path components
        // Per C7: as_os_str().as_encoded_bytes() handles non-UTF paths correctly
        let path_bytes = git_root.as_os_str().as_encoded_bytes();
        let hash = xxh64(path_bytes, Self::HASH_SEED);

        // Avoid collision with NONE sentinel
        if hash == 0 {
            RepoId(1) // Extremely unlikely (1 in 2^64), but handle it
        } else {
            RepoId(hash)
        }
    }

    /// Check if this is the sentinel "no repository" value.
    #[must_use]
    pub const fn is_none(&self) -> bool {
        self.0 == 0
    }

    /// Check if this represents an actual repository.
    #[must_use]
    pub const fn is_some(&self) -> bool {
        self.0 != 0
    }

    /// Get the raw u64 value.
    #[must_use]
    pub const fn as_u64(&self) -> u64 {
        self.0
    }
}

impl Default for RepoId {
    fn default() -> Self {
        Self::NONE
    }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_none() {
            write!(f, "repo_none")
        } else {
            write!(f, "repo_{:016x}", self.0)
        }
    }
}

/// Type alias for interned strings (path, language IDs, etc.)
///
/// Uses `Arc<str>` for cheap cloning and memory efficiency.
pub type StringId = Arc<str>;

/// Metadata for an indexed file within a Project.
///
/// Per `02_DESIGN.md`, `FileEntry` tracks:
/// - Project-relative path (interned for memory efficiency)
/// - Repository association (`RepoId`)
/// - Content hash for incremental change detection
/// - Last modification time
/// - Detected language
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Project-relative path (interned).
    ///
    /// Normalized to forward slashes, no trailing slashes.
    pub path: StringId,

    /// Repository this file belongs to.
    ///
    /// `RepoId::NONE` if the file has no git root in its ancestry.
    pub repo_id: RepoId,

    /// Content hash for change detection.
    ///
    /// `None` if file hasn't been indexed yet.
    pub content_hash: Option<u64>,

    /// Last modification time.
    ///
    /// `None` if stat failed or file is virtual.
    #[serde(with = "system_time_serde")]
    pub modified_at: Option<SystemTime>,

    /// Detected language ID (e.g., "rust", "python").
    ///
    /// `None` if language couldn't be detected.
    pub language_id: Option<StringId>,
}

impl FileEntry {
    /// Create a new `FileEntry` with minimal required fields.
    #[must_use]
    pub fn new(path: StringId, repo_id: RepoId) -> Self {
        Self {
            path,
            repo_id,
            content_hash: None,
            modified_at: None,
            language_id: None,
        }
    }

    /// Create with all fields populated.
    #[must_use]
    pub fn with_metadata(
        path: StringId,
        repo_id: RepoId,
        content_hash: Option<u64>,
        modified_at: Option<SystemTime>,
        language_id: Option<StringId>,
    ) -> Self {
        Self {
            path,
            repo_id,
            content_hash,
            modified_at,
            language_id,
        }
    }

    /// Check if the file belongs to a git repository.
    #[must_use]
    pub fn has_repo(&self) -> bool {
        self.repo_id.is_some()
    }
}

/// Serde support for `Option<SystemTime>`.
mod system_time_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    // Serde's serialize_with signature requires &Option<T> here.
    #[allow(clippy::ref_option)]
    pub fn serialize<S>(time: &Option<SystemTime>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match time {
            Some(t) => {
                let duration = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
                Some((duration.as_secs(), duration.subsec_nanos())).serialize(serializer)
            }
            None => None::<(u64, u32)>.serialize(serializer),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<SystemTime>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<(u64, u32)> = Option::deserialize(deserializer)?;
        Ok(opt.map(|(secs, nanos)| UNIX_EPOCH + Duration::new(secs, nanos)))
    }
}

/// Error types for Project operations.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    /// Failed to canonicalize a path.
    #[error("failed to canonicalize path '{path}': {source}")]
    CanonicalizationFailed {
        /// The path that couldn't be canonicalized.
        path: PathBuf,
        /// The underlying IO error.
        #[source]
        source: std::io::Error,
    },

    /// No git root found and fallback failed.
    #[error("no git root found for '{path}' and no fallback available")]
    NoGitRoot {
        /// The file path that has no git root.
        path: PathBuf,
    },

    /// File is outside all workspace folders.
    #[error("file '{path}' is outside all workspace folders")]
    FileOutsideWorkspace {
        /// The file path that's outside workspace.
        path: PathBuf,
    },

    /// Configuration error.
    #[error("configuration error: {message}")]
    Config {
        /// Description of the configuration issue.
        message: String,
    },

    /// Internal error (invariant violation).
    #[error("internal error: {message}")]
    Internal {
        /// Description of the internal error.
        message: String,
    },

    /// Failed to load the symbol index from disk (legacy).
    #[error("failed to load index from '{path}': {source}")]
    IndexLoad {
        /// The index root path.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: anyhow::Error,
    },

    /// Failed to load the unified graph from disk.
    #[error("failed to load graph from '{path}': {source}")]
    GraphLoad {
        /// The graph root path.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: anyhow::Error,
    },
}

impl ProjectError {
    /// Create a canonicalization error.
    pub fn canonicalization_failed(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::CanonicalizationFailed {
            path: path.into(),
            source,
        }
    }

    /// Create a no-git-root error.
    pub fn no_git_root(path: impl Into<PathBuf>) -> Self {
        Self::NoGitRoot { path: path.into() }
    }

    /// Create a file-outside-workspace error.
    pub fn file_outside_workspace(path: impl Into<PathBuf>) -> Self {
        Self::FileOutsideWorkspace { path: path.into() }
    }

    /// Create a configuration error.
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
        }
    }

    /// Create an internal error.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_root_mode_default() {
        assert_eq!(ProjectRootMode::default(), ProjectRootMode::GitRoot);
    }

    #[test]
    fn test_project_root_mode_from_str() {
        assert_eq!(
            ProjectRootMode::from_str_opt("gitRoot"),
            Some(ProjectRootMode::GitRoot)
        );
        assert_eq!(
            ProjectRootMode::from_str_opt("git_root"),
            Some(ProjectRootMode::GitRoot)
        );
        assert_eq!(
            ProjectRootMode::from_str_opt("workspaceFolder"),
            Some(ProjectRootMode::WorkspaceFolder)
        );
        assert_eq!(
            ProjectRootMode::from_str_opt("workspaceRoot"),
            Some(ProjectRootMode::WorkspaceRoot)
        );
        assert_eq!(ProjectRootMode::from_str_opt("invalid"), None);
    }

    #[test]
    fn test_project_root_mode_as_str() {
        assert_eq!(ProjectRootMode::GitRoot.as_str(), "gitRoot");
        assert_eq!(ProjectRootMode::WorkspaceFolder.as_str(), "workspaceFolder");
        assert_eq!(ProjectRootMode::WorkspaceRoot.as_str(), "workspaceRoot");
    }

    #[test]
    fn test_project_id_deterministic() {
        let path = Path::new("/home/user/project");
        let id1 = ProjectId::from_index_root(path);
        let id2 = ProjectId::from_index_root(path);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_project_id_different_paths() {
        let id1 = ProjectId::from_index_root(Path::new("/home/user/project1"));
        let id2 = ProjectId::from_index_root(Path::new("/home/user/project2"));
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_repo_id_deterministic() {
        let path = Path::new("/home/user/repo");
        let id1 = RepoId::from_git_root(path);
        let id2 = RepoId::from_git_root(path);
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_repo_id_different_paths() {
        let id1 = RepoId::from_git_root(Path::new("/home/user/repo1"));
        let id2 = RepoId::from_git_root(Path::new("/home/user/repo2"));
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_repo_id_none() {
        assert!(RepoId::NONE.is_none());
        assert!(!RepoId::NONE.is_some());
        assert_eq!(RepoId::NONE.as_u64(), 0);
    }

    #[test]
    fn test_repo_id_from_path_is_some() {
        let id = RepoId::from_git_root(Path::new("/any/path"));
        assert!(id.is_some());
        assert!(!id.is_none());
    }

    #[test]
    fn test_repo_id_display() {
        assert_eq!(format!("{}", RepoId::NONE), "repo_none");
        let id = RepoId::from_git_root(Path::new("/test"));
        assert!(format!("{id}").starts_with("repo_"));
    }

    #[test]
    fn test_repo_id_hash_seed_is_sqryrepo() {
        // Verify the seed matches "SQRYREPO" in ASCII
        let expected_bytes = b"SQRYREPO";
        let mut expected: u64 = 0;
        for &byte in expected_bytes {
            expected = (expected << 8) | u64::from(byte);
        }
        assert_eq!(RepoId::HASH_SEED, expected);
    }

    #[test]
    fn test_file_entry_creation() {
        let path: StringId = Arc::from("src/main.rs");
        let repo_id = RepoId::from_git_root(Path::new("/repo"));

        let entry = FileEntry::new(Arc::clone(&path), repo_id);

        assert_eq!(*entry.path, *path);
        assert_eq!(entry.repo_id, repo_id);
        assert!(entry.content_hash.is_none());
        assert!(entry.modified_at.is_none());
        assert!(entry.language_id.is_none());
        assert!(entry.has_repo());
    }

    #[test]
    fn test_file_entry_no_repo() {
        let path: StringId = Arc::from("outside/file.txt");
        let entry = FileEntry::new(path, RepoId::NONE);
        assert!(!entry.has_repo());
    }

    #[test]
    fn test_file_entry_with_metadata() {
        let path: StringId = Arc::from("src/lib.rs");
        let repo_id = RepoId::from_git_root(Path::new("/repo"));
        let lang: StringId = Arc::from("rust");
        let now = SystemTime::now();

        let entry = FileEntry::with_metadata(
            Arc::clone(&path),
            repo_id,
            Some(0x1234_5678_9abc_def0),
            Some(now),
            Some(Arc::clone(&lang)),
        );

        assert_eq!(entry.content_hash, Some(0x1234_5678_9abc_def0));
        assert!(entry.modified_at.is_some());
        assert_eq!(entry.language_id.as_deref(), Some("rust"));
    }

    #[cfg(unix)]
    #[test]
    fn test_repo_id_deterministic_for_non_utf8_path() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        // Create a path with non-UTF8 bytes (invalid UTF-8 sequence)
        let bytes: &[u8] = b"/home/\xff\xfe/repo";
        let os_str = OsStr::from_bytes(bytes);
        let path = Path::new(os_str);

        // Should be deterministic
        let id1 = RepoId::from_git_root(path);
        let id2 = RepoId::from_git_root(path);
        assert_eq!(id1, id2);

        // Should be a valid RepoId (not NONE)
        assert!(id1.is_some());
    }
}
