//! Path canonicalization and resolution utilities for Project root handling
//!
//! Implements the path handling strategy from `PROJECT_ROOT_SPEC.md` and `02_DESIGN.md` \[H1\].
//! All file paths are canonicalized before root resolution to ensure the invariant
//! "at most one Project per `index_root`" holds regardless of how paths are accessed.

use std::io;
use std::path::{Component, Path, PathBuf};

/// Canonicalize a path, resolving symlinks where possible.
///
/// This function attempts full canonicalization (resolving symlinks, `.`, `..`).
/// On failure (path doesn't exist, permission denied, circular symlinks), it
/// falls back to [`absolutize_without_resolution`] which normalizes without
/// touching the filesystem.
///
/// # Platform Behavior
///
/// - **Linux**: Resolves symbolic links via `realpath(3)`
/// - **macOS**: Resolves symbolic links; macOS aliases are NOT resolved
/// - **Windows**: Resolves junction points, symbolic links, and NTFS reparse points
///
/// # Edge Cases (per `02_DESIGN.md` H1)
///
/// - **Broken symlinks**: Uses absolutized path; logs warning
/// - **Circular symlinks**: Canonicalization fails; uses absolutized path; logs error
/// - **Permission denied**: Uses absolutized path; logs warning with context
///
/// # Errors
///
/// Returns an error only if both canonicalization AND absolutize fail,
/// which should only happen if `current_dir()` fails (extremely rare).
///
/// # Examples
///
/// ```
/// use sqry_core::project::path_utils::canonicalize_path;
/// use std::path::Path;
///
/// // Existing path - fully canonicalized
/// let result = canonicalize_path(Path::new("/tmp"));
/// assert!(result.is_ok());
///
/// // Non-existent path - absolutized without resolution
/// let result = canonicalize_path(Path::new("/nonexistent/path/file.rs"));
/// assert!(result.is_ok()); // Falls back to absolutize
/// ```
pub fn canonicalize_path(path: &Path) -> Result<PathBuf, io::Error> {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical),
        Err(e) => {
            // Log the fallback - caller should handle appropriately
            log::debug!(
                "Canonicalization failed with {:?}; using absolutize fallback.",
                e.kind()
            );
            absolutize_without_resolution(path)
        }
    }
}

/// Absolutize a path without touching the filesystem.
///
/// This function provides a deterministic fallback when canonicalization fails.
/// It:
/// 1. Joins relative paths with the current working directory
/// 2. Normalizes `.` and `..` components (purely lexically)
///
/// # Determinism Guarantee (per `02_DESIGN.md` C4)
///
/// This function is deterministic: two accesses to the same logical directory
/// (even via different relative paths) produce the same result when called from
/// the same working directory. This prevents duplicate Project creation.
///
/// # Errors
///
/// Returns an error if `std::env::current_dir()` fails (extremely rare).
///
/// # Examples
///
/// ```
/// use sqry_core::project::path_utils::absolutize_without_resolution;
/// use std::path::Path;
///
/// // Relative paths are joined with CWD and normalized
/// let result1 = absolutize_without_resolution(Path::new("./foo/../bar"));
/// let result2 = absolutize_without_resolution(Path::new("bar"));
/// // Both resolve to same path (when called from same CWD)
/// ```
pub fn absolutize_without_resolution(path: &Path) -> Result<PathBuf, io::Error> {
    // Get current working directory
    let cwd = std::env::current_dir()?;

    // Join with path if relative
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };

    // Normalize . and .. components (without touching filesystem)
    let normalized = normalize_path_components(&absolute);

    Ok(normalized)
}

/// Normalize path components lexically (without filesystem access).
///
/// Handles `.` (current dir) and `..` (parent dir) components:
/// - `.` components are removed
/// - `..` components pop the previous component if possible
/// - Preserves root prefix
/// - Never produces empty path (returns "." if result would be empty)
///
/// # Platform Notes
///
/// - On Unix: Preserves leading `/`
/// - On Windows: Preserves drive prefix (`C:\`) and UNC paths
///
/// # Examples
///
/// ```
/// use sqry_core::project::path_utils::normalize_path_components;
/// use std::path::Path;
///
/// let path = Path::new("/home/user/../user/./project");
/// let normalized = normalize_path_components(path);
/// assert_eq!(normalized, Path::new("/home/user/project"));
/// ```
#[must_use]
pub fn normalize_path_components(path: &Path) -> PathBuf {
    let mut components = Vec::new();

    for component in path.components() {
        match component {
            Component::CurDir => {
                // Skip `.` components
            }
            Component::ParentDir => {
                // Pop last component if it's a normal component
                // Don't pop RootDir, Prefix, or if empty
                match components.last() {
                    Some(Component::Normal(_)) => {
                        components.pop();
                    }
                    Some(Component::ParentDir) | None => {
                        // Keep .. if we can't pop further (relative path going above start)
                        components.push(component);
                    }
                    _ => {
                        // Don't pop RootDir or Prefix
                    }
                }
            }
            _ => {
                // Keep Prefix, RootDir, and Normal components
                components.push(component);
            }
        }
    }

    // Reconstruct path from components
    if components.is_empty() {
        PathBuf::from(".")
    } else {
        components.iter().collect()
    }
}

