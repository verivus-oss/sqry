//! `CodeGraph` and `ConcurrentCodeGraph` implementations.
//!
//! This module provides the core graph types with thread-safe access:
//!
//! - [`CodeGraph`]: Arc-wrapped internals for O(1) `CoW` snapshots
//! - [`ConcurrentCodeGraph`]: `RwLock` wrapper with epoch versioning
//! - [`GraphSnapshot`]: Immutable snapshot for long-running queries

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::confidence::ConfidenceMetadata;
use crate::graph::unified::bind::alias::AliasTable;
use crate::graph::unified::bind::scope::provenance::{
    ScopeProvenance, ScopeProvenanceStore, ScopeStableId,
};
use crate::graph::unified::bind::scope::{ScopeArena, ScopeId};
use crate::graph::unified::bind::shadow::ShadowTable;
use crate::graph::unified::edge::EdgeKind;
#[cfg(test)]
use crate::graph::unified::edge::ResolvedVia;
use crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore;
use crate::graph::unified::file::FileId;
use crate::graph::unified::memory::{GraphMemorySize, HASHMAP_ENTRY_OVERHEAD};
use crate::graph::unified::resolution::display_graph_qualified_name;
use crate::graph::unified::storage::arena::NodeArena;
use crate::graph::unified::storage::c_indirect::CIndirectSideTables;
use crate::graph::unified::storage::edge_provenance::{EdgeProvenance, EdgeProvenanceStore};
use crate::graph::unified::storage::indices::AuxiliaryIndices;
use crate::graph::unified::storage::interner::StringInterner;
use crate::graph::unified::storage::metadata::NodeMetadataStore;
use crate::graph::unified::storage::node_provenance::{NodeProvenance, NodeProvenanceStore};
use crate::graph::unified::storage::registry::{FileProvenanceView, FileRegistry};
use crate::graph::unified::storage::segment::FileSegmentTable;
use crate::graph::unified::string::id::StringId;

/// Core graph with Arc-wrapped internals for O(1) `CoW` snapshots.
///
/// `CodeGraph` uses `Arc` for all internal components, enabling:
/// - O(1) snapshot creation via Arc cloning
/// - Copy-on-write semantics via `Arc::make_mut`
/// - Memory-efficient sharing between snapshots
///
/// # Design
///
/// The Arc wrapping enables the MVCC pattern:
/// - Readers see a consistent snapshot at the time they acquired access
/// - Writers use `Arc::make_mut` to get exclusive copies only when mutating
/// - Multiple snapshots can coexist without copying data
///
/// # Performance
///
/// - Snapshot creation: O(5) Arc clones ≈ <1μs
/// - Read access: Direct Arc dereference, no locking
/// - Write access: `Arc::make_mut` clones only if refcount > 1
///
/// # Phase 2 binding-plane access
///
/// Use the two-line snapshot pattern to access `BindingPlane`:
///
/// ```rust,ignore
/// let snapshot = graph.snapshot();
/// let plane = snapshot.binding_plane();
/// let resolution = plane.resolve(&query);
/// ```
///
/// The two-line form is intentional: `BindingPlane<'g>` borrows from
/// `GraphSnapshot` and the explicit snapshot handle makes the MVCC lifetime
/// visible at the call site. The full Phase 2 scope/alias/shadow and
/// witness-bearing resolution API is exposed through `BindingPlane`.
// Field visibility is `pub(crate)` so the Gate 0c `rebuild_graph` module
// (A2 §H) can destructure `CodeGraph` exhaustively in `clone_for_rebuild`.
// External crates still go through the public accessor methods below.
#[derive(Clone)]
pub struct CodeGraph {
    /// Node storage with generational indices.
    pub(crate) nodes: Arc<NodeArena>,
    /// Bidirectional edge storage (forward + reverse).
    pub(crate) edges: Arc<BidirectionalEdgeStore>,
    /// String interner for symbol names.
    pub(crate) strings: Arc<StringInterner>,
    /// File registry for path deduplication.
    pub(crate) files: Arc<FileRegistry>,
    /// Auxiliary indices for fast lookup.
    pub(crate) indices: Arc<AuxiliaryIndices>,
    /// Sparse macro boundary metadata (keyed by full `NodeId`).
    pub(crate) macro_metadata: Arc<NodeMetadataStore>,
    /// Dense node provenance (Phase 1 fact layer).
    pub(crate) node_provenance: Arc<NodeProvenanceStore>,
    /// Dense edge provenance (Phase 1 fact layer).
    pub(crate) edge_provenance: Arc<EdgeProvenanceStore>,
    /// Monotonic fact-layer epoch (0 until set by the V8 persistence path).
    pub(crate) fact_epoch: u64,
    /// Epoch for version tracking.
    pub(crate) epoch: u64,
    /// Per-language confidence metadata collected during build.
    /// Maps language name (e.g., "rust") to aggregated confidence.
    pub(crate) confidence: HashMap<String, ConfidenceMetadata>,
    /// Phase 2 binding-plane scope arena (populated by Phase 4e).
    pub(crate) scope_arena: Arc<ScopeArena>,
    /// Phase 2 binding-plane alias table (populated by Phase 4e / P2U04).
    pub(crate) alias_table: Arc<AliasTable>,
    /// Phase 2 binding-plane shadow table (populated by Phase 4e / P2U05).
    pub(crate) shadow_table: Arc<ShadowTable>,
    /// Phase 2 binding-plane scope provenance store (populated by Phase 4e / P2U11).
    pub(crate) scope_provenance_store: Arc<ScopeProvenanceStore>,
    /// Phase 3 file segment table mapping `FileId` to contiguous node ranges.
    /// Populated during build Phase 3 parallel commit, persisted in V10+ snapshots.
    pub(crate) file_segments: Arc<FileSegmentTable>,
    /// Phase A (U09): C-only indirect-call resolver side tables.
    ///
    /// `None` on non-C workspaces — the parent slot stays unallocated so
    /// non-C builds incur zero side-table overhead. Populated by Phase 3
    /// commit (U11) and consumed by `pass5b_c_indirect_resolve` (U12).
    /// Persisted as the V11 `Option<CIndirectSideTables>` envelope slot
    /// (DESIGN §10.2); V10 → V11 upconvert sets this to `None`.
    pub(crate) c_indirect_tables: Option<CIndirectSideTables>,
    /// Build-time scratch side-channel from the Go plugin (Cluster A
    /// of the Go T1 implements-and-promotion design).
    ///
    /// Populated during Phase 1 plugin parse, merged into the live
    /// target by Phase 3 commit after `NodeId` / `StringId` remap, drained
    /// by `pass_go_method_set_satisfaction` between Phase 4e and Pass
    /// 5. Not part of `GraphSnapshot`, not persisted in V10 — see
    /// `02_DESIGN` §6. Held by-value (not `Arc<…>`) because no
    /// copy-on-write or shared-reader access is required: only the
    /// rebuild owner mutates, only the pass consumes, then the field
    /// is reset.
    pub(crate) go_hints: crate::graph::unified::build::staging::GoHints,
    /// Whether this graph carries genuine `NodeEntry.is_definition` signal
    /// (R3 marker for the definition-signal feature).
    ///
    /// `true` for graphs freshly built in-process (the GraphBuilder path stamps
    /// `is_definition` at declaration sites) and for graphs loaded from a
    /// V16-or-newer snapshot. `false` for graphs loaded from a pre-V16 snapshot,
    /// whose `is_definition` bits are all defaulted `false` (absent on the wire)
    /// and must NOT be interpreted as "no declarations exist." Consumers that
    /// filter on `is_definition` (e.g. items-only listings) read this marker to
    /// decide whether the signal is trustworthy or a reindex is required.
    pub(crate) definition_signal_present: bool,
}

