mod common;

use assert_cmd::Command;
use common::sqry_bin;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

/// Test that index command emits thread pool metrics when SQRY_EMIT_THREAD_POOL_METRICS=1
///
/// Test that thread_pool_metrics are emitted when SQRY_EMIT_THREAD_POOL_METRICS=1
#[test]
fn index_emits_thread_pool_metrics() {
    let tmp_test_workspace = TempDir::new().unwrap();
    fs::write(
        tmp_test_workspace.path().join("main.rs"),
        r"fn one() {} fn two() {}",
    )
    .unwrap();

    let mut cmd = Command::new(sqry_bin());
    let assert = cmd
        .arg("index")
        .arg(tmp_test_workspace.path())
        .arg("--threads")
        .arg("4")
        .env("SQRY_EMIT_THREAD_POOL_METRICS", "1")
        .assert()
        .success()
        .stdout(predicate::str::contains("thread_pool_creations"));

    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let metrics_line = output
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .expect("metrics JSON was not emitted");

    let metrics: Value = serde_json::from_str(metrics_line).expect("valid JSON metrics");
    let creations = metrics["thread_pool_creations"]
        .as_u64()
        .expect("thread_pool_creations should be u64");
    assert_eq!(creations, 1, "expected exactly one thread pool creation");
}
