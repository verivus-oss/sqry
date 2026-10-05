//! U2 (surface parity W4 round 2, design W4-D11): an expand cache directory
//! is refused where it is accepted if the manifest cannot record it, and a
//! directory whose name is valid UTF-8 but not ASCII is still accepted and
//! round-trips equal.
//!
//! Round 1 recorded the directory through `to_string_lossy`, so a name that
//! is not valid UTF-8 was accepted at index time, written to the JSON
//! manifest with replacement characters, and then refused by the next
//! rebuild because the recorded path no longer named anything on disk. An
//! input a surface accepts has to be reusable as recorded, or refused where
//! it is accepted.
//!
//! The reject leg reaches the resolver the way a user does: through a
//! directory argument that is itself valid UTF-8 (a symlink) whose canonical
//! path is not. The manifest records the canonical path, so that is the form
//! that has to be recordable. A raw argument that is not valid UTF-8 never
//! reaches the resolver at all: the `sqry` binary panics while pre-scanning
//! its arguments with `std::env::args`, which is a separate defect recorded
//! outside this unit.
//!
//! Unix only: a file name that is not valid UTF-8 is a property of the
//! platform's byte path names, and Windows has no way to express one.
//!
//! Record: `docs/development/surface-parity/06_TEST_EXECUTION-surface-parity.md`.

#![cfg(unix)]

mod common;

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::sqry_bin;
use sha2::{Digest, Sha256};
use sqry_core::graph::unified::persistence::GraphStorage;
use tempfile::TempDir;

const LIB_RS: &str = "#[cfg(test)]\npub fn gated() -> u32 { 1 }\n\npub fn always() -> u32 { 2 }\n";
const CARGO_TOML: &str =
    "[package]\nname = \"w4_encoding_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

fn fixture() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    std::fs::write(dir.path().join("Cargo.toml"), CARGO_TOML).expect("write Cargo.toml");
    std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
    std::fs::write(dir.path().join("src").join("lib.rs"), LIB_RS).expect("write lib.rs");
    dir
}

