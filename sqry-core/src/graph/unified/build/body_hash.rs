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
use crate::graph::unified::build::staging::BodySpan;
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
/// # The build seam no longer calls this
///
/// [`StagingGraph::attach_body_hashes`] fingerprints the extent a DECLARATION
/// offered a node, which is not always the extent recorded on the entry, so it
/// uses [`compute_body_hash_at`] (issue #748). This entry point remains for
/// callers that really do mean "hash whatever range this entry sits at", and
/// the two are pinned to agree for the same extent by
/// `entry_addressed_and_extent_addressed_hashes_agree`.
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
///
/// [`StagingGraph::attach_body_hashes`]: crate::graph::unified::build::staging::StagingGraph::attach_body_hashes
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

/// Compute the body hash over an explicit extent rather than the one recorded
/// on the entry.
///
/// The build seam fingerprints the extent a DECLARATION offered a node, which
/// is not always the extent the node is recorded at: a type reference below a
/// `struct X { ... }` wins the recorded location under the latest-ending rule
/// but does not own the body (issue #748). `entry.kind` still decides
/// eligibility, and the minimum body length is unchanged.
#[must_use]
pub fn compute_body_hash_at(
    content: &[u8],
    kind: NodeKind,
    body_span: BodySpan,
    line_offsets: &[usize],
) -> Option<BodyHash128> {
    if !node_kind_supports_body_hash(kind) {
        return None;
    }

    if !is_valid_body_extent(body_span) {
        return None;
    }

    let (start_byte, end_byte) = resolve_body_span(
        line_offsets,
        body_span.start_line,
        body_span.start_column,
        body_span.end_line,
        body_span.end_column,
        content.len(),
    )?;

    let body = &content[start_byte..end_byte];

    // Skip bodies smaller than 4 bytes
    if body.len() < 4 {
        return None;
    }

    Some(BodyHash128::compute(body))
}

/// [`has_valid_body_span`] for a bare extent: non-zero lines, start before end.
///
/// Deliberately a mirror of that function rather than a subset of it, so the
/// two hash entry points cannot drift. The zero-line checks are load-bearing:
/// `resolve_body_span` maps line 0 onto line 1 through `saturating_sub`, so
/// without them a `{0, 0}` data-quality sentinel would hash the first line of
/// the file.
///
/// The same-line `start_column >= end_column` check is NOT load-bearing;
/// `resolve_body_span` rejects `start_byte >= end_byte` downstream and catches
/// the same cases. `has_valid_body_span` carries the same redundant check, and
/// it is kept here for exactly that reason: the two must read alike.
#[must_use]
fn is_valid_body_extent(body_span: BodySpan) -> bool {
    if body_span.start_line == 0 || body_span.end_line == 0 {
        return false;
    }
    if body_span.start_line > body_span.end_line {
        return false;
    }
    if body_span.start_line == body_span.end_line && body_span.start_column >= body_span.end_column
    {
        return false;
    }
    true
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

    /// The extent-addressed entry point the build seam uses and the
    /// entry-addressed one it used to use must give the same answer for the
    /// same range, on every axis that can make them differ: node-kind
    /// eligibility, the minimum body length, span validity, and byte
    /// resolution (issue #748).
    ///
    /// Two functions computing "the body hash" that disagree is exactly the
    /// class of bug this whole change is about.
    #[test]
    fn entry_addressed_and_extent_addressed_hashes_agree() {
        use crate::graph::unified::build::staging::BodySpan;

        let content = b"fn foo() { return 42; }\nfn bar() { let x = 1; x }\nlet v = 0;\n";
        let line_offsets = build_line_offsets(content);

        struct Case {
            name: &'static str,
            kind: NodeKind,
            span: (u32, u32, u32, u32),
        }

        let cases = [
            Case {
                name: "an ordinary hashable body",
                kind: NodeKind::Function,
                span: (1, 0, 1, 23),
            },
            Case {
                name: "a second one, to catch a constant-return implementation",
                kind: NodeKind::Function,
                span: (2, 0, 2, 25),
            },
            Case {
                name: "an ineligible kind",
                kind: NodeKind::Variable,
                span: (3, 0, 3, 10),
            },
            Case {
                name: "another ineligible kind",
                kind: NodeKind::Parameter,
                span: (1, 0, 1, 23),
            },
            Case {
                name: "a body under the four-byte minimum",
                kind: NodeKind::Function,
                span: (1, 0, 1, 2),
            },
            Case {
                name: "a zero start line, the data-quality sentinel",
                kind: NodeKind::Function,
                span: (0, 0, 1, 23),
            },
            Case {
                name: "a zero end line",
                kind: NodeKind::Function,
                span: (1, 0, 0, 23),
            },
            Case {
                name: "an inverted range",
                kind: NodeKind::Function,
                span: (2, 0, 1, 5),
            },
            Case {
                name: "a same-line range with start at or past end",
                kind: NodeKind::Function,
                span: (1, 10, 1, 10),
            },
            Case {
                name: "an end column past the end of the file",
                kind: NodeKind::Function,
                span: (3, 0, 3, 9_999),
            },
            Case {
                name: "a start line past the end of the file",
                kind: NodeKind::Function,
                span: (99, 0, 99, 5),
            },
        ];

        let mut hashed = 0usize;
        let mut declined = 0usize;
        for case in &cases {
            let (sl, sc, el, ec) = case.span;
            let mut entry = NodeEntry::new(case.kind, StringId::new(1), FileId::new(0));
            entry.start_line = sl;
            entry.start_column = sc;
            entry.end_line = el;
            entry.end_column = ec;

            let by_entry = compute_node_body_hash(content, &entry, &line_offsets);
            let by_extent = compute_body_hash_at(
                content,
                case.kind,
                BodySpan {
                    start_line: sl,
                    start_column: sc,
                    end_line: el,
                    end_column: ec,
                },
                &line_offsets,
            );
            assert_eq!(
                by_entry, by_extent,
                "{}: the two entry points disagree for the same extent",
                case.name
            );

            if by_entry.is_some() {
                hashed += 1;
            } else {
                declined += 1;
            }
        }

        // Non-vacuity: the table must exercise BOTH outcomes, or an
        // implementation that always returned `None` would pass it.
        assert!(
            hashed >= 2 && declined >= 8,
            "the case table must cover both outcomes, got {hashed} hashed and \
             {declined} declined"
        );
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
