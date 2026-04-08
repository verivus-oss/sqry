//! Integration tests for P1-11 git-aware index updates
//!
//! These tests verify that `sqry update` correctly leverages git change tracking
//! for incremental builds, with graceful fallback when git is unavailable.
//!
//! # Implementation Notes
//!
//! The git-aware update feature tracks the last indexed commit SHA in the graph
//! manifest (`manifest.json`). This enables:
//! - Git-aware mode: Uses git to detect changed files when a commit is available
//! - Hash-based mode: Falls back to file hash comparison when git is unavailable
//!
//! Environment variables:
//! - `SQRY_GIT_BACKEND=none`: Force hash-based mode even in git repositories
//! - `SQRY_GIT_INCLUDE_UNTRACKED`: Control whether untracked files are indexed

use serial_test::serial;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

mod common;
use common::sqry_bin;

/// Helper to initialize a git repository in a directory
fn init_git_repo(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Initialize git repo
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

    // Configure git user (required for commits)
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

    // Disable commit signing (prevents gitsign OAuth issues in tests)
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["config", "commit.gpgsign", "false"])
        .output()?;

    Ok(())
}

/// Helper to create and commit a Rust file
fn create_and_commit_file(
    dir: &Path,
    filename: &str,
    content: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let file_path = dir.join(filename);
    fs::write(&file_path, content)?;

    // Add to git
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["add", filename])
        .output()?;

    // Commit
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

/// Helper to run sqry index command
fn run_sqry_index(dir: &Path, force: bool) -> Result<String, Box<dyn std::error::Error>> {
    let mut cmd = Command::new(sqry_bin());
    cmd.arg("index").arg(dir);

    if force {
        cmd.arg("--force");
    }

    let output = cmd.output()?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr))
}

/// Helper to run sqry update command
fn run_sqry_update(dir: &Path) -> Result<(String, bool), Box<dyn std::error::Error>> {
    let output = Command::new(sqry_bin()).arg("update").arg(dir).output()?;

    let stdout_stderr = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);

    Ok((stdout_stderr, output.status.success()))
}

/// Helper to check if git is available
fn is_git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Helper to get current HEAD commit SHA
fn get_head_commit(dir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()?;

    if !output.status.success() {
        return Err("Failed to get HEAD commit".into());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Helper to read index metadata and check `last_indexed_commit`
///
/// Reads the graph manifest to get the last indexed commit SHA.
fn get_last_indexed_commit(dir: &Path) -> Result<Option<String>, Box<dyn std::error::Error>> {
    use sqry_core::graph::unified::persistence::GraphStorage;

    let storage = GraphStorage::new(dir);
    if !storage.exists() {
        return Ok(None);
    }

    let manifest = storage.load_manifest()?;
    Ok(manifest.last_indexed_commit)
}

// ============================================================================
// BASIC FUNCTIONALITY TESTS
// ============================================================================

#[test]
#[serial]
fn test_git_aware_update_single_file_change() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    // Initialize git repo
    init_git_repo(repo_path)?;

    // Create initial file and commit
    create_and_commit_file(repo_path, "main.rs", "fn main() { println!(\"v1\"); }")?;

    // Build initial index
    let output = run_sqry_index(repo_path, false)?;
    assert!(output.contains("Index built successfully") || output.contains("indexed"));

    // Verify baseline commit was recorded
    let initial_commit = get_head_commit(repo_path)?;
    let indexed_commit = get_last_indexed_commit(repo_path)?;
    assert_eq!(indexed_commit, Some(initial_commit.clone()));

    // Modify file and commit
    create_and_commit_file(repo_path, "main.rs", "fn main() { println!(\"v2\"); }")?;

    // Update index (should use git-aware mode)
    let (update_output, success) = run_sqry_update(repo_path)?;
    assert!(success, "Update should succeed");

    // Verify it detected the change
    assert!(
        update_output.contains("updated") || update_output.contains("Updated"),
        "Output should indicate files were updated: {update_output}"
    );

    // Verify baseline commit was updated to new HEAD
    let new_commit = get_head_commit(repo_path)?;
    let new_indexed_commit = get_last_indexed_commit(repo_path)?;
    assert_eq!(new_indexed_commit, Some(new_commit));
    assert_ne!(Some(initial_commit), new_indexed_commit);

    Ok(())
}

#[test]
#[serial]
fn test_git_aware_handles_renames() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_git_repo(repo_path)?;

    // Create and commit initial file
    create_and_commit_file(
        repo_path,
        "old_name.rs",
        "fn old_function() { println!(\"hello\"); }",
    )?;

    // Build index
    run_sqry_index(repo_path, false)?;

    // Rename file using git mv
    Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["mv", "old_name.rs", "new_name.rs"])
        .output()?;

    // Commit rename
    Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["commit", "-m", "Rename file"])
        .output()?;

    // Update index
    let (update_output, success) = run_sqry_update(repo_path)?;
    assert!(success);

    // Verify rename was detected and processed
    assert!(
        update_output.contains("updated") || update_output.contains("Updated"),
        "Should detect rename as change: {update_output}"
    );

    Ok(())
}

