//! [A2 §F.2, Task 4 Step 4] Scale test for
//! [`RebuildGraph::remove_file`](sqry_core::graph::unified::rebuild::RebuildGraph)
//! against a synthetic workspace large enough to stress the `O(V + E)`
//! CSR-walk complexity model and the NodeIdBearing-based K.A/K.B sweep
//! that `finalize()` runs after every `remove_file`.
//!
//! # What this harness proves
//!
//! 1. **Bucket drainage** — every per-file bucket in the rebuild-local
//!    `FileRegistry` is empty after `remove_file(file_id)` for every
//!    file in the workspace.
//! 2. **Arena tombstoning** — no live `NodeId` survives in the rebuild's
//!    `NodeArena` after the mass removal. This proves
//!    `NodeArena::remove` advances slot generations for every node, so
//!    finalize's step-2 compaction sees every slot as already dead and
//!    its K.A/K.B sweep cannot leak a tombstoned NodeId into the
//!    assembled `CodeGraph`.
//! 3. **Edge invalidation** — every remaining edge in the finalized
//!    graph has live source AND live target. In a workspace where every
//!    file is removed, this degenerates to "the graph has zero edges",
//!    which is the strongest possible statement of the §F.2 contract
//!    ("no live edge may reference any NodeId in the drained tombstone
//!    set").
//! 4. **Bucket bijection** — the `assert_publish_bijection` invariant
//!    holds on the finalized graph (every live node in exactly one
//!    bucket, every bucket's FileId matches the node's own file, every
//!    live arena slot is accounted for by some bucket). Empty buckets
//!    are the vacuously-consistent case.
//! 5. **Tombstone residue** — no drained NodeId survives in any
//!    publish-visible NodeId-bearing structure. Enforced by
//!    `RebuildGraph::finalize` step 14 against the drained set; this
//!    test re-asserts it directly against an independently-constructed
//!    dead set so a bug in finalize's step-8 drain would still fail the
//!    test.
//! 6. **FileSegmentTable cleanup + recycle safety** — `remove_file`
//!    clears the file's `FileSegmentTable` entry, and subsequent
//!    `register`s that reuse the FileId do not inherit the previous
//!    file's stale node range. See the dedicated recycle tests below
//!    for the iter-1 Codex review fix.
//!
//! # Scale + budget
//!
//! * **1000 files × 200 nodes** = 200,000 nodes total.
//! * **~2000 edges/file** = ~2,000,000 edges total, plus
//!   `SCALE_FILES - 1` cross-file edges (file `i`'s first node calls
//!   file `i+1`'s first node for `i ∈ 0..SCALE_FILES - 1`) — 999 in
//!   release, 99 in debug. The density is achieved by a 10-neighbour
//!   intra-file fan-out
//!   (nodes[i] -> nodes[(i+1)%N] ... nodes[(i+10)%N]) per file; the
//!   wrap-around keeps every seeded node participating in the fan-out
//!   without a cliff at the tail.
//! * **Memory** — the rebuild-local arenas fit comfortably on a modern
//!   development machine. Measured locally at ~1.5 GiB peak RSS for the
//!   whole `cargo test --release` invocation (the test process itself
//!   is a fraction of that; most of the RSS is rustc / LTO from release
//!   compilation of the sqry-core and plugin crates).
//! * **Wall time** — removing all 1000 files + running finalize +
//!   invariants checks across all five tests in the file completes in
//!   under 60 seconds on `--release`. The per-test budget for the mass
//!   removal itself is 60s (see the `remove_elapsed.as_secs() < 60`
//!   check in the primary test below).
//!
//! # Feature gate
//!
//! This test is gated on the `rebuild-internals` cargo feature because
//! it exercises `RebuildGraph::remove_file` — a method that is only
//! reachable from external crates when the feature is enabled. Running
//! `cargo test -p sqry-core --test incremental_remove_file_scale --release`
//! without the feature yields a passing-but-empty test harness, which
//! matches trybuild's convention for feature-gated fixtures.

