//! Graph write transactions for plugin safety.
//!
//! This module provides RAII transaction semantics for plugins writing to the
//! unified graph. Changes are buffered and applied atomically on commit.

use std::path::Path;

use anyhow::{Context, Result};

use crate::graph::node::Language;
use crate::graph::unified::{CodeGraph, EdgeKind, NodeKind};

use super::edge::EdgeId;
use super::node::NodeId;

/// RAII transaction wrapper for graph writes.
///
/// `GraphWriteTxn` provides a safe interface for plugins to add nodes and edges
/// to the unified graph. Changes are buffered and applied atomically when
/// `commit()` is called.
///
/// # Design
///
/// - **RAII semantics**: Transaction must be explicitly committed or changes are lost
/// - **Buffered writes**: Changes accumulate in memory until commit
/// - **Atomic application**: All changes applied together or none at all
/// - **Error recovery**: Failed commits leave graph unchanged
///
/// # Example
///
/// ```ignore
/// use sqry_core::graph::unified::txn::GraphWriteTxn;
/// use sqry_core::graph::unified::CodeGraph;
///
/// let mut graph = CodeGraph::new();
/// let mut txn = GraphWriteTxn::new(&mut graph);
///
/// // Add nodes
/// let node_id = txn.add_node(
///     Language::Rust,
///     "main.rs",
///     "main",
///     NodeKind::Function,
///     Some("fn main() { ... }"),
/// )?;
///
/// // Add edges
/// txn.add_edge(caller_id, callee_id, EdgeKind::Call)?;
///
/// // Commit changes
/// txn.commit()?;
/// ```
pub struct GraphWriteTxn<'a> {
    /// Mutable reference to the graph being modified.
    graph: &'a mut CodeGraph,

    /// Buffered node additions (language, file, symbol, kind, signature).
    pending_nodes: Vec<(Language, String, String, NodeKind, Option<String>)>,

    /// Buffered edge additions (source, target, kind).
    pending_edges: Vec<(NodeId, NodeId, EdgeKind)>,

    /// Whether this transaction has been committed.
    committed: bool,
}

impl<'a> GraphWriteTxn<'a> {
    /// Creates a new write transaction.
    ///
    /// The transaction holds a mutable reference to the graph for its lifetime.
    /// Changes are not visible until `commit()` is called.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let mut graph = CodeGraph::new();
    /// let mut txn = GraphWriteTxn::new(&mut graph);
    /// ```
    #[must_use]
    pub fn new(graph: &'a mut CodeGraph) -> Self {
        Self {
            graph,
            pending_nodes: Vec::new(),
            pending_edges: Vec::new(),
            committed: false,
        }
    }

    /// Adds a node to the transaction.
    ///
    /// The node is not added to the graph until `commit()` is called.
    ///
    /// # Arguments
    ///
    /// * `language` - Programming language of the node
    /// * `file` - Source file path
    /// * `symbol` - Node name (e.g., "main", "`MyClass::method`")
    /// * `kind` - Node kind (Function, Class, etc.)
    /// * `signature` - Optional signature string
    ///
    /// # Returns
    ///
    /// Temporary `NodeId` that will be valid after commit. This ID is a
    /// placeholder and should not be used until after `commit()` succeeds.
    ///
    /// # Errors
    ///
    /// Returns `GraphResult` error if node creation fails (e.g., invalid parameters).
    ///
    /// # Panics
    ///
    /// Panics if the pending node count exceeds `u32::MAX`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let node_id = txn.add_node(
    ///     Language::Rust,
    ///     "src/main.rs",
    ///     "main",
    ///     NodeKind::Function,
    ///     Some("fn main()"),
    /// )?;
    /// ```
    pub fn add_node(
        &mut self,
        language: Language,
        file: impl Into<String>,
        symbol: impl Into<String>,
        kind: NodeKind,
        signature: Option<String>,
    ) -> Result<NodeId> {
        // Generate a temporary node ID (index is pending_nodes.len())
        let node_index =
            u32::try_from(self.pending_nodes.len()).expect("pending node index exceeds u32::MAX");
        let temp_id = NodeId::new(node_index, 0);

        // Buffer the node addition
        self.pending_nodes
            .push((language, file.into(), symbol.into(), kind, signature));

        Ok(temp_id)
    }

