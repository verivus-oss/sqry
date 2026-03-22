//! Recency scoring for hybrid search based on git commit timestamps
//!
//! This module provides repository-relative recency scoring for Stage 3 hybrid search.
//! Scores are normalized to [0.0, 1.0] where 1.0 = newest file, 0.0 = oldest file.
//!
//! # Design Principles
//!
//! - **Deterministic**: Same repo state → same scores (no wall-clock dependency)
//! - **Relative scoring**: Normalized against repo's own history
//! - **Local-only**: Uses local git history (no network operations, always safe in offline mode)
//! - **Graceful fallback**: Returns neutral 0.5 when git unavailable
//!
//! # Example
//!
//! ```no_run
//! use sqry_core::git::recency::RecencyIndex;
//! use std::path::Path;
//!
//! let repo = Path::new("/path/to/repo");
//! let index = RecencyIndex::from_repo(repo)?;
//!
//! let score = index.score_for_file(Path::new("src/main.rs"));
//! println!("Recency score: {score}"); // 0.0 (oldest) to 1.0 (newest)
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use super::{GitBackend, GitError, Result, SubprocessGit};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Recency index that normalizes file timestamps relative to repository history
///
/// This index builds a mapping of file paths to their last commit timestamps,
/// then normalizes scores to [0.0, 1.0] based on the repository's min/max timestamps.
///
/// # Scoring Formula
///
/// ```text
/// score = (timestamp - min_ts) / (max_ts - min_ts)
/// ```
///
/// - **1.0**: Newest file in the repository
/// - **0.5**: Mid-point between oldest and newest (or neutral fallback)
/// - **0.0**: Oldest file in the repository
///
/// # Thread Safety
///
/// This struct is Send + Sync and can be shared across threads.
#[derive(Debug, Clone)]
pub struct RecencyIndex {
    /// Map of file paths to Unix epoch timestamps (seconds)
    by_file: HashMap<PathBuf, i64>,

    /// Minimum timestamp across all tracked files
    min_ts: i64,

    /// Maximum timestamp across all tracked files
    max_ts: i64,

    /// Repository root path (canonicalized)
    repo_root: PathBuf,
}

impl RecencyIndex {
    #[inline]
    #[allow(clippy::cast_precision_loss)] // Timestamp ranges are bounded; lossy f32 cast is acceptable for scoring ratios
    fn to_f32_lossy(value: i64) -> f32 {
        value as f32
    }

    /// Build a recency index from a git repository
    ///
    /// Walks all tracked files in the repository and records their last commit timestamps.
    ///
    /// # Arguments
    ///
    /// * `root` - Path to repository root (or any directory within the repo)
    ///
    /// # Returns
    ///
    /// * `Ok(RecencyIndex)` - Successfully built index
    /// * `Err(GitError::NotARepo)` - Path is not a git repository
    /// * `Err(GitError::NotFound)` - Git binary not in PATH
    /// * `Err(GitError)` - Other git command failures
    ///
    /// # Performance
    ///
    /// This operation is relatively expensive (O(n) where n = tracked files).
    /// Consider caching the index and rebuilding only when the repository changes.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use sqry_core::git::recency::RecencyIndex;
    /// # use std::path::Path;
    /// let index = RecencyIndex::from_repo(Path::new("/path/to/repo"))?;
    /// println!("Indexed {} files", index.file_count());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `GitError` when repository discovery or git commands fail,
    /// or when the underlying git output is malformed.
    pub fn from_repo(root: &Path) -> Result<Self> {
        let backend = SubprocessGit::new();

        // Get canonicalized repo root
        let repo_root = backend.repo_root(root)?;

        // Get list of all tracked files
        let tracked_files = Self::get_tracked_files(&repo_root)?;

        if tracked_files.is_empty() {
            // Empty repository (no commits or no tracked files)
            return Ok(Self {
                by_file: HashMap::new(),
                min_ts: 0,
                max_ts: 0,
                repo_root,
            });
        }

        // Build timestamp map
        let mut by_file = HashMap::new();
        let mut min_ts = i64::MAX;
        let mut max_ts = i64::MIN;

        for file_path in tracked_files {
            if let Some(timestamp) = Self::get_file_timestamp(&repo_root, &file_path)? {
                min_ts = min_ts.min(timestamp);
                max_ts = max_ts.max(timestamp);
                by_file.insert(file_path, timestamp);
            }
        }

        // Handle edge case: all files have same timestamp
        if min_ts == max_ts {
            log::debug!(
                "RecencyIndex: All files have identical timestamps ({min_ts}), scores will be neutral 0.5"
            );
        }

        Ok(Self {
            by_file,
            min_ts,
            max_ts,
            repo_root,
        })
    }

