//! Compaction error types and failure state guarantees.
//!
//! This module implements Step 14b (FR-54, FR-56) of the unified graph architecture.
//!
//! # Failure State Table
//!
//! The compaction process can fail at various phases. Each error variant has
//! documented state guarantees per the 11-column failure state table:
//!
//! | Error | Phase | Forward CSR | Forward Deltas | Forward Seq | Reverse CSR | Reverse Deltas | Reverse Seq | Committed | Reserved | Counter Reconciled |
//! |-------|-------|-------------|----------------|-------------|-------------|----------------|-------------|-----------|----------|-------------------|
//! | `ConcurrentModification` | Phase 2 start | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | N/A |
//! | `ForwardSwapFailed` | Phase 2 forward | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | N/A |
//! | `ReverseSwapFailed` | Phase 2 reverse | ROLLED BACK | ROLLED BACK | ROLLED BACK | UNCHANGED | UNCHANGED | UNCHANGED | RESTORED | RESTORED | YES |
//! | `CounterReconcileFailed` | Phase 2 post-swap | SUCCESS | CLEARED | RESET | SUCCESS | CLEARED | RESET | STALE | UNCHANGED | NO - LOGGED |
//! | `Interrupted` | Phase 1 | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | N/A |
//! | `BuildFailed` | Phase 1 | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | UNCHANGED | N/A |
//!
//! # Invariants
//!
//! - Forward/reverse stores are ALWAYS consistent after any outcome
//! - `ReverseSwapFailed` triggers complete forward rollback before returning
//! - `CounterReconcileFailed` is logged but CSR changes persist (no rollback possible)
//!
//! # Two-Phase Commit
//!
//! Compaction uses a two-phase commit protocol:
//!
//! 1. **Phase 1 (Prepare)**: Build new CSRs offline without holding locks
//! 2. **Phase 2 (Commit)**: Atomic swap with rollback on failure
//!
//! Failures in Phase 1 leave the system completely unchanged.
//! Failures in Phase 2 before forward swap leave the system unchanged.
//! Failures in Phase 2 after forward swap but before reverse swap trigger rollback.

use std::fmt;

/// Error type for compaction operations.
///
/// Each variant has documented state guarantees as per the failure state table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionError {
    /// Sequence counter changed between snapshot and commit attempt.
    ///
    /// **Phase**: Phase 2 start (lock acquisition)
    ///
    /// **State Guarantees**:
    /// - All stores: UNCHANGED
    /// - Counters: UNCHANGED
    /// - Action: Caller should retry compaction
    ConcurrentModification {
        /// Expected sequence number from snapshot
        expected_seq: u64,
        /// Actual sequence number found
        actual_seq: u64,
        /// Which direction had the mismatch
        direction: Direction,
    },

    /// Forward CSR swap failed during Phase 2.
    ///
    /// **Phase**: Phase 2 forward swap
    ///
    /// **State Guarantees**:
    /// - All stores: UNCHANGED
    /// - Counters: UNCHANGED
    /// - Action: Log and return error
    ForwardSwapFailed {
        /// Underlying swap error
        reason: SwapFailureReason,
    },

    /// Reverse CSR swap failed after forward swap succeeded.
    ///
    /// **Phase**: Phase 2 reverse swap
    ///
    /// **State Guarantees**:
    /// - Forward: ROLLED BACK to checkpoint
    /// - Reverse: UNCHANGED
    /// - Counters: RESTORED from checkpoint
    /// - Action: Rollback is automatic before error return
    ReverseSwapFailed {
        /// Underlying swap error
        reason: SwapFailureReason,
        /// Whether rollback succeeded
        rollback_successful: bool,
    },

    /// Counter reconciliation failed after successful CSR swaps.
    ///
    /// **Phase**: Phase 2 post-swap counter update
    ///
    /// **State Guarantees**:
    /// - Forward/Reverse CSR: SUCCESS (new CSRs in place)
    /// - Delta buffers: CLEARED
    /// - Committed counters: STALE (not updated)
    /// - Reserved: UNCHANGED
    /// - Counter reconciled: NO - LOGGED
    ///
    /// This is a partial success state. CSR changes persist but counters
    /// are inconsistent. System should continue functioning but counter
    /// values may drift until next successful compaction.
    CounterReconcileFailed {
        /// Number of active reservation guards that prevented reset (if any).
        /// Non-zero means reset was blocked by concurrent operations.
        active_guards: usize,
        /// Whether the forward CSR was successfully swapped before this error
        forward_swapped: bool,
        /// Whether the reverse CSR was successfully swapped before this error
        reverse_swapped: bool,
    },

    /// Compaction interrupted before Phase 2 began.
    ///
    /// **Phase**: Phase 1 (CSR build)
    ///
    /// **State Guarantees**:
    /// - All stores: UNCHANGED
    /// - Counters: UNCHANGED
    /// - Action: Caller may retry when ready
    Interrupted {
        /// Reason for interruption
        reason: InterruptReason,
        /// Number of edges processed before interruption
        edges_processed: usize,
        /// Total edges that were to be processed
        edges_total: usize,
    },

    /// CSR build failed during Phase 1.
    ///
    /// **Phase**: Phase 1 (CSR build)
    ///
    /// **State Guarantees**:
    /// - All stores: UNCHANGED
    /// - Counters: UNCHANGED
    /// - Action: Investigate build failure cause
    BuildFailed {
        /// Which direction's CSR failed to build
        direction: Direction,
        /// Reason for build failure
        reason: BuildFailureReason,
    },
}

