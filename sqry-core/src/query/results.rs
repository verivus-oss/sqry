//! Query results with Arc-based ownership for zero-copy access.
//!
//! This module provides the `QueryResults` type which wraps matched `NodeId`s
//! with the source `CodeGraph`, enabling zero-copy access to node data.
//!
//! # Design (v6 - CodeGraph-Native Query Executor)
//!
//! The interners return `Arc<str>` and `Arc<Path>`, so `QueryResults` uses
//! these types for efficient, reference-counted string/path access.
//!
//! # Usage
//!
//! ```ignore
//! let results = executor.execute_on_graph("kind:function", path)?;
//! for m in results.iter() {
//!     println!("{}: {} at line {}", m.kind().as_str(), m.name().unwrap_or_default(), m.start_line());
//! }
//! ```

use crate::graph::node::Language;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::node::{NodeId, NodeKind};
use crate::graph::unified::storage::arena::NodeEntry;
use crate::query::pipeline::AggregationResult;
use crate::query::types::JoinEdgeKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Query results containing matched node IDs and the source graph.
///
/// This type stores the matched `NodeId`s along with an `Arc<CodeGraph>` reference,
/// enabling zero-copy access to node names, paths, and other data through the
/// string interners.
pub struct QueryResults {
    /// The graph (Arc for cheap cloning across threads)
    graph: Arc<CodeGraph>,
    /// Matched node IDs (8 bytes each, no allocation)
    matches: Vec<NodeId>,
    /// Workspace root for relative path display
    workspace_root: Option<PathBuf>,
}

impl QueryResults {
    /// Creates new query results from a graph and matched node IDs.
    #[must_use]
    pub fn new(graph: Arc<CodeGraph>, matches: Vec<NodeId>) -> Self {
        Self {
            graph,
            matches,
            workspace_root: None,
        }
    }

    /// Sets the workspace root for relative path resolution.
    #[must_use]
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Returns the number of matched nodes.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// Returns true if no nodes matched.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// Returns the matched node IDs.
    #[must_use]
    pub fn node_ids(&self) -> &[NodeId] {
        &self.matches
    }

    /// Returns a reference to the underlying graph.
    #[must_use]
    pub fn graph(&self) -> &CodeGraph {
        &self.graph
    }

    /// Test-only accessor for the underlying `Arc<CodeGraph>`.
    ///
    /// Used by unit tests that need to compare graph identity via
    /// [`Arc::ptr_eq`] (e.g., verifying that the preloaded-graph execution
    /// path threads the caller-supplied `Arc` through to the results without
    /// cloning the underlying graph data).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn graph_arc_for_test(&self) -> &Arc<CodeGraph> {
        &self.graph
    }

    /// Returns the workspace root, if set.
    #[must_use]
    pub fn workspace_root(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
    }

    /// Iterates over matched nodes with accessor methods.
    pub fn iter(&self) -> impl Iterator<Item = QueryMatch<'_>> + '_ {
        self.matches.iter().filter_map(|&id| {
            self.graph.nodes().get(id).map(|entry| QueryMatch {
                id,
                entry,
                graph: &self.graph,
                workspace_root: self.workspace_root.as_deref(),
            })
        })
    }

    /// Sort by resolved path, then line, then name for deterministic output.
    ///
    /// This ensures consistent CLI output regardless of graph traversal order.
    pub fn sort_by_location(&mut self) {
        let graph = &self.graph;
        self.matches.sort_by(|&a, &b| {
            let ea = graph.nodes().get(a);
            let eb = graph.nodes().get(b);
            match (ea, eb) {
                (Some(ea), Some(eb)) => {
                    // Sort by resolved path string, not FileId
                    let path_a = graph.files().resolve(ea.file);
                    let path_b = graph.files().resolve(eb.file);
                    // Resolve names for comparison
                    let name_a = graph.strings().resolve(ea.name);
                    let name_b = graph.strings().resolve(eb.name);
                    path_a
                        .cmp(&path_b)
                        .then(ea.start_line.cmp(&eb.start_line))
                        .then(name_a.cmp(&name_b))
                }
                _ => std::cmp::Ordering::Equal,
            }
        });
    }

    /// Takes the matched node IDs, consuming self.
    #[must_use]
    pub fn into_node_ids(self) -> Vec<NodeId> {
        self.matches
    }

    /// Consumes self and returns the underlying graph and matches.
    #[must_use]
    pub fn into_parts(self) -> (Arc<CodeGraph>, Vec<NodeId>) {
        (self.graph, self.matches)
    }
}

impl std::fmt::Debug for QueryResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryResults")
            .field("match_count", &self.matches.len())
            .field("graph", &self.graph)
            .field("workspace_root", &self.workspace_root)
            .finish()
    }
}

