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

/// T3 (surface parity W4, design W4-D3): `--validate` compares the orphan
/// ratio with `--threshold-orphaned-files`, not with a constant. The
/// fixture indexes two files and deletes one (ratio 0.5): with no flag the
/// default 0.20 trips (exit 2, the control, green on both heads); with
/// `--threshold-orphaned-files 0.6` the ratio is tolerated (exit 0); with
/// `1.5` clap refuses the value by name before any index is read.
#[test]
fn validate_reads_the_orphaned_files_threshold() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("kept.rs"), "pub fn kept() {}\n").unwrap();
    let removed = dir.path().join("removed.rs");
    fs::write(&removed, "pub fn removed() {}\n").unwrap();

    let path = sqry_bin();
    Command::new(&path)
        .arg("index")
        .arg(dir.path())
        .assert()
        .success();
    fs::remove_file(&removed).unwrap();
    wait_for_filesystem_settle();

    // Control: half the indexed files are gone, the default 0.20 trips.
    let control = Command::new(&path)
        .args(["query", "kind:function"])
        .arg(dir.path())
        .args(["--validate", "fail"])
        .output()
        .unwrap();
    let control_code = control.status.code();
    println!("control exit code (no flag, ratio 0.5): {control_code:?}");
    assert_eq!(
        control_code,
        Some(2),
        "control: the default threshold must trip on a 0.5 orphan ratio: {}",
        String::from_utf8_lossy(&control.stderr)
    );

    // The flag is read: 0.6 tolerates a 0.5 ratio.
    let tolerated = Command::new(&path)
        .args(["query", "kind:function"])
        .arg(dir.path())
        .args(["--validate", "fail", "--threshold-orphaned-files", "0.6"])
        .output()
        .unwrap();
    let tolerated_code = tolerated.status.code();
    println!("exit code with --threshold-orphaned-files 0.6: {tolerated_code:?}");
    assert_eq!(
        tolerated_code,
        Some(0),
        "--threshold-orphaned-files 0.6 must tolerate a 0.5 orphan ratio; stderr: {}",
        String::from_utf8_lossy(&tolerated.stderr)
    );

    // Out of range is refused at parse time, naming the value.
    let refused = Command::new(&path)
        .args(["query", "kind:function"])
        .arg(dir.path())
        .args(["--validate", "fail", "--threshold-orphaned-files", "1.5"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    println!(
        "exit code with --threshold-orphaned-files 1.5: {:?}",
        refused.status.code()
    );
    assert!(!refused.status.success(), "1.5 must be refused");
    assert!(
        stderr.contains("1.5") && stderr.contains("outside 0.0 to 1.0"),
        "the refusal must name the value and the range: {stderr}"
    );
}

/// The `--validate fail --auto-rebuild` rebuild's refusal names its cause.
/// The index records an expand cache directory that is gone by the time the
/// stale index is rebuilt, so the rebuild's macro options are refused; the
/// error is chained (the macro options' context over the missing directory),
/// and the printed line must carry the inner cause, not only the context.
#[test]
fn auto_rebuild_failure_names_its_cause() {
    let dir = tempdir().unwrap();
    let cache_home = tempdir().unwrap();
    let cache = cache_home.path().join("expand-cache");
    fs::create_dir_all(&cache).unwrap();
    fs::write(dir.path().join("keep.rs"), "pub fn kept() {}\n").unwrap();
    let gone = dir.path().join("gone.rs");
    fs::write(&gone, "pub fn gone() {}\n").unwrap();

    let path = sqry_bin();
    Command::new(&path)
        .arg("index")
        .arg("--expand-cache")
        .arg(&cache)
        .arg(dir.path())
        .assert()
        .success();

    // Half the indexed files go missing (stale at the default threshold),
    // and so does the recorded expand cache.
    fs::remove_file(&gone).unwrap();
    fs::remove_dir_all(&cache).unwrap();
    wait_for_filesystem_settle();

    let output = Command::new(&path)
        .args(["query", "kind:function"])
        .arg(dir.path())
        .args(["--validate", "fail", "--auto-rebuild"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr
        .lines()
        .find(|line| line.starts_with("Error: auto-rebuild failed:"))
        .unwrap_or_else(|| panic!("instrument: the auto-rebuild failed: {stderr}"));
    assert!(
        line.contains("recorded in the index manifest"),
        "the auto-rebuild failure must name its cause (the recorded expand cache): {line}"
    );
}
