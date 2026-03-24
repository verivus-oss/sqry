// ! Cache pruning engine for managing cache lifecycle.
//!
//! This module provides the core logic for cache pruning operations,
//! allowing users to reclaim disk space by removing old or excessive entries.
//!
//! # Features
//!
//! - **Time-based retention**: Remove entries older than N days
//! - **Size-based retention**: Cap cache to maximum size (oldest-first eviction)
//! - **Dry-run mode**: Preview deletions without modifying cache
//! - **Detailed reporting**: Summary of removed/remaining entries and bytes
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::cache::{CacheManager, PruneOptions};
//! use std::time::Duration;
//!
//! let cache = CacheManager::default();
//! let options = PruneOptions::new()
//!     .with_max_age(Duration::from_secs(7 * 24 * 3600)) // 7 days
//!     .with_dry_run(true);
//!
//! let report = cache.prune(&options)?;
//! println!("Would remove {} entries ({} bytes)",
//!          report.entries_removed, report.bytes_removed);
//! ```

use anyhow::Result;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;

/// Options for cache pruning operations.
#[derive(Debug, Clone)]
pub struct PruneOptions {
    /// Maximum age for cache entries (older entries will be removed)
    pub max_age: Option<Duration>,
    /// Maximum total cache size in bytes (oldest entries removed first)
    pub max_size: Option<u64>,
    /// Preview mode - don't actually delete files
    pub dry_run: bool,
    /// Output format for results
    pub output_mode: PruneOutputMode,
    /// Target cache directory (defaults to user cache dir)
    pub target_dir: Option<PathBuf>,
}

impl PruneOptions {
    /// Create new prune options with defaults
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_age: None,
            max_size: None,
            dry_run: false,
            output_mode: PruneOutputMode::Human,
            target_dir: None,
        }
    }

    /// Set maximum age for cache entries
    #[must_use]
    pub fn with_max_age(mut self, age: Duration) -> Self {
        self.max_age = Some(age);
        self
    }

    /// Set maximum cache size in bytes
    #[must_use]
    pub fn with_max_size(mut self, size: u64) -> Self {
        self.max_size = Some(size);
        self
    }

    /// Enable dry-run mode (no actual deletions)
    #[must_use]
    pub fn with_dry_run(mut self, enabled: bool) -> Self {
        self.dry_run = enabled;
        self
    }

    /// Set output mode
    #[must_use]
    pub fn with_output_mode(mut self, mode: PruneOutputMode) -> Self {
        self.output_mode = mode;
        self
    }

    /// Set target directory
    #[must_use]
    pub fn with_target_dir(mut self, dir: PathBuf) -> Self {
        self.target_dir = Some(dir);
        self
    }

    /// Validate options
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when no retention policy is specified.
    pub fn validate(&self) -> Result<()> {
        if self.max_age.is_none() && self.max_size.is_none() {
            anyhow::bail!("At least one retention policy must be specified (--days or --size)");
        }
        Ok(())
    }
}

impl Default for PruneOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Output format for prune results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneOutputMode {
    /// Human-readable text output
    Human,
    /// Machine-readable JSON output
    Json,
}

/// Report summarizing a prune operation.
#[derive(Debug, Clone, Serialize)]
pub struct PruneReport {
    /// Total number of entries examined
    pub entries_considered: usize,
    /// Number of entries removed (or would be removed in dry-run)
    pub entries_removed: usize,
    /// Bytes reclaimed (or would be reclaimed in dry-run)
    pub bytes_removed: u64,
    /// Number of entries remaining after prune
    pub remaining_entries: usize,
    /// Bytes remaining after prune
    pub remaining_bytes: u64,
    /// Individual operations performed
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<PruneOperation>,
}

impl PruneReport {
    /// Create a new empty report
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries_considered: 0,
            entries_removed: 0,
            bytes_removed: 0,
            remaining_entries: 0,
            remaining_bytes: 0,
            operations: Vec::new(),
        }
    }
}

