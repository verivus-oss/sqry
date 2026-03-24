//! Root resolution logic for determining Project boundaries
//!
//! Implements the root resolution algorithms from `PROJECT_ROOT_SPEC.md` and `02_DESIGN.md` \[H2\]:
//!
//! - **`GitRoot` mode**: Each git repository gets its own Project
//! - **`WorkspaceFolder` mode**: Each workspace folder is a Project
//! - **`WorkspaceRoot` mode**: Single Project covering all folders
//!
//! # Fallback Chain (gitRoot mode, per C1)
//!
//! ```text
//! 1. Walk ancestors of file looking for .git
//! 2. IF .git found → use git root
//! 3. ELSE IF workspace folders exist → use containing folder or first folder
//! 4. ELSE (single-file mode) → use parent directory
//! ```

use super::path_utils::canonicalize_path;
use super::types::{ProjectError, ProjectRootMode};
use std::path::{Path, PathBuf};

/// Find the git root for a file path by walking ancestors.
///
/// Returns `Some(git_root_path)` if a `.git` directory is found in an ancestor,
/// or `None` if no git root exists.
///
/// The path must already be canonicalized for consistent results.
#[must_use]
pub fn find_git_root(file_path: &Path) -> Option<PathBuf> {
    let mut current = file_path;

    // Walk up the directory tree
    loop {
        let git_dir = current.join(".git");

        // Check if .git exists and is a directory (or file for worktrees)
        if git_dir.exists() {
            // Found git root - return the parent of .git
            return Some(current.to_path_buf());
        }

        // Move to parent directory
        match current.parent() {
            Some(parent) => current = parent,
            None => break, // Reached filesystem root
        }
    }

    None
}

/// Resolve the index root for a file path based on mode and workspace context.
///
/// This implements the full resolution algorithm from `02_DESIGN.md` H2:
///
/// # Arguments
///
/// * `file_path` - The canonical path to resolve (must be pre-canonicalized)
/// * `mode` - The `ProjectRootMode` determining resolution strategy
/// * `workspace_folders` - Ordered list of workspace folders (LSP order, not alphabetical)
///
/// # Returns
///
/// The canonical path to use as `index_root` for the Project.
///
/// # Errors
///
/// Returns an error if resolution fails (extremely rare - only if all fallbacks exhausted).
pub fn resolve_index_root(
    file_path: &Path,
    mode: ProjectRootMode,
    workspace_folders: &[PathBuf],
) -> Result<PathBuf, ProjectError> {
    match mode {
        ProjectRootMode::GitRoot => resolve_git_root_mode(file_path, workspace_folders),
        ProjectRootMode::WorkspaceFolder => {
            resolve_workspace_folder_mode(file_path, workspace_folders)
        }
        ProjectRootMode::WorkspaceRoot => resolve_workspace_root_mode(file_path, workspace_folders),
    }
}

/// Resolve `index_root` for `gitRoot` mode (default).
///
/// Per `02_DESIGN.md` C1, the fallback chain is:
/// 1. Walk ancestors looking for .git
/// 2. If found → use git root
/// 3. Else if workspace folders exist → use containing folder or first folder
/// 4. Else (single-file mode) → use parent directory
fn resolve_git_root_mode(
    file_path: &Path,
    workspace_folders: &[PathBuf],
) -> Result<PathBuf, ProjectError> {
    // Step 1 & 2: Look for git root
    if let Some(git_root) = find_git_root(file_path) {
        log::debug!(
            "Found git root for '{}': '{}'",
            file_path.display(),
            git_root.display()
        );
        return Ok(git_root);
    }

    // Step 3: No git root - check workspace folders
    if !workspace_folders.is_empty() {
        // Try to find containing workspace folder
        if let Some(folder) = find_containing_workspace_folder(file_path, workspace_folders) {
            log::info!(
                "No git root for '{}', using workspace folder '{}'",
                file_path.display(),
                folder.display()
            );
            return Ok(folder);
        }

        // File outside all workspace folders - use first folder
        let first_folder = &workspace_folders[0];
        log::warn!(
            "File '{}' outside all workspace folders, using first folder '{}' as root",
            file_path.display(),
            first_folder.display()
        );
        return Ok(first_folder.clone());
    }

    // Step 4: Single-file mode (no workspace folders) - use parent directory
    if let Some(parent) = file_path.parent() {
        if parent.as_os_str().is_empty() {
            // Handle root directory case
            log::info!(
                "No workspace folders, using current directory as root for '{}'",
                file_path.display()
            );
            return Ok(PathBuf::from("."));
        }
        log::info!(
            "No workspace folders, using parent directory '{}' as root",
            parent.display()
        );
        return Ok(parent.to_path_buf());
    }

    // Edge case: file_path is root itself
    Err(ProjectError::no_git_root(file_path))
}

