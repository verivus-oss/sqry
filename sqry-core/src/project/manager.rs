//! Project lifecycle management with thread-safe access
//!
//! Implements the `ProjectManager` from `02_DESIGN.md`:
//! - Routes file paths to Projects based on `ProjectRootMode`
//! - Manages Project creation/destruction lifecycle
//! - Thread-safe with `RwLock` + double-check pattern (C5)
//!
//! # Thread Safety
//!
//! `ProjectManager` uses `parking_lot::RwLock` with double-check pattern:
//! 1. Read lock: Fast path for existing Projects
//! 2. Write lock: Slow path with double-check for creation

use super::path_utils::canonicalize_path;
use super::persistence::{
    ProjectPersistence, build_persisted_state, compute_config_fingerprint, restore_file_table,
    restore_repo_index,
};
use super::repo_detection::{detect_repos_under, lookup_repo_id};
use super::resolver::resolve_index_root;
use super::types::{FileEntry, ProjectError, ProjectId, ProjectRootMode, RepoId, StringId};
use crate::config::ProjectConfig;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::persistence::{GraphStorage, load_from_path};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A Project represents a single code base with its own `CodeGraph` and caches.
///
/// Per `PROJECT_ROOT_SPEC` and `02_DESIGN.md`, each Project:
/// - Has a unique canonical `index_root`
/// - Owns a `CodeGraph` (future enhancement), symbol/index caches
/// - Tracks repository metadata via `RepoIndex`
/// - Manages file metadata via `FileTable`
/// - Loads configuration from `.sqry-config.toml`
// Manual Debug impl because CodeGraph doesn't implement Debug
#[allow(missing_debug_implementations)]
impl std::fmt::Debug for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Project")
            .field("id", &self.id)
            .field("index_root", &self.index_root)
            .field("config", &self.config)
            .field("repo_index", &"<repo_index>")
            .field("file_table", &"<file_table>")
            .field(
                "graph_cache",
                &self.graph_cache.read().as_ref().map(|_| "<cached>"),
            )
            .field("initialized", &self.initialized.load(Ordering::Relaxed))
            .field("cancelled", &self.cancelled.load(Ordering::Relaxed))
            .finish()
    }
}

/// Represents a single code base with its own `CodeGraph` and caches.
pub struct Project {
    /// Unique identifier for this Project.
    pub id: ProjectId,

    /// Canonical path to the project root.
    pub index_root: PathBuf,

    /// Project configuration loaded from `.sqry-config.toml`.
    ///
    /// Loaded from `index_root` ancestor walk per `02_DESIGN.md` [M3].
    config: ProjectConfig,

    /// Repository index: maps git root paths to `RepoIds`.
    repo_index: RwLock<HashMap<PathBuf, RepoId>>,

    /// File table: maps relative paths to `FileEntry` metadata.
    file_table: RwLock<HashMap<StringId, FileEntry>>,

    /// Cached unified graph for this project.
    ///
    /// Each Project caches its own `CodeGraph` to avoid disk reloads on every
    /// handler request.
    graph_cache: RwLock<Option<Arc<CodeGraph>>>,

    /// Whether this Project has been initialized.
    initialized: AtomicBool,

    /// Cancellation flag for background operations.
    cancelled: AtomicBool,
}

impl Project {
    /// Create a new Project shell.
    ///
    /// The Project is created but not initialized - call `initialize()` to set up
    /// the `CodeGraph` and caches.
    ///
    /// Configuration is loaded from `.sqry-config.toml` by walking up from
    /// `index_root` per `02_DESIGN.md` \[M3\].
    ///
    /// # Errors
    ///
    /// Returns an error if project configuration fails to load or validate.
    pub fn new(index_root: PathBuf) -> Result<Self, ProjectError> {
        let id = ProjectId::from_index_root(&index_root);

        // Load configuration from .sqry-config.toml (ancestor walk)
        let config = ProjectConfig::load_from_index_root(&index_root);

        log::debug!(
            "Created Project {} for root '{}' (config: max_depth={}, cache={})",
            id,
            index_root.display(),
            config.indexing.max_depth,
            config.cache.directory
        );

        Ok(Self {
            id,
            index_root,
            config,
            repo_index: RwLock::new(HashMap::new()),
            file_table: RwLock::new(HashMap::new()),
            graph_cache: RwLock::new(None),
            initialized: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        })
    }

    /// Initialize the Project (detect repos, prepare graph caching).
    ///
    /// This is separate from `new()` to allow lazy initialization.
    /// Per `PROJECT_ROOT_SPEC.md` Section 6.1-6.3, initialization includes:
    /// 1. Attempts to load persisted state (if `cache.persistent = true`)
    /// 2. Falls back to detecting all git repositories under the `index_root`
    /// 3. Populates the `repo_index` with `RepoIds`
    /// 4. Graph caching is lazy-loaded via `graph()` method
    ///
    /// Note: File watchers are handled by the LSP layer, not the Project.
    ///
    /// # Errors
    ///
    /// Returns an error if repository discovery or persistence initialization fails.
    pub fn initialize(&self) -> Result<(), ProjectError> {
        if self.initialized.load(Ordering::Acquire) {
            return Ok(()); // Already initialized (idempotent)
        }

        log::info!(
            "Initializing Project {} at '{}'",
            self.id,
            self.index_root.display()
        );

        // Step 1: Try to load persisted state (if enabled)
        let mut preloaded = false;
        if self.config.cache.persistent {
            preloaded = self.try_preload_state();
        }

        // Step 2: Fall back to repository detection if preload failed/skipped
        if !preloaded {
            self.detect_repositories();
        }

        // Step 3: Graph caching is set up but lazy-loaded via graph() method
        // This avoids loading graphs for projects that may never be queried
        log::debug!(
            "Project {} graph cache ready (lazy-loaded on first access)",
            self.id
        );

        self.initialized.store(true, Ordering::Release);
        Ok(())
    }

