// RKG: CODE:SQRY-CORE implements REQ:SQRY-P2-6-CACHE-EVICTION-POLICY
//! In-memory cache storage with LRU eviction.
//!
//! This module provides a thread-safe, concurrent cache storage layer using:
//! - **`DashMap`**: Sharded hash map for lock-free concurrent reads
//! - **LRU / `TinyLFU`**: Eviction framework (default LRU; adaptive policies wired later)
//! - **Size tracking**: Maintains total byte count for eviction
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────┐
//! │         CacheStorage                │
//! ├─────────────────────────────────────┤
//! │  DashMap<CacheKey, CacheEntry>      │  ← Lock-free reads
//! │  Mutex<LruCache<CacheKey, ()>>      │  ← Protected LRU ordering
//! │  Total size counter (atomic)        │  ← Enforce cap
//! └─────────────────────────────────────┘
//! ```
//!
//! # Concurrency
//!
//! - **Reads**: Lock-free via `DashMap` sharding
//! - **Writes**: Per-key locks (`DashMap` handles this)
//! - **LRU updates**: Brief lock on LRU cache to record access
//! - **Eviction**: Lock-protected to prevent race conditions
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::cache::{CacheStorage, CacheKey, GraphNodeSummary};
//!
//! let storage = CacheStorage::new(50 * 1024 * 1024); // 50 MB cap
//!
//! storage.insert(key, vec![summary1, summary2]);
//!
//! if let Some(summaries) = storage.get(&key) {
//!     // Cache hit
//! }
//! ```

use super::config::CacheConfig;
use super::policy::{
    CacheAdmission, CachePolicy, CachePolicyConfig, CachePolicyKind, CachePolicyMetrics,
    build_cache_policy,
};
use crate::cache::{CacheKey, GraphNodeSummary};
use dashmap::DashMap;
use lru::LruCache;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Cache entry with size metadata.
///
/// Uses `Arc<[GraphNodeSummary]>` for zero-copy sharing across cache hits.
#[derive(Debug, Clone)]
struct CacheEntry {
    /// Node summaries for this file (shared via Arc)
    summaries: Arc<[GraphNodeSummary]>,

    /// Estimated size in bytes (cached to avoid repeated serialization)
    size_bytes: usize,
}

impl CacheEntry {
    /// Create a new cache entry from a vector of summaries.
    fn new(summaries: Vec<GraphNodeSummary>) -> Self {
        // Estimate size: serialize one summary and multiply by count
        let size_bytes = if summaries.is_empty() {
            0
        } else {
            let sample_size = postcard::to_allocvec(&summaries[0])
                .map(|bytes| bytes.len())
                .unwrap_or(256); // Fallback to budget estimate
            sample_size * summaries.len()
        };

        Self {
            summaries: Arc::from(summaries.into_boxed_slice()),
            size_bytes,
        }
    }
}

/// Thread-safe in-memory cache storage with LRU eviction.
///
/// # Thread Safety
///
/// All operations are thread-safe and can be called concurrently from
/// multiple threads (e.g., Rayon parallel queries or MCP server requests).
///
/// # Eviction Policy
///
/// Uses an incremental LRU cache with Mutex protection to prevent race conditions.
/// When the cache exceeds `max_bytes`, the least recently used entries
/// are evicted until the size drops below the cap.
///
/// # Examples
///
/// ```rust,ignore
/// use sqry_core::cache::{CacheStorage, CacheKey};
///
/// let storage = CacheStorage::new(50 * 1024 * 1024); // 50 MB
///
/// // Thread-safe insert
/// storage.insert(key, summaries);
///
/// // Thread-safe get (updates LRU)
/// if let Some(summaries) = storage.get(&key) {
///     // Use summaries
/// }
///
/// // Check stats
/// let stats = storage.stats();
/// println!("Size: {} bytes, Entries: {}", stats.total_bytes, stats.entry_count);
/// ```
pub struct CacheStorage {
    /// Concurrent hash map (sharded for lock-free reads)
    entries: DashMap<CacheKey, CacheEntry>,

    /// LRU ordering (Mutex-protected to prevent eviction races)
    ///
    /// The LRU cache only tracks access order, not the actual data.
    /// We use `NonZeroUsize::MAX` as the capacity since we're managing
    /// eviction based on byte size, not entry count.
    lru: Mutex<LruCache<CacheKey, ()>>,

