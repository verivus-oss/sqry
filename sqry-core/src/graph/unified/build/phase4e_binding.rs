//! Phase 4e — Binding plane derivation.
//!
//! Runs inside `build_unified_graph_inner` between Phase 4d (bulk edge
//! insert) and Pass 5 (cross-language linking). Consumes `&mut CodeGraph`
//! and populates the scope arena, alias table, and shadow table from the
//! language-local edge set produced by Phase 1 plugins.
//!
//! The whitelisted edge kinds are exactly `Contains`, `Defines`, `Imports`,
//! and `Exports` — Phase 4e must never walk Pass 5's cross-language edges
//! (`FfiCall`, `HttpRequest`, `GrpcCall`, etc.) because Pass 5 hasn't run
//! yet. The invariant is enforced by construction: derivation functions
//! match on specific `EdgeKind` variants and never fall through to a
//! wildcard.
//!
//! # Performance: `BindingEdgeIndex`
//!
//! Phase 4e runs before CSR compaction. At this point all edges live in the
//! delta buffer — a flat `Vec<DeltaEdge>` per file. Calling
//! `BidirectionalEdgeStore::edges_to(node)` or `edges_from(node)` for each
//! of the ~500K scope-worthy nodes would trigger an O(E) delta scan per
//! call, producing O(N × E) total work that manifests as a 20+ minute hang
//! on large codebases (e.g., the Linux kernel with 11M nodes / 18M edges).
//!
//! `build_binding_edge_index` resolves this by scanning the forward delta
//! exactly once — O(E) — to populate three pre-indexed lookup tables:
//!
//! - `contains_parents`: `NodeId` → parent `NodeId` via `Contains` edges
//!   (used by `derive_scopes` Pass 2 to climb the parent chain).
//! - `imports_by_target`: `Import`-node `NodeId` → `Vec<(source, EdgeKind)>`
//!   via `Imports` edges (used by `derive_aliases`).
//! - `defines_contains_by_source`: function/class `NodeId` → `Vec<(target, EdgeKind)>`
//!   via `Defines`/`Contains` edges (used by `derive_shadows`).
//!
//! All three derivers receive `&BindingEdgeIndex` instead of `&CodeGraph`
//! edges, making every parent/child lookup O(1).

use std::collections::HashMap;

use crate::graph::unified::bind::scope::provenance::{
    FileStableId, ScopeProvenance, ScopeProvenanceStore, compute_scope_stable_id,
};
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::edge::kind::EdgeKind;
use crate::graph::unified::mutation_target::GraphMutationTarget;
use crate::graph::unified::node::id::NodeId;

use super::super::bind::alias::derive_aliases;
use super::super::bind::scope::derive::derive_scopes;
use super::super::bind::shadow::derive_shadows;

/// Pre-indexed edge tables built from a single O(E) delta scan.
///
/// Eliminates the per-node O(E) `edges_to()` / `edges_from()` calls that
/// caused Phase 4e to degrade to O(N × E) on large graphs. All three
/// binding-plane derivers — `derive_scopes`, `derive_aliases`,
/// `derive_shadows` — receive a shared reference to this index.
pub struct BindingEdgeIndex {
    /// `Contains` reverse index: child `NodeId` → parent `NodeId`.
    ///
    /// Populated from `EdgeKind::Contains` edges in the forward delta.
    /// Used by `derive_scopes` Pass 2 to climb the containment chain in
    /// O(depth) rather than O(N × E).
    pub contains_parents: HashMap<NodeId, NodeId>,

    /// `Imports` reverse index: target (Import node) `NodeId` → list of
    /// `(source NodeId, EdgeKind)` tuples.
    ///
    /// Populated from `EdgeKind::Imports { .. }` edges in the forward delta.
    /// Used by `derive_aliases` to replace per-import `edges_to()` calls.
    pub imports_by_target: HashMap<NodeId, Vec<(NodeId, EdgeKind)>>,

    /// Forward index for `Defines` and `Contains` edges: source `NodeId` →
    /// list of `(target NodeId, EdgeKind)` tuples.
    ///
    /// Populated from `EdgeKind::Defines` and `EdgeKind::Contains` edges in
    /// the forward delta. Used by `derive_shadows` to replace per-function
    /// `edges_from()` calls.
    pub defines_contains_by_source: HashMap<NodeId, Vec<(NodeId, EdgeKind)>>,
}

/// Scans the forward delta buffer once — O(E) — and populates all three
/// lookup tables in `BindingEdgeIndex`.
///
/// Only `Add` operations are indexed; `Remove` tombstones are skipped
/// because Phase 4e runs after the final bulk edge insert (Phase 4d) and
/// before any edge removals.
///
/// The CSR is empty at this point (compaction happens later, during
/// `persist_graph`), so the delta is the sole source of truth.
fn build_binding_edge_index<G: GraphMutationTarget>(graph: &G) -> BindingEdgeIndex {
    let mut contains_parents: HashMap<NodeId, NodeId> = HashMap::new();
    let mut imports_by_target: HashMap<NodeId, Vec<(NodeId, EdgeKind)>> = HashMap::new();
    let mut defines_contains_by_source: HashMap<NodeId, Vec<(NodeId, EdgeKind)>> = HashMap::new();

    // Acquire forward store once; hold the read lock for the duration of the scan.
    let forward = graph.edges().forward();

    for edge in forward.delta().iter() {
        // Only Add operations are relevant; Remove tombstones are ignored.
        if !edge.is_add() {
            continue;
        }

        match &edge.kind {
            EdgeKind::Contains => {
                // contains_parents: child → parent
                contains_parents.insert(edge.target, edge.source);

                // Also populate the forward index entry for derive_shadows.
                defines_contains_by_source
                    .entry(edge.source)
                    .or_default()
                    .push((edge.target, EdgeKind::Contains));
            }
            EdgeKind::Defines => {
                // Forward index entry for derive_shadows.
                defines_contains_by_source
                    .entry(edge.source)
                    .or_default()
                    .push((edge.target, EdgeKind::Defines));
            }
            EdgeKind::Imports { alias, is_wildcard } => {
                // imports_by_target: target → list of (source, full EdgeKind)
                imports_by_target.entry(edge.target).or_default().push((
                    edge.source,
                    EdgeKind::Imports {
                        alias: *alias,
                        is_wildcard: *is_wildcard,
                    },
                ));
            }
            // Other edge kinds are not needed by the binding derivers.
            _ => {}
        }
    }

    // Drop the read lock before returning.
    drop(forward);

    BindingEdgeIndex {
        contains_parents,
        imports_by_target,
        defines_contains_by_source,
    }
}

/// Summary counts returned from `derive_binding_plane` for observability.
/// These values are logged but not persisted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BindingDerivationStats {
    /// Number of scope records derived and placed into the scope arena.
    pub scopes: u64,
    /// Number of alias entries derived (populated by P2U04).
    pub aliases: u64,
    /// Number of `Import` nodes whose enclosing scope could not be resolved
    /// during alias derivation. These entries are emitted with
    /// `ScopeId::INVALID` — callers should treat a non-zero value as a signal
    /// that a Phase 1 plugin omitted a `Contains` edge. See
    /// `derive_aliases` in `bind/alias.rs`.
    pub aliases_with_invalid_scope: u64,
    /// Number of shadow entries derived (populated by P2U05).
    pub shadows: u64,
}

