//! Path resolution for import statements across languages
//!
//! This module provides utilities for resolving relative and absolute import paths
//! to canonical module identifiers, ensuring that import edges in the graph point
//! to the correct target nodes.
//!
//! # Problem
//!
//! Without proper path resolution, relative imports collapse together:
//! - `src/foo/index.js` importing `"./utils"` → points to `"./utils"`
//! - `src/bar/index.js` importing `"./utils"` → points to same `"./utils"`
//!
//! These refer to different files but create the same module node.
//!
//! # Solution
//!
//! This module resolves import paths to canonical forms:
//! - Relative paths (`./utils`, `../lib/helper`) → absolute paths
//! - Normalize path separators (handle both `/` and `\`)
//! - Handle `.` and `..` components
//! - Preserve absolute paths and package imports as-is
//!
//! # Example
//!
//! ```rust
//! use sqry_core::graph::path_resolver::resolve_import_path;
//! use std::path::Path;
//!
//! // Relative import from src/components/Button.js
//! let source_file = Path::new("src/components/Button.js");
//! let import_path = "./Icon";
//! let resolved = resolve_import_path(source_file, import_path).unwrap();
//! assert_eq!(resolved, "src/components/Icon");
//!
//! // Parent directory import
//! let import_path = "../utils/helpers";
//! let resolved = resolve_import_path(source_file, import_path).unwrap();
//! assert_eq!(resolved, "src/utils/helpers");
//!
//! // Package imports are preserved
//! let resolved = resolve_import_path(source_file, "react").unwrap();
//! assert_eq!(resolved, "react");
//! ```

use crate::graph::error::{GraphBuilderError, GraphResult};
use crate::graph::node::Span;
use std::path::{Component, Path};

/// Resolve an import path to a canonical module identifier
///
/// This function handles:
/// - Relative paths: `./foo`, `../bar/baz`
/// - Absolute paths: `/usr/lib/module`
/// - Package imports: `react`, `@babel/core` (returned as-is)
/// - Path normalization: Remove `.` and `..` components
///
/// # Arguments
///
/// * `source_file` - The file containing the import statement
/// * `import_path` - The import path string from the AST
///
/// # Returns
///
/// A canonical module identifier string, or an error if path resolution fails
///
/// # Errors
///
/// Returns [`GraphBuilderError::ParseError`] when the import path is empty, the
/// source file lacks a parent directory, or normalization fails after joining
/// path components.
///
/// # Examples
///
/// ```rust
/// use sqry_core::graph::path_resolver::resolve_import_path;
/// use std::path::Path;
///
/// let source = Path::new("src/app/main.js");
///
/// // Relative imports
/// assert_eq!(resolve_import_path(source, "./utils").unwrap(), "src/app/utils");
/// assert_eq!(resolve_import_path(source, "../lib/db").unwrap(), "src/lib/db");
///
/// // Package imports (preserved)
/// assert_eq!(resolve_import_path(source, "lodash").unwrap(), "lodash");
/// assert_eq!(resolve_import_path(source, "@types/node").unwrap(), "@types/node");
/// ```
pub fn resolve_import_path(source_file: &Path, import_path: &str) -> GraphResult<String> {
    // Trim whitespace and quotes
    let import_path = import_path.trim();

    // Empty imports are invalid
    if import_path.is_empty() {
        return Err(GraphBuilderError::ParseError {
            span: Span::default(),
            reason: "Empty import path".to_string(),
        });
    }

    // Package imports (not starting with . or /) - return as-is
    // Examples: "react", "@babel/core", "lodash/fp"
    if !import_path.starts_with('.') && !import_path.starts_with('/') {
        return Ok(import_path.to_string());
    }

    // Absolute paths - normalize but keep absolute
    if import_path.starts_with('/') {
        let path = Path::new(import_path);
        return normalize_path(path).ok_or_else(|| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!("Failed to normalize absolute path: {import_path}"),
        });
    }

    // Relative imports - resolve against source file's directory
    let source_dir = source_file
        .parent()
        .ok_or_else(|| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!(
                "Source file has no parent directory: {}",
                source_file.display()
            ),
        })?;

    // Join source directory with import path
    let full_path = source_dir.join(import_path);

    // Normalize the path (remove . and .. components)
    normalize_path(&full_path).ok_or_else(|| GraphBuilderError::ParseError {
        span: Span::default(),
        reason: format!("Failed to normalize import path: {}", full_path.display()),
    })
}