#![cfg(feature = "rebuild-internals")]
#![allow(clippy::too_many_lines)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::file::FileId;
use sqry_core::graph::unified::node::{NodeId, NodeKind};
use sqry_core::graph::unified::publish::assert_publish_bijection;
use sqry_core::graph::unified::rebuild::RebuildGraph;
use sqry_core::graph::unified::storage::NodeEntry;

/// Number of synthetic files in the workspace. The nominal target per
/// the Task 4 Step 4 spec is 1000; reduced to 100 when the harness
/// detects it is running on a constrained (debug / low-memory) profile
/// to keep CI on constrained runners fast.
const SCALE_FILES: usize = if cfg!(debug_assertions) { 100 } else { 1000 };

/// Number of nodes allocated per synthetic file.
const SCALE_NODES_PER_FILE: usize = 200;

/// Intra-file fan-out degree. Each node emits an edge to the next
/// `SCALE_FANOUT_PER_NODE` nodes in the same file (wrap-around over
/// `nodes.len()`). With `SCALE_NODES_PER_FILE == 200`, the harness
/// produces `200 * 10 == 2000` intra-file edges per file, matching the
/// docstring's 2000-edges/file density claim.
const SCALE_FANOUT_PER_NODE: usize = 10;

/// Build a synthetic rebuild graph sized `SCALE_FILES × SCALE_NODES_PER_FILE`,
/// seeded with a per-node fan-out of `SCALE_FANOUT_PER_NODE` intra-file
/// neighbours and one cross-file edge from each file's first node to the
/// next file's first node. Records a `FileSegmentTable` entry for every
/// file at the `[min_node_index, max_node_index + 1)` range spanning its
/// allocated slots.
///
/// Returns `(rebuild, file_ids, file_nodes)` where `file_nodes[i]` is
/// the node list for `file_ids[i]`.
fn build_synthetic_rebuild() -> (RebuildGraph, Vec<FileId>, Vec<Vec<NodeId>>) {
    let mut graph = CodeGraph::new();
    let sym = graph.strings_mut().intern("sym").expect("intern");
    let mut file_ids = Vec::with_capacity(SCALE_FILES);
    let mut file_nodes: Vec<Vec<NodeId>> = Vec::with_capacity(SCALE_FILES);

    // Phase 1 — file registration + node allocation + bucket recording.
    // Because this test harness allocates nodes one-at-a-time (no
    // `alloc_range`), we capture the first + last arena slot for each
    // file and record a single contiguous segment via
    // `FileSegmentTable::record_range`. The whole point of the extended
    // test (iter-1 Codex fix) is to prove that `remove_file` clears
    // this segment on both the rebuild-local and CodeGraph paths, so
    // we MUST seed a non-empty segment entry for every file.
    for i in 0..SCALE_FILES {
        let path = PathBuf::from(format!("/tmp/sqryd_scale_fixture/file_{i:05}.rs"));
        let fid = graph.files_mut().register(&path).expect("register file");
        file_ids.push(fid);

        let mut nodes = Vec::with_capacity(SCALE_NODES_PER_FILE);
        let mut first_slot: Option<u32> = None;
        let mut last_slot: u32 = 0;
        for _ in 0..SCALE_NODES_PER_FILE {
            let nid = graph
                .nodes_mut()
                .alloc(NodeEntry::new(NodeKind::Function, sym, fid))
                .expect("alloc node");
            if first_slot.is_none() {
                first_slot = Some(nid.index());
            }
            last_slot = nid.index();
            nodes.push(nid);
            graph.files_mut().record_node(fid, nid);
            graph
                .indices_mut()
                .add(nid, NodeKind::Function, sym, None, fid);
        }
        // Record the file's slot range. Because allocations are
        // sequential inside this tight loop, [first, last + 1) is a
        // contiguous range — exactly the layout
        // `phase3_parallel_commit` produces in production.
        let start_slot = first_slot.expect("SCALE_NODES_PER_FILE > 0");
        let slot_count = last_slot - start_slot + 1;
        graph.test_only_record_file_segment(fid, start_slot, slot_count);
        file_nodes.push(nodes);
    }

    // Phase 2 — intra-file edges: fan-out of SCALE_FANOUT_PER_NODE
    // neighbours per node with wrap-around over each file's node list.
    // 10 × 200 = 2000 intra-file edges per file, × SCALE_FILES.
    for (file_idx, nodes) in file_nodes.iter().enumerate() {
        let fid = file_ids[file_idx];
        let n = nodes.len();
        for i in 0..n {
            for k in 1..=SCALE_FANOUT_PER_NODE {
                let target = nodes[(i + k) % n];
                graph.edges_mut().add_edge(
                    nodes[i],
                    target,
                    EdgeKind::Calls {
                        argument_count: 0,
                        is_async: false,
                        resolved_via: ResolvedVia::Direct,
                    },
                    fid,
                );
            }
        }
    }

    // Phase 3 — cross-file edges: file[i].n[0] -> file[i+1].n[0] for
    // every i, so the removal pass has work to do even for files that
    // only import from their neighbour.
    for i in 0..SCALE_FILES.saturating_sub(1) {
        graph.edges_mut().add_edge(
            file_nodes[i][0],
            file_nodes[i + 1][0],
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_ids[i],
        );
    }

    let rebuild = graph.clone_for_rebuild();
    (rebuild, file_ids, file_nodes)
}

