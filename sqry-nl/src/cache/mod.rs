//! Translation cache for repeated queries.
//!
//! Provides LRU caching with context-aware keys and TTL expiration.

mod key;

pub use key::CacheKey;

use crate::types::Intent;
use lru::LruCache;
use parking_lot::Mutex;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

/// Cache configuration.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Time-to-live for cache entries
    pub ttl: Duration,
    /// Maximum number of cached entries
    pub capacity: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(3600), // 1 hour
            capacity: 1000,
        }
    }
}

/// Cached translation result.
#[derive(Debug, Clone)]
pub struct CachedResult {
    /// The translated command
    pub command: String,
    /// The classified intent
    pub intent: Intent,
    /// The classifier confidence
    pub confidence: f32,
    /// When this entry was created
    pub created_at: Instant,
}

/// Cache statistics.
#[derive(Debug, Clone, Copy)]
pub struct CacheStats {
    /// Current number of entries
    pub size: usize,
    /// Maximum capacity
    pub capacity: usize,
    /// Number of cache hits
    pub hits: u64,
    /// Number of cache misses
    pub misses: u64,
}

/// Thread-safe LRU translation cache.
pub struct TranslationCache {
    cache: Mutex<LruCache<CacheKey, CachedResult>>,
    config: CacheConfig,
    hits: Mutex<u64>,
    misses: Mutex<u64>,
}

impl TranslationCache {
    /// Create a new cache with the given capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self::with_config(CacheConfig {
            capacity,
            ..Default::default()
        })
    }

    /// Create a new cache with full configuration.
    #[must_use]
    pub fn with_config(config: CacheConfig) -> Self {
        let capacity = NonZeroUsize::new(config.capacity).unwrap_or(NonZeroUsize::MIN);
        Self {
            cache: Mutex::new(LruCache::new(capacity)),
            config,
            hits: Mutex::new(0),
            misses: Mutex::new(0),
        }
    }

    /// Get a cached result if valid.
    ///
    /// Returns `None` if not found or expired.
    #[must_use]
    pub fn get(&self, key: &CacheKey) -> Option<CachedResult> {
        let mut cache = self.cache.lock();
        if let Some(result) = cache.get(key) {
            // Check TTL
            if result.created_at.elapsed() < self.config.ttl {
                *self.hits.lock() += 1;
                return Some(result.clone());
            }
            // Expired - remove it
            cache.pop(key);
        }
        *self.misses.lock() += 1;
        None
    }

    /// Store a result in the cache.
    pub fn put(&self, key: CacheKey, result: CachedResult) {
        let mut cache = self.cache.lock();
        cache.put(key, result);
    }

    /// Clear all cached entries.
    pub fn clear(&self) {
        let mut cache = self.cache.lock();
        cache.clear();
        *self.hits.lock() = 0;
        *self.misses.lock() = 0;
    }

    /// Get cache statistics.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        let cache = self.cache.lock();
        CacheStats {
            size: cache.len(),
            capacity: cache.cap().get(),
            hits: *self.hits.lock(),
            misses: *self.misses.lock(),
        }
    }

    /// Get the hit rate (0.0-1.0).
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let hits = *self.hits.lock();
        let misses = *self.misses.lock();
        let total = hits + misses;
        if total == 0 {
            0.0
        } else {
            let hits_f64 = f64::from(u32::try_from(hits).unwrap_or(u32::MAX));
            let total_f64 = f64::from(u32::try_from(total).unwrap_or(u32::MAX));
            hits_f64 / total_f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_put_get() {
        let cache = TranslationCache::new(10);
        let key = CacheKey::new("find foo", &[], None, 100);
        let result = CachedResult {
            command: "sqry query \"foo\"".to_string(),
            intent: Intent::SymbolQuery,
            confidence: 0.95,
            created_at: Instant::now(),
        };

        cache.put(key.clone(), result.clone());

        let cached = cache.get(&key);
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().command, result.command);
    }

    #[test]
    fn test_cache_miss() {
        let cache = TranslationCache::new(10);
        let key = CacheKey::new("find foo", &[], None, 100);

        let cached = cache.get(&key);
        assert!(cached.is_none());
    }

    #[test]
    fn test_cache_expiration() {
        let config = CacheConfig {
            ttl: Duration::from_millis(1),
            capacity: 10,
        };
        let cache = TranslationCache::with_config(config);
        let key = CacheKey::new("find foo", &[], None, 100);
        let result = CachedResult {
            command: "sqry query \"foo\"".to_string(),
            intent: Intent::SymbolQuery,
            confidence: 0.95,
            created_at: Instant::now(),
        };

        cache.put(key.clone(), result);

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(10));

        let cached = cache.get(&key);
        assert!(cached.is_none());
    }

    #[test]
    fn test_cache_stats() {
        let cache = TranslationCache::new(10);
        let key = CacheKey::new("find foo", &[], None, 100);
        let result = CachedResult {
            command: "sqry query \"foo\"".to_string(),
            intent: Intent::SymbolQuery,
            confidence: 0.95,
            created_at: Instant::now(),
        };

        // Miss
        let _ = cache.get(&key);

        // Put
        cache.put(key.clone(), result);

        // Hit
        let _ = cache.get(&key);

        let stats = cache.stats();
        assert_eq!(stats.size, 1);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn test_lru_eviction() {
        let cache = TranslationCache::new(2);

        // Add 3 entries to a cache of size 2
        for i in 0..3 {
            let key = CacheKey::new(&format!("query {i}"), &[], None, 100);
            let result = CachedResult {
                command: format!("sqry query \"{i}\""),
                intent: Intent::SymbolQuery,
                confidence: 0.95,
                created_at: Instant::now(),
            };
            cache.put(key, result);
        }

        let stats = cache.stats();
        assert_eq!(stats.size, 2);

        // First entry should be evicted
        let key0 = CacheKey::new("query 0", &[], None, 100);
        assert!(cache.get(&key0).is_none());

        // Last two should still be there
        let key1 = CacheKey::new("query 1", &[], None, 100);
        let key2 = CacheKey::new("query 2", &[], None, 100);
        assert!(cache.get(&key1).is_some());
        assert!(cache.get(&key2).is_some());
    }
}
