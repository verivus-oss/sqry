//! ONNX model loading and inference.

use crate::error::ClassifierError;
use crate::types::{ClassificationResult, Intent};
use ort::session::Session;
use ort::value::Tensor;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use super::BAKED_MANIFEST;
use super::calibration::CalibrationParams;
use super::manifest::Manifest;
use super::resolve::TrustMode;

// ---------------------------------------------------------------------------
// NL08 — ONNX Runtime "missing dylib" detection
// ---------------------------------------------------------------------------
//
// The `ort` crate (with the `load-dynamic` feature, which sqry-nl uses)
// resolves `libonnxruntime` at first API call via `libloading`. If the
// shared library is absent, ort's `setup_api()` calls `.expect("Failed
// to load ONNX Runtime dylib")` — meaning the failure surfaces as a
// **panic**, not a typed `Result::Err`. Some downstream surfaces (e.g.
// symbol lookup after a successful library open) do return a typed
// `ort::Error` that carries the substring `"libonnxruntime"` /
// `"failed to load"` / `"OrtGetApiBase"` / `"dlopen"` / `"DyLib"` in its
// `Display` form.
//
// We therefore detect the missing-dylib condition through TWO channels:
//
//   1. `std::panic::catch_unwind` around the `Session::builder()` chain
//      to convert panics into a typed `OnnxRuntimeMissing` error.
//   2. String-pattern matching on the `Display` of any returned
//      `ort::Error` for the substrings above, so symbol-lookup failures
//      after a partial library load also surface as
//      `OnnxRuntimeMissing` instead of the opaque `OnnxError(_)`.
//
// A deterministic test seam — the `SQRY_NL_FORCE_ORT_MISSING` env var
// — short-circuits this path before any ORT call. The seam is gated on
// `debug_assertions` so it cannot be exploited in release binaries
// shipped to operators. Cargo test runs under `debug_assertions` by
// default, so the CLI / MCP / LSP integration tests can drive this path
// without needing an actual missing libonnxruntime on the host.

/// Return the platform-specific install hint for missing ONNX Runtime.
///
/// Used to populate
/// [`crate::error::ClassifierError::OnnxRuntimeMissing`] /
/// [`crate::error::NlError::OnnxRuntimeMissing`].
#[must_use]
pub fn onnx_runtime_install_hint() -> String {
    #[cfg(target_os = "linux")]
    {
        "Install via apt: 'sudo apt-get install libonnxruntime-dev' OR \
         download from https://github.com/microsoft/onnxruntime/releases"
            .to_string()
    }
    #[cfg(target_os = "macos")]
    {
        "Install via brew: 'brew install onnxruntime'".to_string()
    }
    #[cfg(target_os = "windows")]
    {
        "Download libonnxruntime.dll from \
         https://github.com/microsoft/onnxruntime/releases and place in PATH"
            .to_string()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        // Other Unix-likes (FreeBSD, etc.) — mirror Linux guidance.
        "Install libonnxruntime via your platform package manager OR \
         download from https://github.com/microsoft/onnxruntime/releases"
            .to_string()
    }
}

/// Return `true` when the env-var test seam is active.
///
/// Gated on `debug_assertions` so release binaries do not honour the
/// override. `cargo test` runs under `debug_assertions` regardless of
/// the harness binary's profile, so subprocess tests of the release
/// `sqry` binary need to spawn the debug-built binary (which `cargo
/// test` always does — `cargo build --release` is a separate command).
#[cfg(debug_assertions)]
fn ort_missing_forced() -> bool {
    match std::env::var("SQRY_NL_FORCE_ORT_MISSING") {
        Ok(v) => {
            let v = v.trim();
            v.eq_ignore_ascii_case("1")
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        }
        Err(_) => false,
    }
}

#[cfg(not(debug_assertions))]
fn ort_missing_forced() -> bool {
    false
}

