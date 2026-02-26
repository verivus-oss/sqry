//! `SharedBufferState`: Atomic counters for admission control.
//!
//! This module implements `SharedBufferState`, which provides the shared atomic
//! counters used by both `DeltaBuffer` and `AdmissionController` for back-pressure
//! management.
//!
//! # Design (FR-55)
//!
//! The state tracks:
//! - **Committed**: Bytes/ops that have been written to the delta buffer
//! - **Reserved**: Bytes/ops that have been reserved but not yet committed
//! - **Active guards**: Number of active `ReservationGuard` instances
//!
//! # Invariants
//!
//! - `committed + reserved <= max_limit` at all times
//! - `reserved = 0` for any consumed reservation (no leak)
//! - Counters cannot be reset while any `ReservationGuard` is active
//!
//! # Thread Safety
//!
//! All operations use atomic instructions with appropriate memory ordering:
//! - `Acquire` for reads that precede dependent operations
//! - `Release` for writes that follow dependent operations
//! - `AcqRel` for read-modify-write operations

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Shared state between `DeltaBuffer` and `AdmissionController`.
///
/// Owned by the delta buffer, referenced by the admission controller via `Arc`.
/// All counters are atomic for concurrent access.
///
/// # Memory Layout
///
/// ```text
/// SharedBufferState:
/// ┌─────────────────────────────────────────────────────────────┐
/// │ committed_bytes: AtomicUsize  │ committed_ops: AtomicUsize  │
/// │ reserved_bytes: AtomicUsize   │ reserved_ops: AtomicUsize   │
/// │ active_guards: AtomicUsize                                   │
/// └─────────────────────────────────────────────────────────────┘
/// ```
#[derive(Debug)]
pub struct SharedBufferState {
    /// Bytes currently in delta buffer (written data).
    pub(crate) committed_bytes: AtomicUsize,
    /// Operations currently in delta buffer.
    pub(crate) committed_ops: AtomicUsize,
    /// Reserved bytes (pending writes, not yet committed).
    pub(crate) reserved_bytes: AtomicUsize,
    /// Reserved operations (pending writes).
    pub(crate) reserved_ops: AtomicUsize,
    /// Number of active `ReservationGuard` instances.
    ///
    /// Used to prevent counter reset during active reservations.
    pub(crate) active_guards: AtomicUsize,
}

