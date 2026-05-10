//! Git worktree management for semantic diff operations.
//!
//! This module provides RAII-based management of temporary git worktrees,
//! used for comparing code between different git refs.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use super::{GitError, Result};

/// Resolves a git ref (commit, branch, tag) to its full commit SHA.
///
/// Equivalent to `git -C <repo> rev-parse <ref>^{commit}`. Used by diff
/// callers to short-circuit when both refs resolve to the same commit
/// (the worktree-build + graph-build pipeline is expensive on large
/// repositories — `HEAD HEAD` against a kernel-scale tree blew the
/// 90s CLI / 120s MCP deadlines before this helper was wired in).
///
/// # Errors
///
/// Returns `GitError::NotFound` if `git` is not in `PATH`,
/// `GitError::CommandFailed` if the ref cannot be resolved (unknown ref,
/// or a tag that does not point at a commit), and `GitError::InvalidOutput`
/// if git returned non-UTF-8 or empty stdout.
pub fn resolve_ref_to_commit(repo_path: &Path, git_ref: &str) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["rev-parse", "--verify", &format!("{git_ref}^{{commit}}")])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GitError::NotFound
            } else {
                GitError::Io(e)
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitError::CommandFailed {
            message: format!("Failed to resolve git ref '{git_ref}' to a commit"),
            stdout: String::new(),
            stderr: stderr.trim().to_string(),
        });
    }

    let stdout = String::from_utf8(output.stdout).map_err(|e| {
        GitError::InvalidOutput(format!(
            "git rev-parse returned non-UTF-8 output for ref '{git_ref}': {e}"
        ))
    })?;
    let sha = stdout.trim();
    if sha.is_empty() {
        return Err(GitError::InvalidOutput(format!(
            "git rev-parse returned empty output for ref '{git_ref}'"
        )));
    }
    Ok(sha.to_string())
}

/// Manages temporary git worktrees with RAII cleanup guarantees.
///
/// Creates two temporary worktrees for comparing different git refs (commits,
/// branches, or tags). Both worktrees are automatically cleaned up when
/// the manager is dropped.
///
/// # Example
///
/// ```no_run
/// use sqry_core::git::WorktreeManager;
/// use std::path::Path;
///
/// let repo = Path::new("/path/to/repo");
/// let manager = WorktreeManager::create(repo, "main", "feature-branch")?;
///
/// // Access worktree paths
/// let base_path = manager.base_path();
/// let target_path = manager.target_path();
///
/// // Worktrees are cleaned up when manager goes out of scope
/// # Ok::<(), sqry_core::git::GitError>(())
/// ```
#[derive(Debug)]
pub struct WorktreeManager {
    base_dir: TempDir,
    target_dir: TempDir,
    repo_path: PathBuf,
}

impl WorktreeManager {
    /// Creates two temporary worktrees for the given refs.
    ///
    /// # Arguments
    ///
    /// * `repo_path` - Path to the git repository root
    /// * `base_ref` - Git ref for the base version (commit, branch, tag)
    /// * `target_ref` - Git ref for the target version (commit, branch, tag)
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Not a git repository
    /// - Refs don't exist
    /// - Git worktree creation fails
    pub fn create(repo_path: &Path, base_ref: &str, target_ref: &str) -> Result<Self> {
        // Validate that this is a git repository
        if !repo_path.join(".git").exists() {
            return Err(GitError::NotARepo(repo_path.to_path_buf()));
        }

        // Validate refs exist
        Self::validate_ref(repo_path, base_ref)?;
        Self::validate_ref(repo_path, target_ref)?;

        // Create temp directories
        let base_dir = TempDir::new().map_err(|e| {
            GitError::Io(std::io::Error::other(format!(
                "Failed to create temporary directory for base worktree: {e}"
            )))
        })?;
        let target_dir = TempDir::new().map_err(|e| {
            GitError::Io(std::io::Error::other(format!(
                "Failed to create temporary directory for target worktree: {e}"
            )))
        })?;

        // Create worktrees
        Self::create_worktree(repo_path, base_ref, base_dir.path())?;
        Self::create_worktree(repo_path, target_ref, target_dir.path())?;

        tracing::debug!(
            base_ref = %base_ref,
            target_ref = %target_ref,
            base_path = %base_dir.path().display(),
            target_path = %target_dir.path().display(),
            "Created git worktrees"
        );

        Ok(Self {
            base_dir,
            target_dir,
            repo_path: repo_path.to_path_buf(),
        })
    }

    /// Returns the path to the base worktree.
    #[must_use]
    pub fn base_path(&self) -> &Path {
        self.base_dir.path()
    }

    /// Returns the path to the target worktree.
    #[must_use]
    pub fn target_path(&self) -> &Path {
        self.target_dir.path()
    }

    /// Returns the original repository path.
    #[must_use]
    pub fn repo_path(&self) -> &Path {
        &self.repo_path
    }

