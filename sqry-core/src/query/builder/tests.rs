//! Integration tests for `QueryBuilder`
//!
//! These tests verify end-to-end query construction including:
//! - Field methods produce correct AST
//! - Boolean combinators work correctly
//! - Validation catches errors appropriately
//! - Regex patterns with flags work as expected

use std::sync::Arc;

use super::{BuildError, QueryBuilder, RegexBuilder};
use crate::query::registry::FieldRegistry;
use crate::query::types::{Expr, Operator, Query as QueryAST, Value};

// ============================================================================
// Basic Field Method Tests
// ============================================================================

#[test]
fn test_kind_field() {
    let query = QueryBuilder::kind("function").build().unwrap();
    assert_condition(&query, "kind", &Operator::Equal, "function");
}

#[test]
fn test_name_field() {
    let query = QueryBuilder::name("main").build().unwrap();
    assert_condition(&query, "name", &Operator::Equal, "main");
}

#[test]
fn test_lang_field() {
    let query = QueryBuilder::lang("rust").build().unwrap();
    assert_condition(&query, "lang", &Operator::Equal, "rust");
}

#[test]
fn test_language_alias() {
    let query = QueryBuilder::language("python").build().unwrap();
    // language() is an alias for lang(), so it should produce the same field
    assert_condition(&query, "lang", &Operator::Equal, "python");
}

#[test]
fn test_path_field() {
    let query = QueryBuilder::path("src/main.rs").build().unwrap();
    assert_condition(&query, "path", &Operator::Equal, "src/main.rs");
}

#[test]
fn test_file_alias() {
    let query = QueryBuilder::file("src/lib.rs").build().unwrap();
    // file() is an alias for path(), so it should produce "path" field
    assert_condition(&query, "path", &Operator::Equal, "src/lib.rs");
}

#[test]
fn test_scope_field() {
    let query = QueryBuilder::scope("module").build().unwrap();
    assert_condition(&query, "scope", &Operator::Equal, "module");
}

#[test]
fn test_scope_type_field() {
    let query = QueryBuilder::scope_type("function").build().unwrap();
    assert_condition(&query, "scope.type", &Operator::Equal, "function");
}

#[test]
fn test_parent_field() {
    let query = QueryBuilder::parent("MyClass").build().unwrap();
    assert_condition(&query, "parent", &Operator::Equal, "MyClass");
}

// ============================================================================
// Regex Method Tests
// ============================================================================

#[test]
fn test_name_matches_default_flags() {
    let query = QueryBuilder::name_matches("test.*").build().unwrap();
    assert_regex_condition(&query, "name", "test.*", false, false, false);
}

#[test]
fn test_name_matches_case_insensitive() {
    let query = QueryBuilder::name_matches_with("Test.*", RegexBuilder::case_insensitive)
        .build()
        .unwrap();
    assert_regex_condition(&query, "name", "Test.*", true, false, false);
}

#[test]
fn test_name_matches_multiline() {
    let query = QueryBuilder::name_matches_with("^test$", RegexBuilder::multiline)
        .build()
        .unwrap();
    assert_regex_condition(&query, "name", "^test$", false, true, false);
}

#[test]
fn test_name_matches_all_flags() {
    let query = QueryBuilder::name_matches_with("pattern", |rb| {
        rb.case_insensitive().multiline().dot_all()
    })
    .build()
    .unwrap();
    assert_regex_condition(&query, "name", "pattern", true, true, true);
}

#[test]
fn test_path_matches() {
    let query = QueryBuilder::path_matches(".*test.*").build().unwrap();
    assert_regex_condition(&query, "path", ".*test.*", false, false, false);
}

#[test]
fn test_path_matches_with_flags() {
    let query = QueryBuilder::path_matches_with(".*TEST.*", RegexBuilder::case_insensitive)
        .build()
        .unwrap();
    assert_regex_condition(&query, "path", ".*TEST.*", true, false, false);
}

