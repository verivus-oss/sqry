//! CLI integration tests for Svelte relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Svelte:
//! - Callers queries (function calls in script section)
//! - Callees queries (what a function calls)
//! - Exports queries (exported functions, components)
//! - Imports queries (import statements in script section)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Exported Functions and Components
// ============================================================================

#[test]
fn cli_svelte_exports_functions() {
    let project = TempDir::new().unwrap();

    let svelte_code = r#"
<script>
export function greet(name) {
    return `Hello, ${name}!`;
}

function privateHelper() {
    return 42;
}

export class User {
    constructor(name, age) {
        this.name = name;
        this.age = age;
    }

    getName() {
        return this.name;
    }
}

export const API_VERSION = "1.0.0";
</script>

<div>
    <h1>Welcome</h1>
</div>
"#;
    std::fs::write(project.path().join("Module.svelte"), svelte_code).unwrap();

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
        .stdout(predicate::str::contains("Module.svelte"));

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Module.svelte"));

    // privateHelper should not be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:privateHelper")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

// ============================================================================
// Callers Queries - Function Calls in Script
// ============================================================================

#[test]
fn cli_svelte_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let svelte_code = r#"
<script>
function validate(input) {
    return input.length > 0;
}

function process(data) {
    if (validate(data)) {
        return data.trim();
    }
    return null;
}

const result = process("test");
</script>

<div>{result}</div>
"#;
    std::fs::write(project.path().join("Processor.svelte"), svelte_code).unwrap();

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
fn cli_svelte_callers_reactive_statements() {
    let project = TempDir::new().unwrap();

    let svelte_code = r#"
<script>
let count = 0;

function increment() {
    count++;
}

function handleClick() {
    increment();
}
</script>

<button on:click={handleClick}>
    Count: {count}
</button>
"#;
    std::fs::write(project.path().join("Counter.svelte"), svelte_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of increment
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:increment")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("handleClick"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_svelte_callees_function() {
    let project = TempDir::new().unwrap();

    let svelte_code = r#"
<script>
function log(message) {
    console.log(message);
}

function warn(message) {
    console.warn(message);
}

function handleError(error) {
    log("Error occurred");
    warn(error.message);
}

handleError(new Error("Test"));
</script>
"#;
    std::fs::write(project.path().join("Logger.svelte"), svelte_code).unwrap();

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
// Imports Queries - Import Statements
// ============================================================================

#[test]
fn cli_svelte_imports() {
    let project = TempDir::new().unwrap();

    let svelte_code = r#"
<script>
import { greet } from './utils';
import UserCard from './UserCard.svelte';
import { createStore } from './store';

const message = greet('World');
const store = createStore();
</script>

<div>
    <p>{message}</p>
    <UserCard />
</div>
"#;
    std::fs::write(project.path().join("App.svelte"), svelte_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports (imports:X matches module names, not symbol names)
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:utils")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("App.svelte"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:UserCard")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("App.svelte"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_svelte_callers_no_results() {
    let project = TempDir::new().unwrap();

    let svelte_code = r#"
<script>
function unusedFunction() {
    return 42;
}

console.log("Hello");
</script>
"#;
    std::fs::write(project.path().join("Unused.svelte"), svelte_code).unwrap();

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