impl Default for PruneReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Details of a single prune operation.
#[derive(Debug, Clone, Serialize)]
pub struct PruneOperation {
    /// Path to the cache entry
    pub path: PathBuf,
    /// Size of the entry in bytes
    pub size_bytes: u64,
    /// Last modified time
    pub last_modified: SystemTime,
    /// Reason for pruning this entry
    pub reason: PruneReason,
    /// Whether this was a dry-run operation
    pub dry_run: bool,
}

/// Reason why an entry was pruned.
#[derive(Debug, Clone, Serialize)]
pub enum PruneReason {
    /// Entry older than max age threshold
    OlderThan(#[serde(with = "duration_serde")] Duration),
    /// Entry removed to enforce size cap
    ExceedsSizeCap {
        /// Maximum cache size in bytes
        cap: u64,
    },
}

/// Serde module for Duration serialization
mod duration_serde {
    use serde::{Serialize, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        duration.as_secs().serialize(serializer)
    }
}

/// Internal representation of a cache entry for pruning.
#[derive(Debug, Clone)]
struct CacheEntry {
    /// Path to the .bin file
    data_path: PathBuf,
    /// Path to the .bin.lock file (if it exists)
    lock_path: Option<PathBuf>,
    /// Total size in bytes (data + lock)
    size_bytes: u64,
    /// Last modified timestamp
    last_modified: SystemTime,
}

impl CacheEntry {
    /// Total size including lock file
    fn total_size(&self) -> u64 {
        self.size_bytes
    }
}

/// Engine for performing cache prune operations.
pub struct PruneEngine {
    options: PruneOptions,
}

impl PruneEngine {
    /// Create a new prune engine with the given options
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when option validation fails.
    pub fn new(options: PruneOptions) -> Result<Self> {
        options.validate()?;
        Ok(Self { options })
    }

    /// Execute the prune operation and return a report
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when determining a prune reason fails.
    pub fn execute(&self, cache_dir: &Path) -> Result<PruneReport> {
        if !cache_dir.exists() {
            return Ok(PruneReport::new());
        }

        // Collect all cache entries
        let entries = Self::collect_entries(cache_dir);

        if entries.is_empty() {
            return Ok(PruneReport::new());
        }

        let to_remove = self.select_entries_to_remove(&entries);
        let mut report = Self::initialize_report(&entries, &to_remove);

        self.perform_pruning(&entries, &to_remove, &mut report)?;

        Ok(report)
    }

    fn initialize_report(
        entries: &[CacheEntry],
        to_remove: &std::collections::HashSet<PathBuf>,
    ) -> PruneReport {
        let mut report = PruneReport::new();
        report.entries_considered = entries.len();

        // Calculate statistics
        for entry in entries {
            if to_remove.contains(&entry.data_path) {
                report.entries_removed += 1;
                report.bytes_removed += entry.total_size();
            } else {
                report.remaining_entries += 1;
                report.remaining_bytes += entry.total_size();
            }
        }
        report
    }

    fn perform_pruning(
        &self,
        entries: &[CacheEntry],
        to_remove: &std::collections::HashSet<PathBuf>,
        report: &mut PruneReport,
    ) -> Result<()> {
        // Perform deletions (unless dry-run)
        for entry in entries {
            if to_remove.contains(&entry.data_path) {
                let reason = self.determine_reason(entry)?;

                if !self.options.dry_run {
                    Self::delete_entry(entry);
                }

                report.operations.push(PruneOperation {
                    path: entry.data_path.clone(),
                    size_bytes: entry.total_size(),
                    last_modified: entry.last_modified,
                    reason,
                    dry_run: self.options.dry_run,
                });
            }
        }
        Ok(())
    }

    /// Collect all cache entries from the cache directory
    fn collect_entries(cache_dir: &Path) -> Vec<CacheEntry> {
        let mut entries = Vec::new();

        for entry in WalkDir::new(cache_dir)
            .follow_links(false)
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            if let Some(cache_entry) = Self::process_dir_entry(entry.path()) {
                entries.push(cache_entry);
            }
        }

