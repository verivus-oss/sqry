//! CSR Build Phase: Offline construction of compacted CSR graphs.
//!
//! This module implements Phase 1 of the two-phase compaction process.
//! It builds new CSR graphs from merged edges without holding any locks.
//!
//! # Design
//!
//! - **Lock-free build**: CSR construction happens on owned data, no locks needed
//! - **Snapshot-based**: Input is a snapshot of merged edges from both CSR and delta
//! - **Deterministic**: Same input always produces the same CSR
//!
//! # Algorithm
//!
//! 1. Take snapshot of current state (CSR edges + delta edges)
//! 2. Merge using last-writer-wins semantics
//! 3. Sort merged edges by source node
//! 4. Build CSR arrays (`row_ptr`, `col_idx`, `edge_kind`, `edge_seq`)
//! 5. Validate and return new CSR
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::compaction::build::{
//!     build_compacted_csr, CompactionSnapshot, snapshot_edges,
//! };
//!
//! // Take snapshot (no locks held after this returns)
//! let snapshot = snapshot_edges(&edge_store);
//!
//! // Build CSR offline (no locks needed)
//! let (new_csr, stats) = build_compacted_csr(snapshot, Direction::Forward)?;
//! ```

use super::super::edge::{DeltaEdge, EdgeKind, EdgeStore};
use super::super::node::NodeId;
use super::super::storage::{CsrError, CsrGraph};
use super::errors::{BuildFailureReason, CompactionError, Direction};
use super::merge::{MergeStats, MergedEdge, merge_delta_edges};

/// Snapshot of edges for offline compaction.
///
/// This struct holds owned copies of edge data, allowing the compaction
/// to proceed without holding any locks on the original data structures.
#[derive(Debug, Clone)]
pub struct CompactionSnapshot {
    /// Edges from the current CSR (filtered by tombstones).
    pub csr_edges: Vec<MergedEdge>,
    /// Edges from the delta buffer.
    pub delta_edges: Vec<DeltaEdge>,
    /// Current node count (determines CSR size).
    pub node_count: usize,
    /// Current CSR version (for precondition checking during swap).
    pub csr_version: u64,
}

impl CompactionSnapshot {
    /// Creates an empty snapshot.
    #[must_use]
    pub fn empty(node_count: usize) -> Self {
        Self {
            csr_edges: Vec::new(),
            delta_edges: Vec::new(),
            node_count,
            csr_version: 0,
        }
    }

    /// Returns the total number of edges in the snapshot (before merge).
    #[must_use]
    pub fn total_edges(&self) -> usize {
        self.csr_edges.len() + self.delta_edges.len()
    }
}

/// Statistics from the CSR build phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BuildStats {
    /// Number of edges in the input CSR.
    pub csr_input_edges: usize,
    /// Number of edges in the delta buffer.
    pub delta_input_edges: usize,
    /// Statistics from the merge phase.
    pub merge_stats: MergeStats,
    /// Number of edges in the output CSR.
    pub output_edges: usize,
    /// Number of nodes in the output CSR.
    pub output_nodes: usize,
}

/// Creates a snapshot of edges from an `EdgeStore` for offline compaction.
///
/// This function reads from the store and creates owned copies of all edges.
/// After this function returns, no locks are needed for the compaction.
///
/// # Arguments
///
/// * `store` - The edge store to snapshot
/// * `node_count` - The current node count (determines CSR size)
///
/// # Returns
///
/// A `CompactionSnapshot` containing all edges ready for merge and build.
///
/// # Panics
///
/// Panics if the CSR contains more than `u32::MAX` nodes.
#[must_use]
pub fn snapshot_edges(store: &EdgeStore, node_count: usize) -> CompactionSnapshot {
    let mut csr_edges = Vec::new();

    // Extract non-tombstoned CSR edges
    if let Some(csr) = store.csr() {
        for node_idx in 0..csr.node_count() {
            let node_idx_u32 = u32::try_from(node_idx).expect("CSR node index exceeds u32::MAX");
            for edge_ref in csr.edges_of(node_idx_u32) {
                // Skip tombstoned edges - check against store's tombstone bitmap
                if store.is_edge_tombstoned(edge_ref.index) {
                    continue;
                }

                csr_edges.push(MergedEdge {
                    source: NodeId::new(node_idx_u32, 0), // Generation not tracked in CSR
                    target: edge_ref.target,
                    kind: edge_ref.kind,
                    seq: edge_ref.seq,
                    file: super::super::file::FileId::new(0), // File not tracked in CSR
                    spans: edge_ref.spans,                    // Preserve spans from CSR
                });
            }
        }
    }

    // Extract all delta edges
    let delta_edges: Vec<DeltaEdge> = store.delta().iter().cloned().collect();

    CompactionSnapshot {
        csr_edges,
        delta_edges,
        node_count,
        csr_version: store.csr_version(),
    }
}