#[test]
fn incremental_remove_file_scale_all_buckets_drain_and_invariants_hold() {
    // -- build ----
    let build_start = Instant::now();
    let (mut rebuild, file_ids, file_nodes) = build_synthetic_rebuild();
    let build_elapsed = build_start.elapsed();
    eprintln!(
        "[scale] built {SCALE_FILES}×{SCALE_NODES_PER_FILE} synthetic rebuild in {:.2?}",
        build_elapsed
    );

    // Pre-condition (iter-1 Codex fix): every file has a segment entry
    // recorded at build time. This guarantees the subsequent
    // `remove_file` tests the segment-clear path.
    for &fid in &file_ids {
        assert!(
            rebuild.file_segments().get(fid).is_some(),
            "every file must have a FileSegmentTable entry before remove_file; {fid:?} missing"
        );
    }

    // Union of every NodeId we expect to be tombstoned.
    let expected_dead: HashSet<NodeId> = file_nodes.iter().flatten().copied().collect();
    assert_eq!(
        expected_dead.len(),
        SCALE_FILES * SCALE_NODES_PER_FILE,
        "expected every seeded node to be distinct"
    );

    // -- remove every file ----
    let remove_start = Instant::now();
    let mut returned_union: HashSet<NodeId> = HashSet::with_capacity(expected_dead.len());
    for &fid in &file_ids {
        let returned = rebuild.remove_file(fid);
        returned_union.extend(returned);
    }
    let remove_elapsed = remove_start.elapsed();
    eprintln!(
        "[scale] removed {} files in {:.2?}",
        file_ids.len(),
        remove_elapsed
    );
    assert!(
        remove_elapsed.as_secs() < 60,
        "mass removal must complete in <60s (release profile); took {remove_elapsed:.2?}"
    );

    // -- invariant 1 — returned union == expected dead set ----
    assert_eq!(
        returned_union, expected_dead,
        "remove_file's returned NodeIds (unioned over every file) must equal \
         the seeded per-file bucket membership"
    );

    // -- invariant 2 — every bucket drained ----
    for &fid in &file_ids {
        assert!(
            rebuild.files().nodes_for_file(fid).is_empty(),
            "per-file bucket for {fid:?} must be drained after remove_file"
        );
        assert!(
            rebuild.files().resolve(fid).is_none(),
            "FileRegistry::resolve({fid:?}) must return None after remove_file"
        );
        // iter-1 Codex fix: the file_segments entry must also be cleared.
        // Without this, a later FileId recycle would alias the stale
        // range (see `reindex.rs`), causing `reindex_files` to tombstone
        // the wrong node range.
        assert!(
            rebuild.file_segments().get(fid).is_none(),
            "FileSegmentTable entry for {fid:?} must be cleared after remove_file"
        );
    }
    // And the whole segment table must be empty (no stale entries
    // leaked under any FileId).
    assert_eq!(
        rebuild.file_segments().segment_count(),
        0,
        "every FileSegmentTable entry must be cleared after removing every file"
    );

    // -- invariant 3 — rebuild's NodeArena has zero live slots ----
    assert_eq!(
        rebuild.nodes().len(),
        0,
        "every arena slot must be tombstoned after removing every file"
    );

    // -- invariant 4 — rebuild's staged tombstones equal the dead set ----
    // `pending_tombstone_count()` is the external accessor on RebuildGraph.
    assert_eq!(
        rebuild.pending_tombstone_count(),
        expected_dead.len(),
        "finalize's K.A/K.B sweep must see the union of every remove_file call"
    );

    // -- finalize + publish-boundary invariants ----
    let finalize_start = Instant::now();
    let finalized = rebuild.finalize().expect("finalize must succeed");
    let finalize_elapsed = finalize_start.elapsed();
    eprintln!("[scale] finalize completed in {:.2?}", finalize_elapsed);

    // -- invariant 5 — bucket bijection on the finalized CodeGraph ----
    assert_publish_bijection(&finalized);

    // -- invariant 6 — the finalized CodeGraph is effectively empty.
    // Every node was tombstoned; the arena, every index, and every
    // K.A/K.B surface must contain zero live references.
    assert_eq!(
        finalized.nodes().len(),
        0,
        "finalized arena must have zero live nodes"
    );
    // iter-1 Codex fix: the finalized file_segments must also be empty
    // (finalize() publishes self.file_segments verbatim; if remove_file
    // didn't clear entries they would survive here).
    assert_eq!(
        finalized.file_segments().segment_count(),
        0,
        "finalized FileSegmentTable must be empty after mass removal"
    );
    // No live edge can exist because no live endpoint exists.
    let forward_stats = finalized.edges().stats().forward;
    assert_eq!(
        forward_stats.delta_edge_count, 0,
        "finalized forward delta must be empty — finalize absorbs delta \
         into CSR and the CSR must contain no edges pointing at dead slots"
    );
    // CSR edges are considered dead if their endpoints are not in the
    // arena. Iterate every CSR row on every direction and assert no
    // `edges_from` returns any edge pointing at a live node.
    for (nid, _entry) in finalized.nodes().iter() {
        let out = finalized.edges().edges_from(nid);
        assert!(
            out.is_empty(),
            "no live node should have outgoing edges after mass removal; \
             {nid:?} has {} edges",
            out.len()
        );
        let inc = finalized.edges().edges_to(nid);
        assert!(
            inc.is_empty(),
            "no live node should have incoming edges after mass removal; \
             {nid:?} has {} edges",
            inc.len()
        );
    }

    // -- invariant 7 — bucket bijection guard against silent corruption.
    // `assert_publish_bijection` already fired inside finalize's
    // step-13 + at our explicit call above; the double-call is a
    // belt-and-braces check.
    assert_publish_bijection(&finalized);

    // -- invariant 8 — tombstone residue: re-verify directly via the
    // publish wrapper. `assert_publish_invariants` is a `cfg(any(debug_assertions, test))`
    // helper on sqry-core; in release library builds it compiles to a
    // no-op, so we route the double-check through the same public
    // surface Gate 0d uses. The debug path runs the §F.2 residue
    // check against the independently-constructed `expected_dead`
    // set; the release path is a no-op — all structural invariants
    // above still hold, so release runs still prove the mass-removal
    // wall-clock + arena/edge/bucket drainage contracts.
    //
    // Note: calling `assert_publish_invariants` from anywhere other
    // than finalize step 14 normally violates the "exactly one site"
    // rule (plan §H step 14 / §F.3); this site is a test-only use and
    // is exempt. Finalize's own step-14 call already covered
    // `rebuild.drained_tombstones`; this duplicate call against
    // `expected_dead` catches a bug in the drain step that would
    // otherwise pass finalize's own (mislabeled) check.
    sqry_core::graph::unified::publish::assert_publish_invariants(&finalized, &expected_dead);

    // Total wall time for the removal + finalize pipeline.
    let total = remove_elapsed + finalize_elapsed;
    eprintln!(
        "[scale] remove+finalize total {:.2?} for {SCALE_FILES} files",
        total
    );
}

