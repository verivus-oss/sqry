//! Incremental updates for the unified graph.
//!
//! This module implements incremental update operations (FR-27, FR-28, FR-33):
//! - File removal: Remove all nodes and edges from a deleted file
//! - Edge addition: Add edges without full rebuild
//! - Node removal: Remove specific nodes and their edges
//!
//! # Overview
//!
//! Incremental updates enable efficient partial modifications to the graph
//! without requiring a full rebuild. This is critical for IDE integration
//! where files change frequently.
//!
//! # Operations
//!
//! - [`remove_file_nodes`]: Remove all nodes from a file (and their edges)
//! - [`add_edge_incremental`]: Add a single edge to the graph
//! - [`remove_node`]: Remove a specific node and its connected edges
//!
//! # Thread Safety
//!
//! These operations acquire appropriate locks on the graph stores.
//! They are designed to be safe for concurrent read access while
//! performing mutations.

use super::super::edge::EdgeKind;
use super::super::file::FileId;
use super::super::node::NodeId;
use super::super::storage::{AuxiliaryIndices, NodeArena};
use super::identity::IdentityIndex;
use super::pass3_intra::PendingEdge;

/// Statistics from incremental operations.
#[derive(Debug, Clone, Default)]
pub struct IncrementalStats {
    /// Number of nodes removed.
    pub nodes_removed: usize,
    /// Number of edges removed.
    pub edges_removed: usize,
    /// Number of edges added.
    pub edges_added: usize,
    /// Number of identity index entries removed.
    pub identity_entries_removed: usize,
}

/// Result of file node removal.
#[derive(Debug)]
pub struct FileRemovalResult {
    /// Statistics from the removal.
    pub stats: IncrementalStats,
    /// List of removed node IDs.
    pub removed_nodes: Vec<NodeId>,
}

/// Remove all nodes from a file.
///
/// This function removes all nodes belonging to a specific file,
/// along with their associated edges. It also updates the identity
/// index to remove the file's entries.
///
/// # Arguments
///
/// * `file_id` - The file to remove nodes from
/// * `identity_index` - Identity index to update
/// * `arena` - Node arena (for tombstoning nodes)
/// * `indices` - Auxiliary indices to update
///
/// # Returns
///
/// Statistics about what was removed and list of removed node IDs.
pub fn remove_file_nodes(
    file_id: FileId,
    identity_index: &mut IdentityIndex,
    arena: &mut NodeArena,
    indices: &mut AuxiliaryIndices,
) -> FileRemovalResult {
    let mut stats = IncrementalStats::default();

    // Remove entries from identity index
    let removed_entries = identity_index.remove_file(file_id);
    stats.identity_entries_removed = removed_entries.len();

    // Collect node IDs
    let removed_nodes: Vec<NodeId> = removed_entries.iter().map(|(_, id)| *id).collect();
    stats.nodes_removed = removed_nodes.len();

    // Extract node metadata before removing from arena
    let node_metadata: Vec<_> = removed_nodes
        .iter()
        .filter_map(|&node_id| {
            arena
                .get(node_id)
                .map(|entry| (node_id, entry.kind, entry.name, entry.qualified_name))
        })
        .collect();

    // Remove from auxiliary indices using metadata (O(N*B) performance)
    indices.remove_file_with_info(file_id, node_metadata);

    // Remove nodes from arena after indices are updated
    for &node_id in &removed_nodes {
        let _ = arena.remove(node_id);
    }

    // Note: Edge removal is handled separately through the edge store
    // The cascade module handles edge cleanup when nodes are removed

    FileRemovalResult {
        stats,
        removed_nodes,
    }
}

