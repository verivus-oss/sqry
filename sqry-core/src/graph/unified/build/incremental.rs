//! Incremental updates for the unified graph.
//!
//! This module implements incremental update operations:
//! - File removal: Remove all nodes and edges from a deleted file
//! - Edge addition: Add edges without full rebuild
//! - Node removal: Remove specific nodes and their edges
//! - Reverse-dependency closure over Pass 4 cross-file `Imports` edges
//! - Incremental rebuild entrypoint (reserved; implemented in Task 4)
//!
//! # Overview
//!
//! Incremental updates enable efficient partial modifications to the graph
//! without requiring a full rebuild. This is critical for IDE integration
//! where files change frequently.
//!
//! # Operations
//!
//! - [`remove_file_nodes`]: Remove all nodes from a file (and their edges)
//! - [`add_edge_incremental`]: Add a single edge to the graph
//! - [`remove_node`]: Remove a specific node and its connected edges
//! - [`compute_reverse_dep_closure`]: BFS closure over reverse-import edges
//! - [`incremental_rebuild`]: Full incremental rebuild (Task 4 implementation)
//!
//! # Thread Safety
//!
//! These operations acquire appropriate locks on the graph stores.
//! They are designed to be safe for concurrent read access while
//! performing mutations.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use super::super::concurrent::CodeGraph;
use super::super::edge::EdgeKind;
#[cfg(test)]
use super::super::edge::ResolvedVia;
use super::super::file::FileId;
use super::super::memory::GraphMemorySize;
use super::super::mutation_target::GraphMutationTarget;
use super::super::node::{NodeId, NodeKind};
use super::super::rebuild::rebuild_graph::RebuildGraph;
use super::super::storage::{AuxiliaryIndices, NodeArena, NodeEntry};
use super::super::string::StringId;
use super::cancellation::CancellationToken;
use super::entrypoint::{BuildConfig, ParsedFileOutcome, parse_file};
use super::identity::IdentityIndex;
use super::parallel_commit::{
    GlobalOffsets, phase2_assign_ranges, phase3_parallel_commit, phase4_apply_global_remap,
    phase4c_prime_unify_cross_file_nodes, phase4d_bulk_insert_edges,
    rebuild_indices as generic_rebuild_indices,
};
use super::pass_go_method_set::{GoMethodSetStats, run_go_method_set_satisfaction_generic};
use super::pass3_intra::PendingEdge;
use super::pass4_cross::ExportMap;
use super::pass5_cross_language::{Pass5Stats, link_cross_language_edges_generic};
use super::phase4e_binding::BindingDerivationStats;
use super::phase4e_incremental::derive_binding_plane_incremental_generic;
use crate::graph::error::{GraphBuilderError, GraphResult};
use crate::plugin::PluginManager;

/// Statistics from incremental operations.
#[derive(Debug, Clone, Default)]
pub struct IncrementalStats {
    /// Number of nodes removed.
    pub nodes_removed: usize,
    /// Number of edges removed.
    pub edges_removed: usize,
    /// Number of edges added.
    pub edges_added: usize,
    /// Number of identity index entries removed.
    pub identity_entries_removed: usize,
}

/// Result of file node removal.
#[derive(Debug)]
pub struct FileRemovalResult {
    /// Statistics from the removal.
    pub stats: IncrementalStats,
    /// List of removed node IDs.
    pub removed_nodes: Vec<NodeId>,
}

/// Remove all nodes from a file.
///
/// This function removes all nodes belonging to a specific file,
/// along with their associated edges. It also updates the identity
/// index to remove the file's entries.
///
/// # Arguments
///
/// * `file_id` - The file to remove nodes from
/// * `identity_index` - Identity index to update
/// * `arena` - Node arena (for tombstoning nodes)
/// * `indices` - Auxiliary indices to update
///
/// # Returns
///
/// Statistics about what was removed and list of removed node IDs.
pub fn remove_file_nodes(
    file_id: FileId,
    identity_index: &mut IdentityIndex,
    arena: &mut NodeArena,
    indices: &mut AuxiliaryIndices,
) -> FileRemovalResult {
    let mut stats = IncrementalStats::default();

    // Remove entries from identity index
    let removed_entries = identity_index.remove_file(file_id);
    stats.identity_entries_removed = removed_entries.len();

    // Collect node IDs
    let removed_nodes: Vec<NodeId> = removed_entries.iter().map(|(_, id)| *id).collect();
    stats.nodes_removed = removed_nodes.len();

    // Extract node metadata before removing from arena
    let node_metadata: Vec<_> = removed_nodes
        .iter()
        .filter_map(|&node_id| {
            arena
                .get(node_id)
                .map(|entry| (node_id, entry.kind, entry.name, entry.qualified_name))
        })
        .collect();

    // Remove from auxiliary indices using metadata (O(N*B) performance)
    indices.remove_file_with_info(file_id, node_metadata);

    // Remove nodes from arena after indices are updated
    for &node_id in &removed_nodes {
        let _ = arena.remove(node_id);
    }

    // Note: Edge removal is handled separately through the edge store
    // The cascade module handles edge cleanup when nodes are removed

    FileRemovalResult {
        stats,
        removed_nodes,
    }
}

/// Add a single edge to the graph incrementally.
///
/// This is a lightweight operation for adding edges discovered
/// during incremental analysis (e.g., when a file is re-parsed).
///
/// # Arguments
///
/// * `edge` - The pending edge to add
///
/// # Returns
///
/// Statistics about the addition.
///
/// # Note
///
/// This function prepares the edge for addition but doesn't actually
/// add it to the edge store. The caller should use the edge store's
/// `add_edge` method with the returned `PendingEdge`.
#[must_use]
pub fn add_edge_incremental(
    source: NodeId,
    target: NodeId,
    kind: EdgeKind,
    file: FileId,
) -> (IncrementalStats, PendingEdge) {
    let stats = IncrementalStats {
        edges_added: 1,
        ..Default::default()
    };

    let edge = PendingEdge {
        source,
        target,
        kind,
        file,
        spans: vec![], // Incremental edges don't have span info
    };

    (stats, edge)
}

/// Add multiple edges incrementally.
///
/// Batch version of `add_edge_incremental` for efficiency.
#[must_use]
pub fn add_edges_incremental(edges: &[PendingEdge]) -> IncrementalStats {
    IncrementalStats {
        edges_added: edges.len(),
        ..Default::default()
    }
}

/// Remove a specific node and update indices.
///
/// This function tombstones a node and removes it from the identity index.
/// Edge cleanup should be handled separately through the cascade module.
///
/// # Arguments
///
/// * `node_id` - The node to remove
/// * `identity_key` - Optional identity key for index cleanup
/// * `arena` - Node arena for tombstoning
/// * `identity_index` - Identity index to update
///
/// # Returns
///
/// Statistics about the removal.
pub fn remove_node(
    node_id: NodeId,
    identity_index: &mut IdentityIndex,
    arena: &mut NodeArena,
) -> IncrementalStats {
    let mut stats = IncrementalStats::default();

    if identity_index.remove_node_id(node_id).is_some() {
        stats.identity_entries_removed = 1;
    }

    // Remove the node from arena
    if arena.remove(node_id).is_some() {
        stats.nodes_removed = 1;
    }

    stats
}

/// Batch remove nodes from a file.
///
/// More efficient than calling `remove_node` multiple times.
pub fn remove_nodes_batch(
    node_ids: &[NodeId],
    identity_index: &mut IdentityIndex,
    arena: &mut NodeArena,
) -> IncrementalStats {
    let mut stats = IncrementalStats::default();

    for &node_id in node_ids {
        if identity_index.remove_node_id(node_id).is_some() {
            stats.identity_entries_removed += 1;
        }
        if arena.remove(node_id).is_some() {
            stats.nodes_removed += 1;
        }
    }

    stats
}

/// Specification of an edge to be removed during file deletion.
///
/// Produced in bulk when a file is deleted, so the edge store can drop all
/// edges that reference nodes in the removed file without walking the full
/// edge list. The actual deletion happens through the edge store's bulk
/// `remove_edge` path; this type carries the minimum information needed to
/// target each edge (endpoints, kind, owning file).
///
/// Forward-scaffolding for Task 4 of the 2026-03-19 sqryd daemon plan,
/// which will consume this type from the incremental rebuild engine when
/// compacting tombstoned edges after file removal.
#[derive(Debug, Clone)]
pub struct EdgeToRemove {
    /// Source node.
    pub source: NodeId,
    /// Target node.
    pub target: NodeId,
    /// Edge kind.
    pub kind: EdgeKind,
    /// File the edge belongs to.
    pub file: FileId,
}

/// Compute the transitive reverse-dependency closure for a set of changed
/// files.
///
/// Returns every file that either *is* one of the changed files, or directly
/// or transitively holds a committed cross-file edge into a node owned by
/// one of the changed files (or transitively into a file already in the
/// closure). The closure always contains `changed_files` themselves, even
/// when no dependent exists in the graph — this is the correct starting
/// set for the incremental rebuild engine (Task 4), which must re-parse the
/// changed files regardless of whether anything currently depends on them.
///
/// Closure walking uses [`CodeGraph::reverse_dependency_index`] — widened
/// from [`CodeGraph::reverse_import_index`] per Phase 3e's correctness
/// requirement. Every inter-file live edge (Imports, Calls, References,
/// TypeOf, Inherits, Implements, FfiCall, HttpRequest, and every other
/// cross-file-capable `EdgeKind`) drives closure widening. Termination is
/// guaranteed because the file set is finite and `HashSet::insert` prevents
/// revisits. Files not registered in the current graph are still included
/// in the result (they may correspond to newly-created files whose reverse
/// index is trivially empty).
///
/// # Why Imports-only is insufficient (Phase 3e blocker root cause)
///
/// Phase 3b tombstones every node in the closure via
/// [`RebuildGraph::remove_file`], which in turn tombstones every edge whose
/// source **or** target is in the removed file. A non-closure file holding a
/// `Calls` / `References` / `TypeOf` / cross-language edge into the changed
/// file therefore loses that edge at sub-step 3, and Phase 4c-prime
/// unification only rewrites `PendingEdge` targets for *re-parsed* files —
/// it does not revisit committed edges owned by unchanged files. Without
/// closure widening, the dependent file never re-parses, its pending edge
/// set is empty, and the tombstoned cross-file link is never re-established.
/// Widening past imports is the minimal correctness fix.
///
/// # Complexity
///
/// `O(|closure| × avg_reverse_index_size)`. For a workspace of `F` files
/// with average fan-in `k`, the worst case is `O(F × k)`; in practice the
/// closure remains a small fraction of the workspace because average
/// cross-file fan-in per file is O(log F) to O(sqrt F) in realistic
/// codebases. The widening (Imports → all cross-file kinds) modestly grows
/// the frontier but does not change the asymptotic bound.
///
/// # Examples
///
/// ```rust,ignore
/// // A → B → C cross-file chain (any edge kind): changing C closes over
/// // {A, B, C}.
/// let closure = compute_reverse_dep_closure(&[file_c], &graph);
/// assert_eq!(closure.len(), 3);
/// assert!(closure.contains(&file_a) && closure.contains(&file_b));
/// ```
#[must_use]
pub fn compute_reverse_dep_closure(changed_files: &[FileId], graph: &CodeGraph) -> HashSet<FileId> {
    let mut closure: HashSet<FileId> = changed_files.iter().copied().collect();
    let mut frontier: VecDeque<FileId> = changed_files.iter().copied().collect();
    while let Some(file_id) = frontier.pop_front() {
        for dependent in graph.reverse_dependency_index(file_id) {
            if closure.insert(dependent) {
                frontier.push_back(dependent);
            }
        }
    }
    closure
}