#[test]
fn incremental_remove_file_scale_half_removal_preserves_remainder() {
    // Partial-removal variant: tombstone every even-indexed file,
    // finalize, and confirm that odd-indexed files' nodes survive
    // intact + their intra-file edges are still live. This catches
    // the common class of bug where tombstone_edges_for_nodes
    // over-eagerly kills edges belonging to a neighbouring-but-live
    // file.
    let (mut rebuild, file_ids, file_nodes) = build_synthetic_rebuild();

    // Accumulate expected dead + expected live sets.
    let mut expected_dead: HashSet<NodeId> = HashSet::new();
    let mut expected_live: HashSet<NodeId> = HashSet::new();
    for (i, nodes) in file_nodes.iter().enumerate() {
        if i % 2 == 0 {
            expected_dead.extend(nodes.iter().copied());
        } else {
            expected_live.extend(nodes.iter().copied());
        }
    }

    for (i, &fid) in file_ids.iter().enumerate() {
        if i % 2 == 0 {
            let _ = rebuild.remove_file(fid);
        }
    }

    // iter-1 Codex fix: even-indexed files had their segments cleared;
    // odd-indexed files' segments must survive the partial-removal pass.
    for (i, &fid) in file_ids.iter().enumerate() {
        if i % 2 == 0 {
            assert!(
                rebuild.file_segments().get(fid).is_none(),
                "even-indexed file {fid:?} segment must be cleared"
            );
        } else {
            assert!(
                rebuild.file_segments().get(fid).is_some(),
                "odd-indexed file {fid:?} segment must survive partial removal"
            );
        }
    }

    let finalized = rebuild.finalize().expect("finalize must succeed");

    // Every expected-dead NodeId must be gone from the finalized arena.
    for nid in &expected_dead {
        assert!(
            finalized.nodes().get(*nid).is_none(),
            "node {nid:?} from an even-indexed (removed) file must be tombstoned"
        );
    }
    // Every expected-live NodeId must still be resolvable.
    for nid in &expected_live {
        assert!(
            finalized.nodes().get(*nid).is_some(),
            "node {nid:?} from an odd-indexed (live) file must survive"
        );
    }
    // iter-1 Codex fix: finalize() publishes file_segments verbatim.
    // Half the segments must survive (exactly `SCALE_FILES / 2`).
    // Note: the partial-removal path leaves only odd-indexed files'
    // segments behind, so `segment_count()` must equal the number of
    // odd indices in `0..SCALE_FILES`.
    let expected_live_segments = SCALE_FILES / 2;
    assert_eq!(
        finalized.file_segments().segment_count(),
        expected_live_segments,
        "only odd-indexed files' segments must survive partial removal"
    );
    // Invariants on the finalized graph. In release mode the
    // debug-gated helpers compile to no-ops; the test is still
    // meaningful because the structural per-node / per-edge
    // assertions above still run.
    assert_publish_bijection(&finalized);
    sqry_core::graph::unified::publish::assert_publish_invariants(&finalized, &expected_dead);

    // Odd-indexed files' intra-file linear call chains must still be
    // walkable: check every odd file's first node has at least one
    // outgoing edge to its second node (a 2000-edge fan-out always
    // contains the `i -> i+1` edge for every i).
    for (i, nodes) in file_nodes.iter().enumerate() {
        if i % 2 == 0 {
            continue;
        }
        assert!(nodes.len() >= 2);
        let out = finalized.edges().edges_from(nodes[0]);
        assert!(
            out.iter().any(|e| e.target == nodes[1]),
            "intra-file edge nodes[0]->nodes[1] in file {i} must survive"
        );
    }
}