/// Runs Phase 4e binding derivation on a finalized `CodeGraph`.
///
/// Derives the scope arena from the `Contains` edges emitted by Phase 1
/// plugins. Stores the result in `CodeGraph::scope_arena`. Also populates
/// the alias table (P2U04) and the shadow table (P2U05). P2U11 will add
/// `ScopeProvenance` stamping and the reverse `NodeId → ScopeId` index.
///
/// Returns a [`BindingDerivationStats`] summary for logging. The function is
/// infallible — all error conditions in scope derivation are invariants that
/// imply a bug in a Phase 1 plugin, and are surfaced via `expect` with
/// descriptive messages rather than propagated as `Result`.
/// Public (external-visible) entry point that preserves the pre-Phase 2
/// `&mut CodeGraph` signature for downstream crates (notably
/// `sqry-test-support` and snapshot-load upconvert). Delegates straight
/// through to [`derive_binding_plane_generic`] with `G = CodeGraph`.
///
/// This shim exists because the `GraphMutationTarget` trait is
/// `pub(crate)` by design: the incremental-rebuild plane must stay
/// daemon-internal. Leaking the trait bound into the public signature
/// of `derive_binding_plane` would require re-exposing the trait, which
/// is the opposite of what Phase 1 established.
pub fn derive_binding_plane(graph: &mut CodeGraph) -> BindingDerivationStats {
    derive_binding_plane_generic(graph)
}

/// Actual implementation, generic over the mutation target. Called by
/// the public shim above and directly by the intra-crate incremental
/// rebuild path (`phase4e_incremental::derive_binding_plane_incremental`
/// and, after Task 4 Step 4 Phase 3, `incremental_rebuild`).
pub(crate) fn derive_binding_plane_generic<G: GraphMutationTarget>(
    graph: &mut G,
) -> BindingDerivationStats {
    let mut stats = BindingDerivationStats::default();

    // Build the edge index once from the forward delta (O(E) scan).
    // All three derivers share this pre-indexed data, eliminating the
    // O(N × E) per-node edge traversals.
    let edge_index = build_binding_edge_index(graph);

    let scope_arena = derive_scopes(graph, &edge_index);
    stats.scopes = u64::try_from(scope_arena.len()).unwrap_or(u64::MAX);
    graph.set_scope_arena(scope_arena);

    // Derive alias table from Imports edges, keyed to enclosing scopes.
    // Must run after set_scope_arena so that derive_aliases can use the
    // scope arena to resolve the enclosing scope of each importing node.
    //
    // The trait-method accessor `scope_arena()` is routed through
    // `GraphMutationTarget` so this call compiles against either
    // `CodeGraph` or `RebuildGraph`. We resolve it through the trait
    // explicitly so the borrow checker sees a shared `&` borrow of
    // `graph` that is compatible with the `&G` accepted by
    // `derive_aliases`.
    let scope_arena_ref = GraphMutationTarget::scope_arena(graph);
    let (alias_table, invalid_scope_count) = derive_aliases(graph, scope_arena_ref, &edge_index);
    stats.aliases = u64::try_from(alias_table.len()).unwrap_or(u64::MAX);
    stats.aliases_with_invalid_scope = invalid_scope_count;
    if invalid_scope_count > 0 {
        log::warn!(
            "Phase 4e: {} Import nodes had no enclosing scope; \
             alias entries emitted with ScopeId::INVALID — \
             check Phase 1 plugins for missing Contains edges",
            invalid_scope_count
        );
    }
    graph.set_alias_table(alias_table);

    // Derive shadow table from Variable/Parameter nodes under Function scopes.
    // Must run after set_scope_arena so that derive_shadows can walk the
    // finalized scope arena to locate enclosing Function scopes.
    let scope_arena_ref = GraphMutationTarget::scope_arena(graph);
    let shadow_table = derive_shadows(graph, scope_arena_ref, &edge_index);
    stats.shadows = u64::try_from(shadow_table.len()).unwrap_or(u64::MAX);
    graph.set_shadow_table(shadow_table);

    // P2U11: stamp ScopeProvenance for every derived scope.
    let fact_epoch = GraphMutationTarget::fact_epoch(graph);
    let mut provenance_store = ScopeProvenanceStore::new();
    provenance_store.resize_to(GraphMutationTarget::scope_arena(graph).slot_count());
    for (scope_id, scope) in GraphMutationTarget::scope_arena(graph).iter() {
        // Resolve the file stable id from the registry's canonical path.
        let file_path = GraphMutationTarget::files(graph)
            .resolve(scope.file)
            .expect("scope file must be registered in the file registry");
        let file_stable = FileStableId::from_registry_path(&file_path);
        // Retrieve the file content hash from Phase 1 provenance.
        // Falls back to all-zero bytes for files registered without a hash
        // (e.g., external files or test fixtures).
        let content_hash = GraphMutationTarget::files(graph)
            .file_provenance(scope.file)
            .map(|v| *v.content_hash)
            .unwrap_or([0u8; 32]);
        let stable =
            compute_scope_stable_id(file_stable, content_hash, scope.kind, scope.byte_span);
        provenance_store.insert(
            scope_id,
            ScopeProvenance {
                first_seen_epoch: fact_epoch,
                last_seen_epoch: fact_epoch,
                file_stable_id: file_stable,
                stable_id: stable,
            },
        );
    }
    log::debug!(
        "Phase 4e: stamped {} scope provenance records (epoch {})",
        provenance_store.len(),
        fact_epoch
    );
    graph.set_scope_provenance_store(provenance_store);

    stats
}

// =====================================================================
// Task 4 Step 4 Phase 2: rebuild-plane coverage for
// `derive_binding_plane` + `derive_binding_plane_incremental`.
//
// The binding-plane deriver composes three leaf helpers
// (`derive_scopes`, `derive_aliases`, `derive_shadows`) and a
// provenance-stamping loop. Migrating it to `<G: GraphMutationTarget>`
// is correct only if the whole composition runs against either
// `CodeGraph` or `RebuildGraph` and produces the same scope/alias/
// shadow/provenance state.
// =====================================================================

#[cfg(all(test, feature = "rebuild-internals"))]
mod phase2_rebuild_tests {
    use std::collections::{HashMap, HashSet};
    use std::path::Path;

    use super::*;
    use crate::graph::unified::bind::alias::{AliasEntry, AliasTable};
    use crate::graph::unified::bind::scope::provenance::ScopeProvenanceStore;
    use crate::graph::unified::bind::scope::{ScopeArena, ScopeId, ScopeKind};
    use crate::graph::unified::bind::shadow::{ShadowEntry, ShadowTable};
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::EdgeKind;
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::NodeEntry;
    use crate::graph::unified::string::id::StringId;

