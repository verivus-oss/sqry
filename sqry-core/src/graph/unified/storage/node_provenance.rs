//! Dense, slot-aligned node provenance store for the Phase 1 fact layer.
//!
//! This module introduces [`NodeProvenanceStore`], a dense attribution store
//! for node-level provenance (first/last seen epoch, content hash) that lives
//! alongside [`crate::graph::unified::storage::arena::NodeArena`] without
//! touching the sparse [`crate::graph::unified::storage::metadata::NodeMetadataStore`],
//! which stays reserved for rare metadata (macro boundaries, classpath origin).
//!
//! # Density model
//!
//! After the first build, essentially every node carries provenance, so the
//! keyspace density approaches 100%. A sparse `FxHashMap<NodeId, _>` would
//! pay hash-map overhead per node; a dense slot-aligned vector pays a single
//! `Option<(u64, NodeProvenance)>` per arena slot and exploits postcard's
//! compact encoding of `None` variants (one byte per vacant slot).
//!
//! # Embedded generation
//!
//! `NodeArena` does not expose a parallel `Vec<u64>` of generations — each
//! slot embeds its own generation via `Slot<NodeEntry>`. Mirroring generations
//! in a parallel vector on this store would introduce a second synchronization
//! point that must be maintained in lockstep with every arena mutation. To
//! eliminate that hazard, the generation is embedded in the tuple:
//! `Vec<Option<(u64, NodeProvenance)>>`. A lookup validates the caller-provided
//! `NodeId::generation()` against the stored generation, rejecting stale
//! handles even if the slot has since been reused by a new node. This is the
//! exact safety property the dense store exists to provide.
//!
//! # Phase 1 scope
//!
//! This unit (`P1U03a`) introduces the store as a standalone, unit-tested type.
//! Wiring into `NodeArena::alloc` / `NodeArena::remove` / growth paths is a
//! separate follow-on unit (`P1U03b`). Compaction hooks against
//! `Slot::new_vacant` are deferred until Step 15 lands; a dedicated
//! `#[ignore]`d integration test in `sqry-core/tests/phase1_fact_layer_build.rs`
//! records the gap and its reason when the build-pipeline unit (P1U08) lands.
//!
//! # MVCC
//!
//! The store is designed to be held as `Arc<NodeProvenanceStore>` inside
//! `CodeGraph` so that snapshot cloning is O(1). Writes go through
//! `Arc::make_mut`, which incurs a one-time O(n) memcpy on the first write
//! per epoch. This is acceptable in single-writer build mode where writes are
//! batched once per build.
//!
//! Spec: `docs/development/generational-analysis-platform/phase1-fact-layer/01_SPEC.md#fr3-node-provenance-dense-slot-aligned`
//! Design: `docs/development/generational-analysis-platform/phase1-fact-layer/02_DESIGN.md`
//! Plan: `docs/development/generational-analysis-platform/phase1-fact-layer/03_IMPLEMENTATION_PLAN.md` P1U03

use serde::{Deserialize, Serialize};

use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::storage::arena::NodeArena;

/// Per-node provenance carried in the dense fact layer.
///
/// Attributes a node to a specific indexing epoch and pins a content hash of
/// the node's body-hash-bearing text region. The three fields are sufficient
/// for Phase 1 temporal and change-detection queries; richer attribution
/// (e.g. commit hashes, contributing files) is scoped to Phase 6.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeProvenance {
    /// First epoch at which this node was observed.
    pub first_seen_epoch: u64,
    /// Most recent epoch at which this node was observed.
    pub last_seen_epoch: u64,
    /// SHA-256 of the node's body-hash-bearing text region.
    pub content_hash: [u8; 32],
}

