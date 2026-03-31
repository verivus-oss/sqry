/// Query classifier - determines whether a query is semantic, text, or hybrid
///
/// Classification rules:
/// - Semantic: Contains relation queries, symbol filters, AST node types
/// - Text: Contains regex metacharacters, literal patterns, comments
/// - Hybrid: Ambiguous queries that should try semantic first, then fallback
use regex::Regex;
use std::sync::OnceLock;

/// Query type classification result
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryType {
    /// Pure semantic (AST-based) search
    Semantic,
    /// Pure text/regex search
    Text,
    /// Try semantic first, fallback to text if no results
    Hybrid,
}

/// Query classifier
pub struct QueryClassifier;

impl QueryClassifier {
    /// Classify a query string into semantic, text, or hybrid
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::search::classifier::{QueryClassifier, QueryType};
    ///
    /// // Semantic patterns
    /// assert_eq!(QueryClassifier::classify("callers(foo)"), QueryType::Semantic);
    /// assert_eq!(QueryClassifier::classify("kind:function AND name:bar"), QueryType::Semantic);
    ///
    /// // Text patterns
    /// assert_eq!(QueryClassifier::classify("TODO: fix this"), QueryType::Text);
    /// assert_eq!(QueryClassifier::classify("^fn \\w+"), QueryType::Text);
    ///
    /// // Hybrid (ambiguous)
    /// assert_eq!(QueryClassifier::classify("find_user"), QueryType::Hybrid);
    /// ```
    #[must_use]
    pub fn classify(query: &str) -> QueryType {
        // Rule 1: Explicit semantic patterns
        if Self::is_semantic_pattern(query) {
            return QueryType::Semantic;
        }

        // Rule 2: Explicit text patterns
        if Self::is_text_pattern(query) {
            return QueryType::Text;
        }

        // Rule 3: Ambiguous - use hybrid mode
        QueryType::Hybrid
    }

