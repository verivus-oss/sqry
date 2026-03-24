//! Cascade cleanup for node removal.
//!
//! This module implements cascade cleanup operations that ensure all graph
//! data structures remain consistent when a node is removed.
//!
//! # Design
//!
//! When a node is removed, the following must be cleaned up:
//! - **`NodeArena`**: Remove the node entry
//! - **`AuxiliaryIndices`**: Remove from kind, name, and file indices
//! - **`BidirectionalEdgeStore`**: Invalidate all edges to/from the node
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::node::cascade::CascadeCleanup;
//!
//! let mut cleanup = CascadeCleanup::new();
//!
//! // Remove a single node
//! let result = cleanup.remove_node(&mut arena, &mut indices, &edge_store, node_id);
//!
//! // Remove all nodes in a file (for incremental re-indexing)
//! let results = cleanup.remove_file(&mut arena, &mut indices, &edge_store, file_id);
//! ```

use crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore;
use crate::graph::unified::file::FileId;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::node::kind::NodeKind;
use crate::graph::unified::storage::arena::{NodeArena, NodeEntry};
use crate::graph::unified::storage::indices::AuxiliaryIndices;
use crate::graph::unified::string::StringId;

/// Result of a cascade node removal operation.
#[derive(Debug, Clone)]
pub struct CascadeRemovalResult {
    /// The node ID that was removed.
    pub node_id: NodeId,
    /// The node entry that was removed, if it existed.
    pub entry: Option<NodeEntry>,
    /// Number of edges invalidated (add tombstones) for this node.
    pub edges_invalidated: usize,
    /// Whether the node was found in auxiliary indices.
    pub removed_from_indices: bool,
}

impl CascadeRemovalResult {
    /// Returns true if the node was actually removed (existed before).
    #[inline]
    #[must_use]
    pub fn was_removed(&self) -> bool {
        self.entry.is_some()
    }
}

/// Summary of a file-level cascade removal.
#[derive(Debug, Clone)]
pub struct FileCascadeResult {
    /// The file that was cleared.
    pub file: FileId,
    /// Number of nodes removed from the arena.
    pub nodes_removed: usize,
    /// Total number of edges invalidated.
    pub edges_invalidated: usize,
}

/// Cascade cleanup coordinator.
///
/// Provides methods to cleanly remove nodes from all graph data structures,
/// ensuring no dangling references remain.
///
/// # Invariants
///
/// After a cascade removal:
/// - The node is no longer in `NodeArena`
/// - The node is no longer in `AuxiliaryIndices`
/// - All edges to/from the node are tombstoned in `BidirectionalEdgeStore`
#[derive(Debug, Default)]
pub struct CascadeCleanup {
    /// Optional statistics tracking.
    stats: CascadeStats,
}

/// Statistics for cascade cleanup operations.
#[derive(Debug, Clone, Copy, Default)]
pub struct CascadeStats {
    /// Total nodes removed.
    pub nodes_removed: u64,
    /// Total edges invalidated.
    pub edges_invalidated: u64,
    /// Total files processed.
    pub files_processed: u64,
}

