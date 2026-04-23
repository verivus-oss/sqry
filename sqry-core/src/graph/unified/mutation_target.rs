//! [Task 4 Step 4 Phase 1+2] [`GraphMutationTarget`] — a mutation-plane
//! abstraction that lets every Pass 1-5 build-pipeline helper operate
//! uniformly on either [`CodeGraph`] (full-build path) or
//! [`RebuildGraph`] (incremental-rebuild path).
//!
//! # Why this trait exists
//!
//! The live build pipeline is `build_unified_graph_inner`
//! (`build/entrypoint.rs`) → Phase 1 parallel parse → Phase 2 range
//! assignment → Phase 3 parallel commit → Phase 4 finalization (4a
//! dedup, 4b remap, 4c index rebuild, 4c-prime unification, 4d edge
//! delta insert, 4e binding plane) → Pass 5 cross-language linking.
//!
//! Historically each helper in that pipeline took `&mut CodeGraph` (or
//! separate `&mut NodeArena` / `&mut StringInterner` borrows extracted
//! from a `CodeGraph`). The Task 4 incremental-rebuild dispatcher needs
//! to run the *same* helpers against a [`RebuildGraph`] — the owned,
//! rebuild-local mirror introduced by A2 §H Gate 0c. Without an
//! abstraction, every helper would need a `_rebuild` twin; with this
//! trait, each helper is generic over `G: GraphMutationTarget` and
//! dispatches through the trait's accessors.
//!
//! # Scope of the accessors
//!
//! The method list below is exhaustive against the mutation / read
//! surface that Pass 1-5 helpers actually touch. For every field on
//! [`sqry_graph_fields!`][crate::graph::unified::rebuild::sqry_graph_fields]
//! that any Pass 1-5 helper reads or writes there is a corresponding
//! trait method here:
//!
//! * `*_mut` accessors return a `&mut` borrow to the underlying field.
//! * Plain-named `*()` accessors (Phase 2 addition) return a `&` borrow
//!   so helpers can intermix read + write on the same graph via a
//!   single trait bound.
//! * `set_*` methods (Phase 2 addition) replace whole-component values
//!   (used by `derive_binding_plane` to install freshly derived scope
//!   arena / alias table / shadow table / scope provenance store).
//! * Disjoint-borrow combinators (`nodes_and_strings_mut`,
//!   `nodes_and_indices_mut`) mirror those on [`CodeGraph`] so callers
//!   that need two non-aliasing borrows at the same time do not have
//!   to reinvent the split.
//!
//! Adding a new field to [`CodeGraph`] (and the `sqry_graph_fields!`
//! macro list) therefore requires three touches:
//!
//! 1. declare the field on [`CodeGraph`] in
//!    `concurrent/graph.rs`,
//! 2. add it to the `sqry_graph_fields!` body in
//!    `rebuild/rebuild_graph.rs` (this is enforced by the
//!    `let CodeGraph { .. } = self;` exhaustive destructure — missing
//!    step 2 is a hard `E0027`),
//! 3. add matching accessor(s) on this trait and route both `impl`s
//!    (step 3 is only required once a pipeline helper actually
//!    reads/writes the new field; for inert scalars like the epoch
//!    counters it is optional until a helper needs them).
//!
//! # Visibility
//!
//! The trait itself is [`pub(crate)`]. External crates (including
//! `sqry-daemon` even with `rebuild-internals` enabled) cannot name
//! `GraphMutationTarget`; the daemon continues to reach the rebuild
//! surface only through [`CodeGraph::clone_for_rebuild`] +
//! [`RebuildGraph::finalize`]. A compile-fail fixture in
//! `tests/rebuild_internals_compile_fail/` pins that invariant.
//!
//! # Migration status
//!
//! As of Task 4 Step 4 Phase 2, every Pass 1-5 helper listed in the
//! plan routes through this trait:
//!
//! * `phase3_parallel_commit` (Phase 1) — parallel commit.
//! * `phase4c_prime_unify_cross_file_nodes` (Phase 2) — cross-file
//!   node unification.
//! * `rebuild_indices` free function (Phase 2) — auxiliary-index
//!   rebuild; the inherent [`CodeGraph::rebuild_indices`] now delegates
//!   to it.
//! * `phase4d_bulk_insert_edges` (Phase 2) — bulk edge delta
//!   installation. Wraps the pure [`pending_edges_to_delta`] +
//!   [`BidirectionalEdgeStore::add_edges_bulk_ordered`] pair that
//!   previously lived inline in `build_unified_graph_inner`.
//! * `derive_binding_plane` and `derive_binding_plane_incremental`
//!   (Phase 2) — scope / alias / shadow / provenance derivation.
//! * `link_cross_language_edges` (Phase 2) — Pass 5 cross-language
//!   linking (FFI + HTTP).
//!
//! [`CodeGraph`]: crate::graph::unified::concurrent::CodeGraph
//! [`RebuildGraph`]: crate::graph::unified::rebuild::rebuild_graph::RebuildGraph
//! [`pending_edges_to_delta`]: crate::graph::unified::build::pending_edges_to_delta
//! [`BidirectionalEdgeStore::add_edges_bulk_ordered`]: crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore::add_edges_bulk_ordered

