//! `CallsitePromiscuousQuery` — derived set of caller nodes whose
//! [`NodeFlags::CALLSITE_PROMISCUOUS`] bit is set.
//!
//! Phase A (C indirect-call precision) U13 — DESIGN §9.3, §11.1.
//!
//! # Invalidation
//!
//! `TRACKS_METADATA_REVISION = true`: the `CALLSITE_PROMISCUOUS` bit
//! lives on the [`NodeMetadataStore`] (Tier-3, the metadata-revision
//! counter). Per DESIGN §9.3, this query reads through the same
//! metadata store as [`super::AddressTakenQuery`] and inherits the
//! same Tier-3 invalidation.
//!
//! `TRACKS_EDGE_REVISION = true`: set conservatively to parallel HEAD's
//! [`EntryPointsQuery`] precedent at
//! `sqry-db/src/queries/unused.rs:67-72` (both flags true). The Phase
//! A pass5b resolver applies `CALLSITE_PROMISCUOUS` marks in the same
//! pass that materializes new `Calls` edges (cap-exceeded callers
//! per DESIGN §4 / §5.2), so over-invalidation on edge-revision
//! bumps is preferred to a subtle under-invalidation. See DAG
//! `[units.U13_DB_QUERIES]` `constraints` ("over-invalidation is
//! preferred to under-invalidation").
//!
//! # Output
//!
//! `Arc<Vec<NodeId>>` sorted by `(NodeId::index, NodeId::generation)`
//! for deterministic output across runs. Matches the discipline in
//! `unused.rs:225`.

use std::sync::Arc;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::id::NodeId;

use crate::QueryDb;
use crate::dependency::record_file_dep;
use crate::query::DerivedQuery;

/// Returns the sorted set of node IDs that carry the
/// [`NodeFlags::CALLSITE_PROMISCUOUS`] marker flag.
///
/// See module docs for invalidation semantics and the rationale for
/// setting both `TRACKS_EDGE_REVISION` and `TRACKS_METADATA_REVISION`
/// to `true`.
pub struct CallsitePromiscuousQuery;

impl DerivedQuery for CallsitePromiscuousQuery {
    type Key = ();
    type Value = Arc<Vec<NodeId>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::CALLSITE_PROMISCUOUS;
    const TRACKS_EDGE_REVISION: bool = true;
    const TRACKS_METADATA_REVISION: bool = true;

    fn execute(_key: &(), _db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<Vec<NodeId>> {
        // Record file deps for cold-start correctness, paralleling
        // `EntryPointsQuery` (unused.rs:75-77).
        for (fid, _) in snapshot.file_segments().iter() {
            record_file_dep(fid);
        }

        let metadata = snapshot.macro_metadata();
        let mut out: Vec<NodeId> = Vec::new();
        for (node_id, entry) in snapshot.nodes().iter() {
            // Skip Phase 4c-prime unified losers defensively — see
            // `AddressTakenQuery::execute` rationale.
            if entry.is_unified_loser() {
                continue;
            }
            if metadata.is_callsite_promiscuous(node_id) {
                out.push(node_id);
            }
        }
        // Deterministic order — matches `unused.rs:225`.
        out.sort_unstable_by_key(|id| (id.index(), id.generation()));
        Arc::new(out)
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
        let file = graph.files_mut().register(Path::new("main.c")).unwrap();
        let name_id = graph.strings_mut().intern(name).unwrap();
        let entry = NodeEntry::new(NodeKind::Function, name_id, file).with_qualified_name(name_id);
        graph.nodes_mut().alloc(entry).unwrap()
    }

    fn build_db(graph: CodeGraph) -> QueryDb {
        let snapshot = Arc::new(graph.snapshot());
        QueryDb::new(snapshot, QueryDbConfig::default())
    }

    #[test]
    fn callsite_promiscuous_query_returns_marked_set() {
        let mut graph = CodeGraph::new();
        let _a = alloc_fn(&mut graph, "alpha");
        let b = alloc_fn(&mut graph, "beta");
        let _c = alloc_fn(&mut graph, "gamma");
        graph.macro_metadata_mut().mark_callsite_promiscuous(b);

        let db = build_db(graph);
        let result = db.get::<CallsitePromiscuousQuery>(&());
        assert_eq!(result.as_ref(), &vec![b]);
    }

    #[test]
    fn callsite_promiscuous_query_cache_invalidates_on_metadata_change() {
        // Build initial state — 3 callers, 1 promiscuous.
        let mut graph = CodeGraph::new();
        let _a = alloc_fn(&mut graph, "alpha");
        let b = alloc_fn(&mut graph, "beta");
        let _c = alloc_fn(&mut graph, "gamma");
        graph.macro_metadata_mut().mark_callsite_promiscuous(b);

        let mut db = build_db(graph);
        let first = db.get::<CallsitePromiscuousQuery>(&());
        assert_eq!(first.as_ref(), &vec![b]);
        let baseline = db.metrics();

        // Second invocation must be a cache hit.
        let cached = db.get::<CallsitePromiscuousQuery>(&());
        assert_eq!(cached.as_ref(), &vec![b]);
        let after_hit = db.metrics();
        assert_eq!(
            after_hit.cache_hits,
            baseline.cache_hits + 1,
            "second CallsitePromiscuousQuery::get must be a cache hit"
        );
        assert_eq!(
            after_hit.cache_misses, baseline.cache_misses,
            "second CallsitePromiscuousQuery::get must not be a miss"
        );

        let original_snapshot = db.snapshot_arc();

        // Build a fresh CodeGraph adding a 4th promiscuous caller.
        let mut graph2 = CodeGraph::new();
        let _a2 = alloc_fn(&mut graph2, "alpha");
        let b2 = alloc_fn(&mut graph2, "beta");
        let _c2 = alloc_fn(&mut graph2, "gamma");
        let d2 = alloc_fn(&mut graph2, "delta");
        graph2.macro_metadata_mut().mark_callsite_promiscuous(b2);
        graph2.macro_metadata_mut().mark_callsite_promiscuous(d2);

        let new_snapshot = Arc::new(graph2.snapshot());
        assert!(!Arc::ptr_eq(&original_snapshot, &new_snapshot));
        db.set_snapshot(new_snapshot);
        db.bump_metadata_revision();

        let invalidated = db.get::<CallsitePromiscuousQuery>(&());
        assert_eq!(invalidated.as_ref(), &vec![b2, d2]);

        let after_invalidation = db.metrics();
        assert_eq!(
            after_invalidation.cache_misses,
            after_hit.cache_misses + 1,
            "metadata-revision bump must force a recompute"
        );
    }

    /// AC5 (DAG): `QueryDb::new` auto-registers both Phase A queries.
    /// Exercising the call path also confirms there is no
    /// "query not registered" panic.
    #[test]
    fn query_db_auto_registers_address_taken_and_callsite_promiscuous() {
        use crate::queries::AddressTakenQuery;

        let graph = CodeGraph::new();
        let db = build_db(graph);
        // Both must dispatch through the registry without panic.
        let at = db.get::<AddressTakenQuery>(&());
        let cp = db.get::<CallsitePromiscuousQuery>(&());
        assert!(at.is_empty());
        assert!(cp.is_empty());
    }
}
