//! Typed deserialization of `sqry-nl/models/manifest.json`.
//!
//! NL03 introduces a structured view over the manifest committed under
//! `sqry-nl/models/manifest.json` so the downloader can verify the
//! top-level archive SHA-256 against a baked-in expected manifest before
//! ever extracting bytes to disk.
//!
//! ## Shape
//!
//! ```json
//! {
//!   "model_version": "1.0.0",
//!   "release_tag":   "models-v1.0.0",
//!   "archive":       "sqry-models-v1.0.0.tar.gz",
//!   "sha256":        "<top-level archive sha256, hex>",
//!   "download_url":  "https://github.com/.../sqry-models-v1.0.0.tar.gz",
//!   "files": {
//!     "intent_classifier.onnx": "<sha256 hex>",
//!     "tokenizer.json":         "<sha256 hex>",
//!     ...
//!   }
//! }
//! ```
//!
//! The top-level integrity field is `sha256` — **not** `archive_sha256`.
//! NL05 rolls `checksums.json` integrity into `files["checksums.json"]`,
//! and that in turn rolls into the top-level `sha256` of the archive.
//!
//! Unknown top-level fields are tolerated (forward-compatible). Unknown
//! entries inside `files` are likewise tolerated by `BTreeMap` semantics.
//! `files` is intentionally a [`BTreeMap`] so iteration order is
//! deterministic across runs and platforms — required for any future
//! re-emission of the manifest as bytes.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::NlResult;

/// Parsed view of `models/manifest.json`.
///
/// All integrity-relevant fields are required. Unknown fields are
/// preserved on the wire but not surfaced through this struct (the
/// type intentionally omits `#[serde(deny_unknown_fields)]` — the
/// manifest schema is expected to grow over time, and an additive
/// field on disk should not break older binaries).
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Semver-style model version, e.g. `"1.0.0"`.
    pub model_version: String,
    /// Release tag in the public model repository, e.g.
    /// `"models-v1.0.0"`.
    pub release_tag: String,
    /// Archive file name (the tar.gz on disk after download), e.g.
    /// `"sqry-models-v1.0.0.tar.gz"`.
    pub archive: String,
    /// Top-level SHA-256 of the archive contents (hex). Verified by
    /// NL03's [`crate::classifier::download::ensure_model_in_cache`]
    /// against bytes streamed during download. Field name on disk is
    /// **`sha256`** — not `archive_sha256`.
    pub sha256: String,
    /// Fully qualified URL the downloader fetches when triggered.
    pub download_url: String,
    /// Per-file SHA-256 hex map — one entry per artifact inside the
    /// extracted archive. Used by `IntentClassifier::load`'s integrity
    /// pass (NL04). Deterministic ordering via [`BTreeMap`].
    pub files: BTreeMap<String, String>,
}

impl Manifest {
    /// Parse a manifest from a JSON byte string.
    ///
    /// # Errors
    ///
    /// Returns [`NlError::ManifestParseFailed`] (wrapping the underlying
    /// `serde_json::Error`) when `json` is not a syntactically valid
    /// manifest.
    pub fn parse(json: &str) -> NlResult<Self> {
        Ok(serde_json::from_str(json)?)
    }

    /// Read and parse a manifest from a file on disk.
    ///
    /// # Errors
    ///
    /// Returns [`NlError::Io`] if the file cannot be read, or
    /// [`NlError::ManifestParseFailed`] if the file is not valid JSON.
    pub fn parse_path(path: &Path) -> NlResult<Self> {
        let contents = fs::read_to_string(path)?;
        Self::parse(&contents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::NlError;

    const SAMPLE: &str = r#"{
        "model_version": "1.0.0",
        "release_tag":   "models-v1.0.0",
        "archive":       "sqry-models-v1.0.0.tar.gz",
        "sha256":        "4f7886e2a381ee4a17bfca81bd41ec88e10169d498b68d2166587e6265d27b3e",
        "download_url":  "https://example.invalid/sqry-models-v1.0.0.tar.gz",
        "files": {
            "intent_classifier.onnx": "f8aef10721d7f55f882576fd0f9f9fefaa1a9ededbf1df157e61b736f811833e",
            "tokenizer.json":         "79ea220416a57ca8acdf069d71875d5d2ddadef66021aeb6c120a7c546c04344"
        }
    }"#;

    #[test]
    fn parses_sample_manifest() {
        let m = Manifest::parse(SAMPLE).expect("parse");
        assert_eq!(m.model_version, "1.0.0");
        assert_eq!(m.release_tag, "models-v1.0.0");
        assert_eq!(m.archive, "sqry-models-v1.0.0.tar.gz");
        assert_eq!(m.sha256.len(), 64);
        assert_eq!(m.files.len(), 2);
        assert!(m.files.contains_key("intent_classifier.onnx"));
    }

    #[test]
    fn tolerates_unknown_top_level_fields() {
        let json = r#"{
            "model_version": "1.0.0",
            "release_tag":   "models-v1.0.0",
            "archive":       "x.tar.gz",
            "sha256":        "00",
            "download_url":  "https://example.invalid/x.tar.gz",
            "files":         {},
            "future_field":  "ignored"
        }"#;
        let m = Manifest::parse(json).expect("parse");
        assert_eq!(m.archive, "x.tar.gz");
    }

    #[test]
    fn malformed_json_is_manifest_parse_failed() {
        let err = Manifest::parse("{not json}").unwrap_err();
        assert!(
            matches!(err, NlError::ManifestParseFailed(_)),
            "expected ManifestParseFailed, got {err:?}"
        );
    }

    #[test]
    fn parses_real_committed_manifest() {
        // The manifest baked into the binary must round-trip through
        // the typed view — guards against drift between the
        // checked-in models/manifest.json and this struct.
        let baked = include_str!("../../models/manifest.json");
        let m = Manifest::parse(baked).expect("baked manifest parses");
        assert!(!m.sha256.is_empty());
        assert!(!m.files.is_empty());
        assert!(
            m.files.contains_key("intent_classifier.onnx"),
            "baked manifest is missing intent_classifier.onnx entry"
        );
    }
}
