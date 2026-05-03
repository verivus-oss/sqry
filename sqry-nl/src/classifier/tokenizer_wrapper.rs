//! `SharedClassifier`: one slot owning one loaded [`IntentClassifier`].
//!
//! Each pool slot wraps a single [`SharedClassifier`]. The
//! `Arc<parking_lot::Mutex<...>>` layer is necessary because
//! [`ort::session::Session::run`](ort::session::Session) requires
//! `&mut self` while the daemon serves shared-reference dispatch.
//! Cloning the inner [`Arc`] is cheap; a clone re-circulates the
//! **same** loaded session, **NOT** a fan-out across slots. Distinct
//! slots = distinct [`SharedClassifier`] instances created via separate
//! [`IntentClassifier::load`] calls (see NL07 `ClassifierPool`).
//!
//! [`parking_lot::Mutex`] is used per the workspace memory & concurrency
//! rule (NOT [`std::sync::Mutex`]). `parking_lot` mutexes are not
//! poisoned, so a panic during `Session::run` leaves the mutex usable —
//! important for the NL07 scopeguard panic-safety contract.
//!
//! The guarded type is `!Sync` (it owns an [`ort::session::Session`]),
//! but the wrapper itself is `Send + Sync + Clone` because the
//! [`parking_lot::Mutex`] guarantees exclusive access at runtime. A
//! compile-time assertion below pins these auto-trait bounds so future
//! refactors cannot silently break them.

use crate::classifier::IntentClassifier;
use parking_lot::Mutex;
use std::sync::Arc;

/// Shared, lockable handle to a single loaded [`IntentClassifier`].
///
/// One handle = one loaded model session. Cloning is `O(1)` and
/// recirculates the same underlying session; it does NOT duplicate
/// model weights or fan out to a parallel session. To serve N
/// concurrent inferences, allocate N independent
/// [`SharedClassifier`] instances (one [`IntentClassifier::load`]
/// each) — see NL07's `ClassifierPool`.
#[derive(Clone)]
pub struct SharedClassifier(pub(crate) Arc<Mutex<IntentClassifier>>);

impl SharedClassifier {
    /// Wrap an already-loaded [`IntentClassifier`] for shared,
    /// lock-mediated access.
    #[must_use]
    pub fn new(classifier: IntentClassifier) -> Self {
        Self(Arc::new(Mutex::new(classifier)))
    }

    /// Acquire the lock for `&mut` access (required by
    /// [`ort::session::Session::run`](ort::session::Session)).
    ///
    /// Use the returned guard within a small scope; do **NOT** hold it
    /// across `.await` points (the wrapping crate is sync-only anyway,
    /// but this rule still applies to any future async wrapper).
    ///
    /// The returned [`parking_lot::MutexGuard`] is itself `#[must_use]`,
    /// so dropping it on the floor is a clippy warning at the call
    /// site.
    pub fn lock(&self) -> parking_lot::MutexGuard<'_, IntentClassifier> {
        self.0.lock()
    }

    /// Stable identity for this slot's underlying `Arc<Mutex<_>>`.
    ///
    /// Distinct loaded sessions = distinct identities. Used by the
    /// NL07 integration test
    /// (`sqry-nl/tests/pool_concurrent_load.rs`) to assert that the
    /// pool's `N` slots map to `N` independent `IntentClassifier`
    /// instances. Cloning a [`SharedClassifier`] re-circulates the
    /// same `Arc` and therefore returns the same identity.
    #[must_use]
    pub fn identity(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time assertion: [`SharedClassifier`] must be
    /// `Send + Sync + Clone` so the NL07 pool can hand clones across
    /// daemon worker threads. If a future refactor smuggles in a
    /// non-`Send`/`Sync` field, this stops compiling.
    #[test]
    fn shared_classifier_is_send_sync_clone() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        fn assert_clone<T: Clone>() {}
        assert_send::<SharedClassifier>();
        assert_sync::<SharedClassifier>();
        assert_clone::<SharedClassifier>();
    }
}
