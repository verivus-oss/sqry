//! Path traversal and absolute path detection.

use regex::Regex;
use std::sync::LazyLock;

/// Pattern to match quoted strings (both single and double quotes)
static QUOTED_STRING: LazyLock<Regex> = LazyLock::new(|| {
    // Matches "..." or '...' (including escaped quotes within)
    Regex::new(r#""(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'"#).expect("Invalid quoted string regex")
});

/// Pattern matching path traversal sequences
static PATH_TRAVERSAL: LazyLock<Regex> = LazyLock::new(|| {
    // Matches:
    // - /../ or \..\  (path traversal segment in path)
    // - ../ or ..\  (at start or after whitespace/delimiter)
    // - /.. or \..  (at end or before delimiter)
    // - Leading .. (at absolute start or after whitespace)
    Regex::new(r"(?:^|\s|[/\\])\.\.(?:[/\\]|\s|$)").expect("Invalid path traversal regex")
});

/// Strip quoted strings from input for path traversal detection.
fn strip_quoted_sections(input: &str) -> String {
    QUOTED_STRING.replace_all(input, " ").to_string()
}

/// Check if string contains path traversal patterns.
///
/// This function checks for path traversal patterns like `../` and `/../` but
/// only outside of quoted strings. This allows search patterns like `..*password`
/// or `module..submodule` within quotes while still blocking actual path traversal.
#[must_use]
pub fn contains_path_traversal(input: &str) -> bool {
    // Strip quoted sections to avoid false positives on search patterns
    let unquoted = strip_quoted_sections(input);

    // Check for path traversal patterns
    PATH_TRAVERSAL.is_match(&unquoted)
}

/// Check if string contains absolute paths.
#[must_use]
pub fn contains_absolute_path(input: &str) -> bool {
    // Check for Unix absolute paths - including inside quotes
    // Pattern: "/..." at start of string or after whitespace, quote, or other delimiter
    let mut chars = input.chars().peekable();
    let mut prev_char = ' '; // Start as if preceded by space

    while let Some(c) = chars.next() {
        if c == '/' {
            // Check if this looks like start of absolute path
            // Allow "//" for comments or URLs, but catch single "/"
            if (prev_char == ' ' || prev_char == '"' || prev_char == '\'' || prev_char == '=')
                && chars.peek() != Some(&'/')
            {
                // This is an absolute path (not // comment or URL)
                return true;
            }
        }
        prev_char = c;
    }

    // Also check if input starts with /
    if input.starts_with('/') && !input.starts_with("//") {
        return true;
    }

    // Windows absolute path (C:\, D:\, etc.)
    if input.contains(":\\") || input.contains(":/") {
        return true;
    }

    // UNC path
    if input.contains("\\\\") {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_traversal_unquoted() {
        // Path traversal outside quotes should be detected
        assert!(contains_path_traversal("../foo"));
        assert!(contains_path_traversal("foo/../bar"));
        assert!(contains_path_traversal("foo/.."));
        assert!(contains_path_traversal("..\\foo")); // Windows style
        assert!(contains_path_traversal(".."));
        assert!(!contains_path_traversal("foo/bar"));
        assert!(!contains_path_traversal("foo.bar")); // Single dot is fine
    }

    #[test]
    fn test_path_traversal_quoted() {
        // Path traversal patterns inside quotes should NOT be detected
        // This allows search patterns like "..*password"
        assert!(!contains_path_traversal("sqry query \"..*password\""));
        assert!(!contains_path_traversal("sqry query \"module..submodule\""));
        assert!(!contains_path_traversal("sqry search '..range'"));
        // But actual path traversal outside quotes should still be detected
        assert!(contains_path_traversal("sqry query \"foo\" ../bar"));
    }

    #[test]
    fn test_strip_quoted_sections() {
        assert_eq!(strip_quoted_sections("hello \"world\" foo"), "hello   foo");
        assert_eq!(strip_quoted_sections("hello 'world' foo"), "hello   foo");
    }

    #[test]
    fn test_absolute_unix() {
        assert!(contains_absolute_path("/etc/passwd"));
        assert!(contains_absolute_path("sqry query \"/etc/passwd\""));
    }

    #[test]
    fn test_absolute_windows() {
        assert!(contains_absolute_path("C:\\Windows"));
        assert!(contains_absolute_path("D:/Users"));
    }

    #[test]
    fn test_unc_path() {
        assert!(contains_absolute_path("\\\\server\\share"));
    }

    #[test]
    fn test_relative_paths_not_flagged_as_absolute() {
        assert!(!contains_absolute_path("src/lib.rs"));
        assert!(!contains_absolute_path("sqry query \"src/**/*.rs\""));
    }
}
