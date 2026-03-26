//! JSON `GraphBuilder` implementation for unified graph extraction.
//!
//! Extracts nodes and edges from JSON config files using a two-phase approach:
//! 1. Parse tree-sitter AST into intermediate `Value` enum
//! 2. Walk the `Value` tree iteratively, emitting graph nodes/edges
//!
//! Format-specific profiles (`now-ui.json`, `package.json`) override default
//! node kinds for domain-relevant entries.

use std::path::Path;

use sqry_core::config::buffers::{json_max_depth, json_max_nodes};
use sqry_core::graph::{
    GraphBuilder, GraphResult, Language, Position, Span,
    unified::{GraphBuildHelper, StagingGraph, node::NodeId},
};
use tree_sitter::{Node, Tree};

use crate::profiles::Profile;

// ─── Excluded Filenames ─────────────────────────────────────────────────────

/// Known high-volume, low-signal JSON files to skip.
const EXCLUDED_FILENAMES: &[&str] = &[
    "package-lock.json",
    "shrinkwrap.json",
    "npm-shrinkwrap.json",
];

/// Check if a file should be excluded from JSON indexing.
fn is_excluded(path: &Path) -> bool {
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_ascii_lowercase())
        .unwrap_or_default();

    EXCLUDED_FILENAMES.contains(&filename.as_str()) || filename.ends_with(".min.json")
}

// ─── Key Segment Escaping ───────────────────────────────────────────────────

/// Escape a JSON key for use as a qualified name segment.
///
/// Dots and backslashes in keys are escaped to prevent ambiguity
/// when joining segments with `.`.
fn escape_segment(key: &str) -> String {
    if !key.contains('.') && !key.contains('\\') {
        return key.to_string();
    }
    let mut escaped = String::with_capacity(key.len() + 4);
    for ch in key.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '.' => escaped.push_str("\\."),
            _ => escaped.push(ch),
        }
    }
    escaped
}

// ─── Intermediate Value Representation (Layer 1) ────────────────────────────

/// Intermediate representation of a JSON value parsed from tree-sitter AST.
#[derive(Debug, Clone)]
enum Value {
    Map(Vec<MapEntry>),
    Seq(Vec<SeqEntry>),
    Scalar,
}

/// A single key-value pair in a JSON object.
#[derive(Debug, Clone)]
struct MapEntry {
    key: String,
    span: Option<Span>,
    value: Value,
}

/// A single element in a JSON array, with source span.
#[derive(Debug, Clone)]
struct SeqEntry {
    value: Value,
    span: Option<Span>,
}

// ─── AST-to-Value Parsing (Layer 1, ported from Pulumi) ─────────────────────

fn parse_json_value(node: Node<'_>, content: &[u8], depth: u32, max_depth: u32) -> Option<Value> {
    if depth >= max_depth {
        return Some(Value::Scalar);
    }
    match node.kind() {
        "object" => Some(Value::Map(parse_json_object(
            node, content, depth, max_depth,
        ))),
        "array" => Some(Value::Seq(parse_json_array(
            node, content, depth, max_depth,
        ))),
        "string" | "number" | "true" | "false" | "null" => Some(Value::Scalar),
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(value) = parse_json_value(child, content, depth, max_depth) {
                    return Some(value);
                }
            }
            None
        }
    }
}

fn parse_json_object(node: Node<'_>, content: &[u8], depth: u32, max_depth: u32) -> Vec<MapEntry> {
    let mut entries = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "pair"
            && let Some(entry) = parse_json_pair(child, content, depth, max_depth)
        {
            entries.push(entry);
        }
    }
    entries
}

fn parse_json_pair(node: Node<'_>, content: &[u8], depth: u32, max_depth: u32) -> Option<MapEntry> {
    let mut cursor = node.walk();
    let mut children = node.named_children(&mut cursor);
    let key_node = children.next()?;
    let key = decode_json_string(key_node, content);
    let value_node = children.next();
    let value = value_node
        .and_then(|child| parse_json_value(child, content, depth + 1, max_depth))
        .unwrap_or(Value::Scalar);

    Some(MapEntry {
        key,
        span: Some(span_from_node(key_node)),
        value,
    })
}

fn parse_json_array(node: Node<'_>, content: &[u8], depth: u32, max_depth: u32) -> Vec<SeqEntry> {
    let mut entries = Vec::new();
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        let span = Some(span_from_node(child));
        if let Some(value) = parse_json_value(child, content, depth + 1, max_depth) {
            entries.push(SeqEntry { value, span });
        }
    }
    entries
}

