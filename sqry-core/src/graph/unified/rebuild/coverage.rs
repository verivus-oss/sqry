//! [A2 §K] `NodeIdBearing` trait + master compaction coverage matrix.
//!
//! **Gate 0b of Task 4** (see
//! `docs/superpowers/plans/2026-03-19-sqryd-daemon.md`, "Step 0b").
//!
//! Every publish-visible structure on [`super::super::CodeGraph`] that
//! stores a [`NodeId`] must implement [`NodeIdBearing`] so that the
//! `RebuildGraph::finalize()` contract (Gate 0c) can compact all
//! NodeId-bearing state uniformly, and the tombstone-residue debug
//! invariant (Gate 0d, A2 §F) can iterate every such structure without
//! special-casing.
//!
//! # Coverage matrix (A2 §K)
//!
//! Two row sets:
//!
//! - **K.A** — structures that have always lived on `CodeGraph`. They
//!   must always be compacted on every rebuild path. Rows K.A1–K.A8
//!   collapse into **four** concrete Rust types:
//!     - [`NodeArena`]              (K.A1 — node arena / the generational slot table).
//!     - [`BidirectionalEdgeStore`] (K.A2–K.A3 — forward + reverse edges, delta + CSR).
//!     - [`AuxiliaryIndices`]       (K.A4–K.A7 — kind / name / qualified-name / file indices).
//!     - [`NodeMetadataStore`]      (K.A8 — macro + classpath per-node metadata).
//!
//!   K.A9 — the CSR adjacency — is NOT a standalone row. CSR is **derived**
//!   state; `finalize()` rebuilds it from the compacted edge store in step 9
//!   rather than mutating it in place. It therefore does not need its own
//!   `NodeIdBearing` impl.
//!
//!   Rows **K.A10–K.A13** cover the remaining publish-visible
//!   `NodeId`-bearing stores that live directly on [`CodeGraph`]:
//!     - [`NodeProvenanceStore`]    (K.A10 — dense, slot-aligned node
//!       provenance; `NodeId` reconstructed from each occupied slot's
//!       index + stored generation).
//!     - [`ScopeArena`]             (K.A11 — Phase 2 binding-plane scope
//!       arena; every live `Scope` records the introducing
//!       `NodeId` in `Scope.node`).
//!     - [`AliasTable`]             (K.A12 — Phase 2 binding-plane alias
//!       table; every entry records the owning `Import` node in
//!       `AliasEntry.import_node`).
//!     - [`ShadowTable`]            (K.A13 — Phase 2 binding-plane shadow
//!       table; every entry records the defining Variable/Parameter
//!       node in `ShadowEntry.node`).
//!
//! - **K.B** — structures introduced by the sqryd daemon plan, added to
//!   this matrix in the same commit that introduces the field itself:
//!     - **K.B1** [`FileRegistry`] — per-file node bucket
//!       (`FileRegistry::per_file_nodes`, introduced by Task 4 Step 1).
//!       Gate 0b adds the **impl shell** (empty iterator + no-op
//!       `retain_nodes`) so the coverage matrix is complete; Step 1
//!       extends the impl when it adds the `per_file_nodes: HashMap`
//!       field.
//!     - **K.B2** Reverse-import index — computed on-demand by
//!       [`super::super::concurrent::CodeGraph::reverse_import_index`]
//!       from the existing edge store. It has no standalone type, no
//!       cached state, and therefore no `NodeIdBearing` impl to add;
//!       when its source edges are compacted by the edge-store impl the
//!       derived view is automatically up to date.
//!     - **K.B3** Future Pass 5 persistent link tables — not yet lifted
//!       onto `CodeGraph` (the current pass 5 output flows through the
//!       edge store). The row is reserved in the plan matrix; no impl is
//!       added here.
//!
//! # Other `CodeGraph` fields that are deliberately NOT in this matrix
//!
//! The following publish-visible fields on [`CodeGraph`] are intentionally
//! excluded because they do not reference `NodeId` at all; re-examining them
//! on every new field addition is cheaper than silently widening the matrix:
//!
//! - `strings: Arc<StringInterner>` — interned symbol bytes; keyed by
//!   [`StringId`], no `NodeId`.
//! - `edge_provenance: Arc<EdgeProvenanceStore>` — slot-aligned to the edge
//!   store; keyed by `EdgeId`, no `NodeId`.
//! - `scope_provenance_store: Arc<ScopeProvenanceStore>` — slot-aligned to
//!   the scope arena; keyed by `ScopeId`, no `NodeId`.
//! - `file_segments: Arc<FileSegmentTable>` — per-file `(start_slot, slot_count)`
//!   ranges; holds slot-index integers, not `NodeId`s.
//! - `fact_epoch: u64`, `epoch: u64`, `confidence: HashMap<String, ConfidenceMetadata>`
//!   — scalars / per-language metadata with no `NodeId` payload.
//!
//! When any of these fields gains a `NodeId`-bearing payload (for example, if
//! `ScopeProvenanceStore` is ever extended to carry the introducing node), a
//! new K.A row must be added to the plan + this matrix in the same commit.
//!
//! # Honest-scope note (A2 §K)
//!
//! `assert_impl_all!` only proves that the four K.A types and the single
//! active K.B1 type implement the trait. It does **not** prove that the
//! set of listed types is exhaustive — a new NodeId-bearing field
//! introduced without a new `assert_impl_all!` entry compiles cleanly.
//! Exhaustiveness is enforced by the four-legged regime spelled out in
//! the plan:
//!
//! 1. Code-owner rule on `concurrent/graph.rs`, `storage/indices.rs`, and
//!    this file;
//! 2. PR template checklist;
//! 3. The CI grep test at
//!    `sqry-core/tests/gate0b_coverage_matrix.rs`, which reads the plan
//!    and counts `assert_impl_all!` invocations against the K.A + active
//!    K.B rows;
//! 4. The §E equivalence harness as semantic backstop.
//!
//! # Iteration semantics
//!
//! Per A2 §F's tombstone-residue invariant
//! (plan lines 765–790): `all_node_ids()` must surface every `NodeId` the
//! container references so the residue check can assert none appears in
//! the drained tombstone set. For containers that track a single canonical
//! `NodeId` per logical entry (arena, metadata, per-file bucket), this
//! means "every `NodeId` currently in the container". For edge stores and
//! auxiliary indices, this means "every source + target `NodeId`", with
//! duplicates permitted (the residue check uses set membership).
//!
//! `retain_nodes(keep)` is the dual: remove every reference whose `keep`
//! predicate returns `false`. For derived-from-arena structures the
//! driver of truth is the arena's live set; for delta buffers and
//! indices we filter in place.

