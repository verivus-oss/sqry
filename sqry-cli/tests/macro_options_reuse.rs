//! T10 and the CLI legs of T14 (surface parity W4, design W4-D6, W4-D7):
//! the macro build options `sqry index` was given are recorded in the
//! manifest and reused by `sqry index --force` and `sqry update`, dropped by
//! `sqry index --force --no-macro-options`, and a recorded expand cache
//! directory that no longer exists is refused by name on every CLI rebuild
//! (nothing written) until the record is reset.
//!
//! The oracle is the Rust plugin's own cfg activation: a `#[cfg(test)]`
//! item's `cfg_active` is `Some(true)` only when the build ran with
//! `--cfg test`, `None` otherwise (`sqry-lang-rust/tests/cfg_flag_wiring.rs`
//! is the existing proof of that channel). On the pre-change head the
//! forced rebuild and the update both gave `None`, the source line did not
//! exist, `--no-macro-options` was rejected by clap, and the manifest had
//! no `macro_options` key.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

mod common;

use std::path::Path;
use std::process::Output;

use common::sqry_bin;
use sha2::{Digest, Sha256};
use sqry_core::graph::unified::persistence::{GraphStorage, MacroOptionsManifest, load_from_path};
use tempfile::TempDir;

const CFG_LIB_RS: &str =
    "#[cfg(test)]\npub fn gated_by_test() -> u32 { 1 }\n\npub fn always_present() -> u32 { 2 }\n";
const CARGO_TOML: &str =
    "[package]\nname = \"w4_cfg_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

fn cfg_fixture() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), CARGO_TOML).expect("write Cargo.toml");
    std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
    std::fs::write(dir.path().join("src").join("lib.rs"), CFG_LIB_RS).expect("write lib.rs");
    dir
}

fn sqry(root: &Path, args: &[&str]) -> Output {
    let mut cmd = std::process::Command::new(sqry_bin());
    cmd.args(args).arg(root).env("NO_COLOR", "1");
    cmd.output().expect("run sqry")
}

