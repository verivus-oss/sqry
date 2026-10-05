//! ADD-form negative control for surface parity W1, verifier round 2, CLI
//! leg: `sqry index --force` with no selection flags over an index whose
//! manifest names `w1-r2-verifier-added-plugin` (an id no build compiles)
//! must exit non-zero, name the id on stderr, and leave the manifest bytes
//! unchanged. Round 2 (design D11) made the forced index reuse the recorded
//! selection; this control shows that reuse cannot turn an unknown id into
//! silent acceptance. The daemon and registry legs are in
//! `sqry-daemon/tests/w1r2_verifier_add_control.rs`.

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::sqry_bin;
use tempfile::TempDir;

const ADDED_ID: &str = "w1-r2-verifier-added-plugin";
const PLUGIN_ENV: [&str; 4] = [
    "SQRY_INCLUDE_HIGH_COST",
    "SQRY_EXCLUDE_HIGH_COST",
    "SQRY_ENABLE_PLUGINS",
    "SQRY_DISABLE_PLUGINS",
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

#[test]
fn w1r2_add_form_control_cli_index_force_refuses_the_added_plugin_id_by_name() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    fs::create_dir_all(root.join("src")).expect("src dir");
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn alpha() -> u32 { 1 }\n",
    )
    .expect("write lib.rs");
    let first = sqry(&["index", &root.display().to_string()], &root);
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
        !ids.iter().any(|id| id.as_str() == Some(ADDED_ID)),
        "precondition: the id is not recorded yet"
    );
    ids.push(serde_json::Value::String(ADDED_ID.to_string()));
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("manifest rewritten");
    let bytes_before = fs::read(&manifest_path).expect("manifest bytes");

    let forced = sqry(&["index", "--force", &root.display().to_string()], &root);
    let stderr = String::from_utf8_lossy(&forced.stderr).to_string();
    assert!(
        !forced.status.success(),
        "a forced index over a manifest naming an uncompiled id must fail; stderr: {stderr}"
    );
    assert!(
        stderr.contains(ADDED_ID),
        "stderr must name the added id; stderr was: {stderr:?}"
    );
    assert_eq!(
        fs::read(&manifest_path).expect("manifest bytes"),
        bytes_before,
        "the refusal must leave the manifest bytes unchanged"
    );
    println!(
        "ADD-FORM r2 cli: exit={:?} stderr={stderr}",
        forced.status.code()
    );
}
