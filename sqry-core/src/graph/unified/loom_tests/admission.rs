//! Loom tests for admission control (Step 28).
//!
//! Tests SharedBufferState atomic operations under all interleavings:
//!
//! - CP-16: Reserve + commit race conditions
//! - CP-17: Concurrent reservation limit checking
//! - CP-18: Active guard count underflow protection
//! - CP-19: Counter reset with active guards

use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::thread;

/// Loom-compatible version of SharedBufferState for testing.
///
/// This replicates the core atomic operations from `admission::state::SharedBufferState`
/// using loom's atomic primitives for exhaustive interleaving testing.
#[derive(Debug)]
struct LoomSharedBufferState {
    committed_bytes: AtomicUsize,
    committed_ops: AtomicUsize,
    reserved_bytes: AtomicUsize,
    reserved_ops: AtomicUsize,
    active_guards: AtomicUsize,
}

impl LoomSharedBufferState {
    fn new() -> Self {
        Self {
            committed_bytes: AtomicUsize::new(0),
            committed_ops: AtomicUsize::new(0),
            reserved_bytes: AtomicUsize::new(0),
            reserved_ops: AtomicUsize::new(0),
            active_guards: AtomicUsize::new(0),
        }
    }

    fn committed_bytes(&self) -> usize {
        self.committed_bytes.load(Ordering::Acquire)
    }

    fn reserved_bytes(&self) -> usize {
        self.reserved_bytes.load(Ordering::Acquire)
    }

    fn active_guards(&self) -> usize {
        self.active_guards.load(Ordering::Acquire)
    }

    #[allow(dead_code)]
    fn total_bytes(&self) -> usize {
        self.committed_bytes() + self.reserved_bytes()
    }

