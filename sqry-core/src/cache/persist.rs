//! Disk persistence for cache entries.
//!
//! This module provides atomic disk persistence with crash safety and multi-process
//! coordination. Cache entries are stored in `.sqry-cache/` with the following layout:
//!
//! ```text
//! .sqry-cache/
//! ├── <user_namespace_id>/               # Per-user namespace (hash of $USER)
//! │   ├── rust/                          # Language-specific directory
//! │   │   ├── <content_hash>/            # BLAKE3 of file content (64 hex chars)
//! │   │   │   ├── <path_hash>/           # BLAKE3 of canonical path (16 hex chars)
//! │   │   │   │   ├── filename.rs.bin       # Cached symbols
//! │   │   │   │   └── filename.rs.bin.lock  # Write lock
//! │   ├── python/
//! │   ├── typescript/
//! │   └── manifest.json                   # Size tracking (Phase 3)
//! ```
//!
//! # Features
//!
//! - **Atomic writes**: Temp file + `fs::rename` for crash safety
//! - **Multi-process safe**: Per-entry lock files prevent corruption
//! - **Stale lock cleanup**: Removes locks from dead processes
//! - **Size tracking**: Manifest tracks total bytes per language
//!
//! # Examples
//!
//! ```rust,ignore
//! use sqry_core::cache::persist::PersistManager;
//!
//! let manager = PersistManager::new("/path/to/.sqry-cache")?;
//!
//! // Write entry
//! manager.write_entry(&key, &summaries)?;
//!
//! // Read entry
//! if let Some(summaries) = manager.read_entry(&key)? {
//!     // Use cached data
//! }
//! ```

use crate::cache::{CacheKey, GraphNodeSummary};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Cache manifest for tracking size and metadata.
///
/// **Status**: Defined but not yet used. Full implementation planned for Phase 3
/// (manifest management, disk size tracking, and enforcement).
///
/// When implemented, the manifest will be stored as `manifest.json` in the cache
/// root directory and used to:
/// - Track total disk usage per language
/// - Enforce disk size limits with LRU eviction
/// - Provide cache statistics for CLI tooling
///
/// See [`docs/development/cache/PHASE_2_COMPLETE.md`](../../../docs/development/cache/PHASE_2_COMPLETE.md)
/// for Phase 3 roadmap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheManifest {
    /// Total bytes used per language
    pub bytes_by_language: HashMap<String, u64>,

    /// sqry version that wrote this manifest
    pub sqry_version: String,

    /// Last updated timestamp
    pub updated_at: SystemTime,
}

impl Default for CacheManifest {
    fn default() -> Self {
        Self {
            bytes_by_language: HashMap::new(),
            sqry_version: env!("CARGO_PKG_VERSION").to_string(),
            updated_at: SystemTime::now(),
        }
    }
}

/// Persistence manager for cache entries.
///
/// Handles atomic writes, lock management, and manifest tracking.
pub struct PersistManager {
    /// Root cache directory (e.g., `.sqry-cache/`)
    cache_root: PathBuf,

    /// User-specific namespace hash (subdirectory under `cache_root`)
    user_namespace_id: String,
}

impl PersistManager {
    /// Create a new persistence manager.
    ///
    /// # Arguments
    ///
    /// - `cache_root`: Root directory for cache storage
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// let manager = PersistManager::new(".sqry-cache")?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when the cache directories cannot be created or when stale
    /// lock cleanup fails.
    pub fn new<P: AsRef<Path>>(cache_root: P) -> Result<Self> {
        let cache_root = cache_root.as_ref().to_path_buf();

        // Create cache directory if it doesn't exist
        fs::create_dir_all(&cache_root).with_context(|| {
            format!("Failed to create cache directory: {}", cache_root.display())
        })?;

        // Generate user-specific namespace
        let user_namespace_id = Self::compute_user_hash();

        let manager = Self {
            cache_root,
            user_namespace_id,
        };

        // Clean up stale locks on initialization
        manager.cleanup_stale_locks()?;

        Ok(manager)
    }

