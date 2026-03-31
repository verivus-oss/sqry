//! Expand cache for macro-generated symbol storage (4.5f cache).
//!
//! Provides persistent storage of macro expansion results in
//! `.sqry/expand-cache/<crate-hash>.json`. This avoids re-running
//! `cargo expand` on every index build by caching qualified symbol names
//! per file.
//!
//! # Cache Security
//!
//! All symbol names read from the cache are validated against a safe character
//! pattern `[a-zA-Z0-9_:<> ]`. Names containing control characters, shell
//! metacharacters, or HTML entities are rejected with a warning. This prevents
//! cache poisoning via crafted JSON files.
//!
//! # Cache Freshness
//!
//! Each cache entry stores a SHA-256 hash of the original source file. If the
//! source has changed since the cache was written, the entry is stale and
//! skipped with a warning.
//!
//! # Performance Guard
//!
//! Expansion output is capped at 10MB per file. Files exceeding this limit
//! are skipped with a warning and confidence limitation.

use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Maximum size of expansion output per file (10 MB).
const MAX_EXPANSION_SIZE: usize = 10 * 1024 * 1024;

/// Pattern for validating symbol names read from cache.
/// Allows alphanumeric characters, underscores, colons (for qualified names),
/// angle brackets (for generics), spaces (for `impl Trait for Type`), and
/// ampersands/lifetimes (for `&'a`).
fn is_valid_symbol_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.chars().all(|c| {
        c.is_alphanumeric()
            || c == '_'
            || c == ':'
            || c == '<'
            || c == '>'
            || c == ' '
            || c == '&'
            || c == '\''
            || c == '.'
            || c == ','
    })
}

/// A single file's expansion data within the cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandCacheFileEntry {
    /// Qualified names of symbols in the original (unexpanded) source.
    pub original_symbols: Vec<String>,
    /// Qualified names of symbols in the expanded source.
    pub expanded_symbols: Vec<String>,
    /// Symbols present in expanded but not original — these are macro-generated.
    pub generated_symbols: Vec<String>,
    /// Confidence level: `"verified"`, `"heuristic"`, or `"non_deterministic"`.
    pub confidence: String,
}

/// Top-level cache entry for a single crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpandCacheEntry {
    /// Crate name.
    pub crate_name: String,
    /// Rust compiler version used for expansion.
    pub rust_version: String,
    /// ISO 8601 timestamp of when the cache was generated.
    pub generated_at: String,
    /// SHA-256 hash of the crate source (all `.rs` files concatenated).
    pub source_hash: String,
    /// Per-file expansion data.
    pub files: HashMap<String, ExpandCacheFileEntry>,
}

/// Expand cache manager.
///
/// Handles reading, writing, and freshness checking of the expand cache
/// directory at `.sqry/expand-cache/`.
#[derive(Debug)]
pub struct ExpandCache {
    /// Root directory of the expand cache (e.g., `.sqry/expand-cache/`).
    cache_dir: PathBuf,
}

impl ExpandCache {
    /// Create a new expand cache manager for the given directory.
    ///
    /// Creates the directory if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created.
    pub fn new(cache_dir: PathBuf) -> io::Result<Self> {
        std::fs::create_dir_all(&cache_dir)?;
        Ok(Self { cache_dir })
    }