#[test]
fn test_text_matches() {
    let query = QueryBuilder::text_matches("TODO:.*").build().unwrap();
    assert_regex_condition(&query, "text", "TODO:.*", false, false, false);
}

#[test]
fn test_text_matches_multiline() {
    let query = QueryBuilder::text_matches_with("^pub fn.*$", RegexBuilder::multiline)
        .build()
        .unwrap();
    assert_regex_condition(&query, "text", "^pub fn.*$", false, true, false);
}

// ============================================================================
// Invalid Regex Tests
// ============================================================================

#[test]
fn test_invalid_regex_name_matches() {
    let result = QueryBuilder::name_matches("[invalid").build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, BuildError::InvalidRegex(_)));
}

#[test]
fn test_invalid_regex_path_matches() {
    let result = QueryBuilder::path_matches("(unclosed").build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, BuildError::InvalidRegex(_)));
}

#[test]
fn test_invalid_regex_field_matches() {
    // Use a valid field (name) to ensure we get InvalidRegex error, not UnknownField
    let result = QueryBuilder::field_matches("name", "[invalid").build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, BuildError::InvalidRegex(_)));
}

// ============================================================================
// Boolean Combinator Tests
// ============================================================================

#[test]
fn test_and_combinator() {
    let query = QueryBuilder::kind("function")
        .and(QueryBuilder::lang("rust"))
        .build()
        .unwrap();

    match &query.root {
        Expr::And(exprs) => {
            assert_eq!(exprs.len(), 2);
            assert_is_condition(&exprs[0], "kind", "function");
            assert_is_condition(&exprs[1], "lang", "rust");
        }
        _ => panic!("Expected And expression"),
    }
}

#[test]
fn test_or_combinator() {
    let query = QueryBuilder::kind("function")
        .or(QueryBuilder::kind("method"))
        .build()
        .unwrap();

    match &query.root {
        Expr::Or(exprs) => {
            assert_eq!(exprs.len(), 2);
            assert_is_condition(&exprs[0], "kind", "function");
            assert_is_condition(&exprs[1], "kind", "method");
        }
        _ => panic!("Expected Or expression"),
    }
}

#[test]
fn test_and_not_combinator() {
    let query = QueryBuilder::kind("function")
        .and_not(QueryBuilder::name_matches("test.*"))
        .build()
        .unwrap();

    match &query.root {
        Expr::And(exprs) => {
            assert_eq!(exprs.len(), 2);
            assert_is_condition(&exprs[0], "kind", "function");
            match &exprs[1] {
                Expr::Not(inner) => {
                    assert!(matches!(inner.as_ref(), Expr::Condition(_)));
                }
                _ => panic!("Expected Not expression"),
            }
        }
        _ => panic!("Expected And expression"),
    }
}

#[test]
fn test_static_all_combinator() {
    let query = QueryBuilder::all(vec![
        QueryBuilder::kind("function"),
        QueryBuilder::lang("rust"),
        QueryBuilder::path_matches("src/.*"),
    ])
    .build()
    .unwrap();

    match &query.root {
        Expr::And(exprs) => {
            assert_eq!(exprs.len(), 3);
        }
        _ => panic!("Expected And expression"),
    }
}

#[test]
fn test_static_any_combinator() {
    let query = QueryBuilder::any(vec![
        QueryBuilder::kind("function"),
        QueryBuilder::kind("method"),
        QueryBuilder::kind("class"),
    ])
    .build()
    .unwrap();

    match &query.root {
        Expr::Or(exprs) => {
            assert_eq!(exprs.len(), 3);
        }
        _ => panic!("Expected Or expression"),
    }
}

#[test]
fn test_kind_any_helper() {
    let query = QueryBuilder::kind_any(&["function", "method", "class"])
        .build()
        .unwrap();

    match &query.root {
        Expr::Or(exprs) => {
            assert_eq!(exprs.len(), 3);
        }
        _ => panic!("Expected Or expression"),
    }
}