    /// Maximum total cache size in bytes
    max_bytes: u64,

    /// Current total size (approximate, updated atomically)
    total_bytes: AtomicU64,

    /// Cache statistics
    hits: AtomicUsize,
    misses: AtomicUsize,
    evictions: AtomicUsize,

    /// Eviction policy controller
    policy: Arc<dyn CachePolicy<CacheKey>>,
}

impl CacheStorage {
    /// Create a new cache storage with the given size limit.
    ///
    /// # Arguments
    ///
    /// - `max_bytes`: Maximum total cache size in bytes
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheStorage;
    ///
    /// let storage = CacheStorage::new(50 * 1024 * 1024); // 50 MB
    /// ```
    #[must_use]
    pub fn new(max_bytes: u64) -> Self {
        Self::with_policy(&CachePolicyConfig::new(
            CachePolicyKind::Lru,
            max_bytes,
            CacheConfig::DEFAULT_POLICY_WINDOW_RATIO,
        ))
    }

    /// Create cache storage with a specific eviction policy configuration.
    #[must_use]
    pub fn with_policy(config: &CachePolicyConfig) -> Self {
        let policy = build_cache_policy::<CacheKey>(config);
        Self {
            entries: DashMap::new(),
            lru: Mutex::new(LruCache::unbounded()),
            max_bytes: config.max_bytes,
            total_bytes: AtomicU64::new(0),
            hits: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
            evictions: AtomicUsize::new(0),
            policy,
        }
    }

    fn handle_policy_evictions(&self) {
        for eviction in self.policy.drain_evictions() {
            let key = eviction.key;
            if let Some((_, removed)) = self.entries.remove(&key) {
                self.total_bytes
                    .fetch_sub(removed.size_bytes as u64, Ordering::Relaxed);
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }

            if let Ok(mut lru) = self.lru.lock() {
                let _ = lru.pop(&key);
            }
        }
    }

