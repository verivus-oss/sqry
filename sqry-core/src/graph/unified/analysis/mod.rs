//! Graph analysis precomputation (Pass 5)
//!
//! This module provides precomputed graph analyses to eliminate query-time O(V+E) costs:
//! - CSR adjacency for fast neighbor iteration
//! - SCC (Strongly Connected Components) for cycle detection
//! - Condensation DAG for dependency analysis
//! - 2-hop interval labels for fast reachability queries
//!
//! All analyses are persisted to disk and memory-mapped for fast loading.

pub mod cache;
pub mod condensation;
pub mod csr;
pub mod persistence;
pub mod reachability;
pub mod scc;

pub use cache::AnalysisCache;
pub use condensation::{
    BudgetExceededPolicy, CondensationDag, Interval, LabelBudgetConfig, ReachabilityStrategy,
};
pub use csr::{CsrAdjacency, EdgeKindDiscriminant};
pub use persistence::{
    AnalysisIdentity, compute_manifest_hash, compute_node_id_hash, load_condensation,
    load_condensation_checked, load_csr, load_csr_checked, load_scc, load_scc_checked,
    persist_condensation, persist_csr, persist_scc, try_load_path_analysis, try_load_scc,
    try_load_scc_and_condensation,
};
pub use scc::SccData;

use crate::graph::unified::compaction::CompactionSnapshot;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::NodeId;
use anyhow::Result;
use rayon::prelude::*;
use std::collections::VecDeque;

/// Complete set of graph analyses for all edge kinds
#[derive(Debug)]
pub struct GraphAnalyses {
    /// CSR adjacency matrix for all edge kinds
    pub adjacency: CsrAdjacency,
    /// Strongly connected components for call edges
    pub scc_calls: SccData,
    /// Strongly connected components for import edges
    pub scc_imports: SccData,
    /// Strongly connected components for reference edges
    pub scc_references: SccData,
    /// Strongly connected components for inheritance edges
    pub scc_inherits: SccData,
    /// Condensation DAG for call edges
    pub cond_calls: CondensationDag,
    /// Condensation DAG for import edges
    pub cond_imports: CondensationDag,
    /// Condensation DAG for reference edges
    pub cond_references: CondensationDag,
    /// Condensation DAG for inheritance edges
    pub cond_inherits: CondensationDag,
}

impl GraphAnalyses {
    /// Build all analyses from a compacted graph snapshot
    /// Returns an error if the operation fails.
    ///
    /// # Errors
    ///
    pub fn build_all(snapshot: &CompactionSnapshot) -> Result<Self> {
        Self::build_all_with_budget(snapshot, &LabelBudgetConfig::default())
    }

    /// Build all analyses from a compacted graph snapshot with configurable label budget.
    ///
    /// # Errors
    ///
    /// Returns an error if analysis building fails.
    pub fn build_all_with_budget(
        snapshot: &CompactionSnapshot,
        label_budget: &LabelBudgetConfig,
    ) -> Result<Self> {
        // Step 1: Build unified CSR adjacency
        let adjacency = CsrAdjacency::build_from_snapshot(snapshot)?;

        // Step 2+3: Per-kind pipeline — SCC → condensation DAG + 2-hop labels.
        // Each edge kind flows independently with no cross-kind barriers.
        let edge_kinds = [
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
            EdgeKind::References,
            EdgeKind::Inherits,
        ];

        let results: Vec<(SccData, CondensationDag)> = edge_kinds
            .into_par_iter()
            .map(|kind| {
                let scc = SccData::compute_tarjan(&adjacency, &kind)?;
                let cond = CondensationDag::build_with_budget(&scc, &adjacency, label_budget)?;
                Ok((scc, cond))
            })
            .collect::<Result<Vec<_>>>()?;

        // Unpack in declaration order: calls, imports, references, inherits
        let mut results = results.into_iter();
        let (scc_calls, cond_calls) = results.next().expect("4 edge kinds");
        let (scc_imports, cond_imports) = results.next().expect("4 edge kinds");
        let (scc_references, cond_references) = results.next().expect("4 edge kinds");
        let (scc_inherits, cond_inherits) = results.next().expect("4 edge kinds");

        Ok(Self {
            adjacency,
            scc_calls,
            scc_imports,
            scc_references,
            scc_inherits,
            cond_calls,
            cond_imports,
            cond_references,
            cond_inherits,
        })
    }