impl NodeProvenance {
    /// Constructs a fresh provenance record with `first_seen == last_seen == epoch`.
    #[must_use]
    pub fn fresh(epoch: u64, content_hash: [u8; 32]) -> Self {
        Self {
            first_seen_epoch: epoch,
            last_seen_epoch: epoch,
            content_hash,
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

/// Dense, slot-aligned store for node provenance with embedded generation.
///
/// The store is indexed by [`NodeId::index`]; [`NodeId::generation`] is
/// validated against the stored generation at lookup time to reject stale
/// handles. See the module docs for the density and MVCC rationale.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NodeProvenanceStore {
    /// One entry per arena slot.
    ///
    /// `None` means the slot is free (never populated, or cleared after
    /// `NodeArena::remove`). `Some((generation, provenance))` binds the
    /// provenance to the exact `NodeId { index, generation }` that was live
    /// when the record was inserted.
    slots: Vec<Option<(u64, NodeProvenance)>>,
}

impl NodeProvenanceStore {
    /// Constructs an empty store with zero slots. Callers should follow with
    /// [`NodeProvenanceStore::resize_to`] or
    /// [`NodeProvenanceStore::empty_for`] to size it against a `NodeArena`.
    #[must_use]
    pub fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Constructs an empty store sized to the given arena's slot count.
    ///
    /// Every slot is `None`. Used on legacy V7 upconvert and on fresh builds
    /// before Pass 1 stamps provenance on new nodes.
    #[must_use]
    pub fn empty_for(arena: &NodeArena) -> Self {
        Self {
            slots: vec![None; arena.slot_count()],
        }
    }

    /// Returns the number of indexable slots the store currently covers.
    ///
    /// This equals the arena's `slot_count()` after any `resize_to` / `empty_for`
    /// call and is independent of the number of occupied slots.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Returns the number of occupied slots (`Some` entries).
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Returns `true` if the store has no occupied slots. Note that a
    /// freshly-resized store with all `None` entries is `is_empty()` even
    /// though its `slot_count()` may be large.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Extends or truncates the store to cover exactly `arena_slot_count`
    /// slots, filling any newly added slots with `None`.
    ///
    /// Truncation drops any stored provenance beyond `arena_slot_count`
    /// without further notice — callers must ensure no live `NodeId` points
    /// into the truncated range. This is intended for use by `P1U03b` (arena
    /// wiring), which will call `resize_to(arena.slot_count())` after growth.
    pub fn resize_to(&mut self, arena_slot_count: usize) {
        self.slots.resize(arena_slot_count, None);
    }

    /// Inserts provenance for `id`, extending the slot vector if `id.index()`
    /// lies beyond the current `slot_count()`.
    ///
    /// The stored entry is `Some((id.generation(), provenance))`. Subsequent
    /// lookups succeed only when the caller's `NodeId::generation()` matches
    /// the stored generation.
    pub fn insert(&mut self, id: NodeId, provenance: NodeProvenance) {
        let index = id.index() as usize;
        if index >= self.slots.len() {
            self.slots.resize(index + 1, None);
        }
        self.slots[index] = Some((id.generation(), provenance));
    }

    /// Returns the provenance for `id` if and only if the stored generation
    /// matches `id.generation()`.
    ///
    /// Returns `None` when:
    /// - `id.index()` is out of range (slot never created),
    /// - the slot is vacant (`None`),
    /// - or the slot is occupied but with a different generation (stale `NodeId`).
    #[must_use]
    pub fn lookup(&self, id: NodeId) -> Option<&NodeProvenance> {
        let index = id.index() as usize;
        let (stored_generation, provenance) = self.slots.get(index)?.as_ref()?;
        if *stored_generation == id.generation() {
            Some(provenance)
        } else {
            None
        }
    }

    /// Clears the provenance at `index`, dropping both the stored generation
    /// and the provenance record. No-op if `index` is out of range or the
    /// slot is already vacant.
    ///
    /// Intended for use by the arena `remove` hook in `P1U03b`.
    pub fn clear_slot(&mut self, index: u32) {
        let i = index as usize;
        if let Some(entry) = self.slots.get_mut(i) {
            *entry = None;
        }
    }

    /// Yields the `NodeId` every occupied slot is bound to, reconstructed from
    /// the slot's `(index, stored generation)` pair.
    ///
    /// Vacant slots are silently skipped. Callers must not rely on any
    /// particular ordering beyond "ascending by slot index", which is a direct
    /// consequence of the dense `Vec`-backed layout. The iterator borrows
    /// from `self`.
    ///
    /// This is the read side of the Gate 0b `NodeIdBearing` coverage matrix
    /// (A2 §K row K.A10): the tombstone-residue invariant (§F) iterates here
    /// to confirm that no tombstoned `NodeId` outlives a `finalize()` call.
    ///
    /// # Panics
    ///
    /// Panics if the provenance slot index exceeds `u32::MAX`, which would
    /// violate the shared `NodeId` slot-index representation.
    pub fn iter_node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.as_ref().map(|(generation, _provenance)| {
                NodeId::new(
                    u32::try_from(index).expect("slot index fits u32"),
                    *generation,
                )
            })
        })
    }

    /// Applies `keep` to every occupied slot's reconstructed `NodeId`,
    /// clearing slots whose `NodeId` fails the predicate.
    ///
    /// This is the mutation side of the Gate 0b `NodeIdBearing` coverage
    /// matrix (A2 §K row K.A10). It does **not** shrink `self.slots`; dense
    /// slot alignment with the parent `NodeArena` is a load-bearing invariant
    /// of this store (each slot index corresponds 1:1 to an arena slot index),
    /// so rejected slots are set to `None` rather than removed.
    ///
    /// `#[allow(dead_code)]` mirrors the `NodeIdBearing` trait itself: Gate 0b
    /// lands the scaffolding and unit tests, Gate 0c adds the production
    /// call site in `RebuildGraph::finalize()`.
    #[allow(dead_code)]
    pub(crate) fn retain_by_node(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let Some((generation, _provenance)) = slot else {
                continue;
            };
            let id = NodeId::new(
                u32::try_from(index).expect("slot index fits u32"),
                *generation,
            );
            if !keep(id) {
                *slot = None;
            }
        }
    }
}