fn sqry_ok(root: &Path, args: &[&str]) -> String {
    let output = sqry(root, args);
    assert!(
        output.status.success(),
        "sqry {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The `(cfg_condition, cfg_active)` pairs the persisted graph records,
/// and the activation of the fixture's `cfg(test)` item.
fn cfg_test_activation(root: &Path) -> Option<bool> {
    let storage = GraphStorage::new(root);
    let graph = load_from_path(storage.snapshot_path(), None).expect("snapshot loads");
    let pairs: Vec<(String, Option<bool>)> = graph
        .macro_metadata()
        .iter()
        .filter_map(|(_, meta)| meta.cfg_condition.clone().map(|c| (c, meta.cfg_active)))
        .collect();
    println!("recorded cfg pairs: {pairs:?}");
    pairs
        .iter()
        .find(|(condition, _)| condition.contains("test"))
        .unwrap_or_else(|| {
            panic!("fixture precondition: the cfg(test) item is recorded: {pairs:?}")
        })
        .1
}

fn recorded_macro_options(root: &Path) -> Option<MacroOptionsManifest> {
    GraphStorage::new(root)
        .load_manifest()
        .expect("manifest readable")
        .macro_options
}

fn sha256_of(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read file");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn index_force_and_update_reuse_the_recorded_cfg_flags() {
    let dir = cfg_fixture();
    let root = dir.path();

    let stdout = sqry_ok(root, &["index", "--cfg", "test"]);
    println!("{stdout}");
    assert!(
        stdout.contains("Macro options: cfg [test] (from the flags)"),
        "sqry index names the source of the options: {stdout}"
    );
    assert_eq!(
        cfg_test_activation(root),
        Some(true),
        "--cfg test activates the item"
    );
    let record = recorded_macro_options(root).expect("the manifest records the options");
    assert_eq!(record.cfg_flags, vec!["test".to_string()]);
    assert_eq!(record.expand_cache_dir, None);

    let stdout = sqry_ok(root, &["index", "--force"]);
    println!("{stdout}");
    assert!(
        stdout.contains("Macro options: cfg [test] (recorded in the manifest)"),
        "a forced rebuild says it reused the record: {stdout}"
    );
    assert_eq!(
        cfg_test_activation(root),
        Some(true),
        "sqry index --force must rebuild with the recorded cfg flags"
    );
    assert_eq!(
        recorded_macro_options(root).map(|r| r.cfg_flags),
        Some(vec!["test".to_string()])
    );

    sqry_ok(root, &["update"]);
    assert_eq!(
        cfg_test_activation(root),
        Some(true),
        "sqry update must rebuild with the recorded cfg flags"
    );
    assert_eq!(
        recorded_macro_options(root).map(|r| r.cfg_flags),
        Some(vec!["test".to_string()])
    );

    let stdout = sqry_ok(root, &["index", "--force", "--no-macro-options"]);
    println!("{stdout}");
    assert!(
        stdout.contains("Macro options: none (no record, no flags)"),
        "the reset says there is nothing: {stdout}"
    );
    assert_eq!(
        cfg_test_activation(root),
        None,
        "--no-macro-options drops the recorded cfg flags"
    );
    assert_eq!(
        recorded_macro_options(root),
        None,
        "the key is gone from the manifest"
    );

    // An explicit flag beside the reset applies to the cleared record.
    sqry_ok(
        root,
        &[
            "index",
            "--force",
            "--no-macro-options",
            "--cfg",
            "feature=w4",
        ],
    );
    let record = recorded_macro_options(root).expect("recorded");
    assert_eq!(record.cfg_flags, vec!["feature=w4".to_string()]);
    assert_eq!(
        cfg_test_activation(root),
        Some(false),
        "test is inactive under feature=w4"
    );
}

/// The ADD-form control, CLI legs (T14 legs 1, 5 and 6): the manifest names
/// an expand cache directory that is then removed; `sqry update` and
/// `sqry index --force` refuse by name and write nothing; `sqry index
/// --force --no-macro-options` is the way out.
#[test]
fn w4_add_form_control_missing_recorded_expand_cache_refuses_cli_rebuilds() {
    let dir = cfg_fixture();
    let root = dir.path();
    let cache = root.join("expand-cache");
    std::fs::create_dir_all(&cache).expect("cache dir");
    let cache_str = cache.to_string_lossy().into_owned();

    sqry_ok(
        root,
        &["index", "--cfg", "test", "--expand-cache", &cache_str],
    );
    let record = recorded_macro_options(root).expect("the manifest records the options");
    let recorded_dir = record
        .expand_cache_dir
        .clone()
        .expect("the manifest names the expand cache directory");
    assert_eq!(
        Path::new(&recorded_dir),
        cache.canonicalize().expect("canonical cache").as_path(),
        "the record is the absolute directory"
    );
    assert_eq!(record.cfg_flags, vec!["test".to_string()]);

    std::fs::remove_dir_all(&cache).expect("remove the cache");
    let storage = GraphStorage::new(root);
    let manifest_sha = sha256_of(storage.manifest_path());
    let snapshot_sha = sha256_of(storage.snapshot_path());

    for args in [&["update"][..], &["index", "--force"][..]] {
        let output = sqry(root, args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        println!("sqry {args:?}: exit {:?}\n{stderr}", output.status.code());
        assert!(!output.status.success(), "sqry {args:?} must refuse");
        assert!(
            stderr.contains(&recorded_dir) && stderr.contains("--no-macro-options"),
            "the refusal names the directory and the way out: {stderr}"
        );
        assert_eq!(
            sha256_of(storage.manifest_path()),
            manifest_sha,
            "manifest untouched"
        );
        assert_eq!(
            sha256_of(storage.snapshot_path()),
            snapshot_sha,
            "snapshot untouched"
        );
    }

    sqry_ok(root, &["index", "--force", "--no-macro-options"]);
    assert_eq!(
        recorded_macro_options(root),
        None,
        "the reset drops the record"
    );
    assert_ne!(
        sha256_of(storage.manifest_path()),
        manifest_sha,
        "the reset rebuilt"
    );
    assert_eq!(cfg_test_activation(root), None);
}
