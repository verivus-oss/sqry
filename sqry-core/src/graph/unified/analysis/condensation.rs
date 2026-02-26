//! Condensation DAG with topological ordering and 2-hop interval labels
//!
//! The condensation graph is a DAG where each SCC becomes a single node.
//! Includes 2-hop interval labels for `O(|L_out`| + |`L_in`|) reachability queries.

use super::csr::CsrAdjacency;
use super::scc::SccData;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::NodeId;
use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};

/// Interval for 2-hop labeling
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Interval {
    /// Start position (inclusive)
    pub start: u32,
    /// End position (exclusive)
    pub end: u32,
}

impl Interval {
    /// Create a new interval with the given start and end positions
    #[must_use]
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Check if the interval contains the given value
    #[must_use]
    pub fn contains(&self, value: u32) -> bool {
        value >= self.start && value < self.end
    }

    /// Check if this interval intersects with another interval
    #[must_use]
    pub fn intersects(&self, other: &Interval) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Condensation DAG for one edge kind
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CondensationDag {
    /// Edge kind for this condensation DAG
    pub edge_kind: EdgeKind,
    /// Number of SCCs (nodes in the condensation DAG)
    pub scc_count: u32,
    /// Number of edges in the condensation DAG
    pub edge_count: u32,

    /// CSR row offsets for SCC→SCC edges
    pub row_offsets: Vec<u32>,
    /// CSR column indices for SCC→SCC edges
    pub col_indices: Vec<u32>,

    /// Topological ordering of SCCs
    pub topo_order: Vec<u32>,

    /// Offsets for outgoing 2-hop interval labels
    pub label_out_offsets: Vec<u32>,
    /// Outgoing 2-hop interval label data
    pub label_out_data: Vec<Interval>,
    /// Offsets for incoming 2-hop interval labels
    pub label_in_offsets: Vec<u32>,
    /// Incoming 2-hop interval label data
    pub label_in_data: Vec<Interval>,
}

impl CondensationDag {
    /// Build condensation DAG from SCC data and original adjacency
    ///
    /// Steps:
    ///
    /// 1. Build SCC→SCC adjacency from node adjacency
    /// 2. Remove duplicate edges and self-loops
    /// 3. Compute topological ordering (Kahn's algorithm)
    /// 4. Compute 2-hop interval labels (Task 5)
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    ///
    /// # Panics
    ///
    /// Panics if a node or target is within adjacency bounds but not in SCC data.
    #[allow(clippy::cast_possible_truncation)] // Graph sizes realistically won't exceed u32::MAX
    pub fn build(scc: &SccData, adjacency: &CsrAdjacency) -> Result<Self> {
        let scc_count = scc.scc_count as usize;

        // Step 1: Extract cross-SCC edges
        let scc_edges = extract_cross_scc_edges(scc, adjacency);

        // Step 2: Build CSR for condensation DAG
        let (row_offsets, col_indices) = build_csr_from_edges(&scc_edges, scc_count);
        let edge_count = col_indices.len() as u32;

        // Step 3: Compute topological ordering using Kahn's algorithm
        let topo_order = compute_topological_order(scc_count, &row_offsets, &col_indices)?;

        // Step 4: Compute 2-hop interval labels
        let mut partial_dag = Self {
            edge_kind: scc.edge_kind.clone(),
            scc_count: scc_count as u32,
            edge_count,
            row_offsets,
            col_indices,
            topo_order,
            label_out_offsets: Vec::new(),
            label_out_data: Vec::new(),
            label_in_offsets: Vec::new(),
            label_in_data: Vec::new(),
        };

        // Budget: 5M intervals per edge kind (20M total for 4 kinds)
        // This accommodates large codebases (sqry needs ~2M as of 2026-02)
        let budget = 5_000_000;
        let (label_out_offsets, label_out_data, label_in_offsets, label_in_data) =
            super::reachability::compute_2hop_labels(&partial_dag, budget)?;

        partial_dag.label_out_offsets = label_out_offsets;
        partial_dag.label_out_data = label_out_data;
        partial_dag.label_in_offsets = label_in_offsets;
        partial_dag.label_in_data = label_in_data;

        Ok(partial_dag)
    }

    /// Get successor SCCs
    #[must_use]
    pub fn successors(&self, scc_id: u32) -> &[u32] {
        let scc_idx = scc_id as usize;
        if scc_idx >= self.row_offsets.len() - 1 {
            return &[];
        }
        let start = self.row_offsets[scc_idx] as usize;
        let end = self.row_offsets[scc_idx + 1] as usize;
        &self.col_indices[start..end]
    }

    /// Check reachability using 2-hop interval labels
    ///
    /// Complexity: O(|label_out\[from\]| + |label_in\[to\]|)
    /// Returns true if there's a path from `from_scc` to `to_scc`
    #[must_use]
    pub fn can_reach(&self, from_scc: u32, to_scc: u32) -> bool {
        if from_scc == to_scc {
            return true;
        }

        // Get label_out for from_scc
        let from_idx = from_scc as usize;
        if from_idx >= self.label_out_offsets.len() - 1 {
            return false;
        }
        let out_start = self.label_out_offsets[from_idx] as usize;
        let out_end = self.label_out_offsets[from_idx + 1] as usize;
        let label_out = &self.label_out_data[out_start..out_end];

        // Get label_in for to_scc
        let to_idx = to_scc as usize;
        if to_idx >= self.label_in_offsets.len() - 1 {
            return false;
        }
        let in_start = self.label_in_offsets[to_idx] as usize;
        let in_end = self.label_in_offsets[to_idx + 1] as usize;
        let label_in = &self.label_in_data[in_start..in_end];

        // Check if label_out[from] ∩ label_in[to] ≠ ∅
        // Using interval intersection
        for out_interval in label_out {
            for in_interval in label_in {
                if out_interval.intersects(in_interval) {
                    return true;
                }
            }
        }

        false
    }

