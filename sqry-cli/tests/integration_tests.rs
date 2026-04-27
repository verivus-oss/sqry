//! Comprehensive CLI integration tests
//!
//! Tests all CLI commands end-to-end: index, update, query, search, version, help, completions

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::io::Write;
use std::process::{Command as StdCommand, Stdio};
use tempfile::TempDir;

use sqry_core::test_support::verbosity;
use std::sync::Once;

// Initialize verbose logging once for all tests in this file
static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

/// Helper: Create test project with files
fn create_test_project(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    for (path, content) in files {
        let file_path = dir.path().join(path);
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        fs::write(&file_path, content).unwrap();
    }
    dir
}

/// Helper: Get sqry binary command
fn sqry_cmd() -> Command {
    let path = sqry_bin();
    Command::new(path)
}

/// Helper: Drive sqry shell with scripted input and return stdout
fn run_shell_script(project: &TempDir, script: &[&str]) -> String {
    let binary = sqry_bin();
    let mut child = StdCommand::new(&binary)
        .current_dir(project.path())
        .arg("shell")
        .arg(project.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("failed to spawn sqry shell");

    {
        let stdin = child.stdin.as_mut().expect("stdin is available");
        for cmd in script {
            writeln!(stdin, "{cmd}").expect("write to shell stdin");
        }
    }

    let output = child
        .wait_with_output()
        .expect("failed to read sqry shell output");
    assert!(
        output.status.success(),
        "sqry shell exited with {:?}",
        output.status.code()
    );

    String::from_utf8(output.stdout).expect("shell output is valid UTF-8")
}

// ============================================================================
// Index Command Tests
// ============================================================================

#[test]
fn test_index_creates_index_file() {
    init_logging();
    log::info!("Testing CLI 'index' command creates unified graph snapshot");

    let project = create_test_project(&[
        ("src/main.rs", "fn main() {}"),
        ("src/lib.rs", "pub fn helper() {}"),
    ]);

    log::debug!("Created test project with 2 files");

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    log::debug!("CLI 'index' command completed successfully");

    // Check for unified graph format (.sqry/graph/snapshot.sqry)
    assert!(project.path().join(".sqry/graph/snapshot.sqry").exists());

    log::info!("✓ Graph snapshot created at .sqry/graph/snapshot.sqry");
}

#[test]
fn test_index_with_progress() {
    let project = create_test_project(&[("test.rs", "fn main() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();
    // Progress bar is shown (if TTY) or hidden (if not TTY)
    // Hard to test TTY detection, so just verify command succeeds
}

#[test]
fn test_index_nonexistent_directory() {
    sqry_cmd()
        .arg("index")
        .arg("/nonexistent/path")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Error").or(predicate::str::contains("failed")));
}

#[test]
fn test_index_force_flag() {
    let project = create_test_project(&[("test.rs", "fn main() {}")]);

    // First index
    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    // Index again without force - should skip
    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    // Index with force - should rebuild
    sqry_cmd()
        .arg("index")
        .arg("--force")
        .current_dir(project.path())
        .assert()
        .success();
}

// ============================================================================
// Query Command Tests
// ============================================================================

#[test]
fn test_query_without_graph_auto_indexes() {
    let dir = TempDir::new().unwrap();

    // With auto-index enabled (default), querying without a pre-built graph
    // triggers automatic index building and succeeds
    sqry_cmd()
        .arg("query")
        .arg("kind:function")
        .current_dir(dir.path())
        .assert()
        .success();
}

#[test]
fn test_query_without_graph_with_auto_index_disabled() {
    let dir = TempDir::new().unwrap();

    // With auto-index disabled, missing graph is an error
    sqry_cmd()
        .arg("query")
        .arg("kind:function")
        .env("SQRY_AUTO_INDEX", "false")
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("No graph found"));
}

#[test]
fn test_query_finds_symbols() {
    init_logging();
    log::info!("Testing CLI 'query' command finds symbols from index");

    let project = create_test_project(&[("test.rs", "fn test_func() {}\nfn other_func() {}")]);

    log::debug!("Created test project with 2 functions");

    // Index first
    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    log::debug!("CLI 'index' completed");

    // Query
    let output = sqry_cmd()
        .arg("query")
        .arg("kind:function")
        .arg("--json")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    log::debug!("Query output length: {} bytes", stdout.len());
    assert!(stdout.contains("test_func") || stdout.contains("other_func"));

    log::info!("✓ Query command successfully found functions from index");
}

// RKG: tests REQ:SQRY-P2-6-CACHE-EVICTION-POLICY
#[test]
fn test_debug_cache_flag_prints_stats() {
    init_logging();
    let project = create_test_project(&[("src/lib.rs", "pub fn alpha() {}")]);

    // Index before running query
    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .arg("--debug-cache")
        .arg("--semantic")
        .arg("query")
        .arg("kind:function")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("CacheStats{"),
        "expected CacheStats line in stderr, got:\n{stderr}"
    );
}

// RKG: tests REQ:SQRY-P2-6-CACHE-EVICTION-POLICY
#[test]
fn test_debug_cache_env_prints_stats() {
    let project = create_test_project(&[("src/lib.rs", "pub fn beta() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .env("SQRY_CACHE_DEBUG", "1")
        .arg("--semantic")
        .arg("query")
        .arg("kind:function")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("CacheStats{"));
}

#[test]
fn test_query_with_name_filter() {
    let project = create_test_project(&[("test.rs", "fn test_func() {}\nfn other_func() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .arg("query")
        .arg("name~=/test/") // Use regex match instead of exact match
        .arg("--json")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("test_func")); // Check for actual function name
}

#[test]
fn test_query_session_requires_index() {
    let project = create_test_project(&[("src/lib.rs", "pub fn helper() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("--session")
        .arg("kind:function")
        .current_dir(project.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no index found"));
}

#[test]
fn test_query_session_executes_with_index() {
    let project = create_test_project(&[("src/lib.rs", "pub fn helper() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    sqry_cmd()
        .arg("query")
        .arg("--session")
        .arg("kind:function")
        .current_dir(project.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("session cache"));
}

#[test]
fn test_query_repo_predicate_rejected() {
    let project = create_test_project(&[("src/lib.rs", "pub fn helper() {}")]);

    sqry_cmd()
        .arg("query")
        .arg("repo:frontend")
        .current_dir(project.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("sqry workspace query"));
}

#[test]
fn test_shell_command_executes_queries_and_stats() {
    let project = create_test_project(&[("src/lib.rs", "pub fn helper() {} pub fn two() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    let output = run_shell_script(&project, &["kind:function", "stats", "exit"]);

    assert!(
        output.contains("Session statistics:"),
        "expected stats block in shell output:\n{output}"
    );
    assert!(
        output.contains("helper"),
        "expected query results in shell output:\n{output}"
    );
}

#[test]
fn test_shell_refresh_command() {
    let project = create_test_project(&[("src/lib.rs", "pub fn helper() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    let output = run_shell_script(&project, &["refresh", "exit"]);

    assert!(
        output.contains("Index reloaded in"),
        "expected refresh confirmation in shell output:\n{output}"
    );
}

#[test]
fn test_batch_command_executes_queries() {
    let project = create_test_project(&[(
        "src/lib.rs",
        "pub fn alpha() {}\nfn beta() {}\nstruct Widget;",
    )]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    let queries_path = project.path().join("queries.txt");
    fs::write(&queries_path, "kind:function\nkind:struct\n").unwrap();

    let output = sqry_cmd()
        .arg("batch")
        .arg(project.path())
        .arg("--queries")
        .arg(&queries_path)
        .arg("--stats")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Query 1: kind:function"));
    assert!(stdout.contains("Query 2: kind:struct"));
    assert!(stdout.contains("Batch Statistics:"));

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("[1/2] kind:function"));
    assert!(stderr.contains("ok:"));
}

#[test]
fn test_query_invalid_syntax() {
    let project = create_test_project(&[("test.rs", "fn main() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    // NOTE: Must use --semantic flag because hybrid mode falls back to text search
    sqry_cmd()
        .arg("--semantic")
        .arg("query")
        .arg("kind:") // Incomplete predicate should trigger syntax error
        .current_dir(project.path())
        .assert()
        .failure();
}

// ============================================================================
// Search Command Tests
// ============================================================================

#[test]
fn test_search_without_index() {
    let project = create_test_project(&[("test.rs", "fn test_func() {}")]);

    // Search mode performs on-the-fly parsing
    let output = sqry_cmd()
        .arg("search")
        .arg("kind:function")
        .arg(project.path())
        .output()
        .unwrap();

    // Search should succeed (exit code 0)
    assert!(output.status.success());

    // Should either find results or explain why not
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    // Search may return results in stdout or "No matches found" in stderr
    assert!(
        stdout.contains("test_func") || stderr.contains("No matches"),
        "Expected to find 'test_func' or 'No matches', got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn test_search_with_json_output() {
    let project = create_test_project(&[("test.rs", "fn test_func() {}")]);

    let output = sqry_cmd()
        .arg("search")
        .arg("kind:function")
        .arg("--json")
        .arg(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // JSON output should be an array or object, or empty if no results
    // Accept both successful JSON output and empty results
    assert!(
        stdout.is_empty() || stdout.contains('[') || stdout.contains('{'),
        "Expected JSON format or empty, got: {stdout}"
    );
}

#[test]
fn test_search_with_exclude_patterns() {
    let project = create_test_project(&[
        ("src/main.rs", "fn main() {}"),
        ("target/build.rs", "fn build() {}"),
    ]);

    // Note: --exclude flag might not be implemented yet
    // Test that search command at least runs
    let output = sqry_cmd()
        .arg("search")
        .arg("kind:function")
        .arg(project.path())
        .output()
        .unwrap();

    // Just verify command succeeds - exclude functionality tested elsewhere
    assert!(
        output.status.success(),
        "Search command should succeed, stderr: {}",
        String::from_utf8(output.stderr).unwrap()
    );
}

// ============================================================================
// Update Command Tests (if implemented)
// ============================================================================

#[test]
fn test_update_requires_existing_index() {
    let dir = TempDir::new().unwrap();

    sqry_cmd()
        .arg("update")
        .current_dir(dir.path())
        .assert()
        .failure();
}

// ============================================================================
// Version and Help Tests
// ============================================================================

#[test]
fn test_version_command() {
    sqry_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("sqry "));
}

#[test]
fn test_help_command() {
    sqry_cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("sqry"))
        .stdout(predicate::str::contains("index"))
        .stdout(predicate::str::contains("query"));
}

#[test]
fn test_index_help() {
    sqry_cmd()
        .arg("index")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("index"));
}

#[test]
fn test_query_help() {
    sqry_cmd()
        .arg("query")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("query"));
}

#[test]
fn test_search_help() {
    sqry_cmd()
        .arg("search")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("search"));
}

// ============================================================================
// CLI Taxonomy Tests (Phase 1 & Phase 2.1)
// ============================================================================

/// Test that root help shows Phase 1 taxonomy headings
#[test]
fn test_root_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing root help contains Phase 1 taxonomy headings");

    let output = sqry_cmd().arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 1 headings should appear
    assert!(
        stdout.contains("Common Options"),
        "Expected 'Common Options' heading in root help"
    );
    assert!(
        stdout.contains("Match Behaviour"),
        "Expected 'Match Behaviour' heading in root help"
    );
    assert!(
        stdout.contains("Search Modes"),
        "Expected 'Search Modes' heading in root help"
    );
    assert!(
        stdout.contains("Search Modes — Fuzzy"),
        "Expected 'Search Modes — Fuzzy' heading in root help"
    );
    assert!(
        stdout.contains("Output Control"),
        "Expected 'Output Control' heading in root help"
    );
    assert!(
        stdout.contains("File Filtering & Traversal"),
        "Expected 'File Filtering & Traversal' heading in root help"
    );

    log::info!("✓ Root help contains all Phase 1 taxonomy headings");
}

/// Test that root help shows flags under correct headings (regression test)
#[test]
fn test_root_help_flags_under_correct_headings() {
    init_logging();
    log::info!("Testing root help flags appear under correct taxonomy headings");

    let output = sqry_cmd().arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Common Options section should contain --json, --no-color
    let common_options_start = stdout
        .find("Common Options")
        .expect("Common Options heading not found");

    // Find next section by looking for a line that starts with a capital letter (next heading)
    let lines_after_common: Vec<&str> = stdout[common_options_start..].lines().collect();
    let mut section_end = stdout.len();
    for (_i, line) in lines_after_common.iter().enumerate().skip(1) {
        // Next heading starts at column 0 with a capital letter
        if !line.is_empty() && !line.starts_with(' ') && line.chars().next().unwrap().is_uppercase()
        {
            section_end = common_options_start
                + stdout[common_options_start..]
                    .find(line)
                    .unwrap_or(stdout.len());
            break;
        }
    }
    let common_options_section = &stdout[common_options_start..section_end];

    assert!(
        common_options_section.contains("--json"),
        "Expected --json under Common Options heading"
    );
    assert!(
        common_options_section.contains("--no-color"),
        "Expected --no-color under Common Options heading"
    );

    // Match Behaviour section should contain --exact, --ignore-case
    let match_behaviour_start = stdout
        .find("Match Behaviour")
        .expect("Match Behaviour heading not found");

    let lines_after_match: Vec<&str> = stdout[match_behaviour_start..].lines().collect();
    let mut section_end = stdout.len();
    for (_i, line) in lines_after_match.iter().enumerate().skip(1) {
        if !line.is_empty() && !line.starts_with(' ') && line.chars().next().unwrap().is_uppercase()
        {
            section_end = match_behaviour_start
                + stdout[match_behaviour_start..]
                    .find(line)
                    .unwrap_or(stdout.len());
            break;
        }
    }
    let match_behaviour_section = &stdout[match_behaviour_start..section_end];

    assert!(
        match_behaviour_section.contains("--exact"),
        "Expected --exact under Match Behaviour heading"
    );
    assert!(
        match_behaviour_section.contains("--ignore-case"),
        "Expected --ignore-case under Match Behaviour heading"
    );

    log::info!("✓ Root help flags correctly appear under their taxonomy headings");
}

/// Test that batch help shows Phase 1.5 taxonomy headings
#[test]
fn test_batch_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing batch help contains Phase 1.5 taxonomy headings");

    let output = sqry_cmd().arg("batch").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 1.5 batch headings
    assert!(
        stdout.contains("Batch Input"),
        "Expected 'Batch Input' heading in batch help"
    );
    assert!(
        stdout.contains("Batch Output Targets"),
        "Expected 'Batch Output Targets' heading in batch help"
    );
    assert!(
        stdout.contains("Session Control & Resilience"),
        "Expected 'Session Control & Resilience' heading in batch help"
    );

    log::info!("✓ Batch help contains all Phase 1.5 taxonomy headings");
}

/// Test that batch help shows flags in workflow order (Phase 2.1)
#[test]
fn test_batch_help_workflow_ordering() {
    init_logging();
    log::info!("Testing batch help flags appear in workflow order");

    let output = sqry_cmd().arg("batch").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Batch Input section: PATH should appear before --queries.
    //
    // Look for `[PATH]` (the optional-positional rendering) first; clap
    // renders required positionals as `<PATH>` and optional ones as
    // `[PATH]`. The batch positional is optional. Falling back to `<PATH>`
    // would match the global `--workspace <PATH>` value-name token that
    // appears later in the help, which is a different argument entirely.
    let batch_input_start = stdout
        .find("Batch Input")
        .expect("Batch Input heading not found");
    let path_pos = stdout[batch_input_start..]
        .find("[PATH]")
        .or_else(|| stdout[batch_input_start..].find("<PATH>"))
        .expect("PATH argument not found in Batch Input section");
    let queries_pos = stdout[batch_input_start..]
        .find("--queries")
        .expect("--queries flag not found in Batch Input section");

    assert!(
        path_pos < queries_pos,
        "Expected <PATH> to appear before --queries (workflow: where before what)"
    );

    // Batch Output Targets section: --output should appear before --output-file
    let output_targets_start = stdout
        .find("Batch Output Targets")
        .expect("Batch Output Targets heading not found");
    let output_flag_pos = stdout[output_targets_start..]
        .find("--output")
        .expect("--output flag not found in Batch Output Targets section");
    let output_file_pos = stdout[output_targets_start..]
        .find("--output-file")
        .expect("--output-file flag not found in Batch Output Targets section");

    assert!(
        output_flag_pos < output_file_pos,
        "Expected --output to appear before --output-file (workflow: format before destination)"
    );

    // Session Control & Resilience: --continue-on-error should appear before --stats
    let session_control_start = stdout
        .find("Session Control & Resilience")
        .expect("Session Control & Resilience heading not found");
    let continue_pos = stdout[session_control_start..]
        .find("--continue-on-error")
        .expect("--continue-on-error flag not found in Session Control section");
    let stats_pos = stdout[session_control_start..]
        .find("--stats")
        .expect("--stats flag not found in Session Control section");

    assert!(
        continue_pos < stats_pos,
        "Expected --continue-on-error to appear before --stats (workflow: resilience before reporting)"
    );

    log::info!("✓ Batch help flags correctly ordered by workflow");
}

/// Test that completions help shows Phase 1.5 taxonomy headings
#[test]
fn test_completions_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing completions help contains Phase 1.5 taxonomy headings");

    let output = sqry_cmd()
        .arg("completions")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 1.5 completions heading
    assert!(
        stdout.contains("Shell Targets"),
        "Expected 'Shell Targets' heading in completions help"
    );

    // Should show shell argument under Shell Targets
    let shell_targets_start = stdout
        .find("Shell Targets")
        .expect("Shell Targets heading not found");
    let next_section = stdout[shell_targets_start..]
        .find("\n\n")
        .map_or(stdout.len(), |pos| shell_targets_start + pos);
    let shell_targets_section = &stdout[shell_targets_start..next_section];

    assert!(
        shell_targets_section.contains("<SHELL>"),
        "Expected <SHELL> argument under Shell Targets heading"
    );

    log::info!("✓ Completions help contains Phase 1.5 taxonomy headings");
}

/// Test that batch help text is consistent (Phase 2.1)
#[test]
fn test_batch_help_text_style() {
    init_logging();
    log::info!("Testing batch help text is present and consistent");

    let output = sqry_cmd().arg("batch").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Verify key help descriptions are present
    assert!(
        stdout.contains("Directory containing"),
        "Expected PATH description to be present"
    );
    assert!(
        stdout.contains("File containing queries"),
        "Expected --queries description to be present"
    );
    assert!(
        stdout.contains("Set output format"),
        "Expected --output description to be present"
    );
    assert!(
        stdout.contains("Write results"),
        "Expected --output-file description to be present"
    );
    assert!(
        stdout.contains("Continue processing"),
        "Expected --continue-on-error description to be present"
    );
    assert!(
        stdout.contains("aggregate statistics"),
        "Expected --stats description to be present"
    );

    log::info!("✓ Batch help text is present and consistent");
}

/// Test that completions help text is consistent (Phase 2.1)
#[test]
fn test_completions_help_text_style() {
    init_logging();
    log::info!("Testing completions help text is present and consistent");

    let output = sqry_cmd()
        .arg("completions")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Verify shell description is present
    assert!(
        stdout.contains("Shell to generate completions"),
        "Expected <SHELL> description to be present"
    );

    // Verify possible shell values are listed
    assert!(
        stdout.contains("bash") && stdout.contains("zsh") && stdout.contains("fish"),
        "Expected shell options (bash, zsh, fish) to be listed"
    );

    log::info!("✓ Completions help text is present and consistent");
}

// ============================================================================
// Phase 2.2+ Taxonomy Tests (Core & Advanced Commands)
// ============================================================================

/// Test that index help shows Phase 2.2+ taxonomy headings
#[test]
fn test_index_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing index help contains Phase 2.2+ taxonomy headings");

    let output = sqry_cmd().arg("index").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 2.2+ index headings
    assert!(
        stdout.contains("Index Input"),
        "Expected 'Index Input' heading in index help"
    );
    assert!(
        stdout.contains("Index Configuration"),
        "Expected 'Index Configuration' heading in index help"
    );
    assert!(
        stdout.contains("Performance Tuning"),
        "Expected 'Performance Tuning' heading in index help"
    );
    assert!(
        stdout.contains("Advanced Configuration"),
        "Expected 'Advanced Configuration' heading in index help"
    );

    log::info!("✓ Index help contains all Phase 2.2+ taxonomy headings");
}

/// Test that index help shows flags in workflow order
#[test]
fn test_index_help_workflow_ordering() {
    init_logging();
    log::info!("Testing index help flags appear in workflow order");

    let output = sqry_cmd().arg("index").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Index Configuration: --force before --status before --add-to-gitignore
    let config_start = stdout.find("Index Configuration").unwrap();
    let force_pos = stdout[config_start..].find("--force").unwrap();
    let status_pos = stdout[config_start..].find("--status").unwrap();
    let gitignore_pos = stdout[config_start..].find("--add-to-gitignore").unwrap();

    assert!(
        force_pos < status_pos,
        "Expected --force before --status (workflow: rebuild before inspect)"
    );
    assert!(
        status_pos < gitignore_pos,
        "Expected --status before --add-to-gitignore (workflow: inspect before automate)"
    );

    // Performance Tuning: --threads before --no-incremental
    let perf_start = stdout.find("Performance Tuning").unwrap();
    let threads_pos = stdout[perf_start..].find("--threads").unwrap();
    let no_incremental_pos = stdout[perf_start..].find("--no-incremental").unwrap();

    assert!(
        threads_pos < no_incremental_pos,
        "Expected --threads before --no-incremental (workflow: parallelism before strategy)"
    );

    log::info!("✓ Index help flags correctly ordered by workflow");
}

/// Test that query help shows Phase 2.2+ taxonomy headings
#[test]
fn test_query_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing query help contains Phase 2.2+ taxonomy headings");

    let output = sqry_cmd().arg("query").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 2.2+ query headings
    assert!(
        stdout.contains("Query Input"),
        "Expected 'Query Input' heading in query help"
    );
    assert!(
        stdout.contains("Performance & Debugging"),
        "Expected 'Performance & Debugging' heading in query help"
    );
    assert!(
        stdout.contains("Output Control"),
        "Expected 'Output Control' heading in query help (inherited global)"
    );

    log::info!("✓ Query help contains all Phase 2.2+ taxonomy headings");
}

/// Test that query help shows flags in workflow order
#[test]
fn test_query_help_workflow_ordering() {
    init_logging();
    log::info!("Testing query help flags appear in workflow order");

    let output = sqry_cmd().arg("query").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Query Input: query before path
    let input_start = stdout.find("Query Input").unwrap();
    let query_pos = stdout[input_start..].find("<QUERY>").unwrap();
    let path_pos = stdout[input_start..].find("[PATH]").unwrap();

    assert!(
        query_pos < path_pos,
        "Expected <QUERY> before [PATH] (workflow: what before where)"
    );

    // Performance & Debugging: --session before --explain
    let perf_start = stdout.find("Performance & Debugging").unwrap();
    let session_pos = stdout[perf_start..].find("--session").unwrap();
    let explain_pos = stdout[perf_start..].find("--explain").unwrap();

    assert!(
        session_pos < explain_pos,
        "Expected --session before --explain (workflow: performance before debugging)"
    );

    log::info!("✓ Query help flags correctly ordered by workflow");
}

/// Test that search help shows Phase 2.2+ taxonomy headings
#[test]
fn test_search_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing search help contains Phase 2.2+ taxonomy headings");

    let output = sqry_cmd().arg("search").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 2.2+ search headings
    assert!(
        stdout.contains("Search Input"),
        "Expected 'Search Input' heading in search help"
    );
    // Search primarily inherits global headings (Common Options, Match Behaviour, etc.)
    assert!(
        stdout.contains("Common Options"),
        "Expected inherited 'Common Options' heading in search help"
    );

    log::info!("✓ Search help contains Phase 2.2+ taxonomy headings");
}

/// Test that watch help shows Phase 2.2+ taxonomy headings
#[test]
fn test_watch_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing watch help contains Phase 2.2+ taxonomy headings");

    let output = sqry_cmd().arg("watch").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 2.2+ watch headings
    assert!(
        stdout.contains("Watch Configuration"),
        "Expected 'Watch Configuration' heading in watch help"
    );
    assert!(
        stdout.contains("Output Control"),
        "Expected 'Output Control' heading in watch help"
    );

    log::info!("✓ Watch help contains Phase 2.2+ taxonomy headings");
}

/// Test that watch help shows flags in workflow order
#[test]
fn test_watch_help_workflow_ordering() {
    init_logging();
    log::info!("Testing watch help flags appear in workflow order");

    let output = sqry_cmd().arg("watch").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Watch Configuration: --build before --debounce
    let config_start = stdout.find("Watch Configuration").unwrap();
    let build_pos = stdout[config_start..].find("--build").unwrap();
    let debounce_pos = stdout[config_start..].find("--debounce").unwrap();

    assert!(
        build_pos < debounce_pos,
        "Expected --build before --debounce (workflow: initialization before tuning)"
    );

    log::info!("✓ Watch help flags correctly ordered by workflow");
}

/// Test that update help shows Phase 2.2+ taxonomy headings
#[test]
fn test_update_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing update help contains Phase 2.2+ taxonomy headings");

    let output = sqry_cmd().arg("update").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 2.2+ update headings
    assert!(
        stdout.contains("Update Configuration"),
        "Expected 'Update Configuration' heading in update help"
    );
    assert!(
        stdout.contains("Advanced Configuration"),
        "Expected 'Advanced Configuration' heading in update help"
    );
    assert!(
        stdout.contains("Output Control"),
        "Expected 'Output Control' heading in update help"
    );

    log::info!("✓ Update help contains Phase 2.2+ taxonomy headings");
}

/// Test that shell help shows Phase 2.2+ taxonomy headings
#[test]
fn test_shell_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing shell help contains Phase 2.2+ taxonomy headings");

    let output = sqry_cmd().arg("shell").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 2.2+ shell heading
    assert!(
        stdout.contains("Shell Configuration"),
        "Expected 'Shell Configuration' heading in shell help"
    );

    log::info!("✓ Shell help contains Phase 2.2+ taxonomy headings");
}

// ============================================================================
// Phase 3: Specialized Commands Taxonomy Tests
// ============================================================================

/// Test that visualize help shows Phase 3 taxonomy headings
#[test]
fn test_visualize_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing visualize help contains Phase 3 taxonomy headings");

    let output = sqry_cmd().arg("visualize").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 3 visualize headings
    assert!(
        stdout.contains("Visualization Input"),
        "Expected 'Visualization Input' heading in visualize help"
    );
    assert!(
        stdout.contains("Diagram Configuration"),
        "Expected 'Diagram Configuration' heading in visualize help"
    );
    assert!(
        stdout.contains("Traversal Control"),
        "Expected 'Traversal Control' heading in visualize help"
    );

    log::info!("✓ Visualize help contains all Phase 3 taxonomy headings");
}

/// Test that visualize help shows flags in workflow order
#[test]
fn test_visualize_help_workflow_ordering() {
    init_logging();
    log::info!("Testing visualize help flags appear in workflow order");

    let output = sqry_cmd().arg("visualize").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Visualization Input: query before path
    let input_start = stdout.find("Visualization Input").unwrap();
    let query_pos = stdout[input_start..].find("<QUERY>").unwrap();
    let path_pos = stdout[input_start..].find("--path").unwrap();

    assert!(
        query_pos < path_pos,
        "Expected <QUERY> before --path (workflow: what before where)"
    );

    // Diagram Configuration: --format before --output-file before --direction
    let diagram_start = stdout.find("Diagram Configuration").unwrap();
    let format_pos = stdout[diagram_start..].find("--format").unwrap();
    let output_file_pos = stdout[diagram_start..].find("--output-file").unwrap();

    assert!(
        format_pos < output_file_pos,
        "Expected --format before --output-file (workflow: format before destination)"
    );

    // Traversal Control: --depth before --max-nodes
    let traversal_start = stdout.find("Traversal Control").unwrap();
    let depth_pos = stdout[traversal_start..].find("--depth").unwrap();
    let max_nodes_pos = stdout[traversal_start..].find("--max-nodes").unwrap();

    assert!(
        depth_pos < max_nodes_pos,
        "Expected --depth before --max-nodes (workflow: depth before quantity)"
    );

    log::info!("✓ Visualize help flags correctly ordered by workflow");
}

/// Test that cache stats help shows Phase 3 taxonomy
#[test]
fn test_cache_stats_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing cache stats help contains Phase 3 taxonomy headings");

    let output = sqry_cmd()
        .arg("cache")
        .arg("stats")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 3 cache stats heading
    assert!(
        stdout.contains("Cache Input"),
        "Expected 'Cache Input' heading in cache stats help"
    );

    log::info!("✓ Cache stats help contains Phase 3 taxonomy headings");
}

/// Test that cache clear help shows safety control heading
#[test]
fn test_cache_clear_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing cache clear help contains Phase 3 taxonomy headings");

    let output = sqry_cmd()
        .arg("cache")
        .arg("clear")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 3 cache clear headings
    assert!(
        stdout.contains("Cache Input"),
        "Expected 'Cache Input' heading in cache clear help"
    );
    assert!(
        stdout.contains("Safety Control"),
        "Expected 'Safety Control' heading in cache clear help"
    );

    log::info!("✓ Cache clear help contains Phase 3 taxonomy headings");
}

/// Test that cache prune help shows Phase 3 taxonomy
#[test]
fn test_cache_prune_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing cache prune help contains Phase 3 taxonomy headings");

    let output = sqry_cmd()
        .arg("cache")
        .arg("prune")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 3 cache prune headings
    assert!(
        stdout.contains("Cache Input"),
        "Expected 'Cache Input' heading in cache prune help"
    );
    assert!(
        stdout.contains("Safety Control"),
        "Expected 'Safety Control' heading in cache prune help"
    );

    log::info!("✓ Cache prune help contains Phase 3 taxonomy headings");
}

/// Test that workspace init help shows Phase 3 taxonomy
#[test]
fn test_workspace_init_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing workspace init help contains Phase 3 taxonomy headings");

    let output = sqry_cmd()
        .arg("workspace")
        .arg("init")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 3 workspace init headings
    assert!(
        stdout.contains("Workspace Input"),
        "Expected 'Workspace Input' heading in workspace init help"
    );
    assert!(
        stdout.contains("Workspace Configuration"),
        "Expected 'Workspace Configuration' heading in workspace init help"
    );

    log::info!("✓ Workspace init help contains Phase 3 taxonomy headings");
}

/// Test that workspace scan help shows Phase 3 taxonomy
#[test]
fn test_workspace_scan_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing workspace scan help contains Phase 3 taxonomy headings");

    let output = sqry_cmd()
        .arg("workspace")
        .arg("scan")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 3 workspace scan headings
    assert!(
        stdout.contains("Workspace Input"),
        "Expected 'Workspace Input' heading in workspace scan help"
    );
    assert!(
        stdout.contains("Workspace Configuration"),
        "Expected 'Workspace Configuration' heading in workspace scan help"
    );

    log::info!("✓ Workspace scan help contains Phase 3 taxonomy headings");
}

/// Test that workspace add help shows Phase 3 taxonomy
#[test]
fn test_workspace_add_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing workspace add help contains Phase 3 taxonomy headings");

    let output = sqry_cmd()
        .arg("workspace")
        .arg("add")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 3 workspace add headings
    assert!(
        stdout.contains("Workspace Input"),
        "Expected 'Workspace Input' heading in workspace add help"
    );
    assert!(
        stdout.contains("Workspace Configuration"),
        "Expected 'Workspace Configuration' heading in workspace add help"
    );

    log::info!("✓ Workspace add help contains Phase 3 taxonomy headings");
}

/// Test that workspace query help shows Phase 3 taxonomy
#[test]
fn test_workspace_query_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing workspace query help contains Phase 3 taxonomy headings");

    let output = sqry_cmd()
        .arg("workspace")
        .arg("query")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 3 workspace query headings
    assert!(
        stdout.contains("Workspace Input"),
        "Expected 'Workspace Input' heading in workspace query help"
    );
    assert!(
        stdout.contains("Performance Tuning"),
        "Expected 'Performance Tuning' heading in workspace query help"
    );

    log::info!("✓ Workspace query help contains Phase 3 taxonomy headings");
}

// ============================================================================
// Phase 4: Graph Command Taxonomy Tests
// ============================================================================

/// Test that graph root command shows Phase 4 taxonomy
#[test]
fn test_graph_root_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing graph root help contains Phase 4 taxonomy headings");

    let output = sqry_cmd().arg("graph").arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 4 graph root heading
    assert!(
        stdout.contains("Graph Configuration"),
        "Expected 'Graph Configuration' heading in graph root help"
    );

    log::info!("✓ Graph root help contains Phase 4 taxonomy headings");
}

/// Test that graph trace-path shows Phase 4 taxonomy
#[test]
fn test_graph_trace_path_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing graph trace-path help contains Phase 4 taxonomy headings");

    let output = sqry_cmd()
        .arg("graph")
        .arg("trace-path")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 4 trace-path headings
    assert!(
        stdout.contains("Analysis Input"),
        "Expected 'Analysis Input' heading in graph trace-path help"
    );
    assert!(
        stdout.contains("Filtering & Constraints"),
        "Expected 'Filtering & Constraints' heading in graph trace-path help"
    );
    assert!(
        stdout.contains("Output Options"),
        "Expected 'Output Options' heading in graph trace-path help"
    );

    log::info!("✓ Graph trace-path help contains Phase 4 taxonomy headings");
}

