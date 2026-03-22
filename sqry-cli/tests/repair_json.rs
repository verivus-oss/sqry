mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn repair_fix_orphans_dry_run_json() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn baz() {}\n").unwrap();

    // Build index
    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();

    // Delete the file to simulate orphaned symbols
    fs::remove_file(&file).unwrap();

    // Run repair in dry-run JSON mode
    Command::new(&path)
        .args(["--json", "repair", "--fix-orphans", "--dry-run"]) // --json global
        .arg(dir.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"dry_run\": true")
                .and(predicate::str::contains("\"stats\""))
                .and(predicate::str::contains("\"orphans_detected\"")),
        );
}
