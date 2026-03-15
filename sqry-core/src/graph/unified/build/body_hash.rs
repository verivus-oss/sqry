//! Body hash computation for `NodeEntry` in the unified graph.
//!
//! This module provides utilities for computing 128-bit body hashes for nodes
//! during graph building. Body hashes enable accurate duplicate code detection
//! by comparing the raw content of symbols rather than their names or signatures.
//!
//! # Supported Node Kinds
//!
//! Only certain node kinds support body hashing (those with meaningful bodies):
//! - Function, Method (code bodies)
//! - Class, Struct, Enum, Interface, Trait (type bodies)
//! - Module (module bodies)
//!
//! # Design Notes
//!
//! - Uses raw body bytes without normalization (whitespace-sensitive)
//! - Minimum body size of 4 bytes to avoid trivial matches
//! - Hash is computed using dual xxh64 with different seeds (128-bit collision resistance)
//!
//! See `docs/development/codegraph-body-hash/` for full specification.

use crate::graph::body_hash::BodyHash128;
use crate::graph::unified::node::kind::NodeKind;
use crate::graph::unified::storage::arena::NodeEntry;

/// Build a line offset index for efficient line/column to byte offset conversion.
///
/// Returns a vector where `line_offsets[i]` is the byte offset of line `i` (0-indexed).
/// Line 0 always starts at offset 0.
///
/// # Example
///
/// ```text
/// Content: "fn foo() {\n    42\n}\n"
/// Lines:    [0,           11,     18, 20]
///           ^line 0       ^line 1 ^line 2
/// ```
#[must_use]
pub fn build_line_offsets(content: &[u8]) -> Vec<usize> {
    let mut line_offsets = vec![0];
    for (i, &byte) in content.iter().enumerate() {
        if byte == b'\n' {
            line_offsets.push(i + 1);
        }
    }
    line_offsets
}

/// Resolve the byte span for a node's body using pre-computed line offsets.
///
/// # Arguments
///
/// * `line_offsets` - Pre-computed line offsets from `build_line_offsets`
/// * `start_line` - 1-indexed start line
/// * `start_column` - 0-indexed start column
/// * `end_line` - 1-indexed end line
/// * `end_column` - 0-indexed end column
/// * `content_len` - Total length of the content in bytes
///
/// # Returns
///
/// `Some((start_byte, end_byte))` if the span is valid, `None` otherwise.
///
/// # EOF Clamping
///
/// If the end position is on the last line past content length, the end byte
/// is clamped to `content_len` to handle trailing content without newlines.
#[must_use]
pub fn resolve_body_span(
    line_offsets: &[usize],
    start_line: u32,
    start_column: u32,
    end_line: u32,
    end_column: u32,
    content_len: usize,
) -> Option<(usize, usize)> {
    // Convert 1-indexed lines to 0-indexed
    let start_line_idx = start_line.saturating_sub(1) as usize;
    let end_line_idx = end_line.saturating_sub(1) as usize;

    // Validate line indices
    if start_line_idx >= line_offsets.len() || end_line_idx >= line_offsets.len() {
        return None;
    }

    let start_byte = line_offsets[start_line_idx] + start_column as usize;

    // EOF clamping: if end is on last line and would exceed content, clamp
    let end_byte = if end_line_idx + 1 < line_offsets.len() {
        line_offsets[end_line_idx] + end_column as usize
    } else {
        // Last line - clamp to content length
        content_len.min(line_offsets[end_line_idx] + end_column as usize)
    };

    // Validate byte range
    if start_byte >= content_len || end_byte > content_len || start_byte >= end_byte {
        return None;
    }

    Some((start_byte, end_byte))
}

/// Check if a node has a valid body span (non-zero lines, start before end).
#[must_use]
pub fn has_valid_body_span(entry: &NodeEntry) -> bool {
    if entry.start_line == 0 || entry.end_line == 0 {
        return false;
    }
    if entry.start_line > entry.end_line {
        return false;
    }
    if entry.start_line == entry.end_line && entry.start_column >= entry.end_column {
        return false;
    }
    true
}

/// Extract the body content of a node from source bytes.
///
/// # Arguments
///
/// * `content` - Full source file content as bytes
/// * `entry` - Node entry with line/column location information
/// * `line_offsets` - Pre-computed line offsets from `build_line_offsets`
///
/// # Returns
///
/// `Some(bytes)` if the node has a valid extractable body, `None` otherwise.
#[must_use]
pub fn extract_node_body(
    content: &[u8],
    entry: &NodeEntry,
    line_offsets: &[usize],
) -> Option<Vec<u8>> {
    if !has_valid_body_span(entry) {
        return None;
    }

    let (start_byte, end_byte) = resolve_body_span(
        line_offsets,
        entry.start_line,
        entry.start_column,
        entry.end_line,
        entry.end_column,
        content.len(),
    )?;

    Some(content[start_byte..end_byte].to_vec())
}

