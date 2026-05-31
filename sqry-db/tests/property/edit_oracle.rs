//! Test-side edit oracle for the WS1 cache-invalidation property suite.
//!
//! Implements the `affects_revisions` decision procedure described in DESIGN
//! §2.5 of `02_DESIGN-graph-fidelity-planner-correctness.md`. This module is
//! the **independent** counterpart to the production `TRACKS_*_REVISION`
//! constants — the harness compares the two so codex review (DAG acceptance
//! `U_WS1_10`) can catch a wrongly-narrow OR wrongly-wide constant.
//!
//! ## Hard contract
//!
//! This file MUST NOT import any `DerivedQuery` impl from `sqry-db::queries`
//! and MUST NOT read `Q::TRACKS_EDGE_REVISION` / `Q::TRACKS_METADATA_REVISION`
//! constants. The tier set the oracle compares against is declared by the
//! cache-invariants test harness, separately from production. If the two
//! agree we get cache correctness; if they disagree the proptest finds the
//! discrepancy. Calling into production to populate the oracle would short-
//! circuit that proof.
//!
//! ## Edit taxonomy
//!
//! Three logical edit tiers map onto the three production cache tiers:
//!
//! | Edit variant | Tier 1 (file) | Tier 2 (edge) | Tier 3 (metadata) |
//! |--------------|---------------|---------------|--------------------|
//! | `AddCallsEdge` | yes (source's file) | YES | no |
//! | `AddImportsEdge` | yes (source's file) | YES | no |
//! | `RemoveExistingEdge` | yes (source's file) | YES | no |
//! | `MarkAddressTaken` | yes (node's file) | no | YES |
//! | `MarkCallsitePromiscuous` | yes (node's file) | no | YES |
//! | `BumpFileRevision` | YES (the named file) | no | no |
//! | `Noop` | no | no | no |
//!
//! Tier 1 is touched whenever ANY file revision counter advances. The harness
//! advances the source file's revision alongside edge / metadata bumps to
//! mirror the production `reindex_files` contract — a single physical edit
//! always advances at least the affected file's Tier 1 counter on top of any
//! global tier.
//!
//! This taxonomy is intentionally minimal. It is NOT a faithful model of
//! every shape of edit production can produce; it is a small set chosen so
//! that (a) every tier flips independently in at least one variant, and (b)
//! the planner-visible result of `Q::execute` actually changes for at least
//! one variant per query.

#![allow(dead_code)]

use std::collections::BTreeSet;

use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::file::FileId;
use sqry_core::graph::unified::node::id::NodeId;

use sqry_db::QueryDb;

use proptest::prelude::*;

#[path = "graph_gen.rs"]
#[allow(unused_imports)]
mod graph_gen;

pub use graph_gen::{GeneratedGraph, well_formed_graph};

// ---------------------------------------------------------------------------
// Tier set
// ---------------------------------------------------------------------------

/// Bit-flag set of cache tiers an `Edit` touches OR a query subscribes to.
///
/// Used as both the oracle output (`tiers_touched(edit)`) and the query input
/// (`tiers_tracked(R)`). `affects_revisions` is set intersection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TierSet {
    bits: u8,
}

impl TierSet {
    pub const NONE: TierSet = TierSet { bits: 0 };
    /// Tier 1: file-level revision counters.
    pub const FILE: TierSet = TierSet { bits: 0b001 };
    /// Tier 2: global edge revision counter.
    pub const EDGE: TierSet = TierSet { bits: 0b010 };
    /// Tier 3: global metadata revision counter.
    pub const METADATA: TierSet = TierSet { bits: 0b100 };

    pub const fn contains(self, other: TierSet) -> bool {
        (self.bits & other.bits) == other.bits
    }

    pub const fn intersects(self, other: TierSet) -> bool {
        (self.bits & other.bits) != 0
    }

    pub const fn union(self, other: TierSet) -> TierSet {
        TierSet {
            bits: self.bits | other.bits,
        }
    }
}

impl std::ops::BitOr for TierSet {
    type Output = TierSet;
    fn bitor(self, rhs: TierSet) -> TierSet {
        self.union(rhs)
    }
}

// ---------------------------------------------------------------------------
// Edit definition
// ---------------------------------------------------------------------------

/// One independently-shrinkable graph mutation.
///
/// Recipe-local indices (`src_idx`, `tgt_idx`, `node_idx`, `file_idx`,
/// `edge_idx`) are validated against the live `GeneratedGraph` at apply time.
/// Out-of-range indices reduce to `Noop` so the proptest strategy can
/// generate edits without pre-knowing the graph shape (shrinking is
/// well-defined even when the graph collapses to a single node).
#[derive(Debug, Clone)]
pub enum Edit {
    /// Add a `Calls { argument_count: 0, is_async: false, Direct }` edge.
    AddCallsEdge { src_idx: usize, tgt_idx: usize },
    /// Add an `Imports { alias: None, is_wildcard: false }` edge.
    AddImportsEdge { src_idx: usize, tgt_idx: usize },
    /// Remove one of the live forward edges by recipe edge index.
    RemoveExistingEdge { edge_idx: usize },
    /// Mark `nodes[node_idx]` as address-taken.
    MarkAddressTaken { node_idx: usize },
    /// Mark `nodes[node_idx]` as callsite-promiscuous.
    MarkCallsitePromiscuous { node_idx: usize },
    /// Bump the Tier-1 revision of `files[file_idx]` without changing edges
    /// or metadata.
    BumpFileRevision { file_idx: usize },
    /// No-op — drives the spurious-invalidation channel.
    Noop,
}

