//! Ephemeral `Scope` model for the Phase 2 binding plane.
//!
//! `Scope` is an arena-addressable construct that represents a module /
//! function / class / namespace / trait / impl in the binding plane. Files
//! are **not** scopes — they are addressed via `Scope.file: FileId`. The
//! witness vocabulary's `ResolutionStep::EnterFileScope { file }` marks file
//! boundaries separately from `ResolutionStep::EnterScope { scope }`.
//!
//! `ScopeArena` mirrors Phase 1's `NodeArena` shape: slot-based storage with
//! embedded `u64` generations, stale-handle rejection on lookup via
//! generation comparison, and `O(1)` allocate/free through a free-list head.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::graph::unified::file::id::FileId;
use crate::graph::unified::node::id::NodeId;

/// Errors returned by mutating `ScopeArena` operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ScopeArenaError {
    /// The supplied `ScopeId` is invalid, out of range, stale (generation
    /// mismatch), or points at a vacant slot.
    #[error("stale or invalid ScopeId")]
    StaleHandle,
}

/// The kind of an arena-addressable scope.
///
/// Deliberately does **not** include a `File` variant — files are named by
/// `Scope.file: FileId`, not by a scope kind. See the witness vocabulary in
/// [`super::super::witness::step::ResolutionStep`] for the `EnterFileScope`
/// marker that tracks file boundaries in the ordered step trace.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScopeKind {
    /// Top-level module or namespace-like unit (Rust `mod`, Python `.py`
    /// file-level module, TypeScript file module).
    Module = 0,
    /// Function body scope (Rust `fn`, Python `def`, JS/TS `function`).
    Function = 1,
    /// Class body scope (Python `class`, Java `class`, TS `class`,
    /// Rust `impl`-for-struct treated as class-like).
    Class = 2,
    /// Namespace scope (C++ `namespace`, C# `namespace`).
    Namespace = 3,
    /// Rust `trait` body scope.
    Trait = 4,
    /// Rust `impl` block body scope (including `impl Type` and
    /// `impl Trait for Type`).
    Impl = 5,
}

impl ScopeKind {
    /// Returns the `u8` discriminant used in `ScopeStableId` hash input.
    #[inline]
    #[must_use]
    pub const fn discriminant(self) -> u8 {
        self as u8
    }
}

/// Generational handle into a `ScopeArena`.
///
/// Mirrors [`crate::graph::unified::node::id::NodeId`]'s shape: 32-bit slot
/// index + 64-bit generation counter. Stale handles are rejected on lookup
/// by comparing the stored generation against the handle's generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct ScopeId {
    index: u32,
    generation: u64,
}

impl ScopeId {
    /// Invalid sentinel `ScopeId` used for "no enclosing scope".
    pub const INVALID: ScopeId = ScopeId {
        index: u32::MAX,
        generation: 0,
    };

    /// Creates a new `ScopeId` with the given index and generation.
    #[inline]
    #[must_use]
    pub const fn new(index: u32, generation: u64) -> Self {
        Self { index, generation }
    }

    /// Returns the slot index.
    #[inline]
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Returns the generation counter.
    #[inline]
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns `true` if this is the `INVALID` sentinel.
    #[inline]
    #[must_use]
    pub const fn is_invalid(self) -> bool {
        self.index == u32::MAX
    }
}

/// A single scope record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    /// Stable kind of the scope.
    pub kind: ScopeKind,
    /// Parent scope, or `ScopeId::INVALID` for top-level module scopes.
    pub parent: ScopeId,
    /// Node that introduced this scope (the `NodeKind::Module` /
    /// `NodeKind::Function` / etc. node in the unified graph).
    pub node: NodeId,
    /// Byte range in the containing file `[start, end)`.
    pub byte_span: (u32, u32),
    /// File that contains this scope. Files are never scopes themselves;
    /// this field is how callers ask "which file does this scope belong to?".
    pub file: FileId,
}

/// Slot state for a `ScopeArena` slot. Mirrors
/// [`crate::graph::unified::storage::arena::SlotState`] for the node arena.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum ScopeSlotState {
    /// Slot contains a live scope record.
    Occupied(Scope),
    /// Slot is free; `next_free` points at the next free slot or `None`.
    Vacant { next_free: Option<u32> },
}

/// One slot in the arena.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScopeSlot {
    generation: u64,
    state: ScopeSlotState,
}

/// Ephemeral slot-based storage for `Scope` records.
///
/// Populated during Phase 4e (`derive_binding_plane`) and persisted as part
/// of the V9 snapshot shape. Does not participate in MVCC directly —
/// `CodeGraph::scope_arena: Arc<ScopeArena>` provides the snapshot sharing,
/// and mutating operations go through `Arc::make_mut`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScopeArena {
    slots: Vec<ScopeSlot>,
    free_head: Option<u32>,
    len: usize,
}

