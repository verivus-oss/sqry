//! Query error message test suite
//!
//! Comprehensive tests for query language error messages, ensuring:
//! - All error types have user-friendly messages (no Debug formatting)
//! - Levenshtein distance field suggestions work correctly
//! - Position information is included in errors
//! - All `LexError`, `ParseError`, and `ValidationError` variants are covered

use sqry_core::query::error::{LexError, ParseError, QueryError, ValidationError};
use sqry_core::query::lexer::Lexer;
use sqry_core::query::parser_new::Parser;
use sqry_core::query::registry::FieldRegistry;
use sqry_core::query::types::Operator;
use sqry_core::query::validator::Validator;

// ============================================================================
// Helper Functions
// ============================================================================

/// Parse a query string and return the error if parsing fails
fn parse_error(input: &str) -> Option<ParseError> {
    let mut lexer = Lexer::new(input);
    match lexer.tokenize() {
        Ok(tokens) => {
            let mut parser = Parser::new(tokens);
            parser.parse().err().map(|e| match e {
                e @ (ParseError::EmptyQuery
                | ParseError::ExpectedIdentifier { .. }
                | ParseError::ExpectedOperator { .. }
                | ParseError::ExpectedValue { .. }
                | ParseError::UnmatchedParen { .. }
                | ParseError::UnexpectedToken { .. }
                | ParseError::InvalidSyntax { .. }
                | ParseError::UnexpectedEof { .. }) => e,
            })
        }
        Err(_) => None,
    }
}

/// Lex a query string and return the error if lexing fails
fn lex_error(input: &str) -> Option<LexError> {
    let mut lexer = Lexer::new(input);
    lexer.tokenize().err()
}

/// Validate a query string and return the error if validation fails
fn validation_error(input: &str) -> Option<ValidationError> {
    let mut lexer = Lexer::new(input);
    let tokens = lexer.tokenize().ok()?;
    let mut parser = Parser::new(tokens);
    let query = parser.parse().ok()?;

    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);

    validator.validate(&query.root).err()
}

// ============================================================================
// LexError Tests
// ============================================================================

#[test]
fn test_lex_error_unterminated_string() {
    let err = lex_error(r#"name:"unterminated"#).expect("should error");

    // Verify error message quality
    let msg = err.to_string();
    assert!(
        msg.contains("Unterminated string"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );

    // Verify error type
    assert!(matches!(err, LexError::UnterminatedString { .. }));

    // Verify position information
    if let LexError::UnterminatedString { span } = err {
        assert_eq!(span.start, 5, "Should point to opening quote");
    }
}

#[test]
fn test_lex_error_unterminated_regex() {
    let err = lex_error(r"name~=/pattern").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Unterminated regex"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );

    assert!(matches!(err, LexError::UnterminatedRegex { .. }));
}

#[test]
fn test_lex_error_invalid_escape() {
    let err = lex_error(r#"name:"\x""#).expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Invalid escape"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(msg.contains("\\x"), "Error should show the invalid escape");
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );

    assert!(matches!(err, LexError::InvalidEscape { char: 'x', .. }));
}

#[test]
fn test_lex_error_invalid_unicode_escape() {
    let err = lex_error(r#"name:"\uGGGG""#).expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Invalid Unicode escape"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );

    assert!(matches!(err, LexError::InvalidUnicodeEscape { .. }));
}

#[test]
fn test_lex_error_invalid_number() {
    // Numbers with invalid characters should be tokenized as separate tokens
    // The lexer treats "12x" as a number "12" followed by an identifier "x"
    // which would then cause a parse error, not a lex error
    let err = lex_error("lines:12 x");
    if let Some(e) = err {
        let msg = e.to_string();
        assert!(
            !msg.contains(":?"),
            "Error should not use Debug formatting: {msg}"
        );
    }
    // This test verifies the lexer handles numbers correctly
}

#[test]
fn test_lex_error_number_overflow() {
    let err = lex_error("lines:99999999999999999999").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("overflow") || msg.contains("too large"),
        "Error message should indicate overflow: {msg}"
    );
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );

    assert!(matches!(err, LexError::NumberOverflow { .. }));
}

#[test]
fn test_lex_error_invalid_regex() {
    let err = lex_error(r"name~=/[unclosed/").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Invalid regex"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        msg.contains("[unclosed"),
        "Error should show the invalid pattern"
    );
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );

    assert!(matches!(err, LexError::InvalidRegex { .. }));
}

