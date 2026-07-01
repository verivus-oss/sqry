mod common;

use assert_cmd::Command;
use common::sqry_bin;
use predicates::prelude::*;
use std::fs;
use std::io::Write;
use std::process::{Command as StdCommand, Stdio};
use tempfile::TempDir;

fn project_with_rust_source() -> TempDir {
    let dir = TempDir::new().unwrap();
    let src = dir.path().join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn helper() {}\npub fn another_helper() {}\n",
    )
    .unwrap();
    dir
}

fn sqry_cmd() -> Command {
    Command::new(sqry_bin())
}

fn index_project(project: &TempDir) {
    sqry_cmd()
        .arg("index")
        .current_dir(project.path())
        .assert()
        .success();
}

fn run_shell_script(project: &TempDir, script: &[&str]) -> (String, String) {
    run_shell_script_with_args(project, &[], script)
}

fn run_shell_script_with_args(
    project: &TempDir,
    leading_args: &[&str],
    script: &[&str],
) -> (String, String) {
    let mut child = StdCommand::new(sqry_bin())
        .current_dir(project.path())
        .args(leading_args)
        .arg("shell")
        .arg(project.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn sqry shell");

    {
        let stdin = child.stdin.as_mut().expect("stdin is available");
        for command in script {
            writeln!(stdin, "{command}").expect("write shell command");
        }
    }

    let output = child.wait_with_output().expect("read shell output");
    assert!(
        output.status.success(),
        "sqry shell failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    (
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn shell_preload_makes_first_valid_query_cache_hit() {
    let project = project_with_rust_source();
    index_project(&project);

    let (stdout, stderr) = run_shell_script(&project, &["kind:function", "exit"]);
    let combined = format!("{stdout}\n{stderr}");

    assert!(combined.contains("Loaded index from"));
    assert!(
        combined.contains("cache hit"),
        "first valid shell query should use preloaded session graph: {combined}"
    );
}

#[test]
fn shell_invalid_query_does_not_mutate_stats_after_preload() {
    let project = project_with_rust_source();
    index_project(&project);

    let (stdout, stderr) = run_shell_script(&project, &["stats", "kind:", "stats", "exit"]);
    let combined = format!("{stdout}\n{stderr}");

    assert!(
        combined.contains("Error:"),
        "invalid query should be reported: {combined}"
    );
    let total_zero_count = combined.matches("Total queries  : 0").count();
    let hit_zero_count = combined.matches("Cache hits     : 0").count();
    let miss_zero_count = combined.matches("Cache misses   : 0").count();
    assert_eq!(
        total_zero_count, 2,
        "stats before and after invalid query must show zero total queries: {combined}"
    );
    assert_eq!(
        hit_zero_count, 2,
        "stats before and after invalid query must show zero cache hits: {combined}"
    );
    assert_eq!(
        miss_zero_count, 2,
        "stats before and after invalid query must show zero cache misses: {combined}"
    );
}

#[test]
fn shell_refresh_reloads_and_next_query_hits_cache() {
    let project = project_with_rust_source();
    index_project(&project);

    let (stdout, stderr) = run_shell_script(&project, &["refresh", "kind:function", "exit"]);
    let combined = format!("{stdout}\n{stderr}");

    assert!(combined.contains("Index reloaded in"));
    assert!(
        combined.contains("cache hit"),
        "query after refresh should be served by refreshed preload: {combined}"
    );
}

#[test]
fn shell_uses_normal_query_plugin_selection() {
    let project = project_with_rust_source();
    index_project(&project);

    sqry_cmd()
        .arg("shell")
        .arg("--disable-plugin")
        .arg("rust")
        .arg(project.path())
        .current_dir(project.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("plugin-selection overrides conflict")
                .or(predicate::str::contains("unknown plugin")),
        );
}

#[test]
fn shell_csv_and_tsv_modes_stay_machine_readable() {
    let project = project_with_rust_source();
    index_project(&project);

    let (csv_stdout, csv_stderr) =
        run_shell_script_with_args(&project, &["--csv"], &["kind:function", "exit"]);
    assert!(
        csv_stdout.lines().any(|line| line.contains(",function,")),
        "csv stdout should contain comma-delimited symbol rows: {csv_stdout}"
    );
    assert!(
        !csv_stderr.contains("cache hit") && !csv_stderr.contains("Showing "),
        "csv stderr must not contain human diagnostics: {csv_stderr}"
    );

    let (tsv_stdout, tsv_stderr) =
        run_shell_script_with_args(&project, &["--tsv"], &["kind:function", "exit"]);
    assert!(
        tsv_stdout.lines().any(|line| line.contains("\tfunction\t")),
        "tsv stdout should contain tab-delimited symbol rows: {tsv_stdout}"
    );
    assert!(
        !tsv_stderr.contains("cache hit") && !tsv_stderr.contains("Showing "),
        "tsv stderr must not contain human diagnostics: {tsv_stderr}"
    );
}
