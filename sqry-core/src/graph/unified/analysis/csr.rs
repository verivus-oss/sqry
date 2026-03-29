//! CSR (Compressed Sparse Row) adjacency representation
//!
//! Builds a read-optimized CSR adjacency matrix directly from the compacted edge snapshot.
//! This avoids per-node `edges_from()` iteration which was the performance bottleneck.

use crate::graph::unified::compaction::CompactionSnapshot;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::NodeId;
use crate::graph::unified::storage::CsrGraph;
use anyhow::Result;
use rayon::prelude::*;

/// Edge kind discriminant for filtering during traversal
///
/// Since `EdgeKind` has struct variants with metadata, we can't simply cast to u16.
/// Instead, we use `std::mem::discriminant` for comparison during filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeKindDiscriminant(std::mem::Discriminant<EdgeKind>);

impl EdgeKindDiscriminant {
    /// Create a discriminant from an edge kind
    #[must_use]
    pub fn from_edge_kind(kind: &EdgeKind) -> Self {
        EdgeKindDiscriminant(std::mem::discriminant(kind))
    }

    /// Check if this discriminant matches the given edge kind
    #[must_use]
    pub fn matches(&self, kind: &EdgeKind) -> bool {
        self.0 == std::mem::discriminant(kind)
    }
}

// Note: EdgeKind already contains all metadata in its variants,
// so we don't need a separate EdgeMetadata type. We'll store
// the full EdgeKind per edge.

/// CSR adjacency matrix for the graph
///
/// Storage layout:
/// - `row_offsets[i]` = index into `col_indices` where edges for node i start
/// - `col_indices[row_offsets[i]..row_offsets[i+1]]` = target nodes for node i
/// - `edge_kinds[row_offsets[i]..row_offsets[i+1]]` = edge kinds (with full metadata)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CsrAdjacency {
    /// Number of nodes in the graph
    pub node_count: u32,
    /// Number of edges in the graph
    pub edge_count: u32,
    /// CSR row offsets (length = `node_count` + 1)
    pub row_offsets: Vec<u32>,
    /// CSR column indices (target nodes)
    pub col_indices: Vec<u32>,
    /// Edge kinds with full metadata
    pub edge_kinds: Vec<EdgeKind>,
}

impl CsrAdjacency {
    /// Build CSR adjacency directly from an already-compacted `CsrGraph`.
    ///
    /// This is the fast path when the caller has already built a storage CSR
    /// (e.g., during pre-save compaction). It avoids the expensive LWW merge
    /// and sort that `build_from_snapshot` performs.
    ///
    /// # Arguments
    ///
    /// * `csr` - A fully compacted forward `CsrGraph`
    #[allow(clippy::cast_possible_truncation)]
    #[must_use]
    pub fn from_csr_graph(csr: &CsrGraph) -> Self {
        let node_count = csr.node_count() as u32;
        let edge_count = csr.edge_count() as u32;
        let row_offsets = csr.row_ptr().to_vec();
        let col_indices: Vec<u32> = csr.col_idx().iter().map(|nid| nid.index()).collect();
        let edge_kinds = csr.edge_kind().to_vec();

        Self {
            node_count,
            edge_count,
            row_offsets,
            col_indices,
            edge_kinds,
        }
    }

