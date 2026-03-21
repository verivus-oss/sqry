//! Cache key generation for context-aware caching.

use std::hash::{Hash, Hasher};

/// Context-aware cache key.
///
/// The key includes the query text plus context that affects translation:
/// - Languages specified
/// - Path root (working directory)
/// - Default limit setting
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct CacheKey {
    /// The normalized query text
    query: String,
    /// Languages specified in context
    languages: Vec<String>,
    /// Path root (affects path validation)
    path_root: Option<String>,
    /// Default limit (affects command generation)
    default_limit: u32,
}

impl CacheKey {
    /// Create a new cache key.
    #[must_use]
    pub fn new(
        query: &str,
        languages: &[String],
        path_root: Option<String>,
        default_limit: u32,
    ) -> Self {
        Self {
            query: query.to_lowercase().trim().to_string(),
            languages: languages.to_vec(),
            path_root,
            default_limit,
        }
    }

    /// Get the query text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }
}

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.query.hash(state);
        self.languages.hash(state);
        self.path_root.hash(state);
        self.default_limit.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    fn hash_key(key: &CacheKey) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn test_key_normalization() {
        let key1 = CacheKey::new("Find Foo", &[], None, 100);
        let key2 = CacheKey::new("  find foo  ", &[], None, 100);

        assert_eq!(key1.query(), key2.query());
    }

    #[test]
    fn test_key_equality() {
        let key1 = CacheKey::new("find foo", &["rust".to_string()], None, 100);
        let key2 = CacheKey::new("find foo", &["rust".to_string()], None, 100);

        assert_eq!(key1, key2);
        assert_eq!(hash_key(&key1), hash_key(&key2));
    }

    #[test]
    fn test_key_inequality_language() {
        let key1 = CacheKey::new("find foo", &["rust".to_string()], None, 100);
        let key2 = CacheKey::new("find foo", &["python".to_string()], None, 100);

        assert_ne!(key1, key2);
    }

    #[test]
    fn test_key_inequality_limit() {
        let key1 = CacheKey::new("find foo", &[], None, 100);
        let key2 = CacheKey::new("find foo", &[], None, 50);

        assert_ne!(key1, key2);
    }

    #[test]
    fn test_key_inequality_path_root() {
        let key1 = CacheKey::new("find foo", &[], Some("/project1".to_string()), 100);
        let key2 = CacheKey::new("find foo", &[], Some("/project2".to_string()), 100);

        assert_ne!(key1, key2);
    }
}
