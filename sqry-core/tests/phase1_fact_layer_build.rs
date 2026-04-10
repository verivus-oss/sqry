//! Phase 1 fact-layer build integration tests.
//!
//! Covers T09 (warm-start first_seen preservation stub), T18 (generation
//! reuse safety), T21 (compaction deferral placeholder).
//!
//! See: `docs/development/generational-analysis-platform/phase1-fact-layer/05_TEST_PLAN.md`

use sqry_core::graph::unified::edge::id::EdgeId;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::storage::edge_provenance::{EdgeProvenance, EdgeProvenanceStore};
use sqry_core::graph::unified::storage::node_provenance::{NodeProvenance, NodeProvenanceStore};

/// T18: generation reuse safety — free a slot, realloc into it with a bumped
/// generation, confirm the new NodeId does not see the prior occupant's
/// provenance. This is the actual safety property the dense store exists to
/// provide.
#[test]
fn t18_generation_reuse_does_not_leak_prior_provenance() {
    let mut store = NodeProvenanceStore::new();

    // Simulate: node at index 5, generation 1 with its own provenance
    let old_id = NodeId::new(5, 1);
    store.insert(old_id, NodeProvenance::fresh(100, [0xAA; 32]));
    assert!(store.lookup(old_id).is_some());

    // Simulate: NodeArena frees slot 5, bumps generation to 2
    store.clear_slot(5);
    assert!(
        store.lookup(old_id).is_none(),
        "cleared slot must not be visible via old NodeId"
    );

    // Simulate: NodeArena allocates a new node into slot 5 with generation 2
    let new_id = NodeId::new(5, 2);
    store.insert(new_id, NodeProvenance::fresh(200, [0xBB; 32]));

    // Old NodeId must not see any provenance
    assert!(
        store.lookup(old_id).is_none(),
        "stale NodeId must not see the new occupant's provenance"
    );

    // New NodeId sees its own provenance
    let prov = store
        .lookup(new_id)
        .expect("new NodeId should find its provenance");
    assert_eq!(prov.first_seen_epoch, 200);
    assert_eq!(prov.content_hash, [0xBB; 32]);
}

/// T18 edge variant: EdgeId has no generation, so this test confirms that
/// clear_slot + re-insert at the same index replaces provenance cleanly.
#[test]
fn t18_edge_slot_reuse_replaces_cleanly() {
    let mut store = EdgeProvenanceStore::new();

    let eid = EdgeId::new(3);
    store.insert(eid, EdgeProvenance::fresh(100));
    assert_eq!(store.lookup(eid).unwrap().first_seen_epoch, 100);

    store.clear_slot(3);
    assert!(store.lookup(eid).is_none());

    store.insert(eid, EdgeProvenance::fresh(200));
    assert_eq!(store.lookup(eid).unwrap().first_seen_epoch, 200);
}

/// T21: compaction interaction — deferred placeholder.
///
/// Compaction via `Slot::new_vacant` (Step 15) is not reachable in Phase 1.
/// This ignored test records the gap so it is visible in `cargo test` output
/// and ensures the follow-up is not forgotten when compaction lands.
#[test]
#[ignore = "Phase 1 defers compaction support until Slot::new_vacant (Step 15) lands; \
            see docs/development/generational-analysis-platform/phase1-fact-layer/02_DESIGN.md \
            risk table and TODO(phase1-compaction) markers in node_provenance.rs"]
fn t21_compaction_interaction_deferred() {
    // When Step 15 lands, this test should:
    // 1. Build a NodeProvenanceStore with provenance in several slots
    // 2. Trigger arena compaction (Slot::new_vacant)
    // 3. Verify the provenance store is co-compacted or that lookups
    //    for compacted-away NodeIds return None
    // 4. Verify surviving NodeIds still return correct provenance
    panic!("compaction support not yet implemented");
}