impl CodeGraph {
    /// Creates a new empty `CodeGraph`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// assert_eq!(graph.epoch(), 0);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Arc::new(NodeArena::new()),
            edges: Arc::new(BidirectionalEdgeStore::new()),
            strings: Arc::new(StringInterner::new()),
            files: Arc::new(FileRegistry::new()),
            indices: Arc::new(AuxiliaryIndices::new()),
            macro_metadata: Arc::new(NodeMetadataStore::new()),
            node_provenance: Arc::new(NodeProvenanceStore::new()),
            edge_provenance: Arc::new(EdgeProvenanceStore::new()),
            fact_epoch: 0,
            epoch: 0,
            confidence: HashMap::new(),
            scope_arena: Arc::new(ScopeArena::new()),
            alias_table: Arc::new(AliasTable::new()),
            shadow_table: Arc::new(ShadowTable::new()),
            scope_provenance_store: Arc::new(ScopeProvenanceStore::new()),
            file_segments: Arc::new(FileSegmentTable::new()),
            c_indirect_tables: None,
            go_hints: crate::graph::unified::build::staging::GoHints::default(),
            // Fresh in-process graph: the GraphBuilder path stamps is_definition.
            definition_signal_present: true,
        }
    }

    /// Creates a `CodeGraph` from existing components.
    ///
    /// This is useful when building a graph from external data or
    /// reconstructing from serialized state.
    #[must_use]
    pub fn from_components(
        nodes: NodeArena,
        edges: BidirectionalEdgeStore,
        strings: StringInterner,
        files: FileRegistry,
        indices: AuxiliaryIndices,
        macro_metadata: NodeMetadataStore,
    ) -> Self {
        Self {
            nodes: Arc::new(nodes),
            edges: Arc::new(edges),
            strings: Arc::new(strings),
            files: Arc::new(files),
            indices: Arc::new(indices),
            macro_metadata: Arc::new(macro_metadata),
            node_provenance: Arc::new(NodeProvenanceStore::new()),
            edge_provenance: Arc::new(EdgeProvenanceStore::new()),
            fact_epoch: 0,
            epoch: 0,
            confidence: HashMap::new(),
            scope_arena: Arc::new(ScopeArena::new()),
            alias_table: Arc::new(AliasTable::new()),
            shadow_table: Arc::new(ShadowTable::new()),
            scope_provenance_store: Arc::new(ScopeProvenanceStore::new()),
            file_segments: Arc::new(FileSegmentTable::new()),
            c_indirect_tables: None,
            go_hints: crate::graph::unified::build::staging::GoHints::default(),
            // Default to present; the load path overrides via
            // `set_definition_signal_present` based on the snapshot format
            // version (pre-V16 loads stamp `false`).
            definition_signal_present: true,
        }
    }

    /// Creates a cheap snapshot of the graph.
    ///
    /// This operation is O(5) Arc clones, which completes in <1μs.
    /// The snapshot is isolated from future mutations to the original graph.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// let snapshot = graph.snapshot();
    /// // snapshot is independent of future mutations to graph
    /// ```
    #[must_use]
    pub fn snapshot(&self) -> GraphSnapshot {
        GraphSnapshot {
            nodes: Arc::clone(&self.nodes),
            edges: Arc::clone(&self.edges),
            strings: Arc::clone(&self.strings),
            files: Arc::clone(&self.files),
            indices: Arc::clone(&self.indices),
            macro_metadata: Arc::clone(&self.macro_metadata),
            node_provenance: Arc::clone(&self.node_provenance),
            edge_provenance: Arc::clone(&self.edge_provenance),
            fact_epoch: self.fact_epoch,
            epoch: self.epoch,
            scope_arena: Arc::clone(&self.scope_arena),
            alias_table: Arc::clone(&self.alias_table),
            shadow_table: Arc::clone(&self.shadow_table),
            scope_provenance_store: Arc::clone(&self.scope_provenance_store),
            file_segments: Arc::clone(&self.file_segments),
            c_indirect_tables: self.c_indirect_tables.clone(),
            definition_signal_present: self.definition_signal_present,
        }
    }

    /// Returns whether this graph carries genuine `is_definition` signal.
    ///
    /// See [`definition_signal_present`](Self::definition_signal_present) (the
    /// field) for the full contract.
    #[inline]
    #[must_use]
    pub fn definition_signal_present(&self) -> bool {
        self.definition_signal_present
    }

    /// Sets the definition-signal marker.
    ///
    /// Used by the snapshot load path to stamp `false` for pre-V16 snapshots
    /// (whose `is_definition` bits are all defaulted) and `true` for V16+.
    #[inline]
    pub fn set_definition_signal_present(&mut self, present: bool) {
        self.definition_signal_present = present;
    }

    /// Returns a reference to the node arena.
    #[inline]
    #[must_use]
    pub fn nodes(&self) -> &NodeArena {
        &self.nodes
    }

    /// Returns a reference to the bidirectional edge store.
    #[inline]
    #[must_use]
    pub fn edges(&self) -> &BidirectionalEdgeStore {
        &self.edges
    }

    /// Returns a reference to the string interner.
    #[inline]
    #[must_use]
    pub fn strings(&self) -> &StringInterner {
        &self.strings
    }

    /// Returns a reference to the file registry.
    #[inline]
    #[must_use]
    pub fn files(&self) -> &FileRegistry {
        &self.files
    }

    /// Returns a reference to the auxiliary indices.
    #[inline]
    #[must_use]
    pub fn indices(&self) -> &AuxiliaryIndices {
        &self.indices
    }

    /// Returns a reference to the macro boundary metadata store.
    #[inline]
    #[must_use]
    pub fn macro_metadata(&self) -> &NodeMetadataStore {
        &self.macro_metadata
    }

    /// Returns a reference to the C indirect-call side tables, if any.
    ///
    /// `None` on non-C workspaces — the slot is allocated lazily and only
    /// when the C plugin's Phase 1 instrumentation observes content worth
    /// recording. On loaded snapshots, the V11 envelope's
    /// `Option<CIndirectSideTables>` field round-trips into this slot
    /// (DESIGN §10.2); V10 → V11 upconvert always sets the slot to `None`.
    ///
    /// `pass5b_c_indirect_resolve` (U12) consumes these tables to rewrite
    /// synthetic indirect-call `Calls` edges into precise binding-plane /
    /// type-match candidates.
    #[inline]
    #[must_use]
    pub fn c_indirect_tables(&self) -> Option<&CIndirectSideTables> {
        self.c_indirect_tables.as_ref()
    }

    /// Returns a mutable reference to the `Option<CIndirectSideTables>`
    /// slot, allowing callers to install / replace / clear the side
    /// tables.
    ///
    /// Mirrors the accessor pattern used for [`Self::macro_metadata_mut`]
    /// but exposes the `Option` directly — the side tables are owned in
    /// place, not behind an `Arc`. Callers that need to merge incremental
    /// state into the existing tables typically do:
    ///
    /// ```rust,ignore
    /// let slot = graph.c_indirect_tables_mut();
    /// let tables = slot.get_or_insert_with(CIndirectSideTables::new);
    /// tables.bindings_by_field.entry(key).or_default().push(entry);
    /// ```
    ///
    /// Populated by the build pipeline (U11). Read-only consumers should
    /// use [`Self::c_indirect_tables`] instead.
    #[inline]
    pub fn c_indirect_tables_mut(&mut self) -> &mut Option<CIndirectSideTables> {
        &mut self.c_indirect_tables
    }

    /// Test-only: strip every Phase-A-introduced piece of state from this
    /// graph, leaving a graph that is byte-identical (modulo persistence
    /// header timestamps) to what a pre-Phase-A build would have produced
    /// on the same fixture.
    ///
    /// Specifically:
    ///
    /// * Clears the [`CIndirectSideTables`] slot (sets it to `None`).
    /// * Removes [`NodeFlags::ADDRESS_TAKEN`] and
    ///   [`NodeFlags::CALLSITE_PROMISCUOUS`] marker bits from every
    ///   [`StoredEntry`] in the macro-metadata store.
    /// * Rewrites the `resolved_via` field of every [`EdgeKind::Calls`]
    ///   edge (across CSR and delta tiers, forward and reverse stores)
    ///   to [`ResolvedVia::Direct`].
    ///
    /// Used exclusively by `sqry-core/tests/snapshot_size_phase_a.rs`
    /// (U19) to materialize a "Phase-A-free" baseline snapshot for the
    /// +10% snapshot-size gate. Gated behind `cfg(any(test, feature =
    /// "test-support"))` so the helper is invisible to production
    /// builds.
    ///
    /// [`NodeFlags::ADDRESS_TAKEN`]: crate::graph::unified::storage::metadata::NodeFlags::ADDRESS_TAKEN
    /// [`NodeFlags::CALLSITE_PROMISCUOUS`]: crate::graph::unified::storage::metadata::NodeFlags::CALLSITE_PROMISCUOUS
    /// [`StoredEntry`]: crate::graph::unified::storage::metadata::StoredEntry
    /// [`EdgeKind::Calls`]: crate::graph::unified::edge::EdgeKind::Calls
    /// [`ResolvedVia::Direct`]: crate::graph::unified::edge::ResolvedVia::Direct
    #[cfg(any(test, feature = "test-support"))]
    pub fn clear_phase_a_state_for_test(&mut self) {
        *self.c_indirect_tables_mut() = None;
        self.macro_metadata_mut().clear_phase_a_flags_for_test();
        self.edges_mut().normalize_calls_resolved_via_for_test();
    }

    // ------------------------------------------------------------------
    // Phase 1 fact-layer provenance accessors (P1U09).
    // ------------------------------------------------------------------

    /// Returns the monotonic fact-layer epoch stamped on the most recently
    /// saved or loaded snapshot. Returns `0` for graphs that have not been
    /// persisted yet or were loaded from V7 snapshots.
    #[inline]
    #[must_use]
    pub fn fact_epoch(&self) -> u64 {
        self.fact_epoch
    }

    /// Looks up node provenance by `NodeId`.
    ///
    /// Returns `None` if the `NodeId` is out of range, the slot is vacant,
    /// or the stored generation does not match (stale handle).
    #[inline]
    #[must_use]
    pub fn node_provenance(
        &self,
        id: crate::graph::unified::node::id::NodeId,
    ) -> Option<&NodeProvenance> {
        self.node_provenance.lookup(id)
    }

    /// Looks up edge provenance by `EdgeId`.
    ///
    /// Returns `None` if the `EdgeId` is out of range, the slot is vacant,
    /// or the edge is the invalid sentinel.
    #[inline]
    #[must_use]
    pub fn edge_provenance(
        &self,
        id: crate::graph::unified::edge::id::EdgeId,
    ) -> Option<&EdgeProvenance> {
        self.edge_provenance.lookup(id)
    }

    /// Returns a borrowed provenance view for a file.
    ///
    /// Returns `None` for invalid/unregistered `FileId`s.
    #[inline]
    #[must_use]
    pub fn file_provenance(
        &self,
        id: crate::graph::unified::file::id::FileId,
    ) -> Option<FileProvenanceView<'_>> {
        self.files.file_provenance(id)
    }

    // ------------------------------------------------------------------
    // Phase 2 binding-plane accessors (P2U03).
    // ------------------------------------------------------------------

    /// Returns a reference to the scope arena derived during Phase 4e.
    ///
    /// The arena is empty on freshly-constructed `CodeGraph` instances and is
    /// populated by calling `phase4e_binding::derive_binding_plane`.
    #[inline]
    #[must_use]
    pub fn scope_arena(&self) -> &ScopeArena {
        &self.scope_arena
    }

    /// Installs a freshly-derived scope arena.
    ///
    /// Called from `phase4e_binding::derive_binding_plane` during the build
    /// pipeline. External callers that run Phase 4e manually (e.g., test
    /// fixture builders) use this to store the result.
    pub(crate) fn set_scope_arena(&mut self, arena: ScopeArena) {
        self.scope_arena = Arc::new(arena);
    }

    /// Returns a reference to the alias table derived during Phase 4e.
    ///
    /// The table is empty on freshly-constructed `CodeGraph` instances and is
    /// populated by calling `phase4e_binding::derive_binding_plane`.
    #[inline]
    #[must_use]
    pub fn alias_table(&self) -> &AliasTable {
        &self.alias_table
    }

    /// Installs a freshly-derived alias table.
    ///
    /// Called from `phase4e_binding::derive_binding_plane` during the build
    /// pipeline. External callers that run Phase 4e manually (e.g., test
    /// fixture builders) use this to store the result.
    pub(crate) fn set_alias_table(&mut self, table: AliasTable) {
        self.alias_table = Arc::new(table);
    }

    /// Returns a reference to the shadow table derived during Phase 4e.
    ///
    /// The table is empty on freshly-constructed `CodeGraph` instances and is
    /// populated by calling `phase4e_binding::derive_binding_plane`.
    #[inline]
    #[must_use]
    pub fn shadow_table(&self) -> &ShadowTable {
        &self.shadow_table
    }

    /// Installs a freshly-derived shadow table.
    ///
    /// Called from `phase4e_binding::derive_binding_plane` during the build
    /// pipeline. External callers that run Phase 4e manually (e.g., test
    /// fixture builders) use this to store the result.
    pub(crate) fn set_shadow_table(&mut self, table: ShadowTable) {
        self.shadow_table = Arc::new(table);
    }

    /// Returns a reference to the scope provenance store derived during Phase 4e.
    ///
    /// The store is empty on freshly-constructed `CodeGraph` instances and is
    /// populated by calling `phase4e_binding::derive_binding_plane`.
    #[inline]
    #[must_use]
    pub fn scope_provenance_store(&self) -> &ScopeProvenanceStore {
        &self.scope_provenance_store
    }

    /// Looks up scope provenance by `ScopeId`.
    ///
    /// Returns `None` if the slot is out of range, vacant, or the stored
    /// generation does not match (stale handle).
    #[inline]
    #[must_use]
    pub fn scope_provenance(&self, id: ScopeId) -> Option<&ScopeProvenance> {
        self.scope_provenance_store.lookup(id)
    }

    /// Looks up the live `ScopeId` for a stable scope identity.
    ///
    /// Returns `None` if no provenance record is registered for that stable id.
    /// The reverse index is populated by `insert` during Phase 4e and must be
    /// rebuilt after V9 deserialization.
    #[inline]
    #[must_use]
    pub fn scope_by_stable_id(&self, stable: ScopeStableId) -> Option<ScopeId> {
        self.scope_provenance_store.scope_by_stable_id(stable)
    }

    /// Installs a freshly-derived scope provenance store.
    ///
    /// Called from `phase4e_binding::derive_binding_plane` during the build
    /// pipeline. External callers that run Phase 4e manually (e.g., test
    /// fixture builders) use this to store the result.
    pub(crate) fn set_scope_provenance_store(&mut self, store: ScopeProvenanceStore) {
        self.scope_provenance_store = Arc::new(store);
    }

    /// Returns a reference to the file segment table.
    #[inline]
    #[must_use]
    pub fn file_segments(&self) -> &FileSegmentTable {
        &self.file_segments
    }

    /// Replaces the file segment table.
    pub(crate) fn set_file_segments(&mut self, table: FileSegmentTable) {
        self.file_segments = Arc::new(table);
    }

    /// Installs the C indirect-call side tables loaded from a V11
    /// snapshot.
    ///
    /// `None` clears the slot (used on V10 → V11 upconvert and on non-C
    /// workspaces). `Some(...)` carries the live tables onto the freshly
    /// reconstructed `CodeGraph`. Build-pipeline callers that incrementally
    /// populate the tables should prefer [`Self::c_indirect_tables_mut`].
    pub(crate) fn set_c_indirect_tables(&mut self, tables: Option<CIndirectSideTables>) {
        self.c_indirect_tables = tables;
    }

    /// Returns a mutable reference to the file segment table (via `Arc::make_mut`).
    pub(crate) fn file_segments_mut(&mut self) -> &mut FileSegmentTable {
        Arc::make_mut(&mut self.file_segments)
    }

    /// Test-only helper that records a file segment directly on the
    /// graph, bypassing the full Phase 3 commit pipeline. Only
    /// available under `#[cfg(feature = "rebuild-internals")]` so the
    /// surface is opt-in for rebuild-plane consumers (the feature is
    /// whitelisted to sqry-daemon + sqry-core integration tests; see
    /// `sqry-core/tests/rebuild_internals_whitelist.rs`).
    ///
    /// Integration tests (notably
    /// `sqry-core/tests/incremental_remove_file_scale.rs`) call this
    /// to seed the synthetic workspaces they build before exercising
    /// `RebuildGraph::remove_file` / `CodeGraph::remove_file`. Production
    /// code paths never touch this method — Phase 3 parallel commit
    /// is the sole production writer, via the crate-internal
    /// `file_segments_mut` accessor above.
    ///
    /// Renamed with `test_only_` prefix so the purpose is unambiguous
    /// at every call site; `#[doc(hidden)]` hides it from rendered
    /// rustdoc so downstream daemon integrations don't discover it by
    /// accident.
    #[cfg(feature = "rebuild-internals")]
    #[doc(hidden)]
    pub fn test_only_record_file_segment(
        &mut self,
        file_id: FileId,
        start_slot: u32,
        slot_count: u32,
    ) {
        Arc::make_mut(&mut self.file_segments).record_range(file_id, start_slot, slot_count);
    }

    /// Sets the provenance stores and fact epoch, typically called by the
    /// persistence loader after deserializing a V8 snapshot.
    pub(crate) fn set_provenance(
        &mut self,
        node_provenance: NodeProvenanceStore,
        edge_provenance: EdgeProvenanceStore,
        fact_epoch: u64,
    ) {
        self.node_provenance = Arc::new(node_provenance);
        self.edge_provenance = Arc::new(edge_provenance);
        self.fact_epoch = fact_epoch;
    }

    /// Returns the current epoch.
    #[inline]
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns a mutable reference to the node arena.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics: if other
    /// references exist (e.g., snapshots), the data is cloned.
    #[inline]
    pub fn nodes_mut(&mut self) -> &mut NodeArena {
        Arc::make_mut(&mut self.nodes)
    }

    /// Returns a mutable reference to the bidirectional edge store.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics.
    #[inline]
    pub fn edges_mut(&mut self) -> &mut BidirectionalEdgeStore {
        Arc::make_mut(&mut self.edges)
    }

    /// Returns a mutable reference to the string interner.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics.
    #[inline]
    pub fn strings_mut(&mut self) -> &mut StringInterner {
        Arc::make_mut(&mut self.strings)
    }

    /// Returns a mutable reference to the file registry.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics.
    #[inline]
    pub fn files_mut(&mut self) -> &mut FileRegistry {
        Arc::make_mut(&mut self.files)
    }

    /// Returns a mutable reference to the auxiliary indices.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics.
    #[inline]
    pub fn indices_mut(&mut self) -> &mut AuxiliaryIndices {
        Arc::make_mut(&mut self.indices)
    }

    /// Returns a mutable reference to the macro boundary metadata store.
    ///
    /// Uses `Arc::make_mut` for copy-on-write semantics.
    #[inline]
    pub fn macro_metadata_mut(&mut self) -> &mut NodeMetadataStore {
        Arc::make_mut(&mut self.macro_metadata)
    }

    /// Returns mutable references to both the node arena and the string interner.
    ///
    /// This avoids the borrow-conflict that arises when calling `nodes_mut()` and
    /// `strings_mut()` separately on `&mut self`.
    #[inline]
    pub fn nodes_and_strings_mut(&mut self) -> (&mut NodeArena, &mut StringInterner) {
        (
            Arc::make_mut(&mut self.nodes),
            Arc::make_mut(&mut self.strings),
        )
    }

    /// Rebuilds auxiliary indices from the current node arena.
    ///
    /// This avoids the borrow conflict that arises when calling `nodes()` and
    /// `indices_mut()` separately on `&mut self`. Uses disjoint field borrowing
    /// to access `nodes` (shared) and `indices` (mutable) simultaneously.
    /// Internally calls `AuxiliaryIndices::build_from_arena` which clears
    /// existing indices and rebuilds in a single pass without per-element
    /// duplicate checking.
    ///
    /// As of Task 4 Step 4 Phase 2 this inherent method delegates to the
    /// generic
    /// [`crate::graph::unified::build::parallel_commit::rebuild_indices`]
    /// free function so the same implementation serves both the
    /// full-build (`CodeGraph`) and incremental-rebuild (`RebuildGraph`)
    /// pipelines. Call sites that hold a concrete `CodeGraph` can keep
    /// using `graph.rebuild_indices()`; incremental rebuild call sites
    /// should use the free function directly.
    pub fn rebuild_indices(&mut self) {
        crate::graph::unified::build::parallel_commit::rebuild_indices(self);
    }

    /// Increments the epoch counter and returns the new value.
    ///
    /// Called automatically by `ConcurrentCodeGraph::write()`.
    #[inline]
    pub fn bump_epoch(&mut self) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }

    /// Sets the epoch to a specific value.
    ///
    /// This is primarily for testing or reconstruction from serialized state.
    #[inline]
    pub fn set_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
    }

    /// Returns the number of nodes in the graph.
    ///
    /// This is a convenience method that delegates to `nodes().len()`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// assert_eq!(graph.node_count(), 0);
    /// ```
    #[inline]
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Returns the number of edges in the graph (forward direction).
    ///
    /// This counts edges in the forward store, including both CSR and delta edges.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// assert_eq!(graph.edge_count(), 0);
    /// ```
    #[inline]
    #[must_use]
    pub fn edge_count(&self) -> usize {
        let stats = self.edges.stats();
        stats.forward.csr_edge_count + stats.forward.delta_edge_count
    }

    /// Returns true if the graph contains no nodes.
    ///
    /// This is a convenience method that delegates to `nodes().is_empty()`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// assert!(graph.is_empty());
    /// ```
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns an iterator over all indexed file paths.
    ///
    /// This is useful for enumerating all files that have been processed
    /// and added to the graph.
    ///
    /// # Example
    ///
    /// ```rust
    /// use sqry_core::graph::unified::concurrent::CodeGraph;
    ///
    /// let graph = CodeGraph::new();
    /// for (file_id, path) in graph.indexed_files() {
    ///     println!("File {}: {}", file_id.index(), path.display());
    /// }
    /// ```
    #[inline]
    pub fn indexed_files(
        &self,
    ) -> impl Iterator<Item = (crate::graph::unified::file::FileId, &std::path::Path)> {
        self.files
            .iter()
            .map(|(id, arc_path)| (id, arc_path.as_ref()))
    }

    /// Returns the set of files that import one or more symbols exported by
    /// `file_id`, deduplicated and sorted ascending.
    ///
    /// For every [`EdgeKind::Imports`] edge whose target node lives in
    /// `file_id`, the source node's [`FileId`] is added to the result. Files
    /// are returned sorted by raw index so the result is deterministic across
    /// runs. The caller's own file is never included — an `Imports` edge
    /// whose source and target both live in `file_id` is treated as a
    /// self-import and elided. Edges whose source node is no longer resolvable
    /// in the arena (tombstoned) are silently skipped.
    ///
    /// This is the file-level view of Pass 4 cross-file `Imports` edges, used
    /// by the incremental rebuild engine to compute reverse-dependency
    /// closures: "if file X changes its exports, which files need to be
    /// re-linked?"
    ///
    /// # Complexity
    ///
    /// `O(|nodes_in_file_id| × avg_incoming_edges_per_node)` amortized. Uses
    /// [`AuxiliaryIndices::by_file`] for O(1)-amortized per-file node lookup
    /// (HashMap-backed) and the bidirectional edge store's reverse adjacency;
    /// no full-graph scan. A final `O(R log R)` sort over the deduplicated
    /// importer set (where `R` is the importer count) is negligible in
    /// practice since `R ≤ file_count`.
    ///
    /// [`AuxiliaryIndices::by_file`]: crate::graph::unified::storage::indices::AuxiliaryIndices::by_file
    #[must_use]
    pub fn reverse_import_index(&self, file_id: FileId) -> Vec<FileId> {
        let mut importers: HashSet<FileId> = HashSet::new();
        for &target_node in self.indices.by_file(file_id) {
            for edge_ref in self.edges.edges_to(target_node) {
                if !matches!(edge_ref.kind, EdgeKind::Imports { .. }) {
                    continue;
                }
                let Some(source_entry) = self.nodes.get(edge_ref.source) else {
                    continue;
                };
                let source_file = source_entry.file;
                if source_file != file_id {
                    importers.insert(source_file);
                }
            }
        }
        let mut result: Vec<FileId> = importers.into_iter().collect();
        result.sort();
        result
    }

    /// Returns the set of files that hold at least one live inter-file edge
    /// targeting a node in `file_id`, deduplicated and sorted ascending.
    ///
    /// Unlike [`reverse_import_index`](Self::reverse_import_index) — which
    /// filters to [`EdgeKind::Imports`] only — this helper treats **every**
    /// cross-file edge as a dependency signal: `Calls`, `References`,
    /// `TypeOf`, `Inherits`, `Implements`, `FfiCall`, `HttpRequest`,
    /// `GrpcCall`, `WebAssemblyCall`, `DbQuery`, `TableRead`, `TableWrite`,
    /// `TriggeredBy`, `MessageQueue`, `WebSocket`, `GraphQLOperation`,
    /// `ProcessExec`, `FileIpc`, `ProtocolCall`, and any future
    /// cross-file-capable variant. This is the reverse-dependency surface the
    /// incremental rebuild engine (Task 4 Step 4 Phase 3e) needs: when
    /// `file_id` changes, every file whose committed edges point into
    /// `file_id`'s nodes must re-enter the rebuild closure so its cross-file
    /// references survive the target-side tombstone-and-reparse cycle.
    ///
    /// The caller's own file is never included — an edge whose source and
    /// target both live in `file_id` is a self-reference and is elided.
    /// Edges whose source node is no longer resolvable in the arena
    /// (tombstoned) are silently skipped.
    ///
    /// # When to use this vs [`reverse_import_index`](Self::reverse_import_index)
    ///
    /// * [`reverse_import_index`](Self::reverse_import_index) remains the
    ///   right surface for consumers that specifically need *import*
    ///   relationships (export surface analysis, module-dependency graphs,
    ///   etc.).
    /// * `reverse_dependency_index` is the right surface for incremental
    ///   rebuild closure computation. Widening past imports is necessary
    ///   because call sites, type references, trait implementations, FFI
    ///   declarations, HTTP clients, and every other cross-file edge kind
    ///   hold a committed edge into the target file that becomes stale the
    ///   moment `remove_file(target)` tombstones its arena nodes. Leaving
    ///   those files out of the closure leaves the committed edges pointing
    ///   at the stale (pre-tombstone) node IDs — Phase 4c-prime only
    ///   rewrites edges on **re-parsed** files' `PendingEdge` sets, never
    ///   committed edges owned by files outside the reparse scope.
    ///
    /// # Complexity
    ///
    /// `O(|nodes_in_file_id| × avg_incoming_edges_per_node)` amortized —
    /// same bound as [`reverse_import_index`](Self::reverse_import_index).
    /// Uses [`AuxiliaryIndices::by_file`] for O(1)-amortized per-file node
    /// lookup and the bidirectional edge store's reverse adjacency; no
    /// full-graph scan. Final `O(R log R)` sort over the deduplicated
    /// dependent set is negligible since `R ≤ file_count`.
    ///
    /// # Over-widening is expected and acceptable
    ///
    /// The closure will include every file that references anything in
    /// `file_id`, not just files whose exports change. In common codebases
    /// this widens the reparse set modestly (a 10-file change may expand
    /// to 20–30 dependent files in a medium crate). Correctness requires
    /// the widening; minimality is a follow-up optimisation if profiling
    /// demands it.
    ///
    /// [`AuxiliaryIndices::by_file`]: crate::graph::unified::storage::indices::AuxiliaryIndices::by_file
    #[must_use]
    pub fn reverse_dependency_index(&self, file_id: FileId) -> Vec<FileId> {
        let mut dependents: HashSet<FileId> = HashSet::new();
        for &target_node in self.indices.by_file(file_id) {
            for edge_ref in self.edges.edges_to(target_node) {
                let Some(source_entry) = self.nodes.get(edge_ref.source) else {
                    continue;
                };
                let source_file = source_entry.file;
                if source_file != file_id {
                    dependents.insert(source_file);
                }
            }
        }
        let mut result: Vec<FileId> = dependents.into_iter().collect();
        result.sort();
        result
    }

    /// Returns the per-language confidence metadata.
    ///
    /// This contains analysis confidence information collected during graph build,
    /// primarily used by language plugins (e.g., Rust) to track analysis quality.
    #[inline]
    #[must_use]
    pub fn confidence(&self) -> &HashMap<String, ConfidenceMetadata> {
        &self.confidence
    }

    /// Merges confidence metadata for a language.
    ///
    /// If confidence already exists for the language, this merges the new
    /// metadata (taking the lower confidence level and combining limitations).
    /// Otherwise, it inserts the new confidence.
    pub fn merge_confidence(&mut self, language: &str, metadata: ConfidenceMetadata) {
        use crate::confidence::ConfidenceLevel;

        self.confidence
            .entry(language.to_string())
            .and_modify(|existing| {
                // Take the lower confidence level (more conservative)
                let new_level = match (&existing.level, &metadata.level) {
                    (ConfidenceLevel::Verified, other) | (other, ConfidenceLevel::Verified) => {
                        *other
                    }
                    (ConfidenceLevel::Partial, ConfidenceLevel::AstOnly)
                    | (ConfidenceLevel::AstOnly, ConfidenceLevel::Partial) => {
                        ConfidenceLevel::AstOnly
                    }
                    (level, _) => *level,
                };
                existing.level = new_level;

                // Merge limitations (deduplicated)
                for limitation in &metadata.limitations {
                    if !existing.limitations.contains(limitation) {
                        existing.limitations.push(limitation.clone());
                    }
                }

                // Merge unavailable features (deduplicated)
                for feature in &metadata.unavailable_features {
                    if !existing.unavailable_features.contains(feature) {
                        existing.unavailable_features.push(feature.clone());
                    }
                }
            })
            .or_insert(metadata);
    }

    /// Sets the confidence metadata map directly.
    ///
    /// This is primarily used when loading a graph from serialized state.
    pub fn set_confidence(&mut self, confidence: HashMap<String, ConfidenceMetadata>) {
        self.confidence = confidence;
    }

    // ------------------------------------------------------------------
    // Task 4 Step 2 (A2 §F.2) — File-level tombstoning on a CodeGraph.
    //
    // Unlike the rebuild pipeline's `RebuildGraph::remove_file`, this
    // path mutates a live `CodeGraph` in place and is the mechanism
    // used by the full-rebuild flow when it needs to evict a file's
    // nodes+edges between compactions. The daemon's incremental
    // `WorkspaceManager` (Task 6) goes through the rebuild path, which
    // is why this entry point is `pub(crate)`.
    // ------------------------------------------------------------------

    /// Tombstone every node that belongs to `file_id`, invalidate every
    /// edge whose source or target is one of those nodes (across both
    /// forward and reverse CSR + delta tiers), drop the file's entry
    /// from the [`FileRegistry`], and return the set of [`NodeId`]s
    /// that were tombstoned.
    ///
    /// This is the §F.2-aware file-removal primitive. Semantically, the
    /// post-condition matches what a full rebuild of the workspace
    /// without `file_id` would produce:
    ///
    /// * Every [`NodeEntry`] whose [`NodeEntry.file`] was `file_id` has
    ///   been `NodeArena::remove`d, advancing its slot generation so
    ///   stale [`NodeId`] handles do not alias a later re-allocation.
    /// * Every CSR edge whose source or target slot matches one of the
    ///   tombstoned slot indices has its `csr_tombstones` bit set; the
    ///   read path's merge step already filters tombstoned CSR edges
    ///   out of every query result.
    /// * Every delta-buffer edge (Add or Remove, any file) whose source
    ///   or target matches a tombstoned slot has been dropped from the
    ///   delta in both directions.
    /// * The [`AuxiliaryIndices`] (kind / name / qualified-name / file)
    ///   no longer reference any of the tombstoned `NodeId`s.
    /// * [`NodeMetadataStore`], [`NodeProvenanceStore`], [`ScopeArena`],
    ///   [`AliasTable`], and [`ShadowTable`] have been compacted through
    ///   the [`NodeIdBearing::retain_nodes`] predicate so no
    ///   tombstoned `NodeId` survives in any publish-visible store.
    /// * [`FileRegistry::per_file_nodes`] no longer holds a bucket for
    ///   `file_id`, the lookup slot is recycled, and
    ///   [`FileRegistry::resolve(file_id)`] returns `None`.
    ///
    /// Returns the `Vec<NodeId>` of tombstoned nodes. The returned list
    /// is useful for downstream housekeeping (e.g., resetting per-file
    /// caches keyed by `NodeId`) and for tests that need to assert on the
    /// exact membership of the tombstone set.
    ///
    /// # Idempotency
    ///
    /// Calling `remove_file` twice with the same `file_id` — or calling
    /// it for a `file_id` that was never registered — is a safe no-op.
    /// The returned `Vec<NodeId>` is empty on the second call; no
    /// arena / edge / index state is mutated (the predicate-based
    /// compaction of `NodeIdBearing` surfaces short-circuits when the
    /// dead set is empty).
    ///
    /// # Visibility
    ///
    /// `pub(crate)` because external callers (Task 6's
    /// `WorkspaceManager` on the sqry-daemon side) route through
    /// [`super::super::rebuild::rebuild_graph::RebuildGraph::remove_file`]
    /// instead. This `CodeGraph`-level variant is used by full-rebuild
    /// housekeeping paths inside sqry-core and by the Task 4 Step 4
    /// incremental fallback for cases where the caller already has a
    /// `&mut CodeGraph` and does not need the clone-and-publish
    /// round-trip of [`clone_for_rebuild`](Self::clone_for_rebuild) →
    /// [`finalize`](super::super::rebuild::rebuild_graph::RebuildGraph::finalize).
    ///
    /// # Performance
    ///
    /// * `O(|tombstoned| + |csr_edges| + |delta_edges|)` amortised.
    ///   The CSR walk is linear in total edge count (not per-file),
    ///   which is the dominant cost; each row check is O(1) via the
    ///   precomputed dead-slot-index `HashSet` in
    ///   [`BidirectionalEdgeStore::tombstone_edges_for_nodes`].
    /// * Delta filtering is `O(|delta|)` per direction.
    /// * Auxiliary-index compaction is `O(|tombstoned|)` amortised
    ///   because each of the four indices keys its entries by the
    ///   tombstoned `NodeIds` directly.
    ///
    /// [`NodeEntry`]: crate::graph::unified::storage::arena::NodeEntry
    /// [`NodeEntry.file`]: crate::graph::unified::storage::arena::NodeEntry::file
    /// [`NodeIdBearing::retain_nodes`]: crate::graph::unified::rebuild::coverage::NodeIdBearing::retain_nodes
    #[allow(dead_code)] // Consumer is Task 4 Step 4 (`incremental_rebuild`)
    // and the unit tests below; published in this commit so the §F.2
    // invariant surface can be reviewed in isolation per the Gate 0c
    // split contract.
    pub(crate) fn remove_file(
        &mut self,
        file_id: FileId,
    ) -> Vec<crate::graph::unified::node::NodeId> {
        use crate::graph::unified::node::NodeId;
        use crate::graph::unified::rebuild::coverage::NodeIdBearing;

        // Drain the per-file bucket. For a file that was never
        // registered, this returns an empty Vec — the rest of the
        // method short-circuits on the `if tombstoned.is_empty()` test
        // below so we still deregister the file on the off chance the
        // bucket was empty but the file was registered (defensive; the
        // common case is the bucket existed iff the file was
        // registered).
        let tombstoned: Vec<NodeId> = self.files_mut().take_nodes(file_id);
        // Always drop the file's path entry + recycle its slot, even
        // when the bucket was empty, so repeated registrations of the
        // same path don't resurrect a zombie FileId. `unregister` is
        // idempotent (returns None for unknown IDs) so the cost is a
        // single HashMap probe when file_id is already gone.
        self.files_mut().unregister(file_id);
        // Clear the file's segment entry unconditionally (idempotent
        // — `FileSegmentTable::remove` no-ops on unknown ids). This
        // MUST run before `FileRegistry::unregister` recycles the
        // FileId slot for reuse, otherwise a later registration of a
        // different path under the reused FileId would inherit the
        // previous file's stale node range (see
        // `sqry-core/src/graph/unified/build/reindex.rs` — which
        // trusts `file_segments().get(file_id)` to decide which slots
        // to tombstone). Note: `unregister` above was called first
        // only to keep the existing bucket-drain ordering; the
        // segment-clear is order-independent with respect to
        // `unregister` because neither touches the other's backing
        // store, and the FileId slot cannot be recycled-and-reissued
        // across a single `remove_file` call (the registry's slot
        // recycler is driven by a later `register`, not by
        // `unregister`).
        self.file_segments_mut().remove(file_id);

        if tombstoned.is_empty() {
            return tombstoned;
        }

        // Dead set keyed on NodeId for NodeIdBearing predicates.
        // `retain_nodes` uses `HashSet::contains` so membership is O(1).
        let dead: HashSet<NodeId> = tombstoned.iter().copied().collect();

        // 1. Tombstone the arena slots. `NodeArena::remove` is
        //    idempotent — stale NodeIds that don't match a slot's
        //    current generation are no-ops, which lets this method be
        //    safely re-run on the same file.
        {
            let arena = self.nodes_mut();
            for &nid in &tombstoned {
                let _ = arena.remove(nid);
            }
        }

        // 2. Invalidate edges across both CSR + delta in both
        //    directions. This is the expensive step; the helper uses a
        //    precomputed dead-slot-index set so the CSR walk is linear
        //    in total edge count, not quadratic.
        self.edges_mut().tombstone_edges_for_nodes(&dead);

        // 3. Compact the auxiliary indices so name/kind/qname/file
        //    lookups do not return tombstoned NodeIds. Using the
        //    NodeIdBearing surface keeps this step in lockstep with the
        //    rebuild pipeline's step 4 — any future publish-visible
        //    NodeId-bearing container added to the K.A/K.B matrix is
        //    automatically swept here too.
        {
            let predicate: Box<dyn Fn(NodeId) -> bool + '_> = Box::new(|nid| !dead.contains(&nid));
            self.indices_mut().retain_nodes(&*predicate);
            self.macro_metadata_mut().retain_nodes(&*predicate);
            // The remaining K.A rows (node_provenance, scope_arena,
            // alias_table, shadow_table) need Arc::make_mut accessors —
            // they are wrapped in Arc at rest so this is where the
            // CoW clone happens on demand. Inline the Arc::make_mut
            // calls here (no public mut accessor exists today because
            // the sole writer has been the rebuild path; extend the
            // set by adding a similar inline call plus a K.A/K.B row
            // in `super::super::rebuild::coverage`).
            Arc::make_mut(&mut self.node_provenance).retain_nodes(&*predicate);
            Arc::make_mut(&mut self.scope_arena).retain_nodes(&*predicate);
            Arc::make_mut(&mut self.alias_table).retain_nodes(&*predicate);
            Arc::make_mut(&mut self.shadow_table).retain_nodes(&*predicate);
        }

        tombstoned
    }

    // ------------------------------------------------------------------
    // Gate 0c (A2 §H, Task 4) — RebuildGraph assembly path.
    // ------------------------------------------------------------------

    /// Assemble a [`CodeGraph`] from owned rebuild-local parts produced
    /// by `RebuildGraph::finalize()` (defined in
    /// `super::super::rebuild::rebuild_graph`).
    ///
    /// This constructor is deliberately `pub(crate)` and named with a
    /// leading `__` so it is inaccessible from downstream crates even
    /// when the `rebuild-internals` feature is enabled: only code in
    /// `sqry-core` itself (specifically `RebuildGraph::finalize`) is
    /// permitted to call it. The trybuild fixture
    /// `sqry-core/tests/rebuild_internals_compile_fail/rebuild_graph_no_public_assembly.rs`
    /// proves there is no other path from `RebuildGraph` to
    /// `Arc<CodeGraph>`.
    ///
    /// The argument order mirrors the `CodeGraph` struct declaration
    /// order exactly; the public `clone_for_rebuild` → `finalize`
    /// round-trip uses the same macro-driven field enumeration so any
    /// new `CodeGraph` field automatically threads through this
    /// constructor as well.
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub(crate) fn __assemble_from_rebuild_parts_internal(
        nodes: NodeArena,
        edges: BidirectionalEdgeStore,
        strings: StringInterner,
        files: FileRegistry,
        indices: AuxiliaryIndices,
        macro_metadata: NodeMetadataStore,
        node_provenance: NodeProvenanceStore,
        edge_provenance: EdgeProvenanceStore,
        fact_epoch: u64,
        epoch: u64,
        confidence: HashMap<String, ConfidenceMetadata>,
        scope_arena: ScopeArena,
        alias_table: AliasTable,
        shadow_table: ShadowTable,
        scope_provenance_store: ScopeProvenanceStore,
        file_segments: FileSegmentTable,
        go_hints: crate::graph::unified::build::staging::GoHints,
    ) -> Self {
        Self {
            nodes: Arc::new(nodes),
            edges: Arc::new(edges),
            strings: Arc::new(strings),
            files: Arc::new(files),
            indices: Arc::new(indices),
            macro_metadata: Arc::new(macro_metadata),
            node_provenance: Arc::new(node_provenance),
            edge_provenance: Arc::new(edge_provenance),
            fact_epoch,
            epoch,
            confidence,
            scope_arena: Arc::new(scope_arena),
            alias_table: Arc::new(alias_table),
            shadow_table: Arc::new(shadow_table),
            scope_provenance_store: Arc::new(scope_provenance_store),
            file_segments: Arc::new(file_segments),
            c_indirect_tables: None,
            go_hints,
            // A rebuild reparses source files, regenerating is_definition fresh,
            // so the reassembled graph always carries genuine signal.
            definition_signal_present: true,
        }
    }

    // ------------------------------------------------------------------
    // Gate 0c (A2 §F) — publish-boundary debug invariants.
    //
    // These checks fire only in debug / test builds; release builds
    // compile them out. They are called from
    // `RebuildGraph::finalize()` steps 13 and 14 (the single source of
    // truth for the residue check, per §F and §H agreement). Gate 0d
    // will additionally wire the bijection check into
    // `build_and_persist_graph`, `WorkspaceManager::publish_graph`, and
    // every §E equivalence-harness run.
    // ------------------------------------------------------------------

    /// Assert the bijective bucket-membership invariant (A2 §F.1).
    ///
    /// Four conditions must hold simultaneously:
    /// (a) every `NodeId` inside any `per_file_nodes` bucket maps to a
    ///     live arena slot;
    /// (b) every `NodeId` appears in exactly one bucket (no duplicates
    ///     across buckets, no duplicates within a bucket);
    /// (c) the bucket's `FileId` matches the node's own `file` field on
    ///     `NodeEntry`;
    /// (d) when at least one bucket is populated, every live node in
    ///     the arena is accounted for by some bucket.
    ///
    /// Condition (d) is guarded on `!seen.is_empty()` so an empty-graph
    /// (no recorded buckets) publish boundary is vacuously consistent:
    /// legacy V7 snapshots, fresh `CodeGraph::new()` instances, and
    /// rebuilds on graphs that predate Gate 0c's parallel-commit
    /// bucketing must not panic. Once any bucket is populated, every
    /// live arena slot must appear in a bucket.
    ///
    /// Iter-2 B2 (verbatim): this check used to be documented as
    /// "vacuous until future `per_file_nodes` work lands". Pulling
    /// base-plan Step 1 into Gate 0c retires that phrasing — the check
    /// is non-vacuous the moment parallel-parse commits nodes, which
    /// happens on every real build. The `!seen.is_empty()` guard now
    /// exists solely for the empty-graph corner case, not as a phased-
    /// delivery deferral.
    ///
    /// The check is a no-op in release builds. Panics with a
    /// descriptive message on violation. This is intentional: publish-
    /// boundary violations are programmer errors that must surface
    /// loudly during CI / test runs.
    ///
    /// # Panics
    ///
    /// Panics in debug/test builds when bucket membership is duplicated, points
    /// at a dead node, assigns a node to the wrong file, or omits a live node
    /// after buckets have been populated.
    #[cfg(any(debug_assertions, test))]
    pub fn assert_bucket_bijection(&self) {
        use std::collections::HashMap as StdHashMap;
        // (a) + (b) + (c): every bucketed node is live, unique across
        // buckets, and the bucket's FileId matches the node's own file.
        let mut seen: StdHashMap<
            crate::graph::unified::node::NodeId,
            crate::graph::unified::file::FileId,
        > = StdHashMap::new();
        let mut any_bucket_populated = false;
        for (file_id, bucket) in self.files.per_file_nodes_for_gate0d() {
            if !bucket.is_empty() {
                any_bucket_populated = true;
            }
            // Local dedup guard within the bucket itself. `retain_nodes_in_buckets`
            // dedups during finalize step 6, but we re-check here so the
            // invariant covers non-finalize publish paths too (e.g. if a
            // future pipeline builds a graph without routing through
            // `RebuildGraph::finalize`).
            let mut within_bucket: std::collections::HashSet<crate::graph::unified::node::NodeId> =
                std::collections::HashSet::new();
            for node_id in bucket {
                assert!(
                    within_bucket.insert(node_id),
                    "assert_bucket_bijection: duplicate node {node_id:?} inside bucket {file_id:?}"
                );
                assert!(
                    self.nodes.get(node_id).is_some(),
                    "assert_bucket_bijection: dead node {node_id:?} in bucket {file_id:?}"
                );
                let prior = seen.insert(node_id, file_id);
                assert!(
                    prior.is_none(),
                    "assert_bucket_bijection: node {node_id:?} in multiple buckets: \
                     prior={prior:?}, current={file_id:?}"
                );
                if let Some(entry) = self.nodes.get(node_id) {
                    assert_eq!(
                        entry.file, file_id,
                        "assert_bucket_bijection: node {node_id:?} misfiled: in bucket \
                         {file_id:?}, actual {:?}",
                        entry.file
                    );
                }
            }
        }
        // (d): every live node is accounted for by `seen` once buckets
        // are populated. The guard keeps legacy-empty-graph boundaries
        // vacuously consistent (see docs above).
        if any_bucket_populated {
            for (node_id, _entry) in self.nodes.iter() {
                assert!(
                    seen.contains_key(&node_id),
                    "assert_bucket_bijection: live node {node_id:?} absent from all buckets"
                );
            }
        }
    }

    /// Assert the pre-reuse tombstone-residue invariant (A2 §F.2).
    ///
    /// Iterates every publish-visible NodeId-bearing structure on
    /// `self` and panics if any contains a node in `dead`. Called from
    /// `RebuildGraph::finalize()` step 14 against the set drained at
    /// step 8 — exactly one site per the plan's §F / §H agreement.
    ///
    /// No-op when `dead` is empty or in release builds.
    ///
    /// # Panics
    ///
    /// Panics in debug/test builds if any publish-visible node-bearing
    /// structure still references a tombstoned node.
    #[cfg(any(debug_assertions, test))]
    pub fn assert_no_tombstone_residue_for<S: std::hash::BuildHasher>(
        &self,
        dead: &std::collections::HashSet<crate::graph::unified::node::NodeId, S>,
    ) {
        use super::super::rebuild::coverage::NodeIdBearing;
        if dead.is_empty() {
            return;
        }
        // Every K.A/K.B row must be inspected per §F.2.
        for nid in self.nodes.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "assert_no_tombstone_residue: tombstone {nid:?} still in NodeArena"
            );
        }
        for nid in self.indices.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "assert_no_tombstone_residue: tombstone {nid:?} still in auxiliary indices"
            );
        }
        for nid in self.edges.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "assert_no_tombstone_residue: tombstone {nid:?} still in edge store"
            );
        }
        for nid in self.macro_metadata.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "assert_no_tombstone_residue: tombstone {nid:?} still in macro metadata"
            );
        }
        for nid in self.node_provenance.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "assert_no_tombstone_residue: tombstone {nid:?} still in node provenance"
            );
        }
        for nid in self.scope_arena.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "assert_no_tombstone_residue: tombstone {nid:?} still in scope arena"
            );
        }
        for nid in self.alias_table.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "assert_no_tombstone_residue: tombstone {nid:?} still in alias table"
            );
        }
        for nid in self.shadow_table.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "assert_no_tombstone_residue: tombstone {nid:?} still in shadow table"
            );
        }
        for nid in self.files.all_node_ids() {
            assert!(
                !dead.contains(&nid),
                "assert_no_tombstone_residue: tombstone {nid:?} still in per-file bucket"
            );
        }
    }
}