/// Returns `true` if the given error string looks like a dylib-load
/// failure for `libonnxruntime`. Matches the substrings ort emits in
/// the load-dynamic path. Case-insensitive on the substring tokens.
///
/// NL08 review iter-1: the broad tokens `"dylib"`, `"dlopen"`, and
/// `"failed to load"` were intentionally excluded from this OR set.
/// They false-positive on operator-supplied paths (e.g.
/// `SQRY_NL_MODEL_DIR=/some/dylib-models/...`) and on unrelated model
/// load errors carrying such paths in their message — a bad-ONNX-bytes
/// failure for a model under such a path would otherwise be
/// misclassified as `OnnxRuntimeMissing`. The three remaining tokens
/// (`libonnxruntime`, `onnxruntime.dll`, `ortgetapibase`) uniquely
/// identify the ort dylib-load surface and will never appear in a
/// legitimate file path or model parse error.
fn looks_like_dylib_load_failure(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("libonnxruntime")
        || lower.contains("onnxruntime.dll")
        || lower.contains("ortgetapibase")
}

/// Construct `ClassifierError::OnnxRuntimeMissing` with the platform hint.
fn onnx_runtime_missing_error() -> ClassifierError {
    ClassifierError::OnnxRuntimeMissing {
        hint: onnx_runtime_install_hint(),
    }
}

/// Intent classifier using an ONNX model (`all-MiniLM-L6-v2` or `DistilBERT`).
pub struct IntentClassifier {
    /// ONNX Runtime session
    session: Session,
    /// `HuggingFace` tokenizer
    tokenizer: tokenizers::Tokenizer,
    /// Calibration parameters for confidence scaling
    calibration: CalibrationParams,
    /// Model version string
    model_version: String,
    /// Whether the ONNX model declares `token_type_ids` as an input.
    /// BERT-architecture models (`MiniLM`) require it; `DistilBERT` does not.
    /// Passing an undeclared input to ort causes a runtime error.
    has_token_type_ids: bool,
}

/// Compute SHA256 hash of a file.
fn compute_file_hash(path: &Path) -> Result<String, ClassifierError> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        ClassifierError::OnnxError(format!("Failed to open {}: {e}", path.display()))
    })?;

    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = file.read(&mut buffer).map_err(|e| {
            ClassifierError::OnnxError(format!("Failed to read {}: {e}", path.display()))
        })?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ---------------------------------------------------------------------------
// NL04 Integrity Contract — AUTHORITATIVE
// ---------------------------------------------------------------------------
//
// `verify_integrity` is the single point at which on-disk model artifacts
// are validated against an expected-hash table. Two distinct failure modes
// must NEVER be conflated:
//
//   1. TAMPERING — a file is present on disk, its sha256 was checked, and
//      the computed hash does NOT match the expected hash. This ALWAYS
//      yields `Err(ChecksumMismatch { file, expected, actual })`,
//      regardless of `allow_unverified`. The escape hatch covers
//      missingness only; it never silences hash mismatch on a present
//      file. This matches spec FR-7 + FR-13.
//
//   2. MISSINGNESS — `checksums.json` itself is absent, or a file listed
//      in `checksums.json` is absent on disk. In strict mode
//      (`allow_unverified == false`, the default per FR-7), missingness
//      is a fatal error (`ChecksumsMissing` / `ChecksummedFileMissing`).
//      With `allow_unverified == true`, missingness downgrades to a
//      `tracing::warn!` and the loader continues — but ALL still-present
//      files are still hashed.
//
// Trust mode (FR-14):
//   - `TrustMode::Trusted` (resolver levels 4-5): the on-disk
//     `checksums.json` is hashed and cross-checked against
//     `BAKED_MANIFEST.files["checksums.json"]`. A mismatch ALWAYS errors,
//     even when `allow_unverified == true`. This anchors integrity in
//     the binary itself rather than the operator-supplied directory.
//   - `TrustMode::Custom` (resolver levels 1-3): the local
//     `manifest.json` (parsed from disk in the same directory) is the
//     trust root. `Translator::new` is responsible for emitting the
//     loud `tracing::warn!` that integrity is rooted in user-supplied
//     data; this function focuses on the actual verification.
// ---------------------------------------------------------------------------

