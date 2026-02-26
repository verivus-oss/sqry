//! Back-Pressure Controller: Enforces delta buffer limits.
//!
//! This module implements back-pressure controls to prevent unbounded delta
//! buffer growth by enforcing soft and hard limits on operations and bytes.
//!
//! # Design (FR-46, FR-52)
//!
//! - **Hard limits**: Reject writes when exceeded
//! - **Soft limits**: Signal compaction need when exceeded (typically 80% of hard)
//! - **Dual tracking**: Both operation count and byte size
//!
//! # Limits (CP-10)
//!
//! | Metric | Soft Limit | Hard Limit |
//! |--------|------------|------------|
//! | Operations | 80,000 (80%) | 100,000 |
//! | Bytes | 8MB (80%) | 10MB |
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::admission::backpressure::{
//!     BackPressureController, BackPressureLimits, BackPressureStatus,
//! };
//!
//! let controller = BackPressureController::default();
//! let status = controller.check_pressure(75_000, 7_000_000);
//!
//! match status {
//!     BackPressureStatus::Normal => { /* accept writes */ }
//!     BackPressureStatus::SoftLimit { reason } => { /* trigger compaction */ }
//!     BackPressureStatus::HardLimit { reason } => { /* reject writes */ }
//! }
//! ```

use std::fmt;

fn u64_to_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn scale_u64_by_ratio(value: u64, ratio: f64) -> u64 {
    let scaled = u64_to_f64(value) * ratio;
    if !scaled.is_finite() || scaled <= 0.0 {
        return 0;
    }
    let capped = scaled.min(u64_to_f64(u64::MAX));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    {
        capped.round() as u64
    }
}

/// Limits configuration for back-pressure control.
#[derive(Debug, Clone, Copy)]
pub struct BackPressureLimits {
    /// Hard limit for operation count. Writes rejected above this.
    pub max_operations: u64,
    /// Hard limit for byte size. Writes rejected above this.
    pub max_bytes: u64,
    /// Soft limit ratio (0.0-1.0). Compaction signaled above this.
    pub soft_limit_ratio: f64,
}

impl Default for BackPressureLimits {
    fn default() -> Self {
        Self {
            max_operations: 100_000,     // FR-46: 100K ops hard limit
            max_bytes: 10 * 1024 * 1024, // FR-46: 10MB hard limit
            soft_limit_ratio: 0.80,      // CP-10: 80% soft limit
        }
    }
}

impl BackPressureLimits {
    /// Creates new limits with custom values.
    #[must_use]
    pub fn new(max_operations: u64, max_bytes: u64) -> Self {
        Self {
            max_operations,
            max_bytes,
            soft_limit_ratio: 0.80,
        }
    }

    /// Sets the soft limit ratio.
    #[must_use]
    pub fn with_soft_ratio(mut self, ratio: f64) -> Self {
        self.soft_limit_ratio = ratio.clamp(0.0, 1.0);
        self
    }

    /// Returns the soft limit for operations.
    #[must_use]
    pub fn soft_operations(&self) -> u64 {
        scale_u64_by_ratio(self.max_operations, self.soft_limit_ratio)
    }

    /// Returns the soft limit for bytes.
    #[must_use]
    pub fn soft_bytes(&self) -> u64 {
        scale_u64_by_ratio(self.max_bytes, self.soft_limit_ratio)
    }
}

/// Reason for back-pressure trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackPressureReason {
    /// Operation count exceeded limit.
    OperationCount {
        /// Current operation count.
        current: u64,
        /// Limit that was exceeded.
        limit: u64,
    },
    /// Byte size exceeded limit.
    ByteSize {
        /// Current byte size.
        current: u64,
        /// Limit that was exceeded.
        limit: u64,
    },
    /// Both limits exceeded (reports worse one).
    Both {
        /// Operation usage (0-100).
        op_usage_percent: u8,
        /// Byte usage (0-100).
        byte_usage_percent: u8,
    },
}