/// Compute the body hash for a node if it has extractable body content.
///
/// # Arguments
///
/// * `content` - Full source file content as bytes
/// * `entry` - Node entry to compute hash for
/// * `line_offsets` - Pre-computed line offsets from `build_line_offsets`
///
/// # Returns
///
/// `Some(hash)` if the node has a hashable body of at least 4 bytes, `None` otherwise.
///
/// # Minimum Body Size
///
/// Bodies smaller than 4 bytes are not hashed to avoid trivial duplicate matches
/// (e.g., empty bodies `{}`, single statements).
#[must_use]
pub fn compute_node_body_hash(
    content: &[u8],
    entry: &NodeEntry,
    line_offsets: &[usize],
) -> Option<BodyHash128> {
    if !node_kind_supports_body_hash(entry.kind) {
        return None;
    }

    let body = extract_node_body(content, entry, line_offsets)?;

    // Skip bodies smaller than 4 bytes
    if body.len() < 4 {
        return None;
    }

    Some(BodyHash128::compute(&body))
}

/// Check if a node kind supports body hashing.
///
/// Only certain node kinds have meaningful bodies that can be hashed:
/// - **Function, Method**: Code bodies
/// - **Class, Struct, Enum, Interface, Trait**: Type definitions
/// - **Module**: Module contents
///
/// Note: `Trait` is included as it maps to `Interface` in the legacy symbol system.
/// Note: There is no `Namespace` in `NodeKind` (unlike the legacy kind enum).
#[must_use]
pub fn node_kind_supports_body_hash(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::Module
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::file::id::FileId;
    use crate::graph::unified::string::id::StringId;

    fn test_file() -> FileId {
        FileId::new(1)
    }

    fn test_name() -> StringId {
        StringId::new(1)
    }

    fn make_entry(
        kind: NodeKind,
        start_line: u32,
        start_col: u32,
        end_line: u32,
        end_col: u32,
    ) -> NodeEntry {
        NodeEntry::new(kind, test_name(), test_file())
            .with_location(start_line, start_col, end_line, end_col)
    }

    #[test]
    fn test_build_line_offsets_empty() {
        let offsets = build_line_offsets(b"");
        assert_eq!(offsets, vec![0]);
    }

    #[test]
    fn test_build_line_offsets_single_line() {
        let offsets = build_line_offsets(b"hello");
        assert_eq!(offsets, vec![0]);
    }

    #[test]
    fn test_build_line_offsets_multiple_lines() {
        let content = b"line1\nline2\nline3";
        let offsets = build_line_offsets(content);
        assert_eq!(offsets, vec![0, 6, 12]);
    }

    #[test]
    fn test_build_line_offsets_trailing_newline() {
        let content = b"line1\nline2\n";
        let offsets = build_line_offsets(content);
        assert_eq!(offsets, vec![0, 6, 12]);
    }

    #[test]
    fn test_resolve_body_span_simple() {
        let content = b"fn foo() {\n    42\n}\n";
        let line_offsets = build_line_offsets(content);

        // Line 1, col 0 to line 3, col 1 => "fn foo() {\n    42\n}"
        let span = resolve_body_span(&line_offsets, 1, 0, 3, 1, content.len());
        assert_eq!(span, Some((0, 19)));
    }

    #[test]
    fn test_resolve_body_span_single_line() {
        let content = b"let x = 42;";
        let line_offsets = build_line_offsets(content);

        let span = resolve_body_span(&line_offsets, 1, 0, 1, 11, content.len());
        assert_eq!(span, Some((0, 11)));
    }

    #[test]
    fn test_resolve_body_span_eof_clamp() {
        let content = b"fn foo() { }"; // No trailing newline
        let line_offsets = build_line_offsets(content);

        // End column past content length should clamp
        let span = resolve_body_span(&line_offsets, 1, 0, 1, 100, content.len());
        assert_eq!(span, Some((0, 12)));
    }

    #[test]
    fn test_resolve_body_span_invalid_lines() {
        let content = b"line1\nline2";
        let line_offsets = build_line_offsets(content);

        // Line 10 doesn't exist
        let span = resolve_body_span(&line_offsets, 10, 0, 10, 5, content.len());
        assert!(span.is_none());
    }

    #[test]
    fn test_resolve_body_span_start_after_end() {
        let content = b"line1\nline2";
        let line_offsets = build_line_offsets(content);

        // Start after end
        let span = resolve_body_span(&line_offsets, 2, 0, 1, 0, content.len());
        assert!(span.is_none());
    }

    #[test]
    fn test_has_valid_body_span_valid() {
        let entry = make_entry(NodeKind::Function, 1, 0, 3, 1);
        assert!(has_valid_body_span(&entry));
    }

    #[test]
    fn test_has_valid_body_span_zero_lines() {
        let entry = make_entry(NodeKind::Function, 0, 0, 0, 0);
        assert!(!has_valid_body_span(&entry));
    }

    #[test]
    fn test_has_valid_body_span_start_after_end_line() {
        let entry = make_entry(NodeKind::Function, 5, 0, 3, 0);
        assert!(!has_valid_body_span(&entry));
    }

    #[test]
    fn test_has_valid_body_span_same_line_bad_columns() {
        let entry = make_entry(NodeKind::Function, 1, 10, 1, 5);
        assert!(!has_valid_body_span(&entry));
    }

    #[test]
    fn test_extract_node_body() {
        let content = b"fn foo() {\n    42\n}\n";
        let line_offsets = build_line_offsets(content);
        let entry = make_entry(NodeKind::Function, 1, 0, 3, 1);

        let body = extract_node_body(content, &entry, &line_offsets);
        assert_eq!(body.as_deref(), Some(b"fn foo() {\n    42\n}".as_slice()));
    }

    #[test]
    fn test_extract_node_body_invalid_span() {
        let content = b"fn foo() {}";
        let line_offsets = build_line_offsets(content);
        let entry = make_entry(NodeKind::Function, 0, 0, 0, 0);

        let body = extract_node_body(content, &entry, &line_offsets);
        assert!(body.is_none());
    }

    #[test]
    fn test_node_kind_supports_body_hash() {
        // Supported kinds
        assert!(node_kind_supports_body_hash(NodeKind::Function));
        assert!(node_kind_supports_body_hash(NodeKind::Method));
        assert!(node_kind_supports_body_hash(NodeKind::Class));
        assert!(node_kind_supports_body_hash(NodeKind::Struct));
        assert!(node_kind_supports_body_hash(NodeKind::Enum));
        assert!(node_kind_supports_body_hash(NodeKind::Interface));
        assert!(node_kind_supports_body_hash(NodeKind::Trait));
        assert!(node_kind_supports_body_hash(NodeKind::Module));

        // Unsupported kinds
        assert!(!node_kind_supports_body_hash(NodeKind::Variable));
        assert!(!node_kind_supports_body_hash(NodeKind::Constant));
        assert!(!node_kind_supports_body_hash(NodeKind::Type));
        assert!(!node_kind_supports_body_hash(NodeKind::Import));
        assert!(!node_kind_supports_body_hash(NodeKind::Export));
        assert!(!node_kind_supports_body_hash(NodeKind::CallSite));
        assert!(!node_kind_supports_body_hash(NodeKind::EnumVariant));
        assert!(!node_kind_supports_body_hash(NodeKind::Macro));
    }

    #[test]
    fn test_compute_node_body_hash_function() {
        let content = b"fn foo() { return 42; }";
        let line_offsets = build_line_offsets(content);
        let entry = make_entry(NodeKind::Function, 1, 0, 1, 23);

        let hash = compute_node_body_hash(content, &entry, &line_offsets);
        assert!(hash.is_some());
        let hash = hash.unwrap();
        assert_ne!(hash.high, 0);
        assert_ne!(hash.low, 0);
    }

    #[test]
    fn test_compute_node_body_hash_deterministic() {
        let content = b"fn foo() { return 42; }";
        let line_offsets = build_line_offsets(content);
        let entry = make_entry(NodeKind::Function, 1, 0, 1, 23);

        let hash1 = compute_node_body_hash(content, &entry, &line_offsets);
        let hash2 = compute_node_body_hash(content, &entry, &line_offsets);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_node_body_hash_different_content() {
        let content1 = b"fn foo() { return 42; }";
        let content2 = b"fn foo() { return 43; }";
        let line_offsets1 = build_line_offsets(content1);
        let line_offsets2 = build_line_offsets(content2);
        let entry = make_entry(NodeKind::Function, 1, 0, 1, 23);

        let hash1 = compute_node_body_hash(content1, &entry, &line_offsets1);
        let hash2 = compute_node_body_hash(content2, &entry, &line_offsets2);
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_node_body_hash_unsupported_kind() {
        let content = b"let x = 42;";
        let line_offsets = build_line_offsets(content);
        let entry = make_entry(NodeKind::Variable, 1, 0, 1, 11);

        let hash = compute_node_body_hash(content, &entry, &line_offsets);
        assert!(hash.is_none());
    }

    #[test]
    fn test_compute_node_body_hash_too_small() {
        let content = b"fn f(){}"; // Body "{}" is only 2 bytes
        let line_offsets = build_line_offsets(content);
        // Just the body "{}"
        let entry = make_entry(NodeKind::Function, 1, 6, 1, 8);

        let hash = compute_node_body_hash(content, &entry, &line_offsets);
        assert!(hash.is_none());
    }

    #[test]
    fn test_compute_node_body_hash_exactly_4_bytes() {
        let content = b"fn f(){ab}"; // Body "{ab}" is exactly 4 bytes
        let line_offsets = build_line_offsets(content);
        let entry = make_entry(NodeKind::Function, 1, 6, 1, 10);

        let hash = compute_node_body_hash(content, &entry, &line_offsets);
        assert!(hash.is_some());
    }
}