fn decode_json_string(node: Node<'_>, content: &[u8]) -> String {
    let Some(text) = node_text(node, content) else {
        return String::new();
    };
    let trimmed = text.trim();
    if trimmed.len() < 2 {
        return trimmed.to_string();
    }
    let bytes = trimmed.as_bytes();
    if bytes[0] != b'"' || bytes[trimmed.len() - 1] != b'"' {
        return trimmed.to_string();
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    let mut output = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(escaped) = chars.next() {
                match escaped {
                    'n' => output.push('\n'),
                    'r' => output.push('\r'),
                    't' => output.push('\t'),
                    '\\' => output.push('\\'),
                    '"' => output.push('"'),
                    '/' => output.push('/'),
                    'b' => output.push('\u{0008}'),
                    'f' => output.push('\u{000C}'),
                    'u' => {
                        let Some(codepoint) = decode_unicode_escape(&mut chars) else {
                            output.push('\u{FFFD}');
                            continue;
                        };
                        // Handle UTF-16 surrogate pairs
                        if (0xD800..=0xDBFF).contains(&codepoint) {
                            let high = codepoint;
                            if chars.next() == Some('\\') && chars.next() == Some('u') {
                                let Some(low) = decode_unicode_escape(&mut chars) else {
                                    output.push('\u{FFFD}');
                                    continue;
                                };
                                if (0xDC00..=0xDFFF).contains(&low) {
                                    let combined =
                                        0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                                    if let Some(c) = char::from_u32(combined) {
                                        output.push(c);
                                    } else {
                                        output.push('\u{FFFD}');
                                    }
                                } else {
                                    output.push('\u{FFFD}');
                                }
                            } else {
                                output.push('\u{FFFD}');
                            }
                        } else if let Some(c) = char::from_u32(codepoint) {
                            output.push(c);
                        } else {
                            output.push('\u{FFFD}');
                        }
                    }
                    _ => output.push(escaped),
                }
            }
        } else {
            output.push(ch);
        }
    }
    output
}

/// Parse exactly 4 hex digits from the iterator into a `u32` codepoint.
/// Returns `None` if any digit is missing or not a valid hex character.
fn decode_unicode_escape(chars: &mut std::str::Chars<'_>) -> Option<u32> {
    let mut codepoint: u32 = 0;
    for _ in 0..4 {
        let digit = chars.next().and_then(|c| c.to_digit(16))?;
        codepoint = (codepoint << 4) | digit;
    }
    Some(codepoint)
}

fn node_text(node: Node<'_>, content: &[u8]) -> Option<String> {
    node.utf8_text(content).ok().map(str::to_string)
}

fn span_from_node(node: Node<'_>) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        Position::new(start.row, start.column),
        Position::new(end.row, end.column),
    )
}

// ─── Iterative Graph Emission (Layer 2) ─────────────────────────────────────

/// Mutable state carried through the iterative tree walk.
struct WalkContext {
    module_id: NodeId,
    profile: Profile,
    node_count: u32,
    max_nodes: u32,
    max_depth: u32,
}

impl WalkContext {
    fn new(module_id: NodeId, profile: Profile) -> Self {
        Self {
            module_id,
            profile,
            node_count: 0,
            max_nodes: json_max_nodes(),
            max_depth: json_max_depth(),
        }
    }

    fn at_limit(&self) -> bool {
        self.node_count >= self.max_nodes
    }

    /// Increment node count. Returns `false` if limit has been reached.
    fn inc(&mut self) -> bool {
        self.node_count += 1;
        self.node_count <= self.max_nodes
    }
}

/// Stack frame for iterative value traversal.
struct WalkFrame<'a> {
    prefix: String,
    parent_id: NodeId,
    parent_key: Option<String>,
    depth: u32,
    value: &'a Value,
}

