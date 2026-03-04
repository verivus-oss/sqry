//! File system watcher for real-time index updates.
//!
//! This module provides cross-platform file system monitoring using OS-level APIs:
//! - **Linux**: inotify (kernel-level, < 1ms latency)
//! - **macOS**: FSEvents (Apple's file system monitoring)
//! - **Windows**: ReadDirectoryChangesW
//!
//! The watcher detects file changes in real-time and enables "watch mode" for
//! automatic index updates during development.
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::watch::FileWatcher;
//! use std::path::Path;
//!
//! // Create watcher for a directory
//! let mut watcher = FileWatcher::new(Path::new("src"))?;
//!
//! // Poll for changes (non-blocking)
//! let changes = watcher.poll_changes();
//! for change in changes {
//!     match change {
//!         FileChange::Created(path) => println!("Created: {:?}", path),
//!         FileChange::Modified(path) => println!("Modified: {:?}", path),
//!         FileChange::Deleted(path) => println!("Deleted: {:?}", path),
//!     }
//! }
//!
//! // Wait for next change (blocking)
//! let changes = watcher.wait_for_change()?;
//! ```

use anyhow::{Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

/// Type of file system change
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    /// File was created
    Created(PathBuf),
    /// File was modified
    Modified(PathBuf),
    /// File was deleted
    Deleted(PathBuf),
}

/// Cross-platform file system watcher
///
/// Uses OS-level APIs for efficient real-time file monitoring:
/// - Linux: inotify
/// - macOS: `FSEvents`
/// - Windows: `ReadDirectoryChangesW`
pub struct FileWatcher {
    /// Underlying notify watcher
    _watcher: RecommendedWatcher,
    /// Channel for receiving file system events
    receiver: Receiver<Result<Event, notify::Error>>,
    /// Root path being watched
    root_path: PathBuf,
}

impl FileWatcher {
    /// Create a new file watcher for a directory
    ///
    /// The watcher monitors all files recursively under `root_path`.
    ///
    /// # Arguments
    ///
    /// * `root_path` - Root directory to watch
    ///
    /// # Errors
    ///
    /// Returns an error if the watcher cannot be created or the path cannot be watched.
    pub fn new(root_path: &Path) -> Result<Self> {
        let (tx, rx) = channel();

        let mut watcher = notify::recommended_watcher(move |res| {
            // Send event to channel (ignore send errors if receiver dropped)
            let _ = tx.send(res);
        })
        .context("Failed to create file system watcher")?;

        // Start watching the directory recursively
        watcher
            .watch(root_path, RecursiveMode::Recursive)
            .with_context(|| format!("Failed to watch directory: {}", root_path.display()))?;

        log::info!("File watcher started for: {}", root_path.display());

        Ok(Self {
            _watcher: watcher,
            receiver: rx,
            root_path: root_path.to_path_buf(),
        })
    }

    /// Poll for file changes (non-blocking)
    ///
    /// Returns all pending file system events without blocking.
    /// Use this for periodic polling in a loop.
    ///
    /// # Returns
    ///
    /// Vector of file changes (empty if no changes)
    #[must_use]
    pub fn poll_changes(&self) -> Vec<FileChange> {
        let mut changes = Vec::new();

        // Drain all pending events
        loop {
            match self.receiver.try_recv() {
                Ok(Ok(event)) => {
                    changes.extend(Self::process_event(event));
                }
                Ok(Err(e)) => {
                    log::warn!("File watcher error: {e}");
                }
                Err(TryRecvError::Empty) => {
                    // No more events
                    break;
                }
                Err(TryRecvError::Disconnected) => {
                    log::error!("File watcher channel disconnected");
                    break;
                }
            }
        }

        changes
    }

    /// Wait for next file change (blocking)
    ///
    /// Blocks until at least one file system event occurs.
    /// Use this for event-driven processing.
    ///
    /// # Returns
    ///
    /// Vector of file changes (at least one)
    ///
    /// # Errors
    ///
    /// Returns an error if the watcher channel is disconnected.
    pub fn wait_for_change(&self) -> Result<Vec<FileChange>> {
        // Wait for first event (blocking)
        let event = self
            .receiver
            .recv()
            .context("File watcher channel disconnected")?
            .context("File watcher error")?;

        let mut changes = Self::process_event(event);

        // Collect any additional pending events (non-blocking)
        changes.extend(self.poll_changes());

        Ok(changes)
    }

