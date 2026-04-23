//! Content-addressed scope identity for Phase 2.
//!
//! Provides [`FileStableId`], [`ScopeStableId`], [`ScopeProvenance`], and
//! [`ScopeProvenanceStore`] — the persistence layer for the binding plane's
//! scope identity model.
//!
//! ## Design
//!
//! `FileStableId` is derived from a file's canonical registry path using
//! BLAKE3, truncated to 16 bytes. Mutable `FileEntry` fields (`source_uri`,
//! `is_external`, `content_hash`, `indexed_at`) are deliberately excluded
//! from the hash input so that identity cannot flip mid-lifetime. There is
//! intentionally no `from_file_entry` constructor — this exclusion is
//! enforced at compile time.
//!
//! `ScopeStableId` is a 16-byte BLAKE3 digest of
//! `(file_stable_id, file_content_hash, scope_kind_discriminant, byte_span)`.
//! Using both the file stable id and the file content hash makes scopes
//! sensitive to both location and content changes.
//!
//! `ScopeProvenanceStore` is a dense slot-aligned store keyed by
//! `ScopeId.index()`, mirroring the shape of Phase 1's
//! [`crate::graph::unified::storage::node_provenance::NodeProvenanceStore`].
//! It embeds a generation check so that stale handles are rejected on lookup.
//! A `HashMap<ScopeStableId, ScopeId>` reverse index is NOT serialized —
//! it is rebuilt after V9 deserialization via
//! [`ScopeProvenanceStore::rebuild_reverse_index`].

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::arena::{ScopeArena, ScopeId, ScopeKind};

// ---------------------------------------------------------------------------
// FileStableId
// ---------------------------------------------------------------------------

/// Content-addressed file identity.
///
/// Derived from the registry's canonical absolute path only.
/// `source_uri`, `is_external`, `content_hash`, and `indexed_at` are
/// intentionally NOT consulted because they can change post-registration.
/// There is no `from_file_entry` constructor — the exclusion is enforced
/// at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileStableId([u8; 16]);

impl FileStableId {
    /// Computes the file identity from the registry's canonical absolute
    /// path. This is the sole constructor.
    #[must_use]
    pub fn from_registry_path(path: &Path) -> Self {
        let identity = path.as_os_str().as_encoded_bytes();
        let hash = blake3::hash(identity);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&hash.as_bytes()[..16]);
        Self(bytes)
    }

    /// Returns the raw 16-byte representation.
    #[inline]
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// ScopeStableId
// ---------------------------------------------------------------------------

/// Content-addressed scope identity.
///
/// A 16-byte BLAKE3 digest of
/// `(file_stable_id, file_content_hash, scope_kind_discriminant, byte_span.0,
/// byte_span.1)`. Scopes that move within a file (byte span changes) or whose
/// file content changes will receive a new stable id, while scopes that are
/// identical across builds share the same id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopeStableId(pub [u8; 16]);

// ---------------------------------------------------------------------------
// compute_scope_stable_id
// ---------------------------------------------------------------------------

/// Computes the 16-byte stable id for a scope.
///
/// Hash inputs (in order):
/// - `file_stable_id` bytes
/// - `file_content_hash` (32 bytes — SHA-256 of file content from Phase 1)
/// - `kind.discriminant()` (1 byte — pinned by `ScopeKind::discriminant`)
/// - `byte_span.0` as little-endian `u32`
/// - `byte_span.1` as little-endian `u32`
#[must_use]
pub fn compute_scope_stable_id(
    file_stable_id: FileStableId,
    file_content_hash: [u8; 32],
    kind: ScopeKind,
    byte_span: (u32, u32),
) -> ScopeStableId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(file_stable_id.as_bytes());
    hasher.update(&file_content_hash);
    hasher.update(&[kind.discriminant()]);
    hasher.update(&byte_span.0.to_le_bytes());
    hasher.update(&byte_span.1.to_le_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    ScopeStableId(bytes)
}

// ---------------------------------------------------------------------------
// ScopeProvenance
// ---------------------------------------------------------------------------

