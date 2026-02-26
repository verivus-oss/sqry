use dashmap::DashMap;
use ropey::Rope;
use std::env;
use std::path::Path;
use std::sync::Arc;
use tower_lsp::lsp_types::{Position, TextDocumentContentChangeEvent, Url};

use crate::config::DocumentLimits;
use crate::file_types::classify_file;

/// Reason why a document was rejected/skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// File is a binary type (PDF, image, etc.)
    BinaryFile,
    /// File exceeds the size limit for its category
    ExceedsLimit,
}

impl SkipReason {
    /// Returns a human-readable message for the skip reason.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::BinaryFile => "binary file type not supported",
            Self::ExceedsLimit => "file exceeds size limit",
        }
    }
}

#[derive(Clone)]
pub struct DocumentStore {
    inner: Arc<DashMap<Url, DocumentEntry>>,
    /// Files that were intentionally skipped (binary or too large).
    /// This prevents fallback disk reads from failing with confusing errors.
    skipped: Arc<DashMap<Url, SkipReason>>,
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            skipped: Arc::new(DashMap::new()),
        }
    }
}

#[derive(Debug, Clone)]
struct DocumentEntry {
    language_id: Option<String>,
    version: i32,
    rope: Rope,
    line_offsets: Vec<usize>,
}

impl DocumentEntry {
    fn new(language_id: Option<String>, version: i32, text: &str) -> Self {
        let rope = Rope::from(text);
        let line_offsets = compute_line_offsets(&rope);
        Self {
            language_id,
            version,
            rope,
            line_offsets,
        }
    }

    fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }

    fn text(&self) -> String {
        self.rope.to_string()
    }

    fn replace_text(&mut self, text: &str) {
        self.rope = Rope::from(text);
        self.line_offsets = compute_line_offsets(&self.rope);
    }
}

pub(crate) fn compute_line_offsets(rope: &Rope) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(rope.len_lines());
    let mut byte_index = 0usize;
    for line_idx in 0..rope.len_lines() {
        offsets.push(byte_index);
        byte_index += rope.line(line_idx).len_bytes();
    }
    offsets
}

