//! Core types for query caching

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Cache key for result cache (5-component hash)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// Hash of query AST (query string based)
    pub query_hash: u64,

    /// Hash of plugin versions (sorted, deterministic)
    pub plugin_hash: u64,

    /// Hash of file metadata (path + mtime + size)
    pub file_set_hash: u64,

    /// Hash of workspace root path
    pub root_path_hash: u64,

    /// Hash of repo filter patterns (0 if universal)
    pub repo_filter_hash: u64,
}

impl CacheKey {
    /// Create cache key from a query string (for AST-based execution)
    ///
    /// This variant is used by the AST execution path where we have a normalized
    /// query string instead of the full AST value.
    ///
    /// IMPORTANT: `repo_filter_patterns` must be included to prevent cache collisions
    /// between queries with different repo filters. For example:
    /// - "repo:frontend AND kind:function"
    /// - "repo:backend AND kind:function"
    ///   Both normalize to "kind:function" but must have different cache keys.
    #[must_use]
    pub fn from_string(
        query_str: &str,
        plugin_hash: u64,
        file_set_hash: u64,
        root_path_hash: u64,
        repo_filter_patterns: &[String],
    ) -> Self {
        let query_hash = Self::hash_string(query_str);
        let repo_filter_hash = Self::hash_repo_patterns(repo_filter_patterns);
        Self {
            query_hash,
            plugin_hash,
            file_set_hash,
            root_path_hash,
            repo_filter_hash,
        }
    }

    /// Hash repo filter patterns for cache key
    fn hash_repo_patterns(patterns: &[String]) -> u64 {
        if patterns.is_empty() {
            return 0; // Universal filter
        }
        let mut hasher = DefaultHasher::new();
        // Sort patterns for deterministic hashing
        let mut sorted_patterns = patterns.to_vec();
        sorted_patterns.sort();
        for pattern in sorted_patterns {
            hasher.write(pattern.as_bytes());
        }
        hasher.finish()
    }

    /// Hash a query string directly
    fn hash_string(query_str: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        hasher.write(query_str.as_bytes());
        hasher.finish()
    }
}

/// Cache statistics for monitoring
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Number of cache hits
    pub hits: u64,
    /// Number of cache misses
    pub misses: u64,
    /// Number of evictions due to capacity
    pub evictions: u64,
}

impl CacheStats {
    /// Calculate cache hit rate (0.0 to 1.0)
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "Hit rate is diagnostic-only; f64 precision is adequate"
    )]
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn cache_key_equality() {
        let key1 = CacheKey {
            query_hash: 123,
            plugin_hash: 456,
            file_set_hash: 789,
            root_path_hash: 101,
            repo_filter_hash: 0,
        };
        let key2 = key1.clone();
        assert_eq!(key1, key2);
    }

    #[test]
    fn cache_key_hash_deterministic() {
        let key1 = CacheKey::from_string("kind:function", 0, 0, 0, &[]);
        let key2 = CacheKey::from_string("kind:function", 0, 0, 0, &[]);

        assert_eq!(key1.query_hash, key2.query_hash);
    }

    #[test]
    fn cache_stats_hit_rate() {
        let mut stats = CacheStats::default();
        assert_abs_diff_eq!(stats.hit_rate(), 0.0, epsilon = 1e-10);

        stats.hits = 7;
        stats.misses = 3;
        assert_abs_diff_eq!(stats.hit_rate(), 0.7, epsilon = 1e-10);
    }

    #[test]
    fn cache_key_with_repo_filters_differs() {
        let key1 = CacheKey::from_string("kind:function", 100, 200, 300, &[]);
        let key2 =
            CacheKey::from_string("kind:function", 100, 200, 300, &["backend-*".to_string()]);

        assert_ne!(
            key1.repo_filter_hash, key2.repo_filter_hash,
            "Repo filter patterns should affect cache key"
        );
    }
}
