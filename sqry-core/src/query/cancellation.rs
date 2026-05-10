//! Query-side cooperative cancellation primitive.
//!
//! Re-exports the [`CancellationToken`] already defined for the
//! incremental rebuild pipeline at
//! [`crate::graph::unified::build::CancellationToken`] and adds a
//! query-side pass-boundary helper [`CancellationToken::query_check`]
//! that returns the query path's [`crate::query::error::QueryError::Cancelled`]
//! variant (distinct from the build pipeline's
//! [`crate::graph::error::GraphBuilderError::Cancelled`]).
//!
//! Both surfaces share the underlying `Arc<AtomicBool>`-backed flag —
//! they differ only in which error type is constructed when the flag
//! is observed cancelled.
//!
//! See `docs/development/sqry-mcp-flakiness-fix/A_cancellation.md` §1
//! and `00_contracts.md` §3.CC-1 for the canonical type-conflict
//! resolution: A's `Arc<AtomicBool>` wrapper wins over the alternative
//! `tokio_util::sync::CancellationToken` because `sqry-core` is
//! deliberately runtime-free (CLAUDE.md "Avoid `tokio` unless
//! IO-bound") and the existing token already carries the
//! `Send + Sync + Clone` and zero-allocation poll semantics the query
//! hot loop requires.

pub use crate::graph::unified::build::CancellationToken;

impl CancellationToken {
    /// Query-side pass-boundary check.
    ///
    /// Returns [`crate::query::error::QueryError::Cancelled`] when the
    /// token is cancelled, distinct from the build pipeline's
    /// [`crate::graph::error::GraphBuilderError::Cancelled`]. Both
    /// share the underlying flag — calling [`Self::cancel`] on one
    /// surface is observed by the other.
    ///
    /// Unlike [`Self::check`] (which is the build-pipeline variant
    /// returning `GraphResult<()>`), this helper does not depend on
    /// `sqry-core::graph::error` and is callable from query-evaluator
    /// code that lives outside the build pipeline.
    ///
    /// # Errors
    ///
    /// Returns [`crate::query::error::QueryError::Cancelled`] when the
    /// token is cancelled.
    #[inline]
    pub fn query_check(&self) -> Result<(), crate::query::error::QueryError> {
        if self.is_cancelled() {
            Err(crate::query::error::QueryError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_check_returns_ok_when_not_cancelled() {
        let token = CancellationToken::new();
        assert!(token.query_check().is_ok());
    }

    #[test]
    fn query_check_returns_query_error_cancelled_when_cancelled() {
        let token = CancellationToken::new();
        token.cancel();
        let err = token.query_check().expect_err("must error after cancel");
        assert!(
            matches!(err, crate::query::error::QueryError::Cancelled),
            "expected QueryError::Cancelled, got: {err:?}"
        );
    }

    #[test]
    fn query_check_observes_cancel_through_clone() {
        // The two helpers (`check` for build, `query_check` for query)
        // must share the underlying flag so either side can propagate
        // a deadline-driven cancel into the other domain.
        let token = CancellationToken::new();
        let clone = token.clone();
        clone.cancel();
        assert!(token.query_check().is_err());
    }
}