/// Per-scope provenance record stamped during Phase 4e.
///
/// `first_seen_epoch` and `last_seen_epoch` are populated from
/// `CodeGraph::fact_epoch()` at derivation time. `stable_id` is pre-computed
/// so the binding-plane facade can hand it out without recomputation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeProvenance {
    /// Fact epoch at which this scope was first observed.
    pub first_seen_epoch: u64,
    /// Fact epoch at which this scope was most recently observed.
    pub last_seen_epoch: u64,
    /// Stable identity of the containing file, derived from its registry path.
    pub file_stable_id: FileStableId,
    /// Content-addressed stable identity for this scope.
    pub stable_id: ScopeStableId,
}

// ---------------------------------------------------------------------------
// ScopeProvenanceStore
// ---------------------------------------------------------------------------

/// Dense slot-aligned provenance store for Phase 2 scopes.
///
/// Mirrors the shape of Phase 1's
/// [`crate::graph::unified::storage::node_provenance::NodeProvenanceStore`]:
/// - `slots: Vec<Option<(u64, ScopeProvenance)>>` stores the generation +
///   provenance pair at each slot index. The generation is used to reject
///   stale `ScopeId` handles (same generation-check as `ScopeArena`).
/// - `reverse_index: HashMap<ScopeStableId, ScopeId>` is a derived cache and
///   is NOT serialized. It is rebuilt after V9 deserialization via
///   [`rebuild_reverse_index`].
///
/// ## Serialization
///
/// `ScopeProvenanceStore` uses `serde`'s derived serialization for `slots`
/// and skips `reverse_index` with `#[serde(skip)]`. After deserialization,
/// callers MUST call [`rebuild_reverse_index`] before using
/// [`scope_by_stable_id`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScopeProvenanceStore {
    slots: Vec<Option<(u64, ScopeProvenance)>>,
    #[serde(skip)]
    reverse_index: HashMap<ScopeStableId, ScopeId>,
}

