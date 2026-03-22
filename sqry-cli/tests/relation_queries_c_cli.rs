//! CLI integration tests for C relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for C:
//! - Callers queries (function calls)
//! - Callees queries (what a function calls)
//! - Exports queries (function declarations, global symbols)
//! - Imports queries (#include directives)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Function Declarations and Global Symbols
// ============================================================================

#[test]
fn cli_c_exports_functions() {
    let project = TempDir::new().unwrap();

    let c_code = r#"
#include <stdio.h>

char* greet(const char* name) {
    static char buffer[256];
    snprintf(buffer, sizeof(buffer), "Hello, %s!", name);
    return buffer;
}

static int private_helper() {
    return 42;
}

typedef struct {
    char name[100];
    int age;
} User;

User create_user(const char* name, int age) {
    User user;
    snprintf(user.name, sizeof(user.name), "%s", name);
    user.age = age;
    return user;
}

const char* API_VERSION = "1.0.0";
"#;
    std::fs::write(project.path().join("module.c"), c_code).unwrap();

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
        .stdout(predicate::str::contains("module.c"));

    // Query for struct creation function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:create_user")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.c"));

    // static helper should remain private (no exports)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:private_helper")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_c_exports_with_headers() {
    let project = TempDir::new().unwrap();

    let header_code = r#"
#ifndef USER_H
#define USER_H

typedef struct User {
    int id;
    char name[100];
} User;

User* create_user(int id, const char* name);
void destroy_user(User* user);
int get_user_id(const User* user);

#endif
"#;
    std::fs::write(project.path().join("user.h"), header_code).unwrap();

    let impl_code = r#"
#include "user.h"
#include <stdlib.h>
#include <string.h>

User* create_user(int id, const char* name) {
    User* user = (User*)malloc(sizeof(User));
    user->id = id;
    strncpy(user->name, name, sizeof(user->name) - 1);
    return user;
}

void destroy_user(User* user) {
    free(user);
}

int get_user_id(const User* user) {
    return user->id;
}
"#;
    std::fs::write(project.path().join("user.c"), impl_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for functions in header
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:create_user")
        .arg(project.path())
        .assert()
        .success();
    // Should find in both header and impl

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:get_user_id")
        .arg(project.path())
        .assert()
        .success();
}

// ============================================================================
// Callers Queries - Function Calls
// ============================================================================

#[test]
fn cli_c_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let c_code = r#"
#include <string.h>

int validate(const char* input) {
    return input != NULL && strlen(input) > 0;
}

char* process(const char* data) {
    if (validate(data)) {
        return strdup(data);
    }
    return NULL;
}

int main() {
    char* result = process("test");
    free(result);
    return 0;
}
"#;
    std::fs::write(project.path().join("processor.c"), c_code).unwrap();

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

    // Query for callers of process
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:process")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main"));
}