    /// Try to preload state from persisted metadata.
    ///
    /// Returns `true` if state was successfully loaded, `false` if preload
    /// was skipped or failed (caller should fall back to detection).
    fn try_preload_state(&self) -> bool {
        let persistence = ProjectPersistence::new(&self.index_root, &self.config.cache.directory);

        // Try to read persisted state
        let state = match persistence.read_metadata(self.id) {
            Ok(Some(s)) => s,
            Ok(None) => {
                log::debug!("No persisted state found for Project {}", self.id);
                return false;
            }
            Err(e) => {
                log::warn!(
                    "Failed to read persisted state for Project {}: {}",
                    self.id,
                    e
                );
                return false;
            }
        };

        // Validate version
        if state.version != 1 {
            log::warn!(
                "Persisted state version mismatch for Project {}: expected 1, got {}",
                self.id,
                state.version
            );
            return false;
        }

        // Validate project_id
        if state.project_id != self.id.as_u64() {
            log::warn!(
                "Persisted state project_id mismatch for Project {}",
                self.id
            );
            return false;
        }

        // Validate config fingerprint
        let current_fingerprint =
            compute_config_fingerprint(&self.config.cache, &self.config.indexing);
        if state.config_fingerprint != current_fingerprint {
            log::warn!(
                "Persisted state config fingerprint mismatch for Project {} \
                 (config changed since last persist)",
                self.id
            );
            return false;
        }

        // Restore repo_index
        let repo_index = restore_repo_index(&state);
        *self.repo_index.write() = repo_index;

        // Restore file_table
        let file_table = restore_file_table(&state);
        *self.file_table.write() = file_table;

        log::info!(
            "Preloaded persisted state for Project {} ({} repos, {} files)",
            self.id,
            self.repo_index.read().len(),
            self.file_table.read().len()
        );

        true
    }

    /// Detect all git repositories under the `index_root` and populate `repo_index`.
    ///
    /// Per `02_DESIGN.md`:
    /// - Walks the `index_root` looking for .git directories
    /// - Assigns `RepoIds` to each detected repository
    /// - Handles nested repos (all are tracked independently)
    fn detect_repositories(&self) {
        let repos = detect_repos_under(&self.index_root);

        let mut index = self.repo_index.write();
        for (git_root, repo_id) in repos {
            index.insert(git_root, repo_id);
        }

        log::debug!(
            "Project {} detected {} repositor{}",
            self.id,
            index.len(),
            if index.len() == 1 { "y" } else { "ies" }
        );
    }

    /// Get the `RepoId` for a file path using the cached repo index.
    ///
    /// Implements "nearest .git wins" rule per `02_DESIGN.md` M2:
    /// - Uses the `repo_index` populated during `initialize()`
    /// - Returns `RepoId::NONE` if no git root found in ancestry
    ///
    /// # Arguments
    ///
    /// * `file_path` - Path to the file (will be canonicalized)
    ///
    /// # Errors
    ///
    /// Returns an error if path canonicalization fails.
    pub fn repo_id_for_file(&self, file_path: &Path) -> Result<RepoId, ProjectError> {
        let canonical = canonicalize_path(file_path)
            .map_err(|e| ProjectError::canonicalization_failed(file_path, e))?;

        let repo_index = self.repo_index.read();
        Ok(lookup_repo_id(&canonical, &repo_index))
    }

    /// Get the cached repo index (for advanced use cases).
    ///
    /// Returns a copy of the repo index mapping git root paths to `RepoIds`.
    #[must_use]
    pub fn repo_index(&self) -> HashMap<PathBuf, RepoId> {
        self.repo_index.read().clone()
    }

    /// Check if the Project has been initialized.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    /// Cancel any in-progress background operations.
    pub fn cancel_operations(&self) {
        self.cancelled.store(true, Ordering::Release);
        log::debug!("Cancelled operations for Project {}", self.id);
    }

    /// Load (or reuse) the cached unified graph for this project.
    ///
    /// Each Project caches its own `CodeGraph` to avoid repeated disk I/O
    /// on every handler request.
    ///
    /// # Errors
    ///
    /// Returns an error when the graph cannot be loaded from disk.
    pub fn graph(&self) -> Result<Option<Arc<CodeGraph>>, ProjectError> {
        // Fast path: return cached graph if available
        if let Some(graph) = self.graph_cache.read().as_ref() {
            return Ok(Some(graph.clone()));
        }

        // Slow path: load from disk
        let storage = GraphStorage::new(&self.index_root);
        if !storage.exists() {
            return Ok(None);
        }

        let graph =
            load_from_path(storage.snapshot_path(), None).map_err(|e| ProjectError::GraphLoad {
                path: self.index_root.clone(),
                source: e.into(),
            })?;

        let graph = Arc::new(graph);

        // Cache the loaded graph
        let mut cache = self.graph_cache.write();
        *cache = Some(graph.clone());

        Ok(Some(graph))
    }

    /// Clear the cached unified graph.
    ///
    /// Call this when the graph is rebuilt or invalidated.
    pub fn clear_graph_cache(&self) {
        let mut cache = self.graph_cache.write();
        *cache = None;
        log::debug!("Cleared graph cache for Project {}", self.id);
    }

