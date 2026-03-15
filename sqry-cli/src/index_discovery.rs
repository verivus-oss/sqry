//! Index discovery module for finding unified graph in ancestor directories.
//!
//! This module implements git-like behavior where sqry walks up the directory
//! tree to find the nearest graph index, enabling queries from subdirectories to
//! automatically use a parent index with appropriate scope filtering.

use sqry_core::graph::unified::persistence::GraphStorage;
use std::path::{Path, PathBuf};

/// Maximum depth to traverse upward (security limit).
const MAX_ANCESTOR_DEPTH: usize = 64;

/// Legacy index file name constant (deprecated).
pub const INDEX_FILE_NAME: &str = ".sqry-index";

/// Characters that need escaping in path patterns for sqry query language.
/// These are glob metacharacters that would be interpreted specially.
const PATH_ESCAPE_CHARS: &[char] = &['*', '?', '[', ']', '{', '}', '\\'];

/// Result of index discovery, containing location and scope information.
#[derive(Debug, Clone)]
pub struct IndexLocation {
    /// Absolute path to the directory containing .sqry-index
    pub index_root: PathBuf,

    /// Original path the user requested (for scoping results)
    pub query_scope: PathBuf,

    /// True if index was found in an ancestor directory (relative to start dir)
    pub is_ancestor: bool,

    /// True if the query scope is a file (not a directory)
    pub is_file_query: bool,

    /// True if query augmentation/filtering is needed.
    /// This is true when:
    /// - Index is in ancestor directory (`is_ancestor`), OR
    /// - Query targets a specific file (`is_file_query`)
    ///
    /// Note: File queries always need filtering even when the index
    /// is in the file's parent directory (`is_ancestor` would be false
    /// due to how we start discovery from the parent).
    pub requires_scope_filter: bool,
}

impl IndexLocation {
    /// Get the relative path from `index_root` to `query_scope` for filtering.
    ///
    /// Returns:
    /// - `Some(relative_path)` when scope filtering is needed and path is inside index root
    /// - `None` when no filtering needed (`query_scope` == `index_root` and !`is_file_query`)
    /// - `None` when `query_scope` is outside `index_root` (edge case, shouldn't happen)
    ///
    /// Note: Uses `requires_scope_filter` (not `is_ancestor`) to ensure file queries
    /// in the index root still compute their relative scope for exact-match filtering.
    #[must_use]
    pub fn relative_scope(&self) -> Option<PathBuf> {
        if self.requires_scope_filter {
            self.query_scope
                .strip_prefix(&self.index_root)
                .ok()
                .map(Path::to_path_buf)
        } else {
            None
        }
    }
}

/// Find the nearest .sqry-index by walking up from the given path.
///
/// # Algorithm
/// 1. Canonicalize the start path (resolve symlinks, make absolute)
/// 2. Check for .sqry-index in current directory
/// 3. If not found, move to parent and repeat
/// 4. Stop at filesystem root or `MAX_ANCESTOR_DEPTH`
///
/// # Arguments
/// * `start` - The directory or file to start searching from
///
/// # Returns
/// * `Some(IndexLocation)` if an index was found
/// * `None` if no index exists in any ancestor
#[must_use]
pub fn find_nearest_index(start: &Path) -> Option<IndexLocation> {
    let query_scope = start.to_path_buf();

    // Canonicalize for consistent path matching; fall back to original if fails
    // (e.g., permission denied, path doesn't exist yet)
    let canonical_start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());

    // Determine if input is a file or directory
    // For file paths, start discovery from the parent directory
    let (mut ancestor_dir, is_file_query) = if canonical_start.is_file() {
        let parent = canonical_start
            .parent()
            .map_or_else(|| canonical_start.clone(), Path::to_path_buf);
        (parent, true)
    } else {
        (canonical_start, false)
    };

    // Ensure we have an absolute path for traversal
    if ancestor_dir.is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        ancestor_dir = cwd.join(&ancestor_dir);
    }

    for ancestor_depth in 0..MAX_ANCESTOR_DEPTH {
        // Check for unified graph format first
        let storage = GraphStorage::new(&ancestor_dir);
        if storage.exists() {
            let is_ancestor = ancestor_depth > 0;
            return Some(IndexLocation {
                index_root: ancestor_dir,
                query_scope: query_scope.canonicalize().unwrap_or(query_scope),
                is_ancestor,
                is_file_query,
                // File queries always need filtering, even when index is in parent
                requires_scope_filter: is_ancestor || is_file_query,
            });
        }

        // Fallback: check for legacy .sqry-index format
        let legacy_index_path = ancestor_dir.join(INDEX_FILE_NAME);
        if legacy_index_path.exists() && legacy_index_path.is_file() {
            let is_ancestor = ancestor_depth > 0;
            return Some(IndexLocation {
                index_root: ancestor_dir,
                query_scope: query_scope.canonicalize().unwrap_or(query_scope),
                is_ancestor,
                is_file_query,
                requires_scope_filter: is_ancestor || is_file_query,
            });
        }

        // Move to parent directory
        if !ancestor_dir.pop() {
            // Reached filesystem root
            break;
        }
    }

    None
}

