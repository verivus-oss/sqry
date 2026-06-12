//! `AddressTakenQuery` — derived set of nodes whose
//! [`NodeFlags::ADDRESS_TAKEN`] bit is set.
//!
//! Phase A (C indirect-call precision) U13 — DESIGN §9.2, §11.1.
//!
//! # Invalidation
//!
//! `TRACKS_METADATA_REVISION = true`: the `ADDRESS_TAKEN` bit lives on the
//! [`NodeMetadataStore`] (Tier-3, the metadata-revision counter). Per
//! DESIGN §9.2, any change to the workspace that adds or removes an
//! address-taken mark bumps the metadata revision and must invalidate
//! this cache.
//!
//! `TRACKS_EDGE_REVISION = true`: set conservatively to parallel HEAD's
//! [`EntryPointsQuery`] precedent at
//! `sqry-db/src/queries/unused.rs:67-72` (which carries both flags
//! true). Phase A's `pass5b_c_indirect` resolver runs during graph
//! commit and can mark previously unmarked C functions as
//! address-taken in the same build pass that adds new `Calls` edges,
//! so over-invalidation on edge-revision bumps is preferred to a
//! subtle under-invalidation. See DAG `[units.U13_DB_QUERIES]`
//! `constraints` ("over-invalidation is preferred to
//! under-invalidation").
//!
//! # Output
//!
//! `Arc<Vec<NodeId>>` sorted by `(NodeId::index, NodeId::generation)`
//! for deterministic output across runs. Mirrors the sort
//! discipline in `unused.rs:225`.
//!
//! # Reading the flag
//!
//! Reached from a [`GraphSnapshot`] via the existing
//! `macro_metadata()` accessor at
//! `sqry-core/src/graph/unified/concurrent/graph.rs:1768`. (The store
//! is shared between macro typed-payloads and Phase A marker flags;
//! the accessor name retains the historical `macro_` prefix.)

use std::sync::Arc;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::id::NodeId;

use crate::QueryDb;
use crate::dependency::record_file_dep;
use crate::query::DerivedQuery;

/// Returns the sorted set of node IDs that carry the
/// [`NodeFlags::ADDRESS_TAKEN`] marker flag.
///
/// See module docs for invalidation semantics and the rationale for
/// setting both `TRACKS_EDGE_REVISION` and `TRACKS_METADATA_REVISION`
/// to `true`.
pub struct AddressTakenQuery;

impl DerivedQuery for AddressTakenQuery {
    type Key = ();
    type Value = Arc<Vec<NodeId>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::ADDRESS_TAKEN;
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
            // Skip Phase 4c-prime unified losers defensively. The
            // canonical post-unification mark application
            // (`apply_deferred_address_taken_marks` in IMP-011) targets
            // winners, but skipping losers here keeps the result set
            // aligned with the rest of the analysis surface.
            if entry.is_unified_loser() {
                continue;
            }
            if metadata.is_address_taken(node_id) {
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
        // `QueryDb::new` auto-registers built-ins (including
        // `AddressTakenQuery` after U13 — exercised by the dedicated
        // auto-registration test below).
        let snapshot = Arc::new(graph.snapshot());
        QueryDb::new(snapshot, QueryDbConfig::default())
    }

    #[test]
    fn address_taken_query_returns_marked_set() {
        let mut graph = CodeGraph::new();
        let _a = alloc_fn(&mut graph, "alpha");
        let b = alloc_fn(&mut graph, "beta");
        let _c = alloc_fn(&mut graph, "gamma");
        graph.macro_metadata_mut().mark_address_taken(b);

        let db = build_db(graph);
        let result = db.get::<AddressTakenQuery>(&());
        assert_eq!(result.as_ref(), &vec![b]);
    }

    #[test]
    fn address_taken_query_cache_invalidates_on_metadata_change() {
        // Build initial state — 3 functions, 1 address-taken.
        let mut graph = CodeGraph::new();
        let _a = alloc_fn(&mut graph, "alpha");
        let b = alloc_fn(&mut graph, "beta");
        let _c = alloc_fn(&mut graph, "gamma");
        graph.macro_metadata_mut().mark_address_taken(b);

        let mut db = build_db(graph);
        let first = db.get::<AddressTakenQuery>(&());
        assert_eq!(first.as_ref(), &vec![b]);
        let baseline = db.metrics();

        // A second invocation against the unchanged snapshot must hit
        // the cache.
        let cached = db.get::<AddressTakenQuery>(&());
        assert_eq!(cached.as_ref(), &vec![b]);
        let after_hit = db.metrics();
        assert_eq!(
            after_hit.cache_hits,
            baseline.cache_hits + 1,
            "second AddressTakenQuery::get must be a cache hit"
        );
        assert_eq!(
            after_hit.cache_misses, baseline.cache_misses,
            "second AddressTakenQuery::get must not be a miss"
        );

        // Snapshot the current snapshot's arc count to confirm the
        // build below produces a brand-new snapshot.
        let original_snapshot = db.snapshot_arc();

        // Build a fresh CodeGraph that adds a 4th address-taken
        // function. The QueryDb snapshot must be replaced and the
        // Tier-3 metadata-revision counter bumped so the cached result
        // is invalidated on the next call.
        let mut graph2 = CodeGraph::new();
        let _a2 = alloc_fn(&mut graph2, "alpha");
        let b2 = alloc_fn(&mut graph2, "beta");
        let _c2 = alloc_fn(&mut graph2, "gamma");
        let d2 = alloc_fn(&mut graph2, "delta");
        graph2.macro_metadata_mut().mark_address_taken(b2);
        graph2.macro_metadata_mut().mark_address_taken(d2);

        let new_snapshot = Arc::new(graph2.snapshot());
        assert!(!Arc::ptr_eq(&original_snapshot, &new_snapshot));
        db.set_snapshot(new_snapshot);
        db.bump_metadata_revision();

        let invalidated = db.get::<AddressTakenQuery>(&());
        assert_eq!(invalidated.as_ref(), &vec![b2, d2]);

        // The metadata-revision bump invalidated the cached entry
        // forcing a recomputation (a miss), not a hit.
        let after_invalidation = db.metrics();
        assert_eq!(
            after_invalidation.cache_misses,
            after_hit.cache_misses + 1,
            "metadata-revision bump must force a recompute"
        );
    }
}
