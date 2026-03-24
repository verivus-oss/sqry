//! Query caching module

mod ast_parse_cache;
mod budget;
mod result_cache;
mod types;

pub use ast_parse_cache::AstParseCache;
pub use budget::{BudgetConfig, BudgetStats, CacheBudgetController, ClampAction, ClampReason};
pub use result_cache::ResultCache;
pub use types::{CacheKey, CacheStats};

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// File metadata for cache invalidation (local definition after legacy index removal)
#[derive(Debug, Clone)]
pub struct FileMetadata {
    /// Path to the file
    pub path: PathBuf,
    /// Last modification time
    pub modified_time: SystemTime,
    /// File size in bytes
    pub size: u64,
}

/// Compute hash of file metadata set
///
/// This hashes (path, mtime, size) for all files in deterministic order.
/// Uses `to_string_lossy` for cross-platform path compatibility.
#[must_use]
pub fn compute_file_set_hash<S>(file_metadata: &HashMap<PathBuf, FileMetadata, S>) -> u64
where
    S: BuildHasher,
{
    let mut hasher = DefaultHasher::new();

    // Sort paths for deterministic ordering
    let mut paths: Vec<_> = file_metadata.keys().collect();
    paths.sort();

    for path in paths {
        let meta = &file_metadata[path];

        // Hash (path, mtime, size) tuple
        // Use to_string_lossy for cross-platform compatibility (Windows safe)
        hasher.write(path.to_string_lossy().as_bytes());

        // Hash mtime (as milliseconds for better precision)
        if let Ok(duration) = meta.modified_time.duration_since(std::time::UNIX_EPOCH) {
            hasher.write(&duration.as_millis().to_le_bytes());
        }

        hasher.write(&meta.size.to_le_bytes());
    }

    hasher.finish()
}

/// Compute hash of workspace root path
///
/// Uses `to_string_lossy` for cross-platform compatibility.
#[must_use]
pub fn compute_root_path_hash(root: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Use to_string_lossy for cross-platform compatibility (Windows safe)
    hasher.write(root.to_string_lossy().as_bytes());
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn file_set_hash_deterministic() {
        let mut metadata = HashMap::new();
        metadata.insert(
            PathBuf::from("file1.rs"),
            FileMetadata {
                path: PathBuf::from("file1.rs"),
                modified_time: SystemTime::now(),
                size: 100,
            },
        );

        let hash1 = compute_file_set_hash(&metadata);
        let hash2 = compute_file_set_hash(&metadata);

        assert_eq!(hash1, hash2, "Hash should be deterministic");
    }

    #[test]
    fn root_path_hash_deterministic() {
        let path = Path::new("/home/user/project");
        let hash1 = compute_root_path_hash(path);
        let hash2 = compute_root_path_hash(path);

        assert_eq!(hash1, hash2, "Hash should be deterministic");
    }

    #[test]
    fn file_set_hash_changes_on_modification() {
        let time1 = SystemTime::now();
        let mut metadata1 = HashMap::new();
        metadata1.insert(
            PathBuf::from("file1.rs"),
            FileMetadata {
                path: PathBuf::from("file1.rs"),
                modified_time: time1,
                size: 100,
            },
        );

        let mut metadata2 = HashMap::new();
        metadata2.insert(
            PathBuf::from("file1.rs"),
            FileMetadata {
                path: PathBuf::from("file1.rs"),
                modified_time: time1,
                size: 200, // Changed
            },
        );

        let hash1 = compute_file_set_hash(&metadata1);
        let hash2 = compute_file_set_hash(&metadata2);

        assert_ne!(hash1, hash2, "Hash should change when file changes");
    }
}
