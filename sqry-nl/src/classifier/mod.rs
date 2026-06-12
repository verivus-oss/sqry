//! Intent classification using MiniLM-L6-v2 ONNX model.
//!
//! The `classifier` feature gates the ONNX-backed implementation; the
//! [`resolve`] submodule (and its public API) is available
//! unconditionally so call sites — CLI, MCP, LSP, daemon — share a
//! single canonical model-directory lookup chain regardless of whether
//! the runtime classifier is compiled in.

#[cfg(feature = "classifier")]
mod calibration;
#[cfg(feature = "classifier")]
pub mod download;
#[cfg(feature = "classifier")]
pub mod manifest;
#[cfg(feature = "classifier")]
mod model;
#[cfg(feature = "classifier")]
pub mod pool;
pub mod resolve;
#[cfg(feature = "classifier")]
mod tokenizer_wrapper;

#[cfg(feature = "classifier")]
pub use calibration::CalibrationParams;
#[cfg(feature = "classifier")]
pub use download::{Downloader, FileDownloader, UreqDownloader, ensure_model_in_cache};
#[cfg(feature = "classifier")]
pub use manifest::Manifest;
#[cfg(feature = "classifier")]
pub use model::{IntentClassifier, onnx_runtime_install_hint};
#[cfg(feature = "classifier")]
pub use pool::{ClassifierPool, POOL_DEFAULT, POOL_MAX, POOL_MIN, PoolGuard, resolve_pool_size};
pub use resolve::{DirsLike, RealDirs, ResolverLevel, TrustMode, resolve_model_dir};
#[cfg(feature = "classifier")]
pub use tokenizer_wrapper::SharedClassifier;

/// Trusted, baked-in expected manifest snapshot.
///
/// Embedded into the binary at compile time via `include_str!` of
/// `sqry-nl/models/manifest.json`. Every trusted-mode download
/// (NL03's `ensure_model_in_cache`) verifies the streamed archive
/// SHA-256 against this snapshot before extraction, anchoring trust at
/// the binary itself rather than the on-disk cache.
///
/// Parsed lazily on first access and cached for the process lifetime.
///
/// # Panics
///
/// `Manifest::parse` is fallible, but the input is a build-time
/// constant. If it ever fails to parse the build is broken in a way
/// that should be caught by `cargo test` (see
/// `manifest::tests::parses_real_committed_manifest`). We unwrap here
/// because a malformed embedded manifest is a programmer error, not a
/// runtime condition.
#[cfg(feature = "classifier")]
pub static BAKED_MANIFEST: std::sync::LazyLock<Manifest> = std::sync::LazyLock::new(|| {
    Manifest::parse(include_str!("../../models/manifest.json"))
        .expect("baked-in models/manifest.json must be valid JSON (build-time invariant)")
});

#[cfg(not(feature = "classifier"))]
use crate::error::ClassifierError;
#[cfg(not(feature = "classifier"))]
use crate::types::{ClassificationResult, Intent};

/// Stub classifier for when the `classifier` feature is disabled.
///
/// Always returns `Intent::Ambiguous` with low confidence.
#[cfg(not(feature = "classifier"))]
pub struct IntentClassifier;

#[cfg(not(feature = "classifier"))]
impl IntentClassifier {
    /// Create a stub classifier.
    ///
    /// # Errors
    ///
    /// The stub implementation is infallible today; it returns `Result` to
    /// preserve the feature-enabled classifier constructor contract.
    pub fn new() -> Result<Self, ClassifierError> {
        Ok(Self)
    }

    /// Classify intent (stub - always returns Ambiguous).
    ///
    /// # Errors
    ///
    /// The stub implementation is infallible today; it returns `Result` to
    /// preserve the feature-enabled classifier API.
    pub fn classify(&self, _input: &str) -> Result<ClassificationResult, ClassifierError> {
        Ok(ClassificationResult {
            intent: Intent::Ambiguous,
            confidence: 0.0,
            all_probabilities: vec![0.0; Intent::NUM_CLASSES],
            model_version: "stub".to_string(),
        })
    }
}

#[cfg(not(feature = "classifier"))]
impl Default for IntentClassifier {
    fn default() -> Self {
        Self
    }
}