    /// Check if query contains semantic indicators
    ///
    /// Optimized with compiled regex for single-pass matching
    fn is_semantic_pattern(query: &str) -> bool {
        // Compile regex once at first use
        static SEMANTIC_RE: OnceLock<Regex> = OnceLock::new();

        let re = SEMANTIC_RE.get_or_init(|| {
            Regex::new(
                r"(?x)
                callers[(:]  |   # callers(foo) or callers:foo
                callees[(:]  |   # callees(bar) or callees:bar
                imports[(:]  |   # imports(baz) or imports:baz
                exports[(:]  |   # exports(qux) or exports:qux
                returns[(:]  |   # returns(type) or returns:type
                impl:        |   # impl:Debug (CD Static Analysis trait implementation)
                duplicates:  |   # duplicates:body (CD Static Analysis duplicate detection)
                unused:      |   # unused:public (CD Static Analysis dead code)
                circular:    |   # circular:calls (CD Static Analysis cycle detection)
                kind:        |   # kind:function
                visibility:  |   # visibility:public
                scope\.      |   # scope.type, scope.name, scope.parent, scope.ancestor (P2-34)
                async:       |   # async:true
                static:      |   # static:false
                @            |   # @function.def
                ::           # foo::bar::baz
            ",
            )
            .expect("Failed to compile semantic pattern regex")
        });

        re.is_match(query)
    }

    /// Check if query contains text search indicators
    fn is_text_pattern(query: &str) -> bool {
        // Text indicators:
        // - Common code markers: TODO, FIXME, HACK, XXX, NOTE, BUG
        // - Regex anchors: ^, $
        // - Regex character classes: \b, \w, \d, \s
        // - Regex quantifiers: +, *, ?, {n,m}
        // - Regex groups: (...)
        // - Comment patterns: //, #, /*

        let markers = ["TODO", "FIXME", "HACK", "XXX", "NOTE", "BUG"];
        if markers.iter().any(|&m| query.contains(m)) {
            return true;
        }

        // Regex metacharacters (strong signal for text search)
        if query.starts_with('^') || query.ends_with('$') {
            return true;
        }

        // Character classes and escape sequences
        if query.contains("\\b")
            || query.contains("\\w")
            || query.contains("\\d")
            || query.contains("\\s")
            || query.contains("\\W")
            || query.contains("\\D")
            || query.contains("\\S")
        {
            return true;
        }

        // Comment patterns
        if query.contains("//") || query.starts_with('#') || query.contains("/*") {
            return true;
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_semantic_relation_queries() {
        assert_eq!(
            QueryClassifier::classify("callers(foo)"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("callees(bar)"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("imports(baz)"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_semantic_symbol_filters() {
        assert_eq!(
            QueryClassifier::classify("kind:function AND name:bar"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("visibility:public"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("async:true AND static:false"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_semantic_scope_queries() {
        assert_eq!(
            QueryClassifier::classify("scope:module::class"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("foo::bar::baz"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_semantic_ast_nodes() {
        assert_eq!(
            QueryClassifier::classify("@function.def"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("@class.name"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_text_markers() {
        assert_eq!(QueryClassifier::classify("TODO: fix this"), QueryType::Text);
        assert_eq!(
            QueryClassifier::classify("FIXME: handle error"),
            QueryType::Text
        );
        assert_eq!(
            QueryClassifier::classify("HACK: temporary"),
            QueryType::Text
        );
        assert_eq!(QueryClassifier::classify("XXX: review"), QueryType::Text);
    }

    #[test]
    fn test_classify_text_regex_anchors() {
        assert_eq!(QueryClassifier::classify("^fn main"), QueryType::Text);
        assert_eq!(QueryClassifier::classify("error$"), QueryType::Text);
    }

    #[test]
    fn test_classify_text_regex_character_classes() {
        assert_eq!(QueryClassifier::classify("fn \\w+"), QueryType::Text);
        assert_eq!(QueryClassifier::classify("\\d{3}"), QueryType::Text);
        assert_eq!(QueryClassifier::classify("\\bfoo\\b"), QueryType::Text);
    }

    #[test]
    fn test_classify_text_comment_patterns() {
        assert_eq!(QueryClassifier::classify("// comment"), QueryType::Text);
        assert_eq!(QueryClassifier::classify("# TODO"), QueryType::Text);
        assert_eq!(QueryClassifier::classify("/* block */"), QueryType::Text);
    }

    #[test]
    fn test_classify_hybrid_ambiguous() {
        // Simple identifiers are ambiguous - could be semantic or text
        assert_eq!(QueryClassifier::classify("find_user"), QueryType::Hybrid);
        assert_eq!(QueryClassifier::classify("calculate"), QueryType::Hybrid);
        assert_eq!(QueryClassifier::classify("UserModel"), QueryType::Hybrid);
    }

    #[test]
    fn test_classify_hybrid_simple_patterns() {
        // Simple patterns without clear indicators
        assert_eq!(QueryClassifier::classify("error"), QueryType::Hybrid);
        assert_eq!(QueryClassifier::classify("config"), QueryType::Hybrid);
    }

    // Edge case tests (Phase 1.2)
    #[test]
    fn test_classify_edge_case_empty_query() {
        // Empty query should be hybrid (ambiguous)
        assert_eq!(QueryClassifier::classify(""), QueryType::Hybrid);
    }

    #[test]
    fn test_classify_edge_case_whitespace_only() {
        // Whitespace-only queries should be hybrid
        assert_eq!(QueryClassifier::classify("   "), QueryType::Hybrid);
        assert_eq!(QueryClassifier::classify("\t"), QueryType::Hybrid);
        assert_eq!(QueryClassifier::classify("\n"), QueryType::Hybrid);
        assert_eq!(QueryClassifier::classify("  \t\n  "), QueryType::Hybrid);
    }

    #[test]
    fn test_classify_edge_case_semantic_with_special_chars() {
        // Semantic patterns with special characters in values
        assert_eq!(
            QueryClassifier::classify("kind:function-name"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("kind:foo_bar"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("kind:foo.bar"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("callers(my::path::Foo)"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_edge_case_colon_without_field() {
        // Colon without semantic field should be hybrid
        assert_eq!(QueryClassifier::classify(":foo"), QueryType::Hybrid);
        assert_eq!(QueryClassifier::classify("::"), QueryType::Semantic); // Double colon is semantic
        assert_eq!(QueryClassifier::classify(":::"), QueryType::Semantic); // Triple colon contains ::
    }

    #[test]
    fn test_classify_edge_case_mixed_semantic_and_text() {
        // If query contains both semantic AND text indicators, semantic wins
        assert_eq!(
            QueryClassifier::classify("kind:function AND TODO"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("@function TODO: fix"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("callers(foo) // comment"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_edge_case_parentheses_without_semantic() {
        // Parentheses alone are not semantic
        assert_eq!(QueryClassifier::classify("(foo)"), QueryType::Hybrid);
        assert_eq!(QueryClassifier::classify("foo(bar)"), QueryType::Hybrid);
    }

    #[test]
    fn test_classify_edge_case_at_symbol_positions() {
        // @ anywhere in query is semantic
        assert_eq!(QueryClassifier::classify("@"), QueryType::Semantic);
        assert_eq!(QueryClassifier::classify("@function"), QueryType::Semantic);
        assert_eq!(QueryClassifier::classify("function@"), QueryType::Semantic);
        assert_eq!(QueryClassifier::classify("foo@bar"), QueryType::Semantic);
    }

    #[test]
    fn test_classify_edge_case_double_colon_positions() {
        // :: anywhere is semantic (symbol paths)
        assert_eq!(QueryClassifier::classify("::"), QueryType::Semantic);
        assert_eq!(QueryClassifier::classify("foo::bar"), QueryType::Semantic);
        assert_eq!(QueryClassifier::classify("::bar"), QueryType::Semantic);
        assert_eq!(QueryClassifier::classify("foo::"), QueryType::Semantic);
    }

    #[test]
    fn test_classify_edge_case_semantic_fields_case_sensitive() {
        // Field names are case-sensitive in the regex
        assert_eq!(
            QueryClassifier::classify("kind:function"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("KIND:function"),
            QueryType::Hybrid
        ); // uppercase not matched
        assert_eq!(
            QueryClassifier::classify("Kind:function"),
            QueryType::Hybrid
        ); // titlecase not matched
    }

    #[test]
    fn test_classify_edge_case_relation_with_colon() {
        // Relations can use both ( and :
        assert_eq!(
            QueryClassifier::classify("callers:foo"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("callees:bar"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("imports:baz"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("exports:qux"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("returns:Type"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_edge_case_relation_with_parentheses() {
        // Relations can also use parentheses - test both branches of [(:]
        assert_eq!(
            QueryClassifier::classify("exports(MyType)"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("returns(Result<T>)"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("imports(std::collections)"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_edge_case_boolean_fields() {
        // Boolean fields (async, static)
        assert_eq!(QueryClassifier::classify("async:true"), QueryType::Semantic);
        assert_eq!(
            QueryClassifier::classify("async:false"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("static:true"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("static:false"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_edge_case_complex_queries() {
        // Complex multi-clause queries
        assert_eq!(
            QueryClassifier::classify("kind:function AND async:true"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("(callers(foo) OR callees(bar)) AND kind:struct"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("visibility:public AND scope:module::MyClass"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_edge_case_text_false_positives() {
        // Ensure these are NOT classified as semantic
        assert_eq!(QueryClassifier::classify("caller"), QueryType::Hybrid); // missing (s
        assert_eq!(QueryClassifier::classify("foo:bar"), QueryType::Hybrid); // not a known field
        assert_eq!(QueryClassifier::classify("name:foo"), QueryType::Hybrid); // 'name' not in semantic regex
        assert_eq!(QueryClassifier::classify("type:Bar"), QueryType::Hybrid); // 'type' not in semantic regex
    }

    #[test]
    fn test_classify_edge_case_unicode() {
        // Unicode in queries (should work with regex)
        assert_eq!(QueryClassifier::classify("kind:函数"), QueryType::Semantic);
        assert_eq!(
            QueryClassifier::classify("foo::バー::baz"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("callers(αβγ)"),
            QueryType::Semantic
        );
    }

    // CD Static Analysis predicate tests
    #[test]
    fn test_classify_cd_duplicates_predicate() {
        assert_eq!(
            QueryClassifier::classify("duplicates:body"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("duplicates:signature"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("duplicates:struct"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("kind:function AND duplicates:body"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_cd_unused_predicate() {
        assert_eq!(
            QueryClassifier::classify("unused:public"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("unused:private"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("unused:function"),
            QueryType::Semantic
        );
        assert_eq!(QueryClassifier::classify("unused:all"), QueryType::Semantic);
    }

    #[test]
    fn test_classify_cd_circular_predicate() {
        assert_eq!(
            QueryClassifier::classify("circular:calls"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("circular:imports"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("circular:all"),
            QueryType::Semantic
        );
    }

    #[test]
    fn test_classify_cd_combined_predicates() {
        // Multiple CD predicates together
        assert_eq!(
            QueryClassifier::classify("kind:function AND duplicates:body AND unused:public"),
            QueryType::Semantic
        );
        assert_eq!(
            QueryClassifier::classify("impl:Debug AND circular:calls"),
            QueryType::Semantic
        );
    }
}
