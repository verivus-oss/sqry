//! Pattern-based detection of paths in arbitrary string fields.
//!
//! This module detects embedded paths in string values that may not be
//! explicitly marked as path fields (e.g., paths in error messages, log entries).

use regex::Regex;
use std::sync::LazyLock;

/// Patterns for detecting file URIs.
static FILE_URI_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"file:///[^\s"'><\]\)]+"#).expect("valid regex"));

/// Patterns for detecting Unix absolute paths.
static UNIX_PATH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:/(?:home|Users|var|srv|opt|tmp|etc)/[^\s"'><\]\)]+)"#).expect("valid regex")
});

/// Patterns for detecting Windows absolute paths.
static WINDOWS_PATH_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[A-Za-z]:\\[^\s"'><\]\)]+"#).expect("valid regex"));

/// Patterns for detecting UNC paths.
static UNC_PATH_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\\\\[a-zA-Z0-9_.\-]+\\[^\s"'><\]\)]+"#).expect("valid regex"));

/// A detected path in a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedPath {
    /// Start position in the original string.
    pub start: usize,
    /// End position in the original string.
    pub end: usize,
    /// The detected path string.
    pub path: String,
    /// Type of path detected.
    pub kind: DetectedPathKind,
}

/// Type of detected path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedPathKind {
    /// `file:///` URI.
    FileUri,
    /// Unix absolute path.
    UnixPath,
    /// Windows absolute path with drive letter.
    WindowsPath,
    /// UNC network path.
    UncPath,
}

/// Detect paths embedded in a string value.
///
/// Scans the input for path patterns and returns all detected paths.
///
/// # Example
///
/// ```rust
/// use sqry_mcp_redaction::rules::detect_paths_in_string;
///
/// let text = "Error in /home/user/project/src/main.rs at line 42";
/// let paths = detect_paths_in_string(text);
/// assert_eq!(paths.len(), 1);
/// assert!(paths[0].path.contains("main.rs"));
/// ```
#[must_use]
pub fn detect_paths_in_string(input: &str) -> Vec<DetectedPath> {
    let mut paths = Vec::new();

    // Check for file:// URIs
    for m in FILE_URI_PATTERN.find_iter(input) {
        paths.push(DetectedPath {
            start: m.start(),
            end: m.end(),
            path: m.as_str().to_string(),
            kind: DetectedPathKind::FileUri,
        });
    }

    // Check for Unix paths
    for m in UNIX_PATH_PATTERN.find_iter(input) {
        // Skip if this is part of a file:// URI
        if is_covered_by(&paths, m.start(), m.end()) {
            continue;
        }
        paths.push(DetectedPath {
            start: m.start(),
            end: m.end(),
            path: m.as_str().to_string(),
            kind: DetectedPathKind::UnixPath,
        });
    }

    // Check for Windows paths
    for m in WINDOWS_PATH_PATTERN.find_iter(input) {
        if is_covered_by(&paths, m.start(), m.end()) {
            continue;
        }
        paths.push(DetectedPath {
            start: m.start(),
            end: m.end(),
            path: m.as_str().to_string(),
            kind: DetectedPathKind::WindowsPath,
        });
    }

    // Check for UNC paths
    for m in UNC_PATH_PATTERN.find_iter(input) {
        if is_covered_by(&paths, m.start(), m.end()) {
            continue;
        }
        paths.push(DetectedPath {
            start: m.start(),
            end: m.end(),
            path: m.as_str().to_string(),
            kind: DetectedPathKind::UncPath,
        });
    }

    // Sort by start position
    paths.sort_by_key(|p| p.start);

    paths
}

/// Check if a range is covered by an existing detected path.
fn is_covered_by(paths: &[DetectedPath], start: usize, end: usize) -> bool {
    paths.iter().any(|p| p.start <= start && end <= p.end)
}

