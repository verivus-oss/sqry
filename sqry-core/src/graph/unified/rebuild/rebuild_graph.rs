//! [A2 §H] `RebuildGraph` + `clone_for_rebuild` + `finalize()` — Gate 0c.
//!
//! **This module is feature-gated.** Every item lives behind
//! `#[cfg(feature = "rebuild-internals")]`; only `sqry-daemon` is
//! whitelisted to enable the feature (see
//! `sqry-core/tests/rebuild_internals_whitelist.rs` and the plan at
//! `docs/superpowers/plans/2026-03-19-sqryd-daemon.md` §H "Placement
//! and feature gate").
//!
//! # What this module delivers
//!
//! Per the plan's A2 §H (pre-implementation Gate 0c, required before
//! any `clone_for_rebuild` caller in Task 4 Step 4 can exist):
//!
//! 1. [`sqry_graph_fields!`] — a **single source of truth** macro that
//!    enumerates every field on [`super::super::concurrent::CodeGraph`].
//!    It is invoked in three places by the rest of this module:
//!      - `sqry_graph_fields!(@decl_rebuild)` declares the
//!        [`RebuildGraph`] struct.
//!      - `sqry_graph_fields!(@clone_inner from self)` is expanded
//!        inside [`CodeGraph::clone_for_rebuild`]; the destructure
//!        `let CodeGraph { .. } = self;` is exhaustive, so adding a
//!        field to [`CodeGraph`] without adding it to the macro's
//!        field list is a hard `E0027` compile error.
//!      - `sqry_graph_fields!(@field_names)` emits a `&[&str]` of
//!        every field name, used by the Gate 0d bijection check
//!        (future) and by diagnostics.
//! 2. [`CodeGraph::clone_for_rebuild`] — deep-copies every Arc-wrapped
//!    field into an owned, rebuild-local [`RebuildGraph`]. The rebuild
//!    dispatcher (Task 4 Step 4) is the only caller.
//! 3. [`RebuildGraph::finalize`] — the canonical 14-step sequence
//!    spelled out in §H lines 658–707. Every step is either an API
//!    call on an owned component (freeze interner, compact arena,
//!    compact edges, compact indices, compact metadata, compact file
//!    buckets, compact K-list extras), a drain/move of scratch state
//!    (drain tombstones), a derived-state reset (rebuild CSR), a
//!    per-language metadata update (confidence), a scalar bump (epoch),
//!    a struct assembly (`Arc::new` wrapping), or a debug invariant
//!    (bucket bijection + tombstone residue).
//!
//! # Why the macro is kind-tagged, not a single callback
//!
//! The plan's literal example invokes `sqry_graph_fields!(CodeGraph,
//! RebuildGraph)` to generate both structs from one sibling macro
//! call. That shape would require `CodeGraph` to be declared by the
//! macro — but `CodeGraph` has dozens of `impl` blocks, Clone
//! semantics, and accessor methods that already destructure the fields
//! by name on stable Rust. Moving the declaration into a macro
//! expansion would obscure every one of those sites.
//!
//! A callback-based idiom (`sqry_graph_fields!(my_cb!)`) was also
//! considered; it worked for the struct declaration but collapsed
//! under `macro_rules!` hygiene constraints for the destructure-and-
//! construct clone path (the `self` keyword cannot be synthesised by
//! a macro; passing identifiers across a callback boundary requires
//! forwarding them through an `:expr` metavar, which loses the
//! token-level decomposition needed for Arc-vs-scalar dispatch).
//!
//! The **kind-tagged single macro** settled on here expands inline at
//! the call site — `sqry_graph_fields!(@clone_inner from self)` —
//! so the destructure runs in the caller's hygiene context, the
//! `self` keyword is the caller's `self`, and the per-field Arc /
//! scalar / owned dispatch is baked into the macro body without a
//! secondary callback hop.
//!
//! The single-source-of-truth guarantee is the same: adding a field
//! requires touching (a) the `CodeGraph` struct in
//! `concurrent/graph.rs`, (b) the field list inside this macro, and
//! (c) the step in `finalize()` that compacts it. Missing any of
//! (a) or (b) makes `clone_for_rebuild_inner` fail to compile.
//!
//! # Release-build posture
//!
//! The debug bucket-bijection and tombstone-residue checks at steps 13
//! and 14 are gated on `cfg(any(debug_assertions, test))`. Release
//! builds pay zero overhead. The §E equivalence harness in CI is the
//! release-time drift backstop (plan §F "Release build semantics").

// Module-level `#[cfg(feature = "rebuild-internals")]` lives on the
// `pub mod rebuild_graph;` declaration in the parent `mod.rs`; duplicating
// it here trips the `duplicated_attributes` clippy lint.
#![allow(clippy::too_many_lines)]

use std::collections::{HashMap, HashSet};

use crate::confidence::ConfidenceMetadata;
use crate::graph::error::GraphResult;
use crate::graph::unified::bind::alias::AliasTable;
use crate::graph::unified::bind::scope::ScopeArena;
use crate::graph::unified::bind::scope::provenance::ScopeProvenanceStore;
use crate::graph::unified::bind::shadow::ShadowTable;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore;
use crate::graph::unified::node::NodeId;
use crate::graph::unified::storage::arena::NodeArena;
use crate::graph::unified::storage::c_indirect::CIndirectSideTables;
use crate::graph::unified::storage::edge_provenance::EdgeProvenanceStore;
use crate::graph::unified::storage::indices::AuxiliaryIndices;
use crate::graph::unified::storage::interner::StringInterner;
use crate::graph::unified::storage::metadata::NodeMetadataStore;
use crate::graph::unified::storage::node_provenance::NodeProvenanceStore;
use crate::graph::unified::storage::registry::FileRegistry;
use crate::graph::unified::storage::segment::FileSegmentTable;

use super::coverage::NodeIdBearing;

// =====================================================================
// Single source of truth: field list
// =====================================================================

/// Single source of truth for the [`CodeGraph`] field list.
///
/// This macro takes a **kind-tag** selector (`@decl_rebuild`,
/// `@clone_inner from ...`, `@field_names`) and emits a tailored
/// expansion for each consumer. The field-list rows live exactly once,
/// inside this macro body, so any new field is added in one place:
/// here.
///
/// # Adding / renaming / removing a field on [`CodeGraph`]
///
/// 1. Edit the `CodeGraph` struct in
///    `sqry-core/src/graph/unified/concurrent/graph.rs` (declaration
///    order matters — match the order here).
/// 2. Edit the field list below. The `@decl_rebuild` arm auto-mirrors
///    into `RebuildGraph`; the `@clone_inner` arm auto-mirrors into
///    `clone_for_rebuild_inner`.
/// 3. Extend `RebuildGraph::finalize()` with the step that compacts
///    the new field (and, if it carries `NodeId`s, add a K.A/K.B row
///    + `NodeIdBearing` impl in `super::coverage`).
///
/// Missing step (1) or (2) is a hard `E0027` compile error in the
/// `let CodeGraph { .. } = ...` destructure inside the `@clone_inner`
/// arm. Step (3) is enforced by the §F tombstone-residue check
/// (Gate 0d) and by the §E incremental-vs-full equivalence harness.
#[macro_export]
macro_rules! sqry_graph_fields {
    // -------------------------------------------------------------
    // Arm @decl_rebuild: emit the `RebuildGraph` struct declaration.
    // -------------------------------------------------------------
    (@decl_rebuild) => {
        /// Owned, rebuild-local mirror of [`CodeGraph`].
        ///
        /// Each field stores a value type rather than an [`Arc`], so
        /// the rebuild task can mutate freely without touching the
        /// live published graph. The only path to produce a
        /// publishable [`CodeGraph`] from a [`RebuildGraph`] is
        /// [`RebuildGraph::finalize`]; the trybuild fixture at
        /// `sqry-core/tests/rebuild_internals_compile_fail/rebuild_graph_no_public_assembly.rs`
        /// locks that invariant in.
        pub struct RebuildGraph {
            pub(crate) nodes: NodeArena,
            pub(crate) edges: BidirectionalEdgeStore,
            pub(crate) strings: StringInterner,
            pub(crate) files: FileRegistry,
            pub(crate) indices: AuxiliaryIndices,
            pub(crate) macro_metadata: NodeMetadataStore,
            pub(crate) node_provenance: NodeProvenanceStore,
            pub(crate) edge_provenance: EdgeProvenanceStore,
            pub(crate) fact_epoch: u64,
            pub(crate) epoch: u64,
            pub(crate) confidence: HashMap<String, ConfidenceMetadata>,
            pub(crate) scope_arena: ScopeArena,
            pub(crate) alias_table: AliasTable,
            pub(crate) shadow_table: ShadowTable,
            pub(crate) scope_provenance_store: ScopeProvenanceStore,
            pub(crate) file_segments: FileSegmentTable,
            /// Phase A (U09): C indirect-call resolver side tables.
            /// `None` outside C workspaces; cloned through the rebuild
            /// pipeline so an in-flight rebuild preserves the side
            /// tables already committed on the source graph.
            pub(crate) c_indirect_tables: Option<CIndirectSideTables>,
            /// Build-time scratch side-channel for the Go plugin's
            /// post-Phase-4e `pass_go_method_set_satisfaction` (Cluster
            /// A of the Go T1 implements-and-promotion design). Mirror
            /// of `CodeGraph::go_hints`; held by value (no Arc) for
            /// the same rationale as on `CodeGraph`.
            pub(crate) go_hints: $crate::graph::unified::build::staging::GoHints,
            /// Active tombstone set during finalize steps 2–7.
            /// Populated by `RebuildGraph::remove_file` /
            /// `RebuildGraph::tombstone` (added by Task 4 Step 4) and
            /// drained into [`drained_tombstones`] at step 8.
            pub(crate) tombstones: HashSet<NodeId>,
            /// Snapshot of [`tombstones`] taken at finalize step 8,
            /// kept for the debug residue check in step 14.
            pub(crate) drained_tombstones: HashSet<NodeId>,
        }
    };

    // -------------------------------------------------------------
    // Arm @clone_inner: emit the body of `clone_for_rebuild_inner`.
    //
    // The caller passes their `self` binding as `$this:expr` so
    // macro hygiene preserves the method-local reference. The
    // destructure is exhaustive — adding a `CodeGraph` field without
    // also adding it here is a hard `E0027` compile error. Removing
    // a row here without removing the `CodeGraph` field is an unused-
    // pattern warning that our `-D warnings` CI setting escalates.
    // -------------------------------------------------------------
    (@clone_inner from $this:expr) => {{
        let CodeGraph {
            nodes,
            edges,
            strings,
            files,
            indices,
            macro_metadata,
            node_provenance,
            edge_provenance,
            fact_epoch,
            epoch,
            confidence,
            scope_arena,
            alias_table,
            shadow_table,
            scope_provenance_store,
            file_segments,
            c_indirect_tables,
            go_hints,
        } = $this;
        RebuildGraph {
            nodes: (**nodes).clone(),
            edges: (**edges).clone(),
            strings: (**strings).clone(),
            files: (**files).clone(),
            indices: (**indices).clone(),
            macro_metadata: (**macro_metadata).clone(),
            node_provenance: (**node_provenance).clone(),
            edge_provenance: (**edge_provenance).clone(),
            fact_epoch: *fact_epoch,
            epoch: *epoch,
            confidence: confidence.clone(),
            scope_arena: (**scope_arena).clone(),
            alias_table: (**alias_table).clone(),
            shadow_table: (**shadow_table).clone(),
            scope_provenance_store: (**scope_provenance_store).clone(),
            file_segments: (**file_segments).clone(),
            c_indirect_tables: c_indirect_tables.clone(),
            go_hints: go_hints.clone(),
            tombstones: ::std::collections::HashSet::new(),
            drained_tombstones: ::std::collections::HashSet::new(),
        }
    }};

    // -------------------------------------------------------------
    // Arm @field_names: emit a comma-separated list of every field
    // name. Used by tests that want to assert coverage of field names
    // against a declared list without re-parsing Rust source.
    // -------------------------------------------------------------
    (@field_names) => {
        &[
            "nodes",
            "edges",
            "strings",
            "files",
            "indices",
            "macro_metadata",
            "node_provenance",
            "edge_provenance",
            "fact_epoch",
            "epoch",
            "confidence",
            "scope_arena",
            "alias_table",
            "shadow_table",
            "scope_provenance_store",
            "file_segments",
            "c_indirect_tables",
            "go_hints",
        ]
    };
}

// =====================================================================
// RebuildGraph struct declaration (macro-driven)
// =====================================================================

sqry_graph_fields!(@decl_rebuild);

// =====================================================================
// clone_for_rebuild: CodeGraph -> RebuildGraph
// =====================================================================

impl CodeGraph {
    /// Produce a fresh [`RebuildGraph`] from this graph's current
    /// committed state, deep-cloning every Arc-wrapped component so the
    /// returned value is decoupled from future mutations to `self`.
    ///
    /// The exhaustive destructure inside the `@clone_inner` arm of
    /// [`sqry_graph_fields!`] guarantees that every field on
    /// [`CodeGraph`] is mirrored into the returned [`RebuildGraph`].
    /// Adding a field to [`CodeGraph`] without also adding it to the
    /// field list inside `sqry_graph_fields!` is a hard `E0027`
    /// compile error on the `let CodeGraph { .. } = self;` destructure.
    ///
    /// # When to call this
    ///
    /// Only the incremental-rebuild dispatcher (Task 4 Step 4) calls
    /// `clone_for_rebuild`, on the rebuild task's background tokio
    /// context, against an `Arc<CodeGraph>` freshly obtained via
    /// `ArcSwap::load_full()`. The Arc's refcount is 1 in the common
    /// case, so the underlying deep clones amount to `Arc::get_mut`-
    /// equivalent cost on each component.
    ///
    /// # Performance budget (A2 §H, line 734)
    ///
    /// On a 384k-node / 1.3M-edge reference graph, this call must
    /// complete in < 50 ms. The daemon's rebuild-latency benchmark
    /// tracks the budget; warning threshold 50 ms, hard record
    /// threshold 200 ms. Exceeding the warning logs but does not fail
    /// the rebuild.
    #[must_use]
    pub fn clone_for_rebuild(&self) -> RebuildGraph {
        Self::clone_for_rebuild_inner(self)
    }

