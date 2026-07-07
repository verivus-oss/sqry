//! Workspace registry data structures and persistence helpers.
//!
//! ## On-disk schema versions
//!
//! - **v1** (legacy): single flat `repositories` array. Carries
//!   `metadata { version: 1 }` and a list of [`WorkspaceRepository`] entries.
//! - **v2** (current): `source_roots` (renamed from `repositories`),
//!   `member_folders`, `exclusions`, `project_root_mode`. Carries
//!   `metadata { version: 2 }`.
//!
//! ## Upgrade path
//!
//! Loading a v1 file via [`WorkspaceRegistry::load`] auto-upgrades it in
//! memory: each v1 repository entry becomes a v2 source root, and the
//! v2-only collections are initialized empty / default. The next
//! [`WorkspaceRegistry::save`] persists v2.
//!
//! Loading a file with `metadata.version > 2` returns
//! [`WorkspaceError::UnsupportedVersion`]; we never silently downgrade
//! a future schema.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::error::{WorkspaceError, WorkspaceResult};
use super::logical::MemberReason;
use super::serde_time;
use crate::graph::unified::persistence::{GraphStorage, ManifestCheck};
use crate::project::types::ProjectRootMode;

/// Current on-disk registry format version.
pub const WORKSPACE_REGISTRY_VERSION: u32 = 2;

/// Serializable workspace registry stored in `.sqry-workspace`.
///
/// On-disk layout corresponds to schema **v2** — see the module-level
/// docs for the v1 → v2 upgrade contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRegistry {
    /// Registry metadata (versioning, timestamps).
    pub metadata: WorkspaceMetadata,
    /// Auto-indexed source roots. Persisted under the `source_roots`
    /// JSON key in v2; deserialization also accepts the v1
    /// `repositories` key (see [`Self::load`]).
    #[serde(default, rename = "source_roots", alias = "repositories")]
    pub repositories: Vec<WorkspaceRepository>,
    /// Workspace member folders — part of the workspace but **not**
    /// auto-indexed (reads still resolve through the workspace's source
    /// roots). Empty in v1.
    #[serde(default)]
    pub member_folders: Vec<WorkspaceMemberFolder>,
    /// Explicitly excluded paths (opaque to sqry). Empty in v1.
    #[serde(default)]
    pub exclusions: Vec<PathBuf>,
    /// Workspace-scoped project-root resolution mode.
    /// Defaults to [`ProjectRootMode::default`] (= `GitRoot`) when absent.
    #[serde(default)]
    pub project_root_mode: ProjectRootMode,
}

impl WorkspaceRegistry {
    /// Construct a new empty registry at the current schema version.
    #[must_use]
    pub fn new(workspace_name: Option<String>) -> Self {
        Self {
            metadata: WorkspaceMetadata::new(workspace_name),
            repositories: Vec::new(),
            member_folders: Vec::new(),
            exclusions: Vec::new(),
            project_root_mode: ProjectRootMode::default(),
        }
    }

    /// Load a registry from `path`.
    ///
    /// Schema v1 files (single-flat `repositories` array) are auto-upgraded
    /// to v2 in memory: existing entries become source roots; member-folder,
    /// exclusion, and project-root-mode fields are initialised to their
    /// defaults. Files with `metadata.version > 2` are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError::Io`] when the file cannot be read,
    /// [`WorkspaceError::Serialization`] when the JSON is malformed, and
    /// [`WorkspaceError::UnsupportedVersion`] when the on-disk version is
    /// newer than this build understands.
    pub fn load(path: &Path) -> WorkspaceResult<Self> {
        let file = File::open(path).map_err(|err| WorkspaceError::io(path, err))?;
        let mut registry: WorkspaceRegistry =
            serde_json::from_reader(file).map_err(WorkspaceError::Serialization)?;

        match registry.metadata.version {
            1 => {
                // v1 → v2 upgrade: keep already-deserialized `repositories`
                // (the `alias = "repositories"` lets us read the v1 key as
                // `source_roots`), default the v2-only collections, and
                // bump the in-memory version. The next `save()` writes v2.
                registry.member_folders = Vec::new();
                registry.exclusions = Vec::new();
                registry.project_root_mode = ProjectRootMode::default();
                registry.metadata.version = WORKSPACE_REGISTRY_VERSION;
            }
            2 => {}
            other => {
                return Err(WorkspaceError::UnsupportedVersion {
                    found: other,
                    expected: WORKSPACE_REGISTRY_VERSION,
                });
            }
        }

        registry.sort_repositories();
        Ok(registry)
    }

