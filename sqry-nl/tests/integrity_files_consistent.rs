//! NL05: post-regeneration integrity verification.
//!
//! Asserts the tracked `sqry-nl/models/` metadata is internally
//! consistent after the NL05 checksums regeneration.
//!
//! These tests are the regression guard for the version.txt hash
//! drift that motivated NL05 (FR-9 / DAG unit NL05), but active CI
//! must not require ignored external model artifacts to be present in
//! a clean checkout.

#![cfg(feature = "classifier")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use sqry_nl::classifier::{BAKED_MANIFEST, IntentClassifier, Manifest, TrustMode};

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Compute the lowercase hex sha256 of a byte slice.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

/// Path to the in-tree committed `sqry-nl/models/` directory.
fn models_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models")
}

fn read_manifest(dir: &Path) -> Manifest {
    let manifest_bytes =
        fs::read_to_string(dir.join("manifest.json")).expect("read sqry-nl/models/manifest.json");
    Manifest::parse(&manifest_bytes).expect("parse manifest.json")
}

fn read_checksums(dir: &Path) -> (Vec<u8>, BTreeMap<String, String>) {
    let checksums_bytes =
        fs::read(dir.join("checksums.json")).expect("read sqry-nl/models/checksums.json");
    let checksums: BTreeMap<String, String> =
        serde_json::from_slice(&checksums_bytes).expect("parse checksums.json");
    (checksums_bytes, checksums)
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn assert_tracked_metadata_consistent(dir: &Path) {
    let manifest = read_manifest(dir);
    let (checksums_bytes, checksums) = read_checksums(dir);

    assert!(
        !checksums.contains_key("checksums.json"),
        "checksums.json must not hash itself; manifest.json anchors checksums.json instead"
    );

    let recorded = manifest
        .files
        .get("checksums.json")
        .expect("manifest.json.files['checksums.json'] must be present");
    let actual_hash = sha256_hex(&checksums_bytes);

    assert_eq!(
        &actual_hash, recorded,
        "manifest.json.files['checksums.json'] must match the byte \
         hash of sqry-nl/models/checksums.json — keeps the integrity \
         chain self-consistent"
    );

    for (filename, checksums_hash) in &checksums {
        assert!(
            is_sha256_hex(checksums_hash),
            "checksums.json entry for {filename} must be lowercase sha256 hex"
        );
        let manifest_hash = manifest
            .files
            .get(filename)
            .unwrap_or_else(|| panic!("manifest.json.files must include {filename}"));
        assert_eq!(
            manifest_hash, checksums_hash,
            "manifest.json.files[{filename}] must match checksums.json[{filename}]"
        );
    }

    for (filename, manifest_hash) in &manifest.files {
        assert!(
            is_sha256_hex(manifest_hash),
            "manifest.json.files entry for {filename} must be lowercase sha256 hex"
        );
    }
}

#[test]
fn tracked_manifest_and_checksums_are_internally_consistent() {
    assert_tracked_metadata_consistent(&models_dir());
}

#[test]
fn baked_manifest_matches_tracked_manifest() {
    let manifest = read_manifest(&models_dir());

    assert_eq!(BAKED_MANIFEST.model_version, manifest.model_version);
    assert_eq!(BAKED_MANIFEST.release_tag, manifest.release_tag);
    assert_eq!(BAKED_MANIFEST.archive, manifest.archive);
    assert_eq!(BAKED_MANIFEST.sha256, manifest.sha256);
    assert_eq!(BAKED_MANIFEST.download_url, manifest.download_url);
    assert_eq!(BAKED_MANIFEST.files, manifest.files);
}

#[test]
fn metadata_only_clean_checkout_fixture_is_sufficient_for_active_tests() {
    let tmp = tempfile::TempDir::new().expect("create clean-checkout fixture");
    let dir = tmp.path().join("models");
    fs::create_dir_all(&dir).expect("create models dir");

    let real_models = models_dir();
    fs::copy(real_models.join("manifest.json"), dir.join("manifest.json"))
        .expect("copy tracked manifest.json");
    fs::copy(
        real_models.join("checksums.json"),
        dir.join("checksums.json"),
    )
    .expect("copy tracked checksums.json");

    for ignored_artifact in [
        "intent_classifier.onnx",
        "tokenizer.json",
        "config.json",
        "temperature.json",
        "version.txt",
    ] {
        assert!(
            !dir.join(ignored_artifact).exists(),
            "clean-checkout fixture must not contain ignored artifact {ignored_artifact}"
        );
    }

    assert_tracked_metadata_consistent(&dir);
}

#[test]
#[ignore = "requires external model archive / ONNX Runtime dylib; ignored artifacts are not committed"]
fn strict_load_against_external_model_tree_succeeds() {
    IntentClassifier::verify_integrity_for_tests(
        &models_dir(),
        /*allow_unverified=*/ false,
        TrustMode::Trusted,
    )
    .expect(
        "strict trusted-mode integrity must pass against the \
         external sqry-nl/models/ tree (NL05 manual acceptance gate)",
    );
}