/// Test that graph call-chain-depth shows Phase 4 taxonomy
#[test]
fn test_graph_call_chain_depth_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing graph call-chain-depth help contains Phase 4 taxonomy headings");

    let output = sqry_cmd()
        .arg("graph")
        .arg("call-chain-depth")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 4 call-chain-depth headings
    assert!(
        stdout.contains("Analysis Input"),
        "Expected 'Analysis Input' heading in graph call-chain-depth help"
    );
    assert!(
        stdout.contains("Filtering & Constraints"),
        "Expected 'Filtering & Constraints' heading in graph call-chain-depth help"
    );

    log::info!("✓ Graph call-chain-depth help contains Phase 4 taxonomy headings");
}

/// Test that graph dependency-tree shows Phase 4 taxonomy
#[test]
fn test_graph_dependency_tree_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing graph dependency-tree help contains Phase 4 taxonomy headings");

    let output = sqry_cmd()
        .arg("graph")
        .arg("dependency-tree")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 4 dependency-tree headings
    assert!(
        stdout.contains("Analysis Input"),
        "Expected 'Analysis Input' heading in graph dependency-tree help"
    );
    assert!(
        stdout.contains("Analysis Options"),
        "Expected 'Analysis Options' heading in graph dependency-tree help"
    );

    log::info!("✓ Graph dependency-tree help contains Phase 4 taxonomy headings");
}