/// Escape special characters in a path component for safe use in path: predicate.
/// Also normalizes Windows backslashes to forward slashes for consistent query syntax.
///
/// # Double Escaping for Glob Patterns
/// Glob metacharacters need double-escaping because there are two parsing stages:
/// 1. Query lexer: `\\[` → `\[` (consumes one level of escaping)
/// 2. Globset matcher: `\[` → literal `[` (consumes second level)
///
/// Without double-escaping, `src/[test]` would become `path:"src/\[test\]/**"`,
/// lexer would yield `src/[test]/**`, and globset would treat `[test]` as a
/// character class instead of a literal directory name.
fn escape_path_for_query(path: &Path) -> String {
    let path_str = path.to_string_lossy();
    let mut escaped = String::with_capacity(path_str.len() + 20);

    for ch in path_str.chars() {
        // Normalize Windows backslashes to forward slashes
        if ch == '\\' && cfg!(windows) {
            escaped.push('/');
            continue;
        }
        if ch == '\\' {
            // Backslash needs 4 chars: `\\\\` → lexer `\\` → globset `\`
            escaped.push_str("\\\\\\\\");
        } else if PATH_ESCAPE_CHARS.contains(&ch) {
            // Other glob chars: `\\[` → lexer `\[` → globset literal `[`
            escaped.push_str("\\\\");
            escaped.push(ch);
        } else {
            escaped.push(ch);
        }
    }

    escaped
}

/// Check if a path requires quoting due to special characters.
/// Paths need quoting when they contain:
/// - Spaces or double quotes (for tokenization)
/// - Glob metacharacters with escapes (backslash escapes only work in quoted strings)
fn path_needs_quoting(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    path_str
        .chars()
        .any(|c| c == ' ' || c == '"' || PATH_ESCAPE_CHARS.contains(&c))
}

