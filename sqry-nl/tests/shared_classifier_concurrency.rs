//! NL06 — `SharedClassifier` concurrency contract.
//!
//! Acceptance gate from the DAG: 10 threads × 100 calls must not
//! deadlock when sharing a single [`SharedClassifier`] across threads.
//!
//! Why this is `#[ignore]` by default: constructing a real
//! [`IntentClassifier`] requires both the ONNX Runtime dynamic library
//! at process load time AND the committed model artifacts under
//! `sqry-nl/models/`. Neither is guaranteed in lean CI runners. A
//! [`IntentClassifier::new()`]-style no-ONNX test seam does not exist
//! (the type owns an [`ort::session::Session`] that requires a real
//! `.onnx` file), so we cannot exercise the concrete wrapper without
//! the dylib.
//!
//! Run manually after a `make dev-classifier` (or equivalent ONNX
//! Runtime install) with:
//!
//! ```bash
//! cargo test -p sqry-nl --features classifier --test \
//!     shared_classifier_concurrency -- --ignored --nocapture
//! ```
//!
//! The compile-time `Send + Sync + Clone` assertion lives in
//! `sqry-nl/src/classifier/tokenizer_wrapper.rs` (unit test
//! `shared_classifier_is_send_sync_clone`) and runs unconditionally —
//! that test alone establishes the auto-trait bounds the NL07 pool
//! depends on. This `#[ignore]`d test additionally proves the runtime
//! lock contract holds under contention against a real loaded session.

#![cfg(feature = "classifier")]

use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use sqry_nl::classifier::{IntentClassifier, SharedClassifier, TrustMode};

/// Locate the in-tree model directory shipped under
/// `sqry-nl/models/`. The path is relative to the crate's
/// `CARGO_MANIFEST_DIR` so it resolves regardless of where the test
/// binary is executed from.
fn in_tree_model_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models")
}

/// 10 threads × 100 calls — proves the [`parking_lot::Mutex`] inside
/// [`SharedClassifier`] does not deadlock under sustained contention.
///
/// Each thread:
/// 1. Waits on a [`Barrier`] so all threads start hammering the lock
///    at the same moment (worst-case contention).
/// 2. Performs 100 lock-acquire / classify / drop-guard cycles.
/// 3. The wall-clock budget is generous (60s) — the assertion is on
///    *liveness* (no deadlock), not throughput.
#[test]
#[ignore = "requires ONNX Runtime dylib + committed model fixtures; run manually with --ignored"]
fn concurrent_classify_does_not_deadlock() {
    const THREADS: usize = 10;
    const CALLS_PER_THREAD: usize = 100;
    const BUDGET: Duration = Duration::from_secs(60);

    let model_dir = in_tree_model_dir();
    assert!(
        model_dir.join("intent_classifier.onnx").exists(),
        "expected committed model at {}; install ONNX fixtures \
         before running this test",
        model_dir.display(),
    );

    // Strict-mode load (NL04 default) against the in-tree fixtures.
    // TrustMode::Custom because the in-tree models/manifest.json is
    // the source of truth in the source tree (the baked Trusted
    // anchor matches the same file, but Custom is what tests use to
    // avoid coupling to the Trusted resolver chain).
    let classifier = IntentClassifier::load(&model_dir, false, TrustMode::Custom)
        .expect("in-tree model fixture must load cleanly under strict integrity");
    let shared = SharedClassifier::new(classifier);

    let barrier = Arc::new(Barrier::new(THREADS));
    let start = Instant::now();
    let mut handles = Vec::with_capacity(THREADS);

    for tid in 0..THREADS {
        let shared = shared.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            barrier.wait();
            for call in 0..CALLS_PER_THREAD {
                let mut guard = shared.lock();
                let result = guard
                    .classify("find all functions that handle authentication")
                    .expect("classify must succeed against in-tree model");
                // Touch the result so the optimiser cannot elide the
                // call. We don't assert on the intent because that
                // would couple this liveness test to model behaviour.
                std::hint::black_box(result);
                drop(guard);
                if call.is_multiple_of(25) {
                    eprintln!("thread {tid} progress: {call}/{CALLS_PER_THREAD}");
                }
            }
        }));
    }

    for h in handles {
        h.join()
            .expect("worker thread panicked — lock contract broken");
    }

    let elapsed = start.elapsed();
    assert!(
        elapsed < BUDGET,
        "contention test exceeded liveness budget: {elapsed:?} >= {BUDGET:?}",
    );
    eprintln!(
        "concurrent_classify_does_not_deadlock: {THREADS} threads × \
         {CALLS_PER_THREAD} calls completed in {elapsed:?}",
    );
}
