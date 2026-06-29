//! Versioned V13 wire types for the snapshot reader.
//!
//! # Why this module exists
//!
//! T2 inserts `NodeKind::Channel` immediately before `NodeKind::Other`
//! (`Other` carries `#[serde(other)]` and must stay last). Postcard encodes
//! enum variants by their positional declaration index, so inserting before
//! `Other` shifts `Other`'s discriminant by one (from `N` to `N+1`). A V13
//! snapshot's node arena was written with the **old** layout (`Other` at
//! index `N`, no `Channel`); deserializing those bytes directly into the live
//! `NodeKind` would decode every legacy `Other` node as `Channel` — silent
//! corruption — because the decode happens positionally before any upconvert
//! could run.
//!
//! This module freezes the V13 `NodeKind` wire layout (`NodeKindV13`) and the
//! containing deserialization path so the V13 reader decodes node kinds under
//! the V13 layout, then translates to the live `NodeKind` (V14). It follows
//! the same versioned-mirror discipline as [`super::legacy_v10`], but on the
//! node side instead of the edge side.
//!
//! # What's mirrored
//!
//! `NodeKind` is embedded in exactly two serialized carriers of
//! `GraphSnapshotData` (the full audit is in
//! `docs/development/go-channels-and-generic-instantiation/02_DESIGN.md`
//! §6.1):
//!
//! * [`NodeArenaV13`] — the node arena, whose [`NodeEntryV13::kind`] is a
//!   `NodeKindV13`. Every other `NodeEntry` field is layout-stable and is
//!   mirrored byte-for-byte.
//! * [`AuxiliaryIndicesV13`] — the auxiliary index set, whose `kind_index`
//!   is keyed by `NodeKindV13`. The other index buckets key on `StringId` /
//!   `FileId`, which are layout-stable.
//!
//! `ResolvedBinding.kind` also holds a `NodeKind`, but it is **never
//! serialized** (it is built at query time from the already-translated live
//! arena), so it needs no mirror.
//!
//! The mirror types derive `Serialize` as well so the V13-payload round-trip
//! tests in `snapshot.rs` can hand-craft a V13 frame.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::graph::body_hash::BodyHash128;
use crate::graph::unified::file::id::FileId;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::node::kind::NodeKind;
use crate::graph::unified::storage::{AuxiliaryIndices, NodeArena, NodeEntry, Slot, SlotState};
use crate::graph::unified::string::StringId;

// ============================================================================
// NodeKindV13 — frozen pre-Channel NodeKind wire layout.
// ============================================================================

/// Frozen V13 wire-format mirror of `NodeKind`.
///
/// Variant order is the pre-T2 declaration order: every variant up to and
/// including `EnumConstant`, then `Other` last (no `Channel`). Postcard
/// encodes by positional index, so this order is the wire contract — it must
/// never change. The serde attributes (`rename_all = "snake_case"`,
/// `#[serde(other)] Other`) match the live enum so the JSON wire form, if
/// ever used, is identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NodeKindV13 {
    Function,
    Method,
    Class,
    Interface,
    Trait,
    Module,
    Variable,
    Constant,
    Type,
    Struct,
    Enum,
    EnumVariant,
    Macro,
    Parameter,
    Property,
    CallSite,
    Import,
    Export,
    StyleRule,
    StyleAtRule,
    StyleVariable,
    Lifetime,
    Component,
    Service,
    Resource,
    Endpoint,
    Test,
    TypeParameter,
    Annotation,
    AnnotationValue,
    LambdaTarget,
    JavaModule,
    EnumConstant,
    #[serde(other)]
    Other,
}

