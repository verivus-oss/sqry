//! CLI integration tests for Groovy relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Groovy:
//! - Callers queries (method calls, closures) - ✅ via `GraphBuilder`
//! - Callees queries (what a method calls) - ✅ via `GraphBuilder`
//! - Exports queries (classes, methods, closures) - ✅ via `GraphBuilder`
//! - Imports queries (import statements) - ✅ via `GraphBuilder`

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Classes and Methods
// ============================================================================

#[test]
fn cli_groovy_exports_classes_and_methods() {
    let project = TempDir::new().unwrap();

    let groovy_code = r#"
package com.example

def greet(String name) {
    return "Hello, ${name}!"
}

class User {
    String name
    int age

    User(String name, int age) {
        this.name = name
        this.age = age
    }

    String getName() {
        return name
    }

    private void validate() {
        // private method
    }
}

def API_VERSION = "1.0.0"
"#;
    std::fs::write(project.path().join("Module.groovy"), groovy_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:greet")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Module.groovy"));

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Module.groovy"));

    // Private validate method should not be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:validate")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

// ============================================================================
// Callers Queries - Method Calls
// ============================================================================

#[test]
fn cli_groovy_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let groovy_code = r#"
def validate(String input) {
    return input != null && input.length() > 0
}

def process(String data) {
    if (validate(data)) {
        return data.trim()
    }
    return null
}

process("test")
"#;
    std::fs::write(project.path().join("Processor.groovy"), groovy_code).unwrap();

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
fn cli_groovy_callers_closures() {
    let project = TempDir::new().unwrap();

    let groovy_code = r"
def transform(int x) {
    return x * 2
}

def processData() {
    def data = [1, 2, 3]
    return data.collect { transform(it) }
}

processData()
";
    std::fs::write(project.path().join("Closures.groovy"), groovy_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of transform (called in closure)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:transform")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("processData"));
}

// ============================================================================
// Callees Queries - What Methods Call
// ============================================================================

#[test]
fn cli_groovy_callees_method() {
    let project = TempDir::new().unwrap();

    let groovy_code = r#"
def log(String message) {
    println(message)
}

def warn(String message) {
    println("WARNING: ${message}")
}

def handleError(Exception error) {
    log("Error occurred")
    warn(error.message)
}

handleError(new Exception("Test"))
"#;
    std::fs::write(project.path().join("Logger.groovy"), groovy_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of handleError
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:handleError")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("log"))
        .stdout(predicate::str::contains("warn"));
}

// ============================================================================
// Imports Queries
// ============================================================================

#[test]
fn cli_groovy_imports() {
    let project = TempDir::new().unwrap();

    let groovy_code = r#"
import java.io.File
import java.nio.file.Files

def readConfig() {
    def file = new File("config.txt")
    if (file.exists()) {
        return file.text
    }
    return ""
}

readConfig()
"#;
    std::fs::write(project.path().join("Config.groovy"), groovy_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:File")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Config.groovy"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_groovy_callers_no_results() {
    let project = TempDir::new().unwrap();

    let groovy_code = r#"
def unusedFunction() {
    return 42
}

println("Hello")
"#;
    std::fs::write(project.path().join("Unused.groovy"), groovy_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of unused function
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:unusedFunction")
        .arg(project.path())
        .assert()
        .success();
    // No specific assertion - just verify it doesn't crash
}
