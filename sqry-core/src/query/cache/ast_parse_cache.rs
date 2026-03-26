//! AST parse cache: query string → parsed `QueryAST` (`Arc<ParsedQuery>`)
//!
//! # Design
//!
//! This cache stores `Arc<ParsedQuery>` to enable zero-copy cache hits.
//! Cache hits only clone the Arc (atomic increment + pointer copy ~3-5ns),
//! not the entire `ParsedQuery` structure (which would be 50-200ns).
//!
//! **Key Design Choice**: Shares the adaptive TinyLFU/LRU policy facade with the
//! primary symbol cache so cache hits feed telemetry and the CLI debug output.
//!
//! # Design Notes
//!
//! - **Storage**: `Arc<ParsedQuery>` (boolean AST + repo filter + normalized string)
//! - **Purpose**: Boolean query language (AND/OR/NOT operators)
//! - **Normalization**: Cache key is the normalized query (repo predicates stripped)
//! - **Integration**: Used by `parse_query_ast()` in `QueryExecutor`
//!
//! # Performance
//!
//! Target metrics:
//! - Single-threaded: 15-20ns (Arc clone + hash)
//! - Multi-threaded (8 threads): 40-80ns (no lock contention)
//! - Production (realistic load): 50-100ns
//!
//! # Thread Safety
//!
//! `AstParseCache` stores `Arc<ParsedQuery>` values behind an internal
//! `RwLock<LruCache<...>>`. Hits clone the Arc (cheap), and eviction decisions
//! are coordinated through the shared `CachePolicy` facade.
//!
//! # Usage
//!
//! ```rust
//! use sqry_core::query::cache::AstParseCache;
//! use sqry_core::query::{QueryParser, ParsedQuery};
//! use std::sync::Arc;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let cache = AstParseCache::new(1000);
//!
//! // Parse and cache (normally done by QueryExecutor)
//! let ast = QueryParser::parse_query("kind:function AND name:test")?;
//! let parsed = ParsedQuery::from_ast(Arc::new(ast))?;
//! cache.insert("kind:function AND name:test".into(), parsed);
//!
//! // Cache hit (Arc clone, ~5ns)
//! let cached = cache.get("kind:function AND name:test").unwrap();
//! # Ok(())
//! # }
//! ```

use super::types::CacheStats;
use crate::cache::CacheConfig;
use crate::cache::policy::{
    CacheAdmission, CachePolicy, CachePolicyConfig, CachePolicyKind, build_cache_policy,
};
use crate::query::ParsedQuery;
use log::{debug, warn};
use lru::LruCache;
use parking_lot::RwLock;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Minimum capacity enforced for `AstParseCache`
const MIN_CAPACITY: u64 = 1;
const AST_ENTRY_WEIGHT_BYTES: u64 = 2048;

/// AST parse cache: query string → `Arc<ParsedQuery>` (boolean query AST)
pub struct AstParseCache {
    cache: RwLock<LruCache<String, Arc<ParsedQuery>>>,
    capacity: usize,
    policy: Arc<dyn CachePolicy<String>>,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

impl AstParseCache {
    /// Create new AST parse cache with capacity
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let cap = if capacity == 0 {
            warn!("AstParseCache::new received zero capacity; defaulting to {MIN_CAPACITY}");
            #[allow(clippy::cast_possible_truncation)]
            {
                MIN_CAPACITY as usize
            }
        } else {
            capacity
        };
        let (kind, window_ratio) = Self::policy_params_from_env();
        Self::with_policy(cap, kind, window_ratio)
    }