fn walk_value(root: &Value, ctx: &mut WalkContext, helper: &mut GraphBuildHelper) {
    let mut stack: Vec<WalkFrame<'_>> = Vec::new();

    // Seed from root value
    match root {
        Value::Map(entries) => {
            for entry in entries {
                if ctx.at_limit() {
                    return;
                }
                let qname = escape_segment(&entry.key);
                let kind = ctx.profile.node_kind_for(None, 0);
                let node_id = helper.add_node(&qname, entry.span, kind);
                if !ctx.inc() {
                    return;
                }
                helper.add_defines_edge(ctx.module_id, node_id);

                if ctx.profile.needs_import_edge(None, 0) {
                    helper.add_import_edge(ctx.module_id, node_id);
                }

                if matches!(entry.value, Value::Map(_) | Value::Seq(_)) {
                    stack.push(WalkFrame {
                        prefix: qname,
                        parent_id: node_id,
                        parent_key: Some(entry.key.clone()),
                        depth: 1,
                        value: &entry.value,
                    });
                }
            }
        }
        Value::Seq(entries) => {
            for (i, entry) in entries.iter().enumerate() {
                if ctx.at_limit() {
                    return;
                }
                let qname = format!("[{i}]");
                let node_id = helper.add_variable(&qname, entry.span);
                if !ctx.inc() {
                    return;
                }
                helper.add_defines_edge(ctx.module_id, node_id);

                if matches!(entry.value, Value::Map(_) | Value::Seq(_)) {
                    stack.push(WalkFrame {
                        prefix: qname,
                        parent_id: node_id,
                        parent_key: None,
                        depth: 1,
                        value: &entry.value,
                    });
                }
            }
        }
        _ => {}
    }

    // Process stack iteratively
    while let Some(frame) = stack.pop() {
        if frame.depth >= ctx.max_depth || ctx.at_limit() {
            continue;
        }

        match frame.value {
            Value::Map(entries) => {
                for entry in entries {
                    if ctx.at_limit() {
                        return;
                    }
                    let qname = format!("{}.{}", frame.prefix, escape_segment(&entry.key));
                    let kind = ctx
                        .profile
                        .node_kind_for(frame.parent_key.as_deref(), frame.depth);
                    let node_id = helper.add_node(&qname, entry.span, kind);
                    if !ctx.inc() {
                        return;
                    }
                    helper.add_contains_edge(frame.parent_id, node_id);

                    if ctx
                        .profile
                        .needs_import_edge(frame.parent_key.as_deref(), frame.depth)
                    {
                        helper.add_import_edge(ctx.module_id, node_id);
                    }

                    if matches!(entry.value, Value::Map(_) | Value::Seq(_)) {
                        stack.push(WalkFrame {
                            prefix: qname,
                            parent_id: node_id,
                            parent_key: Some(entry.key.clone()),
                            depth: frame.depth + 1,
                            value: &entry.value,
                        });
                    }
                }
            }
            Value::Seq(entries) => {
                for (i, entry) in entries.iter().enumerate() {
                    if ctx.at_limit() {
                        return;
                    }
                    let qname = format!("{}.[{i}]", frame.prefix);
                    let node_id = helper.add_variable(&qname, entry.span);
                    if !ctx.inc() {
                        return;
                    }
                    helper.add_contains_edge(frame.parent_id, node_id);

                    if matches!(entry.value, Value::Map(_) | Value::Seq(_)) {
                        stack.push(WalkFrame {
                            prefix: qname,
                            parent_id: node_id,
                            parent_key: None,
                            depth: frame.depth + 1,
                            value: &entry.value,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

// ─── GraphBuilder Implementation ────────────────────────────────────────────

/// Graph builder for JSON config files.
#[derive(Debug, Default)]
pub struct JsonGraphBuilder;

impl GraphBuilder for JsonGraphBuilder {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()> {
        if is_excluded(file) {
            return Ok(());
        }

        let max_depth = json_max_depth();
        let Some(root_value) = parse_json_value(tree.root_node(), content, 0, max_depth) else {
            return Ok(());
        };

        let profile = Profile::detect(file);

        let mut helper = GraphBuildHelper::new(staging, file, Language::Json);
        let module_id = helper.add_module("<module>", None);

        let mut ctx = WalkContext::new(module_id, profile);
        walk_value(&root_value, &mut ctx, &mut helper);

        Ok(())
    }

    fn language(&self) -> Language {
        Language::Json
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_segment_no_special_chars() {
        assert_eq!(escape_segment("hello"), "hello");
    }

    #[test]
    fn test_escape_segment_with_dot() {
        assert_eq!(escape_segment("my.key"), "my\\.key");
    }

    #[test]
    fn test_escape_segment_with_backslash() {
        assert_eq!(escape_segment("back\\slash"), "back\\\\slash");
    }

    #[test]
    fn test_escape_segment_with_both() {
        assert_eq!(escape_segment("a.b\\c"), "a\\.b\\\\c");
    }

    #[test]
    fn test_is_excluded_lockfile() {
        assert!(is_excluded(Path::new("package-lock.json")));
        assert!(is_excluded(Path::new("/path/to/package-lock.json")));
    }

    #[test]
    fn test_is_excluded_shrinkwrap() {
        assert!(is_excluded(Path::new("shrinkwrap.json")));
        assert!(is_excluded(Path::new("npm-shrinkwrap.json")));
    }

    #[test]
    fn test_is_excluded_minified() {
        assert!(is_excluded(Path::new("bundle.min.json")));
    }

    #[test]
    fn test_is_not_excluded_normal() {
        assert!(!is_excluded(Path::new("data.json")));
        assert!(!is_excluded(Path::new("package.json")));
        assert!(!is_excluded(Path::new("now-ui.json")));
    }

    #[test]
    fn test_decode_unicode_escape_basic() {
        let mut chars = "0041".chars();
        assert_eq!(decode_unicode_escape(&mut chars), Some(0x0041)); // 'A'
    }

    #[test]
    fn test_decode_unicode_escape_accented() {
        let mut chars = "00e9".chars();
        assert_eq!(decode_unicode_escape(&mut chars), Some(0x00E9)); // 'é'
    }

    #[test]
    fn test_decode_unicode_escape_short_input() {
        let mut chars = "00".chars();
        assert_eq!(decode_unicode_escape(&mut chars), None);
    }

    #[test]
    fn test_decode_unicode_escape_invalid_hex() {
        let mut chars = "12G4".chars();
        assert_eq!(decode_unicode_escape(&mut chars), None);
    }
}
