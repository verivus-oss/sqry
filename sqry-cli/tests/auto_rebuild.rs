mod common;

use assert_cmd::Command;
use common::sqry_bin;
use std::fs;
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

#[test]
fn fuzzy_search_auto_rebuilds_on_validation_fail() {
    let dir = tempdir().unwrap();
    // Create a file and build index
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn hello_world() {}\n").unwrap();

    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();

    // Delete the file to trigger validation errors (orphaned file)
    fs::remove_file(&file).unwrap();
    wait_for_filesystem_settle();

    // Fuzzy search should auto-rebuild when --validate=fail and --auto-rebuild are set
    Command::new(&path)
        .args(["--fuzzy", "search", "hello_world"]) // fuzzy search requires index
        .arg(dir.path())
        .args(["--validate", "fail", "--auto-rebuild"])
        .assert()
        .success();
}

#[test]
fn query_auto_rebuilds_on_validation_fail() {
    let dir = tempdir().unwrap();
    // Create a file and build index
    let file = dir.path().join("lib.rs");
    fs::write(&file, "pub fn my_func() {}\n").unwrap();

    let path = sqry_bin();

    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();

    // Delete the file to trigger validation errors (orphaned file)
    fs::remove_file(&file).unwrap();
    wait_for_filesystem_settle();

    // Query should auto-rebuild when flags are set and still succeed
    Command::new(&path)
        .args(["query", "kind:function"]) // semantic query
        .arg(dir.path())
        .args(["--validate", "fail", "--auto-rebuild"])
        .assert()
        .success();
}

fn wait_for_filesystem_settle() {
    let delay_ms = std::env::var("SQRY_AUTO_REBUILD_TEST_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(if cfg!(target_os = "macos") { 200 } else { 0 });

    if delay_ms > 0 {
        thread::sleep(Duration::from_millis(delay_ms));
    }
}
