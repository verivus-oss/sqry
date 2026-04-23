//! Cooperative cancellation for incremental-rebuild pipelines.
//!
//! The sqryd daemon's rebuild dispatcher supersedes an in-flight rebuild
//! whenever a newer one arrives for the same workspace. Rather than killing
//! the rayon thread pool mid-pass, the engine cooperates: it polls a shared
//! flag at every pass boundary and short-circuits with
//! [`GraphBuilderError::Cancelled`] when the flag is set.
//!
//! # Design
//!
//! * **Wait-free**: `AtomicBool` with `Acquire`/`Release` ordering. Callers
//!   that cancel never block.
//! * **Cheap to clone**: the token is an `Arc<AtomicBool>` wrapper; cloning
//!   bumps the `Arc` refcount and shares the underlying flag. Producers (the
//!   dispatcher) and consumers (the rebuild task) typically hold separate
//!   clones of the same token.
//! * **No allocation on the happy path**: `is_cancelled()` / `check()` read
//!   one atomic; no heap traffic.
//!
//! # Usage
//!
//! ```
//! use sqry_core::graph::error::GraphResult;
//! use sqry_core::graph::unified::build::CancellationToken;
//!
//! fn example_pipeline(cancel: &CancellationToken) -> GraphResult<()> {
//!     // Pass boundary 1
//!     cancel.check()?;
//!     // ...do some work...
//!
//!     // Pass boundary 2
//!     cancel.check()?;
//!     // ...do more work...
//!
//!     Ok(())
//! }
//! ```
//!
//! # Scope (Step 4 Phase 3a)
//!
//! This module only provides the abstraction + a single pre-flight check in
//! [`incremental_rebuild`][crate::graph::unified::build::incremental::incremental_rebuild].
//! Real per-pass polling is wired in when Phase 3b+ replaces the stub body
//! with the 13-sub-step implementation.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::graph::error::{GraphBuilderError, GraphResult};

/// Cooperative cancellation token for incremental-rebuild pipelines.
///
/// The token is a clonable handle to a shared `Arc<AtomicBool>`. All clones
/// observe the same cancellation state — calling [`cancel`](Self::cancel) on
/// one clone makes every other clone see [`is_cancelled`](Self::is_cancelled)
/// return `true`.
///
/// Tokens are cheap to clone (`Arc::clone`) and always `Send + Sync`, so they
/// can be shared freely across rayon workers, tokio tasks, and the daemon's
/// rebuild dispatcher.
///
/// # Ordering
///
/// [`cancel`](Self::cancel) stores the flag with [`Ordering::Release`] and
/// [`is_cancelled`](Self::is_cancelled) loads with [`Ordering::Acquire`].
/// That pair is sufficient for the single-writer cancellation signal and
/// ensures a reader observing `true` also observes any writes that happened
/// before the cancellation call.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Construct a fresh, un-cancelled token.
    ///
    /// Equivalent to [`CancellationToken::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the token as cancelled.
    ///
    /// Idempotent: calling `cancel` twice has the same effect as calling it
    /// once. All clones of this token will subsequently observe
    /// [`is_cancelled`](Self::is_cancelled) returning `true`.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// Return `true` if [`cancel`](Self::cancel) has been called on this
    /// token (or on any clone of it).
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Pass-boundary check.
    ///
    /// Returns [`GraphBuilderError::Cancelled`] if the token has been
    /// cancelled; otherwise `Ok(())`. Rebuild-pipeline code should call this
    /// at every pass boundary — the happy path cost is a single atomic load.
    ///
    /// # Errors
    ///
    /// Returns [`GraphBuilderError::Cancelled`] when the token is cancelled.
    pub fn check(&self) -> GraphResult<()> {
        if self.is_cancelled() {
            Err(GraphBuilderError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_token_flag_starts_unset() {
        // A fresh token must never report itself as cancelled — otherwise the
        // first pre-flight check in `incremental_rebuild` would short-circuit
        // on every rebuild.
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancellation_token_cancel_sets_flag() {
        // Calling `cancel` once flips the flag permanently for this token
        // (and all of its clones). Idempotent — a second `cancel` does not
        // throw or reset anything.
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancellation_token_check_returns_ok_when_not_cancelled() {
        // Happy-path contract: `check()` on a live token is `Ok(())`, the
        // pipeline proceeds.
        let token = CancellationToken::new();
        assert!(token.check().is_ok());
    }

    #[test]
    fn cancellation_token_check_returns_cancelled_when_cancelled() {
        // After cancellation, `check()` must return
        // `GraphBuilderError::Cancelled` — exact variant match, not just
        // any error — so callers can special-case cooperative cancellation.
        let token = CancellationToken::new();
        token.cancel();
        let err = token
            .check()
            .expect_err("cancelled token must return Err from check()");
        assert!(
            matches!(err, GraphBuilderError::Cancelled),
            "expected GraphBuilderError::Cancelled, got: {err:?}"
        );
    }

    #[test]
    fn cancellation_token_clone_shares_state() {
        // Cloning a token must share the underlying `AtomicBool`. Cancelling
        // a clone cancels the original (and vice versa) — this is the whole
        // point of the abstraction.
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!token.is_cancelled());
        assert!(!clone.is_cancelled());
        clone.cancel();
        assert!(token.is_cancelled());
        assert!(clone.is_cancelled());
    }

    #[test]
    fn cancellation_token_is_send_and_sync() {
        // Static assertion: the token type must be both `Send` and `Sync` so
        // it can be shared across rayon workers and the daemon dispatcher.
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<CancellationToken>();
        assert_sync::<CancellationToken>();
    }
}
