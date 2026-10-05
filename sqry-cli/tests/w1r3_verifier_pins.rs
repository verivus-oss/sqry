//! Verifier pins for surface parity W1, round 3 (battery rows VR10 and
//! VR11). Design D18 puts the CLI's build hook on every executor that
//! loads through its own `get_or_load_graph`: the pipeline executor (T36,
//! T37), the batch executor and the join executor. The round 3 battery
//! found the batch and join sites unobserved: removing either hook
//! survived every shipped test. These two tests are their oracles.
//!
//! `sqry batch` refuses an unindexed root before its executor exists
//! (`ensure_index_exists`), so the batch hook is reachable only on the
//! executor's load-failure branch: an index whose snapshot cannot load.
//! The join path has no such gate and reaches the missing-index branch.

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::sqry_bin;
use tempfile::TempDir;

const PLUGIN_ENV: [&str; 5] = [
    "SQRY_INCLUDE_HIGH_COST",
    "SQRY_EXCLUDE_HIGH_COST",
    "SQRY_ENABLE_PLUGINS",
    "SQRY_DISABLE_PLUGINS",
    "SQRY_AUTO_INDEX",
];

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

fn write_fixture(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("src dir");
    fs::write(
        root.join("src/lib.rs"),
        "pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
    )
    .expect("write lib.rs");
    fs::write(
        root.join("config.json"),
        r#"{"name": "fixture", "nested": {"enabled": true}}"#,
    )
    .expect("write config.json");
}

fn recorded_selection(root: &Path) -> serde_json::Value {
    let manifest: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join(".sqry/graph/manifest.json")).expect("manifest bytes"),
    )
    .expect("manifest json");
    manifest["plugin_selection"].clone()
}

fn ids_of(selection: &serde_json::Value) -> Vec<String> {
    selection["active_plugin_ids"]
        .as_array()
        .expect("active_plugin_ids array")
        .iter()
        .map(|id| id.as_str().expect("id string").to_string())
        .collect()
}

fn plugin_ids(plugins: &sqry_core::plugin::PluginManager) -> Vec<String> {
    plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect()
}

/// VR11's oracle: the join executor over a root with no index builds
/// through the hook and records `fast_path_default` with the fast-path
/// ids. Red on `deb423b87` (`high_cost_mode` absent); red under VR11
/// (no hook: the join answers "no graph" and exits non-zero).
#[test]
fn cli_join_auto_index_records_the_fast_path_selection() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    assert!(
        !root.join(".sqry").exists(),
        "fixture precondition: no index"
    );

    let out = sqry(
        &[
            "query",
            "(kind:function) CALLS (kind:function)",
            &root.display().to_string(),
        ],
        &root,
    );
    assert!(
        out.status.success(),
        "the join query over an unindexed root auto-indexes; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let selection = recorded_selection(&root);
    assert_eq!(
        selection["high_cost_mode"].as_str(),
        Some("fast_path_default"),
        "the join auto-index must record the mode it resolved, not None: {selection}"
    );
    assert_eq!(
        ids_of(&selection),
        plugin_ids(&sqry_plugin_registry::create_plugin_manager()),
        "the join auto-index records the fast-path ids"
    );
}

/// VR10's oracle: `sqry batch` over an `include_all` manifest beside a
/// snapshot that cannot load rebuilds through the hook and keeps
/// `high_cost_mode: include_all` with the recorded ids. Red on
/// `deb423b87` (`high_cost_mode` absent); red under VR10 (no hook: the
/// load error is the answer and the batch exits non-zero).
#[test]
fn cli_batch_self_heal_keeps_the_include_all_selection() {
    use sqry_core::graph::unified::persistence::{
        BuildProvenance, GraphStorage, Manifest, PluginSelectionManifest,
    };

    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    write_fixture(&root);
    let full_ids = plugin_ids(&sqry_plugin_registry::create_plugin_manager_all());
    assert!(
        full_ids.iter().any(|id| id == "json"),
        "fixture precondition"
    );
    let storage = GraphStorage::new(&root);
    fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    Manifest::new(
        root.to_string_lossy().to_string(),
        1,
        1,
        "fixture-sha256",
        BuildProvenance::new("test", "test:vr10"),
    )
    .with_plugin_selection(Some(PluginSelectionManifest {
        active_plugin_ids: full_ids.clone(),
        high_cost_mode: Some("include_all".to_string()),
    }))
    .save(storage.manifest_path())
    .expect("manifest saved");
    fs::write(storage.snapshot_path(), b"not a sqry snapshot").expect("corrupt snapshot");
    let queries = root.join("queries.txt");
    fs::write(&queries, "kind:function\n").expect("queries file");

    let out = sqry(
        &[
            "batch",
            "--queries",
            &queries.display().to_string(),
            "--output",
            "jsonl",
            &root.display().to_string(),
        ],
        &root,
    );
    assert!(
        out.status.success(),
        "the batch self-heals over a corrupt snapshot; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let selection = recorded_selection(&root);
    assert_eq!(
        selection["high_cost_mode"].as_str(),
        Some("include_all"),
        "the batch self-heal must carry high_cost_mode through, not drop it to None: {selection}"
    );
    assert_eq!(
        ids_of(&selection),
        full_ids,
        "the batch self-heal records exactly the ids the manifest recorded"
    );
}