use super::super::bind::alias::AliasTable;
use super::super::bind::scope::ScopeArena;
use super::super::bind::shadow::ShadowTable;
use super::super::edge::BidirectionalEdgeStore;
use super::super::edge::delta::DeltaOp;
use super::super::node::NodeId;
use super::super::storage::arena::NodeArena;
use super::super::storage::indices::AuxiliaryIndices;
use super::super::storage::metadata::NodeMetadataStore;
use super::super::storage::node_provenance::NodeProvenanceStore;
use super::super::storage::registry::FileRegistry;

/// Unified interface for every publish-visible structure that stores
/// [`NodeId`]s (A2 §K).
///
/// The rebuild pipeline calls [`all_node_ids`](Self::all_node_ids) to
/// audit whether any tombstoned `NodeId` has leaked into the finalized
/// `CodeGraph`, and [`retain_nodes`](Self::retain_nodes) to drop every
/// reference whose `keep` predicate returns `false`.
///
/// The trait is `pub(crate)` until Gate 0c lifts the surface into a
/// daemon-only feature; external crates must not depend on it.
///
/// `#[allow(dead_code)]` is applied because Gate 0b delivers only the
/// scaffolding — the call sites in `RebuildGraph::finalize()` (Gate 0c)
/// and the debug-build residue check (Gate 0d) land in follow-up
/// commits. `assert_impl_all!` still enforces that every K.A/K.B row
/// implements the trait at compile time.
#[allow(dead_code)]
pub(crate) trait NodeIdBearing {
    /// Return every [`NodeId`] this container currently references,
    /// with duplicates permitted (the residue check uses
    /// `HashSet::contains`). The iterator borrows from `self` and may
    /// hold internal locks for the duration of iteration.
    fn all_node_ids(&self) -> Box<dyn Iterator<Item = NodeId> + '_>;

    /// Remove every reference whose `keep` predicate returns `false`.
    ///
    /// For containers where the truth-source is an external arena
    /// (e.g. CSR adjacency), this may tombstone derived state rather
    /// than mutating it in place; `finalize()` rebuilds the derived
    /// state in a later step. Implementations document their semantics
    /// on the impl block below.
    fn retain_nodes(&mut self, keep: &dyn Fn(NodeId) -> bool);
}

// ---------------------------------------------------------------------
// K.A1 — NodeArena
// ---------------------------------------------------------------------

impl NodeIdBearing for NodeArena {
    /// Yields one `NodeId` (with the slot's current generation) per
    /// occupied slot. Vacant slots are skipped because they carry no
    /// live `NodeId`. Callers that need tombstoned `NodeIds` must capture
    /// them *before* calling `NodeArena::remove` /
    /// `compact_tombstoned`.
    fn all_node_ids(&self) -> Box<dyn Iterator<Item = NodeId> + '_> {
        Box::new(self.iter().map(|(id, _entry)| id))
    }

    /// Drops every occupied slot whose `NodeId` fails `keep`, returning
    /// the slot to the free list and advancing its generation so any
    /// lingering `NodeId` handle becomes stale. Equivalent to calling
    /// `NodeArena::remove` for every predicate-rejected live ID.
    fn retain_nodes(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        let to_drop: Vec<NodeId> = self
            .iter()
            .filter_map(|(id, _entry)| (!keep(id)).then_some(id))
            .collect();
        for id in to_drop {
            // NodeArena::remove is idempotent; the caller may legitimately
            // have already removed some of these via a higher-level API.
            let _ = self.remove(id);
        }
    }
}

// ---------------------------------------------------------------------
// K.A2 + K.A3 — BidirectionalEdgeStore (forward + reverse tiers)
// ---------------------------------------------------------------------

impl NodeIdBearing for BidirectionalEdgeStore {
    /// Yields every `NodeId` referenced by any edge in either tier of
    /// either direction:
    ///
    /// - Delta buffer (both directions): source + target of every
    ///   `DeltaOp::Add` entry. `DeltaOp::Remove` entries are skipped
    ///   because they represent *deletions* — they cannot resurrect a
    ///   tombstoned `NodeId`.
    /// - CSR (both directions): every column-index entry. CSR rows are
    ///   keyed by arena slot index (no generation); the row axis is
    ///   therefore covered by [`NodeArena`]'s impl and not re-emitted
    ///   here to avoid fabricating a synthetic generation.
    ///
    /// Duplicates across tiers and directions are permitted; callers
    /// deduplicate via set membership.
    fn all_node_ids(&self) -> Box<dyn Iterator<Item = NodeId> + '_> {
        // Snapshot each tier into an owned Vec so the returned iterator
        // does not hold the inner `RwLock` guard for its lifetime.
        // NodeId is Copy so the clone is cheap, and finalize() only
        // invokes this helper once per rebuild.
        let mut out: Vec<NodeId> = Vec::new();