/// Outcome of applying an `Edit` against a `QueryDb`.
///
/// `tier_touches` is the **observable** tier set the edit actually flipped
/// (derived from `Edit` variant + index-validity at apply time). If the edit
/// reduced to `Noop` because an index was out of range, `tier_touches ==
/// TierSet::NONE`.
#[derive(Debug, Clone)]
pub struct AppliedEdit {
    pub edit: Edit,
    pub tier_touches: TierSet,
    /// Files whose Tier-1 counter advanced. Empty when `tier_touches` does
    /// not contain `TierSet::FILE`.
    pub files_touched: BTreeSet<FileId>,
}

// ---------------------------------------------------------------------------
// Oracle
// ---------------------------------------------------------------------------

/// Decision procedure: does `applied` touch any tier the query subscribes
/// to, where the query is described by `tracked`?
///
/// `tracked` is the **test harness's** independent declaration of the
/// tracked tier set, NOT a read of production constants. If `tracked` is
/// wrong, the test harness is wrong, and the codex reviewer is responsible
/// for spotting the divergence between this table (in `cache_invariants.rs`)
/// and the production `TRACKS_*_REVISION` constants.
///
/// Tier 1 (file-level deps) is always implicitly tracked because the cache
/// validator runs `validate_file_deps` unconditionally
/// (`sqry-db/src/lib.rs::validate_cached_result`).
#[must_use]
pub fn affects_revisions(applied: &AppliedEdit, tracked: TierSet) -> bool {
    // Tier 1 is universal — every query validates its recorded file deps.
    let effective = tracked | TierSet::FILE;
    effective.intersects(applied.tier_touches)
}

// ---------------------------------------------------------------------------
// Apply: mutate the graph, bump matching revision counters
// ---------------------------------------------------------------------------

/// Apply `edit` to `db`'s current graph, advancing the matching revision
/// tiers, then install the rebuilt snapshot on `db`. Returns an `AppliedEdit`
/// describing what was actually touched.
///
/// This function is the model of `reindex_files` for the harness: it bumps
/// the per-file revision counters AND any global tier counters the edit
/// touches, mirroring the production rebuild contract.
#[must_use]
pub fn apply_edit(
    graph: &mut CodeGraph,
    db: &mut QueryDb,
    edit: Edit,
    ctx: &EditContext,
) -> AppliedEdit {
    let mut tier_touches = TierSet::NONE;
    let mut files_touched: BTreeSet<FileId> = BTreeSet::new();

    let apply_edge_add =
        |g: &mut CodeGraph, src: NodeId, tgt: NodeId, kind: EdgeKind, file: FileId| {
            g.edges().add_edge(src, tgt, kind, file);
        };

    let edit_owned = edit.clone();
    match edit {
        Edit::AddCallsEdge { src_idx, tgt_idx } => {
            if let (Some(src), Some(tgt)) = (
                ctx.node_ids.get(src_idx).copied(),
                ctx.node_ids.get(tgt_idx).copied(),
            ) {
                let file = ctx.file_for_node(src_idx);
                apply_edge_add(
                    graph,
                    src,
                    tgt,
                    EdgeKind::Calls {
                        argument_count: 0,
                        is_async: false,
                        resolved_via: ResolvedVia::Direct,
                    },
                    file,
                );
                tier_touches = TierSet::EDGE | TierSet::FILE;
                files_touched.insert(file);
            }
        }
        Edit::AddImportsEdge { src_idx, tgt_idx } => {
            if let (Some(src), Some(tgt)) = (
                ctx.node_ids.get(src_idx).copied(),
                ctx.node_ids.get(tgt_idx).copied(),
            ) {
                let file = ctx.file_for_node(src_idx);
                apply_edge_add(
                    graph,
                    src,
                    tgt,
                    EdgeKind::Imports {
                        alias: None,
                        is_wildcard: false,
                    },
                    file,
                );
                tier_touches = TierSet::EDGE | TierSet::FILE;
                files_touched.insert(file);
            }
        }
        Edit::RemoveExistingEdge { edge_idx } => {
            // Walk live forward edges deterministically and remove the
            // edge at position `edge_idx % live.len()`. If the graph has
            // no edges, fall back to Noop.
            let live = graph.edges().all_live_forward_edges();
            if !live.is_empty() {
                let idx = edge_idx % live.len();
                let e = live[idx].clone();
                graph
                    .edges()
                    .remove_edge(e.source, e.target, e.kind, e.file);
                tier_touches = TierSet::EDGE | TierSet::FILE;
                files_touched.insert(e.file);
            }
        }
        Edit::MarkAddressTaken { node_idx } => {
            if let Some(node_id) = ctx.node_ids.get(node_idx).copied() {
                // Only count as a metadata edit when the bit was not
                // already set. Re-marking an already-marked node is a
                // logical noop, and the production `metadata_revision`
                // bump should match that contract.
                let already = graph.macro_metadata().is_address_taken(node_id);
                graph.macro_metadata_mut().mark_address_taken(node_id);
                if !already {
                    let file = ctx.file_for_node(node_idx);
                    tier_touches = TierSet::METADATA | TierSet::FILE;
                    files_touched.insert(file);
                }
            }
        }
        Edit::MarkCallsitePromiscuous { node_idx } => {
            if let Some(node_id) = ctx.node_ids.get(node_idx).copied() {
                let already = graph.macro_metadata().is_callsite_promiscuous(node_id);
                graph
                    .macro_metadata_mut()
                    .mark_callsite_promiscuous(node_id);
                if !already {
                    let file = ctx.file_for_node(node_idx);
                    tier_touches = TierSet::METADATA | TierSet::FILE;
                    files_touched.insert(file);
                }
            }
        }
        Edit::BumpFileRevision { file_idx } => {
            if let Some(file_id) = ctx.file_ids.get(file_idx).copied() {
                tier_touches = TierSet::FILE;
                files_touched.insert(file_id);
            }
        }
        Edit::Noop => {}
    }

    // Reinstall a fresh snapshot reflecting whatever mutation happened.
    db.set_snapshot(std::sync::Arc::new(graph.snapshot()));

    // Bump revision counters matching the tier set. Tier 1 is per-file via
    // `FileInputStore::update`, Tier 2 / 3 are global atomic counters.
    if tier_touches.intersects(TierSet::FILE) {
        for fid in &files_touched {
            if let Some(input) = db.inputs_mut().get_mut(*fid) {
                input.update(smallvec::SmallVec::new());
            }
        }
    }
    if tier_touches.intersects(TierSet::EDGE) {
        db.bump_edge_revision();
    }
    if tier_touches.intersects(TierSet::METADATA) {
        db.bump_metadata_revision();
    }

    AppliedEdit {
        edit: edit_owned,
        tier_touches,
        files_touched,
    }
}

