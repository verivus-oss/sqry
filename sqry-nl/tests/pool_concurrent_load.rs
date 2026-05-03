//! NL07 — `ClassifierPool` concurrent-load + panic-safety contract.
//!
//! Acceptance gates from the DAG:
//!
//! 1. `n_concurrent_translates_use_n_distinct_sessions` — building a
//!    pool of size `N` must invoke `IntentClassifier::load` exactly
//!    `N` times during init and zero further times during a fan-in
//!    of 16 concurrent translate calls.
//! 2. `panic_in_classify_does_not_lose_slot` — a panic inside the
//!    locked classify body MUST return the slot to the pool (the
//!    `PoolGuard` `Drop` impl runs on unwind).
//!
//! Like NL06's `shared_classifier_concurrency` test, these are
//! `#[ignore]`d by default because constructing a real
//! [`IntentClassifier`] requires the ONNX Runtime dynamic library +
//! committed model fixtures. Run manually:
//!
//! ```bash
//! cargo test -p sqry-nl --features classifier --test \
//!     pool_concurrent_load -- --ignored --nocapture
//! ```
//!
//! The pool's structural unit tests (capacity clamp, env-var
//! resolution) live in `sqry-nl/src/classifier/pool.rs`'s `#[cfg(test)]`
//! module and run unconditionally.

#![cfg(feature = "classifier")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use sqry_nl::classifier::{ClassifierPool, IntentClassifier, SharedClassifier, TrustMode};
use sqry_nl::error::NlError;

/// Locate the in-tree model directory shipped under
/// `sqry-nl/models/`. The path is relative to the crate's
/// `CARGO_MANIFEST_DIR` so it resolves regardless of where the test
/// binary is executed from.
fn in_tree_model_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models")
}

/// Pool init must call the loader exactly `N` times — never zero,
/// never more — and 16 fan-in translate calls afterwards must NOT
/// trigger a single additional load.
///
/// Distinct sessions are observed by counting unique
/// `Arc::as_ptr(&shared.0)` addresses across the 16 calls; the
/// channel returns each `SharedClassifier` exactly once before
/// recycling, so within one Barrier-released wave the workers must
/// see at least min(N, 16) distinct pointers.
#[test]
#[ignore = "requires ONNX Runtime dylib + committed model fixtures; run manually with --ignored"]
fn n_concurrent_translates_use_n_distinct_sessions() {
    const POOL_SIZE: usize = 4;
    const FANIN: usize = 16;

    let model_dir = in_tree_model_dir();
    assert!(
        model_dir.join("intent_classifier.onnx").exists(),
        "expected committed model at {}; install ONNX fixtures \
         before running this test",
        model_dir.display(),
    );

    // Load counter — incremented inside the loader closure.
    let load_calls = Arc::new(AtomicUsize::new(0));
    let load_calls_for_pool = Arc::clone(&load_calls);
    let model_dir_clone = model_dir.clone();

    let pool = ClassifierPool::new(POOL_SIZE, move || -> Result<IntentClassifier, NlError> {
        load_calls_for_pool.fetch_add(1, Ordering::SeqCst);
        IntentClassifier::load(&model_dir_clone, false, TrustMode::Custom).map_err(NlError::from)
    })
    .expect("pool init must succeed against in-tree model fixtures");

    let load_calls_after_init = load_calls.load(Ordering::SeqCst);
    assert_eq!(
        load_calls_after_init, POOL_SIZE,
        "pool of size {POOL_SIZE} must call loader exactly {POOL_SIZE} times during init, got {load_calls_after_init}"
    );
    assert_eq!(pool.capacity(), POOL_SIZE);

    // Run FANIN concurrent translate calls — each captures the Arc
    // pointer of the SharedClassifier it observed so the test can
    // count distinct pool slots.
    let pool = Arc::new(pool);
    let barrier = Arc::new(Barrier::new(FANIN));
    let observed = Arc::new(parking_lot::Mutex::new(Vec::with_capacity(FANIN)));

    let mut handles = Vec::with_capacity(FANIN);
    for _ in 0..FANIN {
        let pool = Arc::clone(&pool);
        let barrier = Arc::clone(&barrier);
        let observed = Arc::clone(&observed);
        handles.push(thread::spawn(move || {
            barrier.wait();
            let guard = pool.acquire();
            let shared: &SharedClassifier = guard.classifier();
            // Cast through `Arc::as_ptr` on the underlying Arc to
            // identify slot identity. SharedClassifier's inner field
            // is `pub(crate)`, but the wrapper carries a stable
            // pointer for any Arc clone.
            // `SharedClassifier::identity` returns a stable cast of
            // the underlying Arc pointer — the public API used to
            // assert "N distinct sessions" across pool slots.
            let ptr = shared.identity();
            // Touch the lock so the call really exercises the
            // session — but don't actually classify because that
            // would multiply test runtime by the model latency.
            // The pool invariant is about session identity, not
            // inference correctness.
            let mut classifier = shared.lock();
            std::hint::black_box(&mut *classifier);
            drop(classifier);
            observed.lock().push(ptr);
            // Guard drops here — slot returned.
        }));
    }
    for h in handles {
        h.join().expect("worker thread panicked");
    }

    // Zero further loads during the 16-call wave.
    let load_calls_after_wave = load_calls.load(Ordering::SeqCst);
    assert_eq!(
        load_calls_after_wave, POOL_SIZE,
        "no further loader calls allowed after init; got {load_calls_after_wave} total \
         (expected {POOL_SIZE} from init)"
    );

    // Distinct slots observed: with FANIN > POOL_SIZE the workers
    // race for slots, so the unique-pointer count is bounded by
    // min(POOL_SIZE, FANIN). Equality on POOL_SIZE asserts every slot
    // was visited at least once across the wave.
    let observed = observed.lock();
    let mut unique: std::collections::HashSet<usize> =
        std::collections::HashSet::with_capacity(POOL_SIZE);
    for &ptr in observed.iter() {
        unique.insert(ptr);
    }
    assert_eq!(
        unique.len(),
        POOL_SIZE,
        "expected exactly {POOL_SIZE} distinct pool slots to be observed across {FANIN} calls; \
         got {} unique pointers",
        unique.len()
    );
}