        {
            let forward = self.forward();
            for edge in forward.delta().iter() {
                if matches!(edge.op, DeltaOp::Add) {
                    out.push(edge.source);
                    out.push(edge.target);
                }
            }
            if let Some(csr) = forward.csr() {
                for node_idx in 0..csr.node_count() {
                    let node_idx = u32::try_from(node_idx).expect("CSR node index fits u32");
                    for edge_ref in csr.edges_of(node_idx) {
                        out.push(edge_ref.target);
                    }
                }
            }
        }

        {
            let reverse = self.reverse();
            for edge in reverse.delta().iter() {
                if matches!(edge.op, DeltaOp::Add) {
                    out.push(edge.source);
                    out.push(edge.target);
                }
            }
            if let Some(csr) = reverse.csr() {
                for node_idx in 0..csr.node_count() {
                    let node_idx = u32::try_from(node_idx).expect("CSR node index fits u32");
                    for edge_ref in csr.edges_of(node_idx) {
                        out.push(edge_ref.target);
                    }
                }
            }
        }

        Box::new(out.into_iter())
    }

    /// Filters the delta buffers of **both** the forward and reverse
    /// stores, dropping every edge whose source or target fails `keep`.
    /// The CSR tier is not mutated in place — `finalize()` rebuilds it
    /// from scratch in step 9, sourced from the arena's post-compaction
    /// live set, so CSR edges referencing tombstoned nodes are dropped
    /// transitively.
    fn retain_nodes(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        // Forward tier.
        {
            let mut forward = self.forward_mut();
            forward
                .delta_mut()
                .retain_if(|edge| keep(edge.source) && keep(edge.target));
        }
        // Reverse tier — reverse edges store (target → source) reversed,
        // so the `source` / `target` fields are swapped, but the same
        // predicate logic holds: drop if either endpoint is tombstoned.
        {
            let mut reverse = self.reverse_mut();
            reverse
                .delta_mut()
                .retain_if(|edge| keep(edge.source) && keep(edge.target));
        }
    }
}

// ---------------------------------------------------------------------
// K.A4 + K.A5 + K.A6 + K.A7 — AuxiliaryIndices (kind / name / qn / file)
// ---------------------------------------------------------------------

impl NodeIdBearing for AuxiliaryIndices {
    /// Yields the `NodeId`s stored in every inner bucket across all
    /// four indices (kind, name, qualified name, file). A single
    /// `NodeId` typically appears four times — once per index — which
    /// is harmless for the residue check.
    fn all_node_ids(&self) -> Box<dyn Iterator<Item = NodeId> + '_> {
        Box::new(self.iter_all_node_ids())
    }

    /// Applies `keep` to every bucket in every index, removing
    /// predicate-rejected `NodeIds`. Empty buckets are garbage-collected
    /// from their parent `BTreeMap` to keep iteration order and
    /// serialization bit-stable.
    fn retain_nodes(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        self.retain_node_ids(&keep);
    }
}

// ---------------------------------------------------------------------
// K.A8 — NodeMetadataStore
// ---------------------------------------------------------------------

impl NodeIdBearing for NodeMetadataStore {
    /// Yields the `NodeId` for every node carrying ANY metadata: the
    /// reconstructed key of every `(index, generation)` entry, unioned with the
    /// key of every node carrying only a shape descriptor.
    ///
    /// The shape-descriptor union is load-bearing for the tombstone-residue
    /// audit (`assert_no_tombstone_residue`): most functions carry a descriptor
    /// but no entry, so without it a descriptor-only stale `NodeId` left behind
    /// by a botched tombstone would slip past the audit unseen. Duplicates (a
    /// node with both an entry and a descriptor) are harmless — the residue
    /// check deduplicates via set membership.
    fn all_node_ids(&self) -> Box<dyn Iterator<Item = NodeId> + '_> {
        Box::new(
            self.iter_entries()
                .map(|((index, generation), _entry)| NodeId::new(index, generation))
                .chain(self.iter_shape_descriptor_node_ids()),
        )
    }

    /// Drops every metadata entry AND every shape descriptor whose
    /// reconstructed `NodeId` fails `keep`.
    ///
    /// Delegates to [`NodeMetadataStore::retain_entries`], which prunes both the
    /// entry map and the shape-descriptor map under the same predicate.
    fn retain_nodes(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        self.retain_entries(|index, generation| keep(NodeId::new(index, generation)));
    }
}

// ---------------------------------------------------------------------
// K.A10 — NodeProvenanceStore (dense slot-aligned node-level provenance)
// ---------------------------------------------------------------------

impl NodeIdBearing for NodeProvenanceStore {
    /// Yields a `NodeId` for every occupied slot, reconstructed from the
    /// slot's `(index, stored generation)` pair. Vacant slots are skipped.
    ///
    /// The result is produced in ascending slot-index order; callers that
    /// need set semantics should deduplicate via `HashSet` — the residue
    /// check does this automatically.
    fn all_node_ids(&self) -> Box<dyn Iterator<Item = NodeId> + '_> {
        Box::new(self.iter_node_ids())
    }

    /// Clears every slot whose reconstructed `NodeId` fails `keep`. Dense
    /// slot alignment with the parent `NodeArena` is preserved — rejected
    /// slots become `None` rather than being removed from the underlying
    /// `Vec`, so slot indices remain stable across compaction.
    fn retain_nodes(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        self.retain_by_node(&keep);
    }
}