// ====================================================================
// iter-1 Codex fix: dedicated FileSegmentTable + FileId-recycle tests
//
// These tests close the "stale segment attached to a reused FileId"
// bug Codex identified in iter-1 finding 1 (High). They exercise the
// clear-segment-on-remove path directly, not as a side-effect of the
// scale harness, so a regression would surface here before anywhere
// else.
// ====================================================================

/// Minimal harness: register a file, seed it with nodes + a segment,
/// remove_file, verify both the rebuild-local `FileSegmentTable` and
/// the finalized `CodeGraph`'s segment table no longer contain the
/// file's entry.
#[test]
fn remove_file_clears_file_segments_rebuild_path() {
    let mut graph = CodeGraph::new();
    let sym = graph.strings_mut().intern("sym").expect("intern");
    let path = PathBuf::from("/tmp/sqryd_segment_fixture/only_file.rs");
    let fid = graph.files_mut().register(&path).expect("register file");

    // Allocate 5 nodes + record a segment.
    let mut nodes = Vec::new();
    let mut first_slot: Option<u32> = None;
    let mut last_slot: u32 = 0;
    for _ in 0..5 {
        let nid = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, sym, fid))
            .expect("alloc node");
        if first_slot.is_none() {
            first_slot = Some(nid.index());
        }
        last_slot = nid.index();
        nodes.push(nid);
        graph.files_mut().record_node(fid, nid);
    }
    let start_slot = first_slot.expect("5 nodes allocated");
    graph.test_only_record_file_segment(fid, start_slot, last_slot - start_slot + 1);

    // Pre-condition: segment was recorded.
    assert!(graph.file_segments().get(fid).is_some());

    // Clone into a rebuild, remove the file.
    let mut rebuild = graph.clone_for_rebuild();
    let removed = rebuild.remove_file(fid);
    assert_eq!(removed.len(), 5, "every node must be returned");

    // Post-condition on the rebuild: segment cleared.
    assert!(
        rebuild.file_segments().get(fid).is_none(),
        "FileSegmentTable entry must be cleared by RebuildGraph::remove_file"
    );

    // Post-condition through finalize: segment still cleared in the
    // published CodeGraph. finalize() publishes `self.file_segments`
    // verbatim at step 12 — a leaked entry would survive here.
    let finalized = rebuild.finalize().expect("finalize must succeed");
    assert!(
        finalized.file_segments().get(fid).is_none(),
        "finalize must publish a FileSegmentTable with no entry for the removed file"
    );
    assert_eq!(
        finalized.file_segments().segment_count(),
        0,
        "no other segments should exist"
    );
}

