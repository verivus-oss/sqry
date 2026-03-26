//! Integration tests for pager functionality
//!
//! Tests verify that paging works correctly across different commands,
//! output formats, and environment configurations.

use serial_test::serial;
use std::fs;
use std::process::{Command, Stdio};
use tempfile::{TempDir, tempdir};

mod common;

use common::sqry_bin;

fn create_temp_project() -> TempDir {
    let temp = tempdir().expect("Failed to create temp dir");
    fs::write(
        temp.path().join("main.rs"),
        "fn sample() {}\nfn helper() {}\n",
    )
    .expect("Failed to write sample file");
    temp
}

fn command_with_isolated_env(temp: &TempDir) -> Command {
    let mut cmd = Command::new(sqry_bin());
    cmd.current_dir(temp.path());
    cmd.env("SQRY_CONFIG_DIR", temp.path().join("config"));
    cmd.env("SQRY_CACHE_DIR", temp.path().join("cache"));
    cmd.env("SQRY_DATA_DIR", temp.path().join("data"));
    cmd.env("SQRY_NO_HISTORY", "1");
    cmd
}

fn project_with_index() -> TempDir {
    let temp = create_temp_project();
    let status = command_with_isolated_env(&temp)
        .args(["index", temp.path().to_str().unwrap()])
        .status()
        .expect("Failed to index test project");
    assert!(
        status.success(),
        "Indexing failed for test project with status {:?}",
        status.code()
    );
    temp
}

#[test]
fn test_no_pager_flag_direct_output() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args(["--no-pager", "query", "kind:function", "."])
        .output()
        .expect("Failed to execute");

    assert!(output.status.success());
    // Output should contain results (not empty)
    assert!(!output.stdout.is_empty() || !output.stderr.is_empty());
}

#[test]
fn test_json_bypasses_pager() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args(["--json", "query", "kind:function", "."])
        .output()
        .expect("Failed to execute");

    assert!(output.status.success());
    // Should be valid JSON
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with('{') || stdout.starts_with('['));
}

#[test]
fn test_csv_bypasses_pager() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args(["--csv", "query", "kind:function", "."])
        .output()
        .expect("Failed to execute");

    assert!(output.status.success());
    // CSV should be direct output (no pager)
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty());
}

#[test]
fn test_tsv_bypasses_pager() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args(["--tsv", "query", "kind:function", "."])
        .output()
        .expect("Failed to execute");

    assert!(output.status.success());
    // TSV should be direct output (no pager)
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty());
}

#[test]
fn test_pipe_bypasses_pager() {
    let temp = project_with_index();
    let sqry = command_with_isolated_env(&temp)
        .args(["query", "kind:function", "."])
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to spawn sqry");

    let output = sqry.wait_with_output().expect("Failed to wait");
    assert!(output.status.success());
}

#[test]
fn test_pager_cmd_flag() {
    // Use 'cat' as a simple pager that passes through
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args([
            "--pager-cmd",
            "cat",
            "--pager",
            "query",
            "kind:function",
            ".",
        ])
        .output()
        .expect("Failed to execute");

    assert!(output.status.success());
}

#[test]
#[serial]
fn test_sqry_pager_env_respected() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .env("SQRY_PAGER", "cat")
        .args(["--pager", "query", "kind:function", "."])
        .output()
        .expect("Failed to execute");

    assert!(output.status.success());
}

#[test]
#[serial]
fn test_pager_env_fallback() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .env_remove("SQRY_PAGER")
        .env("PAGER", "cat")
        .args(["--pager", "query", "kind:function", "."])
        .output()
        .expect("Failed to execute");

    assert!(output.status.success());
}

#[test]
#[serial]
fn test_missing_pager_fallback() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .env("SQRY_PAGER", "nonexistent_pager_command_12345")
        .args(["--pager", "query", "kind:function", "."])
        .output()
        .expect("Failed to execute");

    // Per CLI spec: pager not found → success (exit 0) + warning
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Expected success for missing pager (not found fallback), got {:?}. stderr: {}",
        output.status.code(),
        stderr
    );
    // Check for spec-compliant warning message format with actionable guidance
    assert!(
        stderr.contains("Warning: pager") && stderr.contains("not found"),
        "Expected warning about pager not found, got: {}",
        stderr
    );
    // Verify actionable help text is included
    assert!(
        stderr.contains("SQRY_PAGER"),
        "Expected warning to include actionable guidance about SQRY_PAGER, got: {}",
        stderr
    );
}

