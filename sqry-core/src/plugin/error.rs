//! Error types for the plugin system.

use std::path::PathBuf;
use thiserror::Error;

/// Result type for plugin operations.
pub type PluginResult<T> = Result<T, PluginError>;

/// Errors that can occur during plugin operations.
#[derive(Error, Debug)]
pub enum PluginError {
    /// Plugin not found for the given extension or language.
    #[error("no plugin found for extension '{0}'")]
    NotFound(String),

    /// Failed to load external plugin from disk (Phase 3+).
    #[error("failed to load plugin from {path}: {reason}")]
    LoadFailed {
        /// Path to the plugin file
        path: PathBuf,
        /// Reason for failure
        reason: String,
    },

    /// Invalid plugin (missing required symbols, incompatible ABI, etc.).
    #[error("invalid plugin: {0}")]
    InvalidPlugin(String),

    /// AST parsing failed.
    #[error("AST parsing failed: {0}")]
    Parse(#[from] ParseError),

    /// Scope extraction failed.
    #[error("scope extraction failed: {0}")]
    Scope(#[from] ScopeError),

    /// Node resolution failed.
    #[error("symbol resolution failed: {0}")]
    Resolution(#[from] ResolutionError),

    /// Type mismatch during field evaluation.
    #[error("Type mismatch for field '{field}': expected {expected_type}, got {got_type}")]
    TypeMismatch {
        /// Field name
        field: String,
        /// Expected type
        expected_type: String,
        /// Actual type received
        got_type: String,
    },
}

/// Errors during AST parsing.
#[derive(Error, Debug)]
pub enum ParseError {
    /// tree-sitter failed to parse the source code.
    #[error("tree-sitter parsing failed")]
    TreeSitterFailed,

    /// Failed to set language on parser.
    #[error("failed to set language: {0}")]
    LanguageSetFailed(String),

    /// Invalid source code (not UTF-8).
    #[error("invalid source code (not UTF-8)")]
    InvalidSource,

    /// Input exceeds maximum allowed size.
    ///
    /// This error is returned by `SafeParser` when the input content exceeds
    /// the configured maximum size limit. This prevents unbounded memory allocation
    /// from pathological inputs.
    ///
    /// # Remediation
    ///
    /// Split the file into smaller chunks or increase the `--parser-max-bytes` limit
    /// within the allowed bounds (1-32 MiB).
    #[error("input too large: {size} bytes exceeds limit of {max} bytes{}", file.as_ref().map(|f| format!(" (file: {})", f.display())).unwrap_or_default())]
    InputTooLarge {
        /// Actual size of the input in bytes
        size: usize,
        /// Maximum allowed size in bytes
        max: usize,
        /// Optional file path for debugging
        file: Option<PathBuf>,
    },

    /// Parsing timed out.
    ///
    /// This error is returned by `SafeParser` when tree-sitter parsing exceeds
    /// the configured timeout. This prevents runaway parsing on pathological inputs
    /// that could cause exponential backtracking.
    ///
    /// # Remediation
    ///
    /// The file may contain malformed syntax causing parser pathology. Check the file
    /// for validity or increase the `--parser-timeout-ms` limit within bounds.
    #[error("parse timed out after {} ms{}", timeout_micros / 1000, file.as_ref().map(|f| format!(" (file: {})", f.display())).unwrap_or_default())]
    ParseTimedOut {
        /// Timeout in microseconds
        timeout_micros: u64,
        /// Optional file path for debugging
        file: Option<PathBuf>,
    },

    /// Parsing was cancelled by external request.
    ///
    /// This error is returned when parsing is cancelled via a cancellation flag,
    /// typically used by the indexer to abort long-running parses proactively.
    #[error("parse cancelled: {reason}{}", file.as_ref().map(|f| format!(" (file: {})", f.display())).unwrap_or_default())]
    ParseCancelled {
        /// Reason for cancellation
        reason: String,
        /// Optional file path for debugging
        file: Option<PathBuf>,
    },

    /// Other parse error.
    #[error("parse error: {0}")]
    Other(String),
}

/// Errors during scope extraction.
#[derive(Error, Debug)]
pub enum ScopeError {
    /// Failed to compile tree-sitter query for scope extraction.
    #[error("failed to compile scope query: {0}")]
    QueryCompilationFailed(String),

    /// Failed to extract scopes from AST.
    #[error("failed to extract scopes: {0}")]
    ExtractionFailed(String),

    /// Invalid scope structure in AST.
    #[error("invalid scope structure: {0}")]
    InvalidStructure(String),

    /// Other scope error.
    #[error("scope error: {0}")]
    Other(String),
}

/// Errors during symbol resolution (Phase 5+).
#[derive(Error, Debug)]
pub enum ResolutionError {
    /// Node not found.
    #[error("symbol '{0}' not found")]
    NotFound(String),

    /// Ambiguous symbol (multiple definitions).
    #[error("symbol '{0}' is ambiguous (found {1} definitions)")]
    Ambiguous(String, usize),

    /// Resolution requires external data not available.
    #[error("resolution requires {0}")]
    RequiresData(String),

    /// Other resolution error.
    #[error("resolution error: {0}")]
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_error_not_found() {
        let err = PluginError::NotFound("rs".to_string());
        assert_eq!(err.to_string(), "no plugin found for extension 'rs'");
    }

    #[test]
    fn test_plugin_error_load_failed() {
        let err = PluginError::LoadFailed {
            path: PathBuf::from("/path/to/plugin.so"),
            reason: "symbol not found".to_string(),
        };
        assert!(err.to_string().contains("failed to load plugin"));
        assert!(err.to_string().contains("/path/to/plugin.so"));
    }

    #[test]
    fn test_parse_error_display() {
        let err = ParseError::TreeSitterFailed;
        assert_eq!(err.to_string(), "tree-sitter parsing failed");
    }

    #[test]
    fn test_scope_error_display() {
        let err = ScopeError::ExtractionFailed("invalid node".to_string());
        assert!(err.to_string().contains("failed to extract scopes"));
    }

    #[test]
    fn test_resolution_error_ambiguous() {
        let err = ResolutionError::Ambiguous("foo".to_string(), 3);
        assert!(err.to_string().contains("ambiguous"));
        assert!(err.to_string().contains("3 definitions"));
    }

    #[test]
    fn test_parse_error_input_too_large_without_file() {
        let err = ParseError::InputTooLarge {
            size: 15_000_000,
            max: 10_000_000,
            file: None,
        };
        let msg = err.to_string();
        assert!(msg.contains("15000000 bytes"));
        assert!(msg.contains("10000000 bytes"));
        assert!(!msg.contains("file:"));
    }

    #[test]
    fn test_parse_error_input_too_large_with_file() {
        let err = ParseError::InputTooLarge {
            size: 15_000_000,
            max: 10_000_000,
            file: Some(PathBuf::from("/path/to/large.rs")),
        };
        let msg = err.to_string();
        assert!(msg.contains("15000000 bytes"));
        assert!(msg.contains("10000000 bytes"));
        assert!(msg.contains("/path/to/large.rs"));
    }

    #[test]
    fn test_parse_error_timed_out() {
        let err = ParseError::ParseTimedOut {
            timeout_micros: 2_000_000,
            file: Some(PathBuf::from("/path/to/slow.rs")),
        };
        let msg = err.to_string();
        assert!(msg.contains("2000 ms"));
        assert!(msg.contains("/path/to/slow.rs"));
    }

    #[test]
    fn test_parse_error_cancelled() {
        let err = ParseError::ParseCancelled {
            reason: "indexer shutdown".to_string(),
            file: Some(PathBuf::from("/path/to/file.rs")),
        };
        let msg = err.to_string();
        assert!(msg.contains("indexer shutdown"));
        assert!(msg.contains("/path/to/file.rs"));
    }
}