/// Translate one frozen V13 node kind to the live (V14) `NodeKind`.
///
/// Every pre-`Other` variant maps to its identically-named live variant;
/// `Other` maps to live `Other` (which now sits one position later because
/// `Channel` was inserted before it). `Channel` cannot appear here — no V13
/// snapshot contains it.
const fn translate_node_kind_v13_to_v14(kind: NodeKindV13) -> NodeKind {
    match kind {
        NodeKindV13::Function => NodeKind::Function,
        NodeKindV13::Method => NodeKind::Method,
        NodeKindV13::Class => NodeKind::Class,
        NodeKindV13::Interface => NodeKind::Interface,
        NodeKindV13::Trait => NodeKind::Trait,
        NodeKindV13::Module => NodeKind::Module,
        NodeKindV13::Variable => NodeKind::Variable,
        NodeKindV13::Constant => NodeKind::Constant,
        NodeKindV13::Type => NodeKind::Type,
        NodeKindV13::Struct => NodeKind::Struct,
        NodeKindV13::Enum => NodeKind::Enum,
        NodeKindV13::EnumVariant => NodeKind::EnumVariant,
        NodeKindV13::Macro => NodeKind::Macro,
        NodeKindV13::Parameter => NodeKind::Parameter,
        NodeKindV13::Property => NodeKind::Property,
        NodeKindV13::CallSite => NodeKind::CallSite,
        NodeKindV13::Import => NodeKind::Import,
        NodeKindV13::Export => NodeKind::Export,
        NodeKindV13::StyleRule => NodeKind::StyleRule,
        NodeKindV13::StyleAtRule => NodeKind::StyleAtRule,
        NodeKindV13::StyleVariable => NodeKind::StyleVariable,
        NodeKindV13::Lifetime => NodeKind::Lifetime,
        NodeKindV13::Component => NodeKind::Component,
        NodeKindV13::Service => NodeKind::Service,
        NodeKindV13::Resource => NodeKind::Resource,
        NodeKindV13::Endpoint => NodeKind::Endpoint,
        NodeKindV13::Test => NodeKind::Test,
        NodeKindV13::TypeParameter => NodeKind::TypeParameter,
        NodeKindV13::Annotation => NodeKind::Annotation,
        NodeKindV13::AnnotationValue => NodeKind::AnnotationValue,
        NodeKindV13::LambdaTarget => NodeKind::LambdaTarget,
        NodeKindV13::JavaModule => NodeKind::JavaModule,
        NodeKindV13::EnumConstant => NodeKind::EnumConstant,
        NodeKindV13::Other => NodeKind::Other,
    }
}

// ============================================================================
// NodeEntryV13 — wire-stable mirror of NodeEntry with kind: NodeKindV13.
// ============================================================================

/// Frozen V13 wire-format mirror of `NodeEntry`.
///
/// Field order and serde attributes match the live `NodeEntry` exactly; the
/// only difference is `kind: NodeKindV13` instead of `kind: NodeKind`. The
/// `body_hash` / `is_unsafe` `#[serde(default)]` tail and its ordering are
/// replicated so the postcard stream decodes identically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NodeEntryV13 {
    pub(crate) kind: NodeKindV13,
    pub(crate) name: StringId,
    pub(crate) file: FileId,
    pub(crate) start_byte: u32,
    pub(crate) end_byte: u32,
    pub(crate) start_line: u32,
    pub(crate) start_column: u32,
    pub(crate) end_line: u32,
    pub(crate) end_column: u32,
    pub(crate) signature: Option<StringId>,
    pub(crate) doc: Option<StringId>,
    pub(crate) qualified_name: Option<StringId>,
    pub(crate) visibility: Option<StringId>,
    pub(crate) is_async: bool,
    pub(crate) is_static: bool,
    #[serde(default)]
    pub(crate) body_hash: Option<BodyHash128>,
    #[serde(default)]
    pub(crate) is_unsafe: bool,
}

/// Translate a frozen V13 node entry to the live `NodeEntry`, mapping only the
/// `kind` and moving every other field unchanged.
fn translate_node_entry_v13_to_v14(entry: &NodeEntryV13) -> NodeEntry {
    NodeEntry {
        kind: translate_node_kind_v13_to_v14(entry.kind),
        name: entry.name,
        file: entry.file,
        start_byte: entry.start_byte,
        end_byte: entry.end_byte,
        start_line: entry.start_line,
        start_column: entry.start_column,
        end_line: entry.end_line,
        end_column: entry.end_column,
        signature: entry.signature,
        doc: entry.doc,
        qualified_name: entry.qualified_name,
        visibility: entry.visibility,
        is_async: entry.is_async,
        is_static: entry.is_static,
        body_hash: entry.body_hash,
        is_unsafe: entry.is_unsafe,
        is_definition: false,
    }
}

// ============================================================================
// NodeArenaV13 — wire-stable mirror of NodeArena over NodeEntryV13 slots.
// ============================================================================