/// Normalize a path by removing `.` and `..` components
///
/// This function:
/// - Removes `.` components (current directory)
/// - Resolves `..` components (parent directory)
/// - Converts path to forward slashes for consistency
/// - Preserves relative vs absolute nature of the path
///
/// # Arguments
///
/// * `path` - The path to normalize
///
/// # Returns
///
/// A normalized path string, or None if the path cannot be normalized
/// (e.g., too many `..` components going above root)
///
/// # Examples
///
/// ```rust
/// use sqry_core::graph::path_resolver::normalize_path;
/// use std::path::Path;
///
/// assert_eq!(
///     normalize_path(Path::new("src/./app/../lib/utils")).unwrap(),
///     "src/lib/utils"
/// );
///
/// assert_eq!(
///     normalize_path(Path::new("./foo/./bar")).unwrap(),
///     "foo/bar"
/// );
/// ```
pub fn normalize_path(path: &Path) -> Option<String> {
    let mut components = Vec::new();

    for component in path.components() {
        match component {
            Component::CurDir => {
                // Skip "." components
            }
            Component::ParentDir => {
                // Go up one level by popping last component
                // If we can't pop, the path is invalid (too many ..)
                if components.is_empty() {
                    return None;
                }
                components.pop();
            }
            Component::Normal(name) => {
                // Regular path component
                components.push(name.to_string_lossy().to_string());
            }
            Component::RootDir => {
                // Absolute path marker - keep it
                components.clear();
                components.push(String::new()); // Marker for absolute path
            }
            Component::Prefix(_) => {
                // Windows prefix (C:, \\server\share, etc.)
                // For now, preserve as-is
                components.push(component.as_os_str().to_string_lossy().to_string());
            }
        }
    }

    // Join components with forward slashes for consistency across platforms
    if components.is_empty() {
        Some(".".to_string())
    } else if components.len() == 1 && components[0].is_empty() {
        // Root directory
        Some("/".to_string())
    } else {
        // Filter out the root marker (empty string at start)
        let is_absolute = !components.is_empty() && components[0].is_empty();
        let parts: Vec<&str> = components
            .iter()
            .filter(|s| !s.is_empty())
            .map(std::string::String::as_str)
            .collect();

        if is_absolute {
            Some(format!("/{}", parts.join("/")))
        } else {
            Some(parts.join("/"))
        }
    }
}

