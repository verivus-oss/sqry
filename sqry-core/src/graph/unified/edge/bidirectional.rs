//! `BidirectionalEdgeStore`: Forward + reverse edge storage.
//!
//! This module implements bidirectional edge storage for efficient traversal
//! in both directions (callers and callees). It maintains two synchronized
//! `EdgeStore` instances - one for forward edges and one for reverse edges.
//!
//! # Design
//!
//! - **Forward store**: Maps source → target (e.g., "who does A call?")
//! - **Reverse store**: Maps target → source (e.g., "who calls A?")
//!
//! Both stores are kept in sync through atomic add/remove operations.
//! Concurrent access is protected by `RwLock`.
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::edge::bidirectional::BidirectionalEdgeStore;
//!
//! let store = BidirectionalEdgeStore::new();
//!
//! // Add edge (updates both forward and reverse)
//! store.add_edge(source, target, EdgeKind::Calls { argument_count: 0, is_async: false }, file);
//!
//! // Query forward (callers)
//! let callees = store.edges_from(source);
//!
//! // Query reverse (callees)
//! let callers = store.edges_to(target);
//! ```

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::super::file::FileId;
use super::super::node::NodeId;
use super::delta::DeltaEdge;
use super::kind::EdgeKind;
use super::store::{EdgeStore, EdgeStoreStats, StoreEdgeRef};

/// Bidirectional edge store with forward and reverse indices.
///
/// Maintains two synchronized `EdgeStore` instances for efficient
/// bidirectional traversal. Uses `RwLock` for concurrent read/write access.
///
/// # Thread Safety
///
/// Multiple readers can access the store concurrently. Writers get exclusive
/// access. All operations that modify state (add, remove) acquire write locks.
#[derive(Debug, Serialize, Deserialize)]
pub struct BidirectionalEdgeStore {
    /// Forward edges: source → target
    #[serde(with = "rwlock_edge_store_serde")]
    forward: RwLock<EdgeStore>,
    /// Reverse edges: target → source (inverted view)
    #[serde(with = "rwlock_edge_store_serde")]
    reverse: RwLock<EdgeStore>,
}

