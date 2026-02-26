//! JSON error output tests (IT-CLI-DIAG-02)
//!
//! Tests JSON-formatted error output for automation/MCP workflows.
//! Verifies that --json flag produces structured error information with:
//! - Error code (`sqry::parse`, `sqry::validation`, etc.)
//! - Human-readable error message
//! - Query string that caused the error
//! - Precise error location (span with start/end positions)
//! - Helpful suggestions and labels
//! - Valid JSON format

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use serde_json::Value;
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

/// Helper: Parse JSON from output and verify it's valid
fn parse_json_output(output: &str) -> Value {
    serde_json::from_str(output).expect("Output should be valid JSON")
}

// ============================================================================
// Parse Error JSON Output
// ============================================================================

#[test]
fn test_json_unmatched_parenthesis_error() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg("(kind:function AND name:test")
        .arg(project.path())
        .assert()
        .failure()
        .code(2) // Parse error exit code
        .get_output()
        .stdout
        .clone();

    let json = parse_json_output(&String::from_utf8_lossy(&output));

    // Verify error structure
    assert!(json["error"].is_object(), "Should have 'error' object");
    assert_eq!(json["error"]["code"], "sqry::parse");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Unmatched")
    );
    assert_eq!(json["error"]["query"], "(kind:function AND name:test");

    // Verify span is present
    assert!(
        json["error"]["span"].is_object(),
        "Should have span information"
    );
    assert!(json["error"]["span"]["start"].is_number());
    assert!(json["error"]["span"]["end"].is_number());

    // Verify label and help
    assert!(json["error"]["label"].is_string(), "Should have label");
    assert!(json["error"]["help"].is_string(), "Should have help text");
}

#[test]
fn test_json_unterminated_string_error() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg(r#"kind:function AND name:"test"#)
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();

    let json = parse_json_output(&String::from_utf8_lossy(&output));

    // After parser unification (FR-2025-015), error code may be sqry::parse or sqry::syntax
    let code = json["error"]["code"].as_str().unwrap();
    assert!(
        code == "sqry::syntax" || code == "sqry::parse",
        "Expected sqry::syntax or sqry::parse, got {code}"
    );
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Unterminated")
    );
    assert!(json["error"]["span"].is_object());
    // Help text may be included in the wrapped message after parser unification
}

#[test]
fn test_json_unterminated_regex_error() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg("kind:function AND name~=/test")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();

    let json = parse_json_output(&String::from_utf8_lossy(&output));

    // After parser unification (FR-2025-015), error code may be sqry::parse or sqry::syntax
    let code = json["error"]["code"].as_str().unwrap();
    assert!(
        code == "sqry::syntax" || code == "sqry::parse",
        "Expected sqry::syntax or sqry::parse, got {code}"
    );
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Unterminated")
    );
    // Help text may be included in the wrapped message after parser unification
}

// ============================================================================
// Validation Error JSON Output
// ============================================================================

#[test]
fn test_json_unknown_field_with_suggestion() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg("kind:function AND knd:test")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();

    let json = parse_json_output(&String::from_utf8_lossy(&output));

    assert_eq!(json["error"]["code"], "sqry::validation");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Unknown field")
    );
    assert!(json["error"]["message"].as_str().unwrap().contains("knd"));

    // Verify suggestion is present
    assert!(
        json["error"]["suggestion"].is_string(),
        "Should have suggestion"
    );
    assert_eq!(json["error"]["suggestion"], "kind");

    // Verify help includes suggestion
    assert!(
        json["error"]["help"]
            .as_str()
            .unwrap()
            .contains("Did you mean 'kind'?")
    );
}

#[test]
fn test_json_invalid_enum_value() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg("kind:invalid_kind")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();

    let json = parse_json_output(&String::from_utf8_lossy(&output));

    assert_eq!(json["error"]["code"], "sqry::validation");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Invalid value")
    );
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("invalid_kind")
    );

    // Verify help includes valid values
    let help = json["error"]["help"].as_str().unwrap();
    assert!(help.contains("Valid values:"));
    assert!(help.contains("function"));
    assert!(help.contains("method"));
    assert!(help.contains("class"));
}