use std::collections::HashMap;

use crate::confidence::ConfidenceMetadata;
use crate::graph::unified::bind::alias::AliasTable;
use crate::graph::unified::bind::scope::ScopeArena;
use crate::graph::unified::bind::scope::provenance::ScopeProvenanceStore;
use crate::graph::unified::bind::shadow::ShadowTable;
use crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore;
use crate::graph::unified::storage::arena::NodeArena;
use crate::graph::unified::storage::edge_provenance::EdgeProvenanceStore;
use crate::graph::unified::storage::indices::AuxiliaryIndices;
use crate::graph::unified::storage::interner::StringInterner;
use crate::graph::unified::storage::metadata::NodeMetadataStore;
use crate::graph::unified::storage::node_provenance::NodeProvenanceStore;
use crate::graph::unified::storage::registry::FileRegistry;
use crate::graph::unified::storage::segment::FileSegmentTable;

/// Mutation-plane abstraction over the two graph kinds that the build
/// pipeline writes into: [`CodeGraph`] (full rebuild) and
/// [`RebuildGraph`] (incremental rebuild).
///
/// See the [module-level documentation][self] for why this trait
/// exists and what migration stage it is currently at.
///
/// # Invariants
///
/// - Every accessor returns a borrow of *exactly one* underlying
///   field; there are no cross-field mutations hidden inside an
///   accessor. Implementations may materialise shared state
///   (`Arc::make_mut`) before returning a `&mut` borrow, but the
///   resulting borrow is otherwise a direct reference to the field
///   the method names.
/// - `set_*` methods replace the whole component on the implementor.
///   On [`CodeGraph`] the replacement is wrapped in a fresh `Arc`;
///   on [`RebuildGraph`] the owned field is assigned directly. The
///   net observable state is identical in both cases.
/// - The disjoint-borrow combinators
///   ([`nodes_and_strings_mut`][Self::nodes_and_strings_mut],
///   [`nodes_and_indices_mut`][Self::nodes_and_indices_mut])
///   must return two borrows that are provably non-aliasing at the
///   type-system level; implementations route through
///   field-destructuring so the borrow checker admits the split.
///
/// # Visibility
///
/// `pub(crate)` by design. External crates cannot name this trait, so
/// they cannot write a new implementation that smuggles mutations
/// into the graph's interior. The Pass 1-5 helpers that consume this
/// trait are themselves intra-crate, so crate visibility is
/// sufficient.
///
/// [`CodeGraph`]: crate::graph::unified::concurrent::CodeGraph
/// [`RebuildGraph`]: crate::graph::unified::rebuild::rebuild_graph::RebuildGraph
// The trait surface includes accessors that the currently migrated
// Pass 1-5 helpers do not yet consume — notably the `_mut` accessors
// for `macro_metadata`, `node_provenance`, `edge_provenance`,
// `file_segments`, every binding-plane `*_mut`, `fact_epoch_mut`,
// `epoch_mut`, `confidence_mut`, and `strings_mut`. These methods are
// intentionally declared now because Task 4 Step 4 Phase 3's real
// `incremental_rebuild` body needs them to drive steps that mutate
// those surfaces directly (metadata tombstone fix-ups, epoch bump,
// confidence merges, etc.). Gating their declarations on Phase 3
// would either force Phase 3 to re-land all of Phase 2's trait
// surface or create a churn-prone partial trait.
//
// `expect(dead_code)` asserts the warning exists today so Phase 3
// landing is forced to revisit the attribute the moment those
// accessors go live. The `reason` string spells out exactly which
// helpers are expected to consume them.
#[expect(
    dead_code,
    reason = "Phase 2 migration surface: `macro_metadata_mut`, the three \
              provenance `_mut`s, every binding-plane `_mut`, scalar \
              `_mut`s, and `strings_mut` are Phase 3 (real incremental \
              rebuild body) coverage. Remove this `expect` in the Phase 3 \
              landing PR once they are wired up."
)]
pub(crate) trait GraphMutationTarget {
    // ===============================================================
    // Immutable accessors (Phase 2 addition).
    //
    // Pass 1-5 helpers frequently read the graph (scan the node arena,
    // walk the edge delta, look up file registry entries, etc.) on the
    // same call where they also write. Exposing `&self` variants here
    // lets those helpers be generic over `<G: GraphMutationTarget>`
    // without reaching through the concrete type.
    // ===============================================================

