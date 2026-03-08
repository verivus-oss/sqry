//! Reservation and `ReservationGuard`: RAII-based admission control.
//!
//! This module implements the reservation system for back-pressure control:
//! - `Reservation`: A granted allocation of bytes and operations
//! - `ReservationGuard`: RAII wrapper ensuring proper commit/abort
//!
//! # Design
//!
//! The reservation system ensures that:
//! - Reservations cannot leak (auto-abort on drop)
//! - Actual usage is validated against reserved amounts
//! - Full reservation is released on commit (not just unused portion)
//!
//! # Usage Pattern
//!
//! ```rust,ignore
//! let guard = controller.try_reserve(100, 1)?;
//!
//! // Do work...
//! let actual_bytes = write_edge(&edge);
//!
//! // Commit with actual usage
//! guard.commit_with_actual(actual_bytes, 1)?;
//!
//! // Or abort (also happens automatically on drop)
//! // guard.abort();
//! ```

use std::fmt;
use std::sync::Arc;

use super::state::SharedBufferState;

/// Error returned when a reservation cannot be granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionError {
    /// Byte limit would be exceeded.
    ByteLimitExceeded {
        /// Requested bytes
        requested: usize,
        /// Available bytes before limit
        available: usize,
        /// Maximum allowed
        max: usize,
    },
    /// Operation limit would be exceeded.
    OpsLimitExceeded {
        /// Requested operations
        requested: usize,
        /// Available operations before limit
        available: usize,
        /// Maximum allowed
        max: usize,
    },
    /// Zero reservation not allowed.
    ZeroReservation,
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ByteLimitExceeded {
                requested,
                available,
                max,
            } => {
                write!(
                    f,
                    "byte limit exceeded: requested {requested} but only {available} available (max {max})"
                )
            }
            Self::OpsLimitExceeded {
                requested,
                available,
                max,
            } => {
                write!(
                    f,
                    "ops limit exceeded: requested {requested} but only {available} available (max {max})"
                )
            }
            Self::ZeroReservation => {
                write!(f, "zero reservation not allowed: bytes and ops both zero")
            }
        }
    }
}

impl std::error::Error for AdmissionError {}

/// Error returned when a commit operation fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitError {
    /// The guard was already consumed (double commit/abort).
    AlreadyConsumed,
    /// Actual usage exceeds reserved amount.
    ActualExceedsReservation {
        /// Actual bytes used
        actual_bytes: usize,
        /// Reserved bytes
        reserved_bytes: usize,
        /// Actual operations used
        actual_ops: usize,
        /// Reserved operations
        reserved_ops: usize,
    },
}

impl fmt::Display for CommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyConsumed => {
                write!(f, "reservation already consumed")
            }
            Self::ActualExceedsReservation {
                actual_bytes,
                reserved_bytes,
                actual_ops,
                reserved_ops,
            } => {
                write!(
                    f,
                    "actual exceeds reservation: {actual_bytes} bytes > {reserved_bytes} reserved, {actual_ops} ops > {reserved_ops} reserved"
                )
            }
        }
    }
}

impl std::error::Error for CommitError {}

/// A granted reservation of bytes and operations.
///
/// This is the raw reservation data. In practice, you should use
/// `ReservationGuard` which provides RAII semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    /// Reserved bytes
    pub bytes: usize,
    /// Reserved operations
    pub ops: usize,
}

impl Reservation {
    /// Creates a new reservation.
    ///
    /// # Safety Note
    ///
    /// This constructor is crate-private. Only `AdmissionController` should
    /// create reservations, as it ensures the corresponding counters are
    /// updated atomically.
    #[inline]
    #[must_use]
    #[allow(dead_code)] // Used by AdmissionController (Step 12) and tests
    pub(crate) const fn new(bytes: usize, ops: usize) -> Self {
        Self { bytes, ops }
    }
}

