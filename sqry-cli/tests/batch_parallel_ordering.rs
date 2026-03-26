//! Batch command parallel execution ordering tests (P2-7 Phase 2)
//!
//! Verifies that parallel batch execution produces the same deterministic
//! ordering as sequential execution.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// Create a test workspace with Rust source files
fn setup_test_workspace() -> TempDir {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let src_path = temp_dir.path().join("src");
    fs::create_dir(&src_path).expect("Failed to create src dir");

    // Create multiple files with various symbols
    fs::write(
        src_path.join("lib.rs"),
        r#"
pub fn hello_world() -> String {
    "Hello, world!".to_string()
}

pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

pub struct User {
    pub name: String,
    pub age: u32,
}

impl User {
    pub fn new(name: String, age: u32) -> Self {
        User { name, age }
    }

    pub fn display(&self) -> String {
        format!("{} ({})", self.name, self.age)
    }
}
"#,
    )
    .expect("Failed to write lib.rs");

    fs::write(
        src_path.join("utils.rs"),
        r#"
pub fn calculate(x: i32, y: i32) -> i32 {
    x + y
}

pub fn multiply(x: i32, y: i32) -> i32 {
    x * y
}

pub struct Calculator {
    pub result: i32,
}

impl Calculator {
    pub fn add(&mut self, value: i32) {
        self.result += value;
    }
}
"#,
    )
    .expect("Failed to write utils.rs");

    temp_dir
}

/// Create a batch query file
fn create_batch_queries(path: &PathBuf) {
    let queries = [
        "kind:function",
        "kind:struct",
        "kind:method",
        "kind:function AND name~=hello",
        "kind:function AND name~=calculate",
        "kind:function AND name~=multiply",
        "kind:struct AND name:User",
        "kind:struct AND name:Calculator",
    ];

    fs::write(path, queries.join("\n")).expect("Failed to write batch queries");
}

/// Test that batch parallel and sequential modes produce same ordering.
#[test]
fn test_batch_parallel_vs_sequential_ordering() {
    // Setup workspace
    let workspace = setup_test_workspace();
    let workspace_path = workspace.path();

    // Create index
    let sqry = sqry_bin();
    Command::new(&sqry)
        .arg("index")
        .arg(workspace_path)
        .assert()
        .success();

    // Create batch queries file
    let queries_file = workspace_path.join("queries.txt");
    create_batch_queries(&queries_file);

    // Run batch in default (parallel) mode with JSON output for easier comparison
    let parallel_output = Command::new(&sqry)
        .arg("batch")
        .arg(workspace_path)
        .arg("--queries")
        .arg(&queries_file)
        .arg("--output")
        .arg("json")
        .output()
        .expect("Failed to run batch (parallel)");

    assert!(
        parallel_output.status.success(),
        "Parallel batch failed: {}",
        String::from_utf8_lossy(&parallel_output.stderr)
    );

    // Run batch in sequential mode with JSON output
    let sequential_output = Command::new(&sqry)
        .arg("batch")
        .arg(workspace_path)
        .arg("--queries")
        .arg(&queries_file)
        .arg("--output")
        .arg("json")
        .arg("--sequential")
        .output()
        .expect("Failed to run batch (sequential)");

    assert!(
        sequential_output.status.success(),
        "Sequential batch failed: {}",
        String::from_utf8_lossy(&sequential_output.stderr)
    );

    // Parse JSON outputs
    let parallel_json: serde_json::Value =
        serde_json::from_slice(&parallel_output.stdout).expect("Failed to parse parallel JSON");
    let sequential_json: serde_json::Value =
        serde_json::from_slice(&sequential_output.stdout).expect("Failed to parse sequential JSON");

    // Verify ordering is preserved (ignore timing differences which are expected)
    let parallel_queries = parallel_json["queries"]
        .as_array()
        .expect("Missing queries array in parallel output");
    let sequential_queries = sequential_json["queries"]
        .as_array()
        .expect("Missing queries array in sequential output");

    assert_eq!(
        parallel_queries.len(),
        sequential_queries.len(),
        "Different number of results between parallel and sequential"
    );

    // Compare each query result (ignoring timing fields)
    for (idx, (parallel_result, sequential_result)) in parallel_queries
        .iter()
        .zip(sequential_queries.iter())
        .enumerate()
    {
        // Verify positions match
        assert_eq!(
            parallel_result["position"], sequential_result["position"],
            "Position mismatch at index {}",
            idx
        );

        // Verify query strings match
        assert_eq!(
            parallel_result["query"],
            sequential_result["query"],
            "Query mismatch at position {}",
            idx + 1
        );

        // Verify result counts match
        assert_eq!(
            parallel_result["result_count"],
            sequential_result["result_count"],
            "Result count mismatch at position {}",
            idx + 1
        );

        // Verify actual results match (ignoring metadata variations)
        let parallel_results = parallel_result["results"]
            .as_array()
            .expect("Missing results array");
        let sequential_results = sequential_result["results"]
            .as_array()
            .expect("Missing results array");

        assert_eq!(
            parallel_results.len(),
            sequential_results.len(),
            "Different result lengths at position {}",
            idx + 1
        );

        // Verify symbol names and types match (as sets, since ordering within results may vary)
        let mut parallel_symbols: Vec<(String, String)> = parallel_results
            .iter()
            .map(|r| {
                (
                    r["name"].as_str().unwrap().to_string(),
                    r["kind"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let mut sequential_symbols: Vec<(String, String)> = sequential_results
            .iter()
            .map(|r| {
                (
                    r["name"].as_str().unwrap().to_string(),
                    r["kind"].as_str().unwrap().to_string(),
                )
            })
            .collect();

        // Sort to compare as sets (ordering within a single query's results may vary)
        parallel_symbols.sort();
        sequential_symbols.sort();

        assert_eq!(
            parallel_symbols,
            sequential_symbols,
            "Different result sets at query position {}",
            idx + 1
        );
    }

    // Verify that we have results for all queries
    assert_eq!(
        parallel_queries.len(),
        8,
        "Expected 8 query results, got {}",
        parallel_queries.len()
    );

    // Verify each result has a position field in order
    for (idx, result) in parallel_queries.iter().enumerate() {
        let position = result["position"]
            .as_u64()
            .expect("Missing or invalid position field");
        assert_eq!(
            position as usize,
            idx + 1,
            "Position mismatch at index {}: expected {}, got {}",
            idx,
            idx + 1,
            position
        );
    }
}

/// Test that --sequential flag works correctly.
#[test]
fn test_batch_sequential_flag_works() {
    // Setup workspace
    let workspace = setup_test_workspace();
    let workspace_path = workspace.path();

    // Create index
    let sqry = sqry_bin();
    Command::new(&sqry)
        .arg("index")
        .arg(workspace_path)
        .assert()
        .success();

    // Create batch queries file with just 3 queries
    let queries_file = workspace_path.join("queries.txt");
    fs::write(&queries_file, "kind:function\nkind:struct\nkind:method\n")
        .expect("Failed to write batch queries");

    // Run with --sequential flag
    let output = Command::new(&sqry)
        .arg("batch")
        .arg(workspace_path)
        .arg("--queries")
        .arg(&queries_file)
        .arg("--sequential")
        .output()
        .expect("Failed to run batch with --sequential");

    assert!(
        output.status.success(),
        "Batch with --sequential failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify stderr contains progress messages in order
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[1/3]"), "Missing [1/3] progress message");
    assert!(stderr.contains("[2/3]"), "Missing [2/3] progress message");
    assert!(stderr.contains("[3/3]"), "Missing [3/3] progress message");
}