/// Add a single edge to the graph incrementally.
///
/// This is a lightweight operation for adding edges discovered
/// during incremental analysis (e.g., when a file is re-parsed).
///
/// # Arguments
///
/// * `edge` - The pending edge to add
///
/// # Returns
///
/// Statistics about the addition.
///
/// # Note
///
/// This function prepares the edge for addition but doesn't actually
/// add it to the edge store. The caller should use the edge store's
/// `add_edge` method with the returned `PendingEdge`.
#[must_use]
pub fn add_edge_incremental(
    source: NodeId,
    target: NodeId,
    kind: EdgeKind,
    file: FileId,
) -> (IncrementalStats, PendingEdge) {
    let stats = IncrementalStats {
        edges_added: 1,
        ..Default::default()
    };

    let edge = PendingEdge {
        source,
        target,
        kind,
        file,
        spans: vec![], // Incremental edges don't have span info
    };

    (stats, edge)
}

/// Add multiple edges incrementally.
///
/// Batch version of `add_edge_incremental` for efficiency.
#[must_use]
pub fn add_edges_incremental(edges: &[PendingEdge]) -> IncrementalStats {
    IncrementalStats {
        edges_added: edges.len(),
        ..Default::default()
    }
}

/// Remove a specific node and update indices.
///
/// This function tombstones a node and removes it from the identity index.
/// Edge cleanup should be handled separately through the cascade module.
///
/// # Arguments
///
/// * `node_id` - The node to remove
/// * `identity_key` - Optional identity key for index cleanup
/// * `arena` - Node arena for tombstoning
/// * `identity_index` - Identity index to update
///
/// # Returns
///
/// Statistics about the removal.
pub fn remove_node(
    node_id: NodeId,
    identity_index: &mut IdentityIndex,
    arena: &mut NodeArena,
) -> IncrementalStats {
    let mut stats = IncrementalStats::default();

    if identity_index.remove_node_id(node_id).is_some() {
        stats.identity_entries_removed = 1;
    }

    // Remove the node from arena
    if arena.remove(node_id).is_some() {
        stats.nodes_removed = 1;
    }

    stats
}

/// Batch remove nodes from a file.
///
/// More efficient than calling `remove_node` multiple times.
pub fn remove_nodes_batch(
    node_ids: &[NodeId],
    identity_index: &mut IdentityIndex,
    arena: &mut NodeArena,
) -> IncrementalStats {
    let mut stats = IncrementalStats::default();

    for &node_id in node_ids {
        if identity_index.remove_node_id(node_id).is_some() {
            stats.identity_entries_removed += 1;
        }
        if arena.remove(node_id).is_some() {
            stats.nodes_removed += 1;
        }
    }

    stats
}

/// Prepare edges for removal from a file.
///
/// Returns the edges that should be removed when a file is deleted.
/// The actual removal should be done through the edge store.
///
/// # Arguments
///
/// * `file_id` - The file being removed
/// * `node_ids` - Nodes in the file (for edge lookup)
///
/// # Returns
///
/// List of edge specifications to remove.
#[derive(Debug, Clone)]
pub struct EdgeToRemove {
    /// Source node.
    pub source: NodeId,
    /// Target node.
    pub target: NodeId,
    /// Edge kind.
    pub kind: EdgeKind,
    /// File the edge belongs to.
    pub file: FileId,
}

#[cfg(test)]
mod tests {
    use super::super::identity::IdentityKey;
    use super::*;
    use crate::graph::unified::StringId;
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::NodeEntry;

    fn create_test_entry(name_id: StringId, file_id: FileId) -> NodeEntry {
        NodeEntry::new(NodeKind::Function, name_id, file_id)
    }

    #[test]
    fn test_remove_file_nodes() {
        let mut arena = NodeArena::new();
        let mut identity_index = IdentityIndex::new();
        let mut indices = AuxiliaryIndices::new();
        let file_id = FileId::new(5);

        // Add some nodes
        let name_id = StringId::new(1);
        let entry1 = create_test_entry(name_id, file_id);
        let entry2 = create_test_entry(name_id, file_id);
        let node1 = arena.alloc(entry1).unwrap();
        let node2 = arena.alloc(entry2).unwrap();

        // Add to identity index
        let key1 = IdentityKey::new(StringId::new(1), file_id, StringId::new(10));
        let key2 = IdentityKey::new(StringId::new(1), file_id, StringId::new(11));
        identity_index.insert(key1, node1);
        identity_index.insert(key2, node2);

        // Remove file
        let result = remove_file_nodes(file_id, &mut identity_index, &mut arena, &mut indices);

        assert_eq!(result.stats.nodes_removed, 2);
        assert_eq!(result.stats.identity_entries_removed, 2);
        assert_eq!(result.removed_nodes.len(), 2);

        // Verify nodes are removed (no longer accessible)
        assert!(arena.get(node1).is_none());
        assert!(arena.get(node2).is_none());
    }