    /// Check if operations have been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Register a repository in this Project's repo index.
    pub fn register_repo(&self, git_root: PathBuf, repo_id: RepoId) {
        let mut index = self.repo_index.write();
        index.insert(git_root, repo_id);
    }

    /// Get the `RepoId` for a git root path.
    #[must_use]
    pub fn get_repo_id(&self, git_root: &Path) -> Option<RepoId> {
        let index = self.repo_index.read();
        index.get(git_root).copied()
    }

    /// Register a file in this Project's file table.
    pub fn register_file(&self, entry: FileEntry) {
        let mut table = self.file_table.write();
        table.insert(Arc::clone(&entry.path), entry);
    }

    /// Get a file entry by path.
    #[must_use]
    pub fn get_file(&self, path: &str) -> Option<FileEntry> {
        let table = self.file_table.read();
        table.get(path).cloned()
    }

    /// Get the number of registered files.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.file_table.read().len()
    }

    /// Get the number of registered repositories.
    #[must_use]
    pub fn repo_count(&self) -> usize {
        self.repo_index.read().len()
    }

    /// Persist state if configured.
    ///
    /// When `cache.persistent = true`:
    /// 1. Persists `repo_index` + `file_table` as versioned JSON
    ///
    /// Per `02_DESIGN.md`, uses atomic temp + rename for safety.
    ///
    /// # Errors
    ///
    /// Logs warnings on failure but does not panic. Returns quietly
    /// to allow shutdown to proceed.
    pub fn persist_if_configured(&self) {
        // Early exit if persistence disabled
        if !self.config.cache.persistent {
            log::debug!(
                "Persistence disabled for Project {} (cache.persistent=false)",
                self.id
            );
            return;
        }

        log::info!("Persisting state for Project {}", self.id);

        // Compute config fingerprint for invalidation detection
        let fingerprint = compute_config_fingerprint(&self.config.cache, &self.config.indexing);

        // Build persistence helper
        let persistence = ProjectPersistence::new(&self.index_root, &self.config.cache.directory);

        // Snapshot repo_index and file_table under read locks
        let repo_index = self.repo_index.read().clone();
        let file_table = self.file_table.read().clone();

        // Build persisted state
        let state = build_persisted_state(
            self.id,
            &self.index_root,
            fingerprint,
            &repo_index,
            &file_table,
        );

        // Write metadata
        if let Err(e) = persistence.write_metadata(&state) {
            log::warn!("Failed to persist metadata for Project {}: {}", self.id, e);
        }
    }

    /// Get the project configuration.
    ///
    /// Returns a reference to the loaded `.sqry-config.toml` configuration.
    #[must_use]
    pub fn config(&self) -> &ProjectConfig {
        &self.config
    }

    /// Get the effective ignored directories for this project.
    ///
    /// Combines default ignored directories with any additional directories
    /// specified in the project configuration.
    #[must_use]
    pub fn effective_ignored_dirs(&self) -> Vec<&str> {
        self.config.effective_ignored_dirs()
    }

    /// Check if a path should be ignored during indexing.
    ///
    /// Returns true if the path matches any ignore pattern (and doesn't match
    /// an override include pattern).
    #[must_use]
    pub fn is_path_ignored(&self, path: &Path) -> bool {
        self.config.is_ignored(path)
    }

    /// Get the language ID for a path based on configuration hints.
    ///
    /// Returns the configured language if found, or None for default detection.
    #[must_use]
    pub fn language_for_path(&self, path: &Path) -> Option<&str> {
        self.config.language_for_path(path)
    }
}

/// Manages multiple Projects and routes file paths to the appropriate Project.
///
/// Per `02_DESIGN.md`, `ProjectManager`:
/// - Resolves file paths to Projects based on `ProjectRootMode`
/// - Creates Projects on demand (lazy)
/// - Handles configuration changes (mode switch = teardown + rebuild)
/// - Thread-safe with `RwLock` + double-check pattern
#[derive(Debug)]
pub struct ProjectManager {
    /// Current resolution mode.
    mode: RwLock<ProjectRootMode>,

    /// Map from `index_root` to Project.
    projects: RwLock<HashMap<PathBuf, Arc<Project>>>,

    /// Workspace folders (LSP order, not alphabetical).
    workspace_folders: RwLock<Vec<PathBuf>>,
}

impl ProjectManager {
    /// Create a new `ProjectManager` with the given mode.
    #[must_use]
    pub fn new(mode: ProjectRootMode) -> Self {
        log::info!("Created ProjectManager with mode {mode:?}");
        Self {
            mode: RwLock::new(mode),
            projects: RwLock::new(HashMap::new()),
            workspace_folders: RwLock::new(Vec::new()),
        }
    }

    /// Create a `ProjectManager` with default mode (`GitRoot`).
    #[must_use]
    pub fn with_default_mode() -> Self {
        Self::new(ProjectRootMode::default())
    }

    /// Get the current mode.
    #[must_use]
    pub fn mode(&self) -> ProjectRootMode {
        *self.mode.read()
    }

    /// Set workspace folders.
    ///
    /// Folders are canonicalized internally to ensure `starts_with` comparisons
    /// work correctly when `project_for_path` canonicalizes file paths.
    /// On Windows, this resolves 8.3 short names and adds the `\\?\` prefix.
    /// Folders are stored in the order provided (typically LSP order).
    pub fn set_workspace_folders(&self, folders: Vec<PathBuf>) {
        log::info!("Setting {} workspace folder(s)", folders.len());
        let canonicalized: Vec<PathBuf> = folders
            .into_iter()
            .filter_map(|f| {
                canonicalize_path(&f)
                    .map_err(|e| {
                        log::warn!(
                            "Failed to canonicalize workspace folder '{}': {}",
                            f.display(),
                            e
                        );
                    })
                    .ok()
            })
            .collect();
        *self.workspace_folders.write() = canonicalized;
    }