    /// Wait for next file change with debouncing
    ///
    /// Waits for a file system event, then waits for `debounce_duration`
    /// to collect any additional rapid-fire events (e.g., from editor saves).
    ///
    /// This is useful for editors that save files multiple times in quick succession.
    ///
    /// # Arguments
    ///
    /// * `debounce_duration` - How long to wait for additional events (typically 100-500ms)
    ///
    /// # Returns
    ///
    /// Vector of deduplicated file changes
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when the underlying watcher channel disconnects or emits an
    /// unrecoverable error while collecting events.
    pub fn wait_with_debounce(&self, debounce_duration: Duration) -> Result<Vec<FileChange>> {
        // Wait for first event
        let mut changes = self.wait_for_change()?;

        // Wait for debounce period while draining events
        changes.extend(self.wait_until(debounce_duration));

        // Deduplicate changes (keep last change per file)
        Ok(Self::deduplicate_changes(changes))
    }

    /// Wait for a duration while continuously draining events from the channel
    ///
    /// Unlike `std::thread::sleep()`, this actively drains the event channel,
    /// collecting all events that arrive during the wait period. This is crucial
    /// for macOS `FSEvents` which may deliver batched notifications.
    ///
    /// Returns all file changes collected during the wait period.
    ///
    /// Reference: `CI_FAILURE_REMEDIATION_PLAN.md` Section 2 (M-4)
    #[must_use]
    pub fn wait_until(&self, duration: Duration) -> Vec<FileChange> {
        let deadline = std::time::Instant::now() + duration;
        let mut changes = Vec::new();

        while std::time::Instant::now() < deadline {
            // Try to receive with a small timeout
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let poll_interval = Duration::from_millis(10).min(remaining);

            match self.receiver.recv_timeout(poll_interval) {
                Ok(Ok(event)) => {
                    changes.extend(Self::process_event(event));
                }
                Ok(Err(e)) => {
                    log::warn!("File watcher error: {e}");
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Continue waiting
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    log::error!("File watcher channel disconnected");
                    break;
                }
            }
        }

        changes
    }

    /// Get the root path being watched
    #[must_use]
    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// Process a notify event into file changes
    ///
    /// Filters out directory events to only track files, ensuring deterministic behavior
    /// across platforms (some report directory events, some don't).
    fn process_event(event: Event) -> Vec<FileChange> {
        let mut changes = Vec::new();

        match event.kind {
            EventKind::Create(_) => {
                for path in event.paths {
                    // Filter out directory events - only track files
                    if path.is_file() {
                        log::debug!("File created: {}", path.display());
                        changes.push(FileChange::Created(path));
                    } else {
                        log::trace!("Ignoring directory creation: {}", path.display());
                    }
                }
            }
            EventKind::Modify(_) => {
                for path in event.paths {
                    // Filter out directory events - only track files
                    if path.is_file() {
                        log::debug!("File modified: {}", path.display());
                        changes.push(FileChange::Modified(path));
                    } else {
                        log::trace!("Ignoring directory modification: {}", path.display());
                    }
                }
            }
            EventKind::Remove(_) => {
                for path in event.paths {
                    // For deletions, path no longer exists, so we can't check is_file()
                    // We rely on the path extension or accept all Remove events
                    // (directories are rare to delete and won't hurt index)
                    log::debug!("File deleted: {}", path.display());
                    changes.push(FileChange::Deleted(path));
                }
            }
            _ => {
                // Ignore other event types (access, metadata, etc.)
            }
        }

        changes
    }

