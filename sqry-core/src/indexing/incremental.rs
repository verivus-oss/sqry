//! Incremental indexing with hash-based change detection.
//!
//! This module provides fast file change detection using `XXHash64` (~10GB/s hashing speed)
//! to enable incremental re-indexing. Only files that have changed since the last index
//! are re-parsed, achieving 10-100x speedup for re-indexing operations.
//!
//! # Architecture
//!
//! The incremental indexing system uses a 3-level change detection strategy:
//!
//! 1. **Existence check**: Is the file still there?
//! 2. **Metadata check**: Has size or mtime changed?
//! 3. **Hash check**: Has content actually changed? (guards against mtime-only changes)
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::indexing::{HashIndex, FileHash};
//! use std::path::Path;
//!
//! // Load existing hash index
//! let cache_dir = Path::new(".sqry-cache");
//! let mut hash_index = HashIndex::load(cache_dir)?;
//!
//! // Check if a file has changed
//! let file_path = Path::new("src/main.rs");
//! if hash_index.has_changed(file_path)? {
//!     println!("File changed, re-indexing...");
//!     // Re-index the file
//!     let new_hash = FileHash::compute(file_path)?;
//!     hash_index.update(file_path.to_path_buf(), new_hash);
//! }
//!
//! // Save updated hash index
//! hash_index.save(cache_dir)?;
//! ```
//!
//! # Performance
//!
//! - `XXHash64` hashing: ~10 GB/s (vs BLAKE3 ~1 GB/s)
//! - Metadata checks: ~1M files/sec
//! - Target: 10-100x faster re-indexing for typical codebases

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use xxhash_rust::xxh64::xxh64;

use crate::config::buffers::parse_buffer_size;

const HASH_INDEX_MAGIC: [u8; 7] = *b"SQRYHSH";
const HASH_INDEX_ENVELOPE_VERSION: u16 = 1;

#[derive(Serialize, Deserialize)]
struct HashIndexEnvelope {
    magic: [u8; 7],
    version: u16,
    sqry_version: String,
    payload: Vec<u8>,
}

/// Hash information for a single file.
///
/// This structure stores both the content hash (for change detection)
/// and metadata (for quick pre-checks before expensive hashing).
///
/// # Phase 3: Content Caching
///
/// The optional `content` field caches file content in memory to enable
/// content-based diffing without disk I/O. This is controlled by the
/// `HashIndex` content-cache limit (configurable per builder) and is not
/// persisted to disk (`#[serde(skip)]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHash {
    /// Absolute path to the file
    pub path: PathBuf,
    /// `XXHash64` hash of file contents
    pub hash: u64,
    /// File size in bytes
    pub size: u64,
    /// Last modification time
    pub mtime: SystemTime,
    /// Number of symbols extracted from this file (for stats)
    pub symbols_count: usize,
    /// Cached file content for content-based diffing (Phase 3/4).
    ///
    /// Populated according to the content-cache size configuration managed by
    /// `HashIndex`. Not persisted to disk (rebuilt on load if needed).
    #[serde(skip)]
    pub content: Option<String>,
}

impl FileHash {
    /// Compute hash for a file.
    ///
    /// This reads the file in chunks and computes an `XXHash64` hash.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or metadata cannot be accessed.
    pub fn compute(path: &Path) -> Result<Self> {
        use std::io::Read;

        // Get metadata first
        let metadata = fs::metadata(path)
            .with_context(|| format!("Failed to read metadata for {}", path.display()))?;

        let size = metadata.len();
        let mtime = metadata
            .modified()
            .with_context(|| format!("Failed to get modification time for {}", path.display()))?;

        // Read and hash file contents
        let mut file = fs::File::open(path)
            .with_context(|| format!("Failed to open file {}", path.display()))?;

        // Read in chunks for efficiency (respects SQRY_PARSE_BUFFER env var)
        let mut buffer = vec![0u8; parse_buffer_size()];
        let mut hasher = xxhash_rust::xxh64::Xxh64::new(0); // Use seed 0 for consistency

        loop {
            let bytes_read = file
                .read(&mut buffer)
                .with_context(|| format!("Failed to read file {}", path.display()))?;

            if bytes_read == 0 {
                break;
            }

            hasher.update(&buffer[..bytes_read]);
        }

        let hash = hasher.digest();

        Ok(Self {
            path: path.to_path_buf(),
            hash,
            size,
            mtime,
            symbols_count: 0, // Will be updated after indexing
            content: None,    // Phase 3: Not cached during compute (populated later)
        })
    }