impl fmt::Display for BackPressureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OperationCount { current, limit } => {
                write!(f, "operation count {current}/{limit}")
            }
            Self::ByteSize { current, limit } => {
                let current_mb = u64_to_f64(*current) / (1024.0 * 1024.0);
                let limit_mb = u64_to_f64(*limit) / (1024.0 * 1024.0);
                write!(f, "byte size {current_mb:.2}MB/{limit_mb:.2}MB")
            }
            Self::Both {
                op_usage_percent,
                byte_usage_percent,
            } => {
                write!(
                    f,
                    "operations at {op_usage_percent}%, bytes at {byte_usage_percent}%"
                )
            }
        }
    }
}

/// Status of back-pressure check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackPressureStatus {
    /// Below soft limit, normal operation.
    Normal,
    /// Above soft limit, compaction should be triggered.
    SoftLimit {
        /// Reason for soft limit trigger.
        reason: BackPressureReason,
    },
    /// Above hard limit, writes should be rejected.
    HardLimit {
        /// Reason for hard limit trigger.
        reason: BackPressureReason,
    },
}

impl BackPressureStatus {
    /// Returns true if writes should be rejected.
    #[must_use]
    pub fn should_reject(&self) -> bool {
        matches!(self, Self::HardLimit { .. })
    }

    /// Returns true if compaction should be triggered.
    #[must_use]
    pub fn should_compact(&self) -> bool {
        matches!(self, Self::SoftLimit { .. } | Self::HardLimit { .. })
    }
}

impl fmt::Display for BackPressureStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Normal => write!(f, "normal (below soft limit)"),
            Self::SoftLimit { reason } => write!(f, "soft limit: {reason}"),
            Self::HardLimit { reason } => write!(f, "HARD LIMIT: {reason}"),
        }
    }
}

/// Controller for back-pressure enforcement.
///
/// The controller monitors delta buffer utilization and determines when
/// to signal compaction (soft limit) or reject writes (hard limit).
#[derive(Debug, Clone, Default)]
pub struct BackPressureController {
    /// Configured limits.
    limits: BackPressureLimits,
}

impl BackPressureController {
    /// Creates a new controller with custom limits.
    #[must_use]
    pub fn new(limits: BackPressureLimits) -> Self {
        Self { limits }
    }

    /// Returns the current limits configuration.
    #[must_use]
    pub fn limits(&self) -> &BackPressureLimits {
        &self.limits
    }

    /// Updates the limits configuration.
    pub fn set_limits(&mut self, limits: BackPressureLimits) {
        self.limits = limits;
    }

    /// Checks back-pressure status given current utilization.
    ///
    /// # Arguments
    ///
    /// * `operations` - Current operation count in delta buffer
    /// * `bytes` - Current byte size of delta buffer
    ///
    /// # Returns
    ///
    /// The back-pressure status indicating normal, soft, or hard limit.
    #[must_use]
    pub fn check_pressure(&self, operations: u64, bytes: u64) -> BackPressureStatus {
        let op_hard = operations >= self.limits.max_operations;
        let byte_hard = bytes >= self.limits.max_bytes;

        // Check hard limits first
        if op_hard || byte_hard {
            let reason = Self::make_reason(
                operations,
                bytes,
                self.limits.max_operations,
                self.limits.max_bytes,
                op_hard,
                byte_hard,
            );
            return BackPressureStatus::HardLimit { reason };
        }

        let op_soft = operations >= self.limits.soft_operations();
        let byte_soft = bytes >= self.limits.soft_bytes();

        // Check soft limits
        if op_soft || byte_soft {
            let reason = Self::make_reason(
                operations,
                bytes,
                self.limits.soft_operations(),
                self.limits.soft_bytes(),
                op_soft,
                byte_soft,
            );
            return BackPressureStatus::SoftLimit { reason };
        }

        BackPressureStatus::Normal
    }

    /// Checks if a proposed addition would exceed hard limits.
    ///
    /// # Arguments
    ///
    /// * `current_ops` - Current operation count
    /// * `current_bytes` - Current byte size
    /// * `add_ops` - Operations to add
    /// * `add_bytes` - Bytes to add
    ///
    /// # Returns
    ///
    /// The back-pressure status after the proposed addition.
    #[must_use]
    pub fn check_addition(
        &self,
        current_ops: u64,
        current_bytes: u64,
        add_ops: u64,
        add_bytes: u64,
    ) -> BackPressureStatus {
        let new_ops = current_ops.saturating_add(add_ops);
        let new_bytes = current_bytes.saturating_add(add_bytes);
        self.check_pressure(new_ops, new_bytes)
    }

