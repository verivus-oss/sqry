//! Query adapter for unified graph.
//!
//! This module provides query helpers for the unified graph (Arena+CSR storage)
//! used by graph-native query evaluation.
//!
//! # Design (FR-2025-007)
//!
//! The adapter provides:
//! - `GraphQueryAdapter`: A thin wrapper around `CodeGraph`
//! - Scope and reference helpers for graph-native queries

use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::node::NodeKind as UnifiedNodeKind;
use std::path::PathBuf;

/// Query adapter for the unified graph.
///
/// Wraps a `CodeGraph` and exposes query-focused helpers.
pub struct GraphQueryAdapter<'a> {
    graph: &'a CodeGraph,
}

// ============================================================================
// Scope and Reference Info Types (v2.0.0+)
// ============================================================================

/// Information about a scope/container in the graph.
///
/// Used by scope predicates (`scope.type`, `scope.name`, `scope.parent`, `scope.ancestor`).
#[derive(Debug, Clone)]
pub struct ScopeInfo {
    /// The node ID of this scope
    pub node_id: crate::graph::unified::node::NodeId,
    /// Scope type (derived from `NodeKind`, e.g., "class", "function", "module")
    pub scope_type: String,
    /// Scope name
    pub name: String,
}

/// Information about a reference to a symbol.
#[derive(Debug, Clone)]
pub struct ReferenceInfo {
    /// The node ID of the referencing symbol
    pub source_node_id: crate::graph::unified::node::NodeId,
    /// The kind of reference (Calls, References, Imports, etc.)
    pub reference_kind: crate::graph::unified::edge::EdgeKind,
    /// File path where the reference occurs
    pub file_path: PathBuf,
    /// Line number of the reference
    pub line: usize,
}

impl<'a> GraphQueryAdapter<'a> {
    /// Creates a new adapter for the given graph.
    #[must_use]
    pub fn new(graph: &'a CodeGraph) -> Self {
        Self { graph }
    }

    // ========================================================================
    // Scope Query Methods (v2.0.0+)
    // ========================================================================

    /// Get the parent scope (container) of a node.
    ///
    /// Finds the container by looking for an incoming `Contains` edge.
    /// For example, a method's parent scope is its containing class.
    ///
    /// # Returns
    ///
    /// `Some(parent_node_id)` if the node has a parent container, `None` otherwise.
    #[must_use]
    pub fn get_parent_scope(
        &self,
        node_id: crate::graph::unified::node::NodeId,
    ) -> Option<crate::graph::unified::node::NodeId> {
        use crate::graph::unified::edge::EdgeKind;

        let edges = self.graph.edges();

        // Find incoming Contains edge (parent -> this node)
        for edge in edges.edges_to(node_id) {
            if matches!(edge.kind, EdgeKind::Contains) {
                return Some(edge.source);
            }
        }

        None
    }

    /// Get all ancestor scopes of a node (parent chain up to root).
    ///
    /// Returns the chain of containing scopes from immediate parent to top-level.
    /// For example, for a method in a nested class:
    /// `[immediate_class, outer_class, module]`
    ///
    /// # Returns
    ///
    /// Vector of ancestor node IDs, from immediate parent to root.
    #[must_use]
    pub fn get_ancestor_scopes(
        &self,
        node_id: crate::graph::unified::node::NodeId,
    ) -> Vec<crate::graph::unified::node::NodeId> {
        let mut ancestors = Vec::new();
        let mut current = node_id;

        // Walk up the parent chain
        while let Some(parent) = self.get_parent_scope(current) {
            ancestors.push(parent);
            current = parent;
        }

        ancestors
    }

    /// Get scope information for a node.
    ///
    /// Returns the scope type (derived from `NodeKind`) and name.
    ///
    /// # Returns
    ///
    /// `Some(ScopeInfo)` if the node exists, `None` otherwise.
    #[must_use]
    pub fn get_scope_info(
        &self,
        node_id: crate::graph::unified::node::NodeId,
    ) -> Option<ScopeInfo> {
        let arena = self.graph.nodes();
        let strings = self.graph.strings();

        let entry = arena.get(node_id)?;
        let name = strings.resolve(entry.name)?.to_string();
        let scope_type = node_kind_to_scope_type(entry.kind);

        Some(ScopeInfo {
            node_id,
            scope_type,
            name,
        })
    }