/// Direction indicator for bidirectional operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Forward edge direction (source → target)
    Forward,
    /// Reverse edge direction (target → source)
    Reverse,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Forward => write!(f, "forward"),
            Self::Reverse => write!(f, "reverse"),
        }
    }
}

/// Reason for CSR swap failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapFailureReason {
    /// CSR validation failed
    ValidationFailed {
        /// Description of what validation failed
        message: String,
    },
    /// Memory allocation failed
    AllocationFailed,
    /// Internal invariant violation
    InvariantViolation {
        /// Description of the violated invariant
        message: String,
    },
}

impl fmt::Display for SwapFailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValidationFailed { message } => write!(f, "validation failed: {message}"),
            Self::AllocationFailed => write!(f, "memory allocation failed"),
            Self::InvariantViolation { message } => write!(f, "invariant violation: {message}"),
        }
    }
}

/// Reason for compaction interruption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterruptReason {
    /// Shutdown signal received
    ShutdownRequested,
    /// Cancellation token triggered
    Cancelled,
    /// Cancellation token triggered (explicit request)
    CancellationRequested,
    /// Timeout exceeded
    Timeout {
        /// Time elapsed before timeout in milliseconds
        elapsed_ms: u64,
        /// Configured timeout limit in milliseconds
        limit_ms: u64,
    },
}

impl fmt::Display for InterruptReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShutdownRequested => write!(f, "shutdown requested"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::CancellationRequested => write!(f, "cancellation requested"),
            Self::Timeout {
                elapsed_ms,
                limit_ms,
            } => {
                write!(f, "timeout after {elapsed_ms}ms (limit: {limit_ms}ms)")
            }
        }
    }
}

/// Reason for CSR build failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildFailureReason {
    /// Not enough edges to build valid CSR
    InsufficientEdges {
        /// Actual edge count
        count: usize,
        /// Minimum required edges
        minimum: usize,
    },
    /// Edge data corrupted or invalid
    InvalidEdgeData {
        /// Description of the invalid data
        message: String,
    },
    /// Memory allocation failed during build
    AllocationFailed,
    /// Internal builder error
    BuilderError {
        /// Description of the builder error
        message: String,
    },
}

impl fmt::Display for BuildFailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsufficientEdges { count, minimum } => {
                write!(f, "insufficient edges: {count} (minimum: {minimum})")
            }
            Self::InvalidEdgeData { message } => write!(f, "invalid edge data: {message}"),
            Self::AllocationFailed => write!(f, "memory allocation failed"),
            Self::BuilderError { message } => write!(f, "builder error: {message}"),
        }
    }
}

