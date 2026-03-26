//! Filter extraction (kind, limit, depth, format, predicates).

use crate::types::{OutputFormat, PredicateType, SymbolKind, Visibility};
use regex::Regex;
use std::sync::LazyLock;

/// Patterns for limit extraction
static LIMIT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)\b(?:first|top|limit)\s+(\d+)\b").expect("Invalid limit regex"),
        Regex::new(r"(?i)\b(\d+)\s+(?:results?|matches?|hits?)\b").expect("Invalid results regex"),
    ]
});

/// Patterns for depth extraction
static DEPTH_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        Regex::new(r"(?i)\b(?:depth|level|levels)\s+(\d+)\b").expect("Invalid depth regex"),
        Regex::new(r"(?i)\b(\d+)\s+(?:levels?|hops?)\s+(?:deep|max)\b")
            .expect("Invalid levels regex"),
        Regex::new(r"(?i)\bmax[- ]?depth\s+(\d+)\b").expect("Invalid max-depth regex"),
    ]
});

/// Kind keyword mappings
const KIND_MAP: &[(&[&str], SymbolKind)] = &[
    (
        &["function", "func", "fn", "def", "subroutine"],
        SymbolKind::Function,
    ),
    (&["method", "member"], SymbolKind::Method),
    (&["class", "type"], SymbolKind::Class),
    (&["struct", "structure", "record"], SymbolKind::Struct),
    (&["enum", "enumeration"], SymbolKind::Enum),
    (&["trait"], SymbolKind::Trait),
    (&["interface", "protocol"], SymbolKind::Interface),
    (
        &["module", "mod", "package", "namespace"],
        SymbolKind::Module,
    ),
    (&["constant", "const", "static"], SymbolKind::Constant),
    (&["variable", "var", "let"], SymbolKind::Variable),
    (&["typealias", "type_alias", "alias"], SymbolKind::TypeAlias),
];

/// Extract limit from patterns like "first 5", "top 10", "limit 20".
#[must_use]
pub fn extract_limit(input: &str) -> Option<u32> {
    for pattern in LIMIT_PATTERNS.iter() {
        if let Some(caps) = pattern.captures(input)
            && let Some(num) = caps.get(1)
        {
            return num.as_str().parse().ok();
        }
    }
    None
}

/// Extract depth from patterns like "depth 5", "5 levels deep".
#[must_use]
pub fn extract_depth(input: &str) -> Option<u32> {
    for pattern in DEPTH_PATTERNS.iter() {
        if let Some(caps) = pattern.captures(input)
            && let Some(num) = caps.get(1)
        {
            return num.as_str().parse().ok();
        }
    }
    None
}

/// Extract symbol kind from patterns.
#[must_use]
pub fn extract_kind(input: &str) -> Option<SymbolKind> {
    let input_lower = input.to_lowercase();

    for (keywords, kind) in KIND_MAP {
        for keyword in *keywords {
            // Match as whole word, handling plurals (es, s, or nothing)
            // Pattern matches: function, functions, class, classes, etc.
            let plural_pattern = format!(r"\b{}(?:es|s)?\b", regex::escape(keyword));
            if let Ok(re) = Regex::new(&plural_pattern)
                && re.is_match(&input_lower)
            {
                return Some(*kind);
            }
        }
    }
    None
}

/// Extract output format from input.
#[must_use]
pub fn extract_format(input: &str) -> Option<OutputFormat> {
    let input_lower = input.to_lowercase();

    if input_lower.contains("mermaid") {
        Some(OutputFormat::Mermaid)
    } else if input_lower.contains("dot") || input_lower.contains("graphviz") {
        Some(OutputFormat::Dot)
    } else if input_lower.contains("json") {
        Some(OutputFormat::Json)
    } else {
        None
    }
}

// --- CD Predicate extraction ---

/// Patterns for impl: predicate extraction
static IMPL_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Direct predicate syntax: impl:Future, impl:Iterator
        Regex::new(r"(?i)\bimpl:(\w+)").expect("Invalid impl predicate regex"),
        // Natural language: "implements Future", "implementing Iterator"
        Regex::new(r"(?i)\bimplements?\s+(\w+)").expect("Invalid implements regex"),
        Regex::new(r"(?i)\bimplementing\s+(\w+)").expect("Invalid implementing regex"),
        // "implementations of X", "find types implementing X"
        Regex::new(r"(?i)\bimplementations?\s+of\s+(\w+)").expect("Invalid impl of regex"),
        Regex::new(r"(?i)\btypes?\s+implementing\s+(\w+)")
            .expect("Invalid types implementing regex"),
        // "structs implementing X", "classes implementing X"
        Regex::new(r"(?i)\b(?:structs?|classes?|types?)\s+(?:that\s+)?impl(?:ement)?\s+(\w+)")
            .expect("Invalid structs implementing regex"),
    ]
});

