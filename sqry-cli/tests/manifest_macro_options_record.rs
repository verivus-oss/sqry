//! The behavioural companions for T8 and T9 (surface parity W4 round 2, plan
//! step S8).
//!
//! T8's committed command form was `cargo test -p sqry-core --lib --
//! persistence::manifest::tests::manifest_` and T9's was `cargo test -p
//! sqry-core --lib -- build::macro_options::tests`. At round 1's pre-change
//! head both filters matched tests that did not exist there, so each printed
//! `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3729 filtered
//! out` and exited 0: a zero-case, not a red. Run by exact path with the test
//! planted, each is a compile error there, because the record type and the
//! resolver are new.
//!
//! These companions carry the same claims as assertions over what the `sqry`
//! binary writes, through APIs that exist at round 1's pre-change head as well
//! as here (the manifest read as JSON text, and the graph's own macro
//! metadata), so they compile there and fail their assertions there:
//!
//! - T8: `sqry index --cfg test` records what it was given, so the manifest
//!   carries a `macro_options` block naming the flag and no cache key, and a
//!   plain index carries no block at all (invariant I1).
//! - T9: a rebuild with no flags reuses the record (`sqry index --force` and
//!   `sqry update` keep the cfg item active), and `--no-macro-options` drops
//!   it.
//!
//! The existing `sqry-cli/tests/macro_options_reuse.rs` asserts the same
//! reuse, but it names `MacroOptionsManifest`, which does not exist at the
//! pre-change head, so it cannot be T9's companion there.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

mod common;

use std::path::Path;
use std::process::Command;

use common::sqry_bin;
use sqry_core::graph::unified::persistence::{GraphStorage, load_from_path};
use tempfile::TempDir;

const CFG_LIB_RS: &str =
    "#[cfg(test)]\npub fn gated_by_test() -> u32 { 1 }\n\npub fn always_present() -> u32 { 2 }\n";
const CARGO_TOML: &str =
    "[package]\nname = \"w4_record_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

fn cfg_fixture() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), CARGO_TOML).expect("write Cargo.toml");
    std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
    std::fs::write(dir.path().join("src").join("lib.rs"), CFG_LIB_RS).expect("write lib.rs");
    dir
}

/// Run `sqry <args> <root>`; `None` on success, the failure text otherwise.
fn sqry(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(sqry_bin())
        .args(args)
        .arg(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry");
    println!("sqry {args:?}: exit {:?}", output.status.code());
    (!output.status.success()).then(|| {
        format!(
            "sqry {args:?} exited {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn manifest_document(root: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(GraphStorage::new(root).manifest_path())
        .expect("manifest readable");
    serde_json::from_str(&text).expect("manifest is JSON")
}

/// The activation the persisted graph records for the fixture's
/// `#[cfg(test)]` item: `Some(true)` only when the build ran with
/// `--cfg test`.
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

fn report(failed: &[String]) {
    println!("failed checks: {}", failed.len());
    for failure in failed {
        println!("  {failure}");
    }
    assert!(
        failed.is_empty(),
        "{} check(s) failed: {failed:#?}",
        failed.len()
    );
}

/// T8's companion: the manifest records the macro options the index was
/// given and names the flag; a plain index records nothing.
#[test]
fn the_manifest_records_the_cfg_flag_the_index_was_given() {
    let dir = cfg_fixture();
    let root = dir.path();
    let mut failed: Vec<String> = Vec::new();

    if let Some(failure) = sqry(root, &["index"]) {
        panic!("instrument: the plain index must succeed: {failure}");
    }
    let plain = manifest_document(root);
    println!("plain index macro_options: {}", plain["macro_options"]);
    if !plain["macro_options"].is_null() {
        failed.push(format!(
            "a plain index recorded macro options: {}",
            plain["macro_options"]
        ));
    }

    if let Some(failure) = sqry(root, &["index", "--force", "--cfg", "test"]) {
        failed.push(failure);
    }
    let document = manifest_document(root);
    let block = &document["macro_options"];
    println!("recorded macro_options: {block}");
    let flags: Vec<String> = block["cfg_flags"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    println!("recorded cfg flags: {flags:?}");
    if block.is_null() {
        failed.push("the manifest carries no macro_options block after --cfg test".to_string());
    }
    if flags != vec!["test".to_string()] {
        failed.push(format!("the block names {flags:?}, not [\"test\"]"));
    }
    if !block["expand_cache_dir"].is_null() {
        failed.push(format!(
            "no expand cache was given, but the block names one: {block}"
        ));
    }
    report(&failed);
}

/// T9's companion: a rebuild with no flags reuses the recorded options, and
/// `--no-macro-options` drops them.
#[test]
fn a_rebuild_reuses_the_recorded_cfg_flags_until_they_are_dropped() {
    let dir = cfg_fixture();
    let root = dir.path();
    let mut failed: Vec<String> = Vec::new();

    if let Some(failure) = sqry(root, &["index", "--cfg", "test"]) {
        panic!("instrument: the flagged index must succeed: {failure}");
    }
    let first = cfg_test_activation(root);
    println!("activation after the flagged index: {first:?}");
    assert_eq!(
        first,
        Some(true),
        "instrument: --cfg test activates the gated item"
    );

    if let Some(failure) = sqry(root, &["index", "--force"]) {
        failed.push(failure);
    }
    let forced = cfg_test_activation(root);
    println!("activation after the forced rebuild: {forced:?}");
    if forced != Some(true) {
        failed.push(format!(
            "sqry index --force with no flags gave {forced:?}; the recorded --cfg test was not reused"
        ));
    }

    if let Some(failure) = sqry(root, &["update"]) {
        failed.push(failure);
    }
    let updated = cfg_test_activation(root);
    println!("activation after the update: {updated:?}");
    if updated != Some(true) {
        failed.push(format!(
            "sqry update gave {updated:?}; the recorded --cfg test was not reused"
        ));
    }

    let dropped_ok = sqry(root, &["index", "--force", "--no-macro-options"]);
    if let Some(failure) = dropped_ok {
        failed.push(failure);
    } else {
        let dropped = cfg_test_activation(root);
        println!("activation after --no-macro-options: {dropped:?}");
        if dropped.is_some() {
            failed.push(format!(
                "sqry index --force --no-macro-options gave {dropped:?}; the record was not dropped"
            ));
        }
        let block = manifest_document(root)["macro_options"].clone();
        if !block.is_null() {
            failed.push(format!(
                "the dropped record is still in the manifest: {block}"
            ));
        }
    }
    report(&failed);
}