impl fmt::Display for CompactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConcurrentModification {
                expected_seq,
                actual_seq,
                direction,
            } => {
                write!(
                    f,
                    "concurrent modification in {direction} store: expected seq {expected_seq}, found {actual_seq}"
                )
            }
            Self::ForwardSwapFailed { reason } => {
                write!(f, "forward CSR swap failed: {reason}")
            }
            Self::ReverseSwapFailed {
                reason,
                rollback_successful,
            } => {
                let rollback_status = if *rollback_successful {
                    "rollback succeeded"
                } else {
                    "ROLLBACK FAILED"
                };
                write!(f, "reverse CSR swap failed: {reason} ({rollback_status})")
            }
            Self::CounterReconcileFailed {
                active_guards,
                forward_swapped,
                reverse_swapped,
            } => {
                let swap_status = match (forward_swapped, reverse_swapped) {
                    (true, true) => "both CSRs swapped",
                    (true, false) => "forward CSR swapped",
                    (false, true) => "reverse CSR swapped",
                    (false, false) => "no CSRs swapped",
                };
                write!(
                    f,
                    "counter reconciliation failed: {active_guards} active guards prevented reset ({swap_status})"
                )
            }
            Self::Interrupted {
                reason,
                edges_processed,
                edges_total,
            } => {
                write!(
                    f,
                    "compaction interrupted: {reason} ({edges_processed}/{edges_total} edges processed)"
                )
            }
            Self::BuildFailed { direction, reason } => {
                write!(f, "{direction} CSR build failed: {reason}")
            }
        }
    }
}

impl std::error::Error for CompactionError {}

/// Represents the post-error state for a compaction operation.
///
/// This struct documents what state each component is in after a compaction
/// error, enabling callers to understand system state and take appropriate action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostErrorState {
    /// State of the forward CSR
    pub forward_csr: ComponentState,
    /// State of the forward delta buffer
    pub forward_deltas: ComponentState,
    /// State of the forward sequence counter
    pub forward_seq: ComponentState,
    /// State of the reverse CSR
    pub reverse_csr: ComponentState,
    /// State of the reverse delta buffer
    pub reverse_deltas: ComponentState,
    /// State of the reverse sequence counter
    pub reverse_seq: ComponentState,
    /// State of committed counters
    pub committed: ComponentState,
    /// State of reserved counters
    pub reserved: ComponentState,
    /// Whether counter reconciliation was performed
    pub counter_reconciled: CounterReconcileState,
}

/// State of a component after an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentState {
    /// Component unchanged from before operation
    Unchanged,
    /// Component was rolled back to checkpoint
    RolledBack,
    /// Component was restored from checkpoint
    Restored,
    /// Component update succeeded
    Success,
    /// Component was cleared
    Cleared,
    /// Component was reset
    Reset,
    /// Component is now stale/inconsistent
    Stale,
}

impl fmt::Display for ComponentState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unchanged => write!(f, "UNCHANGED"),
            Self::RolledBack => write!(f, "ROLLED BACK"),
            Self::Restored => write!(f, "RESTORED"),
            Self::Success => write!(f, "SUCCESS"),
            Self::Cleared => write!(f, "CLEARED"),
            Self::Reset => write!(f, "RESET"),
            Self::Stale => write!(f, "STALE"),
        }
    }
}

/// State of counter reconciliation after an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterReconcileState {
    /// Counter reconciliation not applicable (error occurred before)
    NotApplicable,
    /// Counter reconciliation succeeded
    Yes,
    /// Counter reconciliation failed and was logged
    NoLogged,
}

impl fmt::Display for CounterReconcileState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotApplicable => write!(f, "N/A"),
            Self::Yes => write!(f, "YES"),
            Self::NoLogged => write!(f, "NO - LOGGED"),
        }
    }
}