    /// Get current workspace folders.
    #[must_use]
    pub fn workspace_folders(&self) -> Vec<PathBuf> {
        self.workspace_folders.read().clone()
    }

    /// Get or create Project for a file path.
    ///
    /// Uses double-check pattern per `02_DESIGN.md` C5:
    /// 1. Canonicalize path (outside lock)
    /// 2. Resolve `index_root` (outside lock)
    /// 3. Read lock: check if Project exists (fast path)
    /// 4. Write lock: double-check + create if needed (slow path)
    ///
    /// # Arguments
    ///
    /// * `file_path` - Path to file (may be relative or contain symlinks)
    ///
    /// # Errors
    ///
    /// Returns an error if path canonicalization fails.
    pub fn project_for_path(&self, file_path: &Path) -> Result<Arc<Project>, ProjectError> {
        // 1. Canonicalize path first (outside any lock)
        let canonical_path = canonicalize_path(file_path)
            .map_err(|e| ProjectError::canonicalization_failed(file_path, e))?;

        // 2. Resolve index_root (outside any lock)
        let workspace_folders = self.workspace_folders.read().clone();
        let mode = *self.mode.read();
        let index_root = resolve_index_root(&canonical_path, mode, &workspace_folders)?;

        // 3. First check: read lock (fast path)
        {
            let projects = self.projects.read();
            if let Some(project) = projects.get(&index_root) {
                return Ok(Arc::clone(project));
            }
        } // Read lock released here

        // 4. Second check + creation: write lock (slow path)
        {
            let mut projects = self.projects.write();

            // Double-check: another thread may have created it
            if let Some(project) = projects.get(&index_root) {
                return Ok(Arc::clone(project));
            }

            // Create new Project while holding write lock
            let project = Arc::new(Project::new(index_root.clone())?);
            projects.insert(index_root, Arc::clone(&project));

            // Initialize the project (detect repos, etc.) - idempotent
            // This is done inside the write lock to ensure consistent state
            // before returning the project to the caller.
            if let Err(e) = project.initialize() {
                log::warn!("Project {} initialization failed: {}", project.id, e);
                // Continue anyway - the project is usable, just without repo detection
            }

            log::info!("Created new Project {} via project_for_path", project.id);
            Ok(project)
        } // Write lock released here
    }

    /// Get a Project by its `index_root` if it exists.
    ///
    /// Does not create the Project if it doesn't exist.
    #[must_use]
    pub fn get_project(&self, index_root: &Path) -> Option<Arc<Project>> {
        let projects = self.projects.read();
        projects.get(index_root).cloned()
    }

    /// Get all Projects.
    #[must_use]
    pub fn all_projects(&self) -> Vec<Arc<Project>> {
        self.projects.read().values().cloned().collect()
    }

    /// Get the number of active Projects.
    #[must_use]
    pub fn project_count(&self) -> usize {
        self.projects.read().len()
    }

    /// Handle configuration change (mode switch).
    ///
    /// Per `02_DESIGN.md` M1, uses full teardown strategy:
    /// 1. Cancel all in-progress operations
    /// 2. Optionally persist state
    /// 3. Drop all Projects
    /// 4. New Projects created lazily on next access
    pub fn handle_config_change(&self, new_mode: ProjectRootMode) {
        let old_mode = *self.mode.read();
        if old_mode == new_mode {
            return; // No change
        }

        log::warn!(
            "projectRootMode changed from {old_mode:?} to {new_mode:?}, rebuilding Projects"
        );

        // Full teardown
        let mut projects = self.projects.write();
        for (_root, project) in projects.drain() {
            project.cancel_operations();
            project.persist_if_configured();
            // Project dropped here
        }
        drop(projects);

        // Update mode
        *self.mode.write() = new_mode;

        log::info!("Mode change complete. New Projects will be created lazily.");
    }

    /// Remove a Project by `index_root`.
    ///
    /// Used when a workspace folder is removed.
    pub fn remove_project(&self, index_root: &Path) -> Option<Arc<Project>> {
        let mut projects = self.projects.write();
        let removed = projects.remove(index_root);
        if let Some(ref project) = removed {
            log::info!(
                "Removed Project {} at '{}'",
                project.id,
                index_root.display()
            );
            project.cancel_operations();
        }
        removed
    }

    /// Shutdown: remove all Projects and clean up.
    pub fn shutdown(&self) {
        log::info!("Shutting down ProjectManager");
        let mut projects = self.projects.write();
        for (_root, project) in projects.drain() {
            project.cancel_operations();
            project.persist_if_configured();
        }
    }
}

