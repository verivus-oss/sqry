//! SCC (Strongly Connected Components) derived query.
//!
//! Wraps SCC computation as a [`DerivedQuery`] with `TRACKS_EDGE_REVISION = true`.
//! The cached result is automatically invalidated when any file's edges change.
//!
//! The actual Tarjan implementation lives in `sqry-core::graph::unified::analysis::scc`.
//! This wrapper provides the caching and invalidation layer. Phase 3C migration
//! (DB14) will wire the existing `find_all_cycles_graph` and `find_cycle_containing_node`
//! to use this cached query instead of computing SCC from scratch on every call.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::kind::EdgeKind;
#[cfg(test)]
use sqry_core::graph::unified::edge::kind::ResolvedVia;
use sqry_core::graph::unified::node::id::NodeId;

use crate::QueryDb;
use crate::dependency::record_file_dep;
use crate::query::DerivedQuery;

// PN3 cold-start persistence: CachedSccData is serialized via postcard at
// cache-insert time. HashMap<NodeId, u32> is serde-able because NodeId derives
// Serialize/Deserialize. EdgeKind also derives Serialize/Deserialize.

/// Type alias for the key used by [`SccQuery`] (an [`EdgeKind`] discriminant).
/// `EdgeKind` already derives `Serialize`/`Deserialize` from sqry-core.
pub type SccKey = EdgeKind;

/// Type alias for the value produced by [`SccQuery`].
/// `Arc` is serde-transparent when the workspace `serde` `rc` feature is enabled.
pub type SccValue = std::sync::Arc<CachedSccData>;

/// Cached SCC result mapping each node to its SCC component index.
///
/// Component indices are arbitrary (0-based). Two nodes in the same SCC
/// have the same component index.
// HashMap<NodeId, u32>: serde-able because NodeId derives Serialize/Deserialize.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedSccData {
    /// NodeId → SCC component index.
    pub node_to_component: HashMap<NodeId, u32>,
    /// SCC component index → member node IDs.
    pub components: Vec<Vec<NodeId>>,
    /// Edge kind this SCC was computed for.
    pub edge_kind: EdgeKind,
}

impl CachedSccData {
    /// Returns the SCC component index for a node, or `None` if not in any SCC.
    #[must_use]
    pub fn component_of(&self, node: NodeId) -> Option<u32> {
        self.node_to_component.get(&node).copied()
    }

    /// Returns true if the given node is part of a non-trivial cycle (SCC size > 1).
    #[must_use]
    pub fn is_in_cycle(&self, node: NodeId) -> bool {
        self.component_of(node)
            .map(|idx| {
                self.components
                    .get(idx as usize)
                    .is_some_and(|c| c.len() > 1)
            })
            .unwrap_or(false)
    }

    /// Returns the total number of SCC components.
    #[must_use]
    pub fn component_count(&self) -> usize {
        self.components.len()
    }
}

/// Computes strongly connected components for a given edge kind.
///
/// # Invalidation
///
/// `TRACKS_EDGE_REVISION = true`: invalidated when any edge changes.
pub struct SccQuery;

