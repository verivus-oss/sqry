//! Regex compilation caching for query performance
//!
//! Provides thread-safe LRU cache for compiled regex patterns to avoid
//! redundant compilation during predicate evaluation (P2-1).
//!
//! Supports both standard regexes and lookaround patterns (P2-10):
//! - Standard patterns use `regex::Regex` for performance
//! - Lookaround patterns (`(?=`, `(?!`, `(?<=`, `(?<!`) use `fancy_regex::Regex`

use crate::cache::CacheConfig;
use crate::cache::policy::{
    CacheAdmission, CachePolicy, CachePolicyConfig, CachePolicyKind, build_cache_policy,
};
use log::debug;
use lru::LruCache;
use regex::Regex;
use std::hash::{Hash, Hasher};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Compiled regex that supports both standard and lookaround patterns
#[derive(Clone)]
pub enum CompiledRegex {
    /// Standard regex (faster, no lookaround support)
    Standard(Arc<Regex>),
    /// Fancy regex with lookaround support
    Fancy(Arc<fancy_regex::Regex>),
}

impl CompiledRegex {
    /// Check if the pattern matches the text
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        match self {
            CompiledRegex::Standard(re) => re.is_match(text),
            CompiledRegex::Fancy(re) => re.is_match(text).unwrap_or(false),
        }
    }
}

/// Check if a pattern contains lookaround assertions
fn has_lookaround(pattern: &str) -> bool {
    pattern.contains("(?=")
        || pattern.contains("(?!")
        || pattern.contains("(?<=")
        || pattern.contains("(?<!")
}

/// Error type for regex compilation that handles both standard and fancy regex errors
#[derive(Debug)]
pub enum RegexCompileError {
    /// Standard regex compilation error
    Standard(regex::Error),
    /// Fancy regex compilation error (lookaround patterns)
    /// Boxed to reduce `Result` size (`fancy_regex::Error` is 136+ bytes)
    Fancy(Box<fancy_regex::Error>),
}

impl std::fmt::Display for RegexCompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegexCompileError::Standard(e) => write!(f, "{e}"),
            RegexCompileError::Fancy(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RegexCompileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RegexCompileError::Standard(e) => Some(e),
            RegexCompileError::Fancy(e) => Some(e.as_ref()),
        }
    }
}

impl From<regex::Error> for RegexCompileError {
    fn from(err: regex::Error) -> Self {
        RegexCompileError::Standard(err)
    }
}

impl From<fancy_regex::Error> for RegexCompileError {
    fn from(err: fancy_regex::Error) -> Self {
        RegexCompileError::Fancy(Box::new(err))
    }
}

/// Cache key for compiled regexes (pattern + flags)
#[derive(Clone, Eq, PartialEq)]
struct RegexCacheKey {
    pattern: String,
    case_insensitive: bool,
    multiline: bool,
    dot_all: bool,
}

impl Hash for RegexCacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.pattern.hash(state);
        self.case_insensitive.hash(state);
        self.multiline.hash(state);
        self.dot_all.hash(state);
    }
}

/// Thread-safe LRU cache for compiled regexes
pub struct RegexCache {
    cache: Arc<Mutex<LruCache<RegexCacheKey, CompiledRegex>>>,
    capacity: usize,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    policy: Arc<dyn CachePolicy<RegexCacheKey>>,
}

