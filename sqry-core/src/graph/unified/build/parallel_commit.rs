//! Parallel commit pipeline for pre-allocated ID ranges.
//!
//! Replaces the serial commit loop with a four-phase pipeline:
//! Phase 2: Count + range assignment via prefix sums
//! Phase 3: Parallel commit into disjoint pre-allocated ranges
//! Phase 4: String dedup, remap, index build, edge bulk insert
//!
//! # Phase 3 Architecture
//!
//! Phase 3 uses `split_at_mut` to carve disjoint sub-slices from pre-allocated
//! arena and interner ranges, then uses `rayon` to commit each file's staging
//! graph in parallel without locks:
//!
//! ```text
//! NodeArena slots:   [   file0   |   file1   |   file2   ]
//! StringInterner:    [   file0   |   file1   |   file2   ]
//!                         ↑            ↑            ↑
//!                    split_at_mut  split_at_mut  remainder
//! ```
//!
//! Each file's `commit_single_file` receives its own disjoint slices and
//! operates independently without contention.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use rayon::prelude::*;

use crate::graph::unified::edge::delta::{DeltaEdge, DeltaOp};
#[cfg(test)]
use crate::graph::unified::edge::kind::ResolvedVia;
use crate::graph::unified::edge::kind::{EdgeKind, MqProtocol};
use crate::graph::unified::file::FileId;
use crate::graph::unified::node::NodeId;
use crate::graph::unified::storage::NodeArena;
use crate::graph::unified::storage::arena::{NodeEntry, Slot};
use crate::graph::unified::storage::c_indirect::LocalScopeIndex;
use crate::graph::unified::string::StringId;

use super::pass3_intra::PendingEdge;
use super::staging::{
    GoEmbeddingHint, GoFunctionSignatureHint, GoMethodReceiverHint, GoMethodSignatureHint,
    GoNamedTypeConversionHint, GoReceiverCallHint, GoReceiverHintKind, PendingBinding,
    PendingIndirectCallsite, StagingGraph, StagingOp,
};

/// Running offsets carried across chunks for deterministic ID assignment.
///
/// Each chunk's ranges begin where the previous chunk ended, ensuring
/// globally unique, contiguous ID spaces.
#[derive(Debug, Clone, Default)]
pub struct GlobalOffsets {
    /// Next available node slot index.
    pub node_offset: u32,
    /// Next available string slot index.
    pub string_offset: u32,
}

/// Per-file commit plan with pre-assigned ID ranges.
#[derive(Debug, Clone)]
pub struct FilePlan {
    /// Index into the chunk's `ParsedFile` vec.
    pub parsed_index: usize,
    /// Pre-assigned `FileId` from batch registration.
    pub file_id: FileId,
    /// Node slot range [start..end) in `NodeArena`.
    pub node_range: Range<u32>,
    /// String slot range [start..end) in `StringInterner`.
    pub string_range: Range<u32>,
}

/// Plan for parallel commit of a single chunk.
#[derive(Debug, Clone)]
pub struct ChunkCommitPlan {
    /// Per-file plans in deterministic file order.
    pub file_plans: Vec<FilePlan>,
    /// Total nodes across all files in this chunk.
    pub total_nodes: u32,
    /// Total strings across all files in this chunk.
    pub total_strings: u32,
    /// Total edges across all files in this chunk.
    pub total_edges: u64,
}

/// Compute commit plan from parsed files using prefix-sum range assignment.
///
/// Each file gets contiguous, non-overlapping ranges for nodes and strings.
/// Ranges start from the given global offsets, which carry forward across
/// chunks.
///
/// # Arguments
///
/// * `node_counts` - Per-file node counts (from `StagingGraph::node_count_u32()`)
/// * `string_counts` - Per-file string counts
/// * `edge_counts` - Per-file edge counts (used for `total_edges` only)
/// * `file_ids` - Pre-assigned `FileId`s from batch registration
/// * `node_offset` - Running global node offset across chunks
/// * `string_offset` - Running global string offset across chunks
///
/// # Panics
///
/// Panics in debug builds if the per-chunk accounting arrays do not have
/// identical lengths.
#[must_use]
pub fn compute_commit_plan(
    node_counts: &[u32],
    string_counts: &[u32],
    edge_counts: &[u32],
    file_ids: &[FileId],
    node_offset: u32,
    string_offset: u32,
) -> ChunkCommitPlan {
    debug_assert_eq!(node_counts.len(), string_counts.len());
    debug_assert_eq!(node_counts.len(), edge_counts.len());
    debug_assert_eq!(node_counts.len(), file_ids.len());

    let mut plans = Vec::with_capacity(node_counts.len());
    let mut node_cursor = node_offset;
    let mut string_cursor = string_offset;
    let mut total_edges: u64 = 0;

    for i in 0..node_counts.len() {
        let nc = node_counts[i];
        let sc = string_counts[i];

        let node_end = node_cursor
            .checked_add(nc)
            .expect("node ID space overflow in commit plan");
        let string_end = string_cursor
            .checked_add(sc)
            .expect("string ID space overflow in commit plan");

        plans.push(FilePlan {
            parsed_index: i,
            file_id: file_ids[i],
            node_range: node_cursor..node_end,
            string_range: string_cursor..string_end,
        });

        node_cursor = node_end;
        string_cursor = string_end;
        total_edges += u64::from(edge_counts[i]);
    }

    ChunkCommitPlan {
        file_plans: plans,
        total_nodes: node_cursor - node_offset,
        total_strings: string_cursor - string_offset,
        total_edges,
    }
}

/// Execute Phase 2: count + range assignment for a parsed chunk.
///
/// Extracts per-file counts from staging graphs and delegates to
/// [`compute_commit_plan`] for prefix-sum range assignment.
#[must_use]
pub fn phase2_assign_ranges(
    staging_graphs: &[&StagingGraph],
    file_ids: &[FileId],
    offsets: &GlobalOffsets,
) -> ChunkCommitPlan {
    let node_counts: Vec<u32> = staging_graphs
        .iter()
        .map(|sg| sg.node_count_u32())
        .collect();
    let string_counts: Vec<u32> = staging_graphs
        .iter()
        .map(|sg| sg.string_count_u32())
        .collect();
    let edge_counts: Vec<u32> = staging_graphs
        .iter()
        .map(|sg| sg.edge_count_u32())
        .collect();

    compute_commit_plan(
        &node_counts,
        &string_counts,
        &edge_counts,
        file_ids,
        offsets.node_offset,
        offsets.string_offset,
    )
}

/// Phase 3 result: per-file edges, per-file node IDs, and total written
/// counts for validation.
pub struct Phase3Result {
    /// Per-file edge collections for Phase 4 bulk insert.
    pub per_file_edges: Vec<Vec<PendingEdge>>,
    /// Per-file node IDs actually committed. Indexed identically to
    /// `per_file_edges` — element `i` is the Vec of `NodeIds` committed
    /// for `plan.file_plans[i]`. Empty Vec when that file wrote no
    /// nodes (slot overflow skip, or a staging graph with only strings).
    ///
    /// Used by the caller to populate
    /// [`crate::graph::unified::storage::registry::FileRegistry::record_node`],
    /// which feeds the Gate 0c bucket-bijection debug invariant.
    pub per_file_node_ids: Vec<Vec<NodeId>>,
    /// Total nodes actually written (for validation against planned totals).
    pub total_nodes_written: usize,
    /// Total strings actually written (for validation against planned totals).
    pub total_strings_written: usize,
    /// Total edges collected across all files.
    pub total_edges_collected: usize,
    /// Per-chunk drained C indirect-call staging payloads (DESIGN §8.2).
    ///
    /// Populated when any file in the chunk staged a
    /// [`super::staging::CIndirectStagingPayload`] (C plugin Phase 1, U10).
    /// `None` for chunks containing no C files, keeping the wire-shape
    /// budget unchanged for non-C workspaces.
    ///
    /// Consumed by [`apply_c_indirect_drain`] from `entrypoint.rs` after
    /// Phase 4c-prime cross-file unification rebuilds the qualified-name
    /// index — see U11 plumbing for the full Phase 3 → Phase 4 hand-off.
    pub c_indirect_drain: Option<PhaseCIndirectDrain>,
}

/// Drained C indirect-call staging payload, resolved to owned `String`s.
///
/// The per-file
/// [`super::staging::CIndirectStagingPayload`] contains:
///   * `pending_address_taken_names: Vec<StringId>` — staging-local string
///     ids that we resolve to owned `String`s via `staging.resolve_local_string`
///     here so the post-4c-prime applier can re-intern through the canonical
///     interner without holding any staging-graph reference;
///   * `pending_struct_field_signatures: Vec<(String, String, String)>` —
///     already owned;
///   * `pending_bindings: Vec<PendingBinding>` — already owned;
///   * `pending_indirect_callsites: Vec<PendingIndirectCallsite>` — already
///     owned (carrier-side stamping of `FileId` happens here so the applier
///     does not need per-file context);
///   * `local_scope_index: Option<LocalScopeIndex>` — moved verbatim.
///
/// The applier ([`apply_c_indirect_drain`]) interns the owned strings into
/// the **post-Phase-4a-dedup** graph interner, resolves names to canonical
/// `NodeId`s via [`crate::graph::unified::storage::indices::AuxiliaryIndices::by_qualified_name`]
/// (with a `by_name` fallback for languages whose canonical qualified name
/// equals the semantic name and therefore leaves
/// [`NodeEntry::qualified_name`] unset — e.g. C, where `cb_alpha` is its
/// own qualified name), and writes them into
/// [`CodeGraph::c_indirect_tables_mut`].
///
/// Per DESIGN §8.2, this drain bridges the parallel-parse-and-commit
/// boundary (Phase 3) to the post-unification application step (Phase 4
/// finalisation, just after Phase 4c-prime returns).
#[derive(Debug, Default)]
pub struct PhaseCIndirectDrain {
    /// Address-taken function qualified-name entries to mark post-unification.
    ///
    /// Each entry pairs the bare/qualified function name the C plugin
    /// captured in `helper.mark_function_address_taken_by_name(...)` with
    /// the source `FileId` (always a C-language file by construction —
    /// only the C plugin populates `CIndirectStagingPayload`).
    ///
    /// Per DESIGN §8.2 lines 1239-1241: "A pending list of
    /// `(function_qualified_name, file_id)` for address-taken marks". The
    /// `file_id` is the *origin* file (where the address-take site lives),
    /// not the file of the resolved callable target. It is carried so the
    /// applier can constrain the workspace-global `by_name` fallback in
    /// [`crate::graph::unified::build::entrypoint::apply_deferred_address_taken_marks`]
    /// to candidate nodes whose own owning file's language is `C` — a
    /// non-C namesake (e.g. a Rust `fn cb_alpha`) must NOT be marked by
    /// the C-scoped contract of SPEC §3.1.2.
    ///
    /// Duplicates on `function_qualified_name` are tolerated —
    /// [`crate::graph::unified::storage::metadata::NodeMetadataStore::mark_address_taken`]
    /// is idempotent.
    pub address_taken_names: Vec<DeferredAddressTakenEntry>,
    /// `(struct_tag, field_name, signature)` triples — DESIGN §3.2.2.
    ///
    /// Drained verbatim from the staging payload. The applier interns each
    /// leg via `graph.strings_mut().intern(...)` and inserts into
    /// `CIndirectSideTables::struct_field_fnptr`.
    pub struct_field_signatures: Vec<(String, String, String)>,
    /// Binding-plane entries (DESIGN §7.1) paired with their origin `FileId`.
    ///
    /// The applier resolves `instance_name` and `target_fn_name` to
    /// canonical `NodeId`s and inserts a [`BindingEntry`] under the
    /// interned `(struct_tag, field_name)` key in
    /// `CIndirectSideTables::bindings_by_field`. The `FileId` is the
    /// origin file (the C TU that staged the binding), retained for the
    /// same C-language-scoped fallback rationale as
    /// [`Self::address_taken_names`].
    pub bindings: Vec<(FileId, PendingBinding)>,
    /// Indirect callsites paired with their owning `FileId`. The applier
    /// resolves `caller_qualified_name` to a `NodeId` and pushes an
    /// [`IndirectCallsite`] onto `CIndirectSideTables::pending_callsites`.
    /// `FileId` is stamped here from the per-file `FilePlan` so the applier
    /// does not need per-file context.
    pub indirect_callsites: Vec<(FileId, PendingIndirectCallsite)>,
    /// Per-file block-scope arenas (DESIGN §4.1). Moved verbatim into
    /// `CIndirectSideTables::local_scope_indices` keyed by `FileId`.
    pub local_scope_indices: Vec<(FileId, LocalScopeIndex)>,
}

/// One deferred address-taken mark, carrying the origin `FileId`
/// alongside the qualified function name (DESIGN §8.2 lines 1239-1241).
///
/// The origin `FileId` is always a C-language file by construction (only
/// the C plugin populates `CIndirectStagingPayload`). It is retained on
/// the drain so the post-unification applier can constrain the
/// workspace-global `by_name` fallback to candidate nodes whose own
/// owning file's language is `C` — defending against the SPEC §3.1.2
/// "Every C `NodeKind::Function`" contract being widened to mark
/// same-named non-C nodes (e.g. Rust `fn cb_alpha`, Python `def
/// cb_alpha`) that happen to share a bare name with a C symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredAddressTakenEntry {
    /// Qualified function name as captured by
    /// `helper.mark_function_address_taken_by_name(...)`.
    pub function_qualified_name: String,
    /// Origin C file that staged this address-taken site. Used only as
    /// metadata for DESIGN §8.2 conformance and provenance — the
    /// candidate-language filter in the applier compares each
    /// candidate's owning-file language to `Language::C`, not to this
    /// `file_id` directly (cross-TU address-takes are legal: a
    /// `cb_alpha` declared in `a.c` may have its address taken in
    /// `b.c`).
    pub file_id: FileId,
}

impl PhaseCIndirectDrain {
    /// Returns `true` when every drained vec/map is empty.
    ///
    /// Used by the chunk-accumulator in `entrypoint.rs` to skip Phase 4
    /// application entirely for non-C workspaces, keeping the
    /// `CodeGraph.c_indirect_tables` slot at its default `None`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.address_taken_names.is_empty()
            && self.struct_field_signatures.is_empty()
            && self.bindings.is_empty()
            && self.indirect_callsites.is_empty()
            && self.local_scope_indices.is_empty()
    }

    /// Merge another drain into this one, taking ownership of its contents.
    ///
    /// Used by the chunk-loop in `entrypoint.rs` to accumulate per-chunk
    /// drains into a single workspace-global drain before invoking
    /// [`apply_c_indirect_drain`].
    pub fn merge(&mut self, mut other: PhaseCIndirectDrain) {
        self.address_taken_names
            .append(&mut other.address_taken_names);
        self.struct_field_signatures
            .append(&mut other.struct_field_signatures);
        self.bindings.append(&mut other.bindings);
        self.indirect_callsites
            .append(&mut other.indirect_callsites);
        self.local_scope_indices
            .append(&mut other.local_scope_indices);
    }
}

/// Execute Phase 3: parallel commit into disjoint pre-allocated ranges.
///
/// Pre-splits arena and interner slices into per-file disjoint sub-slices
/// using `split_at_mut`, then uses `rayon` `par_iter` for lock-free parallel
/// writes. Each file's staging graph is committed independently.
///
/// Returns [`Phase3Result`] with per-file edges and written counts so the
/// caller can validate against plan totals and truncate allocations on
/// mismatch.
///
/// # Parameterisation over the mutation target
///
/// As of Task 4 Step 4 Phase 1, this function is generic over
/// `G: GraphMutationTarget`. At the full-build call site in
/// `build_unified_graph_inner` the target is `CodeGraph`; at the
/// Task 4 Step 4 Phase 2+ incremental rebuild call site the target
/// will be `RebuildGraph`. Both impls live in
/// [`crate::graph::unified::mutation_target`]; see that module's
/// docs for the field-coverage contract.
///
/// The function accesses exactly two fields via the trait —
/// [`GraphMutationTarget::nodes_and_strings_mut`] — and pre-splits
/// those two slices for the per-file parallel commit. Every other
/// piece of the pipeline (the CSR/delta edge store, auxiliary
/// indices, file registry, etc.) is untouched by this helper: the
/// `PendingEdge` vectors in the returned [`Phase3Result`] are
/// threaded through to Phase 4d (`pending_edges_to_delta` +
/// `BidirectionalEdgeStore::add_edges_bulk_ordered`) by the caller.
///
/// # Panics
///
/// Panics if `plan.total_nodes` or `plan.total_strings` exceeds the
/// pre-allocated range in the arena or interner.
#[must_use]
pub(crate) fn phase3_parallel_commit<
    G: crate::graph::unified::mutation_target::GraphMutationTarget,