/// Resolve `index_root` for `workspaceFolder` mode.
///
/// Each workspace folder becomes a Project root, ignoring git boundaries.
fn resolve_workspace_folder_mode(
    file_path: &Path,
    workspace_folders: &[PathBuf],
) -> Result<PathBuf, ProjectError> {
    if workspace_folders.is_empty() {
        // Single-file mode - use parent directory
        return file_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| ProjectError::no_git_root(file_path));
    }

    // Find containing workspace folder
    if let Some(folder) = find_containing_workspace_folder(file_path, workspace_folders) {
        return Ok(folder);
    }

    // File outside all workspace folders - use first folder with warning
    let first_folder = &workspace_folders[0];
    log::warn!(
        "File '{}' outside all workspace folders, using first folder '{}' as root",
        file_path.display(),
        first_folder.display()
    );
    Ok(first_folder.clone())
}

/// Resolve `index_root` for `workspaceRoot` mode.
///
/// Single Project covering all workspace folders.
/// Index root is always the first workspace folder.
fn resolve_workspace_root_mode(
    file_path: &Path,
    workspace_folders: &[PathBuf],
) -> Result<PathBuf, ProjectError> {
    if workspace_folders.is_empty() {
        // Single-file mode - use parent directory
        return file_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| ProjectError::no_git_root(file_path));
    }

    // Always use first workspace folder
    Ok(workspace_folders[0].clone())
}

/// Find the workspace folder that contains a file path.
///
/// Returns the first workspace folder (in order) that is an ancestor of `file_path`,
/// or `None` if the file is outside all workspace folders.
fn find_containing_workspace_folder(
    file_path: &Path,
    workspace_folders: &[PathBuf],
) -> Option<PathBuf> {
    for folder in workspace_folders {
        if file_path.starts_with(folder) {
            return Some(folder.clone());
        }
    }
    None
}

