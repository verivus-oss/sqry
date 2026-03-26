//! Preview extraction for showing code context around symbol matches
//!
//! This module provides `PreviewExtractor` for extracting lines of source code
//! context around matched symbols. It includes LRU caching for file contents
//! and path validation for security.

use anyhow::{Context, Result};
use lru::LruCache;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

/// Maximum file size for preview extraction (1MB)
const MAX_PREVIEW_FILE_SIZE: u64 = 1_048_576;

/// Default LRU cache capacity (number of files)
const DEFAULT_CACHE_CAPACITY: usize = 10;

/// Errors that can occur during path validation
enum PathValidationError {
    /// File does not exist
    NotFound(PathBuf),
    /// Path is outside workspace root
    OutsideWorkspace,
    /// Other error during validation
    Other(String),
}

impl std::fmt::Display for PathValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(path) => write!(f, "file not found: {}", path.display()),
            Self::OutsideWorkspace => write!(f, "outside workspace root"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

/// Configuration for preview/context display
#[derive(Debug, Clone)]
pub struct PreviewConfig {
    /// Number of context lines before match
    pub lines_before: usize,
    /// Number of context lines after match
    pub lines_after: usize,
    /// Maximum line length before truncation
    pub max_line_length: usize,
    /// Truncation indicator
    pub truncation_marker: &'static str,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self {
            lines_before: 3,
            lines_after: 3,
            max_line_length: 120,
            truncation_marker: "...",
        }
    }
}

impl PreviewConfig {
    /// Create a new preview config with specified context lines
    #[must_use]
    pub fn new(lines: usize) -> Self {
        Self {
            lines_before: lines,
            lines_after: lines,
            ..Default::default()
        }
    }

    /// Create preview config showing only matched line (no context)
    #[must_use]
    pub fn no_context() -> Self {
        Self {
            lines_before: 0,
            lines_after: 0,
            ..Default::default()
        }
    }
}

/// A single line with its line number
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NumberedLine {
    /// 1-indexed line number
    pub line_number: usize,
    /// Line content (may be truncated)
    pub content: String,
}

/// Extracted context lines for a symbol
#[derive(Debug, Clone, Serialize)]
pub struct ContextLines {
    /// Lines before the match (in order, oldest first)
    pub before: Vec<NumberedLine>,
    /// The matched line itself
    pub matched: NumberedLine,
    /// Lines after the match (in order)
    pub after: Vec<NumberedLine>,
    /// Error message if extraction failed (None on success)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ContextLines {
    /// Create context lines from extracted content
    #[must_use]
    pub fn new(before: Vec<NumberedLine>, matched: NumberedLine, after: Vec<NumberedLine>) -> Self {
        Self {
            before,
            matched,
            after,
            error: None,
        }
    }

    /// Create an error context (for files that can't be read)
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            before: Vec::new(),
            matched: NumberedLine {
                line_number: 0,
                content: String::new(),
            },
            after: Vec::new(),
            error: Some(message.into()),
        }
    }

    /// Check if this is an error result
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }

    /// Get the error message if any
    #[must_use]
    pub fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Format context as a single preview string (for CSV/TSV)
    #[must_use]
    pub fn to_preview_string(&self, max_length: usize) -> String {
        if let Some(err) = &self.error {
            return err.clone();
        }

        let mut lines: Vec<String> = Vec::new();

        for line in &self.before {
            lines.push(format!("  {} | {}", line.line_number, line.content));
        }

        lines.push(format!(
            "> {} | {}",
            self.matched.line_number, self.matched.content
        ));

        for line in &self.after {
            lines.push(format!("  {} | {}", line.line_number, line.content));
        }

        let result = lines.join("\n");
        if result.len() > max_length {
            format!("{}...", &result[..max_length.saturating_sub(3)])
        } else {
            result
        }
    }
}

