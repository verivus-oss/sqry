//! Project root lifecycle management
//!
//! This module implements the behaviors defined in `docs/architecture/PROJECT_ROOT_SPEC.md`:
//!
//! - **ProjectManager**: Routes file paths to Projects based on `ProjectRootMode`
//! - **Project**: Owns a `CodeGraph`, file table, and caches for one index root
//! - **RepoId**: Tracks which git repository each file belongs to
//!
//! # Architecture Overview
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      ProjectManager                         │
//! │  - mode: ProjectRootMode                                    │
//! │  - projects: HashMap<ProjectId, Arc<Project>>               │
//! │  - workspace_folders: Vec<PathBuf>                          │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              │ project_for_path()
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                         Project                             │
//! │  - id: ProjectId                                            │
//! │  - index_root: PathBuf                                      │
//! │  - graph: CodeGraph                                         │
//! │  - repo_index: HashMap<PathBuf, RepoId>                     │
//! │  - file_table: HashMap<StringId, FileEntry>                 │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Modes
//!
//! - **GitRoot** (default): Each git repository gets its own Project
//! - **WorkspaceFolder**: Each VS Code workspace folder gets a Project
//! - **WorkspaceRoot**: Single Project covering all workspace folders
//!
//! # Lifecycle
//!
//! 1. **Create**: On first file routed to an index_root
//! 2. **Initialize**: Empty graph, caches, watchers
//! 3. **Build**: Full index scan
//! 4. **Update**: Incremental updates on file changes
//! 5. **Destroy**: On workspace removal or shutdown

pub mod manager;
pub mod path_utils;
pub mod persistence;
pub mod repo_detection;
pub mod resolver;
pub mod types;

// Re-export primary types
pub use manager::{Project, ProjectManager};
pub use path_utils::{
    DEFAULT_IGNORED_DIRS, absolutize_without_resolution, canonicalize_path,
    is_ignored_dir_with_config, normalize_path_components,
};
pub use repo_detection::{detect_repos_under, lookup_git_root, lookup_repo_id};
pub use resolver::{canonicalize_and_resolve, find_git_root, resolve_index_root};
pub use types::{FileEntry, ProjectError, ProjectId, ProjectRootMode, RepoId, StringId};
