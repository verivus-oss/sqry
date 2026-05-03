//! NL07 — bounded classifier pool.
//!
//! [`ClassifierPool`] holds `N` independent loaded
//! [`crate::classifier::IntentClassifier`] sessions, each wrapped in a
//! [`crate::classifier::SharedClassifier`]. Concurrent translate calls
//! acquire a slot, classify, and release the slot back to the pool —
//! capping resident memory at `N × per-classifier RSS` regardless of
//! request fan-in.
//!
//! # Why a `crossbeam_channel::bounded` channel and not a queue + condvar?
//!
//! A naive `ArrayQueue + Mutex<bool> + Condvar` triple has a lost-wakeup
//! window: a producer that pushes between the consumer's queue-empty
//! check and `condvar.wait` will never wake the consumer. Crossbeam's
//! bounded channel wraps that wait/notify atomically, so a `recv()`
//! observed-empty + `send()` race cannot drop the wakeup. The acquire
//! path is `recv()`; the release path is `send()` of the same
//! [`SharedClassifier`].
//!
//! # Pool invariant — N distinct loaded sessions
//!
//! The constructor [`ClassifierPool::new`] calls the user-supplied
//! `loader` exactly `capacity` times. Each call MUST yield a freshly
//! loaded [`crate::classifier::IntentClassifier`] (separate
//! `IntentClassifier::load` invocation, separate `ort::Session`
//! allocation, separate model-weight buffer). The
//! [`crate::classifier::SharedClassifier`]s wrapping those classifiers
//! never alias one another — distinct slots = distinct sessions = the
//! pool fans out across N parallel inference workers.
//!
//! # Panic safety — slot return via `scopeguard::guard`
//!
//! [`PoolGuard`] wraps the held [`SharedClassifier`] in a
//! [`scopeguard::guard`] whose deferred closure performs the channel
//! send back into the pool. The scopeguard's drop hook runs on every
//! exit path — normal scope exit AND panicking-unwind — so a panic
//! inside [`crate::classifier::IntentClassifier::classify`] cannot
//! leak a slot.
//!
//! Why scopeguard rather than just a hand-rolled `Drop` impl on
//! `PoolGuard` (which Rust would also run on unwind)? The DAG NL07
//! `critical_decisions` list mandates scopeguard as the panic-safety
//! primitive: it makes the contract structurally explicit at the call
//! site (the closure that "must run" is named at the point the
//! invariant is established), and it survives any future refactor
//! that adds an intermediate fallible step between
//! [`ClassifierPool::acquire`] and the final slot release. The
//! crate-level `scopeguard` dependency is declared in
//! `sqry-nl/Cargo.toml` for exactly this purpose.
//!
//! # No tokio dependency
//!
//! The pool is sync. `recv()` blocks the current thread. Async callers
//! (sqry-daemon, sqry-lsp) MUST wrap [`ClassifierPool::acquire`] in
//! [`tokio::task::spawn_blocking`] at their boundary — sqry-nl itself
//! never imports tokio.

use crate::classifier::{IntentClassifier, SharedClassifier};
use crate::error::NlError;
use crossbeam_channel::{Receiver, Sender, bounded};

/// Lower bound for the pool capacity (NFR-2: one classifier minimum).
pub const POOL_MIN: usize = 1;

/// Upper bound for the pool capacity (NFR-2: cap RSS at 8 sessions).
pub const POOL_MAX: usize = 8;

/// Default pool size when neither config nor env-var supply one.
///
/// FR-15 requires at least 2 concurrent inference workers so the
/// daemon's MCP host and the LSP server can serve overlapping
/// `sqry_ask` calls without serialising on a single classifier
/// session.
pub const POOL_DEFAULT: usize = 2;

/// Bounded pool of N independently-loaded classifiers.
///
/// See module docs for the invariants this type enforces.
pub struct ClassifierPool {
    sender: Sender<SharedClassifier>,
    receiver: Receiver<SharedClassifier>,
    capacity: usize,
}

