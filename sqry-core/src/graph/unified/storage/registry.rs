//! `FileRegistry`: Path deduplication with `FileId` handles.
//!
//! This module implements `FileRegistry`, which provides efficient storage
//! of file paths by deduplicating identical canonical paths.
//!
//! # Design
//!
//! - **Path deduplication**: Same canonical path returns same `FileId`
//! - **Canonical normalization**: Paths are canonicalized before registration
//! - **Bi-directional lookup**: ID → path and path → ID
//!
//! # Thread Safety
//!
//! The registry uses `Arc<Path>` for path storage. However, the registry itself
//! requires external synchronization (e.g., `RwLock`) for concurrent access.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::super::file::id::FileId;
use crate::graph::node::Language;

/// Error returned when a file path cannot be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// Path canonicalization failed.
    CanonicalizationFailed {
        /// The original path
        path: PathBuf,
        /// Error message
        message: String,
    },
    /// Registry capacity exhausted.
    CapacityExhausted,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CanonicalizationFailed { path, message } => {
                write!(
                    f,
                    "failed to canonicalize path '{}': {}",
                    path.display(),
                    message
                )
            }
            Self::CapacityExhausted => {
                write!(f, "file registry capacity exhausted (> 2^32 files)")
            }
        }
    }
}

impl std::error::Error for RegistryError {}

/// File entry storing path and language information.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileEntry {
    /// The canonical file path.
    path: Arc<Path>,
    /// The language of this file (if known).
    language: Option<Language>,
}

/// File registry for path deduplication.
///
/// `FileRegistry` stores file paths efficiently by maintaining a single
/// copy of each unique canonical path. When the same path is registered
/// multiple times (even with different formats), the same `FileId` is returned.
///
/// # Path Normalization
///
/// Paths are normalized before registration using a best-effort canonicalization:
/// - Relative paths are converted to absolute
/// - Symlinks are resolved (when possible)
/// - Path separators are normalized
///
/// # Language Tracking
///
/// Each file can optionally have an associated `Language` for filtering and
/// visualization purposes. Language can be set during registration or updated later.
///
/// # Example
///
/// ```rust,ignore
/// let mut registry = FileRegistry::new();
///
/// let id1 = registry.register(Path::new("src/lib.rs"))?;
/// let id2 = registry.register(Path::new("./src/../src/lib.rs"))?;
/// assert_eq!(id1, id2); // Same canonical path → same ID
///
/// // Set language
/// registry.set_language(id1, Language::Rust);
///
/// let path = registry.resolve(id1).unwrap();
/// assert!(path.ends_with("src/lib.rs"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRegistry {
    /// Storage of registered file entries, indexed by `FileId`.
    entries: Vec<Option<FileEntry>>,
    /// Reverse lookup from canonical path to index.
    lookup: HashMap<Arc<Path>, u32>,
    /// Free list of recycled slot indices.
    free_list: Vec<u32>,
}

impl FileRegistry {
    /// Creates a new empty file registry.
    #[must_use]
    pub fn new() -> Self {
        // Reserve index 0 for INVALID sentinel
        Self {
            entries: vec![None],
            lookup: HashMap::new(),
            free_list: Vec::new(),
        }
    }

    /// Creates a new registry with the specified capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let mut entries = Vec::with_capacity(capacity + 1);
        entries.push(None); // Reserve index 0

