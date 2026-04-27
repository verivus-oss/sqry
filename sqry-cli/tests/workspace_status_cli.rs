//! Integration tests for `sqry workspace status` (STEP_2).

mod common;
use common::sqry_bin;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

fn sqry_cmd() -> Command {
    let path = sqry_bin();
    let mut cmd = Command::new(path);
    cmd.env("NO_COLOR", "1");
    cmd
}

/// Build a minimal indexed source root: `<workspace>/<repo>/.sqry/graph/snapshot.sqry`
/// is a present file whose leading bytes carry the canonical
/// `SQRY_GRAPH_V` magic prefix. The status surface only does a
/// lightweight prefix check (full version-aware integrity verification
/// lives in `sqry-core`'s loader), so a real-shaped header is enough
/// to be classified as `ok`.
fn make_indexed_source_root(workspace: &std::path::Path, name: &str) -> std::path::PathBuf {
    let repo = workspace.join(name);
    let graph = repo.join(".sqry").join("graph");
    fs::create_dir_all(&graph).unwrap();
    let mut snapshot = Vec::new();
    snapshot.extend_from_slice(b"SQRY_GRAPH_V10");
    snapshot.extend_from_slice(b"\0fake-postcard-payload-bytes-for-tests");
    fs::write(graph.join("snapshot.sqry"), &snapshot).unwrap();
    fs::write(graph.join("manifest.json"), b"{\"placeholder\":true}").unwrap();
    repo
}

#[test]
fn workspace_status_help_lists_subcommand() {
    sqry_cmd()
        .args(["workspace", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status"));
}

#[test]
fn workspace_status_text_output_includes_aggregate_counts() {
    let workspace = TempDir::new().unwrap();
    let workspace_path = workspace.path();
    let workspace_str = workspace_path.to_str().unwrap();

    sqry_cmd()
        .args(["workspace", "init", workspace_str, "--name", "WS"])
        .assert()
        .success();

    let svc_a = make_indexed_source_root(workspace_path, "service-a");
    let svc_b_missing = workspace_path.join("service-b");
    fs::create_dir_all(&svc_b_missing).unwrap();
    let svc_b_graph = svc_b_missing.join(".sqry").join("graph");
    fs::create_dir_all(&svc_b_graph).unwrap();
    // service-b is registered but has no snapshot.sqry → status: missing.

    sqry_cmd()
        .args(["workspace", "add", workspace_str, svc_a.to_str().unwrap()])
        .assert()
        .success();
    sqry_cmd()
        .args([
            "workspace",
            "add",
            workspace_str,
            svc_b_missing.to_str().unwrap(),
        ])
        .assert()
        .success();

    sqry_cmd()
        .args(["workspace", "status", workspace_str])
        .assert()
        .success()
        .stdout(predicate::str::contains("Workspace ID:"))
        .stdout(predicate::str::contains("Project root mode:"))
        .stdout(predicate::str::contains("Source roots: 2 total"))
        .stdout(predicate::str::contains("1 indexed"))
        .stdout(predicate::str::contains("1 missing"));
}

#[test]
fn workspace_status_json_output_emits_machine_readable_form() {
    let workspace = TempDir::new().unwrap();
    let workspace_path = workspace.path();
    let workspace_str = workspace_path.to_str().unwrap();

    sqry_cmd()
        .args(["workspace", "init", workspace_str, "--name", "WS"])
        .assert()
        .success();

    let svc = make_indexed_source_root(workspace_path, "svc-one");
    sqry_cmd()
        .args(["workspace", "add", workspace_str, svc.to_str().unwrap()])
        .assert()
        .success();

    let out = sqry_cmd()
        .args(["workspace", "status", workspace_str, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(
        json["workspace_id_short"]
            .as_str()
            .expect("workspace_id_short string")
            .len(),
        16
    );
    assert_eq!(
        json["workspace_id_full"]
            .as_str()
            .expect("workspace_id_full string")
            .len(),
        64
    );
    let aggregate = &json["aggregate"];
    assert_eq!(aggregate["total"], 1);
    assert_eq!(aggregate["ok_count"], 1);
    assert_eq!(aggregate["missing_count"], 0);
    let source_roots = json["source_roots"].as_array().unwrap();
    assert_eq!(source_roots.len(), 1);
    assert_eq!(source_roots[0]["status"], "ok");
    assert!(source_roots[0]["last_indexed_at"].is_string());
    assert_eq!(json["project_root_mode"], "gitRoot");
}

#[test]
fn workspace_status_uses_cache_within_ttl() {
    let workspace = TempDir::new().unwrap();
    let workspace_path = workspace.path();
    let workspace_str = workspace_path.to_str().unwrap();

    sqry_cmd()
        .args(["workspace", "init", workspace_str, "--name", "WS"])
        .assert()
        .success();

    let svc = make_indexed_source_root(workspace_path, "svc");
    sqry_cmd()
        .args(["workspace", "add", workspace_str, svc.to_str().unwrap()])
        .assert()
        .success();

    // Prime the cache.
    sqry_cmd()
        .args(["workspace", "status", workspace_str])
        .assert()
        .success();

    let cache_path = workspace_path
        .join(".sqry")
        .join("workspace-cache")
        .join("status.json");
    assert!(
        cache_path.exists(),
        "expected cache to be written at {}",
        cache_path.display()
    );
    let mtime_before = fs::metadata(&cache_path).unwrap().modified().unwrap();

    // Sleep briefly to let any millisecond-resolution clock tick, then
    // re-run. Because the cache is fresh and within the 60 s TTL, the
    // mtime must NOT change (we never rewrite during a cache hit).
    std::thread::sleep(Duration::from_millis(50));

    sqry_cmd()
        .args(["workspace", "status", workspace_str])
        .assert()
        .success();
    let mtime_after = fs::metadata(&cache_path).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "cache hit must not rewrite the on-disk file"
    );

    // Sanity: a wall-clock recent-enough mtime confirms the cache is
    // still considered fresh by the read path.
    let age = SystemTime::now()
        .duration_since(mtime_after)
        .unwrap_or(Duration::ZERO);
    assert!(
        age < Duration::from_secs(60),
        "cache age {age:?} must be under TTL"
    );

    // `--no-cache` forces a fresh recompute and rewrites the file.
    std::thread::sleep(Duration::from_millis(50));
    sqry_cmd()
        .args(["workspace", "status", workspace_str, "--no-cache"])
        .assert()
        .success();
    let mtime_after_no_cache = fs::metadata(&cache_path).unwrap().modified().unwrap();
    assert!(
        mtime_after_no_cache > mtime_after,
        "--no-cache must force a recompute and rewrite the cache"
    );
}
