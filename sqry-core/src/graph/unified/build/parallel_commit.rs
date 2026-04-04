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
use crate::graph::unified::edge::kind::{EdgeKind, MqProtocol};
use crate::graph::unified::file::FileId;
use crate::graph::unified::node::NodeId;
use crate::graph::unified::storage::NodeArena;
use crate::graph::unified::storage::arena::{NodeEntry, Slot};
use crate::graph::unified::storage::interner::StringInterner;
use crate::graph::unified::string::StringId;

use super::pass3_intra::PendingEdge;
use super::staging::{StagingGraph, StagingOp};

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

/// Phase 3 result: per-file edges and total written counts for validation.
pub struct Phase3Result {
    /// Per-file edge collections for Phase 4 bulk insert.
    pub per_file_edges: Vec<Vec<PendingEdge>>,
    /// Total nodes actually written (for validation against planned totals).
    pub total_nodes_written: usize,
    /// Total strings actually written (for validation against planned totals).
    pub total_strings_written: usize,
    /// Total edges collected across all files.
    pub total_edges_collected: usize,
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
/// # Panics
///
/// Panics if `plan.total_nodes` or `plan.total_strings` exceeds the
/// pre-allocated range in the arena or interner.
#[must_use]
pub fn phase3_parallel_commit(
    plan: &ChunkCommitPlan,
    staging_graphs: &[&StagingGraph],
    arena: &mut NodeArena,
    interner: &mut StringInterner,
) -> Phase3Result {
    if plan.file_plans.is_empty() {
        return Phase3Result {
            per_file_edges: Vec::new(),
            total_nodes_written: 0,
            total_strings_written: 0,
            total_edges_collected: 0,
        };
    }

    // Determine the start of the pre-allocated ranges.
    let node_start = plan.file_plans[0].node_range.start;
    let string_start = plan.file_plans[0].string_range.start;

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
    let per_file_edges = results.into_iter().map(|r| r.edges).collect();

    Phase3Result {
        per_file_edges,
        total_nodes_written,
        total_strings_written,
        total_edges_collected,
    }
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
// Result of committing a single file: edges + actual written counts.
struct FileCommitResult {
    edges: Vec<PendingEdge>,
    nodes_written: usize,
    strings_written: usize,
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
    let (node_remap, nodes_written) = write_nodes(ops, plan, node_slots, &string_remap);

    // --- Step 3: Collect remapped edges with pre-assigned sequence numbers ---
    let edges = collect_edges(ops, plan, &node_remap, &string_remap);

    FileCommitResult {
        edges,
        nodes_written,
        strings_written,
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
        | EdgeKind::SealedPermit => {}
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
/// Returns `(remap, nodes_written)`.
fn write_nodes(
    ops: &[StagingOp],
    plan: &FilePlan,
    node_slots: &mut [Slot<NodeEntry>],
    string_remap: &HashMap<StringId, StringId>,
) -> (HashMap<NodeId, NodeId>, usize) {
    let mut node_remap = HashMap::new();
    let mut node_cursor = 0usize;

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

            node_cursor += 1;
        }
    }

    (node_remap, node_cursor)
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
        | EdgeKind::SealedPermit => {}
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
        };
        remap_edge_kind_string_ids(&mut kind, &remap);
        assert!(matches!(
            kind,
            EdgeKind::Calls {
                argument_count: 3,
                is_async: true,
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
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::NodeArena;
        use crate::graph::unified::storage::interner::StringInterner;

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
            },
            FileId::new(0),
        );

        let file_ids = vec![FileId::new(5)];

        // Pre-allocate with non-zero offsets to verify remap works.
        let mut arena = NodeArena::new();
        let mut interner = StringInterner::new();

        // Pre-fill some slots so our file starts at a non-zero offset.
        arena.alloc_range(10, &placeholder_entry()).unwrap();
        let string_start = interner.alloc_range(1).unwrap();
        assert_eq!(string_start, 1); // past sentinel

        let offsets = GlobalOffsets {
            node_offset: 10, // file's nodes start at index 10
            string_offset: string_start,
        };
        let plan = phase2_assign_ranges(&[&sg], &file_ids, &offsets);
        assert_eq!(plan.file_plans[0].node_range, 10..12);

        // Pre-allocate the actual ranges for Phase 3.
        arena
            .alloc_range(plan.total_nodes, &placeholder_entry())
            .unwrap();
        interner.alloc_range(plan.total_strings).unwrap();

        // Phase 3
        let result = phase3_parallel_commit(&plan, &[&sg], &mut arena, &mut interner);

        // Verify written counts
        assert_eq!(result.total_nodes_written, 2);
        assert_eq!(result.total_strings_written, 1);

        // Verify strings were written
        let global_name = StringId::new(string_start);
        assert_eq!(&*interner.resolve(global_name).unwrap(), "my_func");

        // Verify 1 file, 1 edge
        assert_eq!(result.per_file_edges.len(), 1);
        assert_eq!(result.per_file_edges[0].len(), 1);

        // Verify edge was remapped to global IDs (node_offset=10)
        let edge = &result.per_file_edges[0][0];
        assert_eq!(edge.file, FileId::new(5));
        assert_eq!(edge.source, NodeId::new(10, 1)); // first node at slot 10
        assert_eq!(edge.target, NodeId::new(11, 1)); // second node at slot 11
    }

    #[test]
    fn test_phase3_parallel_commit_empty() {
        use crate::graph::unified::storage::NodeArena;
        use crate::graph::unified::storage::interner::StringInterner;

        let mut arena = NodeArena::new();
        let mut interner = StringInterner::new();

        let plan = ChunkCommitPlan {
            file_plans: vec![],
            total_nodes: 0,
            total_strings: 0,
            total_edges: 0,
        };

        let result = phase3_parallel_commit(&plan, &[], &mut arena, &mut interner);
        assert!(result.per_file_edges.is_empty());
        assert_eq!(result.total_nodes_written, 0);
        assert_eq!(result.total_strings_written, 0);
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
}
