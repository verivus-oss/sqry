//! CLI integration tests for Swift relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Swift:
//! - Callers queries (function calls, method calls, protocol methods)
//! - Callees queries (what a function calls)
//! - Exports queries (public functions, classes, structs, protocols)
//! - Imports queries (import statements)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Public Functions, Classes, Structs, Protocols
// ============================================================================

#[test]
fn cli_swift_exports_functions_and_classes() {
    let project = TempDir::new().unwrap();

    let swift_code = r#"
import Foundation

public func greet(name: String) -> String {
    return "Hello, \(name)!"
}

private func privateHelper() -> Int {
    return 42
}

public class User {
    private var name: String
    private var age: Int

    public init(name: String, age: Int) {
        self.name = name
        self.age = age
    }

    public func getName() -> String {
        return name
    }

    private func validate() -> Bool {
        return !name.isEmpty
    }
}

public let API_VERSION = "1.0.0"
"#;
    std::fs::write(project.path().join("Module.swift"), swift_code).unwrap();

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
        .stdout(predicate::str::contains("Module.swift"));

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Module.swift"));

    // Private helper should not be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:privateHelper")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_swift_exports_protocols_and_structs() {
    let project = TempDir::new().unwrap();

    let swift_code = r#"
import Foundation

public protocol Repository {
    func save(item: String)
    func findById(id: Int) -> String?
}

public struct UserData {
    public var id: Int
    public var name: String

    public init(id: Int, name: String) {
        self.id = id
        self.name = name
    }
}

public class UserRepository: Repository {
    public func save(item: String) {
        // implementation
    }

    public func findById(id: Int) -> String? {
        return nil
    }
}
"#;
    std::fs::write(project.path().join("Repository.swift"), swift_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for protocol
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Repository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Repository.swift"));

    // Query for struct
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Repository.swift"));
}

// ============================================================================
// Callers Queries - Function and Method Calls
// ============================================================================

#[test]
fn cli_swift_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let swift_code = r#"
func validate(input: String) -> Bool {
    return !input.isEmpty
}

func process(data: String) -> String? {
    if validate(input: data) {
        return data.trimmingCharacters(in: .whitespaces)
    }
    return nil
}

process(data: "test")
"#;
    std::fs::write(project.path().join("Processor.swift"), swift_code).unwrap();

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
fn cli_swift_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let swift_code = r#"
class DataService {
    private func fetchData() -> [Int] {
        return [1, 2, 3]
    }

    private func transformData(_ data: [Int]) -> [Int] {
        return data.map { $0 * 2 }
    }

    func process() -> [Int] {
        let data = fetchData()
        return transformData(data)
    }
}

let service = DataService()
service.process()
"#;
    std::fs::write(project.path().join("Service.swift"), swift_code).unwrap();

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
fn cli_swift_callees_function() {
    let project = TempDir::new().unwrap();

    let swift_code = r#"
func log(_ message: String) {
    print(message)
}

func warn(_ message: String) {
    print("WARNING: \(message)")
}

func handleError(_ error: Error) {
    log("Error occurred")
    warn(error.localizedDescription)
}

handleError(NSError(domain: "Test", code: 1))
"#;
    std::fs::write(project.path().join("Logger.swift"), swift_code).unwrap();

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
fn cli_swift_imports() {
    let project = TempDir::new().unwrap();

    let swift_code = r#"
import Foundation
import UIKit

func readConfig() -> String {
    let fileURL = URL(fileURLWithPath: "config.txt")
    do {
        return try String(contentsOf: fileURL)
    } catch {
        return ""
    }
}

readConfig()
"#;
    std::fs::write(project.path().join("Config.swift"), swift_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:Foundation")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Config.swift"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:UIKit")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Config.swift"));
}

// ============================================================================
// Extensions and Protocol Conformance
// ============================================================================

#[test]
fn cli_swift_extensions() {
    let project = TempDir::new().unwrap();

    let swift_code = r#"
extension String {
    func isValidEmail() -> Bool {
        return self.contains("@")
    }
}

func validateUserEmail(_ email: String) -> Bool {
    return email.isValidEmail()
}

validateUserEmail("test@example.com")
"#;
    std::fs::write(project.path().join("Extensions.swift"), swift_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of extension method
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:isValidEmail")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("validateUserEmail"));
}

// ============================================================================
// Async/Await
// ============================================================================

#[test]
fn cli_swift_async_await() {
    let project = TempDir::new().unwrap();

    let swift_code = r#"
import Foundation

struct User {
    var id: Int
    var name: String
}

struct Post {
    var id: Int
    var title: String
}

struct UserData {
    var user: User
    var posts: [Post]
}

func fetchUser(id: Int) async -> User {
    return User(id: id, name: "Test")
}

func fetchPosts(userId: Int) async -> [Post] {
    return []
}

func getUserData(id: Int) async -> UserData {
    let user = await fetchUser(id: id)
    let posts = await fetchPosts(userId: user.id)
    return UserData(user: user, posts: posts)
}

Task {
    await getUserData(id: 1)
}
"#;
    std::fs::write(project.path().join("Async.swift"), swift_code).unwrap();

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
fn cli_swift_private_functions() {
    let project = TempDir::new().unwrap();

    let swift_code = r#"
class Service {
    func execute() {
        validate()
    }

    private func validate() {
        // private function
    }
}
"#;
    std::fs::write(project.path().join("Service.swift"), swift_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of private function
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:validate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("execute"));
}

#[test]
fn cli_swift_callers_no_results() {
    let project = TempDir::new().unwrap();

    let swift_code = r#"
func unusedFunction() -> Int {
    return 42
}

print("Hello")
"#;
    std::fs::write(project.path().join("Unused.swift"), swift_code).unwrap();

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