/// RAII guard that auto-aborts reservation on drop.
///
/// `ReservationGuard` ensures that no reservation can leak even on early
/// return or panic. The guard must be explicitly consumed via one of:
/// - `commit()` - Commit using full reserved amount
/// - `commit_with_actual()` - Commit using actual (possibly smaller) amount
/// - `abort()` - Explicitly abort the reservation
///
/// If the guard is dropped without being consumed, it automatically aborts.
///
/// # Design
///
/// The guard tracks active reservations via `SharedBufferState::active_guards`
/// to prevent counter reset during active reservations (underflow safety).
///
/// # Example
///
/// ```rust,ignore
/// let guard = controller.try_reserve(100, 1)?;
///
/// // Write data (may use less than reserved)
/// let actual = write_data();
///
/// // Commit with actual usage (releases FULL reservation, commits actual)
/// guard.commit_with_actual(actual, 1)?;
/// ```
pub struct ReservationGuard {
    /// Shared buffer state for counter manipulation
    buffer_state: Arc<SharedBufferState>,
    /// The reservation (None if already consumed)
    reservation: Option<Reservation>,
}

impl ReservationGuard {
    /// Creates a new guard for the given reservation.
    ///
    /// This increments `active_guards` to prevent counter reset.
    ///
    /// # Arguments
    ///
    /// * `buffer_state` - Shared state for counter manipulation
    /// * `reservation` - The granted reservation
    ///
    /// # Safety Note
    ///
    /// This constructor is crate-private. Only `AdmissionController` should
    /// create guards, as it ensures:
    /// 1. The reserved counters are incremented atomically BEFORE the guard is created
    /// 2. The reservation amount matches what was added to counters
    ///
    /// If an external caller constructs a guard directly, the Drop impl will
    /// subtract a reservation that was never added, corrupting the counters.
    #[allow(dead_code)] // Used by AdmissionController (Step 12) and tests
    pub(crate) fn new(buffer_state: Arc<SharedBufferState>, reservation: Reservation) -> Self {
        buffer_state.increment_active_guards();
        Self {
            buffer_state,
            reservation: Some(reservation),
        }
    }

    /// Returns the reservation if not yet consumed.
    #[must_use]
    pub fn reservation(&self) -> Option<Reservation> {
        self.reservation
    }

    /// Returns the reserved bytes.
    #[must_use]
    pub fn reserved_bytes(&self) -> usize {
        self.reservation.map_or(0, |r| r.bytes)
    }

    /// Returns the reserved operations.
    #[must_use]
    pub fn reserved_ops(&self) -> usize {
        self.reservation.map_or(0, |r| r.ops)
    }

    /// Returns `true` if this guard has been consumed.
    #[must_use]
    pub fn is_consumed(&self) -> bool {
        self.reservation.is_none()
    }

    /// Commits the reservation using the full reserved amount.
    ///
    /// This is equivalent to `commit_with_actual(reserved_bytes, reserved_ops)`.
    ///
    /// # Errors
    ///
    /// Returns `CommitError::AlreadyConsumed` if the guard was already consumed.
    pub fn commit(self) -> Result<(), CommitError> {
        let r = self.reservation.ok_or(CommitError::AlreadyConsumed)?;
        self.commit_with_actual(r.bytes, r.ops)
    }

    /// Commits with actual usage, validating against reservation.
    ///
    /// # Arguments
    ///
    /// * `actual_bytes` - Actual bytes used (must be ≤ reserved)
    /// * `actual_ops` - Actual operations used (must be ≤ reserved)
    ///
    /// # Errors
    ///
    /// - `CommitError::AlreadyConsumed` if the guard was already consumed
    /// - `CommitError::ActualExceedsReservation` if actual > reserved
    ///
    /// # Design
    ///
    /// - Validates `actual ≤ reserved` to prevent over-commit
    /// - Releases FULL reservation, not just unused portion
    /// - Commits only actual usage to committed counters
    pub fn commit_with_actual(
        mut self,
        actual_bytes: usize,
        actual_ops: usize,
    ) -> Result<(), CommitError> {
        let r = self
            .reservation
            .take()
            .ok_or(CommitError::AlreadyConsumed)?;

        // Validate actual does not exceed reservation
        if actual_bytes > r.bytes || actual_ops > r.ops {
            // Put the reservation back so Drop doesn't double-subtract
            self.reservation = Some(r);
            return Err(CommitError::ActualExceedsReservation {
                actual_bytes,
                reserved_bytes: r.bytes,
                actual_ops,
                reserved_ops: r.ops,
            });
        }

        // Release FULL reservation, commit only actual
        self.buffer_state
            .transfer_reserved_to_committed(r.bytes, r.ops, actual_bytes, actual_ops);

        Ok(())
    }