impl CompactionError {
    /// Get the post-error state for this error variant.
    ///
    /// Returns the documented state guarantee for each system component
    /// after this error occurs.
    #[must_use]
    pub fn post_error_state(&self) -> PostErrorState {
        match self {
            Self::ConcurrentModification { .. } | Self::ForwardSwapFailed { .. } => {
                PostErrorState {
                    forward_csr: ComponentState::Unchanged,
                    forward_deltas: ComponentState::Unchanged,
                    forward_seq: ComponentState::Unchanged,
                    reverse_csr: ComponentState::Unchanged,
                    reverse_deltas: ComponentState::Unchanged,
                    reverse_seq: ComponentState::Unchanged,
                    committed: ComponentState::Unchanged,
                    reserved: ComponentState::Unchanged,
                    counter_reconciled: CounterReconcileState::NotApplicable,
                }
            }
            Self::ReverseSwapFailed { .. } => PostErrorState {
                forward_csr: ComponentState::RolledBack,
                forward_deltas: ComponentState::RolledBack,
                forward_seq: ComponentState::RolledBack,
                reverse_csr: ComponentState::Unchanged,
                reverse_deltas: ComponentState::Unchanged,
                reverse_seq: ComponentState::Unchanged,
                committed: ComponentState::Restored,
                reserved: ComponentState::Restored,
                counter_reconciled: CounterReconcileState::Yes,
            },
            Self::CounterReconcileFailed { .. } => PostErrorState {
                forward_csr: ComponentState::Success,
                forward_deltas: ComponentState::Cleared,
                forward_seq: ComponentState::Reset,
                reverse_csr: ComponentState::Success,
                reverse_deltas: ComponentState::Cleared,
                reverse_seq: ComponentState::Reset,
                committed: ComponentState::Stale,
                reserved: ComponentState::Unchanged,
                counter_reconciled: CounterReconcileState::NoLogged,
            },
            Self::Interrupted { .. } | Self::BuildFailed { .. } => PostErrorState {
                forward_csr: ComponentState::Unchanged,
                forward_deltas: ComponentState::Unchanged,
                forward_seq: ComponentState::Unchanged,
                reverse_csr: ComponentState::Unchanged,
                reverse_deltas: ComponentState::Unchanged,
                reverse_seq: ComponentState::Unchanged,
                committed: ComponentState::Unchanged,
                reserved: ComponentState::Unchanged,
                counter_reconciled: CounterReconcileState::NotApplicable,
            },
        }
    }

    /// Get the phase where this error occurred.
    #[must_use]
    pub fn phase(&self) -> CompactionPhase {
        match self {
            Self::ConcurrentModification { .. } => CompactionPhase::Phase2Start,
            Self::ForwardSwapFailed { .. } => CompactionPhase::Phase2Forward,
            Self::ReverseSwapFailed { .. } => CompactionPhase::Phase2Reverse,
            Self::CounterReconcileFailed { .. } => CompactionPhase::Phase2PostSwap,
            Self::Interrupted { .. } | Self::BuildFailed { .. } => CompactionPhase::Phase1,
        }
    }

    /// Check if the system state is fully consistent after this error.
    ///
    /// Returns `true` if all stores are either unchanged or fully rolled back.
    /// Returns `false` if the error left the system in a partial state.
    #[must_use]
    pub fn is_fully_consistent(&self) -> bool {
        // CounterReconcileFailed leaves counters stale
        !matches!(self, Self::CounterReconcileFailed { .. })
    }

    /// Check if this error should trigger a retry.
    #[must_use]
    pub fn should_retry(&self) -> bool {
        matches!(
            self,
            Self::ConcurrentModification { .. } | Self::Interrupted { .. }
        )
    }
}

/// Phase of the compaction process where an error occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionPhase {
    /// Phase 1: Building CSRs offline
    Phase1,
    /// Phase 2: Lock acquisition and seq check
    Phase2Start,
    /// Phase 2: Forward CSR swap
    Phase2Forward,
    /// Phase 2: Reverse CSR swap
    Phase2Reverse,
    /// Phase 2: Counter reconciliation after successful swaps
    Phase2PostSwap,
}

impl fmt::Display for CompactionPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Phase1 => write!(f, "Phase 1 (prepare)"),
            Self::Phase2Start => write!(f, "Phase 2 start (lock acquisition)"),
            Self::Phase2Forward => write!(f, "Phase 2 forward (CSR swap)"),
            Self::Phase2Reverse => write!(f, "Phase 2 reverse (CSR swap)"),
            Self::Phase2PostSwap => write!(f, "Phase 2 post-swap (counter reconciliation)"),
        }
    }
}