    /// Quick compute using just the file bytes (for testing or small files).
    ///
    /// This is faster for small files but requires loading the entire file into memory.
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when the filesystem metadata for `path` cannot be read.
    pub fn from_bytes(path: &Path, content: &[u8]) -> Result<Self> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("Failed to read metadata for {}", path.display()))?;

        let hash = xxh64(content, 0); // Use seed 0 for consistency

        Ok(Self {
            path: path.to_path_buf(),
            hash,
            size: content.len() as u64,
            mtime: metadata.modified().with_context(|| {
                format!("Failed to get modification time for {}", path.display())
            })?,
            symbols_count: 0,
            content: None, // Phase 3: Not cached by default
        })
    }

    /// Check if file metadata (size or mtime) has changed.
    ///
    /// This is a fast pre-check before expensive hashing.
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when the file's metadata cannot be read.
    pub fn metadata_changed(&self, path: &Path) -> Result<bool> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("Failed to read metadata for {}", path.display()))?;

        let current_size = metadata.len();
        let current_mtime = metadata
            .modified()
            .with_context(|| format!("Failed to get modification time for {}", path.display()))?;

        Ok(current_size != self.size || current_mtime != self.mtime)
    }
}

/// Index of file hashes for incremental indexing.
///
/// This structure maintains a mapping from file paths to their hash information,
/// enabling fast change detection during re-indexing operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashIndex {
    /// Map from file path to hash information
    hashes: HashMap<PathBuf, FileHash>,
    /// Total number of files tracked
    pub file_count: usize,
    /// Total number of symbols across all files
    pub total_symbols: usize,
    /// Maximum number of bytes to store per cached file (None = unlimited)
    #[serde(default)]
    content_cache_max_bytes: Option<usize>,
}

impl HashIndex {
    /// Create a new empty hash index.
    #[must_use]
    pub fn new() -> Self {
        Self::with_content_cache_limit(None)
    }

    /// Create a new hash index with an explicit content cache limit.
    #[must_use]
    pub fn with_content_cache_limit(limit: Option<usize>) -> Self {
        Self {
            hashes: HashMap::new(),
            file_count: 0,
            total_symbols: 0,
            content_cache_max_bytes: limit,
        }
    }

    /// Override the content cache limit at runtime.
    pub fn set_content_cache_limit(&mut self, limit: Option<usize>) {
        self.content_cache_max_bytes = limit;
    }

    /// Check if a file has changed using 3-level detection.
    ///
    /// Returns `true` if the file should be re-indexed, `false` if it can be skipped.
    ///
    /// # Detection Levels
    ///
    /// 1. **Existence**: If file not in index or doesn't exist on disk → changed
    /// 2. **Metadata**: If size or mtime differs → check hash
    /// 3. **Hash**: If hash differs → changed
    ///
    /// # Errors
    ///
    /// Returns an error if file metadata cannot be read or file cannot be hashed.
    pub fn has_changed(&self, path: &Path) -> Result<bool> {
        // Level 1: Existence check
        let Some(stored_hash) = self.hashes.get(path) else {
            // File not in index → definitely changed (new file)
            return Ok(true);
        };

        // Check if file still exists
        if !path.exists() {
            // File was deleted → mark as changed so it can be removed from index
            return Ok(true);
        }

        // Level 2: Metadata check (fast pre-screen)
        if !stored_hash.metadata_changed(path)? {
            // Metadata unchanged → file definitely hasn't changed
            return Ok(false);
        }

        // Level 3: Hash check (metadata changed, need to verify content actually changed)
        // This guards against cases like:
        // - Touch command (mtime changed, content same)
        // - Reverted edits (size/mtime changed, content back to original)
        let current_hash = FileHash::compute(path)?;

        Ok(current_hash.hash != stored_hash.hash)
    }

