//! Language extraction and normalization.

use regex::Regex;
use std::sync::LazyLock;

/// Language keyword normalization map
const LANGUAGE_MAP: &[(&str, &str)] = &[
    ("rust", "rust"),
    ("rs", "rust"),
    ("python", "python"),
    ("py", "python"),
    ("javascript", "javascript"),
    ("js", "javascript"),
    ("typescript", "typescript"),
    ("ts", "typescript"),
    ("go", "go"),
    ("golang", "go"),
    ("java", "java"),
    ("cpp", "cpp"),
    ("c++", "cpp"),
    ("cxx", "cpp"),
    ("c", "c"),
    ("ruby", "ruby"),
    ("rb", "ruby"),
    ("php", "php"),
    ("sql", "sql"),
    ("terraform", "terraform"),
    ("tf", "terraform"),
    ("kotlin", "kotlin"),
    ("kt", "kotlin"),
    ("swift", "swift"),
    ("scala", "scala"),
    ("elixir", "elixir"),
    ("ex", "elixir"),
    ("haskell", "haskell"),
    ("hs", "haskell"),
    ("perl", "perl"),
    ("pl", "perl"),
    ("lua", "lua"),
    ("r", "r"),
    ("dart", "dart"),
    ("zig", "zig"),
    ("groovy", "groovy"),
    ("shell", "shell"),
    ("bash", "shell"),
    ("sh", "shell"),
    ("html", "html"),
    ("css", "css"),
    ("vue", "vue"),
    ("svelte", "svelte"),
];

/// Precompiled language patterns
static LANGUAGE_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    LANGUAGE_MAP
        .iter()
        .map(|(keyword, normalized)| {
            let pattern = format!(r"(?i)\b{}\b", regex::escape(keyword));
            (
                Regex::new(&pattern).expect("Invalid language regex"),
                *normalized,
            )
        })
        .collect()
});

/// Extract and normalize languages from input.
///
/// Supports multi-language queries like "find X in ts and js".
#[must_use]
pub fn extract_languages(input: &str) -> Vec<String> {
    let mut languages = Vec::new();

    for (pattern, normalized) in LANGUAGE_PATTERNS.iter() {
        if pattern.is_match(input) && !languages.contains(&(*normalized).to_string()) {
            languages.push((*normalized).to_string());
        }
    }

    languages
}

/// Normalize a single language name.
#[must_use]
pub fn normalize_language(lang: &str) -> Option<&'static str> {
    let lower = lang.to_lowercase();
    LANGUAGE_MAP
        .iter()
        .find(|(keyword, _)| *keyword == lower)
        .map(|(_, normalized)| *normalized)
}

/// Check if a string is a known language name.
#[must_use]
pub fn is_known_language(lang: &str) -> bool {
    normalize_language(lang).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_single_language() {
        let langs = extract_languages("find foo in rust");
        assert_eq!(langs, vec!["rust"]);
    }

    #[test]
    fn test_extract_multiple_languages() {
        let langs = extract_languages("find foo in typescript and javascript");
        assert!(langs.contains(&"typescript".to_string()));
        assert!(langs.contains(&"javascript".to_string()));
    }

    #[test]
    fn test_extract_alias() {
        let langs = extract_languages("find foo in py");
        assert_eq!(langs, vec!["python"]);
    }

    #[test]
    fn test_extract_case_insensitive() {
        let langs = extract_languages("find foo in RUST");
        assert_eq!(langs, vec!["rust"]);
    }

    #[test]
    fn test_no_duplicates() {
        let langs = extract_languages("find foo in rust and rs");
        assert_eq!(langs, vec!["rust"]);
    }

    #[test]
    fn test_normalize_language() {
        assert_eq!(normalize_language("py"), Some("python"));
        assert_eq!(normalize_language("golang"), Some("go"));
        assert_eq!(normalize_language("unknown"), None);
    }

    #[test]
    fn test_is_known_language() {
        assert!(is_known_language("rust"));
        assert!(is_known_language("JS"));
        assert!(!is_known_language("foobar"));
    }
}
