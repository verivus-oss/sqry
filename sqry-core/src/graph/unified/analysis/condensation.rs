//! Condensation DAG with topological ordering and 2-hop interval labels
//!
//! The condensation graph is a DAG where each SCC becomes a single node.
//! Includes 2-hop interval labels for `O(|L_out| + |L_in|)` reachability queries.

use super::csr::CsrAdjacency;
use super::scc::SccData;
use crate::graph::unified::edge::EdgeKind;
#[cfg(test)]
use crate::graph::unified::edge::ResolvedVia;
use crate::graph::unified::node::NodeId;
use anyhow::Result;
use std::collections::{HashMap, HashSet, VecDeque};

/// Configuration for the 2-hop label budget.
#[derive(Debug, Clone)]
pub struct LabelBudgetConfig {
    /// Maximum intervals per edge kind. Default: 15,000,000.
    pub budget_per_kind: usize,
    /// Behavior on budget exceeded.
    pub on_exceeded: BudgetExceededPolicy,
    /// Density gate threshold: skip labels when `cond_edges > threshold * scc_count`.
    /// 0 = disabled. Default: 64.
    pub density_gate_threshold: usize,
    /// When true, skip 2-hop label computation entirely and use BFS fallback.
    /// This avoids the most expensive phase of analysis. Default: false.
    pub skip_labels: bool,
}

/// What to do when the label budget is exceeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetExceededPolicy {
    /// Fail the build (current behavior).
    Fail,
    /// Skip labels for this edge kind, degrade reachability to BFS fallback.
    Degrade,
}

/// Strategy used for reachability queries on a condensation DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReachabilityStrategy {
    /// 2-hop interval labels available for O(1) reachability
    IntervalLabels,
    /// Labels absent/degraded — use BFS on condensation DAG
    DagBfs,
}

