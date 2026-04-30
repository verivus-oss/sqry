mod common;
use assert_cmd::Command;
use common::sqry_bin;
use std::fs;
use tempfile::TempDir;

fn write_simple_callers_fixture(root: &std::path::Path) {
    fs::write(
        root.join("lib.rs"),
        r"
pub fn helper() -> i32 {
    42
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

#[test]
fn global_json_before_graph_threads_into_provenance_renderer() {
    let temp = TempDir::new().unwrap();
    write_simple_callers_fixture(temp.path());
    index(temp.path());

    let output = Command::new(sqry_bin())
        .arg("--json")
        .arg("graph")
        .arg("--path")
        .arg(temp.path())
        .arg("provenance")
        .arg("helper")
        .output()
        .expect("command failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "command failed: stdout={}, stderr={}",
        stdout,
        stderr,
    );

    // If it's JSON, it should parse. If it's text, this will fail.
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("expected JSON output for provenance, got: {stdout}\nparse error: {e}")
    });
    assert!(parsed.is_object());
}