impl RegexCache {
    /// Create new cache with specified capacity
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (kind, window_ratio) = Self::policy_params_from_env();
        Self::with_policy(capacity, kind, window_ratio)
    }

    /// Get or compile a regex (cache hit or compile + insert)
    ///
    /// Automatically detects lookaround patterns and uses `fancy_regex` for those,
    /// falling back to standard `regex` for better performance on simple patterns.
    ///
    /// # Errors
    ///
    /// Returns a regex compilation error when the pattern or flags are invalid.
    ///
    /// # Panics
    ///
    /// Panics if the internal cache mutex is poisoned (should not occur in normal operation).
    pub fn get_or_compile(
        &self,
        pattern: &str,
        case_insensitive: bool,
        multiline: bool,
        dot_all: bool,
    ) -> Result<CompiledRegex, RegexCompileError> {
        let key = RegexCacheKey {
            pattern: pattern.to_string(),
            case_insensitive,
            multiline,
            dot_all,
        };

        self.handle_policy_evictions();

        {
            let mut cache = self.cache.lock().expect("regex cache mutex poisoned");
            if let Some(regex) = cache.get(&key) {
                self.hits.fetch_add(1, Ordering::Relaxed);
                let _ = self.policy.record_hit(&key);
                return Ok(regex.clone());
            }
        }

        self.misses.fetch_add(1, Ordering::Relaxed);

        // Slow path: compile new regex (outside mutex)
        // P2-10: Use fancy_regex for lookaround patterns, standard regex otherwise
        let compiled = if has_lookaround(pattern) {
            // Build pattern with flags for fancy_regex
            // fancy_regex uses inline flags: (?i) for case-insensitive, (?m) for multiline, (?s) for dot_all
            let mut flag_prefix = String::new();
            if case_insensitive {
                flag_prefix.push_str("(?i)");
            }
            if multiline {
                flag_prefix.push_str("(?m)");
            }
            if dot_all {
                flag_prefix.push_str("(?s)");
            }
            let full_pattern = format!("{flag_prefix}{pattern}");
            let fancy_re = fancy_regex::Regex::new(&full_pattern)?;
            CompiledRegex::Fancy(Arc::new(fancy_re))
        } else {
            // Standard regex for performance
            let mut builder = regex::RegexBuilder::new(pattern);
            builder
                .case_insensitive(case_insensitive)
                .multi_line(multiline)
                .dot_matches_new_line(dot_all);
            let re = builder.build()?;
            CompiledRegex::Standard(Arc::new(re))
        };

        if matches!(self.policy.admit(&key, 1), CacheAdmission::Rejected) {
            debug!(
                "regex cache policy {:?} rejected pattern {:?}",
                self.policy.kind(),
                key.pattern
            );
            return Ok(compiled);
        }

        // Insert into cache (mutex held briefly)
        {
            let mut cache = self.cache.lock().expect("regex cache mutex poisoned");
            if cache.len() == self.capacity
                && let Some((evicted_key, _)) = cache.pop_lru()
            {
                self.policy.invalidate(&evicted_key);
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
            cache.put(key, compiled.clone());
        }

        self.handle_policy_evictions();

        Ok(compiled)
    }

    /// Get cache statistics (for testing)
    ///
    /// # Panics
    ///
    /// Panics if the regex cache mutex is poisoned.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.cache.lock().expect("regex cache mutex poisoned").len()
    }

    /// Returns true when the cache holds no compiled regex entries (test-only helper).
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn handle_policy_evictions(&self) {
        let evicted = self.policy.drain_evictions();
        if evicted.is_empty() {
            return;
        }
        let mut cache = self.cache.lock().expect("regex cache mutex poisoned");
        for eviction in evicted {
            if cache.pop(&eviction.key).is_some() {
                self.evictions.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn with_policy(capacity: usize, kind: CachePolicyKind, window_ratio: f32) -> Self {
        let normalized_capacity = capacity.max(1);
        let config = CachePolicyConfig::new(kind, normalized_capacity as u64, window_ratio);
        Self {
            cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(normalized_capacity).expect("capacity must be > 0"),
            ))),
            capacity: normalized_capacity,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            policy: build_cache_policy(&config),
        }
    }

    fn policy_params_from_env() -> (CachePolicyKind, f32) {
        let cfg = CacheConfig::from_env();
        (cfg.policy_kind(), cfg.policy_window_ratio())
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

/// Global singleton instance (lazy-initialized)
static REGEX_CACHE: OnceLock<RegexCache> = OnceLock::new();

fn get_global_cache() -> &'static RegexCache {
    REGEX_CACHE.get_or_init(|| {
        let size = std::env::var("SQRY_REGEX_CACHE_SIZE")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&s| (1..=10_000).contains(&s))
            .unwrap_or(100);

        RegexCache::new(size)
    })
}