    /// Inner implementation, kept separate so the public entrypoint is
    /// a single-line shim that also makes the exhaustive destructure
    /// visible at the top of the rebuild surface.
    ///
    /// The `@clone_inner from self` macro arm emits the destructure +
    /// struct-construction inline; passing `self` as an `:expr`
    /// metavar preserves call-site hygiene so the macro-introduced
    /// field bindings (`nodes`, `edges`, ...) are resolvable against
    /// the `self` binding in this method.
    fn clone_for_rebuild_inner(&self) -> RebuildGraph {
        sqry_graph_fields!(@clone_inner from self)
    }
}

// =====================================================================
// finalize(): RebuildGraph -> CodeGraph — the 14-step contract
// =====================================================================

impl RebuildGraph {
    /// Consume this [`RebuildGraph`] and assemble a publishable
    /// [`CodeGraph`].
    ///
    /// This is the **only** safe path from [`RebuildGraph`] back to
    /// [`CodeGraph`]. No public API converts a [`RebuildGraph`] into
    /// an [`Arc<CodeGraph>`] any other way, and no
    /// `From<RebuildGraph> for CodeGraph` impl exists — the trybuild
    /// fixture at
    /// `sqry-core/tests/rebuild_internals_compile_fail/rebuild_graph_no_public_assembly.rs`
    /// locks that property in.
    ///
    /// # The 14 ordered steps (plan §H lines 658–707)
    ///
    /// Each numbered comment below traces the matching plan step. In
    /// debug / test builds, steps 13 and 14 also execute the §F
    /// bijection and tombstone-residue checks; release builds compile
    /// them out.
    ///
    /// Steps 2–7 uniformly invoke
    /// [`super::coverage::NodeIdBearing::retain_nodes`] with the
    /// closure `|nid| !self.tombstones.contains(&nid)`, so adding a new
    /// K.A or K.B row for a future `NodeId`-bearing container
    /// automatically extends compaction coverage once the new row's
    /// `retain_nodes` call is appended to step 7.
    ///
    /// # Errors
    ///
    /// Returns `Err(GraphBuilderError::Internal{..})` only if a
    /// compaction primitive fails; all current primitives are
    /// infallible, so `finalize()` is currently infallible. The `Result`
    /// return is preserved for future fallible compaction steps without
    /// a signature change.
    pub fn finalize(mut self) -> GraphResult<CodeGraph> {
        // ----------------------------------------------------------------
        // Step 1 — Freeze the rebuild's interner.
        //
        // The plan §H describes step 1 as `new_strings =
        // self.string_builder.freeze()`. In this codebase the
        // rebuild-local interner is a [`StringInterner`] (no separate
        // "builder" type), so the concrete freeze operation is:
        //
        //   (a) canonicalise the interner by running
        //       [`StringInterner::build_dedup_table`]. This guarantees:
        //         * `lookup_stale == false` (required for all read-path
        //           accessors); any in-flight bulk-alloc residue from a
        //           prior failed rebuild is healed.
        //         * Every string value is backed by exactly one
        //           canonical slot — no duplicate arcs remain. The
        //           `ref_counts` of duplicate slots are folded into
        //           their canonical slot. `StringId` values held by
        //           live `NodeEntry`s remain valid because this pass
        //           only collapses slots that are *not* referenced from
        //           live nodes (nothing in this pass renames already
        //           live slots).
        //   (b) rewrite every live `NodeEntry` through the remap table
        //       produced by (a) so any node whose fields happen to
        //       point at a now-collapsed duplicate slot is rewired to
        //       the canonical slot. This preserves the post-freeze
        //       invariant "every live `NodeEntry` references only
        //       canonical `StringId`s" even if earlier rebuild steps
        //       produced duplicate slots.
        //   (c) prune unreferenced slots with
        //       [`StringInterner::recycle_unreferenced`] so the frozen
        //       interner carries only strings the final graph actually
        //       needs. This is the step that drives
        //       `interner_live_ratio < interner_compaction_threshold`
        //       down over time (plan §H line 713) so the next
        //       housekeeping rebuild observes an accurate live ratio.
        //
        // After this block runs, `self.strings` is in a fully
        // canonical, shared-immutable-ready state. Step 12's
        // `Arc::new(new_strings)` is then purely the ownership-to-
        // shared transition; the semantic freeze itself has already
        // happened here.
        // ----------------------------------------------------------------
        let string_remap = self.strings.build_dedup_table();
        if !string_remap.is_empty() {
            // Rewrite every live node's interned-string fields through
            // the remap so freshly-collapsed duplicate slots are not
            // reachable from any live `NodeEntry`. This is required
            // for (c) below to be safe to call: `recycle_unreferenced`
            // only frees slots with `ref_count == 0`, so the refcount
            // bookkeeping that `build_dedup_table` consolidated into
            // the canonical slot must be honoured by node-level fields
            // as well.
            rewrite_node_entries_through_remap(&mut self.nodes, &string_remap);
            // Iter-2 B1 (verbatim): `AuxiliaryIndices::name_index` /
            // `qualified_name_index` are keyed by `StringId`. If
            // `build_dedup_table()` collapses a key's slot and
            // `recycle_unreferenced` subsequently frees it, the bucket
            // keys would dangle. Remap every `StringId`-backed holder
            // on the rebuild through the same dedup table before the
            // recycle pass runs, merging any buckets that collapse onto
            // the same canonical key.
            //
            // All remap-capable stores are listed here exhaustively.
            // Extending this block when a new `CodeGraph` field gains a
            // `StringId` payload is a code-owner obligation tied to the
            // `sqry_graph_fields!` macro (plan §H "Placement and feature
            // gate"). Today, the StringId-bearing surfaces on the
            // publish-visible graph are:
            //
            //   * `AuxiliaryIndices.name_index` / `qualified_name_index`
            //     (BTreeMap keys — collapse on canonicalisation)
            //   * `FileRegistry` — `FileEntry.source_uri: Option<StringId>`
            //     per slot
            //   * `AliasTable` — `AliasEntry.from_symbol` /
            //     `to_symbol` per entry; the `(scope, from_symbol)`
            //     sort key is re-established after rewrite
            //   * `ShadowTable` — `ShadowEntry.symbol` per entry; the
            //     `(scope, symbol, byte_offset)` sort key and the
            //     `chains` range index are rebuilt after rewrite
            //
            // `NodeArena` (above) is the fifth surface; it is handled
            // inline because it is the single largest consumer and
            // already has a purpose-built helper.
            //
            // Iter-3 B1 (verbatim): `BidirectionalEdgeStore` holds
            // `EdgeKind` instances whose variants — `Imports{alias}`,
            // `Exports{alias}`, `TypeOf{name}`, `TraitMethodBinding
            // {trait_name, impl_type}`, `HttpRequest{url}`,
            // `GrpcCall{service, method}`, `DbQuery{table}`,
            // `TableRead{table_name, schema}`, `TableWrite{table_name,
            // schema}`, `TriggeredBy{trigger_name, schema}`,
            // `MessageQueue{protocol::Other(_), topic}`,
            // `WebSocket{event}`, `GraphQLOperation{operation}`,
            // `ProcessExec{command}`, `FileIpc{path_pattern}`,
            // `ProtocolCall{protocol, metadata}` — carry
            // `StringId` payloads inside both the steady-state CSR
            // (`CsrGraph::edge_kind`) and the mutable delta
            // (`DeltaBuffer` `DeltaEdge::kind`), in **both** the
            // forward and reverse stores. Before this iter-4 fix,
            // step 1 left those payloads un-remapped — so after
            // `recycle_unreferenced` below, those edges would have
            // dangled onto freed interner slots. The full-build
            // pipeline recognises this surface explicitly at
            // `build/entrypoint.rs` (Phase 4b) +
            // `build/parallel_commit.rs::phase4_apply_global_remap`;
            // the rebuild pipeline must do the same against
            // committed storage because, unlike Phase 4b, the edges
            // are no longer a `Vec<Vec<PendingEdge>>`.
            //
            // `BidirectionalEdgeStore::rewrite_edge_kind_string_ids_through_remap`
            // is the sixth surface on this list. The exhaustive match
            // on `EdgeKind` lives in
            // `parallel_commit::remap_edge_kind_string_ids` so adding a
            // new `StringId`-bearing variant becomes a single source of
            // truth: a compile error there forces both pipelines to
            // rewrite it.
            //
            // Stores whose data is keyed by `NodeId`/`FileId`/`EdgeId`
            // only (metadata keyed on NodeId, node / edge provenance,
            // scope arena indexed by ScopeId, scope provenance,
            // file-segments keyed on FileId/EdgeId) do not hold
            // `StringId` payloads and are out of scope for this
            // rewrite. Any future field that lifts a `StringId` onto
            // `CodeGraph` must extend this list in the same commit
            // that declares it, matching the A2 §K discipline.
            self.indices.rewrite_string_ids_through_remap(&string_remap);
            self.files.rewrite_string_ids_through_remap(&string_remap);
            self.alias_table
                .rewrite_string_ids_through_remap(&string_remap);
            self.shadow_table
                .rewrite_string_ids_through_remap(&string_remap);
            // Iter-4 K.B1 fix: rewrite `StringId` payloads carried by
            // committed `EdgeKind`s in both forward and reverse stores,
            // across both CSR and delta tiers. Must run before
            // `recycle_unreferenced` below so freed slots stay unreferenced.
            self.edges
                .rewrite_edge_kind_string_ids_through_remap(&string_remap);
        }
        self.strings.recycle_unreferenced();

        // ----------------------------------------------------------------
        // Step 2 — Compact NodeArena.
        //
        // K.A1 row: `NodeArena::retain_nodes` drops every occupied slot
        // whose NodeId fails the predicate, advancing the slot's
        // generation so lingering NodeId handles become stale.
        // ----------------------------------------------------------------
        {
            let tombstones = &self.tombstones;
            let predicate: Box<dyn Fn(NodeId) -> bool + '_> =
                Box::new(move |nid| !tombstones.contains(&nid));
            self.nodes.retain_nodes(&*predicate);
        }

        // ----------------------------------------------------------------
        // Step 3 — Compact BidirectionalEdgeStore (forward + reverse).
        //
        // K.A2 + K.A3 rows: `retain_nodes` drops every delta edge with
        // a tombstoned endpoint in either direction. CSR compaction is
        // deferred to step 9 (CSR is derived state, rebuilt not mutated).
        // ----------------------------------------------------------------
        {
            let tombstones = &self.tombstones;
            let predicate: Box<dyn Fn(NodeId) -> bool + '_> =
                Box::new(move |nid| !tombstones.contains(&nid));
            self.edges.retain_nodes(&*predicate);
        }

        // ----------------------------------------------------------------
        // Step 4 — Compact AuxiliaryIndices.
        //
        // K.A4–K.A7 rows (kind / name / qualified-name / file indices):
        // a single `AuxiliaryIndices::retain_nodes` call visits every
        // inner bucket.
        // ----------------------------------------------------------------
        {
            let tombstones = &self.tombstones;
            let predicate: Box<dyn Fn(NodeId) -> bool + '_> =
                Box::new(move |nid| !tombstones.contains(&nid));
            self.indices.retain_nodes(&*predicate);
        }

        // ----------------------------------------------------------------
        // Step 5 — Compact NodeMetadataStore.
        //
        // K.A8 row: macro / classpath per-node metadata keyed by the
        // full `(index, generation)` pair.
        // ----------------------------------------------------------------
        {
            let tombstones = &self.tombstones;
            let predicate: Box<dyn Fn(NodeId) -> bool + '_> =
                Box::new(move |nid| !tombstones.contains(&nid));
            self.macro_metadata.retain_nodes(&*predicate);
        }

        // ----------------------------------------------------------------
        // Step 6 — Compact FileRegistry per-file buckets.
        //
        // K.B1 row. Iter-2 B2 (verbatim): this step used to be a
        // no-op scheduled against a future base-plan Step 1. Pulling
        // `per_file_nodes: HashMap<FileId, Vec<NodeId>>` forward into
        // Gate 0c retires the no-op — every `NodeId` the rebuild's
        // parallel-parse pass committed is already bucketed via
        // `FileRegistry::record_node`, so this step now does real work:
        //
        //   * drop every `NodeId` whose arena slot was tombstoned in
        //     step 2 (or earlier) via the shared predicate
        //   * dedup each surviving bucket
        //   * drop buckets that collapse to empty
        //
        // The §F.1 bucket-bijection check at step 13 consumes the
        // result of this compaction, so a mis-applied predicate here
        // surfaces as a panic at the publish boundary in debug builds.
        // ----------------------------------------------------------------
        {
            let tombstones = &self.tombstones;
            let predicate: Box<dyn Fn(NodeId) -> bool + '_> =
                Box::new(move |nid| !tombstones.contains(&nid));
            self.files.retain_nodes(&*predicate);
        }

        // ----------------------------------------------------------------
        // Step 7 — Compact K-list extras.
        //
        // Rows K.A10 (node_provenance), K.A11 (scope_arena),
        // K.A12 (alias_table), K.A13 (shadow_table). Each is a direct
        // NodeIdBearing::retain_nodes call.
        //
        // When a future task lifts a new NodeId-bearing structure onto
        // `CodeGraph`, extend this step with one additional
        // `retain_nodes` call and add a K.A/K.B row to
        // `super::coverage` per the plan §K contract.
        // ----------------------------------------------------------------
        {
            let tombstones = &self.tombstones;
            let predicate: Box<dyn Fn(NodeId) -> bool + '_> =
                Box::new(move |nid| !tombstones.contains(&nid));
            self.node_provenance.retain_nodes(&*predicate);
            self.scope_arena.retain_nodes(&*predicate);
            self.alias_table.retain_nodes(&*predicate);
            self.shadow_table.retain_nodes(&*predicate);
        }

        // ----------------------------------------------------------------
        // Step 8 — Drain tombstones into `drained_tombstones`.
        //
        // The residue check at step 14 asserts that the finalized
        // `CodeGraph` does not contain any of these NodeIds in any
        // NodeId-bearing structure. Keep the drained set until the
        // check completes; then it is dropped with `self`.
        // ----------------------------------------------------------------
        self.drained_tombstones = std::mem::take(&mut self.tombstones);

        // ----------------------------------------------------------------
        // Step 9 — Rebuild CSR adjacency (both directions).
        //
        // CSR is derived state (K.A9 — "CSR adjacency is derived state;
        // rebuilt from compacted edges — never mutated in place"). After
        // step 3 filtered the delta tier to live-only endpoints, the
        // correct operation is to *construct* a fresh CSR from the
        // compacted delta snapshot and *install* it — not merely drop
        // the stale cache. This matches plan §H lines 684–691 (the
        // committed-then-queried graph has a CSR from the first read).
        //
        // Build path: `compaction::snapshot_edges` → compaction merges
        // in-memory delta (no tombstones left after step 3) →
        // `build_compacted_csr` constructs an immutable `CsrGraph` →
        // `swap_csrs_and_clear_deltas` installs both directions and
        // clears the now-absorbed delta buffers. The node_count is the
        // post-compaction slot count of the arena (not the live count —
        // CSR uses dense slot indexing; vacant slots appear as nodes
        // with zero out-edges).
        //
        // The plan's example spells this `self.edges.rebuild_csr()` as
        // shorthand for this sequence; we expand it to concrete calls
        // so reviewers can audit each primitive. The post-condition is
        // identical to the plan's: after step 9 the `CodeGraph` about
        // to be assembled has no CSR referencing a tombstoned NodeId,
        // because the CSR was built from the already-filtered delta.
        // ----------------------------------------------------------------
        {
            use crate::graph::unified::compaction::{
                Direction, build_compacted_csr, snapshot_edges,
            };
            let node_count = self.nodes.slot_count();
            // Snapshot the forward/reverse deltas (CSR is still stale
            // at this point — `build_compacted_csr` uses the delta's
            // live-only contents). Because step 3 tombstoned endpoints
            // out of the delta already, the merged build sees only
            // live edges.
            let forward_snapshot = {
                let forward = self.edges.forward();
                snapshot_edges(&forward, node_count)
            };
            let reverse_snapshot = {
                let reverse = self.edges.reverse();
                snapshot_edges(&reverse, node_count)
            };
            // Build both directions in parallel (matches the pattern
            // used by `build/entrypoint.rs`).
            let (forward_csr, reverse_csr) = rayon::join(
                || build_compacted_csr(&forward_snapshot, Direction::Forward),
                || build_compacted_csr(&reverse_snapshot, Direction::Reverse),
            );
            let (forward_csr, _) =
                forward_csr.map_err(|e| crate::graph::error::GraphBuilderError::Internal {
                    reason: format!("rebuild finalize step 9 (forward CSR build): {e}"),
                })?;
            let (reverse_csr, _) =
                reverse_csr.map_err(|e| crate::graph::error::GraphBuilderError::Internal {
                    reason: format!("rebuild finalize step 9 (reverse CSR build): {e}"),
                })?;
            // Install both CSRs and clear the absorbed deltas.
            self.edges
                .swap_csrs_and_clear_deltas(forward_csr, reverse_csr);
        }

        // ----------------------------------------------------------------
        // Step 10 — Per-language confidence update.
        //
        // Incremental rebuild must ensure the published confidence map
        // matches the set of languages that actually have live nodes in
        // the rebuild's graph. Languages whose only source files were
        // all removed during this rebuild must not linger in the
        // confidence surface — they would mislead MCP clients into
        // believing analysis for that language is still available.
        //
        // The rebuild-local `FileRegistry` carries per-file `Language`
        // tags; we enumerate them, collect the set of languages with at
        // least one live file, and drop confidence entries for any
        // language that no longer appears. Entries for still-present
        // languages are preserved as-is (their `ConfidenceMetadata` is
        // updated by the plugin-level confidence-ingestion path in
        // `CodeGraph::merge_confidence`; this step only filters, it
        // does not fabricate new samples).
        //
        // A language entry is preserved when (a) at least one live file
        // is tagged with that language in the rebuild-local registry,
        // OR (b) the confidence entry carries a non-default set of
        // limitations / unavailable-features — that is analysis
        // metadata the plugin layer deliberately recorded, and dropping
        // it on a zero-file rebuild would be lossy. Both conditions are
        // evaluated against the rebuild-local state that will be
        // published by step 12.
        // ----------------------------------------------------------------
        {
            use std::collections::BTreeSet;
            let mut active_languages: BTreeSet<String> = BTreeSet::new();
            for (_file_id, _path, maybe_language) in self.files.iter_with_language() {
                if let Some(language) = maybe_language {
                    active_languages.insert(language.to_string());
                }
            }
            self.confidence.retain(|language_key, meta| {
                if active_languages.contains(language_key) {
                    true
                } else {
                    // Preserve entries that encode deliberate
                    // plugin-recorded limitations even when no live
                    // files remain — those are analysis-state facts
                    // the daemon must not silently drop.
                    !meta.limitations.is_empty() || !meta.unavailable_features.is_empty()
                }
            });
        }

        // ----------------------------------------------------------------
        // Step 11 — Epoch bump.
        //
        // `prior_epoch` captured at `clone_for_rebuild` time; +1 marks
        // the publication boundary. Wrapping add matches the existing
        // `CodeGraph::bump_epoch` behavior at
        // `sqry-core/src/graph/unified/concurrent/graph.rs`.
        // ----------------------------------------------------------------
        let new_epoch = self.epoch.wrapping_add(1);

        // ----------------------------------------------------------------
        // Step 12 — Assemble the immutable `CodeGraph`.
        //
        // Every Arc-wrapped field is freshly wrapped from the
        // rebuild-local owned value, so the new `CodeGraph` has no
        // Arc-sharing relationship with the pre-rebuild snapshot. This
        // is the point at which the rebuild-local interner becomes
        // immutable (step 1's freeze).
        // ----------------------------------------------------------------
        let mut graph = CodeGraph::__assemble_from_rebuild_parts_internal(
            self.nodes,
            self.edges,
            self.strings,
            self.files,
            self.indices,
            self.macro_metadata,
            self.node_provenance,
            self.edge_provenance,
            self.fact_epoch,
            new_epoch,
            self.confidence,
            self.scope_arena,
            self.alias_table,
            self.shadow_table,
            self.scope_provenance_store,
            self.file_segments,
            self.go_hints,
        );
        // Phase A (U09): the assembled `CodeGraph` resets
        // `c_indirect_tables` to `None`. Re-install the rebuild's owned
        // value so an in-flight rebuild preserves side tables already
        // committed on the source graph.
        graph.set_c_indirect_tables(self.c_indirect_tables);

        // ----------------------------------------------------------------
        // Steps 13 + 14 — (debug) Publish-boundary invariants.
        //
        // Per plan §F.3 "single source of truth", both the §F.1 bucket
        // bijection and the §F.2 tombstone-residue checks fire through
        // the canonical `publish::assert_publish_invariants` helper.
        // That helper is also called at the full-rebuild end
        // (`build_unified_graph_inner`), by Task 6's
        // `WorkspaceManager::publish_graph`, and by every §E harness
        // iteration — so any invariant drift surfaces in all four
        // places simultaneously, not in a subset.
        //
        // The §F.2 assertion is the **single site** against
        // `self.drained_tombstones` (populated at step 8); we do not
        // run the residue check anywhere else.
        // ----------------------------------------------------------------
        #[cfg(any(debug_assertions, test))]
        crate::graph::unified::publish::assert_publish_invariants(&graph, &self.drained_tombstones);

        Ok(graph)
    }

