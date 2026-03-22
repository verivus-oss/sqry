//! Integration tests for P1-17 hard limit enforcement
//!
//! Validates that git output limits and mmap thresholds work correctly
//! via environment variables and remain independent of each other.

use serial_test::serial;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

mod common;
use common::sqry_bin;

/// Helper to initialize a git repository
fn init_git_repo(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["init"])
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    // Configure git (required for commits)
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "user.name", "Test User"])
        .output()?;
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "user.email", "test@example.com"])
        .output()?;
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "commit.gpgsign", "false"])
        .output()?;

    Ok(())
}

/// Helper to create and commit a file
fn create_and_commit_file(
    dir: &Path,
    filename: &str,
    content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(dir.join(filename), content)?;
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["add", filename])
        .output()?;
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-m", &format!("Add {filename}")])
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

/// Scenario 1: Git operations with reasonable limits succeed
#[test]
#[serial]
fn test_git_operations_within_limits() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Set conservative git output limit (20MB)
    unsafe {
        std::env::set_var("SQRY_GIT_MAX_OUTPUT_SIZE", "20971520"); // 20MB
    }

    // Initialize git repo with test files
    init_git_repo(temp_dir.path()).expect("Failed to init git repo");
    create_and_commit_file(
        temp_dir.path(),
        "test.rs",
        "fn main() { println!(\"Hello\"); }",
    )
    .expect("Failed to create test file");

    // Run sqry index - should succeed with reasonable git output
    let output = Command::new(sqry_bin())
        .arg("index")
        .arg(temp_dir.path())
        .output()
        .expect("Failed to run sqry index");

    assert!(
        output.status.success(),
        "sqry index should succeed with reasonable git output limit. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    unsafe {
        std::env::remove_var("SQRY_GIT_MAX_OUTPUT_SIZE");
    }
}

/// Scenario 8: All limits can be set independently without conflicts
#[test]
#[serial]
fn test_all_limits_independent() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Set ALL limits simultaneously (P1-17 new + existing limits)
    unsafe {
        std::env::set_var("SQRY_GIT_MAX_OUTPUT_SIZE", "52428800"); // 50MB (P1-17 new)
        std::env::set_var("SQRY_MMAP_THRESHOLD", "5242880"); // 5MB (P1-17 new)
        std::env::set_var("SQRY_READ_BUFFER", "16384"); // 16KB (P1-14 existing)
        std::env::set_var("SQRY_WRITE_BUFFER", "16384"); // 16KB (P1-14 existing)
        std::env::set_var("SQRY_PARSE_BUFFER", "131072"); // 128KB (P1-14 existing)
    }

    // Initialize git repo with test files
    init_git_repo(temp_dir.path()).expect("Failed to init git repo");
    create_and_commit_file(
        temp_dir.path(),
        "main.rs",
        r#"
fn calculate_sum(numbers: &[i32]) -> i32 {
    numbers.iter().sum()
}

fn main() {
    let nums = vec![1, 2, 3, 4, 5];
    println!("Sum: {}", calculate_sum(&nums));
}
        "#,
    )
    .expect("Failed to create test file");

    // Run sqry index with all limits set
    let output = Command::new(sqry_bin())
        .arg("index")
        .arg(temp_dir.path())
        .output()
        .expect("Failed to run sqry index");

    // All limits should be independent and not conflict
    // The key validation: indexing succeeds with all limits set simultaneously
    assert!(
        output.status.success(),
        "sqry index should succeed with all limits set independently. \
         This validates that P1-17 limits (git output, mmap threshold) are \
         independent of P1-14 limits (buffers). stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify graph snapshot was created (indicates successful indexing)
    // Note: Since v2.0.0, sqry uses unified graph format in .sqry/graph/snapshot.sqry
    let graph_snapshot = temp_dir.path().join(".sqry/graph/snapshot.sqry");
    assert!(
        graph_snapshot.exists(),
        "Graph snapshot should exist after successful indexing with all limits set"
    );

    // Cleanup
    unsafe {
        std::env::remove_var("SQRY_GIT_MAX_OUTPUT_SIZE");
        std::env::remove_var("SQRY_MMAP_THRESHOLD");
        std::env::remove_var("SQRY_READ_BUFFER");
        std::env::remove_var("SQRY_WRITE_BUFFER");
        std::env::remove_var("SQRY_PARSE_BUFFER");
    }
}

/// Validate that git output limit respects environment variable
#[test]
#[serial]
fn test_git_output_limit_env_var_respected() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Set very generous limit
    unsafe {
        std::env::set_var("SQRY_GIT_MAX_OUTPUT_SIZE", "104857600"); // 100MB
    }

    init_git_repo(temp_dir.path()).expect("Failed to init git repo");
    create_and_commit_file(
        temp_dir.path(),
        "lib.rs",
        "pub fn add(a: i32, b: i32) -> i32 { a + b }",
    )
    .expect("Failed to create test file");

    // Should succeed with generous limit
    let output = Command::new(sqry_bin())
        .arg("index")
        .arg(temp_dir.path())
        .output()
        .expect("Failed to run sqry index");

    assert!(
        output.status.success(),
        "Index should succeed with generous git output limit. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    unsafe {
        std::env::remove_var("SQRY_GIT_MAX_OUTPUT_SIZE");
    }
}

/// Validate that mmap threshold respects environment variable
#[test]
#[serial]
fn test_mmap_threshold_env_var_respected() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");

    // Set custom mmap threshold (5MB instead of default 10MB)
    unsafe {
        std::env::set_var("SQRY_MMAP_THRESHOLD", "5242880"); // 5MB
    }

    // Create test file larger than 5MB to trigger mmap
    let large_content = "// ".to_string() + &"x".repeat(6 * 1024 * 1024); // 6MB comment
    fs::write(temp_dir.path().join("large.rs"), large_content)
        .expect("Failed to create large file");

    // Run sqry index - should use mmap for files >5MB with custom threshold
    let output = Command::new(sqry_bin())
        .arg("index")
        .arg(temp_dir.path())
        .output()
        .expect("Failed to run sqry index");

    assert!(
        output.status.success(),
        "Index should succeed with custom mmap threshold. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    unsafe {
        std::env::remove_var("SQRY_MMAP_THRESHOLD");
    }
}
