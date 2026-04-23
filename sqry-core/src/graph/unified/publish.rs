//! Publish-boundary invariants (sqryd daemon — A2 §F / Task 4 Gate 0d).
//!
//! This module is the **single source of truth** for invariant assertions
//! that must hold every time a `CodeGraph` is about to become visible to
//! queries. The two invariants live under two distinct helpers so the
//! plan's "exactly one assembled-`CodeGraph` residue call site" rule is
//! enforced by the type system rather than by convention.
//!
//! # Two helpers, two contracts (plan §F.3 + §H step 14)
//!
//! ## [`assert_publish_bijection`] — every publish boundary
//!
//! Called at every site where a `CodeGraph` transitions from "being
//! assembled" to "visible to queries":
//!
//! 1. End of `build_unified_graph_inner` (full-rebuild path) before the
//!    assembled `CodeGraph` is returned to callers.
//! 2. Step 13 of [`crate::graph::unified::rebuild::RebuildGraph::finalize`]
//!    — bundled into the single-site residue check below.
//! 3. Task 6's `WorkspaceManager::publish_graph` helper (a single wrapper
//!    around every `ArcSwap::store` call) — Task 6's publish helper will
//!    call [`assert_publish_bijection`] verbatim; there is no separate
//!    code path to re-wire later.
//! 4. Every case in the §E equivalence harness
//!    (`sqry-core/tests/incremental_equivalence.rs`).
//!
//! The bijection check is idempotent and cheap on empty sets; calling it
//! at additional sites does not violate the plan.
//!
//! ## [`assert_publish_invariants`] — EXACTLY ONE site
//!
//! Called at **exactly one site**: step 14 of `RebuildGraph::finalize`,
//! on the just-assembled `CodeGraph`, against `self.drained_tombstones`
//! (populated at step 8). Running the residue check at any other site —
//! even against an empty `drained` set — would violate §H step 14's
//! "exactly one assembled-`CodeGraph` residue call site" rule, which
//! Gate 0d iter-1 Major 1 called out explicitly ("Those extra empty-set
//! calls are vacuous, but they still violate the plan's exactly-one
//! rule"). The residue helper fires the bijection check first, then the
//! residue check — both in one shot — at this one authoritative site.
//!
//! Callers that need the bijection ONLY (full rebuild end, §E harness,
//! Task 6 publish helper) MUST use [`assert_publish_bijection`]. The
//! type signatures of the two helpers make this distinction mechanical:
//! [`assert_publish_invariants`] requires a `&HashSet<NodeId>`, so
//! passing `&HashSet::new()` at a non-finalize site is a visible
//! violation at the call site.
//!
//! # Release-build semantics
//!
//! Both assertions are `#[cfg(any(debug_assertions, test))]` at the
//! implementation sites (`concurrent::graph::CodeGraph`). This module's
//! wrappers mirror the cfg so release builds compile both helpers — and
//! every call site — out to a no-op. The §E CI gate certifies drift
//! freedom on debug builds before any release ships.

use std::collections::HashSet;

use super::concurrent::CodeGraph;
use super::node::NodeId;

/// §F.1 bucket-bijection check — callable at every publish boundary.
///
/// Runs **only** the bucket-bijection invariant on `graph` in debug /
/// test builds:
///
/// - Every live node appears in exactly one bucket.
/// - Each bucket's `FileId` matches the node's `NodeEntry.file`.
/// - Every live arena slot is accounted for by some bucket once any
///   bucket is populated.
///
/// This helper is explicitly cheap and idempotent, so it is safe to
/// call at every publish site (full-rebuild end, `RebuildGraph::finalize`
/// step 13, Task 6's `WorkspaceManager::publish_graph`, and every §E
/// harness case).
///
/// # Call sites
///
/// - `build_unified_graph_inner` — full rebuild end.
/// - `RebuildGraph::finalize` — step 13 (bundled inside
///   [`assert_publish_invariants`] along with step 14's residue check).
/// - Task 6's `WorkspaceManager::publish_graph` — every `ArcSwap::store`
///   path.
/// - §E harness `incremental_equivalence.rs` — baseline + every
///   incremental candidate before the sem-graph diff.
///
/// # Panics
///
/// In debug builds, panics with a descriptive message if the bijection
/// fails. In release builds, compiles to a no-op.
#[cfg(any(debug_assertions, test))]
pub fn assert_publish_bijection(graph: &CodeGraph) {
    graph.assert_bucket_bijection();
    // Gate 0d iter-2 defense-in-depth: assert that every unified
    // loser in the arena has its publish-visible content-addressable
    // fields cleared. This is belt-and-braces to the `is_unified_loser`
    // filters at every publish-visible iteration surface (duplicates,
    // list_symbols, hierarchical search, visualization, etc.): if any
    // code path ever leaves a loser with live `signature` /
    // `body_hash` / `qualified_name` / `doc` / `visibility`,
    // this check trips at the publish boundary before a query can
    // observe it.
    //
    // The check only panics when a loser is found with un-cleared
    // metadata; clean graphs and graphs without unification pay a
    // single arena-walk pass.
    assert_losers_have_cleared_metadata(graph);
}

