//! Pass 3: Intra-file Edges - Create edges within a single file.
//!
//! This module implements the third pass of the build pipeline:
//! creating CALLS and REFERENCES edges for relationships within a file.
//!
//! # Overview
//!
//! Pass 3 analyzes references within a single file:
//! - Function calls (caller → callee CALLS edges)
//! - Type references (usage → type REFERENCES edges)
//! - Variable references
//!
//! # Input
//!
//! Uses the `IntraFileReference` type for representing references:
//! - `source_qualified_name` - The qualified name of the symbol making the reference
//! - `target_qualified_name` - Qualified name of the referenced symbol
//! - `kind` - Type of reference (call, type, etc.)

use std::collections::HashMap;

use super::super::edge::EdgeKind;
use super::super::file::FileId;
use super::super::node::NodeId;
use crate::graph::node::Span;

/// Statistics from Pass 3 execution.
#[derive(Debug, Clone, Default)]
pub struct Pass3Stats {
    /// Number of references processed.
    pub references_processed: usize,
    /// Number of CALLS edges created.
    pub calls_edges_created: usize,
    /// Number of REFERENCES edges created.
    pub references_edges_created: usize,
    /// Number of unresolved references (target not found).
    pub unresolved_references: usize,
}

/// Pending edge to be added to the graph.
#[derive(Debug, Clone)]
pub struct PendingEdge {
    /// Source node.
    pub source: NodeId,
    /// Target node.
    pub target: NodeId,
    /// Edge kind.
    pub kind: EdgeKind,
    /// File containing the edge (for partitioning).
    pub file: FileId,
    /// Source spans of the edge (e.g., call site locations for LSP call hierarchy).
    /// Typically has one span per reference; multiple spans are accumulated during merge.
    pub spans: Vec<Span>,
}

/// An unresolved reference for cross-file linking in Pass 4.
///
/// When Pass 3 cannot resolve a target symbol within the same file,
/// this struct captures the data needed for Pass 4 to create
/// cross-file CALLS/REFERENCES edges.
#[derive(Debug, Clone)]
pub struct UnresolvedRef {
    /// Source node that makes the reference.
    pub source_node: NodeId,
    /// Name of the target symbol (to be resolved in Pass 4).
    pub target_name: String,
    /// Kind of reference (call, `type_reference`, etc.).
    pub kind: EdgeKind,
    /// File containing the source of this reference.
    pub source_file: FileId,
    /// Line number of the reference.
    pub line: usize,
    /// Column number of the reference.
    pub column: usize,
    /// Whether explicit location data was provided.
    /// When false, line/column are defaults and should not be used for spans.
    pub has_location: bool,
}

/// A reference from one node to another within a file.
///
/// This is a simplified representation for the build pipeline,
/// separate from the legacy reference model used before graph-native edges.
#[derive(Debug, Clone)]
pub struct IntraFileReference {
    /// Qualified name of the source node (the one making the reference).
    pub source_qualified_name: String,
    /// Qualified name of the target node (the one being referenced).
    pub target_qualified_name: String,
    /// Kind of reference (e.g., "call", "`type_reference`").
    pub kind: String,
    /// Line number of the reference.
    pub line: usize,
    /// Column number of the reference.
    pub column: usize,
    /// Whether explicit location data was provided.
    /// When false, line/column are defaults and should not be used for spans.
    pub has_location: bool,
}

impl IntraFileReference {
    /// Create a new intra-file reference without location data.
    #[must_use]
    pub fn new(source: String, target: String, kind: String) -> Self {
        Self {
            source_qualified_name: source,
            target_qualified_name: target,
            kind,
            line: 0,
            column: 0,
            has_location: false,
        }
    }

    /// Create a new intra-file reference with explicit location.
    #[must_use]
    pub fn with_location(
        source: String,
        target: String,
        kind: String,
        line: usize,
        column: usize,
    ) -> Self {
        Self {
            source_qualified_name: source,
            target_qualified_name: target,
            kind,
            line,
            column,
            has_location: true,
        }
    }
}

/// Result of Pass 3 execution.
#[derive(Debug)]
pub struct Pass3Result {
    /// Statistics for this pass.
    pub stats: Pass3Stats,
    /// Resolved edges within the file.
    pub edges: Vec<PendingEdge>,
    /// Unresolved references for Pass 4 cross-file linking.
    pub unresolved: Vec<UnresolvedRef>,
}