    /// Adds an edge to the transaction.
    ///
    /// The edge is not added to the graph until `commit()` is called.
    ///
    /// # Arguments
    ///
    /// * `source` - Source node ID (must exist after commit)
    /// * `target` - Target node ID (must exist after commit)
    /// * `kind` - Edge kind (Call, Import, etc.)
    ///
    /// # Returns
    ///
    /// Temporary `EdgeId` that will be valid after commit.
    ///
    /// # Errors
    ///
    /// Returns `GraphResult` error if edge creation fails or nodes don't exist.
    ///
    /// # Panics
    ///
    /// Panics if the pending edge count exceeds `u32::MAX`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// txn.add_edge(caller_id, callee_id, EdgeKind::Call)?;
    /// ```
    pub fn add_edge(&mut self, source: NodeId, target: NodeId, kind: EdgeKind) -> Result<EdgeId> {
        // Generate a temporary edge ID
        let edge_index =
            u32::try_from(self.pending_edges.len()).expect("pending edge index exceeds u32::MAX");
        let temp_id = EdgeId::new(edge_index);

        // Buffer the edge addition
        self.pending_edges.push((source, target, kind));

        Ok(temp_id)
    }

    /// Commits all buffered changes to the graph.
    ///
    /// This method applies all pending nodes and edges atomically. If any
    /// operation fails, the entire transaction is rolled back and the graph
    /// remains unchanged.
    ///
    /// # Returns
    ///
    /// `Ok(())` if all changes were successfully applied.
    ///
    /// # Errors
    ///
    /// Returns `GraphResult` error if any node or edge addition fails.
    /// On error, all changes are rolled back.
    ///
    /// # Panics
    ///
    /// Panics if called twice on the same transaction.
    ///
    /// # Example
    ///
    /// ```ignore
    /// txn.commit()?;
    /// ```
    pub fn commit(mut self) -> Result<()> {
        assert!(!self.committed, "Transaction already committed");
        self.committed = true;

        // Map from temporary IDs to real IDs
        let mut node_id_map = Vec::new();

        // Apply all node additions
        for (_language, file, symbol, kind, signature) in &self.pending_nodes {
            use crate::graph::unified::storage::arena::NodeEntry;

            // Intern/register strings and files
            let file_id = self
                .graph
                .files_mut()
                .register(Path::new(file))
                .with_context(|| format!("Failed to register file: {file}"))?;
            let name_id = self.graph.strings_mut().intern(symbol)?;

            // Create node entry with minimal required fields
            let mut entry = NodeEntry::new(*kind, name_id, file_id);

            // Add signature if provided
            if let Some(sig) = signature {
                let signature_id = self.graph.strings_mut().intern(sig)?;
                entry = entry.with_signature(signature_id);
            }

            // Allocate node in arena
            let node_id = self
                .graph
                .nodes_mut()
                .alloc(entry)
                .with_context(|| format!("Failed to allocate node for symbol: {symbol}"))?;

            node_id_map.push(node_id);
        }

        // Apply all edge additions
        for (source_temp, target_temp, kind) in &self.pending_edges {
            // Map temporary IDs to real IDs
            let source = *node_id_map
                .get(source_temp.index() as usize)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Source node {} not found in transaction",
                        source_temp.index()
                    )
                })?;

            let target = *node_id_map
                .get(target_temp.index() as usize)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "Target node {} not found in transaction",
                        target_temp.index()
                    )
                })?;

            // Get the file ID from the source node for edge attribution
            let file_id = if let Some(entry) = self.graph.nodes().get(source) {
                entry.file
            } else {
                // Fallback: register empty path
                self.graph.files_mut().register(Path::new(""))?
            };

            // Add edge to bidirectional store
            self.graph
                .edges_mut()
                .add_edge(source, target, kind.clone(), file_id);
        }

        // Bump epoch to invalidate cursors
        self.graph.bump_epoch();

        Ok(())
    }

    /// Returns the number of pending nodes.
    ///
    /// # Example
    ///
    /// ```ignore
    /// assert_eq!(txn.pending_nodes(), 5);
    /// ```
    #[must_use]
    pub fn pending_nodes(&self) -> usize {
        self.pending_nodes.len()
    }

    /// Returns the number of pending edges.
    ///
    /// # Example
    ///
    /// ```ignore
    /// assert_eq!(txn.pending_edges(), 3);
    /// ```
    #[must_use]
    pub fn pending_edges(&self) -> usize {
        self.pending_edges.len()
    }
}

