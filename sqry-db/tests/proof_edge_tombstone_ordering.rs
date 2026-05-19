//! Phase 3C DB21 Proof Point 5 — Edge-before-node tombstone ordering.
//!
//! From the spec (`docs/superpowers/specs/2026-04-12-derived-analysis-db-query-
//! planner-design.md`, "Proof 5: Edge-before-node tombstone ordering"):
//!
//! > Regression test: index a 2-file fixture where file A calls file B.
//! > Re-index file A. Assert no edge aliasing — old edges from A's nodes
//! > do not appear as edges from the new nodes at the same indices.
//!
//! # Invariant under test
//!
//! [`reindex_files`] must tombstone edges **before** freeing the node
//! slots those edges refer to. The CSR edge store indexes edges by
//! `source.index()` and drops generation. If a node slot is freed first
//! and then re-allocated at a different `(index, generation)`, any
//! lingering edge entries keyed on that slot's old `source.index()` would
//! ghost-attach to the new occupant.
//!
//! The ordering is documented in `sqry_core::graph::unified::build::
//! reindex` as "Codex H1" and in the always-append "Codex H1'" invariant
//! (new segments are always allocated from the append frontier; old
//! ranges enter the free list only for compaction).
//!
//! # Why this test stages its fixture via `allocate_new_segment`
//!
//! `CodeGraph::file_segments_mut` is `pub(crate)`, so external tests
//! cannot directly populate the segment table. `allocate_new_segment`
//! (which `reindex_files` also uses) is the public entrypoint that
//! allocates an arena range AND records it in the segment table. We use
//! it to seed the fixture's per-file segments, then let `reindex_files`
//! exercise the tombstone ordering on top.

use std::path::PathBuf;

use sqry_core::graph::Language;
use sqry_core::graph::unified::build::{allocate_new_segment, reindex_files};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;

/// Seeds a single-node segment for `file_id` and returns the NodeId of
/// the node committed into that segment.
///
/// Uses `allocate_new_segment` to obtain a 1-slot range (which records
/// the segment in the table automatically), writes the node entry into
/// the slot via a second `alloc_range`+`remove` dance that `alloc_range`
/// pre-populated with a placeholder. To replace the placeholder with the
/// real `NodeEntry`, we use `NodeArena::get_mut`.
fn seed_single_node_segment(
    graph: &mut CodeGraph,
    file_id: sqry_core::graph::unified::file::id::FileId,
    entry: NodeEntry,
) -> NodeId {
    let start = allocate_new_segment(graph, file_id, 1).expect("alloc segment");
    // `alloc_range` wrote a placeholder `NodeEntry::new(Other, 0, file)`
    // into the slot; overwrite with the real entry in-place.
    let slot_idx = start;
    let generation = graph.nodes().slot(slot_idx).unwrap().generation();
    let node_id = NodeId::new(slot_idx, generation);
    let place = graph
        .nodes_mut()
        .get_mut(node_id)
        .expect("placeholder slot occupied");
    *place = entry.clone();
    // Register with the name index.
    graph.indices_mut().add(
        node_id,
        entry.kind,
        entry.name,
        entry.qualified_name,
        entry.file,
    );
    node_id
}

/// Builds a 2-file fixture:
///
/// - `src/a.rs` defines `caller_a` which calls `target_b`
/// - `src/b.rs` defines `target_b`
///
/// Both files get single-node segments recorded in the file-segment
/// table so `reindex_files` can find them.
fn build_fixture() -> (CodeGraph, PathBuf, NodeId) {
    let mut graph = CodeGraph::new();
    let file_a_path = PathBuf::from("src/a.rs");
    let file_b_path = PathBuf::from("src/b.rs");

    let file_a = graph
        .files_mut()
        .register_with_language(&file_a_path, Some(Language::Rust))
        .expect("register a.rs");
    let file_b = graph
        .files_mut()
        .register_with_language(&file_b_path, Some(Language::Rust))
        .expect("register b.rs");

    let caller_name = graph.strings_mut().intern("caller_a").expect("intern");
    let target_name = graph.strings_mut().intern("target_b").expect("intern");

    let caller_a = seed_single_node_segment(
        &mut graph,
        file_a,
        NodeEntry::new(NodeKind::Function, caller_name, file_a)
            .with_qualified_name(caller_name)
            .with_byte_range(0, 80),
    );
    let target_b = seed_single_node_segment(
        &mut graph,
        file_b,
        NodeEntry::new(NodeKind::Function, target_name, file_b)
            .with_qualified_name(target_name)
            .with_byte_range(0, 80),
    );

    graph.edges().add_edge(
        caller_a,
        target_b,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        file_a,
    );

    (graph, file_a_path, caller_a)
}

