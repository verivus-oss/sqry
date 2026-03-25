//! Cache budget controller for enforcing memory limits.
//!
//! This module provides adaptive cache budgeting to prevent unbounded memory growth
//! while maintaining high cache hit rates. The controller uses synchronous clamping
//! logic to enforce entry and memory limits across multiple caches.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Configuration for cache budget limits
#[derive(Debug, Clone)]
pub struct BudgetConfig {
    /// Maximum number of entries across all caches (default: 10,000)
    pub max_entries: usize,

    /// Maximum memory in bytes (default: 100 MB)
    /// This is a soft limit based on estimated size tracking
    pub max_memory_bytes: usize,

    /// Estimated bytes per symbol (default: 512 bytes)
    /// Used for rough memory estimation when actual size unavailable
    pub estimated_symbol_size: usize,

    /// Estimated bytes per parse tree (default: 2048 bytes)
    pub estimated_parse_tree_size: usize,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_memory_bytes: 100 * 1024 * 1024, // 100 MB
            estimated_symbol_size: 512,
            estimated_parse_tree_size: 2048,
        }
    }
}

/// Budget controller for tracking and enforcing cache limits
pub struct CacheBudgetController {
    config: BudgetConfig,

    /// Current total entries across all caches
    total_entries: AtomicUsize,

    /// Estimated total memory usage in bytes
    estimated_memory: AtomicUsize,

    /// Number of clamp operations performed
    clamp_count: AtomicUsize,
}

impl CacheBudgetController {
    /// Create a new budget controller with default configuration
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(BudgetConfig::default())
    }

    /// Create a new budget controller with custom configuration
    #[must_use]
    pub fn with_config(config: BudgetConfig) -> Self {
        Self {
            config,
            total_entries: AtomicUsize::new(0),
            estimated_memory: AtomicUsize::new(0),
            clamp_count: AtomicUsize::new(0),
        }
    }

    /// Record an insert operation
    ///
    /// # Arguments
    ///
    /// * `entry_count` - Number of entries being inserted
    /// * `estimated_bytes` - Estimated memory size of the entries
    pub fn record_insert(&self, entry_count: usize, estimated_bytes: usize) {
        self.total_entries.fetch_add(entry_count, Ordering::Relaxed);
        self.estimated_memory
            .fetch_add(estimated_bytes, Ordering::Relaxed);
    }

    /// Record a remove/eviction operation
    ///
    /// # Arguments
    ///
    /// * `entry_count` - Number of entries being removed
    /// * `estimated_bytes` - Estimated memory size of the entries
    pub fn record_remove(&self, entry_count: usize, estimated_bytes: usize) {
        self.total_entries.fetch_sub(entry_count, Ordering::Relaxed);
        self.estimated_memory
            .fetch_sub(estimated_bytes, Ordering::Relaxed);
    }

    /// Check if budget limits are exceeded
    ///
    /// Returns `ClampAction` indicating how to adjust caches
    pub fn check_budget(&self) -> ClampAction {
        let entries = self.total_entries.load(Ordering::Relaxed);
        let memory = self.estimated_memory.load(Ordering::Relaxed);

        let entries_over = entries.saturating_sub(self.config.max_entries);
        let memory_over = memory.saturating_sub(self.config.max_memory_bytes);

        if entries_over > 0 || memory_over > 0 {
            // Calculate how many entries to evict based on whichever limit is more exceeded
            let entries_to_evict_for_count = entries_over;
            let entries_to_evict_for_memory = if memory_over > 0 {
                // Estimate entries needed to free memory (conservative)
                (memory_over / self.config.estimated_symbol_size).max(1)
            } else {
                0
            };

            let entries_to_evict = entries_to_evict_for_count.max(entries_to_evict_for_memory);

            ClampAction::Evict {
                count: entries_to_evict,
                reason: if entries_over > memory_over {
                    ClampReason::EntryLimit
                } else {
                    ClampReason::MemoryLimit
                },
            }
        } else {
            ClampAction::None
        }
    }

    /// Record that a clamp operation was performed
    pub fn record_clamp(&self) {
        self.clamp_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Get current budget statistics
    pub fn stats(&self) -> BudgetStats {
        BudgetStats {
            total_entries: self.total_entries.load(Ordering::Relaxed),
            estimated_memory_bytes: self.estimated_memory.load(Ordering::Relaxed),
            clamp_count: self.clamp_count.load(Ordering::Relaxed),
            max_entries: self.config.max_entries,
            max_memory_bytes: self.config.max_memory_bytes,
        }
    }

    /// Reset budget tracking (used when clearing all caches)
    pub fn reset(&self) {
        self.total_entries.store(0, Ordering::Relaxed);
        self.estimated_memory.store(0, Ordering::Relaxed);
        // Note: We don't reset clamp_count as it's a cumulative statistic
    }

    /// Get the current configuration
    pub fn config(&self) -> &BudgetConfig {
        &self.config
    }
}

impl Default for CacheBudgetController {
    fn default() -> Self {
        Self::new()
    }
}

/// Action to take when budget is exceeded
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClampAction {
    /// No action needed, within budget
    None,

    /// Evict entries to get back within budget
    Evict {
        /// Number of entries to evict
        count: usize,
        /// Reason for clamping
        reason: ClampReason,
    },
}

/// Reason why clamping is needed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClampReason {
    /// Entry count limit exceeded
    EntryLimit,

    /// Memory limit exceeded
    MemoryLimit,
}

/// Statistics about current budget usage
#[derive(Debug, Clone)]
pub struct BudgetStats {
    /// Current total entries
    pub total_entries: usize,

    /// Estimated current memory usage
    pub estimated_memory_bytes: usize,