>(
    plan: &ChunkCommitPlan,
    staging_graphs: &[&StagingGraph],
    graph: &mut G,
) -> Phase3Result {
    if plan.file_plans.is_empty() {
        return Phase3Result {
            per_file_edges: Vec::new(),
            per_file_node_ids: Vec::new(),
            total_nodes_written: 0,
            total_strings_written: 0,
            total_edges_collected: 0,
            c_indirect_drain: None,
        };
    }

    // Determine the start of the pre-allocated ranges.
    let node_start = plan.file_plans[0].node_range.start;
    let string_start = plan.file_plans[0].string_range.start;

    // Borrow the arena and interner disjointly via the mutation-plane
    // trait. This is the one-and-only field access this helper makes
    // on the graph; every downstream step operates on the resulting
    // slices without revisiting `graph`.
    let (arena, interner) = graph.nodes_and_strings_mut();

    // Get mutable slices covering the entire pre-allocated region.
    let node_slice = arena.bulk_slice_mut(node_start, plan.total_nodes);
    let (str_slice, rc_slice) = interner.bulk_slices_mut(string_start, plan.total_strings);

    // Pre-split into per-file disjoint sub-slices using split_at_mut.
    let mut node_remaining = &mut *node_slice;
    let mut str_remaining = &mut *str_slice;
    let mut rc_remaining = &mut *rc_slice;

    #[allow(clippy::type_complexity)]
    let mut file_work: Vec<(
        &mut [Slot<NodeEntry>],
        &mut [Option<Arc<str>>],
        &mut [u32],
        &FilePlan,
        usize,
    )> = Vec::with_capacity(plan.file_plans.len());

    for (i, file_plan) in plan.file_plans.iter().enumerate() {
        let nc = (file_plan.node_range.end - file_plan.node_range.start) as usize;
        let sc = (file_plan.string_range.end - file_plan.string_range.start) as usize;

        let (n, nr) = node_remaining.split_at_mut(nc);
        let (s, sr) = str_remaining.split_at_mut(sc);
        let (r, rr) = rc_remaining.split_at_mut(sc);

        file_work.push((n, s, r, file_plan, i));
        node_remaining = nr;
        str_remaining = sr;
        rc_remaining = rr;
    }

    // Parallel commit — each closure owns disjoint slices, no contention.
    let results: Vec<FileCommitResult> = file_work
        .into_par_iter()
        .map(|(node_slots, str_slots, rc_slots, file_plan, idx)| {
            commit_single_file(
                staging_graphs[idx],
                file_plan,
                node_slots,
                str_slots,
                rc_slots,
            )
        })
        .collect();

    let total_nodes_written: usize = results.iter().map(|r| r.nodes_written).sum();
    let total_strings_written: usize = results.iter().map(|r| r.strings_written).sum();
    let total_edges_collected: usize = results.iter().map(|r| r.edges.len()).sum();
    let mut per_file_edges = Vec::with_capacity(results.len());
    let mut per_file_node_ids = Vec::with_capacity(results.len());

    // Cluster B3 (Go T1): aggregate per-file remapped Go hints. Each
    // file's hints have already been remapped through that file's
    // local→global NodeId / StringId tables inside `commit_single_file`,
    // so the merge into the live target is a straightforward extend.
    let mut all_embedding_hints: Vec<GoEmbeddingHint> = Vec::new();
    let mut all_named_type_conversion_hints: Vec<GoNamedTypeConversionHint> = Vec::new();
    let mut all_receiver_call_hints: Vec<GoReceiverCallHint> = Vec::new();
    // Cluster D2.1: receiver-pointerness per Go method declaration. Same
    // commit-time NodeId / StringId remap discipline as the other three
    // vectors above; the consumer is Cluster D2's T1.1 pass and D2's
    // tightening of D1's bucket classifier.
    let mut all_method_receiver_hints: Vec<GoMethodReceiverHint> = Vec::new();
    // Cluster D3 (Go T1): canonical signatures per Go method / function
    // / named function-type declaration. Same remap discipline as the
    // four hint vectors above. Consumer is the tightened T1.1
    // satisfaction predicate and the new T1.3 function-signature
    // implementation pass.
    let mut all_method_signature_hints: Vec<GoMethodSignatureHint> = Vec::new();
    let mut all_function_signature_hints: Vec<GoFunctionSignatureHint> = Vec::new();

    for r in results {
        per_file_edges.push(r.edges);
        per_file_node_ids.push(r.node_ids);
        all_embedding_hints.extend(r.embedding_hints);
        all_named_type_conversion_hints.extend(r.named_type_conversion_hints);
        all_receiver_call_hints.extend(r.receiver_call_hints);
        all_method_receiver_hints.extend(r.method_receiver_hints);
        all_method_signature_hints.extend(r.method_signature_hints);
        all_function_signature_hints.extend(r.function_signature_hints);
    }

    // Cluster B3 / D2.1 / D3: merge aggregated hints into the live
    // target. This closes the deferred wire-through noted in Cluster A:
    // every per-file `StagingGraph::go_hints` now lands in the live
    // target's `GoHints` buffer during Phase 3, with all NodeId /
    // StringId references remapped to global identities. The
    // post-Phase-4e `pass_go_method_set_satisfaction` will drain this
    // buffer.
    if !all_embedding_hints.is_empty()
        || !all_named_type_conversion_hints.is_empty()
        || !all_receiver_call_hints.is_empty()
        || !all_method_receiver_hints.is_empty()
        || !all_method_signature_hints.is_empty()
        || !all_function_signature_hints.is_empty()
    {
        let go_hints = graph.go_hints_mut();
        go_hints.embeddings.extend(all_embedding_hints);
        go_hints
            .named_type_conversions
            .extend(all_named_type_conversion_hints);
        go_hints.receiver_calls.extend(all_receiver_call_hints);
        go_hints.method_receivers.extend(all_method_receiver_hints);
        go_hints
            .method_signatures
            .extend(all_method_signature_hints);
        go_hints
            .function_signatures
            .extend(all_function_signature_hints);
    }

    // --- C indirect-call drain (DESIGN §8.2 / U11) ---
    //
    // Sequentially walk the per-file staging graphs and drain each
    // `CIndirectStagingPayload` into a single per-chunk
    // [`PhaseCIndirectDrain`]. Sequential (not parallel) because: (a) the
    // payloads are typically tiny — even a large C TU stages ~tens of
    // bindings + ~dozens of callsites — and (b) the address-taken names
    // need their staging-local `StringId`s resolved to owned strings while
    // we still hold the source `StagingGraph` reference; once Phase 3
    // returns, the chunk-local `ParsedFile`s drop and the staged strings
    // become unrecoverable.
    //
    // Local-string resolution: `pending_address_taken_names` contains
    // staging-local `StringId`s interned by `helper.intern(name)` (see
    // `helper::mark_function_address_taken_by_name`). The applier needs
    // the underlying `&str` to re-intern through the **post-dedup** graph
    // interner, so we resolve here via `staging.resolve_local_string`.
    // The string already had its `intern` ref-count bumped on stage —
    // the post-4c-prime applier's re-intern produces the canonical global
    // `StringId` independent of the staging-local id.
    let c_indirect_drain = collect_c_indirect_drain(plan, staging_graphs);

    Phase3Result {
        per_file_edges,
        per_file_node_ids,
        total_nodes_written,
        total_strings_written,
        total_edges_collected,
        c_indirect_drain,
    }
}

/// Drain per-file C indirect-call staging payloads from the chunk.
///
/// Returns `Some(PhaseCIndirectDrain)` when at least one file in the chunk
/// staged a `CIndirectStagingPayload`; otherwise `None` (non-C workspaces).
/// Sequential rather than parallel — see commentary in
/// [`phase3_parallel_commit`] for the rationale.
fn collect_c_indirect_drain(
    plan: &ChunkCommitPlan,
    staging_graphs: &[&StagingGraph],
) -> Option<PhaseCIndirectDrain> {
    debug_assert_eq!(plan.file_plans.len(), staging_graphs.len());

    let mut drain = PhaseCIndirectDrain::default();

    for (file_plan, staging) in plan.file_plans.iter().zip(staging_graphs.iter()) {
        let Some(payload) = staging.c_indirect() else {
            continue;
        };

        // Resolve local string ids → owned Strings for address-taken names.
        // A `None` from `resolve_local_string` would indicate a staging-API
        // misuse (a non-local id was pushed). Skip with a warn rather than
        // panic so a buggy plugin can't take down the build pipeline.
        //
        // Each captured entry pairs the resolved name with the origin
        // `file_plan.file_id` (DESIGN §8.2 lines 1239-1241), allowing the
        // post-unification applier to scope the workspace-global by_name
        // fallback to C-language nodes.
        for &local_id in &payload.pending_address_taken_names {
            match staging.resolve_local_string(local_id) {
                Some(name) => drain.address_taken_names.push(DeferredAddressTakenEntry {
                    function_qualified_name: name.to_owned(),
                    file_id: file_plan.file_id,
                }),
                None => log::warn!(
                    "Phase 3 C-indirect drain: address-taken name local string id \
                     {:?} did not resolve in staging graph for file {:?} — skipping. \
                     This indicates the C plugin staged a non-local StringId via \
                     helper.mark_function_address_taken_by_name.",
                    local_id,
                    file_plan.file_id,
                ),
            }
        }

        // `pending_struct_field_signatures` is already `Vec<(String, String, String)>`
        // — clone the triple set into the drain. We clone (rather than
        // mutate-take) because `staging` is a `&StagingGraph` shared borrow.
        drain
            .struct_field_signatures
            .extend(payload.pending_struct_field_signatures.iter().cloned());

        // `pending_bindings` is `Vec<PendingBinding>` (owned Strings).
        // Stamp each binding with its origin `file_plan.file_id` so the
        // post-unification applier can scope the by_name fallback for
        // `instance_name` / `target_fn_name` resolution to C-language
        // nodes (same rationale as `address_taken_names` above).
        drain.bindings.extend(
            payload
                .pending_bindings
                .iter()
                .cloned()
                .map(|b| (file_plan.file_id, b)),
        );

        // Stamp each indirect callsite with its FileId from the plan, then
        // append. The applier needs the FileId to construct the persisted
        // `IndirectCallsite` (which carries `caller: NodeId` + `file_id:
        // FileId` rather than staging's `caller_qualified_name: String`).
        drain.indirect_callsites.extend(
            payload
                .pending_indirect_callsites
                .iter()
                .cloned()
                .map(|cs| (file_plan.file_id, cs)),
        );

        // Move the per-file scope index by clone (we hold `&StagingGraph`,
        // so cannot take). `LocalScopeIndex: Clone` — see
        // `c_indirect/scope_index.rs:21` documentation header.
        if let Some(scope_index) = payload.local_scope_index.as_ref() {
            drain
                .local_scope_indices
                .push((file_plan.file_id, scope_index.clone()));
        }
    }

    if drain.is_empty() { None } else { Some(drain) }
}

/// Commit a single file's staging graph into pre-allocated disjoint ranges.
///
/// This function operates on slices that belong exclusively to this file,
/// so it requires no locks or synchronization.
///
/// # Steps
///
/// 1. **Strings**: Extract `InternString` ops, write `Arc<str>` values into
///    pre-allocated string slots, build local→global `StringId` remap.
/// 2. **Nodes**: Extract `AddNode` ops, apply string remap to each `NodeEntry`,
///    set `file_id`, write into pre-allocated node slots, build expected→actual
///    `NodeId` remap.
/// 3. **Edges**: Extract `AddEdge` ops, apply node ID remap to source/target,
///    assign pre-computed sequence numbers, return as `PendingEdge` vec.
// Result of committing a single file: edges + committed NodeIds + actual written counts.
struct FileCommitResult {
    edges: Vec<PendingEdge>,
    /// Every `NodeId` committed into the arena for this file, in
    /// commit order. Used by the sequential post-commit step that
    /// populates `FileRegistry::per_file_nodes`.
    node_ids: Vec<NodeId>,
    nodes_written: usize,
    strings_written: usize,
    /// Cluster B3 (Go T1 implements-and-promotion): per-file
    /// [`GoEmbeddingHint`] / [`GoNamedTypeConversionHint`] /
    /// [`GoReceiverCallHint`] entries, with their staging-local
    /// `NodeId` / `StringId` fields remapped to global identities via
    /// the same tables that drive node + edge commit. The sequential
    /// post-rayon step in [`phase3_parallel_commit`] aggregates these
    /// across files and flushes the result into
    /// [`crate::graph::unified::mutation_target::GraphMutationTarget::go_hints_mut`].
    ///
    /// Non-Go staging graphs leave all four vectors empty — no work
    /// is performed for them.
    embedding_hints: Vec<GoEmbeddingHint>,
    named_type_conversion_hints: Vec<GoNamedTypeConversionHint>,
    receiver_call_hints: Vec<GoReceiverCallHint>,
    /// Cluster D2.1: per-method receiver-pointerness hints recovered from
    /// the Go plugin's Phase-1 method emission sites. `method_node` and
    /// `receiver_type_qualified_name` are remapped via the per-file
    /// remap tables in the same `commit_single_file` step that drives
    /// the other three hint vectors.
    method_receiver_hints: Vec<GoMethodReceiverHint>,
    /// Cluster D3: canonical-signature hints for Go method declarations
    /// (top-level methods and interface methods). `method_node` is
    /// remapped via the per-file node table. `canonical_signature` is a
    /// plain `String` so no `StringId` remap is needed.
    method_signature_hints: Vec<GoMethodSignatureHint>,
    /// Cluster D3: canonical-signature hints for Go function
    /// declarations and named function-type declarations. `function_node`
    /// is remapped via the per-file node table; `canonical_signature` is
    /// plain text.
    function_signature_hints: Vec<GoFunctionSignatureHint>,
}

fn commit_single_file(
    staging: &StagingGraph,
    plan: &FilePlan,
    node_slots: &mut [Slot<NodeEntry>],
    str_slots: &mut [Option<Arc<str>>],
    rc_slots: &mut [u32],
) -> FileCommitResult {
    let ops = staging.operations();

    // --- Step 1: Write strings, build local→global remap ---
    let (string_remap, strings_written) = write_strings(ops, plan, str_slots, rc_slots);

    // --- Step 2: Write nodes, build expected→actual node ID remap ---
    let (node_remap, nodes_written, node_ids) = write_nodes(ops, plan, node_slots, &string_remap);

    // --- Step 3: Collect remapped edges with pre-assigned sequence numbers ---
    let edges = collect_edges(ops, plan, &node_remap, &string_remap);

    // --- Step 4 (Cluster B3 / D2.1 / D3): Remap Go side-channel hints ---
    //
    // The Go plugin captures staging-local NodeId / StringId values in
    // GoHints during Phase-1 parse. Both ID spaces are file-local until
    // Phase 3 writes the file's nodes and strings into the globally
    // assigned ranges; once node_remap / string_remap exist, every hint
    // gets the same identity rewrite as a PendingEdge.
    let RemappedGoHints {
        embeddings: embedding_hints,
        named_type_conversions: named_type_conversion_hints,
        receiver_calls: receiver_call_hints,
        method_receivers: method_receiver_hints,
        method_signatures: method_signature_hints,
        function_signatures: function_signature_hints,
    } = remap_go_hints(staging, &node_remap, &string_remap, plan);

    FileCommitResult {
        edges,
        node_ids,
        nodes_written,
        strings_written,
        embedding_hints,
        named_type_conversion_hints,
        receiver_call_hints,
        method_receiver_hints,
        method_signature_hints,
        function_signature_hints,
    }
}

/// Bundle returned by [`remap_go_hints`] so the per-file commit step can
/// destructure without juggling a tuple of six vectors. Each field
/// matches its sibling on [`FileCommitResult`].
struct RemappedGoHints {
    embeddings: Vec<GoEmbeddingHint>,
    named_type_conversions: Vec<GoNamedTypeConversionHint>,
    receiver_calls: Vec<GoReceiverCallHint>,
    method_receivers: Vec<GoMethodReceiverHint>,
    method_signatures: Vec<GoMethodSignatureHint>,
    function_signatures: Vec<GoFunctionSignatureHint>,
}