impl Default for CodeGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CodeGraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodeGraph")
            .field("nodes", &self.nodes.len())
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

impl GraphMemorySize for CodeGraph {
    /// Estimates the total heap bytes owned by this `CodeGraph`.
    ///
    /// Sums heap usage across every component the graph owns: node arena,
    /// bidirectional edge store (forward + reverse CSR + tombstones + delta
    /// buffer), string interner, file registry, auxiliary indices, sparse
    /// macro/classpath metadata, provenance stores, and the per-language
    /// confidence map. Used by the `sqryd` daemon's admission controller
    /// and workspace retention reaper to enforce memory budgets.
    fn heap_bytes(&self) -> usize {
        let mut confidence_bytes = self.confidence.capacity()
            * (std::mem::size_of::<String>()
                + std::mem::size_of::<ConfidenceMetadata>()
                + HASHMAP_ENTRY_OVERHEAD);
        for (key, meta) in &self.confidence {
            confidence_bytes += key.capacity();
            // `ConfidenceMetadata.limitations` / `unavailable_features` are
            // `Vec<String>`; charge their spill and inner String payloads.
            confidence_bytes += meta.limitations.capacity() * std::mem::size_of::<String>();
            for s in &meta.limitations {
                confidence_bytes += s.capacity();
            }
            confidence_bytes +=
                meta.unavailable_features.capacity() * std::mem::size_of::<String>();
            for s in &meta.unavailable_features {
                confidence_bytes += s.capacity();
            }
        }

        self.nodes.heap_bytes()
            + self.edges.heap_bytes()
            + self.strings.heap_bytes()
            + self.files.heap_bytes()
            + self.indices.heap_bytes()
            + self.macro_metadata.heap_bytes()
            + self.node_provenance.heap_bytes()
            + self.edge_provenance.heap_bytes()
            + confidence_bytes
            + self.file_segments.capacity()
                * std::mem::size_of::<Option<crate::graph::unified::storage::segment::FileSegment>>(
                )
    }
}

