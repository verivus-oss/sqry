//! Entity extraction from natural language input.
//!
//! Extracts:
//! - Symbol names (quoted and unquoted identifiers)
//! - Programming languages
//! - Path patterns
//! - Symbol kinds
//! - Limits and depths
//! - Trace-path from/to pairs

mod filters;
mod languages;
mod patterns;
mod symbols;

use crate::types::ExtractedEntities;

/// Convenience function to extract entities from preprocessed input.
///
/// Uses default extractor settings.
#[must_use]
pub fn extract_entities(input: &str) -> ExtractedEntities {
    let extractor = EntityExtractor::new();
    // Parse quoted spans from the input (look for "..." patterns)
    let quoted_spans: Vec<String> = extract_quoted_from_input(input);
    extractor.extract(input, &quoted_spans)
}

/// Extract quoted spans from input for entity extraction.
fn extract_quoted_from_input(input: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let chars = input.chars().peekable();
    let mut in_quote = false;
    let mut current_span = String::new();

    for c in chars {
        if !in_quote && c == '"' {
            in_quote = true;
            current_span.clear();
        } else if in_quote && c == '"' {
            in_quote = false;
            if !current_span.is_empty() {
                spans.push(current_span.clone());
            }
        } else if in_quote {
            current_span.push(c);
        }
    }

    spans
}

/// Entity extractor for natural language queries.
pub struct EntityExtractor {
    /// Default limit when not specified
    default_limit: u32,
    /// Maximum allowed depth
    max_depth: u32,
}