/// Walk the arena and assert every unified loser
/// ([`crate::graph::unified::storage::arena::NodeEntry::is_unified_loser`])
/// has every publish-visible content-addressable field cleared.
///
/// This mirrors the clearing contract in
/// [`crate::graph::unified::build::unification::merge_node_into`]:
/// `name = StringId::INVALID`, `qualified_name = None`, `signature = None`,
/// `body_hash = None`, `doc = None`, `visibility = None`.
///
/// # Panics
///
/// Panics in debug / test builds if any loser has a non-cleared
/// publish-visible field. The panic message names the offending slot
/// index and the first field that failed the invariant.
#[cfg(any(debug_assertions, test))]
fn assert_losers_have_cleared_metadata(graph: &CodeGraph) {
    for (node_id, entry) in graph.nodes().iter() {
        if !entry.is_unified_loser() {
            continue;
        }
        assert!(
            entry.qualified_name.is_none(),
            "publish-boundary invariant: unified loser slot {} has qualified_name set; \
             `merge_node_into` must clear qualified_name",
            node_id.index()
        );
        assert!(
            entry.signature.is_none(),
            "publish-boundary invariant: unified loser slot {} has signature set; \
             `merge_node_into` must clear signature (Gate 0d iter-2 blocker)",
            node_id.index()
        );
        assert!(
            entry.body_hash.is_none(),
            "publish-boundary invariant: unified loser slot {} has body_hash set; \
             `merge_node_into` must clear body_hash (Gate 0d iter-2 blocker)",
            node_id.index()
        );
        assert!(
            entry.doc.is_none(),
            "publish-boundary invariant: unified loser slot {} has doc set; \
             `merge_node_into` must clear doc",
            node_id.index()
        );
        assert!(
            entry.visibility.is_none(),
            "publish-boundary invariant: unified loser slot {} has visibility set; \
             `merge_node_into` must clear visibility",
            node_id.index()
        );
    }
}

/// Release-build stub — compiles the bijection check out so call sites
/// can stay unconditional regardless of build profile.
#[cfg(not(any(debug_assertions, test)))]
#[inline(always)]
pub fn assert_publish_bijection(_graph: &CodeGraph) {}

/// §F.1 bucket-bijection + §F.2 tombstone-residue check — EXACTLY ONE
/// call site (finalize step 14).
///
/// Runs both A2 §F invariants on `graph` in debug / test builds:
///
/// - **§F.1 bucket bijection** via
///   [`CodeGraph::assert_bucket_bijection`].
/// - **§F.2 tombstone residue** via
///   [`CodeGraph::assert_no_tombstone_residue_for`] — no NodeId-bearing
///   structure (K.A1 through K.B1 of the §K master matrix) contains a
///   node present in `drained`.
///
/// # Single call-site contract
///
/// Per plan §H step 14 and §F.3, this helper must be called from
/// **exactly one site**: step 14 of `RebuildGraph::finalize`, on the
/// just-assembled `CodeGraph`, against `self.drained_tombstones`
/// stashed at step 8. **Every other publish boundary** — full-rebuild
/// end, §E harness, Task 6 publish helper — must call
/// [`assert_publish_bijection`] instead. Calling
/// `assert_publish_invariants` at additional sites with an empty
/// `drained` set was Gate 0d iter-1 Major 1.
///
/// # Panics
///
/// In debug builds, panics with a descriptive message if either
/// assertion fails. In release builds, compiles to a no-op.
///
/// # Rationale
///
/// The bijection + residue pair is the publish boundary's "tombstone
/// safety seal", and it has a natural single call site: the rebuild
/// finalize step that drained the tombstone set. A single call site
/// keeps the residue-check workload proportional to rebuilds that
/// actually have drained tombstones and keeps drift-detection
/// unambiguous: either finalize step 14 ran against `drained_tombstones`
/// or no residue check ran at all.
#[cfg(any(debug_assertions, test))]
pub fn assert_publish_invariants(graph: &CodeGraph, drained: &HashSet<NodeId>) {
    graph.assert_bucket_bijection();
    graph.assert_no_tombstone_residue_for(drained);
}

/// Release-build stub — compiles the assertion out so the single entry
/// point's contract ("finalize step 14 only") can stay unconditional at
/// its one call site regardless of build profile.
#[cfg(not(any(debug_assertions, test)))]
#[inline(always)]
pub fn assert_publish_invariants(_graph: &CodeGraph, _drained: &HashSet<NodeId>) {}