/// Load checksums from `checksums.json` if present.
///
/// Returns `Ok(None)` when the file is absent (caller decides whether
/// that is fatal based on `allow_unverified`). Returns `Ok(Some(map))`
/// when present and parseable. Returns `Err` only on parse / I/O
/// failure — those are always fatal.
fn try_load_checksums(
    checksums_path: &Path,
) -> Result<Option<HashMap<String, String>>, ClassifierError> {
    if !checksums_path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(checksums_path)
        .map_err(|e| ClassifierError::OnnxError(format!("Failed to read checksums.json: {e}")))?;
    let map = serde_json::from_str(&content)
        .map_err(|e| ClassifierError::OnnxError(format!("Failed to parse checksums.json: {e}")))?;
    Ok(Some(map))
}

/// Verify model directory integrity per the NL04 contract documented above.
///
/// See the module-level "NL04 Integrity Contract — AUTHORITATIVE" comment
/// block for the full tampering-vs-missingness rules. A short summary:
///
/// - Tampering on a present file ALWAYS errors.
/// - Missingness errors only when `allow_unverified == false`.
/// - In `TrustMode::Trusted`, `checksums.json`'s own bytes are
///   cross-checked against `BAKED_MANIFEST.files["checksums.json"]` —
///   a mismatch is ALWAYS fatal.
fn verify_integrity(
    model_dir: &Path,
    allow_unverified: bool,
    trust_mode: TrustMode,
) -> Result<(), ClassifierError> {
    verify_integrity_with_trusted_manifest(model_dir, allow_unverified, trust_mode, &BAKED_MANIFEST)
}

fn verify_integrity_with_trusted_manifest(
    model_dir: &Path,
    allow_unverified: bool,
    trust_mode: TrustMode,
    trusted_manifest: &Manifest,
) -> Result<(), ClassifierError> {
    let checksums_path = model_dir.join("checksums.json");

    match trust_mode {
        TrustMode::Trusted => {
            verify_trusted_checksums_anchor(&checksums_path, allow_unverified, trusted_manifest)?;
        }
        TrustMode::Custom => verify_custom_checksums_anchor(model_dir, &checksums_path)?,
    }

    // Per-file pass over `checksums.json`. Same tampering-vs-missingness
    // rules apply file-by-file.
    let Some(checksums) = try_load_checksums(&checksums_path)? else {
        if allow_unverified {
            tracing::warn!(
                "No checksums.json found in {} — allow_unverified=true; \
                 skipping integrity verification (development workflow)",
                model_dir.display()
            );
            return Ok(());
        }
        return Err(ClassifierError::ChecksumsMissing);
    };

    let mut verified_count = 0usize;
    for (filename, expected_hash) in &checksums {
        let file_path = model_dir.join(filename);
        if !file_path.exists() {
            // MISSINGNESS — strict by default, warn-and-skip with hatch.
            if allow_unverified {
                tracing::warn!(
                    "Checksummed file missing: {filename} — allow_unverified=true; \
                     continuing (other listed files will still be hashed)"
                );
                continue;
            }
            return Err(ClassifierError::ChecksummedFileMissing(filename.clone()));
        }

        let actual_hash = compute_file_hash(&file_path)?;
        if &actual_hash != expected_hash {
            // TAMPERING — ALWAYS fatal, regardless of allow_unverified.
            return Err(ClassifierError::ChecksumMismatch {
                file: filename.clone(),
                expected: expected_hash.clone(),
                actual: actual_hash,
            });
        }
        verified_count += 1;
        tracing::debug!("Verified checksum for {filename}");
    }
    tracing::info!(
        "Model integrity verified: {} of {} listed files checked",
        verified_count,
        checksums.len()
    );
    Ok(())
}