/// Thread-safe wrapper for `CodeGraph` with epoch versioning.
///
/// `ConcurrentCodeGraph` provides MVCC-style concurrency:
/// - Multiple readers can access the graph simultaneously
/// - Only one writer can hold the lock at a time
/// - Each write operation increments the epoch for cursor invalidation
///
/// # Design
///
/// The wrapper uses `parking_lot::RwLock` for efficient locking:
/// - Fair scheduling prevents writer starvation
/// - No poisoning (unlike `std::sync::RwLock`)
/// - Faster lock/unlock operations
///
/// # Phase 2 binding-plane access
///
/// Use the three-line snapshot pattern to access `BindingPlane`:
///
/// ```rust,ignore
/// let read_guard = concurrent.read();
/// let snapshot = read_guard.snapshot();
/// let plane = snapshot.binding_plane();
/// let resolution = plane.resolve(&query);
/// ```
///
/// The explicit snapshot handle makes the MVCC lifetime visible at the call
/// site. The full Phase 2 scope/alias/shadow and witness-bearing resolution
/// API is exposed through `BindingPlane`.
///
/// # Usage
///
/// ```rust
/// use sqry_core::graph::unified::concurrent::ConcurrentCodeGraph;
///
/// let graph = ConcurrentCodeGraph::new();
///
/// // Read access (multiple readers allowed)
/// {
///     let guard = graph.read();
///     let _nodes = guard.nodes();
/// }
///
/// // Write access (exclusive)
/// {
///     let mut guard = graph.write();
///     let _nodes = guard.nodes_mut();
/// }
///
/// // Snapshot for long queries
/// let snapshot = graph.snapshot();
/// ```
pub struct ConcurrentCodeGraph {
    /// The underlying code graph protected by a read-write lock.
    inner: RwLock<CodeGraph>,
    /// Global epoch counter for cursor validation.
    epoch: AtomicU64,
}

impl ConcurrentCodeGraph {
    /// Creates a new empty `ConcurrentCodeGraph`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(CodeGraph::new()),
            epoch: AtomicU64::new(0),
        }
    }

    /// Creates a `ConcurrentCodeGraph` from an existing `CodeGraph`.
    #[must_use]
    pub fn from_graph(graph: CodeGraph) -> Self {
        let epoch = graph.epoch();
        Self {
            inner: RwLock::new(graph),
            epoch: AtomicU64::new(epoch),
        }
    }

    /// Acquires a read lock on the graph.
    ///
    /// Multiple readers can hold the lock simultaneously.
    /// This does not increment the epoch.
    #[inline]
    pub fn read(&self) -> RwLockReadGuard<'_, CodeGraph> {
        self.inner.read()
    }

    /// Acquires a write lock on the graph.
    ///
    /// Only one writer can hold the lock at a time.
    /// This increments the global epoch counter.
    #[inline]
    pub fn write(&self) -> RwLockWriteGuard<'_, CodeGraph> {
        // Increment the global epoch
        self.epoch.fetch_add(1, Ordering::SeqCst);
        let mut guard = self.inner.write();
        // Sync the inner graph's epoch with the global epoch
        guard.set_epoch(self.epoch.load(Ordering::SeqCst));
        guard
    }

    /// Returns the current global epoch.
    ///
    /// This can be used to detect if the graph has been modified
    /// since a previous operation (cursor invalidation).
    #[inline]
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Creates a cheap snapshot of the graph.
    ///
    /// This acquires a brief read lock to clone the Arc references.
    /// The snapshot is isolated from future mutations.
    #[must_use]
    pub fn snapshot(&self) -> GraphSnapshot {
        self.inner.read().snapshot()
    }

    // ------------------------------------------------------------------
    // Phase 1 fact-layer provenance convenience accessors.
    // These acquire a brief read lock, matching the snapshot() pattern.
    // ------------------------------------------------------------------

    /// Returns the monotonic fact-layer epoch from the underlying graph.
    #[must_use]
    pub fn fact_epoch(&self) -> u64 {
        self.inner.read().fact_epoch()
    }

    /// Looks up node provenance by `NodeId` (acquires a brief read lock).
    #[must_use]
    pub fn node_provenance(
        &self,
        id: crate::graph::unified::node::id::NodeId,
    ) -> Option<NodeProvenance> {
        self.inner.read().node_provenance(id).copied()
    }

    /// Looks up edge provenance by `EdgeId` (acquires a brief read lock).
    #[must_use]
    pub fn edge_provenance(
        &self,
        id: crate::graph::unified::edge::id::EdgeId,
    ) -> Option<EdgeProvenance> {
        self.inner.read().edge_provenance(id).copied()
    }

    /// Returns a file provenance view (acquires a brief read lock).
    ///
    /// Returns an owned copy since the borrow cannot outlive the lock guard.
    #[must_use]
    pub fn file_provenance(
        &self,
        id: crate::graph::unified::file::id::FileId,
    ) -> Option<OwnedFileProvenanceView> {
        let guard = self.inner.read();
        guard.file_provenance(id).map(|v| OwnedFileProvenanceView {
            content_hash: *v.content_hash,
            indexed_at: v.indexed_at,
            source_uri: v.source_uri,
            is_external: v.is_external,
        })
    }

    // ------------------------------------------------------------------
    // Phase 2 binding-plane accessors (P2U03).
    // ------------------------------------------------------------------

    /// Returns the scope arena from the underlying graph (acquires a brief
    /// read lock).
    ///
    /// Returns an `Arc` clone so the caller does not hold the lock beyond
    /// this call site.
    #[must_use]
    pub fn scope_arena(&self) -> Arc<ScopeArena> {
        Arc::clone(&self.inner.read().scope_arena)
    }

    /// Returns the alias table from the underlying graph (acquires a brief
    /// read lock).
    ///
    /// Returns an `Arc` clone so the caller does not hold the lock beyond
    /// this call site.
    #[must_use]
    pub fn alias_table(&self) -> Arc<AliasTable> {
        Arc::clone(&self.inner.read().alias_table)
    }

    /// Returns the shadow table from the underlying graph (acquires a brief
    /// read lock).
    ///
    /// Returns an `Arc` clone so the caller does not hold the lock beyond
    /// this call site.
    #[must_use]
    pub fn shadow_table(&self) -> Arc<ShadowTable> {
        Arc::clone(&self.inner.read().shadow_table)
    }

    /// Returns the scope provenance store from the underlying graph (acquires
    /// a brief read lock).
    ///
    /// Returns an `Arc` clone so the caller does not hold the lock beyond
    /// this call site.
    #[must_use]
    pub fn scope_provenance_store(&self) -> Arc<ScopeProvenanceStore> {
        Arc::clone(&self.inner.read().scope_provenance_store)
    }

    /// Looks up scope provenance by `ScopeId` (acquires a brief read lock).
    ///
    /// Returns an owned copy since the borrow cannot outlive the lock guard.
    #[must_use]
    pub fn scope_provenance(&self, id: ScopeId) -> Option<ScopeProvenance> {
        self.inner.read().scope_provenance(id).cloned()
    }

    /// Looks up the live `ScopeId` for a stable scope identity (acquires a
    /// brief read lock).
    ///
    /// Returns `None` if no provenance record is registered for that stable id.
    #[must_use]
    pub fn scope_by_stable_id(&self, stable: ScopeStableId) -> Option<ScopeId> {
        self.inner.read().scope_by_stable_id(stable)
    }

    /// Returns the file segment table from the underlying graph (acquires a
    /// brief read lock).
    #[must_use]
    pub fn file_segments(&self) -> Arc<FileSegmentTable> {
        Arc::clone(&self.inner.read().file_segments)
    }

    /// Attempts to acquire a read lock without blocking.
    ///
    /// Returns `None` if the lock is currently held exclusively.
    #[inline]
    #[must_use]
    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, CodeGraph>> {
        self.inner.try_read()
    }

    /// Attempts to acquire a write lock without blocking.
    ///
    /// Returns `None` if the lock is currently held by another thread.
    /// If successful, increments the epoch.
    #[inline]
    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, CodeGraph>> {
        self.inner.try_write().map(|mut guard| {
            self.epoch.fetch_add(1, Ordering::SeqCst);
            guard.set_epoch(self.epoch.load(Ordering::SeqCst));
            guard
        })
    }
}

impl Default for ConcurrentCodeGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ConcurrentCodeGraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConcurrentCodeGraph")
            .field("epoch", &self.epoch.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

/// Owned copy of file provenance, returned by [`ConcurrentCodeGraph::file_provenance`]
/// because the borrowed [`FileProvenanceView`] cannot outlive the read-lock guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnedFileProvenanceView {
    /// SHA-256 of the on-disk file bytes (owned copy).
    pub content_hash: [u8; 32],
    /// Unix-epoch seconds at which this file was registered.
    pub indexed_at: u64,
    /// Optional interned physical origin URI.
    pub source_uri: Option<StringId>,
    /// Whether this file originates from an external source.
    pub is_external: bool,
}

/// Immutable snapshot of a `CodeGraph` for long-running queries.
///
/// `GraphSnapshot` holds Arc references to the graph components,
/// providing a consistent view that is isolated from concurrent mutations.
///
/// # Design
///
/// Snapshots are created via `CodeGraph::snapshot()` or
/// `ConcurrentCodeGraph::snapshot()`. They are:
///
/// - **Immutable**: No mutation methods available
/// - **Isolated**: Independent of future graph mutations
/// - **Cheap**: Only Arc clones, no data copying
/// - **Self-contained**: Can outlive the original graph/lock
///
/// # Usage
///
/// ```rust
/// use sqry_core::graph::unified::concurrent::{ConcurrentCodeGraph, GraphSnapshot};
///
/// let graph = ConcurrentCodeGraph::new();
///
/// // Create snapshot for a long query
/// let snapshot: GraphSnapshot = graph.snapshot();
///
/// // Snapshot can be used independently
/// let _epoch = snapshot.epoch();
/// ```
#[derive(Clone)]
pub struct GraphSnapshot {
    /// Node storage snapshot.
    nodes: Arc<NodeArena>,
    /// Edge storage snapshot.
    edges: Arc<BidirectionalEdgeStore>,
    /// String interner snapshot.
    strings: Arc<StringInterner>,
    /// File registry snapshot.
    files: Arc<FileRegistry>,
    /// Auxiliary indices snapshot.
    indices: Arc<AuxiliaryIndices>,
    /// Sparse macro boundary metadata snapshot.
    macro_metadata: Arc<NodeMetadataStore>,
    /// Dense node provenance snapshot (Phase 1).
    node_provenance: Arc<NodeProvenanceStore>,
    /// Dense edge provenance snapshot (Phase 1).
    edge_provenance: Arc<EdgeProvenanceStore>,
    /// Monotonic fact-layer epoch at snapshot time.
    fact_epoch: u64,
    /// Epoch at snapshot time (for cursor validation).
    epoch: u64,
    /// Phase 2 binding-plane scope arena snapshot (populated by Phase 4e).
    scope_arena: Arc<ScopeArena>,
    /// Phase 2 binding-plane alias table snapshot (populated by Phase 4e / P2U04).
    alias_table: Arc<AliasTable>,
    /// Phase 2 binding-plane shadow table snapshot (populated by Phase 4e / P2U05).
    shadow_table: Arc<ShadowTable>,
    /// Phase 2 binding-plane scope provenance store snapshot (populated by Phase 4e / P2U11).
    scope_provenance_store: Arc<ScopeProvenanceStore>,
    /// Phase 3 file segment table snapshot mapping `FileId` to node ranges.
    file_segments: Arc<FileSegmentTable>,
    /// Phase A (U09): C indirect-call resolver side tables snapshot.
    ///
    /// `None` on non-C workspaces. The snapshot owns its own clone of the
    /// `Option<CIndirectSideTables>` so concurrent readers see a stable
    /// view independent of subsequent mutations to the source `CodeGraph`.
    c_indirect_tables: Option<CIndirectSideTables>,
    /// Whether this snapshot carries genuine `NodeEntry.is_definition` signal
    /// (R3 marker). Copied from the source [`CodeGraph`] at snapshot time. See
    /// [`CodeGraph::definition_signal_present`] for the contract.
    definition_signal_present: bool,
}