impl CascadeCleanup {
    /// Creates a new cascade cleanup coordinator.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stats: CascadeStats::default(),
        }
    }

    /// Returns the accumulated statistics.
    #[must_use]
    pub fn stats(&self) -> CascadeStats {
        self.stats
    }

    /// Resets the statistics counters.
    pub fn reset_stats(&mut self) {
        self.stats = CascadeStats::default();
    }

    /// Removes a single node and all associated data.
    ///
    /// # Operations Performed
    ///
    /// 1. Removes node from `NodeArena` (returns entry if existed)
    /// 2. Removes node from `AuxiliaryIndices` (if metadata available)
    /// 3. Tombstones all edges to/from the node in `BidirectionalEdgeStore`
    ///
    /// # Arguments
    ///
    /// * `arena` - The node arena to remove from
    /// * `indices` - The auxiliary indices to update
    /// * `edge_store` - The edge store to tombstone edges in
    /// * `node_id` - The ID of the node to remove
    ///
    /// # Returns
    ///
    /// Details about what was removed.
    pub fn remove_node(
        &mut self,
        arena: &mut NodeArena,
        indices: &mut AuxiliaryIndices,
        edge_store: &BidirectionalEdgeStore,
        node_id: NodeId,
    ) -> CascadeRemovalResult {
        // Step 1: Remove from arena (get metadata for index removal)
        let entry = arena.remove(node_id);

        // Step 2: Remove node from all indices using the entry we just removed
        let removed_from_indices = if let Some(ref e) = entry {
            indices.remove(node_id, e.kind, e.name, e.qualified_name, e.file)
        } else {
            false
        };

        // Step 3: Invalidate edges to/from this node
        let edges_invalidated = Self::invalidate_node_edges(edge_store, node_id, entry.as_ref());

        // Update stats
        if entry.is_some() {
            self.stats.nodes_removed += 1;
        }
        self.stats.edges_invalidated += edges_invalidated as u64;

        CascadeRemovalResult {
            node_id,
            entry,
            edges_invalidated,
            removed_from_indices,
        }
    }

    /// Removes a node using pre-known metadata.
    ///
    /// This is more efficient when the node metadata is already known
    /// (e.g., from iteration), avoiding an extra lookup.
    ///
    /// # Arguments
    ///
    /// * `arena` - The node arena to remove from
    /// * `indices` - The auxiliary indices to update
    /// * `edge_store` - The edge store to tombstone edges in
    /// * `node_id` - The ID of the node to remove
    /// * `kind` - The known kind of the node
    /// * `name` - The known name of the node
    /// * `file` - The known file of the node
    ///
    /// # Returns
    ///
    /// Details about what was removed.
    #[allow(clippy::too_many_arguments)]
    pub fn remove_node_with_metadata(
        &mut self,
        arena: &mut NodeArena,
        indices: &mut AuxiliaryIndices,
        edge_store: &BidirectionalEdgeStore,
        node_id: NodeId,
        kind: NodeKind,
        name: StringId,
        qualified_name: Option<StringId>,
        file: FileId,
    ) -> CascadeRemovalResult {
        // Step 1: Remove from arena
        let entry = arena.remove(node_id);

        // Step 2: Remove from indices using provided metadata
        let removed_from_indices = indices.remove(node_id, kind, name, qualified_name, file);

        // Step 3: Invalidate edges
        let edges_invalidated = Self::invalidate_node_edges(edge_store, node_id, entry.as_ref());

        // Update stats
        if entry.is_some() {
            self.stats.nodes_removed += 1;
        }
        self.stats.edges_invalidated += edges_invalidated as u64;

        CascadeRemovalResult {
            node_id,
            entry,
            edges_invalidated,
            removed_from_indices,
        }
    }

    /// Removes all nodes in a file.
    ///
    /// This is the efficient path for file deletion during incremental updates.
    /// It collects all nodes in the file first, then performs bulk cleanup.
    ///
    /// # Arguments
    ///
    /// * `arena` - The node arena to remove from
    /// * `indices` - The auxiliary indices to update
    /// * `edge_store` - The edge store to tombstone edges in
    /// * `file` - The file ID to remove all nodes from
    ///
    /// # Returns
    ///
    /// Summary of the file removal operation.
    pub fn remove_file(
        &mut self,
        arena: &mut NodeArena,
        indices: &mut AuxiliaryIndices,
        edge_store: &BidirectionalEdgeStore,
        file: FileId,
    ) -> FileCascadeResult {
        // Collect nodes in this file from indices (fast O(1) lookup)
        let node_ids: Vec<NodeId> = indices.by_file(file).to_vec();

        // Collect metadata before removal (need to access arena)
        let nodes_metadata: Vec<_> = node_ids
            .iter()
            .filter_map(|&id| {
                arena
                    .get(id)
                    .map(|e| (id, e.kind, e.name, e.qualified_name))
            })
            .collect();

        // Step 1: Remove from indices using efficient bulk method
        let _indices_removed = indices.remove_file_with_info(file, nodes_metadata.iter().copied());

        // Step 2: Remove each node from arena and invalidate edges
        let mut nodes_removed = 0;
        let mut edges_invalidated = 0;

        for &node_id in &node_ids {
            // Get entry before removal for edge invalidation
            let entry = arena.remove(node_id);

            if entry.is_some() {
                nodes_removed += 1;
            }

            // Invalidate edges
            edges_invalidated += Self::invalidate_node_edges(edge_store, node_id, entry.as_ref());
        }

        // Also clear any file-associated edges that might remain
        let file_edges_cleared = edge_store.clear_file(file);
        edges_invalidated += file_edges_cleared;

        // Update stats
        self.stats.nodes_removed += nodes_removed as u64;
        self.stats.edges_invalidated += edges_invalidated as u64;
        self.stats.files_processed += 1;

        FileCascadeResult {
            file,
            nodes_removed,
            edges_invalidated,
        }
    }

    /// Invalidates all edges to/from a node by adding tombstones.
    ///
    /// This ensures that even if the edges are in the CSR, they will be
    /// filtered out during queries.
    fn invalidate_node_edges(
        edge_store: &BidirectionalEdgeStore,
        node_id: NodeId,
        entry: Option<&NodeEntry>,
    ) -> usize {
        // Get the file ID for tombstone creation
        let file = entry.map_or(FileId::new(0), |e| e.file);

        let mut count = 0;

        // Get outgoing edges and tombstone them
        let outgoing = edge_store.edges_from(node_id);
        for edge in outgoing {
            edge_store.remove_edge(node_id, edge.target, edge.kind.clone(), file);
            count += 1;
        }

        // Get incoming edges and tombstone them
        // edge.source now preserves full NodeId with generation for correct EdgeKey matching
        // edge.file preserves the original source file for correct Remove delta partitioning
        let incoming = edge_store.edges_to(node_id);
        for edge in incoming {
            // Use edge.file (source's file) so Remove delta is partitioned with the original Add
            edge_store.remove_edge(edge.source, node_id, edge.kind.clone(), edge.file);
            count += 1;
        }

        count
    }
}