    /// Shared borrow of the node arena (`nodes` field).
    fn nodes(&self) -> &NodeArena;

    /// Shared borrow of the bidirectional edge store (`edges` field).
    ///
    /// `BidirectionalEdgeStore` exposes interior-mutability
    /// (`forward_mut(&self)`, `reverse_mut(&self)`), so returning
    /// `&BidirectionalEdgeStore` from `&self` is technically enough
    /// for a sufficiently-determined caller to mutate. The trait is
    /// `pub(crate)` which contains the risk; for any new external
    /// surface, wrap in a read-only view struct instead.
    fn edges(&self) -> &BidirectionalEdgeStore;

    /// Shared borrow of the string interner (`strings` field).
    fn strings(&self) -> &StringInterner;

    /// Shared borrow of the file registry (`files` field).
    fn files(&self) -> &FileRegistry;

    /// Shared borrow of the auxiliary indices (`indices` field).
    fn indices(&self) -> &AuxiliaryIndices;

    /// Shared borrow of the binding-plane scope arena (`scope_arena`
    /// field).
    fn scope_arena(&self) -> &ScopeArena;

    /// Monotonic fact-layer epoch stamped on the most recently saved
    /// or loaded snapshot (`fact_epoch` field).
    fn fact_epoch(&self) -> u64;

    // ===============================================================
    // Storage components (arena + CSR + interner + registries)
    // — mutable side.
    // ===============================================================

    /// Mutable borrow of the node arena (`nodes` field on
    /// [`CodeGraph`] / [`RebuildGraph`]).
    ///
    /// On [`CodeGraph`] this materialises the shared `Arc<NodeArena>`
    /// via `Arc::make_mut`. On [`RebuildGraph`] it is a direct
    /// `&mut NodeArena` to the owned rebuild-local field.
    ///
    /// [`CodeGraph`]: crate::graph::unified::concurrent::CodeGraph
    /// [`RebuildGraph`]: crate::graph::unified::rebuild::rebuild_graph::RebuildGraph
    fn nodes_mut(&mut self) -> &mut NodeArena;

    /// Mutable borrow of the bidirectional edge store (`edges`
    /// field).
    fn edges_mut(&mut self) -> &mut BidirectionalEdgeStore;

    /// Mutable borrow of the string interner (`strings` field).
    fn strings_mut(&mut self) -> &mut StringInterner;

    /// Mutable borrow of the file registry (`files` field).
    fn files_mut(&mut self) -> &mut FileRegistry;

    /// Mutable borrow of the auxiliary indices (`indices` field).
    fn indices_mut(&mut self) -> &mut AuxiliaryIndices;

    /// Mutable borrow of the macro/classpath metadata store
    /// (`macro_metadata` field).
    fn macro_metadata_mut(&mut self) -> &mut NodeMetadataStore;

    /// Mutable borrow of the node provenance store
    /// (`node_provenance` field).
    fn node_provenance_mut(&mut self) -> &mut NodeProvenanceStore;

    /// Mutable borrow of the edge provenance store
    /// (`edge_provenance` field).
    fn edge_provenance_mut(&mut self) -> &mut EdgeProvenanceStore;

    /// Mutable borrow of the file segment table (`file_segments`
    /// field).
    fn file_segments_mut(&mut self) -> &mut FileSegmentTable;

    // ---------------------------------------------------------------
    // Binding plane (Phase 4e)
    // ---------------------------------------------------------------

    /// Mutable borrow of the binding-plane scope arena (`scope_arena`
    /// field).
    fn scope_arena_mut(&mut self) -> &mut ScopeArena;

