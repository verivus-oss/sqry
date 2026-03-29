//! Uses storage - background event writing
//!
//! This module handles persisting use events to disk in a robust, concurrent-safe manner.
//!
//! # Storage Layout
//!
//! ```text
//! ~/.sqry/uses/
//! ├── events/
//! │   ├── 2025-12-13.jsonl     # Daily event log (append-only)
//! │   ├── 2025-12-12.jsonl
//! │   └── ...
//! ├── summaries/
//! │   ├── 2025-W50.json        # Weekly aggregated summary
//! │   └── ...
//! └── troubleshoot/
//!     └── 2025-12-13_143022.json  # Issue bundles
//! ```
//!
//! # Cross-Process Safety
//!
//! - File locking (via `fs2`) ensures safe concurrent writes
//! - Atomic writes (via temp file + rename) prevent corruption
//! - Corrupt JSONL lines are skipped during reads

use super::types::{StructuredError, UseEvent};
use chrono::Utc;
use crossbeam_channel::Receiver;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Background writer that receives events from the collector channel
/// and writes them to daily JSONL files.
pub struct UsesWriter {
    uses_dir: PathBuf,
}

impl UsesWriter {
    /// Create a new writer for the given uses directory
    #[must_use]
    pub fn new(uses_dir: PathBuf) -> Self {
        Self { uses_dir }
    }

    /// Run the writer loop, receiving events from the channel
    ///
    /// This method blocks and runs until the channel is disconnected.
    /// It should be called from a dedicated background thread.
    pub fn run(self, receiver: &Receiver<UseEvent>) {
        // Ensure events directory exists
        let events_dir = self.uses_dir.join("events");
        if let Err(e) = fs::create_dir_all(&events_dir) {
            log::warn!("Failed to create uses events directory: {e}");
            // Continue anyway - events will be dropped
        }

        // Process events until channel closes
        while let Ok(event) = receiver.recv() {
            if let Err(e) = Self::write_event(&events_dir, &event) {
                log::debug!("Failed to write use event: {e}");
                // Continue processing - don't let one failure stop the writer
            }
        }
    }

    /// Write a single event to the daily log file
    fn write_event(events_dir: &Path, event: &UseEvent) -> std::io::Result<()> {
        let date = event.timestamp.format("%Y-%m-%d");
        let file_path = events_dir.join(format!("{date}.jsonl"));

        // Open file for append with exclusive lock.
        // `.read(true)` is required on Windows: fs2::lock_exclusive uses
        // LockFileEx which needs read access on the file handle.
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&file_path)?;

        // Acquire exclusive lock for append
        file.lock_exclusive()?;

        let mut writer = BufWriter::new(&file);
        serde_json::to_writer(&mut writer, event)?;
        writeln!(writer)?;
        writer.flush()?;

        file.unlock()?;

        Ok(())
    }
}

/// Storage operations for reading and managing use event files
pub struct UsesStorage {
    uses_dir: PathBuf,
}

impl UsesStorage {
    /// Create a new storage instance for the given uses directory
    #[must_use]
    pub fn new(uses_dir: PathBuf) -> Self {
        Self { uses_dir }
    }

    /// Get the path to the events directory
    #[must_use]
    pub fn events_dir(&self) -> PathBuf {
        self.uses_dir.join("events")
    }

    /// Get the path to the summaries directory
    #[must_use]
    pub fn summaries_dir(&self) -> PathBuf {
        self.uses_dir.join("summaries")
    }

    /// Get the path to the troubleshoot directory
    #[must_use]
    pub fn troubleshoot_dir(&self) -> PathBuf {
        self.uses_dir.join("troubleshoot")
    }

    /// Get the path to the errors directory
    #[must_use]
    pub fn errors_dir(&self) -> PathBuf {
        self.uses_dir.join("errors")
    }

    /// Ensure all required directories exist
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation fails.
    pub fn ensure_directories(&self) -> std::io::Result<()> {
        fs::create_dir_all(self.events_dir())?;
        fs::create_dir_all(self.summaries_dir())?;
        fs::create_dir_all(self.troubleshoot_dir())?;
        fs::create_dir_all(self.errors_dir())?;
        Ok(())
    }

    /// Load events from a daily log file
    ///
    /// Skips malformed lines gracefully.
    ///
    /// # Arguments
    ///
    /// * `date` - Date string in "YYYY-MM-DD" format
    ///
    /// # Returns
    ///
    /// A tuple of (events, `skipped_count`) where `skipped_count` is the number
    /// of malformed lines that were skipped.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or read.
    pub fn load_events_for_date(&self, date: &str) -> std::io::Result<(Vec<UseEvent>, usize)> {
        let file_path = self.events_dir().join(format!("{date}.jsonl"));
        load_events_from_file(&file_path)
    }

