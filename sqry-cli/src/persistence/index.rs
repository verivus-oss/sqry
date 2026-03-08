//! User metadata index storage.
//!
//! Provides atomic read/write operations for user metadata (aliases and history)
//! stored in postcard format. Supports both global and local storage scopes.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::persistence::config::PersistenceConfig;
use crate::persistence::types::{StorageScope, USER_METADATA_VERSION, UserMetadata};

/// Global index file name.
pub const GLOBAL_INDEX_FILE: &str = "global.index.user";

/// Local index file name.
pub const LOCAL_INDEX_FILE: &str = ".sqry-index.user";

/// User metadata index providing atomic access to alias and history storage.
///
/// This struct manages two storage locations:
/// - Global: `~/.config/sqry/global.index.user` for cross-project aliases
/// - Local: `.sqry-index.user` in project root for project-specific aliases
///
/// All operations are atomic (using temp file + rename) and thread-safe.
#[derive(Debug)]
pub struct UserMetadataIndex {
    /// Configuration for paths and settings.
    config: PersistenceConfig,

    /// Project root for local storage (None for global-only mode).
    project_root: Option<PathBuf>,

    /// Cached global metadata.
    global_cache: RwLock<Option<UserMetadata>>,

    /// Cached local metadata.
    local_cache: RwLock<Option<UserMetadata>>,
}

impl UserMetadataIndex {
    /// Open or create a user metadata index.
    ///
    /// # Arguments
    ///
    /// * `project_root` - Project root for local storage. Pass `None` for global-only mode.
    /// * `config` - Persistence configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the global config directory cannot be created.
    pub fn open(project_root: Option<&Path>, config: PersistenceConfig) -> anyhow::Result<Self> {
        // Ensure global directory exists
        let global_dir = config.global_config_dir()?;
        if !global_dir.exists() {
            fs::create_dir_all(&global_dir)?;
        }

        Ok(Self {
            config,
            project_root: project_root.map(Path::to_path_buf),
            global_cache: RwLock::new(None),
            local_cache: RwLock::new(None),
        })
    }