/// Create intra-file edges from references.
///
/// Analyzes the references within a file and creates edges between nodes.
/// Returns both resolved intra-file edges and unresolved references
/// that need cross-file linking in Pass 4.
///
/// # Arguments
///
/// * `references` - References extracted by the language plugin
/// * `symbol_to_node` - Mapping from symbol qualified name to `NodeId`
/// * `file_id` - `FileId` for this file
///
/// # Returns
///
/// `Pass3Result` containing stats, resolved edges, and unresolved references.
#[must_use]
pub fn pass3_intra_edges<S: std::hash::BuildHasher>(
    references: &[IntraFileReference],
    symbol_to_node: &HashMap<String, NodeId, S>,
    file_id: FileId,
) -> Pass3Result {
    let mut stats = Pass3Stats::default();
    let mut edges = Vec::new();
    let mut unresolved = Vec::new();

    for reference in references {
        stats.references_processed += 1;

        // Determine edge kind upfront
        let edge_kind = match reference.kind.as_str() {
            "call" | "function_call" | "method_call" => EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            _ => EdgeKind::References,
        };

        // Find source node
        let Some(&source_id) = symbol_to_node.get(&reference.source_qualified_name) else {
            // Source not found - can't create edge or unresolved ref
            stats.unresolved_references += 1;
            continue;
        };

        // Find target node
        let Some(&target_id) = symbol_to_node.get(&reference.target_qualified_name) else {
            // Target not found - record for Pass 4 cross-file linking
            stats.unresolved_references += 1;
            unresolved.push(UnresolvedRef {
                source_node: source_id,
                target_name: reference.target_qualified_name.clone(),
                kind: edge_kind,
                source_file: file_id,
                line: reference.line,
                column: reference.column,
                has_location: reference.has_location,
            });
            continue;
        };

        // Both found - create intra-file edge
        match edge_kind {
            EdgeKind::Calls { .. } => stats.calls_edges_created += 1,
            _ => stats.references_edges_created += 1,
        }

        // Create span from reference location only if explicit location was provided
        // This avoids creating bogus (0,0) spans when no location data is available
        let spans = if reference.has_location {
            vec![Span {
                start: crate::graph::node::Position {
                    line: reference.line,
                    column: reference.column,
                },
                // End position same as start (point span) since we don't have end info
                end: crate::graph::node::Position {
                    line: reference.line,
                    column: reference.column,
                },
            }]
        } else {
            vec![]
        };

        edges.push(PendingEdge {
            source: source_id,
            target: target_id,
            kind: edge_kind,
            file: file_id,
            spans,
        });
    }

    Pass3Result {
        stats,
        edges,
        unresolved,
    }
}