    // ==================================================================
    // Seed shared between the two Phase 2 rebuild-plane tests.
    //
    // The seed is deliberately richer than a trivial Module + Function
    // because the Codex iter-1 reviewer flagged that the original
    // rebuild-path tests only verified scope_arena parity and never
    // inspected alias_table, shadow_table, or scope_provenance_store.
    // A broken `RebuildGraph::set_alias_table` / `set_shadow_table` /
    // `set_scope_provenance_store` implementation would have left the
    // old tests green.
    //
    // To close that gap, the seed now contains:
    //   * a Module containing a Function (Contains edge) — drives scope
    //     derivation and exercises `set_scope_arena` +
    //     `set_scope_provenance_store`.
    //   * an Import node inside the Module (Contains edge Module→Import)
    //     plus an Imports edge (Module→Import) — drives `derive_aliases`
    //     and exercises `set_alias_table`. The Import has both a local
    //     name `baz` and a qualified target `foo::bar`, so the resulting
    //     AliasEntry pins specific from_symbol / to_symbol values we can
    //     assert against.
    //   * two Variable nodes inside the Function (Contains edges
    //     Function→Var) — drives `derive_shadows` and exercises
    //     `set_shadow_table`. Two definitions of the same name at
    //     different byte offsets form a shadow chain we can sanity-check
    //     via `ShadowTable::effective_binding`.
    //
    // Every mutation routes through `GraphMutationTarget` so the seed is
    // identical for the CodeGraph baseline and the RebuildGraph under
    // test. A divergence in the RebuildGraph's derived tables therefore
    // has exactly one possible cause: a broken rebuild-local setter /
    // accessor on `impl GraphMutationTarget for RebuildGraph`.
    // ==================================================================

    /// Handles returned from `seed` so the asserting code can recover
    /// specific NodeIds and StringIds to pin expected table contents
    /// against (rather than re-looking-them-up from indices).
    struct SeedHandles {
        _module: NodeId,
        _function: NodeId,
        _import: NodeId,
        _var_outer: NodeId,
        _var_inner: NodeId,
        /// Local alias name (`from_symbol` the AliasTable will carry).
        alias_local_name: StringId,
        /// Qualified target (`to_symbol` the AliasTable will carry).
        alias_target_name: StringId,
        /// Variable symbol whose two defs form a shadow chain.
        shadow_symbol: StringId,
        /// Byte offsets of the two shadow-chain definitions.
        shadow_outer_offset: u32,
        shadow_inner_offset: u32,
    }

    /// Seed either a `CodeGraph` or a `RebuildGraph` with a fixed
    /// Module/Function/Import/Variable layout that exercises
    /// `set_scope_arena`, `set_alias_table`, `set_shadow_table`, and
    /// `set_scope_provenance_store`.
    fn seed<G: GraphMutationTarget>(graph: &mut G, file_suffix: &str) -> SeedHandles {
        let file_path = format!("/virtual/bind_{file_suffix}.rs");
        let file_id = graph
            .files_mut()
            .register(Path::new(&file_path))
            .expect("register test file");

        let mod_name = graph.strings_mut().intern("my_mod").unwrap();
        let fn_name = graph.strings_mut().intern("my_fn").unwrap();
        let import_local = graph.strings_mut().intern("baz").unwrap();
        let import_target = graph.strings_mut().intern("foo::bar").unwrap();
        let shadow_symbol = graph.strings_mut().intern("x").unwrap();

        // --- Module node (root scope) ---
        let mut mod_entry = NodeEntry::new(NodeKind::Module, mod_name, file_id);
        mod_entry.qualified_name = Some(mod_name);
        mod_entry.start_byte = 0;
        mod_entry.end_byte = 200;
        mod_entry.start_line = 1;
        mod_entry.end_line = 20;
        let mod_id = graph.nodes_mut().alloc(mod_entry).unwrap();

        // --- Function node under Module ---
        let mut fn_entry = NodeEntry::new(NodeKind::Function, fn_name, file_id);
        fn_entry.qualified_name = Some(fn_name);
        fn_entry.start_byte = 20;
        fn_entry.end_byte = 150;
        fn_entry.start_line = 2;
        fn_entry.end_line = 15;
        let fn_id = graph.nodes_mut().alloc(fn_entry).unwrap();

        // --- Import node under Module (name = local `baz`, qn =
        // target `foo::bar`). Placed by byte-range strictly outside
        // the function scope so it lands in the Module scope — not
        // the Function scope. ---
        let mut import_entry = NodeEntry::new(NodeKind::Import, import_local, file_id);
        import_entry.qualified_name = Some(import_target);
        import_entry.start_byte = 2;
        import_entry.end_byte = 18;
        import_entry.start_line = 1;
        import_entry.end_line = 1;
        let import_id = graph.nodes_mut().alloc(import_entry).unwrap();

        // --- Two Variable nodes under Function, same name, different
        // byte offsets — form a shadow chain. ---
        let mut var_outer_entry = NodeEntry::new(NodeKind::Variable, shadow_symbol, file_id);
        var_outer_entry.qualified_name = Some(shadow_symbol);
        var_outer_entry.start_byte = 30;
        var_outer_entry.end_byte = 35;
        var_outer_entry.start_line = 3;
        var_outer_entry.end_line = 3;
        let var_outer_id = graph.nodes_mut().alloc(var_outer_entry).unwrap();

        let mut var_inner_entry = NodeEntry::new(NodeKind::Variable, shadow_symbol, file_id);
        var_inner_entry.qualified_name = Some(shadow_symbol);
        var_inner_entry.start_byte = 80;
        var_inner_entry.end_byte = 85;
        var_inner_entry.start_line = 8;
        var_inner_entry.end_line = 8;
        let var_inner_id = graph.nodes_mut().alloc(var_inner_entry).unwrap();

        // Rebuild the by_kind / by_file indices so derive_aliases
        // (which iterates `indices().by_kind(NodeKind::Import)`) and
        // derive_scopes (which iterates per-file node buckets) can
        // see the freshly allocated nodes.
        crate::graph::unified::build::parallel_commit::rebuild_indices(graph);

        // --- Structural edges ---
        // Module contains Function.
        graph
            .edges_mut()
            .add_edge(mod_id, fn_id, EdgeKind::Contains, file_id);
        // Module contains Import (so Import's enclosing scope is the
        // Module scope — derive_aliases will tie the entry to it).
        graph
            .edges_mut()
            .add_edge(mod_id, import_id, EdgeKind::Contains, file_id);
        // Function contains both Variable shadows.
        graph
            .edges_mut()
            .add_edge(mod_id, var_outer_id, EdgeKind::Contains, file_id);
        graph
            .edges_mut()
            .add_edge(fn_id, var_outer_id, EdgeKind::Contains, file_id);
        graph
            .edges_mut()
            .add_edge(fn_id, var_inner_id, EdgeKind::Contains, file_id);

        // --- Imports edge: Module → Import, with alias = `baz`. ---
        // `build_binding_edge_index` keys `imports_by_target` on the
        // Import node, and `derive_aliases` walks that map to emit
        // one AliasEntry per incoming edge.
        graph.edges_mut().add_edge(
            mod_id,
            import_id,
            EdgeKind::Imports {
                alias: Some(import_local),
                is_wildcard: false,
            },
            file_id,
        );

        SeedHandles {
            _module: mod_id,
            _function: fn_id,
            _import: import_id,
            _var_outer: var_outer_id,
            _var_inner: var_inner_id,
            alias_local_name: import_local,
            alias_target_name: import_target,
            shadow_symbol,
            shadow_outer_offset: 30,
            shadow_inner_offset: 80,
        }
    }