#[test]
fn test_json_unknown_field_no_suggestion() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg("kind:function AND xyz:test")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();

    let json = parse_json_output(&String::from_utf8_lossy(&output));

    assert_eq!(json["error"]["code"], "sqry::validation");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Unknown field")
    );
    assert!(json["error"]["message"].as_str().unwrap().contains("xyz"));

    // No suggestion field if there's no close match
    // (field may not be present or may be null)
    if json["error"]["suggestion"].is_string() {
        // If present, it should be empty or a different value
        assert_ne!(json["error"]["suggestion"].as_str().unwrap(), "xyz");
    }
}

// ============================================================================
// JSON Format Validation
// ============================================================================

#[test]
fn test_json_output_is_valid_json() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg("(kind:function")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();

    // Should not panic
    let _json = parse_json_output(&String::from_utf8_lossy(&output));
}

#[test]
fn test_json_error_has_required_fields() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg("(kind:function AND name:test")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();

    let json = parse_json_output(&String::from_utf8_lossy(&output));

    // Required fields that should always be present
    assert!(json["error"].is_object());
    assert!(json["error"]["code"].is_string());
    assert!(json["error"]["message"].is_string());
    assert!(json["error"]["query"].is_string());
}

#[test]
fn test_json_span_has_start_and_end() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg("kind:function AND knd:test")
        .arg(project.path())
        .assert()
        .failure()
        .code(2)
        .get_output()
        .stdout
        .clone();

    let json = parse_json_output(&String::from_utf8_lossy(&output));

    // Span should have both start and end positions
    assert!(json["error"]["span"]["start"].is_number());
    assert!(json["error"]["span"]["end"].is_number());

    let start = json["error"]["span"]["start"].as_u64().unwrap();
    let end = json["error"]["span"]["end"].as_u64().unwrap();
    assert!(end > start, "End position should be after start position");
}

// ============================================================================
// Error Code Verification
// ============================================================================

#[test]
fn test_json_parse_error_code() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg("(kind:function")
        .arg(project.path())
        .assert()
        .failure()
        .code(2) // Parse errors should exit with code 2
        .get_output()
        .stdout
        .clone();

    let json = parse_json_output(&String::from_utf8_lossy(&output));
    assert_eq!(json["error"]["code"], "sqry::parse");
}

#[test]
fn test_json_validation_error_code() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    let output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg("kind:function AND knd:test")
        .arg(project.path())
        .assert()
        .failure()
        .code(2) // Validation errors should exit with code 2
        .get_output()
        .stdout
        .clone();

    let json = parse_json_output(&String::from_utf8_lossy(&output));
    assert_eq!(json["error"]["code"], "sqry::validation");
}

// ============================================================================
// Comparison: JSON vs Terminal Output
// ============================================================================

#[test]
fn test_json_flag_changes_output_format() {
    let project = create_test_project(&[("test.rs", "fn test() {}")]);

    // Without --json: should get terminal output (not JSON)
    let terminal_output = sqry_cmd()
        .arg("query")
        .arg("(kind:function")
        .arg(project.path())
        .assert()
        .failure()
        .get_output()
        .stderr // Terminal errors go to stderr
        .clone();

    let terminal_str = String::from_utf8_lossy(&terminal_output);

    // Terminal output should contain box drawing characters, not JSON
    assert!(
        terminal_str.contains("│") || terminal_str.contains("╭") || terminal_str.contains("Error")
    );
    assert!(!terminal_str.contains(r#""error""#));

    // With --json: should get JSON output
    let json_output = sqry_cmd()
        .arg("query")
        .arg("--json")
        .arg("(kind:function")
        .arg(project.path())
        .assert()
        .failure()
        .get_output()
        .stdout // JSON output goes to stdout
        .clone();

    let json_str = String::from_utf8_lossy(&json_output);

    // JSON output should be parseable
    let _json = serde_json::from_str::<Value>(&json_str).expect("Should be valid JSON");
}