impl Default for ProjectManager {
    fn default() -> Self {
        Self::with_default_mode()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_git_repo(temp: &TempDir) -> PathBuf {
        let git_dir = temp.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        temp.path().to_path_buf()
    }

    #[test]
    fn test_project_creation() {
        let temp = TempDir::new().unwrap();
        let project = Project::new(temp.path().to_path_buf()).unwrap();

        assert!(!project.is_initialized());
        assert!(!project.is_cancelled());
        assert_eq!(project.file_count(), 0);
        assert_eq!(project.repo_count(), 0);
    }

    #[test]
    fn test_project_initialization() {
        let temp = TempDir::new().unwrap();
        let project = Project::new(temp.path().to_path_buf()).unwrap();

        project.initialize().unwrap();
        assert!(project.is_initialized());

        // Second call should be no-op
        project.initialize().unwrap();
        assert!(project.is_initialized());
    }

    #[test]
    fn test_project_cancellation() {
        let temp = TempDir::new().unwrap();
        let project = Project::new(temp.path().to_path_buf()).unwrap();

        assert!(!project.is_cancelled());
        project.cancel_operations();
        assert!(project.is_cancelled());
    }

    #[test]
    fn test_project_repo_registration() {
        let temp = TempDir::new().unwrap();
        let project = Project::new(temp.path().to_path_buf()).unwrap();

        let git_root = temp.path().join("repo");
        let repo_id = RepoId::from_git_root(&git_root);

        project.register_repo(git_root.clone(), repo_id);

        assert_eq!(project.repo_count(), 1);
        assert_eq!(project.get_repo_id(&git_root), Some(repo_id));
    }

    #[test]
    fn test_project_file_registration() {
        let temp = TempDir::new().unwrap();
        let project = Project::new(temp.path().to_path_buf()).unwrap();

        let entry = FileEntry::new(Arc::from("src/main.rs"), RepoId::NONE);
        project.register_file(entry);

        assert_eq!(project.file_count(), 1);
        assert!(project.get_file("src/main.rs").is_some());
    }

    #[test]
    fn test_manager_creation() {
        let manager = ProjectManager::new(ProjectRootMode::GitRoot);
        assert_eq!(manager.mode(), ProjectRootMode::GitRoot);
        assert_eq!(manager.project_count(), 0);
    }

    #[test]
    fn test_manager_default() {
        let manager = ProjectManager::default();
        assert_eq!(manager.mode(), ProjectRootMode::GitRoot);
    }

    #[test]
    fn test_manager_workspace_folders() {
        let manager = ProjectManager::new(ProjectRootMode::WorkspaceFolder);

        let temp = TempDir::new().unwrap();
        let folder1 = temp.path().join("proj1");
        let folder2 = temp.path().join("proj2");
        std::fs::create_dir(&folder1).unwrap();
        std::fs::create_dir(&folder2).unwrap();

        manager.set_workspace_folders(vec![folder1.clone(), folder2.clone()]);

        // set_workspace_folders canonicalizes internally, so compare canonicalized forms
        let canon = |p: &Path| -> PathBuf { canonicalize_path(p).unwrap() };
        let workspace_folder_list = manager.workspace_folders();
        assert_eq!(workspace_folder_list.len(), 2);
        assert_eq!(workspace_folder_list[0], canon(&folder1));
        assert_eq!(workspace_folder_list[1], canon(&folder2));
    }

    #[test]
    fn test_manager_project_for_path() {
        let temp = TempDir::new().unwrap();
        let repo_root = setup_git_repo(&temp);

        let file = repo_root.join("src/main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn main() {}").unwrap();

        let manager = ProjectManager::new(ProjectRootMode::GitRoot);
        let project = manager.project_for_path(&file).unwrap();

        // Should have created one project
        assert_eq!(manager.project_count(), 1);

        // Same file should return same project
        let project2 = manager.project_for_path(&file).unwrap();
        assert_eq!(project.id, project2.id);
        assert_eq!(manager.project_count(), 1);
    }

    #[test]
    fn test_manager_project_for_path_different_repos() {
        let temp = TempDir::new().unwrap();

        // Create two separate git repos
        let repo1 = temp.path().join("repo1");
        let repo2 = temp.path().join("repo2");
        std::fs::create_dir(&repo1).unwrap();
        std::fs::create_dir(&repo2).unwrap();
        std::fs::create_dir(repo1.join(".git")).unwrap();
        std::fs::create_dir(repo2.join(".git")).unwrap();

        let file1 = repo1.join("file.rs");
        let file2 = repo2.join("file.rs");
        std::fs::write(&file1, "").unwrap();
        std::fs::write(&file2, "").unwrap();

        let manager = ProjectManager::new(ProjectRootMode::GitRoot);

        let project1 = manager.project_for_path(&file1).unwrap();
        let project2 = manager.project_for_path(&file2).unwrap();

        // Should have two different projects
        assert_eq!(manager.project_count(), 2);
        assert_ne!(project1.id, project2.id);
    }

    #[test]
    fn test_manager_config_change() {
        let temp = TempDir::new().unwrap();
        let repo_root = setup_git_repo(&temp);
        let file = repo_root.join("file.rs");
        std::fs::write(&file, "").unwrap();

        let manager = ProjectManager::new(ProjectRootMode::GitRoot);

        // Create a project
        let _project = manager.project_for_path(&file).unwrap();
        assert_eq!(manager.project_count(), 1);

        // Change mode - should clear projects
        manager.handle_config_change(ProjectRootMode::WorkspaceFolder);
        assert_eq!(manager.mode(), ProjectRootMode::WorkspaceFolder);
        assert_eq!(manager.project_count(), 0);
    }

    #[test]
    fn test_manager_remove_project() {
        let temp = TempDir::new().unwrap();
        let repo_root = setup_git_repo(&temp);
        let file = repo_root.join("file.rs");
        std::fs::write(&file, "").unwrap();

        let manager = ProjectManager::new(ProjectRootMode::GitRoot);
        let project = manager.project_for_path(&file).unwrap();
        assert_eq!(manager.project_count(), 1);

        // Get the canonical index_root
        let index_root = canonicalize_path(&repo_root).unwrap();

        // Remove the project
        let removed = manager.remove_project(&index_root);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().id, project.id);
        assert_eq!(manager.project_count(), 0);
    }