#[test]
fn test_negate_combinator() {
    let query = QueryBuilder::negate(QueryBuilder::name_matches("test.*"))
        .build()
        .unwrap();

    match &query.root {
        Expr::Not(inner) => {
            assert!(matches!(inner.as_ref(), Expr::Condition(_)));
        }
        _ => panic!("Expected Not expression"),
    }
}

#[test]
fn test_complex_nested_expression() {
    // (kind:function OR kind:method) AND lang:rust AND NOT name~=/test/
    let query = QueryBuilder::any(vec![
        QueryBuilder::kind("function"),
        QueryBuilder::kind("method"),
    ])
    .and(QueryBuilder::lang("rust"))
    .and_not(QueryBuilder::name_matches("test"))
    .build()
    .unwrap();

    match &query.root {
        Expr::And(exprs) => {
            assert_eq!(exprs.len(), 3);
            // First should be Or
            assert!(matches!(&exprs[0], Expr::Or(_)));
            // Second should be condition
            assert!(matches!(&exprs[1], Expr::Condition(_)));
            // Third should be Not
            assert!(matches!(&exprs[2], Expr::Not(_)));
        }
        _ => panic!("Expected And expression"),
    }
}

// ============================================================================
// Validation Error Tests
// ============================================================================

#[test]
fn test_empty_query_error() {
    let result = QueryBuilder::new().build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, BuildError::EmptyQuery));
}

#[test]
fn test_unknown_field_error() {
    let result = QueryBuilder::field("nonexistent_field", "value").build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        BuildError::UnknownField { field, available } => {
            assert_eq!(field, "nonexistent_field");
            assert!(available.contains("kind"));
            assert!(available.contains("name"));
        }
        _ => panic!("Expected UnknownField error"),
    }
}

#[test]
fn test_invalid_enum_value_error() {
    // "invalid_kind" is not a valid kind enum value
    let result = QueryBuilder::kind("invalid_kind").build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        BuildError::InvalidEnumValue {
            field,
            value,
            valid,
        } => {
            assert_eq!(field, "kind");
            assert_eq!(value, "invalid_kind");
            assert!(valid.contains("function"));
        }
        _ => panic!("Expected InvalidEnumValue error, got {err:?}"),
    }
}

#[test]
fn test_valid_enum_values_pass() {
    // All valid kind values should pass
    let valid_kinds = ["function", "method", "class", "struct", "module"];
    for kind in valid_kinds {
        let result = QueryBuilder::kind(kind).build();
        assert!(result.is_ok(), "kind '{kind}' should be valid");
    }
}

// ============================================================================
// Generic Field Method Tests
// ============================================================================

#[test]
fn test_generic_field_string() {
    let query = QueryBuilder::field("name", "test").build().unwrap();
    assert_condition(&query, "name", &Operator::Equal, "test");
}

#[test]
fn test_generic_field_matches() {
    let query = QueryBuilder::field_matches("name", "test.*")
        .build()
        .unwrap();
    assert_regex_condition(&query, "name", "test.*", false, false, false);
}

#[test]
fn test_generic_field_matches_with_flags() {
    let query = QueryBuilder::field_matches_with("name", "Test.*", RegexBuilder::case_insensitive)
        .build()
        .unwrap();
    assert_regex_condition(&query, "name", "Test.*", true, false, false);
}

// ============================================================================
// Custom Registry Tests
// ============================================================================

#[test]
fn test_build_with_custom_registry() {
    let registry = FieldRegistry::with_core_fields();
    let query = QueryBuilder::kind("function")
        .build_with_registry(&registry)
        .unwrap();
    assert_condition(&query, "kind", &Operator::Equal, "function");
}

// ============================================================================
// Synthetic Span Tests
// ============================================================================

#[test]
fn test_query_has_synthetic_span() {
    let query = QueryBuilder::kind("function").build().unwrap();
    assert!(query.span.is_synthetic());
}

#[test]
fn test_condition_has_synthetic_span() {
    let query = QueryBuilder::kind("function").build().unwrap();
    match &query.root {
        Expr::Condition(cond) => {
            assert!(cond.span.is_synthetic());
        }
        _ => panic!("Expected Condition expression"),
    }
}

