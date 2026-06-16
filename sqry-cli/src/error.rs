//! CLI error types with custom exit codes
//!
//! This module defines error types that map to specific exit codes:
//! - 0: Success
//! - 1: Runtime error (default)
//! - 2: Validation error (index corruption)
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
}

impl CliError {
    /// Returns the appropriate exit code for this error
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::RuntimeError(_) => 1,
            CliError::PagerExit(code) => *code,
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
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CliError::RuntimeError(err) => err.source(),
            CliError::PagerExit(_) => None,
        }
    }
}

// Allow converting anyhow::Error to CliError (defaults to RuntimeError)
impl From<anyhow::Error> for CliError {
    fn from(err: anyhow::Error) -> Self {
        CliError::RuntimeError(err)
    }
}