    /// Persist all analyses to disk using paths from [`GraphStorage`](crate::graph::unified::persistence::GraphStorage).
    ///
    /// Creates the analysis directory if it doesn't exist and writes all
    /// artifacts (CSR, SCC, condensation DAGs) with identity validation headers.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation or file writing fails.
    pub fn persist_all(
        &self,
        storage: &crate::graph::unified::persistence::GraphStorage,
        identity: &AnalysisIdentity,
    ) -> Result<()> {
        std::fs::create_dir_all(storage.analysis_dir())?;

        persist_csr(&self.adjacency, identity, &storage.analysis_csr_path())?;
        persist_scc(
            &self.scc_calls,
            identity,
            &storage.analysis_scc_path("calls"),
        )?;
        persist_scc(
            &self.scc_imports,
            identity,
            &storage.analysis_scc_path("imports"),
        )?;
        persist_scc(
            &self.scc_references,
            identity,
            &storage.analysis_scc_path("references"),
        )?;
        persist_scc(
            &self.scc_inherits,
            identity,
            &storage.analysis_scc_path("inherits"),
        )?;
        persist_condensation(
            &self.cond_calls,
            identity,
            &storage.analysis_cond_path("calls"),
        )?;
        persist_condensation(
            &self.cond_imports,
            identity,
            &storage.analysis_cond_path("imports"),
        )?;
        persist_condensation(
            &self.cond_references,
            identity,
            &storage.analysis_cond_path("references"),
        )?;
        persist_condensation(
            &self.cond_inherits,
            identity,
            &storage.analysis_cond_path("inherits"),
        )?;

        Ok(())
    }
}

/// Helper to reconstruct paths using precomputed analyses
pub struct PathReconstructor<'a> {
    csr: &'a CsrAdjacency,
    scc_data: &'a SccData,
    cond_dag: &'a CondensationDag,
}

impl<'a> PathReconstructor<'a> {
    /// Create a new path reconstructor for a specific edge kind's analyses.
    #[must_use]
    pub fn new(
        csr: &'a CsrAdjacency,
        scc_data: &'a SccData,
        cond_dag: &'a CondensationDag,
    ) -> Self {
        Self {
            csr,
            scc_data,
            cond_dag,
        }
    }

    /// Reconstruct node-level path from one node to another
    /// Returns an error if the operation fails.
    ///
    /// # Errors
    ///
    pub fn reconstruct_path(&self, from: NodeId, to: NodeId) -> Result<Option<Vec<NodeId>>> {
        let from_idx = from.index();
        let to_idx = to.index();
        if from_idx >= self.csr.node_count || to_idx >= self.csr.node_count {
            return Ok(None);
        }

        let from_scc = self
            .scc_data
            .scc_of(from)
            .ok_or_else(|| anyhow::anyhow!("Invalid from node ID: {from:?}"))?;
        let to_scc = self
            .scc_data
            .scc_of(to)
            .ok_or_else(|| anyhow::anyhow!("Invalid to node ID: {to:?}"))?;

        if !self.cond_dag.can_reach(from_scc, to_scc) {
            return Ok(None);
        }

        if from_scc == to_scc {
            return Ok(self
                .reconstruct_intra_scc_path(from_idx, to_idx, from_scc)
                .map(|path| path.into_iter().map(|idx| NodeId::new(idx, 0)).collect()));
        }

        let Some(scc_path) = self.reconstruct_scc_path(from_scc, to_scc) else {
            return Ok(None);
        };

        let Some(node_path) = self.expand_scc_path_to_nodes(&scc_path, from_idx, to_idx, to_scc)
        else {
            return Ok(None);
        };

        Ok(Some(
            node_path
                .into_iter()
                .map(|idx| NodeId::new(idx, 0))
                .collect(),
        ))
    }

    fn reconstruct_intra_scc_path(&self, from: u32, to: u32, scc_id: u32) -> Option<Vec<u32>> {
        if from == to {
            return Some(vec![from]);
        }

        let node_count = self.csr.node_count as usize;
        let mut parents = vec![None; node_count];
        let mut queue = VecDeque::new();
        parents[from as usize] = Some(from);
        queue.push_back(from);

        while let Some(current) = queue.pop_front() {
            for neighbor in self
                .csr
                .neighbors_filtered(NodeId::new(current, 0), &self.scc_data.edge_kind)
            {
                let neighbor_scc = self
                    .scc_data
                    .scc_of(NodeId::new(neighbor, 0))
                    .unwrap_or(u32::MAX);
                if neighbor_scc != scc_id {
                    continue;
                }
                if parents[neighbor as usize].is_some() {
                    continue;
                }

                parents[neighbor as usize] = Some(current);
                if neighbor == to {
                    return reconstruct_path_from_parents(&parents, from, to);
                }
                queue.push_back(neighbor);
            }
        }

        None
    }

