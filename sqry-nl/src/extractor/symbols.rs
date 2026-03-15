//! Symbol extraction from natural language.

use regex::Regex;
use std::sync::LazyLock;

use super::languages;

/// Pattern for identifiers including namespace separators
static IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| {
    // Matches: foo, Foo::bar, pkg.Func, module.Class.method
    Regex::new(r"[a-zA-Z_][a-zA-Z0-9_]*(?:(?:::|\.)[a-zA-Z_][a-zA-Z0-9_]*)*")
        .expect("Invalid identifier regex")
});

/// Patterns for "X for Y" where Y is the target symbol to extract
/// These verbs are commands, not symbols to search for
static VERB_FOR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches: "grep for X", "look for X", "search for X"
    // Captures the target X after "for"
    Regex::new(r"(?i)\b(?:grep|look|search)\s+for\s+([a-zA-Z_][a-zA-Z0-9_]*)")
        .expect("Invalid verb-for regex")
});

/// Patterns for "X of Y" where Y is the target symbol
/// e.g., "callers of foo", "usages of bar", "imports of serde"
static OF_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:callers?|callees?|usages?|uses?|references?|imports?|exports?)\s+of\s+([a-zA-Z_][a-zA-Z0-9_]*)")
        .expect("Invalid of-pattern regex")
});

/// Patterns for "X to Y" where Y is the target symbol
/// e.g., "references to foo", "calls to bar"
static TO_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:references?|calls?|path)\s+to\s+([a-zA-Z_][a-zA-Z0-9_]*)")
        .expect("Invalid to-pattern regex")
});

/// Patterns for "X on Y" where Y is the target symbol
/// e.g., "depends on foo"
static ON_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bdepends?\s+on\s+([a-zA-Z_][a-zA-Z0-9_]*)").expect("Invalid on-pattern regex")
});

/// Patterns for verb + symbol (callers pattern)
/// e.g., "who uses X", "what calls X", "what invokes X"
static VERB_SYMBOL_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches: "uses X", "invokes X", "calls X", "does X call"
    Regex::new(r"(?i)\b(?:uses?|invokes?|calls?|does)\s+(?:the\s+)?([a-zA-Z_][a-zA-Z0-9_]+)")
        .expect("Invalid verb-symbol regex")
});

/// Pattern for "grep X" without "for" (e.g., "grep unsafe blocks")
/// Note: We can't use negative look-ahead in Rust regex, so we check separately
static GREP_DIRECT_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    // Matches: "grep X" - we'll filter out "for" matches in the extraction code
    Regex::new(r"(?i)^grep\s+([a-zA-Z_][a-zA-Z0-9_]*)").expect("Invalid grep-direct regex")
});

/// Common stopwords to filter out
const STOPWORDS: &[&str] = &[
    "find",
    "search",
    "show",
    "get",
    "list",
    "where",
    "is",
    "are",
    "the",
    "a",
    "an",
    "all",
    "any",
    "some",
    "that",
    "which",
    "who",
    "what",
    "how",
    "when",
    "from",
    "to",
    "in",
    "on",
    "at",
    "for",
    "with",
    "by",
    "of",
    "and",
    "or",
    "not",
    "if",
    "then",
    "else",
    "called",
    "named",
    "defined",
    "implemented",
    "used",
    "using",
    "calls",
    "callers",
    "callees",
    "trace",
    "path",
    "between",
    "first",
    "top",
    "limit",
    "depth",
    "level",
    "levels",
    "results",
    "matches",
    "hits",
    "functions",
    "classes",
    "structs",
    "enums",
    "traits",
    "interfaces",
    "methods",
    "modules",
    "function",
    "class",
    "struct",
    "enum",
    "trait",
    "interface",
    "method",
    "module",
    "visualize",
    "graph",
    "diagram",
    "mermaid",
    "dot",
    "index",
    "status",
    "check",
    "me",
    "please",
    "can",
    "you",
    "help",
    // CD predicate-related words (should not be extracted as symbols)
    "impl",
    "implementations",
    "implementation",
    "implementing",
    "implements",
    "implement",
    "types",
    "duplicates",
    "duplicate",
    "duplicated",
    "duplication",
    "similar",
    "circular",
    "cyclic",
    "cycles",
    "cycle",
    "dependencies",
    "dependency",
    "unused",
    "dead",
    "unreachable",
    "unreferenced",
    "async",
    "asynchronous",
    "unsafe",
    "blocks",
    "visibility",
    "public",
    "private",
    "code",
    "detection",
];