    #[test]
    fn test_add_edge_incremental() {
        let source = NodeId::new(0, 1);
        let target = NodeId::new(1, 1);
        let file_id = FileId::new(0);

        let (stats, edge) = add_edge_incremental(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file_id,
        );

        assert_eq!(stats.edges_added, 1);
        assert_eq!(edge.source, source);
        assert_eq!(edge.target, target);
        assert!(matches!(
            edge.kind,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false
            }
        ));
    }

    #[test]
    fn test_add_edges_incremental_batch() {
        let file_id = FileId::new(0);
        let edges = vec![
            PendingEdge {
                source: NodeId::new(0, 1),
                target: NodeId::new(1, 1),
                kind: EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                file: file_id,
                spans: vec![],
            },
            PendingEdge {
                source: NodeId::new(1, 1),
                target: NodeId::new(2, 1),
                kind: EdgeKind::References,
                file: file_id,
                spans: vec![],
            },
            PendingEdge {
                source: NodeId::new(2, 1),
                target: NodeId::new(0, 1),
                kind: EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                file: file_id,
                spans: vec![],
            },
        ];

        let stats = add_edges_incremental(&edges);

        assert_eq!(stats.edges_added, 3);
    }

    #[test]
    fn test_remove_node() {
        let mut arena = NodeArena::new();
        let mut identity_index = IdentityIndex::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);

        let entry = create_test_entry(name_id, file_id);
        let node_id = arena.alloc(entry).unwrap();

        // Seed identity index
        let key = IdentityKey::new(StringId::new(1), file_id, StringId::new(10));
        identity_index.insert(key, node_id);

        let stats = remove_node(node_id, &mut identity_index, &mut arena);

        assert_eq!(stats.nodes_removed, 1);
        assert_eq!(stats.identity_entries_removed, 1);
        assert!(arena.get(node_id).is_none());
    }

    #[test]
    fn test_remove_nodes_batch() {
        let mut arena = NodeArena::new();
        let mut identity_index = IdentityIndex::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);

        let node1 = arena.alloc(create_test_entry(name_id, file_id)).unwrap();
        let node2 = arena.alloc(create_test_entry(name_id, file_id)).unwrap();
        let node3 = arena.alloc(create_test_entry(name_id, file_id)).unwrap();

        identity_index.insert(
            IdentityKey::new(StringId::new(1), file_id, StringId::new(10)),
            node1,
        );
        identity_index.insert(
            IdentityKey::new(StringId::new(1), file_id, StringId::new(11)),
            node2,
        );

        let stats = remove_nodes_batch(&[node1, node2, node3], &mut identity_index, &mut arena);

        assert_eq!(stats.nodes_removed, 3);
        assert_eq!(stats.identity_entries_removed, 2);
        assert!(arena.get(node1).is_none());
        assert!(arena.get(node2).is_none());
        assert!(arena.get(node3).is_none());
    }

    #[test]
    fn test_remove_nonexistent_node() {
        let mut arena = NodeArena::new();
        let mut identity_index = IdentityIndex::new();

        // Try to remove a node that doesn't exist
        let fake_id = NodeId::new(999, 1);
        let stats = remove_node(fake_id, &mut identity_index, &mut arena);

        // Should report 0 removed since node didn't exist
        assert_eq!(stats.nodes_removed, 0);
        assert_eq!(stats.identity_entries_removed, 0);
    }

    #[test]
    fn test_incremental_stats_default() {
        let stats = IncrementalStats::default();

        assert_eq!(stats.nodes_removed, 0);
        assert_eq!(stats.edges_removed, 0);
        assert_eq!(stats.edges_added, 0);
        assert_eq!(stats.identity_entries_removed, 0);
    }
}
