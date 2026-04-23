//! Task 8 Phase 8c U14 — ShimRegistry cap-race integration tests.
//!
//! These tests drive [`sqry_daemon::ipc::ShimRegistry::try_register_bounded`]
//! from multiple OS threads simultaneously (std threads + `Barrier` for
//! genuine concurrency per Codex iter-1 Q6) and verify that the atomic
//! single-mutex-guard admission never over- or under-subscribes the cap.
//!
//! These are integration tests (not unit tests) because they exercise the
//! full `ShimRegistry` public API as exposed through `sqry_daemon::ipc`,
//! including the RAII `ShimHandle` drop behaviour.

use std::sync::{Arc, Barrier};
use std::thread;

use sqry_daemon::ipc::protocol::ShimProtocol;
use sqry_daemon::ipc::{ShimHandle, ShimRegistry};

// ---------------------------------------------------------------------------
// Test 1: cap_256_race_300_concurrent_registrations_admits_exactly_256
//
// 300 OS threads all block on a Barrier then race into
// `try_register_bounded(cap=256)`. Exactly 256 must succeed; 44 must
// be rejected. This is the regression proof for the iter-0 B2 blocker:
// the old two-step `len() >= cap` + `register()` could oversubscribe
// under this load.
// ---------------------------------------------------------------------------

#[test]
fn cap_256_race_300_concurrent_registrations_admits_exactly_256() {
    let registry = ShimRegistry::new();
    let cap: usize = 256;
    let n_threads: usize = 300;
    let barrier = Arc::new(Barrier::new(n_threads));

    let join_handles: Vec<_> = (0..n_threads)
        .map(|i| {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || -> Option<ShimHandle> {
                barrier.wait();
                registry
                    .try_register_bounded(ShimProtocol::Mcp, i as u32, cap)
                    .ok()
            })
        })
        .collect();

    let mut accepted: Vec<ShimHandle> = Vec::new();
    let mut rejected: usize = 0;
    for jh in join_handles {
        match jh.join().expect("thread panicked") {
            Some(handle) => accepted.push(handle),
            None => rejected += 1,
        }
    }

    assert_eq!(
        accepted.len(),
        cap,
        "exactly cap={cap} registrations must succeed"
    );
    assert_eq!(
        rejected,
        n_threads - cap,
        "remaining {rem} must be rejected",
        rem = n_threads - cap
    );
    assert_eq!(
        registry.len(),
        cap,
        "registry len must equal cap while handles held"
    );

    // Drop all handles: registry must be empty again.
    drop(accepted);
    assert_eq!(
        registry.len(),
        0,
        "registry must be empty after all handles dropped"
    );
}

// ---------------------------------------------------------------------------
// Test 2: cap_2_race_10_concurrent_deterministic_admission
//
// With cap=2 and 10 concurrent threads, at most 2 succeed and at least
// 8 are rejected. The precise accepted count is verified to be exactly 2.
// ---------------------------------------------------------------------------

#[test]
fn cap_2_race_10_concurrent_deterministic_admission() {
    let registry = ShimRegistry::new();
    let cap: usize = 2;
    let n_threads: usize = 10;
    let barrier = Arc::new(Barrier::new(n_threads));

    let join_handles: Vec<_> = (0..n_threads)
        .map(|i| {
            let registry = Arc::clone(&registry);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || -> Option<ShimHandle> {
                barrier.wait();
                registry
                    .try_register_bounded(ShimProtocol::Lsp, i as u32, cap)
                    .ok()
            })
        })
        .collect();

    let mut accepted: Vec<ShimHandle> = Vec::new();
    let mut rejected: usize = 0;
    for jh in join_handles {
        match jh.join().expect("thread panicked") {
            Some(handle) => accepted.push(handle),
            None => rejected += 1,
        }
    }

    assert_eq!(
        accepted.len(),
        cap,
        "exactly cap={cap} registrations must succeed"
    );
    assert_eq!(
        rejected,
        n_threads - cap,
        "remaining threads must be rejected"
    );
    assert_eq!(registry.len(), cap);

    drop(accepted);
    assert_eq!(registry.len(), 0);
}

// ---------------------------------------------------------------------------
// Test 3: cap_change_mid_race_respects_value_at_decision_time
//
// `try_register_bounded` receives the cap value as a parameter rather
// than reading it from internal state. This test verifies that if the
// caller passes a DIFFERENT cap on consecutive calls (simulating a
// config hot-reload scenario), each call respects the cap it was given
// at call time.
//
// Specifically:
//   - Round 1: 5 threads race with cap=3; exactly 3 succeed.
//   - Drop all handles from round 1 (registry back to 0).
//   - Round 2: same 5 threads race with cap=5 on the SAME registry; exactly 5 succeed.
//   - Validates that the cap stored in the registry at decision time is the
//     parameter, not some internal cached value.
// ---------------------------------------------------------------------------

#[test]
fn cap_change_mid_race_respects_value_at_decision_time() {
    let registry = ShimRegistry::new();

    // Round 1: cap=3, 5 threads race.
    {
        let cap: usize = 3;
        let n: usize = 5;
        let barrier = Arc::new(Barrier::new(n));

        let handles: Vec<_> = (0..n)
            .map(|i| {
                let reg = Arc::clone(&registry);
                let b = Arc::clone(&barrier);
                thread::spawn(move || -> Option<ShimHandle> {
                    b.wait();
                    reg.try_register_bounded(ShimProtocol::Lsp, i as u32, cap)
                        .ok()
                })
            })
            .collect();

        let accepted: Vec<ShimHandle> = handles
            .into_iter()
            .filter_map(|h| h.join().expect("thread panicked"))
            .collect();

        assert_eq!(
            accepted.len(),
            cap,
            "round-1: exactly cap={cap} must be admitted"
        );
        assert_eq!(registry.len(), cap);

        // Drop all handles — registry returns to 0.
        drop(accepted);
        assert_eq!(
            registry.len(),
            0,
            "round-1: registry must be empty after drop"
        );
    }

    // Round 2: cap=5 (higher cap), 5 threads race — all must succeed.
    {
        let cap: usize = 5;
        let n: usize = 5;
        let barrier = Arc::new(Barrier::new(n));

        let handles: Vec<_> = (0..n)
            .map(|i| {
                let reg = Arc::clone(&registry);
                let b = Arc::clone(&barrier);
                thread::spawn(move || -> Option<ShimHandle> {
                    b.wait();
                    reg.try_register_bounded(ShimProtocol::Mcp, i as u32, cap)
                        .ok()
                })
            })
            .collect();

        let accepted: Vec<ShimHandle> = handles
            .into_iter()
            .filter_map(|h| h.join().expect("thread panicked"))
            .collect();

        assert_eq!(
            accepted.len(),
            cap,
            "round-2: all {n} threads must succeed at cap={cap}"
        );
        assert_eq!(registry.len(), cap);

        drop(accepted);
        assert_eq!(
            registry.len(),
            0,
            "round-2: registry must be empty after drop"
        );
    }
}
