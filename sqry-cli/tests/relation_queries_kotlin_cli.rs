//! CLI integration tests for Kotlin relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Kotlin:
//! - Callers queries (function calls, method calls, extension functions)
//! - Callees queries (what a function calls)
//! - Exports queries (classes, functions, objects)
//! - Imports queries (import statements)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Classes, Functions, Objects
// ============================================================================

#[test]
fn cli_kotlin_exports_classes_and_functions() {
    let project = TempDir::new().unwrap();

    let kotlin_code = r#"
package com.example

fun greet(name: String): String {
    return "Hello, $name!"
}

private fun privateHelper(): Int {
    return 42
}

class User(val name: String, val age: Int) {
    fun getName(): String {
        return name
    }

    private fun validate(): Boolean {
        return name.isNotEmpty()
    }
}

const val API_VERSION = "1.0.0"
"#;
    std::fs::write(project.path().join("Module.kt"), kotlin_code).unwrap();

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
        .stdout(predicate::str::contains("Module.kt"));

    // Query for exported class
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Module.kt"));

    // privateHelper should remain private
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:privateHelper")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_kotlin_exports_objects_and_interfaces() {
    let project = TempDir::new().unwrap();

    let kotlin_code = r#"
package com.example.data

interface Repository<T> {
    fun save(item: T)
    fun findById(id: Int): T?
}

object Database {
    fun connect(): Connection {
        return Connection()
    }
}

class UserRepository : Repository<User> {
    override fun save(user: User) {
        // implementation
    }

    override fun findById(id: Int): User? {
        return null
    }
}

data class User(val id: Int, val name: String)
class Connection
"#;
    std::fs::write(project.path().join("Repository.kt"), kotlin_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for interface
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Repository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Repository.kt"));

    // Query for object
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Database")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Repository.kt"));
}

// ============================================================================
// Callers Queries - Function and Method Calls
// ============================================================================

#[test]
fn cli_kotlin_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let kotlin_code = r#"
fun validate(input: String): Boolean {
    return input.isNotEmpty()
}

fun process(data: String): String? {
    return if (validate(data)) {
        data.trim()
    } else {
        null
    }
}

fun main() {
    process("test")
}
"#;
    std::fs::write(project.path().join("Processor.kt"), kotlin_code).unwrap();

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
fn cli_kotlin_callers_extension_functions() {
    let project = TempDir::new().unwrap();

    let kotlin_code = r#"
fun String.isValidEmail(): Boolean {
    return this.contains("@")
}

fun validateUserEmail(email: String): Boolean {
    return email.isValidEmail()
}

fun main() {
    validateUserEmail("test@example.com")
}
"#;
    std::fs::write(project.path().join("Extensions.kt"), kotlin_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of extension function
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:isValidEmail")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("validateUserEmail"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_kotlin_callees_function() {
    let project = TempDir::new().unwrap();

    let kotlin_code = r#"
fun log(message: String) {
    println(message)
}

fun warn(message: String) {
    println("WARNING: $message")
}

fun handleError(error: Exception) {
    log("Error occurred")
    warn(error.message ?: "Unknown error")
}

fun main() {
    handleError(Exception("Test"))
}
"#;
    std::fs::write(project.path().join("Logger.kt"), kotlin_code).unwrap();

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
fn cli_kotlin_imports() {
    let project = TempDir::new().unwrap();

    let kotlin_code = r#"
package com.example

import java.io.File
import java.io.IOException
import kotlin.collections.List

fun readConfig(): String {
    val file = File("config.txt")
    return try {
        file.readText()
    } catch (e: IOException) {
        ""
    }
}

fun main() {
    readConfig()
}
"#;
    std::fs::write(project.path().join("Config.kt"), kotlin_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:File")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Config.kt"));
}

// ============================================================================
// Coroutines and Suspend Functions
// ============================================================================

#[test]
fn cli_kotlin_coroutines() {
    let project = TempDir::new().unwrap();

    let kotlin_code = r#"
import kotlinx.coroutines.*

suspend fun fetchUser(id: Int): User {
    delay(100)
    return User(id, "Test")
}

suspend fun fetchPosts(userId: Int): List<Post> {
    delay(100)
    return emptyList()
}

suspend fun getUserData(id: Int): UserData {
    val user = fetchUser(id)
    val posts = fetchPosts(user.id)
    return UserData(user, posts)
}

data class User(val id: Int, val name: String)
data class Post(val id: Int, val title: String)
data class UserData(val user: User, val posts: List<Post>)

fun main() = runBlocking {
    getUserData(1)
}
"#;
    std::fs::write(project.path().join("Async.kt"), kotlin_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of suspend function
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
fn cli_kotlin_private_functions() {
    let project = TempDir::new().unwrap();

    let kotlin_code = r#"
class Service {
    fun execute() {
        validate()
    }

    private fun validate() {
        // private function
    }
}
"#;
    std::fs::write(project.path().join("Service.kt"), kotlin_code).unwrap();

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
fn cli_kotlin_callers_no_results() {
    let project = TempDir::new().unwrap();

    let kotlin_code = r#"
fun unusedFunction(): Int {
    return 42
}

fun main() {
    println("Hello")
}
"#;
    std::fs::write(project.path().join("Unused.kt"), kotlin_code).unwrap();

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
