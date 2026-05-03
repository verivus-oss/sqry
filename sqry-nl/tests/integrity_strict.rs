//! NL04 — Strict Integrity by Default tests.
//!
//! These integration tests exercise the
//! [`sqry_nl::classifier::IntentClassifier::load`] integrity contract
//! introduced by NL04. The contract is documented in detail at the
//! "NL04 Integrity Contract — AUTHORITATIVE" comment block in
//! `sqry-nl/src/classifier/model.rs`. The short version:
//!
//! - **Tampering** (a present file whose sha256 does NOT match
//!   `checksums.json`) ALWAYS errors regardless of `allow_unverified`.
//! - **Missingness** (`checksums.json` absent, or a listed file
//!   absent) is fatal in strict mode (default), warn-and-skip with
//!   `allow_unverified == true`.
//! - **Trusted mode** anchors `checksums.json` integrity in the
//!   binary's baked-in expected manifest. Anchor mismatch is ALWAYS
//!   fatal, even with `allow_unverified == true`.
//! - **Custom mode** anchors integrity in the local user-supplied
//!   `manifest.json`. `Translator::new` emits a loud `tracing::warn!`.
//!
//! Synthetic on-disk fixtures are used: the tests fail at
//! `verify_integrity` BEFORE any ONNX session creation, so the stub
//! `intent_classifier.onnx` bytes never need to be a valid model.

#![cfg(feature = "classifier")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use sqry_nl::classifier::{
    DirsLike, IntentClassifier, Manifest, ResolverLevel, TrustMode, resolve_model_dir,
};
use sqry_nl::error::ClassifierError;
use tempfile::TempDir;

struct MockDirs(PathBuf);

impl DirsLike for MockDirs {
    fn cache_dir(&self) -> Option<PathBuf> {
        Some(self.0.clone())
    }
}

fn copy_tracked_metadata_only_model_dir(dir: &Path) {
    fs::create_dir_all(dir).expect("create metadata-only model dir");

    let real_models = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models");
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
}

// ---------------------------------------------------------------------------
// Fixture builder
// ---------------------------------------------------------------------------

/// In-memory description of the on-disk model directory layout used to
/// drive the integrity contract.
struct Fixture {
    files: BTreeMap<String, Vec<u8>>,
}

impl Fixture {
    fn new() -> Self {
        let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        // Stub artifact bytes — never parsed by ONNX because integrity
        // is checked first.
        files.insert(
            "intent_classifier.onnx".to_string(),
            b"STUB-ONNX-BYTES".to_vec(),
        );
        files.insert(
            "tokenizer.json".to_string(),
            br#"{"version":"1.0","model":{}}"#.to_vec(),
        );
        files.insert(
            "config.json".to_string(),
            br#"{"model_type":"stub"}"#.to_vec(),
        );
        files.insert(
            "temperature.json".to_string(),
            br#"{"temperature":1.0}"#.to_vec(),
        );
        files.insert("version.txt".to_string(), b"model_version=1.0.0\n".to_vec());
        Self { files }
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    /// Write the fixture to disk. Computes `checksums.json` from the
    /// current contents of `self.files`, optionally adds a manifest.
    fn write(&self, dir: &Path, write_manifest: bool, write_checksums: bool) -> Manifest {
        fs::create_dir_all(dir).expect("create model dir");

        // Per-file hashes (excluding checksums.json itself, which is
        // not self-referencing).
        let mut per_file_hashes: BTreeMap<String, String> = BTreeMap::new();
        for (name, bytes) in &self.files {
            fs::write(dir.join(name), bytes).expect("write artifact");
            per_file_hashes.insert(name.clone(), Self::sha256_hex(bytes));
        }

        // Build checksums.json bytes.
        let checksums_json =
            serde_json::to_vec_pretty(&per_file_hashes).expect("serialize checksums");
        if write_checksums {
            fs::write(dir.join("checksums.json"), &checksums_json).expect("write checksums.json");
        }

        let mut manifest_files = per_file_hashes.clone();
        manifest_files.insert(
            "checksums.json".to_string(),
            Self::sha256_hex(&checksums_json),
        );
        let manifest = Manifest {
            model_version: "1.0.0".to_string(),
            release_tag: "models-v1.0.0".to_string(),
            archive: "sqry-models-v1.0.0.tar.gz".to_string(),
            sha256: "00".repeat(32),
            download_url: "https://example.invalid/sqry-models-v1.0.0.tar.gz".to_string(),
            files: manifest_files.clone(),
        };

        if write_manifest {
            // Local manifest.json for custom-mode root-of-trust check.
            let manifest = serde_json::json!({
                "model_version": "1.0.0",
                "release_tag":   "models-v1.0.0",
                "archive":       "sqry-models-v1.0.0.tar.gz",
                "sha256":        "00".repeat(32),
                "download_url":  "https://example.invalid/sqry-models-v1.0.0.tar.gz",
                "files":         manifest_files,
            });
            fs::write(
                dir.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
            )
            .expect("write manifest.json");
        }

        manifest
    }

    /// Mutate the bytes of one named file post-write to simulate
    /// tampering.
    fn tamper_file(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"TAMPERED-BYTES-XYZ").expect("rewrite tampered file");
    }
}