impl ScopeArena {
    /// Creates an empty arena.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of occupied slots.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if no scopes are allocated.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the total number of slots (occupied + vacant). This is the
    /// size any dense slot-aligned companion store (e.g.,
    /// `ScopeProvenanceStore`) must resize to.
    #[inline]
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Allocates a new scope and returns its generational handle.
    ///
    /// Reuses a vacant slot from the free list when one is available;
    /// otherwise appends a fresh slot. Every allocation advances the slot
    /// generation, so stale handles from prior allocations at the same
    /// index can never alias into this new scope.
    ///
    /// # Panics
    ///
    /// Panics if the arena grows beyond `u32::MAX` slots, which would make new
    /// scope ids unrepresentable.
    pub fn allocate(&mut self, scope: Scope) -> ScopeId {
        self.len += 1;
        if let Some(free_idx) = self.free_head {
            let slot = &mut self.slots[free_idx as usize];
            let ScopeSlotState::Vacant { next_free } = slot.state else {
                unreachable!("free_head pointed at an occupied slot");
            };
            self.free_head = next_free;
            slot.generation = slot.generation.saturating_add(1);
            slot.state = ScopeSlotState::Occupied(scope);
            ScopeId {
                index: free_idx,
                generation: slot.generation,
            }
        } else {
            let index = u32::try_from(self.slots.len()).expect("scope arena index fits u32");
            self.slots.push(ScopeSlot {
                generation: 1,
                state: ScopeSlotState::Occupied(scope),
            });
            ScopeId {
                index,
                generation: 1,
            }
        }
    }

    /// Frees the slot named by `id`. Subsequent lookups with the same
    /// handle return `None` because the slot's generation has advanced.
    /// Safe to call with an invalid or stale id — such calls are no-ops.
    pub fn free(&mut self, id: ScopeId) {
        if id.is_invalid() {
            return;
        }
        let Some(slot) = self.slots.get_mut(id.index as usize) else {
            return;
        };
        if slot.generation != id.generation {
            return;
        }
        if !matches!(slot.state, ScopeSlotState::Occupied(_)) {
            return;
        }
        slot.generation = slot.generation.saturating_add(1);
        slot.state = ScopeSlotState::Vacant {
            next_free: self.free_head,
        };
        self.free_head = Some(id.index);
        self.len -= 1;
    }

    /// Updates the parent of an existing scope.
    ///
    /// Used by the two-pass derivation: pass 1 allocates scopes with
    /// `parent: ScopeId::INVALID`, pass 2 walks the now-complete arena and
    /// stamps each scope's parent via this method.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeArenaError::StaleHandle`] if `id` is invalid, out of
    /// range, stale (generation mismatch), or points at a vacant slot.
    pub fn set_parent(&mut self, id: ScopeId, parent: ScopeId) -> Result<(), ScopeArenaError> {
        if id.is_invalid() {
            return Err(ScopeArenaError::StaleHandle);
        }
        let slot = self
            .slots
            .get_mut(id.index as usize)
            .ok_or(ScopeArenaError::StaleHandle)?;
        if slot.generation != id.generation {
            return Err(ScopeArenaError::StaleHandle);
        }
        match &mut slot.state {
            ScopeSlotState::Occupied(scope) => {
                scope.parent = parent;
                Ok(())
            }
            ScopeSlotState::Vacant { .. } => Err(ScopeArenaError::StaleHandle),
        }
    }

    /// Looks up a scope by handle. Returns `None` for invalid or stale ids.
    #[must_use]
    pub fn get(&self, id: ScopeId) -> Option<&Scope> {
        if id.is_invalid() {
            return None;
        }
        let slot = self.slots.get(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        match &slot.state {
            ScopeSlotState::Occupied(scope) => Some(scope),
            ScopeSlotState::Vacant { .. } => None,
        }
    }

    /// Applies `keep` to every live scope's introducing `NodeId`
    /// (`Scope.node`), freeing the slot for any scope whose node fails the
    /// predicate.
    ///
    /// Freed slots advance their generation counter and are returned to the
    /// internal free list exactly as if `free()` had been called with each
    /// rejected `ScopeId`, so stale handles remain stale after compaction.
    ///
    /// This is the mutation entry point used by the Gate 0c `NodeIdBearing`
    /// impl (A2 §K row K.A11). Callers that hold the arena behind an `Arc`
    /// must reach it through `Arc::make_mut` before invoking this method.
    ///
    /// `#[allow(dead_code)]` mirrors the `NodeIdBearing` trait itself: Gate 0b
    /// lands the scaffolding and unit tests, Gate 0c adds the production
    /// call site in `RebuildGraph::finalize()`.
    #[allow(dead_code)]
    pub(crate) fn retain_by_node(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        let to_drop: Vec<ScopeId> = self
            .iter()
            .filter_map(|(id, scope)| (!keep(scope.node)).then_some(id))
            .collect();
        for id in to_drop {
            self.free(id);
        }
    }

    /// Yields the introducing `NodeId` of every live scope in the arena.
    ///
    /// Vacant slots are silently skipped. Used by the Gate 0b `NodeIdBearing`
    /// impl (A2 §K row K.A11).
    pub fn iter_node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.iter().map(|(_id, scope)| scope.node)
    }

