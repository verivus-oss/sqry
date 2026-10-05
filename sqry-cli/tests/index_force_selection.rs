//! Surface parity W1 round 2 (design D11), the legs that need the real
//! `sqry` binary because they read what it writes to stderr or drive a
//! command that only the binary wires end to end.
//!
//! - T25, stderr leg: `sqry index --force` over an index whose manifest
//!   cannot be read prints a warning naming the manifest and records the
//!   fast-path fallback. On `abefdd8e3` the rebuild succeeded silently.
//! - The CLI auto-index site (`sqry query` over a root with no index): the
//!   manifest it writes records `high_cost_mode: fast_path_default`. On
//!   `abefdd8e3` the hook called the 4-argument `build_and_persist_graph`,
//!   which stamps `high_cost_mode: None`.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::sqry_bin;
use tempfile::TempDir;

const PLUGIN_ENV: [&str; 4] = [
    "SQRY_INCLUDE_HIGH_COST",
    "SQRY_EXCLUDE_HIGH_COST",
    "SQRY_ENABLE_PLUGINS",
    "SQRY_DISABLE_PLUGINS",
];

fn write_fixture(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("src dir");
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
    )
    .expect("write lib.rs");
    fs::write(
        root.join("config.json"),
        r#"{"name": "fixture", "nested": {"enabled": true}}"#,
    )
    .expect("write config.json");
}

fn sqry(args: &[&str], cwd: &Path) -> std::process::Output {
    let mut command = Command::new(sqry_bin());
    command
        .args(args)
        .current_dir(cwd)
        .env("NO_COLOR", "1")
        .env("SQRY_FORCE_STANDALONE", "1");
    for key in PLUGIN_ENV {
        command.env_remove(key);
    }
    command.output().expect("run sqry")
}

fn recorded_selection(root: &Path) -> serde_json::Value {
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join(".sqry/graph/manifest.json")).expect("manifest bytes"),
    )
    .expect("manifest json");
    manifest["plugin_selection"].clone()
}

#[test]
fn index_force_over_unreadable_manifest_warns_on_stderr() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    let first = sqry(&["index", &root.display().to_string()], &root);
    assert!(
        first.status.success(),
        "initial index must succeed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let manifest_path = root.join(".sqry/graph/manifest.json");
    fs::write(&manifest_path, b"{}").expect("unparseable manifest");

    let forced = sqry(&["index", "--force", &root.display().to_string()], &root);
    let stderr = String::from_utf8_lossy(&forced.stderr).to_string();
    let stdout = String::from_utf8_lossy(&forced.stdout).to_string();
    assert!(
        forced.status.success(),
        "a forced index over an unreadable manifest must rebuild; stderr: {stderr}"
    );
    assert!(
        stderr.contains(&manifest_path.display().to_string()),
        "stderr must name the unreadable manifest; stderr was: {stderr:?}"
    );
    assert!(
        stdout.contains("Plugin selection: fast_path_default"),
        "the banner must name the selection and its source; stdout was: {stdout:?}"
    );
    let selection = recorded_selection(&root);
    assert_eq!(
        selection["high_cost_mode"].as_str(),
        Some("fast_path_default"),
        "the fallback must be recorded: {selection}"
    );
}

#[test]
fn query_auto_index_records_the_fast_path_selection() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    assert!(
        !root.join(".sqry").exists(),
        "fixture precondition: no index"
    );

    let out = sqry(
        &["query", "kind:function", &root.display().to_string()],
        &root,
    );
    assert!(
        out.status.success(),
        "sqry query over a root with no index auto-indexes; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        root.join(".sqry/graph/manifest.json").exists(),
        "the auto-index must have written a manifest"
    );
    let selection = recorded_selection(&root);
    let ids: Vec<&str> = selection["active_plugin_ids"]
        .as_array()
        .expect("active_plugin_ids array")
        .iter()
        .map(|id| id.as_str().expect("id string"))
        .collect();
    assert!(
        !ids.contains(&"json"),
        "the auto-index must use the fast path, got {ids:?}"
    );
    assert_eq!(
        selection["high_cost_mode"].as_str(),
        Some("fast_path_default"),
        "the auto-index must record the mode it resolved, not None: {selection}"
    );
}