fn sqry(root: &Path, args: &[&OsStr]) -> Output {
    Command::new(sqry_bin())
        .args(args)
        .arg(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run sqry")
}

fn sqry_ok(root: &Path, args: &[&OsStr]) -> Output {
    let output = sqry(root, args);
    assert!(
        output.status.success(),
        "sqry {args:?} must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn sha256_of(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read file");
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn manifest_document(root: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(GraphStorage::new(root).manifest_path())
        .expect("manifest readable");
    serde_json::from_str(&text).expect("manifest is JSON")
}

/// A directory whose name carries a byte that is not valid UTF-8, reached
/// through a symlink whose own path is valid UTF-8. The platform accepts the
/// name, `Path::to_str` does not, and JSON cannot carry it. Returns the
/// symlink (the argument) and the canonical directory (what would be
/// recorded).
fn non_utf8_dir_behind_a_utf8_link(under: &Path) -> (PathBuf, PathBuf) {
    let mut name = b"expand-cache-".to_vec();
    name.push(0xff);
    let dir = under.join(OsString::from_vec(name));
    std::fs::create_dir_all(&dir).expect("create the non-UTF-8 directory");
    let link = under.join("expand-cache-link");
    std::os::unix::fs::symlink(&dir, &link).expect("symlink to the non-UTF-8 directory");
    let canonical = link.canonicalize().expect("canonical target");
    assert!(
        link.to_str().is_some(),
        "instrument: the argument itself must be valid UTF-8"
    );
    assert!(
        canonical.to_str().is_none(),
        "instrument: the canonical directory must not be valid UTF-8"
    );
    assert!(link.is_dir(), "instrument: the argument names a directory");
    (link, canonical)
}

/// U2 reject leg: a cache directory the manifest cannot record is refused
/// where it is accepted, and nothing is written. Every failed check is
/// collected and reported together.
#[test]
fn a_non_utf8_expand_cache_directory_is_refused_and_writes_nothing() {
    let project = fixture();
    let root = project.path();
    let cache_home = TempDir::new().expect("cache tempdir");

    // A plain index first, so there is a manifest to compare against and the
    // "no macro_options key" check below is about a real document.
    sqry_ok(root, &[OsStr::new("index")]);
    let storage = GraphStorage::new(root);
    let manifest_before = sha256_of(storage.manifest_path());
    let snapshot_before = sha256_of(storage.snapshot_path());
    assert!(
        manifest_document(root)["macro_options"].is_null(),
        "instrument: the plain index records no macro options"
    );

    let (link, canonical) = non_utf8_dir_behind_a_utf8_link(cache_home.path());
    let output = sqry(
        root,
        &[
            OsStr::new("index"),
            OsStr::new("--force"),
            OsStr::new("--expand-cache"),
            link.as_os_str(),
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let manifest_after = sha256_of(storage.manifest_path());
    let snapshot_after = sha256_of(storage.snapshot_path());
    let document = manifest_document(root);
    let recorded = document["macro_options"]["expand_cache_dir"]
        .as_str()
        .map(|text| text.as_bytes().to_vec());
    println!("exit status: {:?}", output.status.code());
    println!("--- stderr ---\n{stderr}");
    println!("manifest sha256 {manifest_before} -> {manifest_after}");
    println!("snapshot sha256 {snapshot_before} -> {snapshot_after}");
    println!("recorded expand_cache_dir bytes: {recorded:?}");
    println!(
        "canonical directory bytes: {:?}",
        canonical.as_os_str().as_encoded_bytes()
    );

    let mut failed: Vec<String> = Vec::new();
    if output.status.success() {
        failed.push(format!(
            "sqry index accepted a directory the manifest cannot record (exit {:?}); recorded \
             bytes {recorded:?}",
            output.status.code()
        ));
    }
    if stderr.contains("panicked") {
        failed.push(format!("sqry panicked instead of refusing: {stderr}"));
    }
    if !stderr.contains("not valid UTF-8") {
        failed.push(format!("stderr does not say why: {stderr}"));
    }
    if !(stderr.contains("--expand-cache") && stderr.contains("--no-macro-options")) {
        failed.push(format!("stderr does not name the two ways out: {stderr}"));
    }
    if manifest_after != manifest_before {
        failed.push(format!(
            "the refused index wrote the manifest: sha256 {manifest_before} before, \
             {manifest_after} after"
        ));
    }
    if snapshot_after != snapshot_before {
        failed.push(format!(
            "the refused index wrote the snapshot: sha256 {snapshot_before} before, \
             {snapshot_after} after"
        ));
    }
    if !document["macro_options"].is_null() {
        failed.push(format!(
            "the manifest carries a macro_options record: {}",
            document["macro_options"]
        ));
    }

    println!("failed checks: {}", failed.len());
    assert!(
        failed.is_empty(),
        "{} check(s) failed:\n- {}",
        failed.len(),
        failed.join("\n- ")
    );
}

/// U2 accept leg, invariant I10: a directory name that is valid UTF-8 but
/// not ASCII is accepted, recorded, and reusable as recorded. Without this
/// leg the repair could be "refuse everything that is not ASCII".
#[test]
fn a_non_ascii_utf8_expand_cache_directory_is_accepted_and_round_trips() {
    let project = fixture();
    let root = project.path();
    let cache_home = TempDir::new().expect("cache tempdir");
    let dir = cache_home
        .path()
        .join("expand-cach\u{e9}-\u{3a9}-\u{43a}\u{435}\u{448}");
    std::fs::create_dir_all(&dir).expect("create the non-ASCII directory");
    let canonical = dir.canonicalize().expect("canonical cache");
    assert!(
        !canonical.to_str().expect("valid UTF-8").is_ascii(),
        "instrument: the planted directory name must be non-ASCII"
    );

    sqry_ok(
        root,
        &[
            OsStr::new("index"),
            OsStr::new("--cfg"),
            OsStr::new("test"),
            OsStr::new("--expand-cache"),
            dir.as_os_str(),
        ],
    );

    let recorded = manifest_document(root)["macro_options"]["expand_cache_dir"]
        .as_str()
        .expect("the manifest records the directory")
        .to_string();
    println!("recorded: {recorded}");
    assert_eq!(
        Path::new(&recorded),
        canonical.as_path(),
        "the recorded string is the canonical directory, byte for byte"
    );

    // Reusable as recorded: the next rebuild resolves the record and exits 0.
    let output = sqry_ok(root, &[OsStr::new("update")]);
    println!(
        "update exit {:?}; stdout:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout)
    );
    let after = manifest_document(root)["macro_options"]["expand_cache_dir"]
        .as_str()
        .expect("the update keeps the record")
        .to_string();
    assert_eq!(after, recorded, "the record survives the rebuild unchanged");
}
