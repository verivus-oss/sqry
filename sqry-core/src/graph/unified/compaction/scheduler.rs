//! Compaction Scheduler: Threshold-based compaction triggering.
//!
//! This module implements automatic compaction scheduling based on configurable
//! thresholds for delta buffer size and tombstone ratio.
//!
//! # Design (FR-31, FR-46)
//!
//! - **Operation threshold**: Trigger compaction after N delta operations
//! - **Tombstone ratio**: Trigger compaction when removes exceed threshold
//! - **Configurable**: All thresholds are configurable with sensible defaults
//!
//! # Thresholds (CP-8)
//!
//! | Condition | Default | Description |
//! |-----------|---------|-------------|
//! | Operations | 1,000 | Delta operations before compaction |
//! | Tombstone ratio | 20% | Remove operations as percentage of total |
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::compaction::scheduler::{
//!     CompactionScheduler, CompactionThresholds, CompactionTrigger,
//! };
//!
//! let scheduler = CompactionScheduler::default();
//! let trigger = scheduler.should_compact(1500, 200, 100);
//!
//! match trigger {
//!     CompactionTrigger::None => { /* no compaction needed */ }
//!     CompactionTrigger::OperationThreshold { count, threshold } => {
//!         // Start compaction due to operation count
//!     }
//!     CompactionTrigger::TombstoneRatio { ratio, threshold } => {
//!         // Start compaction due to tombstone ratio
//!     }
//! }
//! ```

use std::fmt;

/// Configuration thresholds for compaction scheduling.
#[derive(Debug, Clone, Copy)]
pub struct CompactionThresholds {
    /// Number of delta operations before triggering compaction.
    /// Default: 1000 (CP-8)
    pub operation_threshold: u64,

    /// Tombstone ratio (as percentage 0-100) before triggering compaction.
    /// Default: 20 (20% removes)
    pub tombstone_ratio_threshold: u8,

    /// Minimum operations before tombstone ratio is considered.
    /// Prevents premature compaction on small deltas.
    /// Default: 100
    pub min_ops_for_ratio_check: u64,
}

impl Default for CompactionThresholds {
    fn default() -> Self {
        Self {
            operation_threshold: 1000,     // CP-8: 1000 ops
            tombstone_ratio_threshold: 20, // 20% removes
            min_ops_for_ratio_check: 100,  // min ops before ratio check
        }
    }
}

impl CompactionThresholds {
    /// Creates new thresholds with custom values.
    #[must_use]
    pub fn new(operation_threshold: u64, tombstone_ratio_threshold: u8) -> Self {
        Self {
            operation_threshold,
            tombstone_ratio_threshold,
            min_ops_for_ratio_check: 100,
        }
    }

    /// Creates thresholds with custom minimum for ratio check.
    #[must_use]
    pub fn with_min_ops(mut self, min_ops: u64) -> Self {
        self.min_ops_for_ratio_check = min_ops;
        self
    }
}

/// Reason why compaction was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTrigger {
    /// No compaction needed.
    None,

    /// Operation count exceeded threshold.
    OperationThreshold {
        /// Current operation count.
        count: u64,
        /// Configured threshold.
        threshold: u64,
    },

    /// Tombstone ratio exceeded threshold.
    TombstoneRatio {
        /// Current tombstone ratio (0-100).
        ratio: u8,
        /// Configured threshold (0-100).
        threshold: u8,
    },
}

impl fmt::Display for CompactionTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "no compaction needed"),
            Self::OperationThreshold { count, threshold } => {
                write!(
                    f,
                    "operation threshold exceeded: {count} ops >= {threshold} threshold"
                )
            }
            Self::TombstoneRatio { ratio, threshold } => {
                write!(
                    f,
                    "tombstone ratio exceeded: {ratio}% >= {threshold}% threshold"
                )
            }
        }
    }
}

/// Scheduler for automatic compaction based on thresholds.
///
/// The scheduler monitors delta buffer statistics and determines when
/// compaction should be triggered based on configurable thresholds.
#[derive(Debug, Clone, Default)]
pub struct CompactionScheduler {
    /// Configuration thresholds.
    thresholds: CompactionThresholds,
}

impl CompactionScheduler {
    /// Creates a new scheduler with custom thresholds.
    #[must_use]
    pub fn new(thresholds: CompactionThresholds) -> Self {
        Self { thresholds }
    }

    /// Returns the current thresholds configuration.
    #[must_use]
    pub fn thresholds(&self) -> &CompactionThresholds {
        &self.thresholds
    }

    /// Updates the thresholds configuration.
    pub fn set_thresholds(&mut self, thresholds: CompactionThresholds) {
        self.thresholds = thresholds;
    }