/// Builds a compacted CSR from a snapshot.
///
/// This function performs the offline build phase of compaction:
/// 1. Merges CSR and delta edges using last-writer-wins
/// 2. Sorts edges by source node
/// 3. Builds CSR arrays
/// 4. Validates the result
///
/// # Arguments
///
/// * `snapshot` - The edge snapshot to compact
/// * `direction` - The direction (Forward/Reverse) for error reporting
///
/// # Returns
///
/// A tuple of (`CsrGraph`, `BuildStats`) on success, or a `CompactionError` on failure.
///
/// # Errors
///
/// Returns `CompactionError::BuildFailed` if:
/// - Edge sorting fails
/// - CSR array construction fails
/// - CSR validation fails
pub fn build_compacted_csr(
    snapshot: CompactionSnapshot,
    direction: Direction,
) -> Result<(CsrGraph, BuildStats), CompactionError> {
    let csr_input_edges = snapshot.csr_edges.len();
    let delta_input_edges = snapshot.delta_edges.len();
    let node_count = snapshot.node_count;

    // Handle empty case
    if node_count == 0 {
        return Ok((
            CsrGraph::empty(0),
            BuildStats {
                csr_input_edges,
                delta_input_edges,
                merge_stats: MergeStats::default(),
                output_edges: 0,
                output_nodes: 0,
            },
        ));
    }

    // Convert CSR edges to DeltaEdge format for merge
    // Note: CSR edges are already "surviving" edges, so they're adds
    // Important: Preserve span data from CSR edges using with_spans
    let mut all_delta_edges: Vec<DeltaEdge> = snapshot
        .csr_edges
        .into_iter()
        .map(|e| {
            DeltaEdge::with_spans(
                e.source,
                e.target,
                e.kind,
                e.seq,
                super::super::edge::DeltaOp::Add,
                e.file,
                e.spans,
            )
        })
        .collect();

    // Add delta buffer edges
    all_delta_edges.extend(snapshot.delta_edges);

    // Merge using LWW semantics
    let (merged_edges, merge_stats) = merge_delta_edges(all_delta_edges);

    // Build CSR from merged edges
    let csr = build_csr_from_edges(&merged_edges, node_count).map_err(|e| {
        CompactionError::BuildFailed {
            direction,
            reason: BuildFailureReason::BuilderError {
                message: e.to_string(),
            },
        }
    })?;

    let output_edges = csr.edge_count();
    let output_nodes = csr.node_count();

    Ok((
        csr,
        BuildStats {
            csr_input_edges,
            delta_input_edges,
            merge_stats,
            output_edges,
            output_nodes,
        },
    ))
}

/// Builds a CSR graph directly from merged edges.
///
/// This is the core CSR construction algorithm:
/// 1. Sort edges by source node
/// 2. Count edges per source (for `row_ptr`)
/// 3. Build arrays
///
/// # Arguments
///
/// * `edges` - Pre-merged edges (no duplicates, removes filtered out)
/// * `node_count` - The number of nodes in the graph
///
/// # Returns
///
/// A validated `CsrGraph` on success, or `CsrError` on failure.
///
/// # Errors
///
/// Returns an error if the CSR structure fails validation.
///
/// # Panics
///
/// Panics if edge counts exceed `u32::MAX` during row pointer construction.
pub fn build_csr_from_edges(edges: &[MergedEdge], node_count: usize) -> Result<CsrGraph, CsrError> {
    if node_count == 0 {
        return Ok(CsrGraph::empty(0));
    }

    // Sort edges by source index
    let mut sorted_edges: Vec<&MergedEdge> = edges.iter().collect();
    sorted_edges.sort_by_key(|e| e.source.index());

    // Count edges per source node
    let mut edge_counts: Vec<usize> = vec![0; node_count];
    for edge in &sorted_edges {
        let src_idx = edge.source.index() as usize;
        if src_idx < node_count {
            edge_counts[src_idx] += 1;
        }
    }

    // Build row_ptr (prefix sum of edge counts)
    let mut row_ptr: Vec<u32> = Vec::with_capacity(node_count + 1);
    row_ptr.push(0);
    let mut cumulative = 0u32;
    for count in &edge_counts {
        let count_u32 = u32::try_from(*count).expect("CSR edge count exceeds u32::MAX");
        cumulative = cumulative
            .checked_add(count_u32)
            .expect("CSR row_ptr overflow");
        row_ptr.push(cumulative);
    }

    // Build col_idx, edge_kind, edge_seq, edge_spans
    let edge_count = sorted_edges.len();
    let mut col_idx: Vec<NodeId> = Vec::with_capacity(edge_count);
    let mut edge_kind: Vec<EdgeKind> = Vec::with_capacity(edge_count);
    let mut edge_seq: Vec<u64> = Vec::with_capacity(edge_count);
    let mut edge_spans: Vec<Vec<crate::graph::node::Span>> = Vec::with_capacity(edge_count);

    for edge in sorted_edges {
        col_idx.push(edge.target);
        edge_kind.push(edge.kind.clone());
        edge_seq.push(edge.seq);
        edge_spans.push(edge.spans.clone());
    }

    // Construct and validate
    let csr = CsrGraph::from_raw(
        node_count, row_ptr, col_idx, edge_kind, edge_seq, edge_spans,
    );
    csr.validate()?;

    Ok(csr)
}