    /// Returns the number of `NodeIds` currently staged for tombstoning
    /// via [`RebuildGraph::tombstone`] (to be added by Task 4 Step 4).
    /// Useful for Gate 0c tests that exercise finalize with an empty
    /// or non-empty tombstone set.
    #[must_use]
    pub fn pending_tombstone_count(&self) -> usize {
        self.tombstones.len()
    }

    /// Shared-reference accessor for the rebuild-local
    /// [`NodeArena`]. Read-only; the writer is the rebuild dispatcher
    /// which holds `&mut self`.
    ///
    /// Used by sqry-daemon's `WorkspaceManager` (Task 6) and by the
    /// Task 4 Step 4 scale test to inspect arena state between
    /// [`remove_file`](Self::remove_file) calls without going through
    /// the pub(crate) field directly.
    #[must_use]
    pub fn nodes(&self) -> &NodeArena {
        &self.nodes
    }

    /// Shared-reference accessor for the rebuild-local
    /// [`FileRegistry`]. Read-only. The rebuild dispatcher writes via
    /// `&mut self` on [`remove_file`](Self::remove_file) and
    /// finalization.
    #[must_use]
    pub fn files(&self) -> &FileRegistry {
        &self.files
    }

    /// Shared-reference accessor for the rebuild-local
    /// [`FileSegmentTable`]. Read-only.
    ///
    /// `FileSegmentTable` is a plain `Vec<Option<FileSegment>>` with no
    /// interior mutability (see
    /// `sqry-core/src/graph/unified/storage/segment.rs`), so this
    /// accessor is genuinely read-only. Writers ride through the
    /// `&mut self` mutation path inside
    /// [`remove_file`](Self::remove_file) and
    /// [`finalize`](Self::finalize).
    ///
    /// Added as part of the iter-1 Codex review fix for Task 4
    /// Steps 2-3: the `remove_file` integration tests need to assert
    /// that a file's segment entry is cleared after removal, closing
    /// the FileId-recycle stale-range bug documented at
    /// `sqry-core/tests/incremental_remove_file_scale.rs`.
    #[must_use]
    pub fn file_segments(&self) -> &FileSegmentTable {
        &self.file_segments
    }

    // Note (iter-1 review fix): a `pub fn edges(&self) -> &BidirectionalEdgeStore`
    // accessor was removed. `BidirectionalEdgeStore` exposes
    // `forward_mut(&self)` and `reverse_mut(&self)` (interior mutability),
    // so returning `&BidirectionalEdgeStore` from `&self` would let
    // external callers escalate a "read-only" handle into a writer. The
    // rebuild contract requires that edge mutation only happens inside
    // `RebuildGraph::remove_file` / `finalize` on `&mut self`. If a
    // read-only edge-view accessor is ever needed by a legitimate
    // consumer, add a wrapper struct (e.g., `BidirectionalEdgeStoreView`)
    // that exposes only `forward()` / `reverse()` / `edges_from` /
    // `edges_to` / `stats` on `&self` — never the `*_mut` methods.

    /// Stage a `NodeId` for tombstoning during the next `finalize` pass.
    ///
    /// Gate 0c ships this helper so finalize tests can drive the
    /// 14-step contract with a realistic tombstone set without waiting
    /// for Task 4 Step 4's `remove_file` plumbing. Production callers
    /// will route through `RebuildGraph::remove_file` (Task 4 Step 4),
    /// which internally populates the same set via
    /// `FileRegistry::take_nodes`.
    pub fn tombstone(&mut self, id: NodeId) {
        self.tombstones.insert(id);
    }

    /// Stage every `NodeId` in `ids` for tombstoning during the next
    /// `finalize` pass. Equivalent to calling [`tombstone`](Self::tombstone)
    /// for each id, but expresses the bulk intent at the call site.
    ///
    /// Used by [`remove_file`](Self::remove_file) to fold every
    /// file-local `NodeId` into the staged set in a single pass.
    pub(crate) fn tombstone_many<I: IntoIterator<Item = NodeId>>(&mut self, ids: I) {
        self.tombstones.extend(ids);
    }