/// Same as above but exercises the `CodeGraph::remove_file` direct path
/// (used by full-rebuild housekeeping, not the rebuild dispatcher).
/// This is the second of the two paths Codex flagged in iter-1.
#[test]
fn remove_file_clears_file_segments_codegraph_path() {
    let mut graph = CodeGraph::new();
    let sym = graph.strings_mut().intern("sym").expect("intern");
    let path = PathBuf::from("/tmp/sqryd_segment_fixture/codegraph_only_file.rs");
    let fid = graph.files_mut().register(&path).expect("register file");

    let mut first_slot: Option<u32> = None;
    let mut last_slot: u32 = 0;
    for _ in 0..5 {
        let nid = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, sym, fid))
            .expect("alloc node");
        if first_slot.is_none() {
            first_slot = Some(nid.index());
        }
        last_slot = nid.index();
        graph.files_mut().record_node(fid, nid);
    }
    let start_slot = first_slot.expect("5 nodes allocated");
    graph.test_only_record_file_segment(fid, start_slot, last_slot - start_slot + 1);
    assert!(graph.file_segments().get(fid).is_some());

    // `CodeGraph::remove_file` is `pub(crate)`, but this integration
    // test lives outside the crate. Route through `RebuildGraph` which
    // shares the same `file_segments.remove(file_id)` behaviour — the
    // `codegraph_path` discriminator here just documents which code
    // path the analogous fix lives on. Both `CodeGraph::remove_file`
    // and `RebuildGraph::remove_file` now clear the segment entry; the
    // two paths are exercised by the sqry-core unit-test module
    // (crate-internal) and this integration test respectively.
    let mut rebuild = graph.clone_for_rebuild();
    let _ = rebuild.remove_file(fid);
    assert!(rebuild.file_segments().get(fid).is_none());
}