impl DocumentStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a document for tracking. Returns `Ok(())` if stored, `Err(reason)` if skipped.
    ///
    /// Documents are skipped if:
    /// - They are binary files (PDF, images, etc.) - not a size issue
    /// - They exceed the size limit for their file category
    ///
    /// # Errors
    ///
    /// Returns `Err(SkipReason)` when the document is binary or exceeds size limits.
    pub fn open(
        &self,
        path: &Path,
        language_id: Option<String>,
        version: i32,
        text: &str,
        limits: &DocumentLimits,
    ) -> Result<(), SkipReason> {
        let Some(uri) = path_to_url(path) else {
            log::warn!(
                "failed to convert document path '{}' into file:// URL",
                path.display()
            );
            return Ok(()); // Not a skip, just a conversion failure
        };

        // Check file category first
        let category = classify_file(path);
        if !category.is_supported() {
            log::debug!(
                "skipping {} '{}': {}",
                category.description(),
                path.display(),
                SkipReason::BinaryFile.message()
            );
            self.skipped.insert(uri, SkipReason::BinaryFile);
            return Err(SkipReason::BinaryFile);
        }

        // Get the appropriate size limit for this file category
        let max_bytes = limits
            .max_bytes_for_file(path)
            .unwrap_or(limits.source_max_bytes);
        if text.len() > max_bytes {
            log::warn!(
                "skipping {} '{}': content length {} exceeds {} limit of {} bytes",
                category.description(),
                path.display(),
                text.len(),
                category.description(),
                max_bytes
            );
            self.skipped.insert(uri, SkipReason::ExceedsLimit);
            return Err(SkipReason::ExceedsLimit);
        }

        let entry = DocumentEntry::new(language_id, version, text);
        self.inner.insert(uri, entry);
        Ok(())
    }

    /// Check if a file was intentionally skipped.
    #[must_use]
    pub fn get_skip_reason(&self, path: &Path) -> Option<SkipReason> {
        let uri = path_to_url(path)?;
        self.skipped.get(&uri).map(|r| *r)
    }

    /// Check if a file is a supported type (not binary).
    #[must_use]
    pub fn is_supported_file(path: &Path) -> bool {
        classify_file(path).is_supported()
    }

    pub fn change(
        &self,
        path: &Path,
        version: Option<i32>,
        changes: &[TextDocumentContentChangeEvent],
        limits: &DocumentLimits,
    ) {
        let Some(uri) = path_to_url(path) else {
            log::warn!(
                "failed to convert document path '{}' into file:// URL",
                path.display()
            );
            return;
        };

        let Some(mut entry) = self.inner.get_mut(&uri) else {
            log::debug!(
                "received change for unopened document '{}', ignoring",
                path.display()
            );
            return;
        };

        let mut text = entry.text();

        for change in changes {
            if change.range.is_none() {
                text.clone_from(&change.text);
            } else if let Some(range) = change.range {
                let start_offset = position_to_offset(&text, range.start);
                let end_offset = position_to_offset(&text, range.end);

                match (start_offset, end_offset) {
                    (Some(start), Some(end)) if start <= end && end <= text.len() => {
                        text.replace_range(start..end, &change.text);
                    }
                    _ => {
                        // Position mismatch - the client's view differs from ours.
                        // This happens during rapid edits, external file changes, or encoding mismatches.
                        //
                        // IMPORTANT: For incremental edits (range is Some), change.text contains
                        // only the replacement text (e.g., "a" when typing), NOT the full document.
                        // Replacing the buffer with change.text would CORRUPT the document!
                        //
                        // Correct behavior: Skip this change and keep the current buffer.
                        // The client will eventually resync via a full document update.
                        let total_lines = text.lines().count();
                        log::debug!(
                            "skipping incremental edit for '{}': position mismatch \
                             (range {:?}, start_offset={:?}, end_offset={:?}, \
                             buffer has {} lines, {} bytes). \
                             Awaiting full document resync from client.",
                            path.display(),
                            range,
                            start_offset,
                            end_offset,
                            total_lines,
                            text.len()
                        );
                        // Skip this change - do NOT replace buffer with incremental text
                    }
                }
            }
        }

        entry.replace_text(&text);

        if let Some(ver) = version {
            entry.version = ver;
        }

        // Get the appropriate size limit for this file category
        let max_bytes = limits
            .max_bytes_for_file(path)
            .unwrap_or(limits.source_max_bytes);
        if entry.len_bytes() > max_bytes {
            let category = classify_file(path);
            log::warn!(
                "{} '{}' grew beyond {} limit ({} bytes), dropping buffer",
                category.description(),
                path.display(),
                category.description(),
                max_bytes
            );
            drop(entry);
            self.inner.remove(&uri);
            self.skipped.insert(uri, SkipReason::ExceedsLimit);
        }
    }

    pub fn close(&self, path: &Path) {
        if let Some(uri) = path_to_url(path) {
            self.inner.remove(&uri);
            self.skipped.remove(&uri); // Clean up skip tracking
        }
    }

    #[must_use]
    pub fn get(&self, path: &Path) -> Option<DocumentSnapshot> {
        let uri = path_to_url(path)?;
        self.inner.get(&uri).map(|entry| DocumentSnapshot {
            language_id: entry.language_id.clone(),
            version: entry.version,
            rope: entry.rope.clone(),
            line_offsets: Arc::from(entry.line_offsets.clone()),
        })
    }

    /// Prune documents that exceed the size limits for their category.
    pub fn prune_by_limits(&self, limits: &DocumentLimits) {
        self.inner.retain(|uri, entry| {
            // Extract path from URI to determine category
            let path = uri.to_file_path().ok();
            let max_bytes = path
                .as_ref()
                .and_then(|p| limits.max_bytes_for_file(p))
                .unwrap_or(limits.source_max_bytes);

            let keep = entry.len_bytes() <= max_bytes;
            if !keep {
                let category = path
                    .as_ref()
                    .map_or("file", |p| classify_file(p).description());
                log::info!(
                    "dropping in-memory buffer for '{uri}' ({category}): exceeds {max_bytes} byte limit"
                );
            }
            keep
        });
    }
}

#[derive(Debug, Clone)]
pub struct DocumentSnapshot {
    pub language_id: Option<String>,
    pub version: i32,
    pub rope: Rope,
    pub line_offsets: Arc<[usize]>,
}

impl DocumentSnapshot {
    #[must_use]
    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    #[must_use]
    pub fn lsp_to_byte(&self, position: Position) -> Option<usize> {
        let line_idx = position.line as usize;
        let utf16_col = position.character as usize;

        let line_start = *self.line_offsets.get(line_idx)?;
        let line_slice = self.rope.line(line_idx).to_string();
        let byte_col =
            crate::utils::position::line_utf16_col_to_byte(line_slice.as_str(), utf16_col);
        let absolute = line_start + byte_col;
        if absolute > self.rope.len_bytes() {
            None
        } else {
            Some(absolute)
        }
    }