        Self {
            entries,
            lookup: HashMap::with_capacity(capacity),
            free_list: Vec::new(),
        }
    }

    /// Returns the number of registered files (excluding INVALID slot).
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.lookup.len()
    }

    /// Returns true if no files are registered.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lookup.is_empty()
    }

    /// Registers a file path and returns its `FileId`.
    ///
    /// The path is normalized using best-effort canonicalization before registration.
    /// If the canonical path was already registered, returns the existing ID.
    ///
    /// # Best-Effort Normalization
    ///
    /// This method uses fallback behavior when canonicalization fails:
    /// 1. Tries full canonicalization (resolve symlinks, make absolute)
    /// 2. Falls back to converting relative path to absolute using current directory
    /// 3. Falls back to using the path as-is
    ///
    /// This means non-existent or inaccessible paths are still registered, but
    /// may not be truly canonical. Use [`try_register_strict`] if you need to
    /// guarantee canonicalization succeeds.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::CapacityExhausted` if the registry has
    /// exhausted all available IDs (> 2^32 - 2 files).
    ///
    /// [`try_register_strict`]: Self::try_register_strict
    pub fn register(&mut self, path: &Path) -> Result<FileId, RegistryError> {
        self.register_with_language(path, None)
    }

    /// Registers a file path with an associated language.
    ///
    /// Similar to [`register`], but allows specifying the file's language.
    /// The language can be updated later using [`set_language`].
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::CapacityExhausted` if the registry has
    /// exhausted all available IDs (> 2^32 - 2 files).
    ///
    /// [`register`]: Self::register
    /// [`set_language`]: Self::set_language
    pub fn register_with_language(
        &mut self,
        path: &Path,
        language: Option<Language>,
    ) -> Result<FileId, RegistryError> {
        // Normalize the path
        let canonical = Self::normalize_path(path);
        let arc_path: Arc<Path> = Arc::from(canonical.as_path());

        // Check if already registered
        if let Some(&index) = self.lookup.get(&arc_path) {
            // Update language if provided and entry exists
            if let Some(lang) = language
                && let Some(Some(entry)) = self.entries.get_mut(index as usize)
            {
                entry.language = Some(lang);
            }
            return Ok(FileId::new(index));
        }

        // Create new file entry
        let entry = FileEntry {
            path: Arc::clone(&arc_path),
            language,
        };

        // Allocate new slot
        let index = if let Some(free_idx) = self.free_list.pop() {
            // Reuse a recycled slot
            self.entries[free_idx as usize] = Some(entry);
            free_idx
        } else {
            // Append new slot
            let idx = self.entries.len();
            if idx > u32::MAX as usize - 1 {
                return Err(RegistryError::CapacityExhausted);
            }
            self.entries.push(Some(entry));
            u32::try_from(idx).map_err(|_| RegistryError::CapacityExhausted)?
        };

        self.lookup.insert(arc_path, index);
        Ok(FileId::new(index))
    }

    /// Registers a file path with strict canonicalization.
    ///
    /// Unlike [`register`], this method returns an error if the path cannot
    /// be canonicalized. Use this when you need to guarantee that all registered
    /// paths are truly canonical.
    ///
    /// # Errors
    ///
    /// - [`RegistryError::CanonicalizationFailed`]: Path cannot be canonicalized
    ///   (e.g., file doesn't exist, permission denied, or symlink loop).
    /// - [`RegistryError::CapacityExhausted`]: Registry capacity exhausted.
    ///
    /// [`register`]: Self::register
    pub fn try_register_strict(&mut self, path: &Path) -> Result<FileId, RegistryError> {
        // Require successful canonicalization
        let canonical = path
            .canonicalize()
            .map_err(|e| RegistryError::CanonicalizationFailed {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;

        let arc_path: Arc<Path> = Arc::from(canonical.as_path());

        // Check if already registered
        if let Some(&index) = self.lookup.get(&arc_path) {
            return Ok(FileId::new(index));
        }

        // Create new file entry without language
        let entry = FileEntry {
            path: Arc::clone(&arc_path),
            language: None,
        };

        // Allocate new slot
        let index = if let Some(free_idx) = self.free_list.pop() {
            self.entries[free_idx as usize] = Some(entry);
            free_idx
        } else {
            let idx = self.entries.len();
            if idx > u32::MAX as usize - 1 {
                return Err(RegistryError::CapacityExhausted);
            }
            self.entries.push(Some(entry));
            u32::try_from(idx).map_err(|_| RegistryError::CapacityExhausted)?
        };

        self.lookup.insert(arc_path, index);
        Ok(FileId::new(index))
    }

    /// Registers a path without normalization (for already-canonical paths).
    ///
    /// Use this when you know the path is already canonical (e.g., from
    /// file system enumeration).
    ///
    /// # Errors
    ///
    /// Returns an error if the registry is at capacity.
    pub fn register_canonical(&mut self, path: &Path) -> Result<FileId, RegistryError> {
        self.register_canonical_with_language(path, None)
    }

    /// Registers a canonical path with an associated language.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry is at capacity.
    pub fn register_canonical_with_language(
        &mut self,
        path: &Path,
        language: Option<Language>,
    ) -> Result<FileId, RegistryError> {
        let arc_path: Arc<Path> = Arc::from(path);

        // Check if already registered
        if let Some(&index) = self.lookup.get(&arc_path) {
            // Update language if provided and entry exists
            if let Some(lang) = language
                && let Some(Some(entry)) = self.entries.get_mut(index as usize)
            {
                entry.language = Some(lang);
            }
            return Ok(FileId::new(index));
        }

        // Create new file entry
        let entry = FileEntry {
            path: Arc::clone(&arc_path),
            language,
        };

        // Allocate new slot
        let index = if let Some(free_idx) = self.free_list.pop() {
            self.entries[free_idx as usize] = Some(entry);
            free_idx
        } else {
            let idx = self.entries.len();
            if idx > u32::MAX as usize - 1 {
                return Err(RegistryError::CapacityExhausted);
            }
            self.entries.push(Some(entry));
            u32::try_from(idx).map_err(|_| RegistryError::CapacityExhausted)?
        };

        self.lookup.insert(arc_path, index);
        Ok(FileId::new(index))
    }

    /// Resolves a `FileId` to its path.
    ///
    /// Returns `None` if the ID is invalid or has been unregistered.
    #[must_use]
    pub fn resolve(&self, id: FileId) -> Option<Arc<Path>> {
        if id.is_invalid() {
            return None;
        }

        let index = id.index() as usize;
        self.entries
            .get(index)
            .and_then(|opt| opt.as_ref().map(|entry| Arc::clone(&entry.path)))
    }

    /// Gets the language for a file.
    ///
    /// Returns `None` if the file ID is invalid, the file was unregistered,
    /// or no language has been set for this file.
    #[must_use]
    pub fn language_for_file(&self, file_id: FileId) -> Option<Language> {
        if file_id.is_invalid() {
            return None;
        }

        let index = file_id.index() as usize;
        self.entries
            .get(index)
            .and_then(|opt| opt.as_ref())
            .and_then(|entry| entry.language)
    }

    /// Sets or updates the language for a file.
    ///
    /// Returns `true` if the language was set successfully, `false` if the
    /// file ID is invalid or the file was unregistered.
    pub fn set_language(&mut self, file_id: FileId, language: Language) -> bool {
        if file_id.is_invalid() {
            return false;
        }

        let index = file_id.index() as usize;
        if let Some(Some(entry)) = self.entries.get_mut(index) {
            entry.language = Some(language);
            true
        } else {
            false
        }
    }

    /// Gets all files that match the specified language.
    ///
    /// Returns a vector of `(FileId, Arc<Path>)` pairs for all files
    /// that have been assigned the given language.
    #[must_use]
    pub fn files_by_language(&self, language: Language) -> Vec<(FileId, Arc<Path>)> {
        self.entries
            .iter()
            .enumerate()
            .skip(1) // Skip INVALID slot
            .filter_map(|(idx, opt)| {
                opt.as_ref().and_then(|entry| {
                    if entry.language == Some(language) {
                        let idx_u32 = u32::try_from(idx).ok()?;
                        Some((FileId::new(idx_u32), Arc::clone(&entry.path)))
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    /// Unregisters a file, freeing its slot for reuse.
    ///
    /// Returns the path that was unregistered, or `None` if the ID was invalid.
    pub fn unregister(&mut self, id: FileId) -> Option<Arc<Path>> {
        if id.is_invalid() {
            return None;
        }

        let index = id.index() as usize;
        if index >= self.entries.len() {
            return None;
        }

        if let Some(entry) = self.entries[index].take() {
            self.lookup.remove(&entry.path);
            if let Ok(index_u32) = u32::try_from(index) {
                self.free_list.push(index_u32);
            } else {
                log::warn!("File registry index overflow when recycling slot {index}");
            }
            Some(entry.path)
        } else {
            None
        }
    }

    /// Checks if a path is registered.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        let canonical = Self::normalize_path(path);
        self.lookup.contains_key(canonical.as_path())
    }

    /// Checks if a path is registered without normalization.
    #[must_use]
    pub fn contains_canonical(&self, path: &Path) -> bool {
        self.lookup.contains_key(path)
    }

    /// Gets the `FileId` for a path if it's registered.
    ///
    /// Unlike `register()`, this does not create a new entry.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<FileId> {
        let canonical = Self::normalize_path(path);
        self.lookup
            .get(canonical.as_path())
            .map(|&idx| FileId::new(idx))
    }

    /// Gets the `FileId` for a canonical path if it's registered.
    #[must_use]
    pub fn get_canonical(&self, path: &Path) -> Option<FileId> {
        self.lookup.get(path).map(|&idx| FileId::new(idx))
    }

    /// Returns an iterator over all registered files with their IDs.
    pub fn iter(&self) -> impl Iterator<Item = (FileId, &Arc<Path>)> {
        self.entries
            .iter()
            .enumerate()
            .skip(1) // Skip INVALID slot
            .filter_map(|(idx, opt)| {
                opt.as_ref().and_then(|entry| {
                    u32::try_from(idx)
                        .ok()
                        .map(|idx_u32| (FileId::new(idx_u32), &entry.path))
                })
            })
    }

    /// Returns an iterator over all registered files with their IDs and languages.
    pub fn iter_with_language(
        &self,
    ) -> impl Iterator<Item = (FileId, &Arc<Path>, Option<Language>)> {
        self.entries
            .iter()
            .enumerate()
            .skip(1) // Skip INVALID slot
            .filter_map(|(idx, opt)| {
                opt.as_ref().and_then(|entry| {
                    u32::try_from(idx)
                        .ok()
                        .map(|idx_u32| (FileId::new(idx_u32), &entry.path, entry.language))
                })
            })
    }

    /// Registers multiple file paths in a single batch operation.
    ///
    /// Each file is registered with its optional language using
    /// [`register_with_language`]. Duplicate paths within the batch (or
    /// already registered) receive the same `FileId`, matching the
    /// existing deduplication behavior.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::CapacityExhausted` if the registry
    /// exhausts all available IDs during the batch.  On error, files
    /// registered before the failure remain in the registry.
    ///
    /// [`register_with_language`]: Self::register_with_language
    pub fn register_batch(
        &mut self,
        files: &[(PathBuf, Option<Language>)],
    ) -> Result<Vec<FileId>, RegistryError> {
        let mut ids = Vec::with_capacity(files.len());
        for (path, language) in files {
            let id = self.register_with_language(path, *language)?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// Clears all registered files.
    pub fn clear(&mut self) {
        self.entries.truncate(1); // Keep INVALID slot
        self.entries[0] = None;
        self.lookup.clear();
        self.free_list.clear();
    }

    /// Reserves capacity for at least `additional` more files.
    pub fn reserve(&mut self, additional: usize) {
        self.entries.reserve(additional);
        self.lookup.reserve(additional);
    }

    /// Returns statistics about the registry.
    #[must_use]
    pub fn stats(&self) -> RegistryStats {
        RegistryStats {
            file_count: self.len(),
            free_slots: self.free_list.len(),
            capacity: self.entries.capacity(),
        }
    }

    /// Normalizes a path for consistent lookup.
    ///
    /// This performs best-effort canonicalization:
    /// - Tries to canonicalize (resolve symlinks, absolute path)
    /// - Falls back to converting to absolute path
    /// - Falls back to using the path as-is if it's already absolute
    fn normalize_path(path: &Path) -> PathBuf {
        // Try full canonicalization first
        if let Ok(canonical) = path.canonicalize() {
            return canonical;
        }

        // Fall back to just making it absolute
        if path.is_relative()
            && let Ok(cwd) = std::env::current_dir()
        {
            return cwd.join(path);
        }

        // Last resort: return as-is (converted to PathBuf)
        path.to_path_buf()
    }
}

impl Default for FileRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for FileRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FileRegistry(files={}, free={})",
            self.len(),
            self.free_list.len()
        )
    }
}

