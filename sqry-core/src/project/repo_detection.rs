//! Repository detection within a project index root
//!
//! Implements repository detection per `02_DESIGN.md`:
//! - \[C3\] `detect_repos_under()` with proper error handling (no panics)
//! - \[C8\] `.git` directory filtering - allow through for detection
//! - \[M2\] `RepoId` assignment with "nearest .git wins" rule
//!
//! # Usage
//!
//! ```ignore
//! let repos = detect_repos_under(&index_root);
//! let repo_id = lookup_repo_id(&file_path, &repos);
//! ```

use super::path_utils::{canonicalize_path, is_ignored_dir};
use super::types::RepoId;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Detect all git repositories under the given `index_root`.
///
/// Walks the directory tree looking for `.git` directories and returns a map
/// from canonical git root paths to their `RepoIds`.
///
/// # Per `02_DESIGN.md` C3/C8
///
/// - Walk errors (permission denied, etc.) are logged but do not abort detection
/// - `.git` directories are allowed through the filter (we need to detect them)
/// - Other ignored directories (`node_modules`, target, etc.) are skipped
/// - Canonicalization failures are logged and the repo is skipped
///
/// # Arguments
///
/// * `index_root` - The project root to scan for repositories
///
/// # Returns
///
/// A map from canonical git root paths to their `RepoIds`. Empty if no `.git` found
/// or if all `.git` directories had errors.
///
/// # Examples
///
/// ```ignore
/// let repos = detect_repos_under(Path::new("/home/user/project"));
/// for (git_root, repo_id) in &repos {
///     println!("Found repo: {} -> {}", git_root.display(), repo_id);
/// }
/// ```
#[must_use]
pub fn detect_repos_under(index_root: &Path) -> HashMap<PathBuf, RepoId> {
    let mut repos = HashMap::new();

    log::debug!(
        "Scanning for git repositories under '{}'",
        index_root.display()
    );

    // Walk index_root looking for .git directories
    // Per C8: We detect .git entries but do NOT descend into them
    // This prevents false detections of .git/modules/foo/.git inside git internals
    let mut iter = WalkDir::new(index_root)
        .follow_links(false) // Don't follow symlinks to avoid loops
        .into_iter();

    while let Some(result) = iter.next() {
        let entry = match result {
            Ok(e) => e,
            Err(err) => {
                // Log but continue - may be permission denied on some dirs
                log::warn!("Error walking directory: {err}");
                continue;
            }
        };

        let name = entry.file_name();

        // Check if this is a .git entry
        if name == ".git" {
            // Could be a file (.git file for worktrees/submodules) or directory
            let file_type = entry.file_type();

            if file_type.is_dir() {
                // Standard .git directory - parent is git root
                // IMPORTANT: Skip descending into .git to avoid false detections
                // (e.g., .git/modules/foo/.git would be wrongly detected)
                iter.skip_current_dir();
                process_git_dir(entry.path(), &mut repos);
            } else if file_type.is_file() {
                // .git file (worktree or submodule) - validate and process
                process_git_file(entry.path(), &mut repos);
            }
            // Symlink .git entries - skip to avoid confusion
        } else if entry.file_type().is_dir() && is_ignored_dir(name) {
            // Skip ignored directories (node_modules, target, vendor, etc.)
            iter.skip_current_dir();
        }
    }

    log::debug!(
        "Found {} git repositor{} under '{}'",
        repos.len(),
        if repos.len() == 1 { "y" } else { "ies" },
        index_root.display()
    );

    repos
}

/// Process a standard .git directory and add the repo to the map.
fn process_git_dir(git_dir: &Path, repos: &mut HashMap<PathBuf, RepoId>) {
    let Some(git_root) = git_dir.parent() else {
        log::warn!("Found .git at filesystem root, skipping");
        return;
    };

    match canonicalize_path(git_root) {
        Ok(canonical) => {
            let repo_id = RepoId::from_git_root(&canonical);
            log::trace!(
                "Detected repository: {} -> {}",
                canonical.display(),
                repo_id
            );
            repos.insert(canonical, repo_id);
        }
        Err(err) => {
            // Canonicalization failed - skip this repo but continue
            log::warn!(
                "Cannot canonicalize git root {}: {}",
                git_root.display(),
                err
            );
        }
    }
}