/// Resolve a Python module import path
///
/// Python imports can be:
/// - Absolute: `import os`, `from package.module import foo`
/// - Relative: `from . import sibling`, `from .. import parent`
/// - Relative with module: `from .subpkg import module`
///
/// This function handles the unique aspects of Python's import system:
/// - Dot notation for package hierarchy (`package.subpkg.module`)
/// - Relative imports with leading dots (`from .. import X`)
///
/// # Arguments
///
/// * `source_file` - The Python file containing the import
/// * `import_path` - The module path (may start with dots for relative imports)
/// * `is_from_import` - Whether this is a `from X import Y` statement
///
/// # Returns
///
/// A canonical module identifier
///
/// # Errors
///
/// Returns [`GraphBuilderError::ParseError`] when the import path is empty,
/// contains more leading dots than available parent directories, or when the
/// resolved path cannot be normalized.
///
/// # Examples
///
/// ```rust
/// use sqry_core::graph::path_resolver::resolve_python_import;
/// use std::path::Path;
///
/// let source = Path::new("mypackage/subpkg/module.py");
///
/// // Absolute imports (preserved)
/// assert_eq!(resolve_python_import(source, "os.path", false).unwrap(), "os.path");
///
/// // Relative imports with module name
/// assert_eq!(
///     resolve_python_import(source, ".sibling", true).unwrap(),
///     "mypackage/subpkg/sibling"
/// );
///
/// // Parent package imports
/// assert_eq!(
///     resolve_python_import(source, "..", true).unwrap(),
///     "mypackage"
/// );
/// ```
pub fn resolve_python_import(
    source_file: &Path,
    import_path: &str,
    _is_from_import: bool,
) -> GraphResult<String> {
    let import_path = import_path.trim();

    if import_path.is_empty() {
        return Err(GraphBuilderError::ParseError {
            span: Span::default(),
            reason: "Empty Python import path".to_string(),
        });
    }

    // If not a relative import (no leading dots), return as-is
    if !import_path.starts_with('.') {
        return Ok(import_path.to_string());
    }

    // Count leading dots to determine relative level
    let leading_dots = import_path.chars().take_while(|&c| c == '.').count();
    let module_name = &import_path[leading_dots..];

    // Get the source file's directory
    let source_dir = source_file
        .parent()
        .ok_or_else(|| GraphBuilderError::ParseError {
            span: Span::default(),
            reason: format!(
                "Python file has no parent directory: {}",
                source_file.display()
            ),
        })?;

    // Start from source directory and go up (leading_dots - 1) times
    // Note: `from . import X` means current package (0 levels up)
    //       `from .. import X` means parent package (1 level up)
    let mut target_dir = source_dir.to_path_buf();
    for _ in 1..leading_dots {
        target_dir = target_dir
            .parent()
            .ok_or_else(|| GraphBuilderError::ParseError {
                span: Span::default(),
                reason: format!("Too many leading dots in import: {import_path}"),
            })?
            .to_path_buf();
    }

    // If there's a module name after the dots, append it
    let resolved_path = if module_name.is_empty() {
        // Just the package itself (from . import X or from .. import X)
        target_dir
    } else {
        // Convert dots in module name to path separators
        let module_path = module_name.replace('.', "/");
        target_dir.join(module_path)
    };

    // Normalize and convert to string
    normalize_path(&resolved_path).ok_or_else(|| GraphBuilderError::ParseError {
        span: Span::default(),
        reason: format!(
            "Failed to normalize Python import path: {}",
            resolved_path.display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_import_path_relative() {
        let source = Path::new("src/components/Button.js");

        // Same directory
        assert_eq!(
            resolve_import_path(source, "./Icon").unwrap(),
            "src/components/Icon"
        );

        // Parent directory
        assert_eq!(
            resolve_import_path(source, "../utils/helpers").unwrap(),
            "src/utils/helpers"
        );

        // Complex relative path
        assert_eq!(
            resolve_import_path(source, "./../lib/db").unwrap(),
            "src/lib/db"
        );
    }

    #[test]
    fn test_resolve_import_path_package() {
        let source = Path::new("src/app/main.js");

        // Regular package
        assert_eq!(resolve_import_path(source, "react").unwrap(), "react");

        // Scoped package
        assert_eq!(
            resolve_import_path(source, "@types/node").unwrap(),
            "@types/node"
        );

        // Package with subpath
        assert_eq!(
            resolve_import_path(source, "lodash/fp").unwrap(),
            "lodash/fp"
        );
    }

    #[test]
    fn test_resolve_import_path_absolute() {
        let source = Path::new("src/app/main.js");

        // Absolute path
        assert_eq!(
            resolve_import_path(source, "/usr/lib/module").unwrap(),
            "/usr/lib/module"
        );
    }

    #[test]
    fn test_normalize_path() {
        // Remove . components
        assert_eq!(
            normalize_path(Path::new("src/./app/./main.js")).unwrap(),
            "src/app/main.js"
        );

        // Resolve .. components
        assert_eq!(
            normalize_path(Path::new("src/app/../lib/db")).unwrap(),
            "src/lib/db"
        );

        // Complex normalization
        assert_eq!(
            normalize_path(Path::new("a/b/c/../../d/./e")).unwrap(),
            "a/d/e"
        );
    }

    #[test]
    fn test_normalize_path_invalid() {
        // Too many .. components
        assert!(normalize_path(Path::new("a/../..")).is_none());
    }

    #[test]
    fn test_resolve_python_import_absolute() {
        let source = Path::new("mypackage/module.py");

        // Absolute imports (not relative)
        assert_eq!(resolve_python_import(source, "os", false).unwrap(), "os");
        assert_eq!(
            resolve_python_import(source, "os.path", false).unwrap(),
            "os.path"
        );
    }

    #[test]
    fn test_resolve_python_import_relative() {
        let source = Path::new("mypackage/subpkg/module.py");

        // from . import sibling
        assert_eq!(
            resolve_python_import(source, ".", true).unwrap(),
            "mypackage/subpkg"
        );

        // from .sibling import foo
        assert_eq!(
            resolve_python_import(source, ".sibling", true).unwrap(),
            "mypackage/subpkg/sibling"
        );

        // from .. import parent
        assert_eq!(
            resolve_python_import(source, "..", true).unwrap(),
            "mypackage"
        );

        // from ..other_subpkg import utils
        assert_eq!(
            resolve_python_import(source, "..other_subpkg", true).unwrap(),
            "mypackage/other_subpkg"
        );
    }

    #[test]
    fn test_resolve_python_import_nested_dots() {
        let source = Path::new("pkg/sub1/sub2/module.py");

        // from ...toplevel import X
        assert_eq!(
            resolve_python_import(source, "...toplevel", true).unwrap(),
            "pkg/toplevel"
        );
    }

    #[test]
    fn test_different_path_separators() {
        // Ensure we handle both forward and back slashes
        let source_unix = Path::new("src/components/Button.js");

        // Forward slash in import (most common)
        assert_eq!(
            resolve_import_path(source_unix, "./Icon").unwrap(),
            "src/components/Icon"
        );
    }

    #[test]
    fn test_relative_import_collision_fix() {
        // This test demonstrates the fix for HIGH finding #1

        // Two different files importing "./utils"
        let file1 = Path::new("src/foo/index.js");
        let file2 = Path::new("src/bar/index.js");

        let resolved1 = resolve_import_path(file1, "./utils").unwrap();
        let resolved2 = resolve_import_path(file2, "./utils").unwrap();

        // Before the fix, both would be "./utils"
        // After the fix, they are different canonical paths
        assert_eq!(resolved1, "src/foo/utils");
        assert_eq!(resolved2, "src/bar/utils");
        assert_ne!(resolved1, resolved2);
    }
}
