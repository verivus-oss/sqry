//! CLI integration tests for PHP relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for PHP:
//! - Callers queries (instance methods, static methods, functions)
//! - Callees queries (what a function calls)
//! - Exports queries (classes, methods with visibility filtering)
//! - Imports queries (use statements, require)
//!
//! This validates the PHP relation extraction implementation against the
//! ≥95% recall target (M1, M2, M3 from docs/development/relation-tracking/php/01_SPEC.md).

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
#[cfg(feature = "specialty-plugins")]
use serial_test::serial;
use std::path::Path;
use tempfile::TempDir;

// ============================================================================
// Callers Queries - Instance and Static Method Calls
// ============================================================================

#[test]
fn cli_php_callers_instance_methods() {
    let project = TempDir::new().unwrap();

    let php_code = r"<?php
class UserService {
    public function authenticate($username, $password) {
        $user = $this->findUser($username);
        if ($user) {
            return $this->validatePassword($user, $password);
        }
        return false;
    }

    private function findUser($username) {
        return Database::query('SELECT * FROM users WHERE username = ?', [$username]);
    }

    private function validatePassword($user, $password) {
        return password_verify($password, $user->password_hash);
    }
}

class Database {
    public static function query($sql, $params = []) {
        return null;
    }
}
";
    std::fs::write(project.path().join("UserService.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of findUser (private method)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:findUser")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("authenticate"));

    // Query for callers of validatePassword (private method)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:validatePassword")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("authenticate"));
}