    /// Update the hash index with new file information.
    ///
    /// This should be called after successfully indexing a file.
    pub fn update(&mut self, path: PathBuf, mut file_hash: FileHash) {
        // Remove old entry if it exists to update total_symbols
        if let Some(old_hash) = self.hashes.remove(&path) {
            self.total_symbols = self.total_symbols.saturating_sub(old_hash.symbols_count);
            self.file_count = self.file_count.saturating_sub(1);
        }

        // Add new entry
        self.total_symbols += file_hash.symbols_count;
        self.file_count += 1;

        // Ensure path is consistent
        file_hash.path.clone_from(&path);

        self.hashes.insert(path, file_hash);
    }

    /// Remove a file from the index.
    ///
    /// This should be called when a file is deleted from the codebase.
    pub fn remove(&mut self, path: &Path) -> Option<FileHash> {
        if let Some(removed) = self.hashes.remove(path) {
            self.total_symbols = self.total_symbols.saturating_sub(removed.symbols_count);
            self.file_count = self.file_count.saturating_sub(1);
            Some(removed)
        } else {
            None
        }
    }

    /// Get hash information for a file.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<&FileHash> {
        self.hashes.get(path)
    }

    /// Iterate over all tracked files.
    pub fn iter(&self) -> impl Iterator<Item = (&PathBuf, &FileHash)> {
        self.hashes.iter()
    }

    /// Get the number of tracked files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.file_count
    }

    /// Check if the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.file_count == 0
    }

    /// Clear all entries from the index.
    pub fn clear(&mut self) {
        self.hashes.clear();
        self.file_count = 0;
        self.total_symbols = 0;
    }

    /// Get cached content for a file (Phase 3).
    ///
    /// Returns the cached content if available in memory.
    /// This method is used by content-based diffing to obtain old file content
    /// for comparison with the current file state.
    ///
    /// # Errors
    ///
    /// Returns an error if the content is not cached (was never cached or file too large).
    /// This is intentional - we ONLY want cached content, not current disk content.
    pub fn get_cached_content(&self, path: &Path) -> Result<String> {
        // Try to get from cache
        if let Some(file_hash) = self.hashes.get(path)
            && let Some(ref content) = file_hash.content
        {
            return Ok(content.clone());
        }

        // Content not cached - this is an error, not a fallback case
        anyhow::bail!("Content not cached for {}", path.display())
    }

    /// Cache file content for a file (Phase 3).
    ///
    /// Stores file content in memory for fast content-based diffing. The
    /// maximum cached size is controlled by `content_cache_max_bytes`; when set
    /// to `None` the cache is unbounded.
    ///
    /// This is called after successfully parsing a file to enable fast
    /// incremental updates on the next change.
    ///
    /// # Size Limit
    ///
    /// Files larger than 100KB are not cached. This threshold can be tuned
    /// based on memory constraints and typical file sizes in the codebase.
    pub fn cache_content(&mut self, path: &Path, content: String) {
        if let Some(limit) = self.content_cache_max_bytes
            && content.len() > limit
        {
            log::trace!(
                "Skipping content cache for {} (size: {} bytes > {} limit)",
                path.display(),
                content.len(),
                limit
            );
            return;
        }

        if let Some(file_hash) = self.hashes.get_mut(path) {
            let size = content.len();
            file_hash.content = Some(content);
            log::trace!("Cached content for {} ({size} bytes)", path.display());
        }
    }

    /// Save the hash index to disk.
    ///
    /// The index is saved to `{cache_dir}/file_hashes.bin` using a versioned
    /// envelope with postcard serialization (atomic write).
    ///
    /// # Errors
    ///
    /// Returns an error if the cache directory cannot be created or the file cannot be written.
    pub fn save(&self, cache_dir: &Path) -> Result<()> {
        // Ensure cache directory exists
        fs::create_dir_all(cache_dir)
            .with_context(|| format!("Failed to create cache directory {}", cache_dir.display()))?;

        let hash_file = cache_dir.join("file_hashes.bin");

        // Serialize payload and envelope
        let payload =
            postcard::to_allocvec(self).context("Failed to serialize hash index payload")?;

        let envelope = HashIndexEnvelope {
            magic: HASH_INDEX_MAGIC,
            version: HASH_INDEX_ENVELOPE_VERSION,
            sqry_version: env!("CARGO_PKG_VERSION").to_string(),
            payload,
        };

        let bytes =
            postcard::to_allocvec(&envelope).context("Failed to serialize hash index envelope")?;

        // Atomic write: write to temp and then rename
        let tmp_hash_index_file_path = hash_file.with_extension("bin.tmp");
        fs::write(&tmp_hash_index_file_path, bytes).with_context(|| {
            format!(
                "Failed to write temp hash index to {}",
                tmp_hash_index_file_path.display()
            )
        })?;

        // Best-effort replace existing target
        if hash_file.exists() {
            let _ = fs::remove_file(&hash_file);
        }
        fs::rename(&tmp_hash_index_file_path, &hash_file).with_context(|| {
            format!(
                "Failed to atomically replace hash index at {} with temp {}",
                hash_file.display(),
                tmp_hash_index_file_path.display()
            )
        })?;

        log::debug!(
            "Saved hash index: {} files, {} symbols to {}",
            self.file_count,
            self.total_symbols,
            hash_file.display()
        );

        Ok(())
    }

    /// Load the hash index from disk.
    ///
    /// If the file doesn't exist or cannot be read, returns an empty index.
    ///
    /// # Errors
    ///
    /// Returns an error only if the file exists but cannot be deserialized
    /// (indicating corruption).
    pub fn load(cache_dir: &Path) -> Result<Self> {
        let hash_file = cache_dir.join("file_hashes.bin");

        // If file doesn't exist, return empty index
        if !hash_file.exists() {
            log::debug!(
                "No hash index found at {}, starting fresh",
                hash_file.display()
            );
            return Ok(Self::new());
        }

        // Read file with size cap to prevent memory exhaustion from crafted files
        const MAX_HASH_INDEX_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB
        let metadata = fs::metadata(&hash_file)
            .with_context(|| format!("Failed to stat hash index: {}", hash_file.display()))?;
        if metadata.len() > MAX_HASH_INDEX_BYTES {
            anyhow::bail!(
                "Hash index file is too large ({} bytes, max {}): {}",
                metadata.len(),
                MAX_HASH_INDEX_BYTES,
                hash_file.display()
            );
        }
        let bytes = fs::read(&hash_file)
            .with_context(|| format!("Failed to read hash index from {}", hash_file.display()))?;

        // Deserialize versioned envelope only (no legacy fallback)
        let env: HashIndexEnvelope =
            postcard::from_bytes(&bytes).context("Failed to deserialize hash index envelope")?;

        if env.magic != HASH_INDEX_MAGIC {
            anyhow::bail!("Invalid hash index magic: expected {HASH_INDEX_MAGIC:?}");
        }
        if env.version != HASH_INDEX_ENVELOPE_VERSION {
            anyhow::bail!(
                "Unsupported hash index version: {} (expected {})",
                env.version,
                HASH_INDEX_ENVELOPE_VERSION
            );
        }

        let index: Self = postcard::from_bytes(&env.payload)
            .context("Failed to deserialize hash index payload")?;

        log::debug!(
            "Loaded hash index: {} files, {} symbols from {}",
            index.file_count,
            index.total_symbols,
            hash_file.display()
        );
        Ok(index)
    }
}