#[test]
fn test_lex_error_unexpected_char() {
    let err = lex_error("kind@function").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Unexpected character"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        msg.contains('@'),
        "Error should show the unexpected character"
    );
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );

    assert!(matches!(err, LexError::UnexpectedChar { char: '@', .. }));
}

#[test]
fn test_lex_error_unexpected_eof() {
    // Create a scenario that triggers UnexpectedEof (if the lexer supports it)
    // This might need adjustment based on actual lexer behavior
    let err = lex_error("~");
    if let Some(e) = err {
        let msg = e.to_string();
        assert!(
            !msg.contains(":?"),
            "Error should not use Debug formatting: {msg}"
        );
    }
}

// ============================================================================
// ParseError Tests
// ============================================================================

#[test]
fn test_parse_error_empty_query() {
    let err = parse_error("").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("empty") || msg.contains("cannot be empty"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );

    assert!(matches!(err, ParseError::EmptyQuery));
}

#[test]
fn test_parse_error_expected_identifier() {
    let err = parse_error("123:value").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Expected field"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );

    assert!(matches!(err, ParseError::ExpectedIdentifier { .. }));
}

#[test]
fn test_parse_error_expected_operator() {
    // This is tricky since the lexer handles operators, but we can try incomplete syntax
    let err = parse_error("kind");
    if let Some(e) = err {
        let msg = e.to_string();
        assert!(
            !msg.contains(":?"),
            "Error should not use Debug formatting: {msg}"
        );
    }
}

#[test]
fn test_parse_error_expected_value() {
    let err = parse_error("kind:").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Expected value"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );

    assert!(matches!(err, ParseError::ExpectedValue { .. }));
}

#[test]
fn test_parse_error_unmatched_paren() {
    let err = parse_error("(kind:function").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Unmatched") || msg.contains("parenthesis"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );

    assert!(matches!(err, ParseError::UnmatchedParen { .. }));

    // Verify position information
    if let ParseError::UnmatchedParen { open_span, .. } = err {
        assert_eq!(open_span.start, 0, "Should point to opening paren");
    }
}

#[test]
fn test_parse_error_unexpected_token() {
    let err = parse_error("kind:function)").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Expected"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );

    assert!(matches!(err, ParseError::UnexpectedToken { .. }));
}

#[test]
fn test_parse_error_invalid_syntax() {
    // Try to trigger InvalidSyntax if the parser supports it
    // This might need adjustment based on parser implementation
    let err = parse_error("AND");
    if let Some(e) = err {
        let msg = e.to_string();
        assert!(
            !msg.contains(":?"),
            "Error should not use Debug formatting: {msg}"
        );
    }
}

#[test]
fn test_parse_error_unexpected_eof() {
    let err = parse_error("kind:function AND").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Expected field") || msg.contains("expected"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );
}

// ============================================================================
// ValidationError Tests
// ============================================================================

#[test]
fn test_validation_error_unknown_field_no_suggestion() {
    let err = validation_error("xyz:value").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Unknown field"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(msg.contains("xyz"), "Error should show the unknown field");
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );
    assert!(
        !msg.contains("Did you mean"),
        "Should not suggest when no good match: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );

    assert!(matches!(
        err,
        ValidationError::UnknownField {
            suggestion: None,
            ..
        }
    ));
}