/// Grouped context for multiple adjacent matches in same file
#[derive(Debug, Clone, Serialize)]
pub struct GroupedContext {
    /// File path
    pub file: PathBuf,
    /// Continuous line range start
    pub start_line: usize,
    /// Continuous line range end
    pub end_line: usize,
    /// All lines in range with match markers
    pub lines: Vec<GroupedLine>,
    /// Error message if grouping failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A line within a grouped context
#[derive(Debug, Clone, Serialize)]
pub struct GroupedLine {
    /// 1-indexed line number
    pub line_number: usize,
    /// Line content
    pub content: String,
    /// True if this line is a matched symbol location
    pub is_match: bool,
}

impl GroupedContext {
    pub fn error(file: PathBuf, message: impl Into<String>) -> Self {
        Self {
            file,
            start_line: 0,
            end_line: 0,
            lines: Vec::new(),
            error: Some(message.into()),
        }
    }
}

/// Match location for grouping
#[derive(Debug, Clone)]
pub struct MatchLocation {
    /// File path
    pub file: PathBuf,
    /// 1-indexed line number
    pub line: usize,
}

/// Extracts code context around symbol locations
#[allow(dead_code)]
pub struct PreviewExtractor {
    config: PreviewConfig,
    /// Cache of recently read files (LRU, max capacity files)
    file_cache: LruCache<PathBuf, Vec<String>>,
    /// Workspace root for path validation
    workspace_root: PathBuf,
}

impl PreviewExtractor {
    /// Create new extractor with config and workspace root
    ///
    /// # Panics
    /// Panics if `DEFAULT_CACHE_CAPACITY` is zero.
    #[must_use]
    pub fn new(config: PreviewConfig, workspace_root: PathBuf) -> Self {
        let capacity = NonZeroUsize::new(DEFAULT_CACHE_CAPACITY)
            .expect("DEFAULT_CACHE_CAPACITY must be non-zero");
        Self {
            config,
            file_cache: LruCache::new(capacity),
            workspace_root,
        }
    }

    /// Create new extractor with custom cache capacity
    #[allow(dead_code)]
    ///
    /// # Panics
    /// Panics if `capacity` is zero after clamping.
    #[must_use]
    pub fn with_capacity(config: PreviewConfig, workspace_root: PathBuf, capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity.max(1)).expect("capacity must be non-zero");
        Self {
            config,
            file_cache: LruCache::new(capacity),
            workspace_root,
        }
    }

    /// Extract context for a symbol at the given file and line
    ///
    /// # Arguments
    /// * `file` - Path to the source file
    /// * `line` - 1-indexed line number of the match
    ///
    /// # Returns
    /// * `Ok(ContextLines)` - Extracted context (may contain error message)
    ///
    /// # Errors
    /// Returns an error if the file cannot be read.
    pub fn extract(&mut self, file: &Path, line: usize) -> Result<ContextLines> {
        // Validate path is within workspace
        let canonical_path = match self.validate_path(file) {
            Ok(p) => p,
            Err(PathValidationError::NotFound(path)) => {
                return Ok(ContextLines::error(format!(
                    "[file not found: {}]",
                    path.display()
                )));
            }
            Err(PathValidationError::OutsideWorkspace) => {
                return Ok(ContextLines::error(
                    "[access denied: outside workspace root]",
                ));
            }
            Err(PathValidationError::Other(msg)) => {
                return Ok(ContextLines::error(format!("[access denied: {msg}]")));
            }
        };

        // Check file size before reading
        let metadata = match fs::metadata(&canonical_path) {
            Ok(m) => m,
            Err(e) => {
                return Ok(ContextLines::error(format!("[file error: {e}]")));
            }
        };

        if metadata.len() > MAX_PREVIEW_FILE_SIZE {
            return Ok(ContextLines::error("[file too large for preview]"));
        }

        // Read file lines (from cache or disk)
        let lines = self.get_file_lines(&canonical_path)?;

        // Extract context
        Ok(self.extract_context(&lines, line))
    }

    /// Extract context for multiple matches, grouping by file for efficiency
    ///
    /// Returns context for each match in the same order as input.
    #[allow(dead_code)]
    pub fn extract_batch(&mut self, matches: &[MatchLocation]) -> Vec<Result<ContextLines>> {
        matches
            .iter()
            .map(|m| self.extract(&m.file, m.line))
            .collect()
    }

