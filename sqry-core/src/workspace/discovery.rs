//! Workspace repository discovery utilities.
//!
//! Discovery scans a workspace root for repositories that have been indexed
//! by `sqry index`. The canonical marker is `.sqry/graph/manifest.json` —
//! the same file that `build_unified_graph_inner` writes (see
//! `graph/unified/persistence/mod.rs`'s `GRAPH_DIR_NAME` and
//! `MANIFEST_FILE_NAME` constants). The earlier `.sqry-index` placeholder
//! was never written by the live build pipeline and is removed outright;
//! there is no legacy fallback (RR-10 Gap #2 retains the per-workspace
//! repository cap to bound walker work regardless of marker name).
//!
//! The walker honours `.gitignore` rules and additionally skips a small
//! set of dependency / build directories whose contents must never be
//! treated as discoverable repositories even when those directories are
//! present without a `.gitignore` (e.g. `node_modules`, `target`). The
//! ignore list is the repo-wide
//! [`crate::project::path_utils::DEFAULT_IGNORED_DIRS`] (consulted via
//! [`crate::project::path_utils::is_ignored_dir`]) so workspace discovery
//! and single-repo project detection share one source of truth.

use std::fs;
use std::path::Path;

use ignore::WalkBuilder;

use super::error::{WorkspaceError, WorkspaceResult};
use super::registry::{WorkspaceRepoId, WorkspaceRepository};
// RR-10 Gap #2: Import repository count limit for DoS prevention
use crate::config::buffers::max_repositories;
// Repo-wide source of truth for directories to skip during repo discovery.
use crate::project::path_utils::is_ignored_dir;

/// Canonical marker filename written by `sqry index` under
/// `<repo>/.sqry/graph/`. Discovery treats any file whose name matches this
/// constant and whose parent directory is `.sqry/graph` as evidence of a
/// repository root one level above.
const MANIFEST_FILE_NAME: &str = "manifest.json";

/// Directory segment containing [`MANIFEST_FILE_NAME`]. Used to validate
/// that a candidate `manifest.json` actually lives inside a sqry graph
/// directory (and not, say, an unrelated NPM `manifest.json`).
const GRAPH_DIR_SEGMENT: &str = "graph";

/// Parent of [`GRAPH_DIR_SEGMENT`]. The full canonical relative path is
/// `.sqry/graph/manifest.json`.
const SQRY_DIR_SEGMENT: &str = ".sqry";

/// Discovery strategy for locating repositories within a workspace root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryMode {
    /// Locate repositories by finding `.sqry/graph/manifest.json` markers
    /// anywhere under root.
    IndexFiles,
    /// Only include repositories that are git roots (contain `.git/`) with
    /// an index marker.
    GitRoots,
}

/// Discover repositories beneath `root` according to `mode`.
///
/// # Errors
///
/// Returns [`WorkspaceError`] when filesystem traversal fails.
pub fn discover_repositories(
    root: &Path,
    mode: DiscoveryMode,
) -> WorkspaceResult<Vec<WorkspaceRepository>> {
    let mut repositories = Vec::new();

    let walker = WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .git_ignore(true)
        .git_exclude(true)
        .parents(true)
        .filter_entry(|entry| {
            // Skip well-known dependency / build directories so discovery
            // never wastes work descending into them. The ignore list is
            // owned by `crate::project::path_utils::DEFAULT_IGNORED_DIRS`
            // and consulted through `is_ignored_dir` to keep workspace
            // discovery and single-repo project detection in lockstep.
            !is_ignored_dir(entry.file_name())
        })
        .build();

    for result in walker {
        let entry = match result {
            Ok(ok) => ok,
            Err(err) => {
                let message = err.to_string();
                let io_err = err
                    .into_io_error()
                    .unwrap_or_else(|| std::io::Error::other(message));
                return Err(WorkspaceError::Discovery {
                    root: root.to_path_buf(),
                    source: io_err,
                });
            }
        };

        if entry.file_type().is_some_and(|ft| ft.is_dir()) {
            continue;
        }

        if entry.file_name() != MANIFEST_FILE_NAME {
            continue;
        }

        let manifest_path = entry.into_path();

        // Validate the candidate sits under `<repo>/.sqry/graph/manifest.json`.
        // Without this guard, any `manifest.json` (e.g. NPM's package
        // manifest) would be misclassified as a sqry index marker.
        let Some(graph_dir) = manifest_path.parent() else {
            continue;
        };
        if graph_dir.file_name().and_then(|s| s.to_str()) != Some(GRAPH_DIR_SEGMENT) {
            continue;
        }
        let Some(sqry_dir) = graph_dir.parent() else {
            continue;
        };
        if sqry_dir.file_name().and_then(|s| s.to_str()) != Some(SQRY_DIR_SEGMENT) {
            continue;
        }
        let Some(repo_root) = sqry_dir.parent().map(Path::to_path_buf) else {
            continue;
        };

        if matches!(mode, DiscoveryMode::GitRoots) && !repo_root.join(".git").is_dir() {
            continue;
        }

        let relative_path = repo_root.strip_prefix(root).unwrap_or(repo_root.as_path());
        let repo_id = WorkspaceRepoId::new(relative_path);
        let name = repo_root.file_name().map_or_else(
            || repo_id.as_str().to_string(),
            |os| os.to_string_lossy().into_owned(),
        );

        let metadata = fs::metadata(&manifest_path);
        let last_indexed_at = metadata.ok().and_then(|meta| meta.modified().ok());

        // RR-10 Gap #2: Enforce repository count limit to prevent DoS via
        // workspaces containing thousands of indexed repositories.
        let max_repos = max_repositories();
        if repositories.len() >= max_repos {
            return Err(WorkspaceError::TooManyRepositories {
                found: repositories.len(),
                limit: max_repos,
            });
        }

        repositories.push(WorkspaceRepository::new(
            repo_id,
            name,
            repo_root,
            manifest_path,
            last_indexed_at,
        ));
    }

    repositories.sort_by(|a, b| a.id.cmp(&b.id));
    repositories.dedup_by(|a, b| a.id == b.id);
    Ok(repositories)
}