/// Pre-conditions for CSR swap operation.
///
/// Implements the `swap_csr` contract as per FR-54.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapPreconditions {
    /// Expected sequence number (from snapshot)
    pub expected_seq: u64,
    /// Expected CSR version
    pub expected_csr_version: u64,
    /// Whether delta buffer should be non-empty
    pub require_deltas: bool,
}

impl SwapPreconditions {
    /// Validate pre-conditions before swap.
    ///
    /// Returns `Ok(())` if all pre-conditions are met, or an error describing
    /// which pre-condition failed.
    ///
    /// # Errors
    ///
    /// Returns `SwapPreconditionError` when any pre-condition is violated.
    pub fn validate(
        &self,
        actual_seq: u64,
        actual_csr_version: u64,
        delta_count: usize,
    ) -> Result<(), SwapPreconditionError> {
        if actual_seq != self.expected_seq {
            return Err(SwapPreconditionError::SequenceMismatch {
                expected: self.expected_seq,
                actual: actual_seq,
            });
        }

        if actual_csr_version != self.expected_csr_version {
            return Err(SwapPreconditionError::CsrVersionMismatch {
                expected: self.expected_csr_version,
                actual: actual_csr_version,
            });
        }

        if self.require_deltas && delta_count == 0 {
            return Err(SwapPreconditionError::EmptyDeltaBuffer);
        }

        Ok(())
    }
}

/// Error from swap pre-condition validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapPreconditionError {
    /// Sequence number doesn't match expected value
    SequenceMismatch {
        /// Expected sequence number from snapshot
        expected: u64,
        /// Actual sequence number found
        actual: u64,
    },
    /// CSR version doesn't match expected value
    CsrVersionMismatch {
        /// Expected CSR version from snapshot
        expected: u64,
        /// Actual CSR version found
        actual: u64,
    },
    /// Delta buffer is empty when non-empty was required
    EmptyDeltaBuffer,
}

impl fmt::Display for SwapPreconditionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SequenceMismatch { expected, actual } => {
                write!(f, "sequence mismatch: expected {expected}, actual {actual}")
            }
            Self::CsrVersionMismatch { expected, actual } => {
                write!(
                    f,
                    "CSR version mismatch: expected {expected}, actual {actual}"
                )
            }
            Self::EmptyDeltaBuffer => write!(f, "delta buffer is empty"),
        }
    }
}

impl std::error::Error for SwapPreconditionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_concurrent_modification_error() {
        let error = CompactionError::ConcurrentModification {
            expected_seq: 100,
            actual_seq: 105,
            direction: Direction::Forward,
        };