    /// Get the containing scope info for a node (its parent's scope info).
    ///
    /// This is a convenience method that combines `get_parent_scope` and `get_scope_info`.
    ///
    /// # Returns
    ///
    /// `Some(ScopeInfo)` of the parent container, `None` if no parent exists.
    #[must_use]
    pub fn get_containing_scope_info(
        &self,
        node_id: crate::graph::unified::node::NodeId,
    ) -> Option<ScopeInfo> {
        let parent_id = self.get_parent_scope(node_id)?;
        self.get_scope_info(parent_id)
    }

    // ========================================================================
    // Reference Query Methods (v2.0.0+)
    // ========================================================================

    /// Get all references to a node.
    ///
    /// Finds incoming `References`, `Calls`, `Imports`, and `FfiCall` edges.
    ///
    /// # Returns
    ///
    /// Vector of `ReferenceInfo` for all references to this node.
    #[must_use]
    pub fn get_references_to(
        &self,
        node_id: crate::graph::unified::node::NodeId,
    ) -> Vec<ReferenceInfo> {
        use crate::graph::unified::edge::EdgeKind;

        let edges = self.graph.edges();
        let arena = self.graph.nodes();
        let files = self.graph.files();

        let mut refs = Vec::new();

        for edge in edges.edges_to(node_id) {
            // Include References, Calls, Imports, and FfiCall edges
            let is_reference = matches!(
                &edge.kind,
                EdgeKind::References
                    | EdgeKind::Calls { .. }
                    | EdgeKind::Imports { .. }
                    | EdgeKind::FfiCall { .. }
            );

            if is_reference {
                // Get source node info for location
                let (file_path, line) = arena
                    .get(edge.source)
                    .map(|entry| {
                        let path = files
                            .resolve(entry.file)
                            .map(|s| PathBuf::from(s.as_ref()))
                            .unwrap_or_default();
                        (path, entry.start_line as usize)
                    })
                    .unwrap_or_default();

                refs.push(ReferenceInfo {
                    source_node_id: edge.source,
                    reference_kind: edge.kind.clone(),
                    file_path,
                    line,
                });
            }
        }

        refs
    }

    /// Check if a node has any references (is referenced by other symbols).
    ///
    /// This is more efficient than `get_references_to` when you only need
    /// to check existence, not enumerate all references.
    ///
    /// # Returns
    ///
    /// `true` if the node has at least one incoming reference edge.
    #[must_use]
    pub fn node_has_references(&self, node_id: crate::graph::unified::node::NodeId) -> bool {
        use crate::graph::unified::edge::EdgeKind;

        let edges = self.graph.edges();

        edges.edges_to(node_id).iter().any(|edge| {
            matches!(
                &edge.kind,
                EdgeKind::References
                    | EdgeKind::Calls { .. }
                    | EdgeKind::Imports { .. }
                    | EdgeKind::FfiCall { .. }
            )
        })
    }

    /// Find nodes by name and return their references.
    ///
    /// This is useful for implementing `references:symbol_name` predicate.
    ///
    /// # Arguments
    ///
    /// * `symbol_name` - The name to search for (exact match or suffix match with `::`)
    ///
    /// # Returns
    ///
    /// Vector of `ReferenceInfo` for all references to matching symbols.
    #[must_use]
    pub fn find_references_to_symbol(&self, symbol_name: &str) -> Vec<ReferenceInfo> {
        let indices = self.graph.indices();
        let arena = self.graph.nodes();
        let strings = self.graph.strings();

        let mut all_refs = Vec::new();

        // Try to find the exact StringId for this symbol name
        if let Some(string_id) = strings.get(symbol_name) {
            // Look up nodes by the interned name
            for &node_id in indices.by_name(string_id) {
                all_refs.extend(self.get_references_to(node_id));
            }
        }

        // Also check for qualified names ending with ::symbol_name
        let suffix = format!("::{symbol_name}");
        for (node_id, entry) in arena.iter() {
            if let Some(name) = strings.resolve(entry.name)
                && name.ends_with(&suffix)
            {
                all_refs.extend(self.get_references_to(node_id));
            }
        }

        all_refs
    }

    /// Returns a reference to the underlying graph.
    #[must_use]
    pub fn graph(&self) -> &CodeGraph {
        self.graph
    }
}

