//! Delta Merge Algorithm: Sequence-numbered last-writer-wins merge.
//!
//! This module implements the merge algorithm for compacting delta edges.
//! The algorithm ensures deterministic outcomes by using sequence numbers
//! to resolve conflicts between duplicate edge keys.
//!
//! # Design
//!
//! - ****: Monotonic sequence numbers enable deterministic ordering
//! - ****: Last-writer-wins semantics based on highest sequence number
//!
//! # Algorithm
//!
//! 1. Sort edges by `EdgeKey` (source, target, kind), then by DESCENDING seq
//! 2. Deduplicate by `EdgeKey` (keeps first element = highest seq due to sort)
//! 3. Filter out Remove operations
//! 4. Return merged edges with their winning sequence numbers
//!
//! # Thread Safety
//!
//! The merge function takes ownership of the input and returns owned output.
//! No internal synchronization is needed.
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::compaction::merge::merge_delta_edges;
//! use sqry_core::graph::unified::edge::{DeltaEdge, DeltaOp, EdgeKind};
//!
//! let edges = vec![
//!     DeltaEdge::new(n1, n2, EdgeKind::Calls { argument_count: 0, is_async: false }, 1, DeltaOp::Add, f1),
//!     DeltaEdge::new(n1, n2, EdgeKind::Calls { argument_count: 0, is_async: false }, 5, DeltaOp::Remove, f1),  // Higher seq wins
//! ];
//!
//! let (merged, stats) = merge_delta_edges(edges);
//! assert!(merged.is_empty());  // Remove won, so edge is excluded
//! ```

use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt;

#[cfg(test)]
use super::super::edge::ResolvedVia;
use super::super::edge::{DeltaEdge, DeltaOp, EdgeKey, EdgeKind};
use super::super::file::FileId;
use super::super::node::NodeId;
use crate::graph::node::Span;

/// A merged edge ready for CSR compaction.
///
/// Contains the edge data plus the winning sequence number for potential
/// future merges (sequence persistence across compaction).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedEdge {
    /// Source node
    pub source: NodeId,
    /// Target node
    pub target: NodeId,
    /// Edge kind
    pub kind: EdgeKind,
    /// Winning sequence number
    pub seq: u64,
    /// Source file
    pub file: FileId,
    /// Source spans of this edge (e.g., call-site locations for LSP call hierarchy).
    /// Multiple spans when the same edge has multiple call sites.
    pub spans: Vec<Span>,
}

impl MergedEdge {
    /// Creates a new merged edge.
    #[must_use]
    pub fn new(source: NodeId, target: NodeId, kind: EdgeKind, seq: u64, file: FileId) -> Self {
        Self {
            source,
            target,
            kind,
            seq,
            file,
            spans: Vec::new(),
        }
    }

    /// Creates a new merged edge with span data.
    #[must_use]
    pub fn with_spans(
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        seq: u64,
        file: FileId,
        spans: Vec<Span>,
    ) -> Self {
        Self {
            source,
            target,
            kind,
            seq,
            file,
            spans,
        }
    }

    /// Creates from a delta edge.
    #[must_use]
    pub fn from_delta(edge: &DeltaEdge) -> Self {
        Self {
            source: edge.source,
            target: edge.target,
            kind: edge.kind.clone(),
            seq: edge.seq,
            file: edge.file,
            spans: edge.spans.clone(),
        }
    }

    /// Returns the edge key for this merged edge.
    #[must_use]
    pub fn edge_key(&self) -> EdgeKey {
        EdgeKey {
            source: self.source,
            target: self.target,
            kind: self.kind.clone(),
        }
    }
}

/// Statistics from a merge operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MergeStats {
    /// Total input edges before merge
    pub input_count: usize,
    /// Output edges after deduplication
    pub output_count: usize,
    /// Edges deduplicated (kept only highest seq)
    pub deduplicated_count: usize,
    /// Edges removed (`DeltaOp::Remove` won)
    pub removed_count: usize,
}

