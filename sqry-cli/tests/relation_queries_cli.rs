//! CLI integration tests for multi-language relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for:
//! - TypeScript
//! - JavaScript
//! - Python
//! - Go
//! - Java

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

#[test]
fn cli_typescript_callers_query() {
    let project = TempDir::new().unwrap();

    // Create TypeScript file with function calls
    let ts_code = r"
function helper() {
    return 42;
}

function fetch() {
    return helper();
}

function process() {
    helper();
}
";
    std::fs::write(project.path().join("test.ts"), ts_code).unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of 'helper'
    // Should return fetch and process, but NOT helper itself
    let path = sqry_bin();

    Command::new(&path)
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("process"))
        .stdout(predicate::str::contains("helper").not()); // Bug: currently fails this assertion
}

#[test]
fn cli_javascript_callers_query() {
    let project = TempDir::new().unwrap();

    let js_code = r"
function helper() {
    return 42;
}

function fetch() {
    return helper();
}
";
    std::fs::write(project.path().join("test.js"), js_code).unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers
    // Should return fetch, but NOT helper itself
    let path = sqry_bin();

    Command::new(&path)
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("helper").not());
}

#[test]
fn cli_python_callers_query() {
    let project = TempDir::new().unwrap();

    let py_code = r"
def helper():
    return 42

def fetch():
    return helper()

def process():
    helper()
";
    std::fs::write(project.path().join("test.py"), py_code).unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers
    // Should return fetch and process, but NOT helper itself
    let path = sqry_bin();

    Command::new(&path)
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("process"))
        .stdout(predicate::str::contains("helper").not());
}

#[test]
fn cli_go_callers_query() {
    let project = TempDir::new().unwrap();

    let go_code = r"
package main

func helper() int {
    return 42
}

func fetch() int {
    return helper()
}
";
    std::fs::write(project.path().join("test.go"), go_code).unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers
    // Should return fetch, but NOT helper itself
    let path = sqry_bin();

    Command::new(&path)
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("helper").not());
}

#[test]
fn cli_java_callers_query() {
    let project = TempDir::new().unwrap();

    let java_code = r"
public class Test {
    public static int helper() {
        return 42;
    }

    public static int fetch() {
        return helper();
    }
}
";
    std::fs::write(project.path().join("Test.java"), java_code).unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers
    // Should return fetch, but NOT helper itself
    let path = sqry_bin();

    Command::new(&path)
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("helper").not());
}

#[test]
fn cli_typescript_imports_query() {
    let project = TempDir::new().unwrap();

    let ts_code = r"
import { useState } from 'react';

const MyComponent = () => {
    const [state] = useState(0);
    return state;
};
";
    std::fs::write(project.path().join("component.ts"), ts_code).unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports
    let path = sqry_bin();

    Command::new(&path)
        .arg("query")
        .arg("imports:react")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("component.ts"));
}

#[test]
fn cli_python_imports_query() {
    let project = TempDir::new().unwrap();

    let py_code = r"
import os
import sys

def process():
    return os.getcwd()
";
    std::fs::write(project.path().join("module.py"), py_code).unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports
    let path = sqry_bin();

    Command::new(&path)
        .arg("query")
        .arg("imports:os")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.py"));
}

#[test]
fn cli_go_imports_query() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

import "fmt"

func main() {
    fmt.Println("Hello")
}
"#;
    std::fs::write(project.path().join("main.go"), go_code).unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports
    let path = sqry_bin();

    Command::new(&path)
        .arg("query")
        .arg("imports:fmt")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main.go"));
}

#[test]
fn cli_cross_language_queries() {
    let project = TempDir::new().unwrap();

    // Create files in multiple languages
    std::fs::write(
        project.path().join("helper.ts"),
        "export function helper() { return 42; }",
    )
    .unwrap();

    std::fs::write(
        project.path().join("utils.py"),
        "def calculate():\n    return 100",
    )
    .unwrap();

    std::fs::write(
        project.path().join("main.go"),
        "package main\n\nfunc process() int {\n    return 1\n}",
    )
    .unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for TypeScript functions
    let path = sqry_bin();

    Command::new(&path)
        .arg("query")
        .arg("name:helper AND lang:typescript")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("helper.ts"));

    // Query for Python functions
    let path = sqry_bin();

    Command::new(&path)
        .arg("query")
        .arg("name:calculate AND lang:python")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("utils.py"));

    // Query for Go functions
    let path = sqry_bin();

    Command::new(&path)
        .arg("query")
        .arg("name:process AND lang:go")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main.go"));
}