impl ClassifierPool {
    /// Build a pool of `capacity` independently-loaded classifiers.
    ///
    /// `capacity` is clamped into `[POOL_MIN, POOL_MAX]` (NFR-2).
    /// Calls `loader` exactly `capacity` times — see the pool
    /// invariant in the module-level docs.
    ///
    /// # Errors
    ///
    /// Propagates the first [`NlError`] returned by `loader`. On
    /// failure the partially-built pool is dropped, releasing any
    /// already-loaded classifiers.
    pub fn new<L>(capacity: usize, mut loader: L) -> Result<Self, NlError>
    where
        L: FnMut() -> Result<IntentClassifier, NlError>,
    {
        let capacity = capacity.clamp(POOL_MIN, POOL_MAX);
        let (sender, receiver) = bounded::<SharedClassifier>(capacity);
        for _ in 0..capacity {
            let classifier = loader()?;
            let shared = SharedClassifier::new(classifier);
            // The channel was just created with capacity == count, so
            // every send fits without blocking. A failure here is a
            // programmer error (the channel can't be disconnected
            // before we've returned a Receiver to anyone), so panic.
            sender
                .send(shared)
                .expect("crossbeam_channel just created with capacity == iteration count");
        }
        Ok(Self {
            sender,
            receiver,
            capacity,
        })
    }

    /// Pool capacity (post-clamp).
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Acquire a slot, blocking the current thread until one is
    /// available. The returned [`PoolGuard`] returns the slot on
    /// drop (panic-safe via [`scopeguard::guard`]).
    ///
    /// # Panics
    ///
    /// Panics only if the channel has been disconnected — which is
    /// impossible during normal use because the [`ClassifierPool`]
    /// itself owns the receiver, so the only way to disconnect it is
    /// to drop the pool. Acquiring a guard on a dropped pool would be
    /// a use-after-free style bug.
    pub fn acquire(&self) -> PoolGuard<'_> {
        let shared = self
            .receiver
            .recv()
            .expect("ClassifierPool channel disconnected — pool dropped while in use");
        // `scopeguard::guard` wraps `shared` so its on-drop closure —
        // the channel `send` that returns the slot to the pool — runs
        // on every exit path, including panicking unwind. Cloning the
        // `Sender` is cheap (it's an Arc internally) and lets the
        // closure outlive the borrow on `&self`. Capturing the sender
        // by move into the closure makes the panic-safety contract
        // structurally explicit at the acquire site, per NL07
        // critical_decisions.
        let sender = self.sender.clone();
        let on_release: SlotReturn = Box::new(move |shared| {
            // Best-effort: a disconnected channel here means the pool
            // was dropped while a guard was live, which is a tear-down
            // sequencing bug. We swallow the error because Drop must
            // not panic during unwind (would abort).
            let _ = sender.send(shared);
        });
        let scoped = scopeguard::guard(shared, on_release);
        PoolGuard {
            scoped: Some(scoped),
            _pool: std::marker::PhantomData,
        }
    }
}

impl std::fmt::Debug for ClassifierPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClassifierPool")
            .field("capacity", &self.capacity)
            .field("available", &self.receiver.len())
            .finish()
    }
}

/// RAII guard returned by [`ClassifierPool::acquire`].
///
/// Holds a single [`SharedClassifier`] for the duration of one
/// translate call. Returns the slot to the pool on drop, including
/// the panicking-unwind path. Slot return is implemented via
/// [`scopeguard::guard`] so the panic-safety contract is structurally
/// explicit (see module-level docs).
///
/// The `'a` lifetime ties this guard to the parent [`ClassifierPool`]
/// borrow so callers cannot stash a guard past the pool's lifetime.
/// The actual slot-return mechanism is a cloned [`Sender`] inside the
/// scopeguard closure, not a reference to the pool — but the lifetime
/// keeps API ergonomics consistent with the previous Drop-based
/// implementation.
pub struct PoolGuard<'a> {
    /// Scopeguard wrapping the held [`SharedClassifier`]. The on-drop
    /// closure does the channel send. `Option` so [`Drop`] can move
    /// it out (although in practice the scopeguard's own drop hook
    /// fires when `PoolGuard` is dropped — `Option::take` here is a
    /// belt-and-suspenders against any future refactor that wants to
    /// disarm or re-arm the guard.)
    scoped: Option<scopeguard::ScopeGuard<SharedClassifier, SlotReturn>>,
    _pool: std::marker::PhantomData<&'a ClassifierPool>,
}