impl GraphSnapshot {
    /// Returns whether this snapshot carries genuine `is_definition` signal.
    ///
    /// See [`CodeGraph::definition_signal_present`] for the full contract.
    #[inline]
    #[must_use]
    pub fn definition_signal_present(&self) -> bool {
        self.definition_signal_present
    }

    /// Returns a reference to the node arena.
    #[inline]
    #[must_use]
    pub fn nodes(&self) -> &NodeArena {
        &self.nodes
    }

    /// Returns a reference to the bidirectional edge store.
    #[inline]
    #[must_use]
    pub fn edges(&self) -> &BidirectionalEdgeStore {
        &self.edges
    }

    /// Returns a reference to the string interner.
    #[inline]
    #[must_use]
    pub fn strings(&self) -> &StringInterner {
        &self.strings
    }

    /// Returns a reference to the file registry.
    #[inline]
    #[must_use]
    pub fn files(&self) -> &FileRegistry {
        &self.files
    }

    /// Returns a reference to the auxiliary indices.
    #[inline]
    #[must_use]
    pub fn indices(&self) -> &AuxiliaryIndices {
        &self.indices
    }

    /// Returns a reference to the macro boundary metadata store.
    #[inline]
    #[must_use]
    pub fn macro_metadata(&self) -> &NodeMetadataStore {
        &self.macro_metadata
    }

    /// Returns a reference to the C indirect-call side tables, if any.
    ///
    /// Mirrors [`CodeGraph::c_indirect_tables`]; see that method for
    /// the contract. Snapshot-level access is read-only.
    #[inline]
    #[must_use]
    pub fn c_indirect_tables(&self) -> Option<&CIndirectSideTables> {
        self.c_indirect_tables.as_ref()
    }

    // ------------------------------------------------------------------
    // Phase 1 fact-layer provenance accessors (P1U09).
    // ------------------------------------------------------------------

    /// Returns the monotonic fact-layer epoch.
    #[inline]
    #[must_use]
    pub fn fact_epoch(&self) -> u64 {
        self.fact_epoch
    }

    /// Looks up node provenance by `NodeId`.
    #[inline]
    #[must_use]
    pub fn node_provenance(
        &self,
        id: crate::graph::unified::node::id::NodeId,
    ) -> Option<&NodeProvenance> {
        self.node_provenance.lookup(id)
    }

    /// Looks up edge provenance by `EdgeId`.
    #[inline]
    #[must_use]
    pub fn edge_provenance(
        &self,
        id: crate::graph::unified::edge::id::EdgeId,
    ) -> Option<&EdgeProvenance> {
        self.edge_provenance.lookup(id)
    }

    /// Returns a borrowed provenance view for a file.
    #[inline]
    #[must_use]
    pub fn file_provenance(
        &self,
        id: crate::graph::unified::file::id::FileId,
    ) -> Option<FileProvenanceView<'_>> {
        self.files.file_provenance(id)
    }

    // ------------------------------------------------------------------
    // Phase 2 binding-plane accessors (P2U03).
    // ------------------------------------------------------------------

    /// Returns a reference to the scope arena at snapshot time.
    ///
    /// Participates in MVCC: the snapshot holds an `Arc` clone of the arena
    /// as it existed when `snapshot()` was called. Subsequent calls to
    /// `set_scope_arena` on the source `CodeGraph` do not affect this view.
    #[inline]
    #[must_use]
    pub fn scope_arena(&self) -> &ScopeArena {
        &self.scope_arena
    }

    /// Returns a reference to the alias table at snapshot time.
    ///
    /// Participates in MVCC: the snapshot holds an `Arc` clone of the table
    /// as it existed when `snapshot()` was called. Subsequent calls to
    /// `set_alias_table` on the source `CodeGraph` do not affect this view.
    #[inline]
    #[must_use]
    pub fn alias_table(&self) -> &AliasTable {
        &self.alias_table
    }

    /// Returns a reference to the shadow table at snapshot time.
    ///
    /// Participates in MVCC: the snapshot holds an `Arc` clone of the table
    /// as it existed when `snapshot()` was called. Subsequent calls to
    /// `set_shadow_table` on the source `CodeGraph` do not affect this view.
    #[inline]
    #[must_use]
    pub fn shadow_table(&self) -> &ShadowTable {
        &self.shadow_table
    }

    /// Returns a reference to the scope provenance store at snapshot time.
    ///
    /// Participates in MVCC: the snapshot holds an `Arc` clone of the store
    /// as it existed when `snapshot()` was called. Subsequent calls to
    /// `set_scope_provenance_store` on the source `CodeGraph` do not affect
    /// this view.
    #[inline]
    #[must_use]
    pub fn scope_provenance_store(&self) -> &ScopeProvenanceStore {
        &self.scope_provenance_store
    }

    /// Looks up scope provenance by `ScopeId` at snapshot time.
    ///
    /// Returns `None` if the slot is out of range, vacant, or the stored
    /// generation does not match (stale handle).
    #[inline]
    #[must_use]
    pub fn scope_provenance(&self, id: ScopeId) -> Option<&ScopeProvenance> {
        self.scope_provenance_store.lookup(id)
    }

    /// Looks up the live `ScopeId` for a stable scope identity at snapshot time.
    ///
    /// Returns `None` if no provenance record is registered for that stable id.
    #[inline]
    #[must_use]
    pub fn scope_by_stable_id(&self, stable: ScopeStableId) -> Option<ScopeId> {
        self.scope_provenance_store.scope_by_stable_id(stable)
    }

    /// Returns a reference to the file segment table at snapshot time.
    #[inline]
    #[must_use]
    pub fn file_segments(&self) -> &FileSegmentTable {
        &self.file_segments
    }

    /// Returns the epoch at which this snapshot was taken.
    ///
    /// This can be compared against the current graph epoch to
    /// detect if the graph has changed since the snapshot.
    #[inline]
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns `true` if this snapshot's epoch matches the given epoch.
    ///
    /// Use this to validate cursors before continuing pagination.
    #[inline]
    #[must_use]
    pub fn epoch_matches(&self, other_epoch: u64) -> bool {
        self.epoch == other_epoch
    }

    // ------------------------------------------------------------------
    // Phase 2 binding-plane facade accessor (P2U07).
    // ------------------------------------------------------------------

    /// Returns a [`BindingPlane`] facade borrowing this snapshot's lifetime.
    ///
    /// The facade is the stable Phase 2 public API for scope/alias/shadow
    /// queries and witness-bearing resolution. It provides a single entry
    /// point (`resolve`) that returns both a `BindingResult` and an ordered
    /// step trace in a `BindingResolution`.
    ///
    /// # MVCC note
    ///
    /// `BindingPlane<'_>` borrows from this snapshot, which is already an
    /// MVCC-consistent view of the graph at snapshot time. Callers from
    /// `CodeGraph` or `ConcurrentCodeGraph` should follow the two-line
    /// pattern so the snapshot lifetime is explicit:
    ///
    /// ```rust,ignore
    /// // CodeGraph caller:
    /// let snapshot = graph.snapshot();
    /// let plane = snapshot.binding_plane();
    ///
    /// // ConcurrentCodeGraph caller:
    /// let read_guard = concurrent.read();
    /// let snapshot = read_guard.snapshot();
    /// let plane = snapshot.binding_plane();
    /// ```
    #[inline]
    #[must_use]
    pub fn binding_plane(&self) -> crate::graph::unified::bind::plane::BindingPlane<'_> {
        crate::graph::unified::bind::plane::BindingPlane::new(self)
    }

    // ============================================================================
    // Query Methods
    // ============================================================================

    /// Finds nodes matching a pattern.
    ///
    /// Performs a simple substring match on node names and qualified names.
    /// Returns all matching node IDs.
    ///
    /// **Synthetic suppression (`C_SUPPRESS`):** synthetic placeholder
    /// nodes — internal scaffolding the language plugins emit for
    /// binding-plane and scope analysis (e.g. the Go plugin's
    /// `<field:operand.field>` field-access shadows and the
    /// `<ident>@<offset>` per-binding-site Variable nodes from the
    /// local-scope resolver) — are filtered out by default. Internal
    /// callers that need to reach these nodes (binding plane, scope /
    /// alias / shadow analysis) use
    /// [`Self::find_by_pattern_with_options`] with
    /// `include_synthetic = true`.
    ///
    /// # Performance
    ///
    /// Optimized to iterate over unique strings in the interner (smaller set)
    /// rather than all nodes in the arena.
    ///
    /// # Arguments
    ///
    /// * `pattern` - The pattern to match (substring search)
    ///
    /// # Returns
    ///
    /// A vector of `NodeIds` for all matching nodes (synthetic
    /// placeholders excluded).
    #[must_use]
    pub fn find_by_pattern(&self, pattern: &str) -> Vec<crate::graph::unified::node::NodeId> {
        self.find_by_pattern_with_options(pattern, false)
    }

    /// Finds nodes matching a pattern with explicit control over synthetic
    /// placeholder visibility.
    ///
    /// `include_synthetic = false` is the default surface used by every
    /// user-facing caller (CLI `search`, MCP `semantic_search` /
    /// `pattern_search` / `relation_query`, etc.). Synthetic
    /// placeholders are suppressed via two parallel checks that must
    /// agree:
    ///
    /// 1. The authoritative `NodeFlags::SYNTHETIC` bit on the
    ///    metadata store
    ///    ([`crate::graph::unified::storage::metadata::NodeMetadataStore::is_synthetic`]).
    /// 2. The structural name-shape fallback
    ///    ([`crate::graph::unified::storage::arena::NodeEntry::is_synthetic_placeholder_name`])
    ///    for V10 snapshots written before the synthetic bit existed
    ///    and for cross-file unification losers that retained their
    ///    name but lost their metadata entry.
    ///
    /// Either check matching is sufficient to suppress the node. The
    /// design lives in `docs/development/public-issue-triage/`
    /// under the `C_SUPPRESS` unit; see also the rationale in
    /// [`crate::graph::unified::storage::metadata::NodeFlags::SYNTHETIC`].
    ///
    /// `include_synthetic = true` is **internal-only**. The binding
    /// plane, scope resolver, and rebuild's coverage gate use this
    /// path to reach synthetic nodes for their structural integrity
    /// checks. **No CLI / MCP surface should ever pass `true`.**
    #[must_use]
    pub fn find_by_pattern_with_options(
        &self,
        pattern: &str,
        include_synthetic: bool,
    ) -> Vec<crate::graph::unified::node::NodeId> {
        let mut matches = Vec::new();

        // 1. Scan unique strings in interner for matches
        for (str_id, s) in self.strings.iter() {
            if s.contains(pattern) {
                // 2. If string matches, look up all nodes with this name
                // Check qualified name index
                matches.extend_from_slice(self.indices.by_qualified_name(str_id));
                // Check simple name index
                matches.extend_from_slice(self.indices.by_name(str_id));
            }
        }

        // Deduplicate matches (a node might match both qualified and simple name)
        matches.sort_unstable();
        matches.dedup();

        if !include_synthetic {
            matches.retain(|&node_id| !self.is_node_synthetic(node_id));
        }

        matches
    }

    /// Finds nodes whose interned simple **or** qualified name equals `name`,
    /// accepting dot- and Ruby-`#` qualified display form as a fallback for
    /// graph-canonical `::` qualified names.
    ///
    /// This is the canonical surface for **exact-name** lookups —
    /// shared by the CLI `--exact <pattern>` shorthand
    /// (`sqry-cli/src/commands/search.rs::run_regular_search`) and the
    /// structural query planner's `name:` predicate
    /// (`sqry-db/src/planner/parse.rs`,
    /// `sqry-db/src/planner/execute.rs`). Both surfaces are
    /// contract-bound (DAG `B1_ALIGN`) to return the same set against
    /// any fixture: the CLI calls this method directly, while the
    /// planner uses the same interner + by-name index pair internally
    /// when scanning, then applies the same synthetic filter.
    ///
    /// **Synthetic suppression.** Synthetic placeholder nodes
    /// (Go-plugin `<field:operand.field>` shadows and
    /// `<ident>@<offset>` per-binding-site Variables; see
    /// [`Self::find_by_pattern_with_options`] for the full taxonomy)
    /// are excluded via [`Self::is_node_synthetic`]. There is **no**
    /// `include_synthetic = true` variant for the exact-match surface
    /// because the synthetic name shapes the structural fallback
    /// recognises (`<…>`, `…@<offset>`) cannot equal a user-typed
    /// name byte-for-byte; the metadata-bit channel is the only
    /// realistic leak vector and it is suppressed unconditionally.
    ///
    /// **Display fallback.** Exact lookup checks the literal input. When the
    /// input is qualified, language-aware display candidates win over raw
    /// canonical matches. This keeps native Rust `identity::T` from also
    /// returning a TypeScript type parameter whose internal canonical form is
    /// `identity::T` but whose display form is `identity.T`. If neither
    /// display nor literal lookup finds candidates, dot- and Ruby-`#`
    /// qualified inputs also check the graph-canonical `::` rewrite.
    ///
    /// # Performance
    ///
    /// `O(1)` interner lookup + `O(matches)` filter. If `name` is not
    /// interned the resolver still tries a native-display to graph-canonical
    /// `::` fallback for user-facing qualified names before returning empty.
    ///
    /// # Arguments
    ///
    /// * `name` - The exact name to look up (no glob, no regex).
    ///
    /// # Returns
    ///
    /// Sorted, deduplicated `NodeId`s for every non-synthetic node
    /// whose `entry.name`, `entry.qualified_name`, or language-aware display
    /// name equals `name`, or matches for the graph-canonical `::` rewrite
    /// when the user supplied a dot- or Ruby-`#` qualified name that had no
    /// literal or display candidates.
    #[must_use]
    pub fn find_by_exact_name(&self, name: &str) -> Vec<crate::graph::unified::node::NodeId> {
        let is_qualified = name.contains('.') || name.contains('#') || name.contains("::");
        if !is_qualified {
            // Bare name lookup: the interned `by_name` index covers
            // every language and is O(1); no display scan is needed.
            return self.find_by_exact_interned_name(name);
        }
        // Qualified name: each plugin stores its native form
        // (PHP `Ledger.promotedField`, Rust `Ledger::promotedField`,
        // Ruby `Ledger#promotedField`). Scan computed display names so
        // a single dotted user query resolves to all language matches,
        // then fall back to the interned form (and the graph-canonical
        // `::` rewrite) when display yields nothing.
        let mut exact_matches = self.find_by_exact_display_name(name);
        if exact_matches.is_empty() {
            exact_matches = self.find_by_exact_interned_name(name);
            if exact_matches.is_empty() && !name.contains("::") {
                let canonical = name.replace(['.', '#'], "::");
                exact_matches.extend(self.find_by_exact_interned_name(&canonical));
                exact_matches.sort_unstable();
                exact_matches.dedup();
            }
        }
        exact_matches
    }

    fn find_by_exact_display_name(&self, name: &str) -> Vec<crate::graph::unified::node::NodeId> {
        let mut matches = self
            .iter_nodes()
            .filter(|(node_id, _)| !self.is_node_synthetic(*node_id))
            .filter_map(|(node_id, entry)| {
                let qualified = entry
                    .qualified_name
                    .and_then(|sid| self.strings.resolve(sid))?;
                let display = self.files.language_for_file(entry.file).map_or_else(
                    || qualified.to_string(),
                    |language| {
                        display_graph_qualified_name(
                            language,
                            qualified.as_ref(),
                            entry.kind,
                            entry.is_static,
                        )
                    },
                );
                (display == name).then_some(node_id)
            })
            .collect::<Vec<_>>();
        matches.sort_unstable();
        matches.dedup();
        matches
    }

    fn find_by_exact_interned_name(&self, name: &str) -> Vec<crate::graph::unified::node::NodeId> {
        let Some(str_id) = self.strings.get(name) else {
            return Vec::new();
        };

        let mut matches: Vec<crate::graph::unified::node::NodeId> = Vec::new();
        matches.extend_from_slice(self.indices.by_name(str_id));
        matches.extend_from_slice(self.indices.by_qualified_name(str_id));
        matches.sort_unstable();
        matches.dedup();
        matches.retain(|&node_id| !self.is_node_synthetic(node_id));
        matches
    }

    /// Returns `true` if the node should be treated as a synthetic
    /// placeholder for user-facing surfaces.
    ///
    /// Combines the metadata-store flag and the structural name-shape
    /// fallback (see [`Self::find_by_pattern_with_options`] for the
    /// full rationale). Returns `false` for missing nodes (an unknown
    /// `NodeId` is not "synthetic" — it is "not present").
    #[must_use]
    pub fn is_node_synthetic(&self, node_id: crate::graph::unified::node::NodeId) -> bool {
        // Authoritative check: metadata-store bit (NodeFlags::SYNTHETIC).
        if self.macro_metadata.is_synthetic(node_id) {
            return true;
        }
        // Structural fallback: name shape recognised as synthetic.
        // Required for V10 snapshots written before the bit existed,
        // for unification losers that lost their metadata entry, and
        // as defence-in-depth against future plugins forgetting to
        // flip the bit.
        let Some(entry) = self.nodes.get(node_id) else {
            return false;
        };
        if entry.is_unified_loser() {
            // Already invisible for other reasons; do not also flag as synthetic.
            return false;
        }
        let Some(name) = self.strings.resolve(entry.name) else {
            return false;
        };
        crate::graph::unified::storage::arena::NodeEntry::is_synthetic_placeholder_name(
            name.as_ref(),
        )
    }

    /// Gets all callees of a node (functions called by this node).
    ///
    /// Queries the forward edge store for all Calls edges from this node.
    ///
    /// # Arguments
    ///
    /// * `node` - The node ID to query
    ///
    /// # Returns
    ///
    /// A vector of `NodeIds` representing functions called by this node.
    #[must_use]
    pub fn get_callees(
        &self,
        node: crate::graph::unified::node::NodeId,
    ) -> Vec<crate::graph::unified::node::NodeId> {
        use crate::graph::unified::edge::EdgeKind;

        self.edges
            .edges_from(node)
            .into_iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Calls { .. }))
            .map(|edge| edge.target)
            .collect()
    }

    /// Gets all callers of a node (functions that call this node).
    ///
    /// Queries the reverse edge store for all Calls edges to this node.
    ///
    /// # Arguments
    ///
    /// * `node` - The node ID to query
    ///
    /// # Returns
    ///
    /// A vector of `NodeIds` representing functions that call this node.
    #[must_use]
    pub fn get_callers(
        &self,
        node: crate::graph::unified::node::NodeId,
    ) -> Vec<crate::graph::unified::node::NodeId> {
        use crate::graph::unified::edge::EdgeKind;

        self.edges
            .edges_to(node)
            .into_iter()
            .filter(|edge| matches!(edge.kind, EdgeKind::Calls { .. }))
            .map(|edge| edge.source)
            .collect()
    }

    /// Iterates over all nodes in the graph.
    ///
    /// Returns an iterator yielding (`NodeId`, &`NodeEntry`) pairs for all
    /// occupied slots in the arena.
    ///
    /// # Returns
    ///
    /// An iterator over (`NodeId`, &`NodeEntry`) pairs.
    pub fn iter_nodes(
        &self,
    ) -> impl Iterator<
        Item = (
            crate::graph::unified::node::NodeId,
            &crate::graph::unified::storage::arena::NodeEntry,
        ),
    > {
        self.nodes.iter()
    }

    /// Iterates over all edges in the graph.
    ///
    /// Returns an iterator yielding (source, target, `EdgeKind`) tuples for
    /// all edges in the forward edge store.
    ///
    /// # Returns
    ///
    /// An iterator over edge tuples.
    pub fn iter_edges(
        &self,
    ) -> impl Iterator<
        Item = (
            crate::graph::unified::node::NodeId,
            crate::graph::unified::node::NodeId,
            crate::graph::unified::edge::EdgeKind,
        ),
    > + '_ {
        // Iterate over all nodes in the arena and get their outgoing edges
        self.nodes.iter().flat_map(move |(node_id, _entry)| {
            // Get all edges from this node
            self.edges
                .edges_from(node_id)
                .into_iter()
                .map(move |edge| (node_id, edge.target, edge.kind))
        })
    }

    /// Gets a node entry by ID.
    ///
    /// Returns a reference to the `NodeEntry` if the ID is valid, or None
    /// if the ID is invalid or stale.
    ///
    /// # Arguments
    ///
    /// * `id` - The node ID to look up
    ///
    /// # Returns
    ///
    /// A reference to the `NodeEntry`, or None if not found.
    #[must_use]
    pub fn get_node(
        &self,
        id: crate::graph::unified::node::NodeId,
    ) -> Option<&crate::graph::unified::storage::arena::NodeEntry> {
        self.nodes.get(id)
    }
}