    /// Load events for a date range
    ///
    /// Loads all events from files in the given date range (inclusive).
    ///
    /// # Arguments
    ///
    /// * `start_date` - Start date in "YYYY-MM-DD" format
    /// * `end_date` - End date in "YYYY-MM-DD" format
    ///
    /// # Returns
    ///
    /// A tuple of (events, `skipped_count`) containing all events from the range.
    ///
    /// # Errors
    ///
    /// Returns an error if any file cannot be read (missing files are skipped).
    pub fn load_events_for_range(
        &self,
        start_date: &str,
        end_date: &str,
    ) -> std::io::Result<(Vec<UseEvent>, usize)> {
        let events_dir = self.events_dir();
        let mut all_events = Vec::new();
        let mut total_skipped = 0;

        // List all JSONL files in the events directory
        if !events_dir.exists() {
            return Ok((all_events, total_skipped));
        }

        for entry in fs::read_dir(&events_dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(ext) = path.extension()
                && ext == "jsonl"
                && let Some(stem) = path.file_stem()
            {
                let date = stem.to_string_lossy();
                // Check if date is in range
                if date.as_ref() >= start_date && date.as_ref() <= end_date {
                    match load_events_from_file(&path) {
                        Ok((events, skipped)) => {
                            all_events.extend(events);
                            total_skipped += skipped;
                        }
                        Err(e) => {
                            log::debug!("Failed to load events from {}: {}", path.display(), e);
                        }
                    }
                }
            }
        }

        // Sort by timestamp
        all_events.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        Ok((all_events, total_skipped))
    }

    /// Load events for the last N days
    ///
    /// # Arguments
    ///
    /// * `days` - Number of days to look back
    ///
    /// # Returns
    ///
    /// A tuple of (events, `skipped_count`) containing recent events.
    ///
    /// # Errors
    ///
    /// Returns an error if any file cannot be read (missing files are skipped).
    pub fn load_recent_events(&self, days: u32) -> std::io::Result<(Vec<UseEvent>, usize)> {
        let now = Utc::now();
        let start = now - chrono::Duration::days(i64::from(days));
        let start_date = start.format("%Y-%m-%d").to_string();
        let end_date = now.format("%Y-%m-%d").to_string();

        self.load_events_for_range(&start_date, &end_date)
    }

    /// Delete event files older than the retention period
    ///
    /// # Arguments
    ///
    /// * `retain_days` - Number of days to retain
    ///
    /// # Returns
    ///
    /// The number of files deleted.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub fn prune_old_events(&self, retain_days: u32) -> std::io::Result<usize> {
        let events_dir = self.events_dir();
        let cutoff = Utc::now() - chrono::Duration::days(i64::from(retain_days));
        let cutoff_date = cutoff.format("%Y-%m-%d").to_string();
        let mut deleted = 0;

        if !events_dir.exists() {
            return Ok(0);
        }

        for entry in fs::read_dir(&events_dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(ext) = path.extension()
                && ext == "jsonl"
                && let Some(stem) = path.file_stem()
            {
                let date = stem.to_string_lossy();
                if date.as_ref() < cutoff_date.as_str() {
                    if let Err(e) = fs::remove_file(&path) {
                        log::debug!("Failed to delete old event file {}: {}", path.display(), e);
                    } else {
                        deleted += 1;
                    }
                }
            }
        }