fn verify_trusted_checksums_anchor(
    checksums_path: &Path,
    allow_unverified: bool,
    trusted_manifest: &Manifest,
) -> Result<(), ClassifierError> {
    let Some(expected_checksums_hash) = trusted_manifest.files.get("checksums.json") else {
        return Ok(());
    };

    if checksums_path.exists() {
        verify_checksums_json_hash(
            checksums_path,
            expected_checksums_hash,
            "Trusted-mode anchor OK: checksums.json matches BAKED_MANIFEST",
        )
    } else if allow_unverified {
        tracing::warn!(
            "checksums.json missing under Trusted resolver level — \
             allow_unverified=true downgrades to warn; baked-in trust \
             anchor cannot be cross-checked"
        );
        Ok(())
    } else {
        Err(ClassifierError::ChecksumsMissing)
    }
}

fn verify_custom_checksums_anchor(
    model_dir: &Path,
    checksums_path: &Path,
) -> Result<(), ClassifierError> {
    let local_manifest_path = model_dir.join("manifest.json");
    if !local_manifest_path.exists() {
        return Err(ClassifierError::ManifestAnchorInvalid(format!(
            "manifest.json missing at {}",
            local_manifest_path.display()
        )));
    }

    let local_manifest = Manifest::parse_path(&local_manifest_path).map_err(|err| {
        ClassifierError::ManifestAnchorInvalid(format!(
            "failed to parse manifest.json at {}: {err}",
            local_manifest_path.display()
        ))
    })?;
    let expected_checksums_hash = local_manifest.files.get("checksums.json").ok_or_else(|| {
        ClassifierError::ManifestAnchorInvalid(format!(
            "manifest.files[\"checksums.json\"] missing in {}",
            local_manifest_path.display()
        ))
    })?;

    if checksums_path.exists() {
        verify_checksums_json_hash(
            checksums_path,
            expected_checksums_hash,
            "Custom-mode anchor OK: checksums.json matches local manifest.json",
        )
    } else {
        tracing::warn!(
            target: "sqry_nl::classifier",
            "Custom-mode integrity anchor skipped: checksums.json missing at {} \
             (operator-supplied dir without a complete manifest)",
            checksums_path.display()
        );
        Ok(())
    }
}

fn verify_checksums_json_hash(
    checksums_path: &Path,
    expected_checksums_hash: &str,
    success_message: &str,
) -> Result<(), ClassifierError> {
    let actual = compute_file_hash(checksums_path)?;
    if actual != expected_checksums_hash {
        // TAMPERING — always fatal, no opt-out.
        return Err(ClassifierError::ChecksumMismatch {
            file: "checksums.json".to_string(),
            expected: expected_checksums_hash.to_string(),
            actual,
        });
    }
    tracing::debug!("{success_message}");
    Ok(())
}

/// Parse model version from version.txt content.
fn parse_model_version(content: &str) -> String {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("model_version=") {
            return line
                .strip_prefix("model_version=")
                .unwrap_or("unknown")
                .to_string();
        }
    }
    "unknown".to_string()
}