#[test]
#[serial]
fn test_pager_spawn_error_exits_nonzero() {
    // Per CLI spec (§3.3): spawn failures (other than not-found) should exit 1
    // Use a directory as the pager command - will fail with permission/not-executable error
    let temp = project_with_index();

    // Use /tmp (or temp dir) as pager - it exists but is not executable
    let non_executable_path = temp.path().to_str().unwrap();
    let output = command_with_isolated_env(&temp)
        .env("SQRY_PAGER", non_executable_path)
        .args(["--pager", "query", "kind:function", "."])
        .output()
        .expect("Failed to execute");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // On some systems, trying to execute a directory returns NotFound,
    // on others it returns PermissionDenied or other errors.
    // The key is: if it's NotFound, we get exit 0 + warning;
    // if it's another error, we get exit 1 + error message.
    if stderr.contains("not found") {
        // NotFound case - should succeed (exit 0) per spec
        assert!(
            output.status.success(),
            "Expected success (exit 0) for 'not found' case, got {:?}",
            output.status.code()
        );
        assert!(
            stderr.contains("Warning:"),
            "Expected warning message for not found case, got: {}",
            stderr
        );
    } else if stderr.contains("Error: Failed to start pager") {
        // Spawn error case - MUST fail with exit code 1 per spec §3.3
        assert!(
            !output.status.success(),
            "Expected non-zero exit for spawn error, got success. stderr: {}",
            stderr
        );
        // Per Codex review: explicitly assert exit code 1 to prevent regressions
        assert_eq!(
            output.status.code(),
            Some(1),
            "Spec requires exit code 1 for pager spawn failures (non-NotFound). Got {:?}. stderr: {}",
            output.status.code(),
            stderr
        );
    }
    // Either way, output should still be written to stdout (fallback behavior)
    // The deferred error pattern ensures output is produced before exit
}

#[test]
fn test_json_with_explicit_pager() {
    // JSON with explicit --pager should use pager
    // This tests that --pager flag is authoritative
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args([
            "--json",
            "--pager",
            "--pager-cmd",
            "cat",
            "query",
            "kind:function",
            ".",
        ])
        .output()
        .expect("Failed to execute");

    assert!(output.status.success());
    // Should still be valid JSON (cat passes through)
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with('{') || stdout.starts_with('['));
}

#[test]
fn test_search_command_with_pager() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args(["--pager-cmd", "cat", "--pager", "search", "test", "."])
        .output()
        .expect("Failed to execute");

    assert!(output.status.success());
}

#[test]
fn test_workspace_command_with_pager() {
    let temp = project_with_index();
    let workspace_path = temp.path().join("workspace");
    fs::create_dir_all(&workspace_path).expect("Failed to create workspace dir");

    let init_output = command_with_isolated_env(&temp)
        .args(["workspace", "init", workspace_path.to_str().unwrap()])
        .output()
        .expect("Failed to initialize workspace");

    if init_output.status.success() {
        let output = command_with_isolated_env(&temp)
            .args([
                "--pager-cmd",
                "cat",
                "--pager",
                "workspace",
                "query",
                workspace_path.to_str().unwrap(),
                "kind:function",
            ])
            .output()
            .expect("Failed to execute");

        // Workspace query may return empty results, but should succeed
        assert!(output.status.success() || output.status.code() == Some(1));
    } else {
        panic!(
            "Workspace init failed with status {:?}",
            init_output.status.code()
        );
    }
}

#[test]
fn test_history_command_with_pager() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args([
            "--pager-cmd",
            "cat",
            "--pager",
            "history",
            "list",
            "--limit",
            "10",
        ])
        .output()
        .expect("Failed to execute");

    // History may be empty, but command should succeed
    assert!(output.status.success());
}

#[test]
fn test_alias_command_with_pager() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args(["--pager-cmd", "cat", "--pager", "alias", "list"])
        .output()
        .expect("Failed to execute");

    // Alias list may be empty, but command should succeed
    assert!(output.status.success());
}

#[test]
fn test_index_status_with_pager() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args([
            "--pager-cmd",
            "cat",
            "--pager",
            "index",
            "status",
            temp.path().to_str().unwrap(),
        ])
        .output()
        .expect("Failed to execute");

    // Index may not exist or have validation issues, but command should handle gracefully
    // Exit codes: 0 = success, 1 = no index found, 2 = validation failure (all valid)
    let exit_code = output.status.code().unwrap_or(-1);
    assert!(
        exit_code == 0 || exit_code == 1 || exit_code == 2,
        "Unexpected exit code: {}",
        exit_code
    );
}

#[test]
#[cfg_attr(
    target_os = "windows",
    ignore = "Windows lacks a portable always-failing pager command"
)]
fn test_pager_exit_code_propagation() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args([
            "--pager",
            "--pager-cmd",
            "false",
            "query",
            "kind:function",
            ".",
        ])
        .output()
        .expect("Failed to execute");

    assert!(
        !output.status.success(),
        "Expected non-zero exit when pager exits with failure"
    );
}

#[test]
fn test_conflicting_pager_and_no_pager_flags() {
    let temp = project_with_index();
    let output = command_with_isolated_env(&temp)
        .args(["--pager", "--no-pager", "query", "kind:function", "."])
        .output()
        .expect("Failed to execute");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "Conflicting pager flags should be rejected cleanly"
    );
    assert!(
        stderr.contains("cannot be used with"),
        "Expected conflict error message about pager flags"
    );
}