/// Redact paths in a string, replacing them with the provided replacement.
///
/// # Arguments
///
/// * `input` - The input string
/// * `workspace_root` - Optional workspace root for relative conversion
/// * `workspace_placeholder` - Placeholder for workspace paths
/// * `hash_filenames` - Whether to hash filenames
/// * `hash_salt` - Optional salt for hashing
///
/// # Returns
///
/// Tuple of (redacted string, number of paths redacted).
pub fn redact_paths_in_string(
    input: &str,
    workspace_root: Option<&str>,
    workspace_placeholder: &str,
    hash_filenames: bool,
    hash_salt: Option<&str>,
) -> (String, usize) {
    let paths = detect_paths_in_string(input);

    if paths.is_empty() {
        return (input.to_string(), 0);
    }

    let mut result = String::with_capacity(input.len());
    let mut last_end = 0;

    for detected in &paths {
        // Add text before this path
        result.push_str(&input[last_end..detected.start]);

        // Redact the path
        match super::path::redact_path(
            &detected.path,
            workspace_root,
            workspace_placeholder,
            hash_filenames,
            hash_salt,
        ) {
            Ok(redacted) => result.push_str(&redacted),
            Err(_) => {
                // On error, use a generic placeholder
                result.push_str(workspace_placeholder);
            }
        }

        last_end = detected.end;
    }

    // Add remaining text
    result.push_str(&input[last_end..]);

    (result, paths.len())
}

/// Check if a string contains any path patterns.
#[must_use]
pub fn contains_path_pattern(input: &str) -> bool {
    FILE_URI_PATTERN.is_match(input)
        || UNIX_PATH_PATTERN.is_match(input)
        || WINDOWS_PATH_PATTERN.is_match(input)
        || UNC_PATH_PATTERN.is_match(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_file_uri() {
        let paths = detect_paths_in_string("See file:///home/user/file.rs");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, DetectedPathKind::FileUri);
        assert_eq!(paths[0].path, "file:///home/user/file.rs");
    }

    #[test]
    fn test_detect_unix_path() {
        let paths = detect_paths_in_string("Error in /home/user/project/src/main.rs");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, DetectedPathKind::UnixPath);
        assert!(paths[0].path.contains("main.rs"));
    }

    #[test]
    fn test_detect_windows_path() {
        let paths = detect_paths_in_string("File at C:\\Users\\john\\project\\file.rs");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, DetectedPathKind::WindowsPath);
        assert!(paths[0].path.contains("file.rs"));
    }

    #[test]
    fn test_detect_unc_path() {
        let paths = detect_paths_in_string("Network file \\\\server\\share\\dir\\file.txt");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, DetectedPathKind::UncPath);
        assert!(paths[0].path.contains("share"));
    }

    #[test]
    fn test_detect_multiple_paths() {
        let text = "Files: /home/user/a.rs and /var/log/b.log";
        let paths = detect_paths_in_string(text);
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_no_paths() {
        let paths = detect_paths_in_string("This is just regular text.");
        assert!(paths.is_empty());
    }

    #[test]
    fn test_file_uri_subsumes_unix_path() {
        // The Unix path inside the URI shouldn't be double-detected
        let paths = detect_paths_in_string("file:///home/user/file.rs");
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].kind, DetectedPathKind::FileUri);
    }

    #[test]
    fn test_redact_paths_in_string() {
        let input = "Error in /home/user/project/src/main.rs at line 42";
        let (redacted, count) = redact_paths_in_string(
            input,
            Some("/home/user/project"),
            "<workspace>",
            false,
            None,
        );
        assert_eq!(count, 1);
        assert!(redacted.contains("src/main.rs"));
        assert!(!redacted.contains("/home/user/project"));
    }

    #[test]
    fn test_redact_preserves_surrounding_text() {
        let input = "Before /home/user/project/file.rs After";
        let (redacted, count) = redact_paths_in_string(
            input,
            Some("/home/user/project"),
            "<workspace>",
            false,
            None,
        );
        assert_eq!(count, 1);
        assert!(redacted.starts_with("Before "));
        assert!(redacted.ends_with(" After"));
    }

    #[test]
    fn test_contains_path_pattern() {
        assert!(contains_path_pattern("file:///path"));
        assert!(contains_path_pattern("/home/user/file"));
        assert!(contains_path_pattern("C:\\Users\\file"));
        assert!(contains_path_pattern("\\\\server\\share"));
        assert!(!contains_path_pattern("just text"));
    }

    #[test]
    fn test_path_in_quoted_string() {
        let paths = detect_paths_in_string(r#"Error: "/home/user/file.rs" not found"#);
        assert_eq!(paths.len(), 1);
        // Should not include the quotes
        assert!(!paths[0].path.contains('"'));
    }

    #[test]
    fn test_path_in_json() {
        let paths = detect_paths_in_string(r#"{"path": "/home/user/file.rs"}"#);
        assert_eq!(paths.len(), 1);
    }
}