impl ScopeProvenanceStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the total number of slots (occupied + vacant).
    ///
    /// Companion stores should `resize_to` this value to stay slot-aligned
    /// with the `ScopeArena`.
    #[inline]
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Returns the number of occupied slots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    /// Returns `true` if no provenance records are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Grows the slot vector to `slot_count` entries (no-op if already large enough).
    ///
    /// Call this after allocating scopes in a `ScopeArena` to keep the two
    /// structures slot-aligned.
    pub fn resize_to(&mut self, slot_count: usize) {
        if self.slots.len() < slot_count {
            self.slots.resize(slot_count, None);
        }
    }

    /// Inserts a provenance record for a scope.
    ///
    /// Extends the slot vector if `id.index()` exceeds the current capacity.
    /// Also inserts the `stable_id → id` mapping in the reverse index.
    pub fn insert(&mut self, id: ScopeId, provenance: ScopeProvenance) {
        let idx = id.index() as usize;
        if self.slots.len() <= idx {
            self.resize_to(idx + 1);
        }
        let stable = provenance.stable_id;
        self.slots[idx] = Some((id.generation(), provenance));
        self.reverse_index.insert(stable, id);
    }

    /// Looks up a provenance record by its `ScopeId`.
    ///
    /// Returns `None` if the slot is vacant, out of range, or if the stored
    /// generation does not match (stale handle).
    #[must_use]
    pub fn lookup(&self, id: ScopeId) -> Option<&ScopeProvenance> {
        let slot = self.slots.get(id.index() as usize)?.as_ref()?;
        if slot.0 != id.generation() {
            return None;
        }
        Some(&slot.1)
    }

    /// Returns an iterator over all live `(ScopeId, &ScopeProvenance)` pairs.
    ///
    /// This iterates all occupied slots. The `ScopeId` returned for each
    /// slot carries the generation stored in that slot.
    pub fn entries(&self) -> impl Iterator<Item = (ScopeId, &ScopeProvenance)> {
        self.slots.iter().enumerate().filter_map(|(idx, slot)| {
            let (slot_gen, prov) = slot.as_ref()?;
            #[allow(clippy::cast_possible_truncation)]
            let id = ScopeId::new(idx as u32, *slot_gen);
            Some((id, prov))
        })
    }

    /// Looks up the live `ScopeId` for a stable identity.
    ///
    /// Returns `None` if the stable id has no registered entry. The reverse
    /// index is populated by [`insert`] and must be rebuilt after
    /// deserialization via [`rebuild_reverse_index`].
    #[must_use]
    pub fn scope_by_stable_id(&self, stable: ScopeStableId) -> Option<ScopeId> {
        self.reverse_index.get(&stable).copied()
    }

    /// Rebuilds the reverse-index map from the current slot vector and arena.
    ///
    /// Called after V9 deserialization because `ScopeId.generation` values
    /// after re-materialization must match the live arena, not the serialized
    /// store's prior generation state.
    ///
    /// Each slot is validated against the `arena` at the corresponding index:
    /// if the arena returns `Some` for `ScopeId::new(idx, gen)`, the stable-id
    /// mapping is re-established; otherwise the slot is silently skipped (the
    /// arena has been compacted or the generation drifted).
    pub fn rebuild_reverse_index(&mut self, arena: &ScopeArena) {
        self.reverse_index.clear();
        for (idx, slot) in self.slots.iter().enumerate() {
            let Some((slot_gen, prov)) = slot.as_ref() else {
                continue;
            };
            #[allow(clippy::cast_possible_truncation)]
            let candidate = ScopeId::new(idx as u32, *slot_gen);
            if arena.get(candidate).is_some() {
                self.reverse_index.insert(prov.stable_id, candidate);
            }
        }
        debug_assert!(
            self.reverse_index.len() <= self.len(),
            "reverse_index must not exceed occupied slot count"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::graph::unified::bind::scope::arena::{Scope, ScopeArena};
    use crate::graph::unified::file::id::FileId;
    use crate::graph::unified::node::id::NodeId;

    #[test]
    fn t21_file_stable_id_is_deterministic_and_path_distinct() {
        let id_a = FileStableId::from_registry_path(Path::new("/a/b/file.rs"));
        let id_b = FileStableId::from_registry_path(Path::new("/a/b/file.rs"));
        let id_c = FileStableId::from_registry_path(Path::new("/c/d/file.rs"));
        assert_eq!(id_a, id_b, "deterministic for same path");
        assert_ne!(id_a, id_c, "distinct for distinct paths");
    }

    #[test]
    fn t22_file_stable_id_ignores_source_uri_and_is_external() {
        // Construct two FileStableIds from the same canonical path but via
        // different mock FileEntry configurations. Since the API only
        // accepts &Path, source_uri and is_external are structurally absent
        // from the input - this test verifies compile-time absence by
        // making sure there is no `from_file_entry` constructor at all.
        // Instead: just test that constructing from the same path twice
        // always produces the same id.
        let id = FileStableId::from_registry_path(Path::new("/workspace/src/main.rs"));
        assert_eq!(
            id,
            FileStableId::from_registry_path(Path::new("/workspace/src/main.rs"))
        );
    }

    #[test]
    fn t23_compute_scope_stable_id_is_input_sensitive() {
        let file_id = FileStableId::from_registry_path(Path::new("/a.rs"));
        let hash = [0u8; 32];
        let base = compute_scope_stable_id(file_id, hash, ScopeKind::Module, (0, 100));
        assert_eq!(
            base,
            compute_scope_stable_id(file_id, hash, ScopeKind::Module, (0, 100))
        );
        // Different span.
        assert_ne!(
            base,
            compute_scope_stable_id(file_id, hash, ScopeKind::Module, (0, 200))
        );
        // Different kind.
        assert_ne!(
            base,
            compute_scope_stable_id(file_id, hash, ScopeKind::Function, (0, 100))
        );
        // Different content hash.
        let mut alt_hash = hash;
        alt_hash[0] = 1;
        assert_ne!(
            base,
            compute_scope_stable_id(file_id, alt_hash, ScopeKind::Module, (0, 100))
        );
        // Different file.
        let other_file = FileStableId::from_registry_path(Path::new("/b.rs"));
        assert_ne!(
            base,
            compute_scope_stable_id(other_file, hash, ScopeKind::Module, (0, 100))
        );
    }

    #[test]
    fn t24_scope_provenance_store_insert_lookup_and_stale_handle() {
        let mut store = ScopeProvenanceStore::new();
        store.resize_to(4);
        let id = ScopeId::new(0, 1);
        let prov = ScopeProvenance {
            first_seen_epoch: 10,
            last_seen_epoch: 20,
            file_stable_id: FileStableId::from_registry_path(Path::new("/a.rs")),
            stable_id: ScopeStableId([0u8; 16]),
        };
        store.insert(id, prov.clone());
        assert_eq!(store.lookup(id), Some(&prov));
        // Stale generation.
        assert_eq!(store.lookup(ScopeId::new(0, 999)), None);
    }

    #[test]
    fn t38_scope_by_stable_id_reverse_lookup() {
        // Build a tiny live arena with one allocated module scope so
        // rebuild_reverse_index has a real arena to consult.
        let mut arena = ScopeArena::new();
        let scope_id = arena.allocate(Scope {
            kind: ScopeKind::Module,
            parent: ScopeId::INVALID,
            node: NodeId::new(0, 1),
            byte_span: (0, 100),
            file: FileId::new(0),
        });

        // Insert a matching ScopeProvenance record at the same slot index.
        let stable = ScopeStableId([7u8; 16]);
        let mut store = ScopeProvenanceStore::new();
        store.resize_to(arena.slot_count());
        store.insert(
            scope_id,
            ScopeProvenance {
                first_seen_epoch: 1,
                last_seen_epoch: 1,
                file_stable_id: FileStableId::from_registry_path(Path::new("/x.rs")),
                stable_id: stable,
            },
        );

        // insert populates the reverse index automatically; verify the lookup works.
        assert_eq!(store.scope_by_stable_id(stable), Some(scope_id));
        assert_eq!(store.scope_by_stable_id(ScopeStableId([0u8; 16])), None);

        // Clear the reverse index and rebuild it from the arena — this
        // simulates the post-V9-load rebuild path.
        store.rebuild_reverse_index(&arena);
        assert_eq!(store.scope_by_stable_id(stable), Some(scope_id));
    }

    #[test]
    fn t_postcard_round_trip_and_stable_id_lookup_restored() {
        let mut arena = ScopeArena::new();
        let scope_id = arena.allocate(Scope {
            kind: ScopeKind::Function,
            parent: ScopeId::INVALID,
            node: NodeId::new(1, 1),
            byte_span: (10, 80),
            file: FileId::new(0),
        });

        let stable = ScopeStableId([42u8; 16]);
        let mut store = ScopeProvenanceStore::new();
        store.resize_to(arena.slot_count());
        store.insert(
            scope_id,
            ScopeProvenance {
                first_seen_epoch: 5,
                last_seen_epoch: 5,
                file_stable_id: FileStableId::from_registry_path(Path::new("/lib.rs")),
                stable_id: stable,
            },
        );

        // Serialize.
        let bytes = postcard::to_allocvec(&store).expect("serialize");
        // Deserialize — reverse_index is empty at this point (serde skip).
        let mut restored: ScopeProvenanceStore = postcard::from_bytes(&bytes).expect("deserialize");

        // Lookup by ScopeId must still work (uses slots, not reverse_index).
        assert_eq!(
            restored.lookup(scope_id).map(|p| p.stable_id),
            Some(stable),
            "lookup by ScopeId must survive round-trip"
        );
        // Reverse-index lookup must fail until rebuilt.
        assert_eq!(
            restored.scope_by_stable_id(stable),
            None,
            "reverse_index is serde(skip) — must be None before rebuild"
        );

        // Rebuild the reverse index.
        restored.rebuild_reverse_index(&arena);
        assert_eq!(
            restored.scope_by_stable_id(stable),
            Some(scope_id),
            "scope_by_stable_id must work after rebuild_reverse_index"
        );
    }
}
