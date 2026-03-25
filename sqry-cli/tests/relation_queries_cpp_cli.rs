//! CLI integration tests for C++ relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for C++:
//! - Callers queries (function calls, method calls, template calls)
//! - Callees queries (what a function calls)
//! - Exports queries (classes, functions, namespaces)
//! - Imports queries (#include directives, using statements)
//!
//! ## Implementation Notes
//! - Inline methods (defined inside class bodies) are extracted via recursive tree walking
//! - Out-of-line methods (defined outside class bodies) are extracted via tree-sitter queries
//! - Both approaches work together to ensure complete method coverage

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Classes, Functions, Namespaces
// ============================================================================

#[test]
fn cli_cpp_exports_classes_and_functions() {
    let project = TempDir::new().unwrap();

    let cpp_code = r#"
#include <string>
#include <iostream>

std::string greet(const std::string& name) {
    return "Hello, " + name + "!";
}

class User {
private:
    std::string name;
    int age;

public:
    User(const std::string& name, int age) : name(name), age(age) {}

    std::string getName() const {
        return name;
    }

    int getAge() const {
        return age;
    }

private:
    void validate() {
        // private method
    }
};

const std::string API_VERSION = "1.0.0";
"#;
    std::fs::write(project.path().join("module.cpp"), cpp_code).unwrap();

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
        .stdout(predicate::str::contains("module.cpp"));

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.cpp"));

    // Private method should NOT be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:validate")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_cpp_exports_namespaces() {
    let project = TempDir::new().unwrap();

    let cpp_code = r#"
#include <string>

namespace utils {
    std::string format(const std::string& str) {
        return str;
    }

    class Formatter {
    public:
        std::string format(const std::string& input) {
            return input;
        }
    };
}

namespace api {
    class Service {
    public:
        void execute() {}
    };
}
"#;
    std::fs::write(project.path().join("namespaces.cpp"), cpp_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for function in namespace
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:format")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("namespaces.cpp"));

    // Query for class in namespace
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Formatter")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("namespaces.cpp"));
}

// ============================================================================
// Callers Queries - Function and Method Calls
// ============================================================================

#[test]
fn cli_cpp_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let cpp_code = r#"
#include <string>

bool validate(const std::string& input) {
    return !input.empty();
}

std::string process(const std::string& data) {
    if (validate(data)) {
        return data;
    }
    return "";
}

int main() {
    process("test");
    return 0;
}
"#;
    std::fs::write(project.path().join("processor.cpp"), cpp_code).unwrap();

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
fn cli_cpp_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let cpp_code = r#"
#include <vector>

class DataService {
private:
    std::vector<int> fetchData() {
        return {1, 2, 3};
    }

    std::vector<int> transformData(const std::vector<int>& data) {
        std::vector<int> result;
        for (int x : data) {
            result.push_back(x * 2);
        }
        return result;
    }

public:
    std::vector<int> process() {
        auto data = fetchData();
        return transformData(data);
    }
};

int main() {
    DataService service;
    service.process();
    return 0;
}
"#;
    std::fs::write(project.path().join("service.cpp"), cpp_code).unwrap();

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

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_cpp_callees_function() {
    let project = TempDir::new().unwrap();

    let cpp_code = r#"
#include <iostream>

void log(const std::string& message) {
    std::cout << message << std::endl;
}

void warn(const std::string& message) {
    std::cerr << "WARNING: " << message << std::endl;
}

void handleError(const std::string& error) {
    log("Error occurred");
    warn(error);
}

int main() {
    handleError("test error");
    return 0;
}
"#;
    std::fs::write(project.path().join("logger.cpp"), cpp_code).unwrap();

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
// Templates and Modern C++ Features
// ============================================================================

#[test]
fn cli_cpp_templates() {
    let project = TempDir::new().unwrap();

    let cpp_code = r#"
#include <iostream>

template<typename T>
class Container {
private:
    T data;

public:
    Container(const T& data) : data(data) {}

    T getData() const {
        return data;
    }

    T process() {
        return transform(data);
    }

private:
    T transform(const T& input) {
        return input;
    }
};

int main() {
    Container<int> container(42);
    container.process();
    return 0;
}
"#;
    std::fs::write(project.path().join("templates.cpp"), cpp_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of process (template method)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:process")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("transform"));
}

#[test]
fn cli_cpp_lambdas() {
    let project = TempDir::new().unwrap();

    let cpp_code = r#"
#include <vector>
#include <algorithm>

int transform(int x) {
    return x * 2;
}

void processData() {
    std::vector<int> data = {1, 2, 3};
    std::transform(data.begin(), data.end(), data.begin(),
        [](int x) { return transform(x); });
}

int main() {
    processData();
    return 0;
}
"#;
    std::fs::write(project.path().join("lambdas.cpp"), cpp_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of transform (called in lambda)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:transform")
        .arg(project.path())
        .assert()
        .success();
    // Lambda may or may not be identified as caller depending on implementation
}

// ============================================================================
// Imports Queries
// ============================================================================

#[test]
fn cli_cpp_imports() {
    let project = TempDir::new().unwrap();

    let cpp_code = r#"
#include <iostream>
#include <vector>
#include <string>
#include "user.h"

void readConfig() {
    std::vector<std::string> config;
    std::cout << "Reading config" << std::endl;
}

int main() {
    readConfig();
    return 0;
}
"#;
    std::fs::write(project.path().join("config.cpp"), cpp_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for includes
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:iostream")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.cpp"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:vector")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.cpp"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_cpp_private_methods() {
    let project = TempDir::new().unwrap();

    let cpp_code = r#"
class Service {
public:
    void execute() {
        validate();
    }

private:
    void validate() {
        // private method
    }
};
"#;
    std::fs::write(project.path().join("service.cpp"), cpp_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of private method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("execute"));
}

#[test]
fn cli_cpp_inline_and_out_of_line_deduplication() {
    // This test verifies that when a method is both declared inline
    // and defined out-of-line, we don't create duplicate symbols
    let project = TempDir::new().unwrap();

    let cpp_code = r#"
class Calculator {
public:
    // Inline method declaration and definition
    int add(int a, int b) {
        return a + b;
    }

    // Inline method declaration only
    int multiply(int a, int b);
};

// Out-of-line definition of multiply
int Calculator::multiply(int a, int b) {
    return a * b;
}

int main() {
    Calculator calc;
    calc.add(1, 2);
    calc.multiply(3, 4);
    return 0;
}
"#;
    std::fs::write(project.path().join("calculator.cpp"), cpp_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for add method (inline only) - should find main as caller
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("callers:add")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);
    // Should find exactly one caller (main), not duplicates
    assert!(stdout.contains("main"));

    // Query for multiply method (inline declaration + out-of-line definition)
    // Should find main as caller, with no duplicate symbols
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("callers:multiply")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);
    assert!(stdout.contains("main"));

    // Verify exports doesn't show duplicates - search for "multiply"
    let output = Command::new(sqry_bin())
        .arg("search")
        .arg("multiply")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);
    // Default text output uses the semantic leaf/simple name.
    let count = stdout.matches("method multiply").count();
    assert_eq!(
        count, 1,
        "Expected exactly 1 occurrence of 'method multiply', found {count}"
    );

    let output = Command::new(sqry_bin())
        .arg("--qualified-names")
        .arg("search")
        .arg("multiply")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&output);
    let count = stdout.matches("method Calculator::multiply").count();
    assert_eq!(
        count, 1,
        "Expected exactly 1 occurrence of 'method Calculator::multiply', found {count}"
    );
}

#[test]
fn cli_cpp_callers_no_results() {
    let project = TempDir::new().unwrap();

    let cpp_code = r#"
int unusedFunction() {
    return 42;
}

int main() {
    std::cout << "Hello" << std::endl;
    return 0;
}
"#;
    std::fs::write(project.path().join("unused.cpp"), cpp_code).unwrap();

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
