//! Admission control for the unified graph architecture.
//!
//! This module provides back-pressure control for the delta buffer:
//! - [`SharedBufferState`]: Atomic counters shared between buffer and controller
//! - [`Reservation`]: A granted allocation of bytes and operations
//! - [`ReservationGuard`]: RAII wrapper ensuring proper commit/abort
//! - [`AdmissionController`]: Reservation-based admission control (Step 12)
//! - [`BackPressureController`]: Hard/soft limit enforcement (Step 16b)
//!
//! # Design Principles
//!
//! - **Reservation-based**: Writers must reserve capacity before writing
//! - **RAII safety**: `ReservationGuard` ensures reservations cannot leak
//! - **Underflow protection**: Active guards prevent counter reset
//! - **Atomic operations**: All counters are atomic for concurrent access
//! - **Dual CAS loops**: Lock-free concurrent reservation with rollback (FR-57, FR-63)
//! - **Back-pressure limits**: Soft limit signals compaction, hard limit rejects writes (FR-46, FR-52)

pub mod backpressure;
pub mod controller;
pub mod reservation;
pub mod state;

pub use backpressure::{
    BackPressureController, BackPressureLimits, BackPressureReason, BackPressureStatus,
};
pub use controller::{AdmissionController, AdmissionControllerStats};
pub use reservation::{AdmissionError, CommitError, Reservation, ReservationGuard};
pub use state::{BufferStateSnapshot, SharedBufferState};
