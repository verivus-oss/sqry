//! ADD-form negative control for surface parity W1, round 3, CLI legs
//! (design D17, D18). The control plants `w1-r3-planted-plugin`, an id no
//! build of sqry compiles, into a valid fast-path manifest and requires
//! refusal by name with nothing written:
//!
//! 4. `sqry index <root>` with no flags over the planted manifest beside a
//!    valid snapshot (S27): exit non-zero, stderr names the id;
//! 5. the CLI pipeline path (`sqry query "kind:function | count" <root>`)
//!    over the planted manifest beside a snapshot that cannot load (S28):
//!    the executor's build hook resolves the recorded selection with the
//!    read-only resolver, which refuses the id by name; exit non-zero; the
//!    manifest bytes are unchanged.
//!
//! The daemon legs live in `sqry-daemon/tests/w1r3_add_form_control.rs`.

mod common;

use std::fs;
use std::path::Path;
use std::process::Command;

use common::sqry_bin;
use tempfile::TempDir;

const PLANTED_ID: &str = "w1-r3-planted-plugin";
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

/// Index `root` with the fast path through the binary and plant the id;
/// returns the manifest bytes after planting.
fn index_and_plant(root: &Path) -> Vec<u8> {
    fs::create_dir_all(root.join("src")).expect("src dir");
    fs::write(
        root.join("src").join("lib.rs"),
        "pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
    )
    .expect("write lib.rs");
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
        !ids.iter().any(|id| id.as_str() == Some(PLANTED_ID)),
        "precondition: the id is not recorded yet"
    );
    ids.push(serde_json::Value::String(PLANTED_ID.to_string()));
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("json"),
    )
    .expect("manifest rewritten");
    fs::read(&manifest_path).expect("manifest bytes")
}

/// Leg 4: `sqry index <root>` with no flags.
#[test]
fn w1r3_add_form_control_cli_index_without_force_refuses_the_planted_id_by_name() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    let bytes_before = index_and_plant(&root);

    let out = sqry(&["index", &root.display().to_string()], &root);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !out.status.success(),
        "sqry index with no flags must refuse the planted id; stderr: {stderr}"
    );
    assert!(
        stderr.contains(PLANTED_ID),
        "stderr must name the planted id; stderr was: {stderr:?}"
    );
    assert_eq!(
        fs::read(root.join(".sqry/graph/manifest.json")).expect("manifest bytes"),
        bytes_before,
        "the refusal must leave the manifest bytes unchanged"
    );
    println!(
        "ADD-FORM r3 cli index: exit={:?} stderr={stderr}",
        out.status.code()
    );
}

/// Leg 5 (a declared control, green on the base `deb423b87`; round 4,
/// R4-6): the CLI pipeline path over the planted manifest beside a
/// snapshot that cannot load. The refusal is
/// `create_executor_with_plugins_for_cli`'s read-only resolution
/// (`PluginSelectionMode::ReadOnly`), which runs before the executor and
/// its hook exist, so the executor never reaches its load-failure branch
/// and the hook's own rule is not observed here. The hook's oracle is T42,
/// `cli_auto_build_hook_refuses_an_unreadable_manifest` in
/// `sqry-cli/src/commands/query.rs`, which calls the hook directly.
#[test]
fn w1r3_add_form_control_cli_pipeline_refuses_the_planted_id_by_name() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    let bytes_before = index_and_plant(&root);
    let snapshot_path = root.join(".sqry/graph/snapshot.sqry");
    fs::write(&snapshot_path, b"not a sqry snapshot").expect("corrupt snapshot");
    let snapshot_before = fs::read(&snapshot_path).expect("snapshot bytes");

    let out = sqry(
        &[
            "query",
            "kind:function | count",
            &root.display().to_string(),
        ],
        &root,
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !out.status.success(),
        "the pipeline path must refuse the planted id; stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stderr.contains(PLANTED_ID),
        "stderr must name the planted id; stderr was: {stderr:?}"
    );
    assert_eq!(
        fs::read(root.join(".sqry/graph/manifest.json")).expect("manifest bytes"),
        bytes_before,
        "the refusal must leave the manifest bytes unchanged"
    );
    assert_eq!(
        fs::read(&snapshot_path).expect("snapshot bytes"),
        snapshot_before,
        "the refusal must leave the snapshot bytes unchanged"
    );
    println!(
        "ADD-FORM r3 cli pipeline: exit={:?} stderr={stderr}",
        out.status.code()
    );
}

/// Leg 5b (a declared control, green on the base `deb423b87`; round 4,
/// R4-6): the CLI pipeline path over a manifest that cannot be read,
/// beside a snapshot that cannot load. As in leg 5 the refusal is the
/// executor constructor's read-only resolution, which names the file
/// before the hook exists, so this leg cannot observe the hook's rule and
/// is corroborating only for battery row K29; T42 is that row's
/// discriminating oracle. Exit non-zero, stderr names the manifest, the
/// bytes are unchanged.
#[test]
fn w1r3_add_form_control_cli_pipeline_refuses_an_unreadable_manifest() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().canonicalize().expect("canonical root");
    index_and_plant(&root);
    let manifest_path = root.join(".sqry/graph/manifest.json");
    fs::write(&manifest_path, b"{}").expect("unparseable manifest");
    let snapshot_path = root.join(".sqry/graph/snapshot.sqry");
    fs::write(&snapshot_path, b"not a sqry snapshot").expect("corrupt snapshot");
    let bytes_before = fs::read(&manifest_path).expect("manifest bytes");

    let out = sqry(
        &[
            "query",
            "kind:function | count",
            &root.display().to_string(),
        ],
        &root,
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        !out.status.success(),
        "the pipeline path must refuse an unreadable manifest on the read path; stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stderr.contains(&manifest_path.display().to_string()),
        "stderr must name the manifest; stderr was: {stderr:?}"
    );
    assert_eq!(
        fs::read(&manifest_path).expect("manifest bytes"),
        bytes_before,
        "the refusal must leave the manifest bytes unchanged (no fallback recorded)"
    );
    println!(
        "ADD-FORM r3 cli pipeline over {{}}: exit={:?} stderr={stderr}",
        out.status.code()
    );
}