    /// Drain every `NodeId` belonging to `file_id`, invalidate every
    /// rebuild-local edge whose source or target is one of those nodes
    /// (across both forward and reverse CSR + delta tiers of the
    /// rebuild-local edge store), drop the file's entry from the
    /// rebuild-local [`FileRegistry`], and return the list of
    /// tombstoned [`NodeId`]s.
    ///
    /// This is the rebuild-side mirror of
    /// [`super::super::concurrent::CodeGraph::remove_file`]. Whereas
    /// the `CodeGraph` variant mutates a live publishable graph in
    /// place (O(1) publish once the caller wraps it in an `Arc`), this
    /// variant operates on the owned, pre-finalize
    /// [`RebuildGraph`] state — so the edge-store tombstones land in
    /// the CSR's tombstone bitmap and the delta buffer, and are later
    /// physically purged by [`finalize`](Self::finalize) step 9 when
    /// the CSR is rebuilt from the compacted delta (§H lines 684–691).
    ///
    /// The method does **not** rewrite `NodeIdBearing` surfaces on the
    /// rebuild (`NodeArena`, `AuxiliaryIndices`, `NodeMetadataStore`,
    /// `NodeProvenanceStore`, `ScopeArena`, `AliasTable`, `ShadowTable`) —
    /// those surfaces are compacted once, uniformly, by `finalize()`
    /// steps 2–7 against the accumulated `tombstones` set. Running a
    /// partial compaction here would duplicate work and violate the
    /// plan's "compact once at step N" contract.
    ///
    /// Instead, the tombstoned `NodeId`s are accumulated in
    /// `self.tombstones` via [`tombstone_many`](Self::tombstone_many).
    /// Successive `remove_file` calls against different files
    /// accumulate into the same set; `finalize()` then sweeps every
    /// K.A/K.B surface once with the union predicate.
    ///
    /// # What is tombstoned immediately
    ///
    /// Only two surfaces are mutated before `finalize()` runs:
    ///
    /// * **`NodeArena`**: each file-local `NodeId` is `remove`d so the
    ///   slot's generation advances and stale handles cannot alias a
    ///   re-allocation. Downstream compaction at step 2 is then a
    ///   no-op for these slots (idempotent; the arena skips stale
    ///   generations).
    /// * **Edge store** (both forward + reverse, both CSR + delta):
    ///   [`BidirectionalEdgeStore::tombstone_edges_for_nodes`] kills
    ///   every edge whose source or target is one of the drained
    ///   `NodeIds`. CSR tombstones land in `csr_tombstones`; delta
    ///   edges are dropped outright. Step 9 of `finalize` then
    ///   rebuilds a fresh CSR from the tombstone-free delta, so the
    ///   CSR tombstones become physically invisible by publish time.
    /// * **`FileRegistry`**: the bucket is drained via
    ///   [`FileRegistry::take_nodes`] and the file entry is
    ///   deregistered via [`FileRegistry::unregister`]. Both are
    ///   idempotent on an already-removed file.
    ///
    /// # Returned value
    ///
    /// The list of `NodeIds` that were staged for tombstoning. Empty on
    /// an unknown or already-removed file. Useful for Gate 0c finalize
    /// tests that need to assert on bucket-drain correctness or on the
    /// union of staged tombstones across several `remove_file` calls.
    ///
    /// # Idempotency
    ///
    /// Calling twice with the same `file_id` is a no-op on the second
    /// call. The bucket is already drained, `take_nodes` returns an
    /// empty `Vec`, the rest of the method short-circuits, and the
    /// `tombstones` set is unchanged.
    #[allow(dead_code)] // Intra-crate consumer is Task 4 Step 4
    // (`incremental_rebuild`); external consumer is sqry-daemon's
    // Task 6 `WorkspaceManager` via the feature-gated `pub use` of
    // `RebuildGraph`. The visibility is `pub` (not `pub(crate)`) so
    // the daemon can drive file removals through the rebuild plane,
    // which is the canonical path for file-deletion events per §F.2.
    pub fn remove_file(&mut self, file_id: super::super::file::FileId) -> Vec<NodeId> {
        // Drain the rebuild-local FileRegistry bucket.
        let tombstoned: Vec<NodeId> = self.files.take_nodes(file_id);
        // Deregister the file entry unconditionally (idempotent).
        self.files.unregister(file_id);
        // Clear the rebuild-local `FileSegmentTable` entry for this
        // file (idempotent — `remove` no-ops on unknown ids). This
        // matches `CodeGraph::remove_file`'s behaviour: finalize()
        // publishes `self.file_segments` verbatim at step 12, so any
        // stale entry leaked here would survive into the assembled
        // `CodeGraph`. Because `FileRegistry::unregister` recycles
        // `FileId` slots, a leaked segment would later alias a
        // different file's node range after the slot is reissued —
        // which would cause `reindex_files` to tombstone the wrong
        // range. Clearing the segment at remove time is the only
        // defence that closes both the "finalize publishes stale
        // segment" path AND the "FileId recycle attaches stale
        // range" path.
        self.file_segments.remove(file_id);

        if tombstoned.is_empty() {
            return tombstoned;
        }

        let dead: std::collections::HashSet<NodeId> = tombstoned.iter().copied().collect();

        // 1. Tombstone each arena slot. `NodeArena::remove` is
        //    idempotent against stale generations, so retriggering a
        //    file removal after finalize is safe.
        for &nid in &tombstoned {
            let _ = self.nodes.remove(nid);
        }

        // 2. Invalidate edges across both CSR + delta in both
        //    directions on the rebuild-local edge store.
        self.edges.tombstone_edges_for_nodes(&dead);

        // 3. Stage the drained NodeIds for the finalize-time K.A/K.B
        //    compaction sweep. finalize() will consume these at steps
        //    2–7 through the NodeIdBearing::retain_nodes predicate.
        self.tombstone_many(tombstoned.iter().copied());

        tombstoned
    }

    /// Returns the rebuild-local epoch captured at `clone_for_rebuild`.
    #[must_use]
    pub fn prior_epoch(&self) -> u64 {
        self.epoch
    }

    /// Pre-finalize tombstone-residue check on the rebuild-state
    /// structures themselves (plan §F.2 literal named API).
    ///
    /// Iterates every `NodeIdBearing` field on this `RebuildGraph` and
    /// panics if any contains a `NodeId` present in `self.tombstones`.
    /// This is a diagnostic helper for mid-rebuild consistency checks:
    /// e.g., Task 4 Step 4's `RebuildGraph::remove_file` can call this
    /// after tombstoning a file but before more closure files land, to
    /// prove the just-tombstoned `NodeIds` really left every index before
    /// the next pass commits. The `RebuildGraph::finalize` flow then
    /// re-asserts against the drained set on the *assembled* `CodeGraph`
    /// at step 14 — those are two different snapshots of the same
    /// invariant; both must hold.
    ///
    /// No-op when `self.tombstones` is empty or in release builds.
    ///
    /// Does **not** replace the step-14 call site; the single source of
    /// truth for the publish-boundary residue check remains
    /// `crate::graph::unified::publish::assert_publish_invariants`. This
    /// helper exists so pre-finalize call sites (Task 4 Step 4's
    /// post-remove sanity check, incremental-engine debug probes, Gate
    /// 0d negative tests) can name the assertion without rolling their
    /// own iteration.
    ///
    /// # Panics
    ///
    /// Panics in debug/test builds when any node-bearing rebuild surface
    /// still references a node recorded in `self.tombstones`. That indicates
    /// the rebuild plane removed a file without fully compacting every
    /// dependent arena, edge, and auxiliary index before the next pass.
    #[cfg(any(debug_assertions, test))]
    pub fn assert_no_tombstone_residue(&self) {
        use super::coverage::NodeIdBearing;
        let dead = &self.tombstones;
        if dead.is_empty() {
            return;
        }
        for nid in self.nodes.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "RebuildGraph::assert_no_tombstone_residue: tombstone {nid:?} still in NodeArena"
            );
        }
        for nid in self.edges.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "RebuildGraph::assert_no_tombstone_residue: tombstone {nid:?} still in edge store"
            );
        }
        for nid in self.indices.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "RebuildGraph::assert_no_tombstone_residue: tombstone {nid:?} still in auxiliary indices"
            );
        }
        for nid in self.macro_metadata.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "RebuildGraph::assert_no_tombstone_residue: tombstone {nid:?} still in macro metadata"
            );
        }
        for nid in self.node_provenance.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "RebuildGraph::assert_no_tombstone_residue: tombstone {nid:?} still in node provenance"
            );
        }
        for nid in self.scope_arena.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "RebuildGraph::assert_no_tombstone_residue: tombstone {nid:?} still in scope arena"
            );
        }
        for nid in self.alias_table.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "RebuildGraph::assert_no_tombstone_residue: tombstone {nid:?} still in alias table"
            );
        }
        for nid in self.shadow_table.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "RebuildGraph::assert_no_tombstone_residue: tombstone {nid:?} still in shadow table"
            );
        }
        for nid in self.files.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "RebuildGraph::assert_no_tombstone_residue: tombstone {nid:?} still in per-file bucket"
            );
        }
    }
}

// =====================================================================
// Step 1 helper: rewrite every live `NodeEntry`'s interned-string
// fields through the dedup-remap table produced by
// `StringInterner::build_dedup_table()`.
// =====================================================================

/// Rewrite every `NodeEntry` field that holds a [`crate::graph::unified::string::id::StringId`]
/// through `remap` so no live node points at a collapsed-duplicate
/// slot. Called from finalize step 1 after the interner has been
/// canonicalised; a no-op when `remap` is empty (the common case of a
/// no-op rebuild).
///
/// Fields rewritten: `name`, `signature`, `doc`, `qualified_name`,
/// `visibility`. These are every `StringId` / `Option<StringId>` field
/// that `NodeEntry` currently declares. Adding a new `StringId`-valued
/// field to `NodeEntry` requires extending this helper — the closest
/// enforcement is the compile-fail coverage in the `sqry_graph_fields!`
/// macro which forces a new `RebuildGraph` field and a matching
/// finalize step for any new `CodeGraph` field; individual arena
/// fields are caught at review time against this helper.
fn rewrite_node_entries_through_remap(
    nodes: &mut NodeArena,
    remap: &HashMap<
        crate::graph::unified::string::id::StringId,
        crate::graph::unified::string::id::StringId,
    >,
) {
    if remap.is_empty() {
        return;
    }
    for (_id, entry) in nodes.iter_mut() {
        if let Some(&canon) = remap.get(&entry.name) {
            entry.name = canon;
        }
        if let Some(sid) = entry.signature
            && let Some(&canon) = remap.get(&sid)
        {
            entry.signature = Some(canon);
        }
        if let Some(sid) = entry.doc
            && let Some(&canon) = remap.get(&sid)
        {
            entry.doc = Some(canon);
        }
        if let Some(sid) = entry.qualified_name
            && let Some(&canon) = remap.get(&sid)
        {
            entry.qualified_name = Some(canon);
        }
        if let Some(sid) = entry.visibility
            && let Some(&canon) = remap.get(&sid)
        {
            entry.visibility = Some(canon);
        }
    }
}