    /// Persist the registry to `path`, creating parent directories if
    /// necessary. Always writes the current ([`WORKSPACE_REGISTRY_VERSION`])
    /// schema regardless of how the in-memory representation was loaded.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when directories cannot be created or
    /// serialization fails.
    pub fn save(&mut self, path: &Path) -> WorkspaceResult<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| WorkspaceError::io(parent, err))?;
        }

        self.sort_repositories();
        self.metadata.version = WORKSPACE_REGISTRY_VERSION;
        self.metadata.touch_updated();

        let file = File::create(path).map_err(|err| WorkspaceError::io(path, err))?;
        serde_json::to_writer_pretty(file, self).map_err(WorkspaceError::Serialization)
    }

    /// Insert or update a source-root repository entry.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when persistence metadata updates fail
    /// (currently infallible placeholder).
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

    /// Remove a source-root repository by id. Returns `true` if an entry
    /// was removed.
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
        self.member_folders.sort_by(|a, b| a.id.cmp(&b.id));
        self.exclusions.sort();
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
    /// Symbol count observed the last time this repository was
    /// registered (`discover_repositories` / `sqry workspace add`),
    /// read once from `.sqry/graph/manifest.json` at that moment via
    /// [`WorkspaceRepository::symbol_count_from_manifest`].
    ///
    /// This is a point-in-time snapshot, not a live value: it goes
    /// stale the moment the member is reindexed directly (`sqry index
    /// --force`) without a matching `workspace remove` + `workspace
    /// add` cycle. `sqry workspace stats`
    /// ([`super::stats::DetailedWorkspaceStats::from_registry`]) does
    /// **not** read this field for that reason; it re-reads each
    /// member's manifest live at stats-computation time instead. This
    /// field is kept for [`super::index::WorkspaceIndex::stats`] (the
    /// coarse, non-detailed stats API) and for any display surface that
    /// wants a cheap last-known count without touching the filesystem
    /// again. On disk the JSON key stays `symbol_count` for backward
    /// compatibility with existing `.sqry-workspace` registry files.
    #[serde(rename = "symbol_count")]
    pub symbol_count_at_registration: Option<u64>,
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
            symbol_count_at_registration: None,
            primary_language: None,
        }
    }

    /// Reads the symbol count for `repo_root` from its
    /// `.sqry/graph/manifest.json` sidecar.
    ///
    /// This is the same cheap per-repo metadata file (no full graph
    /// snapshot load) that `sqry graph status --json` surfaces as its
    /// `symbol_count` field (mapped from `Manifest::node_count`, see
    /// [`crate::graph::unified::persistence::manifest::Manifest`]).
    /// Two distinct call sites use it:
    ///
    /// - `discover_repositories` and `sqry workspace add` call it once,
    ///   at registration time, to populate
    ///   [`WorkspaceRepository::symbol_count_at_registration`] (a
    ///   last-known snapshot).
    /// - [`super::stats::DetailedWorkspaceStats::from_registry`] calls
    ///   it again, live, for every indexed member each time `sqry
    ///   workspace stats` runs, so a direct `sqry index --force`
    ///   reindex is reflected immediately without requiring a
    ///   `workspace remove` + `workspace add` round-trip to refresh the
    ///   registration-time cache.
    ///
    /// Returns `None` when the manifest is missing (race with an
    /// in-progress build) or corrupt/unreadable. Callers must not treat
    /// `None` as "zero symbols": [`super::stats::DetailedWorkspaceStats`]
    /// tracks unreadable manifests separately via
    /// `unknown_symbol_count_repos`, so aggregate totals are never
    /// silently understated.
    #[must_use]
    pub fn symbol_count_from_manifest(repo_root: &Path) -> Option<u64> {
        match GraphStorage::new(repo_root).try_load_manifest() {
            ManifestCheck::Present(manifest) => {
                Some(u64::try_from(manifest.node_count).unwrap_or(u64::MAX))
            }
            ManifestCheck::Missing => None,
            ManifestCheck::Corrupt(err) => {
                log::warn!(
                    "workspace: manifest.json for {} is corrupt or unreadable, symbol count unknown: {err}",
                    repo_root.display()
                );
                None
            }
        }
    }
}

/// Persisted member-folder entry. Mirrors
/// [`super::logical::MemberFolder`] but is keyed by a stable
/// workspace-relative identifier so it round-trips through the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMemberFolder {
    /// Stable identifier (workspace-relative path).
    pub id: WorkspaceRepoId,
    /// Absolute path to the member folder root.
    pub root: PathBuf,
    /// Why the folder was classified as a member.
    pub reason: MemberReason,
}

impl WorkspaceMemberFolder {
    /// Create a member folder entry.
    #[must_use]
    pub fn new(id: WorkspaceRepoId, root: PathBuf, reason: MemberReason) -> Self {
        Self { id, root, reason }
    }
}