    /// Explicitly aborts the reservation.
    ///
    /// This releases the reserved capacity back to the pool without
    /// committing anything.
    ///
    /// # Errors
    ///
    /// Returns `CommitError::AlreadyConsumed` if the guard was already consumed.
    pub fn abort(mut self) -> Result<(), CommitError> {
        let r = self
            .reservation
            .take()
            .ok_or(CommitError::AlreadyConsumed)?;
        self.buffer_state.sub_reserved(r.bytes, r.ops);
        Ok(())
    }

    /// Consumes the guard without committing or aborting.
    ///
    /// This is an internal method for when the reservation is being
    /// transferred to another owner.
    ///
    /// # Safety Note
    ///
    /// The caller is responsible for ensuring the reservation is
    /// properly handled after extraction.
    #[must_use]
    #[allow(dead_code)] // Used by AdmissionController (Step 12)
    pub(crate) fn extract(mut self) -> Option<Reservation> {
        self.reservation.take()
    }
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        // Auto-abort if not consumed
        if let Some(r) = self.reservation.take() {
            // Full underflow protection in all builds (not just debug)
            let prev_bytes = self
                .buffer_state
                .reserved_bytes
                .fetch_sub(r.bytes, std::sync::atomic::Ordering::AcqRel);
            let prev_ops = self
                .buffer_state
                .reserved_ops
                .fetch_sub(r.ops, std::sync::atomic::Ordering::AcqRel);
            assert!(
                prev_bytes >= r.bytes,
                "reserved_bytes underflow in Drop: {} < {}",
                prev_bytes,
                r.bytes
            );
            assert!(
                prev_ops >= r.ops,
                "reserved_ops underflow in Drop: {} < {}",
                prev_ops,
                r.ops
            );
        }

        // Always decrement active guards
        self.buffer_state.decrement_active_guards();
    }
}