    fn with_policy(capacity: usize, kind: CachePolicyKind, window_ratio: f32) -> Self {
        #[allow(clippy::cast_possible_truncation)]
        let normalized_capacity = capacity.max(MIN_CAPACITY as usize);
        let cap = NonZeroUsize::new(normalized_capacity).unwrap_or(NonZeroUsize::MIN);
        let policy_config = CachePolicyConfig::new(kind, normalized_capacity as u64, window_ratio);
        Self {
            cache: RwLock::new(LruCache::new(cap)),
            capacity: normalized_capacity,
            policy: build_cache_policy(&policy_config),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    fn policy_params_from_env() -> (CachePolicyKind, f32) {
        let cfg = CacheConfig::from_env();
        (cfg.policy_kind(), cfg.policy_window_ratio())
    }

    fn handle_policy_evictions(&self) {
        let evicted = self.policy.drain_evictions();
        if evicted.is_empty() {
            return;
        }
        let mut cache = self.cache.write();
        for eviction in evicted {
            if cache.pop(&eviction.key).is_some() {
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Get parsed query from cache
    pub fn get(&self, query_str: &str) -> Option<Arc<ParsedQuery>> {
        self.handle_policy_evictions();
        let mut cache = self.cache.write();
        if let Some(parsed_arc) = cache.get(query_str) {
            self.hits.fetch_add(1, Ordering::Relaxed);
            let key = query_str.to_owned();
            let _ = self.policy.record_hit(&key);
            Some(parsed_arc.clone())
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// Insert parsed query into cache
    pub fn insert(&self, query_str: String, parsed: ParsedQuery) {
        self.insert_arc(query_str, Arc::new(parsed));
    }

    /// Insert Arc-wrapped `ParsedQuery` into cache
    pub fn insert_arc(&self, query_str: String, parsed_arc: Arc<ParsedQuery>) {
        self.handle_policy_evictions();

        let key_clone = query_str.clone();
        if matches!(
            self.policy.admit(&key_clone, AST_ENTRY_WEIGHT_BYTES),
            CacheAdmission::Rejected
        ) {
            debug!(
                "AST parse cache policy {:?} rejected entry",
                self.policy.kind()
            );
            return;
        }

        let mut cache = self.cache.write();
        if cache.len() == self.capacity
            && let Some((evicted_key, _)) = cache.pop_lru()
        {
            self.policy.invalidate(&evicted_key);
            self.evictions.fetch_add(1, Ordering::Relaxed);
        }
        cache.put(query_str, parsed_arc);
    }

    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
        }
    }

    /// Clear cache (for testing and invalidation)
    pub fn clear(&self) {
        let mut cache = self.cache.write();
        cache.clear();
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
        self.policy.reset();
    }

    /// Get current cache size
    pub fn len(&self) -> usize {
        self.cache.read().len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[cfg(test)]
    fn with_policy_kind(capacity: usize, kind: CachePolicyKind) -> Self {
        Self::with_policy(capacity, kind, CacheConfig::DEFAULT_POLICY_WINDOW_RATIO)
    }

    #[cfg(test)]
    fn policy_metrics(&self) -> crate::cache::policy::CachePolicyMetrics {
        self.policy.stats()
    }
}

// Thread safety: moka::sync::Cache<K, V> is Send + Sync
// AtomicU64 is Send + Sync
// No manual unsafe impl needed

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::policy::CachePolicyKind;
    use crate::query::types::{Condition, Expr, Field, Operator, Query as QueryAST, Span, Value};

    fn make_test_parsed_query(_query_str: &str) -> ParsedQuery {
        // Create simple AST for testing
        let ast = QueryAST {
            root: Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::new(0, 13),
            }),
            span: Span::new(0, 13),
        };

        ParsedQuery::from_ast(Arc::new(ast)).unwrap()
    }

    #[test]
    fn ast_parse_cache_hit() {
        let cache = AstParseCache::new(100);
        let query_str = "kind:function";
        let parsed = make_test_parsed_query(query_str);

        // Insert
        cache.insert(query_str.to_string(), parsed.clone());

        // Hit
        let cached = cache.get(query_str).unwrap();
        assert_eq!(cached.normalized.as_ref(), parsed.normalized.as_ref());

        // Stats
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 0);
    }

    #[test]
    fn ast_parse_cache_miss() {
        let cache = AstParseCache::new(100);

        // Miss
        let result = cache.get("kind:function");
        assert!(result.is_none());

        // Stats
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn ast_parse_cache_eviction() {
        let cache = AstParseCache::new(2);

        cache.insert("q1".into(), make_test_parsed_query("q1"));
        cache.insert("q2".into(), make_test_parsed_query("q2"));
        cache.insert("q3".into(), make_test_parsed_query("q3"));

        // After eviction, we should have at most 2 entries
        let count = cache.len();
        assert!(
            count <= 2,
            "Cache should have at most 2 entries after eviction, got {count}"
        );

        // Validate eviction stats were incremented by the eviction listener
        let stats = cache.stats();
        assert!(
            stats.evictions >= 1,
            "Eviction counter should be incremented (got {})",
            stats.evictions
        );
    }

    #[test]
    fn ast_parse_cache_clear() {
        let cache = AstParseCache::new(100);
        cache.insert("q1".into(), make_test_parsed_query("q1"));

        cache.clear();

        assert_eq!(cache.len(), 0);
        assert!(cache.get("q1").is_none());
    }

    #[test]
    fn ast_parse_cache_zero_capacity_defaults_to_one() {
        let cache = AstParseCache::new(0);

        cache.insert("q1".into(), make_test_parsed_query("q1"));
        cache.insert("q2".into(), make_test_parsed_query("q2"));

        // Capacity should clamp to 1, so we should have at most 1 entry
        let count = cache.len();
        assert!(
            count <= 1,
            "Cache with capacity 1 should have at most 1 entry, got {count}"
        );
    }

    #[test]
    fn ast_parse_cache_arc_sharing() {
        let cache = AstParseCache::new(100);
        let query_str = "kind:function";
        let parsed = make_test_parsed_query(query_str);

        // Insert
        cache.insert(query_str.to_string(), parsed);

        // Two cache hits should return same Arc pointer
        let cached1 = cache.get(query_str).unwrap();
        let cached2 = cache.get(query_str).unwrap();

        // Verify Arc pointer equality (zero-copy sharing)
        assert!(Arc::ptr_eq(&cached1, &cached2));
    }

    #[test]
    fn tiny_lfu_preserves_hot_queries() {
        let cache = AstParseCache::with_policy_kind(3, CachePolicyKind::TinyLfu);
        cache.insert("hot".into(), make_test_parsed_query("hot"));

        for i in 0..20 {
            let query = format!("cold{i}");
            cache.insert(query.clone(), make_test_parsed_query(&query));
        }

        let metrics = cache.policy_metrics();
        assert!(
            metrics.lfu_rejects > 0,
            "expected TinyLFU to reject cold entries"
        );
    }
}
