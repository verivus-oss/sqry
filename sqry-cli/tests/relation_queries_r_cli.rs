//! CLI integration tests for R relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for R:
//! - Callers queries (function calls, method calls)
//! - Callees queries (what a function calls)
//! - Imports queries (library(), require(), package::function)
//!
//! Note: Exports in R are handled via NAMESPACE files which don't have file
//! extensions and aren't currently indexed. Export tracking is a future enhancement.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Imports Queries - library() and require()
// ============================================================================

#[test]
fn cli_r_imports() {
    let project = TempDir::new().unwrap();

    let r_code = r#"
library(dplyr)
library(ggplot2)
require(tidyr)

process_data <- function(df) {
  df %>%
    filter(value > 0) %>%
    select(id, value)
}
"#;
    std::fs::write(project.path().join("analysis.R"), r_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for library import
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:dplyr")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("analysis.R"));

    // Query for require import
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:tidyr")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("analysis.R"));
}

// ============================================================================
// Callers Queries - Function Calls
// ============================================================================

#[test]
fn cli_r_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let r_code = r#"
validate <- function(x) {
  !is.null(x) && length(x) > 0
}

process <- function(data) {
  if (validate(data)) {
    return(mean(data))
  }
  return(NULL)
}

analyze <- function(values) {
  if (validate(values)) {
    return(sum(values))
  }
  return(0)
}
"#;
    std::fs::write(project.path().join("functions.R"), r_code).unwrap();

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
        .stdout(predicate::str::contains("process"))
        .stdout(predicate::str::contains("analyze"));
}

#[test]
fn cli_r_callers_with_namespace() {
    let project = TempDir::new().unwrap();

    let r_code = r#"
calculate <- function(x) {
  dplyr::mutate(x, new_col = value * 2)
}

transform <- function(df) {
  dplyr::filter(df, value > 0)
}
"#;
    std::fs::write(project.path().join("transform.R"), r_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of mutate (should find calculate)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:mutate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("calculate"));

    // Query for callers of filter (should find transform)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:filter")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("transform"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_r_callees_function() {
    let project = TempDir::new().unwrap();

    let r_code = r#"
log_message <- function(msg) {
  print(paste("LOG:", msg))
}

warn_message <- function(msg) {
  warning(paste("WARN:", msg))
}

handle_error <- function(error) {
  log_message("Error occurred")
  warn_message(error$message)
}
"#;
    std::fs::write(project.path().join("logger.R"), r_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of handle_error
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:handle_error")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("log_message"))
        .stdout(predicate::str::contains("warn_message"));
}

// ============================================================================
// Method Calls ($ operator)
// ============================================================================

#[test]
fn cli_r_method_calls() {
    let project = TempDir::new().unwrap();

    let r_code = r#"
User <- R6::R6Class("User",
  public = list(
    name = NULL,
    get_name = function() {
      self$name
    },
    validate = function() {
      !is.null(self$name)
    }
  )
)

process_user <- function(user) {
  if (user$validate()) {
    return(user$get_name())
  }
  return(NULL)
}
"#;
    std::fs::write(project.path().join("user.R"), r_code).unwrap();

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
        .stdout(predicate::str::contains("process_user"));

    // Query for callers of get_name
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:get_name")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process_user"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_r_callers_no_results() {
    let project = TempDir::new().unwrap();

    let r_code = r#"
unused_function <- function() {
  return(42)
}

print("Hello")
"#;
    std::fs::write(project.path().join("unused.R"), r_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of unused function
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:unused_function")
        .arg(project.path())
        .assert()
        .success();
    // No specific assertion - just verify it doesn't crash
}