impl Default for EntityExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl EntityExtractor {
    /// Create a new extractor with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            default_limit: 100,
            max_depth: 20,
        }
    }

    /// Create an extractor with custom settings.
    #[must_use]
    pub fn with_defaults(default_limit: u32, max_depth: u32) -> Self {
        Self {
            default_limit,
            max_depth,
        }
    }

    /// Extract entities from preprocessed input.
    ///
    /// # Arguments
    /// * `input` - The preprocessed natural language input
    /// * `quoted_spans` - Quoted strings extracted during preprocessing
    #[must_use]
    pub fn extract(&self, input: &str, quoted_spans: &[String]) -> ExtractedEntities {
        let mut entities = ExtractedEntities::new();

        // 1. Extract symbols (quoted takes priority)
        entities.symbols = symbols::extract_symbols(input, quoted_spans);

        // 2. Extract languages
        entities.languages = languages::extract_languages(input);

        // 3. Extract paths
        entities.paths = patterns::extract_paths(input);

        // 4. Extract kind filter
        entities.kind = filters::extract_kind(input);

        // 5. Extract limit
        entities.limit = filters::extract_limit(input);

        // 6. Extract depth (capped at max_depth)
        entities.depth = filters::extract_depth(input).map(|d| d.min(self.max_depth));

        // 7. Extract format
        entities.format = filters::extract_format(input);

        // 8. Extract trace-path from/to
        let (from, to) = patterns::extract_trace_path(input, quoted_spans);
        entities.from_symbol = from;
        entities.to_symbol = to;

        // 9. Extract relation type
        entities.relation = patterns::extract_relation(input);

        // 10. Extract CD predicate type (impl, duplicates, circular, unused)
        entities.predicate_type = filters::extract_predicate_type(input);

        // 11. Extract impl: trait name
        entities.impl_trait = filters::extract_impl_trait(input);

        // 12. Extract predicate argument (e.g., "body" from "duplicates:body")
        entities.predicate_arg = filters::extract_predicate_arg(input);

        // 13. Extract visibility filter
        entities.visibility = filters::extract_visibility(input);

        // 14. Extract async filter
        entities.is_async = filters::extract_async(input);

        // 15. Extract unsafe filter
        entities.is_unsafe = filters::extract_unsafe(input);

        entities
    }

    /// Get the default limit.
    #[must_use]
    pub const fn default_limit(&self) -> u32 {
        self.default_limit
    }

    /// Get the maximum depth.
    #[must_use]
    pub const fn max_depth(&self) -> u32 {
        self.max_depth
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SymbolKind;

    #[test]
    fn test_extract_basic() {
        let extractor = EntityExtractor::new();
        let entities = extractor.extract("find authentication", &[]);
        assert!(entities.symbols.contains(&"authentication".to_string()));
    }

    #[test]
    fn test_extract_with_quoted() {
        let extractor = EntityExtractor::new();
        let entities =
            extractor.extract("find \"UserAuth::login\"", &["UserAuth::login".to_string()]);
        assert!(entities.symbols.contains(&"UserAuth::login".to_string()));
    }

    #[test]
    fn test_extract_with_language() {
        let extractor = EntityExtractor::new();
        let entities = extractor.extract("find foo in rust", &[]);
        assert!(entities.languages.contains(&"rust".to_string()));
    }

    #[test]
    fn test_extract_with_kind() {
        let extractor = EntityExtractor::new();
        let entities = extractor.extract("find all functions named foo", &[]);
        assert_eq!(entities.kind, Some(SymbolKind::Function));
    }

    #[test]
    fn test_extract_with_limit() {
        let extractor = EntityExtractor::new();
        let entities = extractor.extract("find first 5 functions", &[]);
        assert_eq!(entities.limit, Some(5));
    }

    #[test]
    fn test_verb_for_patterns() {
        let extractor = EntityExtractor::new();

        // "grep for X" pattern
        let entities = extractor.extract("grep for TODO comments", &[]);
        assert!(entities.symbols.contains(&"TODO".to_string()));

        // "look for X" pattern
        let entities = extractor.extract("look for FIXME markers", &[]);
        assert!(entities.symbols.contains(&"FIXME".to_string()));
    }

    #[test]
    fn test_of_patterns() {
        let extractor = EntityExtractor::new();

        // "imports of X" pattern
        let entities = extractor.extract("find all imports of serde", &[]);
        assert!(entities.symbols.contains(&"serde".to_string()));

        // "usages of X" pattern
        let entities = extractor.extract("find usages of parse_json", &[]);
        assert!(entities.symbols.contains(&"parse_json".to_string()));
    }

    #[test]
    fn test_verb_symbol_patterns() {
        let extractor = EntityExtractor::new();

        // "uses X" pattern
        let entities = extractor.extract("who uses the validate method", &[]);
        assert!(entities.symbols.contains(&"validate".to_string()));

        // "invokes X" pattern
        let entities = extractor.extract("what invokes send_email", &[]);
        assert!(entities.symbols.contains(&"send_email".to_string()));

        // "does X call" pattern
        let entities = extractor.extract("what does main call", &[]);
        assert!(entities.symbols.contains(&"main".to_string()));
    }

    #[test]
    fn test_grep_direct_pattern() {
        let extractor = EntityExtractor::new();

        // "grep X" without "for"
        // NOTE: "unsafe" is now a stopword but is extracted as a predicate (is_unsafe = true)
        let entities = extractor.extract("grep unsafe blocks", &[]);
        // "unsafe" is in stopwords list, so it's not extracted as a symbol
        // Instead, the is_unsafe predicate should be set
        assert_eq!(
            entities.is_unsafe,
            Some(true),
            "is_unsafe should be true for 'grep unsafe blocks'"
        );
    }

    #[test]
    fn test_kind_only_extraction() {
        let extractor = EntityExtractor::new();

        // Kind filter with no symbol (e.g., "list all traits")
        let entities = extractor.extract("list all traits", &[]);
        assert_eq!(entities.kind, Some(SymbolKind::Trait));
    }
}

// Predicate extraction regression tests
#[cfg(test)]
mod predicate_tests {
    use super::*;
    use crate::types::SymbolKind;

    #[test]
    fn test_async_functions_extraction() {
        let entities = extract_entities("find async functions");
        assert_eq!(entities.is_async, Some(true));
        assert_eq!(entities.kind, Some(SymbolKind::Function));
        assert!(entities.has_predicate());
    }

    #[test]
    fn test_unsafe_functions_extraction() {
        let entities = extract_entities("find unsafe functions");
        assert_eq!(entities.is_unsafe, Some(true));
        assert_eq!(entities.kind, Some(SymbolKind::Function));
        assert!(entities.has_predicate());
    }

    #[test]
    fn test_public_async_functions_extraction() {
        let entities = extract_entities("find public async functions");
        assert_eq!(entities.visibility, Some(crate::types::Visibility::Public));
        assert_eq!(entities.is_async, Some(true));
        assert_eq!(entities.kind, Some(SymbolKind::Function));
        assert!(entities.has_predicate());
    }
}
