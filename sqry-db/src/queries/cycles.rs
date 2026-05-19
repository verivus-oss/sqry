//! Cycle detection derived queries, built on top of [`super::SccQuery`].
//!
//! Migrates `sqry-core::query::executor::graph_cycles::find_all_cycles_graph`
//! and `is_node_in_cycle` into cache-aware queries. The underlying Tarjan SCC
//! implementation continues to live in [`super::scc`]; the queries in this
//! module only apply cycle filtering (self-loop handling, `min_depth` /
//! `max_depth` bounds, `max_results` cap) on top of the cached SCC data.
//!
//! # Why not call back into `graph_cycles`?
//!
//! The `sqry-core` functions build their own adjacency + Tarjan state from
//! scratch on every call. With DB14, every analysis path that wants cycle
//! detection should share the [`super::SccQuery`] cache so one Tarjan run
//! per edge-revision bump is enough. These queries expose the filtered
//! `Vec<Vec<NodeId>>` / boolean answers the sqry-core functions currently
//! produce.
//!
//! # Self-loop semantics
//!
//! [`sqry_core::query::CircularConfig::should_include_self_loops`] gates
//! whether single-node SCCs containing a self-edge of the relevant kind are
//! reported. A node participates in a self-loop iff it has an edge of the
//! relevant kind back to itself. The queries consult the snapshot's edge
//! store directly — `SccQuery` alone cannot distinguish a size-1 SCC with a
//! self-loop from an isolated node.

use std::sync::Arc;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::query::CircularType;

use crate::QueryDb;
use crate::dependency::record_file_dep;
use crate::query::DerivedQuery;

use super::SccQuery;

// ============================================================================
// Key + result types
// ============================================================================

// PN3 cold-start persistence: CycleBounds, CyclesKey, IsInCycleKey are
// serialized via postcard at cache-insert time. CircularType gains
// Serialize/Deserialize in sqry-core/src/query/cycles_config.rs (PN3 SERDE_DERIVES).

/// Type alias for the value produced by [`CyclesQuery`].
/// Arc is serde-transparent when the workspace `serde` `rc` feature is enabled.
pub type CyclesValue = std::sync::Arc<Vec<Vec<sqry_core::graph::unified::node::id::NodeId>>>;

/// Type alias for the value produced by [`IsInCycleQuery`].
pub type IsInCycleValue = bool;

/// Bounds applied to SCC components before they are reported as cycles.
///
/// Mirrors [`sqry_core::query::CircularConfig`] but lives here so the type
/// is [`Hash`]+[`Eq`]+[`Clone`] for use as a `DerivedQuery` cache key.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CycleBounds {
    /// Minimum cycle depth to report (default: `2`). A size-1 SCC only
    /// counts as a cycle when `should_include_self_loops` is true.
    pub min_depth: usize,
    /// Maximum cycle depth to report (`None` = unbounded).
    pub max_depth: Option<usize>,
    /// Maximum number of cycles to return (truncates large result sets).
    pub max_results: usize,
    /// Whether size-1 SCCs that carry a self-edge count as cycles.
    pub should_include_self_loops: bool,
}

impl Default for CycleBounds {
    fn default() -> Self {
        Self {
            min_depth: 2,
            max_depth: None,
            max_results: 100,
            should_include_self_loops: false,
        }
    }
}

/// Cache key for [`CyclesQuery`].
#[derive(Debug, Clone, Hash, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CyclesKey {
    /// Which edge kind to traverse: `Calls`, `Imports`, or `Modules`.
    pub circular_type: CircularType,
    /// Cycle filtering bounds.
    pub bounds: CycleBounds,
}

/// Cache key for [`IsInCycleQuery`].
#[derive(Debug, Clone, Hash, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IsInCycleKey {
    /// The node to check.
    pub node_id: NodeId,
    /// Which edge kind to traverse.
    pub circular_type: CircularType,
    /// Cycle filtering bounds.
    pub bounds: CycleBounds,
}

// ============================================================================
// CyclesQuery
// ============================================================================

/// Returns every cycle in the graph as a sorted vector of `NodeId` lists.
///
/// Each inner `Vec<NodeId>` is a strongly connected component (SCC) whose
/// size meets the [`CycleBounds`] criteria. Non-trivial SCCs are reported
/// unconditionally; size-1 SCCs are included only when the node carries a
/// self-edge and `should_include_self_loops` is true. The outer vector is
/// truncated to `bounds.max_results`.
///
/// # Invalidation
///
/// `TRACKS_EDGE_REVISION = true`: any edge change rebuilds the SCC data
/// (via [`SccQuery`]) and therefore the filtered cycle list.
pub struct CyclesQuery;