/// Remap a `NodeId` through the per-file node-remap table.
///
/// Hint construction in the plugin uses staging-local `NodeIds` (assigned
/// by `StagingGraph::add_node` / equivalents). After Phase 3 commit the
/// canonical `NodeId` for each staged node lives in `node_remap`; the
/// remap is identity for already-global IDs.
fn remap_node_id_hint(id: NodeId, node_remap: &HashMap<NodeId, NodeId>) -> NodeId {
    node_remap.get(&id).copied().unwrap_or(id)
}

/// Remap a `StringId` through the per-file string-remap table.
///
/// Local-tagged staging `StringIds` are mapped to their global slot ID;
/// already-global IDs are passed through unchanged.
fn remap_string_id_hint(id: StringId, string_remap: &HashMap<StringId, StringId>) -> StringId {
    if id.is_local() {
        string_remap.get(&id).copied().unwrap_or(id)
    } else {
        id
    }
}

fn remap_receiver_hint(
    receiver: &GoReceiverHintKind,
    node_remap: &HashMap<NodeId, NodeId>,
) -> GoReceiverHintKind {
    match receiver {
        GoReceiverHintKind::LocalIdent { binding_local } => GoReceiverHintKind::LocalIdent {
            binding_local: remap_node_id_hint(*binding_local, node_remap),
        },
        // The Type-/Pointer-Prefixed and CallReturn variants carry plain
        // `String` text, so no ID remap is required.
        GoReceiverHintKind::TypePrefixed { type_text } => GoReceiverHintKind::TypePrefixed {
            type_text: type_text.clone(),
        },
        GoReceiverHintKind::PointerPrefixed { type_text } => GoReceiverHintKind::PointerPrefixed {
            type_text: type_text.clone(),
        },
        GoReceiverHintKind::CallReturn { callee_qn } => GoReceiverHintKind::CallReturn {
            callee_qn: callee_qn.clone(),
        },
    }
}

/// Drain the staging graph's Go hint vectors, remap each entry's
/// staging-local `NodeId` / `StringId` fields through the per-file
/// remap tables built by Phase 3 commit, and return four globally-
/// addressable vectors ready to be merged into the live target.
///
/// Non-Go staging graphs return empty vectors with no allocations
/// beyond the empty `Vec::new()` headers.
fn remap_go_hints(
    staging: &StagingGraph,
    node_remap: &HashMap<NodeId, NodeId>,
    string_remap: &HashMap<StringId, StringId>,
    plan: &FilePlan,
) -> RemappedGoHints {
    let hints = staging.go_hints();
    if hints.embeddings.is_empty()
        && hints.named_type_conversions.is_empty()
        && hints.receiver_calls.is_empty()
        && hints.method_receivers.is_empty()
        && hints.method_signatures.is_empty()
        && hints.function_signatures.is_empty()
    {
        return RemappedGoHints {
            embeddings: Vec::new(),
            named_type_conversions: Vec::new(),
            receiver_calls: Vec::new(),
            method_receivers: Vec::new(),
            method_signatures: Vec::new(),
            function_signatures: Vec::new(),
        };
    }

    let embeddings: Vec<GoEmbeddingHint> = hints
        .embeddings
        .iter()
        .map(|h| GoEmbeddingHint {
            outer: remap_node_id_hint(h.outer, node_remap),
            inner_qualified_name: remap_string_id_hint(h.inner_qualified_name, string_remap),
            pointerness: h.pointerness,
            file: plan.file_id,
        })
        .collect();

    let named_type_conversions: Vec<GoNamedTypeConversionHint> = hints
        .named_type_conversions
        .iter()
        .map(|h| GoNamedTypeConversionHint {
            call_site: remap_node_id_hint(h.call_site, node_remap),
            target_type_qualified_name: remap_string_id_hint(
                h.target_type_qualified_name,
                string_remap,
            ),
            argument_node: remap_node_id_hint(h.argument_node, node_remap),
            file: plan.file_id,
        })
        .collect();

    let receiver_calls: Vec<GoReceiverCallHint> = hints
        .receiver_calls
        .iter()
        .map(|h| GoReceiverCallHint {
            call_site: remap_node_id_hint(h.call_site, node_remap),
            callee_method: remap_node_id_hint(h.callee_method, node_remap),
            method_name: remap_string_id_hint(h.method_name, string_remap),
            receiver: remap_receiver_hint(&h.receiver, node_remap),
            argument_count: h.argument_count,
            is_async: h.is_async,
            file: plan.file_id,
        })
        .collect();

    let method_receivers: Vec<GoMethodReceiverHint> = hints
        .method_receivers
        .iter()
        .map(|h| GoMethodReceiverHint {
            method_node: remap_node_id_hint(h.method_node, node_remap),
            receiver_type_qualified_name: remap_string_id_hint(
                h.receiver_type_qualified_name,
                string_remap,
            ),
            receiver_pointerness: h.receiver_pointerness,
            file: plan.file_id,
        })
        .collect();

    // Cluster D3: method-signature and function-signature hints. Only
    // the NodeId field requires remap; `canonical_signature` is a plain
    // `String` produced by `canonicalise_go_signature` and is identity
    // across the commit boundary.
    let method_signatures: Vec<GoMethodSignatureHint> = hints
        .method_signatures
        .iter()
        .map(|h| GoMethodSignatureHint {
            method_node: remap_node_id_hint(h.method_node, node_remap),
            canonical_signature: h.canonical_signature.clone(),
            file: plan.file_id,
        })
        .collect();

    let function_signatures: Vec<GoFunctionSignatureHint> = hints
        .function_signatures
        .iter()
        .map(|h| GoFunctionSignatureHint {
            function_node: remap_node_id_hint(h.function_node, node_remap),
            canonical_signature: h.canonical_signature.clone(),
            file: plan.file_id,
        })
        .collect();

    RemappedGoHints {
        embeddings,
        named_type_conversions,
        receiver_calls,
        method_receivers,
        method_signatures,
        function_signatures,
    }
}

/// Write staged strings into pre-allocated interner slots.
///
/// Validates that each `InternString` op has a local `StringId` and that
/// no duplicate local IDs exist (matching the serial `commit_strings` checks).
///
/// Returns `(remap, strings_written)`.
fn write_strings(
    ops: &[StagingOp],
    plan: &FilePlan,
    str_slots: &mut [Option<Arc<str>>],
    rc_slots: &mut [u32],
) -> (HashMap<StringId, StringId>, usize) {
    let mut remap = HashMap::new();
    let mut string_cursor = 0usize;

    for op in ops {
        if let StagingOp::InternString { local_id, value } = op {
            // Validate: only local IDs are allowed in staging (matching serial commit_strings)
            assert!(
                local_id.is_local(),
                "non-local StringId {:?} in InternString op for file {:?}",
                local_id,
                plan.file_id,
            );
            // Validate: no duplicate local IDs (matching serial commit_strings)
            assert!(
                !remap.contains_key(local_id),
                "duplicate local StringId {:?} in InternString op for file {:?}",
                local_id,
                plan.file_id,
            );

            if string_cursor >= str_slots.len() {
                log::warn!(
                    "string slot overflow in file {:?}: cursor={string_cursor}, slots={}, skipping remaining strings",
                    plan.file_id,
                    str_slots.len()
                );
                break;
            }

            // The global StringId for this string is the pre-allocated slot index.
            #[allow(clippy::cast_possible_truncation)] // cursor is bounded by allocated slot count
            let global_id = StringId::new(plan.string_range.start + string_cursor as u32);

            // Write the string into the pre-allocated slot.
            str_slots[string_cursor] = Some(Arc::from(value.as_str()));
            rc_slots[string_cursor] = 1;

            remap.insert(*local_id, global_id);
            string_cursor += 1;
        }
    }

    (remap, string_cursor)
}

/// Remap all `StringId` fields in a `NodeEntry` using a local→global table.
///
/// Required field (`name`) is always remapped if local.
/// Optional fields (`signature`, `doc`, `qualified_name`, `visibility`)
/// are remapped if present and local.
fn remap_node_entry_string_ids(entry: &mut NodeEntry, remap: &HashMap<StringId, StringId>) {
    remap_required_local(&mut entry.name, remap);
    remap_option_local(&mut entry.signature, remap);
    remap_option_local(&mut entry.doc, remap);
    remap_option_local(&mut entry.qualified_name, remap);
    remap_option_local(&mut entry.visibility, remap);
}

/// Remap all local `StringId` fields in an `EdgeKind`.
///
/// Uses the same exhaustive match as `remap_edge_kind_string_ids`, but
/// only remaps local IDs (those with `LOCAL_TAG_BIT` set).
#[allow(clippy::match_same_arms)]
fn remap_edge_kind_local_string_ids(kind: &mut EdgeKind, remap: &HashMap<StringId, StringId>) {
    match kind {
        EdgeKind::Imports { alias, .. } => remap_option_local(alias, remap),
        EdgeKind::Exports { alias, .. } => remap_option_local(alias, remap),
        EdgeKind::TypeOf { name, .. } => remap_option_local(name, remap),
        EdgeKind::TraitMethodBinding {
            trait_name,
            impl_type,
            ..
        } => {
            remap_required_local(trait_name, remap);
            remap_required_local(impl_type, remap);
        }
        EdgeKind::HttpRequest { url, .. } => remap_option_local(url, remap),
        EdgeKind::GrpcCall { service, method } => {
            remap_required_local(service, remap);
            remap_required_local(method, remap);
        }
        EdgeKind::DbQuery { table, .. } => remap_option_local(table, remap),
        EdgeKind::TableRead { table_name, schema } => {
            remap_required_local(table_name, remap);
            remap_option_local(schema, remap);
        }
        EdgeKind::TableWrite {
            table_name, schema, ..
        } => {
            remap_required_local(table_name, remap);
            remap_option_local(schema, remap);
        }
        EdgeKind::TriggeredBy {
            trigger_name,
            schema,
        } => {
            remap_required_local(trigger_name, remap);
            remap_option_local(schema, remap);
        }
        EdgeKind::MessageQueue { protocol, topic } => {
            if let MqProtocol::Other(s) = protocol {
                remap_required_local(s, remap);
            }
            remap_option_local(topic, remap);
        }
        EdgeKind::WebSocket { event } => remap_option_local(event, remap),
        EdgeKind::GraphQLOperation { operation } => remap_required_local(operation, remap),
        EdgeKind::ProcessExec { command } => remap_required_local(command, remap),
        EdgeKind::FileIpc { path_pattern } => remap_option_local(path_pattern, remap),
        EdgeKind::ProtocolCall { protocol, metadata } => {
            remap_required_local(protocol, remap);
            remap_option_local(metadata, remap);
        }
        // T2.5: remap each TypeArg.name local StringId.
        EdgeKind::Instantiates { type_args, .. } => {
            for ta in type_args.iter_mut() {
                remap_required_local(&mut ta.name, remap);
            }
        }
        // Variants without StringId fields — exhaustive, no wildcard.
        EdgeKind::Defines
        | EdgeKind::Contains
        | EdgeKind::Calls { .. }
        | EdgeKind::References
        | EdgeKind::Inherits
        | EdgeKind::Implements
        | EdgeKind::LifetimeConstraint { .. }
        | EdgeKind::MacroExpansion { .. }
        | EdgeKind::FfiCall { .. }
        | EdgeKind::WebAssemblyCall
        | EdgeKind::GenericBound
        | EdgeKind::AnnotatedWith
        | EdgeKind::AnnotationParam
        | EdgeKind::LambdaCaptures
        | EdgeKind::ModuleExports
        | EdgeKind::ModuleRequires
        | EdgeKind::ModuleOpens
        | EdgeKind::ModuleProvides
        | EdgeKind::TypeArgument
        | EdgeKind::ExtensionReceiver
        | EdgeKind::CompanionOf
        | EdgeKind::SealedPermit
        // T3 Wraps carries WrapKind (Copy) + Option<u16>; no StringId fields.
        | EdgeKind::Wraps { .. }
        // T2.4 ChannelPeer carries only Copy enums; no StringId fields.
        | EdgeKind::ChannelPeer { .. } => {}
    }
}

/// Remap a required local `StringId` in place.
///
/// Panics if a local ID has no mapping, matching the serial
/// `apply_string_remap` behavior that returned `UnmappedLocalStringId`.
fn remap_required_local(id: &mut StringId, remap: &HashMap<StringId, StringId>) {
    if id.is_local() {
        let global = remap.get(id).unwrap_or_else(|| {
            panic!("unmapped local StringId {id:?} — missing intern_string op?")
        });
        *id = *global;
    }
}

/// Remap an optional local `StringId` in place.
fn remap_option_local(opt: &mut Option<StringId>, remap: &HashMap<StringId, StringId>) {
    if let Some(id) = opt
        && id.is_local()
    {
        let global = remap.get(id).unwrap_or_else(|| {
            panic!("unmapped local StringId {id:?} — missing intern_string op?")
        });
        *id = *global;
    }
}

/// Write staged nodes into pre-allocated arena slots.
///
/// Returns `(remap, nodes_written, node_ids)`. `node_ids` is the Vec of
/// every `NodeId` committed for this file, in commit order, for use by
/// the sequential bucket-population post-step.
fn write_nodes(
    ops: &[StagingOp],
    plan: &FilePlan,
    node_slots: &mut [Slot<NodeEntry>],
    string_remap: &HashMap<StringId, StringId>,
) -> (HashMap<NodeId, NodeId>, usize, Vec<NodeId>) {
    let mut node_remap = HashMap::new();
    let mut node_cursor = 0usize;
    let mut node_ids: Vec<NodeId> = Vec::with_capacity(node_slots.len());

    for op in ops {
        if let StagingOp::AddNode {
            entry, expected_id, ..
        } = op
        {
            if node_cursor >= node_slots.len() {
                log::warn!(
                    "node slot overflow in file {:?}: cursor={node_cursor}, slots={}, skipping remaining nodes",
                    plan.file_id,
                    node_slots.len()
                );
                break;
            }

            let mut entry = entry.clone();

            // Apply string remap to all StringId fields in the entry.
            remap_node_entry_string_ids(&mut entry, string_remap);

            // Set the file ID from the plan.
            entry.file = plan.file_id;

            // The actual NodeId is the pre-allocated slot index with generation 1.
            #[allow(clippy::cast_possible_truncation)] // cursor is bounded by allocated slot count
            let actual_index = plan.node_range.start + node_cursor as u32;
            let actual_id = NodeId::new(actual_index, 1);

            // Write into the pre-allocated slot.
            node_slots[node_cursor] = Slot::new_occupied(1, entry);

            if let Some(expected) = expected_id {
                node_remap.insert(*expected, actual_id);
            }

            node_ids.push(actual_id);
            node_cursor += 1;
        }
    }

    (node_remap, node_cursor, node_ids)
}

/// Collect staged edges with remapped node IDs, string IDs, and pre-assigned
/// sequence numbers.
fn collect_edges(
    ops: &[StagingOp],
    plan: &FilePlan,
    node_remap: &HashMap<NodeId, NodeId>,
    string_remap: &HashMap<StringId, StringId>,
) -> Vec<PendingEdge> {
    let mut edges = Vec::new();

    for op in ops {
        if let StagingOp::AddEdge {
            source,
            target,
            kind,
            spans,
            ..
        } = op
        {
            let actual_source = node_remap.get(source).copied().unwrap_or(*source);
            let actual_target = node_remap.get(target).copied().unwrap_or(*target);

            // Clone and remap any local StringIds in the EdgeKind.
            let mut remapped_kind = kind.clone();
            remap_edge_kind_local_string_ids(&mut remapped_kind, string_remap);

            edges.push(PendingEdge {
                source: actual_source,
                target: actual_target,
                kind: remapped_kind,
                file: plan.file_id,
                spans: spans.clone(),
            });
        }
    }

    edges
}

/// Remap a required `StringId` using the dedup remap table.
///
/// If the ID is in the remap table, it is replaced with the canonical ID.
/// Otherwise, it is left unchanged (identity mapping).
#[allow(clippy::implicit_hasher)]
pub fn remap_string_id(id: &mut StringId, remap: &HashMap<StringId, StringId>) {
    if let Some(&canonical) = remap.get(id) {
        *id = canonical;
    }
}