    // -----------------------------------------------------------------
    // Semantic-equivalence helpers. These compare two tables produced
    // from identical seed inputs across CodeGraph vs RebuildGraph. A
    // simple `len() == len()` check would not catch a setter that
    // stored the wrong table object or that silently no-op'd; these
    // helpers project every table down to a value-equal set keyed on
    // the fields derive_* actually populates — **including scope**.
    //
    // Codex iter-2 block (2026-04-17) flagged the earlier projections
    // for dropping `AliasEntry.scope` / `ShadowEntry.scope`. Those
    // fields are semantic payload, not slot-metadata: a rebuild-local
    // setter bug that preserves every other field but corrupts only
    // the `scope` ID would have passed the iter-1 projection. To close
    // that gap the projections now include scope — but via its
    // **intrinsic identity** (`kind`, `byte_span`, `file`, `parent`
    // resolved through the scope arena), not the raw `ScopeId` slot
    // handle. This keeps the comparison stable across CodeGraph vs
    // RebuildGraph even if a future change reorders scope allocation,
    // while still catching a bug that stamps the wrong scope onto an
    // entry: a wrong scope resolves to a different intrinsic identity,
    // which surfaces as a projection inequality.
    // -----------------------------------------------------------------

    /// Intrinsic, arena-independent identity of a scope. Used to
    /// project `AliasEntry.scope` and `ShadowEntry.scope` into a form
    /// that compares meaningfully across a CodeGraph arena and a
    /// RebuildGraph arena. Unlike a raw `ScopeId`, this identity is
    /// determined by the seed alone — not by slot-allocation order —
    /// so both derivation paths must agree on every field when seeded
    /// identically. A setter that silently stamps `ScopeId::INVALID`
    /// or some other wrong scope onto an entry will project into
    /// `ScopeIdentity::Invalid` (or a different `Live` tuple), causing
    /// the assertion to fail. See the
    /// `scope_corruption_in_alias_table_is_caught_by_phase4e_tests`
    /// meta-test that exercises exactly this failure mode.
    /// Tag discriminating the three possible resolution outcomes for
    /// a `ScopeId` against a `ScopeArena`. Laid out so the tag is the
    /// first field of the projected tuple (see `ScopeIdentity`) and
    /// compares cleanly under `PartialOrd`/`Ord` derivation.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Ord, PartialOrd, Hash)]
    enum ScopeIdentityTag {
        /// `ScopeId::INVALID` — distinct from every `Live` projection.
        Invalid = 0,
        /// A `ScopeId` whose slot+generation does not resolve in the
        /// arena we were handed (stale handle).
        Stale = 1,
        /// Handle resolves to a live arena slot. The `kind_disc`,
        /// `byte_span`, and `file_index` tuple carries the intrinsic
        /// identity.
        Live = 2,
    }

    /// Intrinsic, arena-independent identity of a scope. Stored as a
    /// plain tuple so the outer projection Vec can `sort()` without
    /// requiring `Ord` on `ScopeKind` (which would add an API commit
    /// to `arena.rs`).
    ///
    /// Fields:
    ///   0. `ScopeIdentityTag` — discriminator between Invalid / Stale
    ///      / Live. Ordered so Invalid < Stale < Live.
    ///   1. `kind_disc: u8` — stable discriminant of the scope's
    ///      `ScopeKind` (populated only for `Live`; `0` otherwise).
    ///   2. `byte_span: (u32, u32)` — scope's source byte range
    ///      (populated only for `Live`; `(0, 0)` otherwise).
    ///   3. `file_index: u32` — `FileId` slot of the scope's owning
    ///      file (populated only for `Live`; `0` otherwise).
    ///
    /// All five `ScopeIdentity` fields must match for two projections
    /// to compare equal, so a setter that stamps `ScopeId::INVALID`
    /// onto an entry projects to `(Invalid, 0, (0, 0), 0)` while the
    /// CodeGraph baseline projects to e.g.
    /// `(Live, 2 /* Module */, (0, 200), 0)`. The tuple `PartialEq`
    /// fires — which is exactly what
    /// `scope_corruption_in_alias_table_is_caught_by_phase4e_tests`
    /// asserts.
    type ScopeIdentity = (ScopeIdentityTag, u8, (u32, u32), u32);

    /// Resolve a `ScopeId` into its arena-independent intrinsic
    /// identity. Returns an `Invalid` tuple for `ScopeId::INVALID`, a
    /// `Stale` tuple for a handle that misses arena lookup, and a
    /// `Live` tuple otherwise.
    ///
    /// The kind discriminant comes from `ScopeKind::discriminant()`
    /// which is stable across compiler versions (scope kinds are
    /// `#[repr(u8)]` with explicit variant values in `arena.rs`).
    fn scope_identity(id: ScopeId, arena: &ScopeArena) -> ScopeIdentity {
        if id.is_invalid() {
            return (ScopeIdentityTag::Invalid, 0, (0, 0), 0);
        }
        match arena.get(id) {
            Some(scope) => (
                ScopeIdentityTag::Live,
                scope.kind.discriminant(),
                scope.byte_span,
                scope.file.index(),
            ),
            None => (ScopeIdentityTag::Stale, 0, (0, 0), 0),
        }
    }

    /// Sorted-vec projection of an `AliasTable` keyed on
    /// `(scope_identity, from_symbol, to_symbol, import_node, is_wildcard)`.
    ///
    /// Unlike iter-1/iter-2's projection, `scope` is **included** —
    /// projected through `scope_identity()` against the owning
    /// graph's scope arena so the comparison is meaningful across
    /// CodeGraph vs RebuildGraph arenas even if slot allocation
    /// diverges in the future. A setter that stamps the wrong scope
    /// (e.g., `ScopeId::INVALID`) onto an entry projects to a
    /// different `ScopeIdentity`, and the main assertion fails — the
    /// rebuild-local mutation bug this test is designed to catch.
    fn project_alias_entries(
        table: &AliasTable,
        arena: &ScopeArena,
    ) -> Vec<(ScopeIdentity, StringId, StringId, NodeId, bool)> {
        let mut v: Vec<_> = table
            .entries()
            .iter()
            .map(|e: &AliasEntry| {
                (
                    scope_identity(e.scope, arena),
                    e.from_symbol,
                    e.to_symbol,
                    e.import_node,
                    e.is_wildcard,
                )
            })
            .collect();
        v.sort();
        v
    }

    /// Sorted-vec projection of a `ShadowTable` keyed on
    /// `(scope_identity, symbol, byte_offset, node)`.
    ///
    /// `scope` is included via `scope_identity()` for the same reason
    /// as in `project_alias_entries` — so a `set_shadow_table` or
    /// `derive_shadows` bug that silently stamps the wrong scope is
    /// caught at the assertion boundary rather than passing a
    /// tautological tuple comparison.
    fn project_shadow_entries(
        table: &ShadowTable,
        arena: &ScopeArena,
    ) -> Vec<(ScopeIdentity, StringId, u32, NodeId)> {
        let mut v: Vec<_> = table
            .entries()
            .iter()
            .map(|e: &ShadowEntry| {
                (
                    scope_identity(e.scope, arena),
                    e.symbol,
                    e.byte_offset,
                    e.node,
                )
            })
            .collect();
        v.sort();
        v
    }

