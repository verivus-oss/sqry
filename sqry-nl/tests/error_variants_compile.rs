//! NL01 acceptance test.
//!
//! Construct each new `NlError` variant once and assert that `Debug` and
//! `Display` formatting both produce non-empty output. This is the
//! compile-time + smoke-level guard that NL01's additive surface is wired up
//! correctly. NL02 through NL08 will exercise the variants in their natural
//! call sites; here we only need to prove every variant is constructable from
//! outside the crate.

use std::io::{Error as IoError, ErrorKind};

use sqry_nl::NlError;

/// Force a `serde_json::Error` for the `ManifestParseFailed` variant by parsing
/// deliberately malformed JSON. We do not want to construct one through any
/// private API — round-tripping through `serde_json::from_str` is the
/// idiomatic way third-party crates obtain a `serde_json::Error` value.
fn forced_serde_error() -> serde_json::Error {
    serde_json::from_str::<serde_json::Value>("{not valid json").unwrap_err()
}

#[test]
fn all_variants_constructable() {
    let variants: Vec<NlError> = vec![
        NlError::ModelDirNotFound("/nonexistent/path".to_string()),
        NlError::ChecksumMismatch {
            file: "intent_classifier.onnx".to_string(),
            expected: "abc".to_string(),
            actual: "def".to_string(),
        },
        NlError::ChecksumsMissing,
        NlError::ChecksummedFileMissing("tokenizer.json".to_string()),
        NlError::OnnxRuntimeMissing {
            hint: "install libonnxruntime".to_string(),
        },
        NlError::ManifestSha256Mismatch {
            file: "sqry-models-v1.0.0.tar.gz".to_string(),
            expected: "abc".to_string(),
            actual: "def".to_string(),
        },
        // Use the #[from] conversion to make sure `?` will keep working in
        // downstream units (NL02..NL08).
        NlError::from(forced_serde_error()),
        NlError::DownloadDisabled,
        NlError::DownloadFailed("connection refused".to_string()),
    ];

    for err in &variants {
        let display = err.to_string();
        assert!(
            !display.is_empty(),
            "Display impl produced empty string for {err:?}"
        );

        let debug = format!("{err:?}");
        assert!(
            !debug.is_empty(),
            "Debug impl produced empty string for variant rendered as {display}"
        );
    }

    // Independently confirm the existing `#[from] std::io::Error` chain still
    // resolves to the `Io` variant — guards against an accidental shadowing by
    // any of the new variants.
    let io_err: NlError = IoError::new(ErrorKind::NotFound, "missing").into();
    assert!(matches!(io_err, NlError::Io(_)));
}