/// Legacy function for backward compatibility.
///
/// Use `pass3_intra_edges` which returns full `Pass3Result` instead.
#[deprecated(
    since = "0.1.0",
    note = "Use pass3_intra_edges which returns Pass3Result"
)]
#[must_use]
pub fn pass3_intra_edges_legacy<S: std::hash::BuildHasher>(
    references: &[IntraFileReference],
    symbol_to_node: &HashMap<String, NodeId, S>,
    file_id: FileId,
) -> (Pass3Stats, Vec<PendingEdge>) {
    let result = pass3_intra_edges(references, symbol_to_node, file_id);
    (result.stats, result.edges)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_reference(source: &str, target: &str, kind: &str) -> IntraFileReference {
        IntraFileReference::new(source.to_string(), target.to_string(), kind.to_string())
    }

    #[test]
    fn test_pass3_creates_call_edge() {
        let mut symbol_to_node = HashMap::new();
        let caller_id = NodeId::new(0, 1);
        let target_id = NodeId::new(1, 1);
        symbol_to_node.insert("caller".to_string(), caller_id);
        symbol_to_node.insert("callee".to_string(), target_id);

        let references = vec![create_test_reference("caller", "callee", "call")];
        let file_id = FileId::new(0);

        let result = pass3_intra_edges(&references, &symbol_to_node, file_id);

        assert_eq!(result.stats.references_processed, 1);
        assert_eq!(result.stats.calls_edges_created, 1);
        assert_eq!(result.stats.unresolved_references, 0);
        assert_eq!(result.edges.len(), 1);
        assert!(result.unresolved.is_empty());

        let edge = &result.edges[0];
        assert_eq!(edge.source, caller_id);
        assert_eq!(edge.target, target_id);
        assert!(matches!(
            edge.kind,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false
            }
        ));
    }

    #[test]
    fn test_pass3_creates_reference_edge() {
        let mut symbol_to_node = HashMap::new();
        let user_id = NodeId::new(0, 1);
        let type_id = NodeId::new(1, 1);
        symbol_to_node.insert("user_func".to_string(), user_id);
        symbol_to_node.insert("MyType".to_string(), type_id);

        let references = vec![create_test_reference(
            "user_func",
            "MyType",
            "type_reference",
        )];
        let file_id = FileId::new(0);

        let result = pass3_intra_edges(&references, &symbol_to_node, file_id);

        assert_eq!(result.stats.references_processed, 1);
        assert_eq!(result.stats.references_edges_created, 1);
        assert_eq!(result.edges.len(), 1);
        assert!(matches!(result.edges[0].kind, EdgeKind::References));
    }

    #[test]
    fn test_pass3_unresolved_source() {
        let mut symbol_to_node = HashMap::new();
        let target_id = NodeId::new(1, 1);
        symbol_to_node.insert("target".to_string(), target_id);

        let references = vec![create_test_reference("unknown_source", "target", "call")];
        let file_id = FileId::new(0);

        let result = pass3_intra_edges(&references, &symbol_to_node, file_id);

        assert_eq!(result.stats.references_processed, 1);
        assert_eq!(result.stats.unresolved_references, 1);
        assert!(result.edges.is_empty());
        // Source not found, so no unresolved ref created
        assert!(result.unresolved.is_empty());
    }

    #[test]
    fn test_pass3_unresolved_target() {
        let mut symbol_to_node = HashMap::new();
        let source_id = NodeId::new(0, 1);
        symbol_to_node.insert("source".to_string(), source_id);

        let references = vec![create_test_reference("source", "unknown_target", "call")];
        let file_id = FileId::new(0);

        let result = pass3_intra_edges(&references, &symbol_to_node, file_id);

        assert_eq!(result.stats.references_processed, 1);
        assert_eq!(result.stats.unresolved_references, 1);
        assert!(result.edges.is_empty());
        // Source found but target not - creates unresolved ref for Pass 4
        assert_eq!(result.unresolved.len(), 1);
        assert_eq!(result.unresolved[0].source_node, source_id);
        assert_eq!(result.unresolved[0].target_name, "unknown_target");
        assert!(matches!(
            result.unresolved[0].kind,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false
            }
        ));
    }

    #[test]
    fn test_pass3_multiple_edges() {
        let mut symbol_to_node = HashMap::new();
        let func_a = NodeId::new(0, 1);
        let func_b = NodeId::new(1, 1);
        let func_c = NodeId::new(2, 1);
        symbol_to_node.insert("func_a".to_string(), func_a);
        symbol_to_node.insert("func_b".to_string(), func_b);
        symbol_to_node.insert("func_c".to_string(), func_c);

        let references = vec![
            create_test_reference("func_a", "func_b", "call"),
            create_test_reference("func_a", "func_c", "call"),
            create_test_reference("func_b", "func_c", "call"),
        ];
        let file_id = FileId::new(0);

        let result = pass3_intra_edges(&references, &symbol_to_node, file_id);

        assert_eq!(result.stats.references_processed, 3);
        assert_eq!(result.stats.calls_edges_created, 3);
        assert_eq!(result.edges.len(), 3);
    }

    #[test]
    fn test_pass3_mixed_resolved_unresolved() {
        let mut symbol_to_node = HashMap::new();
        let func_a = NodeId::new(0, 1);
        let func_b = NodeId::new(1, 1);
        symbol_to_node.insert("func_a".to_string(), func_a);
        symbol_to_node.insert("func_b".to_string(), func_b);

        let references = vec![
            create_test_reference("func_a", "func_b", "call"), // resolved
            create_test_reference("func_a", "external_func", "call"), // unresolved
            create_test_reference("func_b", "external_type", "type"), // unresolved
        ];
        let file_id = FileId::new(0);

        let result = pass3_intra_edges(&references, &symbol_to_node, file_id);

        assert_eq!(result.stats.references_processed, 3);
        assert_eq!(result.stats.calls_edges_created, 1);
        assert_eq!(result.stats.unresolved_references, 2);
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.unresolved.len(), 2);

        // Verify unresolved refs have correct data
        assert_eq!(result.unresolved[0].target_name, "external_func");
        assert_eq!(result.unresolved[1].target_name, "external_type");
    }

    #[test]
    fn test_intra_file_reference_new() {
        let reference = IntraFileReference::new(
            "my_module::caller".to_string(),
            "my_module::callee".to_string(),
            "call".to_string(),
        );

        assert_eq!(reference.source_qualified_name, "my_module::caller");
        assert_eq!(reference.target_qualified_name, "my_module::callee");
        assert_eq!(reference.kind, "call");
        assert_eq!(reference.line, 0);
        assert_eq!(reference.column, 0);
    }

    #[test]
    fn test_intra_file_reference_with_location() {
        let reference = IntraFileReference::with_location(
            "caller".to_string(),
            "callee".to_string(),
            "call".to_string(),
            42,
            10,
        );

        assert_eq!(reference.line, 42);
        assert_eq!(reference.column, 10);
    }

    #[test]
    fn test_unresolved_ref_preserves_location() {
        let mut symbol_to_node = HashMap::new();
        let source_id = NodeId::new(0, 1);
        symbol_to_node.insert("source".to_string(), source_id);

        let references = vec![IntraFileReference::with_location(
            "source".to_string(),
            "unknown".to_string(),
            "call".to_string(),
            100,
            25,
        )];
        let file_id = FileId::new(5);

        let result = pass3_intra_edges(&references, &symbol_to_node, file_id);

        assert_eq!(result.unresolved.len(), 1);
        assert_eq!(result.unresolved[0].line, 100);
        assert_eq!(result.unresolved[0].column, 25);
        assert_eq!(result.unresolved[0].source_file, file_id);
    }
}