/// A panic inside the classify call must NOT lose the pool slot.
/// We exercise this by acquiring + dropping a guard from inside a
/// `catch_unwind` panic, then verifying the slot is back in the
/// channel by acquiring it again.
#[test]
#[ignore = "requires ONNX Runtime dylib + committed model fixtures; run manually with --ignored"]
fn panic_in_classify_does_not_lose_slot() {
    const POOL_SIZE: usize = 2;

    let model_dir = in_tree_model_dir();
    assert!(
        model_dir.join("intent_classifier.onnx").exists(),
        "expected committed model at {}",
        model_dir.display(),
    );

    let pool = ClassifierPool::new(POOL_SIZE, || -> Result<IntentClassifier, NlError> {
        IntentClassifier::load(&model_dir, false, TrustMode::Custom).map_err(NlError::from)
    })
    .expect("pool init");
    let pool = Arc::new(pool);

    // Drain the pool: panic in worker A while holding a guard.
    let pool_a = Arc::clone(&pool);
    let join_a = thread::spawn(move || {
        let _guard = pool_a.acquire();
        // Touch the SharedClassifier so the lock isn't optimised
        // away, then explicitly panic inside the guard's lifetime.
        let _shared = _guard.classifier().clone();
        panic!("synthetic panic during classify");
    });
    // Catch the panic — `JoinHandle::join` returns Err for a panic.
    let result = join_a.join();
    assert!(
        result.is_err(),
        "worker should have panicked; got {:?}",
        result.map(|()| "no panic")
    );

    // Slot must have been returned by `Drop`. Two further acquires
    // must succeed within a wall-clock budget.
    let pool_b = Arc::clone(&pool);
    let budget = Duration::from_secs(2);
    let start = Instant::now();
    let _g1 = pool_b.acquire();
    let _g2 = pool_b.acquire();
    assert!(
        start.elapsed() < budget,
        "second acquire after panic must succeed within {budget:?}; \
         post-panic deadlock indicates the slot was leaked"
    );
    drop(_g1);
    drop(_g2);

    // Cycle one more guard to confirm the channel is still live.
    let _g3 = pool_b.acquire();
    drop(_g3);
}

/// End-to-end sanity check: running 16 concurrent translates against
/// a real Translator (built over the in-tree model dir) must not
/// deadlock and must complete within an NFR-5-friendly budget.
///
/// This is a smoke test — it does not assert on per-call latency
/// p50/p99 because the harness here is the test crate, not the
/// daemon. The daemon-side assertion lives in
/// `sqry-daemon/tests/concurrent_ask_smoke.rs`.
#[test]
#[ignore = "requires ONNX Runtime dylib + committed model fixtures; run manually with --ignored"]
fn translator_pool_serves_concurrent_translates() {
    use sqry_nl::{Translator, TranslatorConfig};

    const FANIN: usize = 16;
    const BUDGET: Duration = Duration::from_secs(60);

    let model_dir = in_tree_model_dir();
    assert!(
        model_dir.join("intent_classifier.onnx").exists(),
        "expected committed model at {}",
        model_dir.display(),
    );

    let config = TranslatorConfig {
        model_dir_override: Some(model_dir),
        allow_unverified_model: false,
        classifier_pool_size: Some(4),
        ..TranslatorConfig::default()
    };
    let translator = Arc::new(Translator::new(config).expect("translator init"));

    let barrier = Arc::new(Barrier::new(FANIN));
    let start = Instant::now();
    let mut handles = Vec::with_capacity(FANIN);
    for tid in 0..FANIN {
        let translator = Arc::clone(&translator);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            let q = format!("find functions named worker_{tid}");
            let _resp = translator.translate_shared(&q);
        }));
    }
    for h in handles {
        h.join().expect("worker thread panicked — pool deadlock?");
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < BUDGET,
        "16 concurrent translates exceeded {BUDGET:?}: {elapsed:?}"
    );
}
