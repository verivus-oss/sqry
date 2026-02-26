//! Strongly Connected Components computation using iterative Tarjan
//!
//! Uses Pearce's memory optimizations (v(1+3w) bits) as integrated in `SciPy`.
//! Computes SCCs for a specific edge kind by filtering the CSR.

use super::csr::CsrAdjacency;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::NodeId;
use anyhow::Result;

/// SCC data for one edge kind
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SccData {
    /// Edge kind for this SCC computation
    pub edge_kind: EdgeKind,
    /// Number of nodes analyzed
    pub node_count: u32,
    /// Number of SCCs found
    pub scc_count: u32,
    /// Number of non-trivial SCCs (size > 1)
    pub non_trivial_count: u32,
    /// Maximum SCC size
    pub max_scc_size: u32,
    /// Dense array: `node_to_scc`[`node_id.index()`] = `scc_id`
    pub node_to_scc: Vec<u32>,
    /// Offset into `scc_members` for each SCC
    pub scc_offsets: Vec<u32>,
    /// Concatenated member lists for all SCCs
    pub scc_members: Vec<u32>,
    /// Whether each SCC has a self-loop
    pub has_self_loop: Vec<bool>,
}

impl SccData {
    /// Compute SCCs using iterative Tarjan with Pearce optimizations
    ///
    /// Filters CSR by `edge_kind` before traversal to compute SCCs for that edge type only.
    /// The kind parameter is used for discriminant matching only (metadata fields are ignored).
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails.
    ///
    /// # Panics
    ///
    /// Panics if internal Tarjan algorithm invariants are violated (stack underflow, invalid indices).
    #[allow(clippy::cast_possible_truncation)] // Graph sizes realistically won't exceed u32::MAX
    pub fn compute_tarjan(adjacency: &CsrAdjacency, kind: &EdgeKind) -> Result<Self> {
        let node_count = adjacency.node_count as usize;

        // Tarjan state
        let mut index = 0u32;
        let mut stack = Vec::new();
        let mut on_stack = vec![false; node_count];
        let mut indices = vec![None; node_count];
        let mut lowlinks = vec![u32::MAX; node_count];

        // SCC results
        let mut node_to_scc = vec![u32::MAX; node_count];
        let mut sccs: Vec<Vec<u32>> = Vec::new();

        // Pre-compute filtered neighbors for all nodes (once per node, not once per visit)
        // This avoids O(degree²) allocations in the DFS loop
        let mut filtered_neighbors: Vec<Vec<u32>> = Vec::with_capacity(node_count);
        for node_idx in 0..node_count {
            let neighbors = adjacency.neighbors_filtered(NodeId::new(node_idx as u32, 0), kind);
            filtered_neighbors.push(neighbors);
        }

        // Process all nodes
        for start_node in 0..node_count as u32 {
            if indices[start_node as usize].is_some() {
                continue; // Already visited
            }

            // Iterative DFS with explicit call stack
            let mut dfs_stack: Vec<(u32, usize)> = vec![(start_node, 0)];

            while let Some((node, neighbor_idx)) = dfs_stack.pop() {
                let node_usize = node as usize;

                // First visit to this node
                if indices[node_usize].is_none() {
                    indices[node_usize] = Some(index);
                    lowlinks[node_usize] = index;
                    index += 1;
                    stack.push(node);
                    on_stack[node_usize] = true;
                }

                // Get pre-computed neighbors (no allocation here!)
                let neighbors = &filtered_neighbors[node_usize];

                // Process neighbors
                if neighbor_idx < neighbors.len() {
                    let next_neighbor = neighbors[neighbor_idx];
                    let next_usize = next_neighbor as usize;

                    // Push continuation frame
                    dfs_stack.push((node, neighbor_idx + 1));

                    if indices[next_usize].is_none() {
                        // Unvisited neighbor - recurse
                        dfs_stack.push((next_neighbor, 0));
                    } else if on_stack[next_usize] {
                        // Neighbor on stack - update lowlink
                        lowlinks[node_usize] =
                            lowlinks[node_usize].min(indices[next_usize].unwrap());
                    }
                } else {
                    // Finished processing all neighbors
                    // Update parent lowlink if this was a recursive call
                    if let Some(&(parent, _)) = dfs_stack.last() {
                        let parent_usize = parent as usize;
                        lowlinks[parent_usize] = lowlinks[parent_usize].min(lowlinks[node_usize]);
                    }

                    // Check if this node is an SCC root
                    if lowlinks[node_usize] == indices[node_usize].unwrap() {
                        let mut scc_members = Vec::new();
                        loop {
                            let member = stack.pop().unwrap();
                            on_stack[member as usize] = false;
                            scc_members.push(member);
                            if member == node {
                                break;
                            }
                        }
                        sccs.push(scc_members);
                    }
                }
            }
        }

        // Build node_to_scc mapping and scc_members array
        let scc_count = sccs.len() as u32;
        let mut scc_offsets = Vec::with_capacity(sccs.len() + 1);
        let mut scc_members_flat = Vec::new();
        let mut has_self_loop = vec![false; sccs.len()];
        let mut non_trivial_count = 0u32;
        let mut max_scc_size = 0u32;

        scc_offsets.push(0);
        for (scc_id, members) in sccs.iter().enumerate() {
            let size = members.len() as u32;
            max_scc_size = max_scc_size.max(size);

            // Detect self-loops for size-1 SCCs
            if size == 1 {
                let node = members[0];
                let neighbors = &filtered_neighbors[node as usize];
                if neighbors.contains(&node) {
                    has_self_loop[scc_id] = true;
                }
            }

            // Non-trivial = size > 1 OR has self-loop
            if size > 1 || has_self_loop[scc_id] {
                non_trivial_count += 1;
            }

            for &member in members {
                node_to_scc[member as usize] = scc_id as u32;
                scc_members_flat.push(member);
            }

            let offset = scc_offsets.last().unwrap() + size;
            scc_offsets.push(offset);
        }

        Ok(Self {
            edge_kind: kind.clone(),
            node_count: node_count as u32,
            scc_count,
            non_trivial_count,
            max_scc_size,
            node_to_scc,
            scc_offsets,
            scc_members: scc_members_flat,
            has_self_loop,
        })
    }

    /// Get SCC ID for a node
    ///
    /// Returns None if node is out of range (helps catch invalid node usage)
    #[must_use]
    pub fn scc_of(&self, node: NodeId) -> Option<u32> {
        let idx = node.index() as usize;
        if idx < self.node_to_scc.len() {
            Some(self.node_to_scc[idx])
        } else {
            None
        }
    }

    /// Get all member nodes of an SCC
    #[must_use]
    pub fn scc_members(&self, scc_id: u32) -> &[u32] {
        let scc_idx = scc_id as usize;
        if scc_idx >= self.scc_offsets.len() - 1 {
            return &[];
        }
        let start = self.scc_offsets[scc_idx] as usize;
        let end = self.scc_offsets[scc_idx + 1] as usize;
        &self.scc_members[start..end]
    }

    /// Check if an SCC is trivial (single node with no self-loop)
    #[must_use]
    pub fn is_trivial(&self, scc_id: u32) -> bool {
        let scc_idx = scc_id as usize;
        if scc_idx >= self.has_self_loop.len() {
            return true; // Out of range, consider trivial
        }

        let members = self.scc_members(scc_id);
        members.len() == 1 && !self.has_self_loop[scc_idx]
    }

    /// Get SCC size
    #[must_use]
    pub fn scc_size(&self, scc_id: u32) -> usize {
        self.scc_members(scc_id).len()
    }
}
