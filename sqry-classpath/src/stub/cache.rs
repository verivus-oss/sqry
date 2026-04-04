//! Per-JAR stub cache keyed by SHA-256 hash of the JAR file.
//!
//! Cache location: `.sqry/classpath/jars/{hash}.stub`
//! Format: postcard binary serialization of `Vec<ClassStub>`
//!
//! The cache is content-addressed: the cache key is derived from the full
//! SHA-256 hash of the JAR file contents. When a JAR's contents change,
//! its hash changes and the old cache entry becomes orphaned (eventually
//! cleaned up by a cache sweep, or left harmlessly on disk).
//!
//! ## Atomic writes
//!
//! Cache writes use a temporary file with rename to prevent reads of
//! partially-written files. This makes the cache safe for concurrent
//! readers (though not for concurrent writers to the same key — which
//! is fine because each JAR is processed once).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use log::warn;
use sha2::{Digest, Sha256};

use crate::stub::model::ClassStub;
use crate::{ClasspathError, ClasspathResult};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Number of bytes from the SHA-256 hash used as the cache key.
/// 16 bytes = 32 hex chars, providing 128-bit collision resistance.
const HASH_PREFIX_BYTES: usize = 16;

/// Subdirectory under the project root for JAR stub cache files.
const CACHE_SUBDIR: &str = ".sqry/classpath/jars";

/// File extension for cached stub files.
const CACHE_EXTENSION: &str = "stub";

/// Temporary file suffix used during atomic writes.
const TEMP_SUFFIX: &str = ".tmp";

// ---------------------------------------------------------------------------
// StubCache
// ---------------------------------------------------------------------------

/// Per-JAR stub cache keyed by SHA-256 hash of the JAR file.
///
/// Cache location: `.sqry/classpath/jars/{hash}.stub`
/// Format: postcard binary serialization of `Vec<ClassStub>`
///
/// # Thread safety
///
/// `StubCache` is `Send + Sync` (all fields are owned, no interior mutability).
/// Multiple threads may call [`get`](StubCache::get) concurrently. Concurrent
/// [`put`](StubCache::put) calls for different JAR files are safe because they
/// write to different files. Concurrent puts for the same JAR file are
/// idempotent (last writer wins via atomic rename).
#[derive(Debug, Clone)]
pub struct StubCache {
    /// Directory where cache files are stored.
    cache_dir: PathBuf,
}

impl StubCache {
    /// Create a new stub cache rooted at the given project directory.
    ///
    /// The cache directory (`.sqry/classpath/jars/`) is created lazily on
    /// first write.
    #[must_use]
    pub fn new(project_root: &Path) -> Self {
        Self {
            cache_dir: project_root.join(CACHE_SUBDIR),
        }
    }

    /// Try to load cached stubs for a JAR file.
    ///
    /// Returns `None` on cache miss, corrupt cache, hash computation failure,
    /// or I/O error. Errors are logged as warnings but never propagated.
    #[must_use]
    pub fn get(&self, jar_path: &Path) -> Option<Vec<ClassStub>> {
        let key = match Self::cache_key(jar_path) {
            Ok(k) => k,
            Err(e) => {
                warn!(
                    "stub cache: cannot compute key for {}: {e}",
                    jar_path.display()
                );
                return None;
            }
        };

        let cache_path = self.cache_file_path(&key);
        let bytes = match fs::read(&cache_path) {
            Ok(b) => b,
            Err(_) => return None, // Cache miss — not worth logging.
        };

        match postcard::from_bytes::<Vec<ClassStub>>(&bytes) {
            Ok(stubs) => Some(stubs),
            Err(e) => {
                warn!(
                    "stub cache: corrupt cache file {}: {e}",
                    cache_path.display()
                );
                // Attempt to remove the corrupt file.
                let _ = fs::remove_file(&cache_path);
                None
            }
        }
    }