/// Public API: get or compile a regex
///
/// Automatically detects lookaround patterns and uses `fancy_regex` for those,
/// falling back to standard `regex` for better performance on simple patterns.
///
/// # Errors
///
/// Returns a regex compilation error when the pattern or flags are invalid.
pub fn get_or_compile_regex(
    pattern: &str,
    case_insensitive: bool,
    multiline: bool,
    dot_all: bool,
) -> Result<CompiledRegex, RegexCompileError> {
    // # Panics
    // Panics if the global cache mutex is poisoned (unexpected).
    get_global_cache().get_or_compile(pattern, case_insensitive, multiline, dot_all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::policy::CachePolicyKind;

    #[test]
    fn test_cache_hit_reuses_compiled_regex() {
        let cache = RegexCache::new(10);

        // First call: miss, compiles
        let re1 = cache.get_or_compile("foo.*", false, false, false).unwrap();
        assert_eq!(cache.len(), 1);

        // Second call: hit, reuses
        let _re2 = cache.get_or_compile("foo.*", false, false, false).unwrap();
        assert_eq!(cache.len(), 1);
        // Both should match
        assert!(re1.is_match("foobar"));
    }

    #[test]
    fn test_different_flags_create_separate_entries() {
        let cache = RegexCache::new(10);

        let re1 = cache.get_or_compile("foo", false, false, false).unwrap();
        let re2 = cache.get_or_compile("foo", true, false, false).unwrap(); // case_insensitive=true

        assert_eq!(cache.len(), 2); // Two different cache entries
        assert!(re1.is_match("foo"));
        assert!(!re1.is_match("FOO")); // Case-sensitive
        assert!(re2.is_match("FOO")); // Case-insensitive
    }

    #[test]
    fn test_lru_eviction_works() {
        let cache = RegexCache::new(2);

        cache.get_or_compile("a", false, false, false).unwrap();
        cache.get_or_compile("b", false, false, false).unwrap();
        assert_eq!(cache.len(), 2);

        // Third entry evicts LRU (should evict "a")
        cache.get_or_compile("c", false, false, false).unwrap();
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn test_compilation_errors_not_cached() {
        let cache = RegexCache::new(10);

        // Invalid regex pattern
        assert!(
            cache
                .get_or_compile("[invalid", false, false, false)
                .is_err()
        );
        assert_eq!(cache.len(), 0); // Error not cached
    }

    #[test]
    fn tiny_lfu_rejects_cold_bursts() {
        let cache = RegexCache::with_policy_kind(3, CachePolicyKind::TinyLfu);

        let hot = cache
            .get_or_compile("hot", false, false, false)
            .expect("compile hot regex");
        for _ in 0..10 {
            let _ = cache
                .get_or_compile("hot", false, false, false)
                .expect("warm hot regex");
        }

        for i in 0..30 {
            let pattern = format!("cold{i}");
            let _ = cache
                .get_or_compile(&pattern, false, false, false)
                .expect("compile cold regex");
        }

        let warmed = cache
            .get_or_compile("hot", false, false, false)
            .expect("retrieve hot regex");
        // Both should still match the same
        assert!(hot.is_match("hot"));
        assert!(warmed.is_match("hot"));

        let metrics = cache.policy_metrics();
        assert!(
            metrics.lfu_rejects > 0,
            "expected TinyLFU to reject some cold entries"
        );
    }

    // P2-10: Tests for lookaround pattern support
    #[test]
    fn test_lookahead_pattern_compiles() {
        let cache = RegexCache::new(10);
        let re = cache
            .get_or_compile("foo(?=bar)", false, false, false)
            .expect("lookahead should compile");
        assert!(re.is_match("foobar"));
        assert!(!re.is_match("foobaz"));
    }

    #[test]
    fn test_lookbehind_pattern_compiles() {
        let cache = RegexCache::new(10);
        let re = cache
            .get_or_compile("(?<=test_)foo", false, false, false)
            .expect("lookbehind should compile");
        assert!(re.is_match("test_foo"));
        assert!(!re.is_match("prod_foo"));
    }

    #[test]
    fn test_negative_lookahead_pattern() {
        let cache = RegexCache::new(10);
        let re = cache
            .get_or_compile("foo(?!bar)", false, false, false)
            .expect("negative lookahead should compile");
        assert!(re.is_match("foobaz"));
        assert!(!re.is_match("foobar"));
    }

    #[test]
    fn test_negative_lookbehind_pattern() {
        let cache = RegexCache::new(10);
        let re = cache
            .get_or_compile("(?<!test_)foo", false, false, false)
            .expect("negative lookbehind should compile");
        assert!(re.is_match("prod_foo"));
        assert!(!re.is_match("test_foo"));
    }

    #[test]
    fn test_lookaround_with_flags() {
        let cache = RegexCache::new(10);
        // Case-insensitive lookahead
        let re = cache
            .get_or_compile("(?<=TEST_)foo", true, false, false)
            .expect("lookaround with flags should compile");
        assert!(re.is_match("TEST_foo"));
        assert!(re.is_match("test_foo")); // Case insensitive
        assert!(re.is_match("TEST_FOO")); // Case insensitive applies to whole pattern
    }
}