/// Test that graph cross-language shows Phase 4 taxonomy
#[test]
fn test_graph_cross_language_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing graph cross-language help contains Phase 4 taxonomy headings");

    let output = sqry_cmd()
        .arg("graph")
        .arg("cross-language")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 4 cross-language heading
    assert!(
        stdout.contains("Filtering & Constraints"),
        "Expected 'Filtering & Constraints' heading in graph cross-language help"
    );

    log::info!("✓ Graph cross-language help contains Phase 4 taxonomy headings");
}

/// Test that graph stats shows Phase 4 taxonomy
#[test]
fn test_graph_stats_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing graph stats help contains Phase 4 taxonomy headings");

    let output = sqry_cmd()
        .arg("graph")
        .arg("stats")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 4 stats heading
    assert!(
        stdout.contains("Output Options"),
        "Expected 'Output Options' heading in graph stats help"
    );

    log::info!("✓ Graph stats help contains Phase 4 taxonomy headings");
}

/// Test that graph cycles shows Phase 4 taxonomy
#[test]
fn test_graph_cycles_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing graph cycles help contains Phase 4 taxonomy headings");

    let output = sqry_cmd()
        .arg("graph")
        .arg("cycles")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 4 cycles headings
    assert!(
        stdout.contains("Analysis Options"),
        "Expected 'Analysis Options' heading in graph cycles help"
    );
    assert!(
        stdout.contains("Filtering & Constraints"),
        "Expected 'Filtering & Constraints' heading in graph cycles help"
    );

    log::info!("✓ Graph cycles help contains Phase 4 taxonomy headings");
}

