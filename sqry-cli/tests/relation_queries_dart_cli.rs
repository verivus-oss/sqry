//! CLI integration tests for Dart relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Dart:
//! - Callers queries (function calls, method calls)
//! - Callees queries (what a function calls)
//! - Exports queries (public classes, functions, variables)
//! - Imports queries (package imports, dart: imports, aliased imports)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Public Classes and Functions
// ============================================================================

#[test]
fn cli_dart_exports_classes_and_functions() {
    let project = TempDir::new().unwrap();

    let dart_code = r"
class User {
  final String name;
  final int age;

  User(this.name, this.age);

  String getName() => name;
}

class _PrivateService {
  void doWork() {}
}

void processUser(User user) {
  print(user.name);
}

void _privateFunction() {
  // This should not be exported
}
";
    std::fs::write(project.path().join("user.dart"), dart_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("user.dart"));

    // Query for exported function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:processUser")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("user.dart"));

    // Private class should NOT be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:_PrivateService")
        .arg(project.path())
        .assert()
        .success();
    // No assertion - just verify it doesn't crash
}

#[test]
fn cli_dart_exports_hide_private_elements() {
    let project = TempDir::new().unwrap();

    let dart_code = r"
class PublicClass {
  void publicMethod() {}
}

class _PrivateClass {
  void _privateMethod() {}
}

void publicFunction() {
  print('public');
}

void _privateFunction() {
  print('private');
}
";
    std::fs::write(project.path().join("visibility.dart"), dart_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Public elements should be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:PublicClass")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("PublicClass"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:publicFunction")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("publicFunction"));

    // Private class should NOT be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:_PrivateClass")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));

    // Private function should NOT be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:_privateFunction")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

// ============================================================================
// Imports Queries
// ============================================================================

#[test]
fn cli_dart_imports() {
    let project = TempDir::new().unwrap();

    let dart_code = r"
import 'dart:async';
import 'package:flutter/material.dart';
import 'package:http/http.dart' as http;

class DataService {
  Future<void> fetchData() async {
    final response = await http.get(Uri.parse('api/data'));
    print(response.body);
  }
}
";
    std::fs::write(project.path().join("service.dart"), dart_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for dart: import
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:async")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("service.dart"));

    // Query for package import
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:material")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("service.dart"));

    // Query for aliased import
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:http")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("service.dart"));
}

// ============================================================================
// Callers Queries - Function and Method Calls
// ============================================================================

#[test]
fn cli_dart_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let dart_code = r"
bool validate(int value) {
  return value > 0;
}

int process(int input) {
  if (validate(input)) {
    return input * 2;
  }
  return 0;
}

String analyze(int num) {
  if (validate(num)) {
    return 'valid';
  }
  return 'invalid';
}
";
    std::fs::write(project.path().join("validator.dart"), dart_code).unwrap();

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
fn cli_dart_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let dart_code = r"
class DataRepository {
  List<int> _fetchData() {
    return [1, 2, 3];
  }

  List<int> _transformData(List<int> data) {
    return data.map((x) => x * 2).toList();
  }

  List<int> process() {
    final data = _fetchData();
    return _transformData(data);
  }
}
";
    std::fs::write(project.path().join("repository.dart"), dart_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of _fetchData
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:_fetchData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));

    // Query for callers of _transformData
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:_transformData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_dart_callees_function() {
    let project = TempDir::new().unwrap();

    let dart_code = r"
void log(String message) {
  print('LOG: $message');
}

void warn(String message) {
  print('WARN: $message');
}

void handleError(String error) {
  log('Error occurred');
  warn(error);
}
";
    std::fs::write(project.path().join("logger.dart"), dart_code).unwrap();

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
// Flutter Widget Tests
// ============================================================================

#[test]
fn cli_dart_flutter_widgets() {
    let project = TempDir::new().unwrap();

    let dart_code = r"
import 'package:flutter/material.dart';

class UserWidget extends StatelessWidget {
  final String name;

  const UserWidget({required this.name});

  @override
  Widget build(BuildContext context) {
    return Text(name);
  }
}

class HomeScreen extends StatelessWidget {
  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: UserWidget(name: 'Alice'),
    );
  }
}
";
    std::fs::write(project.path().join("widgets.dart"), dart_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported widgets
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserWidget")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("widgets.dart"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:HomeScreen")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("widgets.dart"));
}

// ============================================================================
// Async/Await Tests
// ============================================================================

#[test]
fn cli_dart_async_functions() {
    let project = TempDir::new().unwrap();

    let dart_code = r"
import 'dart:async';

Future<String> fetchData() async {
  await Future.delayed(Duration(seconds: 1));
  return 'data';
}

Future<void> processData() async {
  final data = await fetchData();
  print(data);
}
";
    std::fs::write(project.path().join("async.dart"), dart_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported async functions
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:fetchData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("async.dart"));

    // Query for callers of fetchData
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:fetchData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("processData"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_dart_callers_no_results() {
    let project = TempDir::new().unwrap();

    let dart_code = r"
void unusedFunction() {
  print('Never called');
}

void main() {
  print('Hello');
}
";
    std::fs::write(project.path().join("unused.dart"), dart_code).unwrap();

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
