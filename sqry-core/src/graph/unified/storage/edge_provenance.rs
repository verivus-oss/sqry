//! Dense, slot-aligned edge provenance store for the Phase 1 fact layer.
//!
//! Companion to [`crate::graph::unified::storage::node_provenance::NodeProvenanceStore`],
//! but for edges. See that module for the density rationale and the Phase 1
//! scope.
//!
//! # Layout divergence from node provenance
//!
//! The Phase 1 sub-pack design initially specified
//! `Vec<Option<(u64, EdgeProvenance)>>` symmetric with the node store, using
//! an embedded `u64` generation to guard against ABA on slot reuse. The real
//! [`EdgeId`] type has no generation field:
//!
//! > Unlike `NodeId`, `EdgeId` does not use generational indices because
//! > edges are logically identified by their endpoints and kind. When an
//! > edge is removed and recreated, it gets a new `EdgeId` but the same
//! > logical identity.
//! > — [`crate::graph::unified::edge::id`] module docs
//!
//! The `CsrGraph` rebuilds its edge arena wholesale rather than freeing
//! individual edges into a generational slot pool, so there is no stale-handle
//! failure mode to guard against. The store therefore uses the simpler
//! `Vec<Option<EdgeProvenance>>` shape, dropping the generation tuple. This
//! saves ~8 bytes per slot (a live slot is roughly 16 bytes instead of 24)
//! and still exceeds the Phase 1 ≤24 bytes/slot serialized ceiling with
//! comfortable headroom.
//!
//! The density argument (Gemini iter1: sparse `FxHashMap` at 1M edges costs
//! ~40–60 MB vs dense `Vec` at ~16 MB) still applies.
//!
//! # Phase 1 scope
//!
//! As with `P1U03a`, this unit introduces the store as a standalone, unit-tested
//! module. Wiring into `CsrGraph` and `CodeGraph` lands later (P1U08 and P1U09).
//!
//! Spec: `docs/development/generational-analysis-platform/phase1-fact-layer/01_SPEC.md#fr4-edge-provenance-dense-slot-aligned`
//! Design: `docs/development/generational-analysis-platform/phase1-fact-layer/02_DESIGN.md`
//! Plan: `docs/development/generational-analysis-platform/phase1-fact-layer/03_IMPLEMENTATION_PLAN.md` P1U04

use serde::{Deserialize, Serialize};

use crate::graph::unified::edge::id::EdgeId;

/// Per-edge provenance carried in the dense fact layer.
///
/// Only the first/last seen epochs are tracked in Phase 1. Edges do not
/// carry a content hash because their identity (source, target, kind) is
/// already structural — change detection is covered by the endpoint nodes'
/// content hashes plus the edge's presence/absence across builds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EdgeProvenance {
    /// First epoch at which this edge was observed.
    pub first_seen_epoch: u64,
    /// Most recent epoch at which this edge was observed.
    pub last_seen_epoch: u64,
}

impl EdgeProvenance {
    /// Constructs a fresh provenance record with `first_seen == last_seen == epoch`.
    #[must_use]
    pub fn fresh(epoch: u64) -> Self {
        Self {
            first_seen_epoch: epoch,
            last_seen_epoch: epoch,
        }
    }

    /// Advances `last_seen_epoch` to `epoch` if `epoch` is newer, leaving
    /// `first_seen_epoch` unchanged. No-op if the stored epoch is already at
    /// or beyond `epoch`.
    pub fn touch(&mut self, epoch: u64) {
        if epoch > self.last_seen_epoch {
            self.last_seen_epoch = epoch;
        }
    }
}

/// Dense, slot-aligned store for edge provenance.
///
/// Indexed by [`EdgeId::index`]. No embedded generation: see the module
/// docs for why the `EdgeId` type does not require one.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EdgeProvenanceStore {
    /// One entry per edge slot. `None` means the slot is vacant (never
    /// populated, or cleared on removal). `Some(provenance)` binds the
    /// provenance to the edge currently occupying that slot.
    slots: Vec<Option<EdgeProvenance>>,
}

impl EdgeProvenanceStore {
    /// Constructs an empty store with zero slots.
    #[must_use]
    pub fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Returns the number of indexable slots the store currently covers.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Returns the number of occupied slots (`Some` entries).
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Returns `true` if the store has no occupied slots.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Extends or truncates the store to cover exactly `edge_slot_count`
    /// slots, filling any newly added slots with `None`.
    ///
    /// Intended for use by the CSR wiring in P1U08, which will call this
    /// after the edge arena grows or after a wholesale rebuild.
    pub fn resize_to(&mut self, edge_slot_count: usize) {
        self.slots.resize(edge_slot_count, None);
    }

    /// Inserts provenance for `id`, extending the slot vector if `id.index()`
    /// lies beyond the current `slot_count()`.
    ///
    /// Panics only if `id.is_invalid()`: the invalid sentinel is never a
    /// valid storage slot.
    ///
    /// # Panics
    ///
    /// Panics if `id` is the invalid `EdgeId` sentinel.
    pub fn insert(&mut self, id: EdgeId, provenance: EdgeProvenance) {
        assert!(
            id.is_valid(),
            "EdgeProvenanceStore::insert called with EdgeId::INVALID"
        );
        let index = id.as_usize();
        if index >= self.slots.len() {
            self.slots.resize(index + 1, None);
        }
        self.slots[index] = Some(provenance);
    }

