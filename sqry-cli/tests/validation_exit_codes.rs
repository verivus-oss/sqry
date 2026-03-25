mod common;
use common::sqry_bin;

use assert_cmd::Command;
use std::fs;
use tempfile::tempdir;

#[test]
fn query_validation_fail_exit_code_2() {
    let dir = tempdir().unwrap();
    // Create a file and build index
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn foo() {}\n").unwrap();

    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();

    // Remove file to trigger validation failure (orphaned file ratio > 20%)
    fs::remove_file(&file).unwrap();

    // Run query with strict validation (no auto-rebuild) → exit code 2
    Command::new(&path)
        .args(["query", "kind:function"])
        .arg(dir.path())
        .args(["--validate", "fail"])
        .assert()
        .code(2);
}

#[test]
fn search_validation_fail_exit_code_2() {
    let dir = tempdir().unwrap();
    // Create a file and build index
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn bar() {}\n").unwrap();

    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();

    // Remove file to trigger validation failure (orphaned file ratio > 20%)
    fs::remove_file(&file).unwrap();

    // Fuzzy search requires index; strict validation without auto-rebuild → exit code 2
    Command::new(&path)
        .args(["--fuzzy", "search", "bar"]) // prefer global --fuzzy
        .arg(dir.path())
        .args(["--validate", "fail"]) // strict
        .assert()
        .code(2);
}