impl fmt::Debug for GraphSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphSnapshot")
            .field("nodes", &self.nodes.len())
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

/// Read-only accessor trait shared by [`CodeGraph`] and [`GraphSnapshot`].
///
/// This lets helpers that only *read* graph state (name-matching, relation
/// traversal, reference lookups) be written once and called from both the
/// live `CodeGraph` path in `sqry-core::query::executor::graph_eval` and
/// the snapshot-based path in `sqry-db::queries::*`. No mutation is exposed.
pub trait GraphAccess {
    /// Returns the node arena (read-only).
    fn nodes(&self) -> &NodeArena;
    /// Returns the bidirectional edge store (read-only).
    fn edges(&self) -> &BidirectionalEdgeStore;
    /// Returns the string interner (read-only).
    fn strings(&self) -> &StringInterner;
    /// Returns the file registry (read-only).
    fn files(&self) -> &FileRegistry;
    /// Returns the auxiliary indices (read-only).
    fn indices(&self) -> &AuxiliaryIndices;
}

impl GraphAccess for CodeGraph {
    #[inline]
    fn nodes(&self) -> &NodeArena {
        CodeGraph::nodes(self)
    }
    #[inline]
    fn edges(&self) -> &BidirectionalEdgeStore {
        CodeGraph::edges(self)
    }
    #[inline]
    fn strings(&self) -> &StringInterner {
        CodeGraph::strings(self)
    }
    #[inline]
    fn files(&self) -> &FileRegistry {
        CodeGraph::files(self)
    }
    #[inline]
    fn indices(&self) -> &AuxiliaryIndices {
        CodeGraph::indices(self)
    }
}

impl GraphAccess for GraphSnapshot {
    #[inline]
    fn nodes(&self) -> &NodeArena {
        GraphSnapshot::nodes(self)
    }
    #[inline]
    fn edges(&self) -> &BidirectionalEdgeStore {
        GraphSnapshot::edges(self)
    }
    #[inline]
    fn strings(&self) -> &StringInterner {
        GraphSnapshot::strings(self)
    }
    #[inline]
    fn files(&self) -> &FileRegistry {
        GraphSnapshot::files(self)
    }
    #[inline]
    fn indices(&self) -> &AuxiliaryIndices {
        GraphSnapshot::indices(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::{
        FileScope, NodeId, ResolutionMode, SymbolCandidateOutcome, SymbolQuery,
        SymbolResolutionOutcome,
    };

    fn resolve_symbol_strict(snapshot: &GraphSnapshot, symbol: &str) -> Option<NodeId> {
        match snapshot.resolve_symbol(&SymbolQuery {
            symbol,
            file_scope: FileScope::Any,
            mode: ResolutionMode::Strict,
        }) {
            SymbolResolutionOutcome::Resolved(node_id) => Some(node_id),
            SymbolResolutionOutcome::NotFound
            | SymbolResolutionOutcome::FileNotIndexed
            | SymbolResolutionOutcome::Ambiguous(_) => None,
        }
    }

    fn candidate_nodes(snapshot: &GraphSnapshot, symbol: &str) -> Vec<NodeId> {
        match snapshot.find_symbol_candidates(&SymbolQuery {
            symbol,
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        }) {
            SymbolCandidateOutcome::Candidates(candidates) => candidates,
            SymbolCandidateOutcome::NotFound | SymbolCandidateOutcome::FileNotIndexed => Vec::new(),
        }
    }

    #[test]
    fn test_code_graph_new() {
        let graph = CodeGraph::new();
        assert_eq!(graph.epoch(), 0);
        assert_eq!(graph.nodes().len(), 0);
    }

    #[test]
    fn test_code_graph_default() {
        let graph = CodeGraph::default();
        assert_eq!(graph.epoch(), 0);
    }

    #[test]
    fn test_code_graph_snapshot() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        assert_eq!(snapshot.epoch(), 0);
        assert_eq!(snapshot.nodes().len(), 0);
    }

    #[test]
    fn test_code_graph_bump_epoch() {
        let mut graph = CodeGraph::new();
        assert_eq!(graph.epoch(), 0);
        assert_eq!(graph.bump_epoch(), 1);
        assert_eq!(graph.epoch(), 1);
        assert_eq!(graph.bump_epoch(), 2);
        assert_eq!(graph.epoch(), 2);
    }

    #[test]
    fn test_code_graph_set_epoch() {
        let mut graph = CodeGraph::new();
        graph.set_epoch(42);
        assert_eq!(graph.epoch(), 42);
    }

    #[test]
    fn test_code_graph_from_components() {
        let nodes = NodeArena::new();
        let edges = BidirectionalEdgeStore::new();
        let strings = StringInterner::new();
        let files = FileRegistry::new();
        let indices = AuxiliaryIndices::new();
        let macro_metadata = NodeMetadataStore::new();

        let graph =
            CodeGraph::from_components(nodes, edges, strings, files, indices, macro_metadata);
        assert_eq!(graph.epoch(), 0);
    }

    #[test]
    fn test_code_graph_mut_accessors() {
        let mut graph = CodeGraph::new();

        // Access mutable references - should not panic
        let _nodes = graph.nodes_mut();
        let _edges = graph.edges_mut();
        let _strings = graph.strings_mut();
        let _files = graph.files_mut();
        let _indices = graph.indices_mut();
    }

    #[test]
    fn test_code_graph_snapshot_isolation() {
        let mut graph = CodeGraph::new();
        let snapshot1 = graph.snapshot();

        // Mutate the graph
        graph.bump_epoch();

        let snapshot2 = graph.snapshot();

        // Snapshots should have different epochs
        assert_eq!(snapshot1.epoch(), 0);
        assert_eq!(snapshot2.epoch(), 1);
    }

    #[test]
    fn test_code_graph_debug() {
        let graph = CodeGraph::new();
        let debug_str = format!("{graph:?}");
        assert!(debug_str.contains("CodeGraph"));
        assert!(debug_str.contains("epoch"));
    }

    /// Exact-byte regression for the iter2 fix of CodeGraph.confidence
    /// accounting: adding a `ConfidenceMetadata` entry with known inner
    /// Vec<String> capacities must increase `heap_bytes()` by exactly the sum
    /// of those capacities plus Vec<String> slot overhead.
    #[test]
    fn test_codegraph_heap_bytes_counts_confidence_inner_strings() {
        let mut graph = CodeGraph::new();
        // Seed one confidence entry so the HashMap has non-zero capacity;
        // reserve slack so the next insert cannot rehash.
        graph.set_confidence({
            let mut m = HashMap::with_capacity(8);
            m.insert("seed".to_string(), ConfidenceMetadata::default());
            m
        });
        let before = graph.heap_bytes();
        let before_cap = graph.confidence.capacity();

        let lim1 = String::from("no type inference");
        let lim2 = String::from("no generic specialization");
        let feat1 = String::from("rust-analyzer");
        let l1 = lim1.capacity();
        let l2 = lim2.capacity();
        let f1 = feat1.capacity();

        let limitations = vec![lim1, lim2];
        let lim_vec_cap = limitations.capacity();

        let unavailable_features = vec![feat1];
        let feat_vec_cap = unavailable_features.capacity();

        let key = String::from("rust");
        let key_cap = key.capacity();
        graph.confidence.insert(
            key,
            ConfidenceMetadata {
                limitations,
                unavailable_features,
                ..Default::default()
            },
        );
        assert_eq!(
            graph.confidence.capacity(),
            before_cap,
            "prerequisite: confidence HashMap must not rehash during the test insert",
        );

        let after = graph.heap_bytes();
        let expected_inner = key_cap
            + lim_vec_cap * std::mem::size_of::<String>()
            + l1
            + l2
            + feat_vec_cap * std::mem::size_of::<String>()
            + f1;
        assert_eq!(
            after - before,
            expected_inner,
            "CodeGraph::heap_bytes must count ConfidenceMetadata inner Vec<String> capacity exactly",
        );
    }

    #[test]
    fn test_codegraph_heap_bytes_grows_with_content() {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        // An empty graph reports some heap bytes (FileRegistry seeds index 0
        // with `vec![None]`, HashMaps have non-zero base capacity once touched,
        // etc.) but the value must be finite and well under the 100 MB cap.
        let empty = CodeGraph::new();
        let empty_bytes = empty.heap_bytes();
        assert!(
            empty_bytes < 100 * 1024 * 1024,
            "empty graph heap_bytes should be <100 MiB, got {empty_bytes}"
        );

        let mut graph = CodeGraph::new();
        for i in 0..32u32 {
            let name = format!("sym_{i}");
            let qual = format!("module::sym_{i}");
            let file = format!("file_{i}.rs");

            let name_id = graph.strings_mut().intern(&name).unwrap();
            let qual_id = graph.strings_mut().intern(&qual).unwrap();
            let file_id = graph.files_mut().register(Path::new(&file)).unwrap();

            let entry =
                NodeEntry::new(NodeKind::Function, name_id, file_id).with_qualified_name(qual_id);
            let node_id = graph.nodes_mut().alloc(entry).unwrap();
            graph
                .indices_mut()
                .add(node_id, NodeKind::Function, name_id, Some(qual_id), file_id);
        }

        let populated_bytes = graph.heap_bytes();
        assert!(
            populated_bytes > 0,
            "populated graph should report nonzero heap bytes"
        );
        assert!(
            populated_bytes > empty_bytes,
            "populated graph ({populated_bytes}) should exceed empty graph ({empty_bytes})"
        );
        assert!(
            populated_bytes < 100 * 1024 * 1024,
            "test graph heap_bytes should be <100 MiB, got {populated_bytes}"
        );
    }

    #[test]
    fn test_concurrent_code_graph_new() {
        let graph = ConcurrentCodeGraph::new();
        assert_eq!(graph.epoch(), 0);
    }

    #[test]
    fn test_concurrent_code_graph_default() {
        let graph = ConcurrentCodeGraph::default();
        assert_eq!(graph.epoch(), 0);
    }

    #[test]
    fn test_concurrent_code_graph_from_graph() {
        let mut inner = CodeGraph::new();
        inner.set_epoch(10);
        let graph = ConcurrentCodeGraph::from_graph(inner);
        assert_eq!(graph.epoch(), 10);
    }

    #[test]
    fn test_concurrent_code_graph_read() {
        let graph = ConcurrentCodeGraph::new();
        let guard = graph.read();
        assert_eq!(guard.epoch(), 0);
        assert_eq!(guard.nodes().len(), 0);
    }

    #[test]
    fn test_concurrent_code_graph_write_increments_epoch() {
        let graph = ConcurrentCodeGraph::new();
        assert_eq!(graph.epoch(), 0);

        {
            let guard = graph.write();
            assert_eq!(guard.epoch(), 1);
        }

        assert_eq!(graph.epoch(), 1);

        {
            let _guard = graph.write();
        }

        assert_eq!(graph.epoch(), 2);
    }

    #[test]
    fn test_concurrent_code_graph_snapshot() {
        let graph = ConcurrentCodeGraph::new();

        {
            let _guard = graph.write();
        }

        let snapshot = graph.snapshot();
        assert_eq!(snapshot.epoch(), 1);
    }

    #[test]
    fn test_concurrent_code_graph_try_read() {
        let graph = ConcurrentCodeGraph::new();
        let guard = graph.try_read();
        assert!(guard.is_some());
    }

    #[test]
    fn test_concurrent_code_graph_try_write() {
        let graph = ConcurrentCodeGraph::new();
        let guard = graph.try_write();
        assert!(guard.is_some());
        assert_eq!(graph.epoch(), 1);
    }

    #[test]
    fn test_concurrent_code_graph_debug() {
        let graph = ConcurrentCodeGraph::new();
        let debug_str = format!("{graph:?}");
        assert!(debug_str.contains("ConcurrentCodeGraph"));
        assert!(debug_str.contains("epoch"));
    }

    #[test]
    fn test_graph_snapshot_accessors() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();

        // All accessors should work
        let _nodes = snapshot.nodes();
        let _edges = snapshot.edges();
        let _strings = snapshot.strings();
        let _files = snapshot.files();
        let _indices = snapshot.indices();
        let _epoch = snapshot.epoch();
    }

    #[test]
    fn test_graph_snapshot_epoch_matches() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();