/// Process a .git file (worktree or submodule) and add the repo to the map.
///
/// .git files contain a `gitdir: <path>` line pointing to the actual git directory.
/// For `RepoId` purposes, we use the directory containing the .git file as the root,
/// since that's what the user perceives as the repository location.
///
/// # Validation (per Codex review)
///
/// We validate that the gitdir reference:
/// 1. Has valid `gitdir:` prefix
/// 2. Points to a path that actually exists
/// 3. Contains a HEAD file (basic git directory validation)
///
/// Stale/broken references are logged and skipped per C3.
fn process_git_file(git_file: &Path, repos: &mut HashMap<PathBuf, RepoId>) {
    let Some(git_root) = git_file.parent() else {
        log::warn!("Found .git file at filesystem root, skipping");
        return;
    };

    // Read the .git file to validate it's a proper gitdir reference
    let content = match std::fs::read_to_string(git_file) {
        Ok(c) => c,
        Err(err) => {
            log::warn!("Cannot read .git file {}: {}", git_file.display(), err);
            return;
        }
    };

    let content = content.trim();

    // Must have gitdir: prefix
    let Some(gitdir_value) = content.strip_prefix("gitdir:") else {
        log::debug!(
            "Ignoring .git file without gitdir reference: {}",
            git_file.display()
        );
        return;
    };

    let gitdir_path_str = gitdir_value.trim();
    if gitdir_path_str.is_empty() {
        log::warn!("Empty gitdir reference in {}", git_file.display());
        return;
    }

    // Resolve gitdir path relative to the .git file's parent
    let gitdir_path = Path::new(gitdir_path_str);
    let resolved_gitdir = if gitdir_path.is_absolute() {
        gitdir_path.to_path_buf()
    } else {
        git_root.join(gitdir_path)
    };

    // Validate the gitdir reference exists and is a valid git directory
    if !resolved_gitdir.exists() {
        log::warn!(
            "Stale .git file {}: gitdir reference '{}' does not exist",
            git_file.display(),
            resolved_gitdir.display()
        );
        return;
    }

    // Basic validation: git directory should contain a HEAD file
    let head_file = resolved_gitdir.join("HEAD");
    if !head_file.exists() {
        log::warn!(
            "Invalid gitdir reference in {}: '{}' missing HEAD file",
            git_file.display(),
            resolved_gitdir.display()
        );
        return;
    }

    // Valid git worktree/submodule - use parent of .git file as repo root
    match canonicalize_path(git_root) {
        Ok(canonical) => {
            let repo_id = RepoId::from_git_root(&canonical);
            log::trace!(
                "Detected submodule/worktree: {} -> {} (gitdir: {})",
                canonical.display(),
                repo_id,
                resolved_gitdir.display()
            );
            repos.insert(canonical, repo_id);
        }
        Err(err) => {
            log::warn!(
                "Cannot canonicalize submodule root {}: {}",
                git_root.display(),
                err
            );
        }
    }
}