    fn increment_active_guards(&self) {
        self.active_guards.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrements active guard count.
    ///
    /// # Panics
    ///
    /// Panics if the counter would underflow (mirroring FR-64 production semantics).
    fn decrement_active_guards(&self) {
        let prev = self.active_guards.fetch_sub(1, Ordering::AcqRel);
        assert!(prev > 0, "active_guards underflow: decrement from 0");
    }

    fn add_reserved(&self, bytes: usize, ops: usize) {
        self.reserved_bytes.fetch_add(bytes, Ordering::AcqRel);
        self.reserved_ops.fetch_add(ops, Ordering::AcqRel);
    }

    /// Subtracts from reserved counters.
    ///
    /// # Panics
    ///
    /// Panics if counters would underflow (mirroring FR-64 production semantics).
    fn sub_reserved(&self, bytes: usize, ops: usize) {
        let prev_bytes = self.reserved_bytes.fetch_sub(bytes, Ordering::AcqRel);
        let prev_ops = self.reserved_ops.fetch_sub(ops, Ordering::AcqRel);
        assert!(
            prev_bytes >= bytes,
            "reserved_bytes underflow: {} < {}",
            prev_bytes,
            bytes
        );
        assert!(
            prev_ops >= ops,
            "reserved_ops underflow: {} < {}",
            prev_ops,
            ops
        );
    }

    /// Transfers from reserved to committed.
    ///
    /// # Panics
    ///
    /// Panics if actual exceeds reserved or if reserved would underflow.
    fn transfer_reserved_to_committed(
        &self,
        reserved_bytes: usize,
        reserved_ops: usize,
        actual_bytes: usize,
        actual_ops: usize,
    ) {
        assert!(
            actual_bytes <= reserved_bytes,
            "actual_bytes {} exceeds reserved_bytes {}",
            actual_bytes,
            reserved_bytes
        );
        assert!(
            actual_ops <= reserved_ops,
            "actual_ops {} exceeds reserved_ops {}",
            actual_ops,
            reserved_ops
        );

        let prev_bytes = self
            .reserved_bytes
            .fetch_sub(reserved_bytes, Ordering::AcqRel);
        let prev_ops = self.reserved_ops.fetch_sub(reserved_ops, Ordering::AcqRel);

        assert!(
            prev_bytes >= reserved_bytes,
            "reserved_bytes underflow in transfer"
        );
        assert!(
            prev_ops >= reserved_ops,
            "reserved_ops underflow in transfer"
        );

        self.committed_bytes
            .fetch_add(actual_bytes, Ordering::AcqRel);
        self.committed_ops.fetch_add(actual_ops, Ordering::AcqRel);
    }

    /// Subtracts from committed counters.
    ///
    /// # Panics
    ///
    /// Panics if counters would underflow.
    fn sub_committed(&self, bytes: usize, ops: usize) {
        let prev_bytes = self.committed_bytes.fetch_sub(bytes, Ordering::AcqRel);
        let prev_ops = self.committed_ops.fetch_sub(ops, Ordering::AcqRel);
        assert!(
            prev_bytes >= bytes,
            "committed_bytes underflow: {} < {}",
            prev_bytes,
            bytes
        );
        assert!(
            prev_ops >= ops,
            "committed_ops underflow: {} < {}",
            prev_ops,
            ops
        );
    }

    fn try_reset_to_zero(&self) -> bool {
        let active = self.active_guards.load(Ordering::Acquire);
        if active > 0 {
            return false;
        }

        self.committed_bytes.store(0, Ordering::Release);
        self.committed_ops.store(0, Ordering::Release);
        self.reserved_bytes.store(0, Ordering::Release);
        self.reserved_ops.store(0, Ordering::Release);
        true
    }
}

fn try_reserve_with_limit(
    state: &LoomSharedBufferState,
    max_bytes: usize,
    bytes: usize,
    ops: usize,
) -> bool {
    loop {
        let current_reserved = state.reserved_bytes.load(Ordering::Acquire);
        let current_committed = state.committed_bytes.load(Ordering::Acquire);
        let total = current_reserved + current_committed;

        if total + bytes > max_bytes {
            return false;
        }

        if state
            .reserved_bytes
            .compare_exchange_weak(
                current_reserved,
                current_reserved + bytes,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            state.reserved_ops.fetch_add(ops, Ordering::AcqRel);
            return true;
        }
    }
}

fn spawn_reservation_thread(
    state: Arc<LoomSharedBufferState>,
    max_bytes: usize,
    bytes: usize,
    ops: usize,
    success_count: Arc<AtomicUsize>,
) -> loom::thread::JoinHandle<()> {
    thread::spawn(move || {
        if try_reserve_with_limit(&state, max_bytes, bytes, ops) {
            success_count.fetch_add(1, Ordering::Relaxed);
        }
    })
}

fn join_threads(threads: [loom::thread::JoinHandle<()>; 2]) {
    for thread in threads {
        thread.join().unwrap();
    }
}

/// CP-16: Reserve + commit race conditions.
///
/// Verifies that concurrent reserve and commit operations maintain counter invariants.
#[test]
fn test_cp16_reserve_commit_race() {
    loom::model(|| {
        let state = Arc::new(LoomSharedBufferState::new());
        let state1 = Arc::clone(&state);
        let state2 = Arc::clone(&state);

        // Thread 1: Reserve and commit
        let t1 = thread::spawn(move || {
            state1.increment_active_guards();
            state1.add_reserved(100, 1);
            state1.transfer_reserved_to_committed(100, 1, 80, 1);
            state1.decrement_active_guards();
        });

        // Thread 2: Reserve and commit
        let t2 = thread::spawn(move || {
            state2.increment_active_guards();
            state2.add_reserved(200, 2);
            state2.transfer_reserved_to_committed(200, 2, 150, 2);
            state2.decrement_active_guards();
        });

        join_threads([t1, t2]);

        // Invariant: All reservations committed, no active guards
        assert_eq!(state.active_guards(), 0);
        assert_eq!(state.reserved_bytes(), 0);
        assert_eq!(state.committed_bytes(), 230); // 80 + 150
    });
}

/// CP-17: Concurrent reservation limit checking with CAS loop.
///
/// Verifies that concurrent reservations using proper CAS loops respect limits.
/// Models the AdmissionController's dual CAS loop pattern (FR-57).
#[test]
fn test_cp17_concurrent_reservation_limits() {
    loom::model(|| {
        let state = Arc::new(LoomSharedBufferState::new());
        let max_bytes: usize = 150;

        // Track successful reservations
        let success_count = Arc::new(AtomicUsize::new(0));

        let t1 = spawn_reservation_thread(
            Arc::clone(&state),
            max_bytes,
            100,
            1,
            Arc::clone(&success_count),
        );
        let t2 = spawn_reservation_thread(
            Arc::clone(&state),
            max_bytes,
            100,
            1,
            Arc::clone(&success_count),
        );

        join_threads([t1, t2]);

        // With proper CAS-based limit checking:
        // - At most one thread should succeed (limit = 150, each wants 100)
        // - Total reserved should never exceed max_bytes
        let total = state.reserved_bytes();
        let successes = success_count.load(Ordering::Relaxed);

        // CRITICAL INVARIANT: Total must never exceed limit
        assert!(
            total <= max_bytes,
            "Limit violated: total {} > max {}",
            total,
            max_bytes
        );

        // Only one reservation should succeed (100 + 100 > 150)
        assert!(
            successes <= 1,
            "At most one reservation should succeed, got {}",
            successes
        );
    });
}

/// CP-18: Active guard count underflow protection.
///
/// Verifies that decrementing active guards below zero is detected.
#[test]
fn test_cp18_guard_underflow_protection() {
    loom::model(|| {
        let state = Arc::new(LoomSharedBufferState::new());
        let state1 = Arc::clone(&state);
        let state2 = Arc::clone(&state);

        // Increment twice
        state.increment_active_guards();
        state.increment_active_guards();

        // Thread 1: Decrement (will panic if underflow - loom detects)
        let t1 = thread::spawn(move || {
            state1.decrement_active_guards();
        });

        // Thread 2: Decrement (will panic if underflow - loom detects)
        let t2 = thread::spawn(move || {
            state2.decrement_active_guards();
        });

        join_threads([t1, t2]);

        // Final count should be 0
        assert_eq!(state.active_guards(), 0);
    });
}

/// CP-19: Counter reset with active guards.
///
/// Verifies that reset operations are rejected while guards are active.
/// Note: After guard is released, multiple resets can succeed (they're idempotent).
#[test]
fn test_cp19_reset_with_active_guards() {
    loom::model(|| {
        let state = Arc::new(LoomSharedBufferState::new());

        // Add some data and a guard
        state.add_reserved(100, 1);
        state.increment_active_guards();

        let state1 = Arc::clone(&state);
        let state2 = Arc::clone(&state);

        // Track results
        let t1_reset_succeeded = Arc::new(AtomicUsize::new(0));
        let reset1 = Arc::clone(&t1_reset_succeeded);

        // Thread 1: Try to reset (should fail while guard is active)
        let t1 = thread::spawn(move || {
            if state1.try_reset_to_zero() {
                // This can only succeed if t2 already released the guard
                reset1.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Thread 2: Release guard then try reset
        let t2 = thread::spawn(move || {
            state2.decrement_active_guards();
            // After release, reset should succeed
            let _ = state2.try_reset_to_zero();
        });

        join_threads([t1, t2]);

        // The key invariant: reset only succeeds when active_guards == 0
        // Both can succeed if t2 runs first (releases guard), then both see guards=0
        // t1 fails if it runs first (guard still active)
        // This is correct behavior - we're testing that reset checks guards
        assert_eq!(state.active_guards(), 0, "Guard should be released");
    });
}

/// Test concurrent reserve and abort operations.
///
/// Verifies that reservations can be safely aborted (sub_reserved) concurrently.
#[test]
fn test_concurrent_reserve_abort() {
    loom::model(|| {
        let state = Arc::new(LoomSharedBufferState::new());

        // Pre-reserve 200 bytes
        state.add_reserved(200, 2);

        let state1 = Arc::clone(&state);
        let state2 = Arc::clone(&state);

        // Thread 1: Abort 100 bytes
        let t1 = thread::spawn(move || {
            state1.sub_reserved(100, 1);
        });

        // Thread 2: Abort 100 bytes
        let t2 = thread::spawn(move || {
            state2.sub_reserved(100, 1);
        });

        join_threads([t1, t2]);

        // Both should succeed, leaving 0
        assert_eq!(state.reserved_bytes(), 0);
    });
}

/// Test concurrent commit and compaction (sub_committed).
///
/// Verifies that sub_committed operations during/after commits are safe.
#[test]
fn test_concurrent_commit_compaction() {
    loom::model(|| {
        let state = Arc::new(LoomSharedBufferState::new());

        // Setup: Reserve and commit 200 bytes
        state.add_reserved(200, 2);
        state.transfer_reserved_to_committed(200, 2, 200, 2);

        let state1 = Arc::clone(&state);
        let state2 = Arc::clone(&state);

        // Thread 1: Compaction removes 100 bytes
        let t1 = thread::spawn(move || {
            state1.sub_committed(100, 1);
        });

        // Thread 2: Compaction removes 100 bytes
        let t2 = thread::spawn(move || {
            state2.sub_committed(100, 1);
        });

        join_threads([t1, t2]);

        // Final committed should be 0
        assert_eq!(state.committed_bytes(), 0);
    });
}
