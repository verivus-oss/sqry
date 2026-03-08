//! `AdmissionController`: Reservation-based admission control for delta buffers.
//!
//! This module implements `AdmissionController`, which provides back-pressure
//! management for the delta buffer using atomic CAS operations.
//!
//! # Design
//!
//! - **Reservation-based**: Writers must acquire a reservation before writing
//! - **Dual CAS loops**: Atomic reservation of both bytes and ops
//! - **Compensating rollback**: CAS-based rollback prevents counter corruption
//!
//! # Thread Safety
//!
//! All operations use atomic CAS loops for lock-free concurrent access.
//! The compensating rollback ensures counter integrity even under contention.
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::admission::{AdmissionController, SharedBufferState};
//!
//! let state = Arc::new(SharedBufferState::new());
//! let controller = AdmissionController::new(
//!     Arc::clone(&state),
//!     1024 * 1024, // 1MB max bytes
//!     10_000,      // 10k max ops
//! );
//!
//! let guard = controller.try_reserve(100, 1)?;
//! // ... write edges ...
//! guard.commit();
//! ```

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;

fn usize_to_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

use super::reservation::{AdmissionError, Reservation, ReservationGuard};
use super::state::SharedBufferState;

/// Admission controller for back-pressure management.
///
/// Controls access to the delta buffer by requiring reservations before writes.
/// Uses atomic CAS operations for lock-free concurrent access.
///
/// # Limits
///
/// The controller enforces two limits:
/// - `max_bytes`: Maximum total bytes (committed + reserved)
/// - `max_ops`: Maximum total operations (committed + reserved)
///
/// # Reservation Flow
///
/// 1. Writer calls `try_reserve(bytes, ops)`
/// 2. Controller atomically reserves both counters via dual CAS loops
/// 3. On success, returns `ReservationGuard` for RAII cleanup
/// 4. Writer performs delta buffer operations
/// 5. Guard is committed (transfers to committed) or aborted (releases)
#[derive(Debug)]
pub struct AdmissionController {
    /// Shared state with atomic counters.
    buffer_state: Arc<SharedBufferState>,
    /// Maximum bytes (committed + reserved).
    max_bytes: usize,
    /// Maximum operations (committed + reserved).
    max_ops: usize,
}

impl AdmissionController {
    /// Creates a new admission controller with the given limits.
    ///
    /// # Arguments
    ///
    /// * `buffer_state` - Shared state with atomic counters
    /// * `max_bytes` - Maximum total bytes (committed + reserved)
    /// * `max_ops` - Maximum total operations (committed + reserved)
    #[must_use]
    pub fn new(buffer_state: Arc<SharedBufferState>, max_bytes: usize, max_ops: usize) -> Self {
        Self {
            buffer_state,
            max_bytes,
            max_ops,
        }
    }

    /// Returns the shared buffer state.
    #[must_use]
    pub fn buffer_state(&self) -> &Arc<SharedBufferState> {
        &self.buffer_state
    }

    /// Returns the maximum bytes limit.
    #[must_use]
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Returns the maximum operations limit.
    #[must_use]
    pub fn max_ops(&self) -> usize {
        self.max_ops
    }

    /// Attempts to reserve capacity for a write operation.
    ///
    /// This method atomically reserves both bytes and ops using dual CAS loops.
    /// If the ops reservation fails after bytes succeeds, compensating CAS
    /// rollback ensures no counter corruption.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Number of bytes to reserve
    /// * `ops` - Number of operations to reserve
    ///
    /// # Returns
    ///
    /// * `Ok(ReservationGuard)` - Reservation acquired successfully
    /// * `Err(AdmissionError)` - Limit exceeded or zero reservation
    ///
    /// # Errors
    ///
    /// Returns `AdmissionError::ByteLimitExceeded` if `committed + reserved + bytes > max_bytes`.
    /// Returns `AdmissionError::OpsLimitExceeded` if `committed + reserved + ops > max_ops`.
    /// Returns `AdmissionError::ZeroReservation` if both bytes and ops are zero.
    pub fn try_reserve(
        &self,
        bytes: usize,
        ops: usize,
    ) -> Result<ReservationGuard, AdmissionError> {
        // Reject zero reservations
        if bytes == 0 && ops == 0 {
            return Err(AdmissionError::ZeroReservation);
        }

        // Phase 1: CAS loop for bytes
        self.try_reserve_bytes(bytes)?;

        // Phase 2: CAS loop for ops (, with compensating rollback )
        self.try_reserve_ops(ops, bytes)?;

        // Note: ReservationGuard::new() increments active_guards internally
        Ok(ReservationGuard::new(
            Arc::clone(&self.buffer_state),
            Reservation { bytes, ops },
        ))
    }

