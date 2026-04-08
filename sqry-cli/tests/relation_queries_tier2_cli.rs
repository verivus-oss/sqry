//! CLI integration tests for Tier-2 language relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Tier-2 languages:
//! - Rust, C++, C#, Kotlin, Groovy, Svelte, Vue, Ruby
//!
//! These languages have complete relation extraction via their `GraphBuilder`
//! implementations but lack dedicated CLI test coverage.
//!
//! See docs/internal/LANGUAGE_SUPPORT_AUDIT_2025-11-10.md for methodology.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Rust - Tier 2
// ============================================================================

#[test]
fn cli_rust_callers_query() {
    let project = TempDir::new().unwrap();

    let rust_code = r"
fn helper() -> i32 {
    42
}

fn fetch() -> i32 {
    helper()
}

fn process() {
    let _ = helper();
}
";
    std::fs::write(project.path().join("test.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("process"));
}

#[test]
fn cli_rust_imports_query() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
use std::collections::HashMap;
use std::fs::File;

fn main() {
    let map: HashMap<String, i32> = HashMap::new();
    let _ = File::open("test.txt");
}
"#;
    std::fs::write(project.path().join("main.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:std")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main.rs"));
}

// ============================================================================
// C++ - Tier 2
// ============================================================================

#[test]
fn cli_cpp_callers_query() {
    let project = TempDir::new().unwrap();

    let cpp_code = r"
int helper() {
    return 42;
}

int fetch() {
    return helper();
}

void process() {
    helper();
}
";
    std::fs::write(project.path().join("test.cpp"), cpp_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("process"));
}

// ============================================================================
// C# - Tier 2
// ============================================================================

#[test]
fn cli_csharp_callers_query() {
    let project = TempDir::new().unwrap();

    let csharp_code = r"
using System;

class Test {
    static int Helper() {
        return 42;
    }

    static int Fetch() {
        return Helper();
    }

    static void Process() {
        Helper();
    }
}
";
    std::fs::write(project.path().join("Test.cs"), csharp_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Fetch"))
        .stdout(predicate::str::contains("Process"));
}

// ============================================================================
// Kotlin - Tier 2
// ============================================================================

#[test]
fn cli_kotlin_callers_query() {
    let project = TempDir::new().unwrap();

    let kotlin_code = r"
fun helper(): Int {
    return 42
}

fun fetch(): Int {
    return helper()
}

fun process() {
    helper()
}
";
    std::fs::write(project.path().join("test.kt"), kotlin_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("process"));
}

// ============================================================================
// Groovy - Tier 2
// ============================================================================

#[test]
fn cli_groovy_callers_query() {
    let project = TempDir::new().unwrap();

    let groovy_code = r"
def helper() {
    return 42
}

def fetch() {
    return helper()
}

def process() {
    helper()
}
";
    std::fs::write(project.path().join("test.groovy"), groovy_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("process"));
}

// ============================================================================
// Svelte - Tier 2
// ============================================================================

#[test]
fn cli_svelte_callers_query() {
    let project = TempDir::new().unwrap();

    let svelte_code = r"
<script>
function helper() {
    return 42;
}

function fetch() {
    return helper();
}

function process() {
    helper();
}
</script>

<div>Test</div>
";
    std::fs::write(project.path().join("Test.svelte"), svelte_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("process"));
}

// ============================================================================
// Vue - Tier 2
// ============================================================================

#[test]
fn cli_vue_callers_query() {
    let project = TempDir::new().unwrap();

    let vue_code = r"
<script>
function helper() {
    return 42;
}

function fetch() {
    return helper();
}

function process() {
    helper();
}
</script>

<template>
  <div>Test</div>
</template>
";
    std::fs::write(project.path().join("Test.vue"), vue_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch"))
        .stdout(predicate::str::contains("process"));
}

// ============================================================================
// Ruby - Tier 2
// ============================================================================

#[test]
fn cli_ruby_callers_query() {
    let project = TempDir::new().unwrap();

    let ruby_code = r"
class Calculator
  def add(a, b)
    sum(a, b)
  end

  def multiply(x, y)
    product(x, y)
  end

  def compute
    add(5, 10)
    multiply(2, 3)
  end

  private

  def sum(a, b)
    a + b
  end

  def product(x, y)
    x * y
  end
end
";
    std::fs::write(project.path().join("calculator.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of 'add' method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:add")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("compute"));
}

#[test]
fn cli_ruby_exports_query() {
    let project = TempDir::new().unwrap();

    let ruby_code = r#"
class User
  def public_method
    "public"
  end

  private

  def private_method
    "private"
  end
end

module Helpers
  def helper_method
    "help"
  end
end
"#;
    std::fs::write(project.path().join("models.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exports - should find User class and public_method, but not private_method
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("models.rb"));

    // Verify public_method is exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_method")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("models.rb"));
}

#[test]
fn cli_ruby_imports_query() {
    let project = TempDir::new().unwrap();

    let ruby_code = r"
require 'json'
require_relative 'helpers'

class Parser
  include JSONable

  def parse(data)
    JSON.parse(data)
  end
end
";
    std::fs::write(project.path().join("parser.rb"), ruby_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports - should find json require
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:json")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("parser.rb"));
}