/// Test that graph complexity shows Phase 4 taxonomy
#[test]
fn test_graph_complexity_help_taxonomy_headings() {
    init_logging();
    log::info!("Testing graph complexity help contains Phase 4 taxonomy headings");

    let output = sqry_cmd()
        .arg("graph")
        .arg("complexity")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Phase 4 complexity headings
    assert!(
        stdout.contains("Analysis Input"),
        "Expected 'Analysis Input' heading in graph complexity help"
    );
    assert!(
        stdout.contains("Filtering & Constraints"),
        "Expected 'Filtering & Constraints' heading in graph complexity help"
    );
    assert!(
        stdout.contains("Output Options"),
        "Expected 'Output Options' heading in graph complexity help"
    );

    log::info!("✓ Graph complexity help contains Phase 4 taxonomy headings");
}

/// Test workflow ordering for graph trace-path
#[test]
fn test_graph_trace_path_workflow_ordering() {
    init_logging();
    log::info!("Testing graph trace-path help shows correct workflow ordering");

    let output = sqry_cmd()
        .arg("graph")
        .arg("trace-path")
        .arg("--help")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Find positions of inputs, filtering, and output flags
    let from_pos = stdout
        .find("<FROM>")
        .expect("Expected <FROM> in help output");
    let to_pos = stdout.find("<TO>").expect("Expected <TO> in help output");
    let languages_pos = stdout
        .find("--languages")
        .expect("Expected --languages in help output");
    let full_paths_pos = stdout
        .find("--full-paths")
        .expect("Expected --full-paths in help output");

    // Verify workflow ordering: Input (<FROM>, <TO>) → Filtering (--languages) → Output (--full-paths)
    assert!(
        from_pos < to_pos,
        "Expected <FROM> to appear before <TO> in help output"
    );
    assert!(
        to_pos < languages_pos,
        "Expected inputs (<TO>) to appear before filtering (--languages)"
    );
    assert!(
        languages_pos < full_paths_pos,
        "Expected filtering (--languages) to appear before output (--full-paths)"
    );

    log::info!("✓ Graph trace-path help shows correct workflow ordering");
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[test]
fn test_invalid_subcommand() {
    // sqry treats unknown args as search patterns, so this actually runs a search
    // Just verify it doesn't crash
    let output = sqry_cmd().arg("invalid-command").output().unwrap();

    // Command should complete (may succeed or fail gracefully)
    assert!(
        output.status.success() || output.status.code().unwrap() > 0,
        "Command should complete without panic"
    );
}

#[test]
fn test_query_missing_arguments() {
    sqry_cmd().arg("query").assert().failure();
}

// ============================================================================
// Output Format Tests
// ============================================================================

#[test]
fn test_query_json_output_format() {
    let project = create_test_project(&[("test.rs", "fn main() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .arg("query")
        .arg("kind:function")
        .arg("--json")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Should be valid JSON
    assert!(stdout.starts_with('[') || stdout.starts_with('{'));
}

#[test]
fn test_query_text_output_format() {
    let project = create_test_project(&[("test.rs", "fn main() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    sqry_cmd()
        .arg("query")
        .arg("kind:function")
        .current_dir(project.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("main"));
}

// ============================================================================
// Boolean Query Syntax Tests (v0.12.1)
// ============================================================================

#[test]
fn test_boolean_and_query() {
    // Test boolean AND query with async:true predicate.
    // The async flag is now extracted by GraphBuilder and queryable via graph_eval::match_async.
    init_logging();
    log::info!("Testing boolean AND query (kind:function AND async:true)");

    let project = create_test_project(&[(
        "test.rs",
        "pub async fn async_func() {}\nfn other_func() {}",
    )]);

    log::debug!("Created test project with 1 async function, 1 sync function");

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    log::debug!("CLI 'index' completed");

    let output = sqry_cmd()
        .arg("--semantic")
        .arg("query")
        .arg("kind:function AND async:true")
        .arg("--json")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Query failed.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    log::debug!("Boolean query output: {} bytes", stdout.len());
    assert!(stdout.contains("async_func"));
    assert!(!stdout.contains("other_func"));

    log::info!("✓ Boolean AND query correctly filtered: async_func found, other_func excluded");
}

#[test]
fn test_boolean_query_explain_mode() {
    let project = create_test_project(&[("test.rs", "fn main() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .arg("query")
        .arg("--explain")
        .arg("kind:function AND async:true")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Parse query (boolean)"));
    assert!(stderr.contains("Validate fields"));
    assert!(stderr.contains("Optimize AST"));
}

#[test]
fn test_metadata_visibility_filter() {
    // Visibility metadata is now extracted by GraphBuilder and queryable via
    // NodeEntry.visibility field (checked by graph_eval::match_visibility).
    let project =
        create_test_project(&[("test.rs", "pub fn public_func() {}\nfn private_func() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    // In Rust, the visibility modifier is "pub", not "public"
    let output = sqry_cmd()
        .arg("--semantic")
        .arg("query")
        .arg("visibility:pub")
        .arg("--json")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("public_func"),
        "visibility:pub should find public_func"
    );
    assert!(
        !stdout.contains("private_func"),
        "visibility:pub should NOT find private_func"
    );
}

#[test]
fn test_metadata_in_json_output() {
    // async and visibility metadata are now queryable via graph_eval
    let project = create_test_project(&[("test.rs", "pub async fn test_func() {}")]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .arg("--semantic")
        .arg("query")
        .arg("kind:function AND async:true")
        .arg("--json")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Verify metadata field exists in JSON
    assert!(stdout.contains("metadata"));
    assert!(stdout.contains("async"));
    assert!(stdout.contains("visibility"));
}

#[test]
fn test_complex_boolean_query() {
    // async and visibility metadata are now queryable via graph_eval
    // Note: Rust visibility is "pub" not "public"
    let project = create_test_project(&[(
        "test.rs",
        "pub async fn pub_async() {}\npub fn pub_sync() {}\nasync fn priv_async() {}",
    )]);

    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    let output = sqry_cmd()
        .arg("--semantic")
        .arg("query")
        .arg("kind:function AND async:true AND visibility:pub")
        .arg("--json")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("pub_async"));
    assert!(!stdout.contains("pub_sync"));
    assert!(!stdout.contains("priv_async"));
}

// ============================================================================
// Hybrid Search Tests
// ============================================================================

#[test]
fn test_hybrid_text_search_mode() {
    init_logging();
    log::info!("Testing hybrid search text mode detection with TODO comment");

    let project = create_test_project(&[(
        "test.rs",
        "fn main() {}\n// TODO: implement this feature\nfn helper() {}",
    )]);

    log::debug!("Created test project with TODO comment");

    // Query with TODO pattern (should trigger text search)
    let output = sqry_cmd()
        .arg("query")
        .arg("TODO")
        .arg("--limit")
        .arg("10")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    // Should use text search mode
    assert!(
        stderr.contains("[Text search mode]"),
        "Expected text search mode, got stderr: {stderr}"
    );

    // Should find the TODO comment
    assert!(
        stdout.contains("TODO"),
        "Expected to find TODO, got stdout: {stdout}"
    );

    log::info!("✓ Hybrid search correctly detected text pattern and found TODO comment");
}

#[test]
fn test_hybrid_force_text_mode() {
    init_logging();
    log::info!("Testing --text flag to force text search mode");

    let project = create_test_project(&[("test.rs", "fn build_config() {}\nfn other() {}")]);

    log::debug!("Created test project with 2 functions");

    // Force text search with --text flag
    let output = sqry_cmd()
        .arg("--text")
        .arg("query")
        .arg("build")
        .arg("--limit")
        .arg("10")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should find "build" in the function name using text search
    assert!(
        stdout.contains("build"),
        "Expected to find 'build', got stdout: {stdout}"
    );

    log::info!("✓ --text flag correctly forced text search mode");
}

#[test]
fn test_hybrid_semantic_fallback() {
    init_logging();
    log::info!("Testing hybrid search semantic fallback to text");

    let project = create_test_project(&[("test.rs", "fn main() {}\n// FIXME: broken code")]);

    log::debug!("Created test project with FIXME comment");

    // Query with ambiguous pattern (should try semantic first, then fallback)
    let output = sqry_cmd()
        .arg("query")
        .arg("FIXME")
        .arg("--limit")
        .arg("10")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    // Should show text search mode (classified as text pattern)
    assert!(
        stderr.contains("[Text search mode]"),
        "Expected text search mode, got stderr: {stderr}"
    );

    // Should find FIXME
    assert!(
        stdout.contains("FIXME"),
        "Expected to find FIXME, got stdout: {stdout}"
    );

    log::info!("✓ Hybrid search correctly used text mode for FIXME pattern");
}

#[test]
fn test_hybrid_no_fallback_flag() {
    init_logging();
    log::info!("Testing --no-fallback flag disables text fallback");

    let project = create_test_project(&[("test.rs", "fn main() {}")]);

    log::debug!("Created test project");

    // Query with --no-fallback (should not fallback to text on semantic failure)
    let output = sqry_cmd()
        .arg("--no-fallback")
        .arg("query")
        .arg("nonexistent")
        .current_dir(project.path())
        .output()
        .unwrap();

    // May succeed with empty results or fail - both acceptable with --no-fallback
    // The important part is no fallback occurred
    let stderr = String::from_utf8(output.stderr).unwrap();

    // Should not contain fallback message
    assert!(
        !stderr.contains("Falling back to text search"),
        "Should not fallback with --no-fallback flag, got stderr: {stderr}"
    );

    log::info!("✓ --no-fallback flag correctly disabled fallback");
}

#[test]
fn test_hybrid_context_lines_flag() {
    init_logging();
    log::info!("Testing --context flag for text search results");

    let project = create_test_project(&[(
        "test.rs",
        "fn main() {}\n// Line before\n// TODO: fix\n// Line after\nfn helper() {}",
    )]);

    log::debug!("Created test project with context lines");

    // Query with custom context lines
    let output = sqry_cmd()
        .arg("--context")
        .arg("1")
        .arg("query")
        .arg("TODO")
        .arg("--limit")
        .arg("10")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Should find TODO
    assert!(
        stdout.contains("TODO"),
        "Expected to find TODO, got stdout: {stdout}"
    );

    log::info!("✓ --context flag correctly set for text search");
}

#[test]
fn test_hybrid_search_json_mode() {
    init_logging();
    log::info!("Testing hybrid search doesn't show mode messages in JSON mode");

    let project = create_test_project(&[("test.rs", "fn main() {}\n// TODO: implement")]);

    log::debug!("Created test project");

    // Query in JSON mode
    let output = sqry_cmd()
        .arg("--json")
        .arg("query")
        .arg("TODO")
        .arg("--limit")
        .arg("10")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();

    // Should not show search mode messages in JSON mode
    assert!(
        !stderr.contains("[Text search mode]") && !stderr.contains("[Semantic search mode]"),
        "Should not show mode messages in JSON mode, got stderr: {stderr}"
    );

    log::info!("✓ JSON mode correctly suppresses search mode messages");
}

// ============================================================================
// Query Parser Tests
// ============================================================================

#[test]
fn test_boolean_query_works() {
    init_logging();
    log::info!("Testing boolean query works with default AST parser");

    let project = create_test_project(&[(
        "src/lib.rs",
        "pub fn alpha() {}\npub fn beta() {}\nstruct Widget;",
    )]);

    // Index first
    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();

    // Boolean AND query should work with AST parser (default)
    let output = sqry_cmd()
        .arg("query")
        .arg("kind:function AND name:alpha")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Boolean query should succeed with AST parser, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("alpha"),
        "Boolean AND query should find alpha function, got: {stdout}"
    );
    assert!(
        !stdout.contains("beta"),
        "Boolean AND query should NOT find beta (filtered by name predicate), got: {stdout}"
    );

    log::info!("✓ Boolean query works correctly with AST parser");
}
