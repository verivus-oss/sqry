mod common;

use common::sqry_bin;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn copy_fixture_into_temp(relative_fixture_path: &str, file_name: &str) -> TempDir {
    let temp_dir = TempDir::new().expect("temp dir");
    let fixture_path = workspace_root().join(relative_fixture_path);
    let target_path = temp_dir.path().join(file_name);
    fs::copy(&fixture_path, &target_path).unwrap_or_else(|err| {
        panic!(
            "failed to copy fixture {} to {}: {err}",
            fixture_path.display(),
            target_path.display()
        )
    });
    temp_dir
}

fn run_index_in_temp(temp_dir: &Path) -> std::process::Output {
    Command::new(sqry_bin())
        .current_dir(temp_dir)
        .arg("index")
        .arg("--force")
        .arg(".")
        .output()
        .expect("run sqry index")
}

#[test]
fn index_handles_perl_pod_fixture_without_panicking() {
    let temp_dir = copy_fixture_into_temp("sqry-lang-perl/tests/fixtures/pod.pl", "pod.pl");
    let output = run_index_in_temp(temp_dir.path());

    assert!(
        output.status.success(),
        "sqry index failed for pod.pl\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("panicked"),
        "stderr unexpectedly reported a panic:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn index_handles_literate_haskell_fixture_without_panicking() {
    let temp_dir = copy_fixture_into_temp(
        "sqry-lang-haskell/tests/fixtures/literate.lhs",
        "literate.lhs",
    );
    let output = run_index_in_temp(temp_dir.path());

    assert!(
        output.status.success(),
        "sqry index failed for literate.lhs\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("panicked"),
        "stderr unexpectedly reported a panic:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
