//! NL08 — ONNX Runtime missing surface (CLI).
//!
//! Drives the `SQRY_NL_FORCE_ORT_MISSING` deterministic test seam in
//! `sqry-nl/src/classifier/model.rs` to short-circuit the ONNX dylib
//! load and assert that:
//!
//! 1. stderr contains the verbatim line `error: ONNX Runtime not found`.
//! 2. stderr contains the platform-specific install hint substring
//!    (apt / brew / dll-from-releases per design §8).
//! 3. The process exits with code `65` (`EX_DATAERR` per BSD
//!    `sysexits.h`).
//!
//! The seam is gated on `debug_assertions` in `sqry-nl`, so this test
//! relies on `cargo test` building the `sqry` binary in debug mode
//! (the default — `cargo test --release` is a separate command).
//!
//! Also gated on the `nl-classifier` feature: the seam fires inside
//! `IntentClassifier::load`, which only exists when sqry-nl's
//! `classifier` feature is enabled. Run with:
//!   cargo test -p sqry-cli --features nl-classifier --test ask_ort_missing

#![cfg(feature = "nl-classifier")]

mod common;

use std::process::Command;

use common::sqry_bin;

/// The expected platform install hint substring. Matches
/// `sqry_nl::onnx_runtime_install_hint()`.
fn expected_hint_substring() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "apt-get install libonnxruntime-dev"
    }
    #[cfg(target_os = "macos")]
    {
        "brew install onnxruntime"
    }
    #[cfg(target_os = "windows")]
    {
        "libonnxruntime.dll"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "libonnxruntime"
    }
}

#[test]
fn ort_missing_emits_hint_and_exits_65() {
    let bin = sqry_bin();

    // Run `sqry ask "find functions"` with the env-var seam set.
    // We deliberately use a path inside a temp dir to avoid touching
    // the host's .sqry workspace state — translator init runs even
    // before any indexed-graph access, so any non-error CWD works.
    //
    // We also pin `SQRY_NL_MODEL_DIR` to the in-tree fixtures shipped
    // under `sqry-nl/models/` so the NL02 resolver chain has a hit
    // (otherwise `Translator::new` short-circuits to no-classifier
    // mode and never reaches `IntentClassifier::load` where the seam
    // fires).
    let model_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("sqry-nl/models");
    let tempdir = tempfile::TempDir::new().expect("tempdir");
    let output = Command::new(&bin)
        .arg("ask")
        .arg("--dry-run")
        .arg("find functions")
        .env("SQRY_NL_FORCE_ORT_MISSING", "1")
        .env("SQRY_NL_MODEL_DIR", &model_dir)
        .current_dir(tempdir.path())
        .output()
        .expect("failed to spawn sqry binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("STDOUT:\n{stdout}\nSTDERR:\n{stderr}");

    assert!(
        stderr.contains("error: ONNX Runtime not found"),
        "stderr must contain 'error: ONNX Runtime not found' line. Full output:\n{combined}"
    );

    let hint = expected_hint_substring();
    assert!(
        stderr.contains(hint),
        "stderr must contain platform install hint substring '{hint}'. Full output:\n{combined}"
    );

    let code = output
        .status
        .code()
        .expect("process must exit with a status code (no signal termination)");
    assert_eq!(
        code, 65,
        "exit code must be 65 (EX_DATAERR per sysexits.h). Got {code}. Full output:\n{combined}"
    );
}