    /// Number of times clamping was performed
    pub clamp_count: usize,

    /// Maximum allowed entries
    pub max_entries: usize,

    /// Maximum allowed memory
    pub max_memory_bytes: usize,
}

impl BudgetStats {
    /// Calculate entry utilization as a percentage (0.0-1.0)
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "Utilization percentages are informational; precision is sufficient"
    )]
    pub fn entry_utilization(&self) -> f64 {
        if self.max_entries == 0 {
            0.0
        } else {
            self.total_entries as f64 / self.max_entries as f64
        }
    }

    /// Calculate memory utilization as a percentage (0.0-1.0)
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "Utilization percentages are informational; precision is sufficient"
    )]
    pub fn memory_utilization(&self) -> f64 {
        if self.max_memory_bytes == 0 {
            0.0
        } else {
            self.estimated_memory_bytes as f64 / self.max_memory_bytes as f64
        }
    }

    /// Check if budget is exceeded
    #[must_use]
    pub fn is_over_budget(&self) -> bool {
        self.total_entries > self.max_entries || self.estimated_memory_bytes > self.max_memory_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn test_default_config() {
        let config = BudgetConfig::default();
        assert_eq!(config.max_entries, 10_000);
        assert_eq!(config.max_memory_bytes, 100 * 1024 * 1024);
        assert_eq!(config.estimated_symbol_size, 512);
    }

    #[test]
    fn test_record_insert() {
        let controller = CacheBudgetController::new();

        controller.record_insert(10, 5120);

        let stats = controller.stats();
        assert_eq!(stats.total_entries, 10);
        assert_eq!(stats.estimated_memory_bytes, 5120);
    }

    #[test]
    fn test_record_remove() {
        let controller = CacheBudgetController::new();

        controller.record_insert(20, 10240);
        controller.record_remove(5, 2560);

        let stats = controller.stats();
        assert_eq!(stats.total_entries, 15);
        assert_eq!(stats.estimated_memory_bytes, 7680);
    }

    #[test]
    fn test_budget_within_limits() {
        let config = BudgetConfig {
            max_entries: 100,
            max_memory_bytes: 10240,
            ..Default::default()
        };
        let controller = CacheBudgetController::with_config(config);

        controller.record_insert(50, 5000);

        let action = controller.check_budget();
        assert_eq!(action, ClampAction::None);
    }

    #[test]
    fn test_budget_entry_limit_exceeded() {
        let config = BudgetConfig {
            max_entries: 100,
            max_memory_bytes: 100_000,
            ..Default::default()
        };
        let controller = CacheBudgetController::with_config(config);

        controller.record_insert(150, 5000);

        let action = controller.check_budget();
        match action {
            ClampAction::Evict { count, reason } => {
                assert_eq!(count, 50);
                assert_eq!(reason, ClampReason::EntryLimit);
            }
            ClampAction::None => panic!("Expected eviction"),
        }
    }

    #[test]
    fn test_budget_memory_limit_exceeded() {
        let config = BudgetConfig {
            max_entries: 1000,
            max_memory_bytes: 10_000,
            estimated_symbol_size: 512,
            ..Default::default()
        };
        let controller = CacheBudgetController::with_config(config);

        controller.record_insert(50, 15_000);

        let action = controller.check_budget();
        match action {
            ClampAction::Evict { count, reason } => {
                assert!(count > 0);
                assert_eq!(reason, ClampReason::MemoryLimit);
            }
            ClampAction::None => panic!("Expected eviction"),
        }
    }

    #[test]
    fn test_clamp_count_tracking() {
        let controller = CacheBudgetController::new();

        assert_eq!(controller.stats().clamp_count, 0);

        controller.record_clamp();
        controller.record_clamp();

        assert_eq!(controller.stats().clamp_count, 2);
    }

    #[test]
    fn test_reset() {
        let controller = CacheBudgetController::new();

        controller.record_insert(100, 5000);
        controller.record_clamp();

        controller.reset();

        let stats = controller.stats();
        assert_eq!(stats.total_entries, 0);
        assert_eq!(stats.estimated_memory_bytes, 0);
        assert_eq!(stats.clamp_count, 1); // Clamp count not reset
    }

    #[test]
    fn test_budget_stats_utilization() {
        let config = BudgetConfig {
            max_entries: 100,
            max_memory_bytes: 10_000,
            ..Default::default()
        };
        let controller = CacheBudgetController::with_config(config);

        controller.record_insert(50, 5_000);

        let stats = controller.stats();
        assert_abs_diff_eq!(stats.entry_utilization(), 0.5, epsilon = 1e-10);
        assert_abs_diff_eq!(stats.memory_utilization(), 0.5, epsilon = 1e-10);
        assert!(!stats.is_over_budget());
    }

    #[test]
    fn test_budget_stats_over_budget() {
        let config = BudgetConfig {
            max_entries: 100,
            max_memory_bytes: 10_000,
            ..Default::default()
        };
        let controller = CacheBudgetController::with_config(config);

        controller.record_insert(150, 5_000);

        let stats = controller.stats();
        assert!(stats.is_over_budget());
        assert!(stats.entry_utilization() > 1.0);
    }

    #[test]
    fn test_multiple_inserts_and_removes() {
        let controller = CacheBudgetController::new();

        controller.record_insert(10, 1000);
        controller.record_insert(20, 2000);
        controller.record_remove(5, 500);
        controller.record_insert(15, 1500);
        controller.record_remove(10, 1000);

        let stats = controller.stats();
        assert_eq!(stats.total_entries, 30);
        assert_eq!(stats.estimated_memory_bytes, 3000);
    }
}