/// A single query match with accessors for node data.
///
/// This provides a convenient API for accessing node properties without
/// needing to manually resolve string IDs through the interners.
pub struct QueryMatch<'a> {
    /// The node ID
    pub id: NodeId,
    /// Reference to the node entry
    pub entry: &'a NodeEntry,
    /// Reference to the graph for string resolution
    graph: &'a CodeGraph,
    /// Workspace root for relative path computation
    workspace_root: Option<&'a Path>,
}

impl QueryMatch<'_> {
    /// Returns the node's name (zero-copy from interner).
    #[must_use]
    pub fn name(&self) -> Option<Arc<str>> {
        self.graph.strings().resolve(self.entry.name)
    }

    /// Returns the node's kind.
    #[must_use]
    pub fn kind(&self) -> NodeKind {
        self.entry.kind
    }

    /// Returns the file path (zero-copy from file registry).
    #[must_use]
    pub fn file_path(&self) -> Option<Arc<Path>> {
        self.graph.files().resolve(self.entry.file)
    }

    /// Returns the file path relative to workspace root, if set.
    #[must_use]
    pub fn relative_path(&self) -> Option<PathBuf> {
        let path = self.file_path()?;
        if let Some(root) = self.workspace_root {
            path.strip_prefix(root)
                .ok()
                .map(std::path::Path::to_path_buf)
        } else {
            Some(path.to_path_buf())
        }
    }

    /// Returns the start line (1-indexed).
    #[inline]
    #[must_use]
    pub fn start_line(&self) -> u32 {
        self.entry.start_line
    }

    /// Returns the end line (1-indexed).
    #[inline]
    #[must_use]
    pub fn end_line(&self) -> u32 {
        self.entry.end_line
    }

    /// Returns the start column (0-indexed).
    #[inline]
    #[must_use]
    pub fn start_column(&self) -> u32 {
        self.entry.start_column
    }

    /// Returns the end column (0-indexed).
    #[inline]
    #[must_use]
    pub fn end_column(&self) -> u32 {
        self.entry.end_column
    }

    /// Returns the start byte offset.
    #[inline]
    #[must_use]
    pub fn start_byte(&self) -> u32 {
        self.entry.start_byte
    }

    /// Returns the end byte offset.
    #[inline]
    #[must_use]
    pub fn end_byte(&self) -> u32 {
        self.entry.end_byte
    }

    /// Returns the visibility modifier (e.g., "public", "private").
    ///
    /// Returns `None` if no visibility is set, which typically means "private".
    #[must_use]
    pub fn visibility(&self) -> Option<Arc<str>> {
        self.entry
            .visibility
            .and_then(|id| self.graph.strings().resolve(id))
    }

    /// Returns the signature/type information, if available.
    #[must_use]
    pub fn signature(&self) -> Option<Arc<str>> {
        self.entry
            .signature
            .and_then(|id| self.graph.strings().resolve(id))
    }

    /// Returns the qualified name (e.g., "module.Class.method"), if available.
    #[must_use]
    pub fn qualified_name(&self) -> Option<Arc<str>> {
        self.entry
            .qualified_name
            .and_then(|id| self.graph.strings().resolve(id))
    }

    /// Returns the docstring, if available.
    #[must_use]
    pub fn doc(&self) -> Option<Arc<str>> {
        self.entry
            .doc
            .and_then(|id| self.graph.strings().resolve(id))
    }

    /// Returns whether this is an async function/method.
    #[inline]
    #[must_use]
    pub fn is_async(&self) -> bool {
        self.entry.is_async
    }

    /// Returns whether this is a static member.
    #[inline]
    #[must_use]
    pub fn is_static(&self) -> bool {
        self.entry.is_static
    }

    /// Get language from file registry (NOT from `NodeEntry` field).
    ///
    /// Language is determined by the file's extension through the `FileRegistry`,
    /// not stored per-node. This matches how `sqry index` determines language.
    #[must_use]
    pub fn language(&self) -> Option<Language> {
        self.graph.files().language_for_file(self.entry.file)
    }

    /// Access the underlying graph for edge-based field evaluation.
    #[must_use]
    pub fn graph(&self) -> &CodeGraph {
        self.graph
    }
}

impl std::fmt::Debug for QueryMatch<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryMatch")
            .field("id", &self.id)
            .field("kind", &self.entry.kind)
            .field("name", &self.name())
            .field("start_line", &self.entry.start_line)
            .finish()
    }
}