#[cfg(test)]
mod tests {
    //! Gate 0d positive + negative publish-invariant tests.
    //!
    //! Negative-path coverage for the bijection primitive itself lives
    //! in
    //! `sqry-core/src/graph/unified/rebuild/rebuild_graph.rs` (four
    //! `#[should_panic]` tests covering duplicate / misfiled / missing /
    //! dead node). Negative tombstone-residue coverage lives alongside
    //! those tests. This module's tests exercise the *wrapper* — i.e.
    //! that routing through `assert_publish_invariants` fires both
    //! underlying assertions and passes on a clean graph.

    use super::*;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::NodeEntry;

    /// Seed a `CodeGraph` with two live nodes distributed over two files,
    /// with per-file buckets correctly populated. Mirrors the pattern in
    /// `rebuild_graph.rs::tests::seeded_graph` but simpler (2 nodes / 2
    /// files is enough for the wrapper smoke tests).
    fn seeded_two_file_graph() -> (CodeGraph, super::NodeId, super::NodeId, FileId, FileId) {
        let mut graph = CodeGraph::new();
        let file_a = FileId::new(1);
        let file_b = FileId::new(2);
        let sym = graph.strings_mut().intern("sym").expect("intern");
        let (node_a, node_b);
        {
            let arena = graph.nodes_mut();
            node_a = arena
                .alloc(NodeEntry::new(NodeKind::Function, sym, file_a))
                .expect("alloc a");
            node_b = arena
                .alloc(NodeEntry::new(NodeKind::Struct, sym, file_b))
                .expect("alloc b");
        }
        graph.files_mut().record_node(file_a, node_a);
        graph.files_mut().record_node(file_b, node_b);
        (graph, node_a, node_b, file_a, file_b)
    }

    #[test]
    fn publish_invariants_pass_on_clean_graph_with_empty_drained() {
        let (graph, _, _, _, _) = seeded_two_file_graph();
        let drained: HashSet<NodeId> = HashSet::new();
        assert_publish_invariants(&graph, &drained);
    }

    #[test]
    fn publish_invariants_pass_on_fresh_empty_graph() {
        let graph = CodeGraph::new();
        let drained: HashSet<NodeId> = HashSet::new();
        // Empty graph with no buckets: bijection is vacuously consistent
        // per the documented `!seen.is_empty()` guard; residue is a
        // no-op on an empty `drained` set.
        assert_publish_invariants(&graph, &drained);
    }

    #[test]
    #[should_panic(expected = "misfiled")]
    fn publish_invariants_route_bijection_panic() {
        // Misfile `node_a` into `file_b`'s bucket (while leaving
        // `node_b` also in `file_b`). This makes `file_b`'s bucket
        // contain a node whose `NodeEntry.file == file_a`, so condition
        // (c) trips with the "misfiled" message. We remove `node_a`
        // from its own bucket so the duplicate-across-buckets arm (b)
        // does not preempt (c).
        let (mut graph, node_a, _node_b, file_a, file_b) = seeded_two_file_graph();
        let _ = graph.files_mut().take_nodes(file_a);
        graph.files_mut().record_node(file_b, node_a);
        let drained: HashSet<NodeId> = HashSet::new();
        assert_publish_invariants(&graph, &drained);
    }

    #[test]
    #[should_panic(expected = "still in NodeArena")]
    fn publish_invariants_route_residue_panic() {
        // Pass `node_a` as a drained tombstone while the per-file
        // bucket still contains it. The wrapper runs the bijection
        // check first (which passes — `node_a` is a legitimate live
        // node in bucket `file_a`), then routes to the residue check.
        // `assert_no_tombstone_residue_for` iterates the K-rows in
        // order starting with `NodeArena` (K.A1), so the panic text is
        // "still in NodeArena" — any earlier in the iteration would
        // mean the §K master matrix was mis-ordered.
        let (graph, node_a, _node_b, _file_a, _file_b) = seeded_two_file_graph();
        let mut drained: HashSet<NodeId> = HashSet::new();
        drained.insert(node_a);
        assert_publish_invariants(&graph, &drained);
    }

    #[test]
    fn publish_bijection_passes_on_clean_graph() {
        let (graph, _, _, _, _) = seeded_two_file_graph();
        assert_publish_bijection(&graph);
    }

    #[test]
    fn publish_bijection_passes_on_fresh_empty_graph() {
        let graph = CodeGraph::new();
        assert_publish_bijection(&graph);
    }

    #[test]
    #[should_panic(expected = "misfiled")]
    fn publish_bijection_catches_misfiled_node() {
        // `assert_publish_bijection` is the non-finalize publish helper;
        // it must still catch bijection violations even though the
        // residue check is not bundled in.
        let (mut graph, node_a, _node_b, file_a, file_b) = seeded_two_file_graph();
        let _ = graph.files_mut().take_nodes(file_a);
        graph.files_mut().record_node(file_b, node_a);
        assert_publish_bijection(&graph);
    }
}