impl DerivedQuery for CyclesQuery {
    type Key = CyclesKey;
    type Value = Arc<Vec<Vec<NodeId>>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::CYCLES;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(key: &CyclesKey, db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<Vec<Vec<NodeId>>> {
        for (fid, _) in snapshot.file_segments().iter() {
            record_file_dep(fid);
        }

        let edge_probe = edge_probe_for(key.circular_type);
        let scc = db.get::<SccQuery>(&edge_probe);

        let mut cycles: Vec<Vec<NodeId>> = Vec::new();
        for component in &scc.components {
            if cycles.len() >= key.bounds.max_results {
                break;
            }
            let size = component.len();
            let is_self_loop =
                size == 1 && node_has_self_loop(snapshot, component[0], key.circular_type);
            if !should_include(size, is_self_loop, &key.bounds) {
                continue;
            }
            cycles.push(component.clone());
        }

        Arc::new(cycles)
    }
}

// ============================================================================
// IsInCycleQuery
// ============================================================================

/// Returns `true` if `node_id` belongs to an SCC that satisfies the
/// [`CycleBounds`] criteria.
///
/// Single-node SCCs are considered cycles iff the node has a self-edge of the
/// relevant kind *and* `should_include_self_loops` is true, matching
/// `graph_cycles::is_node_in_cycle`.
pub struct IsInCycleQuery;

impl DerivedQuery for IsInCycleQuery {
    type Key = IsInCycleKey;
    type Value = bool;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::IS_IN_CYCLE;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(key: &IsInCycleKey, db: &QueryDb, snapshot: &GraphSnapshot) -> bool {
        // Record deps for the node's file (narrow dep footprint when the edge
        // revision counter hasn't changed and the node's file isn't edited).
        if let Some(entry) = snapshot.nodes().get(key.node_id) {
            record_file_dep(entry.file);
        }

        let self_loop = node_has_self_loop(snapshot, key.node_id, key.circular_type);
        if self_loop && key.bounds.should_include_self_loops {
            return true;
        }

        let edge_probe = edge_probe_for(key.circular_type);
        let scc = db.get::<SccQuery>(&edge_probe);
        let Some(component_idx) = scc.component_of(key.node_id) else {
            return false;
        };
        let Some(component) = scc.components.get(component_idx as usize) else {
            return false;
        };

        let size = component.len();
        // Size-1 SCCs with a self-loop are already handled above. For other
        // nodes, enforce `min_depth >= 2` implicitly — a size-1 SCC without a
        // self-loop is not a cycle.
        if size < 2 {
            return false;
        }
        if size < key.bounds.min_depth {
            return false;
        }
        if key.bounds.max_depth.is_some_and(|max| size > max) {
            return false;
        }
        true
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn edge_probe_for(circular_type: CircularType) -> EdgeKind {
    match circular_type {
        CircularType::Calls => EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        CircularType::Imports | CircularType::Modules => EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        },
    }
}

fn node_has_self_loop(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    circular_type: CircularType,
) -> bool {
    for edge in &snapshot.edges().edges_from(node_id) {
        if edge.target != node_id {
            continue;
        }
        let is_match = match circular_type {
            CircularType::Calls => matches!(edge.kind, EdgeKind::Calls { .. }),
            CircularType::Imports | CircularType::Modules => {
                matches!(edge.kind, EdgeKind::Imports { .. })
            }
        };
        if is_match {
            return true;
        }
    }
    false
}

fn should_include(size: usize, is_self_loop: bool, bounds: &CycleBounds) -> bool {
    if is_self_loop {
        return bounds.should_include_self_loops;
    }
    if size == 1 {
        return false;
    }
    if size < bounds.min_depth {
        return false;
    }
    if bounds.max_depth.is_some_and(|max| size > max) {
        return false;
    }
    true
}

// ============================================================================
// PN3 serde roundtrip tests
// ============================================================================

#[cfg(test)]
mod serde_roundtrip {
    use super::*;
    use postcard::{from_bytes, to_allocvec};
    use sqry_core::graph::unified::node::id::NodeId;
    use sqry_core::query::CircularType;
    use std::sync::Arc;