impl IntentClassifier {
    /// Load classifier from model directory.
    ///
    /// Expected directory structure:
    /// ```text
    /// model_dir/
    /// ├── intent_classifier.onnx
    /// ├── tokenizer.json
    /// ├── config.json
    /// ├── calibration.json or temperature.json (optional)
    /// ├── checksums.json
    /// └── version.txt
    /// ```
    ///
    /// # Arguments
    ///
    /// * `model_dir` — Resolved model directory (output of NL02
    ///   resolver chain).
    /// * `allow_unverified` — Operator escape hatch. When `false`
    ///   (NL04 default per FR-7), missingness is fatal. When `true`,
    ///   missingness downgrades to `tracing::warn!`. **Tampering on a
    ///   present file ALWAYS errors regardless of this flag** — see
    ///   the inline contract documented at [`verify_integrity`].
    /// * `trust_mode` — Output of [`TrustMode::from(ResolverLevel)`].
    ///   Trusted mode anchors integrity in the binary's baked-in
    ///   manifest; Custom mode trusts the user-supplied
    ///   `manifest.json` shipped alongside the model directory.
    ///
    /// # Errors
    ///
    /// Returns [`ClassifierError`] if:
    /// - Model files not found
    /// - Checksum verification fails (AC-11.8 / NL04 integrity contract)
    /// - ONNX Runtime initialization fails
    pub fn load(
        model_dir: &Path,
        allow_unverified: bool,
        trust_mode: TrustMode,
    ) -> Result<Self, ClassifierError> {
        Self::load_inner(model_dir, allow_unverified, trust_mode)
    }

    /// Run only the NL04 integrity contract for a model directory,
    /// without invoking ONNX Runtime.
    ///
    /// Same contract as [`Self::load`]'s integrity pass — exists so
    /// integration tests can exercise the contract on synthetic
    /// fixtures (stub ONNX bytes) without the dylib dependency.
    ///
    /// # Errors
    ///
    /// Returns [`ClassifierError::ChecksumMismatch`] /
    /// [`ClassifierError::ChecksumsMissing`] /
    /// [`ClassifierError::ChecksummedFileMissing`] per the contract.
    #[doc(hidden)]
    pub fn verify_integrity_for_tests(
        model_dir: &Path,
        allow_unverified: bool,
        trust_mode: TrustMode,
    ) -> Result<(), ClassifierError> {
        verify_integrity(model_dir, allow_unverified, trust_mode)
    }

    /// Run the NL04 integrity contract with a test-supplied trusted
    /// manifest instead of the binary's baked model manifest.
    ///
    /// This keeps active integration tests hermetic: they can exercise
    /// the Trusted-mode anchor and strict per-file pass against
    /// synthetic model fixtures without committing the large external
    /// ONNX model tree.
    ///
    /// # Errors
    ///
    /// Returns the same [`ClassifierError`] variants as
    /// [`Self::verify_integrity_for_tests`].
    #[doc(hidden)]
    pub fn verify_integrity_with_manifest_for_tests(
        model_dir: &Path,
        allow_unverified: bool,
        trust_mode: TrustMode,
        trusted_manifest: &Manifest,
    ) -> Result<(), ClassifierError> {
        verify_integrity_with_trusted_manifest(
            model_dir,
            allow_unverified,
            trust_mode,
            trusted_manifest,
        )
    }