impl Default for LabelBudgetConfig {
    fn default() -> Self {
        Self {
            budget_per_kind: 15_000_000,
            on_exceeded: BudgetExceededPolicy::Degrade,
            density_gate_threshold: 64,
            skip_labels: false,
        }
    }
}

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

    /// Strategy for reachability queries (interval labels or BFS fallback).
    ///
    /// Note: Adding this field changes the postcard binary layout. Old `.dag` files
    /// (persisted before this field existed) will fail to deserialize, triggering a
    /// rebuild via the manifest-hash staleness check in `sqry analyze`.
    pub strategy: ReachabilityStrategy,
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
        Self::build_with_budget(scc, adjacency, &LabelBudgetConfig::default())
    }

    /// Build condensation DAG with configurable label budget.
    ///
    /// When the budget is exceeded and the policy is [`BudgetExceededPolicy::Degrade`],
    /// the DAG is returned without labels — reachability queries fall back to BFS.
    ///
    /// # Errors
    ///
    /// Returns an error if building fails, or if the budget is exceeded
    /// and the policy is [`BudgetExceededPolicy::Fail`].
    #[allow(clippy::cast_possible_truncation)] // Graph sizes realistically won't exceed u32::MAX
    pub fn build_with_budget(
        scc: &SccData,
        adjacency: &CsrAdjacency,
        budget_config: &LabelBudgetConfig,
    ) -> Result<Self> {
        let scc_count = scc.scc_count as usize;

        // Step 1: Extract cross-SCC edges
        let scc_edges = extract_cross_scc_edges(scc, adjacency);

        // Step 2: Build CSR for condensation DAG
        let (row_offsets, col_indices) = build_csr_from_edges(&scc_edges, scc_count);
        let edge_count = col_indices.len() as u32;

        // Step 3: Compute topological ordering using Kahn's algorithm
        let topo_order = compute_topological_order(scc_count, &row_offsets, &col_indices)?;

        // Density gate: skip label computation when condensation DAG is too dense.
        // This prevents the expensive 2-hop label build on graphs where the
        // condensation has high edge-to-node ratio (many cross-SCC edges).
        let cond_edge_count = col_indices.len();
        // Use checked_mul to prevent overflow with large threshold values.
        // If multiplication overflows, the product exceeds usize::MAX, so
        // cond_edge_count can never exceed it — density gate is not triggered.
        let density_gated = budget_config.density_gate_threshold > 0
            && scc_count > 0
            && match budget_config.density_gate_threshold.checked_mul(scc_count) {
                Some(product) => cond_edge_count > product,
                None => false, // overflow means threshold*scc_count > usize::MAX > cond_edge_count
            };

        // Each SCC contributes at least one outgoing and one incoming base
        // interval. If that floor already exceeds the configured budget, the
        // label build cannot succeed and should not spend minutes discovering
        // that during wavefront expansion.
        let budget = if budget_config.budget_per_kind == 0 {
            usize::MAX
        } else {
            budget_config.budget_per_kind
        };
        let minimum_label_intervals = minimum_label_interval_count(scc_count);
        let minimum_budget_exceeded = budget_config.budget_per_kind != 0
            && minimum_label_intervals > budget_config.budget_per_kind;

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
            strategy: ReachabilityStrategy::DagBfs,
        };

        if budget_config.skip_labels {
            log::info!(
                "Labels skipped for {:?}: skip_labels requested, using BFS fallback",
                scc.edge_kind,
            );
            // strategy stays DagBfs
        } else if minimum_budget_exceeded {
            let message = format!(
                "2-hop label budget cannot fit {:?}: minimum {} intervals for {} SCCs > {} budget",
                scc.edge_kind, minimum_label_intervals, scc_count, budget_config.budget_per_kind
            );
            match budget_config.on_exceeded {
                BudgetExceededPolicy::Fail => anyhow::bail!("{message}"),
                BudgetExceededPolicy::Degrade => {
                    log::warn!("{message}; degrading to BFS fallback");
                }
            }
            // strategy stays DagBfs
        } else if density_gated {
            log::info!(
                "Density gate triggered for {:?}: {} cond edges > {} * {} SCCs, skipping labels",
                scc.edge_kind,
                cond_edge_count,
                budget_config.density_gate_threshold,
                scc_count
            );
            // strategy stays DagBfs
        } else {
            match super::reachability::compute_2hop_labels(&partial_dag, budget) {
                Ok((label_out_offsets, label_out_data, label_in_offsets, label_in_data)) => {
                    partial_dag.label_out_offsets = label_out_offsets;
                    partial_dag.label_out_data = label_out_data;
                    partial_dag.label_in_offsets = label_in_offsets;
                    partial_dag.label_in_data = label_in_data;
                    partial_dag.strategy = ReachabilityStrategy::IntervalLabels;
                }
                Err(e) => match budget_config.on_exceeded {
                    BudgetExceededPolicy::Fail => return Err(e),
                    BudgetExceededPolicy::Degrade => {
                        log::warn!(
                            "Label budget exceeded for {:?}, degrading to BFS fallback: {e}",
                            scc.edge_kind
                        );
                        // strategy stays DagBfs
                    }
                },
            }
        }

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

    /// Fix up strategy after deserialization.
    ///
    /// Detects mismatched strategy: if `strategy` is `IntervalLabels` but label data
    /// is empty, corrects to `DagBfs`. This guards against data corruption or
    /// partial writes where the strategy field doesn't match the actual label state.
    pub fn fixup_after_load(&mut self) {
        if self.strategy == ReachabilityStrategy::IntervalLabels
            && self.label_out_data.is_empty()
            && self.label_in_data.is_empty()
        {
            self.strategy = ReachabilityStrategy::DagBfs;
        }
    }

    /// Check reachability between two SCCs.
    ///
    /// When interval labels are available (`IntervalLabels` strategy), uses
    /// `O(|L_out| + |L_in|)`
    /// interval intersection. Otherwise, falls back to BFS on the condensation DAG.
    #[must_use]
    pub fn can_reach(&self, from_scc: u32, to_scc: u32) -> bool {
        if from_scc == to_scc {
            return true;
        }

        match self.strategy {
            ReachabilityStrategy::IntervalLabels => self.can_reach_via_labels(from_scc, to_scc),
            ReachabilityStrategy::DagBfs => self.can_reach_via_bfs(from_scc, to_scc),
        }
    }

    /// Check reachability using 2-hop interval labels.
    ///
    /// Complexity: `O(|label_out[from]| + |label_in[to]|)`.
    fn can_reach_via_labels(&self, from_scc: u32, to_scc: u32) -> bool {
        let from_idx = from_scc as usize;
        if from_idx >= self.label_out_offsets.len().saturating_sub(1) {
            return false;
        }
        let out_start = self.label_out_offsets[from_idx] as usize;
        let out_end = self.label_out_offsets[from_idx + 1] as usize;
        let label_out = &self.label_out_data[out_start..out_end];

        let to_idx = to_scc as usize;
        if to_idx >= self.label_in_offsets.len().saturating_sub(1) {
            return false;
        }
        let in_start = self.label_in_offsets[to_idx] as usize;
        let in_end = self.label_in_offsets[to_idx + 1] as usize;
        let label_in = &self.label_in_data[in_start..in_end];

        for out_interval in label_out {
            for in_interval in label_in {
                if out_interval.intersects(in_interval) {
                    return true;
                }
            }
        }

        false
    }

    /// Check reachability via BFS on condensation DAG edges.
    ///
    /// Used as fallback when 2-hop labels are absent (budget exceeded / density gated).
    fn can_reach_via_bfs(&self, from_scc: u32, to_scc: u32) -> bool {
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(from_scc);
        visited.insert(from_scc);

        while let Some(current) = queue.pop_front() {
            for &neighbor in self.successors(current) {
                if neighbor == to_scc {
                    return true;
                }
                if visited.insert(neighbor) {
                    queue.push_back(neighbor);
                }
            }
        }

        false
    }

    /// Find SCC-level path using BFS.
    ///
    /// When interval labels are available, uses 2-hop pruning to skip
    /// neighbors that cannot reach the target. Otherwise, uses plain BFS.
    #[must_use]
    pub fn find_scc_path(&self, from_scc: u32, to_scc: u32) -> Option<Vec<u32>> {
        if from_scc == to_scc {
            return Some(vec![from_scc]);
        }

        // For IntervalLabels, do a quick reachability check first
        if self.strategy == ReachabilityStrategy::IntervalLabels
            && !self.can_reach(from_scc, to_scc)
        {
            return None;
        }

        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        let mut parent: HashMap<u32, u32> = HashMap::new();

        queue.push_back(from_scc);
        visited.insert(from_scc);

        while let Some(current) = queue.pop_front() {
            if current == to_scc {
                return Some(reconstruct_scc_path(&parent, from_scc, to_scc));
            }

            for &neighbor in self.successors(current) {
                if visited.contains(&neighbor) || !self.should_explore_neighbor(neighbor, to_scc) {
                    continue;
                }
                visited.insert(neighbor);
                parent.insert(neighbor, current);
                queue.push_back(neighbor);
            }
        }

        None
    }

    fn should_explore_neighbor(&self, neighbor: u32, to_scc: u32) -> bool {
        match self.strategy {
            ReachabilityStrategy::IntervalLabels => self.can_reach_via_labels(neighbor, to_scc),
            ReachabilityStrategy::DagBfs => true,
        }
    }
}