    /// Get the file path for a storage scope.
    ///
    /// # Errors
    ///
    /// Returns an error if the global config directory cannot be determined.
    pub fn path_for_scope(&self, scope: StorageScope) -> anyhow::Result<PathBuf> {
        match scope {
            StorageScope::Global => {
                let dir = self.config.global_config_dir()?;
                Ok(dir.join(GLOBAL_INDEX_FILE))
            }
            StorageScope::Local => {
                let project_root = self
                    .project_root
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("No project root set for local storage"))?;
                let dir = self.config.local_config_dir(project_root);
                Ok(dir.join(LOCAL_INDEX_FILE))
            }
        }
    }

    /// Load metadata from a storage scope.
    ///
    /// Returns default metadata if the file doesn't exist yet.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read or parsed.
    pub fn load(&self, scope: StorageScope) -> anyhow::Result<UserMetadata> {
        // Check cache first
        let cache = match scope {
            StorageScope::Global => &self.global_cache,
            StorageScope::Local => &self.local_cache,
        };

        if let Some(cached) = cache.read().as_ref() {
            return Ok(cached.clone());
        }

        // Load from disk
        let path = self.path_for_scope(scope)?;
        let metadata = Self::load_from_path(&path)?;

        // Update cache
        *cache.write() = Some(metadata.clone());

        Ok(metadata)
    }

    /// Save metadata to a storage scope.
    ///
    /// Uses atomic write (temp file + rename) to prevent corruption.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save(&self, scope: StorageScope, metadata: &UserMetadata) -> anyhow::Result<()> {
        let path = self.path_for_scope(scope)?;
        Self::save_to_path(&path, metadata)?;

        // Update cache
        let cache = match scope {
            StorageScope::Global => &self.global_cache,
            StorageScope::Local => &self.local_cache,
        };
        *cache.write() = Some(metadata.clone());

        Ok(())
    }

    /// Atomically update metadata using a closure.
    ///
    /// This is the preferred method for modifications as it handles the
    /// read-modify-write cycle atomically.
    ///
    /// # Errors
    ///
    /// Returns an error if loading or saving fails, or if the closure returns an error.
    pub fn update<F>(&self, scope: StorageScope, f: F) -> anyhow::Result<()>
    where
        F: FnOnce(&mut UserMetadata) -> anyhow::Result<()>,
    {
        let cache = match scope {
            StorageScope::Global => &self.global_cache,
            StorageScope::Local => &self.local_cache,
        };

        // Lock the cache for the entire operation
        let mut cache_guard = cache.write();

        // Load current state
        let path = self.path_for_scope(scope)?;
        let mut metadata = Self::load_from_path(&path)?;

        // Apply the modification
        f(&mut metadata)?;

        // Save atomically
        Self::save_to_path(&path, &metadata)?;

        // Update cache
        *cache_guard = Some(metadata);

        Ok(())
    }

    /// Get the size of the index file for a scope in bytes.
    ///
    /// Returns 0 if the file doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be determined.
    pub fn index_size(&self, scope: StorageScope) -> anyhow::Result<u64> {
        let path = self.path_for_scope(scope)?;
        match fs::metadata(&path) {
            Ok(meta) => Ok(meta.len()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    /// Check if the index needs rotation based on size.
    ///
    /// # Errors
    ///
    /// Returns an error if the index size cannot be determined.
    pub fn needs_rotation(&self, scope: StorageScope) -> anyhow::Result<bool> {
        let size = self.index_size(scope)?;
        Ok(size > self.config.max_index_bytes)
    }

    /// Invalidate the cache for a scope.
    ///
    /// Forces the next load to read from disk.
    pub fn invalidate_cache(&self, scope: StorageScope) {
        let cache = match scope {
            StorageScope::Global => &self.global_cache,
            StorageScope::Local => &self.local_cache,
        };
        *cache.write() = None;
    }

    /// Invalidate all caches.
    pub fn invalidate_all_caches(&self) {
        *self.global_cache.write() = None;
        *self.local_cache.write() = None;
    }

    /// Check if a project root is set for local storage.
    #[must_use]
    pub fn has_project_root(&self) -> bool {
        self.project_root.is_some()
    }

    /// Get the project root if set.
    #[must_use]
    pub fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
    }

    /// Get a reference to the configuration.
    #[must_use]
    pub fn config(&self) -> &PersistenceConfig {
        &self.config
    }

    // --- Private helpers ---

    /// Load metadata from a specific path.
    ///
    /// If the file is corrupted, logs a warning, backs up the corrupted file,
    /// and returns default metadata to allow graceful recovery.
    fn load_from_path(path: &Path) -> anyhow::Result<UserMetadata> {
        if !path.exists() {
            return Ok(UserMetadata::default());
        }

        // Defense in depth: reject unexpectedly large metadata files before allocation.
        // User metadata is typically <1 KB; 10 MB is a generous upper bound.
        const MAX_METADATA_BYTES: u64 = 10 * 1024 * 1024;
        let file_size = fs::metadata(path)?.len();
        if file_size > MAX_METADATA_BYTES {
            anyhow::bail!(
                "metadata file {} is unexpectedly large ({file_size} bytes, max {MAX_METADATA_BYTES})",
                path.display()
            );
        }

        let data = fs::read(path)?;

        let metadata: UserMetadata = match postcard::from_bytes(&data) {
            Ok(m) => m,
            Err(e) => {
                // Check if this looks like a corruption error (e.g., impossible allocation size)
                let err_str = e.to_string();
                if err_str.contains("allocation")
                    || err_str.contains("invalid")
                    || err_str.contains("unexpected end")
                {
                    // Back up the corrupted file for forensics
                    let backup_path = path.with_extension("corrupt.bak");
                    if let Err(backup_err) = fs::copy(path, &backup_path) {
                        log::warn!(
                            "Failed to back up corrupted file {}: {}",
                            path.display(),
                            backup_err
                        );
                    } else {
                        log::warn!(
                            "User metadata at {} was corrupted and has been backed up to {}. \
                             Starting with fresh metadata. Error: {}",
                            path.display(),
                            backup_path.display(),
                            e
                        );
                    }
                    // Remove the corrupted file
                    if let Err(rm_err) = fs::remove_file(path) {
                        log::warn!(
                            "Failed to remove corrupted file {}: {}",
                            path.display(),
                            rm_err
                        );
                    }
                    // Return default metadata for graceful recovery
                    return Ok(UserMetadata::default());
                }
                // For other errors, propagate them
                return Err(anyhow::anyhow!(
                    "Failed to deserialize user metadata from {}: {}. \
                     The index may be corrupted. Try removing the file and recreating your aliases.",
                    path.display(),
                    e
                ));
            }
        };

        // Version check
        if metadata.version != USER_METADATA_VERSION {
            anyhow::bail!(
                "Unsupported user metadata version {} (expected {}). \
                 Please upgrade sqry or remove the index file at {}",
                metadata.version,
                USER_METADATA_VERSION,
                path.display()
            );
        }

        Ok(metadata)
    }

    /// Save metadata to a specific path using atomic write.
    ///
    /// Uses a unique temp file name per process to prevent race conditions
    /// when multiple sqry instances run concurrently.
    fn save_to_path(path: &Path, metadata: &UserMetadata) -> anyhow::Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent)?;
        }

        // Create unique temp file in same directory for atomic rename
        // Include PID to prevent race conditions with concurrent sqry processes
        let temp_name = format!(
            "{}.tmp.{}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("index"),
            std::process::id()
        );
        let temp_path = path.with_file_name(temp_name);

        // Write to temp file
        {
            let data = postcard::to_allocvec(metadata)
                .map_err(|e| anyhow::anyhow!("Failed to serialize user metadata: {e}"))?;
            let mut file = File::create(&temp_path)?;
            file.write_all(&data)?;
            file.flush()?;
            // Ensure data is synced to disk before rename
            file.sync_all()?;
        }

        // Atomic rename
        fs::rename(&temp_path, path)?;

        Ok(())
    }
}