    /// Extract grouped context for multiple adjacent matches in same file
    ///
    /// Merges overlapping context windows into continuous ranges.
    pub fn extract_grouped(&mut self, matches: &[MatchLocation]) -> Vec<GroupedContext> {
        // Group matches by file
        let mut by_file: HashMap<&Path, Vec<usize>> = HashMap::new();
        for m in matches {
            by_file.entry(m.file.as_path()).or_default().push(m.line);
        }

        let mut result = Vec::new();

        for (file, mut lines) in by_file {
            // Sort lines
            lines.sort_unstable();
            lines.dedup();

            // Validate and read file
            let canonical_path = match self.validate_path(file) {
                Ok(p) => p,
                Err(PathValidationError::NotFound(path)) => {
                    result.push(GroupedContext::error(
                        file.to_path_buf(),
                        format!("[file not found: {}]", path.display()),
                    ));
                    continue;
                }
                Err(PathValidationError::OutsideWorkspace) => {
                    result.push(GroupedContext::error(
                        file.to_path_buf(),
                        "[access denied: outside workspace root]",
                    ));
                    continue;
                }
                Err(PathValidationError::Other(msg)) => {
                    result.push(GroupedContext::error(
                        file.to_path_buf(),
                        format!("[access denied: {msg}]"),
                    ));
                    continue;
                }
            };

            let metadata = match fs::metadata(&canonical_path) {
                Ok(m) => m,
                Err(e) => {
                    result.push(GroupedContext::error(
                        file.to_path_buf(),
                        format!("[file error: {e}]"),
                    ));
                    continue;
                }
            };

            if metadata.len() > MAX_PREVIEW_FILE_SIZE {
                result.push(GroupedContext::error(
                    file.to_path_buf(),
                    "[file too large for preview]",
                ));
                continue;
            }

            let file_lines = match self.get_file_lines(&canonical_path) {
                Ok(l) => l,
                Err(e) => {
                    result.push(GroupedContext::error(
                        file.to_path_buf(),
                        format!("[file error: {e}]"),
                    ));
                    continue;
                }
            };

            // Merge overlapping ranges
            let groups = self.merge_adjacent_ranges(&lines);

            for (start, end, match_lines) in groups {
                let grouped = self.create_grouped_context(
                    file.to_path_buf(),
                    &file_lines,
                    start,
                    end,
                    &match_lines,
                );
                result.push(grouped);
            }
        }

        result
    }

    /// Validate and canonicalize path, ensuring it's within workspace root
    fn validate_path(&self, path: &Path) -> Result<PathBuf, PathValidationError> {
        // Check if file exists first (to give proper error message)
        if !path.exists() {
            return Err(PathValidationError::NotFound(path.to_path_buf()));
        }

        let canonical = path
            .canonicalize()
            .with_context(|| format!("Cannot resolve path: {}", path.display()))
            .map_err(|e| PathValidationError::Other(e.to_string()))?;

        let workspace_canonical = self
            .workspace_root
            .canonicalize()
            .with_context(|| {
                format!(
                    "Cannot resolve workspace: {}",
                    self.workspace_root.display()
                )
            })
            .map_err(|e| PathValidationError::Other(e.to_string()))?;

        if !canonical.starts_with(&workspace_canonical) {
            return Err(PathValidationError::OutsideWorkspace);
        }

        Ok(canonical)
    }

    /// Get file lines from cache or read from disk
    fn get_file_lines(&mut self, path: &PathBuf) -> Result<Vec<String>> {
        // Check cache first
        if let Some(lines) = self.file_cache.get(path) {
            return Ok(lines.clone());
        }

        // Read file with lossy UTF-8 conversion
        let content =
            fs::read(path).with_context(|| format!("Failed to read file: {}", path.display()))?;
        let content = String::from_utf8_lossy(&content);

        let lines: Vec<String> = content.lines().map(String::from).collect();

        // Cache the lines
        self.file_cache.put(path.clone(), lines.clone());

        Ok(lines)
    }

    /// Extract context lines around the given line number
    fn extract_context(&self, lines: &[String], line: usize) -> ContextLines {
        if line == 0 || line > lines.len() {
            return ContextLines::error(format!(
                "[line {} out of bounds (file has {} lines)]",
                line,
                lines.len()
            ));
        }

        let line_idx = line - 1; // Convert to 0-indexed

        // Calculate range for before lines
        let before_start = line_idx.saturating_sub(self.config.lines_before);
        let before: Vec<NumberedLine> = (before_start..line_idx)
            .map(|i| NumberedLine {
                line_number: i + 1,
                content: self.truncate_line(&lines[i]),
            })
            .collect();

        // Matched line
        let matched = NumberedLine {
            line_number: line,
            content: self.truncate_line(&lines[line_idx]),
        };

        // Calculate range for after lines
        let after_end = (line_idx + 1 + self.config.lines_after).min(lines.len());
        let after: Vec<NumberedLine> = ((line_idx + 1)..after_end)
            .map(|i| NumberedLine {
                line_number: i + 1,
                content: self.truncate_line(&lines[i]),
            })
            .collect();

        ContextLines::new(before, matched, after)
    }

    /// Truncate a line if it exceeds max length
    fn truncate_line(&self, line: &str) -> String {
        if line.len() > self.config.max_line_length {
            format!(
                "{}{}",
                &line[..self.config.max_line_length - self.config.truncation_marker.len()],
                self.config.truncation_marker
            )
        } else {
            line.to_string()
        }
    }

