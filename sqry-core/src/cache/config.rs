//! Cache configuration types.
//!
//! This module defines configuration structures for controlling cache behavior,
//! including size limits, persistence, eviction policy selection, and TTL policies.

use super::policy::CachePolicyKind;
use std::path::PathBuf;
use std::time::Duration;

/// Cache configuration.
///
/// Controls cache behavior including size limits, persistence, and location.
///
/// # Default Values
///
/// - **`max_bytes`**: 50 MB (52,428,800 bytes)
/// - **`enable_persistence`**: `true`
/// - **`cache_root`**: `.sqry-cache` (relative to working directory)
/// - **`background_writer`**: `true` (use background thread for writes)
///
/// # Environment Variables
///
/// Configuration can be overridden via environment variables:
/// - `SQRY_CACHE_MAX_BYTES`: Maximum cache size in bytes
/// - `SQRY_CACHE_DISABLE_PERSIST`: Set to `1` to disable persistence
/// - `SQRY_CACHE_ROOT`: Custom cache directory location
/// - `SQRY_CACHE_POLICY`: `lru`, `tiny_lfu`, or `hybrid` eviction policy
/// - `SQRY_CACHE_POLICY_WINDOW`: Protected window ratio for hybrid `TinyLFU` (float, e.g. `0.2`)
///
/// # Examples
///
/// ```rust
/// use sqry_core::cache::CacheConfig;
///
/// // Default configuration
/// let config = CacheConfig::default();
/// assert_eq!(config.max_bytes(), 50 * 1024 * 1024); // 50 MB
///
/// // Custom configuration
/// let config = CacheConfig::new()
///     .with_max_bytes(100 * 1024 * 1024) // 100 MB
///     .with_persistence(false); // Memory-only
/// ```
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Maximum cache size in bytes (default: 50 MB).
    max_bytes: u64,

    /// Enable persistent cache to disk (default: true).
    enable_persistence: bool,

    /// Cache root directory (default: `.sqry-cache`).
    cache_root: PathBuf,

    /// Use background writer thread for persistence (default: true).
    background_writer: bool,

    /// Eviction policy selection (default: LRU).
    policy_kind: CachePolicyKind,

    /// Fraction of cache reserved for `TinyLFU` protected window (0.0–1.0).
    policy_window_ratio: f32,
}

impl CacheConfig {
    /// Default maximum cache size (50 MB).
    pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024 * 1024; // 50 MB

    /// Default cache root directory.
    pub const DEFAULT_CACHE_ROOT: &'static str = ".sqry-cache";

    /// Default protected window ratio for hybrid/tiny LFU policies.
    pub const DEFAULT_POLICY_WINDOW_RATIO: f32 = 0.20;
    /// Minimum allowed protected window ratio for `TinyLFU` policies.
    pub const MIN_POLICY_WINDOW_RATIO: f32 = 0.05;
    /// Maximum allowed protected window ratio for `TinyLFU` policies.
    pub const MAX_POLICY_WINDOW_RATIO: f32 = 0.95;

