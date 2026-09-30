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
fn test_exit_code_0_missing_colon_parses_as_implicit_and() {
    let tmp_cli_workspace = setup_indexed_repo();

    // "kind function" is a VALID query, not a tolerated invalid one: the
    // implicit-AND parser promotes both bare words to `name~=/.../` predicates.
    // It succeeded before this change too, but for the wrong stated reason (the
    // text fallback). It parses, so it stays exit 0 with the fallback gone.
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "kind function"])
        .assert()
        .success();
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
fn test_exit_code_2_unknown_field_is_an_error() {
    let tmp_cli_workspace = setup_indexed_repo();

    // Was `test_exit_code_0_unknown_field_fallback`, asserting exit 0 because an
    // unknown field silently became a text search. That is the defect this
    // change removes: the validator already produced a precise diagnostic and
    // it was discarded, so a typo and an honest empty result looked identical.
    // The reply names the command that does do a text search.
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "unknown_field_12345:value"])
        .assert()
        .code(2)
        .stderr(
            predicate::str::contains("Unknown field").and(predicate::str::contains("sqry search")),
        );
}

#[test]
fn test_exit_code_2_predicate_never_silently_text_searches() {
    let tmp_cli_workspace = setup_indexed_repo();

    // A query is structural if and only if it parses and validates. Nothing in
    // the query text switches engines any more, so a genuine regex that the
    // grammar cannot express is an error that names `sqry search`, not a
    // silent change of engine.
    for query in ["^fn ", "\\bmain\\b", "TODO: fix this"] {
        Command::new(sqry_path())
            .current_dir(&tmp_cli_workspace)
            .args(["query", query])
            .assert()
            .code(2)
            .stderr(predicate::str::contains("sqry search"));
    }
}

// Note: Invalid regex test removed - behavior varies depending on whether
// regex validation happens before or after hybrid mode fallback.

#[test]
fn test_exit_code_2_invalid_operator_parse_error() {
    let tmp_cli_workspace = setup_indexed_repo();

    // With the implicit-AND parser (PARSE_1), bare words like "INVALID" are now
    // promoted to `name~=/INVALID/` predicates rather than rejected as parse errors.
    // `kind:function INVALID name:test` now parses as:
    //   And([Condition(kind=function), Condition(name~=/INVALID/), Condition(name=test)])
    // which is a valid query that returns 0 results (exit code 0, not 2).
    //
    // Genuine parse errors still produce exit code 2 — e.g. `kind: :`.
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "kind:function INVALID name:test"]) // Bare word — valid with implicit AND
        .assert()
        .success(); // exit code 0 (valid query, 0 results)

    // Genuinely invalid syntax still produces exit code 2.
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", "kind: :"])
        .assert()
        .code(2);
}

#[test]
fn test_exit_code_2_empty_query_is_rejected_by_the_parser() {
    let tmp_cli_workspace = setup_indexed_repo();

    // Was `test_exit_code_0_empty_query_shows_all`. The parser has always
    // rejected an empty query, with a message naming what to write instead;
    // the old exit 0 came from the text fallback overriding that verdict and
    // dumping the corpus. The CLI second-guessing its own parser is the defect
    // this change removes, so the parser's answer stands.
    Command::new(sqry_path())
        .current_dir(&tmp_cli_workspace)
        .args(["query", ""])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Query cannot be empty"));
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
