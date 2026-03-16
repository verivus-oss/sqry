//! CLI integration tests for Rust relation queries
//!
//! Tests that relation queries work end-to-end through the CLI for Rust:
//! - Callers queries (function calls, method calls, trait method calls)
//! - Callees queries (what a function calls)
//! - Exports queries (pub functions, structs, traits, enums)
//! - Imports queries (use statements)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Public Functions, Structs, Traits, Enums
// ============================================================================

#[test]
fn cli_rust_exports_functions_and_structs() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

fn private_helper() -> i32 {
    42
}

pub struct User {
    name: String,
    age: u32,
}

impl User {
    pub fn new(name: String, age: u32) -> Self {
        User { name, age }
    }

    pub fn get_name(&self) -> &str {
        &self.name
    }

    fn validate(&self) -> bool {
        !self.name.is_empty()
    }
}

pub const API_VERSION: &str = "1.0.0";
"#;
    std::fs::write(project.path().join("lib.rs"), rust_code).unwrap();

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
        .stdout(predicate::str::contains("lib.rs"));

    // Query for exported struct
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("lib.rs"));

    // Query for public method
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:get_name")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("lib.rs"));

    // Query for private helper (should not appear)
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:private_helper")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_rust_exports_traits_and_enums() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
pub trait Repository {
    fn save(&self, item: &str);
    fn find_by_id(&self, id: u32) -> Option<String>;
}

pub enum Status {
    Active,
    Inactive,
    Pending,
}

pub struct UserRepository;

impl Repository for UserRepository {
    fn save(&self, item: &str) {
        // implementation
    }

    fn find_by_id(&self, id: u32) -> Option<String> {
        None
    }
}
"#;
    std::fs::write(project.path().join("repository.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported trait
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Repository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("repository.rs"));

    // Query for exported enum
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Status")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("repository.rs"));
}

// ============================================================================
// Callers Queries - Function and Method Calls
// ============================================================================

#[test]
fn cli_rust_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
fn validate(input: &str) -> bool {
    !input.is_empty()
}

fn process(data: &str) -> Option<String> {
    if validate(data) {
        Some(data.trim().to_string())
    } else {
        None
    }
}

fn main() {
    process("test");
}
"#;
    std::fs::write(project.path().join("main.rs"), rust_code).unwrap();

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
fn cli_rust_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
struct DataService {
    data: Vec<i32>,
}

impl DataService {
    fn fetch_data(&self) -> Vec<i32> {
        self.data.clone()
    }

    fn transform_data(&self, data: &[i32]) -> Vec<i32> {
        data.iter().map(|x| x * 2).collect()
    }

    fn process(&self) -> Vec<i32> {
        let data = self.fetch_data();
        self.transform_data(&data)
    }
}

fn main() {
    let service = DataService { data: vec![1, 2, 3] };
    service.process();
}
"#;
    std::fs::write(project.path().join("service.rs"), rust_code).unwrap();

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
fn cli_rust_callers_chained_method_calls() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
struct QueryBuilder {
    condition: String,
    field: String,
    limit: usize,
}

impl QueryBuilder {
    fn new() -> Self {
        QueryBuilder {
            condition: String::new(),
            field: String::new(),
            limit: 0,
        }
    }

    fn where_clause(mut self, condition: &str) -> Self {
        self.condition = condition.to_string();
        self
    }

    fn order_by(mut self, field: &str) -> Self {
        self.field = field.to_string();
        self
    }

    fn limit(mut self, count: usize) -> Self {
        self.limit = count;
        self
    }

    fn execute(self) -> Vec<String> {
        vec![]
    }
}

fn run_query() -> Vec<String> {
    QueryBuilder::new()
        .where_clause("active = true")
        .order_by("created_at")
        .limit(10)
        .execute()
}

fn main() {
    run_query();
}
"#;
    std::fs::write(project.path().join("query.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers in method chain
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:where_clause")
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
fn cli_rust_callees_function() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
fn log(message: &str) {
    println!("{}", message);
}

fn warn(message: &str) {
    eprintln!("WARNING: {}", message);
}

fn handle_error(error: &str) {
    log("Error occurred");
    warn(error);
    println!("{}", error);
}

fn main() {
    handle_error("test error");
}
"#;
    std::fs::write(project.path().join("logger.rs"), rust_code).unwrap();

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
fn cli_rust_callees_method() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
struct User {
    id: u32,
    name: String,
}

struct UserService;

impl UserService {
    fn find_user(&self, id: u32) -> User {
        User { id, name: "Test".to_string() }
    }

    fn validate_user(&self, user: &User) -> bool {
        user.id > 0
    }

    fn get_user(&self, id: u32) -> Option<User> {
        let user = self.find_user(id);
        if self.validate_user(&user) {
            Some(user)
        } else {
            None
        }
    }
}

fn main() {
    let service = UserService;
    service.get_user(1);
}
"#;
    std::fs::write(project.path().join("user_service.rs"), rust_code).unwrap();

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
// Imports Queries - Use Statements
// ============================================================================

#[test]
fn cli_rust_imports() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
use std::fs;
use std::path::PathBuf;
use std::io::Read;

fn read_config() -> String {
    let path = PathBuf::from("config.txt");
    fs::read_to_string(path).unwrap_or_default()
}

fn main() {
    read_config();
}
"#;
    std::fs::write(project.path().join("config.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:fs")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.rs"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:PathBuf")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.rs"));
}

// ============================================================================
// Trait Methods and Generics
// ============================================================================

#[test]
fn cli_rust_trait_method_calls() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
trait Processor {
    fn process(&self, input: &str) -> String;
}

struct TextProcessor;

impl Processor for TextProcessor {
    fn process(&self, input: &str) -> String {
        self.transform(input)
    }
}

impl TextProcessor {
    fn transform(&self, input: &str) -> String {
        input.to_uppercase()
    }
}

fn run_processor<T: Processor>(processor: &T, data: &str) -> String {
    processor.process(data)
}

fn main() {
    let proc = TextProcessor;
    run_processor(&proc, "test");
}
"#;
    std::fs::write(project.path().join("processor.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of transform
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:transform")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("process"));

    // Query for callers of process (trait method)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:process")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("run_processor"));
}

#[test]
fn cli_rust_async_await() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
async fn fetch_user(id: u32) -> User {
    User { id, name: "Test".to_string() }
}

async fn fetch_posts(user_id: u32) -> Vec<Post> {
    vec![]
}

async fn get_user_data(id: u32) -> UserData {
    let user = fetch_user(id).await;
    let posts = fetch_posts(user.id).await;
    UserData { user, posts }
}

struct User {
    id: u32,
    name: String,
}

struct Post {
    id: u32,
    title: String,
}

struct UserData {
    user: User,
    posts: Vec<Post>,
}

#[tokio::main]
async fn main() {
    get_user_data(1).await;
}
"#;
    std::fs::write(project.path().join("async_code.rs"), rust_code).unwrap();

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

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_rust_private_functions() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
pub fn public_api() -> i32 {
    private_helper()
}

fn private_helper() -> i32 {
    42
}
"#;
    std::fs::write(project.path().join("api.rs"), rust_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for public export
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:public_api")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("api.rs"));

    // private_helper is not pub, so exports query may or may not find it
}

#[test]
fn cli_rust_callers_no_results() {
    let project = TempDir::new().unwrap();

    let rust_code = r#"
fn unused_function() -> i32 {
    42
}

fn main() {
    println!("Hello");
}
"#;
    std::fs::write(project.path().join("unused.rs"), rust_code).unwrap();

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
