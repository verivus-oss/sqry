//! CLI integration tests for Shell relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Shell/Bash:
//! - Callers queries (function calls, command substitutions)
//! - Callees queries (what a function calls)
//! - Exports queries (function definitions, export commands)
//!
//! This validates the Shell relation extraction implementation.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Callers Queries - Function Calls
// ============================================================================

#[test]
fn cli_shell_callers_simple_function() {
    let project = TempDir::new().unwrap();

    let shell_code = r#"#!/bin/bash

helper() {
    echo "helper called"
}

main() {
    helper
    echo "done"
}

main
"#;
    std::fs::write(project.path().join("script.sh"), shell_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of helper function
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main"));
}

#[test]
fn cli_shell_callers_nested_function() {
    let project = TempDir::new().unwrap();

    let shell_code = r#"#!/bin/bash

validate() {
    return 0
}

process() {
    validate
    echo "processing"
}

run() {
    process
}
"#;
    std::fs::write(project.path().join("nested.sh"), shell_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of validate
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));
}

#[test]
fn cli_shell_callers_command_substitution() {
    let project = TempDir::new().unwrap();

    let shell_code = r#"#!/bin/bash

get_value() {
    echo "42"
}

compute() {
    local result=$(get_value)
    echo "$result"
}
"#;
    std::fs::write(project.path().join("subst.sh"), shell_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of get_value
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:get_value")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("compute"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_shell_callees_function() {
    let project = TempDir::new().unwrap();

    let shell_code = r#"#!/bin/bash

log() {
    echo "$1"
}

validate() {
    return 0
}

process() {
    log "starting"
    validate
    log "done"
}
"#;
    std::fs::write(project.path().join("callees.sh"), shell_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for what process calls
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:process")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("log").and(predicate::str::contains("validate")));
}

// ============================================================================
// Integration Tests - Complex Call Patterns
// ============================================================================

#[test]
fn cli_shell_multiple_callers() {
    let project = TempDir::new().unwrap();

    let shell_code = r#"#!/bin/bash

log() {
    echo "$1"
}

process() {
    log "processing"
}

main() {
    log "starting"
    process
}
"#;
    std::fs::write(project.path().join("multiple.sh"), shell_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of log - should find both process and main
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:log")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process").and(predicate::str::contains("main")));
}

// ============================================================================
// Exports Queries
// ============================================================================

#[test]
fn cli_shell_exports_function_definitions() {
    let project = TempDir::new().unwrap();

    let shell_code = r#"#!/bin/bash

# User-defined function
my_function() {
    echo "exported function"
}

# Another function
helper() {
    return 0
}
"#;
    std::fs::write(project.path().join("functions.sh"), shell_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:my_function")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("my_function"));
}

#[test]
fn cli_shell_exports_exclude_builtins() {
    let project = TempDir::new().unwrap();

    let shell_code = r#"#!/bin/bash

my_script() {
    echo "user function"
    cd /tmp
    ls -la
}
"#;
    std::fs::write(project.path().join("script.sh"), shell_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // User-defined function should be found
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:my_script")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("my_script"));

    // Built-in commands should NOT be in exports
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:echo")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found").or(predicate::str::is_empty()));
}
