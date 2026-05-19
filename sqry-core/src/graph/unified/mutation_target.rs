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
use crate::graph::unified::build::staging::GoHints;
use crate::graph::unified::edge::EdgeId;
use crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore;
use crate::graph::unified::node::NodeId;
use crate::graph::unified::storage::arena::NodeArena;
use crate::graph::unified::storage::edge_provenance::EdgeProvenanceStore;
use crate::graph::unified::storage::indices::AuxiliaryIndices;
use crate::graph::unified::storage::interner::StringInterner;
use crate::graph::unified::storage::metadata::NodeMetadataStore;
use crate::graph::unified::storage::node_provenance::NodeProvenanceStore;
use crate::graph::unified::storage::registry::FileRegistry;
use crate::graph::unified::storage::segment::FileSegmentTable;

// =======================================================================
// Receiver kind and Calls-edge metadata
// =======================================================================
//
// These types are defined here (rather than in the edge module or a Go
// plugin module) because they form part of the [`GraphMutationTarget`]
// trait surface — specifically the post-Phase-4d / post-Phase-4e
// `pass_go_method_set_satisfaction` pass needs an in-arena
// representation of Go's value-vs-pointer receiver distinction and a
// projected view of the `EdgeKind::Calls { argument_count, is_async }`
// payload, without coupling the trait to the full `EdgeKind` enum.

/// Pointer-vs-value receiver kind, per the Go spec §"Method sets".
///
/// Carried by the Go plugin's side-channel hints
/// ([`GoEmbeddingHint`][crate::graph::unified::build::staging::GoEmbeddingHint],
/// resolved receivers in the method-set pass) and used by the pass to
/// distinguish `MethodSet(T)` from `MethodSet(*T)` when computing
/// implicit interface satisfaction and promoted methods.
///
/// Public because the Go plugin crate (`sqry-lang-go`) reaches it
/// through [`StagingGraph::go_hints_mut`][crate::graph::unified::build::staging::StagingGraph::go_hints_mut]
/// during Phase-1 parse to record the syntactic receiver pointerness
/// at each emission site. Live constructors outside `sqry-core` land
/// with Cluster B (Go plugin emission); the variants are exercised in
/// the Cluster A unit tests via `Receiver::Value`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Receiver {
    /// `func (t T) M()` — value receiver.
    Value,
    /// `func (t *T) M()` — pointer receiver.
    Pointer,
}