    /// Create a new cache configuration with default values.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheConfig;
    ///
    /// let config = CacheConfig::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_bytes: Self::DEFAULT_MAX_BYTES,
            enable_persistence: true,
            cache_root: PathBuf::from(Self::DEFAULT_CACHE_ROOT),
            background_writer: true,
            policy_kind: CachePolicyKind::default(),
            policy_window_ratio: Self::DEFAULT_POLICY_WINDOW_RATIO,
        }
    }

    /// Create configuration from environment variables.
    ///
    /// Reads configuration from:
    /// - `SQRY_CACHE_MAX_BYTES`: Override max cache size
    /// - `SQRY_CACHE_DISABLE_PERSIST`: Set to `1` to disable persistence
    /// - `SQRY_CACHE_ROOT`: Custom cache directory
    ///
    /// Falls back to default values if environment variables are not set.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheConfig;
    ///
    /// // Reads from environment, falls back to defaults
    /// let config = CacheConfig::from_env();
    /// ```
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Self::new();

        // Override max_bytes from environment
        if let Ok(max_bytes_str) = std::env::var("SQRY_CACHE_MAX_BYTES")
            && let Ok(max_bytes) = max_bytes_str.parse::<u64>()
        {
            config = config.with_max_bytes(max_bytes);
        }

        // Override persistence from environment
        if let Ok(disable_persist) = std::env::var("SQRY_CACHE_DISABLE_PERSIST")
            && (disable_persist == "1" || disable_persist.eq_ignore_ascii_case("true"))
        {
            config = config.with_persistence(false);
        }

        // Override cache root from environment
        if let Ok(cache_root) = std::env::var("SQRY_CACHE_ROOT") {
            config = config.with_cache_root(PathBuf::from(cache_root));
        }

        // Override eviction policy from environment
        if let Ok(policy_str) = std::env::var("SQRY_CACHE_POLICY") {
            if let Some(kind) = CachePolicyKind::parse(&policy_str) {
                config = config.with_policy_kind(kind);
            } else {
                log::warn!(
                    "Invalid SQRY_CACHE_POLICY='{policy_str}' (expected lru|tiny_lfu|hybrid), falling back to LRU"
                );
            }
        }

        if let Ok(window_ratio_str) = std::env::var("SQRY_CACHE_POLICY_WINDOW")
            && let Ok(ratio) = window_ratio_str.parse::<f32>()
        {
            config = config.with_policy_window_ratio(ratio);
        }

        config
    }

    /// Set maximum cache size in bytes.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheConfig;
    ///
    /// let config = CacheConfig::new().with_max_bytes(100 * 1024 * 1024); // 100 MB
    /// ```
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Enable or disable persistent cache to disk.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheConfig;
    ///
    /// // Memory-only cache
    /// let config = CacheConfig::new().with_persistence(false);
    /// ```
    #[must_use]
    pub fn with_persistence(mut self, enable: bool) -> Self {
        self.enable_persistence = enable;
        self
    }

    /// Set cache root directory.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheConfig;
    /// use std::path::PathBuf;
    ///
    /// let config = CacheConfig::new().with_cache_root(PathBuf::from("/tmp/sqry-cache"));
    /// ```
    #[must_use]
    pub fn with_cache_root(mut self, cache_root: PathBuf) -> Self {
        self.cache_root = cache_root;
        self
    }

    /// Enable or disable background writer thread.
    ///
    /// When enabled, cache writes are performed asynchronously to avoid
    /// blocking query execution. Disable for testing or debugging.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheConfig;
    ///
    /// // Synchronous writes (useful for testing)
    /// let config = CacheConfig::new().with_background_writer(false);
    /// ```
    #[must_use]
    pub fn with_background_writer(mut self, enable: bool) -> Self {
        self.background_writer = enable;
        self
    }

    /// Override eviction policy kind.
    #[must_use]
    pub fn with_policy_kind(mut self, kind: CachePolicyKind) -> Self {
        self.policy_kind = kind;
        self
    }

    /// Override protected window ratio for hybrid/TinyLFU policies.
    #[must_use]
    pub fn with_policy_window_ratio(mut self, ratio: f32) -> Self {
        self.policy_window_ratio = Self::clamp_window_ratio(ratio);
        self
    }

    fn clamp_window_ratio(ratio: f32) -> f32 {
        if ratio.is_nan() || !ratio.is_finite() {
            Self::DEFAULT_POLICY_WINDOW_RATIO
        } else {
            ratio.clamp(Self::MIN_POLICY_WINDOW_RATIO, Self::MAX_POLICY_WINDOW_RATIO)
        }
    }

    /// Get maximum cache size in bytes.
    #[must_use]
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Check if persistence is enabled.
    #[must_use]
    pub fn is_persistence_enabled(&self) -> bool {
        self.enable_persistence
    }

    /// Get cache root directory.
    #[must_use]
    pub fn cache_root(&self) -> &PathBuf {
        &self.cache_root
    }

    /// Check if background writer is enabled.
    #[must_use]
    pub fn is_background_writer_enabled(&self) -> bool {
        self.background_writer
    }

    /// Get eviction policy kind.
    #[must_use]
    pub fn policy_kind(&self) -> CachePolicyKind {
        self.policy_kind
    }

    /// Get protected window ratio for TinyLFU/hybrid policies.
    #[must_use]
    pub fn policy_window_ratio(&self) -> f32 {
        self.policy_window_ratio
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Cache policy for language plugins.
///
/// Plugins can opt out of caching or specify custom TTL (time-to-live) values.
///
/// # Default Policy
///
/// By default, all plugins use `CachePolicy::Enabled` with a 1-hour TTL.
///
/// # Examples
///
/// ```rust,ignore
/// use sqry_core::cache::CachePolicy;
/// use sqry_core::plugin::LanguagePlugin;
/// use std::time::Duration;
///
/// impl LanguagePlugin for RustPlugin {
///     fn cache_policy(&self) -> CachePolicy {
///         // Stable grammar - long TTL
///         CachePolicy::Enabled {
///             ttl: Duration::from_secs(3600) // 1 hour
///         }
///     }
/// }
///
/// impl LanguagePlugin for ExperimentalPlugin {
///     fn cache_policy(&self) -> CachePolicy {
///         // Experimental - disable caching
///         CachePolicy::Disabled
///     }
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CachePolicy {
    /// Caching enabled with the specified time-to-live.
    ///
    /// Cache entries expire after the TTL and will be reparsed on next access.
    Enabled {
        /// Time-to-live for cache entries.
        ttl: Duration,
    },

    /// Caching disabled for this plugin.
    ///
    /// All queries will trigger fresh parsing, bypassing the cache entirely.
    Disabled,
}

impl CachePolicy {
    /// Default TTL for enabled caching (1 hour).
    pub const DEFAULT_TTL: Duration = Duration::from_secs(3600);

    /// Create an enabled policy with the default TTL.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CachePolicy;
    ///
    /// let policy = CachePolicy::default_enabled();
    /// ```
    #[must_use]
    pub fn default_enabled() -> Self {
        Self::Enabled {
            ttl: Self::DEFAULT_TTL,
        }
    }

    /// Create an enabled policy with a custom TTL.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CachePolicy;
    /// use std::time::Duration;
    ///
    /// let policy = CachePolicy::enabled(Duration::from_secs(7200)); // 2 hours
    /// ```
    #[must_use]
    pub fn enabled(ttl: Duration) -> Self {
        Self::Enabled { ttl }
    }

    /// Create a disabled policy.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CachePolicy;
    ///
    /// let policy = CachePolicy::disabled();
    /// ```
    #[must_use]
    pub fn disabled() -> Self {
        Self::Disabled
    }

    /// Check if caching is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled { .. })
    }

    /// Get the TTL if caching is enabled.
    #[must_use]
    pub fn ttl(&self) -> Option<Duration> {
        match self {
            Self::Enabled { ttl } => Some(*ttl),
            Self::Disabled => None,
        }
    }
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self::default_enabled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_config_default() {
        let config = CacheConfig::default();

        assert_eq!(config.max_bytes(), CacheConfig::DEFAULT_MAX_BYTES);
        assert!(config.is_persistence_enabled());
        assert_eq!(config.cache_root(), &PathBuf::from(".sqry-cache"));
        assert!(config.is_background_writer_enabled());
        assert_eq!(config.policy_kind(), CachePolicyKind::Lru);
        assert!(
            (config.policy_window_ratio() - CacheConfig::DEFAULT_POLICY_WINDOW_RATIO).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn test_cache_config_builder() {
        let config = CacheConfig::new()
            .with_max_bytes(100 * 1024 * 1024)
            .with_persistence(false)
            .with_cache_root(PathBuf::from("/tmp/cache"))
            .with_background_writer(false)
            .with_policy_kind(CachePolicyKind::TinyLfu)
            .with_policy_window_ratio(0.5);

        assert_eq!(config.max_bytes(), 100 * 1024 * 1024);
        assert!(!config.is_persistence_enabled());
        assert_eq!(config.cache_root(), &PathBuf::from("/tmp/cache"));
        assert!(!config.is_background_writer_enabled());
        assert_eq!(config.policy_kind(), CachePolicyKind::TinyLfu);
        assert!((config.policy_window_ratio() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_cache_config_from_env() {
        // Set environment variables for this test
        // SAFETY: We're in a test environment and immediately clean up after
        unsafe {
            std::env::set_var("SQRY_CACHE_MAX_BYTES", "104857600"); // 100 MB
            std::env::set_var("SQRY_CACHE_DISABLE_PERSIST", "1");
            std::env::set_var("SQRY_CACHE_ROOT", "/tmp/test-cache");
            std::env::set_var("SQRY_CACHE_POLICY", "tiny_lfu");
            std::env::set_var("SQRY_CACHE_POLICY_WINDOW", "0.33");
        }

        let config = CacheConfig::from_env();

        assert_eq!(config.max_bytes(), 104_857_600);
        assert!(!config.is_persistence_enabled());
        assert_eq!(config.cache_root(), &PathBuf::from("/tmp/test-cache"));
        assert_eq!(config.policy_kind(), CachePolicyKind::TinyLfu);
        assert!((config.policy_window_ratio() - 0.33).abs() < f32::EPSILON);

        // Clean up
        // SAFETY: We set these variables above, safe to remove them
        unsafe {
            std::env::remove_var("SQRY_CACHE_MAX_BYTES");
            std::env::remove_var("SQRY_CACHE_DISABLE_PERSIST");
            std::env::remove_var("SQRY_CACHE_ROOT");
            std::env::remove_var("SQRY_CACHE_POLICY");
            std::env::remove_var("SQRY_CACHE_POLICY_WINDOW");
        }
    }

    #[test]
    fn test_policy_window_ratio_clamp() {
        let config = CacheConfig::new()
            .with_policy_window_ratio(0.99)
            .with_policy_window_ratio(0.01)
            .with_policy_window_ratio(f32::NAN);
        assert!(
            (config.policy_window_ratio() - CacheConfig::DEFAULT_POLICY_WINDOW_RATIO).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn test_cache_policy_default() {
        let policy = CachePolicy::default();

        assert!(policy.is_enabled());
        assert_eq!(policy.ttl(), Some(Duration::from_secs(3600)));
    }

    #[test]
    fn test_cache_policy_enabled() {
        let policy = CachePolicy::enabled(Duration::from_secs(7200));

        assert!(policy.is_enabled());
        assert_eq!(policy.ttl(), Some(Duration::from_secs(7200)));
    }

    #[test]
    fn test_cache_policy_disabled() {
        let policy = CachePolicy::disabled();

        assert!(!policy.is_enabled());
        assert_eq!(policy.ttl(), None);
    }

    #[test]
    fn test_cache_policy_equality() {
        let policy1 = CachePolicy::enabled(Duration::from_secs(3600));
        let policy2 = CachePolicy::enabled(Duration::from_secs(3600));
        let policy3 = CachePolicy::enabled(Duration::from_secs(7200));
        let policy4 = CachePolicy::disabled();

        assert_eq!(policy1, policy2);
        assert_ne!(policy1, policy3);
        assert_ne!(policy1, policy4);
    }
}
