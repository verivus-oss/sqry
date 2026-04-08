//! CLI integration tests for Python relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Python:
//! - Callers queries (function calls, method calls)
//! - Callees queries (what a function calls)
//! - Exports queries (module-level functions, classes, constants)
//! - Imports queries (import statements, from X import Y)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Functions, Classes, and Module-Level Definitions
// ============================================================================

#[test]
fn cli_python_exports_functions_and_classes() {
    let project = TempDir::new().unwrap();

    let py_code = r#"
__all__ = ["greet", "User", "API_VERSION"]

def greet(name):
    """Greet a user by name."""
    return f"Hello, {name}!"

def farewell(name):
    """Say goodbye to a user."""
    return f"Goodbye, {name}!"

class User:
    def __init__(self, name):
        self.name = name

    def get_name(self):
        return self.name

API_VERSION = "1.0.0"
"#;
    std::fs::write(project.path().join("module.py"), py_code).unwrap();

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
        .stdout(predicate::str::contains("module.py"));

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.py"));
}

#[test]
fn cli_python_exports_with_all() {
    let project = TempDir::new().unwrap();

    let py_code = r#"
__all__ = ['public_function', 'PublicClass']

def public_function():
    return "public"

def _private_function():
    return "private"

class PublicClass:
    pass

class _PrivateClass:
    pass
"#;
    std::fs::write(project.path().join("exports.py"), py_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for public exports
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_function")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("exports.py"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:PublicClass")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("exports.py"));

    // Private function should not be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:_private_function")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

// ============================================================================
// Callers Queries - Function and Method Calls
// ============================================================================

#[test]
fn cli_python_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let py_code = r#"
def validate(input_str):
    return len(input_str) > 0

def process(data):
    if validate(data):
        return data.strip()
    return None

process("test")
"#;
    std::fs::write(project.path().join("processor.py"), py_code).unwrap();

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
fn cli_python_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let py_code = r"
class DataService:
    def fetch_data(self):
        return []

    def transform_data(self, data):
        return [x * 2 for x in data]

    def process(self):
        data = self.fetch_data()
        return self.transform_data(data)

service = DataService()
service.process()
";
    std::fs::write(project.path().join("service.py"), py_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of fetch_data
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:fetch_data")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));

    // Query for callers of transform_data
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:transform_data")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));
}

#[test]
fn cli_python_callers_chained_method_calls() {
    let project = TempDir::new().unwrap();

    let py_code = r"
class QueryBuilder:
    def where(self, condition):
        return self

    def order_by(self, field):
        return self

    def limit(self, count):
        return self

    def execute(self):
        return []

def run_query():
    return (QueryBuilder()
        .where('active = true')
        .order_by('created_at')
        .limit(10)
        .execute())

run_query()
";
    std::fs::write(project.path().join("query.py"), py_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers in method chain
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:where")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("run_query"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:execute")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("run_query"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_python_callees_function() {
    let project = TempDir::new().unwrap();

    let py_code = r#"
def log(message):
    print(message)

def warn(message):
    print(f"WARNING: {message}")

def handle_error(error):
    log('Error occurred')
    warn(str(error))
    print(error)

handle_error(Exception('Test'))
"#;
    std::fs::write(project.path().join("logger.py"), py_code).unwrap();

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
        .stdout(predicate::str::contains("log"))
        .stdout(predicate::str::contains("warn"));
}

#[test]
fn cli_python_callees_method() {
    let project = TempDir::new().unwrap();

    let py_code = r"
class UserService:
    def find_user(self, user_id):
        return {'id': user_id, 'name': 'Test'}

    def validate_user(self, user):
        return user['id'] > 0

    def get_user(self, user_id):
        user = self.find_user(user_id)
        if self.validate_user(user):
            return user
        return None

service = UserService()
service.get_user(1)
";
    std::fs::write(project.path().join("user_service.py"), py_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of get_user
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:get_user")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("find_user"))
        .stdout(predicate::str::contains("validate_user"));
}

// ============================================================================
// Imports Queries - Import Statements
// ============================================================================

#[test]
fn cli_python_imports_from_import() {
    let project = TempDir::new().unwrap();

    let py_code = r"
from utils import greet, farewell
from user import User
from helpers import *

def main():
    user = User('Alice')
    greet(user.get_name())

main()
";
    std::fs::write(project.path().join("main.py"), py_code).unwrap();

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
        .stdout(predicate::str::contains("main.py"));

    // Query for imports of 'utils' module
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:utils")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main.py"));
}

#[test]
fn cli_python_imports_import_statement() {
    let project = TempDir::new().unwrap();

    let py_code = r"
import os
import sys
import json

def read_config():
    config_path = os.path.join(os.getcwd(), 'config.json')
    with open(config_path) as f:
        return json.load(f)

read_config()
";
    std::fs::write(project.path().join("config.py"), py_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:os")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.py"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:json")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.py"));
}

// ============================================================================
// Decorators and Advanced Python Features
// ============================================================================

#[test]
fn cli_python_decorators() {
    let project = TempDir::new().unwrap();

    let py_code = r#"
def log_decorator(func):
    def wrapper(*args, **kwargs):
        print(f"Calling {func.__name__}")
        return func(*args, **kwargs)
    return wrapper

class UserService:
    @log_decorator
    def create_user(self, name):
        print(f"Creating user: {name}")

    @log_decorator
    def delete_user(self, user_id):
        print(f"Deleting user: {user_id}")

service = UserService()
service.create_user("Alice")
"#;
    std::fs::write(project.path().join("decorators.py"), py_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of decorated method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:create_user")
        .arg(project.path())
        .assert()
        .success();
}

#[test]
fn cli_python_async_await() {
    let project = TempDir::new().unwrap();

    let py_code = r"
async def fetch_user(user_id):
    return {'id': user_id, 'name': 'Test'}

async def fetch_posts(user_id):
    return []

async def get_user_data(user_id):
    user = await fetch_user(user_id)
    posts = await fetch_posts(user['id'])
    return {'user': user, 'posts': posts}

import asyncio
asyncio.run(get_user_data(1))
";
    std::fs::write(project.path().join("async_code.py"), py_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of async function
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:get_user_data")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("fetch_user"))
        .stdout(predicate::str::contains("fetch_posts"));
}

#[test]
fn cli_python_class_methods_and_static_methods() {
    let project = TempDir::new().unwrap();

    let py_code = r"
class MathUtils:
    @staticmethod
    def add(a, b):
        return a + b

    @classmethod
    def multiply(cls, a, b):
        return a * b

    def divide(self, a, b):
        return a / b

def calculate():
    result1 = MathUtils.add(2, 3)
    result2 = MathUtils.multiply(2, 3)
    utils = MathUtils()
    result3 = utils.divide(6, 2)
    return result1 + result2 + result3

calculate()
";
    std::fs::write(project.path().join("math_utils.py"), py_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of static method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:add")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("calculate"));

    // Query for callers of class method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:multiply")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("calculate"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_python_private_methods() {
    let project = TempDir::new().unwrap();

    let py_code = r"
class Service:
    def execute(self):
        self._validate()

    def _validate(self):
        # private method (by convention)
        pass
";
    std::fs::write(project.path().join("service.py"), py_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of private method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:_validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("execute"));
}

#[test]
fn cli_python_callers_no_results() {
    let project = TempDir::new().unwrap();

    let py_code = r"
def unused_function():
    return 42

def main():
    print('Hello')

main()
";
    std::fs::write(project.path().join("unused.py"), py_code).unwrap();

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