/// Projected payload of an `EdgeKind::Calls { argument_count, is_async }`
/// edge, returned by [`GraphMutationTarget::calls_into`].
///
/// Decouples the pass from the full `EdgeKind` enum: the pass only needs
/// the call-site cardinality and async-ness to construct equivalent
/// shadow `Calls` edges targeting a promoted method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallsEdgeMeta {
    /// Number of arguments at the call site (0..=255).
    pub argument_count: u8,
    /// Whether the call expression is directly awaited (`.await` /
    /// equivalent in the source language).
    pub is_async: bool,
}

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

    // ===============================================================
    // Go-plugin pass extension surface (Go T1 implements-and-promotion,
    // Cluster A foundation).
    //
    // These four accessors are consumed by `pass_go_method_set_
    // satisfaction` (Clusters C–D) when it materialises synthetic
    // Method / Type nodes after Phase 4d's bulk-edge-insert and Phase
    // 4e's binding-plane derivation. None of the existing Pass 1–5
    // helpers calls them, so they land here under the same
    // `#[expect(dead_code)]` blanket that already covers other
    // Phase-3 / Phase-4 pre-staged surfaces; the attribute will be
    // narrowed when Cluster E wires the new pass into the entrypoint.
    // ===============================================================

    /// Insert `(qualified_name → NodeId)` entries for `new_nodes` into
    /// the live by-qualified-name index without rebuilding it from
    /// scratch.
    ///
    /// Used by post-Phase-4d / post-Phase-4e passes (e.g.
    /// `pass_go_method_set_satisfaction`) that materialise synthetic
    /// `Method` / `Type` nodes after the regular Phase 4c index rebuild
    /// has already run, and which therefore must extend the index
    /// O(`new_nodes`) at a time rather than O(arena).
    ///
    /// # Pre-conditions
    ///
    /// - Every `NodeId` in `new_nodes` must have been committed via
    ///   `add_node` / equivalent on this target before the call.
    /// - Each new node's `(qualified_name, kind)` tuple must either be
    ///   absent from the index or identical to an existing entry; this
    ///   method does NOT dedupe.
    /// - Nodes without a `qualified_name` (i.e., `qualified_name ==
    ///   None`) are skipped silently — they cannot be inserted into the
    ///   qualified-name bucket.
    fn rebuild_qualified_name_index_for_new_nodes(&mut self, new_nodes: &[NodeId]);

    /// Enumerate `(caller_node, edge_id, edge_meta)` triples for every
    /// existing `EdgeKind::Calls { .. }` edge whose target is `callee`.
    ///
    /// Implementations walk the bidirectional edge store's reverse
    /// adjacency (the CSR + delta merge maintained by
    /// [`BidirectionalEdgeStore::edges_to`]) and filter for `Calls`
    /// edges only.
    ///
    /// # Returned `EdgeId`
    ///
    /// The `EdgeId` carries the call edge's per-result-set identity
    /// (derived from its `StoreEdgeRef::seq` sequence number) so
    /// consumers can dedupe shadow emissions. It is **not** an index
    /// into [`EdgeProvenanceStore`][crate::graph::unified::storage::edge_provenance::EdgeProvenanceStore].
    /// The `seq` → `EdgeId` projection is stable within a single
    /// `calls_into` call but not across rebuilds.
    ///
    /// Returns an empty vector when `callee` has no incoming `Calls`
    /// edges or does not exist in the arena.
    fn calls_into(&self, callee: NodeId) -> Vec<(NodeId, EdgeId, CallsEdgeMeta)>;

    /// Shared borrow of the Go-plugin side-channel hint buffers.
    ///
    /// The hint buffers carry Phase-1 plugin observations
    /// (struct embeddings, named-type conversions, receiver-method
    /// call sites) that `pass_go_method_set_satisfaction` consumes
    /// after Phase 4e completes. Hints are build-time scratch state:
    /// they are populated during Phase 1 parse, merged into the live
    /// target during Phase 3, consumed by the pass, and reset before
    /// the next build.
    fn go_hints(&self) -> &GoHints;

    /// Mutable borrow of the Go-plugin side-channel hint buffers.
    ///
    /// Used by Phase 3's parallel-commit path to merge per-file
    /// [`StagingGraph::go_hints`][crate::graph::unified::build::staging::StagingGraph::go_hints]
    /// into the live target after NodeId / StringId remapping.
    fn go_hints_mut(&mut self) -> &mut GoHints;
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

    // --- Go-plugin pass extension ---

    fn rebuild_qualified_name_index_for_new_nodes(&mut self, new_nodes: &[NodeId]) {
        if new_nodes.is_empty() {
            return;
        }
        // Snapshot the per-node (kind, name, qualified_name, file)
        // tuple before reaching for `indices_mut`, so the `&NodeArena`
        // borrow does not overlap with the `&mut AuxiliaryIndices`
        // borrow required by `AuxiliaryIndices::add`.
        let mut tuples: Vec<(NodeId, _, _, Option<_>, _)> = Vec::with_capacity(new_nodes.len());
        {
            let arena: &NodeArena = self.nodes();
            for nid in new_nodes {
                if let Some(entry) = arena.get(*nid) {
                    tuples.push((
                        *nid,
                        entry.kind,
                        entry.name,
                        entry.qualified_name,
                        entry.file,
                    ));
                }
            }
        }
        let indices = self.indices_mut();
        for (nid, kind, name, qualified_name, file) in tuples {
            indices.add(nid, kind, name, qualified_name, file);
        }
    }

    fn calls_into(&self, callee: NodeId) -> Vec<(NodeId, EdgeId, CallsEdgeMeta)> {
        use crate::graph::unified::edge::EdgeKind;
        let edges_view = self.edges();
        edges_view
            .edges_to(callee)
            .into_iter()
            .filter_map(|edge_ref| {
                if let EdgeKind::Calls {
                    argument_count,
                    is_async,
                    ..
                } = edge_ref.kind
                {
                    // Project the per-edge sequence number onto
                    // `EdgeId`'s 32-bit handle. `seq` is monotonically
                    // increasing within a snapshot, so the projection
                    // is collision-free within a single `calls_into`
                    // result set (the only contract this method makes).
                    let edge_id = EdgeId::new(u32::try_from(edge_ref.seq).unwrap_or(u32::MAX));
                    Some((
                        edge_ref.source,
                        edge_id,
                        CallsEdgeMeta {
                            argument_count,
                            is_async,
                        },
                    ))
                } else {
                    None
                }
            })
            .collect()
    }

    fn go_hints(&self) -> &GoHints {
        &self.go_hints
    }

    fn go_hints_mut(&mut self) -> &mut GoHints {
        &mut self.go_hints
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

    // --- Go-plugin pass extension ---

    fn rebuild_qualified_name_index_for_new_nodes(&mut self, new_nodes: &[NodeId]) {
        // The rebuild plane rebuilds its `AuxiliaryIndices` from
        // scratch in `RebuildGraph::finalize` step 6 against the
        // compacted arena, so per-node index extension is unnecessary
        // here: any node committed via `add_node` on this target will
        // be re-indexed at finalize time. We mirror `CodeGraph`'s
        // behaviour all the same — defensive parity in case a future
        // caller invokes `rebuild_qualified_name_index_for_new_nodes`
        // mid-rebuild and expects the index to be queryable before
        // finalize runs.
        if new_nodes.is_empty() {
            return;
        }
        let mut tuples: Vec<(NodeId, _, _, Option<_>, _)> = Vec::with_capacity(new_nodes.len());
        for nid in new_nodes {
            if let Some(entry) = self.nodes.get(*nid) {
                tuples.push((
                    *nid,
                    entry.kind,
                    entry.name,
                    entry.qualified_name,
                    entry.file,
                ));
            }
        }
        for (nid, kind, name, qualified_name, file) in tuples {
            self.indices.add(nid, kind, name, qualified_name, file);
        }
    }

    fn calls_into(&self, callee: NodeId) -> Vec<(NodeId, EdgeId, CallsEdgeMeta)> {
        use crate::graph::unified::edge::EdgeKind;
        self.edges
            .edges_to(callee)
            .into_iter()
            .filter_map(|edge_ref| {
                if let EdgeKind::Calls {
                    argument_count,
                    is_async,
                    ..
                } = edge_ref.kind
                {
                    let edge_id = EdgeId::new(u32::try_from(edge_ref.seq).unwrap_or(u32::MAX));
                    Some((
                        edge_ref.source,
                        edge_id,
                        CallsEdgeMeta {
                            argument_count,
                            is_async,
                        },
                    ))
                } else {
                    None
                }
            })
            .collect()
    }

    fn go_hints(&self) -> &GoHints {
        &self.go_hints
    }

    fn go_hints_mut(&mut self) -> &mut GoHints {
        &mut self.go_hints
    }
}

