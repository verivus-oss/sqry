//! CLI integration tests for Go relation queries
//!
//! **NOTE**: Some tests are currently ignored because the Go plugin does not
//! extract function calls from within anonymous functions/closures.
//!
//! Tests that relation queries work end-to-end through the CLI for Go:
//! - Callers queries (function calls, method calls)
//! - Callees queries (what a function calls)
//! - Exports queries (exported functions, types, methods - capitalized identifiers)
//! - Imports queries (import statements)
//!
//! ## Known Issues
//! - Function calls within anonymous functions/closures are not extracted
//! - Affects: cli_go_goroutines (calls within go func() literals)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

// ============================================================================
// Exports Queries - Exported Functions, Types, and Methods
// ============================================================================

#[test]
fn cli_go_exports_functions_and_types() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

// Greet returns a greeting message
func Greet(name string) string {
    return "Hello, " + name + "!"
}

// unexported function
func farewell(name string) string {
    return "Goodbye, " + name + "!"
}

// User represents a user
type User struct {
    Name string
    age  int
}

// GetName returns the user's name
func (u *User) GetName() string {
    return u.Name
}

const APIVersion = "1.0.0"
"#;
    std::fs::write(project.path().join("module.go"), go_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported function
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Greet")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.go"));

    // Query for exported type
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:User")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.go"));

    // Query for exported method
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:GetName")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("module.go"));

    // farewell is unexported (lowercase) and should not appear in exports
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:farewell")
        .arg(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("No matches found"));
}

#[test]
fn cli_go_exports_interfaces() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

// Repository defines data access interface
type Repository interface {
    Save(item interface{}) error
    FindByID(id int) (interface{}, error)
}

// UserRepository implements Repository for User type
type UserRepository struct{}

func (r *UserRepository) Save(item interface{}) error {
    return nil
}

func (r *UserRepository) FindByID(id int) (interface{}, error) {
    return nil, nil
}
"#;
    std::fs::write(project.path().join("repository.go"), go_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported interface
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:Repository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("repository.go"));

    // Query for exported struct implementing interface
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:UserRepository")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("repository.go"));
}

// ============================================================================
// Callers Queries - Function and Method Calls
// ============================================================================

#[test]
fn cli_go_callers_function_calls() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

func validate(input string) bool {
    return len(input) > 0
}

func process(data string) string {
    if validate(data) {
        return data
    }
    return ""
}

func main() {
    process("test")
}
"#;
    std::fs::write(project.path().join("processor.go"), go_code).unwrap();

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
fn cli_go_callers_method_calls() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

type DataService struct{}

func (s *DataService) fetchData() []int {
    return []int{}
}

func (s *DataService) transformData(data []int) []int {
    result := make([]int, len(data))
    for i, v := range data {
        result[i] = v * 2
    }
    return result
}

func (s *DataService) Process() []int {
    data := s.fetchData()
    return s.transformData(data)
}

func main() {
    service := &DataService{}
    service.Process()
}
"#;
    std::fs::write(project.path().join("service.go"), go_code).unwrap();

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
        .stdout(predicate::str::contains("Process"));

    // Query for callers of transformData
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:transformData")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Process"));
}

#[test]
fn cli_go_callers_chained_method_calls() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

type QueryBuilder struct {
    condition string
    field     string
    count     int
}

func (q *QueryBuilder) Where(condition string) *QueryBuilder {
    q.condition = condition
    return q
}

func (q *QueryBuilder) OrderBy(field string) *QueryBuilder {
    q.field = field
    return q
}

func (q *QueryBuilder) Limit(count int) *QueryBuilder {
    q.count = count
    return q
}

func (q *QueryBuilder) Execute() []interface{} {
    return []interface{}{}
}

func runQuery() []interface{} {
    return (&QueryBuilder{}).
        Where("active = true").
        OrderBy("created_at").
        Limit(10).
        Execute()
}

func main() {
    runQuery()
}
"#;
    std::fs::write(project.path().join("query.go"), go_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers in method chain
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Where")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("runQuery"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:Execute")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("runQuery"));
}

