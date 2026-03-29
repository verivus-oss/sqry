//! CLI integration tests for Scala relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Scala:
//! - Callers queries (function calls, method calls)
//! - Callees queries (what a function calls)
//! - Exports queries (public classes, objects, traits, defs)
//! - Imports queries (import statements)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Public Classes, Objects, Traits
// ============================================================================

#[test]
fn cli_scala_exports_classes_and_objects() {
    let project = TempDir::new().unwrap();

    let scala_code = r#"
package com.example

class User(val name: String, val age: Int) {
  def getName: String = name
}

object UserService {
  def createUser(name: String): User = new User(name, 0)
}

trait Repository {
  def save(item: String): Unit
}
"#;
    std::fs::write(project.path().join("User.scala"), scala_code).unwrap();

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
        .stdout(predicate::str::contains("User.scala"));

    // Query for exported object
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserService")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("User.scala"));

    // Query for exported trait
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Repository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("User.scala"));
}

#[test]
fn cli_scala_exports_hide_private_members() {
    let project = TempDir::new().unwrap();

    let scala_code = r#"
package com.example

class PublicClass {
  def publicMethod(): Unit = {}
  private def privateMethod(): Unit = {}
}

private class PrivateClass {
  def someMethod(): Unit = {}
}

object PublicObject {
  def publicFunction(): Unit = {}
}

private object PrivateObject {
  def hiddenFunction(): Unit = {}
}
"#;
    std::fs::write(project.path().join("Visibility.scala"), scala_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Public class should be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:PublicClass")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("PublicClass"));

    // Public object should be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:PublicObject")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("PublicObject"));

    // Private class should NOT be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:PrivateClass")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));

    // Private object should NOT be exported
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:PrivateObject")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

// ============================================================================
// Imports Queries
// ============================================================================

#[test]
fn cli_scala_imports() {
    let project = TempDir::new().unwrap();

    let scala_code = r#"
import scala.collection.mutable
import scala.collection.mutable.{Buffer, ListBuffer}
import java.util.{List => JavaList}

object DataProcessor {
  def process(data: List[String]): Unit = {
    println(data)
  }
}
"#;
    std::fs::write(project.path().join("Processor.scala"), scala_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for simple import
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:mutable")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Processor.scala"));

    // Query for selective import
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:Buffer")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Processor.scala"));
}

// ============================================================================
// Callers Queries - Function and Method Calls
// ============================================================================

#[test]
fn cli_scala_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let scala_code = r#"
object Calculator {
  def validate(x: Int): Boolean = x > 0

  def process(value: Int): Int = {
    if (validate(value)) {
      value * 2
    } else {
      0
    }
  }

  def analyze(num: Int): String = {
    if (validate(num)) {
      "valid"
    } else {
      "invalid"
    }
  }
}
"#;
    std::fs::write(project.path().join("Calculator.scala"), scala_code).unwrap();

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
fn cli_scala_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let scala_code = r#"
class DataService {
  private def fetchData(): List[Int] = List(1, 2, 3)

  private def transformData(data: List[Int]): List[Int] = {
    data.map(_ * 2)
  }

  def process(): List[Int] = {
    val data = fetchData()
    transformData(data)
  }
}
"#;
    std::fs::write(project.path().join("Service.scala"), scala_code).unwrap();

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
fn cli_scala_callees_function() {
    let project = TempDir::new().unwrap();

    let scala_code = r#"
object Logger {
  def log(message: String): Unit = println(s"LOG: $message")

  def warn(message: String): Unit = println(s"WARN: $message")

  def handleError(error: String): Unit = {
    log("Error occurred")
    warn(error)
  }
}
"#;
    std::fs::write(project.path().join("Logger.scala"), scala_code).unwrap();

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
// Case Classes and Pattern Matching
// ============================================================================

#[test]
fn cli_scala_case_classes() {
    let project = TempDir::new().unwrap();

    let scala_code = r#"
sealed trait Result
case class Success(value: String) extends Result
case class Failure(error: String) extends Result

object ResultHandler {
  def process(result: Result): String = result match {
    case Success(v) => s"Got: $v"
    case Failure(e) => s"Error: $e"
  }
}
"#;
    std::fs::write(project.path().join("Result.scala"), scala_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported case classes
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Success")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Result.scala"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Failure")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Result.scala"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_scala_callers_no_results() {
    let project = TempDir::new().unwrap();

    let scala_code = r#"
object Unused {
  def unusedFunction(): Int = 42
}
"#;
    std::fs::write(project.path().join("Unused.scala"), scala_code).unwrap();

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