/// Canonicalize a path and resolve its index root.
///
/// This is the high-level entry point combining canonicalization with resolution.
///
/// # Arguments
///
/// * `file_path` - Raw file path (may be relative or contain symlinks)
/// * `mode` - The `ProjectRootMode` determining resolution strategy
/// * `workspace_folders` - Ordered list of workspace folders (already canonicalized)
///
/// # Returns
///
/// The canonical `index_root` path for the Project.
///
/// # Errors
///
/// Returns an error if canonicalization fails or resolution fails.
pub fn canonicalize_and_resolve(
    file_path: &Path,
    mode: ProjectRootMode,
    workspace_folders: &[PathBuf],
) -> Result<PathBuf, ProjectError> {
    // Canonicalize first (per H1)
    let canonical = canonicalize_path(file_path)
        .map_err(|e| ProjectError::canonicalization_failed(file_path, e))?;

    // Then resolve index root
    resolve_index_root(&canonical, mode, workspace_folders)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tempdir_outside_git_repo() -> TempDir {
        #[cfg(unix)]
        fn is_in_git_repo(path: &Path) -> bool {
            path.ancestors()
                .any(|ancestor| ancestor.join(".git").is_dir())
        }

        #[cfg(unix)]
        {
            for base in [Path::new("/var/tmp"), Path::new("/dev/shm")] {
                if base.is_dir()
                    && !is_in_git_repo(base)
                    && let Ok(tmp) = TempDir::new_in(base)
                {
                    return tmp;
                }
            }
        }

        TempDir::new().expect("create temp dir")
    }

    fn setup_git_repo(temp: &TempDir) -> PathBuf {
        let git_dir = temp.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        temp.path().to_path_buf()
    }

    #[test]
    fn test_find_git_root_exists() {
        let temp = TempDir::new().unwrap();
        let repo_root = setup_git_repo(&temp);

        // Create nested file
        let subdir = repo_root.join("src");
        std::fs::create_dir(&subdir).unwrap();
        let file = subdir.join("main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        // Should find git root
        let git_root = find_git_root(&file);
        assert!(git_root.is_some());
        assert_eq!(git_root.unwrap(), repo_root);
    }

    #[test]
    fn test_find_git_root_not_exists() {
        let temp = tempdir_outside_git_repo();
        let file = temp.path().join("loose_file.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        // No .git - should return None
        let git_root = find_git_root(&file);
        assert!(git_root.is_none());
    }

    #[test]
    fn test_find_git_root_nested_repos() {
        let temp = TempDir::new().unwrap();

        // Outer repo
        let outer_git = temp.path().join(".git");
        std::fs::create_dir(&outer_git).unwrap();

        // Inner repo (e.g., submodule)
        let inner = temp.path().join("inner");
        std::fs::create_dir(&inner).unwrap();
        let inner_git = inner.join(".git");
        std::fs::create_dir(&inner_git).unwrap();

        // File in inner repo
        let file = inner.join("lib.rs");
        std::fs::write(&file, "pub fn foo() {}").unwrap();

        // Should find inner repo (nearest .git wins)
        let git_root = find_git_root(&file);
        assert!(git_root.is_some());
        assert_eq!(git_root.unwrap(), inner);
    }

    #[test]
    fn test_resolve_git_root_mode_with_git() {
        let temp = TempDir::new().unwrap();
        let repo_root = setup_git_repo(&temp);

        let file = repo_root.join("file.rs");
        std::fs::write(&file, "").unwrap();

        let result = resolve_git_root_mode(&file, &[]).unwrap();
        assert_eq!(result, repo_root);
    }

    #[test]
    fn test_resolve_git_root_mode_no_git_with_workspace() {
        let temp = tempdir_outside_git_repo();
        let file = temp.path().join("file.rs");
        std::fs::write(&file, "").unwrap();

        let workspace_folders = vec![temp.path().to_path_buf()];
        let result = resolve_git_root_mode(&file, &workspace_folders).unwrap();
        assert_eq!(result, temp.path());
    }

    #[test]
    fn test_resolve_git_root_mode_file_outside_workspace() {
        let temp1 = tempdir_outside_git_repo();
        let temp2 = tempdir_outside_git_repo();

        let file = temp1.path().join("file.rs");
        std::fs::write(&file, "").unwrap();

        // Workspace folder is in different temp dir
        let workspace_folders = vec![temp2.path().to_path_buf()];

        // Should use first workspace folder
        let result = resolve_git_root_mode(&file, &workspace_folders).unwrap();
        assert_eq!(result, temp2.path());
    }

    #[test]
    fn test_resolve_git_root_mode_single_file_mode() {
        let temp = tempdir_outside_git_repo();
        let file = temp.path().join("file.rs");
        std::fs::write(&file, "").unwrap();

        // No workspace folders - single file mode
        let result = resolve_git_root_mode(&file, &[]).unwrap();
        assert_eq!(result, temp.path());
    }

    #[test]
    fn test_resolve_workspace_folder_mode() {
        let temp = TempDir::new().unwrap();
        let folder1 = temp.path().join("proj1");
        let folder2 = temp.path().join("proj2");
        std::fs::create_dir(&folder1).unwrap();
        std::fs::create_dir(&folder2).unwrap();

        let file = folder1.join("src").join("main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "").unwrap();

        let workspace_folders = vec![folder1.clone(), folder2];
        let result = resolve_workspace_folder_mode(&file, &workspace_folders).unwrap();
        assert_eq!(result, folder1);
    }

    #[test]
    fn test_resolve_workspace_root_mode() {
        let temp = TempDir::new().unwrap();
        let folder1 = temp.path().join("proj1");
        let folder2 = temp.path().join("proj2");
        std::fs::create_dir(&folder1).unwrap();
        std::fs::create_dir(&folder2).unwrap();

        let file = folder2.join("file.rs");
        std::fs::write(&file, "").unwrap();

        // Even though file is in folder2, workspaceRoot uses folder1 (first folder)
        let workspace_folders = vec![folder1.clone(), folder2];
        let result = resolve_workspace_root_mode(&file, &workspace_folders).unwrap();
        assert_eq!(result, folder1);
    }

    #[test]
    fn test_resolve_index_root_delegates_correctly() {
        let temp = TempDir::new().unwrap();
        let repo_root = setup_git_repo(&temp);
        let file = repo_root.join("file.rs");
        std::fs::write(&file, "").unwrap();

        // gitRoot mode
        let result = resolve_index_root(&file, ProjectRootMode::GitRoot, &[]).unwrap();
        assert_eq!(result, repo_root);

        // workspaceFolder mode with no folders - falls back to parent
        let result = resolve_index_root(&file, ProjectRootMode::WorkspaceFolder, &[]).unwrap();
        assert_eq!(result, repo_root);

        // workspaceRoot mode with no folders - falls back to parent
        let result = resolve_index_root(&file, ProjectRootMode::WorkspaceRoot, &[]).unwrap();
        assert_eq!(result, repo_root);
    }

    #[test]
    fn test_find_containing_workspace_folder() {
        let temp = TempDir::new().unwrap();
        let folder1 = temp.path().join("a");
        let folder2 = temp.path().join("b");
        std::fs::create_dir(&folder1).unwrap();
        std::fs::create_dir(&folder2).unwrap();

        let file_in_a = folder1.join("file.rs");
        let file_in_b = folder2.join("file.rs");
        let file_outside = temp.path().join("file.rs");

        let workspace_folders = vec![folder1.clone(), folder2.clone()];

        assert_eq!(
            find_containing_workspace_folder(&file_in_a, &workspace_folders),
            Some(folder1)
        );
        assert_eq!(
            find_containing_workspace_folder(&file_in_b, &workspace_folders),
            Some(folder2)
        );
        assert_eq!(
            find_containing_workspace_folder(&file_outside, &workspace_folders),
            None
        );
    }

    #[test]
    fn test_workspace_folder_order_preserved() {
        let temp = TempDir::new().unwrap();
        let folder_z = temp.path().join("z_folder");
        let folder_a = temp.path().join("a_folder");
        std::fs::create_dir(&folder_z).unwrap();
        std::fs::create_dir(&folder_a).unwrap();

        let file_outside = temp.path().join("file.rs");
        std::fs::write(&file_outside, "").unwrap();

        // Order is z_folder first, a_folder second (NOT alphabetical)
        let folders = vec![folder_z.clone(), folder_a];

        // File outside workspace should use FIRST folder (z_folder), not alphabetically first (a_folder)
        let result = resolve_workspace_folder_mode(&file_outside, &folders).unwrap();
        assert_eq!(result, folder_z);
    }

    #[test]
    fn test_canonicalize_and_resolve() {
        let temp = TempDir::new().unwrap();
        let repo_root = setup_git_repo(&temp);
        let file = repo_root.join("file.rs");
        std::fs::write(&file, "").unwrap();

        // Use non-canonical path (with . component)
        let non_canonical = repo_root.join(".").join("file.rs");

        let result = canonicalize_and_resolve(&non_canonical, ProjectRootMode::GitRoot, &[]);
        assert!(result.is_ok());
        // Result should be canonicalized repo root
        let resolved = result.unwrap();
        // Compare canonical forms
        assert_eq!(
            canonicalize_path(&resolved).unwrap(),
            canonicalize_path(&repo_root).unwrap()
        );
    }
}