/// Check if identifier looks like an intentional symbol name (`PascalCase`, `ALL_CAPS`, or has underscores).
/// These should not be filtered as stopwords even if they match.
fn looks_like_symbol_name(ident: &str) -> bool {
    // ALL_CAPS (constants): STATUS, TODO, FIXME
    if ident.chars().all(|c| c.is_ascii_uppercase() || c == '_') && ident.len() > 1 {
        return true;
    }

    // PascalCase (types/classes): Status, UserAuth, HashMap
    if ident.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && ident.chars().skip(1).any(|c| c.is_ascii_lowercase())
    {
        return true;
    }

    // snake_case with underscores (identifiers): user_id, my_function
    if ident.contains('_') && ident.len() > 2 {
        return true;
    }

    // Contains namespace separator: std::collections, pkg.Module
    if ident.contains("::") || (ident.contains('.') && ident.len() > 3) {
        return true;
    }

    false
}

fn push_unique_symbol(symbols: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !symbols.iter().any(|symbol| symbol == value) {
        symbols.push(value.to_string());
    }
}

fn extract_from_patterns(input: &str) -> Option<String> {
    // Try all special patterns in order of specificity
    let patterns: &[&Regex] = &[
        &VERB_FOR_PATTERN,    // "grep for X", "look for X", "search for X"
        &OF_PATTERN,          // "callers of X", "usages of X", "imports of X"
        &TO_PATTERN,          // "references to X", "calls to X"
        &ON_PATTERN,          // "depends on X"
        &VERB_SYMBOL_PATTERN, // "who uses X", "what invokes X"
        &GREP_DIRECT_PATTERN, // "grep X" (without "for")
    ];

    for pattern in patterns {
        if let Some(caps) = pattern.captures(input)
            && let Some(target) = caps.get(1)
        {
            let target_str = target.as_str();
            let target_lower = target_str.to_lowercase();
            // Skip "for" if matched by GREP_DIRECT_PATTERN (since regex doesn't support look-ahead)
            if target_lower == "for" {
                continue;
            }
            // Skip language names and common stopwords
            if !languages::is_known_language(&target_lower)
                && (looks_like_symbol_name(target_str) || !is_stopword(&target_lower))
            {
                return Some(target_str.to_string());
            }
        }
    }

    None
}

fn should_skip_identifier(ident_lower: &str, input_lower: &str) -> bool {
    // Matches action verbs that precede prepositions in the original input.
    // Skip verb commands that precede prepositions (grep, look, search, etc.)
    // These are actions, not symbols to search for
    let verb_preposition_pairs = [
        ("grep", " for"),
        ("look", " for"),
        ("search", " for"),
        ("uses", " of"),
        ("usages", " of"),
        ("callers", " of"),
        ("callees", " of"),
        ("references", " to"),
        ("depends", " on"),
    ];
    let is_verb_with_prep = verb_preposition_pairs.iter().any(|(verb, prep)| {
        ident_lower == *verb && input_lower.contains(&format!("{ident_lower}{prep}"))
    });
    if is_verb_with_prep {
        return true;
    }

    // Also skip common action words that appear before the target
    matches!(
        ident_lower,
        "grep" | "invokes" | "invoke" | "uses" | "does" | "imports" | "references"
    )
}

/// Extract symbols from input, preferring quoted strings.
#[must_use]
pub fn extract_symbols(input: &str, quoted_spans: &[String]) -> Vec<String> {
    let mut symbols = Vec::new();

    // Priority 1: Quoted strings (already extracted during preprocessing)
    push_quoted_symbols(&mut symbols, quoted_spans);

    // Priority 2: Special extraction patterns (preposition-based)
    // These patterns extract the TARGET symbol after prepositions
    if symbols.is_empty() {
        extract_pattern_symbol(input, &mut symbols);
    }

    // Priority 3: Unquoted identifiers (fallback when no special patterns match)
    if symbols.is_empty() {
        extract_identifier_symbols(input, &mut symbols);
    }

    // Warn about unquoted generics
    warn_unquoted_generics(input);

    symbols
}