#[test]
#[serial]
fn test_full_build_populates_baseline() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_git_repo(repo_path)?;
    create_and_commit_file(repo_path, "test.rs", "fn test() {}")?;

    // Build index
    run_sqry_index(repo_path, false)?;

    // Verify baseline commit was recorded
    let head = get_head_commit(repo_path)?;
    let baseline = get_last_indexed_commit(repo_path)?;

    assert_eq!(
        baseline,
        Some(head),
        "Full build should record HEAD as baseline"
    );

    Ok(())
}

#[test]
#[serial]
fn test_uncommitted_changes_detection() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_git_repo(repo_path)?;
    create_and_commit_file(repo_path, "main.rs", "fn main() {}")?;

    // Build index
    run_sqry_index(repo_path, false)?;

    // Modify file WITHOUT committing
    fs::write(
        repo_path.join("main.rs"),
        "fn main() { println!(\"modified\"); }",
    )?;

    // Update should detect uncommitted change
    let (update_output, success) = run_sqry_update(repo_path)?;
    assert!(success);

    assert!(
        update_output.contains("updated") || update_output.contains("Updated"),
        "Should detect uncommitted changes: {update_output}"
    );

    Ok(())
}

#[test]
#[serial]
fn test_empty_changeset() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_git_repo(repo_path)?;
    create_and_commit_file(repo_path, "main.rs", "fn main() {}")?;

    // Build index
    run_sqry_index(repo_path, false)?;

    // Update without any changes
    let (update_output, success) = run_sqry_update(repo_path)?;
    assert!(success);

    // Should complete quickly with no changes
    assert!(
        update_output.contains("unchanged") || update_output.contains("successfully"),
        "Empty changeset should complete successfully: {update_output}"
    );

    Ok(())
}

// ============================================================================
// FALLBACK TESTS
// ============================================================================

#[test]
#[serial]
fn test_fallback_when_not_git_repo() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    // Create file WITHOUT initializing git
    fs::write(repo_path.join("main.rs"), "fn main() {}")?;

    // Build index (should work without git)
    let output = run_sqry_index(repo_path, false)?;
    assert!(output.contains("Index built successfully") || output.contains("indexed"));

    // Modify file
    fs::write(repo_path.join("main.rs"), "fn main() { println!(\"v2\"); }")?;

    // Update should fall back to hash-based
    let (update_output, success) = run_sqry_update(repo_path)?;
    assert!(success);

    assert!(
        update_output.contains("hash-based") || update_output.contains("updated"),
        "Should fall back to hash-based when not a git repo: {update_output}"
    );

    Ok(())
}