    fn load_inner(
        model_dir: &Path,
        allow_unverified: bool,
        trust_mode: TrustMode,
    ) -> Result<Self, ClassifierError> {
        // NL08: deterministic test seam — when
        // `SQRY_NL_FORCE_ORT_MISSING` is truthy AND we are running a
        // debug build (cargo test / cargo run), short-circuit straight
        // to `OnnxRuntimeMissing`. This lets the CLI / MCP / LSP
        // integration tests drive the missing-runtime path without
        // needing an actual missing libonnxruntime on the host. The
        // helper is a no-op in release builds.
        if ort_missing_forced() {
            return Err(onnx_runtime_missing_error());
        }

        // Check model directory exists
        if !model_dir.exists() {
            return Err(ClassifierError::ModelNotFound(
                model_dir.display().to_string(),
            ));
        }

        // Verify integrity BEFORE any artifact load — this is the
        // first-fail gate per the NL04 integrity contract. Tampering
        // detection happens here, prior to ONNX session creation, so
        // synthetic test fixtures (stub ONNX bytes) can exercise the
        // contract without invoking the inference engine.
        verify_integrity(model_dir, allow_unverified, trust_mode)?;

        let model_path = model_dir.join("intent_classifier.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !model_path.exists() {
            return Err(ClassifierError::ModelNotFound(
                model_path.display().to_string(),
            ));
        }

        if !tokenizer_path.exists() {
            return Err(ClassifierError::ModelNotFound(
                tokenizer_path.display().to_string(),
            ));
        }

        // Load ONNX session.
        //
        // NL08: the `ort` crate panics in `setup_api()` (with the
        // `load-dynamic` feature) if `libonnxruntime` cannot be loaded,
        // so we wrap the whole builder chain in `catch_unwind` and
        // reinterpret either a panic or any error string that looks
        // like a dylib-load failure as
        // `ClassifierError::OnnxRuntimeMissing` so callers can surface
        // an actionable platform-specific install hint.
        let model_path_for_load = model_path.clone();
        let session_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Session::builder()?
                .with_intra_threads(1)?
                .commit_from_file(&model_path_for_load)
        }));
        let session = match session_result {
            Ok(Ok(session)) => session,
            Ok(Err(e)) => {
                let msg = e.to_string();
                if looks_like_dylib_load_failure(&msg) {
                    return Err(onnx_runtime_missing_error());
                }
                return Err(ClassifierError::OnnxError(msg));
            }
            Err(panic_payload) => {
                let panic_msg = panic_payload
                    .downcast_ref::<&'static str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "ort panic with unknown payload".to_string());
                if looks_like_dylib_load_failure(&panic_msg) {
                    return Err(onnx_runtime_missing_error());
                }
                // Any other panic from ort is escalated as a generic
                // ONNX error rather than re-thrown — translator
                // construction must always return a typed error.
                return Err(ClassifierError::OnnxError(format!(
                    "ort panic during session init: {panic_msg}"
                )));
            }
        };

        // Detect whether model expects token_type_ids (BERT vs DistilBERT)
        let model_inputs = session.inputs();
        let has_token_type_ids = model_inputs
            .iter()
            .any(|input| input.name() == "token_type_ids");
        tracing::debug!(
            "Model inputs: {:?}, has_token_type_ids: {has_token_type_ids}",
            model_inputs
                .iter()
                .map(ort::value::Outlet::name)
                .collect::<Vec<_>>()
        );

        // Load tokenizer
        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| ClassifierError::TokenizationFailed(e.to_string()))?;

        // Load calibration (optional) — try calibration.json first, then temperature.json
        let calibration_path = model_dir.join("calibration.json");
        let temperature_path = model_dir.join("temperature.json");
        let calibration = if calibration_path.exists() {
            let content = std::fs::read_to_string(&calibration_path)
                .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;
            serde_json::from_str(&content).unwrap_or_default()
        } else if temperature_path.exists() {
            let content = std::fs::read_to_string(&temperature_path)
                .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;
            let params: CalibrationParams = serde_json::from_str(&content).unwrap_or_default();
            tracing::debug!(
                "Loaded calibration temperature={} from temperature.json",
                params.temperature
            );
            params
        } else {
            CalibrationParams::default()
        };

        // Load and parse version
        let version_path = model_dir.join("version.txt");
        let model_version = if version_path.exists() {
            std::fs::read_to_string(&version_path)
                .map_or_else(|_| "unknown".to_string(), |s| parse_model_version(&s))
        } else {
            "unknown".to_string()
        };

        Ok(Self {
            session,
            tokenizer,
            calibration,
            model_version,
            has_token_type_ids,
        })
    }

    /// Classify intent from natural language input.
    ///
    /// # Critical: `batch_size=1` enforcement (C1 mitigation)
    ///
    /// ONNX Runtime may crash with `batch_size` > 1. This method
    /// always processes exactly one input.
    ///
    /// # Errors
    ///
    /// Returns [`ClassifierError`] if tokenization or inference fails.
    ///
    /// # Note
    ///
    /// This method requires `&mut self` due to ort 2.0 API requirements.
    /// Use a Mutex wrapper if concurrent access is needed.
    pub fn classify(&mut self, input: &str) -> Result<ClassificationResult, ClassifierError> {
        // Tokenize input
        let encoding = self
            .tokenizer
            .encode(input, true)
            .map_err(|e| ClassifierError::TokenizationFailed(e.to_string()))?;

        let input_ids = encoding.get_ids();
        let attention_mask = encoding.get_attention_mask();

        // Truncate to max 512 tokens
        let seq_len = input_ids.len().min(512);
        if input_ids.len() > 512 {
            tracing::warn!("Input truncated from {} to 512 tokens", input_ids.len());
        }

        // Prepare input tensors (batch_size=1)
        let input_ids_i64: Vec<i64> = input_ids[..seq_len].iter().map(|&x| i64::from(x)).collect();
        let attention_mask_i64: Vec<i64> = attention_mask[..seq_len]
            .iter()
            .map(|&x| i64::from(x))
            .collect();

        // Create input tensors with shape [1, seq_len] - ort 2.0 requires Vec not slice
        let input_ids_tensor = Tensor::from_array(([1, seq_len], input_ids_i64))
            .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;
        let attention_mask_tensor = Tensor::from_array(([1, seq_len], attention_mask_i64))
            .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;

        // Build inputs conditionally: BERT-family models (MiniLM) require token_type_ids,
        // while DistilBERT does not declare it. ort rejects undeclared input names.
        let inputs = if self.has_token_type_ids {
            let type_ids = encoding.get_type_ids();
            let token_type_ids_i64: Vec<i64> =
                type_ids[..seq_len].iter().map(|&x| i64::from(x)).collect();
            let token_type_ids_tensor = Tensor::from_array(([1, seq_len], token_type_ids_i64))
                .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;
            ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            ]
        } else {
            ort::inputs![
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
            ]
        };

        let outputs = self
            .session
            .run(inputs)
            .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;

        // Extract logits from output
        let logits_tensor = outputs
            .get("logits")
            .ok_or_else(|| ClassifierError::OnnxError("No 'logits' output".to_string()))?;

        // try_extract_tensor returns (&Shape, &[T]) tuple in ort 2.0
        let (_, logits_data) = logits_tensor
            .try_extract_tensor::<f32>()
            .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;

        let logits: Vec<f32> = logits_data.to_vec();

        // Apply calibration and softmax
        let probabilities = self.calibration.apply_temperature_scaling(&logits);

        // Find argmax
        let (intent_idx, confidence) = probabilities
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map_or((Intent::NUM_CLASSES - 1, 0.0), |(idx, &conf)| (idx, conf)); // Default to Ambiguous

        let intent = Intent::from_index(intent_idx);

        Ok(ClassificationResult {
            intent,
            confidence,
            all_probabilities: probabilities,
            model_version: self.model_version.clone(),
        })
    }

    /// Get the model version.
    #[must_use]
    pub fn model_version(&self) -> &str {
        &self.model_version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_model_version() {
        let content = r"
# sqry-nl Intent Classifier Model
model_version=1.0.0
model_date=2025-12-09T07:34:00Z
accuracy=0.9998
";
        assert_eq!(parse_model_version(content), "1.0.0");
    }

    #[test]
    fn test_parse_model_version_missing() {
        let content = "# No version here\naccuracy=0.99";
        assert_eq!(parse_model_version(content), "unknown");
    }

    #[test]
    fn test_parse_model_version_empty() {
        assert_eq!(parse_model_version(""), "unknown");
    }

    // Tests requiring actual model files are marked as ignored
    // and run during integration testing.

    #[test]
    #[ignore = "Requires trained model files"]
    fn test_classifier_load() {
        // Would test model loading
    }

    #[test]
    #[ignore = "Requires trained model files"]
    fn test_classifier_inference() {
        // Would test inference
    }

    #[test]
    #[ignore = "Requires trained model files"]
    fn test_checksum_verification() {
        // Would test checksum verification against deployed model
    }
}