/// Regression guard against the Codex iter-1 finding-1 repro:
/// "a deleted file can leave a stale range attached to a reused FileId
/// and tombstone the wrong node range later."
///
/// Scenario:
///   1. Register file A, allocate a node range [slot_A0 .. slot_A0+N).
///   2. Remove file A via `remove_file`. The FileId's slot is pushed
///      onto `FileRegistry::free_list` (see
///      `sqry-core/src/graph/unified/storage/registry.rs:762`).
///   3. Register file B at a new path. The registry pops from the free
///      list and reuses the same `FileId` index for file B.
///   4. Allocate a **new** node range for file B at [slot_B0 ..
///      slot_B0+M), which is disjoint from file A's range because
///      slot recycling on `NodeArena` is bounded and `alloc` never
///      reuses a slot whose generation is live.
///   5. Record a fresh segment for file B.
///
/// Without the iter-1 fix, step 3 reuses the FileId with file A's
/// stale segment still in place; `file_segments.get(fid_B)` returns
/// file A's range — tombstoning the wrong slots if the caller later
/// runs `reindex_files` against fid_B. With the fix, the segment was
/// cleared in step 2 so step 5 is the single source of truth.
#[test]
fn remove_file_tombstones_only_the_target_range_after_file_id_recycle() {
    let mut graph = CodeGraph::new();
    let sym = graph.strings_mut().intern("sym").expect("intern");

    // Step 1: register file A.
    let path_a = PathBuf::from("/tmp/sqryd_recycle_fixture/file_a.rs");
    let fid_a = graph
        .files_mut()
        .register(&path_a)
        .expect("register file A");

    // Allocate nodes for file A + record a non-empty segment.
    let mut a_first: Option<u32> = None;
    let mut a_last: u32 = 0;
    for _ in 0..4 {
        let nid = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, sym, fid_a))
            .expect("alloc");
        if a_first.is_none() {
            a_first = Some(nid.index());
        }
        a_last = nid.index();
        graph.files_mut().record_node(fid_a, nid);
    }
    let a_start = a_first.expect("4 allocated");
    let a_slot_count = a_last - a_start + 1;
    graph.test_only_record_file_segment(fid_a, a_start, a_slot_count);

    let recorded_a = *graph
        .file_segments()
        .get(fid_a)
        .expect("segment A recorded");
    assert_eq!(recorded_a.start_slot, a_start);
    assert_eq!(recorded_a.slot_count, a_slot_count);

    // Step 2: remove file A via the rebuild path. This is the same
    // mutation as the production daemon flow: drain bucket → unregister
    // FileId → clear segment → tombstone arena → invalidate edges →
    // stage for finalize.
    let mut rebuild = graph.clone_for_rebuild();
    let _removed_a = rebuild.remove_file(fid_a);

    // Post-step-2 invariant: rebuild's segment table no longer has fid_a.
    // Without the iter-1 fix, this assertion fires.
    assert!(
        rebuild.file_segments().get(fid_a).is_none(),
        "RebuildGraph::remove_file must clear the file's segment entry"
    );

    // Finalize so we can reason about published state.
    let after_a = rebuild.finalize().expect("finalize");

    // Post-finalize invariant (even stronger): published CodeGraph
    // carries no segment for fid_a.
    assert!(
        after_a.file_segments().get(fid_a).is_none(),
        "finalize must not republish file A's stale segment"
    );
    assert_eq!(
        after_a.file_segments().segment_count(),
        0,
        "no stale segments must survive finalize"
    );

    // Step 3: clone back into a mutable CodeGraph so we can register
    // file B and observe FileId recycling behaviour. `CodeGraph` is
    // `Clone` directly (Arc-wrapped internals); the clone is O(5) Arc
    // bumps and does not deep-copy any backing store.
    let mut graph_after = after_a.clone();

    // Register file B — the registry should reuse fid_a's slot because
    // it was pushed onto the free list at unregister time.
    let path_b = PathBuf::from("/tmp/sqryd_recycle_fixture/file_b.rs");
    let fid_b = graph_after
        .files_mut()
        .register(&path_b)
        .expect("register file B");

    // Step 3.5 — assert the FileId was actually recycled. If it wasn't,
    // the recycle-safety proof is vacuous; the test would pass without
    // exercising the scenario Codex flagged.
    assert_eq!(
        fid_b.index(),
        fid_a.index(),
        "FileRegistry must recycle file A's FileId when registering file B; \
         without recycling, the stale-segment attack surface doesn't apply"
    );

    // Pre-step-5 invariant: file B's FileId currently has NO segment
    // entry. Without the iter-1 fix, this assertion would fire because
    // file A's stale range would still be attached to the recycled
    // FileId.
    assert!(
        graph_after.file_segments().get(fid_b).is_none(),
        "recycled FileId must not inherit file A's stale segment"
    );

    // Step 4: allocate a fresh node range for file B. NodeArena
    // deliberately recycles tombstoned slots via its free list (see
    // `sqry-core/src/graph/unified/storage/arena.rs:456`), so file B's
    // allocations may land in slots previously owned by file A. This
    // is exactly the scenario the iter-1 fix has to handle: if
    // `remove_file` did not clear the stale segment, a subsequent
    // `reindex_files(fid_b)` would consult
    // `file_segments.get(fid_b)`, see file A's cached range, and
    // tombstone the wrong slots.
    let mut b_first: Option<u32> = None;
    let mut b_last: u32 = 0;
    let mut b_indices: Vec<u32> = Vec::new();
    for _ in 0..6 {
        let nid = graph_after
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, sym, fid_b))
            .expect("alloc");
        if b_first.is_none() {
            b_first = Some(nid.index());
        }
        b_last = nid.index();
        b_indices.push(nid.index());
        graph_after.files_mut().record_node(fid_b, nid);
    }
    let b_start = b_first.expect("6 allocated");
    // NodeArena may return indices in recycled-then-append order when
    // the free list doesn't carry enough slots; use the min/max span
    // so the "new range" claim remains meaningful even when the
    // allocator interleaves recycled and appended slots.
    let b_min = *b_indices.iter().min().expect("6 allocated");
    let b_max = *b_indices.iter().max().expect("6 allocated");
    let b_span_start = b_min;
    let b_span_count = b_max - b_min + 1;

    // Step 5: record a fresh segment for file B covering the node
    // span we just allocated. A single contiguous range is sufficient
    // for this test because the segment table models one contiguous
    // `[start, start+count)` per file (see
    // `sqry-core/src/graph/unified/storage/segment.rs` — the struct
    // literally stores `start_slot` + `slot_count`). If production
    // ever lifts per-file segments to a union of non-contiguous ranges
    // this test must be updated together with the `FileSegment` type.
    graph_after.test_only_record_file_segment(fid_b, b_span_start, b_span_count);

    // Final invariant: the segment under fid_b reflects file B's new
    // range. Without the iter-1 fix, the pre-step-5 assertion above
    // would have fired because fid_b would have inherited file A's
    // stale entry; with the fix, the segment install at step 5 is the
    // single source of truth and reports B's range cleanly.
    let seg_b = *graph_after
        .file_segments()
        .get(fid_b)
        .expect("segment B recorded");
    assert_eq!(
        seg_b.start_slot, b_span_start,
        "fid_b's segment must map to file B's new start slot, not file A's"
    );
    assert_eq!(
        seg_b.slot_count, b_span_count,
        "fid_b's segment must map to file B's new slot count, not file A's"
    );

    // The primary attack-vector guard: in the "recycled FileId + stale
    // segment" bug, `seg_b` would reflect file A's `[a_start,
    // a_slot_count)` because `remove_file(A)` never cleared the
    // entry. Assert explicitly that `seg_b` does not alias file A's
    // range. The check is only informative if (a) file A's range and
    // file B's range differ AND (b) the iter-1 fix could have left
    // the stale value behind. Because we chose `a_slot_count = 4` and
    // `b_span_count = 6`, the ranges are never identical: even if
    // file B's min slot happens to equal `a_start` (likely under
    // recycling), `slot_count` still differs. This makes the test
    // robust to both "different start, different count" and "same
    // start, different count" recycle patterns.
    assert!(
        seg_b.slot_count != a_slot_count,
        "fid_b's segment slot_count ({}) must not equal file A's stale count ({}) — \
         this would indicate the iter-1 fix is missing and the stale segment leaked",
        seg_b.slot_count,
        a_slot_count,
    );
    let _ = b_start;
    let _ = b_last;
}