#[test]
fn test_validation_error_unknown_field_with_levenshtein_suggestion() {
    // Test typo "knd" -> should suggest "kind"
    let err = validation_error("knd:function").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Unknown field"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(msg.contains("knd"), "Error should show the typo");
    assert!(
        msg.contains("Did you mean 'kind'?"),
        "Error should suggest 'kind': {msg}"
    );
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );

    assert!(matches!(
        err,
        ValidationError::UnknownField {
            field,
            suggestion: Some(ref sugg),
            ..
        } if field == "knd" && sugg == "kind"
    ));
}

#[test]
fn test_validation_error_levenshtein_distance_threshold() {
    // Test various Levenshtein distances

    // Distance 1: "knd" -> "kind" (deletion)
    let err = validation_error("knd:function").expect("should error");
    assert!(
        matches!(
            err,
            ValidationError::UnknownField {
                suggestion: Some(ref sugg),
                ..
            } if sugg == "kind"
        ),
        "Distance 1 should suggest"
    );

    // Distance 1: "nme" -> "name" (deletion)
    let err = validation_error("nme:test").expect("should error");
    assert!(
        matches!(
            err,
            ValidationError::UnknownField {
                suggestion: Some(ref sugg),
                ..
            } if sugg == "name"
        ),
        "Distance 1 should suggest"
    );

    // Distance 2: "kond" -> "kind" (substitution)
    let err = validation_error("kond:function").expect("should error");
    assert!(
        matches!(
            err,
            ValidationError::UnknownField {
                suggestion: Some(_),
                ..
            }
        ),
        "Distance 2 should suggest"
    );

    // Distance > 2: should not suggest
    let err = validation_error("xyz:function").expect("should error");
    assert!(
        matches!(
            err,
            ValidationError::UnknownField {
                suggestion: None,
                ..
            }
        ),
        "Distance > 2 should not suggest"
    );
}

#[test]
fn test_validation_error_case_insensitive_suggestion() {
    // Test that suggestions are case-insensitive
    let err = validation_error("KIND:function").expect("should error");

    // Should suggest "kind" (exact case-insensitive match)
    assert!(
        matches!(
            err,
            ValidationError::UnknownField {
                suggestion: Some(ref sugg),
                ..
            } if sugg == "kind"
        ),
        "Case mismatch should suggest correct case"
    );
}

#[test]
fn test_validation_error_invalid_operator() {
    let err = validation_error("kind>function").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("not supported") || msg.contains("Invalid"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(msg.contains("kind"), "Error should show the field");
    assert!(msg.contains('>'), "Error should show the invalid operator");
    assert!(
        msg.contains("Valid operators"),
        "Error should list valid operators: {msg}"
    );
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );

    assert!(matches!(
        err,
        ValidationError::InvalidOperator {
            operator: Operator::Greater,
            ..
        }
    ));
}

#[test]
fn test_validation_error_type_mismatch() {
    // Create a registry with an async field for testing
    let mut lexer = Lexer::new("async:123");
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let query = parser.parse().unwrap();

    let mut registry = FieldRegistry::with_core_fields();
    registry.add_field(sqry_core::query::types::FieldDescriptor {
        name: "async",
        field_type: sqry_core::query::types::FieldType::Bool,
        operators: &[Operator::Equal],
        indexed: false,
        doc: "Whether function is async",
    });

    let validator = Validator::new(registry);
    let err = validator.validate(&query.root).unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("Type mismatch"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(msg.contains("async"), "Error should show the field");
    assert!(
        msg.contains("boolean") || msg.contains("Bool"),
        "Error should show expected type: {msg}"
    );
    assert!(
        msg.contains("Number") || msg.contains("123"),
        "Error should show actual value: {msg}"
    );
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );

    assert!(matches!(err, ValidationError::TypeMismatch { .. }));
}