/// Remap an optional `StringId` using the dedup remap table.
#[allow(clippy::implicit_hasher)]
pub fn remap_option_string_id(id: &mut Option<StringId>, remap: &HashMap<StringId, StringId>) {
    if let Some(inner) = id {
        remap_string_id(inner, remap);
    }
}

/// Exhaustive remap of all `StringId` fields in an `EdgeKind`.
///
/// No wildcard arm — the compiler ensures completeness when new variants
/// are added to `EdgeKind`.
#[allow(clippy::match_same_arms, clippy::implicit_hasher)] // Arms are separated by category for documentation clarity
pub fn remap_edge_kind_string_ids(kind: &mut EdgeKind, remap: &HashMap<StringId, StringId>) {
    match kind {
        // === Variants WITH StringId fields ===
        EdgeKind::Imports { alias, .. } => remap_option_string_id(alias, remap),
        EdgeKind::Exports { alias, .. } => remap_option_string_id(alias, remap),
        EdgeKind::TypeOf { name, .. } => remap_option_string_id(name, remap),
        EdgeKind::TraitMethodBinding {
            trait_name,
            impl_type,
            ..
        } => {
            remap_string_id(trait_name, remap);
            remap_string_id(impl_type, remap);
        }
        EdgeKind::HttpRequest { url, .. } => remap_option_string_id(url, remap),
        EdgeKind::GrpcCall { service, method } => {
            remap_string_id(service, remap);
            remap_string_id(method, remap);
        }
        EdgeKind::DbQuery { table, .. } => remap_option_string_id(table, remap),
        EdgeKind::TableRead { table_name, schema } => {
            remap_string_id(table_name, remap);
            remap_option_string_id(schema, remap);
        }
        EdgeKind::TableWrite {
            table_name, schema, ..
        } => {
            remap_string_id(table_name, remap);
            remap_option_string_id(schema, remap);
        }
        EdgeKind::TriggeredBy {
            trigger_name,
            schema,
        } => {
            remap_string_id(trigger_name, remap);
            remap_option_string_id(schema, remap);
        }
        EdgeKind::MessageQueue { protocol, topic } => {
            if let MqProtocol::Other(s) = protocol {
                remap_string_id(s, remap);
            }
            remap_option_string_id(topic, remap);
        }
        EdgeKind::WebSocket { event } => remap_option_string_id(event, remap),
        EdgeKind::GraphQLOperation { operation } => remap_string_id(operation, remap),
        EdgeKind::ProcessExec { command } => remap_string_id(command, remap),
        EdgeKind::FileIpc { path_pattern } => remap_option_string_id(path_pattern, remap),
        EdgeKind::ProtocolCall { protocol, metadata } => {
            remap_string_id(protocol, remap);
            remap_option_string_id(metadata, remap);
        }
        // T2.5: remap each TypeArg.name StringId. This is the site actually
        // called during the Phase 4d edge-bulk-insert pipeline; without it
        // every TypeArg.name dangles after global string dedup.
        EdgeKind::Instantiates { type_args, .. } => {
            for ta in type_args.iter_mut() {
                remap_string_id(&mut ta.name, remap);
            }
        }
        // === Variants WITHOUT StringId fields — exhaustive, no wildcard ===
        EdgeKind::Defines
        | EdgeKind::Contains
        | EdgeKind::Calls { .. }
        | EdgeKind::References
        | EdgeKind::Inherits
        | EdgeKind::Implements
        | EdgeKind::LifetimeConstraint { .. }
        | EdgeKind::MacroExpansion { .. }
        | EdgeKind::FfiCall { .. }
        | EdgeKind::WebAssemblyCall
        | EdgeKind::GenericBound
        | EdgeKind::AnnotatedWith
        | EdgeKind::AnnotationParam
        | EdgeKind::LambdaCaptures
        | EdgeKind::ModuleExports
        | EdgeKind::ModuleRequires
        | EdgeKind::ModuleOpens
        | EdgeKind::ModuleProvides
        | EdgeKind::TypeArgument
        | EdgeKind::ExtensionReceiver
        | EdgeKind::CompanionOf
        | EdgeKind::SealedPermit
        // T3 Wraps carries WrapKind (Copy) + Option<u16>; no StringId fields.
        | EdgeKind::Wraps { .. }
        // T2.4 ChannelPeer carries only Copy enums; no StringId fields.
        | EdgeKind::ChannelPeer { .. } => {}
    }
}

// === Phase 4: Post-chunk Finalization ===

/// Apply global string dedup remap to all `StringId` fields in a `NodeEntry`.
///
/// This is the Phase 4 counterpart to `remap_node_entry_string_ids` (Phase 3).
/// Phase 3 remaps local→global; Phase 4 remaps duplicate global→canonical global.
#[allow(clippy::implicit_hasher)]
pub fn remap_node_entry_global(entry: &mut NodeEntry, remap: &HashMap<StringId, StringId>) {
    remap_string_id(&mut entry.name, remap);
    remap_option_string_id(&mut entry.signature, remap);
    remap_option_string_id(&mut entry.doc, remap);
    remap_option_string_id(&mut entry.qualified_name, remap);
    remap_option_string_id(&mut entry.visibility, remap);
}

/// Apply global string dedup remap to all nodes in the arena and all pending edges.
///
/// This is Phase 4b of the parallel commit pipeline. After `build_dedup_table()`
/// produces a remap table, this function applies it to every `StringId` in:
/// - All `NodeEntry` fields in the arena
/// - All `EdgeKind` fields in the pending edges
#[allow(clippy::implicit_hasher)]
pub fn phase4_apply_global_remap(
    arena: &mut NodeArena,
    all_edges: &mut [Vec<PendingEdge>],
    remap: &HashMap<StringId, StringId>,
) {
    if remap.is_empty() {
        return;
    }

    // Remap all nodes
    for (_id, entry) in arena.iter_mut() {
        remap_node_entry_global(entry, remap);
    }

    // Remap all edges
    for file_edges in all_edges.iter_mut() {
        for edge in file_edges.iter_mut() {
            remap_edge_kind_string_ids(&mut edge.kind, remap);
        }
    }
}

/// Statistics from Phase 4c-prime cross-file node unification.
#[derive(Debug, Default)]
pub struct UnificationStats {
    /// Total (`qualified_name`, kind) groups of size >= 2 examined.
    pub candidate_pairs_examined: usize,
    /// Number of loser nodes merged into winners.
    pub nodes_merged: usize,
    /// Number of `PendingEdge` fields rewritten.
    pub edges_rewritten: usize,
    /// Number of loser nodes (metadata merged into winners, slot kept inert).
    pub nodes_inert: usize,
    /// Time spent in the unification pass (milliseconds).
    pub elapsed_ms: u64,
}

fn collect_unification_path_keys<G>(
    graph: &G,
    groups_to_unify: &[Vec<NodeId>],
) -> HashMap<NodeId, String>
where
    G: crate::graph::unified::mutation_target::GraphMutationTarget,
{
    use crate::graph::unified::mutation_target::GraphMutationTarget;

    let arena = GraphMutationTarget::nodes(graph);
    let files = GraphMutationTarget::files(graph);
    let mut out = HashMap::with_capacity(groups_to_unify.iter().map(Vec::len).sum());
    for group in groups_to_unify {
        for &node_id in group {
            if out.contains_key(&node_id) {
                continue;
            }
            let key = arena
                .get(node_id)
                .and_then(|entry| files.resolve(entry.file))
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
            out.insert(node_id, key);
        }
    }
    out
}

fn select_unification_winner<G>(
    graph: &G,
    group: &[NodeId],
    path_keys: &HashMap<NodeId, String>,
    empty_path_key: &String,
) -> NodeId
where
    G: crate::graph::unified::mutation_target::GraphMutationTarget,
{
    use crate::graph::unified::mutation_target::GraphMutationTarget;

    *group
        .iter()
        .max_by(|&&a, &&b| {
            let ea = GraphMutationTarget::nodes(graph).get(a);
            let eb = GraphMutationTarget::nodes(graph).get(b);
            match (ea, eb) {
                (Some(ea), Some(eb)) => {
                    let a_real = ea.start_line > 0;
                    let b_real = eb.start_line > 0;
                    match (a_real, b_real) {
                        (true, false) => std::cmp::Ordering::Greater,
                        (false, true) => std::cmp::Ordering::Less,
                        _ => {
                            let a_range = ea.end_line.saturating_sub(ea.start_line);
                            let b_range = eb.end_line.saturating_sub(eb.start_line);
                            a_range
                                .cmp(&b_range)
                                .then_with(|| {
                                    let pa = path_keys.get(&a).unwrap_or(empty_path_key);
                                    let pb = path_keys.get(&b).unwrap_or(empty_path_key);
                                    pb.cmp(pa)
                                })
                                .then_with(|| b.index().cmp(&a.index()))
                        }
                    }
                }
                (Some(_), None) => std::cmp::Ordering::Greater,
                (None, Some(_)) => std::cmp::Ordering::Less,
                (None, None) => std::cmp::Ordering::Equal,
            }
        })
        .expect("group is non-empty")
}

/// Phase 4c-prime: Unify cross-file duplicate nodes sharing the same
/// canonical qualified name and a call-compatible kind.
///
/// Runs after `rebuild_indices` (Phase 4c) which populates `by_qualified_name`,
/// and before `pending_edges_to_delta` (Phase 4d) so the remap operates on
/// `PendingEdge` targets, not committed `DeltaEdge`s.
///
/// **Winner selection**: Among nodes sharing a qualified name and call-compatible
/// kinds, the node with `start_line > 0` wins. Tie-break in order:
///   1. Wider `end_line - start_line` span.
///   2. **Lexicographically smallest file path** (resolved via the rebuild
///      plane's [`FileRegistry`]). Phase 3e correctness requires the
///      path-based tie-break rather than the previous `FileId` comparison,
///      because `FileId` slot assignment differs between a fresh full
///      rebuild and an incremental rebuild — the incremental path clones
///      the existing `FileRegistry` and appends new paths, while the full
///      path assigns `FileIds` in filesystem-walk order from an empty
///      registry. Two builds of the same logical workspace therefore
///      disagree on which `FileId` is smaller when duplicate definitions
///      tie on span width, flipping the unification winner and stranding
///      `qualified_name` on the wrong side of the merge. Tie-breaking on
///      the (stable-across-builds) path makes winner selection
///      representation-independent.
///   3. Final fallback: smaller `NodeId::index()` when paths also tie
///      (e.g. two definitions in the same file — rare but possible via
///      duplicate declarations). `NodeId` is deterministic within a
///      single build so this keeps the fallback stable for any individual
///      build even if it isn't invariant across representations.
///
/// **Safety**: Caller must hold an exclusive write lock on the graph.
pub(crate) fn phase4c_prime_unify_cross_file_nodes<
    G: crate::graph::unified::mutation_target::GraphMutationTarget,
>(
    graph: &mut G,
    all_edges: &mut [Vec<PendingEdge>],
) -> (UnificationStats, super::unification::NodeRemapTable) {
    use crate::graph::unified::mutation_target::GraphMutationTarget;

    use super::helper::CALL_COMPATIBLE_KINDS;
    use super::unification::{NodeRemapTable, merge_node_into};
    use std::time::Instant;

    let start = Instant::now();
    let mut stats = UnificationStats::default();

    // Collect candidates: walk arena, group by qualified_name for nodes
    // with call-compatible kinds. Only groups of size >= 2 need unification.
    let mut qn_groups: HashMap<crate::graph::unified::string::StringId, Vec<NodeId>> =
        HashMap::new();

    for (node_id, entry) in GraphMutationTarget::nodes(graph).iter() {
        if !CALL_COMPATIBLE_KINDS.contains(&entry.kind) {
            continue;
        }
        if let Some(qn_id) = entry.qualified_name {
            qn_groups.entry(qn_id).or_default().push(node_id);
        }
    }

    // Filter to groups with 2+ members
    let groups_to_unify: Vec<Vec<NodeId>> = qn_groups
        .into_values()
        .filter(|group| {
            if group.len() >= 2 {
                stats.candidate_pairs_examined += 1;
                true
            } else {
                false
            }
        })
        .collect();

    // Now perform merges
    let mut remap = NodeRemapTable::with_capacity(groups_to_unify.len());

    // Pre-resolve every candidate node's canonical path-based tie-break
    // key into an owned `String` keyed by `NodeId`. Lifting the resolution
    // here instead of inside the `max_by` comparator avoids re-borrowing
    // `graph` immutably from a closure that lives across the
    // `merge_node_into(&mut graph, …)` call below. Without this
    // precomputation the borrow checker rejects the mutation loop because
    // the comparator closure captures the immutable borrow of `graph`
    // required by `path_key`.
    //
    // Path conversion goes through `Arc<Path>::to_string_lossy()` because
    // `Path` does not implement `Ord` lexicographically across platforms
    // consistently; forcing a canonical string form keeps the tie-break
    // deterministic on any host filesystem. When the registry can't
    // resolve a `FileId` (shouldn't happen in practice — every live
    // node's `FileId` was registered before the node was allocated) we
    // fall back to an empty string so the comparison still produces a
    // total order. Empty resolves tie-break each other stably (then fall
    // through to the `NodeId` index tie-break).
    let path_keys = collect_unification_path_keys(graph, &groups_to_unify);
    let empty_path_key = String::new();

    for group in &groups_to_unify {
        // Pick winner: prefer start_line > 0, tie-break by wider span,
        // then smaller path (stable across rebuild representations),
        // then smaller NodeId index.
        let winner_id = select_unification_winner(graph, group, &path_keys, &empty_path_key);

        // Merge all losers into winner
        for &node_id in group {
            if node_id == winner_id {
                continue;
            }
            match merge_node_into(GraphMutationTarget::nodes_mut(graph), node_id, winner_id) {
                Ok(()) => {
                    remap.insert(node_id, winner_id);
                    stats.nodes_merged += 1;
                    stats.nodes_inert += 1;
                }
                Err(e) => {
                    log::debug!("Phase 4c-prime: skipping merge ({e})");
                }
            }
        }
    }

    // Apply remap table to all pending edges AND to every committed
    // edge already in the graph's edge store.
    //
    // The `apply_to_edges` call keeps PendingEdges (the output of this
    // chunk's parallel commit) pointing at canonical winners before
    // Phase 4d converts them into `DeltaEdge`s. On a full build that is
    // sufficient — no committed edges exist yet.
    //
    // The `apply_to_committed_edges` call closes the Phase 3e incremental
    // gap: the rebuild plane clones the pre-edit graph's committed edges
    // via `clone_for_rebuild`, so a newly-reparsed file whose definition
    // becomes the unification winner can leave surviving cross-file
    // edges pointing at what is now an inert loser slot. Retargeting the
    // committed edges through `remap` is the only way those edges
    // observe the canonical winner after finalize. On a full build the
    // second call is a no-op (edge store is empty).
    if !remap.is_empty() {
        let pre_count: usize = all_edges.iter().map(std::vec::Vec::len).sum();
        remap.apply_to_edges(all_edges);
        remap.apply_to_committed_edges(GraphMutationTarget::edges(graph));
        stats.edges_rewritten = pre_count; // conservative: all edges walked

        // Keep FileRegistry::per_file_nodes consistent with the arena.
        //
        // [`merge_node_into`] (see `unification.rs`) intentionally does
        // **not** vacate the loser slot — the slot stays `Occupied` but
        // inert so `NodeArena::slot_count()` (which CSR row_ptr sizing
        // depends on) is preserved. Because the slot is still live per
        // `NodeArena::iter()`, the §F.1 bucket bijection would panic
        // with "live node absent from all buckets" if we purged losers
        // from their home bucket.
        //
        // Therefore: losers stay in whichever per-file bucket Phase 3
        // first committed them to. That bucket's `FileId` matches the
        // loser's `NodeEntry.file`, so (c) passes. Each loser is in
        // exactly one bucket, so (b) passes. Every live arena slot is
        // accounted for by some bucket, so (d) passes. The §K master
        // matrix already admits this semantics — inert merged-losers
        // are semantically equivalent to any other live `NodeArena`
        // entry for bucket-membership purposes.
        //
        // Name-resolution containment (Gate 0d iter-1 blocker).
        //
        // `merge_node_into` now ALSO clears the loser's `name` and
        // `qualified_name` fields (to `StringId::INVALID` / `None`), and
        // `AuxiliaryIndices::build_from_arena` skips any arena entry
        // whose `name == StringId::INVALID` when rebuilding the name,
        // qualified-name, kind, and file buckets. The second
        // `rebuild_indices()` call in `build_unified_graph_inner`
        // (entrypoint.rs:571, right below this function) runs AFTER
        // unification, so the buckets surfaced by `indices.by_name` /
        // `by_qualified_name` / `by_kind` / `by_file` contain only
        // winners — every public name-resolution surface
        // (`resolution::exact_qualified_bucket`,
        // `graph::find_by_pattern`, etc.) is therefore free of
        // unified-away duplicates. The only publish-visible bucket that
        // still references losers is `FileRegistry::per_file_nodes`,
        // which preserves the §F.1 bucket bijection without surfacing
        // them through name resolution.
        //
        // Historical note: an earlier iteration of this pass called
        // `retain_nodes_in_buckets` to purge losers; that matched a
        // stale understanding where `merge_node_into` was expected to
        // vacate the slot. Gate 0d's bucket-bijection invariant
        // surfaced the mismatch (every full rebuild produced a live
        // slot with no bucket entry). The fix is to align with the
        // unification contract: inert slots remain in their home
        // bucket, but `AuxiliaryIndices` treats them as name-invisible.
    }

    stats.elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    // Return the remap alongside the stats so the new Phase 4d-prime
    // (`phase4d_prime_propagate_staging_metadata`, 02_DESIGN §4.3.e
    // Changes 2 + 4) can drop loser-keyed metadata before merging the
    // per-file staging stores into `CodeGraph::macro_metadata`. The
    // `apply_to_edges` / `apply_to_committed_edges` calls above have
    // already consumed `remap` for edge retargeting; the returned table
    // is the same authoritative map used downstream.
    (stats, remap)
}