/// Frozen V13 wire-format mirror of `NodeArena`.
///
/// Field order (`slots`, `free_head`, `len`) matches the live `NodeArena`
/// exactly. The generic `Slot` / `SlotState` types are reused directly — they
/// do not embed `NodeKind`, only the `NodeEntryV13` payload differs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NodeArenaV13 {
    slots: Vec<Slot<NodeEntryV13>>,
    free_head: Option<u32>,
    len: usize,
}

/// Translate the frozen V13 arena to the live `NodeArena`, preserving slot
/// indices, generations, and the free list exactly (so `NodeId` references in
/// edges stay valid) while remapping each `NodeKindV13` to the live
/// `NodeKind`.
pub(crate) fn translate_node_arena_v13_to_v14(v13: NodeArenaV13) -> NodeArena {
    let slots: Vec<Slot<NodeEntry>> = v13
        .slots
        .into_iter()
        .map(|slot| {
            let generation = slot.generation();
            match slot.state() {
                SlotState::Occupied(entry) => {
                    Slot::new_occupied(generation, translate_node_entry_v13_to_v14(entry))
                }
                SlotState::Vacant { next_free } => Slot::new_vacant(generation, *next_free),
            }
        })
        .collect();
    NodeArena::from_raw_parts(slots, v13.free_head, v13.len)
}

// ============================================================================
// AuxiliaryIndicesV13 — wire-stable mirror with kind_index: NodeKindV13.
// ============================================================================

/// Frozen V13 wire-format mirror of `AuxiliaryIndices`.
///
/// Field order matches the live `AuxiliaryIndices`. `kind_index` is keyed by
/// `NodeKindV13` so the V13 bytes decode under the V13 `NodeKind` layout. The
/// decoded value is **discarded** by the upconvert, which rebuilds the live
/// indices from the translated arena via `AuxiliaryIndices::build_from_arena`
/// — guaranteeing `kind_index` is re-keyed under the new `NodeKind` layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AuxiliaryIndicesV13 {
    kind_index: BTreeMap<NodeKindV13, Vec<NodeId>>,
    name_index: BTreeMap<StringId, Vec<NodeId>>,
    qualified_name_index: BTreeMap<StringId, Vec<NodeId>>,
    file_index: BTreeMap<FileId, Vec<NodeId>>,
    node_count: usize,
}

impl AuxiliaryIndicesV13 {
    /// Rebuild the live `AuxiliaryIndices` from a translated arena.
    ///
    /// The deserialized V13 index contents are not reused — `build_from_arena`
    /// re-derives every bucket (including the re-keyed `kind_index`) from the
    /// arena, which is the only authoritative source after translation.
    #[must_use]
    pub(crate) fn rebuild_live(arena: &NodeArena) -> AuxiliaryIndices {
        let mut indices = AuxiliaryIndices::new();
        indices.build_from_arena(arena);
        indices
    }
}

// ============================================================================
// Pre-V14 demotion: undo the Other→Channel misdecode for V7–V12 snapshots.
// ============================================================================