// =======================================================================
// Unit tests
// =======================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::{EdgeKind, ResolvedVia};
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeId;
    use crate::graph::unified::node::kind::NodeKind;
    use crate::graph::unified::storage::NodeEntry;

    /// Helper: build a minimal `NodeEntry` with an interned name +
    /// qualified name, committed against `graph`.
    fn make_node(graph: &mut CodeGraph, kind: NodeKind, name: &str, qn: &str) -> NodeId {
        let name_id = graph.strings_mut().intern(name).expect("intern name");
        let qn_id = graph
            .strings_mut()
            .intern(qn)
            .expect("intern qualified name");
        let file = FileId::new(0);
        let entry = NodeEntry {
            kind,
            name: name_id,
            file,
            start_byte: 0,
            end_byte: 0,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
            signature: None,
            doc: None,
            qualified_name: Some(qn_id),
            visibility: None,
            is_async: false,
            is_static: false,
            body_hash: None,
            is_unsafe: false,
        };
        graph.nodes_mut().alloc(entry).expect("alloc node")
    }

    #[test]
    fn rebuild_qualified_name_index_for_new_nodes_inserts_into_live_index() {
        let mut graph = CodeGraph::new();
        // Materialise a Method node whose qualified name should
        // resolve via the by-qualified-name index after the targeted
        // extension call.
        let nid = make_node(&mut graph, NodeKind::Method, "M", "pkg.S.M");
        // Sanity: pre-call lookup must be empty (we never went through
        // Phase 4c).
        let qn_id = graph.strings().get("pkg.S.M").expect("qn must be interned");
        assert!(
            graph.indices().by_qualified_name(qn_id).is_empty(),
            "qualified-name bucket must be empty before the targeted update",
        );

        // Call through the trait surface, exercising the
        // GraphMutationTarget impl rather than any inherent helper.
        <CodeGraph as GraphMutationTarget>::rebuild_qualified_name_index_for_new_nodes(
            &mut graph,
            &[nid],
        );

        let bucket = graph.indices().by_qualified_name(qn_id);
        assert_eq!(
            bucket,
            &[nid],
            "the inserted node must resolve through the by-qualified-name index",
        );
    }

    #[test]
    fn calls_into_returns_calls_edges_with_metadata() {
        let mut graph = CodeGraph::new();
        let callee = make_node(&mut graph, NodeKind::Function, "callee", "pkg.callee");
        let caller_a = make_node(&mut graph, NodeKind::Function, "caller_a", "pkg.caller_a");
        let caller_b = make_node(&mut graph, NodeKind::Function, "caller_b", "pkg.caller_b");
        let unrelated = make_node(&mut graph, NodeKind::Function, "noise", "pkg.noise");
        let file = FileId::new(0);

        // Wire two distinct `Calls` edges into `callee` plus a
        // `References` edge that must be filtered out.
        graph.edges().add_edge(
            caller_a,
            callee,
            EdgeKind::Calls {
                argument_count: 2,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        graph.edges().add_edge(
            caller_b,
            callee,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: true,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        graph
            .edges()
            .add_edge(unrelated, callee, EdgeKind::References, file);

        let mut got = <CodeGraph as GraphMutationTarget>::calls_into(&graph, callee);
        // Order from `edges_to` is not contractual, sort for stable
        // assertion. The seq projection into `EdgeId` is opaque, so we
        // only assert that distinct edges yield distinct `EdgeId`s.
        got.sort_by_key(|(src, _, _)| src.index());

        let calls_only: Vec<(NodeId, CallsEdgeMeta)> =
            got.iter().map(|(s, _, m)| (*s, *m)).collect();
        // Sort the expected slice the same way for stable comparison.
        let mut expected = vec![
            (
                caller_a,
                CallsEdgeMeta {
                    argument_count: 2,
                    is_async: false,
                },
            ),
            (
                caller_b,
                CallsEdgeMeta {
                    argument_count: 0,
                    is_async: true,
                },
            ),
        ];
        expected.sort_by_key(|(src, _)| src.index());
        assert_eq!(calls_only, expected);

        // EdgeIds must be distinct between the two Calls edges.
        assert_ne!(
            got[0].1, got[1].1,
            "EdgeId projection must be injective within the returned set"
        );
        // The `References` edge must not appear.
        assert!(
            !got.iter().any(|(src, _, _)| *src == unrelated),
            "`calls_into` must filter out non-Calls edges (got References from {unrelated:?})",
        );
    }

    #[test]
    fn go_hints_accessor_pair_round_trips() {
        let mut graph = CodeGraph::new();
        assert!(
            <CodeGraph as GraphMutationTarget>::go_hints(&graph)
                .embeddings
                .is_empty(),
            "fresh CodeGraph must start with empty GoHints",
        );
        // Mutate via go_hints_mut, observe via go_hints.
        let hints = <CodeGraph as GraphMutationTarget>::go_hints_mut(&mut graph);
        hints
            .embeddings
            .push(crate::graph::unified::build::staging::GoEmbeddingHint {
                outer: NodeId::new(0, 1),
                inner_qualified_name: crate::graph::unified::StringId::new(0),
                pointerness: Receiver::Value,
                file: FileId::new(0),
            });
        assert_eq!(
            <CodeGraph as GraphMutationTarget>::go_hints(&graph)
                .embeddings
                .len(),
            1,
        );
    }
}
