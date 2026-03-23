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
    Classifier(#[from] ClassifierError),

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

    /// Model checksum mismatch
    #[error("Model checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    /// Tokenization failed
    #[error("Tokenization failed: {0}")]
    TokenizationFailed(String),

    /// ONNX Runtime error
    #[error("ONNX Runtime error: {0}")]
    OnnxError(String),

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