    /// Validates that a git ref exists.
    fn validate_ref(repo_path: &Path, git_ref: &str) -> Result<()> {
        let output = Command::new("git")
            .current_dir(repo_path)
            .args(["rev-parse", "--verify", git_ref])
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    GitError::NotFound
                } else {
                    GitError::Io(e)
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GitError::CommandFailed {
                message: format!("Git ref '{git_ref}' does not exist or is invalid"),
                stdout: String::new(),
                stderr: stderr.trim().to_string(),
            });
        }

        Ok(())
    }

    /// Creates a git worktree at the specified path.
    fn create_worktree(repo_path: &Path, git_ref: &str, worktree_path: &Path) -> Result<()> {
        let worktree_str = worktree_path.to_str().ok_or_else(|| {
            GitError::InvalidOutput(format!(
                "Invalid worktree path: {}",
                worktree_path.display()
            ))
        })?;

        let output = Command::new("git")
            .current_dir(repo_path)
            .args(["worktree", "add", "--detach", worktree_str, git_ref])
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    GitError::NotFound
                } else {
                    GitError::Io(e)
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GitError::CommandFailed {
                message: format!("Git worktree creation failed for ref '{git_ref}'"),
                stdout: String::new(),
                stderr: stderr.trim().to_string(),
            });
        }

        Ok(())
    }

    /// Removes a worktree forcefully.
    fn remove_worktree(repo_path: &Path, worktree_path: &Path) {
        let result = Command::new("git")
            .current_dir(repo_path)
            .args([
                "worktree",
                "remove",
                "--force",
                worktree_path.to_str().unwrap_or(""),
            ])
            .output();

        match result {
            Ok(output) if output.status.success() => {
                tracing::debug!(
                    path = %worktree_path.display(),
                    "Removed git worktree"
                );
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(
                    path = %worktree_path.display(),
                    error = %stderr.trim(),
                    "Failed to remove git worktree"
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %worktree_path.display(),
                    error = %e,
                    "Failed to execute git worktree remove"
                );
            }
        }
    }
}

impl Drop for WorktreeManager {
    fn drop(&mut self) {
        // Clean up worktrees
        // Note: We don't panic in drop, just log warnings if cleanup fails
        Self::remove_worktree(&self.repo_path, self.base_dir.path());
        Self::remove_worktree(&self.repo_path, self.target_dir.path());

        tracing::debug!("WorktreeManager dropped, worktrees cleaned up");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worktree_create_valid_refs() -> Result<()> {
        // This test requires being in a git repository
        let repo_path = Path::new(".");
        if !repo_path.join(".git").exists() {
            eprintln!("Skipping test: not in a git repository");
            return Ok(());
        }

        // Use HEAD as both refs (should always exist)
        let manager = WorktreeManager::create(repo_path, "HEAD", "HEAD")?;

        // Verify directories exist
        assert!(manager.base_path().exists());
        assert!(manager.target_path().exists());

        // Verify they contain .git files (worktree marker)
        assert!(manager.base_path().join(".git").exists());
        assert!(manager.target_path().join(".git").exists());

        // Cleanup happens automatically on drop
        Ok(())
    }

    #[test]
    fn test_worktree_create_invalid_ref() {
        let repo_path = Path::new(".");
        if !repo_path.join(".git").exists() {
            eprintln!("Skipping test: not in a git repository");
            return;
        }

        let result = WorktreeManager::create(repo_path, "this-ref-does-not-exist-12345", "HEAD");

        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_ref_to_commit_returns_full_sha() -> Result<()> {
        let repo_path = Path::new(".");
        if !repo_path.join(".git").exists() {
            eprintln!("Skipping test: not in a git repository");
            return Ok(());
        }

        let sha = resolve_ref_to_commit(repo_path, "HEAD")?;
        assert_eq!(sha.len(), 40, "expected 40-char SHA-1, got: {sha:?}");
        assert!(
            sha.chars().all(|c| c.is_ascii_hexdigit()),
            "expected hex SHA, got: {sha:?}"
        );

        // Idempotence: resolving HEAD twice yields the same SHA, which is
        // the invariant that powers the diff fast-path in #213.
        let sha2 = resolve_ref_to_commit(repo_path, "HEAD")?;
        assert_eq!(sha, sha2);
        Ok(())
    }

    #[test]
    fn test_resolve_ref_to_commit_unknown_ref_errors() {
        let repo_path = Path::new(".");
        if !repo_path.join(".git").exists() {
            eprintln!("Skipping test: not in a git repository");
            return;
        }

        let err = resolve_ref_to_commit(repo_path, "definitely-not-a-real-ref-zz12").unwrap_err();
        assert!(
            matches!(err, GitError::CommandFailed { .. }),
            "expected CommandFailed for unknown ref, got: {err:?}"
        );
    }

    #[test]
    fn test_worktree_cleanup_on_drop() -> Result<()> {
        let repo_path = Path::new(".");
        if !repo_path.join(".git").exists() {
            eprintln!("Skipping test: not in a git repository");
            return Ok(());
        }

        let base_path: PathBuf;
        let target_path: PathBuf;

        {
            let manager = WorktreeManager::create(repo_path, "HEAD", "HEAD")?;
            base_path = manager.base_path().to_path_buf();
            target_path = manager.target_path().to_path_buf();

            assert!(base_path.exists());
            assert!(target_path.exists());

            // Manager drops here
        }

        // Give filesystem a moment to clean up
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Verify worktrees are no longer listed
        let output = Command::new("git")
            .current_dir(repo_path)
            .args(["worktree", "list"])
            .output()?;

        let worktree_list = String::from_utf8_lossy(&output.stdout);
        assert!(!worktree_list.contains(&base_path.to_string_lossy().to_string()));
        assert!(!worktree_list.contains(&target_path.to_string_lossy().to_string()));

        Ok(())
    }
}
