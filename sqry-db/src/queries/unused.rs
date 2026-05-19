//! Unused / dead-code detection derived queries.
//!
//! Migrates `sqry-core::query::executor::graph_unused::{find_unused_nodes,
//! compute_reachable_set_graph, is_node_unused}` into cache-aware queries
//! that can be shared between the `sqry-db` planner, the MCP handlers (once
//! DB15+ wires them), and future analysis callers.
//!
//! # Pipeline
//!
//! ```text
//!   EntryPointsQuery   ──▶  (Arc<HashSet<NodeId>>)
//!          │
//!          ▼
//!   ReachableFromEntryPointsQuery  ──▶  (Arc<HashSet<NodeId>>)
//!          │
//!          ▼
//!   UnusedQuery / IsNodeUnusedQuery
//! ```
//!
//! # Entry-point classification
//!
//! A node is an entry point when *any* of the following is true:
//! - Its visibility is `public` / `pub`
//! - Its short name is `main`, begins with `test_`, or ends with `_test`
//! - Its [`NodeKind`] is [`NodeKind::Test`] or [`NodeKind::Export`]
//!
//! This mirrors [`sqry_core::query::executor::graph_unused`]'s
//! `is_entry_point_node` exactly so migration preserves the legacy set of
//! unused-free symbols.
//!
//! # Reachability edges
//!
//! The traversal follows `Calls`, `References`, `Imports`, `Inherits`,
//! `Implements`, and `TypeOf` edges — identical to the legacy
//! `is_reachability_edge` predicate.

use std::collections::HashSet;
use std::sync::Arc;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::kind::EdgeKind;
#[cfg(test)]
use sqry_core::graph::unified::edge::kind::ResolvedVia;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::query::UnusedScope;

use crate::QueryDb;
use crate::dependency::record_file_dep;
use crate::query::DerivedQuery;

// ============================================================================
// EntryPointsQuery
// ============================================================================

/// Set of node IDs that qualify as reachability roots.
///
/// # Invalidation
///
/// `TRACKS_METADATA_REVISION = true`: visibility, kind, and node name are
/// all stored on `NodeEntry`, which mutates with metadata revisions rather
/// than edge revisions. We also track edges at `TRACKS_EDGE_REVISION = true`
/// because edge additions can introduce new `Test`/`Export` nodes.
pub struct EntryPointsQuery;

impl DerivedQuery for EntryPointsQuery {
    type Key = ();
    type Value = Arc<HashSet<NodeId>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::ENTRY_POINTS;
    const TRACKS_EDGE_REVISION: bool = true;
    const TRACKS_METADATA_REVISION: bool = true;

    fn execute(_key: &(), _db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<HashSet<NodeId>> {
        for (fid, _) in snapshot.file_segments().iter() {
            record_file_dep(fid);
        }
        let mut entry_points = HashSet::new();
        for (node_id, entry) in snapshot.nodes().iter() {
            // Gate 0d iter-2 fix: skip unified losers from
            // `EntryPointsQuery`. Losers are inert duplicates; their
            // winner carries the real visibility/name. See
            // `NodeEntry::is_unified_loser`.
            if entry.is_unified_loser() {
                continue;
            }
            if is_entry_point(snapshot, entry) {
                entry_points.insert(node_id);
            }
        }
        Arc::new(entry_points)
    }
}

fn is_entry_point(snapshot: &GraphSnapshot, entry: &NodeEntry) -> bool {
    let is_public = entry
        .visibility
        .and_then(|id| snapshot.strings().resolve(id))
        .is_some_and(|v| {
            let s = v.as_ref();
            s == "public" || s == "pub"
        });
    let is_main_or_test = snapshot.strings().resolve(entry.name).is_some_and(|name| {
        let n = name.as_ref();
        n == "main" || n.starts_with("test_") || n.ends_with("_test")
    });
    let is_export = matches!(entry.kind, NodeKind::Export);
    let is_test_node = matches!(entry.kind, NodeKind::Test);
    is_public || is_main_or_test || is_export || is_test_node
}

// ============================================================================
// ReachableFromEntryPointsQuery
// ============================================================================

/// Set of nodes reachable from the entry-point set by following reachability
/// edges (`Calls`, `References`, `Imports`, `Inherits`, `Implements`,
/// `TypeOf`).
///
/// `TRACKS_EDGE_REVISION = true`: any edge change redraws the reachable set.
pub struct ReachableFromEntryPointsQuery;

impl DerivedQuery for ReachableFromEntryPointsQuery {
    type Key = ();
    type Value = Arc<HashSet<NodeId>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::REACHABLE_FROM_ENTRY_POINTS;
    const TRACKS_EDGE_REVISION: bool = true;
    const TRACKS_METADATA_REVISION: bool = true;