/// Incremental rebuild of a [`CodeGraph`] against a set of changed files.
///
/// Re-parses only the files in `closure` (computed via
/// [`compute_reverse_dep_closure`]) and rebuilds their Pass 1–5 contributions
/// on top of a clone of `current_graph`. Returns a new `CodeGraph` ready to
/// be published via `ArcSwap::store` by the daemon's rebuild dispatcher.
///
/// # Parameters
///
/// * `current_graph` — the last published graph; serves as the clone source
///   and the reverse-import baseline used to compute `closure`.
/// * `changed_files` — absolute paths reported by the file-system watcher.
/// * `closure` — pre-computed reverse-dep closure for `changed_files`. The
///   engine trusts this set: only the files it contains are re-parsed.
/// * `plugins` — plugin manager for re-parsing closure files.
/// * `config` — build configuration (plugin selection, cache settings).
///
/// # Correctness Model
///
/// Per the [sqryd daemon spec][spec] §File Watching → "Incremental rebuild
/// correctness model", the engine:
///
/// 1. Clones `current_graph` (copy-on-write via `Arc` where possible).
/// 2. Removes nodes/edges originating from every file in `closure`.
/// 3. Re-parses closure files (Pass 1 AST → `StagingGraph`).
/// 4. Re-runs Pass 2 enrichment and Pass 3 intra-file edges on closure files.
/// 5. Rebuilds `ExportMap` entries for closure files.
/// 6. Re-runs Pass 4 cross-file linking for closure files.
/// 7. Re-runs Pass 5 cross-language linking if closure files carry
///    FFI/HTTP/gRPC/SQL markers.
/// 8. Rebuilds analysis artefacts (CSR, SCC).
/// 9. Returns the new graph.
///
/// [spec]: ../../../../../../docs/superpowers/specs/2026-03-19-sqryd-daemon-design.md
///
/// # Status — Phase 3e (all 13 sub-steps implemented; no fallback)
///
/// Task 4's implementation landed across Phase 3a (cancellation
/// plumbing), Phase 3b (sub-steps 1–3: acquire `current_graph`, build a
/// [`RebuildGraph`] via [`CodeGraph::clone_for_rebuild`], remove every
/// file in the reverse-dep closure), Phase 3c (sub-steps 4–6: re-parse
/// closure files + Pass 2 range assignment + Pass 3 parallel commit
/// against `rebuild_graph` via the [`GraphMutationTarget`] abstraction),
/// Phase 3d (sub-steps 7–9: ExportMap rebuild + Phase 4a/4b/4c/4c-prime/4d
/// cross-file edge insertion + Pass 5 cross-language linking against
/// `rebuild_graph`), and Phase 3e (sub-steps 10–13: per-pass cancellation
/// polling + [`RebuildGraph::finalize`] + [`GraphMemorySize::heap_bytes`]
/// diagnostic + `CodeGraph` return).
///
/// Phase 3e is the terminal phase: after this commit, the function no
/// longer delegates to [`super::entrypoint::build_unified_graph`] and no
/// longer discards `rebuild_graph`. The finalised [`CodeGraph`] is
/// published directly from the rebuild plane. The §E property-based
/// harness (`sqry-core/tests/incremental_equivalence.rs`) now validates
/// the full incremental pipeline end-to-end: every property-test case
/// compares an incremental rebuild against a baseline full rebuild and
/// asserts semantic equivalence, so a correctness regression inside any
/// Pass 1..=5 / Phase 4a..=4e helper surfaces at harness time rather
/// than at daemon-runtime.
///
/// ## Sub-step 1 — acquire `current_graph`
///
/// In the post-Task-6 daemon world, Step 1 of the plan is
/// `workspace.graph.load_full()` — a single `ArcSwap::load_full` that
/// hands the rebuild task an owned `Arc<CodeGraph>` decoupled from any
/// concurrent publisher. For the direct (daemonless) callers that exist
/// today, accepting `&CodeGraph` in the signature **is** the acquisition
/// step: the caller has already done whatever load operation it needs to
/// perform, and simply passes the borrow through. No code executes at
/// this call-site; the parameter itself is Step 1, and this docstring
/// formalises the mapping so Phase 3c/3d and Task 6 both know where the
/// seam is.
///
/// ## Sub-step 2 — `clone_for_rebuild`
///
/// [`CodeGraph::clone_for_rebuild`] deep-clones the committed graph into
/// a [`RebuildGraph`] that the incremental engine can mutate without
/// interfering with the currently-published graph. The call is preceded
/// and followed by [`CancellationToken::check`] so a dispatcher that
/// supersedes the rebuild between Step 1 and Step 2 sees the cancelled
/// flag and fails fast without doing the deep clone.
///
/// ## Sub-step 3 — `remove_file` per closure member
///
/// The reverse-dep closure is computed via
/// [`compute_reverse_dep_closure`]. Every [`FileId`] in the closure is
/// removed from `rebuild_graph` via [`RebuildGraph::remove_file`]. The
/// cancellation token is polled **at every iteration boundary** plus
/// once more after the loop — so a dispatcher can supersede a rebuild
/// even deep inside the closure-removal phase and the engine will stop
/// within one file. Iteration order is stable (closure members are
/// sorted by raw `FileId` before the loop) so cancellation tests can
/// pick a deterministic point at which to flip the token.
///
/// The [§E property-based equivalence harness][harness] locks in the
/// semantic contract (same inputs → semantically equivalent graphs)
/// between this function and a fresh full rebuild.
///
/// [plan]: ../../../../../../docs/superpowers/plans/2026-03-19-sqryd-daemon.md
/// [harness]: ../../../../../tests/incremental_equivalence.rs
///
/// ## Sub-steps 4–6 — re-parse closure + new files + Pass 2/3 commit
///
/// Sub-step 4 re-parses every closure file against its *current* on-disk
/// state via [`parse_file`] **and** every path in `changed_files` that
/// is not yet registered on `current_graph` — i.e. files the caller
/// added between the previous publish and this rebuild. Phase 3e added
/// the new-file leg because the Phase 3d fallback (`build_unified_graph`
/// against an inferred workspace root) used to discover new files for
/// free via the filesystem walk; without it, any `AddFile` edit would
/// silently vanish from the incremental result and the §E harness
/// would diverge from the baseline. Files that fail to parse (deleted
/// files, I/O errors, plugin mismatches) are silently dropped from the
/// commit plan — the §E harness's `assert_build_errors_equivalent`
/// keeps those drops aligned with the full-build behaviour.
///
/// Sub-step 5 registers the re-parsed files on
/// `rebuild_graph.files_mut()`, computes a [`GlobalOffsets`] anchored
/// at `rebuild_graph.nodes().slot_count()` and the current string
/// offset, and then [`phase2_assign_ranges`] turns those offsets into
/// a deterministic `ChunkCommitPlan`. Node and string ranges are
/// pre-allocated on the rebuild arena and interner before the parallel
/// commit.
///
/// Sub-step 6 drives [`phase3_parallel_commit`] with `&mut rebuild_graph`
/// as the `G: GraphMutationTarget` instance. The commit writes nodes +
/// strings directly into the rebuild plane; per-file [`PendingEdge`]
/// vectors are threaded through [`Phase3cCommitOutput`] so Phase 3d's
/// sub-step 8 Phase 4d bulk edge insert can consume them.
///
/// ## Sub-steps 7–9 — Phase 3d cross-file + cross-language against `rebuild_graph`
///
/// Sub-step 7 scans `rebuild_graph`'s committed arena for exportable
/// [`NodeKind`]s and registers every node under its qualified name in
/// a fresh [`ExportMap`]. This matches the symbol-table the full build
/// implicitly constructs during `phase3_parallel_commit` + Phase 4c-prime
/// unification; having it materialised here makes downstream
/// cross-file resolution explicit in the rebuild plane.
///
/// Sub-step 8 runs the full-build Phase 4a/4b/4c/4c-prime/4d sequence
/// against `rebuild_graph`: global string dedup
/// (`StringInterner::build_dedup_table`), node/edge remap
/// ([`phase4_apply_global_remap`]), auxiliary-index rebuild
/// ([`generic_rebuild_indices`]), cross-file node unification
/// ([`phase4c_prime_unify_cross_file_nodes`]), and finally bulk edge
/// insert via [`phase4d_bulk_insert_edges`]. All five helpers are
/// generic over [`GraphMutationTarget`] (Task 4 Step 4 Phase 1/2), so
/// `G` is inferred as [`RebuildGraph`] here. `per_file_edges` from
/// Phase 3c is consumed by-mutable-reference so the remap + unification
/// passes can rewrite `StringId` and `NodeId` fields in place.
///
/// Sub-step 9 calls [`link_cross_language_edges_generic`] — the
/// `G: GraphMutationTarget` mirror of the public `pass5_cross_language::link_cross_language_edges`
/// shim. Covers FFI declarations → C/C++ functions, HTTP requests →
/// endpoints, and related cross-language edge kinds.
///
/// Between sub-steps 8 and 9 Phase 3e also runs the full-build's Phase
/// 4e binding-plane derivation via
/// [`derive_binding_plane_incremental_generic`]. Phase 3d skipped this
/// because the old fallback ran it inside `build_unified_graph` on a
/// fresh graph; Phase 3e integrates it directly against `rebuild_graph`
/// so alias / shadow / scope tables reflect the combined surviving +
/// re-parsed state before finalize publishes the result.
///
/// ## Sub-steps 10–13 — finalize + `heap_bytes` + return — Phase 3e
///
/// Sub-step 10 is per-pass cancellation polling. Phase 3b/3c/3d already
/// inserted polls at every major pass boundary; Phase 3e tightens them
/// into a uniform contract by adding a poll after Phase 4e binding
/// derivation and one more immediately before [`RebuildGraph::finalize`]
/// so a dispatcher cancelling during finalize's (infallible but not
/// instantaneous) 14-step sweep cannot strand the publish.
///
/// Sub-step 11 consumes `rebuild_graph` via [`RebuildGraph::finalize`].
/// `finalize()` runs the 14-step publish contract that lives in
/// `sqry-core/src/graph/unified/rebuild/rebuild_graph.rs`: interner
/// freeze + string dedup, compaction of every `NodeIdBearing` surface
/// against the drained tombstone set, epoch bump, `CodeGraph`
/// assembly, and the §F.1 bucket bijection + §F.2 tombstone residue
/// debug-assertion checks. After `finalize` returns, `rebuild_graph`
/// is consumed — there is no discard path.
///
/// Sub-step 12 recomputes [`GraphMemorySize::heap_bytes`] on the
/// assembled [`CodeGraph`] and logs it as a diagnostic
/// (`target = "sqry_core::incremental_rebuild"`). Plan line 906
/// specifies that the daemon's `WorkspaceManager` then feeds the
/// value into the admission-control surfaces (`memory_bytes`,
/// `memory_high_water_bytes`); that bookkeeping lands on the daemon
/// side (Task 6) and reads the graph we return here.
///
/// Sub-step 13 returns the assembled [`CodeGraph`]. The daemon's
/// `WorkspaceManager` wraps it in `Arc` and publishes it via
/// `ArcSwap::store`; direct (non-daemon) callers can wrap-or-not at
/// their leisure. The function signature keeps the bare `CodeGraph`
/// return so both call shapes stay trivial.
///
/// # Errors
///
/// Returns [`GraphBuilderError::Cancelled`] if `cancellation` is
/// cancelled at any pass boundary — currently these are:
/// - pre-flight (before sub-step 2, inherited from Phase 3a);
/// - between sub-step 1 and sub-step 2 (before `clone_for_rebuild`);
/// - between sub-step 2 and sub-step 3 (after `clone_for_rebuild`);
/// - at every iteration of the sub-step 3 closure loop;
/// - post sub-step 3 (before sub-step 4 begins);
/// - at every iteration of the sub-step 4 re-parse loop (Phase 3c);
/// - post sub-step 4 (between re-parse and Pass 2 range assignment);
/// - post sub-step 6 (before sub-step 7 begins, Phase 3d);
/// - pre sub-step 7 (before ExportMap rebuild, Phase 3d);
/// - between sub-step 7 and sub-step 8 (after ExportMap, Phase 3d);
/// - between sub-step 8 and sub-step 9 (after Pass 4d, Phase 3d);
/// - post sub-step 9 (after Pass 5, Phase 3d);
/// - between Pass 5 and Phase 4e binding derivation (Phase 3e);
/// - between Phase 4e and `finalize()` (Phase 3e sub-step 10).
///
/// Returns [`GraphBuilderError::Internal`] if:
/// - The rebuild-local `FileRegistry`, `NodeArena`, or `StringInterner`
///   fail to allocate ranges for the re-parsed closure files.
/// - [`RebuildGraph::finalize`] surfaces an internal compaction failure
///   (infallible today; the `Result` exists for future fallible
///   compaction primitives per `rebuild_graph.rs` step 1 contract).
pub fn incremental_rebuild(
    current_graph: &CodeGraph,
    changed_files: &[PathBuf],
    closure: &HashSet<FileId>,
    plugins: &PluginManager,
    config: &BuildConfig,
    cancellation: &CancellationToken,
) -> GraphResult<CodeGraph> {
    // `config` is threaded through for signature parity with the daemon
    // dispatcher (Task 6) — plugin-selection / cache-override overrides
    // flow through `BuildConfig` in the full-build path and will be
    // plumbed into the rebuild pipeline in a follow-up when the daemon
    // exposes them. Today every rebuild inherits the full-build default
    // plugin set via `PluginManager` so the config surface has no
    // observable effect here. Keeping the parameter live at the Phase
    // 3e boundary avoids a churny signature change when Task 6 lands.
    let _ = config;

    // Pre-flight cancellation check. Inherited from Phase 3a: if a
    // dispatcher cancels a rebuild before it even gets scheduled, the
    // request fails fast without touching the graph or paying the
    // clone-for-rebuild cost.
    cancellation.check()?;

    // ------------------------------------------------------------------
    // Sub-step 1 — acquire `current_graph`.
    //
    // No executable code: the caller-provided `&CodeGraph` parameter is
    // Step 1 in the direct-call world. In the daemon world this maps to
    // `workspace.graph.load_full()`. See the function docstring for the
    // full mapping.
    // ------------------------------------------------------------------

    // Cancellation boundary between Step 1 (load_full / parameter) and
    // Step 2 (clone_for_rebuild). A cancelling dispatcher between these
    // two steps would otherwise pay for the deep clone.
    cancellation.check()?;

    // ------------------------------------------------------------------
    // Sub-step 2 — `clone_for_rebuild(&prior)`.
    //
    // Deep-clones the committed graph into a [`RebuildGraph`] that the
    // incremental engine mutates in isolation. In Phase 3b the
    // `RebuildGraph` is consumed by Step 3 (closure removal) and then
    // discarded — Phase 3c/3d will instead drive re-parse + Pass 1–5
    // against it and then call [`RebuildGraph::finalize`].
    // ------------------------------------------------------------------
    let mut rebuild_graph: RebuildGraph = current_graph.clone_for_rebuild();

    // Cancellation boundary between Step 2 and Step 3. A cancelling
    // dispatcher at this point lets us skip the entire closure loop.
    cancellation.check()?;

    // ------------------------------------------------------------------
    // Sub-step 3 — `remove_file(file_id)` per closure member.
    //
    // Closure iteration is ordered by raw `FileId` for determinism — the
    // removal semantics are independent of order (each `remove_file`
    // call is idempotent against a drained or unknown file), but a
    // stable order makes Phase 3b's cancellation tests reproducible:
    // tests that arm a `cancel_after_n_files` hook can rely on "the
    // Nth remove_file call" being a well-defined event.
    // ------------------------------------------------------------------
    let ordered_closure = ordered_closure_file_ids(closure);
    // `iter_index` feeds the
    // `#[cfg(any(test, feature = "rebuild-internals"))]`-gated
    // per-iteration hook below. Production builds drop the value into
    // an explicit `let _ = iter_index;` binding so the variable reads
    // as "used" in every configuration — this avoids both
    // `clippy::unused_enumerate_index` (would fire if truly unused)
    // and `clippy::explicit_counter_loop` (would fire if we
    // hand-rolled a counter), without any warning-suppression
    // attributes on the loop.
    for (iter_index, file_id) in ordered_closure.into_iter().enumerate() {
        // Poll at every iteration boundary so dispatcher cancellation
        // takes effect within one file even for very large closures.
        // This is the Step 3 loop check — distinct from the pre-flight
        // check (line ~440) and the post-clone check (line ~469). Its
        // coverage is load-bearing for the Phase 3b cancellation tests
        // that prove a token cancelled *between* iterations N and N+1
        // short-circuits before the (N+1)th `remove_file`.
        cancellation.check()?;
        let _removed_nodes: Vec<NodeId> = rebuild_graph.remove_file(file_id);

        // Per-iteration observation hook — gated on `test` /
        // `rebuild-internals`. Tests that need to flip the cancellation
        // token *between* `remove_file` calls register a callback here
        // via [`testing::set_phase3b_iter_hook`]. The hook fires
        // exactly once per iteration, immediately *after* the call to
        // `remove_file` completes but *before* the next iteration's
        // `cancellation.check()`. In non-test / non-`rebuild-internals`
        // builds the `let _ = iter_index;` branch drains the enumerate
        // index so no clippy suppression is required.
        #[cfg(any(test, feature = "rebuild-internals"))]
        testing::fire_phase3b_iter_hook(iter_index, file_id, &rebuild_graph);
        #[cfg(not(any(test, feature = "rebuild-internals")))]
        let _ = iter_index;
    }

    // Post-loop cancellation boundary. If the closure was empty we want
    // at least one check between Step 3 and Phase 3c sub-step 4.
    cancellation.check()?;

    // Sub-step 3 observation hook — gated on `test` / `rebuild-internals`.
    // Tests that need to inspect the mid-rebuild `rebuild_graph` +
    // `closure` between sub-step 3 and sub-step 4 register a callback
    // here via [`testing::set_phase3b_post_substep3_hook`].
    // Production builds compile this call out entirely.
    #[cfg(any(test, feature = "rebuild-internals"))]
    testing::fire_phase3b_post_substep3_hook(&rebuild_graph, closure);

    // ------------------------------------------------------------------
    // Sub-steps 4–6 — Phase 3c real body (+ Phase 3e new-file leg).
    //
    // Drive the parse → range-plan → commit pipeline against
    // `rebuild_graph` via the `GraphMutationTarget` abstraction that
    // Phase 1/2 migrated the helpers onto. The re-parse targets the
    // *new* on-disk state of every closure file — not the snapshot
    // that lived in `current_graph`. `rebuild_graph` received the
    // closure removals in sub-step 3, so after this block it contains
    // both the surviving nodes from the non-closure portion of the
    // prior graph AND the freshly-committed nodes for the closure.
    //
    // Phase 3e additionally parses paths from `changed_files` that do
    // not yet correspond to a `FileId` in `current_graph` — i.e. files
    // the caller created between the last publish and this rebuild.
    // Before Phase 3e the fallback `build_unified_graph` call
    // re-discovered these by walking the workspace; without that
    // fallback, Phase 3e must hand them to the parser directly so an
    // `AddFile` edit does not silently vanish from the incremental
    // result.
    // ------------------------------------------------------------------

    let new_file_paths = phase3e_discover_new_file_paths(current_graph, changed_files);
    let reparse_outcome = phase3c_reparse_closure(
        current_graph,
        closure,
        &new_file_paths,
        plugins,
        cancellation,
    )?;

    // Phase 3c post-reparse / pre-commit observation hook — gated on
    // `test` / `rebuild-internals`. Fires IMMEDIATELY after sub-step 4
    // returns and BEFORE the post-reparse cancellation boundary below,
    // so tests can distinguish this boundary from the Phase 3b loop
    // boundary (which never reaches sub-step 4) and from the Phase 3c
    // post-substep6 hook (which only fires after sub-step 6 commits).
    // Production builds compile this call out entirely.
    #[cfg(any(test, feature = "rebuild-internals"))]
    testing::fire_phase3c_post_reparse_hook(reparse_outcome.parsed.len());

    // Post-reparse cancellation boundary (between sub-step 4 and
    // sub-step 5). A cancelling dispatcher at this point lets the
    // pipeline skip every allocation on the rebuild plane.
    cancellation.check()?;
    let commit_output = phase3c_commit_reparsed(&mut rebuild_graph, reparse_outcome)?;
    cancellation.check()?;

    // Split the Phase 3c output into its two halves: diagnostics are
    // observed by the Phase 3c post-substep6 hook (existing contract);
    // per-file edges feed Phase 3d's sub-step 8 Pass 4d bulk insert.
    let Phase3cCommitOutput {
        diagnostics: post_commit,
        mut per_file_edges,
        per_file_metadata,
    } = commit_output;

    // Sub-step 6 observation hook — gated on `test` / `rebuild-internals`.
    // Fires after Phase 3c commits re-parsed closure files into
    // `rebuild_graph`, so tests can snapshot the rebuild plane's node
    // count *before* the Phase 3d sub-steps 7-9 operate on it.
    #[cfg(any(test, feature = "rebuild-internals"))]
    testing::fire_phase3c_post_substep6_hook(&rebuild_graph, &post_commit);
    // Production builds need to acknowledge the Phase 3c diagnostics
    // value to keep the compile graph identical across configurations.
    // The struct carries counters observed by the test-gated hook; in
    // production they are informational-only and discarded.
    #[cfg(not(any(test, feature = "rebuild-internals")))]
    {
        let _ = post_commit;
    }

    // ------------------------------------------------------------------
    // Phase 3d real body — sub-steps 7, 8, 9 against `rebuild_graph`.
    //
    // Phase 3c landed sub-steps 4-6 (re-parse closure + new files +
    // Pass 2 range assignment + Pass 3 parallel commit). Phase 3d
    // extends the rebuild-plane pipeline with:
    //   - Sub-step 7: ExportMap rebuild — scan the committed arena of
    //     `rebuild_graph` and register every exportable node under its
    //     qualified name so downstream cross-file resolution has a
    //     consistent symbol table. Mirrors the effect of the full-build
    //     pipeline's implicit ExportMap (constructed during
    //     `phase3_parallel_commit` + Phase 4c-prime unification).
    //   - Sub-step 8: Pass 4a/4b/4c/4c-prime/4d — string dedup, global
    //     remap, index rebuild, cross-file node unification, and bulk
    //     insert of the per-file `PendingEdge` vectors Phase 3c
    //     collected. Every helper is generic over `GraphMutationTarget`
    //     (Phase 1/2 migrations), so the `G` is inferred as
    //     `RebuildGraph` here.
    //   - Phase 4e binding derivation (Phase 3e addition): the full
    //     build runs this between Phase 4d and Pass 5; Phase 3e does
    //     the same so alias / shadow / scope tables on the rebuild
    //     plane reflect the combined surviving + re-parsed state.
    //   - Sub-step 9: Pass 5 cross-language linking — FFI / HTTP /
    //     similar linkers. Uses `link_cross_language_edges_generic`
    //     with `G = RebuildGraph`.
    //
    // Sub-steps 10-13 follow inline below (finalize + heap_bytes +
    // return). There is no fallback — `rebuild_graph` is consumed by
    // `finalize()` and the assembled `CodeGraph` is returned directly.
    // ------------------------------------------------------------------

    // Sub-step 7 — rebuild the cross-file ExportMap from the committed
    // `rebuild_graph` state. Runs before Phase 4 remaps any strings so
    // the ExportMap captures qualified names as they were written by
    // Phase 3 (the Phase 4a dedup remap is a canonicalisation; the
    // qualified-name strings themselves survive unchanged — dedup
    // replaces duplicate StringIds with a canonical ID for the same
    // underlying str).
    cancellation.check()?;
    let export_map = phase3d_rebuild_export_map(&rebuild_graph);

    // Phase 3d post-ExportMap observation hook — gated on `test` /
    // `rebuild-internals`. Fires after sub-step 7 and before the next
    // cancellation boundary.
    #[cfg(any(test, feature = "rebuild-internals"))]
    testing::fire_phase3d_post_export_map_hook(&rebuild_graph, &export_map);

    // Cancellation boundary between sub-step 7 (ExportMap) and sub-step
    // 8 (Pass 4d bulk edge insert). If a dispatcher cancels here,
    // sub-steps 8 and 9 never run and the rebuild plane is discarded
    // before touching the edge store.
    cancellation.check()?;

    // Sub-step 8 — run Phase 4a dedup + Phase 4b remap + Phase 4c
    // rebuild_indices + Phase 4c-prime cross-file unification +
    // Phase 4d bulk edge insert against `rebuild_graph`, using the
    // per-file edges Phase 3c collected. Preserves the full-build
    // sequence (entrypoint.rs lines 543..=587).
    // The `export_map` value is kept in scope here so downstream
    // Phase 3d observers + future daemon surfaces can consume it; the
    // Phase 3d Phase 4d helper itself does not take it by-parameter
    // (see its docstring).
    let _ = &export_map;
    let pass4d =
        phase3d_insert_cross_file_edges(&mut rebuild_graph, &mut per_file_edges, per_file_metadata);

    // Phase 3d post-Pass-4d observation hook — gated on `test` /
    // `rebuild-internals`. Fires after sub-step 8 commits.
    #[cfg(any(test, feature = "rebuild-internals"))]
    testing::fire_phase3d_post_pass4d_hook(&rebuild_graph, &pass4d);
    #[cfg(not(any(test, feature = "rebuild-internals")))]
    {
        let _ = pass4d;
    }

    // Cancellation boundary between sub-step 8 and Phase 4e binding
    // derivation.
    cancellation.check()?;

    // Phase 4e — binding-plane derivation on the rebuild plane. The
    // full build runs this helper (non-generic) between Phase 4d and
    // Pass 5 so the published graph carries alias / shadow / scope
    // tables consistent with the combined Imports/Exports/Contains/
    // Defines edges. Phase 3d's originally-scheduled sub-steps 7-9
    // skipped this because the old fallback ran it inside
    // `build_unified_graph`. Phase 3e integrates it inline via the
    // `G: GraphMutationTarget` generic so the rebuild plane produces
    // a semantically-complete binding plane before Pass 5 and
    // `finalize()` publish.
    let binding_stats: BindingDerivationStats =
        derive_binding_plane_incremental_generic(&mut rebuild_graph);

    // Cancellation boundary between Phase 4e and the Go T1 method-set
    // satisfaction pass. The pass owns its own tombstone-before-emit
    // step on this plane (02_DESIGN §3.6) so a dispatcher cancelling
    // here skips both the pass and Pass 5.
    cancellation.check()?;

    // Go T1 method-set satisfaction pass on the incremental rebuild
    // plane (Cluster E1 wiring). `changed_files` is `&[PathBuf]` per
    // the function signature; the rebuild plane's `FileRegistry`
    // already has every changed path registered (Phase 3e sub-step 4),
    // so we resolve to `Vec<FileId>` here and pass `Some(slice)` to
    // signal incremental-mode operation. The pass tombstones prior
    // pass-owned nodes / edges whose file is in `changed_file_ids`
    // before emitting the current satisfaction set, ensuring
    // idempotence and orphan removal (02_DESIGN §3.6, AC-12).
    let changed_file_ids: Vec<FileId> = changed_files
        .iter()
        .filter_map(|p| rebuild_graph.files().get(p))
        .collect();
    let go_method_set_stats: GoMethodSetStats =
        run_go_method_set_satisfaction_generic(&mut rebuild_graph, Some(&changed_file_ids));
    log::info!(
        target: "sqry_core::incremental_rebuild",
        "Go method-set: {} value-form Implements, {} pointer-form Implements, \
         {} signature Implements, {} promoted methods, {} shadow Calls/References, \
         changed_go_files={}/{}, elapsed_ms={}",
        go_method_set_stats.implements_edges_value,
        go_method_set_stats.implements_edges_pointer,
        go_method_set_stats.signature_implements_edges,
        go_method_set_stats.promoted_method_nodes,
        go_method_set_stats.promoted_back_reference_edges,
        changed_file_ids.len(),
        changed_files.len(),
        go_method_set_stats.elapsed_ms,
    );

    // Cancellation boundary between the Go T1 pass and Pass 5. A
    // dispatcher cancelling here still pays for sub-steps 7-8 +
    // Phase 4e + Go T1 but skips Pass 5 and the finalize cost.
    cancellation.check()?;

    // Sub-step 9 — Pass 5 cross-language linking against
    // `rebuild_graph`. `link_cross_language_edges_generic` is already
    // generic over `GraphMutationTarget`.
    let pass5_stats: Pass5Stats = link_cross_language_edges_generic(&mut rebuild_graph);

    // Phase 3d post-Pass-5 observation hook — gated on `test` /
    // `rebuild-internals`. Fires after sub-step 9.
    #[cfg(any(test, feature = "rebuild-internals"))]
    testing::fire_phase3d_post_pass5_hook(&rebuild_graph, &pass5_stats);
    #[cfg(not(any(test, feature = "rebuild-internals")))]
    {
        let _ = pass5_stats;
    }

    // ------------------------------------------------------------------
    // Sub-steps 10–13 — Phase 3e publish.
    //
    // Sub-step 10 is the per-pass cancellation polling contract: by
    // the time we reach this block, Phase 3a/3b/3c/3d already installed
    // polls at every major boundary, and the two polls that follow
    // (pre-finalize, plus the finalize cost itself being infallible but
    // not free) tighten the Phase 3e boundary against a dispatcher
    // cancellation.
    //
    // Sub-step 11 consumes `rebuild_graph` via
    // [`RebuildGraph::finalize`]. The 14-step publish contract runs
    // against the rebuild plane and returns the assembled `CodeGraph`.
    // After this point `rebuild_graph` is gone — there is no discard
    // path and no fallback to `build_unified_graph`.
    //
    // Sub-step 12 recomputes [`GraphMemorySize::heap_bytes`] on the
    // assembled graph and logs the value as a diagnostic. The daemon's
    // `WorkspaceManager` (Task 6) will thread the value through
    // Amendment 1 §D accounting once it lands; today the log line is
    // the sole consumer so Phase 3e's diagnostic is observable without
    // daemon wiring.
    //
    // Sub-step 13 returns the assembled `CodeGraph`. Callers (§E
    // harness, the sqryd daemon's rebuild dispatcher) wrap it in `Arc`
    // at the publish site.
    // ------------------------------------------------------------------

    // Post-Pass-5 cancellation boundary (between sub-step 9 and
    // `finalize`). A dispatcher cancelling here skips the 14-step
    // publish sequence entirely.
    cancellation.check()?;

    let finalize_start = Instant::now();
    let code_graph = rebuild_graph.finalize()?;
    let finalize_elapsed = finalize_start.elapsed();

    let heap_bytes = code_graph.heap_bytes();
    log::info!(
        target: "sqry_core::incremental_rebuild",
        "Phase 3e publish: nodes={}, heap_bytes={}, \
         finalize_elapsed_us={}, binding_scopes={}, binding_aliases={}, \
         binding_shadows={}",
        code_graph.node_count(),
        heap_bytes,
        finalize_elapsed.as_micros(),
        binding_stats.scopes,
        binding_stats.aliases,
        binding_stats.shadows,
    );

    // Phase 3e post-finalize observation hook — gated on `test` /
    // `rebuild-internals`. Fires after sub-step 12 recomputed
    // `heap_bytes` on the assembled graph. Tests use this to assert
    // the publish boundary was reached with the expected node count
    // and a non-zero `heap_bytes` estimate, without needing to
    // re-derive those values from the returned graph.
    #[cfg(any(test, feature = "rebuild-internals"))]
    testing::fire_phase3e_post_finalize_hook(&code_graph, heap_bytes, finalize_elapsed);

    Ok(code_graph)
}

