//! AST and symbol caching for performance.
//!
//! This module provides an in-memory LRU cache with optional persistence to disk,
//! designed to avoid redundant tree-sitter parsing on repeat queries.
//!
//! # Architecture
//!
//! The cache has two layers:
//! - **In-memory**: Fast LRU cache using `DashMap` for concurrent access
//! - **Persistent**: Optional `.sqry-cache/` directory for cross-process reuse
//!
//! # Features
//!
//! - **Fast hashing**: BLAKE3 for content-based cache keys
//! - **Atomic writes**: Temp + rename pattern for crash safety
//! - **Concurrency**: Lock-free reads, per-entry write locks
//! - **Eviction**: LRU with configurable size cap (default 50 MB)
//! - **Validation**: Version + ABI checks for cache invalidation
//! - **TTL support**: Per-plugin time-to-live configuration
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::cache::{CacheManager, CacheConfig};
//!
//! // Create cache with default config (50 MB, persistence enabled)
//! let cache = CacheManager::new(CacheConfig::default());
//!
//! // Query cache before parsing
//! if let Some(summary) = cache.get(&key) {
//!     // Cache hit - reuse parsed symbols
//!     return Ok(summary);
//! }
//!
//! // Cache miss - parse and store
//! let summary = parse_file(&path)?;
//! cache.insert(key, summary.clone());
//! ```
//!
//! # Configuration
//!
//! The cache can be configured via:
//! - [`CacheConfig`](crate::cache::config::CacheConfig) struct (programmatic)
//! - Environment variables (`SQRY_CACHE_MAX_BYTES`, `SQRY_CACHE_DISABLE_PERSIST`, `SQRY_CACHE_POLICY`, `SQRY_CACHE_POLICY_WINDOW`)
//! - Plugin policies ([`CachePolicy`](crate::cache::config::CachePolicy))
//!
//! # Persistence
//!
//! Cache entries are stored in `.sqry-cache/` with this layout:
//! ```text
//! .sqry-cache/
//! ├── <user_namespace_id>/              # Per-user namespace (hash of $USER)
//! │   ├── rust/                         # Language-specific directories
//! │   │   ├── <content_hash>/           # BLAKE3 of file content (64 hex chars)
//! │   │   │   ├── <path_hash>/          # BLAKE3 of canonical path (16 hex chars)
//! │   │   │   │   ├── file.rs.bin       # Cached symbol summary
//! │   │   │   │   └── file.rs.bin.lock  # Write lock file
//! │   │   │   └── <another_path>/
//! │   │   │       └── file.rs.bin
//! │   ├── python/
//! │   └── ...
//! └── manifest.json                     # Size tracking (Phase 3)
//! ```
//!
//! # Thread Safety
//!
//! All cache operations are thread-safe and can be called concurrently from
//! multiple threads (e.g., Rayon parallel queries or MCP server requests).
//!
//! # Performance Targets
//!
//! From [`01_SPEC.md`](../../../docs/development/cache/01_SPEC.md):
//! - **Latency reduction**: ≥40% on warm cache (vs cold parse)
//! - **Hit rate**: ≥70% on repeat queries
//! - **Memory footprint**: ≤50 MB default (configurable)
//!
//! # Implementation Status
//!
//! ✅ **Phase 0 Complete** - Foundations (6/6 tasks)
//! - ✅ Hash utility (BLAKE3)
//! - ✅ Module scaffolding
//! - ✅ CacheKey (path + language + content hash)
//! - ✅ GraphNodeSummary (lightweight cached representation)
//! - ✅ Design decisions resolved
//!
//! ✅ **Phase 1 Complete** - In-memory cache (2/2 tasks)
//! - ✅ CacheStorage (DashMap + LRU eviction)
//! - ✅ QueryExecutor integration
//!
//! ✅ **Phase 2 Complete** - Disk persistence (1/1 tasks)
//! - ✅ PersistManager (atomic writes, multi-process locks)
//! - ✅ CacheManager integration
//! - ✅ Graceful degradation on disk errors
//! - ✅ User-namespaced cache directories
//!
//! ⏳ **Phase 3 Planned** - Cache validation & manifest management
//! - ⏳ Version checks (sqry version mismatch detection)
//! - ⏳ ABI checks (postcard schema change detection)
//! - ⏳ Manifest tracking (disk size enforcement)
//! - ⏳ Multi-process integration tests