/// Look up the `RepoId` for a file path using the detected repositories.
///
/// Implements the "nearest .git wins" rule per `02_DESIGN.md` M2:
/// - Walks up from the file path looking for a matching repo root
/// - Nested repos: inner repo's `RepoId` is used (nearest ancestor wins)
/// - Returns `RepoId::NONE` if no git root found in ancestry
///
/// # Arguments
///
/// * `file_path` - Path to the file (should be canonical for accurate matching)
/// * `repos` - Map of detected repositories from `detect_repos_under()`
///
/// # Returns
///
/// The `RepoId` for the nearest enclosing repository, or `RepoId::NONE` if none found.
///
/// # Examples
///
/// ```ignore
/// let repos = detect_repos_under(&index_root);
/// let repo_id = lookup_repo_id(&some_file, &repos);
/// if repo_id.is_none() {
///     println!("File is not in a git repository");
/// }
/// ```
#[must_use]
pub fn lookup_repo_id<S: std::hash::BuildHasher>(
    file_path: &Path,
    repos: &HashMap<PathBuf, RepoId, S>,
) -> RepoId {
    // Walk up from file_path looking for a repo root
    // "Nearest .git wins" means we return as soon as we find a match
    let mut current = file_path;

    loop {
        if let Some(&repo_id) = repos.get(current) {
            return repo_id;
        }

        match current.parent() {
            Some(parent) => current = parent,
            None => break, // Reached filesystem root
        }
    }

    RepoId::NONE
}