    /// Merge adjacent line ranges based on context overlap
    ///
    /// Returns vector of (`start_line`, `end_line`, `matched_lines`)
    fn merge_adjacent_ranges(&self, lines: &[usize]) -> Vec<(usize, usize, Vec<usize>)> {
        if lines.is_empty() {
            return Vec::new();
        }

        let mut groups: Vec<(usize, usize, Vec<usize>)> = Vec::new();
        let context = self.config.lines_before.max(self.config.lines_after);

        for &line in lines {
            let start = line.saturating_sub(self.config.lines_before);
            let end = line + self.config.lines_after;

            if let Some(last) = groups.last_mut() {
                // Check if this range overlaps or touches the previous one
                // Adjacent if: last_end + 1 >= start (context windows touch or overlap)
                if last.1 + 1 >= start || last.1 + context >= line.saturating_sub(context) {
                    // Merge: extend the end and add the match line
                    last.1 = last.1.max(end);
                    last.2.push(line);
                    continue;
                }
            }

            // Start new group
            groups.push((start, end, vec![line]));
        }

        groups
    }

    /// Create a grouped context from merged ranges
    fn create_grouped_context(
        &self,
        file: PathBuf,
        file_lines: &[String],
        start: usize,
        end: usize,
        match_lines: &[usize],
    ) -> GroupedContext {
        let start = start.max(1);
        let end = end.min(file_lines.len());

        let lines: Vec<GroupedLine> = (start..=end)
            .map(|line_num| {
                let idx = line_num - 1;
                let content = if idx < file_lines.len() {
                    self.truncate_line(&file_lines[idx])
                } else {
                    String::new()
                };
                GroupedLine {
                    line_number: line_num,
                    content,
                    is_match: match_lines.contains(&line_num),
                }
            })
            .collect();

        GroupedContext {
            file,
            start_line: start,
            end_line: end,
            lines,
            error: None,
        }
    }

    /// Clear the file cache
    #[allow(dead_code)]
    pub fn clear_cache(&mut self) {
        self.file_cache.clear();
    }