        assert!(error.to_string().contains("concurrent modification"));
        assert!(error.to_string().contains("forward"));
        assert!(error.to_string().contains("100"));
        assert!(error.to_string().contains("105"));
        assert_eq!(error.phase(), CompactionPhase::Phase2Start);
        assert!(error.is_fully_consistent());
        assert!(error.should_retry());
    }

    #[test]
    fn test_forward_swap_failed_error() {
        let error = CompactionError::ForwardSwapFailed {
            reason: SwapFailureReason::ValidationFailed {
                message: "invalid node count".to_string(),
            },
        };

        assert!(error.to_string().contains("forward CSR swap failed"));
        assert!(error.to_string().contains("validation failed"));
        assert_eq!(error.phase(), CompactionPhase::Phase2Forward);
        assert!(error.is_fully_consistent());
        assert!(!error.should_retry());
    }

    #[test]
    fn test_reverse_swap_failed_error() {
        let error = CompactionError::ReverseSwapFailed {
            reason: SwapFailureReason::AllocationFailed,
            rollback_successful: true,
        };

        assert!(error.to_string().contains("reverse CSR swap failed"));
        assert!(error.to_string().contains("rollback succeeded"));
        assert_eq!(error.phase(), CompactionPhase::Phase2Reverse);
        assert!(error.is_fully_consistent());
        assert!(!error.should_retry());
    }

    #[test]
    fn test_reverse_swap_failed_rollback_failed() {
        let error = CompactionError::ReverseSwapFailed {
            reason: SwapFailureReason::InvariantViolation {
                message: "edge count mismatch".to_string(),
            },
            rollback_successful: false,
        };

        assert!(error.to_string().contains("ROLLBACK FAILED"));
        assert!(error.is_fully_consistent()); // Still consistent per design
    }

    #[test]
    fn test_counter_reconcile_failed_error() {
        let error = CompactionError::CounterReconcileFailed {
            active_guards: 2,
            forward_swapped: true,
            reverse_swapped: true,
        };

        assert!(error.to_string().contains("counter reconciliation failed"));
        assert!(error.to_string().contains("2 active guards"));
        assert!(error.to_string().contains("both CSRs swapped"));
        assert_eq!(error.phase(), CompactionPhase::Phase2PostSwap);
        assert!(!error.is_fully_consistent()); // Counters are stale
        assert!(!error.should_retry());
    }

    #[test]
    fn test_interrupted_error() {
        let error = CompactionError::Interrupted {
            reason: InterruptReason::Timeout {
                elapsed_ms: 5000,
                limit_ms: 3000,
            },
            edges_processed: 500,
            edges_total: 1000,
        };

        assert!(error.to_string().contains("compaction interrupted"));
        assert!(error.to_string().contains("timeout"));
        assert!(error.to_string().contains("500/1000"));
        assert_eq!(error.phase(), CompactionPhase::Phase1);
        assert!(error.is_fully_consistent());
        assert!(error.should_retry());
    }

    #[test]
    fn test_build_failed_error() {
        let error = CompactionError::BuildFailed {
            direction: Direction::Reverse,
            reason: BuildFailureReason::InsufficientEdges {
                count: 0,
                minimum: 1,
            },
        };

        assert!(error.to_string().contains("reverse CSR build failed"));
        assert!(error.to_string().contains("insufficient edges"));
        assert_eq!(error.phase(), CompactionPhase::Phase1);
        assert!(error.is_fully_consistent());
        assert!(!error.should_retry());
    }

    #[test]
    fn test_post_error_state_concurrent_modification() {
        let error = CompactionError::ConcurrentModification {
            expected_seq: 1,
            actual_seq: 2,
            direction: Direction::Forward,
        };

        let state = error.post_error_state();
        assert_eq!(state.forward_csr, ComponentState::Unchanged);
        assert_eq!(state.reverse_csr, ComponentState::Unchanged);
        assert_eq!(state.committed, ComponentState::Unchanged);
        assert_eq!(
            state.counter_reconciled,
            CounterReconcileState::NotApplicable
        );
    }

    #[test]
    fn test_post_error_state_reverse_swap_failed() {
        let error = CompactionError::ReverseSwapFailed {
            reason: SwapFailureReason::AllocationFailed,
            rollback_successful: true,
        };

        let state = error.post_error_state();
        assert_eq!(state.forward_csr, ComponentState::RolledBack);
        assert_eq!(state.forward_deltas, ComponentState::RolledBack);
        assert_eq!(state.reverse_csr, ComponentState::Unchanged);
        assert_eq!(state.committed, ComponentState::Restored);
        assert_eq!(state.counter_reconciled, CounterReconcileState::Yes);
    }

    #[test]
    fn test_post_error_state_counter_reconcile_failed() {
        let error = CompactionError::CounterReconcileFailed {
            active_guards: 1,
            forward_swapped: true,
            reverse_swapped: true,
        };

        let state = error.post_error_state();
        assert_eq!(state.forward_csr, ComponentState::Success);
        assert_eq!(state.forward_deltas, ComponentState::Cleared);
        assert_eq!(state.forward_seq, ComponentState::Reset);
        assert_eq!(state.committed, ComponentState::Stale);
        assert_eq!(state.counter_reconciled, CounterReconcileState::NoLogged);
    }

    #[test]
    fn test_direction_display() {
        assert_eq!(Direction::Forward.to_string(), "forward");
        assert_eq!(Direction::Reverse.to_string(), "reverse");
    }

    #[test]
    fn test_swap_failure_reason_display() {
        assert!(
            SwapFailureReason::AllocationFailed
                .to_string()
                .contains("allocation")
        );
        assert!(
            SwapFailureReason::ValidationFailed {
                message: "test".to_string()
            }
            .to_string()
            .contains("test")
        );
    }

    #[test]
    fn test_interrupt_reason_display() {
        assert!(
            InterruptReason::ShutdownRequested
                .to_string()
                .contains("shutdown")
        );
        assert!(InterruptReason::Cancelled.to_string().contains("cancelled"));
        assert!(
            InterruptReason::Timeout {
                elapsed_ms: 100,
                limit_ms: 50
            }
            .to_string()
            .contains("100ms")
        );
    }

    #[test]
    fn test_build_failure_reason_display() {
        assert!(
            BuildFailureReason::AllocationFailed
                .to_string()
                .contains("allocation")
        );
        assert!(
            BuildFailureReason::InsufficientEdges {
                count: 0,
                minimum: 1
            }
            .to_string()
            .contains("insufficient")
        );
    }

    #[test]
    fn test_compaction_phase_display() {
        assert!(CompactionPhase::Phase1.to_string().contains("Phase 1"));
        assert!(
            CompactionPhase::Phase2Start
                .to_string()
                .contains("Phase 2 start")
        );
        assert!(
            CompactionPhase::Phase2Forward
                .to_string()
                .contains("forward")
        );
    }

    #[test]
    fn test_component_state_display() {
        assert_eq!(ComponentState::Unchanged.to_string(), "UNCHANGED");
        assert_eq!(ComponentState::RolledBack.to_string(), "ROLLED BACK");
        assert_eq!(ComponentState::Restored.to_string(), "RESTORED");
        assert_eq!(ComponentState::Success.to_string(), "SUCCESS");
        assert_eq!(ComponentState::Cleared.to_string(), "CLEARED");
        assert_eq!(ComponentState::Reset.to_string(), "RESET");
        assert_eq!(ComponentState::Stale.to_string(), "STALE");
    }

    #[test]
    fn test_counter_reconcile_state_display() {
        assert_eq!(CounterReconcileState::NotApplicable.to_string(), "N/A");
        assert_eq!(CounterReconcileState::Yes.to_string(), "YES");
        assert_eq!(CounterReconcileState::NoLogged.to_string(), "NO - LOGGED");
    }

    #[test]
    fn test_swap_preconditions_validate_success() {
        let preconditions = SwapPreconditions {
            expected_seq: 100,
            expected_csr_version: 5,
            require_deltas: true,
        };

        assert!(preconditions.validate(100, 5, 10).is_ok());
    }

    #[test]
    fn test_swap_preconditions_validate_seq_mismatch() {
        let preconditions = SwapPreconditions {
            expected_seq: 100,
            expected_csr_version: 5,
            require_deltas: false,
        };

        let result = preconditions.validate(101, 5, 0);
        assert!(matches!(
            result,
            Err(SwapPreconditionError::SequenceMismatch {
                expected: 100,
                actual: 101
            })
        ));
    }

    #[test]
    fn test_swap_preconditions_validate_csr_version_mismatch() {
        let preconditions = SwapPreconditions {
            expected_seq: 100,
            expected_csr_version: 5,
            require_deltas: false,
        };

        let result = preconditions.validate(100, 6, 0);
        assert!(matches!(
            result,
            Err(SwapPreconditionError::CsrVersionMismatch {
                expected: 5,
                actual: 6
            })
        ));
    }

    #[test]
    fn test_swap_preconditions_validate_empty_deltas() {
        let preconditions = SwapPreconditions {
            expected_seq: 100,
            expected_csr_version: 5,
            require_deltas: true,
        };

        let result = preconditions.validate(100, 5, 0);
        assert!(matches!(
            result,
            Err(SwapPreconditionError::EmptyDeltaBuffer)
        ));
    }

    #[test]
    fn test_swap_precondition_error_display() {
        assert!(
            SwapPreconditionError::SequenceMismatch {
                expected: 1,
                actual: 2
            }
            .to_string()
            .contains("sequence")
        );
        assert!(
            SwapPreconditionError::CsrVersionMismatch {
                expected: 1,
                actual: 2
            }
            .to_string()
            .contains("CSR version")
        );
        assert!(
            SwapPreconditionError::EmptyDeltaBuffer
                .to_string()
                .contains("empty")
        );
    }

    #[test]
    fn test_error_is_std_error() {
        fn assert_error<E: std::error::Error>() {}
        assert_error::<CompactionError>();
        assert_error::<SwapPreconditionError>();
    }
}