/// Default directories to skip during repository detection.
///
/// These directories are commonly large dependency/build/cache directories
/// that rarely contain git repositories worth indexing.
///
/// Note: `.git` is intentionally NOT in this list (we need to detect it).
///
/// Users can override this list via configuration (see Phase 5: Configuration Integration).
pub const DEFAULT_IGNORED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "dist",
    "vendor",
    ".cache",
    ".npm",
    ".cargo",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".tox",
    ".venv",
    "venv",
    ".gradle",
    ".idea",
    ".vs",
    ".vscode",
];

/// Check if a directory entry should be ignored during repository detection.
///
/// Per `02_DESIGN.md`, we skip common ignored directories to speed up walking.
/// Uses [`DEFAULT_IGNORED_DIRS`] for the ignore list.
///
/// Note: `.git` directories are NOT ignored (we need to detect them).
///
/// # Arguments
///
/// * `name` - The directory name to check
///
/// # See Also
///
/// Use [`is_ignored_dir_with_config`] for custom ignore lists.
#[must_use]
pub fn is_ignored_dir(name: &std::ffi::OsStr) -> bool {
    is_ignored_dir_with_config(name, DEFAULT_IGNORED_DIRS)
}

/// Check if a directory entry should be ignored, using a custom ignore list.
///
/// This allows configuration of which directories to skip during repository
/// detection. Useful when the default list doesn't match project needs.
///
/// # Arguments
///
/// * `name` - The directory name to check
/// * `ignored_dirs` - List of directory names to ignore
///
/// # Examples
///
/// ```
/// use sqry_core::project::path_utils::{is_ignored_dir_with_config, DEFAULT_IGNORED_DIRS};
/// use std::ffi::OsStr;
///
/// // Using custom ignore list
/// let custom_ignores = &["my_deps", "cached_stuff"];
/// assert!(is_ignored_dir_with_config(OsStr::new("my_deps"), custom_ignores));
/// assert!(!is_ignored_dir_with_config(OsStr::new("node_modules"), custom_ignores));
///
/// // Using default list
/// assert!(is_ignored_dir_with_config(OsStr::new("node_modules"), DEFAULT_IGNORED_DIRS));
/// ```
#[must_use]
pub fn is_ignored_dir_with_config(name: &std::ffi::OsStr, ignored_dirs: &[&str]) -> bool {
    // Convert OsStr to str for comparison (if possible)
    if let Some(name_str) = name.to_str() {
        ignored_dirs.contains(&name_str)
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_normalize_removes_current_dir() {
        let path = Path::new("/home/./user/./project");
        let result = normalize_path_components(path);
        assert_eq!(result, Path::new("/home/user/project"));
    }

    #[test]
    fn test_normalize_resolves_parent_dir() {
        let path = Path::new("/home/user/../other/project");
        let result = normalize_path_components(path);
        assert_eq!(result, Path::new("/home/other/project"));
    }

    #[test]
    fn test_normalize_combined() {
        let path = Path::new("/home/user/../user/./project/./src/../lib");
        let result = normalize_path_components(path);
        assert_eq!(result, Path::new("/home/user/project/lib"));
    }

    #[test]
    fn test_normalize_preserves_root() {
        let path = Path::new("/");
        let result = normalize_path_components(path);
        assert_eq!(result, Path::new("/"));
    }

    #[test]
    fn test_normalize_relative_path() {
        let path = Path::new("foo/../bar");
        let result = normalize_path_components(path);
        assert_eq!(result, Path::new("bar"));
    }

    #[test]
    fn test_normalize_relative_above_start() {
        // Can't go above start of relative path - preserve ..
        let path = Path::new("../foo");
        let result = normalize_path_components(path);
        assert_eq!(result, Path::new("../foo"));
    }

    #[test]
    fn test_normalize_empty_result() {
        // Should return "." not empty path
        let path = Path::new("foo/..");
        let result = normalize_path_components(path);
        assert_eq!(result, Path::new("."));
    }

    #[test]
    fn test_absolutize_determinism() {
        // Per C4: same logical path via different relative paths should produce same result
        // This test must run from a consistent CWD
        let result1 = absolutize_without_resolution(Path::new("./foo/../bar")).unwrap();
        let result2 = absolutize_without_resolution(Path::new("bar")).unwrap();
        assert_eq!(result1, result2);
    }

    #[test]
    fn test_absolutize_absolute_path_unchanged() {
        #[cfg(unix)]
        let path = Path::new("/absolute/path");
        #[cfg(windows)]
        let path = Path::new("C:\\absolute\\path");
        let result = absolutize_without_resolution(path).unwrap();
        assert_eq!(result, path);
    }

    #[test]
    fn test_canonicalize_existing_path() {
        // /tmp should exist on Unix systems
        #[cfg(unix)]
        {
            let result = canonicalize_path(Path::new("/tmp"));
            assert!(result.is_ok());
            // Result should be absolute
            assert!(result.unwrap().is_absolute());
        }
    }

    #[test]
    fn test_canonicalize_nonexistent_path_uses_fallback() {
        let path = Path::new("/nonexistent/deeply/nested/path");
        let result = canonicalize_path(path);
        // Should succeed via fallback
        assert!(result.is_ok());
        let resolved = result.unwrap();
        // Should be absolute
        assert!(resolved.is_absolute());
        // Should preserve the path structure (normalized)
        assert!(resolved.to_string_lossy().contains("nonexistent"));
    }

    #[test]
    fn test_is_ignored_dir() {
        use std::ffi::OsStr;

        assert!(is_ignored_dir(OsStr::new("node_modules")));
        assert!(is_ignored_dir(OsStr::new("target")));
        assert!(is_ignored_dir(OsStr::new("__pycache__")));

        // .git is NOT ignored (we need to detect it)
        assert!(!is_ignored_dir(OsStr::new(".git")));
        assert!(!is_ignored_dir(OsStr::new("src")));
        assert!(!is_ignored_dir(OsStr::new("lib")));
    }

    #[test]
    fn test_is_ignored_dir_with_config_custom_list() {
        use std::ffi::OsStr;

        // Custom ignore list
        let custom_ignores = &["my_deps", "cached_stuff", "third_party"];

        // Custom dirs should be ignored
        assert!(is_ignored_dir_with_config(
            OsStr::new("my_deps"),
            custom_ignores
        ));
        assert!(is_ignored_dir_with_config(
            OsStr::new("cached_stuff"),
            custom_ignores
        ));
        assert!(is_ignored_dir_with_config(
            OsStr::new("third_party"),
            custom_ignores
        ));

        // Default dirs NOT in custom list should NOT be ignored
        assert!(!is_ignored_dir_with_config(
            OsStr::new("node_modules"),
            custom_ignores
        ));
        assert!(!is_ignored_dir_with_config(
            OsStr::new("target"),
            custom_ignores
        ));

        // Normal dirs should NOT be ignored
        assert!(!is_ignored_dir_with_config(
            OsStr::new("src"),
            custom_ignores
        ));
        assert!(!is_ignored_dir_with_config(
            OsStr::new(".git"),
            custom_ignores
        ));
    }

    #[test]
    fn test_is_ignored_dir_with_config_empty_list() {
        use std::ffi::OsStr;

        // Empty ignore list = nothing ignored
        let empty: &[&str] = &[];

        assert!(!is_ignored_dir_with_config(
            OsStr::new("node_modules"),
            empty
        ));
        assert!(!is_ignored_dir_with_config(OsStr::new("target"), empty));
        assert!(!is_ignored_dir_with_config(OsStr::new("src"), empty));
    }

    #[test]
    fn test_is_ignored_dir_with_config_default_list() {
        use std::ffi::OsStr;

        // Using DEFAULT_IGNORED_DIRS should match is_ignored_dir()
        assert_eq!(
            is_ignored_dir(OsStr::new("node_modules")),
            is_ignored_dir_with_config(OsStr::new("node_modules"), DEFAULT_IGNORED_DIRS)
        );
        assert_eq!(
            is_ignored_dir(OsStr::new("src")),
            is_ignored_dir_with_config(OsStr::new("src"), DEFAULT_IGNORED_DIRS)
        );
    }

    #[test]
    fn test_default_ignored_dirs_contains_common_dirs() {
        // Verify the default list contains expected directories
        assert!(DEFAULT_IGNORED_DIRS.contains(&"node_modules"));
        assert!(DEFAULT_IGNORED_DIRS.contains(&"target"));
        assert!(DEFAULT_IGNORED_DIRS.contains(&"vendor"));
        assert!(DEFAULT_IGNORED_DIRS.contains(&"__pycache__"));
        assert!(DEFAULT_IGNORED_DIRS.contains(&".venv"));
        assert!(DEFAULT_IGNORED_DIRS.contains(&".idea"));

        // .git should NOT be in the list
        assert!(!DEFAULT_IGNORED_DIRS.contains(&".git"));
    }

    #[cfg(unix)]
    #[test]
    fn test_canonicalize_symlink() {
        use std::os::unix::fs::symlink;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let target = temp.path().join("target_dir");
        let link = temp.path().join("link");

        // Create target directory and symlink
        std::fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();

        // Canonicalize should resolve symlink
        let result = canonicalize_path(&link).unwrap();
        let expected = canonicalize_path(&target).unwrap();
        assert_eq!(result, expected);
    }

    #[cfg(unix)]
    #[test]
    fn test_canonicalize_broken_symlink_uses_fallback() {
        use std::os::unix::fs::symlink;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let link = temp.path().join("broken_link");

        // Create symlink to nonexistent target
        symlink("/nonexistent/target", &link).unwrap();

        // Canonicalize should fall back to absolutize
        let result = canonicalize_path(&link);
        assert!(result.is_ok());
        // Result should be absolutized version of link path
        let resolved = result.unwrap();
        assert!(resolved.is_absolute());
    }
}