// ============================================================================
// Relation Predicate Tests
// ============================================================================

#[test]
fn test_callers_field() {
    let query = QueryBuilder::callers("process_data").build().unwrap();
    assert_condition(&query, "callers", &Operator::Equal, "process_data");
}

#[test]
fn test_callees_field() {
    let query = QueryBuilder::callees("helper_fn").build().unwrap();
    assert_condition(&query, "callees", &Operator::Equal, "helper_fn");
}

#[test]
fn test_imports_field() {
    let query = QueryBuilder::imports("std::collections").build().unwrap();
    assert_condition(&query, "imports", &Operator::Equal, "std::collections");
}

#[test]
fn test_exports_field() {
    let query = QueryBuilder::exports("MyType").build().unwrap();
    assert_condition(&query, "exports", &Operator::Equal, "MyType");
}

#[test]
fn test_returns_field() {
    let query = QueryBuilder::returns("Result").build().unwrap();
    assert_condition(&query, "returns", &Operator::Equal, "Result");
}

#[test]
fn test_references_field() {
    let query = QueryBuilder::references("CONFIG").build().unwrap();
    assert_condition(&query, "references", &Operator::Equal, "CONFIG");
}

// ============================================================================
// Scope Predicate Tests
// ============================================================================

#[test]
fn test_scope_name_field() {
    let query = QueryBuilder::scope_name("MyModule").build().unwrap();
    assert_condition(&query, "scope.name", &Operator::Equal, "MyModule");
}

#[test]
fn test_scope_parent_field() {
    let query = QueryBuilder::scope_parent("ParentModule").build().unwrap();
    assert_condition(&query, "scope.parent", &Operator::Equal, "ParentModule");
}

#[test]
fn test_scope_ancestor_field() {
    let query = QueryBuilder::scope_ancestor("RootModule").build().unwrap();
    assert_condition(&query, "scope.ancestor", &Operator::Equal, "RootModule");
}

// ============================================================================
// RegexBuilder Integration Tests
// ============================================================================

#[test]
fn test_regex_builder_standalone() {
    let regex = RegexBuilder::new("test.*")
        .case_insensitive()
        .multiline()
        .build()
        .unwrap();
    assert_eq!(regex.pattern, "test.*");
    assert!(regex.flags.case_insensitive);
    assert!(regex.flags.multiline);
    assert!(!regex.flags.dot_all);
}

#[test]
fn test_regex_builder_invalid_pattern() {
    let result = RegexBuilder::new("[invalid").build();
    assert!(result.is_err());
}

// ============================================================================
// And/Or Chaining Edge Cases
// ============================================================================

#[test]
fn test_chaining_from_empty() {
    // new().and(X) should equal X
    let query = QueryBuilder::new()
        .and(QueryBuilder::kind("function"))
        .build()
        .unwrap();
    assert_condition(&query, "kind", &Operator::Equal, "function");
}

#[test]
fn test_chaining_multiple_ands() {
    let query = QueryBuilder::kind("function")
        .and(QueryBuilder::lang("rust"))
        .and(QueryBuilder::path_matches("src/.*"))
        .build()
        .unwrap();

    match &query.root {
        Expr::And(exprs) => {
            assert_eq!(exprs.len(), 3);
        }
        _ => panic!("Expected And expression with 3 children"),
    }
}

#[test]
fn test_chaining_multiple_ors() {
    let query = QueryBuilder::kind("function")
        .or(QueryBuilder::kind("method"))
        .or(QueryBuilder::kind("class"))
        .build()
        .unwrap();

    match &query.root {
        Expr::Or(exprs) => {
            assert_eq!(exprs.len(), 3);
        }
        _ => panic!("Expected Or expression with 3 children"),
    }
}

// ============================================================================
// Test Helpers
// ============================================================================