    /// Mutable borrow of the binding-plane alias table (`alias_table`
    /// field).
    fn alias_table_mut(&mut self) -> &mut AliasTable;

    /// Mutable borrow of the binding-plane shadow table
    /// (`shadow_table` field).
    fn shadow_table_mut(&mut self) -> &mut ShadowTable;

    /// Mutable borrow of the binding-plane scope provenance store
    /// (`scope_provenance_store` field).
    fn scope_provenance_store_mut(&mut self) -> &mut ScopeProvenanceStore;

    /// Replace the scope arena with a freshly-derived one.
    ///
    /// Phase 4e's `derive_binding_plane` calls this after running
    /// `derive_scopes`. The inherent `CodeGraph::set_scope_arena` is
    /// `pub(crate)` and wraps the arena in `Arc::new`; on
    /// `RebuildGraph` the owned field is assigned directly.
    fn set_scope_arena(&mut self, arena: ScopeArena);

    /// Replace the alias table with a freshly-derived one (Phase 4e).
    fn set_alias_table(&mut self, table: AliasTable);

    /// Replace the shadow table with a freshly-derived one (Phase 4e).
    fn set_shadow_table(&mut self, table: ShadowTable);

    /// Replace the scope provenance store with a freshly-derived one
    /// (Phase 4e, P2U11 stamping).
    fn set_scope_provenance_store(&mut self, store: ScopeProvenanceStore);

    // ---------------------------------------------------------------
    // Scalar + metadata fields
    // ---------------------------------------------------------------

    /// Mutable borrow of the monotonic fact-layer epoch (`fact_epoch`
    /// field).
    fn fact_epoch_mut(&mut self) -> &mut u64;

    /// Mutable borrow of the version-tracking epoch (`epoch` field).
    fn epoch_mut(&mut self) -> &mut u64;

    /// Mutable borrow of the per-language confidence metadata map
    /// (`confidence` field).
    fn confidence_mut(&mut self) -> &mut HashMap<String, ConfidenceMetadata>;

    // ---------------------------------------------------------------
    // Disjoint-borrow combinators
    // ---------------------------------------------------------------

    /// Mutable disjoint borrows of the node arena and the string
    /// interner in a single call.
    ///
    /// Phase 3 (`phase3_parallel_commit`) needs both borrows
    /// simultaneously so it can pre-split the arena and interner
    /// slices with `split_at_mut` under a single top-level borrow.
    /// Without this combinator, calling `nodes_mut()` followed by
    /// `strings_mut()` would fail the borrow check because both
    /// borrows would be live against `&mut self` at once.
    ///
    /// Implementations must guarantee the two returned borrows are
    /// provably disjoint at the type-system level; the existing
    /// [`CodeGraph::nodes_and_strings_mut`] does this by producing
    /// both `Arc::make_mut` results in one expression, and the
    /// [`RebuildGraph`] impl does it by destructuring the owned
    /// fields directly.
    ///
    /// [`CodeGraph::nodes_and_strings_mut`]: crate::graph::unified::concurrent::CodeGraph::nodes_and_strings_mut
    /// [`RebuildGraph`]: crate::graph::unified::rebuild::rebuild_graph::RebuildGraph
    fn nodes_and_strings_mut(&mut self) -> (&mut NodeArena, &mut StringInterner);

    /// Shared borrow of the node arena paired with a mutable borrow
    /// of the auxiliary indices.
    ///
    /// Phase 4c (`rebuild_indices` free function) reads the arena and
    /// writes the indices in the same expression:
    ///
    /// ```ignore
    /// let (arena, indices) = graph.nodes_and_indices_mut();
    /// indices.build_from_arena(arena);
    /// ```
    ///
    /// Calling `nodes()` then `indices_mut()` would fail the borrow
    /// check because the shared borrow would be live against
    /// `&self` while `&mut self` is required for `indices_mut`. Each
    /// implementor destructures the owned / Arc-wrapped fields
    /// directly so the two borrows are provably disjoint.
    fn nodes_and_indices_mut(&mut self) -> (&NodeArena, &mut AuxiliaryIndices);
}

// =======================================================================
// impl GraphMutationTarget for CodeGraph
// =======================================================================

use crate::graph::unified::concurrent::CodeGraph;
use std::sync::Arc;

impl GraphMutationTarget for CodeGraph {
    // --- Immutable ---

    fn nodes(&self) -> &NodeArena {
        CodeGraph::nodes(self)
    }

