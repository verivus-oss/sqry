//! FT-C.1: Regex Lookahead/Lookbehind Support Tests
//!
//! Tests that the regex validator accepts lookahead and lookbehind patterns.
//! The regex crate 1.10+ supports these patterns, so the validator should
//! transparently accept them without code changes.

use sqry_core::query::registry::FieldRegistry;
use sqry_core::query::types::{
    Condition, Expr, Field, Operator, RegexFlags, RegexValue, Span, Value,
};
use sqry_core::query::validator::Validator;

#[test]
fn test_validate_regex_positive_lookahead() {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);

    // Positive lookahead: Match "async" only if followed by "function"
    let condition = Expr::Condition(Condition {
        field: Field::new("name"),
        operator: Operator::Regex,
        value: Value::Regex(RegexValue {
            pattern: "async(?=\\s*function)".to_string(),
            flags: RegexFlags::default(),
        }),
        span: Span::default(),
    });

    let result = validator.validate(&condition);
    assert!(
        result.is_ok(),
        "Positive lookahead should be valid: {result:?}"
    );
}

#[test]
fn test_validate_regex_negative_lookahead() {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);

    // Negative lookahead: Match "function" not followed by "async"
    let condition = Expr::Condition(Condition {
        field: Field::new("name"),
        operator: Operator::Regex,
        value: Value::Regex(RegexValue {
            pattern: "function(?!\\s*async)".to_string(),
            flags: RegexFlags::default(),
        }),
        span: Span::default(),
    });

    let result = validator.validate(&condition);
    assert!(
        result.is_ok(),
        "Negative lookahead should be valid: {result:?}"
    );
}

#[test]
fn test_validate_regex_positive_lookbehind() {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);

    // Positive lookbehind: Match "function" only if preceded by "async"
    let condition = Expr::Condition(Condition {
        field: Field::new("name"),
        operator: Operator::Regex,
        value: Value::Regex(RegexValue {
            pattern: "(?<=async\\s)function".to_string(),
            flags: RegexFlags::default(),
        }),
        span: Span::default(),
    });

    let result = validator.validate(&condition);
    assert!(
        result.is_ok(),
        "Positive lookbehind should be valid: {result:?}"
    );
}

#[test]
fn test_validate_regex_negative_lookbehind() {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);

    // Negative lookbehind: Match "function" not preceded by "static"
    let condition = Expr::Condition(Condition {
        field: Field::new("name"),
        operator: Operator::Regex,
        value: Value::Regex(RegexValue {
            pattern: "(?<!static\\s)function".to_string(),
            flags: RegexFlags::default(),
        }),
        span: Span::default(),
    });

    let result = validator.validate(&condition);
    assert!(
        result.is_ok(),
        "Negative lookbehind should be valid: {result:?}"
    );
}

#[test]
fn test_validate_regex_complex_lookaround() {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);

    // Complex pattern combining multiple lookarounds
    let condition = Expr::Condition(Condition {
        field: Field::new("name"),
        operator: Operator::Regex,
        value: Value::Regex(RegexValue {
            pattern: "(?<=^|\\s)(?=\\w+)(?!test_)\\w+".to_string(),
            flags: RegexFlags::default(),
        }),
        span: Span::default(),
    });

    let result = validator.validate(&condition);
    assert!(
        result.is_ok(),
        "Complex lookaround should be valid: {result:?}"
    );
}

#[test]
fn test_validate_regex_invalid_lookaround() {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);

    // Invalid lookahead: unclosed parenthesis
    let condition = Expr::Condition(Condition {
        field: Field::new("name"),
        operator: Operator::Regex,
        value: Value::Regex(RegexValue {
            pattern: "(?=unclosed".to_string(),
            flags: RegexFlags::default(),
        }),
        span: Span::default(),
    });

    let result = validator.validate(&condition);
    assert!(result.is_err(), "Invalid lookaround should be rejected");
}

#[test]
fn test_validate_regex_variable_length_lookbehind() {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);

    // Variable-length lookbehind is NOT supported by fancy-regex
    // This is a known limitation - lookbehind must have constant width
    let condition = Expr::Condition(Condition {
        field: Field::new("name"),
        operator: Operator::Regex,
        value: Value::Regex(RegexValue {
            pattern: "(?<=async\\s+)\\w+".to_string(), // \s+ is variable length
            flags: RegexFlags::default(),
        }),
        span: Span::default(),
    });

    let result = validator.validate(&condition);
    // This should fail with a clear error message
    assert!(
        result.is_err(),
        "Variable-length lookbehind should be rejected"
    );
    if let Err(e) = result {
        let error_str = format!("{e:?}");
        assert!(
            error_str.contains("constant size") || error_str.contains("variable"),
            "Error should mention constant/variable length: {error_str}"
        );
    }
}

#[test]
fn test_validate_regex_fixed_length_lookbehind() {
    let registry = FieldRegistry::with_core_fields();
    let validator = Validator::new(registry);

    // Fixed-length lookbehind (constant width) IS supported
    let condition = Expr::Condition(Condition {
        field: Field::new("name"),
        operator: Operator::Regex,
        value: Value::Regex(RegexValue {
            pattern: "(?<=async )\\w+".to_string(), // Single space is fixed length
            flags: RegexFlags::default(),
        }),
        span: Span::default(),
    });

    let result = validator.validate(&condition);
    assert!(
        result.is_ok(),
        "Fixed-length lookbehind should be valid: {result:?}"
    );
}