pub mod config;
pub mod key;
pub mod persist;
pub mod policy;
pub mod prune;
pub mod storage;
pub mod summary;

// Re-export commonly used types
pub use config::{CacheConfig, CachePolicy};
pub use key::CacheKey;
pub use persist::PersistManager;
pub use policy::{CachePolicyConfig, CachePolicyKind, CachePolicyMetrics};
pub use prune::{
    PruneEngine, PruneOperation, PruneOptions, PruneOutputMode, PruneReason, PruneReport,
};
pub use storage::{CacheStats, CacheStorage};
pub use summary::GraphNodeSummary;

// CacheManager is defined inline below (see line ~128)

use crate::hash::Blake3Hash;
use std::path::Path;
use std::sync::Arc;

/// Cache manager (main entry point).
///
/// Provides a high-level API for caching parsed AST symbols to avoid
/// redundant tree-sitter parsing on repeat queries.
///
/// # Thread Safety
///
/// All operations are thread-safe and can be called concurrently.
///
/// # Examples
///
/// ```rust,ignore
/// use sqry_core::cache::{CacheManager, CacheConfig};
/// use sqry_core::hash::hash_file;
///
/// let cache = CacheManager::new(CacheConfig::default());
///
/// // Check cache before parsing
/// let content_hash = hash_file(path)?;
/// if let Some(summaries) = cache.get(path, "rust", content_hash) {
///     // Cache hit - reuse symbols
///     return Ok(summaries);
/// }
///
/// // Cache miss - parse and store
/// let summaries = parse_file(path)?;
/// cache.insert(path, "rust", content_hash, summaries.clone());
/// ```
pub struct CacheManager {
    config: CacheConfig,
    storage: Arc<CacheStorage>,
    persist: Option<Arc<PersistManager>>,
}

impl CacheManager {
    /// Create a new cache manager with the given configuration.
    ///
    /// # Arguments
    ///
    /// * `config` - Cache configuration (size limits, persistence, etc.)
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::{CacheManager, CacheConfig};
    ///
    /// let cache = CacheManager::new(CacheConfig::default());
    /// ```
    #[must_use]
    pub fn new(config: CacheConfig) -> Self {
        let max_bytes = config.max_bytes();

        // Initialize persistence if enabled
        let persist = if config.is_persistence_enabled() {
            match PersistManager::new(config.cache_root()) {
                Ok(manager) => {
                    log::debug!("Persistence enabled at: {}", config.cache_root().display());
                    Some(Arc::new(manager))
                }
                Err(e) => {
                    log::warn!(
                        "Failed to initialize persistence: {e}. Cache will operate in-memory only."
                    );
                    None
                }
            }
        } else {
            log::debug!("Persistence disabled by configuration");
            None
        };

        let policy_config = CachePolicyConfig::new(
            config.policy_kind(),
            max_bytes,
            config.policy_window_ratio(),
        );

        Self {
            storage: Arc::new(CacheStorage::with_policy(&policy_config)),
            config,
            persist,
        }
    }

    /// Get cached symbols for a file.
    ///
    /// Returns `None` if the file is not in cache or the content hash doesn't match.
    ///
    /// # Arguments
    ///
    /// * `path` - File path (will be canonicalized)
    /// * `language` - Language identifier (e.g., "rust", "python")
    /// * `content_hash` - BLAKE3 hash of file contents
    ///
    /// # Returns
    ///
    /// `Some(Arc<[GraphNodeSummary]>)` on cache hit, `None` on miss.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use sqry_core::cache::CacheManager;
    /// use sqry_core::hash::hash_file;
    ///
    /// let cache = CacheManager::default();
    /// let hash = hash_file("example.rs")?;
    ///
    /// if let Some(summaries) = cache.get("example.rs", "rust", hash) {
    ///     println!("Cache hit! {} symbols", summaries.len());
    /// }
    /// ```
    pub fn get(
        &self,
        path: impl AsRef<Path>,
        language: impl AsRef<str>,
        content_hash: Blake3Hash,
    ) -> Option<Arc<[GraphNodeSummary]>> {
        let key = CacheKey::new(path.as_ref(), language.as_ref(), content_hash);

        // Try memory cache first
        if let Some(summaries) = self.storage.get(&key) {
            return Some(summaries);
        }

        // Try disk cache if persistence is enabled
        if let Some(persist) = &self.persist
            && let Ok(Some(summaries)) = persist.read_entry(&key)
        {
            log::debug!("Disk cache hit for: {}", key.path().display());

            // Populate memory cache for future hits
            self.storage.insert(key, summaries.clone());

            // Convert to Arc for return
            return Some(Arc::from(summaries.into_boxed_slice()));
        }

        None
    }

