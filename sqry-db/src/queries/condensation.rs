//! Condensation DAG derived query.
//!
//! The condensation collapses each SCC into a single node, producing a DAG.
//! This is used by `trace_path` for efficient path-finding through large
//! call graphs.

use std::collections::HashMap;
use std::sync::Arc;

use crate::QueryDb;
use crate::dependency::record_file_dep;
use crate::queries::scc::SccQuery;
use crate::query::DerivedQuery;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::kind::EdgeKind;
#[cfg(test)]
use sqry_core::graph::unified::edge::kind::ResolvedVia;

// PN3 cold-start persistence: CachedCondensation is serialized via postcard at
// cache-insert time. HashMap<u32, Vec<u32>> contains only primitive types.
// EdgeKind already derives Serialize/Deserialize from sqry-core.

/// Type alias for the key used by [`CondensationQuery`].
/// Uses the same key type as [`SccQuery`] — the edge kind to condense over.
/// `EdgeKind` already derives `Serialize`/`Deserialize` from sqry-core.
pub type CondensationKey = EdgeKind;

/// Type alias for the value produced by [`CondensationQuery`].
/// `Arc` is serde-transparent when the workspace `serde` `rc` feature is enabled.
pub type CondensationValue = std::sync::Arc<CachedCondensation>;

/// Condensation of the call graph: DAG of SCC components.
///
/// Each SCC is collapsed to a single node. Edges between SCCs represent
/// inter-component calls.
// HashMap<u32, Vec<u32>>: primitive types, cleanly serde-able.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedCondensation {
    /// SCC component index → set of successor component indices.
    pub dag_edges: HashMap<u32, Vec<u32>>,
    /// Number of SCC components (= number of DAG nodes).
    pub component_count: usize,
    /// Edge kind this condensation was computed for.
    pub edge_kind: EdgeKind,
}

/// Computes the condensation DAG from the SCC decomposition.
///
/// # Invalidation
///
/// `TRACKS_EDGE_REVISION = true`: invalidated when any edge changes.
pub struct CondensationQuery;

impl DerivedQuery for CondensationQuery {
    type Key = EdgeKind;
    type Value = Arc<CachedCondensation>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::CONDENSATION;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(key: &EdgeKind, db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<CachedCondensation> {
        // Record all files as deps
        for (fid, _seg) in snapshot.file_segments().iter() {
            record_file_dep(fid);
        }

        // Get the cached SCC data (or compute it)
        let scc = db.get::<SccQuery>(key);

        // Build the condensation DAG: for each edge in the original graph,
        // if source and target are in different SCCs, add a DAG edge.
        let mut dag_edges: HashMap<u32, Vec<u32>> = HashMap::new();

        for (nid, entry) in snapshot.nodes().iter() {
            // Gate 0d iter-2 fix: skip unified losers from
            // condensation DAG. SccQuery already filters them, so
            // `component_of` would return None for a loser, but the
            // explicit guard is belt-and-braces. See
            // `NodeEntry::is_unified_loser`.
            if entry.is_unified_loser() {
                continue;
            }
            let Some(src_comp) = scc.component_of(nid) else {
                continue;
            };

            for edge_ref in &snapshot.edges().edges_from(nid) {
                if std::mem::discriminant(&edge_ref.kind) != std::mem::discriminant(key) {
                    continue;
                }
                if let Some(tgt_comp) = scc.component_of(edge_ref.target)
                    && src_comp != tgt_comp
                {
                    dag_edges.entry(src_comp).or_default().push(tgt_comp);
                }
            }
        }

        // Deduplicate DAG edges
        for successors in dag_edges.values_mut() {
            successors.sort_unstable();
            successors.dedup();
        }

        Arc::new(CachedCondensation {
            component_count: scc.component_count(),
            dag_edges,
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
    fn cached_condensation_roundtrip() {
        let mut dag_edges: HashMap<u32, Vec<u32>> = HashMap::new();
        dag_edges.insert(0, vec![1, 2]);
        dag_edges.insert(1, vec![3]);
        let original = CachedCondensation {
            dag_edges,
            component_count: 4,
            edge_kind: EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
        };
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: CachedCondensation = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded.component_count, original.component_count);
        assert_eq!(decoded.edge_kind, original.edge_kind);
        // dag_edges: verify contents match (HashMap order may differ).
        assert_eq!(decoded.dag_edges.len(), original.dag_edges.len());
        for (k, v) in &original.dag_edges {
            assert_eq!(decoded.dag_edges.get(k), Some(v));
        }
    }

    #[test]
    fn condensation_key_roundtrip() {
        // CondensationKey = EdgeKind
        let original: CondensationKey = EdgeKind::Calls {
            argument_count: 3,
            is_async: true,
            resolved_via: ResolvedVia::Direct,
        };
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: CondensationKey = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn condensation_value_roundtrip() {
        // CondensationValue = Arc<CachedCondensation>
        let original: CondensationValue = Arc::new(CachedCondensation {
            dag_edges: HashMap::new(),
            component_count: 0,
            edge_kind: EdgeKind::References,
        });
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: CondensationValue = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded.component_count, original.component_count);
        assert_eq!(decoded.edge_kind, original.edge_kind);
    }
}