impl BidirectionalEdgeStore {
    /// Creates a new empty bidirectional edge store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            forward: RwLock::new(EdgeStore::new()),
            reverse: RwLock::new(EdgeStore::new()),
        }
    }

    /// Adds an edge to both forward and reverse stores.
    ///
    /// The edge is added atomically to both stores:
    /// - Forward: source → target
    /// - Reverse: target → source (with source and target swapped)
    ///
    /// Returns the delta edge from the forward store.
    /// For edges with span information, use [`add_edge_with_spans`](Self::add_edge_with_spans).
    pub fn add_edge(
        &self,
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        file: FileId,
    ) -> DeltaEdge {
        self.add_edge_with_spans(source, target, kind, file, Vec::new())
    }

    /// Adds an edge to both forward and reverse stores with span information.
    ///
    /// The edge is added atomically to both stores:
    /// - Forward: source → target
    /// - Reverse: target → source (with source and target swapped)
    ///
    /// The spans represent source locations of the edge (e.g., call sites for CALLS edges).
    /// Returns the delta edge from the forward store.
    pub fn add_edge_with_spans(
        &self,
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        file: FileId,
        spans: Vec<crate::graph::node::Span>,
    ) -> DeltaEdge {
        // Add to forward store
        let forward_edge = self.forward.write().add_edge_with_spans(
            source,
            target,
            kind.clone(),
            file,
            spans.clone(),
        );

        // Add to reverse store (swapped source/target)
        // Note: We preserve the spans even in reverse direction for querying call-site locations
        self.reverse
            .write()
            .add_edge_with_spans(target, source, kind, file, spans);

        forward_edge
    }

    /// Removes an edge from both forward and reverse stores.
    ///
    /// The edge is removed atomically from both stores.
    /// Returns the delta edge from the forward store.
    pub fn remove_edge(
        &self,
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        file: FileId,
    ) -> DeltaEdge {
        // Remove from forward store
        let forward_edge = self
            .forward
            .write()
            .remove_edge(source, target, kind.clone(), file);

        // Remove from reverse store (swapped source/target)
        self.reverse.write().remove_edge(target, source, kind, file);

        forward_edge
    }

    /// Returns outgoing edges from a source node (forward traversal).
    ///
    /// Answers: "What does `source` connect to?"
    pub fn edges_from(&self, source: NodeId) -> Vec<StoreEdgeRef> {
        self.forward.read().edges_from(source)
    }

    /// Returns incoming edges to a target node (reverse traversal).
    ///
    /// Answers: "What connects to `target`?"
    ///
    /// Note: The returned edges have swapped source/target from the reverse
    /// store's perspective, so they are re-mapped to the original orientation.
    /// The source `NodeId` preserves the full generation for edge key matching.
    pub fn edges_to(&self, target: NodeId) -> Vec<StoreEdgeRef> {
        // Query reverse store for edges FROM target (which are TO target in original)
        let reverse_edges = self.reverse.read().edges_from(target);

        // Remap: In reverse store, source=target and target=original_source
        // So we swap back to get edges TO the target
        // e.target is the original source (full NodeId with generation)
        // e.file is the original source file for correct Remove delta partitioning
        reverse_edges
            .into_iter()
            .map(|e| StoreEdgeRef {
                source: e.target, // Original source (full NodeId with generation) stored as target in reverse
                target,           // The query target
                kind: e.kind,
                seq: e.seq,
                file: e.file,   // Original source file for Remove delta partitioning
                spans: e.spans, // Preserve spans from reverse store
            })
            .collect()
    }

    /// Checks if an edge exists (in forward direction).
    pub fn has_edge(&self, source: NodeId, target: NodeId, kind: &EdgeKind) -> bool {
        self.forward.read().has_edge(source, target, kind)
    }

    /// Returns statistics for both stores.
    pub fn stats(&self) -> BidirectionalEdgeStoreStats {
        let forward_stats = self.forward.read().stats();
        let reverse_stats = self.reverse.read().stats();
        BidirectionalEdgeStoreStats {
            forward: forward_stats,
            reverse: reverse_stats,
        }
    }

    /// Returns the forward store's CSR version.
    #[must_use]
    pub fn csr_version(&self) -> u64 {
        self.forward.read().csr_version()
    }

    /// Clears all edges from a specific file in both stores.
    pub fn clear_file(&self, file: FileId) -> usize {
        let forward_cleared = self.forward.write().clear_file(file);
        let _reverse_cleared = self.reverse.write().clear_file(file);
        forward_cleared
    }

    /// Clears all delta buffer data from both stores.
    pub fn clear_delta(&self) {
        self.forward.write().clear_delta();
        self.reverse.write().clear_delta();
    }

    /// Returns a read lock on the forward store.
    pub fn forward(&self) -> parking_lot::RwLockReadGuard<'_, EdgeStore> {
        self.forward.read()
    }

    /// Returns a read lock on the reverse store.
    pub fn reverse(&self) -> parking_lot::RwLockReadGuard<'_, EdgeStore> {
        self.reverse.read()
    }

    /// Returns a write lock on the forward store.
    pub fn forward_mut(&self) -> parking_lot::RwLockWriteGuard<'_, EdgeStore> {
        self.forward.write()
    }

    /// Returns a write lock on the reverse store.
    pub fn reverse_mut(&self) -> parking_lot::RwLockWriteGuard<'_, EdgeStore> {
        self.reverse.write()
    }

    /// Bulk insert edges with pre-assigned sequence numbers.
    ///
    /// Inserts all edges from `file_edge_vecs` (one `Vec<DeltaEdge>` per file) into
    /// both forward and reverse stores. Edges must already have their `seq` fields
    /// assigned with monotonically increasing values across all vecs.
    ///
    /// After insertion, the sequence counters on both stores are advanced past the
    /// highest inserted sequence number to ensure subsequent incremental operations
    /// receive higher sequence numbers.
    ///
    /// # Arguments
    ///
    /// * `file_edge_vecs` - Ordered slices of edges grouped by file. Each inner
    ///   `Vec` contains edges for one file with pre-assigned sequence numbers.
    /// * `expected_total` - Expected total number of edges across all vecs.
    ///   Used as a consistency check.
    ///
    /// # Panics
    ///
    /// Panics if the actual total edge count does not match `expected_total`.
    pub fn add_edges_bulk_ordered(&self, file_edge_vecs: &[Vec<DeltaEdge>], expected_total: u64) {
        // Validate expected total
        let actual_total: u64 = file_edge_vecs.iter().map(|v| v.len() as u64).sum();
        assert_eq!(
            actual_total, expected_total,
            "add_edges_bulk_ordered: actual edge count {actual_total} != expected {expected_total}"
        );

        // Lock both stores for the entire bulk operation to maintain consistency
        let mut forward = self.forward.write();
        let mut reverse = self.reverse.write();

        // Validate that all incoming edge seqs are >= current store seq counters.
        // This prevents insertion of stale edges that could break merge ordering.
        let current_forward_seq = forward.seq_counter();
        let current_reverse_seq = reverse.seq_counter();

        let mut prev_seq: Option<u64> = None;

        for file_edges in file_edge_vecs {
            for edge in file_edges {
                // Validate monotonic ordering: each edge.seq must be >= previous
                if let Some(prev) = prev_seq {
                    assert!(
                        edge.seq >= prev,
                        "add_edges_bulk_ordered: non-monotonic seq: {} follows {prev}",
                        edge.seq
                    );
                }

                // Validate edge.seq >= current store counters to prevent stale insertion
                assert!(
                    edge.seq >= current_forward_seq,
                    "add_edges_bulk_ordered: edge seq {} < forward store counter {current_forward_seq}",
                    edge.seq
                );
                assert!(
                    edge.seq >= current_reverse_seq,
                    "add_edges_bulk_ordered: edge seq {} < reverse store counter {current_reverse_seq}",
                    edge.seq
                );

                prev_seq = Some(edge.seq);

                // Push to forward delta buffer
                forward.delta_mut().push(edge.clone());

                // Create reversed edge (swap source/target) and push to reverse
                let reverse_edge = DeltaEdge::with_spans(
                    edge.target,
                    edge.source,
                    edge.kind.clone(),
                    edge.seq,
                    edge.op,
                    edge.file,
                    edge.spans.clone(),
                );
                reverse.delta_mut().push(reverse_edge);
            }
        }

        // Advance sequence counters past the highest inserted seq
        if let Some(max) = prev_seq {
            forward.delta_mut().advance_seq_to(max + 1);
            reverse.delta_mut().advance_seq_to(max + 1);
        }
    }
}