    fn execute(_key: &(), db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<HashSet<NodeId>> {
        for (fid, _) in snapshot.file_segments().iter() {
            record_file_dep(fid);
        }
        let entry_points = db.get::<EntryPointsQuery>(&());
        let mut reachable: HashSet<NodeId> = entry_points.as_ref().clone();
        let mut worklist: Vec<NodeId> = reachable.iter().copied().collect();
        while let Some(node_id) = worklist.pop() {
            for edge in &snapshot.edges().edges_from(node_id) {
                if is_reachability_edge(&edge.kind) && reachable.insert(edge.target) {
                    worklist.push(edge.target);
                }
            }
        }
        Arc::new(reachable)
    }
}

fn is_reachability_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Calls { .. }
            | EdgeKind::References
            | EdgeKind::Imports { .. }
            | EdgeKind::Inherits
            | EdgeKind::Implements
            | EdgeKind::TypeOf { .. }
    )
}

// ============================================================================
// UnusedQuery
// ============================================================================

// PN3 cold-start persistence: UnusedKey and IsNodeUnusedKey are serialized via
// postcard at cache-insert time. UnusedScope gains Serialize/Deserialize in
// sqry-core/src/query/unused_config.rs (PN3 SERDE_DERIVES). NodeId already
// derives Serialize/Deserialize from sqry-core.

/// Type alias for the value produced by [`UnusedQuery`].
/// `Arc` is serde-transparent when the workspace `serde` `rc` feature is enabled.
pub type UnusedValue = Arc<Vec<NodeId>>;

/// Type alias for the value produced by [`IsNodeUnusedQuery`].
pub type IsNodeUnusedValue = bool;

/// Cache key for [`UnusedQuery`].
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UnusedKey {
    /// Scope filter (All / Public / Private / Function / Struct).
    pub scope: UnusedScope,
    /// Maximum number of unused nodes to return.
    pub max_results: usize,
}

/// Returns the sorted list of unused nodes matching the scope filter.
///
/// A node is unused when:
/// 1. It matches the [`UnusedKey::scope`] filter.
/// 2. It is not an entry point (`main`, test, export, or `NodeKind::Test`).
/// 3. It is not reachable from any entry point.
pub struct UnusedQuery;

impl DerivedQuery for UnusedQuery {
    type Key = UnusedKey;
    type Value = Arc<Vec<NodeId>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::UNUSED;
    const TRACKS_EDGE_REVISION: bool = true;
    const TRACKS_METADATA_REVISION: bool = true;