/// Patterns for duplicates: predicate
static DUPLICATES_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Direct predicate syntax: duplicates:, duplicates:body
        Regex::new(r"(?i)\bduplicates?:(\w*)").expect("Invalid duplicates predicate regex"),
        // Natural language
        Regex::new(r"(?i)\b(?:find\s+)?duplicate(?:d|s)?\s+(?:code|functions?|methods?|bodies?)?")
            .expect("Invalid duplicate NL regex"),
        Regex::new(r"(?i)\b(?:find\s+)?code\s+duplication")
            .expect("Invalid code duplication regex"),
        Regex::new(r"(?i)\b(?:find\s+)?similar\s+(?:code|functions?)")
            .expect("Invalid similar code regex"),
    ]
});

/// Patterns for circular: predicate
static CIRCULAR_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Direct predicate syntax: circular:, circular:calls
        Regex::new(r"(?i)\bcircular:(\w*)").expect("Invalid circular predicate regex"),
        // Natural language
        Regex::new(r"(?i)\bcircular\s+(?:dependencies?|calls?|imports?|references?)")
            .expect("Invalid circular NL regex"),
        Regex::new(r"(?i)\b(?:find\s+)?dependency\s+cycles?")
            .expect("Invalid dependency cycle regex"),
        Regex::new(r"(?i)\b(?:find\s+)?cyclic\s+(?:dependencies?|imports?|calls?)")
            .expect("Invalid cyclic regex"),
    ]
});

/// Patterns for unused: predicate
static UNUSED_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Direct predicate syntax: unused:, unused:true
        Regex::new(r"(?i)\bunused:(\w*)").expect("Invalid unused predicate regex"),
        // Natural language
        Regex::new(r"(?i)\b(?:find\s+)?unused\s+(?:code|functions?|methods?|variables?|symbols?)?")
            .expect("Invalid unused NL regex"),
        Regex::new(r"(?i)\b(?:find\s+)?dead\s+code").expect("Invalid dead code regex"),
        Regex::new(r"(?i)\b(?:find\s+)?unreachable\s+(?:code|functions?)")
            .expect("Invalid unreachable regex"),
    ]
});

/// Patterns for async: predicate
static ASYNC_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Direct predicate syntax: async:true
        Regex::new(r"(?i)\basync:(\w+)").expect("Invalid async predicate regex"),
        // Natural language
        Regex::new(r"(?i)\basync(?:hronous)?\s+(?:functions?|methods?|code)?")
            .expect("Invalid async NL regex"),
        Regex::new(r"(?i)\b(?:find\s+)?(?:all\s+)?async\b").expect("Invalid find async regex"),
    ]
});

/// Patterns for unsafe: predicate
static UNSAFE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Direct predicate syntax: unsafe:true
        Regex::new(r"(?i)\bunsafe:(\w+)").expect("Invalid unsafe predicate regex"),
        // Natural language
        Regex::new(r"(?i)\bunsafe\s+(?:code|blocks?|functions?)?")
            .expect("Invalid unsafe NL regex"),
        Regex::new(r"(?i)\b(?:find\s+)?(?:all\s+)?unsafe\b").expect("Invalid find unsafe regex"),
    ]
});

/// Extract predicate type from input.
///
/// Returns the predicate type (impl, duplicates, circular, unused) if detected.
#[must_use]
pub fn extract_predicate_type(input: &str) -> Option<PredicateType> {
    let input_lower = input.to_lowercase();

    // Check for impl: patterns
    for pattern in IMPL_PATTERNS.iter() {
        if pattern.is_match(&input_lower) {
            return Some(PredicateType::Impl);
        }
    }

    // Check for duplicates: patterns
    for pattern in DUPLICATES_PATTERNS.iter() {
        if pattern.is_match(&input_lower) {
            return Some(PredicateType::Duplicates);
        }
    }

    // Check for circular: patterns
    for pattern in CIRCULAR_PATTERNS.iter() {
        if pattern.is_match(&input_lower) {
            return Some(PredicateType::Circular);
        }
    }

    // Check for unused: patterns
    for pattern in UNUSED_PATTERNS.iter() {
        if pattern.is_match(&input_lower) {
            return Some(PredicateType::Unused);
        }
    }

    None
}

