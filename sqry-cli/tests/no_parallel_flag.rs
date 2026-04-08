// Test for --no-parallel flag (P2-7 Phase 4 enhanced validation)

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use tempfile::TempDir;

/// Test that --no-parallel flag executes queries successfully (determinism verified by property tests in sqry-core)
#[test]
fn test_no_parallel_flag_works() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let test_file = temp_dir.path().join("test.rs");
    let sqry = sqry_bin();

    // Create a test Rust file with multiple symbol types
    std::fs::write(
        &test_file,
        r"
fn function_one() {}
fn function_two() {}
fn function_three() {}

struct StructOne;
struct StructTwo;

enum EnumOne { A, B }
enum EnumTwo { X, Y }

trait TraitOne {}
        ",
    )
    .expect("failed to write test file");

    // Index the directory
    Command::new(&sqry)
        .arg("index")
        .arg(temp_dir.path())
        .assert()
        .success();

    // Test 1: Query with parallel execution (default)
    let parallel_output = Command::new(&sqry)
        .arg("query")
        .arg("kind:function OR kind:struct OR kind:enum")
        .arg(temp_dir.path())
        .assert()
        .success();

    let parallel_stdout = String::from_utf8_lossy(&parallel_output.get_output().stdout);

    // Verify we got results
    assert!(
        parallel_stdout.contains("function_one")
            || parallel_stdout.contains("StructOne")
            || parallel_stdout.contains("EnumOne"),
        "Expected to find symbols in parallel mode output"
    );

    // Test 2: Query with --no-parallel flag
    let sequential_output = Command::new(&sqry)
        .arg("query")
        .arg("kind:function OR kind:struct OR kind:enum")
        .arg(temp_dir.path())
        .arg("--no-parallel")
        .assert()
        .success();

    let sequential_stdout = String::from_utf8_lossy(&sequential_output.get_output().stdout);

    // Verify we got results in sequential mode too
    assert!(
        sequential_stdout.contains("function_one")
            || sequential_stdout.contains("StructOne")
            || sequential_stdout.contains("EnumOne"),
        "Expected to find symbols in sequential mode output"
    );

    // Both modes should return the same symbols (order may differ)
    // Count actual symbol lines in stdout to verify same result set
    // Symbol output format: "path/to/file.rs:line:name"
    let parallel_count = parallel_stdout
        .lines()
        .filter(|line| !line.is_empty() && line.contains(".rs:"))
        .count();
    let sequential_count = sequential_stdout
        .lines()
        .filter(|line| !line.is_empty() && line.contains(".rs:"))
        .count();

    assert_eq!(
        parallel_count, sequential_count,
        "Parallel and sequential modes should return same number of results. Parallel: {parallel_count}, Sequential: {sequential_count}"
    );
}

/// Test that --no-parallel and --session are mutually exclusive
#[test]
fn test_no_parallel_and_session_are_mutually_exclusive() {
    let temp_dir = TempDir::new().expect("failed to create temp dir");
    let test_file = temp_dir.path().join("test.rs");
    let sqry = sqry_bin();

    std::fs::write(
        &test_file,
        r"
fn test_fn() {}
struct TestStruct;
        ",
    )
    .expect("failed to write test file");

    // Index first
    Command::new(&sqry)
        .arg("index")
        .arg(temp_dir.path())
        .assert()
        .success();

    // Query with both --session and --no-parallel should fail
    let output = Command::new(&sqry)
        .arg("query")
        .arg("kind:function OR kind:struct")
        .arg(temp_dir.path())
        .arg("--session")
        .arg("--no-parallel")
        .assert()
        .failure();

    // Verify error message explains mutual exclusivity
    let stderr = String::from_utf8_lossy(&output.get_output().stderr);
    assert!(
        stderr.contains("mutually exclusive"),
        "Expected error message about mutual exclusivity, got: {stderr}"
    );
}
