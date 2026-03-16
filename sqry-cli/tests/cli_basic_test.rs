//! Basic CLI integration tests

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;

use sqry_core::test_support::verbosity;
use std::path::PathBuf;
use std::sync::Once;

// Initialize verbose logging once for all tests in this file
static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

/// Get the path to the test fixture directory
fn fixture_path() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    // CARGO_MANIFEST_DIR is sqry-cli, parent is sqry repo root
    PathBuf::from(manifest_dir)
        .parent()
        .unwrap()
        .join("test-fixtures/cli-basic")
}

#[test]
fn test_version_flag() {
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("sqry"));
}

#[test]
fn test_help_flag() {
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage:"));
}

#[test]
fn test_query_help() {
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.arg("query")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("query"));
}

#[test]
fn test_simple_query() {
    init_logging();
    log::info!("Testing simple CLI query execution (kind:function)");

    let path = sqry_bin();
    let fixture = fixture_path();
    let mut cmd = Command::new(path);
    cmd.arg("query")
        .arg("kind:function")
        .arg(&fixture)
        .assert()
        .success();

    log::info!("✓ Simple query completed successfully");
}

#[test]
fn test_query_with_explain() {
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.arg("query")
        .arg("kind:function")
        .arg(".")
        .arg("--explain")
        .assert()
        .success()
        .stderr(predicate::str::contains("Query Plan:"));
}

#[test]
fn test_query_with_verbose() {
    let path = sqry_bin();
    let fixture = fixture_path();
    let mut cmd = Command::new(path);
    cmd.arg("query")
        .arg("kind:function")
        .arg(&fixture)
        .arg("-v")
        .assert()
        .success()
        // Note: Hybrid mode is now the default, cache stats not yet implemented
        // Test just verifies that verbose flag works without errors
        .stderr(predicate::str::contains("Query executed"));
}

#[test]
fn test_query_syntax_error_exit_code() {
    init_logging();
    log::info!("Testing query syntax error handling (exit code 2)");

    // Test that syntax errors exit with error code 2 (user input errors)
    // Parse errors exit with code 2 as defined by QueryError::exit_code()
    // NOTE: Must use --semantic flag because hybrid mode falls back to text search
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.arg("--semantic") // Force semantic-only mode to get parse errors
        .arg("query")
        .arg("kind:") // Incomplete predicate - actual syntax error
        .arg(".")
        .assert()
        .failure()
        .code(2); // Parse errors return exit code 2 (user input errors)

    log::info!("✓ Syntax error correctly returned exit code 2");
}

#[test]
fn test_query_invalid_predicate_exit_code() {
    // Test that invalid predicates exit with error code
    // Validation errors should exit with code 2 (user/validation errors)
    // NOTE: Must use --semantic flag because hybrid mode falls back to text search
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.arg("--semantic") // Force semantic-only mode to get validation errors
        .arg("query")
        .arg("unknown_field:value")
        .arg(".")
        .assert()
        .failure()
        .code(2); // Validation errors return exit code 2
}

#[test]
fn test_query_empty_value_exit_code() {
    // Test that empty values exit with error code 2 (user input errors)
    // Empty values (parse errors) exit with code 2 as defined by QueryError::exit_code()
    // NOTE: Must use --semantic flag because hybrid mode falls back to text search
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.arg("--semantic") // Force semantic-only mode to get parse errors
        .arg("query")
        .arg("kind:")
        .arg(".")
        .assert()
        .failure()
        .code(2); // Parse errors return exit code 2 (user input errors)
}

#[test]
fn test_successful_query_exit_code_zero() {
    init_logging();
    log::info!("Testing successful query returns exit code 0");

    // Test that successful queries exit with code 0
    let path = sqry_bin();
    let fixture = fixture_path();
    let mut cmd = Command::new(path);
    cmd.arg("query")
        .arg("kind:function")
        .arg(&fixture)
        .assert()
        .success()
        .code(0);

    log::info!("✓ Successful query correctly returned exit code 0");
}
