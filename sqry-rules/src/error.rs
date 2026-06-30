//! Error types for the declarative rule layer.

use thiserror::Error;

/// Result alias for declarative rule layer operations.
pub type RuleResult<T> = Result<T, RuleError>;

/// Errors raised by declarative rule construction, validation, or execution.
#[derive(Debug, Error)]
pub enum RuleError {
    /// A later rule layer component was requested before it was initialized.
    #[error("rule layer component is not initialized: {component}")]
    NotInitialized {
        /// Name of the unavailable component.
        component: &'static str,
    },

    /// The selected backend cannot serve a requested rule primitive.
    #[error("rule backend `{backend}` does not support primitive `{primitive}`: {reason}")]
    UnsupportedPrimitive {
        /// Backend implementation that rejected the primitive.
        backend: &'static str,
        /// Primitive requested by the rule engine.
        primitive: &'static str,
        /// Stable, user-facing reason.
        reason: &'static str,
    },

    /// A rule source document or builder state is invalid.
    #[error("invalid rule source: {reason}")]
    InvalidRuleSource {
        /// Stable, user-facing reason.
        reason: &'static str,
    },

    /// Rule execution was cancelled by the caller.
    #[error("rule execution cancelled")]
    ExecutionCancelled,

    /// Downstream analysis infrastructure reported an error.
    #[error(transparent)]
    Analysis(#[from] anyhow::Error),
}