// =====================================================================
// Assembly path notes (A2 §H "Type-enforced publish path")
// =====================================================================
//
// The only route from `RebuildGraph` to `Arc<CodeGraph>` is:
//
//     let code_graph = rebuild.finalize()?;            // Step 12 assembles CodeGraph
//     let arc = Arc::new(code_graph);                  // caller wraps (publish site)
//
// The assembly inside `finalize` uses
// `CodeGraph::__assemble_from_rebuild_parts_internal`, which is
// `pub(crate)` on `CodeGraph`. That means downstream crates — even
// `sqry-daemon` with `rebuild-internals` enabled — cannot call the
// assembler directly; they must route through `finalize()`, which
// guarantees the 14-step compaction sequence runs first.
//
// The trybuild fixture
// `sqry-core/tests/rebuild_internals_compile_fail/rebuild_graph_no_public_assembly.rs`
// exercises this: a downstream crate attempting to call
// `__assemble_from_rebuild_parts_internal` fails with E0603 ("function
// is private"), and attempting to `impl From<RebuildGraph> for CodeGraph`
// from outside this module fails with E0117 (orphan-rule violation).

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::arena::NodeEntry;
    use crate::graph::unified::storage::metadata::MacroNodeMetadata;

    /// Seed a `CodeGraph` with a handful of live nodes distributed over
    /// two files so the per-field compaction steps have something
    /// non-trivial to operate on.
    fn seeded_graph() -> (CodeGraph, NodeId, NodeId, NodeId) {
        let mut graph = CodeGraph::new();
        let file_a = FileId::new(1);
        let file_b = FileId::new(2);
        let sym = graph.strings_mut().intern("symbol_a").expect("intern a");
        let node_a;
        let node_b;
        let node_c;
        {
            let arena = graph.nodes_mut();
            node_a = arena
                .alloc(NodeEntry::new(NodeKind::Function, sym, file_a))
                .expect("alloc a");
            node_b = arena
                .alloc(NodeEntry::new(NodeKind::Method, sym, file_a))
                .expect("alloc b");
            node_c = arena
                .alloc(NodeEntry::new(NodeKind::Struct, sym, file_b))
                .expect("alloc c");
        }
        graph
            .indices_mut()
            .add(node_a, NodeKind::Function, sym, Some(sym), file_a);
        graph
            .indices_mut()
            .add(node_b, NodeKind::Method, sym, Some(sym), file_a);
        graph
            .indices_mut()
            .add(node_c, NodeKind::Struct, sym, Some(sym), file_b);
        graph
            .macro_metadata_mut()
            .insert(node_a, MacroNodeMetadata::default());
        (graph, node_a, node_b, node_c)
    }

    #[test]
    fn clone_for_rebuild_copies_every_field_without_arc_sharing() {
        let (graph, _a, _b, _c) = seeded_graph();
        let rebuild = graph.clone_for_rebuild();

        // Field-level counts match the source graph.
        assert_eq!(rebuild.nodes.len(), graph.nodes().len());
        assert_eq!(rebuild.macro_metadata.len(), graph.macro_metadata().len());
        // The rebuild owns its own arena (not an Arc share), so
        // mutating it in place would not affect the source graph. We
        // verify this by tombstoning a node in the rebuild and
        // confirming the source is unchanged.
        let mut rebuild = rebuild;
        let ids: Vec<NodeId> = rebuild.nodes.all_node_ids().collect();
        let victim = *ids.first().expect("at least one node");
        rebuild.tombstone(victim);
        assert_eq!(rebuild.pending_tombstone_count(), 1);
        // Source graph unaffected.
        assert_eq!(graph.nodes().len(), ids.len());
    }

    #[test]
    fn clone_for_rebuild_preserves_epoch_and_fact_epoch() {
        let (mut graph, _, _, _) = seeded_graph();
        graph.set_epoch(7);
        let rebuild = graph.clone_for_rebuild();
        assert_eq!(rebuild.prior_epoch(), 7);
        assert_eq!(rebuild.fact_epoch, graph.fact_epoch());
    }

    // ---- Step-level finalize tests ---------------------------------

    #[test]
    fn finalize_step11_bumps_epoch_by_one() {
        let (mut graph, _, _, _) = seeded_graph();
        graph.set_epoch(41);
        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");
        assert_eq!(finalized.epoch(), 42);
    }

    #[test]
    fn finalize_step8_drains_tombstones_into_drained_set() {
        // We use a reach-into-guts test: finalize consumes self, so we
        // verify the drain via a direct RebuildGraph constructed inside
        // this module (which has crate visibility into the field).
        let (graph, a, _b, _c) = seeded_graph();
        let mut rebuild = graph.clone_for_rebuild();
        rebuild.tombstone(a);
        assert_eq!(rebuild.pending_tombstone_count(), 1);
        // After finalize, the graph must not contain `a`.
        let finalized = rebuild.finalize().expect("finalize ok");
        assert!(
            finalized.nodes().get(a).is_none(),
            "tombstoned node must be gone from arena"
        );
    }

    #[test]
    fn finalize_steps_2_and_4_compact_arena_and_indices_consistently() {
        let (graph, a, _b, c) = seeded_graph();
        let mut rebuild = graph.clone_for_rebuild();
        rebuild.tombstone(a);
        rebuild.tombstone(c);
        let finalized = rebuild.finalize().expect("finalize ok");

        // Arena compaction (step 2): dropped nodes are gone.
        assert!(finalized.nodes().get(a).is_none());
        assert!(finalized.nodes().get(c).is_none());
        // Step 4 must compact the auxiliary indices in lockstep, so
        // the tombstoned ids do not linger in any index.
        use crate::graph::unified::storage::metadata::TypedMetadata;
        let _ = TypedMetadata::Macro(MacroNodeMetadata::default()); // silence unused import warning on some cfgs
        assert!(!finalized.indices().by_kind(NodeKind::Function).contains(&a));
        assert!(!finalized.indices().by_kind(NodeKind::Struct).contains(&c));
    }

    #[test]
    fn finalize_step5_compacts_macro_metadata() {
        let (graph, a, _b, _c) = seeded_graph();
        let mut rebuild = graph.clone_for_rebuild();
        rebuild.tombstone(a);
        let finalized = rebuild.finalize().expect("finalize ok");
        // `a` had macro metadata seeded; after finalize it must be gone.
        assert!(finalized.macro_metadata().get_macro(a).is_none());
    }

    #[test]
    fn finalize_step9_installs_rebuilt_csr() {
        // Step 9 (plan §H lines 684–691) must install a freshly built
        // CSR — not merely drop the stale cache. The assembled graph's
        // forward and reverse edge stores must both expose a `csr()`
        // after finalize, and the deltas must be empty (absorbed by
        // the swap).
        let (graph, _, _, _) = seeded_graph();
        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");
        assert!(
            finalized.edges().forward().csr().is_some(),
            "step 9 must install forward CSR"
        );
        assert!(
            finalized.edges().reverse().csr().is_some(),
            "step 9 must install reverse CSR"
        );
        // Deltas were absorbed by the swap.
        assert_eq!(finalized.edges().forward().delta_count(), 0);
        assert_eq!(finalized.edges().reverse().delta_count(), 0);
    }

    #[test]
    fn finalize_step10_drops_confidence_for_removed_languages() {
        // Seed a graph with Rust + Python confidence; the rebuild-local
        // FileRegistry has zero files tagged (seeded_graph does not
        // register paths), so step 10 must drop both.
        let (mut graph, _, _, _) = seeded_graph();
        graph.merge_confidence(
            "rust",
            crate::confidence::ConfidenceMetadata {
                level: crate::confidence::ConfidenceLevel::Verified,
                ..Default::default()
            },
        );
        graph.merge_confidence(
            "python",
            crate::confidence::ConfidenceMetadata {
                level: crate::confidence::ConfidenceLevel::Partial,
                ..Default::default()
            },
        );
        assert_eq!(graph.confidence().len(), 2);

        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");
        assert!(
            finalized.confidence().is_empty(),
            "step 10 must drop confidence entries for languages that \
             have no live files and no recorded limitations"
        );
    }

    #[test]
    fn finalize_step10_preserves_confidence_with_limitations() {
        // A confidence entry that encodes deliberate limitations must
        // survive a zero-file rebuild: dropping it would silently lose
        // plugin-recorded analysis state.
        let (mut graph, _, _, _) = seeded_graph();
        graph.merge_confidence(
            "rust",
            crate::confidence::ConfidenceMetadata {
                level: crate::confidence::ConfidenceLevel::AstOnly,
                limitations: vec!["no rust-analyzer".to_string()],
                unavailable_features: vec!["type inference".to_string()],
            },
        );
        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");
        assert_eq!(finalized.confidence().len(), 1);
        assert!(finalized.confidence().contains_key("rust"));
    }

    #[test]
    fn finalize_step1_canonicalises_interner_via_dedup() {
        // Freeze step must leave the interner's lookup consistent
        // (non-stale) and zero duplicate slots after finalize.
        let (graph, _, _, _) = seeded_graph();
        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");
        assert!(
            !finalized.strings().is_lookup_stale(),
            "step 1 freeze must leave lookup_stale == false"
        );
    }

    #[test]
    fn finalize_is_infallible_on_empty_tombstone_set() {
        let (graph, _, _, _) = seeded_graph();
        let rebuild = graph.clone_for_rebuild();
        let result = rebuild.finalize();
        assert!(result.is_ok(), "empty-tombstone finalize must succeed");
        let finalized = result.unwrap();
        // Every node survives.
        assert_eq!(finalized.nodes().len(), 3);
    }

    #[test]
    fn finalize_survives_interner_snapshot_unchanged_when_no_edits() {
        let (graph, _, _, _) = seeded_graph();
        let prior_string_count = graph.strings().len();
        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");
        assert_eq!(
            finalized.strings().len(),
            prior_string_count,
            "freeze step must preserve string count across a no-op rebuild"
        );
    }

    #[test]
    fn rebuild_graph_pending_tombstone_count_is_accurate() {
        let (graph, a, b, c) = seeded_graph();
        let mut rebuild = graph.clone_for_rebuild();
        assert_eq!(rebuild.pending_tombstone_count(), 0);
        rebuild.tombstone(a);
        rebuild.tombstone(b);
        rebuild.tombstone(c);
        // Inserting the same id again is idempotent.
        rebuild.tombstone(a);
        assert_eq!(rebuild.pending_tombstone_count(), 3);
    }

    // ----------------------------------------------------------------
    // Iter-2 B1: StringId remap across every StringId-backed holder.
    // ----------------------------------------------------------------

    /// Construct a RebuildGraph whose interner has intentional duplicate
    /// `StringId` slots (two slots containing the same canonical
    /// "symbol_dup" bytes), with the **live** state referencing both
    /// slots. `build_dedup_table()` must collapse them, step 1 must
    /// rewrite every StringId-backed surface through the remap, and
    /// step 4 must leave no bucket pointing at a recycled StringId.
    #[test]
    fn step1_remaps_auxiliary_indices_keys_through_dedup_table() {
        use crate::graph::unified::storage::interner::StringInterner;

        let mut graph = CodeGraph::new();
        // Intern "alpha" twice via the bulk path so the two slots are
        // NOT coalesced at intern time. We emulate that by manually
        // using `alloc_range` + direct bulk_slices_mut assignment on
        // the interner.
        let interner: &mut StringInterner = graph.strings_mut();
        let start = interner.alloc_range(2).expect("alloc range");
        {
            let (s_slots, rc_slots) = interner.bulk_slices_mut(start, 2);
            s_slots[0] = Some(std::sync::Arc::from("alpha"));
            s_slots[1] = Some(std::sync::Arc::from("alpha"));
            rc_slots[0] = 1;
            rc_slots[1] = 1;
        }
        let id_dup = crate::graph::unified::string::id::StringId::new(start + 1);
        let id_canon = crate::graph::unified::string::id::StringId::new(start);
        let file = FileId::new(1);

        // Allocate one node that references the duplicate StringId so
        // after dedup the arena still points at a canonical id.
        let mut entry = NodeEntry::new(NodeKind::Function, id_dup, file);
        entry.qualified_name = Some(id_dup);
        let node = graph.nodes_mut().alloc(entry).expect("alloc");
        graph
            .indices_mut()
            .add(node, NodeKind::Function, id_dup, Some(id_dup), file);

        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");

        // After step 1 the duplicate slot must be recycled and every
        // live surface must reference the canonical slot.
        // (a) Arena field references canonical
        let entry = finalized.nodes().get(node).expect("node survives");
        assert_eq!(entry.name, id_canon, "NodeEntry.name must be canonicalised");
        assert_eq!(
            entry.qualified_name,
            Some(id_canon),
            "NodeEntry.qualified_name must be canonicalised"
        );
        // (b) AuxiliaryIndices: by_name must find the node under canonical id.
        assert!(
            finalized.indices().by_name(id_canon).contains(&node),
            "indices.by_name under canonical StringId must contain the node"
        );
        // (c) And must NOT have any bucket under the duplicate StringId.
        assert!(
            finalized.indices().by_name(id_dup).is_empty(),
            "duplicate StringId bucket must be empty after remap"
        );
        assert!(
            finalized.indices().by_qualified_name(id_dup).is_empty(),
            "duplicate StringId qname bucket must be empty after remap"
        );
        // (d) Interner has no stale lookup.
        assert!(!finalized.strings().is_lookup_stale());
    }

    #[test]
    fn step1_merges_aux_indices_buckets_when_keys_collapse() {
        use crate::graph::unified::storage::interner::StringInterner;

        let mut graph = CodeGraph::new();
        let interner: &mut StringInterner = graph.strings_mut();
        let start = interner.alloc_range(2).expect("alloc range");
        {
            let (s_slots, rc_slots) = interner.bulk_slices_mut(start, 2);
            s_slots[0] = Some(std::sync::Arc::from("beta"));
            s_slots[1] = Some(std::sync::Arc::from("beta"));
            rc_slots[0] = 1;
            rc_slots[1] = 1;
        }
        let id_canon = crate::graph::unified::string::id::StringId::new(start);
        let id_dup = crate::graph::unified::string::id::StringId::new(start + 1);
        let file = FileId::new(1);

        // Two nodes: one references canonical, one references duplicate.
        // After remap, both must land in the canonical bucket of
        // `name_index` (merged, deduplicated by NodeId).
        let node_canon = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, id_canon, file))
            .expect("alloc canon");
        let node_dup = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, id_dup, file))
            .expect("alloc dup");
        graph
            .indices_mut()
            .add(node_canon, NodeKind::Function, id_canon, None, file);
        graph
            .indices_mut()
            .add(node_dup, NodeKind::Function, id_dup, None, file);

        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");

        let canonical_bucket = finalized.indices().by_name(id_canon);
        assert!(canonical_bucket.contains(&node_canon));
        assert!(canonical_bucket.contains(&node_dup));
        // No duplicates within the merged bucket.
        let mut seen = std::collections::HashSet::new();
        for id in canonical_bucket {
            assert!(seen.insert(*id), "bucket must be dedup'd by NodeId");
        }
        assert!(
            finalized.indices().by_name(id_dup).is_empty(),
            "collapsed duplicate key bucket must be removed"
        );
    }

    #[test]
    fn step1_remaps_file_registry_source_uri() {
        use crate::graph::unified::storage::interner::StringInterner;

        let mut graph = CodeGraph::new();
        // Force two identical "file:///foo" StringId slots.
        let interner: &mut StringInterner = graph.strings_mut();
        let start = interner.alloc_range(2).expect("alloc range");
        {
            let (s, rc) = interner.bulk_slices_mut(start, 2);
            s[0] = Some(std::sync::Arc::from("file:///foo"));
            s[1] = Some(std::sync::Arc::from("file:///foo"));
            rc[0] = 1;
            rc[1] = 1;
        }
        let id_canon = crate::graph::unified::string::id::StringId::new(start);
        let id_dup = crate::graph::unified::string::id::StringId::new(start + 1);

        let fid = graph
            .files_mut()
            .register_external_with_uri(
                "/virtual/Foo.class",
                Some(crate::graph::node::Language::Java),
                Some(id_dup),
            )
            .expect("register external");

        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");

        let view = finalized
            .files()
            .file_provenance(fid)
            .expect("provenance present");
        assert_eq!(
            view.source_uri,
            Some(id_canon),
            "FileRegistry source_uri must be rewritten to canonical StringId"
        );
    }

    // ----------------------------------------------------------------
    // Iter-4 B1: edge-store StringId remap across delta + CSR tiers.
    // ----------------------------------------------------------------

    /// Construct a `RebuildGraph` whose interner has three slots
    /// containing the same canonical bytes ("symbol_edge_dup"), with
    /// two distinct `EdgeKind::Imports { alias }` edges pointing at
    /// different duplicate slots. Step 1 must rewrite both aliases
    /// through the dedup table so the post-step-9 CSR references only
    /// the canonical slot; the duplicate slots must be recycled by
    /// `recycle_unreferenced` (ref_count == 0) and the canonical slot
    /// must retain references.
    ///
    /// This test intentionally exercises both the delta-tier write path
    /// (`DeltaBuffer::iter_mut`) *during* step 1 (where the edges still
    /// live in the delta buffer because no compaction has been run)
    /// and the CSR-tier read path (where the edges land after step 9's
    /// `swap_csrs_and_clear_deltas`). The companion test
    /// `step1_remaps_edge_kind_string_ids_in_csr_tier` covers the
    /// symmetric case where edges start in the CSR tier at the time
    /// step 1 runs.
    #[test]
    fn step1_remaps_edge_kind_string_ids_through_dedup_table() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::storage::interner::StringInterner;

        let mut graph = CodeGraph::new();
        let file_a = FileId::new(1);
        // Force three slots carrying the same canonical bytes so
        // `build_dedup_table()` collapses slot[start+1] and slot[start+2]
        // onto slot[start].
        let interner: &mut StringInterner = graph.strings_mut();
        let start = interner.alloc_range(3).expect("alloc range");
        {
            let (s, rc) = interner.bulk_slices_mut(start, 3);
            s[0] = Some(std::sync::Arc::from("symbol_edge_dup"));
            s[1] = Some(std::sync::Arc::from("symbol_edge_dup"));
            s[2] = Some(std::sync::Arc::from("symbol_edge_dup"));
            rc[0] = 1;
            rc[1] = 1;
            rc[2] = 1;
        }
        let id_canon = crate::graph::unified::string::id::StringId::new(start);
        let id_dup_1 = crate::graph::unified::string::id::StringId::new(start + 1);
        let id_dup_2 = crate::graph::unified::string::id::StringId::new(start + 2);

        // Allocate two nodes so we can hang two edges off distinct
        // (src, tgt) pairs and verify both are remapped.
        let src;
        let tgt;
        {
            let arena = graph.nodes_mut();
            src = arena
                .alloc(NodeEntry::new(NodeKind::Function, id_canon, file_a))
                .expect("alloc src");
            tgt = arena
                .alloc(NodeEntry::new(NodeKind::Function, id_canon, file_a))
                .expect("alloc tgt");
        }
        graph
            .indices_mut()
            .add(src, NodeKind::Function, id_canon, None, file_a);
        graph
            .indices_mut()
            .add(tgt, NodeKind::Function, id_canon, None, file_a);

        // Edge 1: Imports { alias = id_dup_1 } — forward src→tgt.
        graph.edges_mut().add_edge(
            src,
            tgt,
            EdgeKind::Imports {
                alias: Some(id_dup_1),
                is_wildcard: false,
            },
            file_a,
        );
        // Edge 2: Imports { alias = id_dup_2 } — forward tgt→src (so the
        // two edges are distinct keys in the store).
        graph.edges_mut().add_edge(
            tgt,
            src,
            EdgeKind::Imports {
                alias: Some(id_dup_2),
                is_wildcard: true,
            },
            file_a,
        );

        // Pre-finalize sanity: the edges live in the delta tier.
        assert!(
            graph.edges().forward().delta_count() >= 2,
            "pre-finalize: forward delta must hold both Imports edges"
        );
        assert!(
            graph.edges().reverse().delta_count() >= 2,
            "pre-finalize: reverse delta must hold the mirror of both Imports edges"
        );

        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");

        // After finalize step 9, edges have been absorbed into the CSR
        // tier by `swap_csrs_and_clear_deltas`. Step 1's `EdgeKind`
        // rewrite ran on the delta *before* step 9 snapshotted it, so
        // the CSR we assert on is built from the already-remapped delta.
        //
        // Forward CSR: every `Imports` alias must be canonical.
        let forward = finalized.edges().forward();
        let fwd_csr = forward
            .csr()
            .expect("forward CSR must be populated after step 9");
        let mut fwd_imports_seen = 0usize;
        for kind in fwd_csr.edge_kind() {
            if let EdgeKind::Imports { alias, .. } = kind {
                fwd_imports_seen += 1;
                assert_ne!(
                    *alias,
                    Some(id_dup_1),
                    "forward CSR Imports alias must not reference pre-dedup slot id_dup_1"
                );
                assert_ne!(
                    *alias,
                    Some(id_dup_2),
                    "forward CSR Imports alias must not reference pre-dedup slot id_dup_2"
                );
                assert_eq!(
                    *alias,
                    Some(id_canon),
                    "forward CSR Imports alias must be canonicalised"
                );
            }
        }
        drop(forward);
        assert!(
            fwd_imports_seen >= 2,
            "forward CSR must hold both Imports edges after finalize (saw {fwd_imports_seen})"
        );

        // Reverse CSR: mirror edges must be canonicalised identically.
        let reverse = finalized.edges().reverse();
        let rev_csr = reverse
            .csr()
            .expect("reverse CSR must be populated after step 9");
        let mut rev_imports_seen = 0usize;
        for kind in rev_csr.edge_kind() {
            if let EdgeKind::Imports { alias, .. } = kind {
                rev_imports_seen += 1;
                assert_ne!(*alias, Some(id_dup_1));
                assert_ne!(*alias, Some(id_dup_2));
                assert_eq!(*alias, Some(id_canon));
            }
        }
        drop(reverse);
        assert!(
            rev_imports_seen >= 2,
            "reverse CSR must mirror the forward Imports edges (saw {rev_imports_seen})"
        );

        // Step-1 (c) invariant: after `recycle_unreferenced`, the
        // duplicate slots must report `ref_count == 0`. If step 1's
        // edge-store remap had skipped `EdgeKind` payloads, the
        // duplicates' refcounts would stay positive (held by the edge
        // store) and recycle would not reclaim them.
        assert_eq!(
            finalized.strings().ref_count(id_dup_1),
            0,
            "duplicate slot id_dup_1 must be recycled (ref_count == 0) by step 1"
        );
        assert_eq!(
            finalized.strings().ref_count(id_dup_2),
            0,
            "duplicate slot id_dup_2 must be recycled (ref_count == 0) by step 1"
        );
        assert!(
            finalized.strings().ref_count(id_canon) > 0,
            "canonical slot must retain references from the two Imports edges (got {})",
            finalized.strings().ref_count(id_canon)
        );
    }

    /// Same construction as above, but compact the edges into a CSR tier
    /// before calling finalize so the rewrite covers the steady-state
    /// `CsrGraph::edge_kind` path. This exercises the branch of
    /// `BidirectionalEdgeStore::rewrite_edge_kind_string_ids_through_remap`
    /// that is otherwise dead in the tests above.
    #[test]
    fn step1_remaps_edge_kind_string_ids_in_csr_tier() {
        use crate::graph::unified::compaction::{Direction, build_compacted_csr, snapshot_edges};
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::storage::interner::StringInterner;

        let mut graph = CodeGraph::new();
        let file_a = FileId::new(1);
        let interner: &mut StringInterner = graph.strings_mut();
        let start = interner.alloc_range(2).expect("alloc range");
        {
            let (s, rc) = interner.bulk_slices_mut(start, 2);
            s[0] = Some(std::sync::Arc::from("csr_alias_dup"));
            s[1] = Some(std::sync::Arc::from("csr_alias_dup"));
            rc[0] = 1;
            rc[1] = 1;
        }
        let id_canon = crate::graph::unified::string::id::StringId::new(start);
        let id_dup = crate::graph::unified::string::id::StringId::new(start + 1);

        let src;
        let tgt;
        {
            let arena = graph.nodes_mut();
            src = arena
                .alloc(NodeEntry::new(NodeKind::Function, id_canon, file_a))
                .expect("alloc src");
            tgt = arena
                .alloc(NodeEntry::new(NodeKind::Function, id_canon, file_a))
                .expect("alloc tgt");
        }
        graph
            .indices_mut()
            .add(src, NodeKind::Function, id_canon, None, file_a);
        graph
            .indices_mut()
            .add(tgt, NodeKind::Function, id_canon, None, file_a);

        // Push the Imports edge into delta with alias pointing at the
        // duplicate StringId slot.
        graph.edges_mut().add_edge(
            src,
            tgt,
            EdgeKind::Imports {
                alias: Some(id_dup),
                is_wildcard: false,
            },
            file_a,
        );

        // Compact delta → CSR on both directions. This mirrors what the
        // full-build pipeline does after Phase 4d plus an explicit
        // compaction step.
        let node_count = 2;
        let edges = graph.edges_mut();
        let fwd_snap = snapshot_edges(&edges.forward(), node_count);
        let (forward_csr, _) =
            build_compacted_csr(&fwd_snap, Direction::Forward).expect("forward csr");
        let rev_snap = snapshot_edges(&edges.reverse(), node_count);
        let (reverse_csr, _) =
            build_compacted_csr(&rev_snap, Direction::Reverse).expect("reverse csr");
        edges.swap_csrs_and_clear_deltas(forward_csr, reverse_csr);
        assert!(
            edges.forward().csr().is_some(),
            "forward CSR must be present"
        );
        assert!(
            edges.reverse().csr().is_some(),
            "reverse CSR must be present"
        );
        assert_eq!(edges.forward().delta_count(), 0, "delta must be empty");
        assert_eq!(edges.reverse().delta_count(), 0, "delta must be empty");

        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");

        // CSR tier: every `edge_kind` entry must reference the canonical
        // StringId, never the duplicate.
        let forward = finalized.edges().forward();
        let csr = forward
            .csr()
            .expect("forward CSR must survive finalize step 1");
        let mut imports_seen = 0usize;
        for kind in csr.edge_kind() {
            if let EdgeKind::Imports { alias, .. } = kind {
                imports_seen += 1;
                assert_ne!(
                    *alias,
                    Some(id_dup),
                    "CSR Imports alias must not reference pre-dedup duplicate"
                );
                assert_eq!(
                    *alias,
                    Some(id_canon),
                    "CSR Imports alias must be canonicalised"
                );
            }
        }
        assert!(
            imports_seen > 0,
            "forward CSR must hold at least one Imports edge"
        );
        drop(forward);

        // Reverse CSR — same guarantee.
        let reverse = finalized.edges().reverse();
        let rev_csr = reverse
            .csr()
            .expect("reverse CSR must survive finalize step 1");
        let mut rev_imports_seen = 0usize;
        for kind in rev_csr.edge_kind() {
            if let EdgeKind::Imports { alias, .. } = kind {
                rev_imports_seen += 1;
                assert_ne!(*alias, Some(id_dup));
                assert_eq!(*alias, Some(id_canon));
            }
        }
        assert!(rev_imports_seen > 0, "reverse CSR must hold Imports edges");
        drop(reverse);

        assert_eq!(
            finalized.strings().ref_count(id_dup),
            0,
            "duplicate slot must be recycled (ref_count == 0) by step 1"
        );
    }

    /// Exhaustive coverage: every `StringId`-holding surface *reachable
    /// through `CodeGraph`'s public mutation API* must be rewritten by
    /// finalize step 1. This test drives each surface with a distinct
    /// duplicate `StringId` slot and verifies after finalize that (i)
    /// every surface references the canonical slot, never the duplicate;
    /// (ii) every duplicate slot is recycled (`ref_count == 0`); (iii)
    /// the canonical slot retains a positive ref count.
    ///
    /// Surfaces exercised:
    /// * **Arena** (`NodeEntry.name`, `NodeEntry.qualified_name`)
    /// * **AuxiliaryIndices** (`name_index` / `qualified_name_index` keys)
    /// * **FileRegistry** (`FileEntry.source_uri`)
    /// * **BidirectionalEdgeStore** (`EdgeKind::Imports.alias` — delta tier)
    ///
    /// `AliasTable` and `ShadowTable` StringId fields live under private
    /// API (`pub(crate) fn set_alias_table`, no public entry mutators)
    /// and are already covered by the iter-2 `step1_remaps_*` tests above
    /// that exercise their `rewrite_string_ids_through_remap` helpers
    /// end-to-end through the binding-plane derivation path. Covering
    /// them here would require either exposing additional test-only APIs
    /// or duplicating the iter-2 coverage; neither adds assurance beyond
    /// what the dedicated iter-2 tests already provide.
    #[test]
    fn step1_remaps_all_stringid_holders_exhaustively() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::storage::interner::StringInterner;

        let mut graph = CodeGraph::new();
        let file_a = FileId::new(1);
        let interner: &mut StringInterner = graph.strings_mut();
        // 10 slots = 5 surfaces × 2 (canon + dup) interleaved.
        let start = interner.alloc_range(10).expect("alloc range");
        {
            let (s, rc) = interner.bulk_slices_mut(start, 10);
            for (i, label) in [
                "arena_name",
                "arena_name",
                "arena_qname",
                "arena_qname",
                "idx_key",
                "idx_key",
                "edge_alias",
                "edge_alias",
                "file_uri",
                "file_uri",
            ]
            .iter()
            .enumerate()
            {
                s[i] = Some(std::sync::Arc::from(*label));
                rc[i] = 1;
            }
        }
        let sid = |off: u32| crate::graph::unified::string::id::StringId::new(start + off);
        let arena_name_canon = sid(0);
        let arena_name_dup = sid(1);
        let arena_qname_canon = sid(2);
        let arena_qname_dup = sid(3);
        let idx_key_canon = sid(4);
        let idx_key_dup = sid(5);
        let edge_alias_canon = sid(6);
        let edge_alias_dup = sid(7);
        let file_uri_canon = sid(8);
        let file_uri_dup = sid(9);

        // (a) Arena — name + qualified_name reference duplicate slots.
        let node;
        let node2;
        {
            let arena = graph.nodes_mut();
            let mut entry = NodeEntry::new(NodeKind::Function, arena_name_dup, file_a);
            entry.qualified_name = Some(arena_qname_dup);
            node = arena.alloc(entry).expect("alloc arena node");
            node2 = arena
                .alloc(NodeEntry::new(NodeKind::Function, arena_name_dup, file_a))
                .expect("alloc arena node2");
        }
        // (b) Indices — key under the duplicate StringId.
        graph
            .indices_mut()
            .add(node, NodeKind::Function, idx_key_dup, None, file_a);
        graph
            .indices_mut()
            .add(node2, NodeKind::Function, idx_key_dup, None, file_a);
        // (c) File registry — register a file with source_uri = dup.
        let fid = graph
            .files_mut()
            .register_external_with_uri(
                "/virtual/Exhaustive.class",
                Some(crate::graph::node::Language::Java),
                Some(file_uri_dup),
            )
            .expect("register external");
        // (d) Edge store — Imports edge with alias = dup.
        graph.edges_mut().add_edge(
            node,
            node2,
            EdgeKind::Imports {
                alias: Some(edge_alias_dup),
                is_wildcard: false,
            },
            file_a,
        );

        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");

        // (a) Arena: name + qualified_name on every live node.
        for nid in [node, node2] {
            let entry = finalized.nodes().get(nid).expect("node survives");
            assert_eq!(entry.name, arena_name_canon, "arena name canonicalised");
            if let Some(q) = entry.qualified_name {
                assert_eq!(q, arena_qname_canon, "arena qname canonicalised");
            }
        }
        // (b) Indices: canonical bucket populated, duplicate bucket empty.
        assert!(finalized.indices().by_name(idx_key_dup).is_empty());
        assert!(!finalized.indices().by_name(idx_key_canon).is_empty());
        // (c) File registry.
        let view = finalized
            .files()
            .file_provenance(fid)
            .expect("provenance present");
        assert_eq!(view.source_uri, Some(file_uri_canon));
        // (d) Edge store: every Imports alias canonical on both
        // directions. Post-finalize, edges live in CSR (step 9 drained
        // the delta into the CSR on both directions). Step 1's remap
        // ran on the delta *before* step 9, so the CSR is built from
        // the already-remapped delta.
        let fwd = finalized.edges().forward();
        let fwd_csr = fwd
            .csr()
            .expect("forward CSR must be populated after step 9");
        let mut fwd_imports = 0usize;
        for kind in fwd_csr.edge_kind() {
            if let EdgeKind::Imports { alias, .. } = kind {
                fwd_imports += 1;
                assert_ne!(*alias, Some(edge_alias_dup));
                assert_eq!(*alias, Some(edge_alias_canon));
            }
        }
        drop(fwd);
        assert!(fwd_imports > 0, "forward Imports must be present in CSR");
        let rev = finalized.edges().reverse();
        let rev_csr = rev
            .csr()
            .expect("reverse CSR must be populated after step 9");
        let mut rev_imports = 0usize;
        for kind in rev_csr.edge_kind() {
            if let EdgeKind::Imports { alias, .. } = kind {
                rev_imports += 1;
                assert_ne!(*alias, Some(edge_alias_dup));
                assert_eq!(*alias, Some(edge_alias_canon));
            }
        }
        drop(rev);
        assert!(rev_imports > 0, "reverse Imports must be present in CSR");

        // All duplicate slots recycled; canonical slots retained.
        for (dup, canon, label) in [
            (arena_name_dup, arena_name_canon, "arena_name"),
            (arena_qname_dup, arena_qname_canon, "arena_qname"),
            (idx_key_dup, idx_key_canon, "idx_key"),
            (edge_alias_dup, edge_alias_canon, "edge_alias"),
            (file_uri_dup, file_uri_canon, "file_uri"),
        ] {
            assert_eq!(
                finalized.strings().ref_count(dup),
                0,
                "{label}: duplicate slot must be recycled (ref_count == 0)"
            );
            assert!(
                finalized.strings().ref_count(canon) > 0,
                "{label}: canonical slot must retain references (got {})",
                finalized.strings().ref_count(canon)
            );
        }
    }

    // ----------------------------------------------------------------
    // Iter-2 B2: step 6 + step 13 per_file_nodes / bucket bijection.
    // ----------------------------------------------------------------

    #[test]
    fn finalize_step6_drops_tombstoned_nodes_from_buckets() {
        let (graph, a, b, c) = seeded_graph();
        // Seed the registry buckets so finalize step 6 has real work.
        let file_a = FileId::new(1);
        let file_b = FileId::new(2);
        {
            // Direct access via clone-for-rebuild — we need to populate
            // the REBUILD-local registry because `seeded_graph` doesn't.
        }
        // Populate buckets on the graph first so clone captures them.
        {
            let files = graph.files();
            let _ = files; // ensure immut ref scope
        }
        let mut graph = graph;
        graph.files_mut().record_node(file_a, a);
        graph.files_mut().record_node(file_a, b);
        graph.files_mut().record_node(file_b, c);

        let mut rebuild = graph.clone_for_rebuild();
        rebuild.tombstone(a);
        rebuild.tombstone(c);
        let finalized = rebuild.finalize().expect("finalize ok");

        // a and c were tombstoned — must be absent from any bucket.
        // b survives under file_a; file_b's bucket collapsed to empty
        // and must be dropped.
        let buckets: std::collections::BTreeMap<FileId, Vec<crate::graph::unified::node::NodeId>> =
            finalized.files().per_file_nodes_for_gate0d().collect();
        assert_eq!(buckets.len(), 1, "empty bucket must be dropped");
        assert_eq!(buckets.get(&file_a).cloned().unwrap_or_default(), vec![b]);
        assert!(!buckets.contains_key(&file_b));
    }

    #[test]
    fn finalize_step6_dedups_within_bucket() {
        let (mut graph, a, b, c) = seeded_graph();
        let file_a = FileId::new(1);
        let file_b = FileId::new(2);
        // Bucket every live node consistently with seeded_graph's own
        // FileId assignment (so the non-vacuous bijection check passes),
        // but duplicate `a` once to exercise step 6's dedup path.
        graph.files_mut().record_node(file_a, a);
        graph.files_mut().record_node(file_a, a); // intentional duplicate
        graph.files_mut().record_node(file_a, b);
        graph.files_mut().record_node(file_b, c);
        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");
        let buckets: std::collections::BTreeMap<FileId, Vec<crate::graph::unified::node::NodeId>> =
            finalized.files().per_file_nodes_for_gate0d().collect();
        let bucket_a = buckets.get(&file_a).expect("bucket for file_a");
        // Two live nodes (a, b) in file_a, duplicate `a` collapsed.
        assert_eq!(
            bucket_a.len(),
            2,
            "duplicates within bucket must be dedup'd"
        );
        assert!(bucket_a.contains(&a));
        assert!(bucket_a.contains(&b));
    }

    #[test]
    fn finalize_step6_drops_empty_buckets() {
        let (mut graph, a, _b, _c) = seeded_graph();
        let file = FileId::new(1);
        graph.files_mut().record_node(file, a);
        let mut rebuild = graph.clone_for_rebuild();
        rebuild.tombstone(a); // drops the only bucket member
        let finalized = rebuild.finalize().expect("finalize ok");
        assert_eq!(
            finalized.files().per_file_bucket_count(),
            0,
            "empty bucket must be dropped by step 6"
        );
    }

    #[test]
    fn bucket_bijection_passes_when_every_live_node_is_bucketed() {
        let (mut graph, a, b, c) = seeded_graph();
        // Bucket every live node consistently with each node's FileId.
        // seeded_graph uses file_a=1 for a, b and file_b=2 for c.
        let file_a = FileId::new(1);
        let file_b = FileId::new(2);
        graph.files_mut().record_node(file_a, a);
        graph.files_mut().record_node(file_a, b);
        graph.files_mut().record_node(file_b, c);

        // Round-trip through finalize so the bijection check runs.
        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");
        // Explicit assert_bucket_bijection call also passes.
        finalized.assert_bucket_bijection();
    }

    #[test]
    #[should_panic(expected = "duplicate node")]
    fn bucket_bijection_detects_duplicate_within_bucket() {
        let (mut graph, a, _b, _c) = seeded_graph();
        let file_a = FileId::new(1);
        // Manually force a duplicate inside a bucket by bypassing
        // `retain_nodes_in_buckets`. We achieve this by calling
        // `record_node` twice on the live graph and then invoking the
        // bijection check directly *without* routing through finalize
        // (which would dedup in step 6).
        graph.files_mut().record_node(file_a, a);
        graph.files_mut().record_node(file_a, a);
        graph.assert_bucket_bijection();
    }

    #[test]
    #[should_panic(expected = "misfiled")]
    fn bucket_bijection_detects_misfiled_node() {
        let (mut graph, a, _b, _c) = seeded_graph();
        // Node `a` was allocated against FileId(1); put it in a bucket
        // keyed by FileId(99) — the bijection must reject with the
        // precise "misfiled" panic message emitted by
        // `CodeGraph::assert_bucket_bijection` (concurrent/graph.rs
        // around line 865). A broader `expected = "node"` would also
        // match the duplicate-node / dead-node / multi-bucket arms, so
        // it would not discriminate the specific failure mode under test.
        graph.files_mut().record_node(FileId::new(99), a);
        graph.assert_bucket_bijection();
    }

    #[test]
    #[should_panic(expected = "absent from all buckets")]
    fn bucket_bijection_detects_missing_live_node() {
        let (mut graph, a, _b, c) = seeded_graph();
        // Bucket one node but not the other; with at least one populated
        // bucket, condition (d) becomes strict and the missing live node
        // must trigger a panic.
        let file_a = FileId::new(1);
        let _ = c;
        graph.files_mut().record_node(file_a, a);
        graph.assert_bucket_bijection();
    }

    #[test]
    #[should_panic(expected = "dead node")]
    fn bucket_bijection_detects_dead_node_in_bucket() {
        let (mut graph, a, _b, _c) = seeded_graph();
        // Tombstone a, then leave it in a bucket — bijection must reject.
        let file_a = FileId::new(1);
        graph.files_mut().record_node(file_a, a);
        graph.nodes_mut().remove(a);
        graph.assert_bucket_bijection();
    }

    // -----------------------------------------------------------------
    // Gate 0d — Tombstone-residue negative tests.
    //
    // Covers both the `RebuildGraph::assert_no_tombstone_residue`
    // diagnostic helper and the finalize step-14 publish-boundary
    // check. The bijection counterparts live in the block immediately
    // above.
    // -----------------------------------------------------------------

    #[test]
    #[should_panic(expected = "still in edge store")]
    fn rebuild_graph_residue_detects_tombstone_still_in_edge_store() {
        // Gate 0d iter-1 Minor — dedicated negative test for the
        // edge-store residue arm.
        //
        // The residue check iterates K-rows in order (see
        // `assert_no_tombstone_residue` above): NodeArena → edges →
        // AuxiliaryIndices → macro_metadata → ... — so to exercise the
        // `edges` arm specifically we must remove `a` from the
        // NodeArena first (so the NodeArena arm does not fire), leave
        // the edge that references `a` intact, and tombstone it.
        use crate::graph::unified::edge::kind::EdgeKind;
        #[cfg(test)]
        use crate::graph::unified::edge::kind::ResolvedVia;
        let (graph, a, b, _c) = seeded_graph();
        let mut rebuild = graph.clone_for_rebuild();
        // Seed the edge store with an edge that references `a`. This
        // becomes the "dangling reference" the residue check must
        // detect: `a` will be removed from the arena but the edge
        // keeps pointing at it.
        rebuild.edges.add_edge(
            a,
            b,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            FileId::new(1),
        );
        // Remove `a` from the arena so the K.A1 arm passes.
        rebuild.nodes.remove(a);
        rebuild.tombstone(a);
        rebuild.assert_no_tombstone_residue();
    }

    #[test]
    #[should_panic(expected = "still in auxiliary indices")]
    fn rebuild_graph_residue_detects_tombstone_still_in_auxiliary_indices() {
        // The residue check iterates K-rows starting at `NodeArena`, so
        // to exercise the `AuxiliaryIndices` arm specifically we must
        // first remove `a` from the arena (simulating step 2 of
        // finalize), leaving the `AuxiliaryIndices` stale reference
        // as the first arm that can trip. This reproduces the exact
        // failure mode the pre-finalize helper exists to catch: a bug
        // where finalize compacts the arena but forgets one index.
        let (graph, a, _b, _c) = seeded_graph();
        let mut rebuild = graph.clone_for_rebuild();
        rebuild.nodes.remove(a);
        rebuild.tombstone(a);
        rebuild.assert_no_tombstone_residue();
    }

    #[test]
    #[should_panic(expected = "still in NodeArena")]
    fn rebuild_graph_residue_detects_tombstone_still_in_node_arena() {
        // NodeArena is the first `NodeIdBearing` in the residue check's
        // iteration order, so we can trip it with a raw `tombstone(a)`
        // call before we touch any auxiliary index. The live arena
        // entry for `a` remains — that is the bug we want to surface.
        let (graph, a, _b, _c) = seeded_graph();
        let mut rebuild = graph.clone_for_rebuild();
        // Prove the arena still contains `a` before the assertion so
        // the panic expectation maps to a real condition, not a vacuous
        // empty-tombstone skip.
        assert!(
            rebuild.nodes.get(a).is_some(),
            "pre-condition: arena must still contain the staged tombstone"
        );
        rebuild.tombstone(a);
        rebuild.assert_no_tombstone_residue();
    }

    #[test]
    fn rebuild_graph_residue_is_noop_on_empty_tombstone_set() {
        // Positive: with no tombstones staged, the assertion must be a
        // no-op — otherwise every fresh `clone_for_rebuild` would panic.
        let (graph, _a, _b, _c) = seeded_graph();
        let rebuild = graph.clone_for_rebuild();
        assert_eq!(rebuild.pending_tombstone_count(), 0);
        rebuild.assert_no_tombstone_residue();
    }

    #[test]
    #[should_panic(expected = "still in NodeArena")]
    fn finalize_step14_residue_detects_live_reference_to_drained_node() {
        // Drive the `finalize` step-14 residue assertion through the
        // publish-boundary helper. `seeded_graph()` returns a graph in
        // which `a` is a live NodeArena entry; passing `a` in the
        // `drained` set without actually removing the arena slot
        // simulates the bug step 14 exists to catch — step 8 drained
        // `a` into `drained_tombstones`, but something failed to
        // compact the arena. The residue check's K-row iteration
        // starts at `NodeArena`, so that is the arm that fires.
        //
        // The test routes through the canonical
        // `publish::assert_publish_invariants` helper so the test
        // exercises the exact code path `finalize` step 14 executes,
        // not just the underlying `assert_no_tombstone_residue_for`
        // entry point.
        let (graph, a, _b, _c) = seeded_graph();
        let mut drained: ::std::collections::HashSet<NodeId> = ::std::collections::HashSet::new();
        drained.insert(a);
        crate::graph::unified::publish::assert_publish_invariants(&graph, &drained);
    }

    #[test]
    fn finalize_with_empty_drained_set_passes_publish_invariants() {
        // Positive smoke test: the happy path through finalize (no
        // tombstones staged) must pass `assert_publish_invariants`
        // unconditionally on every build profile. This is the case the
        // full-rebuild `build_unified_graph_inner` end-of-function call
        // exercises on every CI run.
        let (graph, _a, _b, _c) = seeded_graph();
        let file_a = FileId::new(1);
        let file_b = FileId::new(2);
        let mut graph = graph;
        graph.files_mut().record_node(file_a, _a);
        graph.files_mut().record_node(file_a, _b);
        graph.files_mut().record_node(file_b, _c);

        let rebuild = graph.clone_for_rebuild();
        let finalized = rebuild.finalize().expect("finalize ok");
        crate::graph::unified::publish::assert_publish_invariants(
            &finalized,
            &::std::collections::HashSet::new(),
        );
    }

    // ---- Task 4 Step 3 — RebuildGraph::remove_file ------------------

    /// Seed a graph + rebuild with 2 files × `per_file` nodes and a
    /// mix of intra- and cross-file `Calls` edges, then clone for
    /// rebuild. Returns `(rebuild, file_a, file_b, file_a_nodes,
    /// file_b_nodes)` — mirrors `seed_two_file_graph` in the
    /// `concurrent::graph::tests` module but yields the rebuild value
    /// so tests here can drive `RebuildGraph::remove_file` directly.
    fn seed_two_file_rebuild(
        per_file: usize,
    ) -> (
        RebuildGraph,
        crate::graph::unified::file::FileId,
        crate::graph::unified::file::FileId,
        Vec<NodeId>,
        Vec<NodeId>,
    ) {
        use crate::graph::unified::edge::{EdgeKind, ResolvedVia};
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();
        let sym = graph.strings_mut().intern("sym").expect("intern");
        let file_a = graph
            .files_mut()
            .register(Path::new("/tmp/rebuild_remove_file_test/a.rs"))
            .expect("register a");
        let file_b = graph
            .files_mut()
            .register(Path::new("/tmp/rebuild_remove_file_test/b.rs"))
            .expect("register b");

        let mut file_a_nodes = Vec::with_capacity(per_file);
        let mut file_b_nodes = Vec::with_capacity(per_file);
        for _ in 0..per_file {
            let n = graph
                .nodes_mut()
                .alloc(NodeEntry::new(NodeKind::Function, sym, file_a))
                .expect("alloc a");
            file_a_nodes.push(n);
            graph.files_mut().record_node(file_a, n);
            graph
                .indices_mut()
                .add(n, NodeKind::Function, sym, None, file_a);
        }
        for _ in 0..per_file {
            let n = graph
                .nodes_mut()
                .alloc(NodeEntry::new(NodeKind::Function, sym, file_b))
                .expect("alloc b");
            file_b_nodes.push(n);
            graph.files_mut().record_node(file_b, n);
            graph
                .indices_mut()
                .add(n, NodeKind::Function, sym, None, file_b);
        }
        for i in 0..per_file.saturating_sub(1) {
            graph.edges_mut().add_edge(
                file_a_nodes[i],
                file_a_nodes[i + 1],
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                file_a,
            );
            graph.edges_mut().add_edge(
                file_b_nodes[i],
                file_b_nodes[i + 1],
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                file_b,
            );
        }
        graph.edges_mut().add_edge(
            file_a_nodes[0],
            file_b_nodes[0],
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_a,
        );
        graph.edges_mut().add_edge(
            file_b_nodes[0],
            file_a_nodes[0],
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_b,
        );

        let rebuild = graph.clone_for_rebuild();
        (rebuild, file_a, file_b, file_a_nodes, file_b_nodes)
    }

    #[test]
    fn rebuild_remove_file_tombstones_all_per_file_nodes() {
        let (mut rebuild, file_a, _file_b, file_a_nodes, _) = seed_two_file_rebuild(3);

        let returned = rebuild.remove_file(file_a);

        let returned_set: std::collections::HashSet<NodeId> = returned.iter().copied().collect();
        let expected_set: std::collections::HashSet<NodeId> =
            file_a_nodes.iter().copied().collect();
        assert_eq!(
            returned_set, expected_set,
            "remove_file must return exactly the file_a nodes drained from the bucket"
        );
        // Each returned NodeId is arena-gone on the rebuild.
        for nid in &file_a_nodes {
            assert!(
                rebuild.nodes.get(*nid).is_none(),
                "node {nid:?} from removed file must be tombstoned on rebuild arena"
            );
        }
        // And staged for the finalize-time NodeIdBearing sweep.
        assert_eq!(rebuild.pending_tombstone_count(), file_a_nodes.len());
    }

    #[test]
    fn rebuild_remove_file_invalidates_all_edges_sourced_or_targeted_at_removed_nodes() {
        let (mut rebuild, file_a, _file_b, file_a_nodes, file_b_nodes) = seed_two_file_rebuild(3);

        // Seed produces 6 forward delta edges (2 intra-A + 2 intra-B
        // + 2 cross). The rebuild's own forward/reverse edge stores
        // mirror this.
        assert_eq!(rebuild.edges.forward().delta().len(), 6);

        let _ = rebuild.remove_file(file_a);

        // After removal: only 2 intra-B edges remain in each direction.
        assert_eq!(rebuild.edges.forward().delta().len(), 2);
        assert_eq!(rebuild.edges.reverse().delta().len(), 2);

        // Cross-file edge b[0] -> a[0] must be gone from any direction.
        let b0 = file_b_nodes[0];
        let a0 = file_a_nodes[0];
        let from_b0: Vec<_> = rebuild
            .edges
            .edges_from(b0)
            .into_iter()
            .filter(|e| e.target == a0)
            .collect();
        assert!(
            from_b0.is_empty(),
            "edge b0 -> a0 must be gone after rebuild.remove_file(file_a)"
        );
    }

    #[test]
    fn rebuild_remove_file_drops_file_registry_entry() {
        let (mut rebuild, file_a, _file_b, _, _) = seed_two_file_rebuild(2);

        assert!(rebuild.files.resolve(file_a).is_some());
        assert!(!rebuild.files.nodes_for_file(file_a).is_empty());

        let _ = rebuild.remove_file(file_a);

        assert!(
            rebuild.files.resolve(file_a).is_none(),
            "rebuild FileRegistry entry must be gone"
        );
        assert!(
            rebuild.files.nodes_for_file(file_a).is_empty(),
            "rebuild per-file bucket for file_a must be drained"
        );
    }

    #[test]
    fn rebuild_remove_file_is_idempotent_on_unknown_file() {
        use crate::graph::unified::file::FileId;
        let (mut rebuild, _file_a, _file_b, _, _) = seed_two_file_rebuild(2);

        let nodes_before = rebuild.nodes.len();
        let delta_fwd_before = rebuild.edges.forward().delta().len();
        let delta_rev_before = rebuild.edges.reverse().delta().len();
        let tombstones_before = rebuild.pending_tombstone_count();

        let bogus = FileId::new(9999);
        let returned = rebuild.remove_file(bogus);
        assert!(returned.is_empty());

        assert_eq!(rebuild.nodes.len(), nodes_before);
        assert_eq!(rebuild.edges.forward().delta().len(), delta_fwd_before);
        assert_eq!(rebuild.edges.reverse().delta().len(), delta_rev_before);
        assert_eq!(rebuild.pending_tombstone_count(), tombstones_before);
    }

    #[test]
    fn rebuild_remove_file_stages_tombstones_for_finalize_sweep() {
        // The whole point of RebuildGraph::remove_file deferring the
        // K.A/K.B sweep to finalize() is that the sweep happens exactly
        // once against the union of every file's tombstones. Drive this
        // end-to-end: remove both files, finalize, and assert the
        // publish-boundary invariants hold.
        let (mut rebuild, file_a, file_b, file_a_nodes, file_b_nodes) = seed_two_file_rebuild(2);

        let _ = rebuild.remove_file(file_a);
        let _ = rebuild.remove_file(file_b);

        // Pending tombstones: union of both files' nodes.
        assert_eq!(
            rebuild.pending_tombstone_count(),
            file_a_nodes.len() + file_b_nodes.len()
        );

        // Pre-finalize residue check against the rebuild-local state
        // is expected to pass: every NodeIdBearing surface on the
        // rebuild must already be clean of the tombstoned IDs (via the
        // immediate arena + edge tombstoning in step 1–2 of
        // remove_file) — NO, the NodeIdBearing K.A/K.B surfaces beyond
        // NodeArena/edges are NOT touched before finalize. The
        // assert_no_tombstone_residue helper on RebuildGraph walks
        // every surface, so it will legitimately find tombstones in
        // the auxiliary indices + metadata stores until finalize runs.
        //
        // So: do NOT run the pre-finalize residue check here. Instead,
        // confirm finalize runs successfully and the assembled
        // CodeGraph passes the publish-boundary invariants against
        // the drained set — that is the real post-condition.
        let finalized = rebuild.finalize().expect("finalize must succeed");

        // Every surface on the finalized CodeGraph must be clean of the
        // tombstoned ids. Use the publish-boundary residue helper
        // against an empty dead set (finalize's own step-14 call already
        // covered the drained set; we re-verify the bijection for
        // paranoia).
        crate::graph::unified::publish::assert_publish_bijection(&finalized);
        // Arena is empty (every seeded node was tombstoned).
        assert_eq!(finalized.nodes().len(), 0);
        // No file registration survives.
        assert!(finalized.files().resolve(file_a).is_none());
        assert!(finalized.files().resolve(file_b).is_none());
    }

    #[test]
    fn rebuild_remove_file_repeated_calls_are_idempotent() {
        let (mut rebuild, file_a, _file_b, file_a_nodes, _) = seed_two_file_rebuild(3);

        let first = rebuild.remove_file(file_a);
        assert_eq!(first.len(), file_a_nodes.len());
        let staged = rebuild.pending_tombstone_count();

        // Second call: bucket is already drained, NodeArena::remove
        // ignores stale generations, and the tombstones set should not
        // grow. The immediate tombstone side effects are idempotent.
        let second = rebuild.remove_file(file_a);
        assert!(second.is_empty());
        assert_eq!(rebuild.pending_tombstone_count(), staged);
    }

    #[test]
    fn rebuild_remove_file_clears_file_segments_entry() {
        // Iter-1 Codex review fix mirror of the CodeGraph-side test
        // in `concurrent/graph.rs`. Seed a segment for file A, invoke
        // `RebuildGraph::remove_file`, and assert both the
        // rebuild-local `file_segments` and the finalized `CodeGraph`'s
        // `file_segments` table carry no entry for file A. The second
        // half of the check is critical: `finalize()` step 12 publishes
        // `self.file_segments` verbatim, so a missing clear here would
        // leak the stale range into the publishable graph.
        use std::path::Path;

        let (mut rebuild, file_a, _file_b, file_a_nodes, _) = seed_two_file_rebuild(3);

        // Seed a segment for file A. Fish out the span from the
        // allocated NodeIds to mirror the shape `phase3_parallel_commit`
        // produces.
        let first_index = file_a_nodes
            .iter()
            .map(|n| n.index())
            .min()
            .expect("per_file = 3");
        let last_index = file_a_nodes
            .iter()
            .map(|n| n.index())
            .max()
            .expect("per_file = 3");
        let slot_count = last_index - first_index + 1;
        rebuild
            .file_segments
            .record_range(file_a, first_index, slot_count);
        assert!(
            rebuild.file_segments().get(file_a).is_some(),
            "seeded segment for file_a must be present before remove_file"
        );

        // Act.
        let _ = rebuild.remove_file(file_a);

        // Rebuild-local post-condition.
        assert!(
            rebuild.file_segments().get(file_a).is_none(),
            "RebuildGraph::remove_file must clear the file_segments entry"
        );

        // Finalize post-condition: the published CodeGraph must also
        // carry no entry for file_a.
        let finalized = rebuild.finalize().expect("finalize must succeed");
        assert!(
            finalized.file_segments().get(file_a).is_none(),
            "finalize must publish a CodeGraph with no stale file_segments for file_a"
        );

        // Defensive suppression: the Path import is used only by the
        // adjacent `seed_two_file_rebuild` helper when the signature
        // is exercised — the let-binding below keeps this test
        // self-contained even if the helper's surface changes later.
        let _ = Path::new("");
    }
}