    /// Returns an iterator over all live `(ScopeId, &Scope)` pairs.
    ///
    /// Vacant slots are silently skipped. The `ScopeId` values carry the real
    /// generation stored in each slot, so they are stable and safe to stash or
    /// re-use as map keys (e.g., in `build_node_to_scope_map`). Avoids the
    /// generation=1 hardcode that a manual `0..slot_count` loop would require.
    ///
    /// # Panics
    ///
    /// Panics if the in-memory slot count exceeds `u32::MAX`, which would make
    /// it impossible to represent a stable [`ScopeId`].
    pub fn iter(&self) -> impl Iterator<Item = (ScopeId, &Scope)> + '_ {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(idx, slot)| match &slot.state {
                ScopeSlotState::Occupied(scope) => {
                    let id = ScopeId::new(
                        u32::try_from(idx).expect("slot index fits u32"),
                        slot.generation,
                    );
                    Some((id, scope))
                }
                ScopeSlotState::Vacant { .. } => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::file::id::FileId;
    use crate::graph::unified::node::id::NodeId;

    fn sample_scope() -> Scope {
        Scope {
            kind: ScopeKind::Module,
            parent: ScopeId::INVALID,
            node: NodeId::new(0, 1),
            byte_span: (0, 100),
            file: FileId::new(0),
        }
    }

    #[test]
    fn allocate_assigns_incrementing_index_and_generation() {
        let mut arena = ScopeArena::new();
        let a = arena.allocate(sample_scope());
        let b = arena.allocate(sample_scope());
        assert_eq!(a.index(), 0);
        assert_eq!(a.generation(), 1);
        assert_eq!(b.index(), 1);
        assert_eq!(b.generation(), 1);
        assert_eq!(arena.len(), 2);
        assert_eq!(arena.slot_count(), 2);
    }

    #[test]
    fn get_returns_the_allocated_scope() {
        let mut arena = ScopeArena::new();
        let id = arena.allocate(sample_scope());
        let scope = arena.get(id).expect("live scope must resolve");
        assert_eq!(scope.kind, ScopeKind::Module);
        assert_eq!(scope.byte_span, (0, 100));
    }

    #[test]
    fn stale_handle_rejected_after_free_and_reallocate() {
        let mut arena = ScopeArena::new();
        let first = arena.allocate(sample_scope());
        arena.free(first);
        assert!(arena.get(first).is_none(), "freed handle must return None");

        // Reallocate into the same slot; generation advances.
        let second = arena.allocate(Scope {
            kind: ScopeKind::Function,
            ..sample_scope()
        });
        assert_eq!(second.index(), first.index(), "same slot reused");
        assert_ne!(
            second.generation(),
            first.generation(),
            "generation must advance on reallocation"
        );
        assert!(
            arena.get(first).is_none(),
            "stale handle must not see the new occupant"
        );
        assert_eq!(arena.get(second).unwrap().kind, ScopeKind::Function);
    }

    #[test]
    fn postcard_round_trip_preserves_arena_layout() {
        let mut arena = ScopeArena::new();
        arena.allocate(sample_scope());
        let bytes = postcard::to_allocvec(&arena).expect("serialize");
        let restored: ScopeArena = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(restored.len(), arena.len());
        assert_eq!(restored.slot_count(), arena.slot_count());
    }

    #[test]
    fn double_free_is_noop() {
        let mut arena = ScopeArena::new();
        let id = arena.allocate(sample_scope());
        arena.free(id);
        let len_after_first_free = arena.len();
        arena.free(id); // stale id; second free must be a no-op
        assert_eq!(
            arena.len(),
            len_after_first_free,
            "double-free must not corrupt len"
        );
    }

