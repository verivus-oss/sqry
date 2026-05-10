//! Index discovery module for finding unified graph in ancestor directories.
//!
//! This module implements git-like behavior where sqry walks up the directory
//! tree to find the nearest graph index, enabling queries from subdirectories to
//! automatically use a parent index with appropriate scope filtering.

use sqry_core::workspace::{MAX_ANCESTOR_DEPTH, WorkspaceRootDiscovery, discover_workspace_root};
use std::path::{Path, PathBuf};

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

/// Find the nearest unified graph (or legacy `.sqry-index` file) by
/// walking up from the given path.
///
/// # Algorithm
///
/// 1. First consult [`discover_workspace_root`] (cluster-E §E.1). The walk
///    is bounded by [`MAX_ANCESTOR_DEPTH`] and stops at the first project
///    marker (`.git`, `Cargo.toml`, `package.json`, `pyproject.toml`,
///    `go.mod`). A graph above the project boundary is discarded — this
///    eliminates the "stray `~/.sqry/graph`" foot-gun where a leftover
///    graph at `$HOME` was silently picked up for a brand-new project.
/// 2. If the discovery returns `BoundaryOnly`, also walk for the legacy
///    `.sqry-index` file from `start` up to (but not above) the project
///    boundary, since `discover_workspace_root` already records legacy
///    `.sqry-index` files but does so without producing an
///    `IndexLocation`. We keep this fallback path for backward
///    compatibility with v1 layouts.
///
/// # Returns
///
/// * `Some(IndexLocation)` if a unified graph (or legacy index) was found
///   inside the project boundary.
/// * `None` if no usable index exists in any ancestor below the project
///   boundary, or if the walk hit [`MAX_ANCESTOR_DEPTH`] without finding
///   either.
#[must_use]
pub fn find_nearest_index(start: &Path) -> Option<IndexLocation> {
    let query_scope = start.to_path_buf();

    // Canonicalize for consistent path matching; fall back to original if fails
    // (e.g., permission denied, path doesn't exist yet)
    let canonical_start = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());

    // Determine if input is a file or directory
    let is_file_query = canonical_start.is_file();

    // Step 1: bounded discovery via the shared workspace walker.
    let (boundary, graph_root, depth_to_graph) = match discover_workspace_root(&canonical_start) {
        WorkspaceRootDiscovery::GraphFound {
            root,
            boundary,
            depth,
            ..
        } => (Some(boundary), Some(root), Some(depth)),
        WorkspaceRootDiscovery::BoundaryOnly { boundary, .. } => (Some(boundary), None, None),
        WorkspaceRootDiscovery::None => (None, None, None),
    };

    if let (Some(root), Some(depth)) = (graph_root, depth_to_graph) {
        // `depth` is measured from the canonicalised start (or its parent for
        // file inputs). The legacy `is_ancestor` flag fires whenever the
        // index lives above the *original* request, including the file-input
        // case (where we walked up from the parent directory).
        let is_ancestor = depth > 0;
        return Some(IndexLocation {
            index_root: root,
            query_scope: query_scope.canonicalize().unwrap_or(query_scope),
            is_ancestor,
            is_file_query,
            requires_scope_filter: is_ancestor || is_file_query,
        });
    }

    // Step 2: legacy `.sqry-index` fallback — walk up from `start` (or its
    // parent for file inputs) but never above the project boundary.
    let mut dir: PathBuf = if is_file_query {
        canonical_start
            .parent()
            .map_or_else(|| canonical_start.clone(), Path::to_path_buf)
    } else {
        canonical_start.clone()
    };
    if dir.is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        dir = cwd.join(&dir);
    }

    for ancestor_depth in 0..MAX_ANCESTOR_DEPTH {
        let legacy_index_path = dir.join(INDEX_FILE_NAME);
        if legacy_index_path.exists() && legacy_index_path.is_file() {
            let is_ancestor = ancestor_depth > 0;
            return Some(IndexLocation {
                index_root: dir,
                query_scope: query_scope.canonicalize().unwrap_or(query_scope),
                is_ancestor,
                is_file_query,
                requires_scope_filter: is_ancestor || is_file_query,
            });
        }
        // Stop at the project boundary so a stray legacy index in $HOME is
        // never picked up for a project that has its own marker.
        if let Some(b) = boundary.as_ref()
            && &dir == b
        {
            break;
        }
        if !dir.pop() {
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
/// - A leading character that the query lexer does not accept as a word start
///   (the lexer's `is_word_start` permits only ASCII alphabetic + `_`, so paths
///   beginning with `.`, `-`, a digit, `/`, etc. must be quoted).
fn path_needs_quoting(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    let leading_requires_quoting = path_str
        .chars()
        .next()
        .is_some_and(|c| !c.is_ascii_alphabetic() && c != '_');
    leading_requires_quoting
        || path_str
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
    fn augment_query_path_with_leading_dot() {
        // Paths starting with `.` (e.g. hidden directories or git worktrees under
        // `.worktrees/`) must be quoted because the query lexer's word-start rule
        // accepts only ASCII alpha + `_`. An unquoted `path:.worktrees/...` value
        // parses as `path:` followed by a stray `.` and fails.
        let result = augment_query_with_scope(
            "kind:function",
            Path::new(".worktrees/phase3a/test-fixtures/cli-basic"),
            false,
        );
        assert_eq!(
            result,
            "(kind:function) AND path:\".worktrees/phase3a/test-fixtures/cli-basic/**\""
        );
    }

    #[test]
    fn augment_query_path_with_leading_digit() {
        // Paths starting with a digit similarly violate the lexer's word-start rule.
        let result =
            augment_query_with_scope("kind:function", Path::new("2024-archive/src"), false);
        assert_eq!(result, "(kind:function) AND path:\"2024-archive/src/**\"");
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
