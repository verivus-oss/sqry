//! Integration tests for the NL02 model-directory resolver.
//!
//! These tests exercise [`sqry_nl::classifier::resolve_model_dir`]
//! end-to-end against a temporary on-disk model layout, mirroring the
//! happy-path lookup the Translator performs inside `Translator::new`.

use std::fs;

use sqry_nl::classifier::{DirsLike, ResolverLevel, resolve_model_dir};
use std::path::PathBuf;
use tempfile::TempDir;

/// Mock `DirsLike` implementation that returns a fixed cache root.
struct MockDirs {
    root: Option<PathBuf>,
}

impl DirsLike for MockDirs {
    fn cache_dir(&self) -> Option<PathBuf> {
        self.root.clone()
    }
}

/// NL07 — end-to-end: `Translator::new` → resolver → pool → translate.
///
/// `#[ignore]` because it requires the ONNX Runtime dylib + committed
/// model fixtures. Run manually:
///
/// ```bash
/// cargo test -p sqry-nl --features classifier --test \
///     translator_loads_classifier -- --ignored end_to_end_translate_with_classifier --nocapture
/// ```
#[cfg(feature = "classifier")]
#[test]
#[ignore = "requires ONNX Runtime dylib + committed model fixtures"]
fn end_to_end_translate_with_classifier() {
    use sqry_nl::{TranslationResponse, Translator, TranslatorConfig};

    let model_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models");
    assert!(
        model_dir.join("intent_classifier.onnx").exists(),
        "expected committed model at {}",
        model_dir.display()
    );

    let config = TranslatorConfig {
        model_dir_override: Some(model_dir),
        allow_unverified_model: false,
        classifier_pool_size: Some(2),
        ..TranslatorConfig::default()
    };
    let translator =
        Translator::new(config).expect("translator init must succeed against in-tree fixtures");

    // Single canonical translate call.
    let resp = translator.translate_shared("find authentication functions");
    match resp {
        TranslationResponse::Execute { .. }
        | TranslationResponse::Confirm { .. }
        | TranslationResponse::Disambiguate { .. } => {}
        TranslationResponse::Reject { reason, .. } => {
            assert!(
                !reason.contains("Could not determine"),
                "model-backed translate must not reject 'find authentication functions' for missing symbol; got reason: {reason}"
            );
        }
    }
    assert!(translator.translation_count() >= 1);
}

#[test]
fn resolver_finds_xdg_cache_dir() {
    let tmp = TempDir::new().expect("tempdir");

    // Stage the canonical XDG layout: <cache_root>/sqry/models/manifest.json
    let cache_root = tmp.path().join("xdg-cache");
    let model_dir = cache_root.join("sqry/models");
    fs::create_dir_all(&model_dir).expect("create xdg model dir");
    fs::write(model_dir.join("manifest.json"), b"{}").expect("write manifest.json");

    let dirs = MockDirs {
        root: Some(cache_root.clone()),
    };

    // No CLI override, no legacy, no env, no exe — XDG must win on its own.
    let (resolved, level) = resolve_model_dir(None, None, None, &dirs, None)
        .expect("resolver must hit the staged XDG model dir");

    assert_eq!(resolved, model_dir);
    assert_eq!(level, ResolverLevel::XdgCache);
}