/// Reconstruct a BFS-discovered path from `from_scc` to `to_scc`.
///
/// # Panics
///
/// Panics if `to_scc` was not reached from `from_scc` in the BFS that produced
/// `parent`, because the reverse walk expects a complete predecessor chain.
fn reconstruct_scc_path(parent: &HashMap<u32, u32>, from_scc: u32, to_scc: u32) -> Vec<u32> {
    let mut path = vec![to_scc];
    let mut node = to_scc;
    while node != from_scc {
        node = parent[&node];
        path.push(node);
    }
    path.reverse();
    path
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

#[must_use]
fn minimum_label_interval_count(scc_count: usize) -> usize {
    scc_count.saturating_mul(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::analysis::csr::CsrAdjacency;
    use crate::graph::unified::analysis::scc::SccData;
    use crate::graph::unified::compaction::{CompactionSnapshot, MergedEdge};
    use crate::graph::unified::edge::EdgeKind;
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeId;

    fn test_snapshot() -> CompactionSnapshot {
        let file = FileId::new(0);
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        CompactionSnapshot {
            csr_edges: vec![
                MergedEdge::new(NodeId::new(0, 0), NodeId::new(1, 0), kind.clone(), 1, file),
                MergedEdge::new(NodeId::new(1, 0), NodeId::new(2, 0), kind.clone(), 2, file),
                MergedEdge::new(NodeId::new(2, 0), NodeId::new(3, 0), kind.clone(), 3, file),
            ],
            delta_edges: Vec::new(),
            node_count: 4,
            csr_version: 0,
        }
    }

    fn isolated_snapshot(node_count: usize) -> CompactionSnapshot {
        CompactionSnapshot {
            csr_edges: Vec::new(),
            delta_edges: Vec::new(),
            node_count,
            csr_version: 0,
        }
    }

    #[test]
    fn test_label_budget_default() {
        let config = LabelBudgetConfig::default();
        assert_eq!(config.budget_per_kind, 15_000_000);
        assert!(matches!(config.on_exceeded, BudgetExceededPolicy::Degrade));
    }

    #[test]
    fn test_label_budget_fail_policy() {
        let config = LabelBudgetConfig {
            budget_per_kind: 1,
            on_exceeded: BudgetExceededPolicy::Fail,
            ..LabelBudgetConfig::default()
        };

        let snapshot = test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let result = CondensationDag::build_with_budget(&scc, &csr, &config);
        assert!(result.is_err(), "Should fail with budget=1 and Fail policy");
    }

    #[test]
    fn test_label_budget_degrade_policy() {
        let config = LabelBudgetConfig {
            budget_per_kind: 1,
            on_exceeded: BudgetExceededPolicy::Degrade,
            ..LabelBudgetConfig::default()
        };

        let snapshot = test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let result = CondensationDag::build_with_budget(&scc, &csr, &config);
        assert!(
            result.is_ok(),
            "Should degrade gracefully with Degrade policy"
        );
        let dag = result.unwrap();
        assert!(dag.label_out_data.is_empty());
        assert!(dag.label_in_data.is_empty());
        assert_eq!(dag.strategy, ReachabilityStrategy::DagBfs);
    }

    #[test]
    fn test_minimum_label_budget_gate_degrades_before_label_build() {
        let config = LabelBudgetConfig {
            budget_per_kind: 7,
            on_exceeded: BudgetExceededPolicy::Degrade,
            density_gate_threshold: 0,
            skip_labels: false,
        };

        let snapshot = isolated_snapshot(4);
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build_with_budget(&scc, &csr, &config).unwrap();

        assert_eq!(dag.strategy, ReachabilityStrategy::DagBfs);
        assert!(dag.label_out_data.is_empty());
        assert!(dag.label_in_data.is_empty());
        assert!(dag.can_reach(0, 0));
        assert!(!dag.can_reach(0, 1));
    }

    #[test]
    fn test_minimum_label_budget_gate_fails_when_policy_is_fail() {
        let config = LabelBudgetConfig {
            budget_per_kind: 7,
            on_exceeded: BudgetExceededPolicy::Fail,
            density_gate_threshold: 0,
            skip_labels: false,
        };

        let snapshot = isolated_snapshot(4);
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let error = CondensationDag::build_with_budget(&scc, &csr, &config)
            .expect_err("minimum label budget gate should fail");

        assert!(
            error.to_string().contains("minimum 8 intervals"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_minimum_label_budget_gate_allows_exact_floor() {
        let config = LabelBudgetConfig {
            budget_per_kind: 8,
            on_exceeded: BudgetExceededPolicy::Fail,
            density_gate_threshold: 0,
            skip_labels: false,
        };

        let snapshot = isolated_snapshot(4);
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build_with_budget(&scc, &csr, &config).unwrap();

        assert_eq!(dag.strategy, ReachabilityStrategy::IntervalLabels);
        assert_eq!(dag.label_out_data.len() + dag.label_in_data.len(), 8);
    }

    #[test]
    fn test_dag_bfs_can_reach() {
        // Build a DAG with DagBfs strategy and verify reachability works
        let config = LabelBudgetConfig {
            budget_per_kind: 1,
            on_exceeded: BudgetExceededPolicy::Degrade,
            ..LabelBudgetConfig::default()
        };

        let snapshot = test_snapshot(); // 0→1→2→3 chain
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build_with_budget(&scc, &csr, &config).unwrap();

        assert_eq!(dag.strategy, ReachabilityStrategy::DagBfs);

        // Self-reachability
        assert!(dag.can_reach(0, 0));

        // Forward reachability (0→1→2→3 in SCC space)
        let scc_0 = scc.scc_of(NodeId::new(0, 0)).unwrap();
        let scc_3 = scc.scc_of(NodeId::new(3, 0)).unwrap();
        assert!(dag.can_reach(scc_0, scc_3));

        // Reverse reachability should be false
        assert!(!dag.can_reach(scc_3, scc_0));
    }

    #[test]
    fn test_dag_bfs_find_scc_path() {
        let config = LabelBudgetConfig {
            budget_per_kind: 1,
            on_exceeded: BudgetExceededPolicy::Degrade,
            ..LabelBudgetConfig::default()
        };

        let snapshot = test_snapshot(); // 0→1→2→3 chain
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build_with_budget(&scc, &csr, &config).unwrap();

        assert_eq!(dag.strategy, ReachabilityStrategy::DagBfs);

        let scc_0 = scc.scc_of(NodeId::new(0, 0)).unwrap();
        let scc_3 = scc.scc_of(NodeId::new(3, 0)).unwrap();

        // Forward path should exist
        let path = dag.find_scc_path(scc_0, scc_3);
        assert!(path.is_some());
        let path = path.unwrap();
        assert_eq!(*path.first().unwrap(), scc_0);
        assert_eq!(*path.last().unwrap(), scc_3);

        // Reverse path should not exist
        assert!(dag.find_scc_path(scc_3, scc_0).is_none());
    }

    #[test]
    fn test_interval_labels_and_bfs_produce_same_results() {
        let snapshot = test_snapshot(); // 0→1→2→3 chain
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();

        // Build with labels
        let dag_labels = CondensationDag::build(&scc, &csr).unwrap();
        assert_eq!(dag_labels.strategy, ReachabilityStrategy::IntervalLabels);

        // Build without labels (degraded)
        let config = LabelBudgetConfig {
            budget_per_kind: 1,
            on_exceeded: BudgetExceededPolicy::Degrade,
            ..LabelBudgetConfig::default()
        };
        let dag_bfs = CondensationDag::build_with_budget(&scc, &csr, &config).unwrap();
        assert_eq!(dag_bfs.strategy, ReachabilityStrategy::DagBfs);

        // Both should produce the same reachability answers
        for from in 0..dag_labels.scc_count {
            for to in 0..dag_labels.scc_count {
                assert_eq!(
                    dag_labels.can_reach(from, to),
                    dag_bfs.can_reach(from, to),
                    "Mismatch for can_reach({from}, {to})"
                );
            }
        }
    }

    #[test]
    fn test_fixup_after_load_corrects_empty_labels() {
        // Simulate a DAG where strategy is IntervalLabels but labels are
        // actually empty (e.g. partial write or data corruption).
        let mut dag = CondensationDag {
            edge_kind: EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            scc_count: 2,
            edge_count: 1,
            row_offsets: vec![0, 1, 1],
            col_indices: vec![1],
            topo_order: vec![0, 1],
            label_out_offsets: Vec::new(),
            label_out_data: Vec::new(),
            label_in_offsets: Vec::new(),
            label_in_data: Vec::new(),
            strategy: ReachabilityStrategy::IntervalLabels, // intentionally wrong — triggers fixup
        };

        // Before fixup: strategy claims IntervalLabels but labels are empty
        assert_eq!(dag.strategy, ReachabilityStrategy::IntervalLabels);

        dag.fixup_after_load();

        // After fixup: strategy corrected to DagBfs
        assert_eq!(dag.strategy, ReachabilityStrategy::DagBfs);
        // Reachability should now work via BFS
        assert!(dag.can_reach(0, 1));
        assert!(!dag.can_reach(1, 0));
    }

    #[test]
    fn test_fixup_after_load_preserves_valid_labels() {
        // Build a DAG with labels and verify fixup doesn't change strategy
        let snapshot = test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let mut dag = CondensationDag::build(&scc, &csr).unwrap();

        assert_eq!(dag.strategy, ReachabilityStrategy::IntervalLabels);
        assert!(!dag.label_out_data.is_empty());

        dag.fixup_after_load();

        // Should remain IntervalLabels since labels are non-empty
        assert_eq!(dag.strategy, ReachabilityStrategy::IntervalLabels);
    }

    /// Dense snapshot: complete DAG on 4 nodes (0→1, 0→2, 0→3, 1→2, 1→3, 2→3).
    /// 6 cross-SCC edges, 4 SCCs → density = 1.5 edges/SCC.
    fn dense_snapshot() -> CompactionSnapshot {
        let file = FileId::new(0);
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        CompactionSnapshot {
            csr_edges: vec![
                MergedEdge::new(NodeId::new(0, 0), NodeId::new(1, 0), kind.clone(), 1, file),
                MergedEdge::new(NodeId::new(0, 0), NodeId::new(2, 0), kind.clone(), 2, file),
                MergedEdge::new(NodeId::new(0, 0), NodeId::new(3, 0), kind.clone(), 3, file),
                MergedEdge::new(NodeId::new(1, 0), NodeId::new(2, 0), kind.clone(), 4, file),
                MergedEdge::new(NodeId::new(1, 0), NodeId::new(3, 0), kind.clone(), 5, file),
                MergedEdge::new(NodeId::new(2, 0), NodeId::new(3, 0), kind.clone(), 6, file),
            ],
            delta_edges: Vec::new(),
            node_count: 4,
            csr_version: 0,
        }
    }

    #[test]
    fn test_density_gate_skips_labels() {
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };

        // Dense graph: 6 cross-SCC edges, 4 SCCs. With threshold=1: 6 > 1*4 = true → gated.
        let snapshot = dense_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();

        let config_gated = LabelBudgetConfig {
            budget_per_kind: 15_000_000,
            on_exceeded: BudgetExceededPolicy::Degrade,
            density_gate_threshold: 1, // 6 > 1*4 → gated
            skip_labels: false,
        };
        let dag = CondensationDag::build_with_budget(&scc, &csr, &config_gated).unwrap();
        assert_eq!(dag.strategy, ReachabilityStrategy::DagBfs);
        assert!(dag.label_out_data.is_empty());
        assert!(dag.label_in_data.is_empty());
        // Reachability should still work via BFS
        let scc_0 = scc.scc_of(NodeId::new(0, 0)).unwrap();
        let scc_3 = scc.scc_of(NodeId::new(3, 0)).unwrap();
        assert!(dag.can_reach(scc_0, scc_3));
        assert!(!dag.can_reach(scc_3, scc_0));

        // With threshold=0 (disabled) — labels should be built
        let config_disabled = LabelBudgetConfig {
            density_gate_threshold: 0,
            ..config_gated.clone()
        };
        let dag2 = CondensationDag::build_with_budget(&scc, &csr, &config_disabled).unwrap();
        assert_eq!(dag2.strategy, ReachabilityStrategy::IntervalLabels);
        assert!(!dag2.label_out_data.is_empty());
    }

    #[test]
    fn test_budget_zero_means_unlimited() {
        let config = LabelBudgetConfig {
            budget_per_kind: 0, // 0 = unlimited
            on_exceeded: BudgetExceededPolicy::Fail,
            density_gate_threshold: 0,
            skip_labels: false,
        };

        let snapshot = test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build_with_budget(&scc, &csr, &config).unwrap();

        // Should succeed with labels (budget=0 means unlimited, not zero)
        assert_eq!(dag.strategy, ReachabilityStrategy::IntervalLabels);
        assert!(!dag.label_out_data.is_empty());
    }
}