        entries
    }

    fn process_dir_entry(path: &Path) -> Option<CacheEntry> {
        // Only process .bin files (not .bin.lock)
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "bin") {
            return None;
        }

        // Skip if this is a .bin.lock file
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".bin.lock"))
        {
            return None;
        }

        // Get metadata
        let metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                log::warn!("Failed to read metadata for {}: {e}", path.display());
                return None;
            }
        };

        // Note: We use .ok() here instead of context() because we are in an Option context
        let last_modified = metadata.modified().ok()?;

        // Check for corresponding lock file
        let mut lock_path_buf = path.to_path_buf();
        lock_path_buf.set_extension("bin.lock");
        let lock_path = if lock_path_buf.exists() {
            Some(lock_path_buf)
        } else {
            None
        };

        // Calculate total size (data + lock)
        let lock_size = if let Some(ref lp) = lock_path {
            fs::metadata(lp).map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };

        Some(CacheEntry {
            data_path: path.to_path_buf(),
            lock_path,
            size_bytes: metadata.len() + lock_size,
            last_modified,
        })
    }

    /// Select which entries should be removed based on policies
    fn select_entries_to_remove(
        &self,
        entries: &[CacheEntry],
    ) -> std::collections::HashSet<PathBuf> {
        let mut to_remove = std::collections::HashSet::new();
        let now = SystemTime::now();

        self.apply_age_policy(entries, now, &mut to_remove);
        self.apply_size_policy(entries, &mut to_remove);

        to_remove
    }

    fn apply_age_policy(
        &self,
        entries: &[CacheEntry],
        now: SystemTime,
        to_remove: &mut std::collections::HashSet<PathBuf>,
    ) {
        if let Some(max_age) = self.options.max_age {
            let cutoff = now - max_age;
            for entry in entries {
                if entry.last_modified < cutoff {
                    to_remove.insert(entry.data_path.clone());
                }
            }
        }
    }

    fn apply_size_policy(
        &self,
        entries: &[CacheEntry],
        to_remove: &mut std::collections::HashSet<PathBuf>,
    ) {
        let Some(max_size) = self.options.max_size else {
            return;
        };

        let mut remaining = collect_remaining_entries(entries, to_remove);
        remaining.sort_by_key(|e| e.last_modified);

        let current_size = remaining_total_size(&remaining);
        if current_size > max_size {
            mark_entries_for_size_limit(&remaining, max_size, current_size, to_remove);
        }
    }

    /// Determine the reason for removing an entry
    fn determine_reason(&self, entry: &CacheEntry) -> Result<PruneReason> {
        let now = SystemTime::now();

        // Check age policy first
        if let Some(max_age) = self.options.max_age {
            let cutoff = now - max_age;
            if entry.last_modified < cutoff {
                return Ok(PruneReason::OlderThan(max_age));
            }
        }

        // Otherwise it must be size policy
        if let Some(max_size) = self.options.max_size {
            return Ok(PruneReason::ExceedsSizeCap { cap: max_size });
        }

        // Should not reach here due to validation
        anyhow::bail!("No valid prune reason found for entry");
    }

    /// Delete a cache entry and its associated lock file
    fn delete_entry(entry: &CacheEntry) {
        // Delete data file
        if let Err(e) = fs::remove_file(&entry.data_path) {
            log::warn!("Failed to delete {}: {}", entry.data_path.display(), e);
        }

        // Delete lock file if it exists
        if let Some(ref lock_path) = entry.lock_path
            && let Err(e) = fs::remove_file(lock_path)
        {
            log::warn!("Failed to delete lock file {}: {e}", lock_path.display());
        }
    }
}

fn collect_remaining_entries<'a>(
    entries: &'a [CacheEntry],
    to_remove: &std::collections::HashSet<PathBuf>,
) -> Vec<&'a CacheEntry> {
    entries
        .iter()
        .filter(|entry| !to_remove.contains(&entry.data_path))
        .collect()
}

