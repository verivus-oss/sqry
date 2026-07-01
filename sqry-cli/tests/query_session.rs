mod common;

use assert_cmd::Command;
use common::sqry_bin;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
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

#[test]
fn query_session_json_output_shape_is_preserved() {
    let project = project_with_rust_source();
    index_project(&project);

    let output = sqry_cmd()
        .arg("--json")
        .arg("query")
        .arg("--session")
        .arg("kind:function")
        .current_dir(project.path())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "query failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let rows = value
        .get("results")
        .and_then(Value::as_array)
        .expect("json output must preserve a results array field");
    assert!(
        rows.iter()
            .any(|row| row.get("name") == Some(&Value::String("helper".to_string()))),
        "expected helper in JSON rows: {value}"
    );
}

#[test]
fn query_session_csv_and_tsv_modes_stay_machine_readable() {
    let project = project_with_rust_source();
    index_project(&project);

    let csv = sqry_cmd()
        .arg("--csv")
        .arg("--headers")
        .arg("query")
        .arg("--session")
        .arg("kind:function")
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(
        csv.status.success(),
        "csv query failed: stderr={}",
        String::from_utf8_lossy(&csv.stderr)
    );
    let csv_stdout = String::from_utf8(csv.stdout).unwrap();
    assert!(csv_stdout.starts_with("name,"));
    assert!(csv_stdout.contains("helper"));

    let tsv = sqry_cmd()
        .arg("--tsv")
        .arg("--headers")
        .arg("query")
        .arg("--session")
        .arg("kind:function")
        .current_dir(project.path())
        .output()
        .unwrap();
    assert!(
        tsv.status.success(),
        "tsv query failed: stderr={}",
        String::from_utf8_lossy(&tsv.stderr)
    );
    let tsv_stdout = String::from_utf8(tsv.stdout).unwrap();
    assert!(tsv_stdout.starts_with("name\t"));
    assert!(tsv_stdout.contains("helper"));
}

#[test]
fn query_session_uses_normal_query_plugin_selection() {
    let project = project_with_rust_source();
    index_project(&project);

    sqry_cmd()
        .arg("query")
        .arg("--disable-plugin")
        .arg("rust")
        .arg("kind:function")
        .current_dir(project.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("plugin-selection overrides conflict")
                .or(predicate::str::contains("unknown plugin")),
        );

    sqry_cmd()
        .arg("query")
        .arg("--session")
        .arg("--disable-plugin")
        .arg("rust")
        .arg("kind:function")
        .current_dir(project.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("plugin-selection overrides conflict")
                .or(predicate::str::contains("unknown plugin")),
        );
}