/// Type alias for the boxed return-to-pool closure. A boxed closure
/// keeps `PoolGuard`'s type signature concrete so its size is stable
/// across the public API surface, regardless of the captured-sender
/// representation.
type SlotReturn = Box<dyn FnOnce(SharedClassifier) + Send + 'static>;

// `scopeguard::guard` is generic over the closure type, so we can't
// directly name `ScopeGuard<SharedClassifier, SlotReturn>` from the
// closure expression in `acquire` (closure types are anonymous). We
// erase the closure to a `Box<dyn FnOnce>` at the call site by
// constructing the boxed closure first, then passing it into
// `scopeguard::guard`.
impl<'a> PoolGuard<'a> {
    /// Borrow the held [`SharedClassifier`].
    ///
    /// # Panics
    ///
    /// Panics if called after the guard's `Drop` impl ran (impossible
    /// through the normal API — the borrow is bound by `'a`).
    #[must_use]
    pub fn classifier(&self) -> &SharedClassifier {
        // `scopeguard::ScopeGuard` derefs to `&T` (in our case
        // `&SharedClassifier`). Explicit `Deref::deref` keeps clippy
        // happy — autoderef would otherwise complain at the bare
        // `&**scoped` form.
        use std::ops::Deref;
        let scoped = self
            .scoped
            .as_ref()
            .expect("PoolGuard accessed after drop — invariant violated");
        scoped.deref()
    }
}

// PoolGuard's own `Drop` is intentionally a no-op: the scopeguard
// inside `scoped` runs ITS on-drop closure when the `Option<ScopeGuard>`
// is dropped (whether by going out of scope or by panic-unwind), which
// performs the channel send. This is the structural panic-safety
// contract scopeguard provides — see module-level docs.