/// Statistics about a `FileRegistry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryStats {
    /// Number of registered files.
    pub file_count: usize,
    /// Number of free (recyclable) slots.
    pub free_slots: usize,
    /// Allocated capacity.
    pub capacity: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_new() {
        let registry = FileRegistry::new();
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
    }

    #[test]
    fn test_with_capacity() {
        let registry = FileRegistry::with_capacity(100);
        assert_eq!(registry.len(), 0);
        assert!(registry.entries.capacity() >= 101); // +1 for INVALID slot
    }

    #[test]
    fn test_register_and_resolve() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "fn main() {}").unwrap();

        let mut registry = FileRegistry::new();
        let id = registry.register(&file_path).unwrap();

        assert!(!id.is_invalid());
        assert_eq!(registry.len(), 1);

        let resolved = registry.resolve(id).unwrap();
        // Both should resolve to the same canonical path
        assert_eq!(resolved.canonicalize().ok(), file_path.canonicalize().ok());
    }

    #[test]
    fn test_register_duplicate() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "").unwrap();

        let mut registry = FileRegistry::new();
        let id1 = registry.register(&file_path).unwrap();
        let id2 = registry.register(&file_path).unwrap();

        assert_eq!(id1, id2);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_register_different() {
        let tmp = TempDir::new().unwrap();
        let file1 = tmp.path().join("a.rs");
        let file2 = tmp.path().join("b.rs");
        fs::write(&file1, "").unwrap();
        fs::write(&file2, "").unwrap();

        let mut registry = FileRegistry::new();
        let id1 = registry.register(&file1).unwrap();
        let id2 = registry.register(&file2).unwrap();

        assert_ne!(id1, id2);
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn test_register_canonical() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/canonical/path/file.rs");
        let id = registry.register_canonical(path).unwrap();

        assert!(!id.is_invalid());
        assert_eq!(registry.resolve(id).unwrap().as_ref(), path);
    }

    #[test]
    fn test_resolve_invalid() {
        let registry = FileRegistry::new();
        assert!(registry.resolve(FileId::INVALID).is_none());
    }

    #[test]
    fn test_resolve_out_of_bounds() {
        let registry = FileRegistry::new();
        assert!(registry.resolve(FileId::new(999)).is_none());
    }

    #[test]
    fn test_unregister() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/test/file.rs");
        let id = registry.register_canonical(path).unwrap();

        assert_eq!(registry.len(), 1);

        let removed = registry.unregister(id);
        assert!(removed.is_some());
        assert_eq!(registry.len(), 0);
        assert!(registry.resolve(id).is_none());
    }

    #[test]
    fn test_unregister_invalid() {
        let mut registry = FileRegistry::new();
        assert!(registry.unregister(FileId::INVALID).is_none());
    }

    #[test]
    fn test_free_list_reuse() {
        let mut registry = FileRegistry::new();
        let path1 = Path::new("/test/a.rs");
        let path2 = Path::new("/test/b.rs");

        let id1 = registry.register_canonical(path1).unwrap();
        registry.unregister(id1);

        let id2 = registry.register_canonical(path2).unwrap();
        assert_eq!(id1.index(), id2.index()); // Same slot reused
    }

    #[test]
    fn test_contains() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "").unwrap();

        let mut registry = FileRegistry::new();
        registry.register(&file_path).unwrap();

        assert!(registry.contains(&file_path));
        assert!(!registry.contains(Path::new("/nonexistent/path.rs")));
    }

    #[test]
    fn test_contains_canonical() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/canonical/test.rs");
        registry.register_canonical(path).unwrap();

        assert!(registry.contains_canonical(path));
        assert!(!registry.contains_canonical(Path::new("/other/path.rs")));
    }

    #[test]
    fn test_get() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "").unwrap();

        let mut registry = FileRegistry::new();
        let id = registry.register(&file_path).unwrap();

        assert_eq!(registry.get(&file_path), Some(id));
        assert_eq!(registry.get(Path::new("/nonexistent.rs")), None);
    }

    #[test]
    fn test_get_canonical() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/canonical/test.rs");
        let id = registry.register_canonical(path).unwrap();

        assert_eq!(registry.get_canonical(path), Some(id));
        assert_eq!(registry.get_canonical(Path::new("/other.rs")), None);
    }

    #[test]
    fn test_iter() {
        let mut registry = FileRegistry::new();
        registry.register_canonical(Path::new("/a.rs")).unwrap();
        registry.register_canonical(Path::new("/b.rs")).unwrap();
        registry.register_canonical(Path::new("/c.rs")).unwrap();

        let paths: Vec<_> = registry.iter().map(|(_, p)| p.to_path_buf()).collect();
        assert_eq!(paths.len(), 3);
        assert!(paths.contains(&PathBuf::from("/a.rs")));
        assert!(paths.contains(&PathBuf::from("/b.rs")));
        assert!(paths.contains(&PathBuf::from("/c.rs")));
    }

    #[test]
    fn test_clear() {
        let mut registry = FileRegistry::new();
        registry.register_canonical(Path::new("/a.rs")).unwrap();
        registry.register_canonical(Path::new("/b.rs")).unwrap();

        assert_eq!(registry.len(), 2);
        registry.clear();
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
    }

    #[test]
    fn test_reserve() {
        let mut registry = FileRegistry::new();
        registry.reserve(1000);
        assert!(registry.entries.capacity() >= 1001);
    }

    #[test]
    fn test_display() {
        let mut registry = FileRegistry::new();
        registry.register_canonical(Path::new("/test.rs")).unwrap();

        let display = format!("{registry}");
        assert!(display.contains("FileRegistry"));
        assert!(display.contains("files=1"));
    }

    #[test]
    fn test_stats() {
        let mut registry = FileRegistry::new();
        registry.register_canonical(Path::new("/a.rs")).unwrap();
        registry.register_canonical(Path::new("/b.rs")).unwrap();

        let stats = registry.stats();
        assert_eq!(stats.file_count, 2);
        assert_eq!(stats.free_slots, 0);
    }

    #[test]
    fn test_default() {
        let registry: FileRegistry = FileRegistry::default();
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_registry_error_display() {
        let err = RegistryError::CanonicalizationFailed {
            path: PathBuf::from("/test/path"),
            message: "not found".to_string(),
        };
        let display = format!("{err}");
        assert!(display.contains("/test/path"));
        assert!(display.contains("not found"));

        let err2 = RegistryError::CapacityExhausted;
        let display2 = format!("{err2}");
        assert!(display2.contains("capacity exhausted"));
    }

    #[test]
    fn test_unicode_path() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/日本語/ファイル.rs");
        let id = registry.register_canonical(path).unwrap();

        let resolved = registry.resolve(id).unwrap();
        assert_eq!(resolved.as_ref(), path);
    }

    #[test]
    fn test_try_register_strict_success() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "fn main() {}").unwrap();

        let mut registry = FileRegistry::new();
        let id = registry.try_register_strict(&file_path).unwrap();

        assert!(!id.is_invalid());
        assert_eq!(registry.len(), 1);

        // Verify it returns canonical path
        let resolved = registry.resolve(id).unwrap();
        assert_eq!(resolved.as_ref(), file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_try_register_strict_nonexistent() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/nonexistent/path/that/does/not/exist.rs");

        let result = registry.try_register_strict(path);
        assert!(result.is_err());

        match result.unwrap_err() {
            RegistryError::CanonicalizationFailed {
                path: err_path,
                message,
            } => {
                assert_eq!(err_path, path);
                assert!(!message.is_empty());
            }
            RegistryError::CapacityExhausted => {
                panic!("Expected CanonicalizationFailed, got CapacityExhausted")
            }
        }
    }

    #[test]
    fn test_register_fallback_nonexistent() {
        // Verify that register() uses fallback for non-existent paths
        let mut registry = FileRegistry::new();
        let path = Path::new("/nonexistent/path/file.rs");

        // register() should succeed with fallback behavior
        let result = registry.register(path);
        assert!(result.is_ok());

        let id = result.unwrap();
        let resolved = registry.resolve(id).unwrap();

        // The path should be stored (as-is since it can't be canonicalized)
        // and should contain the original path components
        assert!(resolved.to_string_lossy().contains("file.rs"));
    }

    #[test]
    fn test_try_register_strict_duplicate() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "").unwrap();

        let mut registry = FileRegistry::new();
        let id1 = registry.try_register_strict(&file_path).unwrap();
        let id2 = registry.try_register_strict(&file_path).unwrap();

        assert_eq!(id1, id2);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_language_tracking_basic() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/test/file.rs");
        let id = registry.register_canonical(path).unwrap();

        // Initially no language
        assert_eq!(registry.language_for_file(id), None);

        // Set language
        assert!(registry.set_language(id, Language::Rust));
        assert_eq!(registry.language_for_file(id), Some(Language::Rust));

        // Update language
        assert!(registry.set_language(id, Language::JavaScript));
        assert_eq!(registry.language_for_file(id), Some(Language::JavaScript));
    }

    #[test]
    fn test_language_tracking_invalid_id() {
        let mut registry = FileRegistry::new();

        // Invalid ID should return None
        assert_eq!(registry.language_for_file(FileId::INVALID), None);

        // Setting language on invalid ID should fail
        assert!(!registry.set_language(FileId::INVALID, Language::Rust));
    }

    #[test]
    fn test_register_with_language() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/test/main.py");

        let id = registry
            .register_canonical_with_language(path, Some(Language::Python))
            .unwrap();

        assert_eq!(registry.language_for_file(id), Some(Language::Python));
        assert_eq!(registry.resolve(id).unwrap().as_ref(), path);
    }

    #[test]
    fn test_register_with_language_duplicate_updates() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/test/script.js");

        // Register with JavaScript
        let id1 = registry
            .register_canonical_with_language(path, Some(Language::JavaScript))
            .unwrap();
        assert_eq!(registry.language_for_file(id1), Some(Language::JavaScript));

        // Re-register same path with TypeScript updates language
        let id2 = registry
            .register_canonical_with_language(path, Some(Language::TypeScript))
            .unwrap();

        assert_eq!(id1, id2, "Should return same ID for duplicate path");
        assert_eq!(registry.language_for_file(id2), Some(Language::TypeScript));
    }

    #[test]
    fn test_files_by_language_empty() {
        let registry = FileRegistry::new();
        let files = registry.files_by_language(Language::Rust);
        assert!(files.is_empty());
    }

    #[test]
    fn test_files_by_language_single() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/src/main.rs");
        let id = registry
            .register_canonical_with_language(path, Some(Language::Rust))
            .unwrap();

        let files = registry.files_by_language(Language::Rust);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, id);
        assert_eq!(files[0].1.as_ref(), path);

        // Different language should return empty
        let js_files = registry.files_by_language(Language::JavaScript);
        assert!(js_files.is_empty());
    }

    #[test]
    fn test_files_by_language_multiple() {
        let mut registry = FileRegistry::new();

        // Register Rust files
        let rs1 = Path::new("/src/main.rs");
        let rs2 = Path::new("/src/lib.rs");
        let id1 = registry
            .register_canonical_with_language(rs1, Some(Language::Rust))
            .unwrap();
        let id2 = registry
            .register_canonical_with_language(rs2, Some(Language::Rust))
            .unwrap();

        // Register Python file
        let py1 = Path::new("/scripts/test.py");
        let id3 = registry
            .register_canonical_with_language(py1, Some(Language::Python))
            .unwrap();

        // Register file without language
        let _ = registry
            .register_canonical(Path::new("/data/config.json"))
            .unwrap();

        // Query Rust files
        let rust_files = registry.files_by_language(Language::Rust);
        assert_eq!(rust_files.len(), 2);
        let rust_ids: Vec<_> = rust_files.iter().map(|(id, _)| *id).collect();
        assert!(rust_ids.contains(&id1));
        assert!(rust_ids.contains(&id2));

        // Query Python files
        let python_files = registry.files_by_language(Language::Python);
        assert_eq!(python_files.len(), 1);
        assert_eq!(python_files[0].0, id3);

        // Query JavaScript files (none registered)
        let js_files = registry.files_by_language(Language::JavaScript);
        assert!(js_files.is_empty());
    }

    #[test]
    fn test_iter_with_language() {
        let mut registry = FileRegistry::new();

        let rs_path = Path::new("/src/main.rs");
        let py_path = Path::new("/scripts/test.py");
        let no_lang_path = Path::new("/config.json");

        let id1 = registry
            .register_canonical_with_language(rs_path, Some(Language::Rust))
            .unwrap();
        let id2 = registry
            .register_canonical_with_language(py_path, Some(Language::Python))
            .unwrap();
        let id3 = registry.register_canonical(no_lang_path).unwrap();

        let entries: Vec<_> = registry.iter_with_language().collect();
        assert_eq!(entries.len(), 3);

        // Find each entry and verify
        let rs_entry = entries.iter().find(|(id, _, _)| *id == id1).unwrap();
        assert_eq!(rs_entry.1.as_ref(), rs_path);
        assert_eq!(rs_entry.2, Some(Language::Rust));

        let py_entry = entries.iter().find(|(id, _, _)| *id == id2).unwrap();
        assert_eq!(py_entry.1.as_ref(), py_path);
        assert_eq!(py_entry.2, Some(Language::Python));

        let no_lang_entry = entries.iter().find(|(id, _, _)| *id == id3).unwrap();
        assert_eq!(no_lang_entry.1.as_ref(), no_lang_path);
        assert_eq!(no_lang_entry.2, None);
    }

    #[test]
    fn test_unregister_with_language() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/test/file.rs");
        let id = registry
            .register_canonical_with_language(path, Some(Language::Rust))
            .unwrap();

        assert_eq!(registry.language_for_file(id), Some(Language::Rust));
        assert_eq!(registry.files_by_language(Language::Rust).len(), 1);

        // Unregister
        let removed = registry.unregister(id);
        assert!(removed.is_some());

        // Language should be gone
        assert_eq!(registry.language_for_file(id), None);
        assert_eq!(registry.files_by_language(Language::Rust).len(), 0);
    }

    #[test]
    fn test_language_serialization() {
        let mut registry = FileRegistry::new();
        let path1 = Path::new("/src/main.rs");
        let path2 = Path::new("/src/lib.py");

        registry
            .register_canonical_with_language(path1, Some(Language::Rust))
            .unwrap();
        registry
            .register_canonical_with_language(path2, Some(Language::Python))
            .unwrap();

        // Serialize to JSON
        let json = serde_json::to_string(&registry).unwrap();

        // Deserialize
        let deserialized: FileRegistry = serde_json::from_str(&json).unwrap();

        // Verify languages are preserved
        let rust_files = deserialized.files_by_language(Language::Rust);
        assert_eq!(rust_files.len(), 1);

        let python_files = deserialized.files_by_language(Language::Python);
        assert_eq!(python_files.len(), 1);
    }

    #[test]
    fn test_language_with_best_effort_register() {
        let mut registry = FileRegistry::new();
        // Non-existent path (uses fallback normalization)
        let path = Path::new("/nonexistent/file.rs");

        let id = registry
            .register_with_language(path, Some(Language::Rust))
            .unwrap();

        assert_eq!(registry.language_for_file(id), Some(Language::Rust));
    }

    #[test]
    fn test_register_batch() {
        let tmp = TempDir::new().unwrap();
        let file1 = tmp.path().join("alpha.rs");
        let file2 = tmp.path().join("beta.py");
        let file3 = tmp.path().join("gamma.js");
        fs::write(&file1, "fn main() {}").unwrap();
        fs::write(&file2, "print('hello')").unwrap();
        fs::write(&file3, "console.log('hi')").unwrap();

        let mut registry = FileRegistry::new();
        let files: Vec<(PathBuf, Option<Language>)> = vec![
            (file1.clone(), Some(Language::Rust)),
            (file2.clone(), Some(Language::Python)),
            (file3.clone(), Some(Language::JavaScript)),
        ];

        let ids = registry.register_batch(&files).unwrap();

        // Should return 3 unique IDs
        assert_eq!(ids.len(), 3);
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[1], ids[2]);
        assert_ne!(ids[0], ids[2]);

        // All IDs should be resolvable
        for (i, id) in ids.iter().enumerate() {
            let resolved = registry.resolve(*id).unwrap();
            let expected_canonical = files[i].0.canonicalize().unwrap();
            assert_eq!(resolved.as_ref(), expected_canonical.as_path());
        }

        // Languages should be set
        assert_eq!(registry.language_for_file(ids[0]), Some(Language::Rust));
        assert_eq!(registry.language_for_file(ids[1]), Some(Language::Python));
        assert_eq!(
            registry.language_for_file(ids[2]),
            Some(Language::JavaScript)
        );

        // Registry should have 3 files
        assert_eq!(registry.len(), 3);
    }

    #[test]
    fn test_register_batch_empty() {
        let mut registry = FileRegistry::new();
        let files: Vec<(PathBuf, Option<Language>)> = vec![];

        let ids = registry.register_batch(&files).unwrap();

        assert!(ids.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_register_batch_duplicate_paths() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("dup.rs");
        fs::write(&file, "fn main() {}").unwrap();

        let mut registry = FileRegistry::new();
        let files: Vec<(PathBuf, Option<Language>)> = vec![
            (file.clone(), Some(Language::Rust)),
            (file.clone(), Some(Language::Rust)),
        ];

        let ids = registry.register_batch(&files).unwrap();

        // Duplicate path returns same FileId (deduplication behavior)
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], ids[1]);

        // Only 1 unique file registered
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_register_batch_duplicate_paths_different_languages() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("polyglot.rs");
        fs::write(&file, "fn main() {}").unwrap();

        let mut registry = FileRegistry::new();
        let files: Vec<(PathBuf, Option<Language>)> = vec![
            (file.clone(), Some(Language::Rust)),
            (file.clone(), Some(Language::Python)),
        ];

        let ids = registry.register_batch(&files).unwrap();

        // Same FileId for same path
        assert_eq!(ids[0], ids[1]);
        assert_eq!(registry.len(), 1);

        // Last language wins (matches register_with_language dedup behavior)
        assert_eq!(registry.language_for_file(ids[0]), Some(Language::Python));
    }
}