    /// Get cached summaries for a key.
    ///
    /// Returns an `Arc<[GraphNodeSummary]>` to avoid cloning the entire vector
    /// on every cache hit. This provides zero-copy sharing of cached data.
    ///
    /// Updates the LRU ordering on hit.
    ///
    /// # Returns
    ///
    /// - `Some(Arc<[GraphNodeSummary]>)` if the key is in cache (hit)
    /// - `None` if the key is not in cache (miss)
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// if let Some(summaries) = storage.get(&key) {
    ///     // Cache hit - Arc clone is cheap (just bumps ref count)
    ///     for summary in summaries.iter() {
    ///         // Process symbols...
    ///     }
    /// }
    /// ```
    pub fn get(&self, key: &CacheKey) -> Option<Arc<[GraphNodeSummary]>> {
        if let Some(entry) = self.entries.get(key) {
            let _ = self.policy.record_hit(key);
            // Update LRU ordering (brief lock)
            if let Ok(mut lru) = self.lru.lock() {
                lru.get_or_insert(key.clone(), || ());
            }

            self.hits.fetch_add(1, Ordering::Relaxed);
            Some(Arc::clone(&entry.summaries))
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// Insert summaries into the cache.
    ///
    /// Triggers eviction if the cache exceeds the size limit.
    ///
    /// # Arguments
    ///
    /// - `key`: Cache key (file path + language + content hash)
    /// - `summaries`: Node summaries to cache
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// storage.insert(key, vec![summary1, summary2]);
    /// ```
    pub fn insert(&self, key: CacheKey, summaries: Vec<GraphNodeSummary>) {
        let entry = CacheEntry::new(summaries);
        let entry_size = entry.size_bytes as u64;
        let key_for_lru = key.clone();

        // Remove old entry if it exists (to update size correctly)
        if let Some((_, old_entry)) = self.entries.remove(&key) {
            self.total_bytes
                .fetch_sub(old_entry.size_bytes as u64, Ordering::Relaxed);
            self.policy.invalidate(&key);
        }

        if matches!(
            self.policy.admit(&key, entry_size),
            CacheAdmission::Rejected
        ) {
            log::debug!(
                "cache policy {:?} rejected entry {:?} ({} bytes)",
                self.policy.kind(),
                &key,
                entry_size
            );
            return;
        }

        // Insert new entry
        self.entries.insert(key, entry);
        self.total_bytes.fetch_add(entry_size, Ordering::Relaxed);

        // Update LRU ordering (brief lock)
        if let Ok(mut lru) = self.lru.lock() {
            lru.put(key_for_lru, ());
        }

        self.handle_policy_evictions();

        // Evict if needed (lock-protected)
        if self.total_bytes.load(Ordering::Relaxed) > self.max_bytes {
            self.evict_lru();
        }
    }

    /// Evict least recently used entries until under size cap.
    ///
    /// This is called automatically by `insert()` when needed.
    /// Uses incremental LRU eviction with lock protection to prevent races.
    fn evict_lru(&self) {
        // Lock LRU to prevent concurrent evictions
        let Ok(mut lru) = self.lru.lock() else {
            log::warn!("Failed to acquire LRU lock for eviction");
            return;
        };

        let mut current_size = self.total_bytes.load(Ordering::Relaxed);

        // Evict oldest entries until under cap
        while current_size > self.max_bytes {
            // Pop least recently used key
            let Some((key, ())) = lru.pop_lru() else {
                break; // No more entries to evict
            };

            // Remove from entries map
            if let Some((_, removed)) = self.entries.remove(&key) {
                current_size = current_size.saturating_sub(removed.size_bytes as u64);
                self.total_bytes
                    .fetch_sub(removed.size_bytes as u64, Ordering::Relaxed);
                self.evictions.fetch_add(1, Ordering::Relaxed);
                self.policy.invalidate(&key);

                log::debug!(
                    "Evicted cache entry: {} ({} bytes)",
                    key,
                    removed.size_bytes
                );
            }
        }
    }

    /// Clear all entries from the cache.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// storage.clear();
    /// ```
    pub fn clear(&self) {
        self.entries.clear();
        self.total_bytes.store(0, Ordering::Relaxed);
        self.hits.store(0, Ordering::Relaxed);
        self.misses.store(0, Ordering::Relaxed);
        self.evictions.store(0, Ordering::Relaxed);
        self.policy.reset();

        if let Ok(mut lru) = self.lru.lock() {
            lru.clear();
        }

        log::debug!("Cache cleared");
    }

    /// Get cache statistics.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let stats = storage.stats();
    /// println!("Hit rate: {:.1}%", stats.hit_rate() * 100.0);
    /// ```
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entry_count: self.entries.len(),
            total_bytes: self.total_bytes.load(Ordering::Relaxed),
            max_bytes: self.max_bytes,
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            policy: self.policy.stats(),
        }
    }
}

/// Cache statistics for telemetry and diagnostics.
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheStats {
    /// Number of entries in cache
    pub entry_count: usize,

    /// Total size in bytes
    pub total_bytes: u64,

    /// Maximum allowed size
    pub max_bytes: u64,

    /// Number of cache hits
    pub hits: usize,

    /// Number of cache misses
    pub misses: usize,

    /// Number of evictions
    pub evictions: usize,

    /// Policy-specific telemetry (LFU rejects, hot/cold evictions, etc.)
    pub policy: CachePolicyMetrics,
}