    fn try_reserve_bytes(&self, bytes: usize) -> Result<(), AdmissionError> {
        if bytes == 0 {
            return Ok(());
        }

        loop {
            let (current_reserved, committed) = self.load_bytes_snapshot();
            self.check_byte_limit(committed, current_reserved, bytes)?;

            if self.try_update_reserved_bytes(current_reserved, bytes) {
                return Ok(());
            }
            // CAS failed due to contention, retry
        }
    }

    fn try_reserve_ops(&self, ops: usize, bytes: usize) -> Result<(), AdmissionError> {
        if ops == 0 {
            return Ok(());
        }

        loop {
            let (current_reserved, committed) = self.load_ops_snapshot();
            if let Err(err) = self.check_ops_limit(committed, current_reserved, ops) {
                self.rollback_bytes_if_needed(bytes);
                return Err(err);
            }

            if self.try_update_reserved_ops(current_reserved, ops) {
                return Ok(());
            }
            // CAS failed due to contention, retry
        }
    }

    fn load_bytes_snapshot(&self) -> (usize, usize) {
        let reserved = self.buffer_state.reserved_bytes.load(Ordering::Acquire);
        let committed = self.buffer_state.committed_bytes.load(Ordering::Acquire);
        (reserved, committed)
    }

    fn load_ops_snapshot(&self) -> (usize, usize) {
        let reserved = self.buffer_state.reserved_ops.load(Ordering::Acquire);
        let committed = self.buffer_state.committed_ops.load(Ordering::Acquire);
        (reserved, committed)
    }

    fn check_byte_limit(
        &self,
        committed: usize,
        reserved: usize,
        requested: usize,
    ) -> Result<(), AdmissionError> {
        let new_total = committed.saturating_add(reserved).saturating_add(requested);
        if new_total > self.max_bytes {
            return Err(AdmissionError::ByteLimitExceeded {
                requested,
                available: self
                    .max_bytes
                    .saturating_sub(committed.saturating_add(reserved)),
                max: self.max_bytes,
            });
        }
        Ok(())
    }

    fn check_ops_limit(
        &self,
        committed: usize,
        reserved: usize,
        requested: usize,
    ) -> Result<(), AdmissionError> {
        let new_total = committed.saturating_add(reserved).saturating_add(requested);
        if new_total > self.max_ops {
            return Err(AdmissionError::OpsLimitExceeded {
                requested,
                available: self
                    .max_ops
                    .saturating_sub(committed.saturating_add(reserved)),
                max: self.max_ops,
            });
        }
        Ok(())
    }