/// Phase 3e sub-step 4 helper — collect paths from `changed_files` that
/// are **not** yet registered on `current_graph.files()`.
///
/// These are the files the caller added between the last publish and
/// this rebuild; they are not present in any closure (closure walks
/// are keyed by `FileId`, and new files have no `FileId` yet) and must
/// be parsed directly.
///
/// Handling of non-existent paths: [`parse_file`] gracefully
/// short-circuits to `Err(io::NotFound)` / `ParsedFileOutcome::Skipped`,
/// so a stale entry (e.g. a path removed by a subsequent edit) gets
/// dropped from the commit plan without aborting the rebuild. The §E
/// harness's `assert_build_errors_equivalent` keeps that behaviour
/// aligned with the full-build baseline.
///
/// Output ordering is the caller's `changed_files` input order
/// (de-duplicated). Sub-step 4's iteration order is stable even when
/// the caller passes multiple paths because
/// `phase3c_reparse_closure` sorts its closure-file list by
/// `FileId::index` and appends new-file paths afterwards in the order
/// we return them here — so the commit plan is reproducible across
/// runs for the same input.
fn phase3e_discover_new_file_paths(
    current_graph: &CodeGraph,
    changed_files: &[PathBuf],
) -> Vec<PathBuf> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut new_paths: Vec<PathBuf> = Vec::new();
    for path in changed_files {
        // Paths already registered under either their current spelling
        // or a canonicalised form correspond to files that Phase 3b
        // already tombstoned (closure coverage) and Phase 3c will
        // re-parse via the closure leg — skip them here to avoid
        // double-parsing.
        if current_graph.files().get(path).is_some() {
            continue;
        }
        // De-duplicate the returned vector: two edits may report the
        // same new path (e.g. `AddFile` then `WhitespaceEdit` against
        // the fresh file in the same rebuild batch).
        if !seen.insert(path.clone()) {
            continue;
        }
        new_paths.push(path.clone());
    }
    new_paths
}

/// Result of Phase 3c sub-step 4 — re-parsed staging graphs for every
/// closure file that parsed successfully. Wraps the owned
/// [`ParsedFile`] values plus their on-disk paths so sub-step 5 can
/// register them on the rebuild-local [`FileRegistry`] and reference
/// them by-index in the [`ChunkCommitPlan`].
///
/// `Skipped` / `TimedOut` / `Err` outcomes from [`parse_file`] are
/// dropped silently: the full-build fallback in Phase 3c/3d will
/// rediscover them (or fail to, identically to how it would on a fresh
/// build), and the §E harness proves that is semantically equivalent.
struct ReparseOutcome {
    parsed: Vec<(PathBuf, super::entrypoint::ParsedFile)>,
}

/// Phase 3c sub-step 4 — re-parse every closure file against its
/// current on-disk state.
///
/// Iterates the closure in deterministic `FileId::index` order so the
/// resulting staging vector is stable across runs (important for the
/// §E harness's same-inputs-same-outputs invariant). File paths come
/// from `current_graph.indexed_files()` — that mapping is accurate
/// regardless of whether the file was modified or deleted on disk
/// between the clone_for_rebuild point and now, because the FileId →
/// path mapping in `current_graph` is immutable from this function's
/// perspective (`current_graph` is a shared borrow).
///
/// Cancellation is polled at the TOP of every iteration (before the
/// potentially-expensive `parse_file` call) so a cancelling dispatcher
/// takes effect within one file even for very large closures. This
/// mirrors the per-iteration polling pattern used by Phase 3b's
/// sub-step 3 `remove_file` loop, which matters because `parse_file`
/// is substantially more expensive than `remove_file` and a closure
/// containing hundreds of modified files would otherwise block
/// cancellation for noticeable wall time.
fn phase3c_reparse_closure(
    current_graph: &CodeGraph,
    closure: &HashSet<FileId>,
    new_file_paths: &[PathBuf],
    plugins: &PluginManager,
    cancellation: &CancellationToken,
) -> GraphResult<ReparseOutcome> {
    // Build FileId -> PathBuf lookup from current_graph.indexed_files()
    // in deterministic closure order. Files that left the indexed-set
    // since clone_for_rebuild (shouldn't happen since current_graph is
    // a &borrow, but defensively) are skipped.
    let mut closure_paths: Vec<(FileId, PathBuf)> = current_graph
        .indexed_files()
        .filter_map(|(fid, path)| {
            if closure.contains(&fid) {
                Some((fid, path.to_path_buf()))
            } else {
                None
            }
        })
        .collect();
    // Ordering: by FileId::index, matching sub-step 3's iteration order.
    closure_paths.sort_by_key(|(fid, _)| fid.index());

    // Phase 3e: append new-file paths after the closure leg. These are
    // files the caller added between the last publish and this rebuild
    // (see `phase3e_discover_new_file_paths`). Ordering is the caller's
    // input order (already de-duplicated) — appending after the sorted
    // closure keeps the overall commit plan reproducible for identical
    // inputs across runs.
    let closure_len = closure_paths.len();
    closure_paths.reserve(new_file_paths.len());
    for path in new_file_paths {
        // Use a sentinel FileId since new files do not yet have one;
        // the iteration body does not consume `_fid` below (only `path`
        // is used for `parse_file`), so the sentinel value is never
        // observable.
        closure_paths.push((FileId::new(u32::MAX), path.clone()));
    }
    debug_assert_eq!(
        closure_paths.len(),
        closure_len + new_file_paths.len(),
        "Phase 3e new-file append must not collide with closure leg",
    );

    let mut parsed = Vec::with_capacity(closure_paths.len());
    // `iter_index` feeds the
    // `#[cfg(any(test, feature = "rebuild-internals"))]`-gated
    // per-iteration hook below. Production builds drop the value into
    // an explicit `let _ = iter_index;` binding — that keeps the
    // variable "used" in every configuration, so neither
    // `clippy::unused_enumerate_index` (would fire if the index were
    // truly unused) nor `clippy::explicit_counter_loop` (would fire
    // if we hand-rolled a counter) applies, and no suppression
    // attributes are needed.
    for (iter_index, (_fid, path)) in closure_paths.into_iter().enumerate() {
        // Poll at every iteration boundary so dispatcher cancellation
        // takes effect within one file even for very large closures.
        // This is the Step 4 loop check — distinct from the pre-flight
        // check, the Step 3 loop check, and the post-substep-3 check
        // that all fire strictly before this loop, and also distinct
        // from the post-substep-4 check that fires strictly after.
        cancellation.check()?;
        match parse_file(path.as_path(), plugins) {
            Ok(ParsedFileOutcome::Parsed(pf)) => {
                parsed.push((path, pf));
            }
            Ok(ParsedFileOutcome::Skipped) => {
                // File's plugin disappeared or graph builder is
                // unavailable. Drop it from the commit plan; the
                // full-build fallback handles whatever is needed.
            }
            Ok(ParsedFileOutcome::TimedOut { .. }) => {
                // Parse/build timeout: same treatment as Skipped.
                // Production full-build emits a warning here; we stay
                // quiet to avoid duplicate log noise — the full-build
                // fallback will re-emit the same warning if the file
                // is still problematic.
            }
            Err(err) => {
                // I/O error (deleted file, permission denied, etc.)
                // or a non-timeout build error. Drop from the commit
                // plan. Phase 3d will own the policy decision on
                // whether a closure-file parse error should abort the
                // rebuild or be absorbed; Phase 3c defers to "absorb
                // and let the full-build fallback decide", which
                // matches the existing full-build behaviour.
                log::debug!(
                    "Phase 3c: dropping closure file from commit plan due to \
                     re-parse failure: {err:#}"
                );
            }
        }

        // Per-iteration observation hook — gated on `test` /
        // `rebuild-internals`. Tests that need to flip the cancellation
        // token *between* successful `parse_file` calls register a
        // callback here via [`testing::set_phase3c_iter_hook`]. The
        // hook fires exactly once per iteration, immediately *after*
        // the call to `parse_file` completes but *before* the next
        // iteration's `cancellation.check()`. In non-test /
        // non-`rebuild-internals` builds the `let _ = iter_index;`
        // branch drains the enumerate index so no clippy suppression
        // is required.
        #[cfg(any(test, feature = "rebuild-internals"))]
        testing::fire_phase3c_iter_hook(iter_index);
        #[cfg(not(any(test, feature = "rebuild-internals")))]
        let _ = iter_index;
    }

    Ok(ReparseOutcome { parsed })
}

/// Result of Phase 3c sub-step 6 — diagnostic counters describing what
/// was committed against `rebuild_graph`. Phase 3d will extend this
/// struct with the per-file edge vectors needed for the Phase 4d bulk
/// edge insertion. For now the struct is append-only — every field is
/// a plain counter — so adding fields in Phase 3d does not break the
/// Phase 3c test contract.
///
/// The struct is `pub` because the [`testing::Phase3cPostSubstep6Hook`]
/// callback signature takes a `&PostCommitDiagnostics`, and the hook is
/// exposed to integration tests (gated on
/// `feature = "rebuild-internals"`). External exposure is acceptable
/// here because the `rebuild-internals` feature is already locked to
/// in-tree consumers by CI (`tests/rebuild_internals_whitelist.rs`);
/// other crates cannot enable the feature and therefore cannot rely on
/// the struct's shape in production.
///
/// # Stability — breaking change warning
///
/// The `pub` field layout of `PostCommitDiagnostics` is observed
/// directly by the integration tests in
/// `sqry-core/tests/phase3c_commit.rs` (gated on
/// `feature = "rebuild-internals"`). Removing, renaming, or changing
/// the type of any field is a breaking change for those tests, and
/// Phase 3d / 3e follow-ups that touch the diagnostics surface must
/// update the Phase 3c test suite in the same commit. Adding new
/// fields is non-breaking because every existing test reads fields
/// by name and the `Default` derive covers the zero-initialised case
/// for new counters.
#[derive(Debug, Default, Clone)]
#[cfg_attr(not(any(test, feature = "rebuild-internals")), allow(dead_code))]
pub struct PostCommitDiagnostics {
    /// Number of closure files successfully re-parsed and included in
    /// the commit plan. Zero when the closure was empty OR every file
    /// hit Skipped/TimedOut/Err.
    pub files_committed: usize,
    /// Total nodes the commit plan wrote into `rebuild_graph`'s arena.
    /// Equals `sum(staging.node_count)` across successfully-parsed
    /// closure files.
    pub nodes_committed: usize,
    /// Total strings the commit plan wrote into `rebuild_graph`'s
    /// interner.
    pub strings_committed: usize,
    /// Total `PendingEdge` entries the commit plan produced. These
    /// are currently discarded (Phase 3d consumes them); the count
    /// is exposed so Phase 3c tests can assert the parse → commit
    /// path produced non-zero intra-file edges for multi-node
    /// fixtures.
    pub edges_collected: usize,
}

/// Combined output of Phase 3c sub-steps 5 + 6 — the diagnostic
/// counters exposed through [`PostCommitDiagnostics`] *plus* the raw
/// per-file [`PendingEdge`] vectors that [`phase3_parallel_commit`]
/// returned.
///
/// Phase 3c's original API returned only [`PostCommitDiagnostics`] and
/// discarded the per-file edges — Phase 3d consumes them for the
/// Phase 4d bulk edge insertion against `rebuild_graph`, so the
/// internal `phase3c_commit_reparsed` helper now threads them through
/// this struct instead of dropping them.
///
/// The struct is private to this module: the Phase 3c post-substep6
/// hook continues to receive only the `PostCommitDiagnostics` half, so
/// the hook signature (and with it the `phase3c_commit.rs` integration
/// tests) stays byte-compatible with the Phase 3c contract.
struct Phase3cCommitOutput {
    /// Diagnostic counters exposed through the Phase 3c hook signature.
    diagnostics: PostCommitDiagnostics,
    /// Per-file [`PendingEdge`] vectors collected by
    /// [`phase3_parallel_commit`]. Preserves file-order so Phase 3d's
    /// Phase 4d bulk insert assigns deterministic edge sequence
    /// numbers (file-by-file, edge-by-edge) identical to the full-build
    /// ordering.
    per_file_edges: Vec<Vec<PendingEdge>>,
    /// T3 Cluster B (02_DESIGN §4.3.e Change 5): per-file staging
    /// [`NodeMetadataStore`] collected during the Phase 3c commit loop.
    /// Carried alongside `per_file_edges` so Phase 3d's Phase 4c-prime /
    /// 4d / 4d-prime sequence has the same metadata input the full-build
    /// entrypoint produces. Empty stores are filtered at extraction time
    /// to keep this vector proportional to actually-stamped files.
    per_file_metadata: Vec<(
        FileId,
        crate::graph::unified::storage::metadata::NodeMetadataStore,
    )>,
}