// ---------------------------------------------------------------------------
// Tracing capture (no tracing-subscriber dep — minimal Subscriber impl).
// ---------------------------------------------------------------------------

/// Captured tracing event message bodies plus their target strings.
#[derive(Default, Clone)]
struct CapturedEvents {
    inner: Arc<Mutex<Vec<(String, String)>>>, // (target, formatted_message)
}

impl CapturedEvents {
    fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self) -> Vec<(String, String)> {
        self.inner.lock().expect("captured events lock").clone()
    }
}

struct CaptureSubscriber {
    events: CapturedEvents,
    next_id: std::sync::atomic::AtomicU64,
}

impl tracing::Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _attrs: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        tracing::span::Id::from_u64(id)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Visitor {
            buf: String,
        }
        impl tracing::field::Visit for Visitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                let _ = write!(&mut self.buf, " {}={:?}", field.name(), value);
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                use std::fmt::Write;
                let _ = write!(&mut self.buf, " {}={}", field.name(), value);
            }
        }
        let mut visitor = Visitor { buf: String::new() };
        event.record(&mut visitor);
        let target = event.metadata().target().to_string();
        self.events
            .inner
            .lock()
            .expect("captured events lock")
            .push((target, visitor.buf));
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

fn with_capture<F, R>(f: F) -> (R, Vec<(String, String)>)
where
    F: FnOnce() -> R,
{
    let events = CapturedEvents::new();
    let subscriber = CaptureSubscriber {
        events: events.clone(),
        next_id: std::sync::atomic::AtomicU64::new(0),
    };
    let result = tracing::subscriber::with_default(subscriber, f);
    (result, events.snapshot())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// All synthetic-fixture tests below drive only the integrity contract
// via `IntentClassifier::verify_integrity_for_tests`. This keeps the
// fixtures from depending on a working `ort` dylib at test time —
// integrity is checked first inside `IntentClassifier::load`, and
// proving it directly here is equivalent for the contract under test.

#[test]
fn tampered_file_errors_even_when_allow_unverified() {
    // Setup: fully self-consistent fixture with manifest+checksums.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("model");
    let fx = Fixture::new();
    fx.write(&dir, /*manifest=*/ true, /*checksums=*/ true);

    // Tamper one file post-write so its bytes no longer match the
    // hash recorded in checksums.json.
    Fixture::tamper_file(&dir, "tokenizer.json");

    // Custom mode + escape hatch ON should still error on tampering —
    // this is THE FR-13 security control.
    let err = IntentClassifier::verify_integrity_for_tests(
        &dir,
        /*allow_unverified=*/ true,
        TrustMode::Custom,
    )
    .expect_err("tampered file must error even with allow_unverified=true");

    match err {
        ClassifierError::ChecksumMismatch { file, .. } => {
            assert_eq!(file, "tokenizer.json", "wrong file reported in mismatch");
        }
        other => panic!("expected ChecksumMismatch, got {other:?}"),
    }
}

#[test]
fn missing_file_errors_strict_by_default() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("model");
    let fx = Fixture::new();
    fx.write(&dir, /*manifest=*/ true, /*checksums=*/ true);

    // Now delete a file listed in checksums.json so it is missing.
    fs::remove_file(dir.join("config.json")).expect("delete listed file");

    let err = IntentClassifier::verify_integrity_for_tests(
        &dir,
        /*allow_unverified=*/ false,
        TrustMode::Custom,
    )
    .expect_err("strict mode must error on missing checksummed file");

    match err {
        ClassifierError::ChecksummedFileMissing(name) => {
            assert_eq!(name, "config.json");
        }
        other => panic!("expected ChecksummedFileMissing, got {other:?}"),
    }
}

#[test]
fn missing_file_warns_when_allow_unverified() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("model");
    let fx = Fixture::new();
    fx.write(&dir, /*manifest=*/ true, /*checksums=*/ true);

    fs::remove_file(dir.join("config.json")).expect("delete listed file");

    // With allow_unverified=true, the missing-file branch downgrades
    // to a warn — verify_integrity_for_tests must return Ok(()).
    IntentClassifier::verify_integrity_for_tests(
        &dir,
        /*allow_unverified=*/ true,
        TrustMode::Custom,
    )
    .expect("allow_unverified=true must downgrade missingness to warn");
}