impl fmt::Debug for ReservationGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReservationGuard")
            .field("buffer_state", &self.buffer_state)
            .field("reservation", &self.reservation)
            .field("consumed", &self.is_consumed())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state() -> Arc<SharedBufferState> {
        Arc::new(SharedBufferState::new())
    }

    #[test]
    fn test_reservation_creation() {
        let r = Reservation::new(100, 5);
        assert_eq!(r.bytes, 100);
        assert_eq!(r.ops, 5);
    }

    #[test]
    fn test_guard_creation() {
        let state = make_state();
        state.add_reserved(100, 5);

        let guard = ReservationGuard::new(Arc::clone(&state), Reservation::new(100, 5));

        assert_eq!(state.active_guards(), 1);
        assert_eq!(guard.reserved_bytes(), 100);
        assert_eq!(guard.reserved_ops(), 5);
        assert!(!guard.is_consumed());

        // Abort to clean up
        guard.abort().unwrap();
        assert_eq!(state.active_guards(), 0);
    }

    #[test]
    fn test_guard_commit() {
        let state = make_state();
        state.add_reserved(100, 5);

        let guard = ReservationGuard::new(Arc::clone(&state), Reservation::new(100, 5));
        guard.commit().unwrap();

        assert_eq!(state.active_guards(), 0);
        assert_eq!(state.reserved_bytes(), 0);
        assert_eq!(state.committed_bytes(), 100);
    }

    #[test]
    fn test_guard_commit_with_actual() {
        let state = make_state();
        state.add_reserved(100, 5);

        let guard = ReservationGuard::new(Arc::clone(&state), Reservation::new(100, 5));
        guard.commit_with_actual(80, 4).unwrap();

        assert_eq!(state.active_guards(), 0);
        // Full reservation released
        assert_eq!(state.reserved_bytes(), 0);
        assert_eq!(state.reserved_ops(), 0);
        // Only actual committed
        assert_eq!(state.committed_bytes(), 80);
        assert_eq!(state.committed_ops(), 4);
    }

    #[test]
    fn test_guard_commit_with_actual_validation() {
        let state = make_state();
        state.add_reserved(100, 5);

        let guard = ReservationGuard::new(Arc::clone(&state), Reservation::new(100, 5));

        // Try to commit more than reserved - should fail
        let result = guard.commit_with_actual(150, 6);
        assert!(matches!(
            result,
            Err(CommitError::ActualExceedsReservation { .. })
        ));

        // Guard should have aborted on drop, check state
        assert_eq!(state.active_guards(), 0);
        assert_eq!(state.committed_bytes(), 0); // Nothing committed
    }

    #[test]
    fn test_guard_abort() {
        let state = make_state();
        state.add_reserved(100, 5);

        let guard = ReservationGuard::new(Arc::clone(&state), Reservation::new(100, 5));
        guard.abort().unwrap();

        assert_eq!(state.active_guards(), 0);
        assert_eq!(state.reserved_bytes(), 0);
        assert_eq!(state.committed_bytes(), 0);
    }

    #[test]
    fn test_guard_auto_abort_on_drop() {
        let state = make_state();
        state.add_reserved(100, 5);

        {
            let _guard = ReservationGuard::new(Arc::clone(&state), Reservation::new(100, 5));
            // Guard dropped without commit/abort
        }

        // Should have auto-aborted
        assert_eq!(state.active_guards(), 0);
        assert_eq!(state.reserved_bytes(), 0);
        assert_eq!(state.committed_bytes(), 0);
    }

    #[test]
    fn test_guard_already_consumed() {
        let state = make_state();
        state.add_reserved(100, 5);

        let guard = ReservationGuard::new(Arc::clone(&state), Reservation::new(100, 5));

        // First extract
        let mut guard = guard;
        let _extracted = guard.reservation.take();

        // Now commit should fail
        let result = guard.commit();
        assert!(matches!(result, Err(CommitError::AlreadyConsumed)));
    }

    #[test]
    fn test_multiple_guards() {
        let state = make_state();
        state.add_reserved(200, 10);

        let guard1 = ReservationGuard::new(Arc::clone(&state), Reservation::new(100, 5));
        let guard2 = ReservationGuard::new(Arc::clone(&state), Reservation::new(100, 5));

        assert_eq!(state.active_guards(), 2);

        guard1.commit().unwrap();
        assert_eq!(state.active_guards(), 1);

        guard2.abort().unwrap();
        assert_eq!(state.active_guards(), 0);

        assert_eq!(state.committed_bytes(), 100);
        assert_eq!(state.reserved_bytes(), 0);
    }

    #[test]
    fn test_guard_debug() {
        let state = make_state();
        state.add_reserved(100, 5);

        let guard = ReservationGuard::new(Arc::clone(&state), Reservation::new(100, 5));
        let debug = format!("{guard:?}");

        assert!(debug.contains("ReservationGuard"));
        assert!(debug.contains("reservation"));
        assert!(debug.contains("consumed"));

        guard.abort().unwrap();
    }

    #[test]
    fn test_admission_error_display() {
        let err = AdmissionError::ByteLimitExceeded {
            requested: 100,
            available: 50,
            max: 1000,
        };
        assert!(format!("{err}").contains("byte limit exceeded"));
        assert!(format!("{err}").contains("available"));

        let err = AdmissionError::OpsLimitExceeded {
            requested: 10,
            available: 5,
            max: 100,
        };
        assert!(format!("{err}").contains("ops limit exceeded"));

        let err = AdmissionError::ZeroReservation;
        assert!(format!("{err}").contains("zero reservation"));
    }

    #[test]
    fn test_commit_error_display() {
        let err = CommitError::AlreadyConsumed;
        assert!(format!("{err}").contains("already consumed"));

        let err = CommitError::ActualExceedsReservation {
            actual_bytes: 150,
            reserved_bytes: 100,
            actual_ops: 6,
            reserved_ops: 5,
        };
        assert!(format!("{err}").contains("actual exceeds reservation"));
    }

    #[test]
    fn test_guard_extract() {
        let state = make_state();
        state.add_reserved(100, 5);

        let guard = ReservationGuard::new(Arc::clone(&state), Reservation::new(100, 5));
        let extracted = guard.extract();

        assert!(extracted.is_some());
        assert_eq!(extracted.unwrap().bytes, 100);

        // Guard is now empty, active_guards decremented on drop
        assert_eq!(state.active_guards(), 0);
        // But reserved wasn't released (extracted takes ownership)
        // In real use, the caller would handle this
    }
}