    /// Build CSR adjacency directly from compacted edge snapshot
    ///
    /// This reuses the existing compaction logic to merge `EdgeStore` CSR + Delta,
    /// apply tombstones, and resolve LWW conflicts.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    ///
    /// # Panics
    ///
    /// Panics if `row_offsets` is empty when computing prefix sum.
    #[allow(clippy::cast_possible_truncation)] // Graph sizes realistically won't exceed u32::MAX
    pub fn build_from_snapshot(snapshot: &CompactionSnapshot) -> Result<Self> {
        let node_count = snapshot.node_count as u32;

        // Step 1: Merge CSR and delta edges using LWW semantics (tombstones honored).
        let (merged_edges, _stats) = crate::graph::unified::compaction::merge::merge_with_csr(
            &snapshot.csr_edges,
            snapshot.delta_edges.clone(),
        );

        // Step 2: Project merged edges into CSR tuples.
        let mut all_edges: Vec<(u32, u32, EdgeKind)> = merged_edges
            .into_iter()
            .map(|edge| (edge.source.index(), edge.target.index(), edge.kind))
            .collect();

        // Step 3: Sort edges by (source, target) for CSR construction and cache locality
        all_edges.par_sort_unstable_by_key(|(src, tgt, _)| (*src, *tgt));

        let edge_count = all_edges.len() as u32;

        // Step 4: Build row_offsets (count edges per source node)
        let mut out_degree = vec![0u32; node_count as usize];
        for &(src, _, _) in &all_edges {
            out_degree[src as usize] += 1;
        }

        // Step 5: Compute row_offsets as prefix sum
        let mut row_offsets = Vec::with_capacity(node_count as usize + 1);
        row_offsets.push(0);
        for &count in &out_degree {
            let last = *row_offsets.last().unwrap();
            row_offsets.push(last + count);
        }

        // Step 6: Fill col_indices and edge_kinds
        let mut col_indices = Vec::with_capacity(edge_count as usize);
        let mut edge_kinds = Vec::with_capacity(edge_count as usize);

        for (_, target, kind) in all_edges {
            col_indices.push(target);
            edge_kinds.push(kind);
        }

        Ok(Self {
            node_count,
            edge_count,
            row_offsets,
            col_indices,
            edge_kinds,
        })
    }

    /// Get neighbors of a node
    #[must_use]
    pub fn neighbors(&self, node: NodeId) -> &[u32] {
        let idx = node.index() as usize;
        if idx >= self.row_offsets.len() - 1 {
            return &[];
        }
        let start = self.row_offsets[idx] as usize;
        let end = self.row_offsets[idx + 1] as usize;
        &self.col_indices[start..end]
    }

    /// Get neighbors filtered by edge kind (matches variant, ignores fields)
    ///
    /// Example: `neighbors_filtered(node, EdgeKind::Calls { argument_count: 0, is_async: false })`
    /// will match ALL Calls edges regardless of `argument_count` or `is_async` values.
    #[must_use]
    pub fn neighbors_filtered(&self, node: NodeId, kind: &EdgeKind) -> Vec<u32> {
        let idx = node.index() as usize;
        if idx >= self.row_offsets.len() - 1 {
            return Vec::new();
        }
        let start = self.row_offsets[idx] as usize;
        let end = self.row_offsets[idx + 1] as usize;

        let kind_discriminant = std::mem::discriminant(kind);
        (start..end)
            .filter(|&i| std::mem::discriminant(&self.edge_kinds[i]) == kind_discriminant)
            .map(|i| self.col_indices[i])
            .collect()
    }

    /// Get edge range for a node (returns (start, end) indices into `col_indices/edge_kinds`)
    ///
    /// Use with `iter_neighbors_filtered` to avoid allocations.
    #[must_use]
    pub fn edge_range(&self, node: NodeId) -> (usize, usize) {
        let idx = node.index() as usize;
        if idx >= self.row_offsets.len() - 1 {
            return (0, 0);
        }
        let start = self.row_offsets[idx] as usize;
        let end = self.row_offsets[idx + 1] as usize;
        (start, end)
    }

    /// Iterate over neighbors filtered by edge kind without allocation
    ///
    /// Returns an iterator over (`edge_index`, `target_node`) pairs.
    pub fn iter_neighbors_filtered(
        &self,
        node: NodeId,
        kind: &EdgeKind,
    ) -> impl Iterator<Item = (usize, u32)> + '_ {
        let (start, end) = self.edge_range(node);
        let kind_discriminant = std::mem::discriminant(kind);

        (start..end)
            .filter(move |&i| std::mem::discriminant(&self.edge_kinds[i]) == kind_discriminant)
            .map(move |i| (i, self.col_indices[i]))
    }
}
