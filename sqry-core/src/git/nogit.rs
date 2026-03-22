//! `NoGit` fallback backend
//!
//! This backend is used when git is unavailable or disabled.
//! It always reports no changes, causing the system to fall back
//! to hash-based change detection.

use super::{ChangeSet, GitBackend, GitCapabilities, Result};
use std::path::{Path, PathBuf};

/// No-op git backend that always reports no changes
///
/// This backend is used when:
/// - Git binary is not found in PATH
/// - User sets `SQRY_GIT_BACKEND=none`
/// - Directory is not a git repository
///
/// It allows the rest of the system to gracefully degrade to
/// hash-based change detection without special-casing git absence.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoGit;

impl GitBackend for NoGit {
    fn is_repo(&self, _root: &Path) -> Result<bool> {
        // NoGit backend always reports "not a repo"
        Ok(false)
    }

    fn repo_root(&self, root: &Path) -> Result<PathBuf> {
        // Return the input path as-is since there's no git root
        Ok(root.to_path_buf())
    }

    fn head(&self, _root: &Path) -> Result<Option<String>> {
        // No git means no HEAD
        Ok(None)
    }

    fn uncommitted(
        &self,
        _root: &Path,
        _include_untracked: bool,
    ) -> Result<(ChangeSet, Option<String>)> {
        // No changes, no HEAD
        Ok((ChangeSet::new(), None))
    }

    fn since(
        &self,
        _root: &Path,
        _baseline: &str,
        _rename_similarity: u8,
    ) -> Result<(ChangeSet, Option<String>)> {
        // No changes, no HEAD
        Ok((ChangeSet::new(), None))
    }

    fn capabilities(&self) -> GitCapabilities {
        // NoGit supports nothing
        GitCapabilities {
            supports_blame: false,
            supports_time_travel: false,
            supports_history_index: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nogit_is_repo() {
        let backend = NoGit;
        let result = backend.is_repo(Path::new("/any/path"));
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_nogit_repo_root() {
        let backend = NoGit;
        let test_path = Path::new("/test/path");
        let result = backend.repo_root(test_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), test_path);
    }

    #[test]
    fn test_nogit_head() {
        let backend = NoGit;
        let result = backend.head(Path::new("/any/path"));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn test_nogit_uncommitted() {
        let backend = NoGit;
        let (changes, head) = backend.uncommitted(Path::new("/any/path"), true).unwrap();
        assert!(changes.is_empty());
        assert_eq!(head, None);
    }

    #[test]
    fn test_nogit_since() {
        let backend = NoGit;
        let (changes, head) = backend.since(Path::new("/any/path"), "abc123", 50).unwrap();
        assert!(changes.is_empty());
        assert_eq!(head, None);
    }

    #[test]
    fn test_nogit_capabilities() {
        let backend = NoGit;
        let caps = backend.capabilities();
        assert!(!caps.supports_blame);
        assert!(!caps.supports_time_travel);
        assert!(!caps.supports_history_index);
    }
}
