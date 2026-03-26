//! CLI diagnostic error message tests (IT-CLI-DIAG-01)
//!
//! Tests rich error diagnostics with miette integration.
//! Verifies error messages contain:
//! - Source code snippets
//! - Precise error locations (spans)
//! - Helpful suggestions and context
//! - Unicode box drawing

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Helper: Create test project with files
fn create_test_project(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    for (path, content) in files {
        let file_path = dir.path().join(path);
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        fs::write(&file_path, content).unwrap();
    }
    dir
}

/// Helper: Get sqry binary command
fn sqry_cmd() -> Command {
    let path = sqry_bin();
    Command::new(path)
}

// ============================================================================
// Parse Error Diagnostics
// ============================================================================

#[test]
fn test_unmatched_parenthesis_error() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("(kind:function AND name:test")
        .arg(project.path())
        .assert()
        .failure()
        .code(2) // Parse error exit code
        .stderr(predicate::str::contains("sqry::parse"))
        .stderr(predicate::str::contains("Unmatched opening parenthesis"))
        .stderr(predicate::str::contains("(kind:function AND name:test"));
    // NOTE: After parser unification, boolean queries that fail
    // to parse show a wrapped error message explaining compat translation
    // is skipped. The detailed miette suggestion "Add a closing ')'"
    // is included in the wrapped message.
}

#[test]
fn test_unterminated_string_error() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg(r#"kind:function AND name:"test"#)
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("sqry::")) // Either sqry::syntax or sqry::parse
        .stderr(predicate::str::contains("Unterminated string"))
        .stderr(predicate::str::contains(r#"kind:function AND name:"test"#));
    // NOTE: After parser unification, boolean queries that fail
    // to parse show a wrapped error message. Detailed miette suggestions
    // are included in the wrapped message.
}

#[test]
fn test_unterminated_regex_error() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("kind:function AND name~=/test")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("sqry::")) // Either sqry::syntax or sqry::parse
        .stderr(predicate::str::contains("Unterminated regex"))
        .stderr(predicate::str::contains("kind:function AND name~=/test"));
    // NOTE: After parser unification, boolean queries that fail
    // to parse show a wrapped error message. Detailed miette suggestions
    // are included in the wrapped message.
}

// ============================================================================
// Validation Error Diagnostics
// ============================================================================

#[test]
fn test_unknown_field_with_suggestion() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("kind:function AND knd:test")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("sqry::validation"))
        .stderr(predicate::str::contains("Unknown field 'knd'"))
        .stderr(predicate::str::contains("Did you mean 'kind'?"))
        .stderr(predicate::str::contains("kind:function AND knd:test"))
        .stderr(predicate::str::contains(
            "unknown field, did you mean 'kind'?",
        ))
        .stderr(predicate::str::contains("Use 'sqry query --help'"));
}

#[test]
fn test_invalid_enum_value() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("kind:invalid_kind")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("sqry::validation"))
        .stderr(predicate::str::contains(
            "Invalid value 'invalid_kind' for field 'kind'",
        ))
        .stderr(predicate::str::contains("kind:invalid_kind"))
        .stderr(predicate::str::contains("invalid enum value"))
        .stderr(predicate::str::contains("Valid values:"))
        .stderr(predicate::str::contains("function"))
        .stderr(predicate::str::contains("method"))
        .stderr(predicate::str::contains("class"));
}

#[test]
fn test_unknown_field_no_suggestion() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("kind:function AND xyz:test")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("sqry::validation"))
        .stderr(predicate::str::contains("Unknown field 'xyz'"))
        .stderr(predicate::str::contains("kind:function AND xyz:test"))
        .stderr(predicate::str::contains("unknown field"));
}

// ============================================================================
// Complex Query Error Diagnostics
// ============================================================================

#[test]
fn test_nested_parenthesis_error() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("kind:function AND (name:test OR (name:foo)")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("sqry::parse"))
        .stderr(predicate::str::contains("Unmatched opening parenthesis"));
}

#[test]
fn test_multiple_boolean_operators_with_error() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("kind:function OR knd:method AND async:true")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("sqry::validation"))
        .stderr(predicate::str::contains("Unknown field 'knd'"));
}

// ============================================================================
// Unicode Box Drawing Verification
// ============================================================================

#[test]
fn test_error_contains_unicode_box_drawing() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("(kind:function")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("×")) // Error marker
        .stderr(predicate::str::contains("│")) // Box drawing
        .stderr(predicate::str::contains("╭")) // Box corner
        .stderr(predicate::str::contains("╰")); // Box corner
}

// ============================================================================
// Error Code Verification
// ============================================================================

#[test]
fn test_parse_error_exit_code() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("(kind:function")
        .arg(project.path())
        .assert()
        .failure()
        .code(2); // Parse errors should exit with code 2
}

#[test]
fn test_validation_error_exit_code() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("kind:function AND knd:test") // Use boolean syntax to force validation
        .arg(project.path())
        .assert()
        .failure()
        .code(2); // Validation errors should exit with code 2
}

// ============================================================================
// Span Location Accuracy
// ============================================================================

#[test]
fn test_error_span_points_to_correct_location() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("kind:function AND knd:test")
        .arg(project.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("position 18")); // Exact position of 'knd'
}

#[test]
fn test_error_shows_full_query_context() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let query = "kind:function AND name:test AND invalid_field:value";

    sqry_cmd()
        .arg("query")
        .arg(query)
        .arg(project.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(query)); // Full query shown in error
}
