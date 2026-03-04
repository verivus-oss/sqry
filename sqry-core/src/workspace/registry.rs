//! Workspace registry data structures and persistence helpers.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::error::{WorkspaceError, WorkspaceResult};
use super::serde_time;

/// Current on-disk registry format version.
pub const WORKSPACE_REGISTRY_VERSION: u32 = 1;

/// Serializable workspace registry stored in `.sqry-workspace`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRegistry {
    /// Registry metadata (versioning, timestamps).
    pub metadata: WorkspaceMetadata,
    /// Registered repositories.
    #[serde(default)]
    pub repositories: Vec<WorkspaceRepository>,
}

impl WorkspaceRegistry {
    /// Construct a new empty registry.
    #[must_use]
    pub fn new(workspace_name: Option<String>) -> Self {
        Self {
            metadata: WorkspaceMetadata::new(workspace_name),
            repositories: Vec::new(),
        }
    }

    /// Load a registry from `path`.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the file cannot be read, parsed, or when the version is unsupported.
    pub fn load(path: &Path) -> WorkspaceResult<Self> {
        let file = File::open(path).map_err(|err| WorkspaceError::io(path, err))?;
        let mut registry: WorkspaceRegistry =
            serde_json::from_reader(file).map_err(WorkspaceError::Serialization)?;

        if registry.metadata.version != WORKSPACE_REGISTRY_VERSION {
            return Err(WorkspaceError::UnsupportedVersion {
                found: registry.metadata.version,
                expected: WORKSPACE_REGISTRY_VERSION,
            });
        }

        registry.sort_repositories();
        Ok(registry)
    }

    /// Persist the registry to `path`, creating parent directories if necessary.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when directories cannot be created or serialization fails.
    pub fn save(&mut self, path: &Path) -> WorkspaceResult<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| WorkspaceError::io(parent, err))?;
        }

        self.sort_repositories();
        self.metadata.touch_updated();

        let file = File::create(path).map_err(|err| WorkspaceError::io(path, err))?;
        serde_json::to_writer_pretty(file, self).map_err(WorkspaceError::Serialization)
    }

    /// Insert or update a repository entry.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when persistence metadata updates fail (currently infallible placeholder).
    pub fn upsert_repo(&mut self, repo: WorkspaceRepository) -> WorkspaceResult<()> {
        let id = repo.id.clone();

        if let Some(existing) = self
            .repositories
            .iter_mut()
            .find(|existing| existing.id == id)
        {
            *existing = repo;
        } else {
            self.repositories.push(repo);
        }

        self.metadata.touch_updated();
        Ok(())
    }

    /// Remove a repository by id. Returns `true` if an entry was removed.
    pub fn remove_repo(&mut self, repo_id: &WorkspaceRepoId) -> bool {
        let len_before = self.repositories.len();
        self.repositories.retain(|repo| repo.id != *repo_id);
        let removed = self.repositories.len() != len_before;

        if removed {
            self.metadata.touch_updated();
        }

        removed
    }

    fn sort_repositories(&mut self) {
        self.repositories.sort_by(|a, b| a.id.cmp(&b.id));
    }

    /// Returns an ordered map keyed by repo id (primarily for testing/introspection).
    #[must_use]
    pub fn as_map(&self) -> BTreeMap<&WorkspaceRepoId, &WorkspaceRepository> {
        self.repositories
            .iter()
            .map(|repo| (&repo.id, repo))
            .collect()
    }
}

/// Registry metadata including versioning and timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceMetadata {
    /// Registry schema version.
    pub version: u32,
    /// Optional human-friendly name for the workspace.
    pub workspace_name: Option<String>,
    /// Preferred discovery mode for scans (`index-files`, `git-roots`, etc.).
    #[serde(default)]
    pub default_discovery_mode: Option<String>,
    /// Timestamp when the registry was created.
    #[serde(with = "serde_time")]
    pub created_at: SystemTime,
    /// Timestamp when the registry was last updated.
    #[serde(with = "serde_time")]
    pub updated_at: SystemTime,
}

impl WorkspaceMetadata {
    fn new(workspace_name: Option<String>) -> Self {
        let now = SystemTime::now();
        Self {
            version: WORKSPACE_REGISTRY_VERSION,
            workspace_name,
            default_discovery_mode: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn touch_updated(&mut self) {
        self.updated_at = SystemTime::now();
    }
}

/// Identifier for registered repositories (workspace-relative path).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorkspaceRepoId(String);

impl WorkspaceRepoId {
    /// Create an identifier from a workspace-relative path.
    pub fn new(relative: impl AsRef<Path>) -> WorkspaceRepoId {
        let path = relative.as_ref();

        let normalized = if path.components().count() == 0 {
            ".".to_string()
        } else {
            path.components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        };

        WorkspaceRepoId(normalized)
    }

    /// Access the underlying identifier as str.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkspaceRepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Repository entry stored in the workspace registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRepository {
    /// Stable identifier (workspace-relative path).
    pub id: WorkspaceRepoId,
    /// Friendly display name.
    pub name: String,
    /// Absolute path to repository root.
    pub root: PathBuf,
    /// Path to serialized index data.
    pub index_path: PathBuf,
    /// Optional timestamp when the index was most recently updated.
    #[serde(with = "serde_time::option")]
    pub last_indexed_at: Option<SystemTime>,
    /// Optional cached symbol count.
    pub symbol_count: Option<u64>,
    /// Optional primary language for the repository.
    pub primary_language: Option<String>,
}

impl WorkspaceRepository {
    /// Create a repository entry with default metadata placeholders.
    #[must_use]
    pub fn new(
        id: WorkspaceRepoId,
        name: String,
        root: PathBuf,
        index_path: PathBuf,
        last_indexed_at: Option<SystemTime>,
    ) -> Self {
        Self {
            id,
            name,
            root,
            index_path,
            last_indexed_at,
            symbol_count: None,
            primary_language: None,
        }
    }
}