    /// Cache parsed stubs for a JAR file.
    ///
    /// Uses atomic write (temp file + rename) to prevent corrupt reads.
    ///
    /// # Errors
    ///
    /// Returns [`ClasspathError::CacheError`] if the cache key cannot be
    /// computed or the cache directory cannot be created. Serialization
    /// and I/O failures during write are returned as `CacheError`.
    pub fn put(&self, jar_path: &Path, stubs: &[ClassStub]) -> ClasspathResult<()> {
        let key = Self::cache_key(jar_path)?;
        let cache_path = self.cache_file_path(&key);

        // Ensure cache directory exists.
        fs::create_dir_all(&self.cache_dir).map_err(|e| {
            ClasspathError::CacheError(format!(
                "cannot create cache directory {}: {e}",
                self.cache_dir.display()
            ))
        })?;

        // Serialize.
        let bytes = postcard::to_allocvec(stubs).map_err(|e| {
            ClasspathError::CacheError(format!(
                "cannot serialize stubs for {}: {e}",
                jar_path.display()
            ))
        })?;

        // Atomic write: write to temp file, then rename.
        let temp_path = cache_path.with_extension(format!("{CACHE_EXTENSION}{TEMP_SUFFIX}"));

        if let Err(e) = fs::write(&temp_path, &bytes) {
            warn!(
                "stub cache: cannot write temp file {}: {e}",
                temp_path.display()
            );
            // Non-fatal: we can continue without caching.
            return Err(ClasspathError::CacheError(format!(
                "cannot write temp cache file: {e}"
            )));
        }

        if let Err(e) = fs::rename(&temp_path, &cache_path) {
            // Clean up the temp file if rename fails.
            let _ = fs::remove_file(&temp_path);
            warn!(
                "stub cache: cannot rename temp file to {}: {e}",
                cache_path.display()
            );
            return Err(ClasspathError::CacheError(format!(
                "cannot rename cache file: {e}"
            )));
        }

        Ok(())
    }

    /// Compute cache key: first 16 bytes of SHA-256 of the JAR file, hex-encoded.
    ///
    /// # Errors
    ///
    /// Returns [`ClasspathError::CacheError`] if the JAR file cannot be read.
    fn cache_key(jar_path: &Path) -> ClasspathResult<String> {
        let mut file = fs::File::open(jar_path).map_err(|e| {
            ClasspathError::CacheError(format!(
                "cannot open JAR for hashing {}: {e}",
                jar_path.display()
            ))
        })?;

        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192];
        loop {
            let n = file.read(&mut buffer).map_err(|e| {
                ClasspathError::CacheError(format!(
                    "cannot read JAR for hashing {}: {e}",
                    jar_path.display()
                ))
            })?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }

        let hash = hasher.finalize();
        let key = hex::encode(&hash[..HASH_PREFIX_BYTES]);
        Ok(key)
    }

    /// Compute the filesystem path for a given cache key.
    fn cache_file_path(&self, key: &str) -> PathBuf {
        self.cache_dir.join(format!("{key}.{CACHE_EXTENSION}"))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::model::{AccessFlags, ClassKind};
    use tempfile::TempDir;

    /// Create a minimal ClassStub for testing.
    fn make_stub(fqn: &str) -> ClassStub {
        ClassStub {
            fqn: fqn.to_owned(),
            name: fqn.rsplit('.').next().unwrap_or(fqn).to_owned(),
            kind: ClassKind::Class,
            access: AccessFlags::new(0x0021),
            superclass: Some("java.lang.Object".to_owned()),
            interfaces: vec![],
            methods: vec![],
            fields: vec![],
            annotations: vec![],
            generic_signature: None,
            inner_classes: vec![],
            lambda_targets: vec![],
            module: None,
            record_components: vec![],
            enum_constants: vec![],
            source_file: None,
            source_jar: None,
            kotlin_metadata: None,
            scala_signature: None,
        }
    }

    /// Create a dummy JAR file for hashing.
    fn create_dummy_jar(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_cache_miss_returns_none() {
        let tmp = TempDir::new().unwrap();
        let cache = StubCache::new(tmp.path());

        let jar_path = create_dummy_jar(tmp.path(), "test.jar", b"some jar content");
        assert!(cache.get(&jar_path).is_none());
    }

    #[test]
    fn test_cache_hit_returns_stubs() {
        let tmp = TempDir::new().unwrap();
        let cache = StubCache::new(tmp.path());

        let jar_path = create_dummy_jar(tmp.path(), "test.jar", b"some jar content");
        let stubs = vec![make_stub("com.example.Foo"), make_stub("com.example.Bar")];

        cache.put(&jar_path, &stubs).unwrap();
        let cached = cache.get(&jar_path).unwrap();

        assert_eq!(cached.len(), 2);
        assert_eq!(cached[0].fqn, "com.example.Foo");
        assert_eq!(cached[1].fqn, "com.example.Bar");
    }

    #[test]
    fn test_hash_change_triggers_miss() {
        let tmp = TempDir::new().unwrap();
        let cache = StubCache::new(tmp.path());

        let jar_path = tmp.path().join("test.jar");
        fs::write(&jar_path, b"version 1").unwrap();

        let stubs = vec![make_stub("com.example.Foo")];
        cache.put(&jar_path, &stubs).unwrap();

        // Overwrite the JAR with different content — hash changes.
        fs::write(&jar_path, b"version 2").unwrap();

        // Old cache entry is keyed to old hash, so this is a miss.
        assert!(cache.get(&jar_path).is_none());
    }

    #[test]
    fn test_corrupt_cache_returns_none() {
        let tmp = TempDir::new().unwrap();
        let cache = StubCache::new(tmp.path());

        let jar_path = create_dummy_jar(tmp.path(), "test.jar", b"some jar content");
        let key = StubCache::cache_key(&jar_path).unwrap();

        // Write garbage to the cache file.
        let cache_dir = tmp.path().join(CACHE_SUBDIR);
        fs::create_dir_all(&cache_dir).unwrap();
        let cache_file = cache_dir.join(format!("{key}.{CACHE_EXTENSION}"));
        fs::write(&cache_file, b"corrupt data").unwrap();

        // get should return None and remove the corrupt file.
        assert!(cache.get(&jar_path).is_none());
        assert!(!cache_file.exists(), "corrupt cache file should be removed");
    }

    #[test]
    fn test_postcard_roundtrip() {
        let stubs = vec![
            make_stub("com.example.Foo"),
            make_stub("com.example.Bar"),
            make_stub("com.example.Baz"),
        ];

        let bytes = postcard::to_allocvec(&stubs).unwrap();
        let deserialized: Vec<ClassStub> = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(deserialized.len(), 3);
        assert_eq!(deserialized[0].fqn, "com.example.Foo");
        assert_eq!(deserialized[1].fqn, "com.example.Bar");
        assert_eq!(deserialized[2].fqn, "com.example.Baz");
    }

    #[test]
    fn test_cache_empty_stubs() {
        let tmp = TempDir::new().unwrap();
        let cache = StubCache::new(tmp.path());

        let jar_path = create_dummy_jar(tmp.path(), "empty.jar", b"empty jar content");
        let stubs: Vec<ClassStub> = vec![];

        cache.put(&jar_path, &stubs).unwrap();
        let cached = cache.get(&jar_path).unwrap();
        assert!(cached.is_empty());
    }

    #[test]
    fn test_cache_key_is_deterministic() {
        let tmp = TempDir::new().unwrap();
        let jar_path = create_dummy_jar(tmp.path(), "test.jar", b"deterministic content");

        let key1 = StubCache::cache_key(&jar_path).unwrap();
        let key2 = StubCache::cache_key(&jar_path).unwrap();

        assert_eq!(key1, key2);
        // 16 bytes hex-encoded = 32 hex chars.
        assert_eq!(key1.len(), 32);
    }

    #[test]
    fn test_cache_nonexistent_jar() {
        let tmp = TempDir::new().unwrap();
        let cache = StubCache::new(tmp.path());

        let result = cache.put(Path::new("/nonexistent/foo.jar"), &[]);
        assert!(result.is_err());
    }
}