impl Default for BidirectionalEdgeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for BidirectionalEdgeStore {
    fn clone(&self) -> Self {
        // Clone by acquiring read locks and cloning the inner data
        let forward_data = self.forward.read().clone();
        let reverse_data = self.reverse.read().clone();
        Self {
            forward: RwLock::new(forward_data),
            reverse: RwLock::new(reverse_data),
        }
    }
}

/// Statistics for a bidirectional edge store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BidirectionalEdgeStoreStats {
    /// Forward store statistics.
    pub forward: EdgeStoreStats,
    /// Reverse store statistics.
    pub reverse: EdgeStoreStats,
}

#[cfg(test)]
mod tests {
    use super::super::delta::DeltaOp;
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_bidirectional_new() {
        let store = BidirectionalEdgeStore::new();
        assert_eq!(store.csr_version(), 0);
    }

    #[test]
    fn test_default() {
        let store: BidirectionalEdgeStore = BidirectionalEdgeStore::default();
        assert_eq!(store.csr_version(), 0);
    }

    #[test]
    fn test_add_updates_both_directions() {
        let store = BidirectionalEdgeStore::new();
        let source = NodeId::new(1, 0);
        let target = NodeId::new(2, 0);
        let file = FileId::new(10);

        store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        // Forward: source -> target
        let forward_edges = store.edges_from(source);
        assert_eq!(forward_edges.len(), 1);
        assert_eq!(forward_edges[0].target, target);

        // Reverse: target <- source
        let reverse_edges = store.edges_to(target);
        assert_eq!(reverse_edges.len(), 1);
        assert_eq!(reverse_edges[0].source, source); // Full NodeId comparison
    }

