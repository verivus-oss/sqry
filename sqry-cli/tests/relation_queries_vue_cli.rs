//! CLI integration tests for Vue relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Vue:
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
fn cli_vue_exports_functions() {
    let project = TempDir::new().unwrap();

    let vue_code = r#"
<template>
  <div>
    <h1>Welcome</h1>
  </div>
</template>

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
"#;
    std::fs::write(project.path().join("Module.vue"), vue_code).unwrap();

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
        .stdout(predicate::str::contains("Module.vue"));

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Module.vue"));

    // Private helper should remain hidden from exports
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
fn cli_vue_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let vue_code = r#"
<template>
  <div>{{ result }}</div>
</template>

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

export default {
    data() {
        return {
            result: process("test")
        };
    }
};
</script>
"#;
    std::fs::write(project.path().join("Processor.vue"), vue_code).unwrap();

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
fn cli_vue_callers_composition_api() {
    let project = TempDir::new().unwrap();

    let vue_code = r#"
<template>
  <button @click="handleClick">Count: {{ count }}</button>
</template>

<script setup>
import { ref } from 'vue';

const count = ref(0);

function increment() {
    count.value++;
}

function handleClick() {
    increment();
}
</script>
"#;
    std::fs::write(project.path().join("Counter.vue"), vue_code).unwrap();

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
fn cli_vue_callees_function() {
    let project = TempDir::new().unwrap();

    let vue_code = r#"
<template>
  <div>Logger</div>
</template>

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

export default {
    mounted() {
        handleError(new Error("Test"));
    }
};
</script>
"#;
    std::fs::write(project.path().join("Logger.vue"), vue_code).unwrap();

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
fn cli_vue_imports() {
    let project = TempDir::new().unwrap();

    let vue_code = r#"
<template>
  <div>
    <p>{{ message }}</p>
    <UserCard />
  </div>
</template>

<script>
import { greet } from './utils';
import UserCard from './UserCard.vue';
import { useStore } from './store';

export default {
    components: {
        UserCard
    },
    setup() {
        const message = greet('World');
        const store = useStore();

        return { message };
    }
};
</script>
"#;
    std::fs::write(project.path().join("App.vue"), vue_code).unwrap();

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
        .stdout(predicate::str::contains("App.vue"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:UserCard")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("App.vue"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_vue_callers_no_results() {
    let project = TempDir::new().unwrap();

    let vue_code = r#"
<template>
  <div>Unused</div>
</template>

<script>
function unusedFunction() {
    return 42;
}

export default {
    mounted() {
        console.log("Hello");
    }
};
</script>
"#;
    std::fs::write(project.path().join("Unused.vue"), vue_code).unwrap();

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
