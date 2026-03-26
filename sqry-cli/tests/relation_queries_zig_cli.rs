//! CLI integration tests for Zig relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Zig:
//! - Callers queries (function calls, method calls)
//! - Callees queries (what a function calls)
//! - Exports queries (pub functions, pub types, pub constants)
//! - Imports queries (@import statements)
//!
//! This validates the Zig relation extraction implementation.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - pub Functions, Types, Constants
// ============================================================================

#[test]
fn cli_zig_exports_functions_and_types() {
    let project = TempDir::new().unwrap();

    let zig_code = r#"
const std = @import("std");

pub fn add(a: i32, b: i32) i32 {
    return a + b;
}

fn privateHelper() i32 {
    return 42;
}

pub const Point = struct {
    x: f32,
    y: f32,

    pub fn distance(self: Point) f32 {
        return @sqrt(self.x * self.x + self.y * self.y);
    }
};

const PrivateType = struct {
    value: i32,
};

pub const API_VERSION = "1.0.0";
"#;
    std::fs::write(project.path().join("module.zig"), zig_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:add")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.zig"));

    // Query for exported type
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Point")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.zig"));

    // Query for exported constant
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:API_VERSION")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.zig"));
}

#[test]
fn cli_zig_exports_hide_private() {
    let project = TempDir::new().unwrap();

    let zig_code = r#"
const std = @import("std");

pub fn publicFunction() i32 {
    return privateHelper();
}

fn privateHelper() i32 {
    return 42;
}

pub const PublicType = struct {
    value: i32,
};

const PrivateType = struct {
    value: i32,
};
"#;
    std::fs::write(project.path().join("visibility.zig"), zig_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Public function should be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:publicFunction")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("publicFunction"));

    // Public type should be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:PublicType")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("PublicType"));

    // Private function should NOT be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:privateHelper")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));

    // Private type should NOT be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:PrivateType")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_zig_exports_nested_pub_members() {
    let project = TempDir::new().unwrap();

    let zig_code = r#"
const PrivateContainer = struct {
    pub fn publicMethod() i32 {
        return 42;
    }

    pub const PUBLIC_CONST: i32 = 100;
};

pub const PublicContainer = struct {
    fn privateMethod() i32 {
        return 42;
    }
};
"#;
    std::fs::write(project.path().join("nested.zig"), zig_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Private container should NOT be exported, even with pub members inside
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:PrivateContainer")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));

    // Public container SHOULD be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:PublicContainer")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("PublicContainer"));
}

// ============================================================================
// Imports Queries - @import Statements
// ============================================================================

#[test]
fn cli_zig_imports() {
    let project = TempDir::new().unwrap();

    let zig_code = r#"
const std = @import("std");
const builtin = @import("builtin");

pub fn main() void {
    std.debug.print("Hello, Zig!\n", .{});
}
"#;
    std::fs::write(project.path().join("main.zig"), zig_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for std import
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:std")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main.zig"));

    // Query for builtin import
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:builtin")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main.zig"));
}

// ============================================================================
// Callers Queries - Function Calls
// ============================================================================

#[test]
fn cli_zig_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let zig_code = r#"
const std = @import("std");

fn validate(x: i32) bool {
    return x > 0;
}

fn process(value: i32) i32 {
    if (validate(value)) {
        return value * 2;
    }
    return 0;
}

fn analyze(num: i32) bool {
    return validate(num);
}
"#;
    std::fs::write(project.path().join("functions.zig"), zig_code).unwrap();

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
fn cli_zig_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let zig_code = r#"
const Point = struct {
    x: f32,
    y: f32,

    fn distance(self: Point) f32 {
        return @sqrt(self.x * self.x + self.y * self.y);
    }

    fn normalize(self: *Point) void {
        const d = self.distance();
        if (d > 0) {
            self.x /= d;
            self.y /= d;
        }
    }
};
"#;
    std::fs::write(project.path().join("methods.zig"), zig_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of distance
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:distance")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("normalize"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_zig_callees_function() {
    let project = TempDir::new().unwrap();

    let zig_code = r#"
const std = @import("std");

fn log(message: []const u8) void {
    std.debug.print("{s}\n", .{message});
}

fn warn(message: []const u8) void {
    std.debug.print("WARNING: {s}\n", .{message});
}

fn handleError(error_msg: []const u8) void {
    log("Error occurred");
    warn(error_msg);
}
"#;
    std::fs::write(project.path().join("logger.zig"), zig_code).unwrap();

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
// Integration Tests - Complex Scenarios
// ============================================================================

#[test]
fn cli_zig_multi_file_calls() {
    let project = TempDir::new().unwrap();

    let utils_code = r#"
const std = @import("std");

pub fn log(message: []const u8) void {
    std.debug.print("[LOG] {s}\n", .{message});
}

pub fn validate(input: i32) bool {
    return input > 0;
}
"#;
    std::fs::write(project.path().join("utils.zig"), utils_code).unwrap();

    let main_code = r#"
const utils = @import("utils.zig");

pub fn process(data: i32) void {
    utils.log("processing");
    _ = utils.validate(data);
}
"#;
    std::fs::write(project.path().join("main.zig"), main_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of log
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:log")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_zig_callers_no_results() {
    let project = TempDir::new().unwrap();

    let zig_code = r#"
fn unusedFunction() i32 {
    return 42;
}

pub fn main() void {
    const value: i32 = 10;
    _ = value;
}
"#;
    std::fs::write(project.path().join("unused.zig"), zig_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of unused function - should return no matches
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:unusedFunction")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}