#[cfg(test)]
mod tests {
    use super::super::super::edge::{DeltaOp, EdgeKind};
    use super::super::super::file::FileId;
    use super::super::super::node::NodeId;
    use super::*;

    fn make_merged_edge(source: u32, target: u32, seq: u64) -> MergedEdge {
        MergedEdge::new(
            NodeId::new(source, 0),
            NodeId::new(target, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            seq,
            FileId::new(1),
        )
    }

    fn make_delta_edge(source: u32, target: u32, seq: u64, op: DeltaOp) -> DeltaEdge {
        DeltaEdge::new(
            NodeId::new(source, 0),
            NodeId::new(target, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            seq,
            op,
            FileId::new(1),
        )
    }

    #[test]
    fn test_compaction_snapshot_empty() {
        let snapshot = CompactionSnapshot::empty(10);
        assert_eq!(snapshot.node_count, 10);
        assert_eq!(snapshot.total_edges(), 0);
        assert_eq!(snapshot.csr_version, 0);
    }

    #[test]
    fn test_build_csr_from_edges_empty() {
        let edges: Vec<MergedEdge> = vec![];
        let csr = build_csr_from_edges(&edges, 5).unwrap();

        assert_eq!(csr.node_count(), 5);
        assert_eq!(csr.edge_count(), 0);
        assert!(csr.validate().is_ok());
    }

    #[test]
    fn test_build_csr_from_edges_simple() {
        let edges = vec![
            make_merged_edge(0, 1, 1),
            make_merged_edge(0, 2, 2),
            make_merged_edge(1, 2, 3),
        ];

        let csr = build_csr_from_edges(&edges, 3).unwrap();

        assert_eq!(csr.node_count(), 3);
        assert_eq!(csr.edge_count(), 3);
        assert_eq!(csr.out_degree(0), 2);
        assert_eq!(csr.out_degree(1), 1);
        assert_eq!(csr.out_degree(2), 0);
        assert!(csr.validate().is_ok());
    }

    #[test]
    fn test_build_csr_from_edges_unsorted() {
        // Edges not sorted by source - should still work
        let edges = vec![
            make_merged_edge(2, 0, 3),
            make_merged_edge(0, 1, 1),
            make_merged_edge(1, 2, 2),
        ];

        let csr = build_csr_from_edges(&edges, 3).unwrap();

        assert_eq!(csr.edge_count(), 3);
        assert_eq!(csr.out_degree(0), 1);
        assert_eq!(csr.out_degree(1), 1);
        assert_eq!(csr.out_degree(2), 1);
        assert!(csr.validate().is_ok());
    }

    #[test]
    fn test_build_csr_from_edges_zero_nodes() {
        let edges: Vec<MergedEdge> = vec![];
        let csr = build_csr_from_edges(&edges, 0).unwrap();

        assert_eq!(csr.node_count(), 0);
        assert_eq!(csr.edge_count(), 0);
        assert!(csr.validate().is_ok());
    }

    #[test]
    fn test_build_compacted_csr_empty_snapshot() {
        let snapshot = CompactionSnapshot::empty(5);
        let (csr, stats) = build_compacted_csr(snapshot, Direction::Forward).unwrap();

        assert_eq!(csr.node_count(), 5);
        assert_eq!(csr.edge_count(), 0);
        assert_eq!(stats.csr_input_edges, 0);
        assert_eq!(stats.delta_input_edges, 0);
        assert_eq!(stats.output_edges, 0);
    }

    #[test]
    fn test_build_compacted_csr_with_delta_edges() {
        let snapshot = CompactionSnapshot {
            csr_edges: vec![],
            delta_edges: vec![
                make_delta_edge(0, 1, 1, DeltaOp::Add),
                make_delta_edge(1, 2, 2, DeltaOp::Add),
            ],
            node_count: 3,
            csr_version: 0,
        };

        let (csr, stats) = build_compacted_csr(snapshot, Direction::Forward).unwrap();

        assert_eq!(csr.edge_count(), 2);
        assert_eq!(stats.delta_input_edges, 2);
        assert_eq!(stats.output_edges, 2);
    }

    #[test]
    fn test_build_compacted_csr_merge_removes_duplicates() {
        let snapshot = CompactionSnapshot {
            csr_edges: vec![make_merged_edge(0, 1, 1)], // Old edge seq=1
            delta_edges: vec![
                make_delta_edge(0, 1, 5, DeltaOp::Add), // Same edge, newer seq=5
            ],
            node_count: 3,
            csr_version: 1,
        };

        let (csr, stats) = build_compacted_csr(snapshot, Direction::Forward).unwrap();

        // Should have 1 edge (deduplicated)
        assert_eq!(csr.edge_count(), 1);
        assert_eq!(stats.csr_input_edges, 1);
        assert_eq!(stats.delta_input_edges, 1);
        assert_eq!(stats.output_edges, 1);

        // The winning edge should have seq=5
        let edge = csr.edge_at(0).unwrap();
        assert_eq!(edge.seq, 5);
    }

    #[test]
    fn test_build_compacted_csr_remove_wins() {
        let snapshot = CompactionSnapshot {
            csr_edges: vec![make_merged_edge(0, 1, 1)], // Edge exists seq=1
            delta_edges: vec![
                make_delta_edge(0, 1, 5, DeltaOp::Remove), // Remove with seq=5
            ],
            node_count: 3,
            csr_version: 1,
        };

        let (csr, stats) = build_compacted_csr(snapshot, Direction::Forward).unwrap();

        // Edge should be removed (Remove wins with higher seq)
        assert_eq!(csr.edge_count(), 0);
        assert_eq!(stats.csr_input_edges, 1);
        assert_eq!(stats.delta_input_edges, 1);
        assert_eq!(stats.output_edges, 0);
    }

    #[test]
    fn test_build_compacted_csr_add_after_remove() {
        let snapshot = CompactionSnapshot {
            csr_edges: vec![],
            delta_edges: vec![
                make_delta_edge(0, 1, 1, DeltaOp::Add),
                make_delta_edge(0, 1, 2, DeltaOp::Remove),
                make_delta_edge(0, 1, 3, DeltaOp::Add), // Add wins with highest seq
            ],
            node_count: 3,
            csr_version: 0,
        };

        let (csr, stats) = build_compacted_csr(snapshot, Direction::Forward).unwrap();

        // Edge should exist (Add with seq=3 wins)
        assert_eq!(csr.edge_count(), 1);
        assert_eq!(stats.delta_input_edges, 3);
        assert_eq!(stats.output_edges, 1);
        assert_eq!(stats.merge_stats.deduplicated_count, 2);
    }

    #[test]
    fn test_build_stats() {
        let snapshot = CompactionSnapshot {
            csr_edges: vec![make_merged_edge(0, 1, 1), make_merged_edge(1, 2, 2)],
            delta_edges: vec![
                make_delta_edge(2, 0, 3, DeltaOp::Add),
                make_delta_edge(0, 1, 4, DeltaOp::Add), // Duplicate of CSR edge
            ],
            node_count: 3,
            csr_version: 1,
        };

        let (csr, stats) = build_compacted_csr(snapshot, Direction::Forward).unwrap();

        assert_eq!(stats.csr_input_edges, 2);
        assert_eq!(stats.delta_input_edges, 2);
        assert_eq!(stats.output_edges, 3); // 2 from CSR (1 updated) + 1 new from delta
        assert_eq!(stats.output_nodes, 3);
        assert!(csr.validate().is_ok());
    }

    #[test]
    fn test_build_csr_preserves_edge_data() {
        use super::super::super::edge::EdgeKind;

        let edges = vec![MergedEdge::new(
            NodeId::new(0, 0),
            NodeId::new(1, 0),
            EdgeKind::References,
            42,
            FileId::new(99),
        )];

        let csr = build_csr_from_edges(&edges, 2).unwrap();
        let edge_ref = csr.edge_at(0).unwrap();

        assert_eq!(edge_ref.target, NodeId::new(1, 0));
        assert_eq!(edge_ref.kind, EdgeKind::References);
        assert_eq!(edge_ref.seq, 42);
    }
}
