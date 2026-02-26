//! # sqry-mcp-redaction
//!
//! Client-side helper library for redacting sensitive data from MCP (Model Context Protocol)
//! responses before sending them to external LLMs or cloud services.
//!
//! ## Overview
//!
//! The sqry MCP server returns detailed code analysis results that may contain sensitive
//! information including:
//! - Absolute file paths (exposing server structure)
//! - Workspace root paths (revealing internal infrastructure)
//! - Source code context (potentially proprietary code)
//! - Documentation strings (extracted comments)
//!
//! This library provides configurable redaction to protect this data while preserving
//! semantic information useful for code understanding.
//!
//! ## Security Model
//!
//! The library operates in **whitelist-first mode** by default:
//! - All fields are considered sensitive unless explicitly whitelisted
//! - Presets define which fields to preserve
//! - Unknown fields are redacted by default
//!
//! ## Quick Start
//!
//! ```rust
//! use sqry_mcp_redaction::{Redactor, RedactionConfig};
//!
//! // Standard redaction (recommended for most cloud LLM integrations)
//! let redactor = Redactor::with_defaults();
//! let mcp_response = r#"{"fileUri": "file:///home/user/file.rs"}"#;
//! let mut response: serde_json::Value = serde_json::from_str(mcp_response)?;
//! let stats = redactor.redact(&mut response);
//! println!("Redacted {} paths", stats.paths_redacted);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! ## Presets
//!
//! | Preset | Paths | Code | Docs | Use Case |
//! |--------|-------|------|------|----------|
//! | `none` | ❌ | ❌ | ❌ | Trusted local tools only |
//! | `minimal` | ✅ | ❌ | ❌ | Cloud LLMs needing code context |
//! | `standard` | ✅ | ✅ | ❌ | Cloud LLMs, code confidential |
//! | `strict` | ✅ | ✅ | ✅ | Untrusted external services |

#![deny(
    missing_docs,
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery
)]
#![allow(
    dead_code,
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::struct_excessive_bools,
    clippy::missing_const_for_fn,
    clippy::uninlined_format_args,
    clippy::doc_markdown,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::non_std_lazy_statics,
    clippy::cognitive_complexity,
    clippy::option_if_let_else,
    clippy::format_push_string,
    clippy::manual_strip,
    clippy::collapsible_if,
    clippy::unnecessary_wraps,
    clippy::or_fun_call,
    clippy::missing_panics_doc,
    clippy::redundant_clone,
    clippy::if_not_else,
    clippy::manual_ignore_case_cmp,
    clippy::unused_peekable,
    clippy::trivially_copy_pass_by_ref,
    clippy::doc_lazy_continuation
)]

mod config;
mod jsonpath;
mod preview;
mod redactor;
mod streaming;
mod walker;
mod whitelist;

pub mod rules;

pub use config::{RedactionConfig, SecurityMode};
pub use preview::{RedactionPreview, RedactionReason, RedactionTarget};
pub use redactor::{RedactionResult, Redactor};

use std::io;

/// Errors that can occur during redaction operations.
#[derive(Debug, thiserror::Error)]
pub enum RedactionError {
    /// JSON parsing error.
    #[error("JSON parse error: {0}")]
    ParseError(#[from] serde_json::Error),

    /// Invalid configuration.
    #[error("Invalid configuration: {0}")]
    ConfigError(String),

    /// Invalid `JSONPath` expression.
    #[error("Invalid JSONPath expression: {0}")]
    InvalidJsonPath(String),

    /// I/O error during streaming.
    #[error("Streaming error: {0}")]
    StreamError(#[from] io::Error),

    /// Path processing error.
    #[error("Path error: {0}")]
    PathError(#[from] PathError),
}

/// Errors that can occur during path canonicalization.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PathError {
    /// Input path is empty.
    #[error("Empty path")]
    EmptyPath,

    /// Path contains null byte (security rejection).
    #[error("Null byte in path")]
    NullByteInPath,

    /// Path contains control characters (security rejection).
    #[error("Control character in path")]
    ControlCharacterInPath,

    /// Input is not a file:// URI.
    #[error("Not a file URI")]
    NotFileUri,

    /// Malformed file:// URI syntax.
    #[error("Malformed file URI")]
    MalformedFileUri,

    /// Percent-decoding produced invalid UTF-8.
    #[error("Invalid UTF-8 in path")]
    InvalidUtf8,

    /// Path exceeds maximum length.
    #[error("Path too long: {len} > {max}")]
    PathTooLong {
        /// Actual length.
        len: usize,
        /// Maximum allowed length.
        max: usize,
    },

    /// Attempted to navigate past UNC share root with `..`.
    #[error("Attempted to escape UNC share root")]
    UncEscapeAttempt,

    /// Windows device path (e.g., `\\.\COM1`) - not a file path.
    #[error("Windows device path not allowed")]
    DevicePath,
}

/// Strategy for handling path canonicalization errors.
#[derive(Debug, Clone, Copy, Default)]
pub enum PathErrorStrategy {
    /// Return error to caller (strict mode).
    #[default]
    Fail,

    /// Hash the raw input as-is (permissive mode).
    HashRawInput,

    /// Replace with placeholder hash (hide errors).
    UsePlaceholder,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = RedactionError::ConfigError("test error".to_string());
        assert!(err.to_string().contains("test error"));

        let path_err = PathError::EmptyPath;
        assert_eq!(path_err.to_string(), "Empty path");
    }
}