    /// Determines if compaction should be triggered.
    ///
    /// # Arguments
    ///
    /// * `total_ops` - Total number of delta operations (adds + removes)
    /// * `add_count` - Number of Add operations
    /// * `remove_count` - Number of Remove operations
    ///
    /// # Returns
    ///
    /// Returns the trigger reason, or `CompactionTrigger::None` if no
    /// compaction is needed.
    #[must_use]
    pub fn should_compact(
        &self,
        total_ops: u64,
        _add_count: u64,
        remove_count: u64,
    ) -> CompactionTrigger {
        // Check operation threshold first (most common trigger)
        if total_ops >= self.thresholds.operation_threshold {
            return CompactionTrigger::OperationThreshold {
                count: total_ops,
                threshold: self.thresholds.operation_threshold,
            };
        }

        // Check tombstone ratio (only if we have enough ops)
        if total_ops >= self.thresholds.min_ops_for_ratio_check && total_ops > 0 {
            let ratio = Self::calculate_ratio(remove_count, total_ops);
            if ratio >= self.thresholds.tombstone_ratio_threshold {
                return CompactionTrigger::TombstoneRatio {
                    ratio,
                    threshold: self.thresholds.tombstone_ratio_threshold,
                };
            }
        }

        CompactionTrigger::None
    }

    /// Checks if compaction should be triggered using buffer state snapshot.
    ///
    /// This is a convenience method that extracts the relevant counts from
    /// a `BufferStateSnapshot` for threshold checking.
    #[must_use]
    pub fn should_compact_from_snapshot(
        &self,
        committed_delta: u64,
        committed_removes: u64,
    ) -> CompactionTrigger {
        // Estimate adds as delta minus removes (could be higher due to overwrites)
        let estimated_adds = committed_delta.saturating_sub(committed_removes);
        self.should_compact(committed_delta, estimated_adds, committed_removes)
    }

    /// Calculates tombstone ratio as percentage.
    #[inline]
    fn calculate_ratio(removes: u64, total: u64) -> u8 {
        if total == 0 {
            return 0;
        }
        // Calculate percentage, cap at 100
        let ratio = (removes.saturating_mul(100)) / total;
        ratio.min(100) as u8
    }
}

/// Statistics about scheduler decisions.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchedulerStats {
    /// Number of times `should_compact` was called.
    pub check_count: u64,
    /// Number of times compaction was triggered.
    pub trigger_count: u64,
    /// Number of operation threshold triggers.
    pub operation_triggers: u64,
    /// Number of tombstone ratio triggers.
    pub ratio_triggers: u64,
}

impl SchedulerStats {
    /// Records a compaction check result.
    pub fn record(&mut self, trigger: &CompactionTrigger) {
        self.check_count += 1;
        match trigger {
            CompactionTrigger::None => {}
            CompactionTrigger::OperationThreshold { .. } => {
                self.trigger_count += 1;
                self.operation_triggers += 1;
            }
            CompactionTrigger::TombstoneRatio { .. } => {
                self.trigger_count += 1;
                self.ratio_triggers += 1;
            }
        }
    }