    fn try_update_reserved_bytes(&self, current_reserved: usize, bytes: usize) -> bool {
        self.buffer_state
            .reserved_bytes
            .compare_exchange_weak(
                current_reserved,
                current_reserved + bytes,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    fn try_update_reserved_ops(&self, current_reserved: usize, ops: usize) -> bool {
        self.buffer_state
            .reserved_ops
            .compare_exchange_weak(
                current_reserved,
                current_reserved + ops,
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    fn rollback_bytes_if_needed(&self, bytes: usize) {
        if bytes > 0 {
            self.compensating_rollback_bytes(bytes);
        }
    }

    /// Compensating CAS rollback for bytes counter.
    ///
    /// Called when ops reservation fails after bytes was successfully reserved.
    /// Uses CAS to ensure we only decrement what we added, preventing corruption
    /// under concurrent modifications.
    ///
    /// # Why CAS Instead of `fetch_sub`
    ///
    /// With `fetch_sub`, if the bytes counter was modified between our increment
    /// and the ops failure (e.g., by compaction), we might:
    /// 1. Underflow (if decremented by compaction)
    /// 2. Corrupt another reservation's state
    ///
    /// Compensating CAS verifies our increment is still present before decrementing.
    fn compensating_rollback_bytes(&self, bytes: usize) {
        loop {
            let current = self.buffer_state.reserved_bytes.load(Ordering::Acquire);
            if current >= bytes {
                if self
                    .buffer_state
                    .reserved_bytes
                    .compare_exchange_weak(
                        current,
                        current - bytes,
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    )
                    .is_ok()
                {
                    return;
                }
                // CAS failed, retry
            } else {
                // Our increment was already consumed (by another rollback or compaction)
                // This is a rare race condition, but safe to exit
                return;
            }
        }
    }

    /// Returns the current utilization as a percentage (0.0 to 1.0).
    ///
    /// Returns the maximum of bytes and ops utilization.
    #[must_use]
    pub fn utilization(&self) -> f64 {
        let bytes_util = if self.max_bytes > 0 {
            usize_to_f64(self.buffer_state.total_bytes()) / usize_to_f64(self.max_bytes)
        } else {
            0.0
        };

        let ops_util = if self.max_ops > 0 {
            usize_to_f64(self.buffer_state.total_ops()) / usize_to_f64(self.max_ops)
        } else {
            0.0
        };

        bytes_util.max(ops_util)
    }

    /// Returns true if utilization exceeds the soft limit (80%).
    #[must_use]
    pub fn is_near_limit(&self) -> bool {
        self.utilization() >= 0.8
    }

    /// Returns statistics about the controller.
    #[must_use]
    pub fn stats(&self) -> AdmissionControllerStats {
        AdmissionControllerStats {
            max_bytes: self.max_bytes,
            max_ops: self.max_ops,
            committed_bytes: self.buffer_state.committed_bytes(),
            committed_ops: self.buffer_state.committed_ops(),
            reserved_bytes: self.buffer_state.reserved_bytes(),
            reserved_ops: self.buffer_state.reserved_ops(),
            active_guards: self.buffer_state.active_guards(),
        }
    }
}

/// Statistics for an admission controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionControllerStats {
    /// Maximum bytes limit.
    pub max_bytes: usize,
    /// Maximum operations limit.
    pub max_ops: usize,
    /// Current committed bytes.
    pub committed_bytes: usize,
    /// Current committed operations.
    pub committed_ops: usize,
    /// Current reserved bytes.
    pub reserved_bytes: usize,
    /// Current reserved operations.
    pub reserved_ops: usize,
    /// Number of active reservation guards.
    pub active_guards: usize,
}

impl AdmissionControllerStats {
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

    /// Available bytes before limit.
    #[inline]
    #[must_use]
    pub const fn available_bytes(&self) -> usize {
        self.max_bytes.saturating_sub(self.total_bytes())
    }

    /// Available operations before limit.
    #[inline]
    #[must_use]
    pub const fn available_ops(&self) -> usize {
        self.max_ops.saturating_sub(self.total_ops())
    }
}

impl fmt::Display for AdmissionControllerStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "bytes: {}/{} ({}% used), ops: {}/{} ({}% used), guards: {}",
            self.total_bytes(),
            self.max_bytes,
            if self.max_bytes > 0 {
                (self.total_bytes() * 100) / self.max_bytes
            } else {
                0
            },
            self.total_ops(),
            self.max_ops,
            if self.max_ops > 0 {
                (self.total_ops() * 100) / self.max_ops
            } else {
                0
            },
            self.active_guards
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_controller(max_bytes: usize, max_ops: usize) -> AdmissionController {
        let state = Arc::new(SharedBufferState::new());
        AdmissionController::new(state, max_bytes, max_ops)
    }

    #[test]
    fn test_new() {
        let state = Arc::new(SharedBufferState::new());
        let controller = AdmissionController::new(Arc::clone(&state), 1000, 100);
        assert_eq!(controller.max_bytes(), 1000);
        assert_eq!(controller.max_ops(), 100);
    }

    #[test]
    fn test_try_reserve_success() {
        let controller = make_controller(1000, 100);
        let guard = controller.try_reserve(100, 1).expect("should succeed");

        assert_eq!(controller.buffer_state().reserved_bytes(), 100);
        assert_eq!(controller.buffer_state().reserved_ops(), 1);
        assert_eq!(controller.buffer_state().active_guards(), 1);

        let _ = guard.abort();
        assert_eq!(controller.buffer_state().reserved_bytes(), 0);
        assert_eq!(controller.buffer_state().reserved_ops(), 0);
        assert_eq!(controller.buffer_state().active_guards(), 0);
    }

    #[test]
    fn test_try_reserve_zero_rejected() {
        let controller = make_controller(1000, 100);
        let result = controller.try_reserve(0, 0);
        assert!(matches!(result, Err(AdmissionError::ZeroReservation)));
    }

    #[test]
    fn test_try_reserve_bytes_only() {
        let controller = make_controller(1000, 100);
        let guard = controller.try_reserve(100, 0).expect("should succeed");

        assert_eq!(controller.buffer_state().reserved_bytes(), 100);
        assert_eq!(controller.buffer_state().reserved_ops(), 0);

        let _ = guard.abort();
    }

    #[test]
    fn test_try_reserve_ops_only() {
        let controller = make_controller(1000, 100);
        let guard = controller.try_reserve(0, 10).expect("should succeed");

        assert_eq!(controller.buffer_state().reserved_bytes(), 0);
        assert_eq!(controller.buffer_state().reserved_ops(), 10);

        let _ = guard.abort();
    }

    #[test]
    fn test_byte_limit_exceeded() {
        let controller = make_controller(100, 100);
        let result = controller.try_reserve(150, 1);

        match result {
            Err(AdmissionError::ByteLimitExceeded {
                requested,
                available,
                max,
            }) => {
                assert_eq!(requested, 150);
                assert_eq!(available, 100);
                assert_eq!(max, 100);
            }
            _ => panic!("Expected ByteLimitExceeded"),
        }
    }

    #[test]
    fn test_ops_limit_exceeded() {
        let controller = make_controller(1000, 10);
        let result = controller.try_reserve(100, 20);

        match result {
            Err(AdmissionError::OpsLimitExceeded {
                requested,
                available,
                max,
            }) => {
                assert_eq!(requested, 20);
                assert_eq!(available, 10);
                assert_eq!(max, 10);
            }
            _ => panic!("Expected OpsLimitExceeded"),
        }

        // Verify compensating rollback worked - bytes should be 0
        assert_eq!(controller.buffer_state().reserved_bytes(), 0);
    }

    #[test]
    fn test_multiple_reservations() {
        let controller = make_controller(1000, 100);

        let guard1 = controller.try_reserve(100, 5).unwrap();
        let guard2 = controller.try_reserve(200, 10).unwrap();
        let guard3 = controller.try_reserve(300, 15).unwrap();

        assert_eq!(controller.buffer_state().reserved_bytes(), 600);
        assert_eq!(controller.buffer_state().reserved_ops(), 30);
        assert_eq!(controller.buffer_state().active_guards(), 3);

        let _ = guard1.abort();
        assert_eq!(controller.buffer_state().reserved_bytes(), 500);
        assert_eq!(controller.buffer_state().active_guards(), 2);

        let _ = guard2.commit();
        assert_eq!(controller.buffer_state().reserved_bytes(), 300);
        assert_eq!(controller.buffer_state().committed_bytes(), 200);
        assert_eq!(controller.buffer_state().active_guards(), 1);

        let _ = guard3.abort();
        assert_eq!(controller.buffer_state().reserved_bytes(), 0);
        assert_eq!(controller.buffer_state().committed_bytes(), 200);
        assert_eq!(controller.buffer_state().active_guards(), 0);
    }

    #[test]
    fn test_commit_with_actual() {
        let controller = make_controller(1000, 100);
        let guard = controller.try_reserve(100, 10).unwrap();

        // Commit with less than reserved
        guard.commit_with_actual(50, 5).unwrap();

        assert_eq!(controller.buffer_state().reserved_bytes(), 0);
        assert_eq!(controller.buffer_state().committed_bytes(), 50);
        assert_eq!(controller.buffer_state().committed_ops(), 5);
    }

    #[test]
    fn test_limit_boundary() {
        let controller = make_controller(100, 10);

        // Exactly at limit should succeed
        let guard = controller.try_reserve(100, 10).unwrap();
        assert_eq!(controller.buffer_state().reserved_bytes(), 100);
        assert_eq!(controller.buffer_state().reserved_ops(), 10);

        // Any more should fail
        let result = controller.try_reserve(1, 1);
        assert!(result.is_err());

        let _ = guard.abort();
    }

    #[test]
    fn test_utilization() {
        let controller = make_controller(100, 100);
        assert!(controller.utilization().abs() < f64::EPSILON);

        let guard = controller.try_reserve(50, 25).unwrap();
        // Max of (50/100, 25/100) = 0.5
        assert!((controller.utilization() - 0.5).abs() < 0.01);
        assert!(!controller.is_near_limit());

        let _ = guard.abort();

        let guard2 = controller.try_reserve(80, 10).unwrap();
        // Max of (80/100, 10/100) = 0.8
        assert!((controller.utilization() - 0.8).abs() < 0.01);
        assert!(controller.is_near_limit());

        let _ = guard2.abort();
    }

    #[test]
    fn test_stats() {
        let controller = make_controller(1000, 100);
        let guard = controller.try_reserve(200, 20).unwrap();
        guard.commit_with_actual(150, 15).unwrap();

        let stats = controller.stats();
        assert_eq!(stats.max_bytes, 1000);
        assert_eq!(stats.max_ops, 100);
        assert_eq!(stats.committed_bytes, 150);
        assert_eq!(stats.committed_ops, 15);
        assert_eq!(stats.reserved_bytes, 0);
        assert_eq!(stats.reserved_ops, 0);
        assert_eq!(stats.total_bytes(), 150);
        assert_eq!(stats.available_bytes(), 850);
    }

    #[test]
    fn test_stats_display() {
        let stats = AdmissionControllerStats {
            max_bytes: 1000,
            max_ops: 100,
            committed_bytes: 100,
            committed_ops: 10,
            reserved_bytes: 50,
            reserved_ops: 5,
            active_guards: 1,
        };

        let display = format!("{stats}");
        assert!(display.contains("bytes: 150/1000"));
        assert!(display.contains("ops: 15/100"));
        assert!(display.contains("guards: 1"));
    }

    #[test]
    fn test_concurrent_reservations() {
        use std::thread;

        let state = Arc::new(SharedBufferState::new());
        let controller = Arc::new(AdmissionController::new(Arc::clone(&state), 10_000, 1_000));

        let mut handles = vec![];

        // Spawn 10 threads, each making 10 reservations
        for _ in 0..10 {
            let controller = Arc::clone(&controller);
            handles.push(thread::spawn(move || {
                for _ in 0..10 {
                    if let Ok(guard) = controller.try_reserve(100, 1) {
                        // Simulate some work
                        std::thread::yield_now();
                        let _ = guard.commit();
                    } else {
                        // Limit exceeded, acceptable under contention
                    }
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        // All guards should be cleaned up
        assert_eq!(state.active_guards(), 0);
        assert_eq!(state.reserved_bytes(), 0);
        assert_eq!(state.reserved_ops(), 0);
    }

    #[test]
    fn test_compensating_rollback_under_contention() {
        // This test verifies that compensating rollback works correctly
        // when ops limit is exceeded after bytes reservation succeeds

        let state = Arc::new(SharedBufferState::new());
        // Set up: bytes limit high, ops limit low
        let controller = AdmissionController::new(Arc::clone(&state), 10_000, 1);

        // First reservation takes the only op
        let guard1 = controller.try_reserve(100, 1).unwrap();

        // Second reservation should fail on ops, but bytes was already incremented
        // Compensating rollback should clean up the bytes increment
        let result = controller.try_reserve(100, 1);
        assert!(matches!(
            result,
            Err(AdmissionError::OpsLimitExceeded { .. })
        ));

        // Verify bytes were rolled back
        assert_eq!(state.reserved_bytes(), 100); // Only guard1's reservation
        assert_eq!(state.reserved_ops(), 1);

        let _ = guard1.abort();
        assert_eq!(state.reserved_bytes(), 0);
        assert_eq!(state.reserved_ops(), 0);
    }
}