impl MergeStats {
    /// Returns the compression ratio (output / input).
    #[must_use]
    pub fn compression_ratio(&self) -> f64 {
        if self.input_count == 0 {
            1.0
        } else {
            usize_to_f64(self.output_count) / usize_to_f64(self.input_count)
        }
    }
}

impl fmt::Display for MergeStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "merge: {} -> {} edges ({} deduped, {} removed, {:.1}% compression)",
            self.input_count,
            self.output_count,
            self.deduplicated_count,
            self.removed_count,
            self.compression_ratio() * 100.0
        )
    }
}

/// Process a single edge group (all edges with the same `EdgeKey`).
///
/// The group must be sorted by descending sequence number (highest first).
/// If the winning edge (highest seq) is an `Add`, a merged edge with accumulated
/// spans is pushed to `merged`. Returns `true` if the winning edge was a `Remove`.
fn process_edge_group(group: &[DeltaEdge], merged: &mut Vec<MergedEdge>) -> bool {
    // group[0] has highest seq (due to descending sort) - it's the winner
    let winner = &group[0];

    if winner.op == DeltaOp::Add {
        let spans = accumulate_and_deduplicate_spans(group);
        merged.push(MergedEdge {
            source: winner.source,
            target: winner.target,
            kind: winner.kind.clone(),
            seq: winner.seq,
            file: winner.file,
            spans,
        });
        false
    } else {
        // Remove won - edge is excluded
        true
    }
}

/// Accumulate and deduplicate spans from all `Add` edges in a group.
fn accumulate_and_deduplicate_spans(group: &[DeltaEdge]) -> Vec<Span> {
    let mut accumulated_spans: Vec<Span> = Vec::new();
    for edge in group {
        if edge.op == DeltaOp::Add {
            accumulated_spans.extend(edge.spans.iter().copied());
        }
    }

    accumulated_spans.sort_by(|a, b| {
        (a.start.line, a.start.column, a.end.line, a.end.column).cmp(&(
            b.start.line,
            b.start.column,
            b.end.line,
            b.end.column,
        ))
    });
    accumulated_spans.dedup();

    accumulated_spans
}

/// Merges delta edges using last-writer-wins semantics with span accumulation.
///
/// # Algorithm
///
/// 1. Sort edges by `EdgeKey` (source, target, kind), then by DESCENDING seq
/// 2. Group edges by `EdgeKey` and determine winner (highest seq)
/// 3. If winner is Add, accumulate all spans from Add edges in that group
/// 4. Filter out Remove winners
/// 5. Return merged edges with statistics
///
/// # Span Accumulation
///
/// When the same edge (source, target, kind) is added multiple times with
/// different call-site spans, ALL spans are preserved in the merged result.
/// This enables LSP call hierarchy to show all call sites for a caller/callee
/// pair.
///
/// # Arguments
///
/// * `edges` - Delta edges to merge (consumed)
///
/// # Returns
///
/// Tuple of (merged edges, statistics)
///
/// # Complexity
///
/// - Time: O(n log n) for sorting
/// - Space: O(n) for output
#[must_use]
pub fn merge_delta_edges(mut edges: Vec<DeltaEdge>) -> (Vec<MergedEdge>, MergeStats) {
    let input_count = edges.len();

    if edges.is_empty() {
        return (
            vec![],
            MergeStats {
                input_count: 0,
                output_count: 0,
                deduplicated_count: 0,
                removed_count: 0,
            },
        );
    }

    // Sort by EdgeKey, then by DESCENDING seq (highest seq first)
    edges.sort_by(|a, b| {
        let key_cmp = compare_edge_keys(&a.edge_key(), &b.edge_key());
        if key_cmp == Ordering::Equal {
            // DESCENDING: b.seq.cmp(&a.seq) puts highest seq first
            b.seq.cmp(&a.seq)
        } else {
            key_cmp
        }
    });

    // Group by EdgeKey and merge spans
    // For each group:
    // - Highest seq determines winner (Add or Remove)
    // - If Add wins, accumulate spans from all Add edges in group
    let mut merged: Vec<MergedEdge> = Vec::new();
    let mut unique_keys = 0usize;
    let mut removed_count = 0usize;

    let mut i = 0;
    while i < edges.len() {
        unique_keys += 1;
        let key = edges[i].edge_key();

        // Find the end of this group (same EdgeKey)
        let mut j = i + 1;
        while j < edges.len() && edges[j].edge_key() == key {
            j += 1;
        }

        if process_edge_group(&edges[i..j], &mut merged) {
            removed_count += 1;
        }

        i = j;
    }

    let deduplicated_count = input_count - unique_keys;

    let stats = MergeStats {
        input_count,
        output_count: merged.len(),
        deduplicated_count,
        removed_count,
    };

    (merged, stats)
}