    /// Deduplicate file changes (keep last change per file)
    ///
    /// When a file is modified multiple times rapidly, we only care about the final state.
    fn deduplicate_changes(changes: Vec<FileChange>) -> Vec<FileChange> {
        use std::collections::HashMap;

        let mut map: HashMap<PathBuf, FileChange> = HashMap::new();

        for change in changes {
            let path = match &change {
                FileChange::Created(p) | FileChange::Modified(p) | FileChange::Deleted(p) => {
                    p.clone()
                }
            };

            map.insert(path, change);
        }

        map.into_values().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::thread;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    fn event_timeout() -> Duration {
        // CI environments need more generous timeouts due to resource constraints
        let base = if cfg!(target_os = "macos") {
            Duration::from_secs(3)
        } else {
            Duration::from_secs(2) // Increased from 1s for CI stability
        };

        // Double timeout in CI environment
        if std::env::var("CI").is_ok() {
            base * 2
        } else {
            base
        }
    }

    fn wait_for<F>(timeout: Duration, mut predicate: F) -> bool
    where
        F: FnMut() -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if predicate() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn test_watcher_creation() {
        let tmp_watch_workspace = TempDir::new().unwrap();
        let watcher = FileWatcher::new(tmp_watch_workspace.path());
        assert!(watcher.is_ok());
    }

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn test_watcher_detects_file_creation() {
        let tmp_watch_workspace = TempDir::new().unwrap();
        let watcher = FileWatcher::new(tmp_watch_workspace.path()).unwrap();

        // Create a file
        let file_path = tmp_watch_workspace.path().join("test.txt");
        fs::write(&file_path, "test content").unwrap();

        let detected = wait_for(event_timeout(), || {
            let changes = watcher.poll_changes();
            changes
                .iter()
                .any(|c| matches!(c, FileChange::Created(p) if p == &file_path))
        });

        assert!(detected, "Expected FileWatcher to detect file creation");
    }

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn test_watcher_detects_file_modification() {
        let tmp_watch_workspace = TempDir::new().unwrap();
        let file_path = tmp_watch_workspace.path().join("test.txt");

        // Create file before starting watcher
        fs::write(&file_path, "initial content").unwrap();

        let watcher = FileWatcher::new(tmp_watch_workspace.path()).unwrap();

        // Give watcher time to initialize
        thread::sleep(Duration::from_millis(50));

        // Modify the file
        fs::write(&file_path, "modified content").unwrap();

        let detected = wait_for(event_timeout(), || {
            let changes = watcher.poll_changes();
            changes
                .iter()
                .any(|c| matches!(c, FileChange::Modified(p) if p == &file_path))
        });

        assert!(detected, "Expected FileWatcher to detect file modification");
    }

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn test_watcher_detects_file_deletion() {
        let tmp_watch_workspace = TempDir::new().unwrap();
        let file_path = tmp_watch_workspace.path().join("test.txt");

        // Create file
        fs::write(&file_path, "test content").unwrap();

        let watcher = FileWatcher::new(tmp_watch_workspace.path()).unwrap();

        // Give watcher time to initialize
        thread::sleep(Duration::from_millis(50));

        // Delete the file
        fs::remove_file(&file_path).unwrap();

        let detected = wait_for(event_timeout(), || {
            let changes = watcher.poll_changes();
            changes
                .iter()
                .any(|c| matches!(c, FileChange::Deleted(p) if p == &file_path))
        });

        assert!(detected, "Expected FileWatcher to detect file deletion");
    }

    #[test]
    fn test_watcher_poll_returns_empty_when_no_changes() {
        let tmp_watch_workspace = TempDir::new().unwrap();
        let watcher = FileWatcher::new(tmp_watch_workspace.path()).unwrap();

        // Poll without making any changes
        let changes = watcher.poll_changes();

        // Should return empty vector
        assert!(changes.is_empty());
    }

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn test_watcher_ignores_directories() {
        let tmp_watch_workspace = TempDir::new().unwrap();
        let watcher = FileWatcher::new(tmp_watch_workspace.path()).unwrap();

        // Create a subdirectory
        let sub_dir = tmp_watch_workspace.path().join("subdir");
        fs::create_dir(&sub_dir).unwrap();

        // Give the watcher time
        thread::sleep(Duration::from_millis(100));

        // Poll for changes
        let changes = watcher.poll_changes();

        // Should not report directory creation (only files)
        // With the fix, process_event now filters out directory events using is_file()
        assert!(
            changes.is_empty(),
            "Watcher should not report directory creation events, found: {changes:?}"
        );

        // Also test that creating a file inside the directory IS detected
        let file_path = sub_dir.join("test.txt");
        fs::write(&file_path, "test").unwrap();

        let detected = wait_for(event_timeout(), || {
            let changes = watcher.poll_changes();
            changes
                .iter()
                .any(|c| matches!(c, FileChange::Created(p) if p == &file_path))
        });

        assert!(
            detected,
            "Expected watcher to detect file creation in subdirectory"
        );
    }

    #[test]
    fn test_deduplicate_changes() {
        let changes = vec![
            FileChange::Modified(PathBuf::from("file1.txt")),
            FileChange::Modified(PathBuf::from("file1.txt")), // duplicate
            FileChange::Created(PathBuf::from("file2.txt")),
            FileChange::Modified(PathBuf::from("file1.txt")), // another duplicate
        ];

        let deduped = FileWatcher::deduplicate_changes(changes);

        // Should have 2 unique files
        assert_eq!(deduped.len(), 2);

        // file1.txt should only appear once (last modification wins)
        assert_eq!(
            deduped
                .iter()
                .filter(|c| matches!(c, FileChange::Modified(p) if p == Path::new("file1.txt")))
                .count(),
            1
        );

        // file2.txt should appear once
        assert_eq!(
            deduped
                .iter()
                .filter(|c| matches!(c, FileChange::Created(p) if p == Path::new("file2.txt")))
                .count(),
            1
        );
    }
}