        Ok(deleted)
    }

    /// Write data atomically to a file
    ///
    /// Uses a temp file + rename pattern to prevent corruption.
    ///
    /// # Arguments
    ///
    /// * `target` - The target file path
    /// * `content` - The content to write
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn atomic_write(target: &Path, content: &[u8]) -> std::io::Result<()> {
        use tempfile::NamedTempFile;

        // Create temp file in same directory as target (same filesystem for atomic rename)
        let parent = target.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;

        let mut temp = NamedTempFile::new_in(parent)?;
        temp.as_file_mut().write_all(content)?;
        temp.as_file().sync_all()?;

        // Atomic rename (persist consumes temp file)
        temp.persist(target)?;

        Ok(())
    }

    /// Write a summary file atomically
    ///
    /// # Arguments
    ///
    /// * `week` - ISO week string (e.g., "2025-W50")
    /// * `content` - JSON content to write
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn write_summary(&self, week: &str, content: &[u8]) -> std::io::Result<()> {
        let path = self.summaries_dir().join(format!("{week}.json"));
        Self::atomic_write(&path, content)
    }

    /// Read a summary file
    ///
    /// # Arguments
    ///
    /// * `week` - ISO week string (e.g., "2025-W50")
    ///
    /// # Returns
    ///
    /// The file content as bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub fn read_summary(&self, week: &str) -> std::io::Result<Vec<u8>> {
        let path = self.summaries_dir().join(format!("{week}.json"));
        fs::read(path)
    }

    /// Check if a summary file exists for a given week
    #[must_use]
    pub fn summary_exists(&self, week: &str) -> bool {
        self.summaries_dir().join(format!("{week}.json")).exists()
    }

    /// Load errors for the last N days
    ///
    /// Loads error records from daily JSONL files for troubleshooting.
    ///
    /// # Arguments
    ///
    /// * `days` - Number of days to look back
    ///
    /// # Returns
    ///
    /// A tuple of (errors, `skipped_count`) containing recent errors.
    ///
    /// # Errors
    ///
    /// Returns an error if any file cannot be read (missing files are skipped).
    pub fn load_recent_errors(&self, days: u32) -> std::io::Result<(Vec<StructuredError>, usize)> {
        let errors_dir = self.errors_dir();
        let mut all_errors = Vec::new();
        let mut total_skipped = 0;

        if !errors_dir.exists() {
            return Ok((all_errors, total_skipped));
        }

        let now = Utc::now();
        let cutoff = now - chrono::Duration::days(i64::from(days));
        let cutoff_date = cutoff.format("%Y-%m-%d").to_string();
        let end_date = now.format("%Y-%m-%d").to_string();

        for entry in fs::read_dir(&errors_dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(ext) = path.extension()
                && ext == "jsonl"
                && let Some(stem) = path.file_stem()
            {
                let date = stem.to_string_lossy();
                // Check if date is in range
                if date.as_ref() >= cutoff_date.as_str() && date.as_ref() <= end_date.as_str() {
                    match load_errors_from_file(&path) {
                        Ok((errors, skipped)) => {
                            all_errors.extend(errors);
                            total_skipped += skipped;
                        }
                        Err(e) => {
                            log::debug!("Failed to load errors from {}: {}", path.display(), e);
                        }
                    }
                }
            }
        }

        // Sort by timestamp (most recent first)
        all_errors.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        Ok((all_errors, total_skipped))
    }

    /// Record an error to the daily error log
    ///
    /// Writes error records in append-only JSONL format for troubleshooting.
    ///
    /// # Arguments
    ///
    /// * `error` - The structured error to record
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn record_error(&self, error: &StructuredError) -> std::io::Result<()> {
        let errors_dir = self.errors_dir();
        fs::create_dir_all(&errors_dir)?;

        let date = error.timestamp.format("%Y-%m-%d");
        let file_path = errors_dir.join(format!("{date}.jsonl"));

        // Open file for append with exclusive lock.
        // `.read(true)` is required on Windows: fs2::lock_exclusive uses
        // LockFileEx which needs read access on the file handle.
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&file_path)?;

        // Acquire exclusive lock for append
        file.lock_exclusive()?;

        let mut writer = BufWriter::new(&file);
        serde_json::to_writer(&mut writer, error)?;
        writeln!(writer)?;
        writer.flush()?;

        file.unlock()?;

        Ok(())
    }
}

/// Load events from a JSONL file, skipping malformed lines
fn load_events_from_file(path: &Path) -> std::io::Result<(Vec<UseEvent>, usize)> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut skipped = 0;

    for line in reader.lines() {
        match line {
            Ok(line_content) => {
                if line_content.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str(&line_content) {
                    Ok(event) => events.push(event),
                    Err(_) => skipped += 1,
                }
            }
            Err(_) => skipped += 1,
        }
    }

    Ok((events, skipped))
}

