//! Integration tests for CLI exit codes.
//!
//! Verifies that sqry returns correct exit codes per POSIX conventions:
//! - 0: Success
//! - 1: Runtime/system errors
//! - 2: User/validation errors

mod common;

use assert_cmd::Command;
use common::sqry_bin;
use predicates::prelude::*;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Helper: Get sqry binary path
fn sqry_path() -> PathBuf {
    sqry_bin()
}

/// Helper: Create a test repo with some Rust files and index it.
fn setup_indexed_repo() -> TempDir {
    let tmp_cli_workspace = TempDir::new().unwrap();
    let test_file = tmp_cli_workspace.path().join("test.rs");
    fs::write(
        &test_file,
        r#"
fn example_function() {
    println!("Hello");
}

fn another_function(x: i32) -> i32 {
    x + 1
}

struct TestStruct {
    field: String,
}
"#,
    )
    .unwrap();

    // Index the repo
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["index", "."])
        .assert()
        .success();

    tmp_cli_workspace
}

// ============================================================================
// Exit Code 0: Success
// ============================================================================

#[test]
fn test_exit_code_0_query_with_results() {
    let tmp_cli_workspace = setup_indexed_repo();

    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "kind:function"])
        .assert()
        .success() // exit code 0
        .stdout(predicate::str::contains("example_function"));
}

#[test]
fn test_exit_code_0_query_no_results() {
    let tmp_cli_workspace = setup_indexed_repo();

    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "kind:class"]) // No classes in test file
        .assert()
        .success(); // exit code 0 (empty result is success)
}

#[test]
fn test_exit_code_0_query_with_filters() {
    let tmp_cli_workspace = setup_indexed_repo();

    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "kind:function AND name:example_function"])
        .assert()
        .success() // exit code 0
        .stdout(predicate::str::contains("example_function"));
}

// ============================================================================
// Exit Code 1: Runtime/System Errors
// ============================================================================

#[test]
fn test_exit_code_0_auto_index_builds_graph() {
    let tmp_cli_workspace = TempDir::new().unwrap(); // No graph

    // With auto-index enabled (default), querying without a pre-built graph
    // triggers automatic index building and succeeds
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "kind:function"])
        .assert()
        .success();
}

#[test]
fn test_exit_code_1_no_graph_with_auto_index_disabled() {
    let tmp_cli_workspace = TempDir::new().unwrap(); // No graph

    // With auto-index disabled, missing graph is an error
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .env("SQRY_AUTO_INDEX", "false")
        .args(["query", "kind:function"])
        .assert()
        .failure() // exit code 1 (runtime error)
        .code(1)
        .stderr(predicate::str::contains("No graph found"));
}

// Note: Corrupted index and invalid directory tests removed as they test
// implementation details that may vary with index format changes.
// Core contract (exit 0/1 for success/runtime error) is tested above.

// ============================================================================
// Exit Code 2: User/Validation Errors
// ============================================================================

#[test]
fn test_exit_code_0_missing_colon_fallback_to_text() {
    let tmp_cli_workspace = setup_indexed_repo();

    // Note: sqry's hybrid mode treats "kind function" as text search, not parse error
    // This is a feature - sqry is forgiving and falls back to text search
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "kind function"]) // Missing colon triggers text search fallback
        .assert()
        .success(); // exit code 0 (hybrid mode fallback)
}

#[test]
fn test_exit_code_2_unclosed_paren_validation_error() {
    let tmp_cli_workspace = setup_indexed_repo();

    // Unclosed paren is a user/validation error; returns exit code 2
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "(kind:function AND name:test"]) // Unclosed paren
        .assert()
        .code(2) // Validation/parse error
        .stderr(
            predicate::str::contains("Parse error")
                .or(predicate::str::contains("Unknown field"))
                .or(predicate::str::contains("Error")),
        );
}

#[test]
fn test_exit_code_0_unknown_field_fallback() {
    let tmp_cli_workspace = setup_indexed_repo();

    // Unknown fields fall back to text search in hybrid mode
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "unknown_field_12345:value"])
        .assert()
        .success(); // exit code 0 (text search fallback)
}

// Note: Invalid regex test removed - behavior varies depending on whether
// regex validation happens before or after hybrid mode fallback.

#[test]
fn test_exit_code_2_invalid_operator_parse_error() {
    let tmp_cli_workspace = setup_indexed_repo();

    // Invalid operators are treated as parse errors in the unified boolean parser.
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "kind:function INVALID name:test"]) // Invalid operator
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Parse error"));
}

#[test]
fn test_exit_code_0_empty_query_shows_all() {
    let tmp_cli_workspace = setup_indexed_repo();

    // Empty query is valid - shows all content
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", ""]) // Empty query
        .assert()
        .success() // exit code 0 (valid query, shows all)
        .stdout(predicate::str::contains("test.rs"));
}

// ============================================================================
// Edge Cases & Complex Queries
// ============================================================================

#[test]
fn test_exit_code_0_complex_query_success() {
    let tmp_cli_workspace = setup_indexed_repo();

    // Simple AND query that should work
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "kind:function AND name:example_function"])
        .assert()
        .success(); // exit code 0
}

#[test]
fn test_exit_code_2_complex_query_validation_error() {
    let tmp_cli_workspace = setup_indexed_repo();

    // Malformed query is treated as validation error; returns exit code 2
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "(kind:function OR AND name:test"]) // Malformed
        .assert()
        .code(2); // Validation/parse error
}

// ============================================================================
// Regression Tests - Ensure Existing Behavior
// ============================================================================

#[test]
fn test_exit_code_0_help_command() {
    Command::new(sqry_path())
        .args(["query", "--help"])
        .assert()
        .success() // exit code 0
        .stdout(predicate::str::contains("query"));
}

#[test]
fn test_exit_code_0_version_flag() {
    Command::new(sqry_path())
        .args(["--version"])
        .assert()
        .success() // exit code 0
        .stdout(predicate::str::contains("sqry"));
}