    /// Create a recency index from explicit timestamp data (for testing)
    ///
    /// This constructor allows creating an index without accessing git,
    /// useful for deterministic unit tests.
    ///
    /// # Arguments
    ///
    /// * `by_file` - Map of file paths to Unix epoch timestamps
    /// * `repo_root` - Repository root path (used for path resolution)
    ///
    /// # Panics
    ///
    /// Panics if `by_file` is empty (use an empty `HashMap` to represent
    /// an empty repository, which will result in neutral 0.5 scores).
    ///
    /// # Examples
    ///
    /// ```
    /// # use sqry_core::git::recency::RecencyIndex;
    /// # use std::collections::HashMap;
    /// # use std::path::{Path, PathBuf};
    /// let timestamps = HashMap::from([
    ///     (PathBuf::from("old.rs"), 1000),
    ///     (PathBuf::from("mid.rs"), 2000),
    ///     (PathBuf::from("new.rs"), 3000),
    /// ]);
    /// let index = RecencyIndex::from_timestamps(timestamps, Path::new("/repo"));
    /// assert!(index.score_for_file(Path::new("new.rs")) > index.score_for_file(Path::new("old.rs")));
    /// ```
    #[must_use]
    pub fn from_timestamps(by_file: HashMap<PathBuf, i64>, repo_root: &Path) -> Self {
        if by_file.is_empty() {
            return Self {
                by_file,
                min_ts: 0,
                max_ts: 0,
                repo_root: repo_root.to_path_buf(),
            };
        }

        let min_ts = *by_file.values().min().expect("by_file is not empty");
        let max_ts = *by_file.values().max().expect("by_file is not empty");

        Self {
            by_file,
            min_ts,
            max_ts,
            repo_root: repo_root.to_path_buf(),
        }
    }

    /// Compute recency score for a file
    ///
    /// Returns a normalized score in [0.0, 1.0] where:
    /// - **1.0**: Newest file in repository
    /// - **0.5**: Neutral (file not in index, or all files have same timestamp)
    /// - **0.0**: Oldest file in repository
    ///
    /// # Arguments
    ///
    /// * `path` - File path (absolute or relative to repo root)
    ///
    /// # Returns
    ///
    /// Normalized recency score (0.0-1.0)
    ///
    /// # Fallback Behavior
    ///
    /// Returns 0.5 (neutral) when:
    /// - File not found in index
    /// - All files have identical timestamps (`min_ts` == `max_ts`)
    /// - Empty repository (no tracked files)
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use sqry_core::git::recency::RecencyIndex;
    /// # use std::path::Path;
    /// let index = RecencyIndex::from_repo(Path::new("/repo"))?;
    ///
    /// // Absolute path
    /// let score = index.score_for_file(Path::new("/repo/src/main.rs"));
    ///
    /// // Relative path (resolved against repo root)
    /// let score = index.score_for_file(Path::new("src/main.rs"));
    ///
    /// // File not in index → neutral 0.5
    /// let score = index.score_for_file(Path::new("not_tracked.txt"));
    /// assert_eq!(score, 0.5);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn score_for_file(&self, path: &Path) -> f32 {
        // Handle empty repository
        if self.by_file.is_empty() {
            return 0.5;
        }

        // Try both absolute and relative paths
        let timestamp = self
            .by_file
            .get(path)
            .or_else(|| {
                // Try making path relative to repo root
                if path.is_absolute() {
                    path.strip_prefix(&self.repo_root)
                        .ok()
                        .and_then(|rel| self.by_file.get(rel))
                } else {
                    None
                }
            })
            .or_else(|| {
                // Try making path absolute
                if path.is_relative() {
                    let abs = self.repo_root.join(path);
                    self.by_file.get(&abs)
                } else {
                    None
                }
            });

        let Some(&ts) = timestamp else {
            return 0.5;
        };