/// Find the git root path for a file using the detected repositories.
///
/// Similar to `lookup_repo_id` but returns the path instead of the ID.
///
/// # Arguments
///
/// * `file_path` - Path to the file (should be canonical for accurate matching)
/// * `repos` - Map of detected repositories from `detect_repos_under()`
///
/// # Returns
///
/// The path to the nearest enclosing git root, or `None` if none found.
#[must_use]
pub fn lookup_git_root<'a, S: std::hash::BuildHasher>(
    file_path: &Path,
    repos: &'a HashMap<PathBuf, RepoId, S>,
) -> Option<&'a PathBuf> {
    let mut current = file_path;

    loop {
        // Use get_key_value for single lookup (per Gemini review 3.2)
        if let Some((key, _value)) = repos.get_key_value(current) {
            return Some(key);
        }

        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a git repo structure at the given path
    fn create_git_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        let git_dir = path.join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        // Add HEAD file for validation
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    }

    /// Create a git submodule structure (with .git file pointing to valid gitdir)
    ///
    /// This creates both the .git file and the target gitdir with HEAD file.
    fn create_git_submodule(path: &Path, gitdir_path: &str) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join(".git"), format!("gitdir: {gitdir_path}\n")).unwrap();

        // Create the actual gitdir target with HEAD file (for validation)
        let gitdir_target = if Path::new(gitdir_path).is_absolute() {
            PathBuf::from(gitdir_path)
        } else {
            path.join(gitdir_path)
        };
        std::fs::create_dir_all(&gitdir_target).unwrap();
        std::fs::write(gitdir_target.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    }

    /// Create a stale .git file (gitdir reference that doesn't exist)
    fn create_stale_git_file(path: &Path, gitdir_path: &str) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join(".git"), format!("gitdir: {gitdir_path}\n")).unwrap();
        // Don't create the gitdir target - this simulates a stale reference
    }

    #[test]
    fn test_detect_single_repo() {
        let temp = TempDir::new().unwrap();
        let repo_root = temp.path().join("myrepo");
        create_git_repo(&repo_root);

        let repos = detect_repos_under(temp.path());

        assert_eq!(repos.len(), 1);

        // Verify the repo was detected
        let canonical_root = canonicalize_path(&repo_root).unwrap();
        assert!(repos.contains_key(&canonical_root));

        // Verify RepoId is valid (not NONE)
        let repo_id = repos.get(&canonical_root).unwrap();
        assert!(repo_id.is_some());
    }

    #[test]
    fn test_detect_multiple_repos() {
        let temp = TempDir::new().unwrap();

        // Create three separate repos
        create_git_repo(&temp.path().join("repo1"));
        create_git_repo(&temp.path().join("repo2"));
        create_git_repo(&temp.path().join("subdir/repo3"));

        let repos = detect_repos_under(temp.path());

        assert_eq!(repos.len(), 3);
    }

    #[test]
    fn test_detect_nested_repos() {
        let temp = TempDir::new().unwrap();

        // Create outer repo
        let outer = temp.path().join("outer");
        create_git_repo(&outer);

        // Create inner repo (nested)
        let inner = outer.join("packages/inner");
        create_git_repo(&inner);

        let repos = detect_repos_under(temp.path());

        // Should find both repos
        assert_eq!(repos.len(), 2);

        let outer_canonical = canonicalize_path(&outer).unwrap();
        let inner_canonical = canonicalize_path(&inner).unwrap();

        assert!(repos.contains_key(&outer_canonical));
        assert!(repos.contains_key(&inner_canonical));

        // They should have different RepoIds
        let outer_id = repos.get(&outer_canonical).unwrap();
        let inner_id = repos.get(&inner_canonical).unwrap();
        assert_ne!(outer_id, inner_id);
    }

    #[test]
    fn test_detect_submodule() {
        let temp = TempDir::new().unwrap();

        // Create main repo
        let main_repo = temp.path().join("main");
        create_git_repo(&main_repo);

        // Create submodule with .git file
        // Note: use "deps" not "vendor" since vendor is in IGNORED_DIRS
        let submodule = main_repo.join("deps/lib");
        create_git_submodule(&submodule, "../../.git/modules/deps/lib");

        let repos = detect_repos_under(temp.path());

        // Should find both main repo and submodule
        assert_eq!(repos.len(), 2);

        let main_canonical = canonicalize_path(&main_repo).unwrap();
        let submodule_canonical = canonicalize_path(&submodule).unwrap();

        assert!(repos.contains_key(&main_canonical));
        assert!(repos.contains_key(&submodule_canonical));
    }

    #[test]
    fn test_detect_skips_ignored_dirs() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("project");
        create_git_repo(&root);

        // Create a repo inside node_modules (should be skipped)
        let node_modules_repo = root.join("node_modules/some-package");
        create_git_repo(&node_modules_repo);

        // Create a repo inside target (should be skipped)
        let target_repo = root.join("target/debug/some-crate");
        create_git_repo(&target_repo);

        let repos = detect_repos_under(temp.path());

        // Should only find the main project repo
        assert_eq!(repos.len(), 1);

        let root_canonical = canonicalize_path(&root).unwrap();
        assert!(repos.contains_key(&root_canonical));
    }

    #[test]
    fn test_detect_no_repos() {
        let temp = TempDir::new().unwrap();

        // Just directories, no .git
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::create_dir(temp.path().join("lib")).unwrap();

        let repos = detect_repos_under(temp.path());

        assert!(repos.is_empty());
    }

    #[test]
    fn test_detect_repo_at_root() {
        let temp = TempDir::new().unwrap();

        // .git at the scan root itself
        std::fs::create_dir(temp.path().join(".git")).unwrap();

        let repos = detect_repos_under(temp.path());

        assert_eq!(repos.len(), 1);
        let root_canonical = canonicalize_path(temp.path()).unwrap();
        assert!(repos.contains_key(&root_canonical));
    }

    #[test]
    fn test_lookup_repo_id_simple() {
        let temp = TempDir::new().unwrap();
        let repo_root = temp.path().join("repo");
        create_git_repo(&repo_root);

        // Create a file in the repo
        let src_dir = repo_root.join("src");
        std::fs::create_dir(&src_dir).unwrap();
        let file = src_dir.join("main.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        let repos = detect_repos_under(temp.path());
        let file_canonical = canonicalize_path(&file).unwrap();

        let repo_id = lookup_repo_id(&file_canonical, &repos);

        assert!(repo_id.is_some());

        // Verify it matches the expected RepoId
        let repo_canonical = canonicalize_path(&repo_root).unwrap();
        assert_eq!(repo_id, *repos.get(&repo_canonical).unwrap());
    }

    #[test]
    fn test_lookup_repo_id_nested_nearest_wins() {
        let temp = TempDir::new().unwrap();

        // Create outer repo
        let outer = temp.path().join("outer");
        create_git_repo(&outer);

        // Create nested inner repo
        let inner = outer.join("packages/inner");
        create_git_repo(&inner);

        // Create file in inner repo
        let file = inner.join("src/lib.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "").unwrap();

        let repos = detect_repos_under(temp.path());
        let file_canonical = canonicalize_path(&file).unwrap();

        let repo_id = lookup_repo_id(&file_canonical, &repos);

        // Should get inner repo's ID (nearest wins)
        let inner_canonical = canonicalize_path(&inner).unwrap();
        assert_eq!(repo_id, *repos.get(&inner_canonical).unwrap());

        // NOT outer repo's ID
        let outer_canonical = canonicalize_path(&outer).unwrap();
        assert_ne!(repo_id, *repos.get(&outer_canonical).unwrap());
    }

    #[test]
    fn test_lookup_repo_id_file_in_outer_repo() {
        let temp = TempDir::new().unwrap();

        // Create outer repo
        let outer = temp.path().join("outer");
        create_git_repo(&outer);

        // Create nested inner repo
        let inner = outer.join("packages/inner");
        create_git_repo(&inner);

        // Create file in outer repo (outside inner)
        let file = outer.join("src/main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "").unwrap();

        let repos = detect_repos_under(temp.path());
        let file_canonical = canonicalize_path(&file).unwrap();

        let repo_id = lookup_repo_id(&file_canonical, &repos);

        // Should get outer repo's ID
        let outer_canonical = canonicalize_path(&outer).unwrap();
        assert_eq!(repo_id, *repos.get(&outer_canonical).unwrap());
    }

    #[test]
    fn test_lookup_repo_id_no_repo() {
        let temp = TempDir::new().unwrap();

        // Create file outside any repo
        let file = temp.path().join("loose/file.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "").unwrap();

        // No repos
        let repos = HashMap::new();
        let file_canonical = canonicalize_path(&file).unwrap();

        let repo_id = lookup_repo_id(&file_canonical, &repos);

        assert!(repo_id.is_none());
        assert_eq!(repo_id, RepoId::NONE);
    }

    #[test]
    fn test_lookup_git_root() {
        let temp = TempDir::new().unwrap();
        let repo_root = temp.path().join("repo");
        create_git_repo(&repo_root);

        let file = repo_root.join("src/main.rs");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "").unwrap();

        let repos = detect_repos_under(temp.path());
        let file_canonical = canonicalize_path(&file).unwrap();

        let git_root = lookup_git_root(&file_canonical, &repos);

        assert!(git_root.is_some());
        let expected = canonicalize_path(&repo_root).unwrap();
        assert_eq!(git_root.unwrap(), &expected);
    }

    #[test]
    fn test_lookup_git_root_none() {
        let temp = TempDir::new().unwrap();

        let file = temp.path().join("file.rs");
        std::fs::write(&file, "").unwrap();

        let repos = HashMap::new();
        let file_canonical = canonicalize_path(&file).unwrap();

        let git_root = lookup_git_root(&file_canonical, &repos);

        assert!(git_root.is_none());
    }

    #[test]
    fn test_detect_invalid_git_file() {
        let temp = TempDir::new().unwrap();
        let dir = temp.path().join("notarepo");
        std::fs::create_dir(&dir).unwrap();

        // Create a .git file without proper gitdir content
        std::fs::write(dir.join(".git"), "invalid content").unwrap();

        let repos = detect_repos_under(temp.path());

        // Should not detect this as a repo
        assert!(repos.is_empty());
    }

    #[test]
    fn test_detect_handles_permission_errors_gracefully() {
        // This test verifies the error handling code path exists
        // We can't easily simulate permission errors in tests,
        // but we verify the function doesn't panic on valid input
        let temp = TempDir::new().unwrap();
        create_git_repo(temp.path());

        // Should complete without panic
        let repos = detect_repos_under(temp.path());
        assert_eq!(repos.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn test_detect_symlink_not_followed() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();

        // Create a real repo
        let real_repo = temp.path().join("real");
        create_git_repo(&real_repo);

        // Create a symlink to the repo
        let link = temp.path().join("link");
        symlink(&real_repo, &link).unwrap();

        let repos = detect_repos_under(temp.path());

        // Should only find the real repo, not the symlink
        // (we set follow_links(false) to avoid loops)
        assert_eq!(repos.len(), 1);

        let real_canonical = canonicalize_path(&real_repo).unwrap();
        assert!(repos.contains_key(&real_canonical));
    }

    #[cfg(unix)]
    #[test]
    fn test_detect_circular_symlink_handled() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();

        // Create circular symlinks
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        symlink(&b, &a).unwrap();
        symlink(&a, &b).unwrap();

        // Create a normal repo
        create_git_repo(&temp.path().join("repo"));

        // Should not panic, should still find the real repo
        let repos = detect_repos_under(temp.path());
        assert_eq!(repos.len(), 1);
    }

    #[test]
    fn test_detect_git_modules_not_false_positive() {
        // Per Codex review: .git/modules/foo/.git inside git internals
        // should NOT be detected as a separate repository
        let temp = TempDir::new().unwrap();

        // Create main repo with .git directory
        let main_repo = temp.path().join("main");
        create_git_repo(&main_repo);

        // Simulate git submodule storage inside .git/modules
        // This is where git stores actual submodule data
        let modules_dir = main_repo.join(".git/modules/lib");
        std::fs::create_dir_all(&modules_dir).unwrap();
        // Create a .git file inside modules (this is what git does)
        std::fs::create_dir(modules_dir.join(".git")).unwrap();
        std::fs::write(modules_dir.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        let repos = detect_repos_under(temp.path());

        // Should only find the main repo, NOT the .git/modules/lib/.git
        assert_eq!(
            repos.len(),
            1,
            "Should not detect repos inside .git/modules"
        );

        let main_canonical = canonicalize_path(&main_repo).unwrap();
        assert!(repos.contains_key(&main_canonical));
    }

    #[test]
    fn test_detect_stale_gitdir_reference_skipped() {
        // Per Codex review: stale .git files should be skipped
        let temp = TempDir::new().unwrap();

        // Create main repo
        let main_repo = temp.path().join("main");
        create_git_repo(&main_repo);

        // Create a stale submodule (gitdir points to non-existent location)
        let stale_submodule = main_repo.join("deps/stale");
        create_stale_git_file(&stale_submodule, "../.git/modules/nonexistent");

        let repos = detect_repos_under(temp.path());

        // Should only find main repo, stale submodule should be skipped
        assert_eq!(repos.len(), 1, "Stale gitdir reference should be skipped");

        let main_canonical = canonicalize_path(&main_repo).unwrap();
        assert!(repos.contains_key(&main_canonical));

        // Stale submodule should NOT be in the map
        let stale_canonical = canonicalize_path(&stale_submodule).unwrap();
        assert!(
            !repos.contains_key(&stale_canonical),
            "Stale submodule should not be detected"
        );
    }

    #[test]
    fn test_detect_gitdir_missing_head_skipped() {
        // Gitdir exists but is missing HEAD file = invalid
        let temp = TempDir::new().unwrap();

        // Create main repo
        let main_repo = temp.path().join("main");
        create_git_repo(&main_repo);

        // Create submodule with gitdir that exists but has no HEAD
        let submodule = main_repo.join("deps/invalid");
        std::fs::create_dir_all(&submodule).unwrap();

        // Create the gitdir target without HEAD file
        let gitdir_target = main_repo.join(".git/modules/invalid");
        std::fs::create_dir_all(&gitdir_target).unwrap();
        // Deliberately NOT creating HEAD file

        // Write .git file pointing to that gitdir
        std::fs::write(
            submodule.join(".git"),
            format!("gitdir: {}\n", gitdir_target.display()),
        )
        .unwrap();

        let repos = detect_repos_under(temp.path());

        // Should only find main repo
        assert_eq!(repos.len(), 1, "Gitdir without HEAD should be skipped");

        let main_canonical = canonicalize_path(&main_repo).unwrap();
        assert!(repos.contains_key(&main_canonical));
    }
}