/// Merges delta edges with existing CSR edges.
///
/// This variant accepts CSR edges (as `MergedEdge`) alongside delta edges,
/// enabling incremental compaction that preserves existing CSR data.
///
/// # Arguments
///
/// * `csr_edges` - Existing edges from CSR (not tombstoned)
/// * `delta_edges` - New delta edges
///
/// # Returns
///
/// Tuple of (merged edges, statistics)
#[must_use]
pub fn merge_with_csr(
    csr_edges: &[MergedEdge],
    delta_edges: Vec<DeltaEdge>,
) -> (Vec<MergedEdge>, MergeStats) {
    // Convert CSR edges to delta edges for uniform processing
    let csr_as_delta: Vec<DeltaEdge> = csr_edges
        .iter()
        .map(|e| DeltaEdge {
            source: e.source,
            target: e.target,
            kind: e.kind.clone(),
            seq: e.seq,
            op: DeltaOp::Add, // CSR edges are always adds
            file: e.file,
            spans: e.spans.clone(),
        })
        .collect();

    // Combine and merge
    let mut all_edges = csr_as_delta;
    all_edges.extend(delta_edges);

    merge_delta_edges(all_edges)
}

/// Merges delta edges grouped by file with cross-file LWW semantics.
///
/// This function properly handles cross-file removes by performing a final
/// global LWW deduplication pass. A Remove in file B will correctly cancel
/// an Add in file A if the Remove has a higher sequence number.
///
/// # Algorithm
///
/// 1. Collect all edges from all files
/// 2. Perform global LWW merge (respects cross-file removes)
/// 3. Partition surviving edges back by their source file
///
/// # Arguments
///
/// * `edges_by_file` - Delta edges grouped by source file
///
/// # Returns
///
/// Tuple of (merged edges grouped by file, total statistics)
#[must_use]
pub fn merge_by_file<S: std::hash::BuildHasher>(
    edges_by_file: HashMap<FileId, Vec<DeltaEdge>, S>,
) -> (HashMap<FileId, Vec<MergedEdge>>, MergeStats) {
    // Collect all edges for global LWW merge
    let input_count: usize = edges_by_file.values().map(std::vec::Vec::len).sum();
    let all_edges: Vec<DeltaEdge> = edges_by_file.into_values().flatten().collect();

    // Perform global merge (handles cross-file removes correctly)
    let (merged, stats) = merge_delta_edges(all_edges);

    // Partition surviving edges back by file
    let mut result: HashMap<FileId, Vec<MergedEdge>> = HashMap::new();
    for edge in merged {
        result.entry(edge.file).or_default().push(edge);
    }

    // Adjust stats since we're reporting the true input count
    let adjusted_stats = MergeStats {
        input_count,
        output_count: stats.output_count,
        deduplicated_count: stats.deduplicated_count,
        removed_count: stats.removed_count,
    };

    (result, adjusted_stats)
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

/// Compares two edge keys for sorting.
///
/// Since `EdgeKey` doesn't implement Ord (`EdgeKind` has associated data),
/// this function provides a stable comparison based on:
/// 1. Source node (by index, then generation)
/// 2. Target node (by index, then generation)
/// 3. Edge kind (by discriminant, then associated data if applicable)
fn compare_edge_keys(a: &EdgeKey, b: &EdgeKey) -> Ordering {
    // Compare source nodes
    let src_cmp = compare_node_ids(&a.source, &b.source);
    if src_cmp != Ordering::Equal {
        return src_cmp;
    }

    // Compare target nodes
    let tgt_cmp = compare_node_ids(&a.target, &b.target);
    if tgt_cmp != Ordering::Equal {
        return tgt_cmp;
    }

    // Compare edge kinds
    compare_edge_kinds(&a.kind, &b.kind)
}

/// Compares two node IDs for sorting.
fn compare_node_ids(a: &NodeId, b: &NodeId) -> Ordering {
    // Compare by index first (most significant)
    match a.index().cmp(&b.index()) {
        Ordering::Equal => a.generation().cmp(&b.generation()),
        other => other,
    }
}

/// Compares two edge kinds for sorting.
///
/// Uses discriminant for primary ordering, then associated data.
fn compare_edge_kinds(a: &EdgeKind, b: &EdgeKind) -> Ordering {
    // Use discriminant for fast comparison
    let disc_a = std::mem::discriminant(a);
    let disc_b = std::mem::discriminant(b);

    if disc_a == disc_b {
        // Same variant, compare associated data if any
        // Most EdgeKind variants have no associated data, so Equal is common
        // For variants with data (Http, Grpc, etc.), use debug format
        format!("{a:?}").cmp(&format!("{b:?}"))
    } else {
        // Compare discriminants by debug format (stable but not ideal)
        // For production, we'd want explicit ordering
        format!("{disc_a:?}").cmp(&format!("{disc_b:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::edge::EdgeKind;
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeId;

    fn make_node(index: u32) -> NodeId {
        NodeId::new(index, 0)
    }

    fn make_file(index: u32) -> FileId {
        FileId::new(index)
    }

    fn make_delta(src: u32, tgt: u32, kind: EdgeKind, seq: u64, op: DeltaOp) -> DeltaEdge {
        DeltaEdge::new(make_node(src), make_node(tgt), kind, seq, op, make_file(1))
    }

    fn make_delta_with_file(
        src: u32,
        tgt: u32,
        kind: EdgeKind,
        seq: u64,
        op: DeltaOp,
        file: u32,
    ) -> DeltaEdge {
        DeltaEdge::new(
            make_node(src),
            make_node(tgt),
            kind,
            seq,
            op,
            make_file(file),
        )
    }

    #[test]
    fn test_empty_merge() {
        let (merged, stats) = merge_delta_edges(vec![]);
        assert!(merged.is_empty());
        assert_eq!(stats.input_count, 0);
        assert_eq!(stats.output_count, 0);
    }

    #[test]
    fn test_single_edge() {
        let edges = vec![make_delta(
            1,
            2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            1,
            DeltaOp::Add,
        )];

        let (merged, stats) = merge_delta_edges(edges);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].source, make_node(1));
        assert_eq!(merged[0].target, make_node(2));
        assert_eq!(
            merged[0].kind,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            }
        );
        assert_eq!(merged[0].seq, 1);

        assert_eq!(stats.input_count, 1);
        assert_eq!(stats.output_count, 1);
        assert_eq!(stats.deduplicated_count, 0);
        assert_eq!(stats.removed_count, 0);
    }

    #[test]
    fn test_last_writer_wins_add() {
        // Same edge, different seq - highest seq wins
        let edges = vec![
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                1,
                DeltaOp::Add,
            ),
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                5,
                DeltaOp::Add,
            ),
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                3,
                DeltaOp::Add,
            ),
        ];

        let (merged, stats) = merge_delta_edges(edges);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].seq, 5); // Highest seq wins

        assert_eq!(stats.input_count, 3);
        assert_eq!(stats.output_count, 1);
        assert_eq!(stats.deduplicated_count, 2);
        assert_eq!(stats.removed_count, 0);
    }

    #[test]
    fn test_last_writer_wins_remove() {
        // Remove with higher seq wins
        let edges = vec![
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                1,
                DeltaOp::Add,
            ),
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                5,
                DeltaOp::Remove,
            ),
        ];

        let (merged, stats) = merge_delta_edges(edges);
        assert!(merged.is_empty()); // Remove won, edge excluded

        assert_eq!(stats.input_count, 2);
        assert_eq!(stats.output_count, 0);
        assert_eq!(stats.deduplicated_count, 1);
        assert_eq!(stats.removed_count, 1);
    }

    #[test]
    fn test_add_after_remove() {
        // Add with higher seq wins over previous remove
        let edges = vec![
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                1,
                DeltaOp::Add,
            ),
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                3,
                DeltaOp::Remove,
            ),
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                7,
                DeltaOp::Add,
            ),
        ];

        let (merged, stats) = merge_delta_edges(edges);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].seq, 7);

        assert_eq!(stats.input_count, 3);
        assert_eq!(stats.output_count, 1);
        assert_eq!(stats.deduplicated_count, 2);
        assert_eq!(stats.removed_count, 0);
    }

    #[test]
    fn test_different_edge_kinds() {
        // Same source/target, different kinds - both preserved
        let edges = vec![
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                1,
                DeltaOp::Add,
            ),
            make_delta(
                1,
                2,
                EdgeKind::Imports {
                    alias: None,
                    is_wildcard: false,
                },
                2,
                DeltaOp::Add,
            ),
        ];

        let (merged, stats) = merge_delta_edges(edges);
        assert_eq!(merged.len(), 2);

        assert_eq!(stats.input_count, 2);
        assert_eq!(stats.output_count, 2);
        assert_eq!(stats.deduplicated_count, 0);
    }

    #[test]
    fn test_different_targets() {
        // Same source, different targets - both preserved
        let edges = vec![
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                1,
                DeltaOp::Add,
            ),
            make_delta(
                1,
                3,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                2,
                DeltaOp::Add,
            ),
        ];

        let (merged, stats) = merge_delta_edges(edges);
        assert_eq!(merged.len(), 2);
        assert_eq!(stats.deduplicated_count, 0);
    }

    #[test]
    fn test_different_sources() {
        // Different sources, same target - both preserved
        let edges = vec![
            make_delta(
                1,
                3,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                1,
                DeltaOp::Add,
            ),
            make_delta(
                2,
                3,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                2,
                DeltaOp::Add,
            ),
        ];

        let (merged, stats) = merge_delta_edges(edges);
        assert_eq!(merged.len(), 2);
        assert_eq!(stats.deduplicated_count, 0);
    }

    #[test]
    fn test_complex_merge() {
        let edges = vec![
            // Edge A: add at 1, remove at 3, add at 5 -> add wins (seq 5)
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                1,
                DeltaOp::Add,
            ),
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                3,
                DeltaOp::Remove,
            ),
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                5,
                DeltaOp::Add,
            ),
            // Edge B: add at 2, remove at 4 -> remove wins (seq 4)
            make_delta(
                2,
                3,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                2,
                DeltaOp::Add,
            ),
            make_delta(
                2,
                3,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                4,
                DeltaOp::Remove,
            ),
            // Edge C: add at 6 -> add (seq 6)
            make_delta(
                3,
                4,
                EdgeKind::Imports {
                    alias: None,
                    is_wildcard: false,
                },
                6,
                DeltaOp::Add,
            ),
        ];

        let (merged, stats) = merge_delta_edges(edges);
        assert_eq!(merged.len(), 2); // A and C survive

        // Verify edge A
        let edge_a = merged.iter().find(|e| e.source == make_node(1)).unwrap();
        assert_eq!(edge_a.seq, 5);

        // Verify edge C
        let edge_c = merged.iter().find(|e| e.source == make_node(3)).unwrap();
        assert_eq!(edge_c.seq, 6);

        assert_eq!(stats.input_count, 6);
        assert_eq!(stats.output_count, 2);
        assert_eq!(stats.deduplicated_count, 3); // A: 2 deduped, B: 1 deduped
        assert_eq!(stats.removed_count, 1); // Edge B removed
    }

    #[test]
    fn test_merge_stats_display() {
        let stats = MergeStats {
            input_count: 100,
            output_count: 60,
            deduplicated_count: 30,
            removed_count: 10,
        };

        let display = format!("{stats}");
        assert!(display.contains("100 -> 60"));
        assert!(display.contains("30 deduped"));
        assert!(display.contains("10 removed"));
    }

    #[test]
    fn test_compression_ratio() {
        let stats = MergeStats {
            input_count: 100,
            output_count: 50,
            deduplicated_count: 40,
            removed_count: 10,
        };

        assert!((stats.compression_ratio() - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_compression_ratio_empty() {
        let stats = MergeStats::default();
        assert!((stats.compression_ratio() - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_merged_edge_from_delta() {
        let delta = make_delta(
            1,
            2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            42,
            DeltaOp::Add,
        );
        let merged = MergedEdge::from_delta(&delta);

        assert_eq!(merged.source, delta.source);
        assert_eq!(merged.target, delta.target);
        assert_eq!(merged.kind, delta.kind);
        assert_eq!(merged.seq, delta.seq);
        assert_eq!(merged.file, delta.file);
    }

    #[test]
    fn test_merge_with_csr() {
        let csr_edges = vec![MergedEdge::new(
            make_node(1),
            make_node(2),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            1,
            make_file(1),
        )];

        let delta_edges = vec![
            // Higher seq update to existing edge
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                5,
                DeltaOp::Add,
            ),
            // New edge
            make_delta(
                3,
                4,
                EdgeKind::Imports {
                    alias: None,
                    is_wildcard: false,
                },
                6,
                DeltaOp::Add,
            ),
        ];

        let (merged, stats) = merge_with_csr(&csr_edges, delta_edges);

        // Edge 1->2 should have seq 5 (delta wins)
        let edge_1_2 = merged.iter().find(|e| e.source == make_node(1)).unwrap();
        assert_eq!(edge_1_2.seq, 5);

        // Edge 3->4 should be present
        let edge_3_4 = merged.iter().find(|e| e.source == make_node(3)).unwrap();
        assert_eq!(edge_3_4.seq, 6);

        assert_eq!(merged.len(), 2);
        assert_eq!(stats.input_count, 3);
        assert_eq!(stats.deduplicated_count, 1);
    }

    #[test]
    fn test_merge_by_file() {
        let mut edges_by_file = HashMap::new();

        // Use make_delta_with_file to set correct file IDs in edges
        edges_by_file.insert(
            make_file(1),
            vec![
                make_delta_with_file(
                    1,
                    2,
                    EdgeKind::Calls {
                        argument_count: 0,
                        is_async: false,
                        resolved_via: ResolvedVia::Direct,
                    },
                    1,
                    DeltaOp::Add,
                    1,
                ),
                make_delta_with_file(
                    1,
                    2,
                    EdgeKind::Calls {
                        argument_count: 0,
                        is_async: false,
                        resolved_via: ResolvedVia::Direct,
                    },
                    3,
                    DeltaOp::Add,
                    1,
                ),
            ],
        );

        edges_by_file.insert(
            make_file(2),
            vec![make_delta_with_file(
                3,
                4,
                EdgeKind::Imports {
                    alias: None,
                    is_wildcard: false,
                },
                2,
                DeltaOp::Add,
                2,
            )],
        );

        let (result, stats) = merge_by_file(edges_by_file);

        assert_eq!(result.len(), 2);
        assert_eq!(result.get(&make_file(1)).unwrap().len(), 1);
        assert_eq!(result.get(&make_file(2)).unwrap().len(), 1);

        assert_eq!(stats.input_count, 3);
        assert_eq!(stats.output_count, 2);
        assert_eq!(stats.deduplicated_count, 1);
    }

    #[test]
    fn test_merge_by_file_cross_file_remove() {
        // Test that a Remove in file2 correctly cancels an Add in file1
        // This is the "cross-chunk removes" fix
        let mut edges_by_file = HashMap::new();

        // File 1: Add edge 1->2 with seq=1
        edges_by_file.insert(
            make_file(1),
            vec![make_delta_with_file(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                1,
                DeltaOp::Add,
                1,
            )],
        );

        // File 2: Remove same edge 1->2 with higher seq=5
        edges_by_file.insert(
            make_file(2),
            vec![make_delta_with_file(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                5,
                DeltaOp::Remove,
                2,
            )],
        );

        let (result, stats) = merge_by_file(edges_by_file);

        // The Remove should win (higher seq), so edge should be gone
        assert!(
            result.is_empty() || result.values().all(Vec::is_empty),
            "Cross-file remove should cancel the add"
        );
        assert_eq!(stats.input_count, 2);
        assert_eq!(stats.output_count, 0);
        assert_eq!(stats.removed_count, 1);
    }

    #[test]
    fn test_merge_by_file_cross_file_add_wins() {
        // Test that a later Add in file2 correctly wins over Remove in file1
        let mut edges_by_file = HashMap::new();

        // File 1: Remove edge 1->2 with seq=3
        edges_by_file.insert(
            make_file(1),
            vec![make_delta_with_file(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                3,
                DeltaOp::Remove,
                1,
            )],
        );

        // File 2: Add same edge 1->2 with higher seq=7
        edges_by_file.insert(
            make_file(2),
            vec![make_delta_with_file(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                7,
                DeltaOp::Add,
                2,
            )],
        );

        let (result, stats) = merge_by_file(edges_by_file);

        // The Add with seq=7 should win and be in file2 (winning edge's file)
        assert_eq!(result.len(), 1);
        let file2_edges = result.get(&make_file(2)).expect("Edge should be in file2");
        assert_eq!(file2_edges.len(), 1);
        assert_eq!(file2_edges[0].seq, 7);
        assert_eq!(stats.output_count, 1);
    }

    #[test]
    fn test_sequence_stability() {
        // When multiple edges have same key and different ops,
        // the highest seq determines the outcome
        let edges = vec![
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                10,
                DeltaOp::Add,
            ),
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                5,
                DeltaOp::Remove,
            ),
            make_delta(
                1,
                2,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                1,
                DeltaOp::Add,
            ),
        ];

        let (merged, _) = merge_delta_edges(edges);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].seq, 10);
    }

    #[test]
    fn test_preserves_file_id() {
        let delta = DeltaEdge::new(
            make_node(1),
            make_node(2),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            1,
            DeltaOp::Add,
            make_file(42),
        );

        let (merged, _) = merge_delta_edges(vec![delta]);
        assert_eq!(merged[0].file, make_file(42));
    }

    #[test]
    fn test_node_id_ordering() {
        // Verify nodes are sorted by index then generation
        let node_a = NodeId::new(1, 0);
        let node_b = NodeId::new(1, 1);
        let node_c = NodeId::new(2, 0);

        assert_eq!(compare_node_ids(&node_a, &node_b), Ordering::Less);
        assert_eq!(compare_node_ids(&node_b, &node_a), Ordering::Greater);
        assert_eq!(compare_node_ids(&node_a, &node_c), Ordering::Less);
        assert_eq!(compare_node_ids(&node_a, &node_a), Ordering::Equal);
    }
}