/// Demote spurious `Channel` nodes back to `Other` for pre-V13 snapshots.
///
/// `NodeKind` was layout-stable across V7–V13, so the V7–V12 read paths
/// (which deserialize the node arena directly into the live `NodeArena`,
/// reusing the live types) suffer the same `Other`→`Channel` positional
/// misdecode as the V13 path. Those versions provably contain **no** real
/// `Channel` nodes (the variant did not exist), so any node that decoded as
/// `Channel` was an `Other` node at the old terminal discriminant. This
/// reverses that misdecode in place and rebuilds the kind index from the
/// corrected arena.
///
/// The V13 path does **not** use this — it decodes through the frozen
/// `NodeKindV13` mirror, which never produces a spurious `Channel` in the
/// first place.
pub(crate) fn demote_spurious_channel_nodes(arena: &mut NodeArena, indices: &mut AuxiliaryIndices) {
    let mut changed = false;
    for (_, entry) in arena.iter_mut() {
        if entry.kind == NodeKind::Channel {
            entry.kind = NodeKind::Other;
            changed = true;
        }
    }
    if changed {
        indices.build_from_arena(arena);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::string::StringId;

    fn entry_v13(kind: NodeKindV13) -> NodeEntryV13 {
        NodeEntryV13 {
            kind,
            name: StringId::new(1),
            file: FileId::new(0),
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            start_column: 0,
            end_line: 1,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: Some(StringId::new(2)),
            visibility: None,
            is_async: false,
            is_static: false,
            body_hash: None,
            is_unsafe: false,
        }
    }

    fn arena_v13_with(kinds: &[NodeKindV13]) -> NodeArenaV13 {
        let slots = kinds
            .iter()
            .map(|k| Slot::new_occupied(0, entry_v13(*k)))
            .collect();
        NodeArenaV13 {
            slots,
            free_head: None,
            len: kinds.len(),
        }
    }

    /// A frozen V13 `Other` node must translate to live `NodeKind::Other`, not
    /// to `Channel` (which now occupies `Other`'s old positional index).
    #[test]
    fn test_v13_other_translates_to_other_not_channel() {
        let arena_v13 = arena_v13_with(&[NodeKindV13::Function, NodeKindV13::Other]);
        // Round-trip through postcard to prove the wire bytes decode under the
        // frozen V13 layout.
        let bytes = postcard::to_allocvec(&arena_v13).expect("serialize V13 arena");
        let decoded: NodeArenaV13 = postcard::from_bytes(&bytes).expect("decode V13 arena");

        let live = translate_node_arena_v13_to_v14(decoded);
        let kinds: Vec<NodeKind> = live.iter().map(|(_, e)| e.kind).collect();
        assert_eq!(
            kinds,
            vec![NodeKind::Function, NodeKind::Other],
            "V13 Other must land as live Other, never Channel"
        );
        assert!(
            !kinds.contains(&NodeKind::Channel),
            "no spurious Channel node may be produced from V13 bytes"
        );
    }

    /// Demonstrates exactly the misdecode the mirror exists to prevent:
    /// decoding V13 `Other` bytes directly under the live `NodeKind` layout
    /// yields `Channel`. This is why the frozen mirror is required.
    #[test]
    fn test_direct_live_decode_of_v13_other_is_misread_as_channel() {
        // Serialize a single V13 Other kind, then decode it as live NodeKind.
        let v13_other = NodeKindV13::Other;
        let bytes = postcard::to_allocvec(&v13_other).expect("serialize V13 Other");
        let live: NodeKind = postcard::from_bytes(&bytes).expect("decode as live NodeKind");
        assert_eq!(
            live,
            NodeKind::Channel,
            "positional decode of V13 Other under live layout misreads as Channel \
             — this is the corruption the frozen NodeKindV13 mirror prevents"
        );
    }

    /// After translation + index rebuild, the (formerly `Other`) node must be
    /// reachable via `kind_index` under the `Other` key, not `Channel`.
    #[test]
    fn test_kind_index_rebuilt_under_other_key() {
        let arena_v13 = arena_v13_with(&[NodeKindV13::Other, NodeKindV13::Function]);
        let live = translate_node_arena_v13_to_v14(arena_v13);
        let indices = AuxiliaryIndicesV13::rebuild_live(&live);
        assert_eq!(
            indices.by_kind(NodeKind::Other).len(),
            1,
            "the translated node must be reachable under the Other kind key"
        );
        assert_eq!(
            indices.by_kind(NodeKind::Channel).len(),
            0,
            "no node may be indexed under Channel after a V13 upconvert"
        );
    }

    /// The demotion path (V7–V12) reverses a spurious `Channel` decode back to
    /// `Other` and re-keys the index.
    #[test]
    fn test_demote_spurious_channel_nodes() {
        // Build a live arena that (as if mis-decoded from a pre-V13 snapshot)
        // contains a Channel node which was really an Other node.
        let mut arena = NodeArena::new();
        let id = arena
            .alloc(NodeEntry::new(
                NodeKind::Channel,
                StringId::new(1),
                FileId::new(0),
            ))
            .expect("alloc");
        let mut indices = AuxiliaryIndices::new();
        indices.build_from_arena(&arena);
        assert_eq!(indices.by_kind(NodeKind::Channel).len(), 1);

        demote_spurious_channel_nodes(&mut arena, &mut indices);

        assert_eq!(
            arena.get(id).expect("node present").kind,
            NodeKind::Other,
            "spurious Channel must be demoted to Other"
        );
        assert_eq!(indices.by_kind(NodeKind::Channel).len(), 0);
        assert_eq!(indices.by_kind(NodeKind::Other).len(), 1);
    }
}