    /// Insert symbols into cache.
    ///
    /// Triggers eviction if the cache exceeds the configured size limit.
    ///
    /// # Arguments
    ///
    /// * `path` - File path (will be canonicalized)
    /// * `language` - Language identifier (e.g., "rust", "python")
    /// * `content_hash` - BLAKE3 hash of file contents
    /// * `summaries` - Node summaries to cache
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use sqry_core::cache::{CacheManager, GraphNodeSummary};
    /// use sqry_core::hash::hash_file;
    ///
    /// let cache = CacheManager::default();
    /// let hash = hash_file("example.rs")?;
    /// let summaries = vec![/* ... */];
    ///
    /// cache.insert("example.rs", "rust", hash, summaries);
    /// ```
    pub fn insert(
        &self,
        path: impl AsRef<Path>,
        language: impl AsRef<str>,
        content_hash: Blake3Hash,
        summaries: Vec<GraphNodeSummary>,
    ) {
        let key = CacheKey::new(path.as_ref(), language.as_ref(), content_hash);

        // Write to disk if persistence is enabled
        if let Some(persist) = &self.persist
            && let Err(e) = persist.write_entry(&key, &summaries)
        {
            log::warn!(
                "Failed to persist cache entry for {}: {}",
                key.path().display(),
                e
            );
            // Continue with memory cache even if disk write fails
        }

        // Insert into memory cache
        self.storage.insert(key, summaries);
    }

    /// Get cache statistics.
    ///
    /// Returns metrics including hit rate, miss rate, total size, and evictions.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheManager;
    ///
    /// let cache = CacheManager::default();
    /// let stats = cache.stats();
    ///
    /// println!("Hit rate: {:.1}%", stats.hit_rate() * 100.0);
    /// println!("Total size: {} bytes", stats.total_bytes);
    /// ```
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        self.storage.stats()
    }

    /// Clear all cache entries.
    ///
    /// Removes all cached symbols and resets statistics.
    /// Also clears disk cache if persistence is enabled.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheManager;
    ///
    /// let cache = CacheManager::default();
    /// cache.clear();
    /// assert_eq!(cache.stats().entry_count, 0);
    /// ```
    pub fn clear(&self) {
        // Clear memory cache
        self.storage.clear();

        // Also clear disk cache if persistence is enabled
        if let Some(persist) = &self.persist
            && let Err(e) = persist.clear_all()
        {
            log::warn!("Failed to clear disk cache: {e}");
        }
    }

    /// Get the cache configuration.
    ///
    /// Returns a reference to the configuration used to create this manager.
    #[must_use]
    pub fn config(&self) -> &CacheConfig {
        &self.config
    }

    /// Prune the cache based on retention policies.
    ///
    /// Removes old or excessive cache entries according to the provided options.
    /// Can operate in dry-run mode to preview deletions without modifying the cache.
    ///
    /// # Arguments
    ///
    /// * `options` - Pruning options (age limit, size limit, dry-run mode, etc.)
    ///
    /// # Returns
    ///
    /// `PruneReport` containing statistics about the prune operation.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No retention policy is specified (neither `max_age` nor `max_size`)
    /// - Persistence is disabled and no target directory is provided
    /// - IO errors occur during cache traversal or deletion
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use sqry_core::cache::{CacheManager, PruneOptions};
    /// use std::time::Duration;
    ///
    /// let cache = CacheManager::default();
    ///
    /// // Remove entries older than 7 days
    /// let options = PruneOptions::new()
    ///     .with_max_age(Duration::from_secs(7 * 24 * 3600));
    /// let report = cache.prune(&options)?;
    ///
    /// println!("Removed {} entries ({} bytes)",
    ///          report.entries_removed, report.bytes_removed);
    /// ```
    pub fn prune(&self, options: &PruneOptions) -> anyhow::Result<PruneReport> {
        // Validate options
        options.validate()?;

        // Determine target directory
        let cache_dir = if let Some(ref dir) = options.target_dir {
            dir.clone()
        } else if let Some(ref persist) = self.persist {
            persist.user_cache_dir()
        } else {
            anyhow::bail!(
                "Cannot prune cache: persistence is disabled and no --path specified. \
                 Enable persistence or provide --path to target cache directory."
            );
        };

        // Execute prune operation
        let engine = PruneEngine::new(options.clone())?;
        engine.execute(&cache_dir)
    }
}