    /// Returns the trigger rate as a percentage (0-100).
    #[must_use]
    pub fn trigger_rate(&self) -> u8 {
        if self.check_count == 0 {
            return 0;
        }
        let rate = (self.trigger_count.saturating_mul(100)) / self.check_count;
        rate.min(100) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_thresholds() {
        let thresholds = CompactionThresholds::default();
        assert_eq!(thresholds.operation_threshold, 1000);
        assert_eq!(thresholds.tombstone_ratio_threshold, 20);
        assert_eq!(thresholds.min_ops_for_ratio_check, 100);
    }

    #[test]
    fn test_custom_thresholds() {
        let thresholds = CompactionThresholds::new(500, 30);
        assert_eq!(thresholds.operation_threshold, 500);
        assert_eq!(thresholds.tombstone_ratio_threshold, 30);
    }

    #[test]
    fn test_thresholds_with_min_ops() {
        let thresholds = CompactionThresholds::new(500, 30).with_min_ops(50);
        assert_eq!(thresholds.min_ops_for_ratio_check, 50);
    }

    #[test]
    fn test_scheduler_default() {
        let scheduler = CompactionScheduler::default();
        assert_eq!(scheduler.thresholds().operation_threshold, 1000);
    }

    #[test]
    fn test_no_compaction_below_threshold() {
        let scheduler = CompactionScheduler::default();
        // 500 ops with 50 removes = 10% tombstone ratio < 20% threshold
        let trigger = scheduler.should_compact(500, 450, 50);
        assert_eq!(trigger, CompactionTrigger::None);
    }

    #[test]
    fn test_operation_threshold_trigger() {
        let scheduler = CompactionScheduler::default();
        let trigger = scheduler.should_compact(1000, 800, 200);
        match trigger {
            CompactionTrigger::OperationThreshold { count, threshold } => {
                assert_eq!(count, 1000);
                assert_eq!(threshold, 1000);
            }
            _ => panic!("expected OperationThreshold trigger"),
        }
    }

    #[test]
    fn test_operation_threshold_exceeded() {
        let scheduler = CompactionScheduler::default();
        let trigger = scheduler.should_compact(1500, 1200, 300);
        match trigger {
            CompactionTrigger::OperationThreshold { count, .. } => {
                assert_eq!(count, 1500);
            }
            _ => panic!("expected OperationThreshold trigger"),
        }
    }

    #[test]
    fn test_tombstone_ratio_trigger() {
        let thresholds = CompactionThresholds::new(2000, 20);
        let scheduler = CompactionScheduler::new(thresholds);

        // 150 total ops, 50 removes = 33% tombstone ratio > 20% threshold
        let trigger = scheduler.should_compact(150, 100, 50);
        match trigger {
            CompactionTrigger::TombstoneRatio { ratio, threshold } => {
                assert_eq!(ratio, 33);
                assert_eq!(threshold, 20);
            }
            _ => panic!("expected TombstoneRatio trigger"),
        }
    }

    #[test]
    fn test_tombstone_ratio_below_min_ops() {
        let scheduler = CompactionScheduler::default();

        // 50 ops is below min_ops_for_ratio_check (100), so ratio not checked
        // Even with 100% removes, shouldn't trigger
        let trigger = scheduler.should_compact(50, 0, 50);
        assert_eq!(trigger, CompactionTrigger::None);
    }

    #[test]
    fn test_operation_threshold_takes_priority() {
        let thresholds = CompactionThresholds::new(100, 20);
        let scheduler = CompactionScheduler::new(thresholds);

        // Both thresholds exceeded, but operation threshold checked first
        // 100 ops, 80 removes = 80% tombstone ratio, but operation threshold triggers first
        let trigger = scheduler.should_compact(100, 20, 80);
        match trigger {
            CompactionTrigger::OperationThreshold { .. } => {}
            _ => panic!("expected OperationThreshold trigger (takes priority)"),
        }
    }

    #[test]
    fn test_trigger_display() {
        let none = CompactionTrigger::None;
        assert_eq!(format!("{none}"), "no compaction needed");

        let ops = CompactionTrigger::OperationThreshold {
            count: 1500,
            threshold: 1000,
        };
        assert!(format!("{ops}").contains("1500 ops"));
        assert!(format!("{ops}").contains("1000 threshold"));

        let ratio = CompactionTrigger::TombstoneRatio {
            ratio: 25,
            threshold: 20,
        };
        assert!(format!("{ratio}").contains("25%"));
        assert!(format!("{ratio}").contains("20% threshold"));
    }

    #[test]
    fn test_scheduler_stats_recording() {
        let mut stats = SchedulerStats::default();

        stats.record(&CompactionTrigger::None);
        assert_eq!(stats.check_count, 1);
        assert_eq!(stats.trigger_count, 0);

        stats.record(&CompactionTrigger::OperationThreshold {
            count: 1000,
            threshold: 1000,
        });
        assert_eq!(stats.check_count, 2);
        assert_eq!(stats.trigger_count, 1);
        assert_eq!(stats.operation_triggers, 1);

        stats.record(&CompactionTrigger::TombstoneRatio {
            ratio: 30,
            threshold: 20,
        });
        assert_eq!(stats.check_count, 3);
        assert_eq!(stats.trigger_count, 2);
        assert_eq!(stats.ratio_triggers, 1);
    }

    #[test]
    fn test_trigger_rate_calculation() {
        let mut stats = SchedulerStats::default();
        assert_eq!(stats.trigger_rate(), 0);

        // 1 trigger out of 4 checks = 25%
        stats.check_count = 4;
        stats.trigger_count = 1;
        assert_eq!(stats.trigger_rate(), 25);

        // 3 triggers out of 4 checks = 75%
        stats.trigger_count = 3;
        assert_eq!(stats.trigger_rate(), 75);
    }

    #[test]
    fn test_should_compact_from_snapshot() {
        let scheduler = CompactionScheduler::default();

        // Below threshold (500 ops, 50 removes = 10% < 20%)
        let trigger = scheduler.should_compact_from_snapshot(500, 50);
        assert_eq!(trigger, CompactionTrigger::None);

        // Above threshold (1500 ops >= 1000 threshold)
        let trigger = scheduler.should_compact_from_snapshot(1500, 300);
        match trigger {
            CompactionTrigger::OperationThreshold { count, .. } => {
                assert_eq!(count, 1500);
            }
            _ => panic!("expected OperationThreshold"),
        }
    }

    #[test]
    fn test_calculate_ratio_edge_cases() {
        // Zero total
        assert_eq!(CompactionScheduler::calculate_ratio(10, 0), 0);

        // All removes
        assert_eq!(CompactionScheduler::calculate_ratio(100, 100), 100);

        // No removes
        assert_eq!(CompactionScheduler::calculate_ratio(0, 100), 0);

        // Normal case
        assert_eq!(CompactionScheduler::calculate_ratio(25, 100), 25);
    }

    #[test]
    fn test_set_thresholds() {
        let mut scheduler = CompactionScheduler::default();
        assert_eq!(scheduler.thresholds().operation_threshold, 1000);

        scheduler.set_thresholds(CompactionThresholds::new(500, 30));
        assert_eq!(scheduler.thresholds().operation_threshold, 500);
        assert_eq!(scheduler.thresholds().tombstone_ratio_threshold, 30);
    }
}