    /// Sorted-vec projection of a `ScopeProvenanceStore` keyed on
    /// `(file_stable_id_bytes, stable_id_bytes)`. The epoch fields
    /// also match but are not part of the dedup key because both
    /// sides read `fact_epoch()` from the same source graph.
    fn project_provenance_stable_ids(store: &ScopeProvenanceStore) -> Vec<([u8; 16], [u8; 16])> {
        let mut v: Vec<_> = store
            .entries()
            .map(|(_, prov)| (*prov.file_stable_id.as_bytes(), prov.stable_id.0))
            .collect();
        v.sort();
        v
    }

    /// Phase 2 rebuild-plane test: run `derive_binding_plane_generic`
    /// against a `CodeGraph` and a `RebuildGraph` seeded with the
    /// same Module + Function + Import + Variable shape, then assert
    /// that the RebuildGraph's rebuild-local `alias_table`,
    /// `shadow_table`, `scope_provenance_store`, and `scope_arena`
    /// are **non-empty** and **semantically equivalent** to the
    /// CodeGraph baseline — not merely same-length or same-as-self.
    ///
    /// This closes Codex iter-1's coverage gap: a broken
    /// `RebuildGraph::set_alias_table` (or any of the three other
    /// rebuild-local setters) would now surface as a failed
    /// projection-equality assertion instead of slipping past a
    /// tautological `rebuild.scope_arena.len() == rb_scope_count`
    /// comparison.
    #[test]
    fn derive_binding_plane_runs_against_rebuild_graph() {
        // === CodeGraph baseline ===
        let mut cg = CodeGraph::new();
        let cg_handles = seed(&mut cg, "cg");
        let cg_stats = derive_binding_plane_generic(&mut cg);

        // Use the inherent public `CodeGraph::*` getters — they do
        // NOT go through `Arc::make_mut`, so we observe the exact
        // tables the setter wrote.
        let cg_scope_count = cg.scope_arena().len();
        let cg_alias_table = cg.alias_table();
        let cg_shadow_table = cg.shadow_table();
        let cg_provenance_store = cg.scope_provenance_store();

        // Baseline sanity: the CodeGraph path MUST have populated
        // all four surfaces. If this fails, the seed is wrong, not
        // the setter wiring.
        assert!(
            cg_scope_count > 0,
            "baseline: CodeGraph scope arena must be non-empty"
        );
        assert!(
            !cg_alias_table.is_empty(),
            "baseline: CodeGraph alias table must be non-empty"
        );
        assert!(
            !cg_shadow_table.is_empty(),
            "baseline: CodeGraph shadow table must be non-empty"
        );
        assert!(
            !cg_provenance_store.is_empty(),
            "baseline: CodeGraph scope provenance store must be non-empty"
        );

        // === RebuildGraph under test ===
        let mut rebuild = {
            let graph = CodeGraph::new();
            graph.clone_for_rebuild()
        };
        let rb_handles = seed(&mut rebuild, "cg"); // same path → same FileStableId
        let rb_stats = derive_binding_plane_generic(&mut rebuild);

        // Sanity: the two seed calls produced the same interned
        // StringIds so alias/shadow checks below can compare them
        // directly. The test allocates StringIds as it seeds, and
        // both paths intern the same literal strings in the same
        // order, so the IDs must agree. If this ever diverges the
        // shadow/alias checks would still work because they go
        // through the rebuild-local interner on both sides — but
        // this `assert_eq!` pin is cheap insurance against silent
        // seed drift.
        assert_eq!(cg_handles.alias_local_name, rb_handles.alias_local_name);
        assert_eq!(cg_handles.alias_target_name, rb_handles.alias_target_name);
        assert_eq!(cg_handles.shadow_symbol, rb_handles.shadow_symbol);

        // === Stats parity (kept from iter-1 — still useful as a
        // quick smoke-test before we dig into table contents). ===
        assert_eq!(
            cg_stats.scopes, rb_stats.scopes,
            "scope-count parity across CodeGraph and RebuildGraph"
        );
        assert_eq!(cg_stats.aliases, rb_stats.aliases);
        assert_eq!(cg_stats.shadows, rb_stats.shadows);

        // === Inspect the rebuild-local fields DIRECTLY (not via the
        // `*_mut` accessors, which would COW a CodeGraph's Arc but
        // which on a RebuildGraph return `&mut self.{field}`). The
        // crate-visible `pub(crate)` fields on `RebuildGraph` give
        // us a read-only handle that cannot be aliased by any
        // setter implementation detail. ===
        let rb_scope_arena = &rebuild.scope_arena;
        let rb_alias_table = &rebuild.alias_table;
        let rb_shadow_table = &rebuild.shadow_table;
        let rb_provenance_store = &rebuild.scope_provenance_store;

        // Non-empty invariants — catches a setter that silently
        // no-op'd (e.g., `fn set_alias_table(&mut self, _: AliasTable) {}`).
        assert!(
            !rb_scope_arena.is_empty(),
            "rebuild.scope_arena must be populated by set_scope_arena"
        );
        assert!(
            !rb_alias_table.is_empty(),
            "rebuild.alias_table must be populated by set_alias_table"
        );
        assert!(
            !rb_shadow_table.is_empty(),
            "rebuild.shadow_table must be populated by set_shadow_table"
        );
        assert!(
            !rb_provenance_store.is_empty(),
            "rebuild.scope_provenance_store must be populated by set_scope_provenance_store"
        );

        // === Scope arena parity (length + kinds). ===
        let rb_scope_count = rb_scope_arena.len();
        assert_eq!(
            cg_scope_count, rb_scope_count,
            "CodeGraph vs RebuildGraph scope arena length parity"
        );

        let cg_scope_kinds: HashSet<ScopeKind> =
            cg.scope_arena().iter().map(|(_, s)| s.kind).collect();
        let rb_scope_kinds: HashSet<ScopeKind> =
            rb_scope_arena.iter().map(|(_, s)| s.kind).collect();
        assert_eq!(
            cg_scope_kinds, rb_scope_kinds,
            "CodeGraph vs RebuildGraph scope arena must contain the same set of scope kinds"
        );
        assert!(rb_scope_kinds.contains(&ScopeKind::Module));
        assert!(rb_scope_kinds.contains(&ScopeKind::Function));

        // === Alias table semantic equivalence ===
        //
        // Projections now include `scope` (via `scope_identity()`
        // against each graph's own arena) in addition to the other
        // four `AliasEntry` fields. Codex iter-2 block: a setter
        // that stamps the wrong scope (e.g., `ScopeId::INVALID`)
        // onto a rebuild-local entry would have passed the iter-1
        // projection that dropped `scope`. The
        // `scope_corruption_in_alias_table_is_caught_by_phase4e_tests`
        // meta-test proves this strengthened projection fires on
        // deliberate scope corruption.
        let cg_alias_proj = project_alias_entries(cg_alias_table, cg.scope_arena());
        let rb_alias_proj = project_alias_entries(rb_alias_table, rb_scope_arena);
        assert_eq!(
            cg_alias_proj, rb_alias_proj,
            "CodeGraph vs RebuildGraph alias_table must contain the \
             same (scope_identity, from_symbol, to_symbol, import_node, \
             is_wildcard) tuples. Divergence here indicates \
             `set_alias_table` / `alias_table_mut` is not wired to the \
             rebuild-local field, OR a `derive_aliases` bug is stamping \
             the wrong scope onto rebuild-local entries."
        );

        // Pin specific expected content: `baz` → `foo::bar`, non-wildcard.
        let has_expected_alias = rb_alias_table.entries().iter().any(|e| {
            e.from_symbol == rb_handles.alias_local_name
                && e.to_symbol == rb_handles.alias_target_name
                && !e.is_wildcard
        });
        assert!(
            has_expected_alias,
            "rebuild alias table must contain the expected entry \
             (from=`baz`, to=`foo::bar`, wildcard=false)"
        );

        // === Shadow table semantic equivalence ===
        //
        // Same `scope_identity`-inclusive projection contract as the
        // alias-table check above — see the iter-2-fix comment there.
        let cg_shadow_proj = project_shadow_entries(cg_shadow_table, cg.scope_arena());
        let rb_shadow_proj = project_shadow_entries(rb_shadow_table, rb_scope_arena);
        assert_eq!(
            cg_shadow_proj, rb_shadow_proj,
            "CodeGraph vs RebuildGraph shadow_table must contain the \
             same (scope_identity, symbol, byte_offset, node) tuples. \
             Divergence here indicates `set_shadow_table` / \
             `shadow_table_mut` is not wired to the rebuild-local field, \
             OR a `derive_shadows` bug is stamping the wrong scope onto \
             rebuild-local entries."
        );

        // Pin specific expected content: two entries for the same
        // `x` symbol at the seeded byte offsets.
        let shadow_offsets_for_x: HashSet<u32> = rb_shadow_table
            .entries()
            .iter()
            .filter(|e| e.symbol == rb_handles.shadow_symbol)
            .map(|e| e.byte_offset)
            .collect();
        assert!(
            shadow_offsets_for_x.contains(&rb_handles.shadow_outer_offset),
            "rebuild shadow table must contain outer def of `x` at byte {}",
            rb_handles.shadow_outer_offset
        );
        assert!(
            shadow_offsets_for_x.contains(&rb_handles.shadow_inner_offset),
            "rebuild shadow table must contain inner def of `x` at byte {}",
            rb_handles.shadow_inner_offset
        );

        // === Scope provenance store semantic equivalence ===
        let cg_prov_proj = project_provenance_stable_ids(cg_provenance_store);
        let rb_prov_proj = project_provenance_stable_ids(rb_provenance_store);
        assert_eq!(
            cg_prov_proj, rb_prov_proj,
            "CodeGraph vs RebuildGraph scope_provenance_store must stamp \
             the same (file_stable_id, scope_stable_id) pairs. Divergence \
             here indicates `set_scope_provenance_store` is not wired to \
             the rebuild-local field."
        );

        // One provenance record per scope — this cross-checks the
        // arena length against the store length on the RebuildGraph
        // side specifically (NOT tautological because the two
        // fields are independently assigned by separate setters).
        assert_eq!(
            rb_provenance_store.len(),
            rb_scope_count,
            "rebuild: one provenance record per scope"
        );

        // Fact-epoch stamping parity: every provenance record on
        // the RebuildGraph carries the same fact_epoch that was on
        // the RebuildGraph at derivation time. This exercises the
        // fact_epoch() trait accessor on RebuildGraph.
        let rb_fact_epoch = GraphMutationTarget::fact_epoch(&rebuild);
        for (_, prov) in rb_provenance_store.entries() {
            assert_eq!(
                prov.first_seen_epoch, rb_fact_epoch,
                "rebuild provenance first_seen_epoch must equal rebuild.fact_epoch()"
            );
            assert_eq!(
                prov.last_seen_epoch, rb_fact_epoch,
                "rebuild provenance last_seen_epoch must equal rebuild.fact_epoch()"
            );
        }
    }

