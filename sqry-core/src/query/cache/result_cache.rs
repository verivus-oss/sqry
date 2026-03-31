//! Result cache: `CacheKey` → `Vec<NodeId>`

use super::budget::{CacheBudgetController, ClampAction};
use super::types::{CacheKey, CacheStats};
use crate::cache::CacheConfig;
use crate::cache::policy::{
    CacheAdmission, CachePolicy, CachePolicyConfig, CachePolicyKind, build_cache_policy,
};
use crate::graph::unified::node::NodeId;
use log::debug;
use lru::LruCache;
use parking_lot::RwLock;
use std::num::NonZeroUsize;
use std::sync::Arc;

/// Result cache: `CacheKey` → `Vec<NodeId>`
pub struct ResultCache {
    /// LRU cache: `CacheKey` directly (no manual hashing)
    cache: RwLock<LruCache<CacheKey, Vec<NodeId>>>,

    /// Statistics
    stats: RwLock<CacheStats>,

    /// Optional budget controller for memory limiting
    budget_controller: Option<Arc<CacheBudgetController>>,

    /// Adaptive eviction policy shared with core cache
    policy: Arc<dyn CachePolicy<CacheKey>>,
}

impl ResultCache {
    /// Create new result cache with capacity
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self::with_budget(cap, None)
    }

    /// Create new result cache with optional budget controller
    #[must_use]
    pub fn with_budget(
        capacity: usize,
        budget_controller: Option<Arc<CacheBudgetController>>,
    ) -> Self {
        let normalized_capacity = capacity.max(1);
        let cap = NonZeroUsize::new(normalized_capacity).unwrap_or(NonZeroUsize::MIN);
        let (kind, window_ratio) = Self::policy_params_from_env();
        let policy_config = CachePolicyConfig::new(kind, normalized_capacity as u64, window_ratio);
        Self {
            cache: RwLock::new(LruCache::new(cap)),
            stats: RwLock::new(CacheStats::default()),
            budget_controller,
            policy: build_cache_policy(&policy_config),
        }
    }

    /// Get cached results
    pub fn get(&self, key: &CacheKey) -> Option<Vec<NodeId>> {
        self.handle_policy_evictions();
        let mut cache = self.cache.write();

        if let Some(results) = cache.get(key) {
            // Cache hit: update stats and return clone
            let mut stats = self.stats.write();
            stats.hits += 1;
            drop(stats);
            let _ = self.policy.record_hit(key);
            Some(results.clone())
        } else {
            // Cache miss
            let mut stats = self.stats.write();
            stats.misses += 1;
            None
        }
    }

    /// Insert results into cache with LRU eviction and budget enforcement
    pub fn insert(&self, key: CacheKey, results: Vec<NodeId>) {
        self.handle_policy_evictions();
        let cache = self.cache.read();
        let is_update = cache.contains(&key);
        drop(cache);

        let estimated_bytes = if let Some(budget) = &self.budget_controller {
            results.len() * budget.config().estimated_symbol_size
        } else {
            0
        };

        let mut budget_recorded = false;
        if let Some(budget) = &self.budget_controller {
            if !is_update {
                budget.record_insert(1, estimated_bytes);
                budget_recorded = true;
            }

            match budget.check_budget() {
                ClampAction::Evict { count, .. } => {
                    self.evict_entries(count);
                    budget.record_clamp();
                }
                ClampAction::None => {}
            }
        }

        if matches!(
            self.policy.admit(&key, estimated_bytes as u64),
            CacheAdmission::Rejected
        ) {
            if let Some(budget) = &self.budget_controller
                && budget_recorded
            {
                budget.record_remove(1, estimated_bytes);
            }
            debug!(
                "result cache policy {:?} rejected entry",
                self.policy.kind()
            );
            return;
        }

        let mut cache = self.cache.write();
        if cache.len() == cache.cap().get()
            && !is_update
            && let Some((evicted_key, _)) = cache.pop_lru()
        {
            self.policy.invalidate(&evicted_key);
            {
                let mut stats = self.stats.write();
                stats.evictions += 1;
            }
            if let Some(budget) = &self.budget_controller {
                budget.record_remove(1, budget.config().estimated_symbol_size);
            }
        }

        cache.put(key, results);
    }

    /// Evict entries to reduce cache size
    fn evict_entries(&self, count: usize) {
        let mut cache = self.cache.write();
        let to_evict = count.min(cache.len());

        for _ in 0..to_evict {
            if let Some((evicted_key, _)) = cache.pop_lru() {
                self.policy.invalidate(&evicted_key);
                // Record eviction
                let mut stats = self.stats.write();
                stats.evictions += 1;

                // Update budget tracking
                if let Some(budget) = &self.budget_controller {
                    budget.record_remove(1, budget.config().estimated_symbol_size);
                }
            }
        }
    }

    /// Clear all cache entries (on invalidation)
    pub fn clear(&self) {
        let mut cache = self.cache.write();
        cache.clear();

        // Reset budget tracking
        if let Some(budget) = &self.budget_controller {
            budget.reset();
        }

        self.policy.reset();
    }

    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        self.stats.read().clone()
    }

    /// Get current cache size
    pub fn len(&self) -> usize {
        self.cache.read().len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn handle_policy_evictions(&self) {
        let evicted = self.policy.drain_evictions();
        if evicted.is_empty() {
            return;
        }
        let mut cache = self.cache.write();
        for eviction in evicted {
            if cache.pop(&eviction.key).is_some() {
                {
                    let mut stats = self.stats.write();
                    stats.evictions += 1;
                }
                if let Some(budget) = &self.budget_controller {
                    budget.record_remove(1, budget.config().estimated_symbol_size);
                }
            }
        }
    }

    fn policy_params_from_env() -> (CachePolicyKind, f32) {
        let cfg = CacheConfig::from_env();
        (cfg.policy_kind(), cfg.policy_window_ratio())
    }

    #[cfg(test)]
    fn with_policy_kind(
        capacity: usize,
        budget_controller: Option<Arc<CacheBudgetController>>,
        kind: CachePolicyKind,
    ) -> Self {
        Self {
            cache: RwLock::new(LruCache::new(
                NonZeroUsize::new(capacity.max(1)).unwrap_or(NonZeroUsize::MIN),
            )),
            stats: RwLock::new(CacheStats::default()),
            budget_controller,
            policy: build_cache_policy(&CachePolicyConfig::new(
                kind,
                capacity.max(1) as u64,
                CacheConfig::DEFAULT_POLICY_WINDOW_RATIO,
            )),
        }
    }

    #[cfg(test)]
    fn policy_metrics(&self) -> crate::cache::policy::CachePolicyMetrics {
        self.policy.stats()
    }
}