/// Load errors from a JSONL file, skipping malformed lines
fn load_errors_from_file(path: &Path) -> std::io::Result<(Vec<StructuredError>, usize)> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut errors = Vec::new();
    let mut skipped = 0;

    for line in reader.lines() {
        match line {
            Ok(line_content) => {
                if line_content.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str(&line_content) {
                    Ok(error) => errors.push(error),
                    Err(_) => skipped += 1,
                }
            }
            Err(_) => skipped += 1,
        }
    }

    Ok((errors, skipped))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uses::types::{QueryKind, UseEventType};
    use tempfile::tempdir;

    #[test]
    fn test_storage_ensure_directories() {
        let dir = tempdir().unwrap();
        let storage = UsesStorage::new(dir.path().join("uses"));

        storage.ensure_directories().unwrap();

        assert!(storage.events_dir().exists());
        assert!(storage.summaries_dir().exists());
        assert!(storage.troubleshoot_dir().exists());
        assert!(storage.errors_dir().exists());
    }

    #[test]
    fn test_writer_creates_daily_file() {
        let dir = tempdir().unwrap();
        let storage = UsesStorage::new(dir.path().join("uses"));
        storage.ensure_directories().unwrap();

        let events_dir = dir.path().join("uses").join("events");

        let event = UseEvent::new(UseEventType::QueryExecuted {
            kind: QueryKind::CallChain,
            result_count: 42,
        });

        UsesWriter::write_event(&events_dir, &event).unwrap();

        let today = Utc::now().format("%Y-%m-%d").to_string();
        let file = events_dir.join(format!("{today}.jsonl"));
        assert!(file.exists());
    }

    #[test]
    fn test_load_events_from_file() {
        let dir = tempdir().unwrap();
        let storage = UsesStorage::new(dir.path().join("uses"));
        storage.ensure_directories().unwrap();

        // Write some events
        let events_dir = storage.events_dir();
        let file_path = events_dir.join("2025-12-13.jsonl");

        let event = UseEvent::new(UseEventType::QueryExecuted {
            kind: QueryKind::CallChain,
            result_count: 42,
        });

        // Write event to file
        let mut file = File::create(&file_path).unwrap();
        serde_json::to_writer(&mut file, &event).unwrap();
        writeln!(file).unwrap();

        // Read it back
        let (events, skipped) = load_events_from_file(&file_path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn test_load_events_skips_malformed() {
        let dir = tempdir().unwrap();
        let storage = UsesStorage::new(dir.path().join("uses"));
        storage.ensure_directories().unwrap();

        // Write mix of valid and invalid lines
        let events_dir = storage.events_dir();
        let file_path = events_dir.join("2025-12-13.jsonl");

        let event = UseEvent::new(UseEventType::QueryExecuted {
            kind: QueryKind::CallChain,
            result_count: 42,
        });

        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "invalid json").unwrap();
        serde_json::to_writer(&mut file, &event).unwrap();
        writeln!(file).unwrap();
        writeln!(file, "another invalid").unwrap();

        let (events, skipped) = load_events_from_file(&file_path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn test_atomic_write() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("test.json");

        UsesStorage::atomic_write(&target, b"{\"test\": true}").unwrap();

        let content = fs::read_to_string(&target).unwrap();
        assert_eq!(content, "{\"test\": true}");
    }

    #[test]
    fn test_summary_read_write() {
        let dir = tempdir().unwrap();
        let storage = UsesStorage::new(dir.path().join("uses"));
        storage.ensure_directories().unwrap();

        let content = b"{\"period\": \"2025-W50\"}";
        storage.write_summary("2025-W50", content).unwrap();

        assert!(storage.summary_exists("2025-W50"));
        assert!(!storage.summary_exists("2025-W51"));

        let read_content = storage.read_summary("2025-W50").unwrap();
        assert_eq!(read_content, content);
    }

    #[test]
    fn test_load_recent_events() {
        let dir = tempdir().unwrap();
        let storage = UsesStorage::new(dir.path().join("uses"));
        storage.ensure_directories().unwrap();

        // Write an event for today
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let file_path = storage.events_dir().join(format!("{today}.jsonl"));

        let event = UseEvent::new(UseEventType::QueryExecuted {
            kind: QueryKind::CallChain,
            result_count: 42,
        });

        let mut file = File::create(&file_path).unwrap();
        serde_json::to_writer(&mut file, &event).unwrap();
        writeln!(file).unwrap();

        // Load recent events
        let (events, skipped) = storage.load_recent_events(1).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn test_prune_old_events() {
        let dir = tempdir().unwrap();
        let storage = UsesStorage::new(dir.path().join("uses"));
        storage.ensure_directories().unwrap();

        // Create files with old dates
        let old_date = "2020-01-01";
        let recent_date = Utc::now().format("%Y-%m-%d").to_string();

        let old_file = storage.events_dir().join(format!("{old_date}.jsonl"));
        let recent_file = storage.events_dir().join(format!("{recent_date}.jsonl"));

        File::create(&old_file).unwrap();
        File::create(&recent_file).unwrap();

        // Prune events older than 365 days
        let deleted = storage.prune_old_events(365).unwrap();

        assert_eq!(deleted, 1);
        assert!(!old_file.exists());
        assert!(recent_file.exists());
    }
}