impl DerivedQuery for SccQuery {
    type Key = EdgeKind;
    type Value = Arc<CachedSccData>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::SCC;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(key: &EdgeKind, _db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<CachedSccData> {
        // Record all files as deps (global topology query)
        for (fid, _seg) in snapshot.file_segments().iter() {
            record_file_dep(fid);
        }

        // Iterative Tarjan SCC via BidirectionalEdgeStore::edges_from.
        // This works on both CSR and delta buffer edges transparently.
        //
        // # Determinism contract
        //
        // `EdgeStore::edges_from` documents that delta-buffer iteration uses
        // `HashMap` order and is non-deterministic both across and within
        // process runs. The iterative Tarjan loop below MUST therefore:
        //
        // 1. Compute each node's outgoing-neighbour list exactly once — at
        //    the moment the node is first pushed onto the work stack — and
        //    cache it inside the work-stack frame. Re-querying `edges_from`
        //    on subsequent iterations of the same frame is the original
        //    bug: between two queries the `Vec` may be returned in a
        //    different order, so the cursor `pos` advances over a
        //    different element than the algorithm intends, mis-targeting
        //    `lowlink` propagation and producing different SCC results on
        //    the same input.
        // 2. Sort each cached neighbour list by `NodeId` (which orders by
        //    `(index, generation)`) so the algorithm processes edges in a
        //    canonical order regardless of the underlying `edges_from`
        //    iteration order on this particular run.
        let mut index_counter = 0u32;
        let mut stack: Vec<NodeId> = Vec::new();
        let mut on_stack: HashSet<NodeId> = HashSet::new();
        let mut indices: HashMap<NodeId, u32> = HashMap::new();
        let mut lowlinks: HashMap<NodeId, u32> = HashMap::new();
        let mut components: Vec<Vec<NodeId>> = Vec::new();

        // Compute the set of outgoing neighbours for `node` filtered to the
        // requested `EdgeKind` discriminant, sorted by `NodeId` for
        // determinism. Called exactly once per node-push.
        let neighbours_of = |node: NodeId| -> Vec<NodeId> {
            let mut ns: Vec<NodeId> = snapshot
                .edges()
                .edges_from(node)
                .iter()
                .filter(|e| std::mem::discriminant(&e.kind) == std::mem::discriminant(key))
                .map(|e| e.target)
                .collect();
            ns.sort_unstable();
            ns
        };

        // Collect all nodes.
        // Gate 0d iter-2 fix: skip unified losers from SCC
        // computation. They have no outgoing edges post-finalize
        // (remapped to the winner via `NodeRemapTable`) so they would
        // each be a trivial 1-element SCC, but that's still a
        // publish-visible leak of loser IDs. See
        // `NodeEntry::is_unified_loser`.
        //
        // `NodeArena::iter` yields entries in slot order (deterministic
        // `Vec` walk) so this iteration is already stable.
        let all_nodes: Vec<NodeId> = snapshot
            .nodes()
            .iter()
            .filter(|(_nid, entry)| !entry.is_unified_loser())
            .map(|(nid, _)| nid)
            .collect();

        // Iterative Tarjan using an explicit work stack.
        // Frame layout: (node, cursor into cached neighbours, cached
        // sorted neighbour list). The cache makes Tarjan order-stable
        // regardless of `edges_from`'s `HashMap` delta-buffer order.
        for &start in &all_nodes {
            if indices.contains_key(&start) {
                continue;
            }

            let mut work: Vec<(NodeId, usize, Vec<NodeId>)> =
                vec![(start, 0, neighbours_of(start))];
            indices.insert(start, index_counter);
            lowlinks.insert(start, index_counter);
            index_counter += 1;
            stack.push(start);
            on_stack.insert(start);

            while let Some((node, pos, neighbors)) = work.last_mut() {
                if *pos < neighbors.len() {
                    let neighbor = neighbors[*pos];
                    *pos += 1;

                    if let std::collections::hash_map::Entry::Vacant(e) = indices.entry(neighbor) {
                        e.insert(index_counter);
                        lowlinks.insert(neighbor, index_counter);
                        index_counter += 1;
                        stack.push(neighbor);
                        on_stack.insert(neighbor);
                        let neighbor_neighbours = neighbours_of(neighbor);
                        work.push((neighbor, 0, neighbor_neighbours));
                    } else if on_stack.contains(&neighbor) {
                        let node_copy = *node;
                        let neighbor_idx = indices[&neighbor];
                        let current_low = lowlinks[&node_copy];
                        if neighbor_idx < current_low {
                            lowlinks.insert(node_copy, neighbor_idx);
                        }
                    }
                } else {
                    // All neighbours processed; check if root of SCC
                    let node_copy = *node;
                    let node_idx = indices[&node_copy];
                    let node_low = lowlinks[&node_copy];

                    if node_low == node_idx {
                        // Root of SCC — pop stack to form component
                        let mut component = Vec::new();
                        while let Some(w) = stack.pop() {
                            on_stack.remove(&w);
                            component.push(w);
                            if w == node_copy {
                                break;
                            }
                        }
                        components.push(component);
                    }

                    // Propagate lowlink to parent
                    work.pop();
                    if let Some((parent, _, _)) = work.last() {
                        let parent_copy = *parent;
                        let parent_low = lowlinks[&parent_copy];
                        if node_low < parent_low {
                            lowlinks.insert(parent_copy, node_low);
                        }
                    }
                }
            }
        }

        // Build the node-to-component map
        let mut node_to_component = HashMap::with_capacity(all_nodes.len());
        for (idx, component) in components.iter().enumerate() {
            for &nid in component {
                node_to_component.insert(nid, idx as u32);
            }
        }

        Arc::new(CachedSccData {
            node_to_component,
            components,
            edge_kind: key.clone(),
        })
    }
}

// ============================================================================
// Determinism regression tests
// ============================================================================

#[cfg(test)]
mod determinism_tests {
    use super::*;
    use crate::QueryDbConfig;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::node::kind::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;
    use std::path::Path;