// ---------------------------------------------------------------------------
// EditContext — indexed lookup for `apply_edit`
// ---------------------------------------------------------------------------

/// Pre-resolved node-id / file-id table the harness builds once per graph.
#[derive(Debug, Clone)]
pub struct EditContext {
    pub node_ids: Vec<NodeId>,
    pub file_ids: Vec<FileId>,
    /// Per recipe-node-index, the file index that node belongs to. Aligned
    /// with `node_ids` 1:1.
    pub node_file_idx: Vec<usize>,
}

impl EditContext {
    #[must_use]
    pub fn from_generated(graph: &GeneratedGraph) -> Self {
        let node_file_idx: Vec<usize> = graph.recipe.nodes.iter().map(|n| n.file_idx).collect();
        Self {
            node_ids: graph.node_ids.clone(),
            file_ids: graph.file_ids.clone(),
            node_file_idx,
        }
    }

    fn file_for_node(&self, node_idx: usize) -> FileId {
        let fidx = self.node_file_idx[node_idx];
        self.file_ids[fidx]
    }
}

// ---------------------------------------------------------------------------
// Proptest strategy
// ---------------------------------------------------------------------------

/// Generate an arbitrary `Edit`. Index ranges are deliberately wider than the
/// generated graph (modulo handles in `apply_edit` clamp to valid range) so
/// the strategy is independent of any specific graph shape.
pub fn arbitrary_edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        2 => (0usize..64, 0usize..64)
            .prop_map(|(s, t)| Edit::AddCallsEdge { src_idx: s, tgt_idx: t }),
        2 => (0usize..64, 0usize..64)
            .prop_map(|(s, t)| Edit::AddImportsEdge { src_idx: s, tgt_idx: t }),
        2 => (0usize..256).prop_map(|i| Edit::RemoveExistingEdge { edge_idx: i }),
        2 => (0usize..64).prop_map(|i| Edit::MarkAddressTaken { node_idx: i }),
        2 => (0usize..64).prop_map(|i| Edit::MarkCallsitePromiscuous { node_idx: i }),
        2 => (0usize..16).prop_map(|i| Edit::BumpFileRevision { file_idx: i }),
        1 => Just(Edit::Noop),
    ]
}

// ---------------------------------------------------------------------------
// QueryDb construction helper
// ---------------------------------------------------------------------------

/// Build a `(CodeGraph clone, QueryDb)` pair from a `GeneratedGraph` so the
/// harness can mutate the graph independently of the proptest input.
#[must_use]
pub fn fresh_db_and_graph(generated: &GeneratedGraph) -> (CodeGraph, QueryDb) {
    let graph: CodeGraph = (*generated.graph).clone();
    let snapshot: GraphSnapshot = graph.snapshot();
    let db = QueryDb::new(
        std::sync::Arc::new(snapshot),
        sqry_db::QueryDbConfig::default(),
    );
    (graph, db)
}