    /// Returns the provenance for `id` if the slot is populated.
    ///
    /// Returns `None` for out-of-range indices, vacant slots, and the
    /// invalid sentinel.
    #[must_use]
    pub fn lookup(&self, id: EdgeId) -> Option<&EdgeProvenance> {
        if id.is_invalid() {
            return None;
        }
        self.slots.get(id.as_usize())?.as_ref()
    }

    /// Clears the provenance at `index`, dropping the stored record. No-op
    /// if `index` is out of range or the slot is already vacant.
    pub fn clear_slot(&mut self, index: u32) {
        let i = index as usize;
        if let Some(entry) = self.slots.get_mut(i) {
            *entry = None;
        }
    }
}

impl crate::graph::unified::memory::GraphMemorySize for EdgeProvenanceStore {
    fn heap_bytes(&self) -> usize {
        self.slots.capacity() * std::mem::size_of::<Option<EdgeProvenance>>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prov(epoch: u64) -> EdgeProvenance {
        EdgeProvenance::fresh(epoch)
    }

    #[test]
    fn new_store_is_empty_and_has_zero_slots() {
        let store = EdgeProvenanceStore::new();
        assert_eq!(store.slot_count(), 0);
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn resize_extends_with_none() {
        let mut store = EdgeProvenanceStore::new();
        store.resize_to(16);
        assert_eq!(store.slot_count(), 16);
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn insert_lookup_round_trip() {
        let mut store = EdgeProvenanceStore::new();
        let id = EdgeId::new(7);
        store.insert(id, prov(100));

        let got = store.lookup(id).expect("provenance present");
        assert_eq!(got.first_seen_epoch, 100);
        assert_eq!(got.last_seen_epoch, 100);
        assert_eq!(store.len(), 1);
        assert!(store.slot_count() >= 8);
    }

    #[test]
    fn insert_auto_extends_slots() {
        let mut store = EdgeProvenanceStore::new();
        store.insert(EdgeId::new(0), prov(1));
        store.insert(EdgeId::new(5), prov(2));
        store.insert(EdgeId::new(3), prov(3));

        assert_eq!(store.slot_count(), 6);
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn lookup_out_of_range_returns_none() {
        let store = EdgeProvenanceStore::new();
        assert!(store.lookup(EdgeId::new(999)).is_none());
    }

    #[test]
    fn lookup_vacant_slot_returns_none() {
        let mut store = EdgeProvenanceStore::new();
        store.resize_to(10);
        assert!(store.lookup(EdgeId::new(5)).is_none());
    }

    #[test]
    fn lookup_invalid_sentinel_returns_none() {
        let mut store = EdgeProvenanceStore::new();
        store.resize_to(10);
        assert!(store.lookup(EdgeId::INVALID).is_none());
    }

    #[test]
    #[should_panic(expected = "EdgeId::INVALID")]
    fn insert_invalid_sentinel_panics() {
        let mut store = EdgeProvenanceStore::new();
        store.insert(EdgeId::INVALID, prov(1));
    }

    #[test]
    fn clear_slot_drops_provenance() {
        let mut store = EdgeProvenanceStore::new();
        let id = EdgeId::new(2);
        store.insert(id, prov(100));
        assert!(store.lookup(id).is_some());

        store.clear_slot(2);
        assert!(store.lookup(id).is_none());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn clear_slot_out_of_range_is_noop() {
        let mut store = EdgeProvenanceStore::new();
        store.resize_to(4);
        store.clear_slot(99);
        assert_eq!(store.slot_count(), 4);
    }

    #[test]
    fn provenance_touch_advances_only_forward() {
        let mut p = prov(100);
        p.touch(50);
        assert_eq!(
            p.last_seen_epoch, 100,
            "stale touch must not move last_seen backwards"
        );
        p.touch(200);
        assert_eq!(p.last_seen_epoch, 200);
        assert_eq!(p.first_seen_epoch, 100, "first_seen must never move");
    }

    #[test]
    fn postcard_round_trip_preserves_dense_layout() {
        let mut store = EdgeProvenanceStore::new();
        store.resize_to(4);
        store.insert(EdgeId::new(0), prov(10));
        store.insert(EdgeId::new(2), prov(20));

        let encoded = postcard::to_allocvec(&store).expect("encode");
        let decoded: EdgeProvenanceStore = postcard::from_bytes(&encoded).expect("decode");

        assert_eq!(decoded.slot_count(), 4);
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded.lookup(EdgeId::new(0)).unwrap().first_seen_epoch, 10);
        assert_eq!(decoded.lookup(EdgeId::new(2)).unwrap().last_seen_epoch, 20);
        assert!(decoded.lookup(EdgeId::new(1)).is_none());
    }

    #[test]
    fn default_is_new() {
        let a = EdgeProvenanceStore::default();
        let b = EdgeProvenanceStore::new();
        assert_eq!(a.slot_count(), b.slot_count());
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn memory_budget_24_bytes_per_slot_serialized_at_1k_edges() {
        // P1U04 acceptance: dense edge store stays within an absolute budget
        // of <=24 bytes/slot serialized at 1k fully-dense edges.
        let mut store = EdgeProvenanceStore::new();
        for i in 0..1000u32 {
            store.insert(EdgeId::new(i), prov(u64::from(i) + 1));
        }
        let encoded = postcard::to_allocvec(&store).expect("encode");
        assert!(
            encoded.len() <= 24 * 1000,
            "dense edge store budget exceeded: {} bytes > {} bytes ceiling",
            encoded.len(),
            24 * 1000
        );
    }
}