        if self.max_ts == self.min_ts {
            0.5
        } else {
            let score = Self::to_f32_lossy(ts - self.min_ts)
                / Self::to_f32_lossy(self.max_ts - self.min_ts);
            score.clamp(0.0, 1.0)
        }
    }

    /// Get the number of files tracked in this index
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.by_file.len()
    }

    /// Get the repository root path
    #[must_use]
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Get the timestamp range (min, max) in Unix epoch seconds
    ///
    /// Returns `None` for empty repositories.
    #[must_use]
    pub fn timestamp_range(&self) -> Option<(i64, i64)> {
        if self.by_file.is_empty() {
            None
        } else {
            Some((self.min_ts, self.max_ts))
        }
    }

    /// Get list of all tracked files in repository
    ///
    /// Uses `git ls-files` to enumerate tracked files.
    ///
    /// # Security
    ///
    /// - Uses `SubprocessGit`'s `execute_git` (enforces output limits and timeouts)
    /// - Uses null-terminated output (-z) to handle special characters in filenames
    /// - Respects .gitignore and git configuration
    /// - No shell invocation (command array arguments)
    fn get_tracked_files(repo_root: &Path) -> Result<Vec<PathBuf>> {
        // Use SubprocessGit's execute_git for safety (output limits, timeouts)
        let stdout = SubprocessGit::execute_git(
            &["-C", &repo_root.display().to_string(), "ls-files", "-z"],
            None, // Use default timeout
        )?;

        // Parse null-terminated output
        let files: Vec<PathBuf> = stdout
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();

        Ok(files)
    }

    /// Get last commit timestamp for a file
    ///
    /// Uses `git log -1 --format=%ct -- <file>` to get the committer timestamp.
    ///
    /// # Security
    ///
    /// - Uses `SubprocessGit`'s `execute_git` (enforces output limits and timeouts)
    /// - No shell invocation (command array arguments)
    ///
    /// # Returns
    ///
    /// * `Ok(Some(timestamp))` - File has commit history
    /// * `Ok(None)` - File is tracked but has no commits (newly added)
    /// * `Err(GitError)` - Git command failed
    fn get_file_timestamp(repo_root: &Path, file_path: &Path) -> Result<Option<i64>> {
        // Convert paths to strings (must bind to variables for lifetime)
        let repo_root_str = repo_root.display().to_string();
        let file_path_str = file_path.display().to_string();

        // Build args
        let args = vec![
            "-C",
            &repo_root_str,
            "log",
            "-1",
            "--format=%ct",
            "--",
            &file_path_str,
        ];

        // Use SubprocessGit's execute_git for safety (output limits, timeouts)
        let stdout = SubprocessGit::execute_git(&args, None)?;

        // Empty output means file has no commits yet (newly added)
        if stdout.trim().is_empty() {
            return Ok(None);
        }

        // Parse timestamp
        let timestamp: i64 = stdout.trim().parse().map_err(|e| {
            GitError::InvalidOutput(format!(
                "Failed to parse timestamp '{}' for {}: {e}",
                stdout.trim(),
                file_path.display()
            ))
        })?;

        Ok(Some(timestamp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    const SCORE_EPSILON: f32 = 1.0e-6;

    fn assert_score_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < SCORE_EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    /// Helper to create a git repo with explicit timestamps
    ///
    /// Creates files and commits them with controlled timestamps for deterministic testing.
    fn create_test_repo_with_timestamps() -> (TempDir, Vec<(&'static str, i64)>) {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path();

        // Initialize git repo
        let init = Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("git init failed");
        assert!(init.status.success());

        // Configure git
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "commit.gpgSign", "false"])
            .current_dir(path)
            .output()
            .unwrap();

        // Create files with different timestamps
        let files = vec![
            ("old.rs", 1000i64), // Oldest
            ("mid.rs", 2000i64), // Middle
            ("new.rs", 3000i64), // Newest
        ];

        for (filename, timestamp) in &files {
            // Create file
            fs::write(path.join(filename), format!("// {filename}")).unwrap();

            // Stage file
            Command::new("git")
                .args(["add", filename])
                .current_dir(path)
                .output()
                .unwrap();

            // Commit with explicit timestamp
            let commit = Command::new("git")
                .env("GIT_COMMITTER_DATE", timestamp.to_string())
                .env("GIT_AUTHOR_DATE", timestamp.to_string())
                .args(["commit", "-m", &format!("Add {filename}")])
                .current_dir(path)
                .output()
                .unwrap();
            assert!(
                commit.status.success(),
                "commit failed for {filename}: {commit:?}"
            );
        }

        (tmpdir, files)
    }

    #[test]
    fn test_from_timestamps_normalization() {
        let timestamps = HashMap::from([
            (PathBuf::from("old.rs"), 1000),
            (PathBuf::from("mid.rs"), 2000),
            (PathBuf::from("new.rs"), 3000),
        ]);

        let index = RecencyIndex::from_timestamps(timestamps, Path::new("/repo"));

        // Check normalization
        assert_score_close(index.score_for_file(Path::new("old.rs")), 0.0); // Oldest = 0.0
        assert_score_close(index.score_for_file(Path::new("mid.rs")), 0.5); // Middle = 0.5
        assert_score_close(index.score_for_file(Path::new("new.rs")), 1.0); // Newest = 1.0
    }

    #[test]
    fn test_from_timestamps_ordering() {
        let timestamps = HashMap::from([
            (PathBuf::from("old.rs"), 1000),
            (PathBuf::from("mid.rs"), 2000),
            (PathBuf::from("new.rs"), 3000),
        ]);

        let index = RecencyIndex::from_timestamps(timestamps, Path::new("/repo"));

        // Verify ordering
        let old_score = index.score_for_file(Path::new("old.rs"));
        let mid_score = index.score_for_file(Path::new("mid.rs"));
        let new_score = index.score_for_file(Path::new("new.rs"));

        assert!(new_score > mid_score);
        assert!(mid_score > old_score);
    }

    #[test]
    fn test_from_timestamps_missing_file() {
        let timestamps = HashMap::from([
            (PathBuf::from("old.rs"), 1000),
            (PathBuf::from("new.rs"), 3000),
        ]);

        let index = RecencyIndex::from_timestamps(timestamps, Path::new("/repo"));

        // Missing file returns neutral 0.5
        assert_score_close(index.score_for_file(Path::new("missing.rs")), 0.5);
    }

    #[test]
    fn test_from_timestamps_identical_timestamps() {
        let timestamps = HashMap::from([
            (PathBuf::from("a.rs"), 1000),
            (PathBuf::from("b.rs"), 1000),
            (PathBuf::from("c.rs"), 1000),
        ]);

        let index = RecencyIndex::from_timestamps(timestamps, Path::new("/repo"));

        // All files have same timestamp → neutral 0.5
        assert_score_close(index.score_for_file(Path::new("a.rs")), 0.5);
        assert_score_close(index.score_for_file(Path::new("b.rs")), 0.5);
        assert_score_close(index.score_for_file(Path::new("c.rs")), 0.5);
    }

    #[test]
    fn test_from_timestamps_empty() {
        let timestamps = HashMap::new();
        let index = RecencyIndex::from_timestamps(timestamps, Path::new("/repo"));

        // Empty repository → neutral 0.5
        assert_score_close(index.score_for_file(Path::new("any.rs")), 0.5);
        assert_eq!(index.file_count(), 0);
    }

    #[test]
    #[ignore = "Requires git binary and filesystem access"]
    fn test_from_repo_real_git() {
        let (tmpdir, _files) = create_test_repo_with_timestamps();
        let index = RecencyIndex::from_repo(tmpdir.path()).unwrap();

        assert_eq!(index.file_count(), 3);

        // Verify score ordering (newer files score higher)
        let old_score = index.score_for_file(Path::new("old.rs"));
        let mid_score = index.score_for_file(Path::new("mid.rs"));
        let new_score = index.score_for_file(Path::new("new.rs"));

        assert!(
            new_score > mid_score,
            "new ({new_score}) should be > mid ({mid_score})"
        );
        assert!(
            mid_score > old_score,
            "mid ({mid_score}) should be > old ({old_score})"
        );

        // Newest should be close to 1.0, oldest close to 0.0
        assert!(
            new_score > 0.9,
            "newest file should score > 0.9, got {new_score}"
        );
        assert!(
            old_score < 0.1,
            "oldest file should score < 0.1, got {old_score}"
        );
    }

    #[test]
    #[ignore = "Requires git binary and filesystem access"]
    fn test_from_repo_absolute_and_relative_paths() {
        let (tmpdir, _files) = create_test_repo_with_timestamps();
        let index = RecencyIndex::from_repo(tmpdir.path()).unwrap();

        // Relative path
        let rel_score = index.score_for_file(Path::new("new.rs"));

        // Absolute path
        let abs_path = tmpdir.path().join("new.rs");
        let abs_score = index.score_for_file(&abs_path);

        // Should be identical
        assert_score_close(rel_score, abs_score);
    }

    #[test]
    fn test_repo_root_accessor() {
        let timestamps = HashMap::from([(PathBuf::from("test.rs"), 1000)]);
        let index = RecencyIndex::from_timestamps(timestamps, Path::new("/test/repo"));

        assert_eq!(index.repo_root(), Path::new("/test/repo"));
    }

    #[test]
    fn test_timestamp_range() {
        let timestamps = HashMap::from([
            (PathBuf::from("old.rs"), 1000),
            (PathBuf::from("new.rs"), 5000),
        ]);

        let index = RecencyIndex::from_timestamps(timestamps, Path::new("/repo"));
        assert_eq!(index.timestamp_range(), Some((1000, 5000)));

        // Empty index
        let empty = RecencyIndex::from_timestamps(HashMap::new(), Path::new("/repo"));
        assert_eq!(empty.timestamp_range(), None);
    }

    #[test]
    fn test_file_count() {
        let timestamps = HashMap::from([
            (PathBuf::from("a.rs"), 1000),
            (PathBuf::from("b.rs"), 2000),
            (PathBuf::from("c.rs"), 3000),
        ]);

        let index = RecencyIndex::from_timestamps(timestamps, Path::new("/repo"));
        assert_eq!(index.file_count(), 3);
    }
}