#[test]
fn trusted_mode_uses_supplied_manifest_for_synthetic_tree() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("model");
    let fx = Fixture::new();
    let manifest = fx.write(&dir, /*manifest=*/ true, /*checksums=*/ true);

    // Trusted mode + strict integrity. This goes through the same
    // anchor and per-file path as the baked manifest, but uses a
    // synthetic manifest root so clean release checkouts do not need
    // the ignored ONNX/tokenizer artifact tree.
    IntentClassifier::verify_integrity_with_manifest_for_tests(
        &dir,
        /*allow_unverified=*/ false,
        TrustMode::Trusted,
        &manifest,
    )
    .expect("trusted-mode integrity must pass against a self-consistent synthetic model tree");
}

#[test]
fn trusted_mode_metadata_only_clean_checkout_anchor_passes_with_allow_unverified() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("model");
    copy_tracked_metadata_only_model_dir(&dir);

    IntentClassifier::verify_integrity_for_tests(
        &dir,
        /*allow_unverified=*/ true,
        TrustMode::Trusted,
    )
    .expect("tracked metadata-only clean checkout must satisfy the Trusted checksums anchor");
}

#[test]
fn trusted_mode_metadata_only_clean_checkout_errors_in_strict_mode() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("model");
    copy_tracked_metadata_only_model_dir(&dir);

    let err = IntentClassifier::verify_integrity_for_tests(
        &dir,
        /*allow_unverified=*/ false,
        TrustMode::Trusted,
    )
    .expect_err("strict mode must reject a metadata-only clean-checkout fixture");

    match err {
        ClassifierError::ChecksummedFileMissing(name) => {
            assert!(
                [
                    "intent_classifier.onnx",
                    "tokenizer.json",
                    "config.json",
                    "temperature.json",
                    "version.txt",
                ]
                .contains(&name.as_str()),
                "strict-mode diagnostic should name a listed missing artifact, got {name}"
            );
        }
        other => panic!("expected ChecksummedFileMissing, got {other:?}"),
    }
}

#[test]
#[ignore = "requires external model archive / ONNX Runtime dylib; ignored artifacts are not committed"]
fn trusted_mode_against_external_model_tree_succeeds() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models");

    IntentClassifier::verify_integrity_for_tests(
        &dir,
        /*allow_unverified=*/ false,
        TrustMode::Trusted,
    )
    .expect("trusted-mode integrity must pass against the full external model tree");
}

#[test]
fn custom_mode_warns_root_of_trust() {
    use sqry_nl::{Translator, TranslatorConfig};

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("model");
    let fx = Fixture::new();
    fx.write(&dir, /*manifest=*/ true, /*checksums=*/ true);

    // Translator::new emits the loud root-of-trust warn at custom-mode
    // entry — BEFORE invoking IntentClassifier::load. Even though the
    // classifier load itself will fail downstream (stub ONNX bytes),
    // the warn must already have fired.
    let cfg = TranslatorConfig {
        model_dir_override: Some(dir.clone()),
        // Allow the load attempt to proceed past integrity (we only
        // care about the warn). Tampering is not at play here.
        allow_unverified_model: true,
        ..Default::default()
    };

    // The warn is emitted BEFORE `IntentClassifier::load` is invoked
    // inside `Translator::new`, so even if the downstream ONNX load
    // panics on a missing dylib (environment-dependent), the warn has
    // already fired into the active subscriber. Wrap in `catch_unwind`
    // to absorb the dylib panic — what we are asserting is the warn,
    // not the load outcome.
    let (_result, events) = with_capture(|| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = Translator::new(cfg);
        }))
    });

    let any_root_of_trust_warn = events
        .iter()
        .any(|(target, msg)| target.starts_with("sqry_nl") && msg.contains("custom trust mode"));
    assert!(
        any_root_of_trust_warn,
        "expected a sqry_nl::classifier custom-trust warn at Translator::new; \
         captured events: {events:?}"
    );
}

#[test]
fn custom_mode_missing_manifest_errors_even_when_allow_unverified() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("model");
    let fx = Fixture::new();
    fx.write(&dir, /*manifest=*/ false, /*checksums=*/ true);

    let err = IntentClassifier::verify_integrity_for_tests(
        &dir,
        /*allow_unverified=*/ true,
        TrustMode::Custom,
    )
    .expect_err("custom-mode manifest trust anchor must be mandatory");

    match err {
        ClassifierError::ManifestAnchorInvalid(msg) => {
            assert!(
                msg.contains("manifest.json missing"),
                "missing-manifest diagnostic should name manifest.json, got: {msg}"
            );
        }
        other => panic!("expected ManifestAnchorInvalid, got {other:?}"),
    }
}