        assert!(snapshot.epoch_matches(0));
        assert!(!snapshot.epoch_matches(1));
    }

    #[test]
    fn test_graph_snapshot_clone() {
        let graph = CodeGraph::new();
        let snapshot1 = graph.snapshot();
        let snapshot2 = snapshot1.clone();

        assert_eq!(snapshot1.epoch(), snapshot2.epoch());
    }

    #[test]
    fn test_graph_snapshot_debug() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let debug_str = format!("{snapshot:?}");
        assert!(debug_str.contains("GraphSnapshot"));
        assert!(debug_str.contains("epoch"));
    }

    #[test]
    fn test_multiple_readers() {
        let graph = ConcurrentCodeGraph::new();

        // Multiple readers should be able to acquire locks simultaneously
        let guard1 = graph.read();
        let guard2 = graph.read();
        let guard3 = graph.read();

        assert_eq!(guard1.epoch(), 0);
        assert_eq!(guard2.epoch(), 0);
        assert_eq!(guard3.epoch(), 0);
    }

    #[test]
    fn test_code_graph_clone() {
        let mut graph = CodeGraph::new();
        graph.bump_epoch();

        let cloned = graph.clone();
        assert_eq!(cloned.epoch(), 1);
    }

    #[test]
    fn test_epoch_wrapping() {
        let mut graph = CodeGraph::new();
        graph.set_epoch(u64::MAX);
        let new_epoch = graph.bump_epoch();
        assert_eq!(new_epoch, 0); // Should wrap around
    }

    // ============================================================================
    // Query method tests
    // ============================================================================

    #[test]
    fn test_snapshot_resolve_symbol() {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Add some nodes with qualified names
        let name_id = graph.strings_mut().intern("test_func").unwrap();
        let qual_name_id = graph.strings_mut().intern("module::test_func").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let entry =
            NodeEntry::new(NodeKind::Function, name_id, file_id).with_qualified_name(qual_name_id);

        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph.indices_mut().add(
            node_id,
            NodeKind::Function,
            name_id,
            Some(qual_name_id),
            file_id,
        );

        let snapshot = graph.snapshot();

        // Find by qualified name
        let found = resolve_symbol_strict(&snapshot, "module::test_func");
        assert_eq!(found, Some(node_id));

        // Find by exact simple name
        let found2 = resolve_symbol_strict(&snapshot, "test_func");
        assert_eq!(found2, Some(node_id));

        // Not found
        assert!(resolve_symbol_strict(&snapshot, "nonexistent").is_none());
    }

    #[test]
    fn test_snapshot_find_by_pattern() {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Add nodes with different names
        let name1 = graph.strings_mut().intern("foo_bar").unwrap();
        let name2 = graph.strings_mut().intern("baz_bar").unwrap();
        let name3 = graph.strings_mut().intern("qux_test").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node1 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name1, file_id))
            .unwrap();
        let node2 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name2, file_id))
            .unwrap();
        let node3 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name3, file_id))
            .unwrap();

        graph
            .indices_mut()
            .add(node1, NodeKind::Function, name1, None, file_id);
        graph
            .indices_mut()
            .add(node2, NodeKind::Function, name2, None, file_id);
        graph
            .indices_mut()
            .add(node3, NodeKind::Function, name3, None, file_id);

        let snapshot = graph.snapshot();

        // Find by pattern
        let matches = snapshot.find_by_pattern("bar");
        assert_eq!(matches.len(), 2);
        assert!(matches.contains(&node1));
        assert!(matches.contains(&node2));

        // Find single match
        let matches = snapshot.find_by_pattern("qux");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], node3);

        // No matches
        let matches = snapshot.find_by_pattern("nonexistent");
        assert!(matches.is_empty());
    }

    #[test]
    fn synthetic_nodes_are_filtered_from_find_by_pattern_default() {
        // C_SUPPRESS: synthetic placeholder nodes (Go plugin
        // `<field:operand.field>` shadows + `<ident>@<offset>`
        // per-binding-site Variables) must NOT surface from
        // find_by_pattern, but must still be reachable via
        // find_by_pattern_with_options(_, true).
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();
        let real_property = graph
            .strings_mut()
            .intern("main.SelectorSource.NeedTags")
            .unwrap();
        let real_local_var = graph.strings_mut().intern("NeedTags").unwrap();
        let synthetic_field = graph
            .strings_mut()
            .intern("<field:selector.NeedTags>")
            .unwrap();
        let synthetic_offset_a = graph.strings_mut().intern("NeedTags@469").unwrap();
        let synthetic_offset_b = graph.strings_mut().intern("NeedTags@508").unwrap();
        let file_id = graph.files_mut().register(Path::new("main.go")).unwrap();

        let prop_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Property, real_property, file_id))
            .unwrap();
        let local_var_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Variable, real_local_var, file_id))
            .unwrap();
        let syn_field_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Variable, synthetic_field, file_id))
            .unwrap();
        let syn_a_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(
                NodeKind::Variable,
                synthetic_offset_a,
                file_id,
            ))
            .unwrap();
        let syn_b_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(
                NodeKind::Variable,
                synthetic_offset_b,
                file_id,
            ))
            .unwrap();

        graph
            .indices_mut()
            .add(prop_id, NodeKind::Property, real_property, None, file_id);
        graph.indices_mut().add(
            local_var_id,
            NodeKind::Variable,
            real_local_var,
            None,
            file_id,
        );
        graph.indices_mut().add(
            syn_field_id,
            NodeKind::Variable,
            synthetic_field,
            None,
            file_id,
        );
        graph.indices_mut().add(
            syn_a_id,
            NodeKind::Variable,
            synthetic_offset_a,
            None,
            file_id,
        );
        graph.indices_mut().add(
            syn_b_id,
            NodeKind::Variable,
            synthetic_offset_b,
            None,
            file_id,
        );

        // Flag two of them via the metadata-store bit (the canonical
        // Go-plugin emission path) and leave one (`<field:...>`) only
        // covered by the structural name-shape fallback to verify both
        // recognition channels suppress the leak.
        graph.macro_metadata_mut().mark_synthetic(syn_a_id);
        graph.macro_metadata_mut().mark_synthetic(syn_b_id);

        let snapshot = graph.snapshot();

        // Default surface (CLI `search --exact`, MCP, LSP): no synthetics.
        let matches = snapshot.find_by_pattern("NeedTags");
        assert!(matches.contains(&prop_id), "Property must be surfaced");
        assert!(
            matches.contains(&local_var_id),
            "real local var must be surfaced"
        );
        assert!(
            !matches.contains(&syn_field_id),
            "<field:...> synthetic must be suppressed (name-shape fallback)"
        );
        assert!(
            !matches.contains(&syn_a_id),
            "NeedTags@469 must be suppressed (metadata bit)"
        );
        assert!(
            !matches.contains(&syn_b_id),
            "NeedTags@508 must be suppressed (metadata bit)"
        );
        assert_eq!(matches.len(), 2, "exactly Property + local var, no leakage");

        // Internal include-synthetic surface (binding plane, scope analysis):
        // every node remains reachable.
        let all_matches = snapshot.find_by_pattern_with_options("NeedTags", true);
        assert_eq!(
            all_matches.len(),
            5,
            "include_synthetic surfaces everything"
        );
        assert!(all_matches.contains(&prop_id));
        assert!(all_matches.contains(&local_var_id));
        assert!(all_matches.contains(&syn_field_id));
        assert!(all_matches.contains(&syn_a_id));
        assert!(all_matches.contains(&syn_b_id));

        // is_node_synthetic exposed for surface-level filters
        // (e.g., MCP semantic_search/relation_query post-filters).
        assert!(snapshot.is_node_synthetic(syn_field_id));
        assert!(snapshot.is_node_synthetic(syn_a_id));
        assert!(snapshot.is_node_synthetic(syn_b_id));
        assert!(!snapshot.is_node_synthetic(prop_id));
        assert!(!snapshot.is_node_synthetic(local_var_id));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn find_by_exact_name_aligns_with_planner_name_predicate() {
        // B1_ALIGN: `find_by_exact_name("NeedTags")` is the canonical
        // surface for the CLI `--exact NeedTags` shorthand and the
        // planner's `name:NeedTags` predicate. Both paths must return
        // the same set against this fixture.
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();
        // Property nodes carry the package-qualified name as
        // `entry.name` (Go plugin convention; see `helper.rs`'s
        // `semantic_name_for_node_input`).
        let property_qname = graph
            .strings_mut()
            .intern("main.SelectorSource.NeedTags")
            .unwrap();
        let local_var_name = graph.strings_mut().intern("NeedTags").unwrap();
        let synthetic_field_name = graph
            .strings_mut()
            .intern("<field:selector.NeedTags>")
            .unwrap();
        let synthetic_offset_name = graph.strings_mut().intern("NeedTags@469").unwrap();
        let unrelated_name = graph.strings_mut().intern("NeedTagsHelper").unwrap();
        let display_fallback_name = graph.strings_mut().intern("Other").unwrap();
        let display_fallback_qname = graph
            .strings_mut()
            .intern("main::SelectorSource::Other")
            .unwrap();
        let file_id = graph.files_mut().register(Path::new("main.go")).unwrap();

        let prop_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Property, property_qname, file_id))
            .unwrap();
        let local_var_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Variable, local_var_name, file_id))
            .unwrap();
        let syn_field_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(
                NodeKind::Variable,
                synthetic_field_name,
                file_id,
            ))
            .unwrap();
        let syn_offset_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(
                NodeKind::Variable,
                synthetic_offset_name,
                file_id,
            ))
            .unwrap();
        let unrelated_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, unrelated_name, file_id))
            .unwrap();
        let display_fallback_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Property, display_fallback_name, file_id)
                    .with_qualified_name(display_fallback_qname),
            )
            .unwrap();

        graph
            .indices_mut()
            .add(prop_id, NodeKind::Property, property_qname, None, file_id);
        graph.indices_mut().add(
            local_var_id,
            NodeKind::Variable,
            local_var_name,
            None,
            file_id,
        );
        graph.indices_mut().add(
            syn_field_id,
            NodeKind::Variable,
            synthetic_field_name,
            None,
            file_id,
        );
        graph.indices_mut().add(
            syn_offset_id,
            NodeKind::Variable,
            synthetic_offset_name,
            None,
            file_id,
        );
        graph.indices_mut().add(
            unrelated_id,
            NodeKind::Function,
            unrelated_name,
            None,
            file_id,
        );
        graph.indices_mut().add(
            display_fallback_id,
            NodeKind::Property,
            display_fallback_name,
            Some(display_fallback_qname),
            file_id,
        );

        // Mark the offset-suffixed synthetic via the metadata bit so we
        // exercise both recognition channels in this fixture.
        graph.macro_metadata_mut().mark_synthetic(syn_offset_id);

        let snapshot = graph.snapshot();

        // Exact-name lookup on "NeedTags" — should pick up only the
        // local variable (its `entry.name` is exactly "NeedTags"); it
        // must NOT pick up the Property (qualified name contains but
        // does not equal "NeedTags") and must NOT pick up either
        // synthetic placeholder.
        let exact = snapshot.find_by_exact_name("NeedTags");
        assert_eq!(
            exact,
            vec![local_var_id],
            "exact match must be byte-for-byte against entry.name / qualified_name and exclude synthetics"
        );

        // The Property's full qualified name is exact-addressable.
        let qualified = snapshot.find_by_exact_name("main.SelectorSource.NeedTags");
        assert_eq!(qualified, vec![prop_id]);

        // Dot-qualified display form falls back to graph-canonical `::` only
        // when the exact dot string was absent.
        let display_fallback = snapshot.find_by_exact_name("main.SelectorSource.Other");
        assert_eq!(display_fallback, vec![display_fallback_id]);

        // Substring-only matches must not surface from exact lookup.
        assert!(
            snapshot
                .find_by_exact_name("NeedTagsHelper")
                .contains(&unrelated_id)
        );
        assert!(
            !snapshot
                .find_by_exact_name("NeedTags")
                .contains(&unrelated_id),
            "exact 'NeedTags' must not match 'NeedTagsHelper'"
        );

        // Unknown name short-circuits to an empty vec without
        // scanning any nodes.
        assert!(
            snapshot
                .find_by_exact_name("ThisStringIsNotInterned")
                .is_empty()
        );
    }

    #[test]
    fn test_snapshot_get_callees() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Create caller and callee nodes
        let caller_name = graph.strings_mut().intern("caller").unwrap();
        let callee1_name = graph.strings_mut().intern("callee1").unwrap();
        let callee2_name = graph.strings_mut().intern("callee2").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let caller_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, caller_name, file_id))
            .unwrap();
        let callee1_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, callee1_name, file_id))
            .unwrap();
        let callee2_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, callee2_name, file_id))
            .unwrap();

        // Add call edges
        graph.edges_mut().add_edge(
            caller_id,
            callee1_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );
        graph.edges_mut().add_edge(
            caller_id,
            callee2_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        let snapshot = graph.snapshot();

        // Query callees
        let callees = snapshot.get_callees(caller_id);
        assert_eq!(callees.len(), 2);
        assert!(callees.contains(&callee1_id));
        assert!(callees.contains(&callee2_id));

        // Node with no callees
        let callees = snapshot.get_callees(callee1_id);
        assert!(callees.is_empty());
    }

    #[test]
    fn test_snapshot_get_callers() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Create caller and callee nodes
        let caller1_name = graph.strings_mut().intern("caller1").unwrap();
        let caller2_name = graph.strings_mut().intern("caller2").unwrap();
        let callee_name = graph.strings_mut().intern("callee").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let caller1_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, caller1_name, file_id))
            .unwrap();
        let caller2_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, caller2_name, file_id))
            .unwrap();
        let callee_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, callee_name, file_id))
            .unwrap();

        // Add call edges
        graph.edges_mut().add_edge(
            caller1_id,
            callee_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );
        graph.edges_mut().add_edge(
            caller2_id,
            callee_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        let snapshot = graph.snapshot();

        // Query callers
        let callers = snapshot.get_callers(callee_id);
        assert_eq!(callers.len(), 2);
        assert!(callers.contains(&caller1_id));
        assert!(callers.contains(&caller2_id));

        // Node with no callers
        let callers = snapshot.get_callers(caller1_id);
        assert!(callers.is_empty());
    }

    #[test]
    fn test_snapshot_find_symbol_candidates() {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Add nodes with same symbol name but different qualified names
        let symbol_name = graph.strings_mut().intern("test").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node1 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, symbol_name, file_id))
            .unwrap();
        let node2 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Method, symbol_name, file_id))
            .unwrap();

        // Add a different symbol
        let other_name = graph.strings_mut().intern("other").unwrap();
        let node3 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, other_name, file_id))
            .unwrap();

        graph
            .indices_mut()
            .add(node1, NodeKind::Function, symbol_name, None, file_id);
        graph
            .indices_mut()
            .add(node2, NodeKind::Method, symbol_name, None, file_id);
        graph
            .indices_mut()
            .add(node3, NodeKind::Function, other_name, None, file_id);

        let snapshot = graph.snapshot();

        // Find by symbol
        let matches = candidate_nodes(&snapshot, "test");
        assert_eq!(matches.len(), 2);
        assert!(matches.contains(&node1));
        assert!(matches.contains(&node2));

        // Find other symbol
        let matches = candidate_nodes(&snapshot, "other");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], node3);

        // No matches
        let matches = candidate_nodes(&snapshot, "nonexistent");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_snapshot_iter_nodes() {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Add some nodes
        let name1 = graph.strings_mut().intern("func1").unwrap();
        let name2 = graph.strings_mut().intern("func2").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node1 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name1, file_id))
            .unwrap();
        let node2 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name2, file_id))
            .unwrap();

        let snapshot = graph.snapshot();

        // Iterate nodes
        let snapshot_nodes: Vec<_> = snapshot.iter_nodes().collect();
        assert_eq!(snapshot_nodes.len(), 2);

        let node_ids: Vec<_> = snapshot_nodes.iter().map(|(id, _)| *id).collect();
        assert!(node_ids.contains(&node1));
        assert!(node_ids.contains(&node2));
    }

    #[test]
    fn test_snapshot_iter_edges() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Create nodes
        let name1 = graph.strings_mut().intern("func1").unwrap();
        let name2 = graph.strings_mut().intern("func2").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node1 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name1, file_id))
            .unwrap();
        let node2 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name2, file_id))
            .unwrap();

        // Add edges
        graph.edges_mut().add_edge(
            node1,
            node2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        let snapshot = graph.snapshot();

        // Iterate edges
        let edges: Vec<_> = snapshot.iter_edges().collect();
        assert_eq!(edges.len(), 1);

        let (src, tgt, kind) = &edges[0];
        assert_eq!(*src, node1);
        assert_eq!(*tgt, node2);
        assert!(matches!(
            kind,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            }
        ));
    }

    #[test]
    fn test_snapshot_get_node() {
        use crate::graph::unified::node::NodeId;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Add a node
        let name = graph.strings_mut().intern("test_func").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name, file_id))
            .unwrap();

        let snapshot = graph.snapshot();

        // Get node
        let entry = snapshot.get_node(node_id);
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().kind, NodeKind::Function);

        // Invalid node
        let invalid_id = NodeId::INVALID;
        assert!(snapshot.get_node(invalid_id).is_none());
    }

    #[test]
    fn test_snapshot_query_empty_graph() {
        use crate::graph::unified::node::NodeId;

        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();

        // All queries should return empty on empty graph
        assert!(resolve_symbol_strict(&snapshot, "test").is_none());
        assert!(snapshot.find_by_pattern("test").is_empty());
        assert!(candidate_nodes(&snapshot, "test").is_empty());

        let dummy_id = NodeId::new(0, 1);
        assert!(snapshot.get_callees(dummy_id).is_empty());
        assert!(snapshot.get_callers(dummy_id).is_empty());

        assert_eq!(snapshot.iter_nodes().count(), 0);
        assert_eq!(snapshot.iter_edges().count(), 0);
    }

    #[test]
    fn test_snapshot_edge_filtering_by_kind() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();

        // Create nodes
        let name1 = graph.strings_mut().intern("func1").unwrap();
        let name2 = graph.strings_mut().intern("func2").unwrap();
        let file_id = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let node1 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name1, file_id))
            .unwrap();
        let node2 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name2, file_id))
            .unwrap();

        // Add different kinds of edges
        graph.edges_mut().add_edge(
            node1,
            node2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );
        graph
            .edges_mut()
            .add_edge(node1, node2, EdgeKind::References, file_id);

        let snapshot = graph.snapshot();

        // get_callees should only return Calls edges
        let callees = snapshot.get_callees(node1);
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0], node2);

        // iter_edges returns all edges regardless of kind
        let edges: Vec<_> = snapshot.iter_edges().collect();
        assert_eq!(edges.len(), 2);
    }

    // -------- reverse_import_index tests --------

    /// Helper: build an empty graph with the given file paths registered, a
    /// single placeholder node allocated per file, and indices rebuilt so
    /// `by_file` returns the per-file node sets. Returns the file IDs in
    /// the order passed in and the per-file node ID.
    #[cfg(test)]
    fn build_import_test_graph(files: &[&str]) -> (CodeGraph, Vec<FileId>, Vec<NodeId>) {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();
        let placeholder_name = graph.strings_mut().intern("sym").unwrap();
        let mut file_ids = Vec::with_capacity(files.len());
        let mut node_ids = Vec::with_capacity(files.len());
        for path in files {
            let file_id = graph.files_mut().register(Path::new(path)).unwrap();
            let node_id = graph
                .nodes_mut()
                .alloc(NodeEntry::new(
                    NodeKind::Function,
                    placeholder_name,
                    file_id,
                ))
                .unwrap();
            file_ids.push(file_id);
            node_ids.push(node_id);
        }
        graph.rebuild_indices();
        (graph, file_ids, node_ids)
    }

    /// Helper: add an `Imports` edge from `source_node` (in importer file) to
    /// `target_node` (in exporter file). The edge is recorded against the
    /// importer file so `clear_file` cleanup would behave identically to
    /// production Pass 4 writes.
    #[cfg(test)]
    fn add_import_edge(
        graph: &mut CodeGraph,
        source_node: NodeId,
        target_node: NodeId,
        importer_file: FileId,
    ) {
        graph.edges_mut().add_edge(
            source_node,
            target_node,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
            importer_file,
        );
    }

    #[test]
    fn reverse_import_index_empty_graph_returns_empty() {
        let (graph, files, _) = build_import_test_graph(&["only.rs"]);
        assert!(graph.reverse_import_index(files[0]).is_empty());
    }

    #[test]
    fn reverse_import_index_single_importer() {
        // File A imports a symbol exported by file B. Reverse index of B
        // should return exactly [A]; reverse index of A should be empty.
        let (mut graph, files, nodes) = build_import_test_graph(&["a.rs", "b.rs"]);
        let (a, b) = (files[0], files[1]);
        add_import_edge(&mut graph, nodes[0], nodes[1], a);

        let importers_of_b = graph.reverse_import_index(b);
        assert_eq!(importers_of_b, vec![a]);

        let importers_of_a = graph.reverse_import_index(a);
        assert!(
            importers_of_a.is_empty(),
            "A has no inbound Imports edges; reverse index must be empty"
        );
    }

    #[test]
    fn reverse_import_index_multiple_importers_deduped_and_sorted() {
        // A, B, C all import from D. Reverse index of D must contain each
        // importer exactly once, sorted ascending by FileId.
        let (mut graph, files, nodes) = build_import_test_graph(&["a.rs", "b.rs", "c.rs", "d.rs"]);
        let (a, b, c, d) = (files[0], files[1], files[2], files[3]);
        add_import_edge(&mut graph, nodes[0], nodes[3], a);
        add_import_edge(&mut graph, nodes[1], nodes[3], b);
        add_import_edge(&mut graph, nodes[2], nodes[3], c);
        // Add a duplicate edge from A to D to confirm dedup behavior.
        add_import_edge(&mut graph, nodes[0], nodes[3], a);

        let importers_of_d = graph.reverse_import_index(d);
        assert_eq!(importers_of_d, vec![a, b, c]);
        // Sort invariant: Vec is ascending by raw index.
        let mut sorted = importers_of_d.clone();
        sorted.sort();
        assert_eq!(importers_of_d, sorted);
    }

    #[test]
    fn reverse_import_index_filters_non_import_edges() {
        // A `Calls` edge from A into B must not contribute to B's reverse
        // import index. Only `EdgeKind::Imports` edges count.
        let (mut graph, files, nodes) = build_import_test_graph(&["a.rs", "b.rs"]);
        let (a, b) = (files[0], files[1]);
        graph.edges_mut().add_edge(
            nodes[0],
            nodes[1],
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            a,
        );
        graph
            .edges_mut()
            .add_edge(nodes[0], nodes[1], EdgeKind::References, a);

        assert!(
            graph.reverse_import_index(b).is_empty(),
            "non-Imports edges must not register as importers"
        );
    }

    #[test]
    fn reverse_import_index_elides_self_imports() {
        // An Imports edge whose source and target are both in the same file
        // is a self-import; the caller's own file must not appear in its own
        // reverse index.
        let (mut graph, files, nodes) = build_import_test_graph(&["a.rs"]);
        let a = files[0];
        // Add a second node in the same file so we have a distinct source.
        let name2 = graph.strings_mut().intern("sym2").unwrap();
        let second_in_a = graph
            .nodes_mut()
            .alloc(crate::graph::unified::storage::arena::NodeEntry::new(
                crate::graph::unified::node::NodeKind::Function,
                name2,
                a,
            ))
            .unwrap();
        graph.rebuild_indices();
        add_import_edge(&mut graph, second_in_a, nodes[0], a);

        assert!(
            graph.reverse_import_index(a).is_empty(),
            "self-imports must be elided from reverse index"
        );
    }

    #[test]
    fn reverse_import_index_mixed_edge_kinds_counts_only_imports() {
        // Two files: A has both Calls and Imports edges into B. Reverse
        // index must return exactly [A] — the Calls edge contributes
        // nothing.
        let (mut graph, files, nodes) = build_import_test_graph(&["a.rs", "b.rs"]);
        let (a, b) = (files[0], files[1]);
        add_import_edge(&mut graph, nodes[0], nodes[1], a);
        graph.edges_mut().add_edge(
            nodes[0],
            nodes[1],
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            a,
        );

        assert_eq!(graph.reverse_import_index(b), vec![a]);
    }

    #[test]
    fn reverse_import_index_uninitialized_file_returns_empty() {
        // Querying a FileId that is not registered in the graph must return
        // an empty Vec, not panic.
        let (graph, _, _) = build_import_test_graph(&["a.rs"]);
        let bogus = FileId::new(9999);
        assert!(
            graph.reverse_import_index(bogus).is_empty(),
            "unknown FileId must return empty Vec without panicking"
        );
    }

    #[test]
    fn reverse_import_index_skips_tombstoned_source_nodes() {
        // In the incremental rebuild path the prior graph retains tombstoned
        // arena slots for nodes belonging to closure files that have been
        // removed but not yet fully compacted. reverse_import_index is
        // called on that prior graph to widen the closure, so its
        // tombstone guard (`let Some(source_entry) = self.nodes.get(...)`)
        // is a semantically important branch, not a theoretical one.
        // This test exercises it directly by tombstoning the source node of
        // an Imports edge and asserting the edge silently disappears from
        // the reverse index.
        let (mut graph, files, nodes) = build_import_test_graph(&["a.rs", "b.rs"]);
        let (a, b) = (files[0], files[1]);
        add_import_edge(&mut graph, nodes[0], nodes[1], a);
        // Sanity: before tombstoning, A shows up as the importer of B.
        assert_eq!(graph.reverse_import_index(b), vec![a]);
        // Tombstone the source node via the arena (generation bump). The
        // Imports edge still exists in the edge store but its source NodeId
        // is now stale; `nodes.get(edge_ref.source)` returns None and the
        // guard skips the edge.
        let removed = graph.nodes_mut().remove(nodes[0]);
        assert!(
            removed.is_some(),
            "arena.remove must succeed for a live node"
        );
        assert!(
            graph.nodes().get(nodes[0]).is_none(),
            "tombstoned lookup must return None"
        );
        assert!(
            graph.reverse_import_index(b).is_empty(),
            "Imports edges whose source is tombstoned must be silently skipped"
        );
    }

    // ------------------------------------------------------------------
    // Task 4 Step 2 — CodeGraph::remove_file
    // ------------------------------------------------------------------

    /// Seed a graph with 2 files × `per_file` nodes per file, plus a set
    /// of intra- and inter-file edges. Each call site produces a
    /// canonical topology so tests below can assert bit-level on edge
    /// survival. Returns `(graph, file_a, file_b, file_a_nodes,
    /// file_b_nodes)`.
    fn seed_two_file_graph(
        per_file: usize,
    ) -> (
        CodeGraph,
        crate::graph::unified::file::FileId,
        crate::graph::unified::file::FileId,
        Vec<NodeId>,
        Vec<NodeId>,
    ) {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();
        let sym = graph.strings_mut().intern("sym").expect("intern");
        let file_a = graph
            .files_mut()
            .register(Path::new("/tmp/remove_file_test/a.rs"))
            .expect("register a");
        let file_b = graph
            .files_mut()
            .register(Path::new("/tmp/remove_file_test/b.rs"))
            .expect("register b");

        let mut file_a_nodes = Vec::with_capacity(per_file);
        let mut file_b_nodes = Vec::with_capacity(per_file);

        for _ in 0..per_file {
            let n = graph
                .nodes_mut()
                .alloc(NodeEntry::new(NodeKind::Function, sym, file_a))
                .expect("alloc a-node");
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
                .expect("alloc b-node");
            file_b_nodes.push(n);
            graph.files_mut().record_node(file_b, n);
            graph
                .indices_mut()
                .add(n, NodeKind::Function, sym, None, file_b);
        }

        // Intra-file edges inside each file: pairwise a[i] -> a[i+1],
        // b[i] -> b[i+1]. These are the ones that must die when the
        // corresponding file is removed.
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
        // Cross-file edges: a[0] -> b[0], b[0] -> a[0]. Both must die
        // when *either* endpoint's file is removed (plan §F.2).
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

        (graph, file_a, file_b, file_a_nodes, file_b_nodes)
    }

    #[test]
    fn code_graph_remove_file_tombstones_all_per_file_nodes() {
        let (mut graph, file_a, _file_b, file_a_nodes, _file_b_nodes) = seed_two_file_graph(3);

        let returned = graph.remove_file(file_a);

        // Returned list must equal the original per-file bucket
        // membership, deterministically.
        let returned_set: std::collections::HashSet<NodeId> = returned.iter().copied().collect();
        let expected_set: std::collections::HashSet<NodeId> =
            file_a_nodes.iter().copied().collect();
        assert_eq!(
            returned_set, expected_set,
            "remove_file must return exactly the file_a nodes drained from the bucket"
        );

        // Every returned NodeId is gone from the arena.
        for nid in &file_a_nodes {
            assert!(
                graph.nodes().get(*nid).is_none(),
                "node {nid:?} from removed file must be tombstoned in arena"
            );
        }
    }

    #[test]
    fn code_graph_remove_file_invalidates_all_edges_sourced_or_targeted_at_removed_nodes() {
        use crate::graph::unified::edge::EdgeKind;

        let (mut graph, file_a, _file_b, file_a_nodes, file_b_nodes) = seed_two_file_graph(3);

        // Sanity: the seed has 2 intra-A edges, 2 intra-B edges, plus
        // 2 cross-file edges (a0↔b0).
        let before_delta = graph.edges().stats().forward.delta_edge_count;
        assert_eq!(
            before_delta, 6,
            "seed must produce 2 intra-A + 2 intra-B + 2 cross edges"
        );

        let _ = graph.remove_file(file_a);

        // Forward delta after removal: all 2 intra-A edges are gone,
        // both cross edges are gone, only the 2 intra-B edges remain.
        let after_delta_forward = graph.edges().stats().forward.delta_edge_count;
        assert_eq!(
            after_delta_forward, 2,
            "only intra-B forward edges must remain after removing file_a"
        );
        let after_delta_reverse = graph.edges().stats().reverse.delta_edge_count;
        assert_eq!(
            after_delta_reverse, 2,
            "only intra-B reverse edges must remain after removing file_a"
        );

        // Cross-file edge from b[0] -> a[0] must no longer be visible
        // from either direction, because a[0] is tombstoned.
        let b0 = file_b_nodes[0];
        let a0 = file_a_nodes[0];
        let remaining_from_b0: Vec<_> = graph
            .edges()
            .edges_from(b0)
            .into_iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    EdgeKind::Calls {
                        argument_count: 0,
                        is_async: false,
                        resolved_via: ResolvedVia::Direct,
                    }
                )
            })
            .collect();
        assert!(
            !remaining_from_b0.iter().any(|e| e.target == a0),
            "edge b0 -> a0 must be gone after remove_file(file_a)"
        );
        let remaining_to_a0: Vec<_> = graph.edges().edges_to(a0).into_iter().collect();
        assert!(
            remaining_to_a0.is_empty(),
            "every edge targeting the tombstoned a0 must be gone"
        );
    }

    #[test]
    fn code_graph_remove_file_drops_file_registry_entry() {
        let (mut graph, file_a, _file_b, _, _) = seed_two_file_graph(2);

        assert!(
            graph.files().resolve(file_a).is_some(),
            "seed registered file_a"
        );
        assert!(
            !graph.files().nodes_for_file(file_a).is_empty(),
            "seed populated the file_a bucket"
        );

        let _ = graph.remove_file(file_a);

        assert!(
            graph.files().resolve(file_a).is_none(),
            "FileRegistry::resolve must return None after remove_file"
        );
        assert!(
            graph.files().nodes_for_file(file_a).is_empty(),
            "per-file bucket for file_a must be drained"
        );
    }

    #[test]
    fn code_graph_remove_file_is_idempotent_on_unknown_file() {
        use crate::graph::unified::file::FileId;
        let (mut graph, _file_a, _file_b, _, _) = seed_two_file_graph(2);

        // Snapshot state before idempotent no-op.
        let nodes_before = graph.nodes().len();
        let delta_fwd_before = graph.edges().stats().forward.delta_edge_count;
        let delta_rev_before = graph.edges().stats().reverse.delta_edge_count;
        let files_before = graph.files().len();

        // A FileId that was never registered: the caller may legitimately
        // receive a bogus id from stale indexing state. `remove_file`
        // must be a silent no-op.
        let bogus = FileId::new(9999);
        let returned = graph.remove_file(bogus);
        assert!(
            returned.is_empty(),
            "remove_file on unknown FileId must return an empty Vec"
        );

        assert_eq!(graph.nodes().len(), nodes_before, "arena count unchanged");
        assert_eq!(
            graph.edges().stats().forward.delta_edge_count,
            delta_fwd_before,
            "forward delta unchanged"
        );
        assert_eq!(
            graph.edges().stats().reverse.delta_edge_count,
            delta_rev_before,
            "reverse delta unchanged"
        );
        assert_eq!(graph.files().len(), files_before, "file count unchanged");
    }

    #[test]
    fn code_graph_remove_file_clears_file_segments_entry() {
        // Iter-1 Codex review fix: a file's `FileSegmentTable` entry
        // must be cleared on `remove_file`. Without this, a later
        // `FileId` recycle (via `FileRegistry::free_list`) would
        // inherit the previous file's stale node-range and
        // `reindex_files` (`build/reindex.rs`) would tombstone the
        // wrong slots. This unit test seeds a segment entry, removes
        // the file, and asserts the entry is gone.
        use crate::graph::unified::storage::segment::FileSegmentTable;

        let (mut graph, file_a, _file_b, file_a_nodes, _file_b_nodes) = seed_two_file_graph(3);

        // Seed a segment for file A. Production code sets this via
        // Phase 3 parallel commit; here we go through the crate-
        // internal accessor because we are a unit test inside the
        // crate. (The feature-gated `test_only_record_file_segment`
        // public helper exists for the integration tests in
        // `sqry-core/tests/incremental_remove_file_scale.rs`.)
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
        let table: &mut FileSegmentTable = graph.file_segments_mut();
        table.record_range(file_a, first_index, slot_count);
        assert!(
            graph.file_segments().get(file_a).is_some(),
            "seed must install a segment for file_a before remove_file"
        );

        // Remove the file.
        let _ = graph.remove_file(file_a);

        // The segment entry must be gone. `FileSegmentTable::remove`
        // is idempotent (a no-op on unknown ids), so this assertion
        // holds whether or not the seeded range was contiguous.
        assert!(
            graph.file_segments().get(file_a).is_none(),
            "remove_file must clear the FileSegmentTable entry for file_a"
        );
    }

    #[test]
    fn code_graph_remove_file_repeated_calls_are_idempotent() {
        let (mut graph, file_a, _file_b, file_a_nodes, _file_b_nodes) = seed_two_file_graph(3);

        // First call does the work.
        let first = graph.remove_file(file_a);
        assert_eq!(first.len(), file_a_nodes.len());

        // Snapshot post-first-call state.
        let nodes_after = graph.nodes().len();
        let delta_fwd_after = graph.edges().stats().forward.delta_edge_count;
        let delta_rev_after = graph.edges().stats().reverse.delta_edge_count;
        let files_after = graph.files().len();

        // Second call must be a silent no-op — the bucket is empty,
        // the file is unregistered, and the arena slots are already
        // tombstoned (NodeArena::remove ignores stale generations).
        let second = graph.remove_file(file_a);
        assert!(
            second.is_empty(),
            "second remove_file on the same file must return an empty Vec"
        );

        assert_eq!(graph.nodes().len(), nodes_after);
        assert_eq!(
            graph.edges().stats().forward.delta_edge_count,
            delta_fwd_after
        );
        assert_eq!(
            graph.edges().stats().reverse.delta_edge_count,
            delta_rev_after
        );
        assert_eq!(graph.files().len(), files_after);
    }
}