/// Augment a query with an implicit path filter when using ancestor index.
///
/// # Arguments
/// * `query` - Original query string
/// * `relative_scope` - Path relative to index root to filter by
/// * `is_file_query` - True if scope is a file, false if directory
///
/// # Returns
/// Query string with path filter appended
///
/// # Path Handling
/// - Paths with spaces, quotes, or glob metacharacters are quoted automatically
/// - Inside quotes, glob metacharacters are escaped with backslashes
/// - The implicit filter is `ANDed` with the original query
/// - Parentheses ensure correct precedence
/// - File queries use exact path match; directory queries use `/**` glob
#[must_use]
pub fn augment_query_with_scope(query: &str, relative_scope: &Path, is_file_query: bool) -> String {
    // Empty scope means no filtering needed
    if relative_scope.as_os_str().is_empty() {
        return query.to_string();
    }

    // Build the path filter pattern
    // - File query: exact match (no glob suffix)
    // - Directory query: recursive glob (/**)
    let scope_pattern = if path_needs_quoting(relative_scope) {
        // Escape glob metacharacters (backslash escapes only work in quoted strings)
        let escaped_path = escape_path_for_query(relative_scope);
        // Also escape internal double quotes
        let quoted = escaped_path.replace('"', "\\\"");
        if is_file_query {
            format!("\"{quoted}\"")
        } else {
            format!("\"{quoted}/**\"")
        }
    } else {
        // Simple path without special characters - use unquoted
        let path_str = relative_scope.to_string_lossy();
        if is_file_query {
            path_str.into_owned()
        } else {
            format!("{path_str}/**")
        }
    };

    let path_filter = format!("path:{scope_pattern}");

    if query.trim().is_empty() {
        path_filter
    } else {
        // Wrap original query in parentheses to preserve precedence
        // Example: "kind:fn OR kind:method" -> "(kind:fn OR kind:method) AND path:src/**"
        format!("({query}) AND {path_filter}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper to create a minimal index file for discovery tests.
    fn create_test_index(path: &Path) {
        let index_path = path.join(INDEX_FILE_NAME);
        fs::write(&index_path, "test-index-marker").unwrap();
    }

    #[test]
    fn find_nearest_index_at_current_dir() {
        let tmp = TempDir::new().unwrap();
        create_test_index(tmp.path());

        let result = find_nearest_index(tmp.path());

        assert!(result.is_some());
        let loc = result.unwrap();
        assert_eq!(loc.index_root, tmp.path().canonicalize().unwrap());
        assert!(!loc.is_ancestor);
        assert!(!loc.is_file_query);
        assert!(!loc.requires_scope_filter);
    }

    #[test]
    fn find_nearest_index_in_parent() {
        let tmp = TempDir::new().unwrap();
        create_test_index(tmp.path());

        let subdir = tmp.path().join("src");
        fs::create_dir(&subdir).unwrap();

        let result = find_nearest_index(&subdir);

        assert!(result.is_some());
        let loc = result.unwrap();
        assert_eq!(loc.index_root, tmp.path().canonicalize().unwrap());
        assert!(loc.is_ancestor);
        assert!(!loc.is_file_query);
        assert!(loc.requires_scope_filter);
    }

    #[test]
    fn find_nearest_index_in_grandparent() {
        let tmp = TempDir::new().unwrap();
        create_test_index(tmp.path());

        let deep = tmp.path().join("src").join("utils");
        fs::create_dir_all(&deep).unwrap();

        let result = find_nearest_index(&deep);

        assert!(result.is_some());
        let loc = result.unwrap();
        assert_eq!(loc.index_root, tmp.path().canonicalize().unwrap());
        assert!(loc.is_ancestor);
        assert!(loc.requires_scope_filter);
    }

    #[test]
    fn find_nearest_index_none_found() {
        let tmp = TempDir::new().unwrap();
        // No index created

        let result = find_nearest_index(tmp.path());

        // The search traverses ancestor directories, so if a .sqry/ exists
        // in an ancestor of the temp dir (e.g. /tmp/.sqry/ from a previous
        // run), it will be found. We only assert no index was found *within*
        // the temp dir itself.
        match &result {
            None => {} // expected
            Some(loc) => {
                let tmp_canonical = tmp.path().canonicalize().unwrap();
                assert!(
                    !loc.index_root.starts_with(&tmp_canonical),
                    "found unexpected index inside temp dir: {:?}",
                    loc.index_root
                );
            }
        }
    }

    #[test]
    fn find_nearest_index_nested_repos() {
        let tmp = TempDir::new().unwrap();
        create_test_index(tmp.path()); // Root index

        let inner = tmp.path().join("packages").join("web");
        fs::create_dir_all(&inner).unwrap();
        create_test_index(&inner); // Inner index

        let query_path = inner.join("src");
        fs::create_dir(&query_path).unwrap();

        let result = find_nearest_index(&query_path);

        // Should find the nearest (inner) index
        assert!(result.is_some());
        let loc = result.unwrap();
        assert_eq!(loc.index_root, inner.canonicalize().unwrap());
        assert!(loc.is_ancestor);
    }

    #[test]
    fn find_nearest_index_file_input() {
        let tmp = TempDir::new().unwrap();
        create_test_index(tmp.path());

        let subdir = tmp.path().join("src");
        fs::create_dir(&subdir).unwrap();
        let file = subdir.join("main.rs");
        fs::write(&file, "fn main() {}").unwrap();

        let result = find_nearest_index(&file);

        assert!(result.is_some());
        let loc = result.unwrap();
        assert!(loc.is_file_query);
        assert!(loc.is_ancestor); // Index is in grandparent
        assert!(loc.requires_scope_filter);
    }

    #[test]
    fn find_nearest_index_file_in_index_dir() {
        let tmp = TempDir::new().unwrap();
        create_test_index(tmp.path());

        let file = tmp.path().join("main.rs");
        fs::write(&file, "fn main() {}").unwrap();

        let result = find_nearest_index(&file);

        assert!(result.is_some());
        let loc = result.unwrap();
        assert!(!loc.is_ancestor); // Index is in file's parent
        assert!(loc.is_file_query);
        assert!(loc.requires_scope_filter); // File queries always need filtering
    }

    #[test]
    fn relative_scope_calculation() {
        let loc = IndexLocation {
            index_root: PathBuf::from("/project"),
            query_scope: PathBuf::from("/project/src/utils"),
            is_ancestor: true,
            is_file_query: false,
            requires_scope_filter: true,
        };

        let scope = loc.relative_scope();
        assert_eq!(scope, Some(PathBuf::from("src/utils")));
    }

    #[test]
    fn relative_scope_same_dir() {
        let loc = IndexLocation {
            index_root: PathBuf::from("/project"),
            query_scope: PathBuf::from("/project"),
            is_ancestor: false,
            is_file_query: false,
            requires_scope_filter: false,
        };

        let scope = loc.relative_scope();
        assert!(scope.is_none());
    }

    #[test]
    fn relative_scope_file_in_root() {
        let loc = IndexLocation {
            index_root: PathBuf::from("/project"),
            query_scope: PathBuf::from("/project/main.rs"),
            is_ancestor: false,
            is_file_query: true,
            requires_scope_filter: true,
        };

        let scope = loc.relative_scope();
        assert_eq!(scope, Some(PathBuf::from("main.rs")));
    }

    #[test]
    fn augment_query_with_scope_basic() {
        let result = augment_query_with_scope("kind:function", Path::new("src"), false);
        assert_eq!(result, "(kind:function) AND path:src/**");
    }

    #[test]
    fn augment_query_with_scope_empty_query() {
        let result = augment_query_with_scope("", Path::new("src"), false);
        assert_eq!(result, "path:src/**");
    }

    #[test]
    fn augment_query_with_scope_empty_path() {
        let result = augment_query_with_scope("kind:fn", Path::new(""), false);
        assert_eq!(result, "kind:fn");
    }

    #[test]
    fn augment_query_with_scope_file_query() {
        let result = augment_query_with_scope("kind:function", Path::new("src/main.rs"), true);
        assert_eq!(result, "(kind:function) AND path:src/main.rs");
    }

    #[test]
    fn augment_query_with_scope_directory_query() {
        let result = augment_query_with_scope("kind:function", Path::new("src"), false);
        assert_eq!(result, "(kind:function) AND path:src/**");
    }

    #[test]
    fn augment_query_file_with_spaces() {
        let result =
            augment_query_with_scope("kind:function", Path::new("my project/main.rs"), true);
        assert_eq!(result, "(kind:function) AND path:\"my project/main.rs\"");
    }

    #[test]
    fn augment_query_with_scope_path_with_spaces() {
        let result = augment_query_with_scope("kind:function", Path::new("my project/src"), false);
        assert_eq!(result, "(kind:function) AND path:\"my project/src/**\"");
    }

    #[test]
    fn augment_query_with_scope_path_with_glob_chars() {
        // Paths with glob metacharacters must be quoted and double-escaped:
        // - CLI emits `\\[` so lexer returns `\[` for globset to interpret as literal `[`
        let result = augment_query_with_scope("kind:function", Path::new("src/[test]"), false);
        assert_eq!(result, "(kind:function) AND path:\"src/\\\\[test\\\\]/**\"");
    }

    #[test]
    fn augment_query_preserves_precedence() {
        let result = augment_query_with_scope("kind:fn OR kind:method", Path::new("src"), false);
        assert_eq!(result, "(kind:fn OR kind:method) AND path:src/**");
    }

    #[test]
    fn augment_query_with_existing_path_predicate() {
        let result =
            augment_query_with_scope("kind:fn AND path:*.rs", Path::new("src/utils"), false);
        assert_eq!(result, "(kind:fn AND path:*.rs) AND path:src/utils/**");
    }

    #[test]
    #[cfg(unix)]
    fn escape_path_with_backslash_on_unix() {
        // Backslash in path gets double-escaped: `\` → `\\\\` (4 chars in raw string)
        // So lexer returns `\\` and globset matches literal backslash
        let result = escape_path_for_query(Path::new("src/file\\name"));
        assert_eq!(result, "src/file\\\\\\\\name");
    }

    /// Test that augmented queries with special characters can be parsed by the lexer.
    /// This ensures the escaping strategy produces valid query syntax.
    #[test]
    fn augmented_queries_are_parseable() {
        use sqry_core::query::Lexer;

        let test_cases = [
            // Simple path (no escaping needed)
            ("kind:fn", Path::new("src"), false),
            // Path with spaces (quoted)
            ("kind:fn", Path::new("my project/src"), false),
            // Path with glob metacharacters (quoted + escaped)
            ("kind:fn", Path::new("src/[test]"), false),
            ("kind:fn", Path::new("src/test*"), false),
            ("kind:fn", Path::new("src/test?"), false),
            ("kind:fn", Path::new("src/{a,b}"), false),
            // File queries
            ("kind:fn", Path::new("src/main.rs"), true),
            ("kind:fn", Path::new("src/[test]/main.rs"), true),
            // Complex query with special path
            ("kind:fn OR kind:method", Path::new("src/[utils]"), false),
        ];

        for (query, path, is_file) in test_cases {
            let augmented = augment_query_with_scope(query, path, is_file);
            let mut lexer = Lexer::new(&augmented);
            let result = lexer.tokenize();
            assert!(
                result.is_ok(),
                "Failed to parse augmented query for path {:?}: {:?}\nQuery: {}",
                path,
                result.err(),
                augmented
            );
        }
    }
}
