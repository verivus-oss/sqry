mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn index_status_json_includes_validation_ok() {
    let dir = tempdir().unwrap();
    // Create a small Rust file
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn hello() {}\n").unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();

    // Status in JSON with unified graph schema (no legacy validation field)
    Command::new(&path)
        .arg("--json")
        .arg("index")
        .arg("--status")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"exists\": true")
                .and(predicate::str::contains("\"symbol_count\"")),
        );
}

#[test]
fn index_status_json_reports_index_state() {
    let dir = tempdir().unwrap();
    // Create multiple files
    let files = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"];
    for f in &files {
        fs::write(dir.path().join(f), "pub fn f(){}\n").unwrap();
    }

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();

    // Remove one file after indexing
    fs::remove_file(dir.path().join("e.rs")).unwrap();

    // Status should still return valid index information
    // (The unified graph snapshot persists independently of source files)
    Command::new(&path)
        .arg("--json")
        .arg("index")
        .arg("--status")
        .arg(dir.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"exists\": true")
                .and(predicate::str::contains("\"symbol_count\"")),
        );
}
