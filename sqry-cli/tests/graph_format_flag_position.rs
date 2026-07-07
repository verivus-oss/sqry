//! Regression coverage for verivus-oss/sqry#517.
//!
//! `sqry graph --help` promises "All commands support --format json", and
//! `--json` works in any position (it is a top-level `global = true` flag).
//! But `--format` was defined only on the `graph` parent command, so it bound
//! before the operation (`sqry graph --format json stats`) and was rejected
//! after it (`sqry graph stats --format json`). The `Graph::format` arg is now
//! `global = true`, so it is position-independent just like `--json`.
//!
//! These tests lock both placements plus the pre-existing `--json` alias and
//! the `--format` vs `--json` conflict diagnostic.

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

fn write_fixture(root: &std::path::Path) {
    fs::write(
        root.join("lib.rs"),
        r"
pub fn helper() -> i32 {
    42
}

pub fn caller() -> i32 {
    helper()
}
",
    )
    .unwrap();
}

fn index(root: &std::path::Path) {
    Command::new(sqry_bin())
        .arg("index")
        .arg(root)
        .assert()
        .success();
}

/// Run `sqry graph <op> --format json` with `--format` AFTER the operation and
/// assert the output parses as JSON. This is the exact invocation from #517.
#[test]
fn graph_format_json_after_operation_produces_json() {
    let temp = TempDir::new().unwrap();
    write_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("stats")
        .arg("--format")
        .arg("json")
        .output()
        .expect("command failed");
    assert!(
        output.status.success(),
        "`graph stats --format json` must succeed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected JSON output, got: {stdout}\nparse error: {e}"));
    assert!(
        parsed.get("node_count").is_some() || parsed.get("edge_count").is_some(),
        "graph stats JSON should expose node/edge counts, got {parsed}"
    );
}

/// `--format json` BEFORE the operation must keep working (regression guard for
/// the pre-existing placement now that the arg is global).
#[test]
fn graph_format_json_before_operation_still_produces_json() {
    let temp = TempDir::new().unwrap();
    write_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .arg("stats")
        .output()
        .expect("command failed");
    assert!(
        output.status.success(),
        "`graph --format json stats` must succeed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<serde_json::Value>(&stdout)
        .unwrap_or_else(|e| panic!("expected JSON output, got: {stdout}\nparse error: {e}"));
}

/// The global `--json` alias after the operation must still produce JSON.
#[test]
fn graph_json_alias_after_operation_still_produces_json() {
    let temp = TempDir::new().unwrap();
    write_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("stats")
        .arg("--json")
        .output()
        .expect("command failed");
    assert!(
        output.status.success(),
        "`graph stats --json` must succeed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(&output.stdout))
        .expect("`--json` alias must still produce JSON");
}

/// `--format text` combined with `--json` after the operation must still error
/// loudly, naming both flags. The conflict guard must survive the global-arg
/// change.
#[test]
fn graph_conflicting_format_text_and_json_after_operation_errors() {
    let temp = TempDir::new().unwrap();
    write_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("stats")
        .arg("--format")
        .arg("text")
        .arg("--json")
        .output()
        .expect("command failed");
    assert!(
        !output.status.success(),
        "conflicting --format text + --json must fail: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        combined.contains("--json") && combined.contains("--format"),
        "diagnostic must name both flags: {combined}"
    );
}