    /// Returns utilization percentages.
    ///
    /// # Returns
    ///
    /// A tuple of (`operation_percent`, `byte_percent`) from 0-100.
    #[must_use]
    pub fn utilization(&self, operations: u64, bytes: u64) -> (u8, u8) {
        let op_pct = Self::calculate_percent(operations, self.limits.max_operations);
        let byte_pct = Self::calculate_percent(bytes, self.limits.max_bytes);
        (op_pct, byte_pct)
    }

    /// Helper to construct the appropriate reason.
    fn make_reason(
        operations: u64,
        bytes: u64,
        op_limit: u64,
        byte_limit: u64,
        op_exceeded: bool,
        byte_exceeded: bool,
    ) -> BackPressureReason {
        match (op_exceeded, byte_exceeded) {
            (true, true) => BackPressureReason::Both {
                op_usage_percent: Self::calculate_percent(operations, op_limit),
                byte_usage_percent: Self::calculate_percent(bytes, byte_limit),
            },
            (true, false) => BackPressureReason::OperationCount {
                current: operations,
                limit: op_limit,
            },
            (false, true) => BackPressureReason::ByteSize {
                current: bytes,
                limit: byte_limit,
            },
            (false, false) => {
                // This shouldn't happen if called correctly
                BackPressureReason::OperationCount {
                    current: operations,
                    limit: op_limit,
                }
            }
        }
    }