impl crate::graph::unified::memory::GraphMemorySize for NodeProvenanceStore {
    fn heap_bytes(&self) -> usize {
        self.slots.capacity() * std::mem::size_of::<Option<(u64, NodeProvenance)>>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::node::id::NodeId;

    fn prov(epoch: u64, byte: u8) -> NodeProvenance {
        NodeProvenance::fresh(epoch, [byte; 32])
    }

    #[test]
    fn new_store_is_empty_and_has_zero_slots() {
        let store = NodeProvenanceStore::new();
        assert_eq!(store.slot_count(), 0);
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn resize_extends_with_none() {
        let mut store = NodeProvenanceStore::new();
        store.resize_to(16);
        assert_eq!(store.slot_count(), 16);
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
    }

    #[test]
    fn insert_lookup_round_trip() {
        let mut store = NodeProvenanceStore::new();
        let id = NodeId::new(7, 3);
        let p = prov(100, 0xAB);
        store.insert(id, p);

        let got = store.lookup(id).expect("provenance present");
        assert_eq!(got.first_seen_epoch, 100);
        assert_eq!(got.last_seen_epoch, 100);
        assert_eq!(got.content_hash, [0xAB; 32]);
        assert_eq!(store.len(), 1);
        assert!(store.slot_count() >= 8);
    }

    #[test]
    fn insert_auto_extends_slots() {
        let mut store = NodeProvenanceStore::new();
        store.insert(NodeId::new(0, 1), prov(1, 0));
        store.insert(NodeId::new(5, 1), prov(2, 0));
        store.insert(NodeId::new(3, 1), prov(3, 0));

        assert_eq!(store.slot_count(), 6);
        assert_eq!(store.len(), 3);
    }

    #[test]
    fn stale_generation_returns_none_even_when_slot_is_populated() {
        let mut store = NodeProvenanceStore::new();
        let old_id = NodeId::new(4, 1);
        let new_id = NodeId::new(4, 2);
        store.insert(new_id, prov(50, 0xCC));

        assert!(store.lookup(new_id).is_some());
        assert!(store.lookup(old_id).is_none());
    }

    #[test]
    fn lookup_out_of_range_returns_none() {
        let store = NodeProvenanceStore::new();
        assert!(store.lookup(NodeId::new(999, 1)).is_none());
    }

    #[test]
    fn lookup_vacant_slot_returns_none() {
        let mut store = NodeProvenanceStore::new();
        store.resize_to(10);
        assert!(store.lookup(NodeId::new(5, 1)).is_none());
    }

    #[test]
    fn clear_slot_drops_provenance_and_generation() {
        let mut store = NodeProvenanceStore::new();
        let id = NodeId::new(2, 5);
        store.insert(id, prov(100, 0x11));
        assert!(store.lookup(id).is_some());

        store.clear_slot(2);
        assert!(store.lookup(id).is_none());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn clear_slot_out_of_range_is_noop() {
        let mut store = NodeProvenanceStore::new();
        store.resize_to(4);
        store.clear_slot(99);
        assert_eq!(store.slot_count(), 4);
    }

    #[test]
    fn realloc_into_recycled_slot_does_not_leak_prior_provenance() {
        let mut store = NodeProvenanceStore::new();
        let first = NodeId::new(3, 1);
        store.insert(first, prov(10, 0xAA));

        // Simulate NodeArena freeing the slot, bumping its generation, and
        // allocating a new node into the same slot.
        store.clear_slot(3);
        let second = NodeId::new(3, 2);
        store.insert(second, prov(20, 0xBB));

        assert!(
            store.lookup(first).is_none(),
            "stale first NodeId must not see any provenance"
        );
        let got = store
            .lookup(second)
            .expect("second NodeId must see its own provenance");
        assert_eq!(got.first_seen_epoch, 20);
        assert_eq!(got.content_hash, [0xBB; 32]);
    }

    #[test]
    fn provenance_touch_advances_only_forward() {
        let mut p = prov(100, 0);
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
        let mut store = NodeProvenanceStore::new();
        store.resize_to(4);
        store.insert(NodeId::new(0, 1), prov(10, 0x01));
        store.insert(NodeId::new(2, 4), prov(20, 0x02));

        let encoded = postcard::to_allocvec(&store).expect("encode");
        let decoded: NodeProvenanceStore = postcard::from_bytes(&encoded).expect("decode");

        assert_eq!(decoded.slot_count(), 4);
        assert_eq!(decoded.len(), 2);
        assert_eq!(
            decoded.lookup(NodeId::new(0, 1)).unwrap().first_seen_epoch,
            10
        );
        assert_eq!(
            decoded.lookup(NodeId::new(2, 4)).unwrap().content_hash,
            [0x02; 32]
        );
        // Stale generation still rejected after round-trip.
        assert!(decoded.lookup(NodeId::new(0, 9)).is_none());
    }

    #[test]
    fn empty_for_sizes_to_arena_slot_count() {
        let arena = NodeArena::new();
        let store = NodeProvenanceStore::empty_for(&arena);
        assert_eq!(store.slot_count(), arena.slot_count());
        assert!(store.is_empty());
    }

    #[test]
    fn default_is_new() {
        let a = NodeProvenanceStore::default();
        let b = NodeProvenanceStore::new();
        assert_eq!(a.slot_count(), b.slot_count());
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn memory_budget_64_bytes_per_slot_serialized_at_1k_nodes() {
        // P1U03 acceptance: dense store stays within an absolute budget of
        // <=64 bytes/slot serialized. A 1k-node fixture encodes to <=65_536
        // bytes when fully dense. At 100% density with unique provenance per
        // node, postcard encodes each live tuple as roughly the sum of the
        // generation varint, first/last epoch varints, the 32-byte hash, and
        // the `Some` discriminator plus container framing.
        let mut store = NodeProvenanceStore::new();
        for i in 0..1000u32 {
            let id = NodeId::new(i, 1);
            store.insert(id, prov(u64::from(i) + 1, (i & 0xff) as u8));
        }
        let encoded = postcard::to_allocvec(&store).expect("encode");
        assert!(
            encoded.len() <= 64 * 1000,
            "dense store budget exceeded: {} bytes > {} bytes ceiling",
            encoded.len(),
            64 * 1000
        );
    }
}