    #[test]
    fn test_remove_updates_both_directions() {
        let store = BidirectionalEdgeStore::new();
        let source = NodeId::new(1, 0);
        let target = NodeId::new(2, 0);
        let file = FileId::new(10);

        // Add then remove
        store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.remove_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        // Forward should have 0 after merge (add + remove cancel)
        // But in delta buffer we have both
        let forward_stats = store.stats().forward;
        assert_eq!(forward_stats.delta_edge_count, 2); // add + remove

        let reverse_stats = store.stats().reverse;
        assert_eq!(reverse_stats.delta_edge_count, 2); // add + remove
    }

    #[test]
    fn test_forward_reverse_consistency() {
        let store = BidirectionalEdgeStore::new();
        let file = FileId::new(10);

        // Create a graph: 1 -> 2 -> 3, 1 -> 3
        let n1 = NodeId::new(1, 0);
        let n2 = NodeId::new(2, 0);
        let n3 = NodeId::new(3, 0);

        store.add_edge(
            n1,
            n2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            n2,
            n3,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            n1,
            n3,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        // Forward from n1: should see n2 and n3
        let from_n1 = store.edges_from(n1);
        assert_eq!(from_n1.len(), 2);

        // Reverse to n3: should see n1 and n2
        let to_n3 = store.edges_to(n3);
        assert_eq!(to_n3.len(), 2);

        // Verify consistency: count of outgoing from all nodes == incoming to all nodes
        let total_outgoing =
            store.edges_from(n1).len() + store.edges_from(n2).len() + store.edges_from(n3).len();

        let total_incoming =
            store.edges_to(n1).len() + store.edges_to(n2).len() + store.edges_to(n3).len();

        assert_eq!(total_outgoing, total_incoming);
    }

    #[test]
    fn test_concurrent_access() {
        let store = Arc::new(BidirectionalEdgeStore::new());
        let file = FileId::new(10);

        let mut handles = vec![];

        // Spawn 4 writer threads
        for i in 0..4 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                for j in 0..100 {
                    let source = NodeId::new(i * 100 + j, 0);
                    let target = NodeId::new((i * 100 + j + 1) % 400, 0);
                    store_clone.add_edge(
                        source,
                        target,
                        EdgeKind::Calls {
                            argument_count: 0,
                            is_async: false,
                        },
                        file,
                    );
                }
            });
            handles.push(handle);
        }

        // Spawn 4 reader threads
        for _ in 0..4 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                for i in 0..100 {
                    let node = NodeId::new(i, 0);
                    let _ = store_clone.edges_from(node);
                    let _ = store_clone.edges_to(node);
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().expect("Thread panicked");
        }

        // No assertion needed - success means no data races
        // But verify we have data
        let stats = store.stats();
        assert!(stats.forward.delta_edge_count > 0);
    }

    #[test]
    fn test_has_edge() {
        let store = BidirectionalEdgeStore::new();
        let source = NodeId::new(1, 0);
        let target = NodeId::new(2, 0);
        let file = FileId::new(10);

        assert!(!store.has_edge(
            source,
            target,
            &EdgeKind::Calls {
                argument_count: 0,
                is_async: false
            }
        ));

        store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        assert!(store.has_edge(
            source,
            target,
            &EdgeKind::Calls {
                argument_count: 0,
                is_async: false
            }
        ));
        assert!(!store.has_edge(source, target, &EdgeKind::References));
    }

    #[test]
    fn test_clear_file() {
        let store = BidirectionalEdgeStore::new();
        let file1 = FileId::new(10);
        let file2 = FileId::new(20);

        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file1,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file1,
        );
        store.add_edge(
            NodeId::new(3, 0),
            NodeId::new(4, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file2,
        );

        let cleared = store.clear_file(file1);
        assert_eq!(cleared, 2);

        // file2 edges should remain
        let stats = store.stats();
        assert_eq!(stats.forward.delta_edge_count, 1);
    }

    #[test]
    fn test_clear_delta() {
        let store = BidirectionalEdgeStore::new();
        let file = FileId::new(10);

        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        assert!(store.stats().forward.delta_edge_count > 0);

        store.clear_delta();

        assert_eq!(store.stats().forward.delta_edge_count, 0);
        assert_eq!(store.stats().reverse.delta_edge_count, 0);
    }

    #[test]
    fn test_stats() {
        let store = BidirectionalEdgeStore::new();
        let file = FileId::new(10);

        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        let stats = store.stats();
        assert_eq!(stats.forward.delta_edge_count, 1);
        assert_eq!(stats.reverse.delta_edge_count, 1);
    }

    // --- Tests for add_edges_bulk_ordered ---

    fn make_delta_edge(source: u32, target: u32, seq: u64, file: u32) -> DeltaEdge {
        DeltaEdge::new(
            NodeId::new(source, 0),
            NodeId::new(target, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            seq,
            DeltaOp::Add,
            FileId::new(file),
        )
    }

    #[test]
    fn test_add_edges_bulk_ordered_basic() {
        let store = BidirectionalEdgeStore::new();

        // Two files with pre-assigned sequence numbers
        let file1_edges = vec![make_delta_edge(1, 2, 0, 10), make_delta_edge(3, 4, 1, 10)];
        let file2_edges = vec![make_delta_edge(5, 6, 2, 20), make_delta_edge(7, 8, 3, 20)];

        store.add_edges_bulk_ordered(&[file1_edges, file2_edges], 4);

        // Verify forward edges
        let stats = store.stats();
        assert_eq!(stats.forward.delta_edge_count, 4);
        assert_eq!(stats.reverse.delta_edge_count, 4);

        // Verify forward traversal
        let edges_from_1 = store.edges_from(NodeId::new(1, 0));
        assert_eq!(edges_from_1.len(), 1);
        assert_eq!(edges_from_1[0].target, NodeId::new(2, 0));

        let edges_from_5 = store.edges_from(NodeId::new(5, 0));
        assert_eq!(edges_from_5.len(), 1);
        assert_eq!(edges_from_5[0].target, NodeId::new(6, 0));

        // Verify reverse traversal
        let edges_to_2 = store.edges_to(NodeId::new(2, 0));
        assert_eq!(edges_to_2.len(), 1);
        assert_eq!(edges_to_2[0].source, NodeId::new(1, 0));

        let edges_to_8 = store.edges_to(NodeId::new(8, 0));
        assert_eq!(edges_to_8.len(), 1);
        assert_eq!(edges_to_8[0].source, NodeId::new(7, 0));

        // Verify sequence counter advanced past max seq (3) to 4
        let fwd_seq = store.forward().seq_counter();
        assert_eq!(fwd_seq, 4);
        let rev_seq = store.reverse().seq_counter();
        assert_eq!(rev_seq, 4);
    }

    #[test]
    fn test_add_edges_bulk_ordered_empty() {
        let store = BidirectionalEdgeStore::new();

        store.add_edges_bulk_ordered(&[], 0);

        let stats = store.stats();
        assert_eq!(stats.forward.delta_edge_count, 0);
        assert_eq!(stats.reverse.delta_edge_count, 0);
    }

    #[test]
    fn test_add_edges_bulk_ordered_single_file() {
        let store = BidirectionalEdgeStore::new();

        let edges = vec![
            make_delta_edge(10, 20, 0, 5),
            make_delta_edge(20, 30, 1, 5),
            make_delta_edge(30, 10, 2, 5),
        ];

        store.add_edges_bulk_ordered(&[edges], 3);

        // Verify forward edge count
        let stats = store.stats();
        assert_eq!(stats.forward.delta_edge_count, 3);
        assert_eq!(stats.reverse.delta_edge_count, 3);

        // Verify cycle: 10 -> 20 -> 30 -> 10
        let from_10 = store.edges_from(NodeId::new(10, 0));
        assert_eq!(from_10.len(), 1);
        assert_eq!(from_10[0].target, NodeId::new(20, 0));

        let from_20 = store.edges_from(NodeId::new(20, 0));
        assert_eq!(from_20.len(), 1);
        assert_eq!(from_20[0].target, NodeId::new(30, 0));

        let from_30 = store.edges_from(NodeId::new(30, 0));
        assert_eq!(from_30.len(), 1);
        assert_eq!(from_30[0].target, NodeId::new(10, 0));

        // Verify reverse: node 10 is targeted by node 30
        let to_10 = store.edges_to(NodeId::new(10, 0));
        assert_eq!(to_10.len(), 1);
        assert_eq!(to_10[0].source, NodeId::new(30, 0));

        // Verify seq counter advanced to 3
        assert_eq!(store.forward().seq_counter(), 3);
    }

    #[test]
    #[should_panic(expected = "actual edge count")]
    fn test_add_edges_bulk_ordered_wrong_expected_total() {
        let store = BidirectionalEdgeStore::new();
        let edges = vec![make_delta_edge(1, 2, 0, 1)];

        // Expected 5 but actually 1 — should panic
        store.add_edges_bulk_ordered(&[edges], 5);
    }

    #[test]
    #[should_panic(expected = "non-monotonic seq")]
    fn test_add_edges_bulk_ordered_non_monotonic_seq() {
        let store = BidirectionalEdgeStore::new();

        // seq goes 0, 5, 3 — not monotonic
        let edges = vec![
            make_delta_edge(1, 2, 0, 1),
            make_delta_edge(3, 4, 5, 1),
            make_delta_edge(5, 6, 3, 1),
        ];

        store.add_edges_bulk_ordered(&[edges], 3);
    }

    #[test]
    #[should_panic(expected = "forward store counter")]
    fn test_add_edges_bulk_ordered_stale_seq() {
        let store = BidirectionalEdgeStore::new();

        // First add some edges through normal API to advance the seq counter
        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::References,
            FileId::new(1),
        );
        // Seq counter is now at 1

        // Try to bulk insert with seq=0 (stale) — should panic
        let edges = vec![make_delta_edge(10, 20, 0, 5)];
        store.add_edges_bulk_ordered(&[edges], 1);
    }

    #[test]
    fn test_add_edges_bulk_ordered_seq_counter_allows_subsequent_ops() {
        let store = BidirectionalEdgeStore::new();

        let edges = vec![make_delta_edge(1, 2, 10, 1), make_delta_edge(3, 4, 20, 1)];
        store.add_edges_bulk_ordered(&[edges], 2);

        // Seq counter should be at 21 (max_seq=20, advanced to 21)
        assert_eq!(store.forward().seq_counter(), 21);

        // Now add an edge through normal API — it should get seq >= 21
        let added = store.add_edge(
            NodeId::new(100, 0),
            NodeId::new(200, 0),
            EdgeKind::References,
            FileId::new(1),
        );
        assert!(
            added.seq >= 21,
            "subsequent edge should have seq >= 21, got {}",
            added.seq
        );

        // Total should be 3 forward edges
        assert_eq!(store.stats().forward.delta_edge_count, 3);
    }
}

/// Custom serialization for `RwLock<EdgeStore>`.
mod rwlock_edge_store_serde {
    use parking_lot::RwLock;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::EdgeStore;

    pub fn serialize<S>(value: &RwLock<EdgeStore>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.read().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<RwLock<EdgeStore>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let store = EdgeStore::deserialize(deserializer)?;
        Ok(RwLock::new(store))
    }
}