    fn execute(key: &UnusedKey, db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<Vec<NodeId>> {
        for (fid, _) in snapshot.file_segments().iter() {
            record_file_dep(fid);
        }
        let reachable = db.get::<ReachableFromEntryPointsQuery>(&());
        let mut unused: Vec<NodeId> = Vec::new();
        for (node_id, entry) in snapshot.nodes().iter() {
            if unused.len() >= key.max_results {
                break;
            }
            // Gate 0d iter-2 fix: never report a unified loser as
            // "unused". See `NodeEntry::is_unified_loser`.
            if entry.is_unified_loser() {
                continue;
            }
            if !scope_matches(entry, snapshot, key.scope) {
                continue;
            }
            if is_always_entry_point(snapshot, entry) {
                continue;
            }
            if !reachable.contains(&node_id) {
                unused.push(node_id);
            }
        }
        unused.sort_unstable_by_key(|id| (id.index(), id.generation()));
        Arc::new(unused)
    }
}

fn scope_matches(entry: &NodeEntry, snapshot: &GraphSnapshot, scope: UnusedScope) -> bool {
    match scope {
        UnusedScope::All => true,
        UnusedScope::Public => entry
            .visibility
            .and_then(|id| snapshot.strings().resolve(id))
            .is_some_and(|v| {
                let s = v.as_ref();
                s == "public" || s == "pub"
            }),
        UnusedScope::Private => {
            let vis = entry
                .visibility
                .and_then(|id| snapshot.strings().resolve(id));
            vis.is_none()
                || vis.is_some_and(|v| {
                    let s = v.as_ref();
                    s != "public" && s != "pub"
                })
        }
        UnusedScope::Function => matches!(entry.kind, NodeKind::Function | NodeKind::Method),
        UnusedScope::Struct => matches!(entry.kind, NodeKind::Struct | NodeKind::Class),
    }
}

/// Entry points that should never be marked unused regardless of scope.
/// Mirrors the secondary entry-point check in
/// [`sqry_core::query::executor::graph_unused::is_node_unused`].
fn is_always_entry_point(snapshot: &GraphSnapshot, entry: &NodeEntry) -> bool {
    let is_main_or_test = snapshot.strings().resolve(entry.name).is_some_and(|name| {
        let n = name.as_ref();
        n == "main" || n.starts_with("test_") || n.ends_with("_test")
    });
    let is_export = matches!(entry.kind, NodeKind::Export);
    let is_test_node = matches!(entry.kind, NodeKind::Test);
    is_main_or_test || is_export || is_test_node
}

// ============================================================================
// IsNodeUnusedQuery
// ============================================================================

/// Cache key for [`IsNodeUnusedQuery`].
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct IsNodeUnusedKey {
    /// The node to check.
    pub node_id: NodeId,
    /// Scope filter to apply.
    pub scope: UnusedScope,
}

/// Returns `true` when the given node is unused per the rules in
/// [`UnusedQuery`]'s documentation. Used by the MCP `find_unused` single-node
/// tool and the CLI `sqry unused` check mode.
pub struct IsNodeUnusedQuery;

impl DerivedQuery for IsNodeUnusedQuery {
    type Key = IsNodeUnusedKey;
    type Value = bool;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::IS_NODE_UNUSED;
    const TRACKS_EDGE_REVISION: bool = true;
    const TRACKS_METADATA_REVISION: bool = true;

    fn execute(key: &IsNodeUnusedKey, db: &QueryDb, snapshot: &GraphSnapshot) -> bool {
        let Some(entry) = snapshot.nodes().get(key.node_id) else {
            return false;
        };
        record_file_dep(entry.file);
        if !scope_matches(entry, snapshot, key.scope) {
            return false;
        }
        if is_always_entry_point(snapshot, entry) {
            return false;
        }
        let reachable = db.get::<ReachableFromEntryPointsQuery>(&());
        !reachable.contains(&key.node_id)
    }
}

// ============================================================================
// PN3 serde roundtrip tests
// ============================================================================

#[cfg(test)]
mod serde_roundtrip {
    use super::*;
    use postcard::{from_bytes, to_allocvec};
    use sqry_core::graph::unified::node::id::NodeId;
    use sqry_core::query::UnusedScope;