    /// Find SCC-level path using 2-hop pruned BFS
    #[must_use]
    pub fn find_scc_path(&self, from_scc: u32, to_scc: u32) -> Option<Vec<u32>> {
        if !self.can_reach(from_scc, to_scc) {
            return None;
        }

        if from_scc == to_scc {
            return Some(vec![from_scc]);
        }

        // BFS with 2-hop pruning
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        let mut parent: HashMap<u32, u32> = HashMap::new();

        queue.push_back(from_scc);
        visited.insert(from_scc);

        while let Some(current) = queue.pop_front() {
            if current == to_scc {
                // Reconstruct path
                let mut path = vec![to_scc];
                let mut node = to_scc;
                while node != from_scc {
                    node = parent[&node];
                    path.push(node);
                }
                path.reverse();
                return Some(path);
            }

            for &neighbor in self.successors(current) {
                if !visited.contains(&neighbor) && self.can_reach(neighbor, to_scc) {
                    visited.insert(neighbor);
                    parent.insert(neighbor, current);
                    queue.push_back(neighbor);
                }
            }
        }

        None
    }
}

/// Extract cross-SCC edges from node-level adjacency.
///
/// Iterates over all nodes and their filtered neighbors, mapping each to its SCC
/// and collecting unique cross-SCC edges (excluding self-loops).
///
/// # Panics
///
/// Panics if a node or target is within adjacency bounds but not in SCC data.
#[allow(clippy::cast_possible_truncation)]
fn extract_cross_scc_edges(scc: &SccData, adjacency: &CsrAdjacency) -> HashSet<(u32, u32)> {
    let mut scc_edges: HashSet<(u32, u32)> = HashSet::new();

    for node in 0..adjacency.node_count {
        let src_scc = scc
            .scc_of(NodeId::new(node, 0))
            .expect("Node within adjacency should have valid SCC");

        let neighbors = adjacency.neighbors_filtered(NodeId::new(node, 0), &scc.edge_kind);

        for &target in &neighbors {
            let tgt_scc = scc
                .scc_of(NodeId::new(target, 0))
                .expect("Target within adjacency should have valid SCC");

            if src_scc != tgt_scc {
                scc_edges.insert((src_scc, tgt_scc));
            }
        }
    }

    scc_edges
}

/// Build CSR arrays from a set of SCC-level edges.
///
/// Sorts successors per SCC for cache locality. Returns `(row_offsets, col_indices)`.
#[allow(clippy::cast_possible_truncation)]
fn build_csr_from_edges(scc_edges: &HashSet<(u32, u32)>, scc_count: usize) -> (Vec<u32>, Vec<u32>) {
    let mut scc_adjacency: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(src, tgt) in scc_edges {
        scc_adjacency.entry(src).or_default().push(tgt);
    }

    for successors in scc_adjacency.values_mut() {
        successors.sort_unstable();
    }

    let mut row_offsets = Vec::with_capacity(scc_count + 1);
    let mut col_indices = Vec::new();
    row_offsets.push(0);

    for scc_id in 0..scc_count as u32 {
        if let Some(successors) = scc_adjacency.get(&scc_id) {
            col_indices.extend_from_slice(successors);
        }
        row_offsets.push(col_indices.len() as u32);
    }

    (row_offsets, col_indices)
}

/// Compute topological ordering of SCCs using Kahn's algorithm.
///
/// # Errors
///
/// Returns an error if the graph contains cycles (topological sort is incomplete).
#[allow(clippy::cast_possible_truncation)]
fn compute_topological_order(
    scc_count: usize,
    row_offsets: &[u32],
    col_indices: &[u32],
) -> Result<Vec<u32>> {
    let mut in_degree = vec![0u32; scc_count];
    for &target in col_indices {
        in_degree[target as usize] += 1;
    }

    let mut queue: VecDeque<u32> = VecDeque::new();
    for (scc_id, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            queue.push_back(scc_id as u32);
        }
    }

    let mut topo_order = Vec::with_capacity(scc_count);
    while let Some(scc_id) = queue.pop_front() {
        topo_order.push(scc_id);

        let start = row_offsets[scc_id as usize] as usize;
        let end = row_offsets[scc_id as usize + 1] as usize;
        for &successor in &col_indices[start..end] {
            in_degree[successor as usize] -= 1;
            if in_degree[successor as usize] == 0 {
                queue.push_back(successor);
            }
        }
    }

    if topo_order.len() != scc_count {
        anyhow::bail!(
            "Topological sort failed: expected {} SCCs, got {}. Graph has cycles!",
            scc_count,
            topo_order.len()
        );
    }

    Ok(topo_order)
}