    #[test]
    fn test_manager_get_project() {
        let temp = TempDir::new().unwrap();
        let repo_root = setup_git_repo(&temp);
        let file = repo_root.join("file.rs");
        std::fs::write(&file, "").unwrap();

        let manager = ProjectManager::new(ProjectRootMode::GitRoot);

        // Before creation
        let canonical_root = canonicalize_path(&repo_root).unwrap();
        assert!(manager.get_project(&canonical_root).is_none());

        // After creation
        let _project = manager.project_for_path(&file).unwrap();
        assert!(manager.get_project(&canonical_root).is_some());
    }

    #[test]
    fn test_manager_all_projects() {
        let temp = TempDir::new().unwrap();

        let repo1 = temp.path().join("repo1");
        let repo2 = temp.path().join("repo2");
        std::fs::create_dir(&repo1).unwrap();
        std::fs::create_dir(&repo2).unwrap();
        std::fs::create_dir(repo1.join(".git")).unwrap();
        std::fs::create_dir(repo2.join(".git")).unwrap();

        let manager = ProjectManager::new(ProjectRootMode::GitRoot);

        let file1 = repo1.join("file.rs");
        let file2 = repo2.join("file.rs");
        std::fs::write(&file1, "").unwrap();
        std::fs::write(&file2, "").unwrap();

        let _project1 = manager.project_for_path(&file1).unwrap();
        let _project2 = manager.project_for_path(&file2).unwrap();

        let all = manager.all_projects();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_manager_shutdown() {
        let temp = TempDir::new().unwrap();
        let repo_root = setup_git_repo(&temp);
        let file = repo_root.join("file.rs");
        std::fs::write(&file, "").unwrap();

        let manager = ProjectManager::new(ProjectRootMode::GitRoot);
        let project = manager.project_for_path(&file).unwrap();
        assert_eq!(manager.project_count(), 1);

        manager.shutdown();
        assert_eq!(manager.project_count(), 0);
        // Project should have been cancelled
        assert!(project.is_cancelled());
    }

    #[test]
    fn test_manager_concurrent_access() {
        use std::thread;

        let temp = TempDir::new().unwrap();
        let repo_root = setup_git_repo(&temp);

        // Create multiple files in the same repo
        for i in 0..10 {
            let file = repo_root.join(format!("file{i}.rs"));
            std::fs::write(&file, "").unwrap();
        }

        let manager = Arc::new(ProjectManager::new(ProjectRootMode::GitRoot));
        let mut handles = vec![];

        // Spawn threads that all access the same repo concurrently
        for i in 0..10 {
            let manager = Arc::clone(&manager);
            let file = repo_root.join(format!("file{i}.rs"));
            handles.push(thread::spawn(move || {
                manager.project_for_path(&file).unwrap()
            }));
        }

        // Collect results
        let projects: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All threads should get the same project (only one created)
        assert_eq!(manager.project_count(), 1);
        let first_id = projects[0].id;
        for project in projects {
            assert_eq!(project.id, first_id);
        }
    }

    // ===== Phase 4: Repository Detection Tests =====

    #[test]
    fn test_project_initialize_detects_repos() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("project");
        std::fs::create_dir(&project_root).unwrap();
        std::fs::create_dir(project_root.join(".git")).unwrap();

        let project = Project::new(project_root.clone()).unwrap();

        // Before initialize, repo_index should be empty
        assert_eq!(project.repo_count(), 0);

        // Initialize should detect the repo
        project.initialize().unwrap();

        // After initialize, repo_index should have one entry
        assert_eq!(project.repo_count(), 1);
    }

    #[test]
    fn test_project_initialize_detects_nested_repos() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("monorepo");
        std::fs::create_dir(&project_root).unwrap();
        std::fs::create_dir(project_root.join(".git")).unwrap();

        // Create nested repos
        let inner1 = project_root.join("packages/app1");
        let inner2 = project_root.join("packages/app2");
        std::fs::create_dir_all(&inner1).unwrap();
        std::fs::create_dir_all(&inner2).unwrap();
        std::fs::create_dir(inner1.join(".git")).unwrap();
        std::fs::create_dir(inner2.join(".git")).unwrap();

        let project = Project::new(project_root).unwrap();
        project.initialize().unwrap();