fn assert_condition(query: &Arc<QueryAST>, field: &str, operator: &Operator, value: &str) {
    match &query.root {
        Expr::Condition(cond) => {
            assert_eq!(cond.field.as_str(), field, "field mismatch");
            assert_eq!(&cond.operator, operator, "operator mismatch");
            match &cond.value {
                Value::String(s) => assert_eq!(s, value, "value mismatch"),
                _ => panic!("Expected String value"),
            }
        }
        _ => panic!("Expected Condition expression, got {:?}", query.root),
    }
}

fn assert_regex_condition(
    query: &Arc<QueryAST>,
    field: &str,
    pattern: &str,
    case_insensitive: bool,
    multiline: bool,
    dot_all: bool,
) {
    match &query.root {
        Expr::Condition(cond) => {
            assert_eq!(cond.field.as_str(), field, "field mismatch");
            assert_eq!(cond.operator, Operator::Regex, "expected Regex operator");
            match &cond.value {
                Value::Regex(regex) => {
                    assert_eq!(regex.pattern, pattern, "pattern mismatch");
                    assert_eq!(
                        regex.flags.case_insensitive, case_insensitive,
                        "case_insensitive flag mismatch"
                    );
                    assert_eq!(regex.flags.multiline, multiline, "multiline flag mismatch");
                    assert_eq!(regex.flags.dot_all, dot_all, "dot_all flag mismatch");
                }
                _ => panic!("Expected Regex value"),
            }
        }
        _ => panic!("Expected Condition expression"),
    }
}

fn assert_is_condition(expr: &Expr, field: &str, value: &str) {
    match expr {
        Expr::Condition(cond) => {
            assert_eq!(cond.field.as_str(), field);
            match &cond.value {
                Value::String(s) => assert_eq!(s, value),
                _ => panic!("Expected String value"),
            }
        }
        _ => panic!("Expected Condition expression"),
    }
}

// ============================================================================
// Enum Regex Tests (HIGH finding fix: FT-C.1)
// ============================================================================

#[test]
fn test_enum_regex_kind_field() {
    // kind~=/function|method/ should be valid (enum regex support)
    let query = QueryBuilder::field_matches("kind", "function|method")
        .build()
        .unwrap();
    assert_regex_condition(&query, "kind", "function|method", false, false, false);
}

#[test]
fn test_enum_regex_scope_type_field() {
    // scope.type~=/module|class/ should be valid
    let query = QueryBuilder::field_matches("scope.type", "module|class")
        .build()
        .unwrap();
    assert_regex_condition(&query, "scope.type", "module|class", false, false, false);
}

#[test]
fn test_enum_regex_with_flags() {
    // Case-insensitive enum regex
    let query =
        QueryBuilder::field_matches_with("kind", "FUNCTION|METHOD", RegexBuilder::case_insensitive)
            .build()
            .unwrap();
    assert_regex_condition(&query, "kind", "FUNCTION|METHOD", true, false, false);
}

// ============================================================================
// Lookaround Regex Tests (HIGH finding fix: FT-C.1)
// ============================================================================

#[test]
fn test_lookahead_positive() {
    // Positive lookahead: (?=...)
    let query = QueryBuilder::name_matches("foo(?=bar)").build().unwrap();
    assert_regex_condition(&query, "name", "foo(?=bar)", false, false, false);
}

#[test]
fn test_lookahead_negative() {
    // Negative lookahead: (?!...)
    let query = QueryBuilder::name_matches("foo(?!bar)").build().unwrap();
    assert_regex_condition(&query, "name", "foo(?!bar)", false, false, false);
}

#[test]
fn test_lookbehind_positive() {
    // Positive lookbehind: (?<=...)
    let query = QueryBuilder::name_matches("(?<=test_)foo").build().unwrap();
    assert_regex_condition(&query, "name", "(?<=test_)foo", false, false, false);
}

#[test]
fn test_lookbehind_negative() {
    // Negative lookbehind: (?<!...)
    let query = QueryBuilder::name_matches("(?<!test_)foo").build().unwrap();
    assert_regex_condition(&query, "name", "(?<!test_)foo", false, false, false);
}