    /// `derive_binding_plane_incremental_generic` is the intra-crate
    /// wrapper Task 4 Step 4 Phase 3 will call on the rebuild side.
    /// Today it delegates straight to `derive_binding_plane_generic`.
    ///
    /// The test shape mirrors
    /// `derive_binding_plane_runs_against_rebuild_graph` — same seed,
    /// same four-table inspection, same CodeGraph-baseline parity —
    /// so a regression in the setter wiring fails BOTH tests rather
    /// than just the non-incremental one.
    #[test]
    fn derive_binding_plane_incremental_runs_against_rebuild_graph() {
        use crate::graph::unified::build::phase4e_incremental::derive_binding_plane_incremental_generic;

        // --- CodeGraph baseline via the incremental wrapper ---
        let mut cg = CodeGraph::new();
        let _ = seed(&mut cg, "cg_inc");
        let cg_stats =
            crate::graph::unified::build::phase4e_incremental::derive_binding_plane_incremental(
                &mut cg,
            );
        // Projections include scope-identity so a rebuild-local
        // setter bug that corrupts only `scope` fields is caught by
        // the equivalence assertions below (iter-2 fix).
        let cg_alias_proj = project_alias_entries(cg.alias_table(), cg.scope_arena());
        let cg_shadow_proj = project_shadow_entries(cg.shadow_table(), cg.scope_arena());
        let cg_prov_proj = project_provenance_stable_ids(cg.scope_provenance_store());

        // --- RebuildGraph path through the generic incremental wrapper ---
        let mut rebuild = {
            let graph = CodeGraph::new();
            graph.clone_for_rebuild()
        };
        let handles = seed(&mut rebuild, "cg_inc"); // match CodeGraph file path
        let rb_stats = derive_binding_plane_incremental_generic(&mut rebuild);

        assert!(rb_stats.scopes > 0, "incremental deriver populated scopes");
        assert_eq!(
            cg_stats.scopes, rb_stats.scopes,
            "incremental CodeGraph vs RebuildGraph scope-count parity"
        );
        assert_eq!(
            cg_stats.aliases, rb_stats.aliases,
            "incremental alias-count parity"
        );
        assert_eq!(
            cg_stats.shadows, rb_stats.shadows,
            "incremental shadow-count parity"
        );

        // Inspect rebuild-local fields directly.
        let rb_scope_arena = &rebuild.scope_arena;
        let rb_alias_table = &rebuild.alias_table;
        let rb_shadow_table = &rebuild.shadow_table;
        let rb_provenance_store = &rebuild.scope_provenance_store;

        assert!(
            !rb_scope_arena.is_empty(),
            "incremental: rebuild.scope_arena must be populated"
        );
        assert!(
            !rb_alias_table.is_empty(),
            "incremental: rebuild.alias_table must be populated by set_alias_table"
        );
        assert!(
            !rb_shadow_table.is_empty(),
            "incremental: rebuild.shadow_table must be populated by set_shadow_table"
        );
        assert!(
            !rb_provenance_store.is_empty(),
            "incremental: rebuild.scope_provenance_store must be populated \
             by set_scope_provenance_store"
        );

        // Semantic equivalence against the CodeGraph baseline. All
        // three projections now include the full semantic payload of
        // each entry — alias/shadow include `scope` via
        // `scope_identity()` so the assertion below fires on any
        // rebuild-local setter that stamps the wrong scope.
        assert_eq!(
            cg_alias_proj,
            project_alias_entries(rb_alias_table, rb_scope_arena),
            "incremental alias_table semantic equivalence (scope-inclusive)"
        );
        assert_eq!(
            cg_shadow_proj,
            project_shadow_entries(rb_shadow_table, rb_scope_arena),
            "incremental shadow_table semantic equivalence (scope-inclusive)"
        );
        assert_eq!(
            cg_prov_proj,
            project_provenance_stable_ids(rb_provenance_store),
            "incremental scope_provenance_store semantic equivalence"
        );

        // Pin expected content on the rebuild side.
        assert!(
            rb_alias_table.entries().iter().any(|e| {
                e.from_symbol == handles.alias_local_name
                    && e.to_symbol == handles.alias_target_name
                    && !e.is_wildcard
            }),
            "incremental: rebuild alias table has expected `baz` → `foo::bar` entry"
        );
        let shadow_nodes_for_x: HashMap<u32, NodeId> = rb_shadow_table
            .entries()
            .iter()
            .filter(|e| e.symbol == handles.shadow_symbol)
            .map(|e| (e.byte_offset, e.node))
            .collect();
        assert!(
            shadow_nodes_for_x.contains_key(&handles.shadow_outer_offset),
            "incremental: outer `x` def at byte {} present",
            handles.shadow_outer_offset
        );
        assert!(
            shadow_nodes_for_x.contains_key(&handles.shadow_inner_offset),
            "incremental: inner `x` def at byte {} present",
            handles.shadow_inner_offset
        );
    }

