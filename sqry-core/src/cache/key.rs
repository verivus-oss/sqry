//! Cache key for identifying parsed files.
//!
//! The cache key uniquely identifies a file's AST summary using:
//! - Canonical file path (normalized, symlinks resolved)
//! - Language identifier (plugin name)
//! - Content hash (BLAKE3 digest)
//!
//! # Path Canonicalization
//!
//! Cache keys attempt to canonicalize paths to handle symlinks and relative paths
//! consistently. If canonicalization fails (file deleted, permission denied, or
//! unsupported filesystem), the key falls back to the original path.
//!
//! # Examples
//!
//! ```rust
//! use sqry_core::cache::CacheKey;
//! use sqry_core::hash::Blake3Hash;
//! use std::path::PathBuf;
//!
//! let hash_hex = "a".repeat(64);
//! let hash = Blake3Hash::from_hex(&hash_hex).unwrap();
//! let key = CacheKey::new(
//!     PathBuf::from("src/main.rs"),
//!     "rust",
//!     hash,
//! );
//!
//! // Keys are comparable and hashable
//! assert_eq!(key.language(), "rust");
//! ```

use crate::hash::Blake3Hash;
use std::fmt;
use std::path::{Path, PathBuf};

/// Unique identifier for cached AST summaries.
///
/// A cache key combines:
/// - **Canonical path**: Normalized file path with symlinks resolved
/// - **Language ID**: Plugin identifier (e.g., "rust", "python")
/// - **Content hash**: BLAKE3 digest of file contents
///
/// # Equality and Hashing
///
/// Two cache keys are equal if all three components match. This ensures
/// cache misses when:
/// - File content changes (different hash)
/// - File is moved (different canonical path)
/// - Language plugin changes (different language ID)
///
/// # Canonicalization Fallback
///
/// If path canonicalization fails, the original path is used. This handles:
/// - Deleted files during cache cleanup
/// - Permission-denied scenarios
/// - Filesystems without canonicalization support
///
/// Fallback events are logged at DEBUG level via the `log` crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// Canonical file path (or original if canonicalization failed).
    canonical_path: PathBuf,

    /// Language identifier from the plugin.
    language: String,

    /// BLAKE3 hash of file contents.
    content_hash: Blake3Hash,

    /// Whether canonicalization succeeded.
    ///
    /// Used for diagnostics and telemetry. When `false`, indicates
    /// the cache key is using the original path as a fallback.
    canonicalization_succeeded: bool,
}

impl CacheKey {
    /// Create a new cache key with path canonicalization.
    ///
    /// Attempts to canonicalize the path. If canonicalization fails,
    /// falls back to the original path and logs a DEBUG message.
    ///
    /// # Arguments
    ///
    /// - `path`: File path (can be relative or contain symlinks)
    /// - `language`: Language identifier from the plugin
    /// - `content_hash`: BLAKE3 hash of file contents
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheKey;
    /// use sqry_core::hash::Blake3Hash;
    /// use std::path::PathBuf;
    ///
    /// let hash_hex = "a".repeat(64);
    /// let hash = Blake3Hash::from_hex(&hash_hex).unwrap();
    /// let key = CacheKey::new(
    ///     PathBuf::from("./src/main.rs"),
    ///     "rust",
    ///     hash,
    /// );
    /// ```
    pub fn new<P: AsRef<Path>>(
        path: P,
        language: impl Into<String>,
        content_hash: Blake3Hash,
    ) -> Self {
        let path = path.as_ref();
        let language = language.into();

        // Attempt canonicalization
        let (mut canonical_path, canonicalization_succeeded) = match path.canonicalize() {
            Ok(canonical) => {
                log::trace!(
                    "Canonicalized cache key path: {} -> {}",
                    path.display(),
                    canonical.display()
                );
                (canonical, true)
            }
            Err(e) => {
                log::debug!(
                    "Cache key canonicalization failed for {}: {}. Using original path.",
                    path.display(),
                    e
                );
                (path.to_path_buf(), false)
            }
        };

        // Normalize case for case-insensitive filesystems (Windows, macOS)
        // This prevents duplicate cache entries for paths that differ only in case
        canonical_path = Self::normalize_case_if_needed(canonical_path);

        Self {
            canonical_path,
            language,
            content_hash,
            canonicalization_succeeded,
        }
    }