    /// Regression test for a non-determinism bug in `SccQuery::execute` exposed
    /// by the WS1 differential property harness (proptest counter-example
    /// seed `cc a8168cea9a32524e5950c6f414a84d72214577aa76365c73a86b4f0248002fbe`).
    ///
    /// The original iterative Tarjan loop recomputed `neighbors = edges_from(node)`
    /// on every iteration of the work-stack while-loop. `EdgeStore::edges_from`
    /// documents that the delta-buffer portion is returned in `HashMap` order,
    /// which is non-deterministic across — and within — process runs. Between
    /// two iterations of the same `(node, pos)` frame the `Vec` could be
    /// reordered, so `neighbors[pos]` advanced over a different element than
    /// the algorithm intended, mis-targeting lowlink propagation and producing
    /// spurious SCCs in acyclic graphs.
    ///
    /// The minimal counter-example: 4 Imports edges `{(17,21), (17,23),
    /// (1,14), (0,22)}`, no cycle possible. Planner produced SCC
    /// `[NodeId(22), NodeId(1)]`; baseline produced singletons. We use the
    /// same shape here (4 nodes, no Calls edges, 4 Imports edges all pushed
    /// through the delta buffer) and assert across 100 invocations on the
    /// same snapshot that:
    ///   1. Every invocation returns the same SCC partition.
    ///   2. Every component is a singleton (no false cycle).
    ///   3. Per-component member order is stable.
    #[test]
    fn scc_is_deterministic_across_repeated_invocations() {
        let mut graph = CodeGraph::new();
        let file = graph.files_mut().register(Path::new("lib.rs")).unwrap();

        // Allocate 8 distinct function nodes; we only wire 4 acyclic Imports
        // edges among them. The exact NodeIds depend on alloc order, but the
        // graph topology is acyclic regardless.
        let mut nodes: Vec<NodeId> = Vec::new();
        for i in 0..8u32 {
            let name = graph.strings_mut().intern(&format!("sym_{i}")).unwrap();
            let id = graph
                .nodes_mut()
                .alloc(NodeEntry::new(NodeKind::Function, name, file).with_qualified_name(name))
                .unwrap();
            nodes.push(id);
        }

        // Wire 4 acyclic Imports edges. With node alloc ids `n0..n7` these
        // are: n0→n5, n2→n6, n3→n4, n7→n1. No cycles, even ignoring edge
        // kinds — every target index differs from every source index in
        // each edge.
        let import = EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        };
        graph
            .edges_mut()
            .add_edge(nodes[0], nodes[5], import.clone(), file);
        graph
            .edges_mut()
            .add_edge(nodes[2], nodes[6], import.clone(), file);
        graph
            .edges_mut()
            .add_edge(nodes[3], nodes[4], import.clone(), file);
        graph
            .edges_mut()
            .add_edge(nodes[7], nodes[1], import.clone(), file);

        let snapshot = Arc::new(graph.snapshot());
        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());

        // First run is the reference. We bypass the cache (which would
        // mask non-determinism trivially) by calling `SccQuery::execute`
        // directly 100 times against the same snapshot.
        let reference = SccQuery::execute(&import, &db, &snapshot);

        // Sanity: the input has no cycle so every component must be a
        // singleton. Pre-fix the bug produced a 2-element SCC for this
        // shape on at least some runs.
        for component in &reference.components {
            assert_eq!(
                component.len(),
                1,
                "input is acyclic — every SCC must be a singleton, \
                 got component {component:?}"
            );
        }
        assert_eq!(
            reference.components.len(),
            nodes.len(),
            "expected one singleton SCC per node ({}), got {}",
            nodes.len(),
            reference.components.len(),
        );

        for run in 0..100 {
            let result = SccQuery::execute(&import, &db, &snapshot);
            assert_eq!(
                result.components, reference.components,
                "SCC components differ on run {run} — non-deterministic"
            );
            assert_eq!(
                result.node_to_component, reference.node_to_component,
                "SCC node→component map differs on run {run} — \
                 non-deterministic"
            );
            assert_eq!(
                result.edge_kind, reference.edge_kind,
                "SCC edge_kind differs on run {run}",
            );
        }
    }
}

// ============================================================================
// PN3 serde roundtrip tests
// ============================================================================

#[cfg(test)]
mod serde_roundtrip {
    use super::*;
    use postcard::{from_bytes, to_allocvec};

    #[test]
    fn cached_scc_data_roundtrip() {
        let mut node_to_component = HashMap::new();
        node_to_component.insert(NodeId::new(1, 1), 0u32);
        node_to_component.insert(NodeId::new(2, 1), 0u32);
        node_to_component.insert(NodeId::new(3, 1), 1u32);
        let original = CachedSccData {
            node_to_component,
            components: vec![
                vec![NodeId::new(1, 1), NodeId::new(2, 1)],
                vec![NodeId::new(3, 1)],
            ],
            edge_kind: EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
        };
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: CachedSccData = from_bytes(&bytes).expect("deserialize failed");
        // Verify components round-trip exactly (ordered).
        assert_eq!(decoded.components, original.components);
        // Verify edge_kind round-trips exactly.
        assert_eq!(decoded.edge_kind, original.edge_kind);
        // Verify node_to_component round-trips (check each entry).
        for (node, comp) in &original.node_to_component {
            assert_eq!(decoded.node_to_component.get(node), Some(comp));
        }
    }

    #[test]
    fn scc_key_roundtrip() {
        // SccKey = EdgeKind — already has serde; roundtrip confirms usage.
        let original: SccKey = EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        };
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: SccKey = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn scc_value_roundtrip() {
        // SccValue = Arc<CachedSccData>
        let data = CachedSccData {
            node_to_component: HashMap::new(),
            components: vec![],
            edge_kind: EdgeKind::References,
        };
        let original: SccValue = Arc::new(data);
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: SccValue = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded.components, original.components);
        assert_eq!(decoded.edge_kind, original.edge_kind);
    }
}