impl SharedBufferState {
    /// Creates a new `SharedBufferState` with all counters at zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            committed_bytes: AtomicUsize::new(0),
            committed_ops: AtomicUsize::new(0),
            reserved_bytes: AtomicUsize::new(0),
            reserved_ops: AtomicUsize::new(0),
            active_guards: AtomicUsize::new(0),
        }
    }

    // ==================== Read Operations ====================

    /// Returns the current committed bytes.
    #[inline]
    #[must_use]
    pub fn committed_bytes(&self) -> usize {
        self.committed_bytes.load(Ordering::Acquire)
    }

    /// Returns the current committed operations.
    #[inline]
    #[must_use]
    pub fn committed_ops(&self) -> usize {
        self.committed_ops.load(Ordering::Acquire)
    }

    /// Returns the current reserved bytes.
    #[inline]
    #[must_use]
    pub fn reserved_bytes(&self) -> usize {
        self.reserved_bytes.load(Ordering::Acquire)
    }

    /// Returns the current reserved operations.
    #[inline]
    #[must_use]
    pub fn reserved_ops(&self) -> usize {
        self.reserved_ops.load(Ordering::Acquire)
    }

    /// Returns the number of active reservation guards.
    #[inline]
    #[must_use]
    pub fn active_guards(&self) -> usize {
        self.active_guards.load(Ordering::Acquire)
    }

    /// Returns total bytes (committed + reserved).
    #[inline]
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.committed_bytes() + self.reserved_bytes()
    }

    /// Returns total operations (committed + reserved).
    #[inline]
    #[must_use]
    pub fn total_ops(&self) -> usize {
        self.committed_ops() + self.reserved_ops()
    }

    // ==================== Snapshot ====================

    /// Takes a consistent snapshot of all counters.
    ///
    /// Note: This is not strictly atomic across all fields, but provides
    /// a reasonable approximation for monitoring purposes.
    #[must_use]
    pub fn snapshot(&self) -> BufferStateSnapshot {
        BufferStateSnapshot {
            committed_bytes: self.committed_bytes(),
            committed_ops: self.committed_ops(),
            reserved_bytes: self.reserved_bytes(),
            reserved_ops: self.reserved_ops(),
            active_guards: self.active_guards(),
        }
    }

    // ==================== Modification Operations ====================

    /// Increments the active guards counter.
    ///
    /// Called when a new `ReservationGuard` is created.
    #[inline]
    #[allow(dead_code)] // Used by ReservationGuard::new (Step 4) and AdmissionController (Step 12)
    pub(crate) fn increment_active_guards(&self) {
        self.active_guards.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrements the active guards counter.
    ///
    /// Called when a `ReservationGuard` is dropped.
    ///
    /// # Panics
    ///
    /// Panics if the counter would underflow (FR-64: underflow protection).
    #[inline]
    pub(crate) fn decrement_active_guards(&self) {
        let prev = self.active_guards.fetch_sub(1, Ordering::AcqRel);
        assert!(prev > 0, "active_guards underflow: decrement from 0");
    }

    /// Adds to reserved counters.
    ///
    /// Called during reservation acquisition by `AdmissionController`.
    #[inline]
    #[allow(dead_code)] // Used by AdmissionController (Step 12)
    pub(crate) fn add_reserved(&self, bytes: usize, ops: usize) {
        self.reserved_bytes.fetch_add(bytes, Ordering::AcqRel);
        self.reserved_ops.fetch_add(ops, Ordering::AcqRel);
    }

    /// Subtracts from reserved counters.
    ///
    /// Called during reservation abort.
    ///
    /// # Panics
    ///
    /// Panics if the counters would underflow (FR-64: underflow protection).
    #[inline]
    pub(crate) fn sub_reserved(&self, bytes: usize, ops: usize) {
        let prev_bytes = self.reserved_bytes.fetch_sub(bytes, Ordering::AcqRel);
        let prev_ops = self.reserved_ops.fetch_sub(ops, Ordering::AcqRel);
        assert!(
            prev_bytes >= bytes,
            "reserved_bytes underflow: {prev_bytes} < {bytes}"
        );
        assert!(
            prev_ops >= ops,
            "reserved_ops underflow: {prev_ops} < {ops}"
        );
    }

    /// Transfers from reserved to committed.
    ///
    /// Called during reservation commit.
    ///
    /// # Arguments
    ///
    /// * `reserved_bytes` - Total bytes that were reserved
    /// * `reserved_ops` - Total ops that were reserved
    /// * `actual_bytes` - Actual bytes used (≤ reserved)
    /// * `actual_ops` - Actual ops used (≤ reserved)
    ///
    /// # Panics
    ///
    /// Panics if actual exceeds reserved or on underflow (FR-64, FR-66).
    pub(crate) fn transfer_reserved_to_committed(
        &self,
        reserved_bytes: usize,
        reserved_ops: usize,
        actual_bytes: usize,
        actual_ops: usize,
    ) {
        assert!(
            actual_bytes <= reserved_bytes,
            "actual_bytes {actual_bytes} exceeds reserved_bytes {reserved_bytes}"
        );
        assert!(
            actual_ops <= reserved_ops,
            "actual_ops {actual_ops} exceeds reserved_ops {reserved_ops}"
        );

        // Release FULL reservation (FR-66)
        let prev_bytes = self
            .reserved_bytes
            .fetch_sub(reserved_bytes, Ordering::AcqRel);
        let prev_ops = self.reserved_ops.fetch_sub(reserved_ops, Ordering::AcqRel);
        assert!(
            prev_bytes >= reserved_bytes,
            "reserved_bytes underflow: {prev_bytes} < {reserved_bytes}"
        );
        assert!(
            prev_ops >= reserved_ops,
            "reserved_ops underflow: {prev_ops} < {reserved_ops}"
        );

        // Add only actual usage to committed
        self.committed_bytes
            .fetch_add(actual_bytes, Ordering::AcqRel);
        self.committed_ops.fetch_add(actual_ops, Ordering::AcqRel);
    }

    /// Subtracts from committed counters.
    ///
    /// Called during compaction to reflect cleared delta buffer.
    ///
    /// # Panics
    ///
    /// Panics (not just debug) if the counters would underflow,
    /// as this indicates a serious invariant violation.
    pub fn sub_committed(&self, bytes: usize, ops: usize) {
        let prev_bytes = self.committed_bytes.fetch_sub(bytes, Ordering::AcqRel);
        let prev_ops = self.committed_ops.fetch_sub(ops, Ordering::AcqRel);
        assert!(
            prev_bytes >= bytes,
            "committed_bytes underflow: {prev_bytes} < {bytes}"
        );
        assert!(
            prev_ops >= ops,
            "committed_ops underflow: {prev_ops} < {ops}"
        );
    }

    // ==================== Reset Operations ====================

    /// Safe counter reset - asserts no active guards.
    ///
    /// This is used during checkpoint restoration. All reservation guards
    /// must be dropped before calling this method.
    ///
    /// # Arguments
    ///
    /// * `committed_bytes` - New committed bytes value
    /// * `committed_ops` - New committed ops value
    /// * `reserved_bytes` - New reserved bytes value
    /// * `reserved_ops` - New reserved ops value
    ///
    /// # Panics
    ///
    /// Panics if `active_guards > 0`. This is a programming error - all
    /// reservations must be completed before counter reset.
    pub fn reset_counters(
        &self,
        committed_bytes: usize,
        committed_ops: usize,
        reserved_bytes: usize,
        reserved_ops: usize,
    ) {
        let active = self.active_guards.load(Ordering::Acquire);
        assert_eq!(
            active, 0,
            "Cannot reset counters with {active} active guards"
        );

        self.committed_bytes
            .store(committed_bytes, Ordering::Release);
        self.committed_ops.store(committed_ops, Ordering::Release);
        self.reserved_bytes.store(reserved_bytes, Ordering::Release);
        self.reserved_ops.store(reserved_ops, Ordering::Release);
    }

    /// Resets all counters to zero.
    ///
    /// # Panics
    ///
    /// Panics if `active_guards > 0`.
    pub fn reset_to_zero(&self) {
        self.reset_counters(0, 0, 0, 0);
    }

    /// Attempts to reset all counters to zero, returning an error if active guards exist.
    ///
    /// This is the non-panicking version of `reset_to_zero()`.
    ///
    /// # Returns
    ///
    /// `Ok(())` if counters were successfully reset, or `Err(active_count)` if
    /// there are active reservation guards preventing the reset.
    ///
    /// # Errors
    ///
    /// Returns `Err(active_count)` when active reservation guards are present.
    pub fn try_reset_to_zero(&self) -> Result<(), usize> {
        let active = self.active_guards.load(Ordering::Acquire);
        if active > 0 {
            return Err(active);
        }

        self.committed_bytes.store(0, Ordering::Release);
        self.committed_ops.store(0, Ordering::Release);
        self.reserved_bytes.store(0, Ordering::Release);
        self.reserved_ops.store(0, Ordering::Release);
        Ok(())
    }
}