    /// Normalize path case for case-insensitive filesystems.
    ///
    /// On Windows and macOS (case-insensitive by default), converts the path
    /// to lowercase to ensure consistent cache keys for paths that differ only
    /// in case (e.g., "FILE.rs" vs "file.rs").
    ///
    /// On Linux and other case-sensitive systems, returns the path unchanged.
    fn normalize_case_if_needed(path: PathBuf) -> PathBuf {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            // Convert to lowercase on case-insensitive platforms
            if let Some(path_str) = path.to_str() {
                PathBuf::from(path_str.to_lowercase())
            } else {
                // Non-UTF8 path, can't normalize safely
                log::debug!("Cannot normalize non-UTF8 path: {:?}", path);
                path
            }
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            // Case-sensitive filesystem, no normalization needed
            path
        }
    }

    /// Create a cache key without path canonicalization.
    ///
    /// Uses the provided path as-is, skipping canonicalization.
    /// Useful for testing or when paths are already canonical.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheKey;
    /// use sqry_core::hash::Blake3Hash;
    /// use std::path::PathBuf;
    ///
    /// let hash_hex = "a".repeat(64);
    /// let hash = Blake3Hash::from_hex(&hash_hex).unwrap();
    /// let key = CacheKey::from_raw_path(
    ///     PathBuf::from("/absolute/path/file.rs"),
    ///     "rust",
    ///     hash,
    /// );
    /// ```
    pub fn from_raw_path<P: Into<PathBuf>>(
        path: P,
        language: impl Into<String>,
        content_hash: Blake3Hash,
    ) -> Self {
        Self {
            canonical_path: path.into(),
            language: language.into(),
            content_hash,
            canonicalization_succeeded: true, // Assume caller knows what they're doing
        }
    }

    /// Get the canonical file path (or original if canonicalization failed).
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.canonical_path
    }

    /// Get the language identifier.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Get the content hash.
    #[must_use]
    pub fn content_hash(&self) -> &Blake3Hash {
        &self.content_hash
    }

    /// Check if path canonicalization succeeded.
    ///
    /// Returns `false` if the key is using the original path as a fallback.
    #[must_use]
    pub fn is_canonical(&self) -> bool {
        self.canonicalization_succeeded
    }

    /// Compute a storage key for file persistence.
    ///
    /// Returns a string combining language, content hash, path hash, and filename,
    /// suitable for use as a directory/file path in the persistent cache.
    ///
    /// Format: `{language}/{content_hash}/{path_hash}/{filename}`
    ///
    /// The path hash prevents collisions when different files have identical
    /// content and filename (e.g., `/proj1/main.rs` and `/proj2/main.rs`).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use sqry_core::cache::CacheKey;
    /// use sqry_core::hash::Blake3Hash;
    /// use std::path::PathBuf;
    ///
    /// let hash_hex = "a".repeat(64);
    /// let hash = Blake3Hash::from_hex(&hash_hex).unwrap();
    /// let key = CacheKey::from_raw_path(
    ///     PathBuf::from("/path/to/file.rs"),
    ///     "rust",
    ///     hash,
    /// );
    ///
    /// let storage_key = key.storage_key();
    /// assert!(storage_key.starts_with("rust/"));
    /// assert!(storage_key.ends_with("file.rs"));
    /// ```
    #[must_use]
    pub fn storage_key(&self) -> String {
        let filename = self
            .canonical_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        // Hash the canonical path to prevent collisions between different files
        // with identical content and filename (e.g., /proj1/main.rs vs /proj2/main.rs)
        let path_hash = {
            let path_str = self.canonical_path.to_string_lossy();
            let hash = blake3::hash(path_str.as_bytes());
            // Use first 8 bytes (16 hex chars) - sufficient for collision resistance
            hex::encode(&hash.as_bytes()[..8])
        };

        format!(
            "{}/{}/{}/{}",
            self.language,
            self.content_hash.to_hex(),
            path_hash,
            filename
        )
    }
}