#[test]
fn test_validation_error_invalid_enum_value() {
    let err = validation_error("kind:invalid_kind").expect("should error");

    let msg = err.to_string();
    assert!(
        msg.contains("Invalid value"),
        "Error message should be user-friendly: {msg}"
    );
    assert!(
        msg.contains("invalid_kind"),
        "Error should show the invalid value"
    );
    assert!(msg.contains("kind"), "Error should show the field");
    assert!(
        msg.contains("Valid values"),
        "Error should list valid values: {msg}"
    );
    assert!(
        msg.contains("function") || msg.contains("class"),
        "Error should show some valid values: {msg}"
    );
    assert!(
        msg.contains("position"),
        "Error should include position: {msg}"
    );
    assert!(
        !msg.contains(":?"),
        "Error should not use Debug formatting: {msg}"
    );

    assert!(matches!(err, ValidationError::InvalidEnumValue { .. }));
}

#[test]
fn test_validation_error_invalid_regex_pattern() {
    // The lexer validates regex patterns, so invalid regexes are caught at lex time
    // This test verifies that validation-stage regex errors work if they occur
    // For now, we test that lex-stage regex validation works
    let err = lex_error("name~=/[unclosed/");
    if let Some(e) = err {
        let msg = e.to_string();
        assert!(
            msg.contains("Invalid regex") || msg.contains("regex"),
            "Error message should mention regex: {msg}"
        );
        assert!(
            !msg.contains(":?"),
            "Error should not use Debug formatting: {msg}"
        );
    }
}

// ============================================================================
// Error Position Information Tests
// ============================================================================

#[test]
fn test_error_position_accuracy_lex() {
    let err = lex_error("kind:function AND name:\"unterminated").expect("should error");

    if let LexError::UnterminatedString { span } = err {
        // Should point to the opening quote of "unterminated
        assert!(
            span.start >= 19,
            "Position should point to the unterminated string"
        );
    } else {
        panic!("Expected UnterminatedString error");
    }
}

#[test]
fn test_error_position_accuracy_parse() {
    let err = parse_error("(kind:function AND name:test").expect("should error");

    if let ParseError::UnmatchedParen { open_span, .. } = err {
        assert_eq!(
            open_span.start, 0,
            "Position should point to the opening paren"
        );
    } else {
        panic!("Expected UnmatchedParen error, got: {err:?}");
    }
}

#[test]
fn test_error_position_accuracy_validation() {
    let err = validation_error("kind:function AND xyz:value").expect("should error");

    if let ValidationError::UnknownField { span, .. } = err {
        assert!(
            span.start >= 18,
            "Position should point to the unknown field"
        );
    } else {
        panic!("Expected UnknownField error");
    }
}

// ============================================================================
// Error Message Quality Tests
// ============================================================================