/// Create a shared user metadata index.
///
/// This is the recommended way to create an index for use across components.
///
/// # Errors
///
/// Returns an error if the index cannot be opened.
pub fn open_shared_index(
    project_root: Option<&Path>,
    config: PersistenceConfig,
) -> anyhow::Result<Arc<UserMetadataIndex>> {
    let index = UserMetadataIndex::open(project_root, config)?;
    Ok(Arc::new(index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::types::SavedAlias;
    use chrono::Utc;
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> PersistenceConfig {
        PersistenceConfig {
            global_dir_override: Some(dir.path().join("global")),
            local_dir_override: None,
            history_enabled: true,
            max_history_entries: 100,
            max_index_bytes: 1024 * 1024,
            redact_secrets: false,
        }
    }

    #[test]
    fn test_open_creates_global_dir() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let global_dir = config.global_config_dir().unwrap();

        assert!(!global_dir.exists());

        let _index = UserMetadataIndex::open(Some(dir.path()), config).unwrap();

        assert!(global_dir.exists());
    }

    #[test]
    fn test_load_returns_default_for_missing_file() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let index = UserMetadataIndex::open(Some(dir.path()), config).unwrap();

        let metadata = index.load(StorageScope::Global).unwrap();

        assert_eq!(metadata.version, USER_METADATA_VERSION);
        assert!(metadata.aliases.is_empty());
        assert!(metadata.history.entries.is_empty());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let index = UserMetadataIndex::open(Some(dir.path()), config).unwrap();

        let mut metadata = UserMetadata::default();
        metadata.aliases.insert(
            "test".to_string(),
            SavedAlias {
                command: "search".to_string(),
                args: vec!["main".to_string()],
                created: Utc::now(),
                description: Some("Test alias".to_string()),
            },
        );

        index.save(StorageScope::Global, &metadata).unwrap();

        // Invalidate cache to force disk read
        index.invalidate_cache(StorageScope::Global);

        let loaded = index.load(StorageScope::Global).unwrap();

        assert_eq!(loaded.aliases.len(), 1);
        assert!(loaded.aliases.contains_key("test"));
        assert_eq!(loaded.aliases["test"].command, "search");
    }

    #[test]
    fn test_update_atomic() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let index = UserMetadataIndex::open(Some(dir.path()), config).unwrap();

        // Add first alias
        index
            .update(StorageScope::Global, |m| {
                m.aliases.insert(
                    "first".to_string(),
                    SavedAlias {
                        command: "query".to_string(),
                        args: vec![],
                        created: Utc::now(),
                        description: None,
                    },
                );
                Ok(())
            })
            .unwrap();

        // Add second alias
        index
            .update(StorageScope::Global, |m| {
                m.aliases.insert(
                    "second".to_string(),
                    SavedAlias {
                        command: "search".to_string(),
                        args: vec![],
                        created: Utc::now(),
                        description: None,
                    },
                );
                Ok(())
            })
            .unwrap();

        let metadata = index.load(StorageScope::Global).unwrap();
        assert_eq!(metadata.aliases.len(), 2);
        assert!(metadata.aliases.contains_key("first"));
        assert!(metadata.aliases.contains_key("second"));
    }

    #[test]
    fn test_local_and_global_scopes_independent() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let index = UserMetadataIndex::open(Some(dir.path()), config).unwrap();

        // Save to global
        index
            .update(StorageScope::Global, |m| {
                m.aliases.insert(
                    "global-alias".to_string(),
                    SavedAlias {
                        command: "query".to_string(),
                        args: vec![],
                        created: Utc::now(),
                        description: None,
                    },
                );
                Ok(())
            })
            .unwrap();

        // Save to local
        index
            .update(StorageScope::Local, |m| {
                m.aliases.insert(
                    "local-alias".to_string(),
                    SavedAlias {
                        command: "search".to_string(),
                        args: vec![],
                        created: Utc::now(),
                        description: None,
                    },
                );
                Ok(())
            })
            .unwrap();

        let global = index.load(StorageScope::Global).unwrap();
        let local = index.load(StorageScope::Local).unwrap();

        assert_eq!(global.aliases.len(), 1);
        assert!(global.aliases.contains_key("global-alias"));

        assert_eq!(local.aliases.len(), 1);
        assert!(local.aliases.contains_key("local-alias"));
    }

    #[test]
    fn test_path_for_scope() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let index = UserMetadataIndex::open(Some(dir.path()), config.clone()).unwrap();

        let global_path = index.path_for_scope(StorageScope::Global).unwrap();
        assert!(global_path.ends_with(GLOBAL_INDEX_FILE));

        let local_path = index.path_for_scope(StorageScope::Local).unwrap();
        assert!(local_path.ends_with(LOCAL_INDEX_FILE));
    }

    #[test]
    fn test_index_size() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let index = UserMetadataIndex::open(Some(dir.path()), config).unwrap();

        // Empty/missing file returns 0
        assert_eq!(index.index_size(StorageScope::Global).unwrap(), 0);

        // Save some data
        let metadata = UserMetadata::default();
        index.save(StorageScope::Global, &metadata).unwrap();

        // Now should have non-zero size
        let size = index.index_size(StorageScope::Global).unwrap();
        assert!(size > 0);
    }

    #[test]
    fn test_needs_rotation() {
        let dir = TempDir::new().unwrap();
        let config = PersistenceConfig {
            global_dir_override: Some(dir.path().join("global")),
            max_index_bytes: 1, // Very small limit (postcard varint encoding is compact)
            ..Default::default()
        };
        let index = UserMetadataIndex::open(Some(dir.path()), config).unwrap();

        // Empty file doesn't need rotation
        assert!(!index.needs_rotation(StorageScope::Global).unwrap());

        // Save data that exceeds limit
        let metadata = UserMetadata::default();
        index.save(StorageScope::Global, &metadata).unwrap();

        // Should need rotation with tiny limit
        assert!(index.needs_rotation(StorageScope::Global).unwrap());
    }

    #[test]
    fn test_cache_invalidation() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let index = UserMetadataIndex::open(Some(dir.path()), config).unwrap();

        // Load populates cache
        let _metadata = index.load(StorageScope::Global).unwrap();

        // Modify file directly (simulating external change)
        let path = index.path_for_scope(StorageScope::Global).unwrap();
        let mut modified = UserMetadata::default();
        modified.aliases.insert(
            "external".to_string(),
            SavedAlias {
                command: "test".to_string(),
                args: vec![],
                created: Utc::now(),
                description: None,
            },
        );

        let data = postcard::to_allocvec(&modified).unwrap();
        let mut file = File::create(&path).unwrap();
        file.write_all(&data).unwrap();
        file.flush().unwrap();

        // Without invalidation, we get stale data
        let cached = index.load(StorageScope::Global).unwrap();
        assert!(cached.aliases.is_empty());

        // After invalidation, we get fresh data
        index.invalidate_cache(StorageScope::Global);
        let fresh = index.load(StorageScope::Global).unwrap();
        assert!(fresh.aliases.contains_key("external"));
    }

    #[test]
    fn test_open_shared_index() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);

        let shared = open_shared_index(Some(dir.path()), config).unwrap();

        assert!(shared.has_project_root());
        assert_eq!(shared.project_root(), Some(dir.path()));
    }

    #[test]
    fn test_no_project_root_local_fails() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let index = UserMetadataIndex::open(None, config).unwrap();

        assert!(!index.has_project_root());

        let result = index.path_for_scope(StorageScope::Local);
        assert!(result.is_err());
    }
}