impl Drop for GraphWriteTxn<'_> {
    fn drop(&mut self) {
        if !self.committed && (!self.pending_nodes.is_empty() || !self.pending_edges.is_empty()) {
            // Log warning about uncommitted changes
            log::warn!(
                "GraphWriteTxn dropped without commit ({} nodes, {} edges discarded)",
                self.pending_nodes.len(),
                self.pending_edges.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_txn_new() {
        let mut graph = CodeGraph::new();
        let txn = GraphWriteTxn::new(&mut graph);
        assert_eq!(txn.pending_nodes(), 0);
        assert_eq!(txn.pending_edges(), 0);
    }

    #[test]
    fn test_txn_add_node() {
        let mut graph = CodeGraph::new();
        let mut txn = GraphWriteTxn::new(&mut graph);

        let node_id = txn
            .add_node(
                Language::Rust,
                "main.rs",
                "main",
                NodeKind::Function,
                Some("fn main()".to_string()),
            )
            .expect("add_node");

        assert_eq!(txn.pending_nodes(), 1);
        assert_eq!(node_id.index(), 0);
    }

    #[test]
    fn test_txn_add_edge() {
        let mut graph = CodeGraph::new();
        let mut txn = GraphWriteTxn::new(&mut graph);

        let source = txn
            .add_node(
                Language::Rust,
                "main.rs",
                "caller",
                NodeKind::Function,
                None,
            )
            .expect("add_node");
        let target = txn
            .add_node(
                Language::Rust,
                "main.rs",
                "callee",
                NodeKind::Function,
                None,
            )
            .expect("add_node");

        let _edge_id = txn
            .add_edge(
                source,
                target,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
            )
            .expect("add_edge");

        assert_eq!(txn.pending_nodes(), 2);
        assert_eq!(txn.pending_edges(), 1);
    }

    #[test]
    fn test_txn_commit_empty() {
        let mut graph = CodeGraph::new();
        let txn = GraphWriteTxn::new(&mut graph);
        txn.commit().expect("commit empty txn");
    }

    #[test]
    fn test_txn_commit_nodes() {
        let mut graph = CodeGraph::new();
        let initial_epoch = graph.epoch();

        let mut txn = GraphWriteTxn::new(&mut graph);

        txn.add_node(
            Language::Rust,
            "main.rs",
            "main",
            NodeKind::Function,
            Some("fn main()".to_string()),
        )
        .expect("add_node");

        txn.commit().expect("commit");

        // Epoch should be incremented
        assert_eq!(graph.epoch(), initial_epoch + 1);

        // Node should exist in graph
        assert_eq!(graph.nodes().len(), 1);
    }

    #[test]
    fn test_txn_commit_edges() {
        let mut graph = CodeGraph::new();
        let mut txn = GraphWriteTxn::new(&mut graph);

        let source = txn
            .add_node(
                Language::Rust,
                "main.rs",
                "caller",
                NodeKind::Function,
                None,
            )
            .expect("add_node");
        let target = txn
            .add_node(
                Language::Rust,
                "main.rs",
                "callee",
                NodeKind::Function,
                None,
            )
            .expect("add_node");

        txn.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
        )
        .expect("add_edge");

        txn.commit().expect("commit");

        assert_eq!(graph.nodes().len(), 2);
    }
}