/// Convert `NodeKind` to scope type string for predicate matching.
fn node_kind_to_scope_type(kind: UnifiedNodeKind) -> String {
    match kind {
        UnifiedNodeKind::Function | UnifiedNodeKind::Test => "function".to_string(),
        UnifiedNodeKind::Method => "method".to_string(),
        UnifiedNodeKind::Class | UnifiedNodeKind::Service => "class".to_string(),
        UnifiedNodeKind::Interface | UnifiedNodeKind::Trait => "interface".to_string(),
        UnifiedNodeKind::Struct => "struct".to_string(),
        UnifiedNodeKind::Enum => "enum".to_string(),
        UnifiedNodeKind::Module => "module".to_string(),
        UnifiedNodeKind::Macro => "macro".to_string(),
        UnifiedNodeKind::Component => "component".to_string(),
        UnifiedNodeKind::Resource | UnifiedNodeKind::Endpoint => "resource".to_string(),
        // Non-container types
        UnifiedNodeKind::Variable
        | UnifiedNodeKind::Constant
        | UnifiedNodeKind::Parameter
        | UnifiedNodeKind::Property
        | UnifiedNodeKind::EnumVariant
        | UnifiedNodeKind::Type
        | UnifiedNodeKind::Import
        | UnifiedNodeKind::Export
        | UnifiedNodeKind::CallSite
        | UnifiedNodeKind::Other
        | UnifiedNodeKind::Lifetime
        | UnifiedNodeKind::StyleRule
        | UnifiedNodeKind::StyleAtRule
        | UnifiedNodeKind::StyleVariable => kind.as_str().to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::BidirectionalEdgeStore;
    use crate::graph::unified::edge::{EdgeKind, FfiConvention};
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::{AuxiliaryIndices, FileRegistry};
    use crate::graph::unified::storage::{NodeArena, NodeEntry, StringInterner};
    use std::path::Path;

    /// Build a minimal `CodeGraph` with two function nodes connected by an `FfiCall` edge.
    ///
    /// Graph: `caller --FfiCall(C)--> ffi_target`
    fn build_graph_with_ffi_edge() -> (
        CodeGraph,
        crate::graph::unified::node::NodeId,
        crate::graph::unified::node::NodeId,
    ) {
        let mut arena = NodeArena::new();
        let edges = BidirectionalEdgeStore::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let mut indices = AuxiliaryIndices::new();

        let caller_name = strings.intern("caller_fn").unwrap();
        let target_name = strings.intern("ffi_target").unwrap();
        let file_id = files.register(Path::new("test.r")).unwrap();

        let caller_id = arena
            .alloc(NodeEntry {
                kind: NodeKind::Function,
                name: caller_name,
                file: file_id,
                start_byte: 0,
                end_byte: 100,
                start_line: 1,
                start_column: 0,
                end_line: 5,
                end_column: 0,
                signature: None,
                doc: None,
                qualified_name: None,
                visibility: None,
                is_async: false,
                is_static: false,
                is_unsafe: false,
                body_hash: None,
            })
            .unwrap();

        let target_id = arena
            .alloc(NodeEntry {
                kind: NodeKind::Function,
                name: target_name,
                file: file_id,
                start_byte: 200,
                end_byte: 300,
                start_line: 10,
                start_column: 0,
                end_line: 15,
                end_column: 0,
                signature: None,
                doc: None,
                qualified_name: None,
                visibility: None,
                is_async: false,
                is_static: false,
                is_unsafe: false,
                body_hash: None,
            })
            .unwrap();

        indices.add(caller_id, NodeKind::Function, caller_name, None, file_id);
        indices.add(target_id, NodeKind::Function, target_name, None, file_id);

        edges.add_edge(
            caller_id,
            target_id,
            EdgeKind::FfiCall {
                convention: FfiConvention::C,
            },
            file_id,
        );

        let graph = CodeGraph::from_components(arena, edges, strings, files, indices);
        (graph, caller_id, target_id)
    }

    #[test]
    fn test_ffi_call_edge_in_get_references_to() {
        let (graph, caller_id, target_id) = build_graph_with_ffi_edge();
        let adapter = GraphQueryAdapter::new(&graph);

        let refs = adapter.get_references_to(target_id);
        assert_eq!(
            refs.len(),
            1,
            "FfiCall edge should be included in references"
        );
        assert_eq!(refs[0].source_node_id, caller_id);
        assert!(
            matches!(
                &refs[0].reference_kind,
                EdgeKind::FfiCall {
                    convention: FfiConvention::C
                }
            ),
            "reference kind should be FfiCall with C convention"
        );
    }

    #[test]
    fn test_ffi_call_edge_in_node_has_references() {
        let (graph, _caller_id, target_id) = build_graph_with_ffi_edge();
        let adapter = GraphQueryAdapter::new(&graph);

        assert!(
            adapter.node_has_references(target_id),
            "node_has_references should return true for FfiCall target"
        );
    }
}
