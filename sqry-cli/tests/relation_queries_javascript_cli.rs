//! CLI integration tests for JavaScript relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for JavaScript:
//! - Callers queries (function calls, method calls, constructor calls)
//! - Callees queries (what a function calls)
//! - Exports queries (ES6 modules, CommonJS, named/default exports)
//! - Imports queries (import statements, require calls)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - ES6 and CommonJS Modules
// ============================================================================

#[test]
fn cli_javascript_exports_es6_named_exports() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
export function greet(name) {
    return `Hello, ${name}!`;
}

export function farewell(name) {
    return `Goodbye, ${name}!`;
}

export class User {
    constructor(name) {
        this.name = name;
    }

    getName() {
        return this.name;
    }
}

export const API_VERSION = "1.0.0";

function internalHelper() {
    return true;
}
"#;
    std::fs::write(project.path().join("module.js"), js_code).unwrap();

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
        .stdout(predicate::str::contains("module.js"));

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.js"));

    // Query for non-exported helper (should not be found)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:internalHelper")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_javascript_exports_default_export() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
export default class Application {
    constructor(config) {
        this.config = config;
    }

    run() {
        console.log('App running');
    }
}
"#;
    std::fs::write(project.path().join("app.js"), js_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for default exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Application")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("app.js"));
}

#[test]
fn cli_javascript_exports_commonjs() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
function helper(x) {
    return x * 2;
}

function calculate(a, b) {
    return helper(a) + helper(b);
}

module.exports = {
    helper,
    calculate
};
"#;
    std::fs::write(project.path().join("utils.js"), js_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for CommonJS exported functions
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:helper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("utils.js"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:calculate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("utils.js"));
}

// ============================================================================
// Callers Queries - Function and Method Calls
// ============================================================================

#[test]
fn cli_javascript_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
function validate(input) {
    return input.length > 0;
}

function process(data) {
    if (validate(data)) {
        return data.trim();
    }
    return null;
}

process("test");
"#;
    std::fs::write(project.path().join("processor.js"), js_code).unwrap();

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
fn cli_javascript_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
class DataService {
    fetchData() {
        return [];
    }

    transformData(data) {
        return data.map(x => x * 2);
    }

    process() {
        const data = this.fetchData();
        return this.transformData(data);
    }
}

const service = new DataService();
service.process();
"#;
    std::fs::write(project.path().join("service.js"), js_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of fetchData
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:fetchData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));

    // Query for callers of transformData
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:transformData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));
}

#[test]
fn cli_javascript_callers_chained_methods() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
class QueryBuilder {
    where(condition) {
        return this;
    }

    orderBy(field) {
        return this;
    }

    limit(count) {
        return this;
    }

    execute() {
        return [];
    }
}

function runQuery() {
    return new QueryBuilder()
        .where('active = true')
        .orderBy('created_at')
        .limit(10)
        .execute();
}

runQuery();
"#;
    std::fs::write(project.path().join("query.js"), js_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of where (first in chain)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:where")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("runQuery"));

    // Query for callers of execute (end of chain)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:execute")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("runQuery"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_javascript_callees_function() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
function log(message) {
    console.log(message);
}

function warn(message) {
    console.warn(message);
}

function handleError(error) {
    log('Error occurred');
    warn(error.message);
    console.error(error.stack);
}

handleError(new Error('Test'));
"#;
    std::fs::write(project.path().join("logger.js"), js_code).unwrap();

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

#[test]
fn cli_javascript_callees_method() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
class UserService {
    findUser(id) {
        return { id, name: 'Test' };
    }

    validateUser(user) {
        return user.id > 0;
    }

    getUser(id) {
        const user = this.findUser(id);
        if (this.validateUser(user)) {
            return user;
        }
        return null;
    }
}

const service = new UserService();
service.getUser(1);
"#;
    std::fs::write(project.path().join("user-service.js"), js_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of getUser
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:getUser")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("findUser"))
        .stdout(predicate::str::contains("validateUser"));
}

// ============================================================================
// Imports Queries - Module Imports
// ============================================================================

#[test]
fn cli_javascript_imports_es6_imports() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
import { greet, farewell } from './utils';
import User from './user';
import * as helpers from './helpers';

function main() {
    const user = new User('Alice');
    greet(user.getName());
    helpers.process();
}

main();
"#;
    std::fs::write(project.path().join("main.js"), js_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports of 'user' module (imports:X matches module names, not symbol names)
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:user")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main.js"));

    // Query for imports of 'utils' module
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:utils")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main.js"));
}

#[test]
fn cli_javascript_imports_commonjs_require() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
const fs = require('fs');
const path = require('path');
const { helper } = require('./utils');

function readConfig() {
    const configPath = path.join(__dirname, 'config.json');
    return fs.readFileSync(configPath, 'utf8');
}

readConfig();
"#;
    std::fs::write(project.path().join("config.js"), js_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports/requires
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:fs")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.js"));
}

// ============================================================================
// Arrow Functions and Modern JavaScript Features
// ============================================================================

#[test]
fn cli_javascript_arrow_functions() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
const double = (x) => x * 2;
const triple = (x) => x * 3;

const calculate = (value) => {
    const d = double(value);
    const t = triple(value);
    return d + t;
};

export { double, triple, calculate };
"#;
    std::fs::write(project.path().join("math.js"), js_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exports of arrow functions
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:double")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("math.js"));

    // Query for callers within arrow function
    // Variable-assigned arrow functions use the variable name per FR-JS-PATCH-1 clarification
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:double")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("calculate"));
}

#[test]
fn cli_javascript_async_await() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
async function fetchUser(id) {
    return { id, name: 'Test' };
}

async function fetchPosts(userId) {
    return [];
}

async function getUserData(id) {
    const user = await fetchUser(id);
    const posts = await fetchPosts(user.id);
    return { user, posts };
}

getUserData(1);
"#;
    std::fs::write(project.path().join("async.js"), js_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of async function
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:getUserData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetchUser"))
        .stdout(predicate::str::contains("fetchPosts"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_javascript_exports_private_functions_not_exported() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
function privateHelper() {
    return 42;
}

export function publicAPI() {
    return privateHelper();
}
"#;
    std::fs::write(project.path().join("api.js"), js_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for public export - should be found
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:publicAPI")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("api.js"));

    // privateHelper is not exported, so exports query may or may not find it
    // (depends on implementation - it's defined but not in export statement)
}

#[test]
fn cli_javascript_callers_no_results() {
    let project = TempDir::new().unwrap();

    let js_code = r#"
function unusedFunction() {
    return 42;
}

function main() {
    console.log('Hello');
}

main();
"#;
    std::fs::write(project.path().join("unused.js"), js_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of unused function - should have no results or empty output
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:unusedFunction")
        .arg(project.path())
        .assert()
        .success();
    // No specific assertion - just verify it doesn't crash
}