// Thread safety: parking_lot::RwLock<T> is Send + Sync when T: Send + Sync
// No manual unsafe impl needed

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::policy::CachePolicyKind;

    fn create_test_node_id(index: u32) -> NodeId {
        NodeId::new(index, 1)
    }

    #[test]
    fn result_cache_hit() {
        let cache = ResultCache::new(100);
        let key = CacheKey {
            query_hash: 123,
            plugin_hash: 456,
            file_set_hash: 789,
            root_path_hash: 101,
            repo_filter_hash: 0,
        };
        let results = vec![create_test_node_id(1)];

        // Insert
        cache.insert(key.clone(), results.clone());

        // Hit
        let cached = cache.get(&key).unwrap();
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0], NodeId::new(1, 1));

        // Stats
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 0);
    }

    #[test]
    fn result_cache_miss() {
        let cache = ResultCache::new(100);
        let key = CacheKey {
            query_hash: 123,
            plugin_hash: 456,
            file_set_hash: 789,
            root_path_hash: 101,
            repo_filter_hash: 0,
        };

        // Miss
        let result = cache.get(&key);
        assert!(result.is_none());

        // Stats
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn result_cache_eviction() {
        let cache = ResultCache::new(2);

        let key1 = CacheKey {
            query_hash: 1,
            plugin_hash: 0,
            file_set_hash: 0,
            root_path_hash: 0,
            repo_filter_hash: 0,
        };
        let key2 = CacheKey {
            query_hash: 2,
            plugin_hash: 0,
            file_set_hash: 0,
            root_path_hash: 0,
            repo_filter_hash: 0,
        };
        let key3 = CacheKey {
            query_hash: 3,
            plugin_hash: 0,
            file_set_hash: 0,
            root_path_hash: 0,
            repo_filter_hash: 0,
        };

        cache.insert(key1.clone(), vec![create_test_node_id(1)]);
        cache.insert(key2.clone(), vec![create_test_node_id(2)]);
        cache.insert(key3.clone(), vec![create_test_node_id(3)]); // Evicts key1

        assert!(cache.get(&key1).is_none()); // Evicted
        assert!(cache.get(&key2).is_some());
        assert!(cache.get(&key3).is_some());

        let stats = cache.stats();
        assert_eq!(stats.evictions, 1);
    }

    #[test]
    fn result_cache_clear() {
        let cache = ResultCache::new(100);
        let key = CacheKey {
            query_hash: 1,
            plugin_hash: 0,
            file_set_hash: 0,
            root_path_hash: 0,
            repo_filter_hash: 0,
        };

        cache.insert(key.clone(), vec![create_test_node_id(10)]);

        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn result_cache_with_budget_enforcement() {
        use super::super::{BudgetConfig, CacheBudgetController};
        use std::sync::Arc;

        // Create budget controller with small limits
        let budget_config = BudgetConfig {
            max_entries: 5,
            max_memory_bytes: 10_000,
            estimated_symbol_size: 512,
            ..Default::default()
        };
        let budget = Arc::new(CacheBudgetController::with_config(budget_config));

        // Create cache with budget
        let cache = ResultCache::with_budget(100, Some(Arc::clone(&budget)));

        // Insert 10 entries (should trigger budget enforcement)
        for i in 0..10 {
            let key = CacheKey {
                query_hash: i,
                plugin_hash: 0,
                file_set_hash: 0,
                root_path_hash: 0,
                repo_filter_hash: 0,
            };
            cache.insert(key, vec![create_test_node_id(i as u32)]);
        }

        // Cache should have evicted some entries to stay within budget
        let budget_stats = budget.stats();
        assert!(budget_stats.total_entries <= 5, "Budget should be enforced");
        assert!(
            budget_stats.clamp_count > 0,
            "Clamping should have occurred"
        );

        // Cache evictions should have been recorded
        let cache_stats = cache.stats();
        assert!(cache_stats.evictions > 0, "Evictions should be recorded");
    }

    #[test]
    fn result_cache_budget_reset_on_clear() {
        use super::super::CacheBudgetController;
        use std::sync::Arc;

        let budget = Arc::new(CacheBudgetController::new());
        let cache = ResultCache::with_budget(100, Some(Arc::clone(&budget)));

        // Insert some entries
        for i in 0..5 {
            let key = CacheKey {
                query_hash: i,
                plugin_hash: 0,
                file_set_hash: 0,
                root_path_hash: 0,
                repo_filter_hash: 0,
            };
            cache.insert(key, vec![create_test_node_id(i as u32)]);
        }

        let stats_before = budget.stats();
        assert!(stats_before.total_entries > 0);

        // Clear cache
        cache.clear();

        // Budget should be reset
        let stats_after = budget.stats();
        assert_eq!(stats_after.total_entries, 0);
        assert_eq!(stats_after.estimated_memory_bytes, 0);
    }

    #[test]
    fn result_cache_without_budget() {
        // Cache without budget should work normally
        let cache = ResultCache::new(10);

        for i in 0..15 {
            let key = CacheKey {
                query_hash: i,
                plugin_hash: 0,
                file_set_hash: 0,
                root_path_hash: 0,
                repo_filter_hash: 0,
            };
            cache.insert(key, vec![create_test_node_id(i as u32)]);
        }

        // Should only evict based on LRU capacity, not budget
        assert_eq!(cache.len(), 10);
        let stats = cache.stats();
        assert_eq!(stats.evictions, 5); // 15 - 10 = 5 evictions
    }

    #[test]
    fn tiny_lfu_preserves_hot_results() {
        let cache = ResultCache::with_policy_kind(3, None, CachePolicyKind::TinyLfu);
        let hot_key = CacheKey {
            query_hash: 1,
            plugin_hash: 0,
            file_set_hash: 0,
            root_path_hash: 0,
            repo_filter_hash: 0,
        };
        cache.insert(hot_key.clone(), vec![create_test_node_id(42)]);
        for _ in 0..5 {
            assert!(cache.get(&hot_key).is_some());
        }

        for i in 0_u64..20 {
            let key = CacheKey {
                query_hash: 100 + i,
                plugin_hash: 0,
                file_set_hash: 0,
                root_path_hash: 0,
                repo_filter_hash: 0,
            };
            cache.insert(key, vec![create_test_node_id(100 + i as u32)]);
        }

        assert!(
            cache.get(&hot_key).is_some(),
            "hot entry should survive cache churn"
        );
        assert!(cache.policy_metrics().lfu_rejects > 0);
    }
}