    /// Get current cache size
    #[allow(dead_code)]
    #[must_use]
    pub fn cache_size(&self) -> usize {
        self.file_cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_file(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        let mut file = File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_preview_extract_basic() {
        let tmp = TempDir::new().unwrap();
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\n";
        let file = create_test_file(&tmp, "test.rs", content);

        let config = PreviewConfig::new(2);
        let mut extractor = PreviewExtractor::new(config, tmp.path().to_path_buf());

        let ctx = extractor.extract(&file, 4).unwrap();
        assert!(!ctx.is_error());
        assert_eq!(ctx.matched.line_number, 4);
        assert_eq!(ctx.matched.content, "line 4");
        assert_eq!(ctx.before.len(), 2);
        assert_eq!(ctx.after.len(), 2);
    }

    #[test]
    fn test_preview_extract_at_file_start() {
        let tmp = TempDir::new().unwrap();
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let file = create_test_file(&tmp, "test.rs", content);

        let config = PreviewConfig::new(3);
        let mut extractor = PreviewExtractor::new(config, tmp.path().to_path_buf());

        let ctx = extractor.extract(&file, 2).unwrap();
        assert!(!ctx.is_error());
        assert_eq!(ctx.matched.line_number, 2);
        assert_eq!(ctx.before.len(), 1); // Only line 1 before
        assert_eq!(ctx.after.len(), 3);
    }

    #[test]
    fn test_preview_extract_at_file_end() {
        let tmp = TempDir::new().unwrap();
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let file = create_test_file(&tmp, "test.rs", content);

        let config = PreviewConfig::new(3);
        let mut extractor = PreviewExtractor::new(config, tmp.path().to_path_buf());

        let ctx = extractor.extract(&file, 4).unwrap();
        assert!(!ctx.is_error());
        assert_eq!(ctx.matched.line_number, 4);
        assert_eq!(ctx.before.len(), 3);
        assert_eq!(ctx.after.len(), 1); // Only line 5 after
    }

    #[test]
    fn test_preview_truncate_long_line() {
        let tmp = TempDir::new().unwrap();
        let long_line = "a".repeat(200);
        let content = format!("short\n{}\nshort", long_line);
        let file = create_test_file(&tmp, "test.rs", &content);

        let mut config = PreviewConfig::new(1);
        config.max_line_length = 50;
        let mut extractor = PreviewExtractor::new(config, tmp.path().to_path_buf());

        let ctx = extractor.extract(&file, 2).unwrap();
        assert!(!ctx.is_error());
        assert!(ctx.matched.content.len() <= 50);
        assert!(ctx.matched.content.ends_with("..."));
    }

    #[test]
    fn test_preview_missing_file() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("nonexistent.rs");

        let config = PreviewConfig::new(3);
        let mut extractor = PreviewExtractor::new(config, tmp.path().to_path_buf());

        let ctx = extractor.extract(&file, 1).unwrap();
        assert!(ctx.is_error());
        assert!(ctx.error_message().unwrap().contains("file not found"));
    }

    #[test]
    fn test_preview_line_out_of_bounds() {
        let tmp = TempDir::new().unwrap();
        let content = "line 1\nline 2\nline 3\n";
        let file = create_test_file(&tmp, "test.rs", content);

        let config = PreviewConfig::new(3);
        let mut extractor = PreviewExtractor::new(config, tmp.path().to_path_buf());

        let ctx = extractor.extract(&file, 100).unwrap();
        assert!(ctx.is_error());
        assert!(ctx.error_message().unwrap().contains("out of bounds"));
    }

    #[test]
    fn test_preview_lru_cache_hit() {
        let tmp = TempDir::new().unwrap();
        let content = "line 1\nline 2\nline 3\n";
        let file = create_test_file(&tmp, "test.rs", content);

        let config = PreviewConfig::new(1);
        let mut extractor = PreviewExtractor::new(config, tmp.path().to_path_buf());

        // First read
        let _ = extractor.extract(&file, 1).unwrap();
        assert_eq!(extractor.cache_size(), 1);

        // Second read (should hit cache)
        let _ = extractor.extract(&file, 2).unwrap();
        assert_eq!(extractor.cache_size(), 1); // Still 1 file cached
    }

    #[test]
    fn test_preview_adjacent_matches_merged() {
        let tmp = TempDir::new().unwrap();
        let content = (1..=20)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let file = create_test_file(&tmp, "test.rs", &content);

        let config = PreviewConfig::new(2);
        let mut extractor = PreviewExtractor::new(config, tmp.path().to_path_buf());

        // Lines 5 and 7 are adjacent with 2-line context
        let matches = vec![
            MatchLocation {
                file: file.clone(),
                line: 5,
            },
            MatchLocation {
                file: file.clone(),
                line: 7,
            },
        ];

        let groups = extractor.extract_grouped(&matches);
        assert_eq!(groups.len(), 1); // Should be merged into one group
        assert!(groups[0].lines.iter().filter(|l| l.is_match).count() == 2);
    }

    #[test]
    fn test_preview_non_adjacent_separate() {
        let tmp = TempDir::new().unwrap();
        let content = (1..=30)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let file = create_test_file(&tmp, "test.rs", &content);

        let config = PreviewConfig::new(2);
        let mut extractor = PreviewExtractor::new(config, tmp.path().to_path_buf());

        // Lines 5 and 25 are far apart
        let matches = vec![
            MatchLocation {
                file: file.clone(),
                line: 5,
            },
            MatchLocation {
                file: file.clone(),
                line: 25,
            },
        ];

        let groups = extractor.extract_grouped(&matches);
        assert_eq!(groups.len(), 2); // Should be separate groups
    }

    #[test]
    fn test_preview_no_context() {
        let tmp = TempDir::new().unwrap();
        let content = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let file = create_test_file(&tmp, "test.rs", content);

        let config = PreviewConfig::no_context();
        let mut extractor = PreviewExtractor::new(config, tmp.path().to_path_buf());

        let ctx = extractor.extract(&file, 3).unwrap();
        assert!(!ctx.is_error());
        assert_eq!(ctx.matched.line_number, 3);
        assert_eq!(ctx.before.len(), 0);
        assert_eq!(ctx.after.len(), 0);
    }

    #[test]
    fn test_context_lines_to_preview_string() {
        let ctx = ContextLines::new(
            vec![NumberedLine {
                line_number: 1,
                content: "before".to_string(),
            }],
            NumberedLine {
                line_number: 2,
                content: "match".to_string(),
            },
            vec![NumberedLine {
                line_number: 3,
                content: "after".to_string(),
            }],
        );

        let preview = ctx.to_preview_string(1000);
        assert!(preview.contains("> 2 | match"));
        assert!(preview.contains("  1 | before"));
        assert!(preview.contains("  3 | after"));
    }
}