fn remaining_total_size(remaining: &[&CacheEntry]) -> u64 {
    remaining.iter().map(|entry| entry.total_size()).sum()
}

fn mark_entries_for_size_limit(
    remaining: &[&CacheEntry],
    max_size: u64,
    current_size: u64,
    to_remove: &mut std::collections::HashSet<PathBuf>,
) {
    let mut cumulative_size = current_size;
    for entry in remaining {
        if cumulative_size <= max_size {
            break;
        }
        to_remove.insert(entry.data_path.clone());
        cumulative_size -= entry.total_size();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_entry(dir: &Path, name: &str, size: u64, age_days: u64) -> PathBuf {
        let path = dir.join(name);
        // Test file sizes may exceed usize::MAX on 32-bit systems; clamp to max
        let size_usize = size.try_into().unwrap_or(usize::MAX);
        let content = vec![0u8; size_usize];
        fs::write(&path, content).unwrap();

        // Set modification time
        let mtime = SystemTime::now() - Duration::from_secs(age_days * 24 * 3600);
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(mtime)).unwrap();

        path
    }

    #[test]
    fn test_prune_options_validation() {
        let opts = PruneOptions::new();
        assert!(
            opts.validate().is_err(),
            "Should require at least one policy"
        );

        let opts_age = PruneOptions::new().with_max_age(Duration::from_secs(86400));
        assert!(opts_age.validate().is_ok());

        let opts_size = PruneOptions::new().with_max_size(1024 * 1024);
        assert!(opts_size.validate().is_ok());
    }

    #[test]
    fn test_age_policy_filters_correctly() {
        let tmp_cache_dir = TempDir::new().unwrap();

        // Create entries of different ages
        create_test_entry(tmp_cache_dir.path(), "old.bin", 100, 10); // 10 days old
        create_test_entry(tmp_cache_dir.path(), "recent.bin", 100, 2); // 2 days old

        let opts = PruneOptions::new()
            .with_max_age(Duration::from_secs(7 * 24 * 3600)) // 7 days
            .with_dry_run(true);

        let engine = PruneEngine::new(opts).unwrap();
        let report = engine.execute(tmp_cache_dir.path()).unwrap();

        assert_eq!(report.entries_considered, 2);
        assert_eq!(report.entries_removed, 1);
        assert_eq!(report.remaining_entries, 1);
    }

    #[test]
    fn test_dry_run_no_deletions() {
        let tmp_cache_dir = TempDir::new().unwrap();
        create_test_entry(tmp_cache_dir.path(), "old.bin", 100, 10);

        let opts = PruneOptions::new()
            .with_max_age(Duration::from_secs(7 * 24 * 3600))
            .with_dry_run(true);

        let engine = PruneEngine::new(opts).unwrap();
        let _report = engine.execute(tmp_cache_dir.path()).unwrap();

        // File should still exist
        assert!(tmp_cache_dir.path().join("old.bin").exists());
    }

    #[test]
    fn test_size_policy_culls_oldest_first() {
        let tmp_cache_dir = TempDir::new().unwrap();

        // Create 3 entries: oldest, middle, newest
        create_test_entry(tmp_cache_dir.path(), "oldest.bin", 100, 10);
        create_test_entry(tmp_cache_dir.path(), "middle.bin", 100, 5);
        create_test_entry(tmp_cache_dir.path(), "newest.bin", 100, 1);

        // Set size cap to allow only 2 entries
        let opts = PruneOptions::new().with_max_size(200).with_dry_run(true);

        let engine = PruneEngine::new(opts).unwrap();
        let report = engine.execute(tmp_cache_dir.path()).unwrap();

        assert_eq!(report.entries_considered, 3);
        assert_eq!(report.entries_removed, 1);
        assert_eq!(report.remaining_entries, 2);
        assert!(report.remaining_bytes <= 200);
    }
}