#[test]
fn proof5_reindex_tombstones_old_edges_and_frees_slots() {
    let (mut graph, file_a_path, caller_a_id) = build_fixture();

    // Pre-reindex sanity: outgoing edges from `caller_a` include the
    // `Calls -> target_b` edge.
    let pre_outgoing = graph.edges().edges_from(caller_a_id).len();
    assert_eq!(
        pre_outgoing, 1,
        "caller_a must have exactly one outgoing edge before reindex"
    );

    // Drive the reindex tombstone path for file_a only.
    let stats = reindex_files(&mut graph, std::slice::from_ref(&file_a_path));
    assert_eq!(stats.files_reindexed, 1);
    assert_eq!(stats.nodes_tombstoned, 1);
    assert!(
        stats.edges_tombstoned >= 1,
        "at least one edge tombstone must be recorded (edges-before-nodes ordering)"
    );

    // The old slot index must now be vacant — node tombstoning occurred.
    let old_slot = graph.nodes().slot(caller_a_id.index()).unwrap();
    assert!(
        !old_slot.is_occupied(),
        "old caller_a slot must be vacant after reindex"
    );

    // The segment table entry for file_a must be cleared (reindex removes
    // it so that the next parse/commit cycle can re-record a new range).
    let caller_file = graph.files().get(&file_a_path).expect("file_a_path");
    assert!(
        graph.file_segments().get(caller_file).is_none(),
        "file_a's segment table entry must be removed after reindex"
    );
}

#[test]
fn proof5_no_edge_aliasing_after_reindex_and_fresh_commit() {
    // The headline regression: after reindex tombstones caller_a's
    // outgoing edges AND frees its slot, a freshly-committed `caller_a`
    // at a NEW arena index (always-append per Codex H1') must start
    // with ZERO outgoing edges. If ordering were reversed (node freed
    // BEFORE edges tombstoned), the CSR lookup on the new slot would
    // surface ghost entries for the old edges.
    let (mut graph, file_a_path, old_caller_id) = build_fixture();
    let old_caller_slot = old_caller_id.index();

    let _ = reindex_files(&mut graph, std::slice::from_ref(&file_a_path));

    // Commit a fresh caller_a node via the public seed path. This
    // allocates a brand-new segment (always-append), so new_start !=
    // old_caller_slot.
    let caller_name = graph.strings_mut().intern("caller_a").expect("intern");
    let caller_file = graph.files().get(&file_a_path).expect("file_a_path");

    let new_caller_id = seed_single_node_segment(
        &mut graph,
        caller_file,
        NodeEntry::new(NodeKind::Function, caller_name, caller_file)
            .with_qualified_name(caller_name)
            .with_byte_range(0, 80),
    );

    assert_ne!(
        new_caller_id.index(),
        old_caller_slot,
        "Codex H1' invariant: the fresh segment must NOT reuse the \
         tombstoned slot range"
    );

    // Core regression assertion: zero edges on the new node.
    let new_outgoing = graph.edges().edges_from(new_caller_id).len();
    assert_eq!(
        new_outgoing, 0,
        "the replacement caller_a must start with ZERO outgoing edges — \
         no edge aliasing from the tombstoned slot"
    );
    let new_incoming = graph.edges().edges_to(new_caller_id).len();
    assert_eq!(
        new_incoming, 0,
        "the replacement caller_a must start with ZERO incoming edges"
    );
}

#[test]
fn proof5_append_only_allocation_never_reuses_tombstoned_range() {
    // Codex H1' invariant: `allocate_new_segment` never re-issues the
    // tombstoned range. The old range is available to compaction only;
    // live allocators must walk past it.
    let (mut graph, file_a_path, old_caller_id) = build_fixture();
    let old_start = old_caller_id.index();

    let _ = reindex_files(&mut graph, std::slice::from_ref(&file_a_path));

    let caller_file = graph.files().get(&file_a_path).expect("file_a_path");
    let new_start = allocate_new_segment(&mut graph, caller_file, 1).expect("alloc");
    assert!(
        new_start > old_start,
        "always-append: new_start ({new_start}) must be strictly greater \
         than the tombstoned range's start ({old_start})"
    );
}

#[test]
fn proof5_reindex_with_no_matching_file_is_a_noop() {
    // Defence-in-depth: calling `reindex_files` with a path not in the
    // registry must skip cleanly and not disturb existing edges.
    let (mut graph, _file_a_path, caller_a_id) = build_fixture();

    let stats = reindex_files(&mut graph, &[PathBuf::from("nonexistent/ghost.rs")]);
    assert_eq!(stats.files_reindexed, 0);
    assert_eq!(stats.files_skipped, 1);

    // Edge store is intact.
    assert_eq!(graph.edges().edges_from(caller_a_id).len(), 1);
}