/// Extract trait name from impl: predicate.
///
/// Returns the trait name (e.g., "Future" from "impl:Future" or "implements Future").
#[must_use]
pub fn extract_impl_trait(input: &str) -> Option<String> {
    for pattern in IMPL_PATTERNS.iter() {
        if let Some(caps) = pattern.captures(input)
            && let Some(trait_name) = caps.get(1)
        {
            let name = trait_name.as_str().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

/// Extract predicate argument (e.g., "body" from "duplicates:body").
#[must_use]
pub fn extract_predicate_arg(input: &str) -> Option<String> {
    // Check duplicates: argument
    if let Some(caps) = Regex::new(r"(?i)\bduplicates?:(\w+)").ok()?.captures(input)
        && let Some(arg) = caps.get(1)
    {
        let arg_str = arg.as_str();
        if !arg_str.is_empty() {
            return Some(arg_str.to_string());
        }
    }

    // Check circular: argument
    if let Some(caps) = Regex::new(r"(?i)\bcircular:(\w+)").ok()?.captures(input)
        && let Some(arg) = caps.get(1)
    {
        let arg_str = arg.as_str();
        if !arg_str.is_empty() {
            return Some(arg_str.to_string());
        }
    }

    None
}

/// Extract visibility filter from input.
#[must_use]
pub fn extract_visibility(input: &str) -> Option<Visibility> {
    let input_lower = input.to_lowercase();

    // Check direct predicate syntax first
    if let Some(caps) = Regex::new(r"(?i)\bvisibility:(\w+)")
        .ok()?
        .captures(&input_lower)
        && let Some(val) = caps.get(1)
    {
        return match val.as_str() {
            "public" | "pub" => Some(Visibility::Public),
            "private" | "priv" => Some(Visibility::Private),
            _ => None,
        };
    }

    // Check natural language patterns
    if input_lower.contains("public") {
        return Some(Visibility::Public);
    }
    if input_lower.contains("private") {
        return Some(Visibility::Private);
    }

    None
}

/// Extract async filter from input.
///
/// Returns `Some(true)` if async functions should be found.
#[must_use]
pub fn extract_async(input: &str) -> Option<bool> {
    let input_lower = input.to_lowercase();

    // Check direct predicate syntax
    if let Some(caps) = Regex::new(r"(?i)\basync:(\w+)")
        .ok()?
        .captures(&input_lower)
        && let Some(val) = caps.get(1)
    {
        return match val.as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        };
    }

    // Check natural language patterns
    for pattern in ASYNC_PATTERNS.iter() {
        if pattern.is_match(&input_lower) {
            return Some(true);
        }
    }

    None
}

/// Extract unsafe filter from input.
///
/// Returns `Some(true)` if unsafe code should be found.
#[must_use]
pub fn extract_unsafe(input: &str) -> Option<bool> {
    let input_lower = input.to_lowercase();

    // Check direct predicate syntax
    if let Some(caps) = Regex::new(r"(?i)\bunsafe:(\w+)")
        .ok()?
        .captures(&input_lower)
        && let Some(val) = caps.get(1)
    {
        return match val.as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        };
    }

    // Check natural language patterns
    for pattern in UNSAFE_PATTERNS.iter() {
        if pattern.is_match(&input_lower) {
            return Some(true);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_limit_first() {
        assert_eq!(extract_limit("first 5 results"), Some(5));
    }

    #[test]
    fn test_extract_limit_top() {
        assert_eq!(extract_limit("top 10 matches"), Some(10));
    }

    #[test]
    fn test_extract_limit_explicit() {
        assert_eq!(extract_limit("limit 20"), Some(20));
    }

    #[test]
    fn test_extract_limit_results() {
        assert_eq!(extract_limit("show 15 results"), Some(15));
    }

    #[test]
    fn test_extract_limit_none() {
        assert_eq!(extract_limit("find all functions"), None);
    }

    #[test]
    fn test_extract_depth() {
        assert_eq!(extract_depth("depth 5"), Some(5));
        assert_eq!(extract_depth("3 levels deep"), Some(3));
        assert_eq!(extract_depth("max-depth 10"), Some(10));
    }

    #[test]
    fn test_extract_kind_function() {
        assert_eq!(
            extract_kind("find all functions"),
            Some(SymbolKind::Function)
        );
        assert_eq!(
            extract_kind("find function foo"),
            Some(SymbolKind::Function)
        );
    }

    #[test]
    fn test_extract_kind_class() {
        assert_eq!(extract_kind("find all classes"), Some(SymbolKind::Class));
    }

    #[test]
    fn test_extract_kind_struct() {
        assert_eq!(extract_kind("find structs"), Some(SymbolKind::Struct));
    }

    #[test]
    fn test_extract_kind_method() {
        assert_eq!(extract_kind("find methods"), Some(SymbolKind::Method));
    }

    #[test]
    fn test_extract_format() {
        assert_eq!(
            extract_format("generate mermaid diagram"),
            Some(OutputFormat::Mermaid)
        );
        assert_eq!(extract_format("output as dot"), Some(OutputFormat::Dot));
        assert_eq!(extract_format("json format"), Some(OutputFormat::Json));
        assert_eq!(extract_format("find foo"), None);
    }

    // --- CD Predicate tests ---

    #[test]
    fn test_extract_predicate_type_impl() {
        assert_eq!(
            extract_predicate_type("impl:Future"),
            Some(PredicateType::Impl)
        );
        assert_eq!(
            extract_predicate_type("find implementations of Iterator"),
            Some(PredicateType::Impl)
        );
        assert_eq!(
            extract_predicate_type("types implementing Clone"),
            Some(PredicateType::Impl)
        );
    }

    #[test]
    fn test_extract_predicate_type_duplicates() {
        assert_eq!(
            extract_predicate_type("duplicates:"),
            Some(PredicateType::Duplicates)
        );
        assert_eq!(
            extract_predicate_type("duplicates:body"),
            Some(PredicateType::Duplicates)
        );
        assert_eq!(
            extract_predicate_type("find duplicate code"),
            Some(PredicateType::Duplicates)
        );
    }

    #[test]
    fn test_extract_predicate_type_circular() {
        assert_eq!(
            extract_predicate_type("circular:"),
            Some(PredicateType::Circular)
        );
        assert_eq!(
            extract_predicate_type("find circular dependencies"),
            Some(PredicateType::Circular)
        );
        assert_eq!(
            extract_predicate_type("cyclic imports"),
            Some(PredicateType::Circular)
        );
    }

    #[test]
    fn test_extract_predicate_type_unused() {
        assert_eq!(
            extract_predicate_type("unused:"),
            Some(PredicateType::Unused)
        );
        assert_eq!(
            extract_predicate_type("find unused code"),
            Some(PredicateType::Unused)
        );
        assert_eq!(
            extract_predicate_type("dead code"),
            Some(PredicateType::Unused)
        );
    }

    #[test]
    fn test_extract_impl_trait() {
        assert_eq!(
            extract_impl_trait("impl:Future"),
            Some("Future".to_string())
        );
        assert_eq!(
            extract_impl_trait("implements Iterator"),
            Some("Iterator".to_string())
        );
        assert_eq!(
            extract_impl_trait("implementations of Clone"),
            Some("Clone".to_string())
        );
        assert_eq!(extract_impl_trait("find functions"), None);
    }

    #[test]
    fn test_extract_predicate_arg() {
        assert_eq!(
            extract_predicate_arg("duplicates:body"),
            Some("body".to_string())
        );
        assert_eq!(
            extract_predicate_arg("circular:calls"),
            Some("calls".to_string())
        );
        assert_eq!(extract_predicate_arg("duplicates:"), None);
        assert_eq!(extract_predicate_arg("find foo"), None);
    }

    #[test]
    fn test_extract_visibility() {
        assert_eq!(
            extract_visibility("visibility:public"),
            Some(Visibility::Public)
        );
        assert_eq!(
            extract_visibility("visibility:private"),
            Some(Visibility::Private)
        );
        assert_eq!(
            extract_visibility("find public functions"),
            Some(Visibility::Public)
        );
        assert_eq!(
            extract_visibility("private methods"),
            Some(Visibility::Private)
        );
        assert_eq!(extract_visibility("find foo"), None);
    }

    #[test]
    fn test_extract_async() {
        assert_eq!(extract_async("async:true"), Some(true));
        assert_eq!(extract_async("async:false"), Some(false));
        assert_eq!(extract_async("find async functions"), Some(true));
        assert_eq!(extract_async("async methods"), Some(true));
        assert_eq!(extract_async("find foo"), None);
    }

    #[test]
    fn test_extract_unsafe() {
        assert_eq!(extract_unsafe("unsafe:true"), Some(true));
        assert_eq!(extract_unsafe("unsafe:false"), Some(false));
        assert_eq!(extract_unsafe("find unsafe code"), Some(true));
        assert_eq!(extract_unsafe("unsafe blocks"), Some(true));
        assert_eq!(extract_unsafe("find foo"), None);
    }
}