    fn edges(&self) -> &BidirectionalEdgeStore {
        CodeGraph::edges(self)
    }

    fn strings(&self) -> &StringInterner {
        CodeGraph::strings(self)
    }

    fn files(&self) -> &FileRegistry {
        CodeGraph::files(self)
    }

    fn indices(&self) -> &AuxiliaryIndices {
        CodeGraph::indices(self)
    }

    fn scope_arena(&self) -> &ScopeArena {
        CodeGraph::scope_arena(self)
    }

    fn fact_epoch(&self) -> u64 {
        CodeGraph::fact_epoch(self)
    }

    // --- Storage (mutable) ---

    fn nodes_mut(&mut self) -> &mut NodeArena {
        // Delegate to the existing `pub fn nodes_mut` on CodeGraph —
        // that method already performs `Arc::make_mut`.
        CodeGraph::nodes_mut(self)
    }

    fn edges_mut(&mut self) -> &mut BidirectionalEdgeStore {
        CodeGraph::edges_mut(self)
    }

    fn strings_mut(&mut self) -> &mut StringInterner {
        CodeGraph::strings_mut(self)
    }

    fn files_mut(&mut self) -> &mut FileRegistry {
        CodeGraph::files_mut(self)
    }

    fn indices_mut(&mut self) -> &mut AuxiliaryIndices {
        CodeGraph::indices_mut(self)
    }

    fn macro_metadata_mut(&mut self) -> &mut NodeMetadataStore {
        CodeGraph::macro_metadata_mut(self)
    }

    fn node_provenance_mut(&mut self) -> &mut NodeProvenanceStore {
        // No existing public accessor — reach through the `pub(crate)`
        // field directly using `Arc::make_mut` for COW semantics.
        Arc::make_mut(&mut self.node_provenance)
    }

    fn edge_provenance_mut(&mut self) -> &mut EdgeProvenanceStore {
        Arc::make_mut(&mut self.edge_provenance)
    }

    fn file_segments_mut(&mut self) -> &mut FileSegmentTable {
        CodeGraph::file_segments_mut(self)
    }

    // --- Binding plane ---

    fn scope_arena_mut(&mut self) -> &mut ScopeArena {
        Arc::make_mut(&mut self.scope_arena)
    }

    fn alias_table_mut(&mut self) -> &mut AliasTable {
        Arc::make_mut(&mut self.alias_table)
    }

    fn shadow_table_mut(&mut self) -> &mut ShadowTable {
        Arc::make_mut(&mut self.shadow_table)
    }

    fn scope_provenance_store_mut(&mut self) -> &mut ScopeProvenanceStore {
        Arc::make_mut(&mut self.scope_provenance_store)
    }

    fn set_scope_arena(&mut self, arena: ScopeArena) {
        // Route through the inherent pub(crate) setter so the
        // Arc-wrap semantics stay in one place.
        CodeGraph::set_scope_arena(self, arena);
    }

    fn set_alias_table(&mut self, table: AliasTable) {
        CodeGraph::set_alias_table(self, table);
    }

    fn set_shadow_table(&mut self, table: ShadowTable) {
        CodeGraph::set_shadow_table(self, table);
    }

    fn set_scope_provenance_store(&mut self, store: ScopeProvenanceStore) {
        CodeGraph::set_scope_provenance_store(self, store);
    }

    // --- Scalars + metadata ---

    fn fact_epoch_mut(&mut self) -> &mut u64 {
        &mut self.fact_epoch
    }

    fn epoch_mut(&mut self) -> &mut u64 {
        &mut self.epoch
    }

    fn confidence_mut(&mut self) -> &mut HashMap<String, ConfidenceMetadata> {
        &mut self.confidence
    }

    // --- Disjoint combinators ---

    fn nodes_and_strings_mut(&mut self) -> (&mut NodeArena, &mut StringInterner) {
        CodeGraph::nodes_and_strings_mut(self)
    }

    fn nodes_and_indices_mut(&mut self) -> (&NodeArena, &mut AuxiliaryIndices) {
        // Destructure `self` so the borrow checker sees two disjoint
        // fields. `Arc::make_mut` on `indices` materialises a unique
        // handle; `&*nodes` reborrows the `Arc<NodeArena>` as a plain
        // shared reference, which does not conflict with the mutable
        // borrow of `indices`.
        let Self { nodes, indices, .. } = self;
        (&**nodes, Arc::make_mut(indices))
    }
}