    fn reconstruct_scc_path(&self, from_scc: u32, to_scc: u32) -> Option<Vec<u32>> {
        if from_scc == to_scc {
            return Some(vec![from_scc]);
        }

        let scc_count = self.cond_dag.scc_count as usize;
        let mut parents = vec![None; scc_count];
        let mut queue = VecDeque::new();
        parents[from_scc as usize] = Some(from_scc);
        queue.push_back(from_scc);

        while let Some(current) = queue.pop_front() {
            for &successor in self.cond_dag.successors(current) {
                if parents[successor as usize].is_some() {
                    continue;
                }
                parents[successor as usize] = Some(current);
                if successor == to_scc {
                    break;
                }
                queue.push_back(successor);
            }
        }

        reconstruct_path_from_parents(&parents, from_scc, to_scc)
    }

    fn expand_scc_path_to_nodes(
        &self,
        scc_path: &[u32],
        from: u32,
        to: u32,
        to_scc: u32,
    ) -> Option<Vec<u32>> {
        if scc_path.is_empty() {
            return None;
        }

        let mut full_path: Vec<u32> = Vec::new();
        let mut current_node = from;

        for window in scc_path.windows(2) {
            let current_scc = window[0];
            let next_scc = window[1];

            let (segment, entry_node) = self.find_exit_path(current_node, current_scc, next_scc)?;

            if full_path.is_empty() {
                full_path.extend(segment);
            } else {
                full_path.extend(segment.into_iter().skip(1));
            }

            full_path.push(entry_node);
            current_node = entry_node;
        }

        let tail = self.reconstruct_intra_scc_path(current_node, to, to_scc)?;
        if full_path.is_empty() {
            full_path = tail;
        } else {
            full_path.extend(tail.into_iter().skip(1));
        }

        Some(full_path)
    }

    fn find_exit_path(
        &self,
        start: u32,
        current_scc: u32,
        next_scc: u32,
    ) -> Option<(Vec<u32>, u32)> {
        use std::collections::VecDeque;
        let node_count = self.csr.node_count as usize;
        let mut parents = vec![None; node_count];
        let mut queue = VecDeque::new();
        parents[start as usize] = Some(start);
        queue.push_back(start);

        while let Some(current) = queue.pop_front() {
            for neighbor in self
                .csr
                .neighbors_filtered(NodeId::new(current, 0), &self.scc_data.edge_kind)
            {
                let neighbor_scc = self
                    .scc_data
                    .scc_of(NodeId::new(neighbor, 0))
                    .unwrap_or(u32::MAX);
                if neighbor_scc == next_scc {
                    let segment = reconstruct_path_from_parents(&parents, start, current)?;
                    return Some((segment, neighbor));
                }
                if neighbor_scc != current_scc {
                    continue;
                }
                if parents[neighbor as usize].is_some() {
                    continue;
                }
                parents[neighbor as usize] = Some(current);
                queue.push_back(neighbor);
            }
        }

        None
    }
}