impl Default for HashIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    fn test_file_hash_compute() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"test content").unwrap();
        temp_file.flush().unwrap();

        let hash = FileHash::compute(temp_file.path()).unwrap();

        assert_eq!(hash.size, 12); // "test content" is 12 bytes
        assert!(hash.hash != 0); // Should have computed a hash
        assert_eq!(hash.symbols_count, 0); // Default is 0
    }

    #[test]
    fn test_file_hash_from_bytes() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"test").unwrap();
        temp_file.flush().unwrap();

        let content = b"test";
        let hash = FileHash::from_bytes(temp_file.path(), content).unwrap();

        assert_eq!(hash.size, 4);
        assert_eq!(hash.hash, xxh64(content, 0));
    }

    #[test]
    fn test_file_hash_deterministic() {
        let mut temp_file = NamedTempFile::new().unwrap();
        let content = b"deterministic test content";
        temp_file.write_all(content).unwrap();
        temp_file.flush().unwrap();

        let hash1 = FileHash::compute(temp_file.path()).unwrap();
        let hash2 = FileHash::compute(temp_file.path()).unwrap();

        assert_eq!(hash1.hash, hash2.hash);
        assert_eq!(hash1.size, hash2.size);
    }

    #[test]
    fn test_file_hash_different_content() {
        let mut temp1 = NamedTempFile::new().unwrap();
        temp1.write_all(b"content A").unwrap();
        temp1.flush().unwrap();

        let mut temp2 = NamedTempFile::new().unwrap();
        temp2.write_all(b"content B").unwrap();
        temp2.flush().unwrap();

        let hash1 = FileHash::compute(temp1.path()).unwrap();
        let hash2 = FileHash::compute(temp2.path()).unwrap();

        assert_ne!(hash1.hash, hash2.hash);
    }

    #[test]
    fn test_hash_index_new_file() {
        let index = HashIndex::new();
        let path = Path::new("nonexistent.rs");

        // New file should be marked as changed
        assert!(index.has_changed(path).unwrap());
    }

    #[test]
    fn test_hash_index_unchanged_file() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"unchanged content").unwrap();
        temp_file.flush().unwrap();

        let mut index = HashIndex::new();
        let hash = FileHash::compute(temp_file.path()).unwrap();
        index.update(temp_file.path().to_path_buf(), hash);

        // File should not be marked as changed
        assert!(!index.has_changed(temp_file.path()).unwrap());
    }

    #[test]
    fn test_hash_index_changed_content() {
        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"original content").unwrap();
        temp_file.flush().unwrap();

        let mut index = HashIndex::new();
        let hash = FileHash::compute(temp_file.path()).unwrap();
        index.update(temp_file.path().to_path_buf(), hash);

        // Modify file
        temp_file.write_all(b" modified").unwrap();
        temp_file.flush().unwrap();

        // File should be marked as changed
        assert!(index.has_changed(temp_file.path()).unwrap());
    }

    #[test]
    fn test_hash_index_update_and_remove() {
        let mut index = HashIndex::new();
        let path = PathBuf::from("test.rs");

        let mut hash = FileHash {
            path: path.clone(),
            hash: 12345,
            size: 100,
            mtime: SystemTime::now(),
            symbols_count: 5,
            content: None,
        };

        // Update with new file
        index.update(path.clone(), hash.clone());
        assert_eq!(index.len(), 1);
        assert_eq!(index.total_symbols, 5);

        // Update existing file with more symbols
        hash.symbols_count = 10;
        index.update(path.clone(), hash.clone());
        assert_eq!(index.len(), 1); // Still 1 file
        assert_eq!(index.total_symbols, 10); // Updated symbols

        // Remove file
        let removed = index.remove(&path);
        assert!(removed.is_some());
        assert_eq!(index.len(), 0);
        assert_eq!(index.total_symbols, 0);
    }

    #[test]
    fn test_hash_index_save_and_load() {
        let tmp_index_dir = TempDir::new().unwrap();
        let cache_dir = tmp_index_dir.path();

        // Create index with some data
        let mut index = HashIndex::new();
        let path = PathBuf::from("test.rs");
        let hash = FileHash {
            path: path.clone(),
            hash: 67890,
            size: 200,
            mtime: SystemTime::now(),
            symbols_count: 15,
            content: None,
        };
        index.update(path, hash);

        // Save
        index.save(cache_dir).unwrap();

        // Load
        let loaded = HashIndex::load(cache_dir).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.total_symbols, 15);
        assert_eq!(loaded.get(Path::new("test.rs")).unwrap().hash, 67890);
    }

    #[test]
    fn test_hash_index_mtime_change_no_content_change() {
        use filetime::{FileTime, set_file_mtime};
        use std::time::Duration;

        let mut temp_file = NamedTempFile::new().unwrap();
        temp_file.write_all(b"same content").unwrap();
        temp_file.flush().unwrap();

        let mut index = HashIndex::new();
        let hash = FileHash::compute(temp_file.path()).unwrap();
        index.update(temp_file.path().to_path_buf(), hash);

        // Change only the mtime (simulate touch) without modifying content
        let meta = fs::metadata(temp_file.path()).unwrap();
        let orig_mtime = meta.modified().unwrap();
        let new_mtime = FileTime::from_system_time(orig_mtime + Duration::from_secs(60));
        set_file_mtime(temp_file.path(), new_mtime).unwrap();

        // Should detect metadata change, compute hash, and conclude unchanged
        assert!(!index.has_changed(temp_file.path()).unwrap());
    }

    #[test]
    fn test_hash_index_load_nonexistent() {
        let tmp_index_dir = TempDir::new().unwrap();
        let cache_dir = tmp_index_dir.path().join("nonexistent");

        // Loading from nonexistent directory should return empty index
        let index = HashIndex::load(&cache_dir).unwrap();

        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_hash_index_clear() {
        let mut index = HashIndex::new();

        // Add some entries
        for i in 0_u64..5 {
            let path = PathBuf::from(format!("file{i}.rs"));
            let hash = FileHash {
                path: path.clone(),
                hash: i,
                size: 100,
                mtime: SystemTime::now(),
                symbols_count: 3,
                content: None,
            };
            index.update(path, hash);
        }

        assert_eq!(index.len(), 5);
        assert_eq!(index.total_symbols, 15);

        // Clear
        index.clear();

        assert_eq!(index.len(), 0);
        assert_eq!(index.total_symbols, 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_xxhash64_performance_characteristic() {
        // Test that XXHash64 is indeed very fast
        // Generate 1MB of test data
        let data = vec![0u8; 1_000_000];

        let start = std::time::Instant::now();
        let _hash = xxh64(&data, 0);
        let elapsed = start.elapsed();

        // XXHash64 should hash 1MB in well under 100ms on any modern CPU
        // (typically <1ms locally, but CI shared runners can be much slower)
        assert!(
            elapsed.as_millis() < 100,
            "XXHash64 took {elapsed:?} to hash 1MB (expected <100ms)"
        );
    }

    #[test]
    fn test_cache_small_file() {
        // Ensure small files are cached when under the configured limit (default: unlimited)
        let mut temp_file = NamedTempFile::new().unwrap();
        let content = "Small file content for caching test";
        temp_file.write_all(content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let mut index = HashIndex::new();
        let hash = FileHash::compute(temp_file.path()).unwrap();
        index.update(temp_file.path().to_path_buf(), hash);

        // Cache the content
        index.cache_content(temp_file.path(), content.to_string());

        // Verify it was cached
        let cached = index.get_cached_content(temp_file.path()).unwrap();
        assert_eq!(cached, content);

        // Verify it's stored in the FileHash struct
        let file_hash = index.get(temp_file.path()).unwrap();
        assert!(file_hash.content.is_some());
        assert_eq!(file_hash.content.as_ref().unwrap(), content);
    }

    #[test]
    fn test_skip_large_file_when_limit_configured() {
        // Verify that the optional limit is honoured when configured
        let mut temp_file = NamedTempFile::new().unwrap();
        // Create content larger than 100KB
        let large_content = "x".repeat(101_000); // 101KB
        temp_file.write_all(large_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let mut index = HashIndex::with_content_cache_limit(Some(100_000));
        let hash = FileHash::compute(temp_file.path()).unwrap();
        index.update(temp_file.path().to_path_buf(), hash);

        // Attempt to cache large content
        index.cache_content(temp_file.path(), large_content.clone());

        // Verify it was NOT cached
        let file_hash = index.get(temp_file.path()).unwrap();
        assert!(file_hash.content.is_none());

        // get_cached_content should return error (content not cached)
        assert!(index.get_cached_content(temp_file.path()).is_err());
    }

    #[test]
    fn test_large_file_cached_without_limit() {
        // By default the cache is unbounded; large files should therefore be cached
        let mut temp_file = NamedTempFile::new().unwrap();
        let large_content = "x".repeat(101_000); // 101KB
        temp_file.write_all(large_content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let mut index = HashIndex::new();
        let hash = FileHash::compute(temp_file.path()).unwrap();
        index.update(temp_file.path().to_path_buf(), hash);

        index.cache_content(temp_file.path(), large_content.clone());

        let cached = index.get_cached_content(temp_file.path()).unwrap();
        assert_eq!(cached.len(), large_content.len());
    }

    #[test]
    fn test_get_cached_content_error_when_not_cached() {
        // Phase 3: Test that get_cached_content returns error if not cached
        let mut temp_file = NamedTempFile::new().unwrap();
        let content = "Test content";
        temp_file.write_all(content.as_bytes()).unwrap();
        temp_file.flush().unwrap();

        let mut index = HashIndex::new();
        let hash = FileHash::compute(temp_file.path()).unwrap();
        index.update(temp_file.path().to_path_buf(), hash);

        // Don't cache the content

        // get_cached_content should return error (content not cached)
        assert!(index.get_cached_content(temp_file.path()).is_err());
    }
}
