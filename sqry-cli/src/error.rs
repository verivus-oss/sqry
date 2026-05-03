//! CLI error types with custom exit codes
//!
//! This module defines error types that map to specific exit codes:
//! - 0: Success
//! - 1: Runtime error (default)
//! - 2: Validation error (index corruption)
//! - 65: ONNX Runtime missing (`EX_DATAERR` per BSD `sysexits.h`)
//! - N: Pager exit code (when pager exits with non-zero)

use std::fmt;

/// CLI-specific error type with custom exit codes
#[derive(Debug)]
pub enum CliError {
    /// Runtime error (exit code 1)
    RuntimeError(anyhow::Error),

    /// Pager exited with non-zero status (exit code from pager)
    ///
    /// This is used to propagate pager exit codes to the CLI exit code.
    /// For example, if the user kills the pager with Ctrl-C (SIGINT),
    /// this would propagate exit code 130 (128 + 2).
    PagerExit(i32),

    /// ONNX Runtime shared library not available (exit code 65,
    /// `EX_DATAERR` per BSD `sysexits.h`).
    ///
    /// Surfaced by `sqry ask` when the NL classifier's model load fails
    /// because `libonnxruntime` cannot be located on the host. The
    /// `hint` payload is platform-specific install guidance produced by
    /// [`sqry_nl::onnx_runtime_install_hint`] and rendered as a
    /// two-line stderr message:
    ///
    /// ```text
    /// error: ONNX Runtime not found
    /// hint: <platform hint>
    /// ```
    OnnxRuntimeMissing { hint: String },
}

impl CliError {
    /// Returns the appropriate exit code for this error
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::RuntimeError(_) => 1,
            CliError::PagerExit(code) => *code,
            // `EX_DATAERR` per BSD `sysexits.h`: the user-supplied
            // execution environment is wrong (missing dylib).
            CliError::OnnxRuntimeMissing { .. } => 65,
        }
    }

    /// Create a runtime error
    #[allow(dead_code)]
    #[must_use]
    pub fn runtime(err: impl Into<anyhow::Error>) -> Self {
        CliError::RuntimeError(err.into())
    }

    /// Create a pager exit error
    ///
    /// Use this when the pager exits with a non-zero status and you want
    /// to propagate that exit code to the CLI.
    #[must_use]
    pub fn pager_exit(code: i32) -> Self {
        CliError::PagerExit(code)
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::RuntimeError(err) => write!(f, "{err}"),
            CliError::PagerExit(code) => write!(f, "pager exited with code {code}"),
            CliError::OnnxRuntimeMissing { hint } => {
                // Multi-line stderr surface — see `handle_cli_error` in
                // `main.rs` for the full rendering. Display impl keeps
                // the single-line "Error: <…>" path useful for logging
                // / `anyhow` chains.
                write!(f, "ONNX Runtime not found: {hint}")
            }
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CliError::RuntimeError(err) => err.source(),
            CliError::PagerExit(_) | CliError::OnnxRuntimeMissing { .. } => None,
        }
    }
}

// Allow converting anyhow::Error to CliError (defaults to RuntimeError)
impl From<anyhow::Error> for CliError {
    fn from(err: anyhow::Error) -> Self {
        CliError::RuntimeError(err)
    }
}