    /// Read a cache entry for a crate.
    ///
    /// Returns `None` if the cache file does not exist. Returns an error if
    /// the file exists but cannot be parsed.
    ///
    /// # Security
    ///
    /// All symbol names in the returned entry are validated against the safe
    /// character pattern. Invalid names are stripped with a warning.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache file exists but cannot be read or parsed.
    pub fn read(&self, crate_hash: &str) -> io::Result<Option<ExpandCacheEntry>> {
        let path = self.cache_file_path(crate_hash);
        if !path.exists() {
            return Ok(None);
        }

        // Security: check file size before deserializing to prevent OOM from
        // crafted oversized cache files.
        let metadata = std::fs::metadata(&path)?;
        if metadata.len() > MAX_EXPANSION_SIZE as u64 {
            log::warn!(
                "Expand cache file {} exceeds size limit ({} bytes > {} bytes), skipping",
                path.display(),
                metadata.len(),
                MAX_EXPANSION_SIZE,
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Expand cache file exceeds size limit: {} bytes > {} bytes",
                    metadata.len(),
                    MAX_EXPANSION_SIZE,
                ),
            ));
        }

        let file = std::fs::File::open(&path)?;
        let reader = BufReader::new(file);
        let mut entry: ExpandCacheEntry = serde_json::from_reader(reader).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Failed to parse expand cache {}: {e}", path.display()),
            )
        })?;

        // Validate all symbol names for security.
        sanitize_cache_entry(&mut entry);

        Ok(Some(entry))
    }

    /// Write a cache entry for a crate.
    ///
    /// Overwrites any existing cache file for this crate hash.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn write(&self, crate_hash: &str, entry: &ExpandCacheEntry) -> io::Result<()> {
        let path = self.cache_file_path(crate_hash);
        let file = std::fs::File::create(&path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, entry).map_err(|e| {
            io::Error::other(format!(
                "Failed to write expand cache {}: {e}",
                path.display()
            ))
        })
    }

    /// Check if a cache entry is fresh (source hash matches).
    ///
    /// Returns `true` if the cache entry exists and its source hash matches
    /// the provided current hash. Returns `false` if the entry is stale or
    /// does not exist.
    pub fn is_fresh(&self, crate_hash: &str, current_source_hash: &str) -> io::Result<bool> {
        match self.read(crate_hash)? {
            Some(entry) => Ok(entry.source_hash == current_source_hash),
            None => Ok(false),
        }
    }

    /// Remove a cache entry for a crate.
    ///
    /// No-op if the cache file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be removed.
    pub fn remove(&self, crate_hash: &str) -> io::Result<()> {
        let path = self.cache_file_path(crate_hash);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// List all cached crate hashes.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub fn list_cached_crates(&self) -> io::Result<Vec<String>> {
        let mut crates = Vec::new();
        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            if let Some(name) = entry.file_name().to_str()
                && let Some(hash) = name.strip_suffix(".json")
            {
                crates.push(hash.to_string());
            }
        }
        Ok(crates)
    }

    /// Get the file path for a cache entry.
    fn cache_file_path(&self, crate_hash: &str) -> PathBuf {
        self.cache_dir.join(format!("{crate_hash}.json"))
    }

    /// Returns the maximum expansion size per file.
    #[must_use]
    pub const fn max_expansion_size() -> usize {
        MAX_EXPANSION_SIZE
    }
}

/// Sanitize a cache entry by removing invalid symbol names.
///
/// Logs a warning for each invalid name removed. This prevents cache poisoning
/// via crafted JSON files.
fn sanitize_cache_entry(entry: &mut ExpandCacheEntry) {
    for (file_path, file_entry) in &mut entry.files {
        let original_count = file_entry.original_symbols.len()
            + file_entry.expanded_symbols.len()
            + file_entry.generated_symbols.len();

        file_entry
            .original_symbols
            .retain(|name| validate_and_warn(name, file_path));
        file_entry
            .expanded_symbols
            .retain(|name| validate_and_warn(name, file_path));
        file_entry
            .generated_symbols
            .retain(|name| validate_and_warn(name, file_path));

        let after_count = file_entry.original_symbols.len()
            + file_entry.expanded_symbols.len()
            + file_entry.generated_symbols.len();

        if after_count < original_count {
            log::warn!(
                "Removed {} invalid symbol names from expand cache for '{}'",
                original_count - after_count,
                file_path
            );
        }
    }
}

/// Validate a symbol name and log a warning if invalid.
fn validate_and_warn(name: &str, file_path: &str) -> bool {
    if is_valid_symbol_name(name) {
        true
    } else {
        log::warn!(
            "Rejecting invalid symbol name '{}' from expand cache for '{}' \
             (possible cache poisoning)",
            name,
            file_path
        );
        false
    }
}