// ---------------------------------------------------------------------
// K.A11 — ScopeArena (Phase 2 binding-plane scope arena)
// ---------------------------------------------------------------------

impl NodeIdBearing for ScopeArena {
    /// Yields the introducing `NodeId` (the `Scope.node` field) for every
    /// live scope in the arena. Vacant slots are skipped. The same
    /// `NodeId` may appear more than once if two sibling scopes happen to
    /// record the same introducing node (rare but legal); the residue
    /// check deduplicates via set membership.
    fn all_node_ids(&self) -> Box<dyn Iterator<Item = NodeId> + '_> {
        Box::new(self.iter_node_ids())
    }

    /// Frees every scope whose introducing node fails `keep`. Freed slots
    /// advance their generation counter and return to the free list, so
    /// stale `ScopeId` handles cannot alias into reused scopes.
    fn retain_nodes(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        self.retain_by_node(&keep);
    }
}

// ---------------------------------------------------------------------
// K.A12 — AliasTable (Phase 2 binding-plane alias table)
// ---------------------------------------------------------------------

impl NodeIdBearing for AliasTable {
    /// Yields the `import_node: NodeId` recorded on every alias entry in
    /// the table. Duplicates are permitted — the same import node can
    /// back several alias rows when a single `Import` edge expands to
    /// multiple alias targets in the same scope — and the residue check
    /// deduplicates via set membership.
    fn all_node_ids(&self) -> Box<dyn Iterator<Item = NodeId> + '_> {
        Box::new(self.entries().iter().map(|entry| entry.import_node))
    }

    /// Drops every alias entry whose `import_node` fails `keep`,
    /// reassigns dense `AliasEntryId`s to survivors, and rebuilds the
    /// `(scope, from_symbol)` range index so `resolve_alias` /
    /// `aliases_in` continue to return consistent results.
    fn retain_nodes(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        self.retain_by_node(&keep);
    }
}

// ---------------------------------------------------------------------
// K.A13 — ShadowTable (Phase 2 binding-plane shadow table)
// ---------------------------------------------------------------------

impl NodeIdBearing for ShadowTable {
    /// Yields the defining `node: NodeId` recorded on every shadow entry.
    /// Duplicates are permitted; the residue check deduplicates via set
    /// membership.
    fn all_node_ids(&self) -> Box<dyn Iterator<Item = NodeId> + '_> {
        Box::new(self.entries().iter().map(|entry| entry.node))
    }

    /// Drops every shadow entry whose defining node fails `keep`,
    /// reassigns dense `ShadowEntryId`s to survivors, and rebuilds the
    /// `(scope, symbol)` range index so `effective_binding` /
    /// `shadows_in` continue to return consistent results.
    fn retain_nodes(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        self.retain_by_node(&keep);
    }
}

// ---------------------------------------------------------------------
// K.B1 — FileRegistry
// ---------------------------------------------------------------------

impl NodeIdBearing for FileRegistry {
    /// Yields every `NodeId` stored in any `per_file_nodes` bucket.
    /// Duplicates across buckets are permitted; the residue check uses
    /// set membership. Returns an empty iterator when no buckets have
    /// been recorded yet (empty graph, or a legacy V7 snapshot whose
    /// buckets have not been rebuilt).
    ///
    /// Per Task 4 Gate 0c (pulled forward from base-plan Step 1), the
    /// `per_file_nodes` bucket is the real storage behind this impl;
    /// parallel-parse commit populates it via
    /// [`FileRegistry::record_node`].
    fn all_node_ids(&self) -> Box<dyn Iterator<Item = NodeId> + '_> {
        Box::new(self.iter_all_bucket_node_ids())
    }

    /// Filter every per-file bucket through `keep`; drop rejected IDs,
    /// dedup each bucket, and drop buckets that collapse to empty.
    ///
    /// Run by `RebuildGraph::finalize()` step 6 after the arena-level
    /// tombstone predicate has resolved (step 2). An empty input bucket
    /// state is handled as a no-op.
    fn retain_nodes(&mut self, keep: &dyn Fn(NodeId) -> bool) {
        self.retain_nodes_in_buckets(&keep);
    }
}

// ---------------------------------------------------------------------
// Compile-time coverage matrix (A2 §K, plan lines 530–541)
// ---------------------------------------------------------------------
//
// The CI grep test at `sqry-core/tests/gate0b_coverage_matrix.rs` parses
// this block and asserts it matches the rows listed in §K of the plan.
// Adding a row to §K without an `assert_impl_all!` line here — or vice
// versa — makes the grep test fail.

use static_assertions::assert_impl_all;

// --- K.A rows (eight Rust types, thirteen logical rows) ---------------
assert_impl_all!(NodeArena: NodeIdBearing); // K.A1
assert_impl_all!(BidirectionalEdgeStore: NodeIdBearing); // K.A2 + K.A3
assert_impl_all!(AuxiliaryIndices: NodeIdBearing); // K.A4 + K.A5 + K.A6 + K.A7
assert_impl_all!(NodeMetadataStore: NodeIdBearing); // K.A8
// K.A9 (CSR adjacency) is derived state, not a standalone row — see
// module docs above.
assert_impl_all!(NodeProvenanceStore: NodeIdBearing); // K.A10
assert_impl_all!(ScopeArena: NodeIdBearing); // K.A11
assert_impl_all!(AliasTable: NodeIdBearing); // K.A12
assert_impl_all!(ShadowTable: NodeIdBearing); // K.A13

