//! Error types for the sqry-nl crate.
//!
//! Uses `thiserror` for ergonomic error handling with automatic
//! `std::error::Error` implementation.

use thiserror::Error;

/// Result type alias for sqry-nl operations.
pub type NlResult<T> = Result<T, NlError>;

/// Top-level error type for sqry-nl operations.
#[derive(Error, Debug)]
pub enum NlError {
    /// Preprocessing failed (Unicode normalization, input validation)
    #[error("Preprocessing failed: {0}")]
    Preprocess(#[from] PreprocessError),

    /// Entity extraction failed
    #[error("Entity extraction failed: {0}")]
    Extractor(#[from] ExtractorError),

    /// Intent classification failed
    #[error("Classification failed: {0}")]
    Classifier(ClassifierError),

    /// Command assembly failed
    #[error("Assembly failed: {0}")]
    Assembler(#[from] AssemblerError),

    /// Validation failed (safety checks)
    #[error("Validation failed: {0}")]
    Validator(#[from] ValidatorError),

    /// Cache operation failed
    #[error("Cache error: {0}")]
    Cache(#[from] CacheError),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Resolved model directory does not exist on disk.
    #[error("Model directory not found: {0}")]
    ModelDirNotFound(String),

    /// Checksum of an on-disk model file does not match the manifest.
    #[error("Model file checksum mismatch for {file}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },

    /// `checksums.json` (or equivalent integrity manifest) is absent from the model directory.
    #[error("Model checksums file is missing from model directory")]
    ChecksumsMissing,

    /// A file referenced by the integrity manifest is missing from the model directory.
    #[error("File listed in checksums is missing from model directory: {0}")]
    ChecksummedFileMissing(String),

    /// ONNX Runtime shared library could not be loaded at runtime.
    #[error("ONNX Runtime is not available: {hint}")]
    OnnxRuntimeMissing {
        /// Operator-facing remediation hint (populated in NL08).
        hint: String,
    },

    /// Top-level manifest SHA-256 does not match the expected pinned value.
    ///
    /// Raised by NL03's downloader when the streaming SHA-256 computed over
    /// the freshly downloaded archive bytes does not match the
    /// `manifest.json.sha256` value baked into the binary. Always fatal —
    /// there is no `--allow-unverified-model` opt-out for tampering on a
    /// trusted-mode payload.
    #[error("Model manifest SHA-256 mismatch for {file}: expected {expected}, got {actual}")]
    ManifestSha256Mismatch {
        /// The archive file name from the manifest (e.g.
        /// `sqry-models-v1.0.0.tar.gz`).
        file: String,
        /// SHA-256 hex from the trusted baked-in manifest.
        expected: String,
        /// SHA-256 hex computed over the on-the-wire bytes.
        actual: String,
    },

    /// Model manifest could not be parsed as JSON.
    #[error("Model manifest parse failed: {0}")]
    ManifestParseFailed(#[from] serde_json::Error),

    /// Network download was attempted but `allow_model_download` is `false`.
    #[error("Model download is disabled by configuration")]
    DownloadDisabled,

    /// Network download failed (transport, HTTP status, or post-fetch I/O).
    #[error("Model download failed: {0}")]
    DownloadFailed(String),
}

// Manual `From<ClassifierError>` impl: replaces the previous `#[from]`
// derive on `NlError::Classifier`. The special case here promotes
// `ClassifierError::OnnxRuntimeMissing { hint }` to the top-level
// `NlError::OnnxRuntimeMissing { hint }` variant so every consumer
// (CLI, MCP, LSP, daemon) can pattern-match on a single, stable
// wire-facing variant instead of having to dig into nested
// classifier-specific error structures.
//
// All other `ClassifierError` variants pass through unchanged into
// `NlError::Classifier(_)`. This preserves the `?` ergonomics that
// the previous `#[from]` derive provided.
impl From<ClassifierError> for NlError {
    fn from(err: ClassifierError) -> Self {
        match err {
            ClassifierError::OnnxRuntimeMissing { hint } => NlError::OnnxRuntimeMissing { hint },
            other => NlError::Classifier(other),
        }
    }
}

/// Errors from the preprocessing stage.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum PreprocessError {
    /// Input exceeds maximum length
    #[error("Input too long: {len} bytes (max: {max})")]
    InputTooLong { len: usize, max: usize },

    /// Input contains only whitespace or is empty
    #[error("Input is empty or contains only whitespace")]
    EmptyInput,

    /// Homoglyph attack detected
    #[error("Suspicious character detected: possible homoglyph attack")]
    HomoglyphDetected,

    /// Invalid UTF-8 encoding
    #[error("Invalid UTF-8 encoding")]
    InvalidUtf8,
}

/// Errors from the entity extraction stage.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ExtractorError {
    /// No symbols found in input
    #[error("No symbol or pattern found in query")]
    NoSymbolFound,

    /// Ambiguous symbol reference
    #[error("Ambiguous symbol reference: multiple interpretations possible")]
    AmbiguousSymbol,

    /// Invalid language specified
    #[error("Unknown language: {0}")]
    UnknownLanguage(String),

    /// Invalid symbol kind specified
    #[error("Unknown symbol kind: {0}")]
    UnknownKind(String),

    /// Regex compilation error
    #[error("Pattern compilation failed: {0}")]
    RegexError(String),
}

/// Errors from the intent classification stage.
#[derive(Error, Debug)]
pub enum ClassifierError {
    /// Model file not found
    #[error("Model not found at: {0}")]
    ModelNotFound(String),

    /// Model checksum mismatch on a present file (tampering).
    ///
    /// NL04: ALWAYS fatal regardless of `allow_unverified`. Mirrors the
    /// shape of [`NlError::ChecksumMismatch`] so the boundary between
    /// the classifier-internal error and the top-level NL error stays
    /// clean.
    #[error("Model checksum mismatch for {file}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },

    /// `checksums.json` is absent from the model directory (strict mode).
    #[error("Model checksums file is missing from model directory")]
    ChecksumsMissing,

    /// A file referenced by the integrity manifest is missing from disk
    /// (strict mode).
    #[error("File listed in checksums is missing from model directory: {0}")]
    ChecksummedFileMissing(String),

    /// Tokenization failed
    #[error("Tokenization failed: {0}")]
    TokenizationFailed(String),

    /// ONNX Runtime error
    #[error("ONNX Runtime error: {0}")]
    OnnxError(String),

    /// Custom-mode local manifest cannot anchor `checksums.json`.
    ///
    /// Raised when a custom model directory's `manifest.json` is
    /// missing, malformed, or lacks `files["checksums.json"]`. This is
    /// distinct from missing `checksums.json` itself: the operator
    /// escape hatch can downgrade missing checksums, but it must not
    /// silently remove the custom-mode manifest trust anchor.
    #[error("Model manifest integrity anchor invalid: {0}")]
    ManifestAnchorInvalid(String),

    /// ONNX Runtime shared library could not be loaded.
    ///
    /// Raised by [`crate::classifier::IntentClassifier::load`] when the
    /// `ort` crate's `Session::builder()` chain fails (or panics) due to
    /// `libonnxruntime` being absent on the host. The `hint` field
    /// carries a platform-aware remediation string (apt / brew / .dll
    /// download URL) baked at compile time. NL08 surfaces this variant
    /// across CLI / MCP / LSP / daemon as an actionable diagnostic
    /// rather than the opaque `OnnxError(...)` string the lower-level
    /// crate would otherwise produce.
    ///
    /// Always converted to [`NlError::OnnxRuntimeMissing`] at the
    /// `From<ClassifierError>` boundary — the top-level error variant
    /// is the wire-facing name.
    #[error("ONNX Runtime is not available: {hint}")]
    OnnxRuntimeMissing {
        /// Operator-facing remediation hint (platform-specific install
        /// instructions). Populated by
        /// [`crate::classifier::model::onnx_runtime_install_hint`].
        hint: String,
    },

    /// Model version incompatible
    #[error("Model version {model_version} incompatible with sqry-nl {crate_version}")]
    VersionMismatch {
        model_version: String,
        crate_version: String,
    },

    /// Inference timeout
    #[error("Classification timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },
}

/// Errors from the command assembly stage.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum AssemblerError {
    /// Required symbol not provided
    #[error("Missing required symbol for this command type")]
    MissingSymbol,

    /// Missing from/to symbols for trace-path
    #[error("Trace-path requires both 'from' and 'to' symbols")]
    MissingTracePath,

    /// Intent is ambiguous and cannot be assembled
    #[error("Cannot assemble command: intent is ambiguous")]
    AmbiguousIntent,

    /// Generated command exceeds length limit
    #[error("Generated command too long: {len} chars (max: {max})")]
    CommandTooLong { len: usize, max: usize },

    /// Template not found for intent
    #[error("No template found for intent: {0}")]
    NoTemplate(String),
}

/// Errors from the validation stage.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ValidatorError {
    /// Command doesn't match any allowed template
    #[error("Command rejected: doesn't match any allowed template")]
    TemplateMismatch,

    /// Dangerous shell metacharacters detected
    #[error("Command rejected: contains shell metacharacters")]
    MetacharDetected,

    /// Environment variable expansion detected
    #[error("Command rejected: contains environment variable")]
    EnvVarDetected,

    /// Path traversal attempt detected
    #[error("Command rejected: path traversal detected")]
    PathTraversal,

    /// Absolute path detected
    #[error("Command rejected: absolute paths not allowed")]
    AbsolutePath,

    /// Write-mode operation detected
    #[error("Command rejected: write operations not allowed via NL")]
    WriteOperation,

    /// Command too long
    #[error("Command rejected: exceeds maximum length")]
    CommandTooLong,
}

/// Errors from the cache operations.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum CacheError {
    /// Cache is disabled
    #[error("Cache is disabled")]
    Disabled,

    /// Cache entry expired
    #[error("Cache entry has expired")]
    Expired,

    /// Cache key generation failed
    #[error("Failed to generate cache key: {0}")]
    KeyGenerationFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = PreprocessError::InputTooLong {
            len: 5000,
            max: 4096,
        };
        assert!(err.to_string().contains("5000"));
        assert!(err.to_string().contains("4096"));
    }

    #[test]
    fn test_error_conversion() {
        let preprocess_err = PreprocessError::EmptyInput;
        let nl_err: NlError = preprocess_err.into();
        assert!(matches!(nl_err, NlError::Preprocess(_)));
    }

    #[test]
    fn test_errors_implement_std_error() {
        fn assert_error<T: std::error::Error>() {}

        assert_error::<NlError>();
        assert_error::<PreprocessError>();
        assert_error::<ExtractorError>();
        assert_error::<ClassifierError>();
        assert_error::<AssemblerError>();
        assert_error::<ValidatorError>();
        assert_error::<CacheError>();
    }
}