    #[test]
    fn cycle_bounds_roundtrip() {
        let original = CycleBounds {
            min_depth: 3,
            max_depth: Some(10),
            max_results: 50,
            should_include_self_loops: true,
        };
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: CycleBounds = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn cycle_bounds_no_max_depth_roundtrip() {
        let original = CycleBounds::default();
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: CycleBounds = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn cycles_key_roundtrip() {
        let original = CyclesKey {
            circular_type: CircularType::Calls,
            bounds: CycleBounds::default(),
        };
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: CyclesKey = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn is_in_cycle_key_roundtrip() {
        let original = IsInCycleKey {
            node_id: NodeId::new(42, 7),
            circular_type: CircularType::Imports,
            bounds: CycleBounds {
                min_depth: 2,
                max_depth: None,
                max_results: 100,
                should_include_self_loops: false,
            },
        };
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: IsInCycleKey = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn cycles_value_roundtrip() {
        // CyclesValue = Arc<Vec<Vec<NodeId>>>
        let original: CyclesValue = Arc::new(vec![vec![NodeId::new(1, 1), NodeId::new(2, 1)]]);
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: CyclesValue = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded.as_ref(), original.as_ref());
    }

    #[test]
    fn is_in_cycle_value_roundtrip() {
        // IsInCycleValue = bool
        for val in [true, false] {
            let bytes = to_allocvec(&val).expect("serialize failed");
            let decoded: IsInCycleValue = from_bytes(&bytes).expect("deserialize failed");
            assert_eq!(decoded, val);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueryDbConfig;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::node::kind::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;
    use std::path::Path;

    fn alloc_fn(graph: &mut CodeGraph, name: &str) -> NodeId {
        let file = graph.files_mut().register(Path::new("x.rs")).unwrap();
        let name_id = graph.strings_mut().intern(name).unwrap();
        graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name_id, file).with_qualified_name(name_id))
            .unwrap()
    }

    fn add_call(graph: &mut CodeGraph, src: NodeId, tgt: NodeId) {
        let file = graph.nodes().get(src).unwrap().file;
        graph.edges_mut().add_edge(
            src,
            tgt,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
    }

    fn build_db(graph: CodeGraph) -> QueryDb {
        let snapshot = Arc::new(graph.snapshot());
        let mut db = QueryDb::new(snapshot, QueryDbConfig::default());
        db.register::<SccQuery>();
        db.register::<CyclesQuery>();
        db.register::<IsInCycleQuery>();
        db
    }

    #[test]
    fn cycles_query_detects_two_node_cycle() {
        let mut graph = CodeGraph::new();
        let a = alloc_fn(&mut graph, "a");
        let b = alloc_fn(&mut graph, "b");
        add_call(&mut graph, a, b);
        add_call(&mut graph, b, a);

        let db = build_db(graph);
        let key = CyclesKey {
            circular_type: CircularType::Calls,
            bounds: CycleBounds::default(),
        };
        let cycles = db.get::<CyclesQuery>(&key);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].len(), 2);
    }

    #[test]
    fn is_in_cycle_self_loop_requires_opt_in() {
        let mut graph = CodeGraph::new();
        let a = alloc_fn(&mut graph, "a");
        add_call(&mut graph, a, a);

        let db = build_db(graph);
        let default_key = IsInCycleKey {
            node_id: a,
            circular_type: CircularType::Calls,
            bounds: CycleBounds::default(),
        };
        assert!(
            !db.get::<IsInCycleQuery>(&default_key),
            "self-loop excluded by default"
        );
        let opted_key = IsInCycleKey {
            node_id: a,
            circular_type: CircularType::Calls,
            bounds: CycleBounds {
                should_include_self_loops: true,
                min_depth: 1,
                ..CycleBounds::default()
            },
        };
        assert!(
            db.get::<IsInCycleQuery>(&opted_key),
            "self-loop reported when opted-in"
        );
    }

    #[test]
    fn cycles_query_respects_max_results() {
        let mut graph = CodeGraph::new();
        let a = alloc_fn(&mut graph, "a");
        let b = alloc_fn(&mut graph, "b");
        let c = alloc_fn(&mut graph, "c");
        let d = alloc_fn(&mut graph, "d");
        // Two independent 2-cycles
        add_call(&mut graph, a, b);
        add_call(&mut graph, b, a);
        add_call(&mut graph, c, d);
        add_call(&mut graph, d, c);

        let db = build_db(graph);
        let key = CyclesKey {
            circular_type: CircularType::Calls,
            bounds: CycleBounds {
                max_results: 1,
                ..CycleBounds::default()
            },
        };
        let cycles = db.get::<CyclesQuery>(&key);
        assert_eq!(cycles.len(), 1);
    }
}
