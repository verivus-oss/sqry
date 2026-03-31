//! Workspace repository discovery utilities.
//!
//! RR-10 Gap #2: Repository count limit enforced to prevent `DoS` attacks
//! via workspaces containing thousands of `.sqry-index` files.

use std::fs;
use std::path::Path;

use ignore::WalkBuilder;

use super::error::{WorkspaceError, WorkspaceResult};
use super::registry::{WorkspaceRepoId, WorkspaceRepository};
// RR-10 Gap #2: Import repository count limit for DoS prevention
use crate::config::buffers::max_repositories;

/// Discovery strategy for locating repositories within a workspace root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryMode {
    /// Locate repositories by finding `.sqry-index` files anywhere under root.
    IndexFiles,
    /// Only include repositories that are git roots (contain `.git/`) with an index.
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
            // Skip target directories to avoid heavy traversals.
            let file_name = entry.file_name().to_string_lossy();
            file_name != "target"
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

        if entry.file_name() != ".sqry-index" {
            continue;
        }

        let index_path = entry.into_path();
        let repo_root = index_path
            .parent()
            .map_or_else(|| root.to_path_buf(), Path::to_path_buf);

        if matches!(mode, DiscoveryMode::GitRoots) && !repo_root.join(".git").is_dir() {
            continue;
        }

        let relative_path = repo_root.strip_prefix(root).unwrap_or(repo_root.as_path());
        let repo_id = WorkspaceRepoId::new(relative_path);
        let name = repo_root.file_name().map_or_else(
            || repo_id.as_str().to_string(),
            |os| os.to_string_lossy().into_owned(),
        );

        let metadata = fs::metadata(&index_path);
        let last_indexed_at = metadata.ok().and_then(|meta| meta.modified().ok());

        // RR-10 Gap #2: Enforce repository count limit to prevent DoS via thousands of .sqry-index files
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
            index_path,
            last_indexed_at,
        ));
    }

    repositories.sort_by(|a, b| a.id.cmp(&b.id));
    repositories.dedup_by(|a, b| a.id == b.id);
    Ok(repositories)
}