impl Default for CacheManager {
    fn default() -> Self {
        Self::new(CacheConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::node::NodeKind;
    use crate::hash::hash_bytes;
    use approx::assert_abs_diff_eq;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_test_hash(byte: u8) -> Blake3Hash {
        hash_bytes(&[byte; 32])
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

    /// Create a cache manager with a unique temporary directory for isolation
    fn make_test_cache() -> (CacheManager, TempDir) {
        let tmp_cache_dir = TempDir::new().unwrap();
        let config = CacheConfig::default().with_cache_root(tmp_cache_dir.path().to_path_buf());
        let cache = CacheManager::new(config);
        (cache, tmp_cache_dir)
    }

    #[test]
    fn test_cache_manager_new() {
        let config = CacheConfig::default();
        let cache = CacheManager::new(config);

        // Verify initial stats
        let stats = cache.stats();
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.total_bytes, 0);
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0);
    }

    #[test]
    fn test_cache_manager_default() {
        let cache = CacheManager::default();

        // Verify default configuration
        assert_eq!(cache.config().max_bytes(), CacheConfig::DEFAULT_MAX_BYTES);
        assert!(cache.config().is_persistence_enabled());
    }

    #[test]
    fn test_cache_manager_get_miss() {
        let (cache, _tmp_cache_dir) = make_test_cache();
        let hash = make_test_hash(0x42);

        // Get on empty cache should miss
        let result = cache.get("test.rs", "rust", hash);
        assert!(result.is_none());

        // Stats should show one miss
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn test_cache_manager_insert_and_get() {
        let (cache, _tmp_cache_dir) = make_test_cache();
        let hash = make_test_hash(0x42);

        // Create test data
        let summaries = vec![make_test_summary("test_fn")];

        // Insert into cache
        cache.insert("test.rs", "rust", hash, summaries.clone());

        // Get should return cached data
        let retrieved = cache
            .get("test.rs", "rust", hash)
            .expect("Should be cached");
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].name.as_ref(), "test_fn");

        // Stats should show one hit
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.entry_count, 1);
        assert!(stats.total_bytes > 0);
    }

    #[test]
    fn test_cache_manager_different_hashes() {
        let (cache, _tmp_cache_dir) = make_test_cache();
        let hash1 = make_test_hash(0x42);
        let hash2 = make_test_hash(0x43);

        let summaries = vec![make_test_summary("test_fn")];

        // Insert with hash1
        cache.insert("test.rs", "rust", hash1, summaries.clone());

        // Get with hash2 should miss (different content)
        let result = cache.get("test.rs", "rust", hash2);
        assert!(result.is_none());

        // Get with hash1 should hit
        let result = cache.get("test.rs", "rust", hash1);
        assert!(result.is_some());
    }

    #[test]
    fn test_cache_manager_different_languages() {
        let (cache, _tmp_cache_dir) = make_test_cache();
        let hash = make_test_hash(0x42);

        let summaries = vec![make_test_summary("test_fn")];

        // Insert as Rust
        cache.insert("test.txt", "rust", hash, summaries.clone());

        // Get as Python should miss (different language)
        let result = cache.get("test.txt", "python", hash);
        assert!(result.is_none());

        // Get as Rust should hit
        let result = cache.get("test.txt", "rust", hash);
        assert!(result.is_some());
    }

    #[test]
    fn test_cache_manager_clear() {
        let (cache, _tmp_cache_dir) = make_test_cache();
        let hash = make_test_hash(0x42);

        let summaries = vec![make_test_summary("test_fn")];

        // Insert some data
        cache.insert("test.rs", "rust", hash, summaries);

        // Verify it's cached
        assert!(cache.get("test.rs", "rust", hash).is_some());
        assert_eq!(cache.stats().entry_count, 1);

        // Clear cache
        cache.clear();

        // Cache should be empty
        assert!(cache.get("test.rs", "rust", hash).is_none());
        let stats = cache.stats();
        assert_eq!(stats.entry_count, 0);
        assert_eq!(stats.total_bytes, 0);
    }

    #[test]
    fn test_cache_manager_stats_tracking() {
        let (cache, _tmp_cache_dir) = make_test_cache();
        let hash = make_test_hash(0x42);

        let summaries = vec![make_test_summary("test_fn")];

        // Insert
        cache.insert("test.rs", "rust", hash, summaries);

        // First get - hit
        cache.get("test.rs", "rust", hash);

        // Second get - another hit
        cache.get("test.rs", "rust", hash);

        // Get different file - miss
        cache.get("other.rs", "rust", hash);

        // Check stats
        let stats = cache.stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 1);
        assert_abs_diff_eq!(stats.hit_rate(), 2.0 / 3.0, epsilon = 1e-10);
    }

    #[test]
    fn test_cache_manager_eviction() {
        // Create cache with very small limit to force eviction (postcard is compact)
        let config = CacheConfig::new().with_max_bytes(100);
        let cache = CacheManager::new(config);

        let summaries = vec![make_test_summary("test_fn")];

        // Insert multiple entries to trigger eviction
        for i in 0..10 {
            let hash = make_test_hash(i);
            cache.insert(format!("file{i}.rs"), "rust", hash, summaries.clone());
        }

        // Cache should have evicted some entries
        let stats = cache.stats();
        assert!(stats.evictions > 0);
        assert!(stats.total_bytes <= 100);
    }

    #[test]
    fn test_cache_manager_config_access() {
        let config = CacheConfig::new()
            .with_max_bytes(100 * 1024 * 1024)
            .with_cache_root(PathBuf::from("/tmp/test-cache"));

        let cache = CacheManager::new(config);

        // Verify config is accessible
        assert_eq!(cache.config().max_bytes(), 100 * 1024 * 1024);
        assert_eq!(
            cache.config().cache_root(),
            &PathBuf::from("/tmp/test-cache")
        );
    }

    // ========================================================================
    // Persistence Integration Tests
    // ========================================================================

    #[test]
    fn test_persistence_enabled_by_default() {
        use tempfile::TempDir;

        let tmp_cache_dir = TempDir::new().unwrap();
        let config = CacheConfig::new()
            .with_cache_root(tmp_cache_dir.path().to_path_buf())
            .with_persistence(true);

        let cache = CacheManager::new(config);

        // Verify persistence is initialized
        assert!(cache.persist.is_some());
    }

    #[test]
    fn test_persistence_disabled() {
        use tempfile::TempDir;

        let tmp_cache_dir = TempDir::new().unwrap();
        let config = CacheConfig::new()
            .with_cache_root(tmp_cache_dir.path().to_path_buf())
            .with_persistence(false);

        let cache = CacheManager::new(config);

        // Verify persistence is not initialized
        assert!(cache.persist.is_none());
    }

    #[test]
    fn test_disk_cache_write_and_read() {
        use tempfile::TempDir;

        let tmp_cache_dir = TempDir::new().unwrap();
        let config = CacheConfig::new()
            .with_cache_root(tmp_cache_dir.path().to_path_buf())
            .with_persistence(true);

        let cache = CacheManager::new(config);
        let hash = make_test_hash(0x42);

        // Create and insert test data
        let summaries = vec![make_test_summary("test_fn")];

        cache.insert("test.rs", "rust", hash, summaries.clone());

        // Verify data was written to disk by creating a new cache instance
        let cache2 = CacheManager::new(
            CacheConfig::new()
                .with_cache_root(tmp_cache_dir.path().to_path_buf())
                .with_persistence(true),
        );

        // Get from cache2 (memory is empty, should read from disk)
        let retrieved = cache2.get("test.rs", "rust", hash);
        assert!(retrieved.is_some(), "Should retrieve from disk");

        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].name.as_ref(), "test_fn");
    }

    #[test]
    fn test_disk_cache_miss() {
        use tempfile::TempDir;

        let tmp_cache_dir = TempDir::new().unwrap();
        let config = CacheConfig::new()
            .with_cache_root(tmp_cache_dir.path().to_path_buf())
            .with_persistence(true);

        let cache = CacheManager::new(config);
        let hash = make_test_hash(0x99);

        // Get non-existent entry
        let result = cache.get("missing.rs", "rust", hash);
        assert!(result.is_none());

        // Stats should show miss
        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn test_clear_removes_disk_cache() {
        use tempfile::TempDir;

        let tmp_cache_dir = TempDir::new().unwrap();
        let config = CacheConfig::new()
            .with_cache_root(tmp_cache_dir.path().to_path_buf())
            .with_persistence(true);

        let cache = CacheManager::new(config);
        let hash = make_test_hash(0x42);

        // Insert data
        let summaries = vec![make_test_summary("test_fn")];
        cache.insert("test.rs", "rust", hash, summaries.clone());

        // Clear cache
        cache.clear();

        // Verify memory cache is empty
        let stats = cache.stats();
        assert_eq!(stats.entry_count, 0);

        // Create new cache instance and verify disk is also cleared
        let cache2 = CacheManager::new(
            CacheConfig::new()
                .with_cache_root(tmp_cache_dir.path().to_path_buf())
                .with_persistence(true),
        );

        let result = cache2.get("test.rs", "rust", hash);
        assert!(result.is_none(), "Disk cache should be cleared");
    }

    #[test]
    fn test_memory_cache_populated_on_disk_hit() {
        use tempfile::TempDir;

        let tmp_cache_dir = TempDir::new().unwrap();
        let config = CacheConfig::new()
            .with_cache_root(tmp_cache_dir.path().to_path_buf())
            .with_persistence(true);

        // First cache instance - write to disk
        let cache1 = CacheManager::new(config.clone());
        let hash = make_test_hash(0x42);

        let summaries = vec![make_test_summary("test_fn")];
        cache1.insert("test.rs", "rust", hash, summaries.clone());

        // Second cache instance - read from disk
        let cache2 = CacheManager::new(config);

        // First get reads from disk (miss in memory)
        let result1 = cache2.get("test.rs", "rust", hash);
        assert!(result1.is_some());
        assert_eq!(cache2.stats().hits, 0); // Memory miss

        // Second get should hit memory cache
        let result2 = cache2.get("test.rs", "rust", hash);
        assert!(result2.is_some());
        assert_eq!(cache2.stats().hits, 1); // Memory hit
    }

    #[test]
    fn test_persistence_graceful_failure() {
        use tempfile::TempDir;

        // Test that cache continues to work even if disk writes fail
        let tmp_cache_dir = TempDir::new().unwrap();
        let config = CacheConfig::new()
            .with_cache_root(tmp_cache_dir.path().to_path_buf())
            .with_persistence(true);

        let cache = CacheManager::new(config);

        // Persistence should be enabled initially
        assert!(cache.persist.is_some());

        // Insert data - this should work
        let hash = make_test_hash(0x42);
        let summaries = vec![make_test_summary("test_fn")];

        // Even if disk write fails (e.g., disk full), memory cache should work
        cache.insert("test.rs", "rust", hash, summaries.clone());
        let result = cache.get("test.rs", "rust", hash);
        assert!(
            result.is_some(),
            "Memory cache should work even if disk write fails"
        );

        // Verify memory cache is functioning
        let stats = cache.stats();
        assert_eq!(stats.entry_count, 1);
        assert_eq!(stats.hits, 1);
    }
}