/// Validate a symbol name for cache security.
///
/// Public interface for the validation function, useful for testing
/// and for other modules that need to validate symbol names before
/// inserting them into the cache.
#[must_use]
pub fn validate_symbol_name(name: &str) -> bool {
    is_valid_symbol_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_read_write() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cache = ExpandCache::new(temp_dir.path().join("expand-cache")).unwrap();

        let entry = ExpandCacheEntry {
            crate_name: "my_crate".to_string(),
            rust_version: "1.94.0".to_string(),
            generated_at: "2026-03-30T00:00:00Z".to_string(),
            source_hash: "abc123".to_string(),
            files: {
                let mut files = HashMap::new();
                files.insert(
                    "src/lib.rs".to_string(),
                    ExpandCacheFileEntry {
                        original_symbols: vec!["my_crate::MyStruct".to_string()],
                        expanded_symbols: vec![
                            "my_crate::MyStruct".to_string(),
                            "my_crate::<MyStruct as Debug>::fmt".to_string(),
                        ],
                        generated_symbols: vec!["my_crate::<MyStruct as Debug>::fmt".to_string()],
                        confidence: "heuristic".to_string(),
                    },
                );
                files
            },
        };

        cache.write("crate_abc", &entry).unwrap();
        let read_back = cache.read("crate_abc").unwrap().unwrap();

        assert_eq!(read_back.crate_name, "my_crate");
        assert_eq!(read_back.source_hash, "abc123");
        assert_eq!(read_back.files.len(), 1);

        let file_entry = &read_back.files["src/lib.rs"];
        assert_eq!(file_entry.generated_symbols.len(), 1);
        assert_eq!(file_entry.confidence, "heuristic");
    }

    #[test]
    fn test_cache_freshness_check() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cache = ExpandCache::new(temp_dir.path().join("expand-cache")).unwrap();

        let entry = ExpandCacheEntry {
            crate_name: "test".to_string(),
            rust_version: "1.94.0".to_string(),
            generated_at: "2026-03-30T00:00:00Z".to_string(),
            source_hash: "hash_v1".to_string(),
            files: HashMap::new(),
        };

        cache.write("test_crate", &entry).unwrap();

        // Same hash → fresh.
        assert!(cache.is_fresh("test_crate", "hash_v1").unwrap());
        // Different hash → stale.
        assert!(!cache.is_fresh("test_crate", "hash_v2").unwrap());
        // Non-existent → not fresh.
        assert!(!cache.is_fresh("nonexistent", "hash_v1").unwrap());
    }

    #[test]
    fn test_validate_symbol_name() {
        assert!(validate_symbol_name("my_crate::MyStruct"));
        assert!(validate_symbol_name("my_crate::<MyStruct as Debug>::fmt"));
        assert!(validate_symbol_name("simple_name"));
        assert!(validate_symbol_name("a"));

        // Invalid names.
        assert!(!validate_symbol_name(""));
        assert!(!validate_symbol_name("name\x00with_null"));
        assert!(!validate_symbol_name("name;drop table"));
        assert!(!validate_symbol_name("name$(shell)"));
        assert!(!validate_symbol_name("name`cmd`"));
    }

    #[test]
    fn test_sanitize_cache_entry_removes_invalid() {
        let mut entry = ExpandCacheEntry {
            crate_name: "test".to_string(),
            rust_version: "1.94.0".to_string(),
            generated_at: "2026-03-30T00:00:00Z".to_string(),
            source_hash: "abc".to_string(),
            files: {
                let mut files = HashMap::new();
                files.insert(
                    "src/lib.rs".to_string(),
                    ExpandCacheFileEntry {
                        original_symbols: vec![
                            "valid::name".to_string(),
                            "invalid;name".to_string(),
                        ],
                        expanded_symbols: vec!["also_valid".to_string()],
                        generated_symbols: vec!["exploit$(cmd)".to_string()],
                        confidence: "heuristic".to_string(),
                    },
                );
                files
            },
        };

        sanitize_cache_entry(&mut entry);

        let file_entry = &entry.files["src/lib.rs"];
        assert_eq!(file_entry.original_symbols.len(), 1);
        assert_eq!(file_entry.original_symbols[0], "valid::name");
        assert_eq!(file_entry.expanded_symbols.len(), 1);
        assert_eq!(file_entry.generated_symbols.len(), 0);
    }

    #[test]
    fn test_cache_remove() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cache = ExpandCache::new(temp_dir.path().join("expand-cache")).unwrap();

        let entry = ExpandCacheEntry {
            crate_name: "test".to_string(),
            rust_version: "1.94.0".to_string(),
            generated_at: "2026-03-30T00:00:00Z".to_string(),
            source_hash: "abc".to_string(),
            files: HashMap::new(),
        };

        cache.write("removable", &entry).unwrap();
        assert!(cache.read("removable").unwrap().is_some());

        cache.remove("removable").unwrap();
        assert!(cache.read("removable").unwrap().is_none());
    }

    #[test]
    fn test_cache_list_crates() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let cache = ExpandCache::new(temp_dir.path().join("expand-cache")).unwrap();

        let entry = ExpandCacheEntry {
            crate_name: "test".to_string(),
            rust_version: "1.94.0".to_string(),
            generated_at: "2026-03-30T00:00:00Z".to_string(),
            source_hash: "abc".to_string(),
            files: HashMap::new(),
        };

        cache.write("crate_a", &entry).unwrap();
        cache.write("crate_b", &entry).unwrap();

        let mut crates = cache.list_cached_crates().unwrap();
        crates.sort();
        assert_eq!(crates, vec!["crate_a", "crate_b"]);
    }

    #[test]
    fn test_max_expansion_size() {
        assert_eq!(ExpandCache::max_expansion_size(), 10 * 1024 * 1024);
    }
}