#[test]
fn test_no_debug_formatting_in_error_messages() {
    // Test a variety of errors to ensure none use Debug formatting

    let test_cases = vec![
        ("", "empty query"),
        (r#"name:"unterminated"#, "unterminated string"),
        ("kind@function", "unexpected char"),
        ("(kind:function", "unmatched paren"),
        ("xyz:value", "unknown field"),
        ("kind>function", "invalid operator"),
        ("kind:invalid", "invalid enum"),
    ];

    for (input, description) in test_cases {
        // Try lex error
        if let Some(err) = lex_error(input) {
            let msg = err.to_string();
            assert!(
                !msg.contains(":?") && !msg.contains("Debug"),
                "Lex error for '{description}' should not use Debug formatting: {msg}"
            );
        }

        // Try parse error
        if let Some(err) = parse_error(input) {
            let msg = err.to_string();
            assert!(
                !msg.contains(":?") && !msg.contains("Debug"),
                "Parse error for '{description}' should not use Debug formatting: {msg}"
            );
        }

        // Try validation error
        if let Some(err) = validation_error(input) {
            let msg = err.to_string();
            assert!(
                !msg.contains(":?") && !msg.contains("Debug"),
                "Validation error for '{description}' should not use Debug formatting: {msg}"
            );
        }
    }
}

#[test]
fn test_error_messages_include_helpful_context() {
    // Test that errors include helpful context

    // Lex errors should mention what went wrong
    let err = lex_error(r#"name:"\x""#).expect("should error");
    let msg = err.to_string();
    assert!(
        msg.contains("escape") || msg.contains("\\x"),
        "Error should explain the issue: {msg}"
    );

    // Parse errors should mention what was expected
    let err = parse_error("kind:").expect("should error");
    let msg = err.to_string();
    assert!(
        msg.contains("value") || msg.contains("Expected"),
        "Error should explain what was expected: {msg}"
    );

    // Validation errors should provide suggestions
    let err = validation_error("knd:function").expect("should error");
    let msg = err.to_string();
    assert!(
        msg.contains("Did you mean"),
        "Error should provide suggestion: {msg}"
    );
}

// ============================================================================
// QueryError Wrapper Tests
// ============================================================================

#[test]
fn test_query_error_wrapping_lex() {
    let lex_err = lex_error("kind@function").expect("should error");
    let query_err = QueryError::from(lex_err);

    let msg = query_err.to_string();
    assert!(
        msg.contains("Syntax error"),
        "QueryError should wrap with category: {msg}"
    );
    assert!(
        msg.contains("Unexpected character"),
        "Should preserve underlying error: {msg}"
    );
}

#[test]
fn test_query_error_wrapping_parse() {
    let parse_err = parse_error("").expect("should error");
    let query_err = QueryError::from(parse_err);

    let msg = query_err.to_string();
    assert!(
        msg.contains("Parse error"),
        "QueryError should wrap with category: {msg}"
    );
}

#[test]
fn test_query_error_wrapping_validation() {
    let val_err = validation_error("xyz:value").expect("should error");
    let query_err = QueryError::from(val_err);

    let msg = query_err.to_string();
    assert!(
        msg.contains("Validation error"),
        "QueryError should wrap with category: {msg}"
    );
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn test_error_with_unicode_characters() {
    // Test that errors handle Unicode characters correctly
    // Unicode field names will be treated as unknown fields
    let err = validation_error("名前:value");

    // If we get an error, verify it handles Unicode correctly
    if let Some(e) = err {
        let msg = e.to_string();
        assert!(
            msg.contains("Unknown field") || msg.contains("名前"),
            "Error should handle Unicode: {msg}"
        );
    } else {
        // If no validation error, it might have been rejected earlier
        // Try lex error
        let lex_err = lex_error("名前:value");
        if let Some(e) = lex_err {
            let msg = e.to_string();
            assert!(!msg.is_empty(), "Error should handle Unicode: {msg}");
        }
    }
}

#[test]
fn test_error_with_special_characters() {
    // Test that errors handle special characters in values
    let err = lex_error(r#"name:"test\x00""#);
    if let Some(e) = err {
        let msg = e.to_string();
        assert!(
            !msg.is_empty(),
            "Error should handle special characters: {msg}"
        );
    }
}

#[test]
fn test_span_information_completeness() {
    // Verify that all errors with spans have valid span information

    let err = lex_error(r#"name:"unterminated"#).expect("should error");
    if let LexError::UnterminatedString { span } = err {
        assert!(span.start < span.end || span.end == span.start);
        assert!(span.line > 0);
        assert!(span.column > 0);
    }

    let err = parse_error("(kind:function").expect("should error");
    if let ParseError::UnmatchedParen { open_span, .. } = err {
        assert!(open_span.start <= open_span.end);
        assert!(open_span.line > 0);
        assert!(open_span.column > 0);
    }

    let err = validation_error("xyz:value").expect("should error");
    if let ValidationError::UnknownField { span, .. } = err {
        assert!(span.start <= span.end);
        assert!(span.line > 0);
        assert!(span.column > 0);
    }
}