/// Convenience function for single node removal.
///
/// This is a standalone function for simple use cases where you don't
/// need statistics tracking.
pub fn cascade_remove_node(
    arena: &mut NodeArena,
    indices: &mut AuxiliaryIndices,
    edge_store: &BidirectionalEdgeStore,
    node_id: NodeId,
) -> CascadeRemovalResult {
    let mut cleanup = CascadeCleanup::new();
    cleanup.remove_node(arena, indices, edge_store, node_id)
}

/// Convenience function for file removal.
///
/// This is a standalone function for simple use cases where you don't
/// need statistics tracking.
pub fn cascade_remove_file(
    arena: &mut NodeArena,
    indices: &mut AuxiliaryIndices,
    edge_store: &BidirectionalEdgeStore,
    file: FileId,
) -> FileCascadeResult {
    let mut cleanup = CascadeCleanup::new();
    cleanup.remove_file(arena, indices, edge_store, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::edge::kind::EdgeKind;

    fn test_file(id: u32) -> FileId {
        FileId::new(id)
    }

    fn test_name(id: u32) -> StringId {
        StringId::new(id)
    }

    fn test_entry(kind: NodeKind, name: StringId, file: FileId) -> NodeEntry {
        NodeEntry::new(kind, name, file)
    }

    #[test]
    fn test_cascade_cleanup_new() {
        let cleanup = CascadeCleanup::new();
        assert_eq!(cleanup.stats().nodes_removed, 0);
        assert_eq!(cleanup.stats().edges_invalidated, 0);
    }

    #[test]
    fn test_remove_single_node() {
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let file = test_file(1);
        let name = test_name(1);
        let entry = test_entry(NodeKind::Function, name, file);

        let node_id = arena.alloc(entry).unwrap();
        indices.add(node_id, NodeKind::Function, name, None, file);

        assert!(arena.contains(node_id));
        assert_eq!(indices.len(), 1);

        let mut cleanup = CascadeCleanup::new();
        let result = cleanup.remove_node(&mut arena, &mut indices, &edge_store, node_id);

        assert!(result.was_removed());
        assert!(!arena.contains(node_id));
        assert_eq!(indices.len(), 0);
        assert_eq!(cleanup.stats().nodes_removed, 1);
    }

    #[test]
    fn test_remove_nonexistent_node() {
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let fake_id = NodeId::new(999, 1);

        let mut cleanup = CascadeCleanup::new();
        let result = cleanup.remove_node(&mut arena, &mut indices, &edge_store, fake_id);

        assert!(!result.was_removed());
        assert_eq!(result.entry, None);
        assert!(!result.removed_from_indices);
        assert_eq!(cleanup.stats().nodes_removed, 0);
    }

    #[test]
    fn test_remove_node_with_edges() {
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let file = test_file(1);

        // Create three nodes
        let entry1 = test_entry(NodeKind::Function, test_name(1), file);
        let entry2 = test_entry(NodeKind::Function, test_name(2), file);
        let entry3 = test_entry(NodeKind::Function, test_name(3), file);

        let id1 = arena.alloc(entry1).unwrap();
        let id2 = arena.alloc(entry2).unwrap();
        let id3 = arena.alloc(entry3).unwrap();

        indices.add(id1, NodeKind::Function, test_name(1), None, file);
        indices.add(id2, NodeKind::Function, test_name(2), None, file);
        indices.add(id3, NodeKind::Function, test_name(3), None, file);

        // Create edges: id1 -> id2, id3 -> id1
        edge_store.add_edge(
            id1,
            id2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        edge_store.add_edge(
            id3,
            id1,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        // Verify edges exist
        assert_eq!(edge_store.edges_from(id1).len(), 1);
        assert_eq!(edge_store.edges_to(id1).len(), 1);

        // Remove id1
        let mut cleanup = CascadeCleanup::new();
        let result = cleanup.remove_node(&mut arena, &mut indices, &edge_store, id1);

        assert!(result.was_removed());
        assert_eq!(result.edges_invalidated, 2); // one outgoing, one incoming

        // Node should be gone
        assert!(!arena.contains(id1));
        assert_eq!(indices.len(), 2);

        // Stats should reflect the removal
        assert_eq!(cleanup.stats().nodes_removed, 1);
        assert_eq!(cleanup.stats().edges_invalidated, 2);
    }

    #[test]
    fn test_remove_node_with_metadata() {
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let file = test_file(1);
        let name = test_name(1);
        let entry = test_entry(NodeKind::Function, name, file);

        let node_id = arena.alloc(entry).unwrap();
        indices.add(node_id, NodeKind::Function, name, None, file);

        let mut cleanup = CascadeCleanup::new();
        let result = cleanup.remove_node_with_metadata(
            &mut arena,
            &mut indices,
            &edge_store,
            node_id,
            NodeKind::Function,
            name,
            None,
            file,
        );

        assert!(result.was_removed());
        assert!(result.removed_from_indices);
        assert!(!arena.contains(node_id));
        assert_eq!(indices.len(), 0);
    }

    #[test]
    fn test_remove_file() {
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let file1 = test_file(1);
        let file2 = test_file(2);

        // Create nodes in file1
        let entry1 = test_entry(NodeKind::Function, test_name(1), file1);
        let entry2 = test_entry(NodeKind::Class, test_name(2), file1);
        let entry3 = test_entry(NodeKind::Method, test_name(3), file1);

        // Create node in file2
        let entry4 = test_entry(NodeKind::Function, test_name(4), file2);

        let id1 = arena.alloc(entry1).unwrap();
        let id2 = arena.alloc(entry2).unwrap();
        let id3 = arena.alloc(entry3).unwrap();
        let id4 = arena.alloc(entry4).unwrap();

        indices.add(id1, NodeKind::Function, test_name(1), None, file1);
        indices.add(id2, NodeKind::Class, test_name(2), None, file1);
        indices.add(id3, NodeKind::Method, test_name(3), None, file1);
        indices.add(id4, NodeKind::Function, test_name(4), None, file2);

        // Add edges within file1
        edge_store.add_edge(
            id1,
            id2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file1,
        );
        edge_store.add_edge(
            id2,
            id3,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file1,
        );

        assert_eq!(arena.len(), 4);
        assert_eq!(indices.len(), 4);

        // Remove file1
        let mut cleanup = CascadeCleanup::new();
        let result = cleanup.remove_file(&mut arena, &mut indices, &edge_store, file1);

        assert_eq!(result.file, file1);
        assert_eq!(result.nodes_removed, 3);

        // Verify file1 nodes are gone
        assert!(!arena.contains(id1));
        assert!(!arena.contains(id2));
        assert!(!arena.contains(id3));

        // Verify file2 node remains
        assert!(arena.contains(id4));

        // Verify indices updated
        assert_eq!(indices.len(), 1);
        assert!(indices.by_file(file1).is_empty());
        assert_eq!(indices.by_file(file2).len(), 1);

        // Verify stats
        assert_eq!(cleanup.stats().nodes_removed, 3);
        assert_eq!(cleanup.stats().files_processed, 1);
    }

    #[test]
    fn test_remove_empty_file() {
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let file = test_file(1);

        let mut cleanup = CascadeCleanup::new();
        let result = cleanup.remove_file(&mut arena, &mut indices, &edge_store, file);

        assert_eq!(result.nodes_removed, 0);
        assert_eq!(result.edges_invalidated, 0);
    }

    #[test]
    fn test_convenience_cascade_remove_node() {
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let file = test_file(1);
        let name = test_name(1);
        let entry = test_entry(NodeKind::Function, name, file);

        let node_id = arena.alloc(entry).unwrap();
        indices.add(node_id, NodeKind::Function, name, None, file);

        let result = cascade_remove_node(&mut arena, &mut indices, &edge_store, node_id);

        assert!(result.was_removed());
        assert!(!arena.contains(node_id));
    }

    #[test]
    fn test_convenience_cascade_remove_file() {
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let file = test_file(1);

        let entry = test_entry(NodeKind::Function, test_name(1), file);
        let node_id = arena.alloc(entry).unwrap();
        indices.add(node_id, NodeKind::Function, test_name(1), None, file);

        let result = cascade_remove_file(&mut arena, &mut indices, &edge_store, file);

        assert_eq!(result.nodes_removed, 1);
        assert!(!arena.contains(node_id));
    }

    #[test]
    fn test_reset_stats() {
        let mut cleanup = CascadeCleanup::new();
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let file = test_file(1);
        let entry = test_entry(NodeKind::Function, test_name(1), file);
        let node_id = arena.alloc(entry).unwrap();
        indices.add(node_id, NodeKind::Function, test_name(1), None, file);

        cleanup.remove_node(&mut arena, &mut indices, &edge_store, node_id);

        assert_eq!(cleanup.stats().nodes_removed, 1);

        cleanup.reset_stats();

        assert_eq!(cleanup.stats().nodes_removed, 0);
        assert_eq!(cleanup.stats().edges_invalidated, 0);
    }

    #[test]
    fn test_cascade_removal_result_methods() {
        let result_with_entry = CascadeRemovalResult {
            node_id: NodeId::new(1, 1),
            entry: Some(NodeEntry::new(
                NodeKind::Function,
                StringId::new(1),
                FileId::new(1),
            )),
            edges_invalidated: 5,
            removed_from_indices: true,
        };
        assert!(result_with_entry.was_removed());

        let result_without_entry = CascadeRemovalResult {
            node_id: NodeId::new(2, 1),
            entry: None,
            edges_invalidated: 0,
            removed_from_indices: false,
        };
        assert!(!result_without_entry.was_removed());
    }

    #[test]
    fn test_no_dangling_edges_after_removal() {
        // Comprehensive test for No dangling edges after node removal
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let file = test_file(1);

        // Create a graph: A -> B -> C, A -> C
        let entry_a = test_entry(NodeKind::Function, test_name(1), file);
        let entry_b = test_entry(NodeKind::Function, test_name(2), file);
        let entry_c = test_entry(NodeKind::Function, test_name(3), file);

        let id_a = arena.alloc(entry_a).unwrap();
        let id_b = arena.alloc(entry_b).unwrap();
        let id_c = arena.alloc(entry_c).unwrap();

        indices.add(id_a, NodeKind::Function, test_name(1), None, file);
        indices.add(id_b, NodeKind::Function, test_name(2), None, file);
        indices.add(id_c, NodeKind::Function, test_name(3), None, file);

        edge_store.add_edge(
            id_a,
            id_b,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        edge_store.add_edge(
            id_b,
            id_c,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        edge_store.add_edge(
            id_a,
            id_c,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        // Remove node B (middle of chain)
        let mut cleanup = CascadeCleanup::new();
        let result = cleanup.remove_node(&mut arena, &mut indices, &edge_store, id_b);

        // Verify B was removed
        assert!(result.was_removed());
        assert!(!arena.contains(id_b));

        // Verify A and C still exist
        assert!(arena.contains(id_a));
        assert!(arena.contains(id_c));

        // Verify edges from/to B have tombstones (will be filtered in queries)
        // After removal, querying edges from B should show removed edges have tombstones
        assert_eq!(result.edges_invalidated, 2); // A->B and B->C

        // CRITICAL: Assert edge visibility post-removal
        // Edges from A should no longer include B (only A->C should remain visible)
        let edges_from_a = edge_store.edges_from(id_a);
        assert_eq!(
            edges_from_a.len(),
            1,
            "A should have exactly 1 visible edge (A->C)"
        );
        assert_eq!(
            edges_from_a[0].target, id_c,
            "A's edge should point to C, not B"
        );

        // Edges to B should be empty (A->B has been tombstoned)
        let edges_to_b = edge_store.edges_to(id_b);
        assert!(
            edges_to_b.is_empty(),
            "No edges should be visible to removed node B"
        );

        // Edges from B should be empty (B->C has been tombstoned)
        let edges_from_b = edge_store.edges_from(id_b);
        assert!(
            edges_from_b.is_empty(),
            "No edges should be visible from removed node B"
        );

        // Edges to C should no longer include B (only A->C should be visible)
        let edges_to_c = edge_store.edges_to(id_c);
        assert_eq!(
            edges_to_c.len(),
            1,
            "C should have exactly 1 incoming visible edge"
        );
        assert_eq!(
            edges_to_c[0].source, id_a,
            "C's incoming edge should be from A, not B"
        );
    }

    #[test]
    fn test_cross_file_edges_removed_with_file() {
        // Tests that cross-file incoming edges are properly removed when a file is removed.
        // This validates the fix for file-partition mismatch (CRITICAL M7).
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let file_a = test_file(1);
        let file_b = test_file(2);

        // Create node A in file_a
        let entry_a = test_entry(NodeKind::Function, test_name(1), file_a);
        let id_a = arena.alloc(entry_a).unwrap();
        indices.add(id_a, NodeKind::Function, test_name(1), None, file_a);

        // Create node B in file_b
        let entry_b = test_entry(NodeKind::Function, test_name(2), file_b);
        let id_b = arena.alloc(entry_b).unwrap();
        indices.add(id_b, NodeKind::Function, test_name(2), None, file_b);

        // Add cross-file edge: A (file_a) -> B (file_b)
        // The edge is associated with file_a (source's file)
        edge_store.add_edge(
            id_a,
            id_b,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file_a,
        );

        // Verify edge exists
        assert_eq!(edge_store.edges_from(id_a).len(), 1);
        assert_eq!(edge_store.edges_to(id_b).len(), 1);

        // Remove file_b (which contains node B)
        let mut cleanup = CascadeCleanup::new();
        let result = cleanup.remove_file(&mut arena, &mut indices, &edge_store, file_b);

        assert_eq!(result.nodes_removed, 1);
        assert!(!arena.contains(id_b));
        assert!(arena.contains(id_a));

        // CRITICAL: The incoming edge A->B must be invisible after B's file is removed.
        // Before the fix, the Remove delta would be stored in file_b's partition,
        // but the Add delta lives in file_a's partition, so they wouldn't cancel.
        let edges_from_a = edge_store.edges_from(id_a);
        assert!(
            edges_from_a.is_empty(),
            "Cross-file edge A->B should be invisible after B's file removal"
        );

        let edges_to_b = edge_store.edges_to(id_b);
        assert!(
            edges_to_b.is_empty(),
            "No edges should be visible to removed node B"
        );
    }

    #[test]
    fn test_delta_only_edge_removal_with_matching_generation() {
        // Tests that delta-only edges with matching generations are properly removed.
        // This validates the fix for edge generation mismatch (CRITICAL M7).
        let mut arena = NodeArena::new();
        let mut indices = AuxiliaryIndices::new();
        let edge_store = BidirectionalEdgeStore::new();

        let file = test_file(1);

        // Create nodes A and B
        let entry_a = test_entry(NodeKind::Function, test_name(1), file);
        let entry_b = test_entry(NodeKind::Function, test_name(2), file);

        let id_a = arena.alloc(entry_a).unwrap();
        let id_b = arena.alloc(entry_b).unwrap();

        indices.add(id_a, NodeKind::Function, test_name(1), None, file);
        indices.add(id_b, NodeKind::Function, test_name(2), None, file);

        // Verify generations are > 0 (nodes are not generation 0)
        assert!(
            id_a.generation() > 0,
            "Node A should have non-zero generation"
        );
        assert!(
            id_b.generation() > 0,
            "Node B should have non-zero generation"
        );

        // Add edge A -> B (only in delta buffer, not compacted to CSR)
        edge_store.add_edge(
            id_a,
            id_b,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        // Verify edge exists
        let edges_before = edge_store.edges_from(id_a);
        assert_eq!(edges_before.len(), 1);
        assert_eq!(edges_before[0].target, id_b);

        // Remove node B
        let mut cleanup = CascadeCleanup::new();
        let result = cleanup.remove_node(&mut arena, &mut indices, &edge_store, id_b);

        assert!(result.was_removed());
        assert_eq!(result.edges_invalidated, 1);

        // CRITICAL: Edge A->B must be invisible after B is removed.
        // Before the fix, the Remove delta would use generation 0,
        // but the Add delta has the real generation, so they wouldn't match.
        let edges_after = edge_store.edges_from(id_a);
        assert!(
            edges_after.is_empty(),
            "Delta-only edge A->B should be invisible after B removal"
        );

        let edges_to_b = edge_store.edges_to(id_b);
        assert!(
            edges_to_b.is_empty(),
            "No edges should be visible to removed node B"
        );
    }
}