/// Phase 3c sub-steps 5 + 6 — assign ranges on `rebuild_graph` and
/// commit every re-parsed closure file via
/// [`phase3_parallel_commit`].
///
/// Returns a [`Phase3cCommitOutput`] containing the
/// [`PostCommitDiagnostics`] exposed through the hook surface *and* the
/// per-file [`PendingEdge`] vectors collected by
/// [`phase3_parallel_commit`]. Phase 3d consumes the per-file edges for
/// the Phase 4d bulk insertion; Phase 3c's integration tests continue
/// to read diagnostics via the hook as before.
fn phase3c_commit_reparsed(
    rebuild_graph: &mut RebuildGraph,
    outcome: ReparseOutcome,
) -> GraphResult<Phase3cCommitOutput> {
    if outcome.parsed.is_empty() {
        // Empty closure (or every file dropped to Skipped). No-op
        // commit — no allocation, no file registration. Return zero
        // diagnostics AND an empty per-file edge vector so Phase 3d's
        // Phase 4d call is a trivially-empty bulk insert.
        return Ok(Phase3cCommitOutput {
            diagnostics: PostCommitDiagnostics::default(),
            per_file_edges: Vec::new(),
            per_file_metadata: Vec::new(),
        });
    }

    // --- Sub-step 5 — register files + Pass 2 range assignment ---
    //
    // Consume `outcome.parsed` into two owned vectors so that the
    // PathBuf values move exactly once (from the ReparseOutcome into
    // `file_info`) instead of being cloned per-file. The ParsedFile
    // values move into `parsed_files`; the staging graphs are then
    // borrowed out of that vector for `phase3_parallel_commit`.
    let parsed_count = outcome.parsed.len();
    let mut file_info: Vec<(PathBuf, Option<crate::graph::Language>)> =
        Vec::with_capacity(parsed_count);
    let mut parsed_files: Vec<super::entrypoint::ParsedFile> = Vec::with_capacity(parsed_count);
    for (path, pf) in outcome.parsed {
        file_info.push((path, Some(pf.language)));
        parsed_files.push(pf);
    }
    let file_ids = rebuild_graph
        .files_mut()
        .register_batch(&file_info)
        .map_err(|err| GraphBuilderError::Internal {
            reason: format!(
                "Phase 3c sub-step 5 file registration failed on rebuild plane: {err:?}"
            ),
        })?;

    // Anchor the running offsets at the rebuild plane's current
    // arena + interner watermarks so newly-committed slots stack on
    // top of the surviving (post-sub-step-3) data.
    let node_offset = u32::try_from(rebuild_graph.nodes_mut().slot_count()).map_err(|_| {
        GraphBuilderError::Internal {
            reason: "Phase 3c sub-step 5: rebuild arena slot count exceeds u32::MAX".to_string(),
        }
    })?;
    // `alloc_range(0)` on the interner returns the *current* next-slot
    // index without allocating, matching the full-build seed pattern
    // at `build_unified_graph_inner`'s top. Falls back to 1 only for
    // fresh interners (the sentinel at index 0 is reserved).
    let string_offset = rebuild_graph.strings_mut().alloc_range(0).unwrap_or(1);
    let offsets = GlobalOffsets {
        node_offset,
        string_offset,
    };

    let staging_refs: Vec<&super::StagingGraph> =
        parsed_files.iter().map(|pf| &pf.staging).collect();
    let plan = phase2_assign_ranges(&staging_refs, &file_ids, &offsets);

    // Pre-allocate arena + interner ranges on the rebuild plane. Uses
    // the same placeholder entry the full-build path uses (`Other`
    // kind + zero name + zero file — harmless since every slot is
    // overwritten by `phase3_parallel_commit`).
    let placeholder = NodeEntry::new(NodeKind::Other, StringId::new(0), FileId::new(0));
    rebuild_graph
        .nodes_mut()
        .alloc_range(plan.total_nodes, &placeholder)
        .map_err(|err| GraphBuilderError::Internal {
            reason: format!("Phase 3c sub-step 5: alloc_range on rebuild arena failed: {err:?}"),
        })?;
    rebuild_graph
        .strings_mut()
        .alloc_range(plan.total_strings)
        .map_err(|err| GraphBuilderError::Internal {
            reason: format!("Phase 3c sub-step 5: alloc_range on rebuild interner failed: {err}"),
        })?;

    // --- Sub-step 6 — Pass 3 parallel commit against rebuild_graph ---
    //
    // `phase3_parallel_commit` is generic over `G: GraphMutationTarget`
    // (Task 4 Step 4 Phase 1). Here the inferred `G` is `RebuildGraph`.
    // The helper writes nodes/strings into the pre-allocated ranges
    // via `rebuild_graph.nodes_and_strings_mut()` and returns per-file
    // PendingEdge vectors for Phase 4d bulk insertion. Phase 3c
    // discards the per-file edges; Phase 3d will consume them.
    let phase3 = phase3_parallel_commit(&plan, &staging_refs, rebuild_graph);

    // --- Post-commit bookkeeping (mirrors full-build `build_unified_graph_inner`) ---
    //
    // `phase3_parallel_commit` writes nodes/strings into the arena +
    // interner slices but does NOT touch the `FileSegmentTable` or the
    // `FileRegistry::per_file_nodes` bucket map. The full-build pipeline
    // does both AFTER `phase3_parallel_commit` returns (see
    // `entrypoint.rs` around the `Populate FileSegmentTable` /
    // `Populate FileRegistry::per_file_nodes` comments). The rebuild
    // pipeline must perform the same bookkeeping against `rebuild_graph`
    // so:
    //
    //   * `FileSegmentTable` records `(file_id → slot-range)` for every
    //     re-parsed file. Downstream query surfaces (`list_files`,
    //     `list_symbols`, `get_document_symbols`, all MCP / CLI
    //     per-file iterations) expect one segment entry per live file
    //     in the finalized `CodeGraph`.
    //   * `FileRegistry::per_file_nodes` records `(file_id → NodeIds)`
    //     for every node `phase3_parallel_commit` wrote. The §F.1
    //     bucket-bijection invariant at `RebuildGraph::finalize` step 13
    //     (and at every §E harness iteration) compares the committed
    //     arena against this bucket map and panics if a live NodeId is
    //     absent from every bucket. Omitting this loop makes the
    //     finalize call fail the publish invariant check under
    //     `debug_assertions` as soon as a non-empty closure re-parses.
    //
    // Iteration order matches `plan.file_plans`, which is deterministic
    // across runs. `per_file_node_ids[i]` pairs with `plan.file_plans[i]`.
    for fp in &plan.file_plans {
        let start = fp.node_range.start;
        let count = fp.node_range.end.saturating_sub(start);
        rebuild_graph
            .file_segments_mut()
            .record_range(fp.file_id, start, count);
    }
    debug_assert_eq!(
        phase3.per_file_node_ids.len(),
        plan.file_plans.len(),
        "Phase 3c sub-step 6: phase3 per-file node ID vector length must match plan length"
    );
    for (fp, node_ids) in plan.file_plans.iter().zip(phase3.per_file_node_ids.iter()) {
        for nid in node_ids {
            rebuild_graph.files_mut().record_node(fp.file_id, *nid);
        }
    }

    let edges_collected = phase3.per_file_edges.iter().map(Vec::len).sum::<usize>();

    // T3 Cluster B (02_DESIGN §4.3.e Change 5): extract per-file staging
    // metadata BEFORE `parsed_files` is dropped, rekeying staging-local
    // NodeIds to canonical arena NodeIds via `phase3.per_file_node_ids[i]`.
    // The rekey is required because staging.add_node returns staging-local
    // IDs while `CodeGraph::macro_metadata` is keyed under arena IDs;
    // mirrors the full-build entrypoint's chunk-loop block. Empty stores
    // are filtered out so this vector is proportional to actually-stamped
    // files.
    debug_assert_eq!(
        plan.file_plans.len(),
        parsed_files.len(),
        "Phase 3c sub-step 6: parsed-file vector length must match plan length"
    );
    debug_assert_eq!(
        phase3.per_file_node_ids.len(),
        plan.file_plans.len(),
        "Phase 3c sub-step 6: per-file node ID vector length must match plan length \
         for metadata rekey"
    );
    let mut per_file_metadata: Vec<(
        FileId,
        crate::graph::unified::storage::metadata::NodeMetadataStore,
    )> = Vec::new();
    for ((fp, parsed), arena_ids) in plan
        .file_plans
        .iter()
        .zip(parsed_files.iter_mut())
        .zip(phase3.per_file_node_ids.iter())
    {
        let metadata = parsed.staging.take_macro_metadata();
        if metadata.is_empty() {
            continue;
        }
        let rekeyed = super::parallel_commit::rekey_staging_metadata_to_arena(metadata, arena_ids);
        if !rekeyed.is_empty() {
            per_file_metadata.push((fp.file_id, rekeyed));
        }
    }

    Ok(Phase3cCommitOutput {
        diagnostics: PostCommitDiagnostics {
            files_committed: parsed_count,
            nodes_committed: phase3.total_nodes_written,
            strings_committed: phase3.total_strings_written,
            edges_collected,
        },
        per_file_edges: phase3.per_file_edges,
        per_file_metadata,
    })
}

/// Diagnostic counters emitted by Phase 3d sub-step 8 — the combined
/// Phase 4a/4b/4c/4c-prime/4d sequence against `rebuild_graph`.
///
/// The struct is `pub` because the Phase 3d `testing::Phase3dPostPass4dHookGuard`
/// callback signature observes it, and the hook is exposed to
/// integration tests gated on `feature = "rebuild-internals"`. External
/// exposure is acceptable for the same reason [`PostCommitDiagnostics`]
/// is: the `rebuild-internals` feature is locked to in-tree consumers
/// by CI (`tests/rebuild_internals_whitelist.rs`).
///
/// # Stability — breaking change warning
///
/// The `pub` field layout is observed directly by integration tests in
/// `sqry-core/tests/phase3d_cross_file.rs`. Removing, renaming, or
/// changing the type of any field is a breaking change for those
/// tests. Adding new fields is non-breaking because every existing
/// test reads fields by name and the `Default` derive covers the
/// zero-initialised case for new counters.
#[derive(Debug, Default, Clone)]
#[cfg_attr(not(any(test, feature = "rebuild-internals")), allow(dead_code))]
pub struct Pass4dDiagnostics {
    /// Total `PendingEdge` entries submitted to the Phase 4d bulk
    /// insert (sum of per-file vector lengths at entry).
    pub edges_submitted: usize,
    /// Size of the Phase 4a dedup remap table. Zero when every
    /// interned string already had a canonical form.
    pub dedup_remap_size: usize,
    /// Phase 4c-prime: total `(qualified_name, kind)` groups of size
    /// >= 2 examined for unification.
    pub unification_candidate_pairs_examined: usize,
    /// Phase 4c-prime: number of loser nodes merged into winners.
    pub unification_nodes_merged: usize,
    /// Phase 4c-prime: number of `PendingEdge` fields rewritten to
    /// point at the winner node after unification.
    pub unification_edges_rewritten: usize,
    /// Final edge sequence counter emitted by
    /// [`phase4d_bulk_insert_edges`]. Useful for asserting that the
    /// bulk insert advanced the counter by `edges_submitted`.
    pub final_edge_seq: u64,
    /// T3 Cluster B (02_DESIGN §4.3.e Change 7): `true` when Phase
    /// 4d-prime merged at least one per-file staging
    /// `NodeMetadataStore` into `RebuildGraph::macro_metadata`. The
    /// boolean is retained inside this diagnostics struct so the
    /// Phase 3d post-Pass-4d hook (used by `staging_macro_metadata_*`
    /// integration tests) can assert metadata flowed through; it is
    /// intentionally NOT threaded to `SqrydHook::on_publish` because
    /// per-publish `QueryDb::new` is the de-facto invalidator (see
    /// 02_DESIGN §4.3.e "Reindex cache freshness" + §5.3).
    pub staged_metadata_merged: bool,
}

/// Phase 3d sub-step 7 — rebuild the cross-file [`ExportMap`] from the
/// committed `rebuild_graph` state.
///
/// Walks the rebuild plane's [`NodeArena`] slot-by-slot and registers
/// every node whose kind is in [`EXPORTABLE_KINDS`] (defined below)
/// AND whose `qualified_name` resolves to a non-empty string. The
/// resulting map is structurally identical to what the full-build
/// pipeline implicitly produces during
/// `phase3_parallel_commit` + Phase 4c-prime unification — the
/// qualified names on the `NodeEntry` records are authoritative.
///
/// Iteration order is the arena's slot order (by `NodeId::index`);
/// `ExportMap::register` appends to a `Vec<(FileId, NodeId)>` per
/// qualified name, so repeated registrations for the same name record
/// every definition in slot order. That matches the full-build
/// behaviour where repeated `build_export_map` calls over the same
/// symbol-to-node map also preserve insertion order.
///
/// Tombstoned / stale-generation slots are skipped by the arena's
/// `iter()` (it only surfaces live entries), so the ExportMap is free
/// of closure-removal residue even when Phase 3b sub-step 3 drained
/// several closure files.
///
/// # Complexity
///
/// `O(N)` where `N` is the number of live node slots. The constant
/// factor is small: one hashmap insert per exportable node, plus one
/// string-interner resolve per qualified name.
fn phase3d_rebuild_export_map(rebuild_graph: &RebuildGraph) -> ExportMap {
    let mut export_map = ExportMap::new();
    let strings = GraphMutationTarget::strings(rebuild_graph);

    for (node_id, entry) in GraphMutationTarget::nodes(rebuild_graph).iter() {
        if !EXPORTABLE_KINDS.contains(&entry.kind) {
            continue;
        }
        let Some(qn_id) = entry.qualified_name else {
            continue;
        };
        let Some(qn_str) = strings.resolve(qn_id) else {
            // Interner returned None — the qualified-name StringId
            // points at a vacated slot (should not happen post-commit
            // but is defensive). Skip rather than panic: a partially-
            // missing ExportMap entry is preferable to a rebuild
            // abort, and the full-build fallback that runs after
            // Phase 3d will reconstruct the symbol table from scratch
            // if Phase 3e ever stops relying on it.
            continue;
        };
        if qn_str.is_empty() {
            continue;
        }
        export_map.register(qn_str.to_string(), entry.file, node_id);
    }

    export_map
}

/// Node kinds that participate in cross-file symbol resolution.
///
/// Mirrors the union of [`super::helper::CALL_COMPATIBLE_KINDS`]
/// (Function / Method / Macro / Constant / LambdaTarget) plus type-
/// and container-kinds that plugins routinely cross-reference via
/// `Imports` / `References` / `TypeOf` edges. Keeping the list broad
/// makes Phase 3d's ExportMap a superset of whatever Phase 4c-prime
/// would unify, which is the safe direction — it would be worse to
/// miss a genuine cross-file import than to emit an extra benign
/// ExportMap entry.
///
/// The list is an intentional subset of [`NodeKind`]; adding a new
/// `NodeKind` variant without extending this list is a conscious
/// decision that the new kind is *not* cross-file-referenceable
/// (e.g., `Other`, `CallSite`, intra-file-only nodes). Extending the
/// list is free — the ExportMap just registers more entries.
const EXPORTABLE_KINDS: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Macro,
    NodeKind::Constant,
    NodeKind::LambdaTarget,
    NodeKind::Class,
    NodeKind::Interface,
    NodeKind::Trait,
    NodeKind::Struct,
    NodeKind::Enum,
    NodeKind::EnumVariant,
    NodeKind::EnumConstant,
    NodeKind::Type,
    NodeKind::TypeParameter,
    NodeKind::Module,
    NodeKind::JavaModule,
    NodeKind::Variable,
    NodeKind::Property,
    NodeKind::Component,
    NodeKind::Service,
    NodeKind::Resource,
    NodeKind::Endpoint,
    NodeKind::Annotation,
];

/// Phase 3d sub-step 8 — run Phase 4a/4b/4c/4c-prime/4d against
/// `rebuild_graph`, consuming the per-file [`PendingEdge`] vectors
/// Phase 3c collected.
///
/// Exactly mirrors the full-build sequence in
/// `build_unified_graph_inner` (entrypoint.rs lines 543..=587):
///
/// 1. Phase 4a — `StringInterner::build_dedup_table` on the rebuild
///    interner. Returns a `HashMap<StringId, StringId>` mapping
///    duplicate interned strings to their canonical IDs.
/// 2. Phase 4b — [`phase4_apply_global_remap`] rewrites every
///    `NodeEntry` field and every `PendingEdge::kind` field that
///    stores a `StringId`, swapping duplicates for canonical IDs.
///    No-op when the dedup table is empty.
/// 3. Phase 4c — [`generic_rebuild_indices`] rebuilds the
///    [`AuxiliaryIndices`] on the rebuild plane. Needed before
///    Phase 4c-prime because the unification pass reads
///    `by_qualified_name` (though it also has a fallback: the pass
///    collects qn groups via arena iteration + `qualified_name`
///    StringIds, and the rebuilt indices ensure downstream
///    name-resolution surfaces observe only winners after
///    unification).
/// 4. Phase 4c-prime — [`phase4c_prime_unify_cross_file_nodes`] merges
///    per-file stub nodes that share a canonical qualified name +
///    call-compatible kind. Rewrites every `PendingEdge::target`
///    through the internal `NodeRemapTable`. When a merge occurs,
///    runs `rebuild_indices` a second time so the loser slots are
///    invisible to `by_qualified_name` / `by_name` lookups.
/// 5. Phase 4d — [`phase4d_bulk_insert_edges`] converts per-file
///    `PendingEdge` vectors to `DeltaEdge`s with monotonically
///    increasing sequence numbers and calls
///    `BidirectionalEdgeStore::add_edges_bulk_ordered`.
///
/// Returns [`Pass4dDiagnostics`] with counters for the observation
/// hook and the Phase 3d integration tests.
///
/// # `export_map` usage
///
/// The ExportMap built by sub-step 7 is **not** consumed by Phase 4d
/// itself — the full build's cross-file edge resolution happens
/// implicitly through Phase 4c-prime unification (which rewrites
/// pending-edge targets across files sharing a qualified name).
/// Phase 3d's ExportMap is a superset of that unification's effect
/// and gives the rebuild plane an explicit cross-file symbol table
/// that the Phase 3d observation hook and integration tests consume;
/// Phase 3e and Task 6 may later feed the ExportMap into the daemon's
/// workspace manifest or a future cross-file resolver without
/// re-scanning the arena. Because this helper does not consume it,
/// the ExportMap is held by the caller (`incremental_rebuild`) and is
/// passed only to the post-ExportMap observation hook; this function
/// takes no ExportMap parameter.
fn phase3d_insert_cross_file_edges(
    rebuild_graph: &mut RebuildGraph,
    per_file_edges: &mut [Vec<PendingEdge>],
    per_file_metadata: Vec<(
        FileId,
        crate::graph::unified::storage::metadata::NodeMetadataStore,
    )>,
) -> Pass4dDiagnostics {
    let edges_submitted: usize = per_file_edges.iter().map(Vec::len).sum();

    // --- Phase 4a: Global string dedup -----------------------------------
    let string_remap = rebuild_graph.strings_mut().build_dedup_table();
    let dedup_remap_size = string_remap.len();

    // --- Phase 4b: Apply dedup remap to nodes and pending edges ----------
    if !string_remap.is_empty() {
        phase4_apply_global_remap(rebuild_graph.nodes_mut(), per_file_edges, &string_remap);
    }

    // --- Phase 4c: Rebuild auxiliary indices from the finalized arena ----
    // Generic counterpart to `CodeGraph::rebuild_indices`; see
    // `parallel_commit::rebuild_indices`.
    generic_rebuild_indices(rebuild_graph);

    // --- Phase 4c-prime: Cross-file node unification ---------------------
    let (unification, unification_remap) =
        phase4c_prime_unify_cross_file_nodes(rebuild_graph, per_file_edges);
    if unification.nodes_merged > 0 {
        // Rebuild indices after merging so loser slots become
        // name-invisible via `AuxiliaryIndices::build_from_arena`'s
        // `StringId::INVALID` skip. Mirrors entrypoint.rs:576.
        generic_rebuild_indices(rebuild_graph);
    }

    // --- Phase 4d: Bulk insert cross-file + intra-file pending edges ----
    // `phase4d_bulk_insert_edges` is generic over `GraphMutationTarget`
    // (Phase 2), so the `G` is inferred as `RebuildGraph` here. The
    // helper advances the edge store's sequence counter by
    // `edges_submitted` and returns the final seq value.
    let final_edge_seq = phase4d_bulk_insert_edges(rebuild_graph, per_file_edges);

    // --- Phase 4d-prime (T3 Cluster B, 02_DESIGN §4.3.e Changes 4 + 7):
    // propagate per-file staging metadata into `RebuildGraph::macro_metadata`
    // using the Phase 4c-prime `NodeRemapTable` to drop loser-keyed
    // entries first. Same helper as the full-build plane (generic over
    // `GraphMutationTarget`). The returned bool is retained inside this
    // diagnostics struct for the Phase 3d post-Pass-4d hook used by
    // integration tests; it is not threaded to `SqrydHook::on_publish`
    // because per-publish `QueryDb::new` is the de-facto invalidator
    // (see 02_DESIGN §4.3.e "Reindex cache freshness").
    let staged_metadata_merged = super::parallel_commit::phase4d_prime_propagate_staging_metadata(
        rebuild_graph,
        per_file_metadata,
        &unification_remap,
    );

    Pass4dDiagnostics {
        edges_submitted,
        dedup_remap_size,
        unification_candidate_pairs_examined: unification.candidate_pairs_examined,
        unification_nodes_merged: unification.nodes_merged,
        unification_edges_rewritten: unification.edges_rewritten,
        final_edge_seq,
        staged_metadata_merged,
    }
}