/// Rekey a per-file staging `NodeMetadataStore` from staging-local
/// `NodeId`s to canonical arena `NodeId`s using the per-file commit
/// order.
///
/// `02_DESIGN` §4.3.e Change 1 assumes staging metadata reaches Phase
/// 4d-prime under the arena `NodeIds` Phase 3 assigned. In practice
/// `StagingGraph::add_node` returns `NodeId::new(i, 1)` where `i` is the
/// staging-local sequential index (see `staging.rs:355`), and plugins
/// key their `NodeMetadataStore` entries under those staging-local IDs
/// (see e.g. the Rust plugin's `metadata_store.get_or_insert_default(func_id)`
/// at `sqry-lang-rust/src/macro_boundaries/proc_macro_classify.rs:84`).
/// Phase 3 then renumbers those into arena slots; `per_file_node_ids[i]`
/// is the arena `NodeId` for staging `NodeId(i, 1)`.
///
/// This helper rekeys each metadata entry by index: an entry under
/// staging `NodeId(i, 1)` is moved to `per_file_node_ids[i]`. Entries
/// whose staging index is out of bounds or whose generation is not the
/// staging-canonical `1` are dropped (defensive — should never happen
/// under the documented `StagingGraph::add_node` contract).
///
/// Returns a fresh arena-keyed [`NodeMetadataStore`]. The input is borrowed
/// because this helper only reads staging entries and clones the values that
/// survive rekeying into the returned store.
#[must_use]
pub(crate) fn rekey_staging_metadata_to_arena(
    staging_metadata: &crate::graph::unified::storage::metadata::NodeMetadataStore,
    per_file_node_ids: &[crate::graph::unified::node::id::NodeId],
) -> crate::graph::unified::storage::metadata::NodeMetadataStore {
    use crate::graph::unified::node::id::NodeId;
    use crate::graph::unified::storage::metadata::NodeMetadataStore;

    let mut rekeyed = NodeMetadataStore::new();
    for ((index, generation), entry) in staging_metadata.iter_entries() {
        // Defensive: staging.add_node always emits generation 1. Drop
        // any entry that does not match that contract; it cannot
        // correspond to a Phase 3 commit slot.
        if generation != 1 {
            continue;
        }
        let idx_usize = index as usize;
        let Some(&arena_id) = per_file_node_ids.get(idx_usize) else {
            // Stale key beyond the file's committed range — drop silently.
            continue;
        };
        let _ = NodeId::new(index, generation); // documentation: this was the staging-local id
        // Re-insert the whole `StoredEntry` (typed payload + flags) so both
        // `cfg_condition`/macro metadata AND synthetic markers survive the
        // staging-to-arena rekey.
        rekeyed.insert_entry(arena_id, entry.clone());
    }
    // Rekey shape descriptors the same way: staging keys them under
    // `NodeId(i, 1)`, the same staging-local index `i` that Phase 3 renumbered
    // into `per_file_node_ids[i]`. Without this, descriptors computed in the
    // staging store never reach the arena and the feature silently no-ops.
    for (&staging_id, descriptor) in staging_metadata.shape_descriptors() {
        if staging_id.generation() != 1 {
            continue;
        }
        let Some(&arena_id) = per_file_node_ids.get(staging_id.index() as usize) else {
            continue;
        };
        rekeyed.insert_shape_descriptor(arena_id, descriptor.clone());
    }
    rekeyed
}

/// Phase 4d-prime — propagate per-file staging `NodeMetadataStore` into
/// the live graph's `macro_metadata` after Phase 4d (bulk edge insert)
/// and before Phase 4e (binding-plane derivation).
///
/// `02_DESIGN` §4.3.e (Changes 4 + 7): the active Phase 3 commit path does
/// not read `staging.macro_metadata`, and `StagingGraph::take_macro_metadata`
/// was previously defined but never called — staging metadata never reached
/// `CodeGraph::macro_metadata`. T3.8's `cfg_condition` cannot ride the Go
/// plugin's parallel synthetic-flag channel (per-symbol metadata, not a
/// boolean bit on a known set of placeholders), so this sub-phase wires
/// the missing path.
///
/// For each `(file_id, store)` entry:
/// 1. Apply the Phase 4c-prime `NodeRemapTable` via
///    [`NodeRemapTable::apply_to_metadata_store`] so loser-keyed entries
///    are dropped (per `01_SPEC` §5.3.f the spec contract is "losers'
///    constraints are lost"; the winner's own per-file store carries the
///    authoritative metadata).
/// 2. If the store still has entries after the remap, call
///    [`NodeMetadataStore::merge`] into the graph's authoritative metadata
///    store.
///
/// Returns `true` when at least one entry was merged, `false` when every
/// staged store was empty or fully consumed by loser-drops. The boolean
/// is observed by the Phase 3d post-Pass-4d hook on the incremental
/// rebuild plane; production callers ignore it.
///
/// Generic over [`GraphMutationTarget`] so both the full-build
/// (`build_unified_graph_inner`) and incremental
/// (`incremental_rebuild` → `phase3d_insert_cross_file_edges`) planes
/// can call it against `CodeGraph` and `RebuildGraph` respectively.
///
/// Runs after Phase 4d (`NodeRemapTable` produced by 4c is final) and
/// before Phase 4e (binding-plane synthesis can observe `cfg_condition`
/// if it later needs to). The Rust plugin's existing `merge_macro_metadata`
/// call automatically benefits: Rust-side `#[cfg(...)]` strings start
/// flowing into the live snapshot for the first time as an incidental
/// fix of an existing latent gap.
#[must_use]
pub(crate) fn phase4d_prime_propagate_staging_metadata<G>(
    graph: &mut G,
    staged_metadata: Vec<(
        crate::graph::unified::file::id::FileId,
        crate::graph::unified::storage::metadata::NodeMetadataStore,
    )>,
    remap: &super::unification::NodeRemapTable,
) -> bool
where
    G: crate::graph::unified::mutation_target::GraphMutationTarget,
{
    let target = graph.macro_metadata_mut();
    let mut any_inserted = false;
    for (_file_id, mut metadata) in staged_metadata {
        remap.apply_to_metadata_store(&mut metadata);
        if !metadata.is_empty() {
            target.merge(&metadata);
            any_inserted = true;
        }
    }
    any_inserted
}

/// Convert per-file `PendingEdge` collections to per-file `DeltaEdge` collections
/// with monotonically increasing sequence numbers.
///
/// The sequence numbers are assigned file-by-file, edge-by-edge, starting from
/// `seq_start`. This produces the deterministic ordering required by
/// `BidirectionalEdgeStore::add_edges_bulk_ordered()`.
#[must_use]
pub fn pending_edges_to_delta(
    per_file_edges: &[Vec<PendingEdge>],
    seq_start: u64,
) -> (Vec<Vec<DeltaEdge>>, u64) {
    let mut seq = seq_start;
    let mut result = Vec::with_capacity(per_file_edges.len());

    for file_edges in per_file_edges {
        let mut delta_vec = Vec::with_capacity(file_edges.len());
        for edge in file_edges {
            delta_vec.push(DeltaEdge::with_spans(
                edge.source,
                edge.target,
                edge.kind.clone(),
                seq,
                DeltaOp::Add,
                edge.file,
                edge.spans.clone(),
            ));
            seq += 1;
        }
        result.push(delta_vec);
    }

    (result, seq)
}

/// Rebuild the auxiliary indices on `graph` from its current node arena.
///
/// Generic counterpart to the inherent [`CodeGraph::rebuild_indices`].
/// Takes a [`GraphMutationTarget`] so both the full-build
/// (`build_unified_graph_inner`) and incremental-rebuild
/// (`incremental_rebuild` on `RebuildGraph`) pipelines can share the
/// same helper. The inherent method now delegates here so the
/// implementation lives in exactly one place.
///
/// Internally uses [`GraphMutationTarget::nodes_and_indices_mut`] to
/// acquire a disjoint `(&NodeArena, &mut AuxiliaryIndices)` pair, then
/// hands them to [`AuxiliaryIndices::build_from_arena`] which clears
/// the existing indices and rebuilds in a single pass without
/// per-element duplicate checking.
///
/// [`CodeGraph::rebuild_indices`]: crate::graph::unified::concurrent::CodeGraph::rebuild_indices
/// [`AuxiliaryIndices::build_from_arena`]: crate::graph::unified::storage::indices::AuxiliaryIndices::build_from_arena
pub(crate) fn rebuild_indices<G: crate::graph::unified::mutation_target::GraphMutationTarget>(
    graph: &mut G,
) {
    let (nodes, indices) = graph.nodes_and_indices_mut();
    indices.build_from_arena(nodes);
}

/// Phase 4d — bulk-insert every pending edge into the graph via the
/// deterministic `DeltaEdge` conversion path.
///
/// Wraps the pure [`pending_edges_to_delta`] conversion + the
/// [`BidirectionalEdgeStore::add_edges_bulk_ordered`] call that
/// `build_unified_graph_inner` ran inline between Phase 4c-prime and
/// Phase 4e. The wrapper is generic over [`GraphMutationTarget`] so
/// the Task 4 Step 4 Phase 3 `incremental_rebuild` body can call it
/// against a [`RebuildGraph`] without duplicating the seq-counter +
/// flatten logic.
///
/// Returns the final edge sequence counter (for callers that need to
/// continue allocating deterministic sequence numbers downstream).
/// The counter flows from
/// [`BidirectionalEdgeStore::forward().seq_counter()`] on the way in
/// and advances by one per inserted edge.
///
/// # Semantics
///
/// * `per_file_edges` is consumed by-reference; the function does not
///   mutate the caller's buffer. Callers who no longer need the
///   vectors may drop them after the call.
/// * If `per_file_edges` is empty (or every inner vector is empty),
///   the edge store is left untouched.
/// * The helper does not `bump_epoch()` on the graph — Phase 4d is
///   edge-level only; the full pipeline bumps epoch separately.
///
/// # Edge-source-identity invariant (`C_EDGE_MIGRATE`)
///
/// Phase 4d does NOT dedup edges by `(source, target, kind)`. Every
/// `PendingEdge` from every file becomes one `DeltaEdge` with a unique
/// monotonically increasing `seq` number; the
/// [`BidirectionalEdgeStore::add_edges_bulk_ordered`] insertion contract
/// preserves that 1:1 mapping. This is what lets the Cluster C
/// `C_EDGE_MIGRATE` DAG unit (2026-04-29 `BadLiveware` Go batch) move the
/// `TypeOf{Field}` edge source from the struct node to the per-field
/// `Property` node without touching this helper: the new
/// Property-sourced edge addresses a distinct `(source, target)` pair
/// from the legacy struct-sourced edge, and Phase 4d emits both shapes
/// with no collapsing. Plugins that only emit the new shape (Go after
/// `C_EDGE_MIGRATE`) therefore produce a clean Property-sourced
/// `TypeOf{Field}` edge set with no struct-sourced shadows. Plugins
/// outside Cluster C's scope (`C_OTHER_PLUGINS`) keep emitting the
/// legacy shape until they migrate; the bulk-insert path treats both
/// shapes identically.
///
/// Determinism: per-file `PendingEdge` order is fixed by the parser
/// pass, and `pending_edges_to_delta` walks the per-file vectors in
/// the input order. So `phase4d_bulk_insert_edges` produces a
/// byte-identical `DeltaEdge` sequence on every fresh rebuild of the
/// same source tree, which is what guarantees the
/// `SnapshotReader → SnapshotWriter` round-trip identity required by
/// the `C_EDGE_MIGRATE` acceptance criteria.
///
/// [`BidirectionalEdgeStore::add_edges_bulk_ordered`]: crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore::add_edges_bulk_ordered
/// [`RebuildGraph`]: crate::graph::unified::rebuild::rebuild_graph::RebuildGraph
pub(crate) fn phase4d_bulk_insert_edges<
    G: crate::graph::unified::mutation_target::GraphMutationTarget,