impl Default for SharedBufferState {
    fn default() -> Self {
        Self::new()
    }
}

/// Immutable snapshot of buffer state for monitoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferStateSnapshot {
    /// Committed bytes at snapshot time.
    pub committed_bytes: usize,
    /// Committed operations at snapshot time.
    pub committed_ops: usize,
    /// Reserved bytes at snapshot time.
    pub reserved_bytes: usize,
    /// Reserved operations at snapshot time.
    pub reserved_ops: usize,
    /// Active guards at snapshot time.
    pub active_guards: usize,
}

impl BufferStateSnapshot {
    /// Total bytes (committed + reserved).
    #[inline]
    #[must_use]
    pub const fn total_bytes(&self) -> usize {
        self.committed_bytes + self.reserved_bytes
    }

    /// Total operations (committed + reserved).
    #[inline]
    #[must_use]
    pub const fn total_ops(&self) -> usize {
        self.committed_ops + self.reserved_ops
    }
}

impl fmt::Display for BufferStateSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "committed: {} bytes/{} ops, reserved: {} bytes/{} ops, guards: {}",
            self.committed_bytes,
            self.committed_ops,
            self.reserved_bytes,
            self.reserved_ops,
            self.active_guards
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state() {
        let state = SharedBufferState::new();
        assert_eq!(state.committed_bytes(), 0);
        assert_eq!(state.committed_ops(), 0);
        assert_eq!(state.reserved_bytes(), 0);
        assert_eq!(state.reserved_ops(), 0);
        assert_eq!(state.active_guards(), 0);
    }

    #[test]
    fn test_default() {
        let state: SharedBufferState = SharedBufferState::default();
        assert_eq!(state.committed_bytes(), 0);
    }

    #[test]
    fn test_add_reserved() {
        let state = SharedBufferState::new();
        state.add_reserved(100, 5);
        assert_eq!(state.reserved_bytes(), 100);
        assert_eq!(state.reserved_ops(), 5);
        assert_eq!(state.committed_bytes(), 0);
    }

    #[test]
    fn test_sub_reserved() {
        let state = SharedBufferState::new();
        state.add_reserved(100, 5);
        state.sub_reserved(50, 2);
        assert_eq!(state.reserved_bytes(), 50);
        assert_eq!(state.reserved_ops(), 3);
    }

    #[test]
    fn test_transfer_reserved_to_committed() {
        let state = SharedBufferState::new();
        state.add_reserved(100, 5);

        // Transfer with exact usage
        state.transfer_reserved_to_committed(100, 5, 80, 4);

        // Full reservation released, only actual committed
        assert_eq!(state.reserved_bytes(), 0);
        assert_eq!(state.reserved_ops(), 0);
        assert_eq!(state.committed_bytes(), 80);
        assert_eq!(state.committed_ops(), 4);
    }

    #[test]
    fn test_sub_committed() {
        let state = SharedBufferState::new();
        state.add_reserved(100, 5);
        state.transfer_reserved_to_committed(100, 5, 100, 5);
        assert_eq!(state.committed_bytes(), 100);

        state.sub_committed(60, 3);
        assert_eq!(state.committed_bytes(), 40);
        assert_eq!(state.committed_ops(), 2);
    }

    #[test]
    fn test_active_guards() {
        let state = SharedBufferState::new();
        assert_eq!(state.active_guards(), 0);

        state.increment_active_guards();
        assert_eq!(state.active_guards(), 1);

        state.increment_active_guards();
        assert_eq!(state.active_guards(), 2);

        state.decrement_active_guards();
        assert_eq!(state.active_guards(), 1);

        state.decrement_active_guards();
        assert_eq!(state.active_guards(), 0);
    }

    #[test]
    fn test_total_bytes_ops() {
        let state = SharedBufferState::new();
        state.add_reserved(100, 5);
        state.transfer_reserved_to_committed(50, 2, 50, 2);

        assert_eq!(state.total_bytes(), 50 + 50); // committed + remaining reserved
        assert_eq!(state.total_ops(), 2 + 3);
    }

    #[test]
    fn test_snapshot() {
        let state = SharedBufferState::new();
        state.add_reserved(100, 5);
        state.increment_active_guards();

        let snap = state.snapshot();
        assert_eq!(snap.reserved_bytes, 100);
        assert_eq!(snap.reserved_ops, 5);
        assert_eq!(snap.committed_bytes, 0);
        assert_eq!(snap.active_guards, 1);
        assert_eq!(snap.total_bytes(), 100);
    }

    #[test]
    fn test_reset_counters() {
        let state = SharedBufferState::new();
        state.add_reserved(100, 5);
        state.transfer_reserved_to_committed(100, 5, 100, 5);

        // Reset to specific values
        state.reset_counters(50, 3, 20, 1);
        assert_eq!(state.committed_bytes(), 50);
        assert_eq!(state.committed_ops(), 3);
        assert_eq!(state.reserved_bytes(), 20);
        assert_eq!(state.reserved_ops(), 1);
    }

    #[test]
    fn test_reset_to_zero() {
        let state = SharedBufferState::new();
        state.add_reserved(100, 5);
        state.transfer_reserved_to_committed(100, 5, 100, 5);

        state.reset_to_zero();
        assert_eq!(state.committed_bytes(), 0);
        assert_eq!(state.committed_ops(), 0);
        assert_eq!(state.reserved_bytes(), 0);
        assert_eq!(state.reserved_ops(), 0);
    }

    #[test]
    #[should_panic(expected = "Cannot reset counters with 1 active guards")]
    fn test_reset_with_active_guards_panics() {
        let state = SharedBufferState::new();
        state.increment_active_guards();
        state.reset_to_zero(); // Should panic
    }

    #[test]
    #[should_panic(expected = "committed_bytes underflow")]
    fn test_sub_committed_underflow_panics() {
        let state = SharedBufferState::new();
        state.sub_committed(100, 0); // Nothing committed, should panic
    }

    #[test]
    fn test_snapshot_display() {
        let snap = BufferStateSnapshot {
            committed_bytes: 100,
            committed_ops: 5,
            reserved_bytes: 50,
            reserved_ops: 2,
            active_guards: 1,
        };

        let display = format!("{snap}");
        assert!(display.contains("committed: 100 bytes/5 ops"));
        assert!(display.contains("reserved: 50 bytes/2 ops"));
        assert!(display.contains("guards: 1"));
    }

    #[test]
    fn test_concurrent_add_reserved() {
        use std::sync::Arc;
        use std::thread;

        let state = Arc::new(SharedBufferState::new());
        let mut handles = vec![];

        // Spawn 10 threads, each adding 100 bytes and 1 op
        for _ in 0..10 {
            let state = Arc::clone(&state);
            handles.push(thread::spawn(move || {
                state.add_reserved(100, 1);
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(state.reserved_bytes(), 1000);
        assert_eq!(state.reserved_ops(), 10);
    }
}