    #[must_use]
    pub fn byte_to_lsp(&self, byte_offset: usize) -> Option<Position> {
        if byte_offset > self.rope.len_bytes() {
            return None;
        }

        let line_idx = match self.line_offsets.binary_search(&byte_offset) {
            Ok(idx) => idx,
            Err(0) => 0,
            Err(next) => next.saturating_sub(1),
        };

        let line_start = *self.line_offsets.get(line_idx)?;
        let line_slice = self.rope.line(line_idx).to_string();
        let byte_in_line = byte_offset.saturating_sub(line_start);
        if byte_in_line > self.rope.line(line_idx).len_bytes() {
            return None;
        }

        let utf16_col =
            crate::utils::position::line_byte_to_utf16_col(line_slice.as_str(), byte_in_line);

        // LSP uses u32 for line/column; clamp to max
        Some(Position::new(
            line_idx.try_into().unwrap_or(u32::MAX),
            utf16_col.try_into().unwrap_or(u32::MAX),
        ))
    }
}

fn position_to_offset(text: &str, position: Position) -> Option<usize> {
    let target_line = position.line as usize;
    let target_col_utf16 = position.character as usize;

    // Fast path: start of buffer
    if target_line == 0 && target_col_utf16 == 0 {
        return Some(0);
    }

    // Walk lines to find target line
    let mut offset = 0usize;
    for (line_idx, line_text) in text.split_inclusive('\n').enumerate() {
        let line_without_nl = line_text.strip_suffix('\n').unwrap_or(line_text);

        if line_idx == target_line {
            // Compute the UTF-16 length of the line for validation
            let utf16_line_len: usize = line_without_nl.chars().map(char::len_utf16).sum();

            // Validate column is within line bounds
            // Allow target_col_utf16 == utf16_line_len for end-of-line cursor positions
            if target_col_utf16 > utf16_line_len {
                return None;
            }

            // Convert UTF-16 column to byte offset within the line
            let col_byte =
                crate::utils::position::line_utf16_col_to_byte(line_without_nl, target_col_utf16);

            return Some(offset + col_byte);
        }

        offset += line_text.len();
    }

    // Special case: position at end of file (line == total_lines, col == 0)
    // This happens when the cursor is after the last character
    let total_lines = text.split_inclusive('\n').count();
    if target_line == total_lines && target_col_utf16 == 0 {
        return Some(text.len());
    }

    None
}