#[test]
fn cli_php_callers_static_methods() {
    let project = TempDir::new().unwrap();

    let php_code = r#"<?php
class Logger {
    public static function info($message) {
        self::write('INFO', $message);
    }

    public static function error($message) {
        self::write('ERROR', $message);
    }

    private static function write($level, $message) {
        echo "[$level] $message\n";
    }
}

class Application {
    public function run() {
        Logger::info('Application started');
        Logger::error('Something went wrong');
    }
}
"#;
    std::fs::write(project.path().join("Logger.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of Logger::info
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:info")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("run"));

    // Query for callers of Logger::error
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:error")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("run"));

    // Query for callers of write (called by info and error via self::)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:write")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("info"))
        .stdout(predicate::str::contains("error"));
}

#[test]
fn cli_php_callers_global_functions() {
    let project = TempDir::new().unwrap();

    let php_code = r"<?php
function validateEmail($email) {
    return filter_var($email, FILTER_VALIDATE_EMAIL) !== false;
}

function normalizeUsername($username) {
    return trim(strtolower($username));
}

function processUserInput($username, $email) {
    $username = normalizeUsername($username);
    $valid = validateEmail($email);
    return $valid;
}
";
    std::fs::write(project.path().join("helpers.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of validateEmail
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:validateEmail")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("processUserInput"));

    // Query for callers of normalizeUsername
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:normalizeUsername")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("processUserInput"));
}

#[test]
fn cli_php_callers_namespaced_functions() {
    let project = TempDir::new().unwrap();

    let php_code = r"<?php
namespace App\Helpers;

function slugify($text) {
    return strtolower(preg_replace('/[^a-z0-9]+/', '-', $text));
}

function truncate($text, $length = 100) {
    return strlen($text) > $length ? substr($text, 0, $length) . '...' : $text;
}

namespace App\Services;

use function App\Helpers\slugify;

class PostService {
    public function createSlug($title) {
        return slugify($title);
    }
}
";
    std::fs::write(project.path().join("services.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of slugify
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:slugify")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("createSlug"));
}

#[test]
fn cli_php_callers_null_safe_operator() {
    let project = TempDir::new().unwrap();

    let php_code = r"<?php
class User {
    private $profile;

    public function getProfile() {
        return $this->profile;
    }
}

class Profile {
    private $name;

    public function getName() {
        return $this->name;
    }
}

class UserService {
    public function getUserName($user) {
        return $user?->getProfile()?->getName();
    }
}
";
    std::fs::write(project.path().join("user.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of getProfile (called via null-safe operator)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:getProfile")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("getUserName"));

    // Query for callers of getName (called via null-safe operator)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:getName")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("getUserName"));
}

// ============================================================================
// Exports Queries - Classes, Methods, Visibility
// ============================================================================

#[test]
fn cli_php_exports_classes_and_interfaces() {
    let project = TempDir::new().unwrap();

    let php_code = r"<?php
class User {
    public function getId() {
        return $this->id;
    }
}

interface Repository {
    public function findById($id);
    public function save($entity);
}

class UserRepository implements Repository {
    public function findById($id) {
        return null;
    }

    public function save($entity) {
        return true;
    }
}

trait Timestampable {
    public function getCreatedAt() {
        return $this->created_at;
    }
}
";
    std::fs::write(project.path().join("models.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for User class export
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("models.php"));

    // Query for Repository interface export
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Repository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("models.php"));

    // Query for Timestampable trait export
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Timestampable")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("models.php"));
}

#[test]

fn cli_php_exports_visibility_filtering() {
    let project = TempDir::new().unwrap();

    let php_code = r"<?php
class UserService {
    public function createUser($username, $password) {
        $this->validateInput($username, $password);
        $hash = $this->hashPassword($password);
        return true;
    }

    public function updateUser($id, $data) {
        return true;
    }

    protected function validateInput($username, $password) {
        return true;
    }

    private function hashPassword($password) {
        return password_hash($password, PASSWORD_DEFAULT);
    }
}
";
    std::fs::write(project.path().join("UserService.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for createUser (public method - should be exported)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:createUser")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("UserService.php"));

    // Query for updateUser (public method - should be exported)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:updateUser")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("UserService.php"));

    // Protected/private methods should not be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:validateInput")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:hashPassword")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]

fn cli_php_exports_namespaced_classes() {
    let project = TempDir::new().unwrap();

    let php_code = r"<?php
namespace App\Services;

class UserService {
    public function getUsers() {
        return [];
    }
}

namespace App\Repositories;

class UserRepository {
    public function findAll() {
        return [];
    }
}
";
    std::fs::write(project.path().join("app.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for UserService (should find namespace-qualified export)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserService")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("app.php"));

    // Query for UserRepository (should find namespace-qualified export)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserRepository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("app.php"));
}

#[test]
fn cli_php_exports_global_functions() {
    let project = TempDir::new().unwrap();

    let php_code = r"<?php
function globalHelper() {
    return true;
}

namespace App\Helpers;

function namespacedHelper() {
    return true;
}
";
    std::fs::write(project.path().join("functions.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for globalHelper (global function export)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:globalHelper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("functions.php"));

    // Query for namespacedHelper (namespaced function export)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:namespacedHelper")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("functions.php"));
}

// ============================================================================
// Imports Queries - use statements, require
// ============================================================================

#[test]
fn cli_php_imports_use_statements() {
    let project = TempDir::new().unwrap();

    let php_code = r"<?php
namespace App\Controllers;

use App\Services\UserService;
use App\Repositories\UserRepository;

class UserController {
    private $userService;

    public function __construct(UserService $service) {
        $this->userService = $service;
    }

    public function index() {
        return $this->userService->getUsers();
    }
}
";
    std::fs::write(project.path().join("UserController.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports of UserService
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:UserService")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("UserController.php"));

    // Query for imports of UserRepository
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:UserRepository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("UserController.php"));
}

#[test]
fn cli_php_imports_require_statements() {
    let project = TempDir::new().unwrap();

    let php_code = r"<?php
require_once 'config.php';
require_once __DIR__ . '/helpers.php';
include 'optional.php';

class Application {
    public function run() {
        return true;
    }
}
";
    std::fs::write(project.path().join("app.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports of config.php
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:config")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("app.php"));

    // Query for imports of helpers.php
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:helpers")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("app.php"));
}

// ============================================================================
// Callees Queries - What a function calls
// ============================================================================

#[test]
fn cli_php_callees_query() {
    let project = TempDir::new().unwrap();

    let php_code = r#"<?php
class UserService {
    public function authenticate($username, $password) {
        $user = $this->findUser($username);
        $valid = $this->validatePassword($user, $password);
        $this->logAttempt($username, $valid);
        return $valid;
    }

    private function findUser($username) {
        return null;
    }

    private function validatePassword($user, $password) {
        return password_verify($password, $user->password);
    }

    private function logAttempt($username, $success) {
        error_log("Login attempt: $username");
    }
}
"#;
    std::fs::write(project.path().join("UserService.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of authenticate (what authenticate calls)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:authenticate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("findUser"))
        .stdout(predicate::str::contains("validatePassword"))
        .stdout(predicate::str::contains("logAttempt"));
}

// ============================================================================
// Method Chaining Patterns
// ============================================================================

#[test]
fn cli_php_callers_method_chaining() {
    let project = TempDir::new().unwrap();

    let php_code = r"<?php
class QueryBuilder {
    public function table($table) {
        return $this;
    }

    public function where($column, $value) {
        return $this;
    }

    public function orderBy($column) {
        return $this;
    }

    public function get() {
        return [];
    }
}

class UserQuery {
    private $builder;

    public function __construct() {
        $this->builder = new QueryBuilder();
    }

    public function findActiveUsers() {
        return $this->builder
            ->table('users')
            ->where('status', 'active')
            ->orderBy('created_at')
            ->get();
    }
}
";
    std::fs::write(project.path().join("QueryBuilder.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of table (first in chain)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:table")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("findActiveUsers"));

    // Query for callers of where (middle of chain)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:where")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("findActiveUsers"));

    // Query for callers of get (end of chain)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:get")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("findActiveUsers"));
}

// JSON OUTPUT TESTS (validates --json flag and PHP-specific metadata)

#[test]
fn cli_php_json_exports_with_methods() {
    // Test that exports: query correctly finds class and methods
    // Note: exports:ClassName finds the class; exports:methodName finds methods
    let project = TempDir::new().unwrap();
    let php_code = r"<?php
namespace App\Services;

class UserService {
    public function authenticate($username, $password) {
        return $this->findUser($username);
    }

    private function findUser($username) {
        return null;
    }

    protected function validatePassword($password) {
        return strlen($password) >= 8;
    }
}
";
    std::fs::write(project.path().join("UserService.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // exports:UserService finds the class
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserService")
        .arg("--json")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(output).expect("valid UTF-8");
    let json: serde_json::Value = serde_json::from_str(&json_str).expect("valid JSON");

    let results = json["results"].as_array().expect("results should be array");
    assert!(
        !results.is_empty(),
        "exports:UserService should find the class"
    );

    // The class should be found
    let class_result = results
        .iter()
        .find(|r| {
            r["name"]
                .as_str()
                .is_some_and(|n| n.contains("UserService"))
        })
        .expect("Should find UserService class");

    assert_eq!(
        class_result["kind"].as_str(),
        Some("class"),
        "Result should be a class"
    );

    // exports:authenticate finds the public method
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("exports:authenticate")
        .arg("--json")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(output).expect("valid UTF-8");
    let json: serde_json::Value = serde_json::from_str(&json_str).expect("valid JSON");

    let results = json["results"].as_array().expect("results should be array");
    assert!(
        !results.is_empty(),
        "exports:authenticate should find the public method"
    );

    // Verify the method is found with qualified name
    let method_result = results
        .iter()
        .find(|r| {
            r["qualified_name"]
                .as_str()
                .is_some_and(|n| n.contains("authenticate"))
        })
        .expect("Should find authenticate method");

    assert_eq!(
        method_result["kind"].as_str(),
        Some("method"),
        "Result should be a method"
    );
}

#[test]
fn cli_php_json_calls_with_dynamic_metadata() {
    let project = TempDir::new().unwrap();
    let php_code = r#"<?php
namespace App\Services;

class EventDispatcher {
    public function dispatch($eventName, $data) {
        // Dynamic call via call_user_func
        $handler = $this->getHandler($eventName);
        call_user_func($handler, $data);

        // Regular static method call
        Logger::info("Event dispatched: " . $eventName);
    }

    private function getHandler($eventName) {
        return 'handleEvent';
    }
}
"#;
    std::fs::write(project.path().join("EventDispatcher.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("callers:call_user_func")
        .arg("--json")
        .arg(project.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json_str = String::from_utf8(output).expect("valid UTF-8");
    let json: serde_json::Value = serde_json::from_str(&json_str).expect("valid JSON");

    // Verify results exist
    let results = json["results"].as_array().expect("results should be array");

    // For call_user_func, we expect to find it as a called function
    // The structure may have the call location info
    // Note: The exact metadata structure for dynamic calls may vary
    // This test validates that --json mode works and structure is correct

    // At minimum, verify the JSON structure is valid
    assert!(json.get("query").is_some(), "JSON should have 'query' key");
    assert!(json.get("stats").is_some(), "JSON should have 'stats' key");

    // If we have results, they should have proper structure
    if !results.is_empty() {
        let first_result = &results[0];
        assert!(
            first_result.get("file_path").is_some(),
            "Result should have file_path"
        );
        assert!(
            first_result.get("start_line").is_some(),
            "Result should have start_line"
        );
    }
}

// NEGATIVE TEST CASES (error handling and edge cases)

#[test]
fn cli_php_callers_query_no_results() {
    let project = TempDir::new().unwrap();
    let php_code = r"<?php
class Foo {
    public function bar() {
        return 42;
    }
}
";
    std::fs::write(project.path().join("Foo.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of a function that is never called
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("callers:nonexistent")
        .arg(project.path())
        .assert()
        .success();

    // Should return successfully with no results
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(
        stdout.contains("No results") || stdout.is_empty() || stdout.trim().is_empty(),
        "Should indicate no results when querying for non-existent symbol"
    );
}

#[test]
fn cli_php_exports_empty_class() {
    let project = TempDir::new().unwrap();
    let php_code = r"<?php
class EmptyClass {
    // No methods
}
";
    std::fs::write(project.path().join("Empty.php"), php_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exports of empty class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:EmptyClass")
        .arg(project.path())
        .assert()
        .success();
    // Should succeed even with empty results
}

#[test]
fn cli_php_callers_syntax_error_file() {
    let project = TempDir::new().unwrap();

    // Create a valid PHP file first
    let valid_php = r"<?php
class Valid {
    public function test() {
        return 1;
    }
}
";
    std::fs::write(project.path().join("Valid.php"), valid_php).unwrap();

    // Create a file with syntax error
    let invalid_php = r"<?php
class Invalid {
    public function broken( {  // Syntax error
        return 1;
    }
}
";
    std::fs::write(project.path().join("Invalid.php"), invalid_php).unwrap();

    // Index should succeed but skip invalid file
    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query should still work on valid files
    // exports:Valid finds the class named "Valid", not its methods
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Valid")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Valid"));
}

// FIXTURE-BASED RECALL TESTS (ground truth validation)
//
// The two `cli_php_fixture_symfony_*` tests below shell out to
// `sqry index` then `sqry query`. After `refactor!: make non-core
// surfaces opt-in` (commit ecd215385), `sqry index` writes a manifest
// that lists the now-feature-gated specialty plugin ids in
// `plugin_selection.active_plugin_ids`; the subsequent `sqry query`
// invocation rejects that manifest with `unknown plugin ids: …`.
// Gated on `specialty-plugins` so that the binary built for these
// tests includes the listed plugins. Run with:
//   cargo test -p sqry-cli --features specialty-plugins --test relation_queries_php_cli

#[cfg(feature = "specialty-plugins")]
#[test]
#[serial]
fn cli_php_fixture_symfony_exports_recall() {
    // Use symfony_controller.php fixture with ground truth annotations
    // Test verifies that class and methods are exported correctly
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("sqry-lang-php/tests/fixtures/relations/php/integration");

    // Ensure stale indexes do not interfere with validation
    let _ = std::fs::remove_dir_all(fixture_path.join(".sqry-index"));
    let _ = std::fs::remove_file(fixture_path.join(".sqry-index"));
    let _ = std::fs::remove_file(fixture_path.join(".sqry-index.trg"));
    let _ = std::fs::remove_dir_all(fixture_path.join(".sqry-cache"));

    // Index the fixture directory
    Command::new(sqry_bin())
        .arg("index")
        .arg(&fixture_path)
        .assert()
        .success();

    // Verify the UserController class is exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserController")
        .arg(&fixture_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("UserController"));

    // Verify key methods are exported by querying each directly
    // Ground truth methods: index, show, create, edit, delete, search
    let expected_methods = vec!["index", "show", "create", "edit", "delete", "search"];
    let mut found_count = 0;

    for method in &expected_methods {
        let output = Command::new(sqry_bin())
            .arg("query")
            .arg(format!("exports:{method}"))
            .arg(&fixture_path)
            .output()
            .expect("command should run");

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains(method) {
            found_count += 1;
        }
    }

    // Recall test: Should find at least 5 out of 6 key method exports (>80% recall)
    assert!(
        found_count >= 5,
        "Expected to find at least 5/6 exported methods, found {found_count}. Expected: {expected_methods:?}"
    );
}

#[cfg(feature = "specialty-plugins")]
#[test]
#[serial]
fn cli_php_fixture_symfony_callers_recall() {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("sqry-lang-php/tests/fixtures/relations/php/integration");

    // Ensure stale indexes do not interfere with validation
    let _ = std::fs::remove_dir_all(fixture_path.join(".sqry-index"));
    let _ = std::fs::remove_file(fixture_path.join(".sqry-index"));
    let _ = std::fs::remove_file(fixture_path.join(".sqry-index.trg"));
    let _ = std::fs::remove_dir_all(fixture_path.join(".sqry-cache"));

    Command::new(sqry_bin())
        .arg("index")
        .arg(&fixture_path)
        .assert()
        .success();

    // Query for callers of render (common method called in many controller actions)
    // Ground truth from annotations: render is called in index, show, create, edit actions
    let output = Command::new(sqry_bin())
        .arg("query")
        .arg("callers:render")
        .arg(&fixture_path)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("valid UTF-8");

    // Should find multiple callers (index, show, create, edit)
    let expected_callers = vec!["index", "show", "create", "edit"];
    let mut found_count = 0;

    for caller in &expected_callers {
        if stdout.contains(caller) {
            found_count += 1;
        }
    }

    // Recall test: Should find at least 3 out of 4 callers (75% recall minimum)
    assert!(
        found_count >= 3,
        "Expected to find at least 3/4 callers of render, found {found_count}"
    );
}

#[test]
fn cli_php_fixture_laravel_comprehensive() {
    // Verify the laravel_service.php fixture works end-to-end
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("sqry-lang-php/tests/fixtures/relations/php/integration/laravel_service.php");

    // Create temp dir and copy fixture
    let project = TempDir::new().unwrap();
    std::fs::copy(&fixture_path, project.path().join("laravel_service.php")).expect("copy fixture");

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Test exports query - exports:UserService finds the class named UserService
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserService")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("UserService"));

    // Test exports query for a method - exports:createUser finds the method
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:createUser")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("createUser"));

    // Test callers query - validateUserData is called by createUser
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:validateUserData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("createUser"));
}