#[test]
#[serial]
fn test_repo_without_commits() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    // Initialize git repo but DON'T commit anything (HEAD-less)
    init_git_repo(repo_path)?;

    // Create file without committing
    fs::write(repo_path.join("main.rs"), "fn main() {}")?;

    // Build index (should work, baseline will be None)
    let output = run_sqry_index(repo_path, false)?;
    assert!(output.contains("Index built successfully") || output.contains("indexed"));

    // Verify baseline is None (no commits)
    let baseline = get_last_indexed_commit(repo_path)?;
    assert_eq!(baseline, None, "HEAD-less repo should have no baseline");

    // Update should fall back to hash-based
    fs::write(repo_path.join("main.rs"), "fn main() { println!(\"v2\"); }")?;

    let (update_output, success) = run_sqry_update(repo_path)?;
    assert!(success);

    assert!(
        update_output.contains("updated") || update_output.contains("hash-based"),
        "HEAD-less repo should fall back to hash-based: {update_output}"
    );

    Ok(())
}

// ============================================================================
// ENVIRONMENT VARIABLE TESTS
// ============================================================================

#[test]
#[serial]
fn test_untracked_toggle() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_git_repo(repo_path)?;
    create_and_commit_file(repo_path, "main.rs", "fn main() {}")?;

    // Build index
    run_sqry_index(repo_path, false)?;

    // Create NEW untracked file (not added to git)
    fs::write(repo_path.join("new.rs"), "fn new() {}")?;

    // Test 1: With SQRY_GIT_INCLUDE_UNTRACKED=1 (default), should include untracked
    let output1 = Command::new(sqry_bin())
        .arg("update")
        .arg(repo_path)
        .env("SQRY_GIT_INCLUDE_UNTRACKED", "1")
        .output()?;

    let output1_str = String::from_utf8_lossy(&output1.stdout).to_string()
        + &String::from_utf8_lossy(&output1.stderr);

    assert!(
        output1_str.contains("updated") || output1_str.contains("Updated"),
        "With SQRY_GIT_INCLUDE_UNTRACKED=1, should index untracked files"
    );

    // Force rebuild for next test
    run_sqry_index(repo_path, true)?;

    // Create another untracked file
    fs::write(repo_path.join("another.rs"), "fn another() {}")?;

    // Test 2: With SQRY_GIT_INCLUDE_UNTRACKED=0, should ignore untracked
    let output2 = Command::new(sqry_bin())
        .arg("update")
        .arg(repo_path)
        .env("SQRY_GIT_INCLUDE_UNTRACKED", "0")
        .output()?;

    assert!(
        output2.status.success(),
        "Update should succeed even with SQRY_GIT_INCLUDE_UNTRACKED=0"
    );

    Ok(())
}

#[test]
#[serial]
fn test_git_backend_none() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_git_repo(repo_path)?;
    create_and_commit_file(repo_path, "main.rs", "fn main() {}")?;

    // Build index
    run_sqry_index(repo_path, false)?;

    // Modify and commit
    create_and_commit_file(repo_path, "main.rs", "fn main() { println!(\"v2\"); }")?;

    // Update with SQRY_GIT_BACKEND=none (force hash-based)
    let output = Command::new(sqry_bin())
        .arg("update")
        .arg(repo_path)
        .env("SQRY_GIT_BACKEND", "none")
        .output()?;

    assert!(output.status.success());

    let output_str = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);

    // Should not use git-aware mode when explicitly disabled
    assert!(
        output_str.contains("hash-based") || output_str.contains("updated"),
        "SQRY_GIT_BACKEND=none should force hash-based mode"
    );

    Ok(())
}

// ============================================================================
// EDGE CASE TESTS
// ============================================================================

#[test]
#[serial]
fn test_rename_case_change() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_git_repo(repo_path)?;
    create_and_commit_file(repo_path, "Main.rs", "fn main() {}")?;

    // Build index
    run_sqry_index(repo_path, false)?;

    // Rename with only case change (Main.rs -> main.rs)
    // Note: This may behave differently on case-insensitive filesystems
    Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["mv", "Main.rs", "main.rs"])
        .output()?;

    Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["commit", "-m", "Change case"])
        .output()?;

    // Update should handle case change
    let (update_output, success) = run_sqry_update(repo_path)?;
    assert!(success, "Case-change rename should be handled");

    assert!(
        update_output.contains("updated") || update_output.contains("successfully"),
        "Case change should be detected: {update_output}"
    );

    Ok(())
}