    /// Calculates percentage (0-100), capped at 100.
    fn calculate_percent(current: u64, limit: u64) -> u8 {
        if limit == 0 {
            return 100;
        }
        let pct = (current.saturating_mul(100)) / limit;
        pct.min(100) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_limits() {
        let limits = BackPressureLimits::default();
        assert_eq!(limits.max_operations, 100_000);
        assert_eq!(limits.max_bytes, 10 * 1024 * 1024);
        assert!((limits.soft_limit_ratio - 0.80).abs() < f64::EPSILON);
    }

    #[test]
    fn test_soft_limit_calculations() {
        let limits = BackPressureLimits::default();
        assert_eq!(limits.soft_operations(), 80_000);
        assert_eq!(limits.soft_bytes(), 8 * 1024 * 1024);
    }

    #[test]
    fn test_custom_limits() {
        let limits = BackPressureLimits::new(50_000, 5 * 1024 * 1024);
        assert_eq!(limits.max_operations, 50_000);
        assert_eq!(limits.soft_operations(), 40_000);
    }

    #[test]
    fn test_custom_soft_ratio() {
        let limits = BackPressureLimits::new(100_000, 10 * 1024 * 1024).with_soft_ratio(0.5);
        assert_eq!(limits.soft_operations(), 50_000);
        assert_eq!(limits.soft_bytes(), 5 * 1024 * 1024);
    }

    #[test]
    fn test_controller_default() {
        let controller = BackPressureController::default();
        assert_eq!(controller.limits().max_operations, 100_000);
    }

    #[test]
    fn test_check_pressure_normal() {
        let controller = BackPressureController::default();

        // Well below soft limit
        let status = controller.check_pressure(50_000, 5 * 1024 * 1024);
        assert!(matches!(status, BackPressureStatus::Normal));
        assert!(!status.should_reject());
        assert!(!status.should_compact());
    }

    #[test]
    fn test_check_pressure_soft_ops() {
        let controller = BackPressureController::default();

        // Above soft limit (80K), below hard limit (100K)
        let status = controller.check_pressure(85_000, 5 * 1024 * 1024);
        assert!(matches!(status, BackPressureStatus::SoftLimit { .. }));
        assert!(!status.should_reject());
        assert!(status.should_compact());
    }

    #[test]
    fn test_check_pressure_soft_bytes() {
        let controller = BackPressureController::default();

        // Above soft limit (8MB), below hard limit (10MB)
        let status = controller.check_pressure(50_000, 9 * 1024 * 1024);
        assert!(matches!(status, BackPressureStatus::SoftLimit { .. }));
        assert!(status.should_compact());
    }

    #[test]
    fn test_check_pressure_hard_ops() {
        let controller = BackPressureController::default();

        // At hard limit (100K)
        let status = controller.check_pressure(100_000, 5 * 1024 * 1024);
        assert!(matches!(status, BackPressureStatus::HardLimit { .. }));
        assert!(status.should_reject());
        assert!(status.should_compact());
    }

    #[test]
    fn test_check_pressure_hard_bytes() {
        let controller = BackPressureController::default();

        // At hard limit (10MB)
        let status = controller.check_pressure(50_000, 10 * 1024 * 1024);
        assert!(matches!(status, BackPressureStatus::HardLimit { .. }));
        assert!(status.should_reject());
    }

    #[test]
    fn test_check_pressure_both_exceeded() {
        let controller = BackPressureController::default();

        // Both hard limits exceeded
        let status = controller.check_pressure(110_000, 12 * 1024 * 1024);
        match status {
            BackPressureStatus::HardLimit { reason } => {
                assert!(matches!(reason, BackPressureReason::Both { .. }));
            }
            _ => panic!("expected HardLimit"),
        }
    }

    #[test]
    fn test_check_addition_normal() {
        let controller = BackPressureController::default();

        // Adding 10K ops to 50K current = 60K total (below soft)
        let status = controller.check_addition(50_000, 5 * 1024 * 1024, 10_000, 1024 * 1024);
        assert!(matches!(status, BackPressureStatus::Normal));
    }

    #[test]
    fn test_check_addition_would_exceed() {
        let controller = BackPressureController::default();

        // Adding 30K ops to 80K current = 110K total (above hard)
        let status = controller.check_addition(80_000, 5 * 1024 * 1024, 30_000, 1024 * 1024);
        assert!(matches!(status, BackPressureStatus::HardLimit { .. }));
    }

    #[test]
    fn test_utilization() {
        let controller = BackPressureController::default();

        let (op_pct, byte_pct) = controller.utilization(50_000, 5 * 1024 * 1024);
        assert_eq!(op_pct, 50);
        assert_eq!(byte_pct, 50);

        let (op_pct, byte_pct) = controller.utilization(100_000, 10 * 1024 * 1024);
        assert_eq!(op_pct, 100);
        assert_eq!(byte_pct, 100);
    }

    #[test]
    fn test_reason_display() {
        let op_reason = BackPressureReason::OperationCount {
            current: 100_000,
            limit: 100_000,
        };
        assert!(format!("{op_reason}").contains("100000"));

        let byte_reason = BackPressureReason::ByteSize {
            current: 10 * 1024 * 1024,
            limit: 10 * 1024 * 1024,
        };
        assert!(format!("{byte_reason}").contains("10.00MB"));

        let both_reason = BackPressureReason::Both {
            op_usage_percent: 100,
            byte_usage_percent: 100,
        };
        assert!(format!("{both_reason}").contains("100%"));
    }

    #[test]
    fn test_status_display() {
        let normal = BackPressureStatus::Normal;
        assert!(format!("{normal}").contains("normal"));

        let soft = BackPressureStatus::SoftLimit {
            reason: BackPressureReason::OperationCount {
                current: 85_000,
                limit: 80_000,
            },
        };
        assert!(format!("{soft}").contains("soft limit"));

        let hard = BackPressureStatus::HardLimit {
            reason: BackPressureReason::OperationCount {
                current: 100_000,
                limit: 100_000,
            },
        };
        assert!(format!("{hard}").contains("HARD LIMIT"));
    }

    #[test]
    fn test_set_limits() {
        let mut controller = BackPressureController::default();
        assert_eq!(controller.limits().max_operations, 100_000);

        controller.set_limits(BackPressureLimits::new(50_000, 5 * 1024 * 1024));
        assert_eq!(controller.limits().max_operations, 50_000);
    }

    #[test]
    fn test_soft_ratio_clamping() {
        // Ratio should be clamped to 0.0-1.0
        let limits = BackPressureLimits::new(100_000, 10 * 1024 * 1024).with_soft_ratio(1.5);
        assert!((limits.soft_limit_ratio - 1.0).abs() < f64::EPSILON);

        let limits = BackPressureLimits::new(100_000, 10 * 1024 * 1024).with_soft_ratio(-0.5);
        assert!((limits.soft_limit_ratio - 0.0).abs() < f64::EPSILON);
    }
}