#[test]
fn test_invalid_lookaround_pattern() {
    // Invalid lookaround pattern should be rejected
    let result = QueryBuilder::name_matches("(?<=invalid[)foo").build();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, BuildError::InvalidFancyRegex { .. }));
}

// ============================================================================
// Numeric Comparison Helper Tests (MEDIUM finding)
// ============================================================================

use crate::query::types::{FieldDescriptor, FieldType};

/// Creates a registry with a numeric "line" field for testing numeric comparisons
fn registry_with_line_field() -> FieldRegistry {
    let mut registry = FieldRegistry::with_core_fields();
    registry.add_field(FieldDescriptor {
        name: "line",
        field_type: FieldType::Number,
        operators: &[
            Operator::Equal,
            Operator::Greater,
            Operator::GreaterEq,
            Operator::Less,
            Operator::LessEq,
        ],
        indexed: true,
        doc: "Line number in file",
    });
    registry
}

#[test]
fn test_field_gt() {
    let registry = registry_with_line_field();
    let query = QueryBuilder::field_gt("line", 100)
        .build_with_registry(&registry)
        .unwrap();
    match &query.root {
        Expr::Condition(cond) => {
            assert_eq!(cond.field.as_str(), "line");
            assert_eq!(cond.operator, Operator::Greater);
            match &cond.value {
                Value::Number(n) => assert_eq!(*n, 100),
                _ => panic!("Expected Number value"),
            }
        }
        _ => panic!("Expected Condition expression"),
    }
}

#[test]
fn test_field_gte() {
    let registry = registry_with_line_field();
    let query = QueryBuilder::field_gte("line", 50)
        .build_with_registry(&registry)
        .unwrap();
    match &query.root {
        Expr::Condition(cond) => {
            assert_eq!(cond.field.as_str(), "line");
            assert_eq!(cond.operator, Operator::GreaterEq);
            match &cond.value {
                Value::Number(n) => assert_eq!(*n, 50),
                _ => panic!("Expected Number value"),
            }
        }
        _ => panic!("Expected Condition expression"),
    }
}

#[test]
fn test_field_lt() {
    let registry = registry_with_line_field();
    let query = QueryBuilder::field_lt("line", 200)
        .build_with_registry(&registry)
        .unwrap();
    match &query.root {
        Expr::Condition(cond) => {
            assert_eq!(cond.field.as_str(), "line");
            assert_eq!(cond.operator, Operator::Less);
            match &cond.value {
                Value::Number(n) => assert_eq!(*n, 200),
                _ => panic!("Expected Number value"),
            }
        }
        _ => panic!("Expected Condition expression"),
    }
}

#[test]
fn test_field_lte() {
    let registry = registry_with_line_field();
    let query = QueryBuilder::field_lte("line", 150)
        .build_with_registry(&registry)
        .unwrap();
    match &query.root {
        Expr::Condition(cond) => {
            assert_eq!(cond.field.as_str(), "line");
            assert_eq!(cond.operator, Operator::LessEq);
            match &cond.value {
                Value::Number(n) => assert_eq!(*n, 150),
                _ => panic!("Expected Number value"),
            }
        }
        _ => panic!("Expected Condition expression"),
    }
}

#[test]
fn test_numeric_range_combination() {
    let registry = registry_with_line_field();
    // line > 100 AND line < 200
    let query = QueryBuilder::field_gt("line", 100)
        .and(QueryBuilder::field_lt("line", 200))
        .build_with_registry(&registry)
        .unwrap();

    match &query.root {
        Expr::And(exprs) => {
            assert_eq!(exprs.len(), 2);
            // Verify both conditions
            match (&exprs[0], &exprs[1]) {
                (Expr::Condition(c1), Expr::Condition(c2)) => {
                    assert_eq!(c1.operator, Operator::Greater);
                    assert_eq!(c2.operator, Operator::Less);
                }
                _ => panic!("Expected two Condition expressions"),
            }
        }
        _ => panic!("Expected And expression"),
    }
}