        // Should detect all three repos
        assert_eq!(project.repo_count(), 3);
    }

    #[test]
    fn test_project_repo_id_for_file_simple() {
        let temp = TempDir::new().unwrap();
        let repo_root = setup_git_repo(&temp);

        // Create a file in the repo
        let file = repo_root.join("src/main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "fn main() {}").unwrap();

        let project = Project::new(repo_root).unwrap();
        project.initialize().unwrap();

        let repo_id = project.repo_id_for_file(&file).unwrap();
        assert!(repo_id.is_some());
    }

    #[test]
    fn test_project_repo_id_for_file_nested_nearest_wins() {
        let temp = TempDir::new().unwrap();
        let outer = temp.path().join("outer");
        std::fs::create_dir(&outer).unwrap();
        std::fs::create_dir(outer.join(".git")).unwrap();

        // Create nested inner repo
        let inner = outer.join("packages/inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::create_dir(inner.join(".git")).unwrap();

        // Create file in inner repo
        let file = inner.join("src/lib.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "").unwrap();

        let project = Project::new(outer.clone()).unwrap();
        project.initialize().unwrap();

        // Get RepoIds
        let inner_canonical = canonicalize_path(&inner).unwrap();
        let outer_canonical = canonicalize_path(&outer).unwrap();

        let repo_index = project.repo_index();
        let inner_id = repo_index.get(&inner_canonical).unwrap();
        let outer_id = repo_index.get(&outer_canonical).unwrap();

        // File in inner repo should get inner repo's ID
        let file_repo_id = project.repo_id_for_file(&file).unwrap();
        assert_eq!(file_repo_id, *inner_id);
        assert_ne!(file_repo_id, *outer_id);
    }

    #[test]
    fn test_project_repo_id_for_file_outer_repo() {
        let temp = TempDir::new().unwrap();
        let outer = temp.path().join("outer");
        std::fs::create_dir(&outer).unwrap();
        std::fs::create_dir(outer.join(".git")).unwrap();

        // Create nested inner repo
        let inner = outer.join("packages/inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::create_dir(inner.join(".git")).unwrap();

        // Create file in outer repo (outside inner)
        let file = outer.join("src/main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "").unwrap();

        let project = Project::new(outer.clone()).unwrap();
        project.initialize().unwrap();

        // Get RepoIds
        let outer_canonical = canonicalize_path(&outer).unwrap();
        let repo_index = project.repo_index();
        let outer_id = repo_index.get(&outer_canonical).unwrap();

        // File in outer repo should get outer repo's ID
        let file_repo_id = project.repo_id_for_file(&file).unwrap();
        assert_eq!(file_repo_id, *outer_id);
    }

    #[test]
    fn test_project_repo_id_for_file_no_repo() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("norepro");
        std::fs::create_dir(&project_root).unwrap();

        // No .git directory

        // Create a file
        let file = project_root.join("file.rs");
        std::fs::write(&file, "").unwrap();

        let project = Project::new(project_root).unwrap();
        project.initialize().unwrap();

        // No repos detected
        assert_eq!(project.repo_count(), 0);

        // File should get NONE
        let repo_id = project.repo_id_for_file(&file).unwrap();
        assert!(repo_id.is_none());
        assert_eq!(repo_id, RepoId::NONE);
    }

    #[test]
    fn test_project_repo_index_returns_all_repos() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("multi");
        std::fs::create_dir(&project_root).unwrap();

        // Create three separate repos
        for name in &["repo1", "repo2", "repo3"] {
            let repo = project_root.join(name);
            std::fs::create_dir(&repo).unwrap();
            std::fs::create_dir(repo.join(".git")).unwrap();
        }

        let project = Project::new(project_root).unwrap();
        project.initialize().unwrap();

        let repo_index = project.repo_index();
        assert_eq!(repo_index.len(), 3);
    }

    #[test]
    fn test_project_initialize_submodule() {
        let temp = TempDir::new().unwrap();
        let main_repo = temp.path().join("main");
        std::fs::create_dir(&main_repo).unwrap();
        std::fs::create_dir(main_repo.join(".git")).unwrap();

        // Create submodule with .git file
        // Note: use "deps" not "vendor" since vendor is in IGNORED_DIRS
        let submodule = main_repo.join("deps/lib");
        std::fs::create_dir_all(&submodule).unwrap();

        // Create the gitdir target (Phase 4 validation requires this to exist with HEAD)
        let gitdir_target = main_repo.join(".git/modules/deps/lib");
        std::fs::create_dir_all(&gitdir_target).unwrap();
        std::fs::write(gitdir_target.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        std::fs::write(
            submodule.join(".git"),
            "gitdir: ../../.git/modules/deps/lib\n",
        )
        .unwrap();

        let project = Project::new(main_repo).unwrap();
        project.initialize().unwrap();

        // Should detect both main repo and submodule
        assert_eq!(project.repo_count(), 2);
    }

    #[test]
    fn test_project_initialize_skips_ignored_dirs() {
        let temp = TempDir::new().unwrap();
        let project_root = temp.path().join("project");
        std::fs::create_dir(&project_root).unwrap();
        std::fs::create_dir(project_root.join(".git")).unwrap();

        // Create repo in node_modules (should be skipped)
        let ignored_repo = project_root.join("node_modules/pkg");
        std::fs::create_dir_all(&ignored_repo).unwrap();
        std::fs::create_dir(ignored_repo.join(".git")).unwrap();

        let project = Project::new(project_root).unwrap();
        project.initialize().unwrap();

        // Should only find the main project repo
        assert_eq!(project.repo_count(), 1);
    }

    // ============================================================
    // Integration tests for project root mode routing (Codex LOW)
    // Per PROJECT_ROOT_SPEC.md Section 9.2 path-aware index routing
    // ============================================================

    #[test]
    fn test_project_manager_routes_path_to_correct_project() {
        let temp = TempDir::new().unwrap();

        // Create two separate workspace folders (projects) with git repos
        let project_a = temp.path().join("project-a");
        let project_b = temp.path().join("project-b");
        std::fs::create_dir_all(&project_a).unwrap();
        std::fs::create_dir_all(&project_b).unwrap();
        std::fs::create_dir(project_a.join(".git")).unwrap();
        std::fs::create_dir(project_b.join(".git")).unwrap();

        // Create files in each project
        std::fs::write(project_a.join("file_a.rs"), "fn a() {}").unwrap();
        std::fs::write(project_b.join("file_b.rs"), "fn b() {}").unwrap();

        // Use GitRoot mode - each git repo becomes its own project
        let manager = ProjectManager::new(ProjectRootMode::GitRoot);

        // Route path to project_a - should succeed
        let resolved_a = manager.project_for_path(&project_a.join("file_a.rs"));
        assert!(resolved_a.is_ok());
        let canonical_a = canonicalize_path(&project_a).unwrap();
        assert_eq!(resolved_a.unwrap().index_root, canonical_a);

        // Route path to project_b - should succeed
        let resolved_b = manager.project_for_path(&project_b.join("file_b.rs"));
        assert!(resolved_b.is_ok());
        let canonical_b = canonicalize_path(&project_b).unwrap();
        assert_eq!(resolved_b.unwrap().index_root, canonical_b);

        // Should have created two distinct projects
        assert_eq!(manager.project_count(), 2);
    }

    #[test]
    fn test_project_manager_nested_path_routes_to_containing_project() {
        let temp = TempDir::new().unwrap();

        // Create project with nested directories
        let project = temp.path().join("workspace");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir(project.join(".git")).unwrap();

        let nested_path = project.join("src/components/deeply/nested");
        std::fs::create_dir_all(&nested_path).unwrap();
        std::fs::write(nested_path.join("component.rs"), "struct C;").unwrap();

        let manager = ProjectManager::new(ProjectRootMode::GitRoot);

        // Deeply nested path should route to containing project (git root)
        let resolved = manager.project_for_path(&nested_path.join("component.rs"));
        assert!(resolved.is_ok());
        let canonical_project = canonicalize_path(&project).unwrap();
        assert_eq!(resolved.unwrap().index_root, canonical_project);
    }

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn test_project_manager_multi_workspace_folder_isolation() {
        let temp = TempDir::new().unwrap();

        // Create three workspace folders with git repos
        let frontend = temp.path().join("frontend");
        let backend = temp.path().join("backend");
        let shared = temp.path().join("shared");

        for p in [&frontend, &backend, &shared] {
            std::fs::create_dir_all(p).unwrap();
            std::fs::create_dir(p.join(".git")).unwrap();
            // Create a file so path resolution works
            std::fs::write(p.join("file.rs"), "").unwrap();
        }

        let manager = ProjectManager::new(ProjectRootMode::WorkspaceFolder);
        manager.set_workspace_folders(vec![frontend.clone(), backend.clone(), shared.clone()]);

        // Verify workspace folders are tracked (set_workspace_folders canonicalizes internally)
        let canon = |p: &Path| -> PathBuf { canonicalize_path(p).unwrap() };
        let folders = manager.workspace_folders();
        assert_eq!(folders.len(), 3);
        assert!(folders.contains(&canon(&frontend)));
        assert!(folders.contains(&canon(&backend)));
        assert!(folders.contains(&canon(&shared)));

        // Each path should route to its own project
        let frontend_proj = manager.project_for_path(&frontend.join("file.rs")).unwrap();
        let backend_proj = manager.project_for_path(&backend.join("file.rs")).unwrap();
        let shared_proj = manager.project_for_path(&shared.join("file.rs")).unwrap();

        assert_eq!(frontend_proj.index_root, canon(&frontend));
        assert_eq!(backend_proj.index_root, canon(&backend));
        assert_eq!(shared_proj.index_root, canon(&shared));
    }

    #[test]
    fn test_project_manager_workspace_folder_update() {
        let temp = TempDir::new().unwrap();

        let project_a = temp.path().join("project-a");
        let project_b = temp.path().join("project-b");
        std::fs::create_dir_all(&project_a).unwrap();
        std::fs::create_dir_all(&project_b).unwrap();
        std::fs::create_dir(project_a.join(".git")).unwrap();
        std::fs::create_dir(project_b.join(".git")).unwrap();
        std::fs::write(project_a.join("file.rs"), "").unwrap();
        std::fs::write(project_b.join("file.rs"), "").unwrap();

        let manager = ProjectManager::new(ProjectRootMode::WorkspaceFolder);
        manager.set_workspace_folders(vec![project_a.clone(), project_b.clone()]);

        // Both should be present (set_workspace_folders canonicalizes internally)
        let canon = |p: &Path| -> PathBuf { canonicalize_path(p).unwrap() };
        assert_eq!(manager.workspace_folders().len(), 2);

        // Update to only include project_a (simulates removing project_b)
        manager.set_workspace_folders(vec![project_a.clone()]);

        // Only project_a should remain
        let folders = manager.workspace_folders();
        assert_eq!(folders.len(), 1);
        assert!(folders.contains(&canon(&project_a)));
        assert!(!folders.contains(&canon(&project_b)));
    }

    #[test]
    fn test_project_index_routing_per_project() {
        let temp = TempDir::new().unwrap();

        // Create two projects with different git repos
        let lib_project = temp.path().join("lib");
        let app_project = temp.path().join("app");
        std::fs::create_dir_all(&lib_project).unwrap();
        std::fs::create_dir_all(&app_project).unwrap();
        std::fs::create_dir(lib_project.join(".git")).unwrap();
        std::fs::create_dir(app_project.join(".git")).unwrap();

        // Create files for path resolution
        std::fs::create_dir_all(lib_project.join("src")).unwrap();
        std::fs::create_dir_all(app_project.join("src")).unwrap();
        std::fs::write(lib_project.join("src/lib.rs"), "").unwrap();
        std::fs::write(app_project.join("src/main.rs"), "").unwrap();

        let manager = ProjectManager::new(ProjectRootMode::GitRoot);

        // Get project for each path
        let lib_proj = manager.project_for_path(&lib_project.join("src/lib.rs"));
        let app_proj = manager.project_for_path(&app_project.join("src/main.rs"));

        // Projects should be resolved successfully
        assert!(lib_proj.is_ok());
        assert!(app_proj.is_ok());

        // Projects should be distinct
        let lib_root = lib_proj.unwrap().index_root.clone();
        let app_root = app_proj.unwrap().index_root.clone();
        assert_ne!(lib_root, app_root);

        // Verify they map to correct canonical paths
        let lib_canonical = canonicalize_path(&lib_project).unwrap();
        let app_canonical = canonicalize_path(&app_project).unwrap();
        assert_eq!(lib_root, lib_canonical);
        assert_eq!(app_root, app_canonical);
    }
}