>(
    graph: &mut G,
    per_file_edges: &[Vec<PendingEdge>],
) -> u64 {
    // Start seq numbering from the edge store's current counter to
    // support non-empty graphs (incremental rebuild carries forward
    // the prior build's counter).
    let edge_seq_start = graph.edges().forward().seq_counter();
    let (delta_edge_vecs, final_seq) = pending_edges_to_delta(per_file_edges, edge_seq_start);
    let total_edge_count: u64 = delta_edge_vecs.iter().map(|v| v.len() as u64).sum();
    if total_edge_count > 0 {
        graph
            .edges_mut()
            .add_edges_bulk_ordered(&delta_edge_vecs, total_edge_count);
    }
    final_seq
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_commit_plan_basic() {
        let file_ids = vec![FileId::new(0), FileId::new(1), FileId::new(2)];
        let node_counts = vec![3, 0, 5];
        let string_counts = vec![2, 1, 3];
        let edge_counts = vec![4, 0, 6];

        let plan = compute_commit_plan(
            &node_counts,
            &string_counts,
            &edge_counts,
            &file_ids,
            0,
            1, // string_offset=1 for sentinel
        );

        assert_eq!(plan.total_nodes, 8);
        assert_eq!(plan.total_strings, 6);
        assert_eq!(plan.total_edges, 10);

        // File 0: nodes [0..3), strings [1..3)
        assert_eq!(plan.file_plans[0].node_range, 0..3);
        assert_eq!(plan.file_plans[0].string_range, 1..3);

        // File 1: nodes [3..3), strings [3..4) — empty nodes
        assert_eq!(plan.file_plans[1].node_range, 3..3);
        assert_eq!(plan.file_plans[1].string_range, 3..4);

        // File 2: nodes [3..8), strings [4..7)
        assert_eq!(plan.file_plans[2].node_range, 3..8);
        assert_eq!(plan.file_plans[2].string_range, 4..7);
    }

    #[test]
    fn test_compute_commit_plan_with_offsets() {
        let file_ids = vec![FileId::new(5)];
        let plan = compute_commit_plan(&[10], &[5], &[7], &file_ids, 100, 50);
        assert_eq!(plan.file_plans[0].node_range, 100..110);
        assert_eq!(plan.file_plans[0].string_range, 50..55);
        assert_eq!(plan.total_nodes, 10);
        assert_eq!(plan.total_strings, 5);
        assert_eq!(plan.total_edges, 7);
    }

    #[test]
    fn test_compute_commit_plan_empty() {
        let plan = compute_commit_plan(&[], &[], &[], &[], 0, 1);
        assert_eq!(plan.total_nodes, 0);
        assert_eq!(plan.total_strings, 0);
        assert_eq!(plan.total_edges, 0);
        assert!(plan.file_plans.is_empty());
    }

    #[test]
    fn test_remap_string_id_basic() {
        let mut remap = HashMap::new();
        remap.insert(StringId::new(1), StringId::new(100));

        let mut id = StringId::new(1);
        remap_string_id(&mut id, &remap);
        assert_eq!(id, StringId::new(100));
    }

    #[test]
    fn test_remap_string_id_not_in_remap() {
        let remap = HashMap::new();
        let mut id = StringId::new(42);
        remap_string_id(&mut id, &remap);
        assert_eq!(id, StringId::new(42)); // unchanged
    }

    #[test]
    fn test_remap_option_string_id() {
        let mut remap = HashMap::new();
        remap.insert(StringId::new(5), StringId::new(50));

        let mut some_id = Some(StringId::new(5));
        remap_option_string_id(&mut some_id, &remap);
        assert_eq!(some_id, Some(StringId::new(50)));

        let mut none_id: Option<StringId> = None;
        remap_option_string_id(&mut none_id, &remap);
        assert_eq!(none_id, None);
    }

    #[test]
    fn test_remap_edge_kind_imports() {
        let mut remap = HashMap::new();
        remap.insert(StringId::new(1), StringId::new(100));

        let mut kind = EdgeKind::Imports {
            alias: Some(StringId::new(1)),
            is_wildcard: false,
        };
        remap_edge_kind_string_ids(&mut kind, &remap);
        assert!(
            matches!(kind, EdgeKind::Imports { alias: Some(id), .. } if id == StringId::new(100))
        );
    }

    #[test]
    fn test_remap_edge_kind_trait_method_binding() {
        let mut remap = HashMap::new();
        remap.insert(StringId::new(1), StringId::new(100));
        remap.insert(StringId::new(2), StringId::new(200));

        let mut kind = EdgeKind::TraitMethodBinding {
            trait_name: StringId::new(1),
            impl_type: StringId::new(2),
            is_ambiguous: false,
        };
        remap_edge_kind_string_ids(&mut kind, &remap);
        assert!(
            matches!(kind, EdgeKind::TraitMethodBinding { trait_name, impl_type, .. }
                if trait_name == StringId::new(100) && impl_type == StringId::new(200))
        );
    }

    #[test]
    fn test_remap_edge_kind_no_op_variants() {
        let remap = HashMap::new();

        // Defines — no StringId fields
        let mut kind = EdgeKind::Defines;
        remap_edge_kind_string_ids(&mut kind, &remap);
        assert!(matches!(kind, EdgeKind::Defines));

        // Calls — no StringId fields
        let mut kind = EdgeKind::Calls {
            argument_count: 3,
            is_async: true,
            resolved_via: ResolvedVia::Direct,
        };
        remap_edge_kind_string_ids(&mut kind, &remap);
        assert!(matches!(
            kind,
            EdgeKind::Calls {
                argument_count: 3,
                is_async: true,
                resolved_via: ResolvedVia::Direct,
            }
        ));
    }

    fn placeholder_entry() -> NodeEntry {
        use crate::graph::unified::node::NodeKind;
        NodeEntry::new(NodeKind::Function, StringId::new(0), FileId::new(0))
    }

    #[test]
    fn test_phase2_assign_ranges_basic() {
        use super::super::staging::StagingGraph;

        // Create 2 staging graphs with known counts
        let mut sg0 = StagingGraph::new();
        let mut sg1 = StagingGraph::new();

        // sg0: 2 nodes, 1 string, 1 edge
        let entry0 = placeholder_entry();
        let n0 = sg0.add_node(entry0.clone());
        let n1 = sg0.add_node(entry0.clone());
        sg0.intern_string(StringId::new_local(0), "hello".into());
        sg0.add_edge(
            n0,
            n1,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            FileId::new(0),
        );

        // sg1: 1 node, 2 strings, 0 edges
        sg1.add_node(entry0);
        sg1.intern_string(StringId::new_local(0), "world".into());
        sg1.intern_string(StringId::new_local(1), "foo".into());

        let file_ids = vec![FileId::new(10), FileId::new(11)];
        let offsets = GlobalOffsets {
            node_offset: 5,
            string_offset: 3,
        };

        let plan = phase2_assign_ranges(&[&sg0, &sg1], &file_ids, &offsets);

        // sg0: 2 nodes, 1 string, 1 edge
        assert_eq!(plan.file_plans[0].node_range, 5..7);
        assert_eq!(plan.file_plans[0].string_range, 3..4);

        // sg1: 1 node, 2 strings, 0 edges
        assert_eq!(plan.file_plans[1].node_range, 7..8);
        assert_eq!(plan.file_plans[1].string_range, 4..6);

        assert_eq!(plan.total_nodes, 3);
        assert_eq!(plan.total_strings, 3);
        assert_eq!(plan.total_edges, 1);
    }

    #[test]
    fn test_phase3_parallel_commit_basic() {
        use super::super::staging::StagingGraph;
        use crate::graph::unified::concurrent::CodeGraph;
        use crate::graph::unified::node::NodeKind;
        // The `nodes_mut` / `strings_mut` method calls below resolve
        // to inherent methods on `CodeGraph`; the `GraphMutationTarget`
        // trait impl provides the same surface for `RebuildGraph`
        // (see `phase3_parallel_commit_runs_against_rebuild_graph`).
        // No trait import is needed here because inherent-method
        // resolution wins for `CodeGraph`.

        // Create a staging graph with 2 nodes, 1 string, 1 edge
        let mut sg = StagingGraph::new();
        let local_name = StringId::new_local(0);
        sg.intern_string(local_name, "my_func".into());

        let entry = NodeEntry::new(NodeKind::Function, local_name, FileId::new(0));
        let n0 = sg.add_node(entry.clone());

        let entry2 = NodeEntry::new(NodeKind::Variable, local_name, FileId::new(0));
        let n1 = sg.add_node(entry2);

        sg.add_edge(
            n0,
            n1,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            FileId::new(0),
        );

        let file_ids = vec![FileId::new(5)];

        // Pre-allocate with non-zero offsets to verify remap works,
        // against a full `CodeGraph` so the new generic signature is
        // exercised end-to-end via `GraphMutationTarget`.
        let mut graph = CodeGraph::new();
        graph
            .nodes_mut()
            .alloc_range(10, &placeholder_entry())
            .unwrap();
        let string_start = graph.strings_mut().alloc_range(1).unwrap();
        assert_eq!(string_start, 1); // past sentinel

        let offsets = GlobalOffsets {
            node_offset: 10, // file's nodes start at index 10
            string_offset: string_start,
        };
        let plan = phase2_assign_ranges(&[&sg], &file_ids, &offsets);
        assert_eq!(plan.file_plans[0].node_range, 10..12);

        // Pre-allocate the actual ranges for Phase 3.
        graph
            .nodes_mut()
            .alloc_range(plan.total_nodes, &placeholder_entry())
            .unwrap();
        graph.strings_mut().alloc_range(plan.total_strings).unwrap();

        // Phase 3 — generic over `G: GraphMutationTarget`. Passing
        // `&mut graph` infers `G = CodeGraph`.
        let result = phase3_parallel_commit(&plan, &[&sg], &mut graph);

        // Verify written counts
        assert_eq!(result.total_nodes_written, 2);
        assert_eq!(result.total_strings_written, 1);

        // Verify strings were written
        let global_name = StringId::new(string_start);
        assert_eq!(&*graph.strings().resolve(global_name).unwrap(), "my_func");

        // Verify 1 file, 1 edge
        assert_eq!(result.per_file_edges.len(), 1);
        assert_eq!(result.per_file_edges[0].len(), 1);

        // Verify edge was remapped to global IDs (node_offset=10)
        let edge = &result.per_file_edges[0][0];
        assert_eq!(edge.file, FileId::new(5));
        assert_eq!(edge.source, NodeId::new(10, 1)); // first node at slot 10
        assert_eq!(edge.target, NodeId::new(11, 1)); // second node at slot 11

        // Gate 0c (iter-2 B2): per-file node IDs must be recorded in
        // commit order, one Vec per FilePlan, so the caller can
        // populate FileRegistry::per_file_nodes deterministically.
        assert_eq!(result.per_file_node_ids.len(), 1);
        assert_eq!(
            result.per_file_node_ids[0],
            vec![NodeId::new(10, 1), NodeId::new(11, 1)]
        );
    }

    #[test]
    fn test_phase3_parallel_commit_empty() {
        use crate::graph::unified::concurrent::CodeGraph;

        let mut graph = CodeGraph::new();

        let plan = ChunkCommitPlan {
            file_plans: vec![],
            total_nodes: 0,
            total_strings: 0,
            total_edges: 0,
        };

        let result = phase3_parallel_commit(&plan, &[], &mut graph);
        assert!(result.per_file_edges.is_empty());
        assert!(result.per_file_node_ids.is_empty());
        assert_eq!(result.total_nodes_written, 0);
        assert_eq!(result.total_strings_written, 0);
    }

    /// Task 4 Step 4 Phase 1 — exercise the `GraphMutationTarget`
    /// trait's second implementor.
    ///
    /// Builds a tiny staging graph, hosts it in a fresh `RebuildGraph`,
    /// and asserts the committed nodes land in the **rebuild-local**
    /// arena — not in a `CodeGraph`. The test also confirms the
    /// per-file edges / node-id vectors the helper returns agree with
    /// the `CodeGraph` call-path result shape.
    ///
    /// If a future refactor accidentally routed Phase 3 back to a
    /// `CodeGraph` (e.g. through a hidden static `Arc::make_mut`), this
    /// test would observe an empty rebuild arena and fail.
    #[test]
    #[cfg(feature = "rebuild-internals")]
    fn phase3_parallel_commit_runs_against_rebuild_graph() {
        use super::super::staging::StagingGraph;
        use crate::graph::unified::concurrent::CodeGraph;
        use crate::graph::unified::mutation_target::GraphMutationTarget;
        use crate::graph::unified::node::NodeKind;

        // Staging graph: 2 nodes + 1 string + 1 Calls edge (identical
        // shape to the CodeGraph test above, so any behavioural drift
        // between the two paths surfaces as different assertions).
        let mut sg = StagingGraph::new();
        let local_name = StringId::new_local(0);
        sg.intern_string(local_name, "rebuild_target".into());
        let entry = NodeEntry::new(NodeKind::Function, local_name, FileId::new(0));
        let n0 = sg.add_node(entry.clone());
        let entry2 = NodeEntry::new(NodeKind::Variable, local_name, FileId::new(0));
        let n1 = sg.add_node(entry2);
        sg.add_edge(
            n0,
            n1,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            FileId::new(0),
        );

        // Produce a RebuildGraph from an empty CodeGraph; drop the
        // CodeGraph immediately so any subsequent mutation observed in
        // the rebuild cannot possibly be leaking back to a shared Arc.
        let mut rebuild = {
            let graph = CodeGraph::new();
            graph.clone_for_rebuild()
        };

        // Pre-allocate leading slots on the rebuild-local arena +
        // interner so the file's ranges begin at a non-zero offset —
        // this is the same pattern the CodeGraph test uses, verifying
        // the trait's disjoint-borrow combinator threads through
        // identically.
        rebuild
            .nodes_mut()
            .alloc_range(10, &placeholder_entry())
            .unwrap();
        let string_start = rebuild.strings_mut().alloc_range(1).unwrap();
        assert_eq!(string_start, 1);

        let file_ids = vec![FileId::new(5)];
        let offsets = GlobalOffsets {
            node_offset: 10,
            string_offset: string_start,
        };
        let plan = phase2_assign_ranges(&[&sg], &file_ids, &offsets);

        rebuild
            .nodes_mut()
            .alloc_range(plan.total_nodes, &placeholder_entry())
            .unwrap();
        rebuild
            .strings_mut()
            .alloc_range(plan.total_strings)
            .unwrap();

        // Phase 3 against the RebuildGraph. Inferred `G = RebuildGraph`.
        let result = phase3_parallel_commit(&plan, &[&sg], &mut rebuild);

        // === Invariant: the written data lives in the rebuild-local
        // arena, not in any CodeGraph field. ===
        //
        // Two slot ranges exist on the rebuild's arena now:
        //   * slots 0..10 = pre-fill placeholders (each `Function` /
        //     `StringId::new(0)` — note every alloc_range writes a
        //     clone of the template entry).
        //   * slots 10..12 = the two committed nodes from `sg`.
        //
        // Fetch the two committed NodeIds and resolve their names
        // through the rebuild-local interner; the string must match
        // the staged value "rebuild_target", proving the commit ran
        // on the rebuild's own fields.
        let committed_ids = &result.per_file_node_ids[0];
        assert_eq!(
            committed_ids,
            &vec![NodeId::new(10, 1), NodeId::new(11, 1)],
            "Phase 3 must commit into slots 10..12 on the rebuild-local arena"
        );

        let resolved_name = rebuild
            .nodes_mut()
            .get(NodeId::new(10, 1))
            .map(|entry| entry.name)
            .expect("committed node must exist in rebuild arena");
        // The name StringId on the committed node is a global ID
        // (Phase 3 remaps local → global); resolving it through the
        // rebuild-local interner must produce the staged value.
        let resolved_str = rebuild
            .strings_mut()
            .resolve(resolved_name)
            .expect("name must resolve in rebuild-local interner");
        assert_eq!(&*resolved_str, "rebuild_target");

        // === Shape invariants match the CodeGraph path ===
        assert_eq!(result.total_nodes_written, 2);
        assert_eq!(result.total_strings_written, 1);
        assert_eq!(result.per_file_edges.len(), 1);
        assert_eq!(result.per_file_edges[0].len(), 1);
        let edge = &result.per_file_edges[0][0];
        assert_eq!(edge.file, FileId::new(5));
        assert_eq!(edge.source, NodeId::new(10, 1));
        assert_eq!(edge.target, NodeId::new(11, 1));
    }

    #[test]
    fn test_commit_single_file_string_remap() {
        use super::super::staging::StagingGraph;
        use crate::graph::unified::node::NodeKind;

        let mut sg = StagingGraph::new();
        let local_0 = StringId::new_local(0);
        let local_1 = StringId::new_local(1);
        sg.intern_string(local_0, "alpha".into());
        sg.intern_string(local_1, "beta".into());

        let mut entry = NodeEntry::new(NodeKind::Function, local_0, FileId::new(0));
        entry.signature = Some(local_1);
        sg.add_node(entry);

        let plan = FilePlan {
            parsed_index: 0,
            file_id: FileId::new(42),
            node_range: 10..11,
            string_range: 20..22,
        };

        let mut node_slots = vec![Slot::new_occupied(1, placeholder_entry())];
        let mut str_slots: Vec<Option<Arc<str>>> = vec![None, None];
        let mut rc_slots: Vec<u32> = vec![0, 0];

        let result = commit_single_file(&sg, &plan, &mut node_slots, &mut str_slots, &mut rc_slots);

        // Strings written
        assert_eq!(str_slots[0].as_deref(), Some("alpha"));
        assert_eq!(str_slots[1].as_deref(), Some("beta"));
        assert_eq!(rc_slots[0], 1);
        assert_eq!(rc_slots[1], 1);
        assert_eq!(result.strings_written, 2);

        // Node entry has remapped StringIds
        if let crate::graph::unified::storage::SlotState::Occupied(entry) = node_slots[0].state() {
            assert_eq!(entry.name, StringId::new(20)); // global slot 20
            assert_eq!(entry.signature, Some(StringId::new(21))); // global slot 21
            assert_eq!(entry.file, FileId::new(42));
        } else {
            panic!("Expected occupied slot");
        }
        assert_eq!(result.nodes_written, 1);

        // Per-file node IDs are recorded in commit order (Gate 0c bucket contract).
        assert_eq!(result.node_ids, vec![NodeId::new(10, 1)]);

        // No edges
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_remap_edge_kind_message_queue_other() {
        let mut remap = HashMap::new();
        remap.insert(StringId::new(10), StringId::new(110));
        remap.insert(StringId::new(20), StringId::new(220));

        let mut kind = EdgeKind::MessageQueue {
            protocol: MqProtocol::Other(StringId::new(10)),
            topic: Some(StringId::new(20)),
        };
        remap_edge_kind_string_ids(&mut kind, &remap);
        assert!(matches!(
            kind,
            EdgeKind::MessageQueue {
                protocol: MqProtocol::Other(proto),
                topic: Some(topic),
            } if proto == StringId::new(110) && topic == StringId::new(220)
        ));
    }

    // === Phase 4 tests ===

    #[test]
    fn test_phase4_apply_global_remap_basic() {
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::NodeArena;

        let mut arena = NodeArena::new();

        // Allocate two nodes with duplicate string IDs (2 and 3 are dupes of 1)
        let entry1 = NodeEntry::new(NodeKind::Function, StringId::new(1), FileId::new(0));
        let mut entry2 = NodeEntry::new(NodeKind::Variable, StringId::new(2), FileId::new(0));
        entry2.signature = Some(StringId::new(3));

        arena.alloc(entry1).unwrap();
        arena.alloc(entry2).unwrap();

        // Edges with string IDs that need remapping
        let mut all_edges = vec![vec![PendingEdge {
            source: NodeId::new(0, 1),
            target: NodeId::new(1, 1),
            kind: EdgeKind::Imports {
                alias: Some(StringId::new(3)),
                is_wildcard: false,
            },
            file: FileId::new(0),
            spans: vec![],
        }]];

        // Dedup remap: 2→1, 3→1
        let mut remap = HashMap::new();
        remap.insert(StringId::new(2), StringId::new(1));
        remap.insert(StringId::new(3), StringId::new(1));

        phase4_apply_global_remap(&mut arena, &mut all_edges, &remap);

        // Check that node 1's name was remapped from 2→1
        let (_, entry) = arena.iter().nth(1).unwrap();
        assert_eq!(entry.name, StringId::new(1));
        assert_eq!(entry.signature, Some(StringId::new(1)));

        // Check that edge's alias was remapped from 3→1
        if let EdgeKind::Imports { alias, .. } = &all_edges[0][0].kind {
            assert_eq!(*alias, Some(StringId::new(1)));
        } else {
            panic!("Expected Imports edge");
        }
    }

    #[test]
    fn test_phase4_apply_global_remap_empty() {
        use crate::graph::unified::storage::NodeArena;

        let mut arena = NodeArena::new();
        let mut edges: Vec<Vec<PendingEdge>> = vec![];
        let remap = HashMap::new();

        // Should be a no-op
        phase4_apply_global_remap(&mut arena, &mut edges, &remap);
    }

    #[test]
    fn test_pending_edges_to_delta_basic() {
        let edges = vec![
            vec![
                PendingEdge {
                    source: NodeId::new(0, 1),
                    target: NodeId::new(1, 1),
                    kind: EdgeKind::Calls {
                        argument_count: 0,
                        is_async: false,
                        resolved_via: ResolvedVia::Direct,
                    },
                    file: FileId::new(0),
                    spans: vec![],
                },
                PendingEdge {
                    source: NodeId::new(1, 1),
                    target: NodeId::new(2, 1),
                    kind: EdgeKind::References,
                    file: FileId::new(0),
                    spans: vec![],
                },
            ],
            vec![PendingEdge {
                source: NodeId::new(3, 1),
                target: NodeId::new(4, 1),
                kind: EdgeKind::Defines,
                file: FileId::new(1),
                spans: vec![],
            }],
        ];

        let (deltas, final_seq) = pending_edges_to_delta(&edges, 100);

        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].len(), 2);
        assert_eq!(deltas[1].len(), 1);
        assert_eq!(final_seq, 103);

        // Check sequence numbers are monotonic
        assert_eq!(deltas[0][0].seq, 100);
        assert_eq!(deltas[0][1].seq, 101);
        assert_eq!(deltas[1][0].seq, 102);

        // Check all are Add operations
        assert!(matches!(deltas[0][0].op, DeltaOp::Add));
        assert!(matches!(deltas[1][0].op, DeltaOp::Add));
    }

    #[test]
    fn test_pending_edges_to_delta_empty() {
        let edges: Vec<Vec<PendingEdge>> = vec![];
        let (deltas, final_seq) = pending_edges_to_delta(&edges, 0);
        assert!(deltas.is_empty());
        assert_eq!(final_seq, 0);
    }

    // ==================================================================
    // Task 4 Step 4 Phase 2: rebuild-plane coverage for migrated helpers.
    //
    // Each test below proves that the migrated helper runs against a
    // `RebuildGraph` (not just a `CodeGraph`) and that the mutation
    // lands on the rebuild-local state. Together with the CodeGraph
    // tests that still exercise the same helpers on the full-build
    // path, they form the "runs on both implementors" coverage
    // contract for `GraphMutationTarget` consumers.
    // ==================================================================

    /// Seed two call-compatible nodes (both `NodeKind::Function`) under
    /// the same qualified-name StringId across two distinct files, then
    /// run [`phase4c_prime_unify_cross_file_nodes`] against a
    /// [`RebuildGraph`]. Verify the loser node is tombstoned
    /// (name + qualified_name cleared per `merge_node_into`'s contract)
    /// and that pending edges pointing at the loser are rewritten to
    /// the winner.
    #[test]
    #[cfg(feature = "rebuild-internals")]
    fn phase4c_prime_unify_cross_file_nodes_runs_against_rebuild_graph() {
        use crate::graph::unified::concurrent::CodeGraph;
        use crate::graph::unified::mutation_target::GraphMutationTarget;
        use crate::graph::unified::node::NodeKind;

        let mut rebuild = {
            let graph = CodeGraph::new();
            graph.clone_for_rebuild()
        };

        // Intern a shared qualified name. On the rebuild-local
        // interner; strings() resolves it for later assertions.
        let qname_sid = rebuild.strings_mut().intern("my_mod::my_func").unwrap();

        // Register two files that host the duplicate Function nodes.
        let file_a = FileId::new(7);
        let file_b = FileId::new(8);

        // Build two `NodeKind::Function` entries sharing the same
        // qualified_name. Winner has a wider span (start_line > 0 and
        // end_line > start_line) to exercise the winner-selection
        // tie-break.
        let mut winner_entry = NodeEntry::new(NodeKind::Function, qname_sid, file_a);
        winner_entry.qualified_name = Some(qname_sid);
        winner_entry.start_line = 10;
        winner_entry.end_line = 30;

        let mut loser_entry = NodeEntry::new(NodeKind::Function, qname_sid, file_b);
        loser_entry.qualified_name = Some(qname_sid);
        // Narrower span → loses the tie-break.
        loser_entry.start_line = 5;
        loser_entry.end_line = 6;

        let winner_id = rebuild.nodes_mut().alloc(winner_entry).unwrap();
        let loser_id = rebuild.nodes_mut().alloc(loser_entry).unwrap();

        // A pending edge whose target is the loser — the remap table
        // should rewrite it to point at the winner.
        let mut all_edges = vec![vec![PendingEdge {
            source: winner_id, // any valid source — the helper only rewrites targets here
            target: loser_id,
            kind: EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file: file_b,
            spans: vec![],
        }]];

        let (stats, _remap) = phase4c_prime_unify_cross_file_nodes(&mut rebuild, &mut all_edges);

        // Stats shape
        assert_eq!(stats.nodes_merged, 1, "exactly one loser was tombstoned");
        assert_eq!(stats.candidate_pairs_examined, 1);
        assert_eq!(stats.edges_rewritten, 1);

        // Winner node survived with qualified_name intact.
        let winner_entry_after = GraphMutationTarget::nodes(&rebuild)
            .get(winner_id)
            .expect("winner must remain live");
        assert_eq!(
            winner_entry_after.qualified_name,
            Some(qname_sid),
            "winner keeps its qualified_name"
        );

        // Loser entry was merged via `merge_node_into`, which clears
        // `name` and `qualified_name` to make the slot name-invisible.
        let loser_entry_after = GraphMutationTarget::nodes(&rebuild)
            .get(loser_id)
            .expect("loser slot remains live (inert) per §F.1 bijection");
        assert_eq!(
            loser_entry_after.qualified_name, None,
            "loser qualified_name cleared by merge_node_into"
        );

        // Pending edge target rewritten winner-ward.
        assert_eq!(
            all_edges[0][0].target, winner_id,
            "PendingEdge.target rewritten from loser → winner"
        );
    }

    /// Lock in the Phase 4c-prime tie-break ordering Codex blessed in iter-1:
    /// primary = `start_line > 0`, tie-break 1 = wider span, tie-break 2 =
    /// lexicographically smaller **file path** (stable across rebuild
    /// representations), final fallback = smaller `NodeId::index()`.
    ///
    /// This test exercises the tie-break 2 path: two candidates with real
    /// spans of identical width, hosted in two different files that differ
    /// only in filename ordering. The winner must be the node whose file
    /// path sorts earlier, regardless of NodeId allocation order.
    #[test]
    #[cfg(feature = "rebuild-internals")]
    fn phase4c_prime_tie_break_prefers_lex_smaller_path_over_node_id() {
        use crate::graph::unified::concurrent::CodeGraph;
        use crate::graph::unified::node::NodeKind;
        use std::path::Path;

        let mut graph = CodeGraph::new();
        let qname = graph.strings_mut().intern("shared_qname").unwrap();
        // Register two paths whose lexical ordering is the reverse of
        // the registration (and hence NodeId) order. This isolates the
        // path-based tie-break from any accidental NodeId-ordering
        // coincidence: if the helper fell back to NodeId the "wrong"
        // node would win.
        let high_path_file = graph
            .files_mut()
            .register(Path::new("zzz_late.rs"))
            .unwrap();
        let low_path_file = graph
            .files_mut()
            .register(Path::new("aaa_early.rs"))
            .unwrap();

        // Allocate the `zzz_late.rs` node first so its NodeId::index() is
        // numerically smaller than the `aaa_early.rs` node's. With
        // identical spans, NodeId-only tie-break would incorrectly pick
        // the `zzz_late.rs` node. The correct behaviour is that the
        // path-based tie-break picks the `aaa_early.rs` node.
        let mut high_entry = NodeEntry::new(NodeKind::Function, qname, high_path_file);
        high_entry.qualified_name = Some(qname);
        high_entry.start_line = 10;
        high_entry.end_line = 20;
        let high_node = graph.nodes_mut().alloc(high_entry).unwrap();

        let mut low_entry = NodeEntry::new(NodeKind::Function, qname, low_path_file);
        low_entry.qualified_name = Some(qname);
        // Identical span width — forces the tie-break to ignore primary
        // + tie-break 1 (span width) and reach tie-break 2 (path).
        low_entry.start_line = 10;
        low_entry.end_line = 20;
        let low_node = graph.nodes_mut().alloc(low_entry).unwrap();

        graph.rebuild_indices();

        let mut all_edges: Vec<Vec<PendingEdge>> = Vec::new();
        let (stats, _remap) = phase4c_prime_unify_cross_file_nodes(&mut graph, &mut all_edges);

        assert_eq!(
            stats.nodes_merged, 1,
            "one of the duplicate nodes must be merged into the other"
        );

        // The `aaa_early.rs` node wins because its path sorts lexically
        // smaller. Verify its qualified_name is intact.
        let low_after = graph
            .nodes()
            .get(low_node)
            .expect("winner slot remains live");
        assert_eq!(
            low_after.qualified_name,
            Some(qname),
            "path-earlier node keeps qualified_name as the unification winner"
        );

        // And the `zzz_late.rs` node — despite a numerically smaller
        // NodeId::index() — was merged away.
        let high_after = graph
            .nodes()
            .get(high_node)
            .expect("loser slot remains inert (Gate 0d bijection contract)");
        assert_eq!(
            high_after.qualified_name, None,
            "path-later node loses even when its NodeId::index() is smaller"
        );
    }

    /// When the path-based tie-break ALSO ties (two duplicate nodes in the
    /// same file — rare but possible via duplicate definitions), the
    /// deterministic fallback is `b.index().cmp(&a.index())` which picks
    /// the node with the **smaller** NodeId index. Lock that in so future
    /// refactors of the tie-break don't accidentally flip the fallback
    /// direction.
    #[test]
    #[cfg(feature = "rebuild-internals")]
    fn phase4c_prime_tie_break_falls_back_to_smaller_node_id_on_identical_path() {
        use crate::graph::unified::concurrent::CodeGraph;
        use crate::graph::unified::node::NodeKind;
        use std::path::Path;

        let mut graph = CodeGraph::new();
        let qname = graph.strings_mut().intern("shared_qname").unwrap();
        let file = graph.files_mut().register(Path::new("shared.rs")).unwrap();

        // Allocate two duplicate nodes in the SAME file with identical
        // spans. The only thing that differs between them is their
        // NodeId index (allocation order). Tie-breaks 1 (span width)
        // and 2 (path) both return Equal; the final `b.index().cmp(&a.index())`
        // fallback picks the smaller index as the winner.
        let mut first_entry = NodeEntry::new(NodeKind::Function, qname, file);
        first_entry.qualified_name = Some(qname);
        first_entry.start_line = 1;
        first_entry.end_line = 5;
        let first_node = graph.nodes_mut().alloc(first_entry).unwrap();

        let mut second_entry = NodeEntry::new(NodeKind::Function, qname, file);
        second_entry.qualified_name = Some(qname);
        second_entry.start_line = 1;
        second_entry.end_line = 5;
        let second_node = graph.nodes_mut().alloc(second_entry).unwrap();

        assert!(
            first_node.index() < second_node.index(),
            "precondition: first_node's arena slot precedes second_node's"
        );

        graph.rebuild_indices();

        let mut all_edges: Vec<Vec<PendingEdge>> = Vec::new();
        let (stats, _remap) = phase4c_prime_unify_cross_file_nodes(&mut graph, &mut all_edges);

        assert_eq!(stats.nodes_merged, 1);

        // Smaller NodeId::index() wins.
        let winner_after = graph.nodes().get(first_node).expect("winner live");
        assert_eq!(
            winner_after.qualified_name,
            Some(qname),
            "smaller-index node wins the same-path / same-span tie-break"
        );
        let loser_after = graph.nodes().get(second_node).expect("loser inert");
        assert_eq!(
            loser_after.qualified_name, None,
            "larger-index node loses the same-path / same-span tie-break"
        );
    }

    /// Drive the free [`rebuild_indices`] function against both a
    /// `RebuildGraph` and a `CodeGraph` seeded with identical data,
    /// and verify the resulting `AuxiliaryIndices` are structurally
    /// equivalent (same name buckets, same kind buckets).
    #[test]
    #[cfg(feature = "rebuild-internals")]
    fn rebuild_indices_runs_against_rebuild_graph() {
        use crate::graph::unified::concurrent::CodeGraph;
        use crate::graph::unified::mutation_target::GraphMutationTarget;
        use crate::graph::unified::node::NodeKind;

        // === CodeGraph baseline ===
        let mut code_graph = CodeGraph::new();
        let alpha_id_code = code_graph.strings_mut().intern("alpha").unwrap();
        let mut code_entry = NodeEntry::new(NodeKind::Function, alpha_id_code, FileId::new(1));
        code_entry.qualified_name = Some(alpha_id_code);
        let code_node_id = code_graph.nodes_mut().alloc(code_entry).unwrap();
        rebuild_indices(&mut code_graph);
        let code_buckets_function: Vec<NodeId> =
            code_graph.indices().by_kind(NodeKind::Function).to_vec();

        // === RebuildGraph path ===
        let mut rebuild = {
            let graph = CodeGraph::new();
            graph.clone_for_rebuild()
        };
        let alpha_id_rebuild = rebuild.strings_mut().intern("alpha").unwrap();
        let mut rebuild_entry =
            NodeEntry::new(NodeKind::Function, alpha_id_rebuild, FileId::new(1));
        rebuild_entry.qualified_name = Some(alpha_id_rebuild);
        let rebuild_node_id = rebuild.nodes_mut().alloc(rebuild_entry).unwrap();
        rebuild_indices(&mut rebuild);

        // The node ids are both the first allocation on their
        // respective arenas, so they share slot indices and
        // generations.
        assert_eq!(code_node_id, rebuild_node_id);

        // The trait-method accessor routes through the impl on
        // `RebuildGraph`; the returned indices came from the
        // rebuild-local `AuxiliaryIndices` (not a CodeGraph's).
        let rebuild_buckets_function: Vec<NodeId> = GraphMutationTarget::indices(&rebuild)
            .by_kind(NodeKind::Function)
            .to_vec();

        assert_eq!(
            code_buckets_function, rebuild_buckets_function,
            "rebuild_indices must produce equivalent Function buckets on both paths"
        );
        // Name bucket also present on the rebuild side.
        let by_name: Vec<NodeId> = GraphMutationTarget::indices(&rebuild)
            .by_name(alpha_id_rebuild)
            .to_vec();
        assert_eq!(by_name, vec![rebuild_node_id]);
    }

    /// Drive [`phase4d_bulk_insert_edges`] against a `RebuildGraph`.
    /// Seed two nodes, construct a per-file `PendingEdge` vector, and
    /// prove the edges land on the rebuild-local edge store with the
    /// expected monotonically-advancing sequence counter.
    #[test]
    #[cfg(feature = "rebuild-internals")]
    fn phase4d_bulk_insert_edges_runs_against_rebuild_graph() {
        use crate::graph::unified::concurrent::CodeGraph;
        use crate::graph::unified::mutation_target::GraphMutationTarget;
        use crate::graph::unified::node::NodeKind;

        let mut rebuild = {
            let graph = CodeGraph::new();
            graph.clone_for_rebuild()
        };

        let name_sid = rebuild.strings_mut().intern("edge_target").unwrap();
        let file = FileId::new(3);

        let n_source = rebuild
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name_sid, file))
            .unwrap();
        let n_target = rebuild
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Variable, name_sid, file))
            .unwrap();

        // Pre-condition: no edges in the rebuild-local forward store.
        let pre_counter = GraphMutationTarget::edges(&rebuild).forward().seq_counter();

        let per_file_edges = vec![vec![
            PendingEdge {
                source: n_source,
                target: n_target,
                kind: EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                file,
                spans: vec![],
            },
            PendingEdge {
                source: n_source,
                target: n_target,
                kind: EdgeKind::Calls {
                    argument_count: 1,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                file,
                spans: vec![],
            },
        ]];

        let final_seq = phase4d_bulk_insert_edges(&mut rebuild, &per_file_edges);

        // Seq counter advanced by exactly two edges.
        assert_eq!(
            final_seq,
            pre_counter + 2,
            "phase4d_bulk_insert_edges must advance seq by edge count"
        );

        // Rebuild-local forward store now contains both edges.
        let forward = GraphMutationTarget::edges(&rebuild).forward();
        let after_counter = forward.seq_counter();
        assert_eq!(after_counter, pre_counter + 2);
        // Forward delta must carry the two new edges.
        assert!(
            forward.delta().iter().filter(|e| e.is_add()).count() >= 2,
            "expected at least two Add edges in the rebuild-local forward delta"
        );
        drop(forward);

        // Empty input is a no-op on the edge store.
        let empty_final = phase4d_bulk_insert_edges(&mut rebuild, &[]);
        assert_eq!(empty_final, pre_counter + 2, "empty input is a no-op");
    }

    /// `C_EDGE_MIGRATE` regression: when a Cluster C plugin migrates a
    /// `TypeOf{Field}` edge's source from a struct node to the per-field
    /// `Property` node, Phase 4d must NOT collapse the new shape onto
    /// any sibling edge. Both Property-sourced and struct-sourced
    /// edges - including a struct-sourced edge over the same target /
    /// kind tuple - must round-trip into the bulk-insert path with
    /// distinct `(source, target)` identities and stable seq ordering.
    ///
    /// This locks the property the
    /// `phase4d_bulk_insert_edges` doc-comment promises to plugin
    /// authors: per-file `PendingEdge` order is preserved 1:1 by
    /// `pending_edges_to_delta`, and no `(source, target, kind)` dedup
    /// fires inside Phase 4d. Without this guarantee the migration
    /// would silently drop the new Property-sourced edges whenever an
    /// older legacy snapshot mixed both shapes during a partial
    /// rebuild.
    #[test]
    fn phase4d_preserves_property_sourced_typeof_field_edges() {
        use crate::graph::unified::edge::kind::TypeOfContext;

        // Synthetic NodeIds standing in for `main.SelectorSource` (struct),
        // `main.SelectorSource.NeedTags` (Property), and `bool` (target type).
        let struct_id = NodeId::new(10, 1);
        let property_id = NodeId::new(11, 1);
        let bool_id = NodeId::new(12, 1);

        let typeof_field_kind = EdgeKind::TypeOf {
            context: Some(TypeOfContext::Field),
            index: Some(0),
            name: None,
        };

        // Two PendingEdges over the same (target, kind) discriminator
        // but different sources - the post-migration Property-sourced
        // shape and a hypothetical legacy struct-sourced shadow that
        // could appear during a partial rebuild. Phase 4d must keep
        // both.
        let per_file_edges = vec![vec![
            PendingEdge {
                source: property_id,
                target: bool_id,
                kind: typeof_field_kind.clone(),
                file: FileId::new(0),
                spans: vec![],
            },
            PendingEdge {
                source: struct_id,
                target: bool_id,
                kind: typeof_field_kind.clone(),
                file: FileId::new(0),
                spans: vec![],
            },
        ]];

        let (deltas, final_seq) = pending_edges_to_delta(&per_file_edges, 500);

        // No dedup: both edges land in the per-file delta vector with
        // distinct seq numbers, in input order.
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].len(), 2);
        assert_eq!(final_seq, 502);

        assert_eq!(deltas[0][0].source, property_id);
        assert_eq!(deltas[0][0].target, bool_id);
        assert_eq!(deltas[0][0].seq, 500);
        assert!(matches!(
            deltas[0][0].kind,
            EdgeKind::TypeOf {
                context: Some(TypeOfContext::Field),
                ..
            }
        ));

        assert_eq!(deltas[0][1].source, struct_id);
        assert_eq!(deltas[0][1].target, bool_id);
        assert_eq!(deltas[0][1].seq, 501);

        // Determinism re-check: re-running the conversion against the
        // same input produces an identical DeltaEdge sequence (same
        // sources, same targets, same kinds, same seq numbers when
        // re-anchored to the same `seq_start`). This is the property
        // the SnapshotReader → SnapshotWriter byte-identity round-trip
        // assertion relies on for fresh-rebuild reproducibility.
        let (deltas_again, final_seq_again) = pending_edges_to_delta(&per_file_edges, 500);
        assert_eq!(final_seq_again, final_seq);
        assert_eq!(deltas_again.len(), deltas.len());
        assert_eq!(deltas_again[0].len(), deltas[0].len());
        for (a, b) in deltas[0].iter().zip(deltas_again[0].iter()) {
            assert_eq!(a.source, b.source);
            assert_eq!(a.target, b.target);
            assert_eq!(a.seq, b.seq);
        }
    }

    // ----------------------------------------------------------------------
    // T3 Cluster B (02_DESIGN §4.3.e Change 4): Phase 4d-prime propagation
    // ----------------------------------------------------------------------

    /// Build a per-file `NodeMetadataStore` carrying one Macro entry with
    /// a `cfg_condition` so the merge step is non-vacuous.
    fn macro_store_with(
        node_id: NodeId,
        cfg: &str,
    ) -> crate::graph::unified::storage::metadata::NodeMetadataStore {
        use crate::graph::unified::storage::metadata::{MacroNodeMetadata, NodeMetadataStore};
        let mut store = NodeMetadataStore::new();
        let m = MacroNodeMetadata {
            cfg_condition: Some(cfg.to_string()),
            ..Default::default()
        };
        store.insert(node_id, m);
        store
    }

    #[test]
    fn phase4d_prime_merges_per_file_metadata_into_graph_macro_metadata() {
        use super::super::unification::NodeRemapTable;
        use crate::graph::unified::concurrent::CodeGraph;
        use crate::graph::unified::mutation_target::GraphMutationTarget;

        let mut graph = CodeGraph::new();
        let nid_a = NodeId::new(101, 1);
        let nid_b = NodeId::new(202, 1);
        let file_a = FileId::new(7);
        let file_b = FileId::new(8);

        let staged = vec![
            (file_a, macro_store_with(nid_a, "linux")),
            (file_b, macro_store_with(nid_b, "darwin")),
        ];

        let remap = NodeRemapTable::default();
        let merged = phase4d_prime_propagate_staging_metadata(&mut graph, staged, &remap);

        assert!(
            merged,
            "non-empty staged stores must report metadata_changed=true"
        );
        assert_eq!(
            GraphMutationTarget::macro_metadata_mut(&mut graph)
                .get_macro(nid_a)
                .and_then(|m| m.cfg_condition.clone()),
            Some("linux".to_string())
        );
        assert_eq!(
            GraphMutationTarget::macro_metadata_mut(&mut graph)
                .get_macro(nid_b)
                .and_then(|m| m.cfg_condition.clone()),
            Some("darwin".to_string())
        );
    }

    #[test]
    fn phase4d_prime_drops_loser_metadata_before_merge() {
        // Pins 02_DESIGN §4.3.e Change 3 contract: when the unifier
        // tombstones a loser, its staged metadata must NOT survive into
        // the graph (the winner's own per-file store carries the
        // authoritative cfg_condition; 01_SPEC §5.3.f spec text).
        use super::super::unification::NodeRemapTable;
        use crate::graph::unified::concurrent::CodeGraph;
        use crate::graph::unified::mutation_target::GraphMutationTarget;

        let mut graph = CodeGraph::new();
        let loser = NodeId::new(101, 1);
        let winner = NodeId::new(202, 1);
        let file_loser = FileId::new(7);
        let file_winner = FileId::new(8);

        // Loser file stages `linux`, winner file stages `darwin`.
        let staged = vec![
            (file_loser, macro_store_with(loser, "linux")),
            (file_winner, macro_store_with(winner, "darwin")),
        ];

        // Unifier marks `loser → winner`.
        let mut remap = NodeRemapTable::default();
        remap.insert(loser, winner);

        let merged = phase4d_prime_propagate_staging_metadata(&mut graph, staged, &remap);
        assert!(
            merged,
            "winner's store still merges so metadata_changed=true"
        );

        // The winner gets `darwin` from its own file's store. The loser
        // entry is dropped before merge — it never reaches the graph
        // under the winner key.
        assert_eq!(
            GraphMutationTarget::macro_metadata_mut(&mut graph)
                .get_macro(winner)
                .and_then(|m| m.cfg_condition.clone()),
            Some("darwin".to_string()),
            "winner's authoritative cfg_condition wins; loser's `linux` is dropped"
        );
        assert!(
            GraphMutationTarget::macro_metadata_mut(&mut graph)
                .get_macro(loser)
                .is_none(),
            "loser key has no metadata in the graph after Phase 4d-prime"
        );
    }

    #[test]
    fn rekey_staging_metadata_to_arena_maps_local_to_arena() {
        // Stage metadata under staging-local NodeIds (i, 1) for i ∈ {0, 1, 2}
        // and confirm the rekeyed store carries the same payload under
        // the corresponding arena NodeIds drawn from per_file_node_ids.
        use crate::graph::unified::storage::metadata::{MacroNodeMetadata, NodeMetadataStore};

        let mut staging = NodeMetadataStore::new();
        for (i, cond) in ["linux", "darwin", "windows"].iter().enumerate() {
            let m = MacroNodeMetadata {
                cfg_condition: Some((*cond).to_string()),
                ..Default::default()
            };
            staging.insert(NodeId::new(i as u32, 1), m);
        }

        // Arena NodeIds — note generation 1 (the standard staging.add_node
        // contract) and arbitrary non-sequential arena slots.
        let arena_ids = vec![
            NodeId::new(100, 1),
            NodeId::new(101, 1),
            NodeId::new(102, 1),
        ];

        let rekeyed = rekey_staging_metadata_to_arena(&staging, &arena_ids);

        assert_eq!(rekeyed.len(), 3);
        for (i, cond) in ["linux", "darwin", "windows"].iter().enumerate() {
            let m = rekeyed
                .get_macro(arena_ids[i])
                .expect("arena NodeId carries the remapped entry");
            assert_eq!(m.cfg_condition.as_deref(), Some(*cond));
        }
        // Original staging keys are gone (no longer in the rekeyed store).
        assert!(rekeyed.get_macro(NodeId::new(0, 1)).is_none());
    }

    #[test]
    fn rekey_staging_metadata_maps_shape_descriptors_local_to_arena() {
        // The shape-only path: descriptors keyed under staging-local NodeIds
        // must land under the corresponding arena NodeIds, with no entry
        // metadata present at all (the common case for ordinary functions).
        use crate::graph::unified::build::shape::CfBucket;
        use crate::graph::unified::storage::metadata::NodeMetadataStore;
        use crate::graph::unified::storage::shape::ShapeDescriptor;

        let mut staging = NodeMetadataStore::new();
        for i in 0..3u32 {
            let mut d = ShapeDescriptor::default();
            // Stamp a distinguishable histogram per node so we can confirm the
            // exact descriptor rode the rekey, not just some descriptor.
            d.cf_histogram[CfBucket::Branch.index()] = (i + 1) as u16;
            staging.insert_shape_descriptor(NodeId::new(i, 1), d);
        }
        assert!(staging.get_macro(NodeId::new(0, 1)).is_none());
        assert!(
            !staging.is_empty(),
            "a shape-only staging store is non-empty"
        );

        let arena_ids = vec![
            NodeId::new(100, 1),
            NodeId::new(101, 1),
            NodeId::new(102, 1),
        ];
        let rekeyed = rekey_staging_metadata_to_arena(&staging, &arena_ids);

        assert_eq!(rekeyed.shape_descriptors().len(), 3);
        for i in 0..3u32 {
            let d = rekeyed
                .shape_descriptor(arena_ids[i as usize])
                .expect("arena NodeId carries the remapped descriptor");
            assert_eq!(d.cf_histogram[CfBucket::Branch.index()], (i + 1) as u16);
        }
        // Original staging key carries nothing in the rekeyed store.
        assert!(rekeyed.shape_descriptor(NodeId::new(0, 1)).is_none());
    }

    #[test]
    fn rekey_staging_metadata_drops_out_of_range_keys() {
        // Staging metadata keyed at index 5 but per_file_node_ids only has
        // 3 entries: the helper drops the stale key rather than panicking.
        use crate::graph::unified::storage::metadata::{MacroNodeMetadata, NodeMetadataStore};

        let mut staging = NodeMetadataStore::new();
        let in_range = MacroNodeMetadata {
            cfg_condition: Some("good".to_string()),
            ..Default::default()
        };
        staging.insert(NodeId::new(0, 1), in_range);

        let stale = MacroNodeMetadata {
            cfg_condition: Some("bad".to_string()),
            ..Default::default()
        };
        staging.insert(NodeId::new(5, 1), stale);

        let arena_ids = vec![NodeId::new(100, 1)];
        let rekeyed = rekey_staging_metadata_to_arena(&staging, &arena_ids);

        assert_eq!(rekeyed.len(), 1, "stale out-of-range key dropped");
        assert_eq!(
            rekeyed
                .get_macro(NodeId::new(100, 1))
                .and_then(|m| m.cfg_condition.clone()),
            Some("good".to_string())
        );
    }

    #[test]
    fn phase4d_prime_empty_staged_metadata_returns_false() {
        use super::super::unification::NodeRemapTable;
        use crate::graph::unified::concurrent::CodeGraph;

        let mut graph = CodeGraph::new();
        let remap = NodeRemapTable::default();
        let merged = phase4d_prime_propagate_staging_metadata(&mut graph, Vec::new(), &remap);
        assert!(!merged, "no staged stores → metadata_changed=false");
    }

    #[test]
    fn phase4d_prime_empty_store_after_loser_drop_returns_false() {
        // Single staged store that is ENTIRELY losers — after
        // `apply_to_metadata_store` drops them all, the store is empty
        // and `merge` should not be called.
        use super::super::unification::NodeRemapTable;
        use crate::graph::unified::concurrent::CodeGraph;
        use crate::graph::unified::mutation_target::GraphMutationTarget;

        let mut graph = CodeGraph::new();
        let loser = NodeId::new(101, 1);
        let winner = NodeId::new(202, 1);
        let file_loser = FileId::new(7);

        let staged = vec![(file_loser, macro_store_with(loser, "linux"))];

        let mut remap = NodeRemapTable::default();
        remap.insert(loser, winner);

        let merged = phase4d_prime_propagate_staging_metadata(&mut graph, staged, &remap);

        assert!(
            !merged,
            "store collapsed to empty by loser-drop → no merge → metadata_changed=false"
        );
        assert!(
            GraphMutationTarget::macro_metadata_mut(&mut graph).is_empty(),
            "graph metadata store stays empty"
        );
    }
}