// --- K.B rows (active) -----------------------------------------------
assert_impl_all!(FileRegistry: NodeIdBearing); // K.B1
// K.B2 (reverse-import index) is a computed view over the edge store;
// no standalone type exists, so no `assert_impl_all!` entry.
// K.B3 (future Pass 5 persistent link tables) is reserved for a future
// task; no type exists yet.

// ---------------------------------------------------------------------
// Unit tests — one per K.A + active K.B row per the plan's coverage
// requirement ("every K.A/K.B entry has at least one unit test
// exercising its `retain_nodes`").
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::super::edge::{EdgeKind, ResolvedVia};
    use super::super::super::file::FileId;
    use super::super::super::node::NodeKind;
    use super::super::super::storage::arena::NodeEntry;
    use super::super::super::storage::metadata::{MacroNodeMetadata, TypedMetadata};
    use super::super::super::string::id::StringId;
    use super::*;
    use std::collections::HashSet;
    use std::path::Path;

    // ---- K.A1: NodeArena -------------------------------------------

    #[test]
    fn arena_all_node_ids_enumerates_every_live_slot() {
        let mut arena = NodeArena::new();
        let name = StringId::new(0);
        let file = FileId::new(1);
        let a = arena
            .alloc(NodeEntry::new(NodeKind::Function, name, file))
            .unwrap();
        let b = arena
            .alloc(NodeEntry::new(NodeKind::Method, name, file))
            .unwrap();
        let c = arena
            .alloc(NodeEntry::new(NodeKind::Struct, name, file))
            .unwrap();
        let ids: HashSet<NodeId> = arena.all_node_ids().collect();
        assert_eq!(ids, HashSet::from([a, b, c]));
    }

    #[test]
    fn arena_retain_nodes_drops_rejected_live_slots() {
        let mut arena = NodeArena::new();
        let name = StringId::new(0);
        let file = FileId::new(1);
        let a = arena
            .alloc(NodeEntry::new(NodeKind::Function, name, file))
            .unwrap();
        let b = arena
            .alloc(NodeEntry::new(NodeKind::Method, name, file))
            .unwrap();
        let c = arena
            .alloc(NodeEntry::new(NodeKind::Struct, name, file))
            .unwrap();

        let keep_only_b = |id: NodeId| id == b;
        arena.retain_nodes(&keep_only_b);

        assert!(arena.get(a).is_none(), "A must be tombstoned");
        assert!(arena.get(b).is_some(), "B must remain");
        assert!(arena.get(c).is_none(), "C must be tombstoned");
        assert_eq!(arena.len(), 1);
        let survivors: HashSet<NodeId> = arena.all_node_ids().collect();
        assert_eq!(survivors, HashSet::from([b]));
    }

    // ---- K.A2 + K.A3: BidirectionalEdgeStore -----------------------

    fn sample_edge_kind() -> EdgeKind {
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        }
    }

    #[test]
    fn edge_store_all_node_ids_includes_delta_sources_and_targets() {
        let store = BidirectionalEdgeStore::new();
        let src = NodeId::new(1, 1);
        let tgt = NodeId::new(2, 1);
        let file = FileId::new(1);
        store.add_edge(src, tgt, sample_edge_kind(), file);

        let ids: HashSet<NodeId> = store.all_node_ids().collect();
        assert!(ids.contains(&src), "forward source present");
        assert!(ids.contains(&tgt), "forward target present");
        // Reverse delta stores (target, source) swapped; the enumeration
        // still emits both endpoints.
        assert_eq!(ids.len(), 2, "exactly the two endpoints, deduplicated");
    }

    #[test]
    fn edge_store_retain_nodes_drops_delta_edges_with_rejected_endpoints() {
        let mut store = BidirectionalEdgeStore::new();
        let keep_src = NodeId::new(10, 1);
        let keep_tgt = NodeId::new(11, 1);
        let drop_src = NodeId::new(12, 1);
        let drop_tgt = NodeId::new(13, 1);
        let file = FileId::new(1);

        store.add_edge(keep_src, keep_tgt, sample_edge_kind(), file);
        store.add_edge(drop_src, keep_tgt, sample_edge_kind(), file); // source tombstoned
        store.add_edge(keep_src, drop_tgt, sample_edge_kind(), file); // target tombstoned

        let live: HashSet<NodeId> = HashSet::from([keep_src, keep_tgt]);
        store.retain_nodes(&|id| live.contains(&id));

        let ids: HashSet<NodeId> = store.all_node_ids().collect();
        assert_eq!(ids, live, "only fully-live edges survive in either tier");
    }

    // ---- K.A4 + K.A5 + K.A6 + K.A7: AuxiliaryIndices ---------------

    #[test]
    fn indices_all_node_ids_surfaces_every_inner_bucket_entry() {
        let mut indices = AuxiliaryIndices::new();
        let a = NodeId::new(1, 1);
        let b = NodeId::new(2, 1);
        let name = StringId::new(0);
        let qname = StringId::new(1);
        let file = FileId::new(1);
        indices.add(a, NodeKind::Function, name, Some(qname), file);
        indices.add(b, NodeKind::Method, name, Some(qname), file);

        let ids: HashSet<NodeId> = indices.all_node_ids().collect();
        assert_eq!(ids, HashSet::from([a, b]));
    }

    #[test]
    fn indices_retain_nodes_drops_rejected_ids_from_all_four_indices() {
        let mut indices = AuxiliaryIndices::new();
        let keep = NodeId::new(1, 1);
        let drop = NodeId::new(2, 1);
        let name = StringId::new(0);
        let qname = StringId::new(1);
        let file = FileId::new(1);
        indices.add(keep, NodeKind::Function, name, Some(qname), file);
        indices.add(drop, NodeKind::Method, name, Some(qname), file);

        indices.retain_nodes(&|id| id == keep);

        let survivors: HashSet<NodeId> = indices.all_node_ids().collect();
        assert_eq!(survivors, HashSet::from([keep]));

        // The dropped NodeId must be gone from every inner index, not
        // just the one the caller happened to query.
        assert!(!indices.by_kind(NodeKind::Method).contains(&drop));
        assert!(!indices.by_name(name).contains(&drop));
        assert!(!indices.by_qualified_name(qname).contains(&drop));
        assert!(!indices.by_file(file).contains(&drop));

        // The surviving NodeId is still findable via every inner index.
        assert!(indices.by_kind(NodeKind::Function).contains(&keep));
        assert!(indices.by_name(name).contains(&keep));
        assert!(indices.by_qualified_name(qname).contains(&keep));
        assert!(indices.by_file(file).contains(&keep));
    }

    // ---- K.A8: NodeMetadataStore -----------------------------------

    #[test]
    fn metadata_all_node_ids_covers_every_entry() {
        let mut store = NodeMetadataStore::new();
        let a = NodeId::new(1, 1);
        let b = NodeId::new(2, 1);
        store.insert(a, MacroNodeMetadata::default());
        store.insert_typed(b, TypedMetadata::Macro(MacroNodeMetadata::default()));

        let ids: HashSet<NodeId> = store.all_node_ids().collect();
        assert_eq!(ids, HashSet::from([a, b]));
    }

    #[test]
    fn metadata_retain_nodes_drops_rejected_entries() {
        let mut store = NodeMetadataStore::new();
        let keep = NodeId::new(1, 1);
        let drop = NodeId::new(2, 1);
        store.insert(keep, MacroNodeMetadata::default());
        store.insert(drop, MacroNodeMetadata::default());

        store.retain_nodes(&|id| id == keep);

        assert_eq!(store.len(), 1);
        assert!(store.get_macro(keep).is_some());
        assert!(store.get_macro(drop).is_none());
    }

    #[test]
    fn metadata_all_node_ids_unions_descriptor_only_nodes() {
        // Hook 3b (read side): a node carrying ONLY a shape descriptor must be
        // surfaced by all_node_ids, or the tombstone-residue audit is blind to
        // a stranded descriptor-only stale NodeId.
        use crate::graph::unified::storage::shape::ShapeDescriptor;

        let entry_node = NodeId::new(1, 1);
        let descriptor_only = NodeId::new(2, 1);
        let mut store = NodeMetadataStore::new();
        store.insert(entry_node, MacroNodeMetadata::default());
        store.insert_shape_descriptor(descriptor_only, ShapeDescriptor::default());

        let ids: HashSet<NodeId> = store.all_node_ids().collect();
        assert!(ids.contains(&entry_node));
        assert!(
            ids.contains(&descriptor_only),
            "descriptor-only node must appear in the residue-audit set"
        );
    }

    #[test]
    fn metadata_retain_nodes_drops_descriptor_only_node() {
        // The residue-audit catch only works if retain ALSO prunes the
        // descriptor-only node it surfaced.
        use crate::graph::unified::storage::shape::ShapeDescriptor;

        let keep = NodeId::new(1, 1);
        let drop = NodeId::new(2, 1);
        let mut store = NodeMetadataStore::new();
        store.insert_shape_descriptor(keep, ShapeDescriptor::default());
        store.insert_shape_descriptor(drop, ShapeDescriptor::default());

        store.retain_nodes(&|id| id == keep);

        assert!(store.shape_descriptor(keep).is_some());
        assert!(
            store.shape_descriptor(drop).is_none(),
            "retain_nodes must drop the descriptor-only rejected node"
        );
    }

    // ---- K.A10: NodeProvenanceStore --------------------------------

    fn sample_provenance(
        epoch: u64,
        byte: u8,
    ) -> super::super::super::storage::node_provenance::NodeProvenance {
        super::super::super::storage::node_provenance::NodeProvenance::fresh(epoch, [byte; 32])
    }

    #[test]
    fn node_provenance_all_node_ids_uses_stored_generation_not_one() {
        let mut store = NodeProvenanceStore::new();
        let a = NodeId::new(0, 7);
        let b = NodeId::new(3, 42);
        store.insert(a, sample_provenance(100, 0xAA));
        store.insert(b, sample_provenance(200, 0xBB));

        let ids: HashSet<NodeId> = store.all_node_ids().collect();
        assert_eq!(
            ids,
            HashSet::from([a, b]),
            "NodeIds must be reconstructed from each slot's stored generation, \
             not hardcoded to generation 1"
        );
    }

    #[test]
    fn node_provenance_retain_nodes_clears_rejected_slots_and_keeps_slot_count() {
        let mut store = NodeProvenanceStore::new();
        let keep = NodeId::new(0, 1);
        let drop = NodeId::new(2, 1);
        store.insert(keep, sample_provenance(10, 0x01));
        store.insert(drop, sample_provenance(20, 0x02));
        let slots_before = store.slot_count();

        store.retain_nodes(&|id| id == keep);

        let survivors: HashSet<NodeId> = store.all_node_ids().collect();
        assert_eq!(survivors, HashSet::from([keep]));
        assert!(store.lookup(keep).is_some(), "kept provenance must survive");
        assert!(
            store.lookup(drop).is_none(),
            "rejected slot must report None on lookup"
        );
        assert_eq!(
            store.slot_count(),
            slots_before,
            "retain_nodes must preserve dense slot alignment with NodeArena"
        );
    }

    // ---- K.A11: ScopeArena -----------------------------------------

    fn sample_scope(node: NodeId) -> super::super::super::bind::scope::Scope {
        use super::super::super::bind::scope::{Scope, ScopeId, ScopeKind};
        Scope {
            kind: ScopeKind::Module,
            parent: ScopeId::INVALID,
            node,
            byte_span: (0, 32),
            file: FileId::new(0),
        }
    }

    #[test]
    fn scope_arena_all_node_ids_surfaces_every_scope_node() {
        let mut arena = ScopeArena::new();
        let a = NodeId::new(1, 2);
        let b = NodeId::new(5, 9);
        arena.allocate(sample_scope(a));
        arena.allocate(sample_scope(b));

        let ids: HashSet<NodeId> = arena.all_node_ids().collect();
        assert_eq!(ids, HashSet::from([a, b]));
    }

    #[test]
    fn scope_arena_retain_nodes_frees_rejected_scopes_and_advances_generation() {
        let mut arena = ScopeArena::new();
        let keep_node = NodeId::new(1, 1);
        let drop_node = NodeId::new(2, 1);
        let keep_id = arena.allocate(sample_scope(keep_node));
        let drop_id = arena.allocate(sample_scope(drop_node));

        arena.retain_nodes(&|id| id == keep_node);

        assert!(
            arena.get(keep_id).is_some(),
            "scope with kept node must survive"
        );
        assert!(
            arena.get(drop_id).is_none(),
            "scope with rejected node must be freed (stale handle)"
        );
        assert_eq!(arena.len(), 1);
        let survivors: HashSet<NodeId> = arena.all_node_ids().collect();
        assert_eq!(survivors, HashSet::from([keep_node]));
    }

    // ---- K.A12: AliasTable -----------------------------------------

    fn alias_entry(
        scope_ix: u32,
        from: u32,
        to: u32,
        import_node: NodeId,
    ) -> super::super::super::bind::alias::AliasEntry {
        use super::super::super::bind::alias::{AliasEntry, AliasEntryId};
        use super::super::super::bind::scope::ScopeId;
        AliasEntry {
            id: AliasEntryId(0),
            scope: ScopeId::new(scope_ix, 1),
            from_symbol: StringId::new(from),
            to_symbol: StringId::new(to),
            import_node,
            is_wildcard: false,
        }
    }

    fn alias_table_from_entries(
        mut entries: Vec<super::super::super::bind::alias::AliasEntry>,
    ) -> AliasTable {
        use super::super::super::bind::alias::AliasEntryId;
        entries.sort_by(|a, b| (a.scope, a.from_symbol).cmp(&(b.scope, b.from_symbol)));
        for (idx, entry) in entries.iter_mut().enumerate() {
            entry.id = AliasEntryId(u32::try_from(idx).unwrap());
        }
        // AliasTable serializes as `Vec<AliasEntry>` and rebuilds `by_scope`
        // on deserialize, so round-tripping the entries slice is the most
        // concise way to build a populated table from a test fixture
        // without touching the private fields directly.
        let encoded = postcard::to_allocvec(&entries).expect("encode entries");
        postcard::from_bytes(&encoded).expect("decode AliasTable")
    }

    #[test]
    fn alias_table_all_node_ids_returns_every_import_node() {
        let import_a = NodeId::new(1, 1);
        let import_b = NodeId::new(2, 3);
        let entries = vec![
            alias_entry(0, 10, 100, import_a),
            alias_entry(0, 20, 200, import_b),
        ];
        let table = alias_table_from_entries(entries);

        let ids: HashSet<NodeId> = table.all_node_ids().collect();
        assert_eq!(ids, HashSet::from([import_a, import_b]));
    }

    #[test]
    fn alias_table_retain_nodes_drops_rejected_entries_and_rebuilds_index() {
        use super::super::super::bind::alias::AliasEntryId;
        use super::super::super::bind::scope::ScopeId;

        let keep_node = NodeId::new(1, 1);
        let drop_node = NodeId::new(2, 1);
        let scope = ScopeId::new(0, 1);
        let entries = vec![
            alias_entry(0, 10, 100, keep_node),
            alias_entry(0, 20, 200, drop_node),
        ];
        let mut table = alias_table_from_entries(entries);

        table.retain_nodes(&|id| id == keep_node);

        // Survivor count collapses to 1.
        assert_eq!(table.len(), 1);

        // Dense ids must be reassigned to the new position.
        let entry = table.get(AliasEntryId(0)).expect("entry 0 must exist");
        assert_eq!(entry.import_node, keep_node);
        assert_eq!(entry.id, AliasEntryId(0));
        assert!(
            table.get(AliasEntryId(1)).is_none(),
            "survivor count is 1, id 1 must be vacant"
        );

        // by_scope index must be rebuilt so resolve_alias sees the survivor.
        let resolved = table.resolve_alias(scope, StringId::new(10));
        assert_eq!(
            resolved,
            Some(StringId::new(100)),
            "resolve_alias must still work after retain_nodes (by_scope rebuilt)"
        );
        // The rejected entry must be gone from the scope range too.
        assert_eq!(
            table.resolve_alias(scope, StringId::new(20)),
            None,
            "dropped alias must not resolve"
        );
    }

    // ---- K.A13: ShadowTable ----------------------------------------

    fn shadow_entry(
        scope_ix: u32,
        symbol: u32,
        node: NodeId,
        byte_offset: u32,
    ) -> super::super::super::bind::shadow::ShadowEntry {
        use super::super::super::bind::scope::ScopeId;
        use super::super::super::bind::shadow::{ShadowEntry, ShadowEntryId};
        ShadowEntry {
            id: ShadowEntryId(0),
            scope: ScopeId::new(scope_ix, 1),
            symbol: StringId::new(symbol),
            node,
            byte_offset,
        }
    }

    fn shadow_table_from_entries(
        mut entries: Vec<super::super::super::bind::shadow::ShadowEntry>,
    ) -> ShadowTable {
        use super::super::super::bind::shadow::ShadowEntryId;
        entries.sort_by(|a, b| {
            (a.scope, a.symbol, a.byte_offset).cmp(&(b.scope, b.symbol, b.byte_offset))
        });
        for (idx, entry) in entries.iter_mut().enumerate() {
            entry.id = ShadowEntryId(u32::try_from(idx).unwrap());
        }
        let encoded = postcard::to_allocvec(&entries).expect("encode entries");
        postcard::from_bytes(&encoded).expect("decode ShadowTable")
    }

    #[test]
    fn shadow_table_all_node_ids_returns_every_defining_node() {
        let a = NodeId::new(10, 1);
        let b = NodeId::new(11, 1);
        let entries = vec![shadow_entry(0, 1, a, 10), shadow_entry(0, 1, b, 30)];
        let table = shadow_table_from_entries(entries);

        let ids: HashSet<NodeId> = table.all_node_ids().collect();
        assert_eq!(ids, HashSet::from([a, b]));
    }

    #[test]
    fn shadow_table_retain_nodes_drops_rejected_entries_and_rebuilds_chains() {
        use super::super::super::bind::scope::ScopeId;
        use super::super::super::bind::shadow::ShadowEntryId;

        let keep = NodeId::new(10, 1);
        let drop = NodeId::new(11, 1);
        let scope = ScopeId::new(0, 1);
        let sym = StringId::new(1);
        let entries = vec![shadow_entry(0, 1, keep, 10), shadow_entry(0, 1, drop, 30)];
        let mut table = shadow_table_from_entries(entries);

        table.retain_nodes(&|id| id == keep);

        // Survivor count collapses to 1.
        assert_eq!(table.len(), 1);

        // Dense ids reassigned.
        let entry = table.get(ShadowEntryId(0)).expect("entry 0 must exist");
        assert_eq!(entry.node, keep);
        assert_eq!(entry.id, ShadowEntryId(0));

        // chains index must be rebuilt so effective_binding still finds the
        // surviving definition at an offset past it.
        assert_eq!(
            table.effective_binding(scope, sym, 40),
            Some(keep),
            "effective_binding must still resolve after retain_nodes (chains rebuilt)"
        );
        // And must NOT still see the dropped definition.
        assert_eq!(
            table.effective_binding(scope, sym, 20),
            Some(keep),
            "dropped definition at offset 30 must not shadow the kept def at 10"
        );
    }

    // ---- K.B1: FileRegistry ----------------------------------------

    #[test]
    fn file_registry_all_node_ids_empty_when_no_buckets_recorded() {
        // A registry with registered paths but no `record_node` calls
        // exposes no NodeIds — the empty-bucket contract that the
        // bucket-bijection check relies on.
        let mut reg = FileRegistry::new();
        reg.register(Path::new("/tmp/unused_fixture.rs")).unwrap();
        let ids: Vec<NodeId> = reg.all_node_ids().collect();
        assert!(
            ids.is_empty(),
            "FileRegistry has no NodeIds when no buckets are recorded"
        );
    }

    #[test]
    fn file_registry_all_node_ids_yields_every_recorded_bucket_entry() {
        let mut reg = FileRegistry::new();
        let file_a = crate::graph::unified::file::FileId::new(1);
        let file_b = crate::graph::unified::file::FileId::new(2);
        let n1 = NodeId::new(10, 1);
        let n2 = NodeId::new(11, 1);
        let n3 = NodeId::new(12, 1);
        reg.record_node(file_a, n1);
        reg.record_node(file_a, n2);
        reg.record_node(file_b, n3);

        let ids: HashSet<NodeId> = reg.all_node_ids().collect();
        assert_eq!(ids, HashSet::from([n1, n2, n3]));
    }

    #[test]
    fn file_registry_retain_nodes_drops_rejected_from_buckets() {
        let mut reg = FileRegistry::new();
        let file = crate::graph::unified::file::FileId::new(1);
        let keep = NodeId::new(10, 1);
        let drop = NodeId::new(11, 1);
        reg.record_node(file, keep);
        reg.record_node(file, drop);

        reg.retain_nodes(&|id| id == keep);

        let ids: HashSet<NodeId> = reg.all_node_ids().collect();
        assert_eq!(ids, HashSet::from([keep]));
    }

    #[test]
    fn file_registry_retain_nodes_preserves_file_slots() {
        // Bucket filtering must not touch file-path slots.
        let mut reg = FileRegistry::new();
        let fid = reg.register(Path::new("/tmp/unused_fixture.rs")).unwrap();
        reg.record_node(fid, NodeId::new(10, 1));
        let before = reg.len();
        reg.retain_nodes(&|_id| false); // reject everything
        assert_eq!(
            reg.len(),
            before,
            "retain_nodes must not touch file-slot accounting"
        );
        assert!(reg.resolve(fid).is_some(), "file path itself must survive");
    }
}