    #[test]
    fn free_invalid_or_out_of_bounds_id_is_noop() {
        let mut arena = ScopeArena::new();
        arena.free(ScopeId::INVALID);
        arena.free(ScopeId::new(9999, 1));
        assert!(arena.is_empty());
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.slot_count(), 0);
    }

    #[test]
    fn postcard_round_trip_preserves_free_list_and_generations() {
        let mut arena = ScopeArena::new();
        let a = arena.allocate(sample_scope());
        let b = arena.allocate(sample_scope());
        let c = arena.allocate(sample_scope());
        arena.free(b);

        let bytes = postcard::to_allocvec(&arena).expect("serialize");
        let mut restored: ScopeArena = postcard::from_bytes(&bytes).expect("deserialize");

        assert_eq!(restored.len(), 2);
        assert_eq!(restored.slot_count(), 3);
        assert!(
            restored.get(a).is_some(),
            "live handle a must survive round-trip"
        );
        assert!(
            restored.get(b).is_none(),
            "freed slot must stay stale across round-trip"
        );
        assert!(
            restored.get(c).is_some(),
            "live handle c must survive round-trip"
        );

        // Next allocation should reuse slot b.index() with an advanced generation,
        // proving the free-list head and slot generations were persisted.
        let d = restored.allocate(sample_scope());
        assert_eq!(d.index(), b.index(), "free-list head must be persisted");
        assert_ne!(
            d.generation(),
            b.generation(),
            "generation must have advanced past the stale handle"
        );
        assert!(
            restored.get(b).is_none(),
            "stale handle b must remain stale after slot reuse"
        );
        assert_eq!(restored.len(), 3);
    }

    #[test]
    fn scope_kind_discriminant_pins_wire_format() {
        assert_eq!(ScopeKind::Module.discriminant(), 0);
        assert_eq!(ScopeKind::Function.discriminant(), 1);
        assert_eq!(ScopeKind::Class.discriminant(), 2);
        assert_eq!(ScopeKind::Namespace.discriminant(), 3);
        assert_eq!(ScopeKind::Trait.discriminant(), 4);
        assert_eq!(ScopeKind::Impl.discriminant(), 5);
    }

    #[test]
    fn set_parent_updates_existing_scope() {
        let mut arena = ScopeArena::new();
        let parent = arena.allocate(sample_scope());
        let child = arena.allocate(sample_scope());
        assert!(arena.set_parent(child, parent).is_ok());
        assert_eq!(arena.get(child).unwrap().parent, parent);
    }

    #[test]
    fn set_parent_rejects_stale_handle() {
        let mut arena = ScopeArena::new();
        let id = arena.allocate(sample_scope());
        arena.free(id);
        let other = arena.allocate(sample_scope());
        assert!(
            arena.set_parent(id, ScopeId::INVALID).is_err(),
            "stale handle must be rejected"
        );
        assert_eq!(
            arena.get(other).unwrap().parent,
            ScopeId::INVALID,
            "unrelated scope must be unaffected"
        );
    }

    #[test]
    fn scope_arena_iter_yields_live_slots_with_real_generation() {
        let mut arena = ScopeArena::new();
        let a = arena.allocate(sample_scope());
        let b = arena.allocate(sample_scope());
        let c = arena.allocate(sample_scope());
        arena.free(b); // b becomes vacant; generation advances.

        let pairs: Vec<(ScopeId, _)> = arena.iter().collect();
        // Only live slots: a and c.
        assert_eq!(pairs.len(), 2, "iter must skip the vacant slot for b");
        let ids: Vec<ScopeId> = pairs.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&a), "iter must include slot a");
        assert!(ids.contains(&c), "iter must include slot c");
        assert!(
            !ids.contains(&b),
            "iter must not include freed slot b (stale id)"
        );

        // Verify the yielded IDs are usable with arena.get().
        for (id, scope_ref) in &pairs {
            let looked_up = arena.get(*id).expect("iter-yielded id must be live");
            assert_eq!(
                std::ptr::from_ref::<Scope>(*scope_ref),
                std::ptr::from_ref::<Scope>(looked_up),
                "iter ref must alias arena.get ref"
            );
        }
    }

    #[test]
    fn memory_budget_under_32_bytes_per_slot_at_1k_scopes() {
        let mut arena = ScopeArena::new();
        for idx in 0..1000 {
            let scope = Scope {
                kind: ScopeKind::Module,
                parent: ScopeId::INVALID,
                node: NodeId::new(idx, 1),
                byte_span: (idx * 100, idx * 100 + 80),
                file: FileId::new(0),
            };
            arena.allocate(scope);
        }
        let bytes = postcard::to_allocvec(&arena).expect("serialize");
        let per_slot = bytes.len() as f64 / 1000.0;
        assert!(
            per_slot <= 48.0,
            "scope arena postcard size {per_slot} B/slot exceeds 48-byte absolute ceiling \
             (target: <=32 average, 48 hard cap per 05_TEST_PLAN.md T02)"
        );
    }
}
