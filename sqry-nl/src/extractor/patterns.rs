//! Pattern extraction for paths, trace-path, and relations.

use regex::Regex;
use std::sync::LazyLock;

/// Pattern for path-like strings
static PATH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches: src/lib.rs, **/*.rs, foo/bar/**, etc.
    Regex::new(r"(?:^|\s)([a-zA-Z0-9_./\-*]+(?:/[a-zA-Z0-9_./\-*]+)+)(?:\s|$)")
        .expect("Invalid path regex")
});

/// Pattern for "from X to Y" style trace-path
static TRACE_PATH_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:from|between)\s+["']?([^"'\s]+)["']?\s+(?:to|and)\s+["']?([^"'\s]+)["']?"#)
        .expect("Invalid trace-path regex")
});

/// Relation type keywords
const RELATION_MAP: &[(&str, &str)] = &[
    ("call", "call"),
    ("calls", "call"),
    ("calling", "call"),
    ("import", "import"),
    ("imports", "import"),
    ("importing", "import"),
    ("export", "export"),
    ("exports", "export"),
    ("exporting", "export"),
    ("inherit", "inherit"),
    ("inherits", "inherit"),
    ("inheritance", "inherit"),
    ("extends", "inherit"),
    ("implement", "impl"),
    ("implements", "impl"),
    ("impl", "impl"),
];

/// Extract path patterns from input.
#[must_use]
pub fn extract_paths(input: &str) -> Vec<String> {
    let mut paths = Vec::new();

    for cap in PATH_PATTERN.captures_iter(input) {
        if let Some(path) = cap.get(1) {
            let path_str = path.as_str().to_string();
            // Filter out things that look like language names
            if !is_likely_not_path(&path_str) && !paths.contains(&path_str) {
                paths.push(path_str);
            }
        }
    }

    paths
}

/// Check if a string is likely not a path (e.g., language name).
fn is_likely_not_path(s: &str) -> bool {
    // Very short strings without slashes are likely not paths
    if s.len() < 3 && !s.contains('/') {
        return true;
    }
    // Known non-path patterns
    matches!(
        s.to_lowercase().as_str(),
        "c++" | "c/c++" | "node.js" | "vue.js" | "react.js"
    )
}

/// Extract trace-path from/to symbols.
///
/// Returns (`from_symbol`, `to_symbol`) if found.
#[must_use]
pub fn extract_trace_path(
    input: &str,
    quoted_spans: &[String],
) -> (Option<String>, Option<String>) {
    // Priority 1: Check quoted spans (need exactly 2)
    if quoted_spans.len() >= 2 {
        // Check if input contains trace-path indicators
        let lower = input.to_lowercase();
        if lower.contains("from")
            || lower.contains("to")
            || lower.contains("between")
            || lower.contains("trace")
            || lower.contains("path")
        {
            return (Some(quoted_spans[0].clone()), Some(quoted_spans[1].clone()));
        }
    }

    // Priority 2: Pattern matching
    if let Some(caps) = TRACE_PATH_PATTERN.captures(input) {
        let from = caps.get(1).map(|m| m.as_str().to_string());
        let to = caps.get(2).map(|m| m.as_str().to_string());
        return (from, to);
    }

    (None, None)
}

/// Extract relation type from input.
#[must_use]
pub fn extract_relation(input: &str) -> Option<String> {
    let input_lower = input.to_lowercase();

    for (keyword, relation) in RELATION_MAP {
        let pattern = format!(r"\b{}\b", regex::escape(keyword));
        if let Ok(re) = Regex::new(&pattern)
            && re.is_match(&input_lower)
        {
            return Some((*relation).to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_paths() {
        let paths = extract_paths("find foo in src/lib.rs");
        assert!(paths.contains(&"src/lib.rs".to_string()));
    }

    #[test]
    fn test_extract_glob_paths() {
        let paths = extract_paths("find foo in src/**/*.rs");
        assert!(paths.contains(&"src/**/*.rs".to_string()));
    }

    #[test]
    fn test_extract_trace_path_quoted() {
        let (from, to) = extract_trace_path(
            "trace from \"login\" to \"database\"",
            &["login".to_string(), "database".to_string()],
        );
        assert_eq!(from, Some("login".to_string()));
        assert_eq!(to, Some("database".to_string()));
    }

    #[test]
    fn test_extract_trace_path_pattern() {
        let (from, to) = extract_trace_path("trace from authenticate to save_user", &[]);
        assert_eq!(from, Some("authenticate".to_string()));
        assert_eq!(to, Some("save_user".to_string()));
    }

    #[test]
    fn test_extract_trace_path_between() {
        let (from, to) = extract_trace_path("path between login and logout", &[]);
        assert_eq!(from, Some("login".to_string()));
        assert_eq!(to, Some("logout".to_string()));
    }

    #[test]
    fn test_extract_relation_call() {
        assert_eq!(
            extract_relation("show call graph"),
            Some("call".to_string())
        );
        assert_eq!(extract_relation("who calls foo"), Some("call".to_string()));
    }

    #[test]
    fn test_extract_relation_import() {
        assert_eq!(extract_relation("show imports"), Some("import".to_string()));
    }

    #[test]
    fn test_extract_relation_inherit() {
        assert_eq!(
            extract_relation("show inheritance"),
            Some("inherit".to_string())
        );
        assert_eq!(
            extract_relation("what extends Foo"),
            Some("inherit".to_string())
        );
    }

    #[test]
    fn test_extract_relation_impl() {
        assert_eq!(
            extract_relation("what implements Trait"),
            Some("impl".to_string())
        );
    }
}