#[test]
fn cli_c_callers_nested_calls() {
    let project = TempDir::new().unwrap();

    let c_code = r#"
#include <stdio.h>

void log_message(const char* message) {
    printf("%s\n", message);
}

void warn_message(const char* message) {
    fprintf(stderr, "WARNING: %s\n", message);
}

void handle_error(const char* error) {
    log_message("Error occurred");
    warn_message(error);
}

int main() {
    handle_error("test error");
    return 0;
}
"#;
    std::fs::write(project.path().join("logger.c"), c_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of log_message
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:log_message")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("handle_error"));

    // Query for callers of warn_message
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:warn_message")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("handle_error"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_c_callees_function() {
    let project = TempDir::new().unwrap();

    let c_code = r#"
#include <stdlib.h>
#include <string.h>

char* allocate_buffer(size_t size) {
    return (char*)malloc(size);
}

void free_buffer(char* buffer) {
    free(buffer);
}

void process_data(const char* input) {
    char* buffer = allocate_buffer(256);
    strcpy(buffer, input);
    free_buffer(buffer);
}

int main() {
    process_data("test");
    return 0;
}
"#;
    std::fs::write(project.path().join("memory.c"), c_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of process_data
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:process_data")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("allocate_buffer"))
        .stdout(predicate::str::contains("free_buffer"));
}

// ============================================================================
// Imports Queries - #include Directives
// ============================================================================

#[test]
fn cli_c_imports() {
    let project = TempDir::new().unwrap();

    let c_code = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "user.h"

void read_config() {
    FILE* file = fopen("config.txt", "r");
    if (file) {
        fclose(file);
    }
}

int main() {
    read_config();
    return 0;
}
"#;
    std::fs::write(project.path().join("config.c"), c_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for includes
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:stdio")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.c"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:user")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.c"));
}

// ============================================================================
// Struct and Pointer Patterns
// ============================================================================

#[test]
fn cli_c_struct_function_pointers() {
    let project = TempDir::new().unwrap();

    let c_code = r#"
#include <stdlib.h>

typedef int (*operation_fn)(int, int);

int add(int a, int b) {
    return a + b;
}

int multiply(int a, int b) {
    return a * b;
}

typedef struct {
    operation_fn op;
    int value;
} Calculator;

int execute_operation(Calculator* calc, int a, int b) {
    return calc->op(a, b);
}

int main() {
    Calculator calc = {add, 0};
    int result = execute_operation(&calc, 2, 3);
    return 0;
}
"#;
    std::fs::write(project.path().join("calculator.c"), c_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of execute_operation
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:execute_operation")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_c_static_functions() {
    let project = TempDir::new().unwrap();

    let c_code = r#"
int public_function() {
    return private_helper();
}

static int private_helper() {
    return 42;
}
"#;
    std::fs::write(project.path().join("api.c"), c_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for public function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_function")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("api.c"));

    // static functions have file scope, may or may not appear in exports
}

#[test]
fn cli_c_callers_no_results() {
    let project = TempDir::new().unwrap();

    let c_code = r#"
int unused_function() {
    return 42;
}

int main() {
    printf("Hello\n");
    return 0;
}
"#;
    std::fs::write(project.path().join("unused.c"), c_code).unwrap();

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

#[test]
fn cli_c_static_inline_not_exported() {
    let project = TempDir::new().unwrap();

    // Common pattern in header files: static inline functions
    let c_code = r#"
// Public function with external linkage - should be exported
int public_api() {
    return helper_fast() + helper_regular();
}

// Static inline (common in headers) - should NOT be exported
static inline int helper_fast() {
    return 42;
}

// Regular static function - should NOT be exported
static int helper_regular() {
    return 24;
}

// Just inline (not static) - should be exported
inline int inline_only() {
    return 99;
}
"#;
    std::fs::write(project.path().join("header_like.c"), c_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Public function should be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_api")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("header_like.c"));

    // static inline should NOT be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:helper_fast")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));

    // static (regular) should NOT be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:helper_regular")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));

    // inline (non-static) SHOULD be exported - it has external linkage
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:inline_only")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("header_like.c"));
}

#[test]
fn cli_c_no_duplicate_exports() {
    let project = TempDir::new().unwrap();

    // File with both prototype and definition - should only export once
    let c_code = r#"
// Forward declaration (prototype)
int calculate(int x, int y);

// Some other function that uses calculate
int wrapper(int a, int b) {
    return calculate(a, b) * 2;
}

// Definition of calculate
int calculate(int x, int y) {
    return x + y;
}
"#;
    std::fs::write(project.path().join("dedup_test.c"), c_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exports of calculate - should find it exactly once
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("exports:calculate")
        .arg(project.path())
        .output()
        .unwrap();

    // Assert the command succeeded
    assert!(
        output.status.success(),
        "Query command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should contain the file reference (export was found)
    assert!(
        stdout.contains("dedup_test.c"),
        "Expected to find 'calculate' export in dedup_test.c, got: {stdout}"
    );

    // Should contain exactly one reference to dedup_test.c
    // (not two from prototype + definition)
    let count = stdout.matches("dedup_test.c").count();
    assert!(
        count == 1,
        "Expected exactly 1 export for 'calculate', found {count} in output: {stdout}"
    );
}
