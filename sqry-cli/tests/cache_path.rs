//! T6c (surface parity W4, design W4-D1 census row `_path`): the `<PATH>`
//! positional of `sqry cache stats` and `sqry cache clear` is read. The
//! cache the two commands act on is `<PATH>/.sqry-cache` when a path is
//! given, and both name that root in their output. On the pre-change head
//! the positional was bound under an underscore and both commands used the
//! current directory's (or `SQRY_CACHE_ROOT`'s) cache whatever path was
//! given; `cache stats <dir>` printed `Cache location: .sqry-cache`.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

mod common;

use assert_cmd::Command;
use common::sqry_bin;
use serde_json::Value;

#[test]
fn cache_stats_and_clear_act_on_the_given_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let expected_root = dir.path().join(".sqry-cache");
    let expected = expected_root.display().to_string();

    let stats = Command::new(sqry_bin())
        .args(["cache", "stats"])
        .arg(dir.path())
        .env_remove("SQRY_CACHE_ROOT")
        .output()
        .expect("run sqry cache stats");
    assert!(
        stats.status.success(),
        "{}",
        String::from_utf8_lossy(&stats.stderr)
    );
    let text = String::from_utf8_lossy(&stats.stdout).into_owned();
    println!("{text}");
    assert!(
        text.contains(&format!("Cache location: {expected}")),
        "cache stats <PATH> must report <PATH>/.sqry-cache: {text}"
    );

    let json_out = Command::new(sqry_bin())
        .arg("--json")
        .args(["cache", "stats"])
        .arg(dir.path())
        .env_remove("SQRY_CACHE_ROOT")
        .output()
        .expect("run sqry --json cache stats");
    assert!(json_out.status.success());
    let json: Value = serde_json::from_slice(&json_out.stdout).expect("json output");
    assert_eq!(
        json["cache_root"].as_str(),
        Some(expected.as_str()),
        "the JSON names the same root: {json}"
    );

    // An explicit path wins over the environment root, as flags win over
    // the environment elsewhere.
    let other = tempfile::tempdir().expect("tempdir");
    let stats = Command::new(sqry_bin())
        .args(["cache", "stats"])
        .arg(dir.path())
        .env("SQRY_CACHE_ROOT", other.path().join("env-cache"))
        .output()
        .expect("run sqry cache stats");
    let text = String::from_utf8_lossy(&stats.stdout).into_owned();
    assert!(
        text.contains(&format!("Cache location: {expected}")),
        "the positional wins over SQRY_CACHE_ROOT: {text}"
    );

    let clear = Command::new(sqry_bin())
        .args(["cache", "clear", "--confirm"])
        .arg(dir.path())
        .env_remove("SQRY_CACHE_ROOT")
        .output()
        .expect("run sqry cache clear");
    assert!(
        clear.status.success(),
        "{}",
        String::from_utf8_lossy(&clear.stderr)
    );
    let text = String::from_utf8_lossy(&clear.stdout).into_owned();
    println!("{text}");
    assert!(
        text.contains(&format!("Cache location: {expected}")),
        "cache clear <PATH> must report <PATH>/.sqry-cache: {text}"
    );
}