/// Join query results containing matched node pairs connected by edges.
///
/// Each pair represents an (LHS, RHS) match where LHS has an edge of the
/// specified kind connecting to RHS.
pub struct JoinResults {
    /// The graph (Arc for cheap cloning)
    graph: Arc<CodeGraph>,
    /// Matched (source, target) node ID pairs
    pairs: Vec<(NodeId, NodeId)>,
    /// The edge kind used for the join
    edge_kind: JoinEdgeKind,
    /// Workspace root for relative path display
    workspace_root: Option<PathBuf>,
    /// Whether results were truncated by a result cap
    truncated: bool,
}

impl JoinResults {
    /// Creates new join results.
    #[must_use]
    pub fn new(
        graph: Arc<CodeGraph>,
        pairs: Vec<(NodeId, NodeId)>,
        edge_kind: JoinEdgeKind,
        truncated: bool,
    ) -> Self {
        Self {
            graph,
            pairs,
            edge_kind,
            workspace_root: None,
            truncated,
        }
    }

    /// Sets the workspace root for relative path resolution.
    #[must_use]
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Returns the number of matched pairs.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.pairs.len()
    }

    /// Returns true if no pairs matched.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Returns the edge kind used for the join.
    #[must_use]
    pub fn edge_kind(&self) -> &JoinEdgeKind {
        &self.edge_kind
    }

    /// Returns whether the results were truncated by a result cap.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Returns the underlying graph.
    #[must_use]
    pub fn graph(&self) -> &CodeGraph {
        &self.graph
    }

    /// Iterates over matched pairs with accessor methods.
    pub fn iter(&self) -> impl Iterator<Item = JoinMatch<'_>> + '_ {
        self.pairs.iter().filter_map(|&(left_id, right_id)| {
            let left = self.graph.nodes().get(left_id)?;
            let right = self.graph.nodes().get(right_id)?;
            Some(JoinMatch {
                left: QueryMatch {
                    id: left_id,
                    entry: left,
                    graph: &self.graph,
                    workspace_root: self.workspace_root.as_deref(),
                },
                right: QueryMatch {
                    id: right_id,
                    entry: right,
                    graph: &self.graph,
                    workspace_root: self.workspace_root.as_deref(),
                },
                edge_kind: &self.edge_kind,
            })
        })
    }
}

impl std::fmt::Debug for JoinResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinResults")
            .field("graph", &self.graph)
            .field("pairs", &self.pairs)
            .field("edge_kind", &self.edge_kind)
            .field("workspace_root", &self.workspace_root)
            .field("truncated", &self.truncated)
            .finish()
    }
}

/// A single join match with accessors for the left and right nodes.
pub struct JoinMatch<'a> {
    /// The left (source) node.
    pub left: QueryMatch<'a>,
    /// The right (target) node.
    pub right: QueryMatch<'a>,
    /// The edge kind connecting them.
    pub edge_kind: &'a JoinEdgeKind,
}

/// Unified query output that can be either regular results, join results, or aggregation.
pub enum QueryOutput {
    /// Standard query results (matched nodes).
    Results(QueryResults),
    /// Join query results (matched node pairs).
    Join(JoinResults),
    /// Aggregation results (pipeline output).
    Aggregation(AggregationResult),
}

impl QueryOutput {
    /// Returns the number of results (nodes for Results, pairs for Join, 0 for Aggregation).
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Results(r) => r.len(),
            Self::Join(j) => j.len(),
            Self::Aggregation(_) => 0,
        }
    }

    /// Returns true if there are no results.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Results(r) => r.is_empty(),
            Self::Join(j) => j.is_empty(),
            Self::Aggregation(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_results_empty() {
        let graph = Arc::new(CodeGraph::new());
        let results = QueryResults::new(graph, vec![]);
        assert!(results.is_empty());
        assert_eq!(results.len(), 0);
        assert_eq!(results.iter().count(), 0);
    }

    #[test]
    fn test_query_results_debug() {
        let graph = Arc::new(CodeGraph::new());
        let results = QueryResults::new(graph, vec![]);
        let debug_str = format!("{results:?}");
        assert!(debug_str.contains("QueryResults"));
        assert!(debug_str.contains("match_count"));
    }

    #[test]
    fn test_query_results_with_workspace_root() {
        let graph = Arc::new(CodeGraph::new());
        let results =
            QueryResults::new(graph, vec![]).with_workspace_root(PathBuf::from("/test/path"));
        assert_eq!(results.workspace_root(), Some(Path::new("/test/path")));
    }

    #[test]
    fn test_query_results_into_parts() {
        let graph = Arc::new(CodeGraph::new());
        let results = QueryResults::new(graph.clone(), vec![]);
        let (returned_graph, matches) = results.into_parts();
        assert!(Arc::ptr_eq(&returned_graph, &graph));
        assert!(matches.is_empty());
    }
}