/// Materialise the reverse-dep closure as a deterministic `Vec<FileId>`
/// sorted by raw [`FileId::index`]. `HashSet` iteration order is
/// intentionally unspecified, so a stable sort is the minimum-viable
/// contract to make cancellation tests reproducible.
fn ordered_closure_file_ids(closure: &HashSet<FileId>) -> Vec<FileId> {
    let mut ordered: Vec<FileId> = closure.iter().copied().collect();
    ordered.sort_by_key(|fid| fid.index());
    ordered
}

/// Test-only observation hooks for the Phase 3b / Phase 3c incremental
/// rebuild boundaries.
///
/// Gated on `#[cfg(any(test, feature = "rebuild-internals"))]`:
///
/// - Under `#[cfg(test)]` the hooks are compiled into unit tests inside
///   this module.
/// - Under `feature = "rebuild-internals"` they are also exported to
///   integration tests in `sqry-core/tests/`. The feature is already
///   locked to `sqry-daemon` + in-tree integration tests by CI
///   (`tests/rebuild_internals_whitelist.rs`), so no external crate can
///   observe these hooks; exposing them to internal integration tests
///   is the only way to exercise the full parse → commit pipeline on a
///   realistic `RebuildGraph` — unit tests inside `sqry-core` cannot
///   import language plugins without a crate cycle.
/// - Production builds (neither cfg) compile the entire module out.
#[cfg(any(test, feature = "rebuild-internals"))]
pub mod testing {
    use super::{CodeGraph, ExportMap, FileId, HashSet, Pass5Stats, RebuildGraph};
    use std::cell::RefCell;

    /// Callback invoked at the end of sub-step 3 with an immutable
    /// reference to the mid-rebuild [`RebuildGraph`] and the
    /// caller-supplied closure. Tests typically snapshot the
    /// `rebuild_graph.pending_tombstone_count()` and the closure size
    /// to assert that sub-step 3 actually removed every closure
    /// member.
    type Phase3bPostSubstep3Hook = Box<dyn FnMut(&RebuildGraph, &HashSet<FileId>)>;

    thread_local! {
        static PHASE3B_POST_SUBSTEP3_HOOK: RefCell<Option<Phase3bPostSubstep3Hook>>
            = const { RefCell::new(None) };
    }