    /// Compute a hash of the current user for namespacing.
    ///
    /// Uses the current username to create a subdirectory under cache root.
    /// This prevents collisions when multiple users share the same filesystem.
    fn compute_user_hash() -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let username = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "default".to_string());

        let mut hasher = DefaultHasher::new();
        username.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    /// Get the user-specific cache directory.
    ///
    /// Returns the path where this user's cache entries are stored.
    #[must_use]
    pub fn user_cache_dir(&self) -> PathBuf {
        self.cache_root.join(&self.user_namespace_id)
    }

    /// Get the file path for a cache entry.
    ///
    /// Format: `<user_namespace_id>/<language>/<hash>/<filename>.bin`
    fn entry_path(&self, key: &CacheKey) -> PathBuf {
        let storage_key = key.storage_key();
        self.user_cache_dir().join(format!("{storage_key}.bin"))
    }

    /// Get the lock file path for a cache entry.
    fn lock_path(&self, key: &CacheKey) -> PathBuf {
        let mut path = self.entry_path(key);
        path.set_extension("bin.lock");
        path
    }

    /// Write a cache entry to disk atomically.
    ///
    /// Uses temp file + rename pattern for crash safety.
    ///
    /// # Arguments
    ///
    /// - `key`: Cache key identifying the entry
    /// - `summaries`: Node summaries to persist
    ///
    /// # Returns
    ///
    /// `Ok(bytes_written)` on success
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when locking, serialization, writing, or atomic rename steps fail.
    pub fn write_entry(&self, key: &CacheKey, summaries: &[GraphNodeSummary]) -> Result<usize> {
        let entry_path = self.entry_path(key);
        let lock_path = self.lock_path(key);

        // Create parent directories
        if let Some(parent) = entry_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Acquire lock
        let _lock_guard = Self::acquire_lock(&lock_path)?;

        // Serialize to postcard
        let data = postcard::to_allocvec(summaries).context("Failed to serialize cache entry")?;

        // Write to temporary file
        let tmp_cache_file_path = entry_path.with_extension("tmp");
        {
            let mut temp_file = fs::File::create(&tmp_cache_file_path).with_context(|| {
                format!(
                    "Failed to create temp file: {}",
                    tmp_cache_file_path.display()
                )
            })?;

            temp_file.write_all(&data)?;
            temp_file.sync_all()?; // Ensure data is flushed
        } // Close file handle before rename (required on Windows)

        // On Windows, remove destination file first if it exists
        // (Windows doesn't allow renaming over existing files with open handles)
        #[cfg(windows)]
        if entry_path.exists() {
            fs::remove_file(&entry_path).with_context(|| {
                format!("Failed to remove existing file: {}", entry_path.display())
            })?;
        }

        // Atomic rename
        match fs::rename(&tmp_cache_file_path, &entry_path) {
            Ok(()) => {
                log::debug!(
                    "Wrote cache entry: {} ({} bytes)",
                    entry_path.display(),
                    data.len()
                );
                Ok(data.len())
            }
            Err(e) => {
                // Clean up temp file on failure
                let _ = fs::remove_file(&tmp_cache_file_path);
                Err(e).with_context(|| {
                    format!(
                        "Failed to rename {} to {}",
                        tmp_cache_file_path.display(),
                        entry_path.display()
                    )
                })
            }
        }
    }

    /// Read a cache entry from disk.
    ///
    /// # Returns
    ///
    /// - `Ok(Some(summaries))` if entry exists and is valid
    /// - `Ok(None)` if entry doesn't exist
    /// - `Err(_)` on I/O or deserialization errors
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when the entry cannot be read or deserialized.
    pub fn read_entry(&self, key: &CacheKey) -> Result<Option<Vec<GraphNodeSummary>>> {
        let entry_path = self.entry_path(key);

        if !entry_path.exists() {
            return Ok(None);
        }

        // Size cap to prevent memory exhaustion from crafted cache entries
        const MAX_CACHE_ENTRY_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB
        let metadata = fs::metadata(&entry_path)
            .with_context(|| format!("Failed to stat cache entry: {}", entry_path.display()))?;
        if metadata.len() > MAX_CACHE_ENTRY_BYTES {
            anyhow::bail!(
                "Cache entry is too large ({} bytes, max {}): {}",
                metadata.len(),
                MAX_CACHE_ENTRY_BYTES,
                entry_path.display()
            );
        }
        let data = fs::read(&entry_path)
            .with_context(|| format!("Failed to read cache entry: {}", entry_path.display()))?;

        let summaries: Vec<GraphNodeSummary> = postcard::from_bytes(&data).with_context(|| {
            format!(
                "Failed to deserialize cache entry: {}",
                entry_path.display()
            )
        })?;

        log::debug!(
            "Read cache entry: {} ({} symbols)",
            entry_path.display(),
            summaries.len()
        );

        Ok(Some(summaries))
    }

    /// Delete a cache entry from disk.
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when either the entry or its lock file cannot be removed.
    pub fn delete_entry(&self, key: &CacheKey) -> Result<()> {
        let entry_path = self.entry_path(key);
        let lock_path = self.lock_path(key);

        // Remove entry file
        if entry_path.exists() {
            fs::remove_file(&entry_path).with_context(|| {
                format!("Failed to delete cache entry: {}", entry_path.display())
            })?;
        }

        // Remove lock file if it exists
        if lock_path.exists() {
            let _ = fs::remove_file(&lock_path); // Best effort
        }

        Ok(())
    }

    /// Acquire a write lock for an entry.
    ///
    /// Creates a lock file to prevent concurrent writes from other processes.
    /// Returns a guard that automatically removes the lock on drop.
    fn acquire_lock(lock_path: &Path) -> Result<LockGuard> {
        // Try to create lock file (exclusive creation)
        // If it already exists, wait and retry
        let max_retries = 50;
        let retry_delay = Duration::from_millis(100);

        for attempt in 0..max_retries {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(lock_path)
            {
                Ok(mut file) => {
                    // Write our PID to the lock file
                    let pid = std::process::id();
                    writeln!(file, "{pid}")?;
                    file.sync_all()?;

                    return Ok(LockGuard {
                        path: lock_path.to_path_buf(),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Lock exists, check if it's stale
                    if Self::is_lock_stale(lock_path)? {
                        // Remove stale lock and retry
                        let _ = fs::remove_file(lock_path);
                        continue;
                    }

                    // Lock is active, wait and retry
                    if attempt < max_retries - 1 {
                        std::thread::sleep(retry_delay);
                    } else {
                        anyhow::bail!(
                            "Failed to acquire lock after {} attempts: {}",
                            max_retries,
                            lock_path.display()
                        );
                    }
                }
                Err(e) => {
                    return Err(e).context("Failed to create lock file");
                }
            }
        }

        anyhow::bail!("Failed to acquire lock: {}", lock_path.display())
    }

    /// Check if a lock file is stale (process no longer exists).
    ///
    /// First checks if the process exists (immediate recovery from crashes).
    /// If process exists, verifies lock age as a safety check against hung processes.
    fn is_lock_stale(lock_path: &Path) -> Result<bool> {
        // Try to read PID from lock file
        let content = fs::read_to_string(lock_path)?;
        let pid = content
            .trim()
            .parse::<u32>()
            .context("Failed to parse PID from lock file")?;

        // Check if process exists (immediate crash recovery)
        if !Self::process_exists(pid) {
            log::debug!("Process {pid} no longer exists, lock is stale");
            return Ok(true);
        }

        // Process exists - check age as secondary safety check
        let metadata = fs::metadata(lock_path)?;
        let modified = metadata.modified()?;
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or(Duration::ZERO);

        // If lock is older than 5 minutes, force cleanup (hung process)
        if age > Duration::from_secs(300) {
            log::warn!(
                "Lock held by PID {} for {:?} - forcing cleanup: {}",
                pid,
                age,
                lock_path.display()
            );
            return Ok(true);
        }

        Ok(false)
    }

    /// Check if a process with the given PID exists.
    ///
    /// Uses platform-specific methods:
    /// - Linux: Check /proc/<pid> directory
    /// - macOS/BSD: Use kill(pid, 0) signal probe via nix crate
    /// - Windows: Always returns true (no portable check available)
    #[cfg(unix)]
    fn process_exists(pid: u32) -> bool {
        #[cfg(target_os = "linux")]
        {
            // Linux: Check /proc/<pid> directory
            let proc_path = format!("/proc/{pid}");
            std::path::Path::new(&proc_path).exists()
        }

        #[cfg(not(target_os = "linux"))]
        {
            // macOS/BSD: Use kill(pid, 0) signal probe
            // Sending signal 0 checks if the process exists without actually sending a signal
            use nix::sys::signal::kill;
            use nix::unistd::Pid;

            match kill(Pid::from_raw(pid as i32), None) {
                Ok(_) => true,   // Process exists
                Err(_) => false, // Process doesn't exist or no permission
            }
        }
    }

    #[cfg(not(unix))]
    fn process_exists(_pid: u32) -> bool {
        // On non-Unix, we can't reliably check process existence
        // Conservative approach: assume it exists
        true
    }

    /// Clean up stale lock files on initialization.
    fn cleanup_stale_locks(&self) -> Result<()> {
        let user_dir = self.user_cache_dir();

        if !user_dir.exists() {
            return Ok(()); // No cache directory yet
        }

        // Find all .lock files
        let walker = walkdir::WalkDir::new(&user_dir)
            .max_depth(10)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext == "lock")
            });

        let mut cleaned = 0;
        for entry in walker {
            let path = entry.path();

            if Self::is_lock_stale(path)? {
                if let Err(e) = fs::remove_file(path) {
                    log::warn!("Failed to remove stale lock {}: {}", path.display(), e);
                } else {
                    log::debug!("Removed stale lock: {}", path.display());
                    cleaned += 1;
                }
            }
        }

        if cleaned > 0 {
            log::info!("Cleaned up {cleaned} stale lock files");
        }

        Ok(())
    }

    /// Clear all cache entries for this user.
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when directory traversal or entry deletion fails.
    pub fn clear_all(&self) -> Result<()> {
        let user_dir = self.user_cache_dir();

        if user_dir.exists() {
            fs::remove_dir_all(&user_dir).with_context(|| {
                format!("Failed to remove cache directory: {}", user_dir.display())
            })?;

            log::info!("Cleared all cache entries in {}", user_dir.display());
        }

        Ok(())
    }
}