fn reconstruct_path_from_parents(
    parents: &[Option<u32>],
    start: u32,
    goal: u32,
) -> Option<Vec<u32>> {
    if parents.get(goal as usize)?.is_none() {
        return None;
    }

    let mut path = vec![goal];
    let mut current = goal;
    while current != start {
        let parent = parents[current as usize]?; // Safe: if None, no path.
        current = parent;
        path.push(current);
    }
    path.reverse();
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::compaction::{CompactionSnapshot, MergedEdge};
    use crate::graph::unified::edge::{DeltaEdge, DeltaOp, EdgeKind};
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeId;

    /// Create a simple test graph with known structure
    ///
    /// Graph: 0 -> 1 -> 2 -> 3
    ///            \-> 4 -/
    ///        5 -> 6 (separate component)
    fn create_test_snapshot() -> CompactionSnapshot {
        let file = FileId::new(0);
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };

        let edges = vec![
            // Main component
            MergedEdge::new(NodeId::new(0, 0), NodeId::new(1, 0), kind.clone(), 1, file),
            MergedEdge::new(NodeId::new(1, 0), NodeId::new(2, 0), kind.clone(), 2, file),
            MergedEdge::new(NodeId::new(2, 0), NodeId::new(3, 0), kind.clone(), 3, file),
            MergedEdge::new(NodeId::new(1, 0), NodeId::new(4, 0), kind.clone(), 4, file),
            MergedEdge::new(NodeId::new(4, 0), NodeId::new(3, 0), kind.clone(), 5, file),
            // Separate component
            MergedEdge::new(NodeId::new(5, 0), NodeId::new(6, 0), kind.clone(), 6, file),
        ];

        CompactionSnapshot {
            csr_edges: edges,
            delta_edges: Vec::new(),
            node_count: 7,
            csr_version: 0,
        }
    }

    #[test]
    fn test_csr_construction() {
        let snapshot = create_test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();

        assert_eq!(csr.node_count, 7);
        assert_eq!(csr.edge_count, 6);

        // Check node 0's neighbors
        let neighbors = csr.neighbors(NodeId::new(0, 0));
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0], 1);

        // Check node 1's neighbors (has 2 outgoing edges)
        let neighbors = csr.neighbors(NodeId::new(1, 0));
        assert_eq!(neighbors.len(), 2);
        assert!(neighbors.contains(&2));
        assert!(neighbors.contains(&4));
    }

    #[test]
    fn test_csr_merges_lww_and_tombstones() {
        let file = FileId::new(0);
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };

        let csr_edges = vec![MergedEdge::new(
            NodeId::new(0, 0),
            NodeId::new(1, 0),
            kind.clone(),
            1,
            file,
        )];

        let delta_edges = vec![
            DeltaEdge::new(
                NodeId::new(0, 0),
                NodeId::new(1, 0),
                kind.clone(),
                2,
                DeltaOp::Remove,
                file,
            ),
            DeltaEdge::new(
                NodeId::new(0, 0),
                NodeId::new(2, 0),
                kind.clone(),
                3,
                DeltaOp::Add,
                file,
            ),
        ];

        let snapshot = CompactionSnapshot {
            csr_edges,
            delta_edges,
            node_count: 3,
            csr_version: 0,
        };

        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        assert_eq!(csr.edge_count, 1);
        assert_eq!(csr.neighbors(NodeId::new(0, 0)), &[2]);
    }

    #[test]
    fn test_scc_computation() {
        let snapshot = create_test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();

        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();

        // All nodes should be in trivial SCCs (no cycles)
        assert_eq!(scc.scc_count, 7);
        assert_eq!(scc.non_trivial_count, 0);
        assert_eq!(scc.max_scc_size, 1);
    }

    #[test]
    fn test_scc_with_cycle() {
        // Create a graph with a cycle: 0 -> 1 -> 2 -> 0
        let file = FileId::new(0);
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };

        let edges = vec![
            MergedEdge::new(NodeId::new(0, 0), NodeId::new(1, 0), kind.clone(), 1, file),
            MergedEdge::new(NodeId::new(1, 0), NodeId::new(2, 0), kind.clone(), 2, file),
            MergedEdge::new(NodeId::new(2, 0), NodeId::new(0, 0), kind.clone(), 3, file),
        ];

        let snapshot = CompactionSnapshot {
            csr_edges: edges,
            delta_edges: Vec::new(),
            node_count: 3,
            csr_version: 0,
        };

        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();

        // Should detect one non-trivial SCC containing nodes 0, 1, 2
        assert_eq!(scc.scc_count, 1);
        assert_eq!(scc.non_trivial_count, 1);
        assert_eq!(scc.max_scc_size, 3);

        // All three nodes should be in the same SCC
        let scc_0 = scc.scc_of(NodeId::new(0, 0)).unwrap();
        let scc_1 = scc.scc_of(NodeId::new(1, 0)).unwrap();
        let scc_2 = scc.scc_of(NodeId::new(2, 0)).unwrap();
        assert_eq!(scc_0, scc_1);
        assert_eq!(scc_1, scc_2);
    }

    #[test]
    fn test_condensation_dag() {
        let snapshot = create_test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();

        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build(&scc, &csr).unwrap();

        // Should have 7 SCCs (all trivial)
        assert_eq!(dag.scc_count, 7);

        // Should have 6 edges (same as original DAG since no cycles)
        assert_eq!(dag.edge_count, 6);

        // Topological order should exist (no cycles)
        assert_eq!(dag.topo_order.len(), 7);
    }

    #[test]
    fn test_2hop_reachability() {
        let snapshot = create_test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();

        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build(&scc, &csr).unwrap();

        // Check reachability: 0 can reach 3
        let scc_0 = scc.scc_of(NodeId::new(0, 0)).unwrap();
        let scc_3 = scc.scc_of(NodeId::new(3, 0)).unwrap();
        assert!(dag.can_reach(scc_0, scc_3));

        // Check non-reachability: 3 cannot reach 0
        assert!(!dag.can_reach(scc_3, scc_0));

        // Separate component: 0 cannot reach 6
        let scc_6 = scc.scc_of(NodeId::new(6, 0)).unwrap();
        assert!(!dag.can_reach(scc_0, scc_6));
    }

    #[test]
    fn test_persistence_roundtrip() {
        use tempfile::TempDir;

        let snapshot = create_test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();

        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build(&scc, &csr).unwrap();

        // Create temp directory
        let temp_dir = TempDir::new().unwrap();
        let csr_path = temp_dir.path().join("test.csr");
        let scc_path = temp_dir.path().join("test.scc");
        let dag_path = temp_dir.path().join("test.dag");

        let identity = AnalysisIdentity::new("manifest".to_string(), [42u8; 32]);

        // Persist
        persistence::persist_csr(&csr, &identity, &csr_path).unwrap();
        persistence::persist_scc(&scc, &identity, &scc_path).unwrap();
        persistence::persist_condensation(&dag, &identity, &dag_path).unwrap();

        // Load back
        let (csr_loaded, identity_loaded) = persistence::load_csr(&csr_path).unwrap();
        let (scc_loaded, identity_loaded_scc) = persistence::load_scc(&scc_path).unwrap();
        let (dag_loaded, identity_loaded_dag) = persistence::load_condensation(&dag_path).unwrap();

        // Verify
        assert_eq!(csr_loaded.node_count, csr.node_count);
        assert_eq!(csr_loaded.edge_count, csr.edge_count);
        assert_eq!(scc_loaded.scc_count, scc.scc_count);
        assert_eq!(dag_loaded.scc_count, dag.scc_count);
        assert_eq!(dag_loaded.edge_count, dag.edge_count);
        assert_eq!(identity_loaded, identity);
        assert_eq!(identity_loaded_scc, identity);
        assert_eq!(identity_loaded_dag, identity);
    }

    #[test]
    fn test_persistence_identity_mismatch_rejected() {
        use tempfile::TempDir;

        let snapshot = create_test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();

        let temp_dir = TempDir::new().unwrap();
        let csr_path = temp_dir.path().join("test.csr");

        let identity = AnalysisIdentity::new("manifest".to_string(), [1u8; 32]);
        let wrong_identity = AnalysisIdentity::new("other".to_string(), [2u8; 32]);

        persistence::persist_csr(&csr, &identity, &csr_path).unwrap();

        let result = persistence::load_csr_checked(&csr_path, &wrong_identity);
        assert!(result.is_err());
    }

    #[test]
    fn test_path_reconstruction_across_sccs() {
        let snapshot = create_test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();

        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build(&scc, &csr).unwrap();

        let recon = PathReconstructor::new(&csr, &scc, &dag);
        let path = recon
            .reconstruct_path(NodeId::new(0, 0), NodeId::new(3, 0))
            .unwrap()
            .unwrap();

        assert_eq!(path.first(), Some(&NodeId::new(0, 0)));
        assert_eq!(path.last(), Some(&NodeId::new(3, 0)));
    }

    #[test]
    fn test_path_reconstruction_unreachable() {
        let snapshot = create_test_snapshot();
        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();

        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build(&scc, &csr).unwrap();

        let recon = PathReconstructor::new(&csr, &scc, &dag);
        let path = recon
            .reconstruct_path(NodeId::new(0, 0), NodeId::new(6, 0))
            .unwrap();
        assert!(path.is_none());
    }

    #[test]
    fn test_path_reconstruction_intra_scc() {
        let file = FileId::new(0);
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };

        let edges = vec![
            MergedEdge::new(NodeId::new(0, 0), NodeId::new(1, 0), kind.clone(), 1, file),
            MergedEdge::new(NodeId::new(1, 0), NodeId::new(0, 0), kind.clone(), 2, file),
        ];

        let snapshot = CompactionSnapshot {
            csr_edges: edges,
            delta_edges: Vec::new(),
            node_count: 2,
            csr_version: 0,
        };

        let csr = CsrAdjacency::build_from_snapshot(&snapshot).unwrap();
        let scc = SccData::compute_tarjan(&csr, &kind).unwrap();
        let dag = CondensationDag::build(&scc, &csr).unwrap();

        let recon = PathReconstructor::new(&csr, &scc, &dag);
        let path = recon
            .reconstruct_path(NodeId::new(0, 0), NodeId::new(1, 0))
            .unwrap()
            .unwrap();

        assert_eq!(path.first(), Some(&NodeId::new(0, 0)));
        assert_eq!(path.last(), Some(&NodeId::new(1, 0)));
    }
}