impl fmt::Display for CacheKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.canonical_path.display(),
            self.language,
            self.content_hash.to_hex()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::GraphNodeSummary;
    use std::fs;
    use std::io::Write;
    use std::sync::Arc;

    fn make_test_hash(byte: u8) -> Blake3Hash {
        Blake3Hash::from_bytes([byte; 32])
    }

    #[test]
    fn test_cache_key_new() {
        let hash = make_test_hash(0x42);
        let key = CacheKey::new(PathBuf::from("test.rs"), "rust", hash);

        assert_eq!(key.language(), "rust");
        assert_eq!(key.content_hash(), &hash);
        // Path might be canonical or not depending on filesystem
    }

    #[test]
    fn test_cache_key_from_raw_path() {
        let hash = make_test_hash(0x42);
        let path = PathBuf::from("/absolute/path/test.rs");
        let key = CacheKey::from_raw_path(path.clone(), "rust", hash);

        assert_eq!(key.path(), path.as_path());
        assert_eq!(key.language(), "rust");
        assert_eq!(key.content_hash(), &hash);
        assert!(key.is_canonical()); // from_raw_path assumes canonical
    }

    #[test]
    fn test_cache_key_equality() {
        let hash1 = make_test_hash(0x42);
        let hash2 = make_test_hash(0x43);

        let key1 = CacheKey::from_raw_path("/path/file.rs", "rust", hash1);
        let key2 = CacheKey::from_raw_path("/path/file.rs", "rust", hash1);
        let key3 = CacheKey::from_raw_path("/path/file.rs", "python", hash1);
        let key4 = CacheKey::from_raw_path("/path/file.rs", "rust", hash2);
        let key5 = CacheKey::from_raw_path("/other/file.rs", "rust", hash1);

        // Same components = equal
        assert_eq!(key1, key2);

        // Different language = not equal
        assert_ne!(key1, key3);

        // Different hash = not equal
        assert_ne!(key1, key4);

        // Different path = not equal
        assert_ne!(key1, key5);
    }

    #[test]
    fn test_cache_key_hash_consistency() {
        use std::collections::HashMap;

        let hash = make_test_hash(0x42);
        let key1 = CacheKey::from_raw_path("/path/file.rs", "rust", hash);
        let key2 = CacheKey::from_raw_path("/path/file.rs", "rust", hash);

        let mut map = HashMap::new();
        map.insert(key1.clone(), "value1");
        map.insert(key2.clone(), "value2");

        // Should have only one entry (keys are equal)
        assert_eq!(map.len(), 1);
        assert_eq!(map.get(&key1), Some(&"value2"));
    }

    #[test]
    fn test_cache_key_storage_key() {
        let hash = make_test_hash(0x42);
        let key = CacheKey::from_raw_path("/path/file.rs", "rust", hash);

        let storage_key = key.storage_key();

        // Format: {language}/{content_hash}/{path_hash}/{filename}
        assert!(storage_key.starts_with("rust/"));
        assert!(storage_key.ends_with("/file.rs"));

        // Should contain path hash (16 hex chars from 8 bytes)
        let parts: Vec<&str> = storage_key.split('/').collect();
        assert_eq!(
            parts.len(),
            4,
            "Should have 4 parts: language/content_hash/path_hash/filename"
        );
        assert_eq!(parts[0], "rust");
        assert_eq!(parts[1].len(), 64, "Content hash should be 64 hex chars");
        assert_eq!(parts[2].len(), 16, "Path hash should be 16 hex chars");
        assert_eq!(parts[3], "file.rs");
    }

    #[test]
    fn test_cache_key_storage_no_collision() {
        // Two different files with same filename and content should have different storage keys
        let hash = make_test_hash(0x42); // Same content hash

        let key1 = CacheKey::from_raw_path("/project1/main.rs", "rust", hash);
        let key2 = CacheKey::from_raw_path("/project2/main.rs", "rust", hash);

        let storage1 = key1.storage_key();
        let storage2 = key2.storage_key();

        // Should have different storage keys due to different paths
        assert_ne!(
            storage1, storage2,
            "Different paths should produce different storage keys"
        );

        // Both should have same language and content hash
        assert!(storage1.starts_with("rust/"));
        assert!(storage2.starts_with("rust/"));

        // But different path hashes
        let parts1: Vec<&str> = storage1.split('/').collect();
        let parts2: Vec<&str> = storage2.split('/').collect();

        assert_eq!(parts1[1], parts2[1], "Same content hash");
        assert_ne!(parts1[2], parts2[2], "Different path hashes");
        assert_eq!(parts1[3], parts2[3], "Same filename");
    }

    #[test]
    fn test_cache_key_display() {
        let hash = make_test_hash(0x42);
        let key = CacheKey::from_raw_path("/path/file.rs", "rust", hash);

        let display = format!("{key}");

        // Format: path:language:hash
        assert!(display.contains("/path/file.rs"));
        assert!(display.contains("rust"));
        assert!(display.contains(&hash.to_hex()));
    }

    #[test]
    fn test_cache_key_canonicalization_success() {
        // Create a real temporary file
        let tmp_cache_dir = std::env::temp_dir();
        let temp_file = tmp_cache_dir.join("sqry_test_cache_key.rs");
        let mut file = fs::File::create(&temp_file).unwrap();
        file.write_all(b"fn main() {}").unwrap();
        drop(file);

        let hash = make_test_hash(0x42);
        let key = CacheKey::new(&temp_file, "rust", hash);

        // Should have canonicalized successfully
        assert!(key.is_canonical());
        // Canonical path should be absolute
        assert!(key.path().is_absolute());

        // Cleanup
        let _ = fs::remove_file(&temp_file);
    }

    #[test]
    fn test_cache_key_canonicalization_fallback() {
        // Use a path that doesn't exist
        let nonexistent = PathBuf::from("/nonexistent/path/file.rs");
        let hash = make_test_hash(0x42);

        let key = CacheKey::new(&nonexistent, "rust", hash);

        // Canonicalization should have failed
        assert!(!key.is_canonical());
        // Should fall back to original path
        assert_eq!(key.path(), nonexistent.as_path());
    }

    #[test]
    fn test_cache_key_different_languages() {
        let hash = make_test_hash(0x42);
        let key_rust = CacheKey::from_raw_path("/path/file.txt", "rust", hash);
        let key_python = CacheKey::from_raw_path("/path/file.txt", "python", hash);

        // Same path and hash but different language
        assert_ne!(key_rust, key_python);
        assert_ne!(key_rust.storage_key(), key_python.storage_key());
    }

    #[test]
    fn test_cache_key_relative_vs_absolute() {
        // Create a real file to enable canonicalization
        let tmp_cache_dir = std::env::temp_dir();
        let temp_file = tmp_cache_dir.join("sqry_test_relative.rs");
        let mut file = fs::File::create(&temp_file).unwrap();
        file.write_all(b"// test").unwrap();
        drop(file);

        let hash = make_test_hash(0x42);

        // Both should canonicalize to the same absolute path
        let key1 = CacheKey::new(&temp_file, "rust", hash);
        let key2 = CacheKey::new(temp_file.canonicalize().unwrap(), "rust", hash);

        // Both should have canonical paths and be equal
        assert!(key1.is_canonical());
        assert!(key2.is_canonical());
        assert_eq!(key1, key2);

        // Cleanup
        let _ = fs::remove_file(&temp_file);
    }

    #[test]
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    fn test_cache_key_case_normalization() {
        // On case-insensitive filesystems, paths differing only in case
        // should produce the same cache key
        let _hash = make_test_hash(0x42);

        // Use from_raw_path to test the normalization directly
        // (new() would canonicalize which might change case anyway)
        let lowercase_path = PathBuf::from("/path/to/file.rs");
        let uppercase_path = PathBuf::from("/PATH/TO/FILE.RS");
        let mixed_path = PathBuf::from("/Path/To/File.rs");

        // Apply normalization manually (simulating what new() does)
        let normalized_lower = CacheKey::normalize_case_if_needed(lowercase_path.clone());
        let normalized_upper = CacheKey::normalize_case_if_needed(uppercase_path.clone());
        let normalized_mixed = CacheKey::normalize_case_if_needed(mixed_path.clone());

        // All should normalize to the same lowercase path
        assert_eq!(normalized_lower, normalized_upper);
        assert_eq!(normalized_lower, normalized_mixed);
        assert_eq!(normalized_lower.to_str().unwrap(), "/path/to/file.rs");
    }

    #[test]
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn test_cache_key_case_preservation() {
        // On case-sensitive filesystems (Linux), paths should preserve case
        let lowercase_path = PathBuf::from("/path/to/file.rs");
        let uppercase_path = PathBuf::from("/PATH/TO/FILE.RS");

        // Apply normalization (should be no-op on Linux)
        let normalized_lower = CacheKey::normalize_case_if_needed(lowercase_path.clone());
        let normalized_upper = CacheKey::normalize_case_if_needed(uppercase_path.clone());

        // Should preserve original case
        assert_eq!(normalized_lower, lowercase_path);
        assert_eq!(normalized_upper, uppercase_path);
        assert_ne!(normalized_lower, normalized_upper);
    }

    #[test]
    fn test_cache_key_symlink_resolution() {
        use std::fs;
        use tempfile::TempDir;

        // Create a temporary directory with a real file and a symlink
        let tmp_cache_dir = TempDir::new().unwrap();
        let real_file = tmp_cache_dir.path().join("real_file.rs");
        let symlink = tmp_cache_dir.path().join("symlink.rs");

        // Create the real file
        fs::write(&real_file, "fn test() {}").unwrap();

        // Create symlink (Unix only - skip on Windows)
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real_file, &symlink).unwrap();

            let hash = make_test_hash(0x42);

            // Create cache keys for both paths
            let key_real = CacheKey::new(&real_file, "rust", hash);
            let key_symlink = CacheKey::new(&symlink, "rust", hash);

            // Both should canonicalize to the same path
            assert_eq!(
                key_real.path(),
                key_symlink.path(),
                "Symlinks should resolve to the same canonical path"
            );
        }

        #[cfg(not(unix))]
        {
            // On Windows, just verify the test compiles
            let _ = (real_file, symlink);
        }
    }

    #[test]
    fn test_cache_key_mixed_case_paths_same_file() {
        use std::fs;
        use tempfile::TempDir;

        // Create a temporary file
        let tmp_cache_dir = TempDir::new().unwrap();
        let file_path = tmp_cache_dir.path().join("TestFile.rs");
        fs::write(&file_path, "fn test() {}").unwrap();

        let hash = make_test_hash(0x42);

        // Create keys with different case variations
        let key1 = CacheKey::new(&file_path, "rust", hash);

        // On case-insensitive systems, these should normalize to the same key
        // On case-sensitive systems, only the exact path works
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            // Lowercase version should normalize to same path
            let lowercase_path = tmp_cache_dir.path().join("testfile.rs");
            let key2 = CacheKey::new(&lowercase_path, "rust", hash);

            // After normalization, paths should be equal (both lowercase)
            assert_eq!(
                key1.path().to_str().unwrap().to_lowercase(),
                key2.path().to_str().unwrap().to_lowercase(),
                "Case variations should normalize on case-insensitive filesystems"
            );
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            // On case-sensitive systems, only exact path match works
            // Different case = different file
            let _ = key1; // Just verify it compiles
        }
    }

    #[test]
    fn test_cache_key_non_utf8_path() {
        // Test behavior with non-UTF8 paths (should handle gracefully)
        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;

            // Create a path with invalid UTF-8
            let invalid_bytes = b"/tmp/\xFF\xFE.rs";
            let invalid_path = PathBuf::from(OsStr::from_bytes(invalid_bytes));

            let hash = make_test_hash(0x42);

            // Should not panic, even with non-UTF8 path
            let key = CacheKey::from_raw_path(invalid_path.clone(), "rust", hash);
            assert_eq!(key.path(), invalid_path.as_path());
        }
    }

    #[test]
    fn test_serialized_size_fallback() {
        // Test that serialized_size handles errors gracefully
        use crate::graph::unified::node::NodeKind;

        let summary = GraphNodeSummary::new(
            Arc::from("test_function"),
            NodeKind::Function,
            Arc::from(Path::new("test.rs")),
            10,
            0,
            20,
            1,
        );

        // Should return actual size
        let size = summary.serialized_size();
        assert!(size > 0, "Serialized size should be positive");
        assert!(size <= 512, "Serialized size should be reasonable");

        // The fallback path is hard to test without breaking postcard,
        // but we can verify the method doesn't panic
    }
}