const R3_PLANTED_ID: &str = "w1-r3-planted-plugin";

/// Index `root` with the fast path and plant `R3_PLANTED_ID` into the
/// recorded selection; returns the manifest bytes after planting.
fn index_and_plant(root: &Path) -> Vec<u8> {
    let first = sqry(&["index", &root.display().to_string()], root);
    assert!(
        first.status.success(),
        "initial index must succeed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let manifest_path = root.join(".sqry/graph/manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("manifest")).expect("json");
    let ids = manifest["plugin_selection"]["active_plugin_ids"]
        .as_array_mut()
        .expect("active_plugin_ids array");
    assert!(
        !ids.iter().any(|id| id.as_str() == Some(R3_PLANTED_ID)),
        "precondition: the id is not recorded yet"
    );
    ids.push(serde_json::Value::String(R3_PLANTED_ID.to_string()));
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("manifest rewritten");
    fs::read(&manifest_path).expect("manifest bytes")
}

/// T35, planted-id leg (surface parity W1 round 3, design D17, codex
/// Finding 3): `sqry index <root>` with no flags over an existing index
/// whose manifest names an id this binary did not compile exits non-zero,
/// names the id on stderr, and leaves the manifest bytes unchanged. On
/// `416debe48` it printed "Index already exists" and exited 0.
#[test]
fn index_without_force_refuses_a_recorded_selection_naming_an_uncompiled_plugin() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    let bytes_before = index_and_plant(&root);

    let out = sqry(&["index", &root.display().to_string()], &root);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "sqry index without --force must refuse the planted id; stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stderr.contains(R3_PLANTED_ID),
        "stderr must name the id; stderr was: {stderr:?}"
    );
    assert!(
        !stdout.contains("Index already exists"),
        "the index must not be reported before it is classified; stdout: {stdout}"
    );
    assert_eq!(
        fs::read(root.join(".sqry/graph/manifest.json")).expect("manifest bytes"),
        bytes_before,
        "the refusal must leave the manifest bytes unchanged"
    );
    println!(
        "T35 planted id: exit={:?} stderr={stderr}",
        out.status.code()
    );
}

/// T35, unreadable-manifest leg: `sqry index <root>` with no flags over an
/// existing index whose manifest cannot be read exits non-zero, names the
/// manifest path and `--force` on stderr, and leaves the bytes unchanged.
#[test]
fn index_without_force_refuses_an_unreadable_manifest() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    let first = sqry(&["index", &root.display().to_string()], &root);
    assert!(first.status.success(), "initial index must succeed");
    let manifest_path = root.join(".sqry/graph/manifest.json");
    fs::write(&manifest_path, b"{}").expect("unparseable manifest");
    let bytes_before = fs::read(&manifest_path).expect("manifest bytes");

    let out = sqry(&["index", &root.display().to_string()], &root);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "sqry index without --force must refuse an unreadable manifest; stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stderr.contains(&manifest_path.display().to_string()),
        "stderr must name the manifest; stderr was: {stderr:?}"
    );
    assert!(
        stderr.contains("--force"),
        "stderr must name --force as the repair; stderr was: {stderr:?}"
    );
    assert!(
        !stdout.contains("Index already exists"),
        "the index must not be reported before it is classified; stdout: {stdout}"
    );
    assert_eq!(
        fs::read(&manifest_path).expect("manifest bytes"),
        bytes_before,
        "the refusal must leave the manifest bytes unchanged"
    );
    println!(
        "T35 unreadable manifest: exit={:?} stderr={stderr}",
        out.status.code()
    );
}