#[test]
fn custom_mode_malformed_manifest_errors_even_when_allow_unverified() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("model");
    let fx = Fixture::new();
    fx.write(&dir, /*manifest=*/ true, /*checksums=*/ true);

    fs::write(dir.join("manifest.json"), b"{not valid json").expect("corrupt manifest");

    let err = IntentClassifier::verify_integrity_for_tests(
        &dir,
        /*allow_unverified=*/ true,
        TrustMode::Custom,
    )
    .expect_err("malformed custom-mode manifest must be fatal");

    match err {
        ClassifierError::ManifestAnchorInvalid(msg) => {
            assert!(
                msg.contains("failed to parse manifest.json"),
                "malformed-manifest diagnostic should name parse failure, got: {msg}"
            );
        }
        other => panic!("expected ManifestAnchorInvalid, got {other:?}"),
    }
}

#[test]
fn custom_mode_manifest_without_checksums_anchor_errors_even_when_allow_unverified() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("model");
    let fx = Fixture::new();
    fx.write(&dir, /*manifest=*/ true, /*checksums=*/ true);

    let manifest = serde_json::json!({
        "model_version": "1.0.0",
        "release_tag":   "models-v1.0.0",
        "archive":       "sqry-models-v1.0.0.tar.gz",
        "sha256":        "00".repeat(32),
        "download_url":  "https://example.invalid/sqry-models-v1.0.0.tar.gz",
        "files":         {},
    });
    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write anchorless manifest");

    let err = IntentClassifier::verify_integrity_for_tests(
        &dir,
        /*allow_unverified=*/ true,
        TrustMode::Custom,
    )
    .expect_err("custom-mode manifest must anchor checksums.json");

    match err {
        ClassifierError::ManifestAnchorInvalid(msg) => {
            assert!(
                msg.contains("checksums.json"),
                "anchorless-manifest diagnostic should name checksums.json, got: {msg}"
            );
        }
        other => panic!("expected ManifestAnchorInvalid, got {other:?}"),
    }
}

#[test]
fn xdg_cache_hit_uses_trusted_baked_manifest() {
    // Stage an XDG-shaped synthetic layout. This guards the
    // resolver-to-trust-mode wiring and the strict Trusted integrity
    // path without requiring the ignored real model artifacts.
    let tmp = TempDir::new().unwrap();
    let xdg_root = tmp.path().join("xdg");
    let xdg_models = xdg_root.join("sqry/models");
    let fx = Fixture::new();
    let manifest = fx.write(
        &xdg_models,
        /*manifest=*/ true,
        /*checksums=*/ true,
    );

    let (resolved, level) =
        resolve_model_dir(None, None, None, &MockDirs(xdg_root), None).expect("xdg hit");
    assert_eq!(level, ResolverLevel::XdgCache);

    let trust_mode = TrustMode::from(level);
    assert_eq!(
        trust_mode,
        TrustMode::Trusted,
        "XDG cache hit must classify as Trusted"
    );

    // Drive integrity via the test helper to avoid the ONNX dylib.
    IntentClassifier::verify_integrity_with_manifest_for_tests(
        &resolved, /*allow_unverified=*/ false, trust_mode, &manifest,
    )
    .expect("XDG-cache trusted-mode integrity must pass against the synthetic fixture");
}

#[test]
fn xdg_cache_hit_with_tampered_present_file_errors_even_with_allow_unverified() {
    // Build a fully synthesized XDG layout using our fixture so this
    // does not depend on the ignored real model artifacts.
    let tmp = TempDir::new().unwrap();
    let xdg_root = tmp.path().join("xdg");
    let xdg_models = xdg_root.join("sqry/models");
    let fx = Fixture::new();
    let manifest = fx.write(
        &xdg_models,
        /*manifest=*/ true,
        /*checksums=*/ true,
    );

    // Tamper a present file post-write.
    Fixture::tamper_file(&xdg_models, "config.json");

    let (resolved, level) =
        resolve_model_dir(None, None, None, &MockDirs(xdg_root), None).expect("xdg hit");
    assert_eq!(level, ResolverLevel::XdgCache);
    let trust_mode = TrustMode::from(level);
    assert_eq!(trust_mode, TrustMode::Trusted);

    // allow_unverified=true must NOT silence tampering.
    let err = IntentClassifier::verify_integrity_with_manifest_for_tests(
        &resolved, /*allow_unverified=*/ true, trust_mode, &manifest,
    )
    .expect_err("trusted-mode tampered XDG hit must error even with allow_unverified=true");

    match err {
        ClassifierError::ChecksumMismatch { file, .. } => {
            assert_eq!(file, "config.json");
        }
        other => panic!("expected ChecksumMismatch, got {other:?}"),
    }
}