// =======================================================================
// impl GraphMutationTarget for RebuildGraph
// =======================================================================

use crate::graph::unified::rebuild::rebuild_graph::RebuildGraph;

impl GraphMutationTarget for RebuildGraph {
    // --- Immutable ---

    fn nodes(&self) -> &NodeArena {
        &self.nodes
    }

    fn edges(&self) -> &BidirectionalEdgeStore {
        // `RebuildGraph` deliberately does not expose a public `edges`
        // accessor (see the comment in `rebuild_graph.rs` around the
        // `edges` field: `BidirectionalEdgeStore` exposes interior
        // mutability). The trait impl is intra-crate (`pub(crate)`
        // trait) so the exposure is bounded and matches the `CodeGraph`
        // parity that every Pass 1-5 helper expects.
        &self.edges
    }

    fn strings(&self) -> &StringInterner {
        &self.strings
    }

    fn files(&self) -> &FileRegistry {
        &self.files
    }

    fn indices(&self) -> &AuxiliaryIndices {
        &self.indices
    }

    fn scope_arena(&self) -> &ScopeArena {
        &self.scope_arena
    }

    fn fact_epoch(&self) -> u64 {
        self.fact_epoch
    }

    // --- Storage (mutable) ---

    fn nodes_mut(&mut self) -> &mut NodeArena {
        // `RebuildGraph` owns the field directly (no Arc wrapper) —
        // per the `sqry_graph_fields!(@decl_rebuild)` declaration in
        // `rebuild/rebuild_graph.rs`. A plain `&mut` borrow is
        // sufficient.
        &mut self.nodes
    }

    fn edges_mut(&mut self) -> &mut BidirectionalEdgeStore {
        &mut self.edges
    }

    fn strings_mut(&mut self) -> &mut StringInterner {
        &mut self.strings
    }

    fn files_mut(&mut self) -> &mut FileRegistry {
        &mut self.files
    }

    fn indices_mut(&mut self) -> &mut AuxiliaryIndices {
        &mut self.indices
    }

    fn macro_metadata_mut(&mut self) -> &mut NodeMetadataStore {
        &mut self.macro_metadata
    }

    fn node_provenance_mut(&mut self) -> &mut NodeProvenanceStore {
        &mut self.node_provenance
    }

    fn edge_provenance_mut(&mut self) -> &mut EdgeProvenanceStore {
        &mut self.edge_provenance
    }

    fn file_segments_mut(&mut self) -> &mut FileSegmentTable {
        &mut self.file_segments
    }

    // --- Binding plane ---

    fn scope_arena_mut(&mut self) -> &mut ScopeArena {
        &mut self.scope_arena
    }

    fn alias_table_mut(&mut self) -> &mut AliasTable {
        &mut self.alias_table
    }

    fn shadow_table_mut(&mut self) -> &mut ShadowTable {
        &mut self.shadow_table
    }

    fn scope_provenance_store_mut(&mut self) -> &mut ScopeProvenanceStore {
        &mut self.scope_provenance_store
    }

    fn set_scope_arena(&mut self, arena: ScopeArena) {
        self.scope_arena = arena;
    }

    fn set_alias_table(&mut self, table: AliasTable) {
        self.alias_table = table;
    }

    fn set_shadow_table(&mut self, table: ShadowTable) {
        self.shadow_table = table;
    }

    fn set_scope_provenance_store(&mut self, store: ScopeProvenanceStore) {
        self.scope_provenance_store = store;
    }

    // --- Scalars + metadata ---

    fn fact_epoch_mut(&mut self) -> &mut u64 {
        &mut self.fact_epoch
    }

    fn epoch_mut(&mut self) -> &mut u64 {
        &mut self.epoch
    }

    fn confidence_mut(&mut self) -> &mut HashMap<String, ConfidenceMetadata> {
        &mut self.confidence
    }

    // --- Disjoint combinators ---

    fn nodes_and_strings_mut(&mut self) -> (&mut NodeArena, &mut StringInterner) {
        // `RebuildGraph` owns both fields directly, so a single
        // destructure yields two provably-disjoint borrows.
        let Self { nodes, strings, .. } = self;
        (nodes, strings)
    }

    fn nodes_and_indices_mut(&mut self) -> (&NodeArena, &mut AuxiliaryIndices) {
        let Self { nodes, indices, .. } = self;
        (nodes, indices)
    }
}