/// RAII guard for lock files.
///
/// Automatically removes the lock file when dropped.
struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Best effort removal - log error but don't panic
        if let Err(e) = fs::remove_file(&self.path) {
            log::warn!("Failed to remove lock file {}: {}", self.path.display(), e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheKey;
    use crate::graph::unified::node::NodeKind;
    use crate::hash::Blake3Hash;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn make_test_key() -> CacheKey {
        let hash = Blake3Hash::from_bytes([42; 32]);
        CacheKey::from_raw_path(PathBuf::from("/test/file.rs"), "rust", hash)
    }

    fn make_test_summary() -> GraphNodeSummary {
        GraphNodeSummary::new(
            Arc::from("test_fn"),
            NodeKind::Function,
            Arc::from(Path::new("test.rs")),
            1,
            0,
            1,
            10,
        )
    }

    #[test]
    fn test_persist_manager_new() {
        let tmp_cache_dir = TempDir::new().unwrap();
        let manager = PersistManager::new(tmp_cache_dir.path()).unwrap();

        // Cache root should exist
        assert!(tmp_cache_dir.path().exists());
        // User directory path should be valid (may not exist until first write)
        assert!(!manager.user_namespace_id.is_empty());
    }

    #[test]
    fn test_write_and_read_entry() {
        let tmp_cache_dir = TempDir::new().unwrap();
        let manager = PersistManager::new(tmp_cache_dir.path()).unwrap();

        let key = make_test_key();
        let summaries = vec![make_test_summary()];

        // Write entry
        let bytes_written = manager.write_entry(&key, &summaries).unwrap();
        assert!(bytes_written > 0);

        // Read entry back
        let read_summaries = manager.read_entry(&key).unwrap().unwrap();
        assert_eq!(read_summaries.len(), 1);
        assert_eq!(read_summaries[0].name, summaries[0].name);
    }

    #[test]
    fn test_read_nonexistent_entry() {
        let tmp_cache_dir = TempDir::new().unwrap();
        let manager = PersistManager::new(tmp_cache_dir.path()).unwrap();

        let key = make_test_key();
        let result = manager.read_entry(&key).unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn test_delete_entry() {
        let tmp_cache_dir = TempDir::new().unwrap();
        let manager = PersistManager::new(tmp_cache_dir.path()).unwrap();

        let key = make_test_key();
        let summaries = vec![make_test_summary()];

        // Write entry
        manager.write_entry(&key, &summaries).unwrap();
        assert!(manager.read_entry(&key).unwrap().is_some());

        // Delete entry
        manager.delete_entry(&key).unwrap();
        assert!(manager.read_entry(&key).unwrap().is_none());
    }

    #[test]
    fn test_clear_all() {
        let tmp_cache_dir = TempDir::new().unwrap();
        let manager = PersistManager::new(tmp_cache_dir.path()).unwrap();

        let key = make_test_key();
        let summaries = vec![make_test_summary()];

        // Write entry
        manager.write_entry(&key, &summaries).unwrap();
        assert!(manager.read_entry(&key).unwrap().is_some());

        // Clear all
        manager.clear_all().unwrap();
        assert!(manager.read_entry(&key).unwrap().is_none());
    }
}
