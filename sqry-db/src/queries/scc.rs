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
        let mut index_counter = 0u32;
        let mut stack: Vec<NodeId> = Vec::new();
        let mut on_stack: HashSet<NodeId> = HashSet::new();
        let mut indices: HashMap<NodeId, u32> = HashMap::new();
        let mut lowlinks: HashMap<NodeId, u32> = HashMap::new();
        let mut components: Vec<Vec<NodeId>> = Vec::new();

        // Collect all nodes.
        // Gate 0d iter-2 fix: skip unified losers from SCC
        // computation. They have no outgoing edges post-finalize
        // (remapped to the winner via `NodeRemapTable`) so they would
        // each be a trivial 1-element SCC, but that's still a
        // publish-visible leak of loser IDs. See
        // `NodeEntry::is_unified_loser`.
        let all_nodes: Vec<NodeId> = snapshot
            .nodes()
            .iter()
            .filter(|(_nid, entry)| !entry.is_unified_loser())
            .map(|(nid, _)| nid)
            .collect();

        // Iterative Tarjan using an explicit work stack
        for &start in &all_nodes {
            if indices.contains_key(&start) {
                continue;
            }

            // DFS work stack: (node, neighbor_iterator_position)
            let mut work: Vec<(NodeId, usize)> = vec![(start, 0)];
            indices.insert(start, index_counter);
            lowlinks.insert(start, index_counter);
            index_counter += 1;
            stack.push(start);
            on_stack.insert(start);

            while let Some((node, pos)) = work.last_mut() {
                let neighbors: Vec<NodeId> = snapshot
                    .edges()
                    .edges_from(*node)
                    .iter()
                    .filter(|e| std::mem::discriminant(&e.kind) == std::mem::discriminant(key))
                    .map(|e| e.target)
                    .collect();

                if *pos < neighbors.len() {
                    let neighbor = neighbors[*pos];
                    *pos += 1;

                    if let std::collections::hash_map::Entry::Vacant(e) = indices.entry(neighbor) {
                        e.insert(index_counter);
                        lowlinks.insert(neighbor, index_counter);
                        index_counter += 1;
                        stack.push(neighbor);
                        on_stack.insert(neighbor);
                        work.push((neighbor, 0));
                    } else if on_stack.contains(&neighbor) {
                        let node_copy = *node;
                        let neighbor_idx = indices[&neighbor];
                        let current_low = lowlinks[&node_copy];
                        if neighbor_idx < current_low {
                            lowlinks.insert(node_copy, neighbor_idx);
                        }
                    }
                } else {
                    // All neighbors processed; check if root of SCC
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
                    if let Some((parent, _)) = work.last() {
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