fn push_quoted_symbols(symbols: &mut Vec<String>, quoted_spans: &[String]) {
    for span in quoted_spans {
        push_unique_symbol(symbols, span);
    }
}

fn extract_pattern_symbol(input: &str, symbols: &mut Vec<String>) {
    if let Some(target) = extract_from_patterns(input) {
        push_unique_symbol(symbols, &target);
    }
}

fn extract_identifier_symbols(input: &str, symbols: &mut Vec<String>) {
    let input_lower = input.to_lowercase();
    for cap in IDENTIFIER.captures_iter(input) {
        let ident = cap.get(0).unwrap().as_str();
        let ident_lower = ident.to_lowercase();

        // Skip language names (rust, python, etc.)
        if languages::is_known_language(&ident_lower) {
            continue;
        }

        if should_skip_identifier(&ident_lower, &input_lower) {
            continue;
        }

        // Keep identifiers that look like symbol names (PascalCase, ALL_CAPS, snake_case)
        // even if they match stopwords
        if looks_like_symbol_name(ident) {
            push_unique_symbol(symbols, ident);
        } else if !is_stopword(&ident_lower) {
            // For lowercase words, apply stopword filtering
            push_unique_symbol(symbols, ident);
        }
    }
}

fn warn_unquoted_generics(input: &str) {
    if input.contains('<') && !input.contains('"') && !input.contains('\'') {
        tracing::warn!("Unquoted generics detected. Use quotes for generic types: \"Vec<String>\"");
    }
}

/// Check if a word is a stopword.
#[must_use]
pub fn is_stopword(word: &str) -> bool {
    STOPWORDS.contains(&word.to_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractor::languages::is_known_language;

    #[test]
    fn test_extract_simple() {
        let symbols = extract_symbols("find authentication", &[]);
        assert!(symbols.contains(&"authentication".to_string()));
    }

    #[test]
    fn test_extract_quoted_priority() {
        let symbols = extract_symbols(
            "find \"UserAuth\" authentication",
            &["UserAuth".to_string()],
        );
        // Should only have quoted symbol
        assert_eq!(symbols, vec!["UserAuth"]);
    }

    #[test]
    fn test_extract_namespaced() {
        let symbols = extract_symbols("find std::collections::HashMap", &[]);
        assert!(symbols.contains(&"std::collections::HashMap".to_string()));
    }

    #[test]
    fn test_extract_dotted() {
        let symbols = extract_symbols("find pkg.Func", &[]);
        assert!(symbols.contains(&"pkg.Func".to_string()));
    }

    #[test]
    fn test_stopword_filtering() {
        let symbols = extract_symbols("find all functions", &[]);
        assert!(!symbols.contains(&"find".to_string()));
        assert!(!symbols.contains(&"all".to_string()));
        assert!(!symbols.contains(&"functions".to_string()));
    }

    #[test]
    fn test_language_filtering() {
        let symbols = extract_symbols("find foo in rust", &[]);
        assert!(symbols.contains(&"foo".to_string()));
        assert!(!symbols.contains(&"rust".to_string()));
    }

    #[test]
    fn test_is_stopword() {
        assert!(is_stopword("find"));
        assert!(is_stopword("FIND"));
        assert!(!is_stopword("authenticate"));
    }

    #[test]
    fn test_is_language_name() {
        assert!(is_known_language("rust"));
        assert!(is_known_language("Python"));
        assert!(is_known_language("JS"));
        assert!(!is_known_language("foo"));
    }

    #[test]
    fn test_pascal_case_not_filtered() {
        // PascalCase identifiers should not be filtered even if they match stopwords
        let symbols = extract_symbols("find enum with Status", &[]);
        assert!(
            symbols.contains(&"Status".to_string()),
            "PascalCase 'Status' should not be filtered"
        );
    }

    #[test]
    fn test_all_caps_not_filtered() {
        // ALL_CAPS identifiers should not be filtered
        let symbols = extract_symbols("grep for TODO comments", &[]);
        assert!(
            symbols.contains(&"TODO".to_string()),
            "ALL_CAPS 'TODO' should not be filtered"
        );
    }

    #[test]
    fn test_snake_case_not_filtered() {
        // snake_case identifiers should not be filtered
        let symbols = extract_symbols("find user_id variable", &[]);
        assert!(
            symbols.contains(&"user_id".to_string()),
            "snake_case 'user_id' should not be filtered"
        );
    }
}