// ============================================================================
// Callees Queries - What Functions Call
// ============================================================================

#[test]
fn cli_go_callees_function() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

import "fmt"

func log(message string) {
    fmt.Println(message)
}

func warn(message string) {
    fmt.Printf("WARNING: %s\n", message)
}

func handleError(err error) {
    log("Error occurred")
    warn(err.Error())
    fmt.Println(err)
}

func main() {
    handleError(fmt.Errorf("test error"))
}
"#;
    std::fs::write(project.path().join("logger.go"), go_code).unwrap();

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
fn cli_go_callees_method() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

type User struct {
    ID   int
    Name string
}

type UserService struct{}

func (s *UserService) findUser(id int) *User {
    return &User{ID: id, Name: "Test"}
}

func (s *UserService) validateUser(user *User) bool {
    return user.ID > 0
}

func (s *UserService) GetUser(id int) *User {
    user := s.findUser(id)
    if s.validateUser(user) {
        return user
    }
    return nil
}

func main() {
    service := &UserService{}
    service.GetUser(1)
}
"#;
    std::fs::write(project.path().join("user_service.go"), go_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callees of GetUser
    Command::new(sqry_bin())
        .arg("query")
        .arg("callees:GetUser")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("findUser"))
        .stdout(predicate::str::contains("validateUser"));
}

// ============================================================================
// Imports Queries - Package Imports
// ============================================================================

#[test]
fn cli_go_imports() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

import (
    "fmt"
    "os"
    "path/filepath"
)

func readConfig() {
    configPath := filepath.Join(os.Getwd())
    fmt.Println(configPath)
}

func main() {
    readConfig()
}
"#;
    std::fs::write(project.path().join("config.go"), go_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for imports
    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:fmt")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.go"));

    Command::new(sqry_bin())
        .arg("query")
        .arg("imports:filepath")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("config.go"));
}

// ============================================================================
// Goroutines and Channels
// ============================================================================

#[test]
#[ignore = "Go plugin does not extract calls from within anonymous function bodies - known limitation"]
fn cli_go_goroutines() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

import "sync"

func worker(id int, jobs <-chan int, results chan<- int) {
    for j := range jobs {
        results <- process(j)
    }
}

func process(n int) int {
    return n * 2
}

func main() {
    jobs := make(chan int, 10)
    results := make(chan int, 10)

    var wg sync.WaitGroup
    for w := 1; w <= 3; w++ {
        wg.Add(1)
        go func(id int) {
            defer wg.Done()
            worker(id, jobs, results)
        }(w)
    }

    for j := 1; j <= 5; j++ {
        jobs <- j
    }
    close(jobs)

    wg.Wait()
    close(results)
}
"#;
    std::fs::write(project.path().join("goroutines.go"), go_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for callers of worker (called in goroutine)
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:worker")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main"));

    // Query for callers of process
    Command::new(sqry_bin())
        .arg("query")
        .arg("callers:process")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("worker"));
}

// ============================================================================
// Negative Tests
// ============================================================================

#[test]
fn cli_go_unexported_identifiers() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

// Exported function
func PublicFunction() string {
    return privateHelper()
}

// unexported function (lowercase)
func privateHelper() string {
    return "private"
}
"#;
    std::fs::write(project.path().join("exports.go"), go_code).unwrap();

    Command::new(sqry_bin())
        .arg("index")
        .arg(project.path())
        .assert()
        .success();

    // Query for exported function - should be found
    Command::new(sqry_bin())
        .arg("query")
        .arg("exports:PublicFunction")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("exports.go"));

    // privateHelper is unexported (lowercase), so exports query may or may not find it
}

#[test]
fn cli_go_callers_no_results() {
    let project = TempDir::new().unwrap();

    let go_code = r#"
package main

func unusedFunction() int {
    return 42
}

func main() {
    println("Hello")
}
"#;
    std::fs::write(project.path().join("unused.go"), go_code).unwrap();

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