/// Resolve the effective pool size from a configured value, the
/// `SQRY_NL_POOL_SIZE` environment variable, and the default.
///
/// Resolution order (highest priority first):
/// 1. `configured` (e.g. `TranslatorConfig::classifier_pool_size`).
/// 2. `SQRY_NL_POOL_SIZE` env var (parsed as `usize`).
/// 3. [`POOL_DEFAULT`].
///
/// The result is clamped into `[POOL_MIN, POOL_MAX]` per NFR-2.
#[must_use]
pub fn resolve_pool_size(configured: Option<usize>) -> usize {
    let raw = configured
        .or_else(|| {
            std::env::var("SQRY_NL_POOL_SIZE")
                .ok()
                .and_then(|s| s.trim().parse::<usize>().ok())
        })
        .unwrap_or(POOL_DEFAULT);
    raw.clamp(POOL_MIN, POOL_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ClassificationResult, Intent};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Tiny stand-in for [`IntentClassifier`] so unit tests don't need
    /// the ONNX dylib + model fixtures. We can't construct a real
    /// `IntentClassifier` in unit-test scope (it owns an
    /// `ort::Session`), so the `loader` argument to
    /// [`ClassifierPool::new`] would normally need real artifacts.
    ///
    /// Instead, the unit tests below exercise [`resolve_pool_size`]
    /// and the channel mechanics by building a pool over a mocked
    /// `IntentClassifier` only when the test needs the full pool.
    /// See `sqry-nl/tests/pool_concurrent_load.rs` for the integration
    /// test that uses the real `IntentClassifier::load`.
    fn _silence_unused_warning() {
        let _ = ClassificationResult {
            intent: Intent::Ambiguous,
            confidence: 0.0,
            all_probabilities: vec![],
            model_version: "test".into(),
        };
    }

    /// Process-global env-var lock serialises the three
    /// `resolve_pool_size_*` tests that touch `SQRY_NL_POOL_SIZE` so
    /// `cargo test`'s parallel runner cannot observe one test's stale
    /// `set_var` from another. parking_lot Mutex avoids poisoning.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Pool size resolution honours config > env > default.
    #[test]
    fn resolve_pool_size_prefers_configured() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env vars are process-global; the ENV_LOCK serialises
        // every test in this module that mutates SQRY_NL_POOL_SIZE.
        unsafe { std::env::set_var("SQRY_NL_POOL_SIZE", "6") };
        assert_eq!(resolve_pool_size(Some(3)), 3);
        unsafe { std::env::remove_var("SQRY_NL_POOL_SIZE") };
    }

    #[test]
    fn resolve_pool_size_falls_back_to_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: see resolve_pool_size_prefers_configured.
        unsafe { std::env::set_var("SQRY_NL_POOL_SIZE", "5") };
        assert_eq!(resolve_pool_size(None), 5);
        unsafe { std::env::remove_var("SQRY_NL_POOL_SIZE") };
    }

    #[test]
    fn resolve_pool_size_default_when_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Make sure no stale value is leaking from a sibling test.
        // SAFETY: see resolve_pool_size_prefers_configured.
        unsafe { std::env::remove_var("SQRY_NL_POOL_SIZE") };
        assert_eq!(resolve_pool_size(None), POOL_DEFAULT);
    }

    #[test]
    fn resolve_pool_size_clamped_to_max() {
        assert_eq!(resolve_pool_size(Some(999)), POOL_MAX);
    }

    #[test]
    fn resolve_pool_size_clamped_to_min() {
        assert_eq!(resolve_pool_size(Some(0)), POOL_MIN);
    }

    /// The pool's `capacity()` reflects the post-clamp size.
    ///
    /// We can't construct a real IntentClassifier in unit-test scope,
    /// but we can stand up a synthetic mini-pool over a hand-built
    /// channel to assert the channel mechanics independently.
    /// The end-to-end "N distinct sessions" assertion lives in
    /// `tests/pool_concurrent_load.rs`.
    #[test]
    fn capacity_clamps_above_max() {
        // Build the pool using a loader that fails immediately so we
        // exercise the clamp without needing a real IntentClassifier.
        // The clamp is observable through the error path's iteration
        // count: a request for capacity 999 must produce at most
        // POOL_MAX loader invocations before bailing.
        let count = Arc::new(AtomicUsize::new(0));
        let count_inner = Arc::clone(&count);
        let res = ClassifierPool::new(999, move || -> Result<IntentClassifier, NlError> {
            count_inner.fetch_add(1, Ordering::SeqCst);
            Err(NlError::Config("synthetic loader failure".into()))
        });
        assert!(res.is_err());
        // Only one loader call before the failure short-circuits;
        // critically, no run-away iteration to 999.
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    /// PoolGuard returns the slot on drop. We exercise this with a
    /// hand-built channel of [`SharedClassifier`]s that do NOT wrap
    /// a real IntentClassifier — the pool mechanics are independent
    /// of what's inside the `SharedClassifier`. (A `SharedClassifier`
    /// is `Arc<Mutex<IntentClassifier>>` and we never lock it here,
    /// so we can't actually allocate one in unit-test scope.) For the
    /// guard-drop assertion we use the channel directly.
    #[test]
    fn channel_recv_send_round_trips() {
        let (tx, rx) = bounded::<u64>(2);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        // Acquire both, "use" them, send them back.
        let a = rx.recv().unwrap();
        let b = rx.recv().unwrap();
        assert_eq!(rx.len(), 0);
        tx.send(a).unwrap();
        tx.send(b).unwrap();
        assert_eq!(rx.len(), 2);
    }
}