    #[test]
    fn unused_key_roundtrip() {
        let original = UnusedKey {
            scope: UnusedScope::Public,
            max_results: 250,
        };
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: UnusedKey = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn unused_key_all_scopes_roundtrip() {
        for scope in [
            UnusedScope::All,
            UnusedScope::Public,
            UnusedScope::Private,
            UnusedScope::Function,
            UnusedScope::Struct,
        ] {
            let original = UnusedKey {
                scope,
                max_results: 100,
            };
            let bytes = to_allocvec(&original).expect("serialize failed");
            let decoded: UnusedKey = from_bytes(&bytes).expect("deserialize failed");
            assert_eq!(decoded, original, "scope={scope:?}");
        }
    }

    #[test]
    fn is_node_unused_key_roundtrip() {
        let original = IsNodeUnusedKey {
            node_id: NodeId::new(99, 3),
            scope: UnusedScope::Function,
        };
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: IsNodeUnusedKey = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn unused_value_roundtrip() {
        // UnusedValue = Arc<Vec<NodeId>>
        let original: UnusedValue = Arc::new(vec![
            NodeId::new(1, 1),
            NodeId::new(2, 1),
            NodeId::new(3, 1),
        ]);
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: UnusedValue = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded.as_ref(), original.as_ref());
    }

    #[test]
    fn is_node_unused_value_roundtrip() {
        // IsNodeUnusedValue = bool
        for val in [true, false] {
            let bytes = to_allocvec(&val).expect("serialize failed");
            let decoded: IsNodeUnusedValue = from_bytes(&bytes).expect("deserialize failed");
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
    use std::path::Path;

    fn alloc_fn_with_vis(graph: &mut CodeGraph, name: &str, vis: Option<&str>) -> NodeId {
        let file = graph.files_mut().register(Path::new("main.rs")).unwrap();
        let name_id = graph.strings_mut().intern(name).unwrap();
        let mut entry =
            NodeEntry::new(NodeKind::Function, name_id, file).with_qualified_name(name_id);
        if let Some(v) = vis {
            let vid = graph.strings_mut().intern(v).unwrap();
            entry = entry.with_visibility(vid);
        }
        graph.nodes_mut().alloc(entry).unwrap()
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
        db.register::<EntryPointsQuery>();
        db.register::<ReachableFromEntryPointsQuery>();
        db.register::<UnusedQuery>();
        db.register::<IsNodeUnusedQuery>();
        db
    }

    #[test]
    fn entry_points_include_main_test_public_export() {
        let mut graph = CodeGraph::new();
        let main = alloc_fn_with_vis(&mut graph, "main", None);
        let pub_fn = alloc_fn_with_vis(&mut graph, "exported_api", Some("public"));
        let test_fn = alloc_fn_with_vis(&mut graph, "test_thing", None);
        let private_fn = alloc_fn_with_vis(&mut graph, "helper", None);

        let db = build_db(graph);
        let entry_points = db.get::<EntryPointsQuery>(&());
        assert!(entry_points.contains(&main));
        assert!(entry_points.contains(&pub_fn));
        assert!(entry_points.contains(&test_fn));
        assert!(!entry_points.contains(&private_fn));
    }

    #[test]
    fn unused_query_reports_unreachable_private_symbols() {
        let mut graph = CodeGraph::new();
        let main = alloc_fn_with_vis(&mut graph, "main", None);
        let used = alloc_fn_with_vis(&mut graph, "used_helper", None);
        let unused = alloc_fn_with_vis(&mut graph, "unused_helper", None);
        add_call(&mut graph, main, used);

        let db = build_db(graph);
        let key = UnusedKey {
            scope: UnusedScope::All,
            max_results: 100,
        };
        let result = db.get::<UnusedQuery>(&key);
        assert_eq!(result.as_ref(), &vec![unused]);
    }

    #[test]
    fn is_node_unused_honours_scope_filter() {
        let mut graph = CodeGraph::new();
        let main = alloc_fn_with_vis(&mut graph, "main", None);
        let unused = alloc_fn_with_vis(&mut graph, "ghost", None);
        let _ = main;

        let db = build_db(graph);
        let all_key = IsNodeUnusedKey {
            node_id: unused,
            scope: UnusedScope::All,
        };
        assert!(db.get::<IsNodeUnusedQuery>(&all_key));
        let struct_key = IsNodeUnusedKey {
            node_id: unused,
            scope: UnusedScope::Struct,
        };
        assert!(!db.get::<IsNodeUnusedQuery>(&struct_key));
    }

    #[test]
    fn reachable_set_follows_reachability_edges() {
        let mut graph = CodeGraph::new();
        let main = alloc_fn_with_vis(&mut graph, "main", None);
        let mid = alloc_fn_with_vis(&mut graph, "mid", None);
        let deep = alloc_fn_with_vis(&mut graph, "deep", None);
        add_call(&mut graph, main, mid);
        add_call(&mut graph, mid, deep);

        let db = build_db(graph);
        let reachable = db.get::<ReachableFromEntryPointsQuery>(&());
        assert!(reachable.contains(&main));
        assert!(reachable.contains(&mid));
        assert!(reachable.contains(&deep));
    }

    /// Gate 0d iter-2 regression: `EntryPointsQuery` and
    /// `UnusedQuery` MUST skip Phase 4c-prime unified losers. We
    /// construct the end-state `merge_node_into` leaves in the arena
    /// (name == INVALID + content-addressable fields cleared)
    /// directly, because the merge helper is `pub(crate)` inside
    /// sqry-core and not reachable from sqry-db tests.
    #[test]
    fn entry_points_and_unused_exclude_unified_losers() {
        use sqry_core::graph::unified::string::StringId;

        let mut graph = CodeGraph::new();
        let main = alloc_fn_with_vis(&mut graph, "main", None);
        let _ = main;

        // Build a winner (public) + loser pair sharing a qualified
        // name in different files.
        let file_a = graph.files_mut().register(Path::new("a.rs")).unwrap();
        let file_b = graph.files_mut().register(Path::new("b.rs")).unwrap();
        let name_id = graph.strings_mut().intern("shared").unwrap();
        let qn_id = graph.strings_mut().intern("mod::shared").unwrap();
        let pub_id = graph.strings_mut().intern("public").unwrap();

        let (winner, loser): (NodeId, NodeId) = {
            let arena = graph.nodes_mut();
            let mut w = NodeEntry::new(NodeKind::Function, name_id, file_a).with_visibility(pub_id);
            w.qualified_name = Some(qn_id);
            let w_id = arena.alloc(w).unwrap();
            let mut l = NodeEntry::new(NodeKind::Function, name_id, file_b).with_visibility(pub_id);
            l.qualified_name = Some(qn_id);
            let l_id = arena.alloc(l).unwrap();
            (w_id, l_id)
        };
        graph.files_mut().record_node(file_a, winner);
        graph.files_mut().record_node(file_b, loser);

        // Simulate the end-state `merge_node_into` leaves in the
        // arena for a loser. This matches the clearing contract in
        // `sqry-core/src/graph/unified/build/unification.rs`.
        {
            let arena = graph.nodes_mut();
            let l_mut = arena.get_mut(loser).expect("loser present");
            l_mut.name = StringId::INVALID;
            l_mut.qualified_name = None;
            l_mut.signature = None;
            l_mut.body_hash = None;
            l_mut.doc = None;
            l_mut.visibility = None;
        }
        graph.rebuild_indices();

        let db = build_db(graph);
        let entry_points = db.get::<EntryPointsQuery>(&());
        assert!(
            !entry_points.contains(&loser),
            "EntryPointsQuery leaked unified loser"
        );
        // Note: the winner lost its visibility metadata when we
        // simulated unification on the loser (the winner is
        // distinct), so the winner still carries "public" and is
        // reported as an entry point.
        assert!(
            entry_points.contains(&winner),
            "EntryPointsQuery must still see the winner"
        );

        let unused_key = UnusedKey {
            scope: UnusedScope::All,
            max_results: 100,
        };
        let unused = db.get::<UnusedQuery>(&unused_key);
        assert!(!unused.contains(&loser), "UnusedQuery leaked unified loser");
    }
}