    /// Install a callback that runs immediately after Phase 3b
    /// sub-step 3 but before the `RebuildGraph` is dropped. Replaces
    /// any previously-installed hook on the same thread. Returns the
    /// prior hook so callers (e.g. `cargo test` harness wrappers) can
    /// restore it if needed — most call sites ignore the return and
    /// rely on [`clear_phase3b_post_substep3_hook`] for cleanup.
    pub fn set_phase3b_post_substep3_hook<F>(hook: F) -> Option<Phase3bPostSubstep3Hook>
    where
        F: FnMut(&RebuildGraph, &HashSet<FileId>) + 'static,
    {
        PHASE3B_POST_SUBSTEP3_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove any currently-installed Phase 3b sub-step 3 hook on the
    /// current thread. Idempotent.
    pub fn clear_phase3b_post_substep3_hook() {
        PHASE3B_POST_SUBSTEP3_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Fire the installed hook (if any) with the current rebuild
    /// graph + closure. Called from
    /// [`super::incremental_rebuild`] at the end of sub-step 3.
    pub(super) fn fire_phase3b_post_substep3_hook(
        rebuild_graph: &RebuildGraph,
        closure: &HashSet<FileId>,
    ) {
        PHASE3B_POST_SUBSTEP3_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(rebuild_graph, closure);
            }
        });
    }

    /// `RAII` guard: installs `hook` on construction and clears it on
    /// drop. Tests should prefer this over raw `set`/`clear` so a
    /// panic mid-test cannot leak a hook into a sibling test on the
    /// same thread.
    pub struct Phase3bHookGuard {
        _sealed: (),
    }

    impl Phase3bHookGuard {
        /// Install `hook` as the thread-local Phase 3b post-substep3
        /// callback, returning a guard that clears it on drop.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(&RebuildGraph, &HashSet<FileId>) + 'static,
        {
            let _previous = set_phase3b_post_substep3_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for Phase3bHookGuard {
        fn drop(&mut self) {
            clear_phase3b_post_substep3_hook();
        }
    }

    // ------------------------------------------------------------------
    // Per-iteration hook (Phase 3b Step 3 loop boundary)
    // ------------------------------------------------------------------
    //
    // Fires once per iteration inside the sub-step 3 loop, immediately
    // *after* the iteration's call to `rebuild_graph.remove_file(file_id)`
    // and *before* the next iteration's `cancellation.check()` call.
    //
    // This is the hook plane required by the Phase 3b nit fix: it lets a
    // test distinguish the Step 3 loop's `check()` at the top of the
    // loop body (the "new" Phase 3b cancellation surface) from the
    // pre-flight `check()` at the top of `incremental_rebuild` (the
    // "inherited" Phase 3a surface). A test installs this hook with a
    // body that cancels the token on a specific iteration index, then
    // asserts that:
    //   - the pre-flight check at entry did NOT fire (pre-flight observed
    //     an un-cancelled token);
    //   - the first N `remove_file` calls completed (hook observed them);
    //   - the (N+1)th iteration short-circuits at the loop-top `check()`
    //     and the function returns `GraphBuilderError::Cancelled` without
    //     firing the post-substep3 hook.

    /// Callback signature for the Phase 3b per-iteration hook. Arguments
    /// are the zero-based iteration index, the `FileId` that was just
    /// handed to `remove_file`, and an immutable reference to the
    /// mid-rebuild `RebuildGraph` so assertions about pending tombstones
    /// between iterations are possible.
    type Phase3bIterHook = Box<dyn FnMut(usize, FileId, &RebuildGraph)>;

    thread_local! {
        static PHASE3B_ITER_HOOK: RefCell<Option<Phase3bIterHook>>
            = const { RefCell::new(None) };
    }

    /// Install a per-iteration callback that fires after each
    /// `remove_file` call inside sub-step 3's loop. Replaces any
    /// previously-installed iter hook on the same thread. Returns the
    /// prior hook for manual restore; most call sites rely on
    /// [`Phase3bIterHookGuard`] instead.
    pub fn set_phase3b_iter_hook<F>(hook: F) -> Option<Phase3bIterHook>
    where
        F: FnMut(usize, FileId, &RebuildGraph) + 'static,
    {
        PHASE3B_ITER_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove any currently-installed Phase 3b iter hook on the current
    /// thread. Idempotent.
    pub fn clear_phase3b_iter_hook() {
        PHASE3B_ITER_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Fire the installed iter hook (if any) with the current iteration
    /// index, the `FileId` just removed, and the mid-rebuild graph.
    /// Called from [`super::incremental_rebuild`] inside the sub-step 3
    /// loop, after `remove_file`.
    pub(super) fn fire_phase3b_iter_hook(
        iter_index: usize,
        file_id: FileId,
        rebuild_graph: &RebuildGraph,
    ) {
        PHASE3B_ITER_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(iter_index, file_id, rebuild_graph);
            }
        });
    }

    /// `RAII` guard for the per-iteration hook. Installs on construction,
    /// clears on drop. Same rationale as [`Phase3bHookGuard`]: a panic
    /// inside a test must not leak state into a sibling test on the
    /// same thread.
    pub struct Phase3bIterHookGuard {
        _sealed: (),
    }

    impl Phase3bIterHookGuard {
        /// Install `hook` as the thread-local Phase 3b per-iteration
        /// callback (fires after every `remove_file` call in the Step
        /// 3 loop), returning a guard that clears it on drop.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(usize, FileId, &RebuildGraph) + 'static,
        {
            let _previous = set_phase3b_iter_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for Phase3bIterHookGuard {
        fn drop(&mut self) {
            clear_phase3b_iter_hook();
        }
    }

    // ------------------------------------------------------------------
    // Phase 3c per-iteration hook (sub-step 4 re-parse loop)
    // ------------------------------------------------------------------
    //
    // Fires once per iteration of the sub-step 4 re-parse loop,
    // immediately *after* the iteration's call to `parse_file`
    // completes (whether it produced a `Parsed`, `Skipped`, `TimedOut`,
    // or `Err` outcome) and *before* the next iteration's
    // `cancellation.check()` call.
    //
    // This hook lets tests distinguish the Phase 3c sub-step 4 loop
    // cancellation boundary from the Phase 3b sub-step 3 loop
    // boundary. A test installs this hook with a body that cancels
    // the token on a specific iteration index, then asserts that:
    //   - the Phase 3b iter hook fired for every Phase 3b iteration
    //     (proving Phase 3b ran cleanly);
    //   - the Phase 3c iter hook fired for iterations 0..=N
    //     (proving sub-step 4 reached those iterations);
    //   - iteration (N+1) short-circuits at the sub-step 4 loop-top
    //     `cancellation.check()` and the function returns
    //     `GraphBuilderError::Cancelled` without firing either the
    //     post-reparse hook or the post-substep6 hook.

    /// Callback signature for the Phase 3c per-iteration hook.
    /// Argument is the zero-based iteration index within the sub-step
    /// 4 re-parse loop.
    type Phase3cIterHook = Box<dyn FnMut(usize)>;

    thread_local! {
        static PHASE3C_ITER_HOOK: RefCell<Option<Phase3cIterHook>>
            = const { RefCell::new(None) };
    }

    /// Install a per-iteration callback that fires after each
    /// `parse_file` call inside Phase 3c sub-step 4's re-parse loop.
    /// Replaces any previously-installed Phase 3c iter hook on the
    /// same thread. Returns the prior hook for manual restore; most
    /// call sites rely on [`Phase3cIterHookGuard`] instead.
    pub fn set_phase3c_iter_hook<F>(hook: F) -> Option<Phase3cIterHook>
    where
        F: FnMut(usize) + 'static,
    {
        PHASE3C_ITER_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove any currently-installed Phase 3c iter hook on the current
    /// thread. Idempotent.
    pub fn clear_phase3c_iter_hook() {
        PHASE3C_ITER_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Fire the installed Phase 3c iter hook (if any) with the current
    /// iteration index. Called from
    /// [`super::phase3c_reparse_closure`] inside the sub-step 4 loop,
    /// after `parse_file`.
    pub(super) fn fire_phase3c_iter_hook(iter_index: usize) {
        PHASE3C_ITER_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(iter_index);
            }
        });
    }

    /// `RAII` guard for the Phase 3c per-iteration hook. Installs on
    /// construction, clears on drop. Same rationale as
    /// [`Phase3bIterHookGuard`]: a panic inside a test must not leak
    /// state into a sibling test on the same thread.
    pub struct Phase3cIterHookGuard {
        _sealed: (),
    }

    impl Phase3cIterHookGuard {
        /// Install `hook` as the thread-local Phase 3c per-iteration
        /// callback (fires after every `parse_file` call in Phase 3c's
        /// sub-step 4 re-parse loop), returning a guard that clears it
        /// on drop.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(usize) + 'static,
        {
            let _previous = set_phase3c_iter_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for Phase3cIterHookGuard {
        fn drop(&mut self) {
            clear_phase3c_iter_hook();
        }
    }

    // ------------------------------------------------------------------
    // Phase 3c post-reparse / pre-commit hook
    // ------------------------------------------------------------------
    //
    // Fires once between sub-step 4 (re-parse closure) and the
    // post-reparse cancellation boundary that gates sub-step 5 (file
    // registration + range assignment). This is the hook plane required
    // by the Codex iter-1 MINOR: it lets a test distinguish the
    // post-reparse / pre-commit cancellation boundary from the Phase
    // 3b loop boundary (which cancels before sub-step 4 ever runs) and
    // from the Phase 3c post-substep6 hook (which fires strictly after
    // sub-step 6 commits). A test installs this hook with a body that
    // cancels the token, then asserts that:
    //   - the pre-flight, post-clone, Phase 3b loop, and post-substep3
    //     cancellation checks all observed an un-cancelled token;
    //   - this hook DID fire (proving sub-step 4 ran to completion);
    //   - the function returns `GraphBuilderError::Cancelled` at the
    //     post-reparse boundary;
    //   - the Phase 3c post-substep6 hook did NOT fire (proving sub-
    //     steps 5 and 6 were skipped).

    /// Callback signature for the Phase 3c post-reparse / pre-commit
    /// hook. Argument is the number of closure files that were
    /// successfully re-parsed (i.e. the length of
    /// `ReparseOutcome::parsed`), which Phase 3d may need when
    /// asserting on pre-commit pipeline state.
    type Phase3cReparseHook = Box<dyn FnMut(usize)>;

    thread_local! {
        static PHASE3C_POST_REPARSE_HOOK: RefCell<Option<Phase3cReparseHook>>
            = const { RefCell::new(None) };
    }

    /// Install a callback that runs after Phase 3c sub-step 4
    /// (`phase3c_reparse_closure`) returns and before the post-reparse
    /// cancellation boundary gates sub-step 5. Replaces any previously-
    /// installed hook on the same thread. Returns the prior hook for
    /// manual restore; most call sites rely on
    /// [`Phase3cReparseHookGuard`] instead.
    pub fn set_phase3c_post_reparse_hook<F>(hook: F) -> Option<Phase3cReparseHook>
    where
        F: FnMut(usize) + 'static,
    {
        PHASE3C_POST_REPARSE_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove any currently-installed Phase 3c post-reparse hook on the
    /// current thread. Idempotent.
    pub fn clear_phase3c_post_reparse_hook() {
        PHASE3C_POST_REPARSE_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Fire the installed Phase 3c post-reparse hook (if any) with the
    /// number of successfully re-parsed closure files. Called from
    /// [`super::incremental_rebuild`] immediately after sub-step 4
    /// returns.
    pub(super) fn fire_phase3c_post_reparse_hook(parsed_count: usize) {
        PHASE3C_POST_REPARSE_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(parsed_count);
            }
        });
    }

    /// `RAII` guard for the Phase 3c post-reparse / pre-commit hook.
    /// Installs on construction, clears on drop.
    pub struct Phase3cReparseHookGuard {
        _sealed: (),
    }

    impl Phase3cReparseHookGuard {
        /// Install `hook` as the thread-local Phase 3c post-reparse /
        /// pre-commit callback (fires once after sub-step 4's
        /// `phase3c_reparse_closure` returns, before the cancellation
        /// boundary that gates sub-step 5), returning a guard that
        /// clears it on drop.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(usize) + 'static,
        {
            let _previous = set_phase3c_post_reparse_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for Phase3cReparseHookGuard {
        fn drop(&mut self) {
            clear_phase3c_post_reparse_hook();
        }
    }

    // ------------------------------------------------------------------
    // Phase 3c post-substep6 hook
    // ------------------------------------------------------------------
    //
    // Fires at the very end of Phase 3c — after sub-step 6's
    // `phase3_parallel_commit` has returned and `PostCommitDiagnostics`
    // are fully populated, but before the `rebuild_graph` is dropped
    // and the Phase 3c/3d fallback runs `build_unified_graph`.
    //
    // Tests use this hook to snapshot `rebuild_graph.nodes().slot_count()`
    // (proving the re-parse commit actually landed nodes in the rebuild
    // arena) and to inspect `PostCommitDiagnostics` (files_committed,
    // nodes_committed, strings_committed, edges_collected) so Phase 3c
    // test assertions can precisely distinguish "re-parse ran and
    // committed N files" from "re-parse was skipped entirely".

    /// Callback signature for the Phase 3c post-substep6 hook. Receives
    /// an immutable reference to the mid-rebuild `RebuildGraph` and the
    /// populated [`super::PostCommitDiagnostics`].
    type Phase3cPostSubstep6Hook = Box<dyn FnMut(&RebuildGraph, &super::PostCommitDiagnostics)>;

    thread_local! {
        static PHASE3C_POST_SUBSTEP6_HOOK: RefCell<Option<Phase3cPostSubstep6Hook>>
            = const { RefCell::new(None) };
    }

    /// Install a callback that runs at the end of Phase 3c sub-step 6,
    /// before `rebuild_graph` is dropped. Replaces any previously-
    /// installed hook on the same thread. Returns the prior hook.
    pub fn set_phase3c_post_substep6_hook<F>(hook: F) -> Option<Phase3cPostSubstep6Hook>
    where
        F: FnMut(&RebuildGraph, &super::PostCommitDiagnostics) + 'static,
    {
        PHASE3C_POST_SUBSTEP6_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove any currently-installed Phase 3c post-substep6 hook on
    /// the current thread. Idempotent.
    pub fn clear_phase3c_post_substep6_hook() {
        PHASE3C_POST_SUBSTEP6_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Fire the installed Phase 3c post-substep6 hook (if any). Called
    /// from [`super::incremental_rebuild`] at the end of sub-step 6.
    pub(super) fn fire_phase3c_post_substep6_hook(
        rebuild_graph: &RebuildGraph,
        diagnostics: &super::PostCommitDiagnostics,
    ) {
        PHASE3C_POST_SUBSTEP6_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(rebuild_graph, diagnostics);
            }
        });
    }

    /// `RAII` guard for the Phase 3c post-substep6 hook. Installs on
    /// construction, clears on drop.
    pub struct Phase3cHookGuard {
        _sealed: (),
    }

    impl Phase3cHookGuard {
        /// Install `hook` as the thread-local Phase 3c post-substep6
        /// callback (fires after the re-parse → Pass 2 → Pass 3 pipeline
        /// commits re-parsed closure files into `rebuild_graph`),
        /// returning a guard that clears it on drop.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(&RebuildGraph, &super::PostCommitDiagnostics) + 'static,
        {
            let _previous = set_phase3c_post_substep6_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for Phase3cHookGuard {
        fn drop(&mut self) {
            clear_phase3c_post_substep6_hook();
        }
    }

    // Hold onto `CodeGraph` in the module import list so a future
    // extension of the hook that needs the pre-rebuild graph does not
    // have to re-add the import. The type is used indirectly via
    // `RebuildGraph` today; this stub makes the cohabitation explicit.
    const _: fn(&CodeGraph) = |_| {};

    // ------------------------------------------------------------------
    // Phase 3d post-ExportMap hook (sub-step 7 boundary)
    // ------------------------------------------------------------------
    //
    // Fires once between Phase 3d sub-step 7 (ExportMap rebuild) and
    // sub-step 8 (Phase 4a/4b/4c/4c-prime/4d cross-file edge insert).
    //
    // Tests install this hook with a body that:
    //   - inspects `ExportMap::len` / `lookup` to assert the rebuild
    //     plane's symbol table is populated;
    //   - optionally flips the cancellation token to gate the rest of
    //     sub-step 8 / 9 and assert the post-ExportMap cancellation
    //     boundary fires before edge insertion starts.

    /// Callback signature for the Phase 3d post-ExportMap hook.
    /// Arguments are an immutable reference to the mid-rebuild
    /// `RebuildGraph` (so tests can snapshot the committed arena) and
    /// an immutable reference to the freshly-built `ExportMap`.
    type Phase3dPostExportMapHook = Box<dyn FnMut(&RebuildGraph, &ExportMap)>;

    thread_local! {
        static PHASE3D_POST_EXPORT_MAP_HOOK: RefCell<Option<Phase3dPostExportMapHook>>
            = const { RefCell::new(None) };
    }

    /// Install a callback that runs after Phase 3d sub-step 7
    /// (ExportMap rebuild) and before the post-ExportMap cancellation
    /// boundary. Replaces any previously-installed hook on the same
    /// thread. Returns the prior hook for manual restore; prefer
    /// [`Phase3dPostExportMapHookGuard`] for RAII cleanup.
    pub fn set_phase3d_post_export_map_hook<F>(hook: F) -> Option<Phase3dPostExportMapHook>
    where
        F: FnMut(&RebuildGraph, &ExportMap) + 'static,
    {
        PHASE3D_POST_EXPORT_MAP_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove any currently-installed Phase 3d post-ExportMap hook on
    /// the current thread. Idempotent.
    pub fn clear_phase3d_post_export_map_hook() {
        PHASE3D_POST_EXPORT_MAP_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Fire the installed Phase 3d post-ExportMap hook (if any).
    /// Called from [`super::incremental_rebuild`] right after
    /// [`super::phase3d_rebuild_export_map`] returns.
    pub(super) fn fire_phase3d_post_export_map_hook(
        rebuild_graph: &RebuildGraph,
        export_map: &ExportMap,
    ) {
        PHASE3D_POST_EXPORT_MAP_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(rebuild_graph, export_map);
            }
        });
    }

    /// `RAII` guard for the Phase 3d post-ExportMap hook. Installs on
    /// construction, clears on drop.
    pub struct Phase3dPostExportMapHookGuard {
        _sealed: (),
    }

    impl Phase3dPostExportMapHookGuard {
        /// Install `hook` as the thread-local Phase 3d post-ExportMap
        /// callback (fires once after sub-step 7 completes), returning
        /// a guard that clears it on drop.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(&RebuildGraph, &ExportMap) + 'static,
        {
            let _previous = set_phase3d_post_export_map_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for Phase3dPostExportMapHookGuard {
        fn drop(&mut self) {
            clear_phase3d_post_export_map_hook();
        }
    }

    // ------------------------------------------------------------------
    // Phase 3d post-Pass-4d hook (sub-step 8 boundary)
    // ------------------------------------------------------------------
    //
    // Fires once between Phase 3d sub-step 8 (Phase 4a/4b/4c/4c-prime/
    // 4d edge insertion) and sub-step 9 (Pass 5 cross-language
    // linking). The hook receives the [`super::Pass4dDiagnostics`] so
    // tests can assert on `edges_submitted`, `unification.nodes_merged`,
    // etc.

    /// Callback signature for the Phase 3d post-Pass-4d hook.
    /// Arguments are an immutable reference to the mid-rebuild
    /// `RebuildGraph` (after the edge-insert commit) and the
    /// [`super::Pass4dDiagnostics`] emitted by the Phase 4a..4d
    /// sequence.
    type Phase3dPostPass4dHook = Box<dyn FnMut(&RebuildGraph, &super::Pass4dDiagnostics)>;

    thread_local! {
        static PHASE3D_POST_PASS4D_HOOK: RefCell<Option<Phase3dPostPass4dHook>>
            = const { RefCell::new(None) };
    }

    /// Install a callback that runs after Phase 3d sub-step 8 and
    /// before the post-Pass-4d cancellation boundary. Replaces any
    /// previously-installed hook on the same thread.
    pub fn set_phase3d_post_pass4d_hook<F>(hook: F) -> Option<Phase3dPostPass4dHook>
    where
        F: FnMut(&RebuildGraph, &super::Pass4dDiagnostics) + 'static,
    {
        PHASE3D_POST_PASS4D_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove any currently-installed Phase 3d post-Pass-4d hook on
    /// the current thread. Idempotent.
    pub fn clear_phase3d_post_pass4d_hook() {
        PHASE3D_POST_PASS4D_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Fire the installed Phase 3d post-Pass-4d hook (if any). Called
    /// from [`super::incremental_rebuild`] right after
    /// [`super::phase3d_insert_cross_file_edges`] returns.
    pub(super) fn fire_phase3d_post_pass4d_hook(
        rebuild_graph: &RebuildGraph,
        diagnostics: &super::Pass4dDiagnostics,
    ) {
        PHASE3D_POST_PASS4D_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(rebuild_graph, diagnostics);
            }
        });
    }

    /// `RAII` guard for the Phase 3d post-Pass-4d hook. Installs on
    /// construction, clears on drop.
    pub struct Phase3dPostPass4dHookGuard {
        _sealed: (),
    }

    impl Phase3dPostPass4dHookGuard {
        /// Install `hook` as the thread-local Phase 3d post-Pass-4d
        /// callback (fires once after sub-step 8 completes), returning
        /// a guard that clears it on drop.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(&RebuildGraph, &super::Pass4dDiagnostics) + 'static,
        {
            let _previous = set_phase3d_post_pass4d_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for Phase3dPostPass4dHookGuard {
        fn drop(&mut self) {
            clear_phase3d_post_pass4d_hook();
        }
    }

    // ------------------------------------------------------------------
    // Phase 3d post-Pass-5 hook (sub-step 9 boundary)
    // ------------------------------------------------------------------
    //
    // Fires once between Phase 3d sub-step 9 (Pass 5 cross-language
    // linking) and the Phase 3d → Phase 3e boundary (where
    // `rebuild_graph` is still discarded and the full-build fallback
    // runs). The hook receives the [`Pass5Stats`] emitted by
    // [`super::link_cross_language_edges_generic`].

    /// Callback signature for the Phase 3d post-Pass-5 hook.
    /// Arguments are an immutable reference to the mid-rebuild
    /// `RebuildGraph` (post cross-language linking) and the
    /// [`Pass5Stats`] counters.
    type Phase3dPostPass5Hook = Box<dyn FnMut(&RebuildGraph, &Pass5Stats)>;

    thread_local! {
        static PHASE3D_POST_PASS5_HOOK: RefCell<Option<Phase3dPostPass5Hook>>
            = const { RefCell::new(None) };
    }

    /// Install a callback that runs after Phase 3d sub-step 9 and
    /// before the post-Pass-5 cancellation boundary.
    pub fn set_phase3d_post_pass5_hook<F>(hook: F) -> Option<Phase3dPostPass5Hook>
    where
        F: FnMut(&RebuildGraph, &Pass5Stats) + 'static,
    {
        PHASE3D_POST_PASS5_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove any currently-installed Phase 3d post-Pass-5 hook on
    /// the current thread. Idempotent.
    pub fn clear_phase3d_post_pass5_hook() {
        PHASE3D_POST_PASS5_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Fire the installed Phase 3d post-Pass-5 hook (if any). Called
    /// from [`super::incremental_rebuild`] right after
    /// [`super::link_cross_language_edges_generic`] returns.
    pub(super) fn fire_phase3d_post_pass5_hook(rebuild_graph: &RebuildGraph, stats: &Pass5Stats) {
        PHASE3D_POST_PASS5_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(rebuild_graph, stats);
            }
        });
    }

    /// `RAII` guard for the Phase 3d post-Pass-5 hook. Installs on
    /// construction, clears on drop.
    pub struct Phase3dPostPass5HookGuard {
        _sealed: (),
    }

    impl Phase3dPostPass5HookGuard {
        /// Install `hook` as the thread-local Phase 3d post-Pass-5
        /// callback (fires once after sub-step 9 completes), returning
        /// a guard that clears it on drop.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(&RebuildGraph, &Pass5Stats) + 'static,
        {
            let _previous = set_phase3d_post_pass5_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for Phase3dPostPass5HookGuard {
        fn drop(&mut self) {
            clear_phase3d_post_pass5_hook();
        }
    }

    // ------------------------------------------------------------------
    // Phase 3e post-finalize hook
    // ------------------------------------------------------------------
    //
    // Fires exactly once after [`super::RebuildGraph::finalize`] consumes
    // the rebuild plane and returns the assembled `CodeGraph`, and after
    // [`super::GraphMemorySize::heap_bytes`] is recomputed on that graph.
    // Tests use this hook to assert the publish boundary was reached
    // without having to re-derive `node_count()` / `heap_bytes()` /
    // finalize-elapsed from the returned graph.

    use std::time::Duration;

    /// Callback signature for the Phase 3e post-finalize hook. Arguments
    /// are an immutable reference to the assembled [`CodeGraph`], the
    /// recomputed heap-byte estimate (as returned by
    /// [`super::GraphMemorySize::heap_bytes`]), and the wall-clock
    /// duration of [`super::RebuildGraph::finalize`].
    type Phase3ePostFinalizeHook = Box<dyn FnMut(&CodeGraph, usize, Duration)>;

    thread_local! {
        static PHASE3E_POST_FINALIZE_HOOK: RefCell<Option<Phase3ePostFinalizeHook>>
            = const { RefCell::new(None) };
    }

    /// Install a callback that runs after Phase 3e sub-step 12 recomputes
    /// `heap_bytes` on the assembled `CodeGraph`.
    pub fn set_phase3e_post_finalize_hook<F>(hook: F) -> Option<Phase3ePostFinalizeHook>
    where
        F: FnMut(&CodeGraph, usize, Duration) + 'static,
    {
        PHASE3E_POST_FINALIZE_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove any currently-installed Phase 3e post-finalize hook on the
    /// current thread. Idempotent.
    pub fn clear_phase3e_post_finalize_hook() {
        PHASE3E_POST_FINALIZE_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Fire the installed Phase 3e post-finalize hook (if any). Called
    /// from [`super::incremental_rebuild`] immediately after sub-step 12.
    pub(super) fn fire_phase3e_post_finalize_hook(
        code_graph: &CodeGraph,
        heap_bytes: usize,
        finalize_elapsed: Duration,
    ) {
        PHASE3E_POST_FINALIZE_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(code_graph, heap_bytes, finalize_elapsed);
            }
        });
    }

    /// `RAII` guard for the Phase 3e post-finalize hook. Installs on
    /// construction, clears on drop.
    pub struct Phase3ePostFinalizeHookGuard {
        _sealed: (),
    }

    impl Phase3ePostFinalizeHookGuard {
        /// Install `hook` as the thread-local Phase 3e post-finalize
        /// callback, returning a guard that clears it on drop.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(&CodeGraph, usize, Duration) + 'static,
        {
            let _previous = set_phase3e_post_finalize_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for Phase3ePostFinalizeHookGuard {
        fn drop(&mut self) {
            clear_phase3e_post_finalize_hook();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::identity::IdentityKey;
    use super::*;
    use crate::graph::unified::StringId;
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::NodeEntry;

    fn create_test_entry(name_id: StringId, file_id: FileId) -> NodeEntry {
        NodeEntry::new(NodeKind::Function, name_id, file_id)
    }

    #[test]
    fn test_remove_file_nodes() {
        let mut arena = NodeArena::new();
        let mut identity_index = IdentityIndex::new();
        let mut indices = AuxiliaryIndices::new();
        let file_id = FileId::new(5);

        // Add some nodes
        let name_id = StringId::new(1);
        let entry1 = create_test_entry(name_id, file_id);
        let entry2 = create_test_entry(name_id, file_id);
        let node1 = arena.alloc(entry1).unwrap();
        let node2 = arena.alloc(entry2).unwrap();

        // Add to identity index
        let key1 = IdentityKey::new(StringId::new(1), file_id, StringId::new(10));
        let key2 = IdentityKey::new(StringId::new(1), file_id, StringId::new(11));
        identity_index.insert(key1, node1);
        identity_index.insert(key2, node2);

        // Remove file
        let result = remove_file_nodes(file_id, &mut identity_index, &mut arena, &mut indices);

        assert_eq!(result.stats.nodes_removed, 2);
        assert_eq!(result.stats.identity_entries_removed, 2);
        assert_eq!(result.removed_nodes.len(), 2);

        // Verify nodes are removed (no longer accessible)
        assert!(arena.get(node1).is_none());
        assert!(arena.get(node2).is_none());
    }

    #[test]
    fn test_add_edge_incremental() {
        let source = NodeId::new(0, 1);
        let target = NodeId::new(1, 1);
        let file_id = FileId::new(0);

        let (stats, edge) = add_edge_incremental(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        assert_eq!(stats.edges_added, 1);
        assert_eq!(edge.source, source);
        assert_eq!(edge.target, target);
        assert!(matches!(
            edge.kind,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            }
        ));
    }

    #[test]
    fn test_add_edges_incremental_batch() {
        let file_id = FileId::new(0);
        let edges = vec![
            PendingEdge {
                source: NodeId::new(0, 1),
                target: NodeId::new(1, 1),
                kind: EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                file: file_id,
                spans: vec![],
            },
            PendingEdge {
                source: NodeId::new(1, 1),
                target: NodeId::new(2, 1),
                kind: EdgeKind::References,
                file: file_id,
                spans: vec![],
            },
            PendingEdge {
                source: NodeId::new(2, 1),
                target: NodeId::new(0, 1),
                kind: EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                file: file_id,
                spans: vec![],
            },
        ];

        let stats = add_edges_incremental(&edges);

        assert_eq!(stats.edges_added, 3);
    }

    #[test]
    fn test_remove_node() {
        let mut arena = NodeArena::new();
        let mut identity_index = IdentityIndex::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);

        let entry = create_test_entry(name_id, file_id);
        let node_id = arena.alloc(entry).unwrap();

        // Seed identity index
        let key = IdentityKey::new(StringId::new(1), file_id, StringId::new(10));
        identity_index.insert(key, node_id);

        let stats = remove_node(node_id, &mut identity_index, &mut arena);

        assert_eq!(stats.nodes_removed, 1);
        assert_eq!(stats.identity_entries_removed, 1);
        assert!(arena.get(node_id).is_none());
    }

    #[test]
    fn test_remove_nodes_batch() {
        let mut arena = NodeArena::new();
        let mut identity_index = IdentityIndex::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);

        let node1 = arena.alloc(create_test_entry(name_id, file_id)).unwrap();
        let node2 = arena.alloc(create_test_entry(name_id, file_id)).unwrap();
        let node3 = arena.alloc(create_test_entry(name_id, file_id)).unwrap();

        identity_index.insert(
            IdentityKey::new(StringId::new(1), file_id, StringId::new(10)),
            node1,
        );
        identity_index.insert(
            IdentityKey::new(StringId::new(1), file_id, StringId::new(11)),
            node2,
        );

        let stats = remove_nodes_batch(&[node1, node2, node3], &mut identity_index, &mut arena);

        assert_eq!(stats.nodes_removed, 3);
        assert_eq!(stats.identity_entries_removed, 2);
        assert!(arena.get(node1).is_none());
        assert!(arena.get(node2).is_none());
        assert!(arena.get(node3).is_none());
    }

    #[test]
    fn test_remove_nonexistent_node() {
        let mut arena = NodeArena::new();
        let mut identity_index = IdentityIndex::new();

        // Try to remove a node that doesn't exist
        let fake_id = NodeId::new(999, 1);
        let stats = remove_node(fake_id, &mut identity_index, &mut arena);

        // Should report 0 removed since node didn't exist
        assert_eq!(stats.nodes_removed, 0);
        assert_eq!(stats.identity_entries_removed, 0);
    }

    #[test]
    fn test_incremental_stats_default() {
        let stats = IncrementalStats::default();

        assert_eq!(stats.nodes_removed, 0);
        assert_eq!(stats.edges_removed, 0);
        assert_eq!(stats.edges_added, 0);
        assert_eq!(stats.identity_entries_removed, 0);
    }

    // -------- compute_reverse_dep_closure tests --------

    /// Helper: build a graph with `files` registered, one placeholder node
    /// per file, and the indices rebuilt so `reverse_import_index` works.
    /// Returns the graph, the per-file IDs, and the per-file nodes.
    fn build_closure_graph(files: &[&str]) -> (CodeGraph, Vec<FileId>, Vec<NodeId>) {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::arena::NodeEntry;
        use std::path::Path;

        let mut graph = CodeGraph::new();
        let placeholder = graph.strings_mut().intern("sym").unwrap();
        let mut fids = Vec::with_capacity(files.len());
        let mut nids = Vec::with_capacity(files.len());
        for path in files {
            let fid = graph.files_mut().register(Path::new(path)).unwrap();
            let nid = graph
                .nodes_mut()
                .alloc(NodeEntry::new(NodeKind::Function, placeholder, fid))
                .unwrap();
            fids.push(fid);
            nids.push(nid);
        }
        graph.rebuild_indices();
        (graph, fids, nids)
    }

    /// Helper: add an `Imports` edge from `importer_node` (in `importer_file`)
    /// into `exporter_node`.
    fn add_import(
        graph: &mut CodeGraph,
        importer_node: NodeId,
        exporter_node: NodeId,
        importer_file: FileId,
    ) {
        graph.edges_mut().add_edge(
            importer_node,
            exporter_node,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
            importer_file,
        );
    }

    #[test]
    fn closure_singleton_when_no_importers() {
        // Lone file with no reverse-import edges: closure must contain only
        // the changed file itself, not balloon.
        let (graph, files, _) = build_closure_graph(&["lone.rs"]);
        let closure = compute_reverse_dep_closure(&[files[0]], &graph);
        assert_eq!(closure.len(), 1);
        assert!(closure.contains(&files[0]));
    }

    #[test]
    fn closure_transitive_a_imports_b_imports_c() {
        // File A imports from B, B imports from C. Change C → closure should
        // be {A, B, C}; change B → {A, B}; change A → {A} only.
        let (mut graph, files, nodes) = build_closure_graph(&["a.rs", "b.rs", "c.rs"]);
        let (a, b, c) = (files[0], files[1], files[2]);
        let (na, nb, nc) = (nodes[0], nodes[1], nodes[2]);
        add_import(&mut graph, na, nb, a); // A imports from B
        add_import(&mut graph, nb, nc, b); // B imports from C

        let closure_c = compute_reverse_dep_closure(&[c], &graph);
        assert_eq!(closure_c, [a, b, c].into_iter().collect::<HashSet<_>>());

        let closure_b = compute_reverse_dep_closure(&[b], &graph);
        assert_eq!(closure_b, [a, b].into_iter().collect::<HashSet<_>>());

        let closure_a = compute_reverse_dep_closure(&[a], &graph);
        assert_eq!(closure_a, [a].into_iter().collect::<HashSet<_>>());
    }

    #[test]
    fn closure_diamond_shape_deduplicates() {
        // Diamond: A and B both import from C; D imports from both A and B.
        // Changing C must close over {A, B, C, D} with each file appearing
        // exactly once.
        let (mut graph, files, nodes) = build_closure_graph(&["a.rs", "b.rs", "c.rs", "d.rs"]);
        let (a, b, c, d) = (files[0], files[1], files[2], files[3]);
        let (na, nb, nc, nd) = (nodes[0], nodes[1], nodes[2], nodes[3]);
        add_import(&mut graph, na, nc, a);
        add_import(&mut graph, nb, nc, b);
        add_import(&mut graph, nd, na, d);
        add_import(&mut graph, nd, nb, d);

        let closure = compute_reverse_dep_closure(&[c], &graph);
        assert_eq!(
            closure,
            [a, b, c, d].into_iter().collect::<HashSet<_>>(),
            "diamond shape must close over all four files exactly once"
        );
    }

    #[test]
    fn closure_handles_cyclic_reverse_deps() {
        // Pathological cycle: A imports B, B imports A. Must terminate and
        // return {A, B} regardless of which is the starting file.
        let (mut graph, files, nodes) = build_closure_graph(&["a.rs", "b.rs"]);
        let (a, b) = (files[0], files[1]);
        let (na, nb) = (nodes[0], nodes[1]);
        add_import(&mut graph, na, nb, a);
        add_import(&mut graph, nb, na, b);

        let closure_from_a = compute_reverse_dep_closure(&[a], &graph);
        assert_eq!(closure_from_a, [a, b].into_iter().collect::<HashSet<_>>());
        let closure_from_b = compute_reverse_dep_closure(&[b], &graph);
        assert_eq!(closure_from_b, [a, b].into_iter().collect::<HashSet<_>>());
    }

    #[test]
    fn closure_multiple_starting_files_all_included() {
        // Two independent chains: P→Q and X→Y (P imports from Q, X imports
        // from Y). Closure over {Q, Y} must include all four.
        let (mut graph, files, nodes) = build_closure_graph(&["p.rs", "q.rs", "x.rs", "y.rs"]);
        let (p, q, x, y) = (files[0], files[1], files[2], files[3]);
        let (np, nq, nx, ny) = (nodes[0], nodes[1], nodes[2], nodes[3]);
        add_import(&mut graph, np, nq, p);
        add_import(&mut graph, nx, ny, x);

        let closure = compute_reverse_dep_closure(&[q, y], &graph);
        assert_eq!(closure, [p, q, x, y].into_iter().collect::<HashSet<_>>());
    }

    #[test]
    fn closure_empty_input_returns_empty() {
        let (graph, _, _) = build_closure_graph(&["a.rs"]);
        let closure = compute_reverse_dep_closure(&[], &graph);
        assert!(closure.is_empty());
    }

    #[test]
    fn closure_unregistered_files_passed_through() {
        // Input file IDs that don't exist in the graph registry must still
        // appear in the output set; the engine's job is to widen, not
        // validate, and a freshly-created file may not yet be registered.
        let (graph, _, _) = build_closure_graph(&["a.rs"]);
        let bogus = FileId::new(9999);
        let closure = compute_reverse_dep_closure(&[bogus], &graph);
        assert_eq!(closure, [bogus].into_iter().collect::<HashSet<_>>());
    }

    // -------- Phase 3e: non-Import edge kinds widen the closure ----------
    //
    // The three tests below lock in the Phase 3e correctness fix. Before the
    // switch to `reverse_dependency_index`, `compute_reverse_dep_closure`
    // filtered on `EdgeKind::Imports` only — Calls / References / HttpRequest
    // cross-file edges did NOT widen the closure, and dependent files were
    // stranded with tombstoned edge targets after the target file was
    // re-parsed. See the Codex analysis at
    // `docs/reviews/sqryd-daemon/2026-04-17/phase3e-cross-file-closure-blocker_codex_analysis.md`
    // for the full failure mode.

    /// Helper: add a non-default `Calls` edge from `caller_node` (owned by
    /// `caller_file`) into `callee_node`. Uses `argument_count: 0` + `is_async:
    /// false` because these tests don't care about call-site metadata.
    fn add_call(
        graph: &mut CodeGraph,
        caller_node: NodeId,
        callee_node: NodeId,
        caller_file: FileId,
    ) {
        graph.edges_mut().add_edge(
            caller_node,
            callee_node,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            caller_file,
        );
    }

    /// Helper: add a `References` edge from `referrer_node` (owned by
    /// `referrer_file`) to `referenced_node`.
    fn add_reference(
        graph: &mut CodeGraph,
        referrer_node: NodeId,
        referenced_node: NodeId,
        referrer_file: FileId,
    ) {
        graph.edges_mut().add_edge(
            referrer_node,
            referenced_node,
            EdgeKind::References,
            referrer_file,
        );
    }

    /// Helper: add an `HttpRequest` edge from `client_node` (owned by
    /// `client_file`) into `endpoint_node` (typically an `Endpoint` kind in
    /// another file). Minimal metadata — only the cross-file propagation
    /// semantics matter here, not the routing payload.
    fn add_http_request(
        graph: &mut CodeGraph,
        client_node: NodeId,
        endpoint_node: NodeId,
        client_file: FileId,
    ) {
        let url = graph.strings_mut().intern("/api/harness").unwrap();
        graph.edges_mut().add_edge(
            client_node,
            endpoint_node,
            EdgeKind::HttpRequest {
                method: crate::graph::unified::edge::HttpMethod::Get,
                url: Some(url),
            },
            client_file,
        );
    }

    #[test]
    fn closure_includes_cross_file_callers() {
        // File A contains a node with a Calls edge into a node in file B.
        // Changing B must close over {A, B} even though A never issued an
        // `Imports` edge into B — the live cross-file Calls edge is a real
        // dependency and its target will be tombstoned by Phase 3b's
        // `remove_file(B)`.
        let (mut graph, files, nodes) = build_closure_graph(&["a.rs", "b.rs"]);
        let (a, b) = (files[0], files[1]);
        let (na, nb) = (nodes[0], nodes[1]);
        add_call(&mut graph, na, nb, a);

        let closure = compute_reverse_dep_closure(&[b], &graph);
        assert_eq!(
            closure,
            [a, b].into_iter().collect::<HashSet<_>>(),
            "cross-file Calls edge must drive closure widening"
        );

        // Sanity check the reverse direction: changing A with no Imports /
        // outgoing dependents of A other than the call into B should close
        // over {A} only. (Reverse-dep closure asks "who depends on this?",
        // not "what does this depend on?")
        let closure_a = compute_reverse_dep_closure(&[a], &graph);
        assert_eq!(closure_a, [a].into_iter().collect::<HashSet<_>>());
    }

    #[test]
    fn closure_includes_cross_file_references() {
        // Same shape as the Calls test, but with a `References` edge. Covers
        // type references, field reads, generic binding references, and any
        // other "I point at a symbol defined elsewhere" relationship the
        // plugins emit as `EdgeKind::References`.
        let (mut graph, files, nodes) = build_closure_graph(&["a.rs", "b.rs"]);
        let (a, b) = (files[0], files[1]);
        let (na, nb) = (nodes[0], nodes[1]);
        add_reference(&mut graph, na, nb, a);

        let closure = compute_reverse_dep_closure(&[b], &graph);
        assert_eq!(
            closure,
            [a, b].into_iter().collect::<HashSet<_>>(),
            "cross-file References edge must drive closure widening"
        );
    }

    #[test]
    fn closure_includes_cross_language_http_dependents() {
        // Cross-language dependency: file `client.ts` holds an `HttpRequest`
        // edge into an Endpoint node in `server.ts`. Changing `server.ts`
        // must close over `{client.ts, server.ts}`. Pre-Phase-3e this
        // relationship was invisible to the closure because only
        // `EdgeKind::Imports` drove `reverse_import_index` — HttpRequest
        // edges were silently filtered out.
        let (mut graph, files, nodes) = build_closure_graph(&["client.ts", "server.ts"]);
        let (client, server) = (files[0], files[1]);
        let (nclient, nserver) = (nodes[0], nodes[1]);
        add_http_request(&mut graph, nclient, nserver, client);

        let closure = compute_reverse_dep_closure(&[server], &graph);
        assert_eq!(
            closure,
            [client, server].into_iter().collect::<HashSet<_>>(),
            "cross-language HttpRequest edge must drive closure widening"
        );
    }

    // -------- incremental_rebuild Phase 3b scaffolding tests --------

    #[test]
    fn incremental_rebuild_empty_inputs_return_empty_graph() {
        // Phase 3e contract: an empty `current_graph` + empty changed-files
        // + empty closure is a legitimate no-op rebuild. The function must
        // return a fresh empty `CodeGraph` (no nodes, epoch bumped to 1 by
        // `RebuildGraph::finalize`), NOT an error.
        //
        // Before Phase 3e the Gate 0a stub couldn't infer a workspace root
        // from an empty graph + empty changes and returned
        // `GraphBuilderError::Internal { "cannot infer workspace root" }`.
        // Phase 3e removed the `build_unified_graph` fallback entirely —
        // the engine no longer needs to canonicalise a workspace root
        // because it publishes the rebuild plane directly. An empty
        // rebuild walks every phase as a no-op and returns the empty
        // finalized graph.
        //
        // This behaviour matters for the sqryd daemon's reaper path: a
        // spurious rebuild trigger with no changes must not fail the
        // workspace — it must cheaply return a representation of the
        // unchanged published graph.
        let graph = CodeGraph::new();
        let plugins = crate::plugin::PluginManager::new();
        let config = super::super::entrypoint::BuildConfig::default();
        let closure: HashSet<FileId> = HashSet::new();
        let cancellation = CancellationToken::new();

        let result = incremental_rebuild(&graph, &[], &closure, &plugins, &config, &cancellation)
            .expect("empty rebuild is a no-op; must succeed without error");
        assert_eq!(
            result.node_count(),
            0,
            "empty rebuild must return a graph with zero nodes"
        );
    }

    #[test]
    fn incremental_rebuild_returns_cancelled_if_preflight_check_fails() {
        // Phase 3a pre-flight contract: if the cancellation token is
        // already cancelled when `incremental_rebuild` is entered, the
        // function MUST return `GraphBuilderError::Cancelled` before doing
        // any work. This is the first line of defence against rebuilds
        // whose dispatcher has already scheduled a newer rebuild over them.
        //
        // We deliberately pass a graph + inputs that would otherwise cause
        // the stub to error with `Internal { "cannot infer workspace
        // root" }` — cancellation must win over that failure mode because
        // it is checked first. That proves the pre-flight runs at the
        // very top of the body.
        let graph = CodeGraph::new();
        let plugins = crate::plugin::PluginManager::new();
        let config = super::super::entrypoint::BuildConfig::default();
        let closure: HashSet<FileId> = HashSet::new();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let err = incremental_rebuild(&graph, &[], &closure, &plugins, &config, &cancellation)
            .expect_err("cancelled token must short-circuit incremental_rebuild");
        assert!(
            matches!(err, GraphBuilderError::Cancelled),
            "expected GraphBuilderError::Cancelled (not Internal), got: {err:?}"
        );
    }

    #[test]
    fn incremental_rebuild_delegates_to_full_build_when_not_cancelled() {
        // Sanity-check the Phase 3a wrapper contract: with a live
        // (un-cancelled) token the call must NOT short-circuit via
        // `GraphBuilderError::Cancelled`. It must reach the delegation step
        // and then fail for whatever reason the full-build pipeline
        // surfaces with the minimal inputs available to a sqry-core unit
        // test (no language plugins registered — core cannot depend on
        // plugin crates because the dependency direction is core ← plugin).
        //
        // Concretely: the bare `PluginManager` has no graph builders, so
        // `build_unified_graph` correctly returns
        // "No graph builders registered – cannot build code graph", which
        // is surfaced as `GraphBuilderError::Internal`. That is exactly
        // what Phase 3a's delegation is supposed to pass through
        // unchanged — any regression where the pre-flight check accidentally
        // swallows the call and returns `Cancelled` instead would fail
        // this assertion.
        //
        // The §E property-based harness in
        // `sqry-core/tests/incremental_equivalence.rs` carries the
        // full-language integration coverage against the real plugin set.
        use std::fs;

        let temp = tempfile::tempdir().expect("create tempdir");
        let src_dir = temp.path().join("src");
        fs::create_dir_all(&src_dir).expect("create src dir");
        fs::write(src_dir.join("lib.rs"), b"pub fn noop() {}\n").expect("write src file");

        let current_graph = CodeGraph::new();
        let plugins = crate::plugin::PluginManager::new();
        let config = super::super::entrypoint::BuildConfig::default();
        let closure: HashSet<FileId> = HashSet::new();
        let cancellation = CancellationToken::new();
        // Pass the temp workspace path as a changed file so the Phase 3e
        // new-file leg sees a non-empty input even though `current_graph`
        // is empty.
        let changed = vec![src_dir.join("lib.rs")];

        let result = incremental_rebuild(
            &current_graph,
            &changed,
            &closure,
            &plugins,
            &config,
            &cancellation,
        );

        // The pre-flight must NOT have fired. The only acceptable
        // outcomes at this boundary are `Ok(_)` (real plugins) or
        // `Err(Internal { .. })` (no plugins, but the delegation step was
        // reached). `Err(Cancelled)` here would mean Phase 3a leaked a
        // false-positive cancellation on a live token.
        match result {
            Ok(_) => {
                // Reachable in the §E harness where real plugins are
                // registered. Accepted outcome.
            }
            Err(GraphBuilderError::Internal { reason }) => {
                assert!(
                    reason.contains("No graph builders registered")
                        || reason.contains("Gate 0a stub")
                        || reason.contains("Phase 3b fallback")
                        || reason.contains("Phase 3c fallback")
                        || reason.contains("Phase 3d fallback"),
                    "delegation reached full-build but failed for an unexpected reason: {reason}"
                );
            }
            Err(GraphBuilderError::Cancelled) => {
                panic!(
                    "Phase 3a pre-flight must NOT fire on a live (un-cancelled) token, but \
                     incremental_rebuild returned GraphBuilderError::Cancelled"
                );
            }
            Err(other) => {
                panic!("unexpected error shape from Phase 3a stub delegation: {other:?}",)
            }
        }
        // Token must still be un-cancelled after the call — we never
        // mutate the token from inside the engine.
        assert!(!cancellation.is_cancelled());
    }

    // Phase 3e removed `infer_workspace_root`, `canonicalize_or_normalize`,
    // and `longest_common_directory_prefix` together with the
    // `build_unified_graph` fallback delegation. The five tests that
    // locked in those helpers (including the Gate 0a symlink-alias
    // regression) were deleted in lockstep because the surfaces they
    // observed no longer exist — Phase 3e publishes the rebuild plane
    // directly via `RebuildGraph::finalize` and never walks a workspace
    // root from inside `incremental_rebuild`. The §E property-based
    // harness (sqry-core/tests/incremental_equivalence.rs) is now the
    // load-bearing oracle against workspace-level regressions.

    // -------- incremental_rebuild Phase 3b sub-step 1-3 tests --------
    //
    // These tests exercise the Phase 3b contract from the inside: they
    // install a thread-local observation hook via
    // `testing::Phase3bHookGuard` that fires at the end of sub-step 3
    // and receives both the mid-rebuild `RebuildGraph` and the closure
    // that drove the removal loop. That lets the tests make concrete
    // assertions about what the three real sub-steps did without
    // needing a whole plugin-backed rebuild to succeed.

    use std::cell::RefCell;
    use std::rc::Rc;

    /// Build a closure graph AND record each node in the FileRegistry's
    /// per-file bucket. `build_closure_graph` alone does NOT call
    /// `FileRegistry::record_node`, because the callers of that helper
    /// (closure-BFS tests) only need `reverse_import_index` wiring —
    /// they never exercise the Task 4 Step 3 `remove_file` path.
    ///
    /// Phase 3b IS that `remove_file` path, so we need the bucket
    /// populated for `take_nodes` to return non-empty in sub-step 3.
    /// Rebuilding indices is mandatory: `reverse_import_index` reads
    /// from auxiliary indices, and `remove_file` interacts with the
    /// edge store via `tombstone_edges_for_nodes` which expects a
    /// consistent state.
    fn build_closure_graph_with_buckets(files: &[&str]) -> (CodeGraph, Vec<FileId>, Vec<NodeId>) {
        let (mut graph, fids, nids) = build_closure_graph(files);
        for (&fid, &nid) in fids.iter().zip(nids.iter()) {
            graph.files_mut().record_node(fid, nid);
        }
        // No need to rebuild indices here — `build_closure_graph`
        // already did, and `record_node` only touches the FileRegistry
        // which is not an auxiliary index.
        (graph, fids, nids)
    }

    /// Build a small committed graph with a chain of imports:
    /// `a.rs` ← imports — `b.rs` ← imports — `c.rs` (i.e. `a` imports
    /// from `b`, `b` imports from `c`). Returns the graph plus the
    /// `FileId`s for `(a, b, c)` — useful for Phase 3b removal tests
    /// that need `compute_reverse_dep_closure` to widen over more than
    /// one file AND `remove_file` to observe per-file node buckets.
    fn build_chain_graph() -> (CodeGraph, FileId, FileId, FileId) {
        let (mut graph, files, nodes) = build_closure_graph_with_buckets(&["a.rs", "b.rs", "c.rs"]);
        let (a, b, c) = (files[0], files[1], files[2]);
        let (na, nb, nc) = (nodes[0], nodes[1], nodes[2]);
        add_import(&mut graph, na, nb, a); // a imports from b
        add_import(&mut graph, nb, nc, b); // b imports from c
        (graph, a, b, c)
    }

    #[test]
    fn incremental_rebuild_phase3b_constructs_rebuild_graph_and_removes_closure_members() {
        // Contract: Phase 3b sub-step 2 constructs a RebuildGraph via
        // `clone_for_rebuild`, and sub-step 3 calls
        // `rebuild_graph.remove_file(file_id)` for EVERY FileId in the
        // closure.
        //
        // We observe this from the inside by installing a hook that
        // fires at the end of sub-step 3. The hook captures:
        //   (1) `rebuild_graph.pending_tombstone_count()` — how many
        //       NodeIds were staged for tombstoning. Must equal the
        //       number of nodes across all closure files. With one node
        //       per file this simplifies to `closure.len()`.
        //   (2) the closure's own size, so we can assert the hook was
        //       called exactly once with the real closure.
        //
        // Changing `c.rs` in the chain fixture closes over {a, b, c};
        // each file contributes one node, so sub-step 3 must tombstone
        // exactly 3 NodeIds.
        let (graph, _a, _b, c) = build_chain_graph();
        let plugins = crate::plugin::PluginManager::new();
        let config = super::super::entrypoint::BuildConfig::default();
        let cancellation = CancellationToken::new();

        let closure = compute_reverse_dep_closure(&[c], &graph);
        assert_eq!(closure.len(), 3, "chain graph must widen over {{a, b, c}}");

        // Hook state: capture sizes. Rc<RefCell<_>> because the hook
        // closure outlives the local scope.
        #[derive(Default)]
        struct Observations {
            hook_fired: u32,
            pending_tombstones_after_substep3: usize,
            closure_size_seen_by_hook: usize,
        }
        let obs = Rc::new(RefCell::new(Observations::default()));
        let obs_hook = Rc::clone(&obs);
        let _guard = testing::Phase3bHookGuard::install(move |rebuild_graph, hook_closure| {
            let mut o = obs_hook.borrow_mut();
            o.hook_fired += 1;
            o.pending_tombstones_after_substep3 = rebuild_graph.pending_tombstone_count();
            o.closure_size_seen_by_hook = hook_closure.len();
        });

        // Drive the rebuild. We don't care about the final returned
        // graph (Phase 3b discards `rebuild_graph` anyway and falls
        // through to `build_unified_graph`, which will fail with "no
        // plugins" in this unit test — that is fine; the hook has
        // already fired by then).
        let _ = incremental_rebuild(&graph, &[], &closure, &plugins, &config, &cancellation);

        let o = obs.borrow();
        assert_eq!(
            o.hook_fired, 1,
            "sub-step 3 hook must fire exactly once per incremental_rebuild call"
        );
        assert_eq!(
            o.closure_size_seen_by_hook, 3,
            "hook must receive the same closure that was passed in"
        );
        assert_eq!(
            o.pending_tombstones_after_substep3, 3,
            "sub-step 3 must call rebuild_graph.remove_file on every closure file; \
             3 closure files × 1 node each = 3 staged tombstones"
        );
    }

    #[test]
    fn incremental_rebuild_phase3b_hook_sees_fresh_rebuild_graph_on_empty_closure() {
        // Edge case: when the closure is empty (e.g. a changed file
        // nobody imports from), sub-step 3's loop body never runs, but
        // sub-step 2 still constructs the `RebuildGraph` and the hook
        // must still fire. Pending tombstone count must be zero since
        // nothing was removed.
        let (graph, files, _nodes) = build_closure_graph_with_buckets(&["lone.rs"]);
        let plugins = crate::plugin::PluginManager::new();
        let config = super::super::entrypoint::BuildConfig::default();
        let cancellation = CancellationToken::new();

        // Deliberately start with an EMPTY closure so sub-step 3's
        // loop body never executes. Sub-step 2 and the hook must
        // still run.
        let closure: HashSet<FileId> = HashSet::new();

        let hook_fired = Rc::new(RefCell::new(0u32));
        let pending_after = Rc::new(RefCell::new(usize::MAX));
        let hook_fired_clone = Rc::clone(&hook_fired);
        let pending_after_clone = Rc::clone(&pending_after);
        let _guard = testing::Phase3bHookGuard::install(move |rebuild_graph, _| {
            *hook_fired_clone.borrow_mut() += 1;
            *pending_after_clone.borrow_mut() = rebuild_graph.pending_tombstone_count();
        });

        let _ = incremental_rebuild(
            &graph,
            &[std::path::PathBuf::from("lone.rs")],
            &closure,
            &plugins,
            &config,
            &cancellation,
        );

        assert_eq!(
            *hook_fired.borrow(),
            1,
            "hook must fire even when the closure is empty (sub-step 2 + trivial sub-step 3)"
        );
        assert_eq!(
            *pending_after.borrow(),
            0,
            "empty closure must leave `rebuild_graph` with zero staged tombstones"
        );

        // Also prove the committed graph still has `lone.rs`: Phase 3b
        // must never touch `current_graph`, only its clone.
        let indexed: Vec<_> = graph.indexed_files().collect();
        assert!(
            indexed.iter().any(|(fid, _)| *fid == files[0]),
            "committed graph must be untouched by Phase 3b (only the cloned RebuildGraph is mutated)"
        );
    }

    #[test]
    fn incremental_rebuild_phase3b_cancels_mid_closure_without_full_completion() {
        // Pre-flight cancellation test. Distinct from the Step 3 loop
        // cancellation test below.
        //
        // Scenario modelled: a dispatcher cancels a rebuild request
        // BEFORE the engine runs any work. The cancellation token is
        // flipped *before* `incremental_rebuild` is called, so the
        // pre-flight check at the top of the function
        // (`cancellation.check()?` right after the docstring) is the
        // first `check()` site reached. That site MUST short-circuit
        // the engine with `GraphBuilderError::Cancelled` before
        //   (a) `clone_for_rebuild` runs,
        //   (b) the sub-step 3 loop runs any iteration, or
        //   (c) the post-substep3 hook fires.
        //
        // Coverage target: pre-flight check only. The per-iteration
        // hook is installed and asserted *not* to fire, which proves
        // the Step 3 loop never ran — implying the pre-flight check
        // caught cancellation (the only earlier check-site is the
        // one between Step 1 and Step 2, which could not alone have
        // stopped Step 3 without the iter hook being invoked).
        //
        // The distinguishing test for the Step 3 loop `check()` lives
        // in `..._iteration_cancellation_between_remove_calls` below.
        let (graph, _a, _b, c) = build_chain_graph();
        let plugins = crate::plugin::PluginManager::new();
        let config = super::super::entrypoint::BuildConfig::default();
        let closure = compute_reverse_dep_closure(&[c], &graph);

        let cancellation = CancellationToken::new();

        // Pre-flight precondition: the token must be cancelled
        // BEFORE `incremental_rebuild` is called, so the pre-flight
        // check at the top of the function is the first `check()`
        // site reached.
        assert!(
            !cancellation.is_cancelled(),
            "sanity check: token must start un-cancelled before we cancel it for the pre-flight path"
        );
        cancellation.cancel();
        assert!(
            cancellation.is_cancelled(),
            "pre-flight precondition: token must be cancelled before incremental_rebuild is invoked"
        );

        let post_fired = Rc::new(RefCell::new(0u32));
        let post_fired_hook = Rc::clone(&post_fired);
        let _post_guard = testing::Phase3bHookGuard::install(move |_, _| {
            *post_fired_hook.borrow_mut() += 1;
        });

        // Install a per-iteration hook that simply records iteration
        // events. If the pre-flight check fires as expected, the Step
        // 3 loop never runs, so the iter-hook must see zero events.
        // Any non-zero reading here would indicate the pre-flight
        // check was bypassed and the Step 3 loop ran instead.
        let iter_events = Rc::new(RefCell::new(Vec::<(usize, FileId)>::new()));
        let iter_events_hook = Rc::clone(&iter_events);
        let _iter_guard = testing::Phase3bIterHookGuard::install(move |idx, fid, _rg| {
            iter_events_hook.borrow_mut().push((idx, fid));
        });

        let result = incremental_rebuild(&graph, &[], &closure, &plugins, &config, &cancellation);
        let err =
            result.expect_err("pre-flight cancellation must short-circuit incremental_rebuild");
        assert!(
            matches!(err, GraphBuilderError::Cancelled),
            "expected GraphBuilderError::Cancelled, got: {err:?}"
        );
        assert_eq!(
            *post_fired.borrow(),
            0,
            "pre-flight cancellation must NOT let execution reach the post-substep3 hook"
        );
        assert_eq!(
            iter_events.borrow().len(),
            0,
            "pre-flight cancellation must prevent the Step 3 loop from running any iteration; \
             iter-hook fire count would be >0 if the loop ran even once"
        );
    }

    #[test]
    fn incremental_rebuild_phase3b_iteration_cancellation_between_remove_calls() {
        // Step 3 loop cancellation test. This is the distinguishing
        // test for the loop-top `cancellation.check()?` that Phase 3b
        // added at sub-step 3 — completely separate from the Phase 3a
        // pre-flight check exercised by the test above.
        //
        // Contract:
        //   (1) pre-flight check observes an UN-cancelled token and
        //       passes (we assert by pre-test sanity check and by
        //       `clone_for_rebuild` having happened: the post-clone
        //       iter-hook is the only place we could observe evidence
        //       of the clone, and the hook firing at all proves the
        //       clone succeeded);
        //   (2) the first `remove_file` completes and the iter-hook
        //       at iteration 0 fires (because cancellation has not
        //       yet been set);
        //   (3) from inside the iter-hook at iteration 0 we flip the
        //       cancellation token;
        //   (4) the next iteration's loop-top `cancellation.check()?`
        //       at iteration 1 returns `GraphBuilderError::Cancelled`;
        //   (5) the iter-hook at iteration 1 does NOT fire (because
        //       the loop body stopped at `check()` before reaching
        //       `remove_file`);
        //   (6) the post-substep3 hook does NOT fire.
        //
        // If the loop-top check were removed or broken, step (4)
        // would not fire `Cancelled`; the iter-hook would see TWO
        // firings (both iterations complete), `remove_file` would
        // run on both files, and the post-substep3 hook would fire.
        // The three assertions below rule out each of those
        // regressions.
        //
        // Iteration order is deterministic: `ordered_closure_file_ids`
        // sorts by `FileId::index`. The chain fixture builds files in
        // the order `a`, `b`, `c`, so iteration 0 targets `a` (lowest
        // index). We do not *need* to assert on the file identity,
        // only on the iteration index, but keeping the chain fixture
        // order stable is documented so a future refactor knows to
        // update both this test and the fixture together.
        let (graph, _a, _b, c) = build_chain_graph();
        let plugins = crate::plugin::PluginManager::new();
        let config = super::super::entrypoint::BuildConfig::default();
        let closure = compute_reverse_dep_closure(&[c], &graph);
        assert_eq!(
            closure.len(),
            3,
            "chain graph's reverse closure over `c` must contain {{a, b, c}} for this test to \
             actually exercise multiple iterations"
        );

        let cancellation = CancellationToken::new();

        // CRITICAL: the token starts un-cancelled. This is what
        // distinguishes this test from the pre-flight test above.
        // The pre-flight check at the top of `incremental_rebuild`
        // must observe an un-cancelled token and pass. The
        // cancellation flip happens *inside* the iter-hook at
        // iteration 0 — i.e., *after* the first `remove_file` call
        // has completed but *before* the next iteration's
        // `cancellation.check()` call.
        assert!(
            !cancellation.is_cancelled(),
            "Step 3 loop precondition: token MUST start un-cancelled; this is the sole invariant \
             that separates this test from the pre-flight test"
        );

        let post_fired = Rc::new(RefCell::new(0u32));
        let post_fired_hook = Rc::clone(&post_fired);
        let _post_guard = testing::Phase3bHookGuard::install(move |_, _| {
            *post_fired_hook.borrow_mut() += 1;
        });

        // Install the per-iteration hook. It records every (idx,
        // file_id) pair it observes AND flips the cancellation
        // token on iteration 0. Because the loop-top `check()`
        // runs *before* `remove_file` on the next iteration, the
        // iter-hook for iteration 1 must NOT be reached.
        let iter_events = Rc::new(RefCell::new(Vec::<(usize, FileId)>::new()));
        let iter_events_hook = Rc::clone(&iter_events);
        let cancel_from_hook = cancellation.clone();
        let _iter_guard = testing::Phase3bIterHookGuard::install(move |idx, fid, _rg| {
            iter_events_hook.borrow_mut().push((idx, fid));
            if idx == 0 {
                // Flip cancellation right after the first
                // `remove_file` completes. The next loop iteration's
                // top-of-body `cancellation.check()` must short-circuit.
                cancel_from_hook.cancel();
            }
        });

        let result = incremental_rebuild(&graph, &[], &closure, &plugins, &config, &cancellation);
        let err = result.expect_err(
            "token cancelled mid-loop must short-circuit at the loop-top cancellation.check()",
        );
        assert!(
            matches!(err, GraphBuilderError::Cancelled),
            "expected GraphBuilderError::Cancelled from the Step 3 loop check, got: {err:?}"
        );

        // Exactly one iteration's worth of iter-hook activity must
        // have been observed. Two events would mean the loop-top
        // `check()` did not short-circuit between iterations.
        let events = iter_events.borrow();
        assert_eq!(
            events.len(),
            1,
            "Step 3 loop-top `cancellation.check()` must short-circuit between iterations 0 and \
             1; iter-hook observed {:?} instead of exactly one (idx=0) event",
            events
        );
        assert_eq!(
            events[0].0, 0,
            "first and only iter-hook fire must be for iteration 0 (the iteration that flipped \
             cancellation)"
        );

        // Post-substep3 hook must not have run: the loop-top
        // `check()` at iteration 1 returned `Cancelled` before the
        // loop body completed, so control never reached the
        // post-loop `cancellation.check()` or the post-substep3
        // hook fire site.
        assert_eq!(
            *post_fired.borrow(),
            0,
            "Step 3 loop cancellation must NOT let execution reach the post-substep3 hook"
        );
    }

    #[test]
    fn incremental_rebuild_phase3b_still_delegates_to_full_build_fallback() {
        // Contract: sub-steps 4–13 remain delegated to
        // `build_unified_graph`. Phase 3b's scaffolding must not
        // accidentally swallow the fallback. We verify this by
        // installing the post-substep3 hook (proves sub-steps 1–3
        // ran) AND checking that the final return value is either
        // Ok(_) (real plugins) or Err(Internal { "No graph builders
        // registered" }) (bare PluginManager in a unit test). An
        // `Err(Cancelled)` from a live token — or any other error
        // shape — would indicate that the Phase 3b wiring broke the
        // fallback. The §E property-based harness
        // (`sqry-core/tests/incremental_equivalence.rs`) carries the
        // real-plugin coverage side of this invariant.
        let (graph, _a, _b, c) = build_chain_graph();
        let plugins = crate::plugin::PluginManager::new();
        let config = super::super::entrypoint::BuildConfig::default();
        let cancellation = CancellationToken::new();
        let closure = compute_reverse_dep_closure(&[c], &graph);

        let hook_fired = Rc::new(RefCell::new(false));
        let hook_fired_clone = Rc::clone(&hook_fired);
        let _guard = testing::Phase3bHookGuard::install(move |_, _| {
            *hook_fired_clone.borrow_mut() = true;
        });

        let result = incremental_rebuild(&graph, &[], &closure, &plugins, &config, &cancellation);

        assert!(
            *hook_fired.borrow(),
            "Phase 3b sub-step 3 hook must fire before the fallback delegates to \
             build_unified_graph — otherwise the scaffolding is bypassed"
        );

        match result {
            Ok(_) => {
                // Reachable if the test environment somehow has
                // plugins registered; accepted.
            }
            Err(GraphBuilderError::Internal { reason }) => {
                assert!(
                    reason.contains("No graph builders registered")
                        || reason.contains("Phase 3b fallback")
                        || reason.contains("Phase 3c fallback")
                        || reason.contains("Phase 3d fallback"),
                    "unexpected Internal reason from Phase 3b fallback: {reason}"
                );
            }
            Err(GraphBuilderError::Cancelled) => {
                panic!(
                    "live (un-cancelled) token must NOT produce Cancelled from Phase 3b — \
                     scaffolding must not leak cancellation"
                );
            }
            Err(other) => panic!("unexpected error shape from Phase 3b fallback: {other:?}"),
        }
    }

    #[test]
    fn incremental_rebuild_phase3b_preserves_current_graph_reference() {
        // Sub-step 2's `clone_for_rebuild` is a deep-clone by design:
        // the incremental rebuild engine must never mutate the
        // caller-provided `current_graph`. This test registers a
        // handful of files, drives the rebuild, and asserts the
        // committed graph's indexed-files set is unchanged.
        let (graph, files, _nodes) = build_closure_graph_with_buckets(&["x.rs", "y.rs", "z.rs"]);
        let plugins = crate::plugin::PluginManager::new();
        let config = super::super::entrypoint::BuildConfig::default();
        let cancellation = CancellationToken::new();

        // Close over every file so sub-step 3 calls `remove_file`
        // for each one on the cloned RebuildGraph. If the cloning
        // were shallow, those removals would bleed into
        // `current_graph`.
        let closure: HashSet<FileId> = files.iter().copied().collect();

        let before: HashSet<FileId> = graph.indexed_files().map(|(fid, _)| fid).collect();
        let _ = incremental_rebuild(&graph, &[], &closure, &plugins, &config, &cancellation);
        let after: HashSet<FileId> = graph.indexed_files().map(|(fid, _)| fid).collect();

        assert_eq!(
            before, after,
            "sub-step 2's clone_for_rebuild must deep-clone; `current_graph` must be \
             untouched by any Phase 3b closure removals"
        );
    }

    #[test]
    fn ordered_closure_file_ids_is_deterministic_and_sorted_by_index() {
        // The sub-step 3 iteration order is load-bearing for the
        // cancellation tests: without a stable order, a test that
        // "flips the token after the first remove" would see
        // non-deterministic results because HashSet iteration order
        // is unspecified. Lock in the ordering contract.
        let mut closure: HashSet<FileId> = HashSet::new();
        closure.insert(FileId::new(5));
        closure.insert(FileId::new(1));
        closure.insert(FileId::new(9));
        closure.insert(FileId::new(3));

        let ordered = super::ordered_closure_file_ids(&closure);
        assert_eq!(
            ordered,
            vec![
                FileId::new(1),
                FileId::new(3),
                FileId::new(5),
                FileId::new(9),
            ]
        );

        // Idempotent: running twice on the same closure yields the
        // same vector.
        let ordered_again = super::ordered_closure_file_ids(&closure);
        assert_eq!(ordered, ordered_again);
    }

    #[test]
    fn phase3b_hook_guard_clears_hook_on_drop() {
        // Defensive test for `Phase3bHookGuard`'s RAII contract: if
        // a test panics mid-run, the guard must remove the
        // thread-local hook so subsequent tests on the same thread
        // do not see stale state. We simulate the "after drop"
        // state directly.
        let fire_count = Rc::new(RefCell::new(0u32));
        let fire_count_hook = Rc::clone(&fire_count);

        {
            let _guard = testing::Phase3bHookGuard::install(move |_, _| {
                *fire_count_hook.borrow_mut() += 1;
            });
            // Guard drops here.
        }

        // Drive a rebuild after the guard has been dropped. The hook
        // must NOT fire.
        let (graph, _a, _b, c) = build_chain_graph();
        let plugins = crate::plugin::PluginManager::new();
        let config = super::super::entrypoint::BuildConfig::default();
        let cancellation = CancellationToken::new();
        let closure = compute_reverse_dep_closure(&[c], &graph);

        let _ = incremental_rebuild(&graph, &[], &closure, &plugins, &config, &cancellation);

        assert_eq!(
            *fire_count.borrow(),
            0,
            "dropped Phase3bHookGuard must leave no installed hook"
        );
    }
}