    // ==================================================================
    // Broken-setter + scope-corruption regression coverage.
    //
    // Codex iter-1 asked for a test that proves the strengthened
    // assertions ACTUALLY catch a broken setter. Iter-2 raised the
    // bar: the projections dropped `AliasEntry.scope` /
    // `ShadowEntry.scope` even though both fields are semantic
    // payload, so a rebuild-local setter bug that preserved every
    // other field but stamped the wrong scope (e.g.,
    // `ScopeId::INVALID` or a mismatched slot) would still have
    // slipped past the projection-equality assertions.
    //
    // The iter-2 fix is two-part:
    //   1. Projections now include `scope` via `scope_identity()`
    //      (arena-intrinsic form), so scope divergence between the
    //      CodeGraph baseline and the RebuildGraph under test causes
    //      the main `assert_eq!` at the alias/shadow equivalence
    //      checks to fire.
    //   2. A new meta-test —
    //      `scope_corruption_in_alias_table_is_caught_by_phase4e_tests`
    //      — rebuilds a corrupted alias/shadow table where ONE entry
    //      has its `scope` overwritten with `ScopeId::INVALID`, then
    //      re-runs the same projection-equality comparison that the
    //      main tests use and asserts that the comparison now
    //      DIVERGES. That proves the strengthened projection actually
    //      catches the bug class the finding was designed to block.
    //
    // The prior `broken_alias_table_setter_is_caught_by_phase4e_tests`
    // is retained under its original name below so the no-op setter
    // class (tautological, but catches a zeroed table) also remains
    // exercised. Both test functions run in the same
    // `rebuild-internals` feature gate.
    // ==================================================================

    /// Mutates one entry of a sorted `AliasTable` by rewriting its
    /// `scope` to `corrupt_to` and rebuilding the table through the
    /// `Deserialize` pathway (which sorts + reindexes). Used by the
    /// scope-corruption meta-test to produce a table that has the
    /// same other-field content as the derived table but a
    /// deliberately-wrong scope on exactly one entry.
    fn corrupt_alias_scope(table: &AliasTable, corrupt_to: ScopeId) -> AliasTable {
        let mut entries: Vec<AliasEntry> = table.entries().to_vec();
        assert!(
            !entries.is_empty(),
            "corrupt_alias_scope precondition: table must have at least one entry"
        );
        entries[0].scope = corrupt_to;
        // Serialize-then-deserialize the entries Vec through the
        // `AliasTable` serde path: AliasTable serializes as its inner
        // `Vec<AliasEntry>`, and deserializes by rebuilding the
        // `by_scope` index from the entries slice. This gives us a
        // valid (sort+index-consistent) AliasTable with the corrupted
        // scope baked in — no new API surface on AliasTable needed.
        let bytes = postcard::to_allocvec(&entries).expect("serialize alias entries");
        postcard::from_bytes::<AliasTable>(&bytes).expect("deserialize corrupted alias table")
    }

    /// Mutates one entry of a sorted `ShadowTable` the same way
    /// `corrupt_alias_scope` handles aliases. The ShadowTable's
    /// serde impl writes just the entries Vec and rebuilds the
    /// per-`(scope, symbol)` chain index on deserialize, so we go
    /// through that same roundtrip.
    fn corrupt_shadow_scope(table: &ShadowTable, corrupt_to: ScopeId) -> ShadowTable {
        let mut entries: Vec<ShadowEntry> = table.entries().to_vec();
        assert!(
            !entries.is_empty(),
            "corrupt_shadow_scope precondition: table must have at least one entry"
        );
        entries[0].scope = corrupt_to;
        let bytes = postcard::to_allocvec(&entries).expect("serialize shadow entries");
        postcard::from_bytes::<ShadowTable>(&bytes).expect("deserialize corrupted shadow table")
    }