impl CacheStats {
    fn usize_to_f64(value: usize) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        {
            value as f64
        }
    }

    fn u64_to_f64(value: u64) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        {
            value as f64
        }
    }

    /// Calculate cache hit rate (0.0 to 1.0).
    ///
    /// Returns 0.0 if no requests have been made yet.
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            Self::usize_to_f64(self.hits) / Self::usize_to_f64(total)
        }
    }

    /// Calculate cache utilization (0.0 to 1.0).
    ///
    /// Returns the fraction of `max_bytes` currently in use.
    #[must_use]
    pub fn utilization(&self) -> f64 {
        if self.max_bytes == 0 {
            0.0
        } else {
            Self::u64_to_f64(self.total_bytes) / Self::u64_to_f64(self.max_bytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::policy::{CachePolicyConfig, CachePolicyKind};
    use crate::graph::unified::node::NodeKind;
    use crate::hash::Blake3Hash;
    use approx::assert_abs_diff_eq;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::thread;

    fn make_test_key(name: &str, lang: &str) -> CacheKey {
        let hash = Blake3Hash::from_bytes([name.as_bytes()[0]; 32]);
        CacheKey::from_raw_path(PathBuf::from(name), lang, hash)
    }

    fn make_test_summary(name: &str) -> GraphNodeSummary {
        GraphNodeSummary::new(
            Arc::from(name),
            NodeKind::Function,
            Arc::from(Path::new("test.rs")),
            1,
            0,
            1,
            10,
        )
    }

    #[test]
    fn test_storage_new() {
        let storage = CacheStorage::new(1024);
        let stats = storage.stats();

        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.total_bytes, 0);
        assert_eq!(stats.max_bytes, 1024);
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
    }

    #[test]
    fn test_storage_insert_and_get() {
        let storage = CacheStorage::new(10 * 1024);

        let key = make_test_key("file.rs", "rust");
        let summaries = vec![make_test_summary("test_fn")];

        storage.insert(key.clone(), summaries.clone());

        // Get should return the inserted value
        let retrieved = storage.get(&key).unwrap();
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].name.as_ref(), "test_fn");

        // Stats should show one hit
        let stats = storage.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.entry_count, 1);
    }

    #[test]
    fn test_storage_miss() {
        let storage = CacheStorage::new(10 * 1024);

        let key = make_test_key("file.rs", "rust");

        // Get on empty cache should miss
        assert!(storage.get(&key).is_none());

        let stats = storage.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn test_storage_update() {
        let storage = CacheStorage::new(10 * 1024);

        let key = make_test_key("file.rs", "rust");
        let summaries1 = vec![make_test_summary("fn1")];
        let summaries2 = vec![make_test_summary("fn2"), make_test_summary("fn3")];

        // Insert first value
        storage.insert(key.clone(), summaries1);

        // Update with new value
        storage.insert(key.clone(), summaries2.clone());

        // Should get the updated value
        let retrieved = storage.get(&key).unwrap();
        assert_eq!(retrieved.len(), 2);
        assert_eq!(retrieved[0].name.as_ref(), "fn2");
    }

    #[test]
    fn test_storage_clear() {
        let storage = CacheStorage::new(10 * 1024);

        let key = make_test_key("file.rs", "rust");
        storage.insert(key.clone(), vec![make_test_summary("test")]);

        assert!(storage.get(&key).is_some());

        storage.clear();

        assert!(storage.get(&key).is_none());
        let stats = storage.stats();
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.total_bytes, 0);
    }

    #[test]
    fn test_storage_eviction() {
        // Small cache: 100 bytes (postcard varint encoding is compact)
        let storage = CacheStorage::new(100);

        // Insert entries until eviction triggers
        for i in 0..10 {
            let key = make_test_key(&format!("file{i}.rs"), "rust");
            let summaries = vec![make_test_summary(&format!("fn{i}"))];
            storage.insert(key, summaries);
        }

        let stats = storage.stats();

        // Should have evicted entries
        assert!(stats.evictions > 0, "Expected evictions, got 0");
        assert!(
            stats.entry_count < 10,
            "Expected < 10 entries due to eviction"
        );
        assert!(
            stats.total_bytes <= 100,
            "Cache size should be under cap, got {}",
            stats.total_bytes
        );
    }

    #[test]
    fn test_storage_lru_order() {
        // Use very small cache to ensure eviction happens (postcard is compact)
        let storage = CacheStorage::new(80);

        // Insert three entries
        let key1 = make_test_key("file1.rs", "rust");
        let key2 = make_test_key("file2.rs", "rust");
        let key3 = make_test_key("file3.rs", "rust");

        storage.insert(key1.clone(), vec![make_test_summary("fn1")]);
        storage.insert(key2.clone(), vec![make_test_summary("fn2")]);
        storage.insert(key3.clone(), vec![make_test_summary("fn3")]);

        // Access key1 to make it most recently used
        storage.get(&key1);

        // Insert many more entries to force eviction
        for i in 4..20 {
            let key = make_test_key(&format!("file{i}.rs"), "rust");
            storage.insert(key, vec![make_test_summary(&format!("fn{i}"))]);
        }

        let stats = storage.stats();

        // At least one eviction should have happened
        assert!(
            stats.evictions > 0,
            "Expected evictions with small cache, got 0"
        );

        // key1 was accessed most recently, so has best chance to survive
        // This is probabilistic with small caches
        let key1_present = storage.get(&key1).is_some();
        let key2_present = storage.get(&key2).is_some();

        // If key2 survived but key1 didn't, that's an LRU violation
        assert!(
            key1_present || !key2_present,
            "LRU violation: older key2 survived but recently accessed key1 didn't"
        );
    }

    #[test]
    fn test_concurrent_insert_and_get() {
        let storage = Arc::new(CacheStorage::new(10 * 1024));
        let mut handles = vec![];

        // Spawn 10 threads that insert and get concurrently
        for i in 0..10 {
            let storage = Arc::clone(&storage);
            let handle = thread::spawn(move || {
                let key = make_test_key(&format!("file{i}.rs"), "rust");
                let summaries = vec![make_test_summary(&format!("fn{i}"))];

                storage.insert(key.clone(), summaries.clone());

                // Verify we can get it back
                let retrieved = storage.get(&key).expect("Should retrieve inserted value");
                assert_eq!(retrieved.len(), 1);
                assert_eq!(retrieved[0].name.as_ref(), &format!("fn{i}"));
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // All 10 entries should be present
        let stats = storage.stats();
        assert_eq!(stats.entry_count, 10);
        assert_eq!(stats.hits, 10); // One get per thread
    }

    #[test]
    fn test_concurrent_eviction() {
        // Very small cache to force evictions (postcard is compact)
        let storage = Arc::new(CacheStorage::new(100));
        let mut handles = vec![];

        // Spawn 20 threads that insert concurrently
        for i in 0..20 {
            let storage = Arc::clone(&storage);
            let handle = thread::spawn(move || {
                let key = make_test_key(&format!("file{i}.rs"), "rust");
                let summaries = vec![make_test_summary(&format!("fn{i}"))];
                storage.insert(key, summaries);
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        let stats = storage.stats();

        // Should have evicted some entries
        assert!(
            stats.evictions > 0,
            "Expected evictions with small cache and concurrent inserts"
        );

        // Should respect size cap
        assert!(
            stats.total_bytes <= 100,
            "Cache size should be under cap, got {}",
            stats.total_bytes
        );

        // Should have some entries remaining
        assert!(stats.entry_count > 0, "Should have entries after eviction");
        assert!(
            stats.entry_count < 20,
            "Should have evicted some of 20 entries"
        );
    }

    #[test]
    fn test_cache_stats_hit_rate() {
        let stats = CacheStats {
            entry_count: 10,
            total_bytes: 1000,
            max_bytes: 2000,
            hits: 75,
            misses: 25,
            evictions: 0,
            policy: CachePolicyMetrics::default(),
        };

        assert_abs_diff_eq!(stats.hit_rate(), 0.75, epsilon = 1e-10);
    }

    #[test]
    fn test_cache_stats_utilization() {
        let stats = CacheStats {
            entry_count: 10,
            total_bytes: 1000,
            max_bytes: 2000,
            hits: 0,
            misses: 0,
            evictions: 0,
            policy: CachePolicyMetrics::default(),
        };

        assert_abs_diff_eq!(stats.utilization(), 0.5, epsilon = 1e-10);
    }

    #[test]
    fn test_cache_stats_empty() {
        let stats = CacheStats {
            entry_count: 0,
            total_bytes: 0,
            max_bytes: 1000,
            hits: 0,
            misses: 0,
            evictions: 0,
            policy: CachePolicyMetrics::default(),
        };

        assert_abs_diff_eq!(stats.hit_rate(), 0.0, epsilon = 1e-10);
        assert_abs_diff_eq!(stats.utilization(), 0.0, epsilon = 1e-10);
    }

    #[test]
    fn test_tiny_lfu_rejects_cold_workload() {
        let storage =
            CacheStorage::with_policy(&CachePolicyConfig::new(CachePolicyKind::TinyLfu, 1024, 0.2));

        let hot_key = make_test_key("hot.rs", "rust");
        storage.insert(hot_key.clone(), vec![make_test_summary("hot_fn")]);
        for _ in 0..8 {
            assert!(storage.get(&hot_key).is_some());
        }

        for i in 0..50 {
            let key = make_test_key(&format!("cold{i}.rs"), "rust");
            storage.insert(key.clone(), vec![make_test_summary(&format!("cold{i}"))]);
            let _ = storage.get(&key);
        }

        let stats = storage.stats();
        assert!(
            stats.policy.lfu_rejects > 0,
            "expected TinyLFU policy to reject some cold inserts"
        );
    }
}