#[test]
#[serial]
fn test_multiple_files_changed() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_git_repo(repo_path)?;

    // Create multiple files
    for i in 1..=10 {
        create_and_commit_file(
            repo_path,
            &format!("file{i}.rs"),
            &format!("fn func{i}() {{}}"),
        )?;
    }

    // Build index
    run_sqry_index(repo_path, false)?;

    // Modify 5 files
    for i in 1..=5 {
        fs::write(
            repo_path.join(format!("file{i}.rs")),
            format!("fn func{i}() {{ println!(\"modified\"); }}"),
        )?;
    }

    // Commit changes
    Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["add", "."])
        .output()?;

    Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["commit", "-m", "Modify 5 files"])
        .output()?;

    // Update should detect all 5 changes
    let (update_output, success) = run_sqry_update(repo_path)?;
    assert!(success);

    assert!(
        update_output.contains("updated") || update_output.contains("Updated"),
        "Should detect multiple file changes: {update_output}"
    );

    Ok(())
}

#[test]
#[serial]
fn test_deleted_file() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_git_repo(repo_path)?;
    create_and_commit_file(repo_path, "to_delete.rs", "fn delete_me() {}")?;
    create_and_commit_file(repo_path, "keep.rs", "fn keep() {}")?;

    // Build index
    run_sqry_index(repo_path, false)?;

    // Delete file
    Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rm", "to_delete.rs"])
        .output()?;

    Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["commit", "-m", "Delete file"])
        .output()?;

    // Update should handle deletion
    let (update_output, success) = run_sqry_update(repo_path)?;
    assert!(success);

    assert!(
        update_output.contains("removed")
            || update_output.contains("updated")
            || update_output.contains("successfully"),
        "Should handle file deletion: {update_output}"
    );

    Ok(())
}

#[test]
#[serial]
fn test_added_file() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_git_repo(repo_path)?;
    create_and_commit_file(repo_path, "existing.rs", "fn existing() {}")?;

    // Build index
    run_sqry_index(repo_path, false)?;

    // Add new file
    create_and_commit_file(repo_path, "new.rs", "fn new() {}")?;

    // Update should detect addition
    let (update_output, success) = run_sqry_update(repo_path)?;
    assert!(success);

    assert!(
        update_output.contains("updated") || update_output.contains("Updated"),
        "Should detect new file: {update_output}"
    );

    Ok(())
}

// ============================================================================
// PERFORMANCE / SMOKE TESTS
// ============================================================================

#[test]
#[serial]
fn test_baseline_commit_updates_after_each_update() -> Result<(), Box<dyn std::error::Error>> {
    if !is_git_available() {
        eprintln!("Skipping test: git not available");
        return Ok(());
    }

    let temp_dir = TempDir::new()?;
    let repo_path = temp_dir.path();

    init_git_repo(repo_path)?;
    create_and_commit_file(repo_path, "v1.rs", "fn v1() {}")?;

    // Build index
    run_sqry_index(repo_path, false)?;

    let baseline1 = get_last_indexed_commit(repo_path)?;
    let head1 = get_head_commit(repo_path)?;
    assert_eq!(baseline1, Some(head1.clone()));

    // Make change 1
    create_and_commit_file(repo_path, "v2.rs", "fn v2() {}")?;
    run_sqry_update(repo_path)?;

    let baseline2 = get_last_indexed_commit(repo_path)?;
    let head2 = get_head_commit(repo_path)?;
    assert_eq!(baseline2, Some(head2.clone()));
    assert_ne!(baseline1, baseline2);

    // Make change 2
    create_and_commit_file(repo_path, "v3.rs", "fn v3() {}")?;
    run_sqry_update(repo_path)?;

    let baseline3 = get_last_indexed_commit(repo_path)?;
    let head3 = get_head_commit(repo_path)?;
    assert_eq!(baseline3, Some(head3));
    assert_ne!(baseline2, baseline3);

    Ok(())
}