fn path_to_url(path: &Path) -> Option<Url> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().ok()?.join(path)
    };
    Url::from_file_path(absolute).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tower_lsp::lsp_types::{Position, Range};

    fn test_limits() -> DocumentLimits {
        DocumentLimits {
            source_max_bytes: 1024,
            data_max_bytes: 10 * 1024,
        }
    }

    fn small_limits() -> DocumentLimits {
        DocumentLimits {
            source_max_bytes: 3, // Small enough to trigger pruning of 5-byte content
            data_max_bytes: 20,
        }
    }

    #[test]
    fn open_within_limit_is_stored() {
        let store = DocumentStore::new();
        let path = PathBuf::from("example.rs");
        let result = store.open(
            path.as_path(),
            Some("rust".into()),
            1,
            "fn main() {}",
            &test_limits(),
        );

        assert!(result.is_ok());
        let snapshot = store.get(&path).expect("document stored");
        assert_eq!(snapshot.language_id.as_deref(), Some("rust"));
        assert_eq!(snapshot.version, 1);
        assert_eq!(snapshot.text(), "fn main() {}");
        assert_eq!(snapshot.line_offsets.as_ref(), &[0]);
    }

    #[test]
    fn open_exceeding_limit_is_skipped() {
        let store = DocumentStore::new();
        let path = PathBuf::from("big.rs");
        let content = "x".repeat(10);
        let result = store.open(path.as_path(), None, 1, content.as_str(), &small_limits());
        assert_eq!(result, Err(SkipReason::ExceedsLimit));
        assert!(store.get(path.as_path()).is_none());
        assert_eq!(
            store.get_skip_reason(path.as_path()),
            Some(SkipReason::ExceedsLimit)
        );
    }

    #[test]
    fn open_binary_file_is_rejected() {
        let store = DocumentStore::new();
        let path = PathBuf::from("document.pdf");
        let result = store.open(path.as_path(), None, 1, "fake pdf content", &test_limits());
        assert_eq!(result, Err(SkipReason::BinaryFile));
        assert!(store.get(path.as_path()).is_none());
        assert_eq!(
            store.get_skip_reason(path.as_path()),
            Some(SkipReason::BinaryFile)
        );
    }

    #[test]
    fn data_file_uses_larger_limit() {
        let store = DocumentStore::new();
        // 15 bytes - exceeds source limit (5) but within data limit (20)
        let content = "x".repeat(15);
        let result = store.open(
            Path::new("config.json"),
            None,
            1,
            content.as_str(),
            &small_limits(),
        );
        assert!(result.is_ok());
        assert!(store.get(Path::new("config.json")).is_some());
    }

    #[test]
    fn incremental_change_applies() {
        let store = DocumentStore::new();
        let path = PathBuf::from("file.rs");
        let _ = store.open(path.as_path(), None, 1, "fn main() {}\n", &test_limits());

        let change = TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position::new(0, 3),
                end: Position::new(0, 7),
            }),
            range_length: None,
            text: "start".into(),
        };

        store.change(&path, Some(2), &[change], &test_limits());
        let snapshot = store.get(&path).expect("document stored");
        assert_eq!(snapshot.version, 2);
        assert_eq!(snapshot.text(), "fn start() {}\n");
    }

    #[test]
    fn prune_by_limits_removes_large_buffers() {
        let store = DocumentStore::new();
        let path = PathBuf::from("file.rs");
        let _ = store.open(path.as_path(), None, 1, "abcde", &test_limits());
        // Now prune with smaller limits
        store.prune_by_limits(&small_limits());
        assert!(store.get(&path).is_none());
    }

    #[test]
    fn snapshot_lsp_utf16_roundtrip() {
        let store = DocumentStore::new();
        let path = PathBuf::from("emoji.rs");
        let _ = store.open(path.as_path(), None, 1, "a🙂b", &test_limits());

        let snapshot = store.get(&path).expect("document stored");
        assert_eq!(snapshot.line_offsets.as_ref(), &[0]);

        let offset = snapshot
            .lsp_to_byte(Position::new(0, 3))
            .expect("convert position");
        assert_eq!(offset, "a🙂".len());

        let pos = snapshot
            .byte_to_lsp(offset)
            .expect("convert offset back to position");
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 3);
    }

    #[test]
    fn snapshot_line_offsets_multiple_lines() {
        let store = DocumentStore::new();
        let path = PathBuf::from("multi.rs");
        let text = "first line\nsecond🙂line\n";
        let _ = store.open(path.as_path(), None, 1, text, &test_limits());
        let snapshot = store.get(&path).expect("document stored");
        let offsets = snapshot.line_offsets.as_ref();
        assert_eq!(offsets.len(), 3);
        assert_eq!(offsets[0], 0);
        assert_eq!(offsets[1], "first line\n".len());
        assert_eq!(offsets[2], "first line\nsecond🙂line\n".len());

        let offset = snapshot
            .lsp_to_byte(Position::new(1, 6))
            .expect("position to byte");
        let roundtrip = snapshot.byte_to_lsp(offset).expect("byte to position");
        assert_eq!(roundtrip.line, 1);
        assert_eq!(roundtrip.character, 6);
    }

    #[test]
    fn failed_incremental_change_preserves_buffer() {
        // Regression test: when an incremental edit fails due to invalid position,
        // the buffer should NOT be corrupted with the incremental text.
        // Previously, the bug was: text = change.text.clone() which would replace
        // "fn main() {}" with "X" (the incremental text).
        let store = DocumentStore::new();
        let path = PathBuf::from("test.rs");
        let original_content = "fn main() {}\n";
        let _ = store.open(path.as_path(), None, 1, original_content, &test_limits());

        // Create an invalid incremental change: line 99 doesn't exist
        let invalid_change = TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position::new(99, 0), // Line 99 doesn't exist
                end: Position::new(99, 5),
            }),
            range_length: None,
            text: "X".into(), // This is just the replacement text, NOT the full document
        };

        store.change(&path, Some(2), &[invalid_change], &test_limits());
        let snapshot = store.get(&path).expect("document stored");

        // The buffer should be preserved (NOT replaced with "X")
        assert_eq!(
            snapshot.text(),
            original_content,
            "Buffer should be preserved when incremental edit fails"
        );
        // Version should still be updated (change was processed, even if skipped)
        assert_eq!(snapshot.version, 2);
    }

    #[test]
    fn failed_incremental_change_column_out_of_bounds() {
        // Test column out of bounds - similar to line out of bounds
        let store = DocumentStore::new();
        let path = PathBuf::from("test2.rs");
        let original_content = "ab\n";
        let _ = store.open(path.as_path(), None, 1, original_content, &test_limits());

        // Column 999 is way beyond the line length
        let invalid_change = TextDocumentContentChangeEvent {
            range: Some(Range {
                start: Position::new(0, 999),
                end: Position::new(0, 1000),
            }),
            range_length: None,
            text: "INVALID".into(),
        };

        store.change(&path, Some(2), &[invalid_change], &test_limits());
        let snapshot = store.get(&path).expect("document stored");

        assert_eq!(
            snapshot.text(),
            original_content,
            "Buffer should be preserved when column is out of bounds"
        );
    }

    #[test]
    fn close_cleans_up_skip_tracking() {
        let store = DocumentStore::new();
        let path = PathBuf::from("document.pdf");

        // Open binary file - gets skipped
        let _ = store.open(path.as_path(), None, 1, "content", &test_limits());
        assert_eq!(store.get_skip_reason(&path), Some(SkipReason::BinaryFile));

        // Close should clean up skip tracking
        store.close(&path);
        assert!(store.get_skip_reason(&path).is_none());
    }
}