    /// Meta-test proving the strengthened Phase 2 projections catch a
    /// `derive_aliases` / `derive_shadows` / setter bug that corrupts
    /// only the `scope` field of an alias/shadow entry while
    /// preserving every other field.
    ///
    /// Flow:
    ///   1. Derive a RebuildGraph's binding plane against the shared
    ///      seed.
    ///   2. Snapshot the CodeGraph-equivalent projections (taken from
    ///      a parallel CodeGraph run with the same seed).
    ///   3. Install a corrupted alias table on the RebuildGraph where
    ///      entry[0].scope = ScopeId::INVALID.
    ///   4. Re-project the rebuild's alias table and assert the
    ///      projection DIFFERS from the CodeGraph baseline — i.e.,
    ///      the strengthened `assert_eq!(cg_alias_proj, rb_alias_proj)`
    ///      WOULD fire in the real test if the rebuild-local setter
    ///      ever stamped the wrong scope.
    ///   5. Repeat steps 3–4 for the shadow table (corrupt
    ///      entry[0].scope = a different scope, not INVALID, to cover
    ///      both "wrong slot" and "stale/invalid handle" failure
    ///      modes across the two tables).
    ///
    /// If this test ever flips to green (i.e., the corrupted
    /// projection matches the baseline), the Codex iter-2 gap is
    /// re-opened and the strengthened assertions are inert.
    #[test]
    fn scope_corruption_in_alias_table_is_caught_by_phase4e_tests() {
        // --- CodeGraph baseline ---
        let mut cg = CodeGraph::new();
        let _ = seed(&mut cg, "corrupt_baseline");
        let _ = derive_binding_plane_generic(&mut cg);
        let baseline_alias_proj = project_alias_entries(cg.alias_table(), cg.scope_arena());
        let baseline_shadow_proj = project_shadow_entries(cg.shadow_table(), cg.scope_arena());

        // --- RebuildGraph under test ---
        let mut rebuild = {
            let graph = CodeGraph::new();
            graph.clone_for_rebuild()
        };
        let _ = seed(&mut rebuild, "corrupt_baseline");
        let _ = derive_binding_plane_generic(&mut rebuild);

        // Sanity: pre-corruption, the two projections must match (a
        // separate assertion from the main tests, but we re-prove it
        // here so the test is self-contained).
        let clean_alias_proj = project_alias_entries(&rebuild.alias_table, &rebuild.scope_arena);
        let clean_shadow_proj = project_shadow_entries(&rebuild.shadow_table, &rebuild.scope_arena);
        assert_eq!(
            baseline_alias_proj, clean_alias_proj,
            "pre-corruption: alias projections must match across CodeGraph and RebuildGraph"
        );
        assert_eq!(
            baseline_shadow_proj, clean_shadow_proj,
            "pre-corruption: shadow projections must match across CodeGraph and RebuildGraph"
        );
        assert!(
            !rebuild.alias_table.is_empty(),
            "meta-test precondition: rebuild alias table must have ≥1 entry"
        );
        assert!(
            !rebuild.shadow_table.is_empty(),
            "meta-test precondition: rebuild shadow table must have ≥1 entry"
        );

        // === Scope corruption #1: stamp `ScopeId::INVALID` onto the
        // first alias entry. Tests the "Invalid" branch of
        // `ScopeIdentity`. ===
        let corrupted_alias = corrupt_alias_scope(&rebuild.alias_table, ScopeId::INVALID);
        let corrupted_alias_proj = project_alias_entries(&corrupted_alias, &rebuild.scope_arena);
        assert_ne!(
            baseline_alias_proj, corrupted_alias_proj,
            "SCOPE-INVALID corruption on alias_table[0].scope MUST cause the \
             projection to diverge from the CodeGraph baseline. If this ever \
             becomes equal, the Codex iter-2 finding is re-opened — the \
             strengthened `project_alias_entries` is NOT including scope in \
             its tuple, and a real setter bug that stamps INVALID onto a \
             rebuild-local entry would silently pass the main test."
        );

        // === Scope corruption #2: stamp a DIFFERENT live scope (not
        // INVALID) onto the first shadow entry. Tests the "Live with
        // wrong intrinsic identity" branch of `ScopeIdentity`. ===
        //
        // Find a scope in the arena whose intrinsic identity differs
        // from entries[0].scope's identity. Any Function scope when
        // the corrupted entry was in the Module scope (or vice versa)
        // works — the seed deliberately creates both.
        let (first_entry_scope_id, _) = rebuild
            .shadow_table
            .entries()
            .first()
            .map(|e| (e.scope, e.symbol))
            .expect("shadow table has at least one entry");
        let original_identity = scope_identity(first_entry_scope_id, &rebuild.scope_arena);
        let wrong_live_scope: ScopeId = rebuild
            .scope_arena
            .iter()
            .find_map(|(id, _)| {
                let candidate_identity = scope_identity(id, &rebuild.scope_arena);
                (candidate_identity != original_identity).then_some(id)
            })
            .expect("seed must produce ≥2 distinct scopes (Module + Function)");
        assert_ne!(
            scope_identity(first_entry_scope_id, &rebuild.scope_arena),
            scope_identity(wrong_live_scope, &rebuild.scope_arena),
            "corruption precondition: picked a scope with a distinct intrinsic identity"
        );

        let corrupted_shadow = corrupt_shadow_scope(&rebuild.shadow_table, wrong_live_scope);
        let corrupted_shadow_proj = project_shadow_entries(&corrupted_shadow, &rebuild.scope_arena);
        assert_ne!(
            baseline_shadow_proj, corrupted_shadow_proj,
            "WRONG-LIVE-SCOPE corruption on shadow_table[0].scope MUST cause \
             the projection to diverge from the CodeGraph baseline. If this \
             ever becomes equal, the Codex iter-2 finding is re-opened — the \
             strengthened `project_shadow_entries` is NOT including scope in \
             its tuple, and a real setter bug that swaps scopes between \
             entries would silently pass the main test."
        );

        // === Scope corruption #3: stamp a STALE scope (valid shape,
        // but not present in the arena) onto another alias entry.
        // Tests the "Stale" branch of `ScopeIdentity`. Uses a clearly-
        // out-of-range slot index so arena lookup returns None. ===
        let stale_scope = ScopeId::new(u32::MAX - 1, u64::MAX);
        assert!(
            !stale_scope.is_invalid(),
            "stale_scope precondition: distinct from ScopeId::INVALID"
        );
        assert!(
            rebuild.scope_arena.get(stale_scope).is_none(),
            "stale_scope precondition: must NOT resolve in the arena"
        );
        let corrupted_alias_stale = corrupt_alias_scope(&rebuild.alias_table, stale_scope);
        let corrupted_alias_stale_proj =
            project_alias_entries(&corrupted_alias_stale, &rebuild.scope_arena);
        assert_ne!(
            baseline_alias_proj, corrupted_alias_stale_proj,
            "STALE-HANDLE corruption on alias_table[0].scope MUST cause the \
             projection to diverge from the CodeGraph baseline."
        );
    }

    /// Simulates a broken `set_alias_table` / `set_shadow_table` /
    /// `set_scope_provenance_store` by deriving the binding plane,
    /// then clearing each rebuild-local field in turn, and
    /// reasserting the SAME non-empty invariants the passing tests
    /// rely on. Each clear MUST trigger a failure of the same
    /// assertion the real test uses; this test panics iff those
    /// invariants correctly catch the broken state. Retained from
    /// iter-1 — covers the "setter silently no-op'd" failure mode,
    /// complementary to the scope-corruption meta-test above.
    #[test]
    fn broken_alias_table_setter_is_caught_by_phase4e_tests() {
        let mut rebuild = {
            let graph = CodeGraph::new();
            graph.clone_for_rebuild()
        };
        let _ = seed(&mut rebuild, "broken");
        let _ = derive_binding_plane_generic(&mut rebuild);

        // Baseline (the real tests assert all four below).
        assert!(!rebuild.alias_table.is_empty());
        assert!(!rebuild.shadow_table.is_empty());
        assert!(!rebuild.scope_provenance_store.is_empty());
        assert!(!rebuild.scope_arena.is_empty());

        // Simulate broken set_alias_table: wipe the rebuild-local
        // table. A no-op setter would leave it at its constructor
        // default (empty). The real test's
        //   `assert!(!rb_alias_table.is_empty())`
        // must catch this.
        rebuild.alias_table = AliasTable::new();
        assert!(
            rebuild.alias_table.is_empty(),
            "broken-setter simulation: alias_table cleared"
        );

        // Re-derive — this populates it again through set_alias_table.
        let _ = derive_binding_plane_generic(&mut rebuild);
        assert!(
            !rebuild.alias_table.is_empty(),
            "re-deriving after clear must refill alias_table via set_alias_table; \
             if THIS assertion fails in production tests, the Codex iter-1 gap \
             is re-opened — set_alias_table is a no-op"
        );

        // Same for shadow_table.
        rebuild.shadow_table = ShadowTable::new();
        assert!(rebuild.shadow_table.is_empty());
        let _ = derive_binding_plane_generic(&mut rebuild);
        assert!(
            !rebuild.shadow_table.is_empty(),
            "re-deriving after clear must refill shadow_table via set_shadow_table"
        );

        // Same for scope_provenance_store.
        rebuild.scope_provenance_store = ScopeProvenanceStore::new();
        assert!(rebuild.scope_provenance_store.is_empty());
        let _ = derive_binding_plane_generic(&mut rebuild);
        assert!(
            !rebuild.scope_provenance_store.is_empty(),
            "re-deriving after clear must refill scope_provenance_store via \
             set_scope_provenance_store"
        );
    }
}
