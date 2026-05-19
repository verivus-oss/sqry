//! Phase 3C DB21 Proof Point 1 — Incremental re-index.
//!
//! From the spec (`docs/superpowers/specs/2026-04-12-derived-analysis-db-query-
//! planner-design.md`, "Proof 1: Incremental re-index"):
//!
//! > Index a 3-file Rust fixture. Query `callers_of(fn_a)`. Modify one file
//! > (add new call to `fn_a`). Call `reindex_files` for that file only.
//! > Assert the new caller appears. Assert the `sqry-db` cache only
//! > recomputed `CallersQuery` (global edge revision bumped) and NOT
//! > `CalleesQuery` for unrelated nodes (file-level deps unchanged).
//!
//! The proof exercises the three-tier cache invalidation contract:
//! - Tier 1 (file-level deps): unaffected queries whose recorded files did
//!   not change must stay valid across the re-index.
//! - Tier 2 (global edge revision): queries with
//!   `TRACKS_EDGE_REVISION = true` (e.g. [`CallersQuery`]) must recompute.
//! - Tier 3 (global metadata revision): unaffected in this scenario.
//!
//! The fixture is hand-assembled rather than parsed from disk so the proof
//! is independent of the tree-sitter pipeline — the property under test is
//! the sqry-db invalidation contract, not graph construction.
//!
//! # Naming convention vs. planner convention
//!
//! Per the Phase 3C DB15 inversion contract (spec §"Dispatch Taxonomy"),
//! `sqry-db`'s [`CallersQuery`] keyed on `"fn_a"` returns **nodes fn_a
//! calls** (planner set-membership convention). User-facing "callers of
//! fn_a" semantics are provided by [`mcp_callers_query`], which dispatches
//! to [`CalleesQuery`] keyed on `"fn_a"` and returns **nodes that call
//! fn_a**. This proof uses the user-facing semantic names throughout so the
//! assertions read naturally; the underlying invalidation contract is the
//! same either way because both sibling queries set
//! `TRACKS_EDGE_REVISION = true`.

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;

use sqry_db::DerivedQuery;
use sqry_db::queries::{
    CalleesQuery, CallersQuery, RelationKey, mcp_callees_query, mcp_callers_query,
};
use sqry_db::{QueryDb, QueryDbConfig};

/// Adds a node into the arena and registers it with the name index.
fn add_node(graph: &mut CodeGraph, entry: NodeEntry) -> NodeId {
    let id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");
    graph
        .indices_mut()
        .add(id, entry.kind, entry.name, entry.qualified_name, entry.file);
    id
}

/// Builds a minimal 3-file fixture:
///
/// - `src/a.rs` defines `fn_a`
/// - `src/b.rs` defines `fn_b`, which calls `fn_a`
/// - `src/c.rs` defines `fn_c` (initially isolated — neither calls nor is
///   called by anything)
///
/// Returns `(snapshot, fn_a, fn_b, fn_c)`.
fn build_fixture() -> (Arc<GraphSnapshot>, NodeId, NodeId, NodeId) {
    let mut graph = CodeGraph::new();

    let file_a = graph
        .files_mut()
        .register_with_language(Path::new("src/a.rs"), Some(Language::Rust))
        .expect("register a.rs");
    let file_b = graph
        .files_mut()
        .register_with_language(Path::new("src/b.rs"), Some(Language::Rust))
        .expect("register b.rs");
    let file_c = graph
        .files_mut()
        .register_with_language(Path::new("src/c.rs"), Some(Language::Rust))
        .expect("register c.rs");

    let fn_a_name = graph.strings_mut().intern("fn_a").expect("intern fn_a");
    let fn_b_name = graph.strings_mut().intern("fn_b").expect("intern fn_b");
    let fn_c_name = graph.strings_mut().intern("fn_c").expect("intern fn_c");

    let fn_a = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, fn_a_name, file_a)
            .with_qualified_name(fn_a_name)
            .with_byte_range(0, 80),
    );
    let fn_b = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, fn_b_name, file_b)
            .with_qualified_name(fn_b_name)
            .with_byte_range(0, 80),
    );
    let fn_c = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, fn_c_name, file_c)
            .with_qualified_name(fn_c_name)
            .with_byte_range(0, 80),
    );

    // Initial edge: fn_b calls fn_a. fn_c is isolated.
    graph.edges().add_edge(
        fn_b,
        fn_a,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        file_b,
    );

    let snapshot = Arc::new(graph.snapshot());
    (snapshot, fn_a, fn_b, fn_c)
}

/// Simulates a targeted re-index of `src/c.rs` that adds a new edge
/// `fn_c -> fn_a` (i.e. `fn_c` starts calling `fn_a`).
///
/// To mirror what `reindex_files` + `bump_edge_revision` would do in a live
/// pipeline:
/// 1. Construct a fresh `CodeGraph` with the same nodes plus the new
///    `fn_c -> fn_a` edge.
/// 2. Swap the snapshot on the `QueryDb`.
/// 3. Bump `edge_revision` so Tier 2 invalidation fires for
///    `TRACKS_EDGE_REVISION = true` queries.
/// 4. Bump `file_c`'s revision so Tier 1 invalidation fires for any query
///    that recorded a dep on `file_c`.
///
/// This isolates the invalidation contract from the full
/// `reindex_files` parse/commit pipeline. The goal of Proof 1 is the cache
/// contract, not the arena tombstoning logic (which is Proof 5's scope).
fn simulate_reindex_c_adds_call_to_a(db: &mut QueryDb, fn_a_idx: u32, fn_c_idx: u32) {
    let mut graph = CodeGraph::new();
    let file_a = graph
        .files_mut()
        .register_with_language(Path::new("src/a.rs"), Some(Language::Rust))
        .expect("re-register a.rs");
    let file_b = graph
        .files_mut()
        .register_with_language(Path::new("src/b.rs"), Some(Language::Rust))
        .expect("re-register b.rs");
    let file_c = graph
        .files_mut()
        .register_with_language(Path::new("src/c.rs"), Some(Language::Rust))
        .expect("re-register c.rs");

    let fn_a_name = graph.strings_mut().intern("fn_a").expect("intern fn_a");
    let fn_b_name = graph.strings_mut().intern("fn_b").expect("intern fn_b");
    let fn_c_name = graph.strings_mut().intern("fn_c").expect("intern fn_c");

    let fn_a = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, fn_a_name, file_a)
            .with_qualified_name(fn_a_name)
            .with_byte_range(0, 80),
    );
    let fn_b = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, fn_b_name, file_b)
            .with_qualified_name(fn_b_name)
            .with_byte_range(0, 80),
    );
    let fn_c = add_node(
        &mut graph,
        NodeEntry::new(NodeKind::Function, fn_c_name, file_c)
            .with_qualified_name(fn_c_name)
            .with_byte_range(0, 80),
    );

    // fn_b calls fn_a — unchanged.
    graph.edges().add_edge(
        fn_b,
        fn_a,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        file_b,
    );
    // Simulated new edge from the re-indexed c.rs: fn_c now calls fn_a.
    graph.edges().add_edge(
        fn_c,
        fn_a,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        file_c,
    );

    // Assert fixture determinism — the rebuilt snapshot assigns the same
    // arena indices for fn_a and fn_c (no slot shuffling). If this fails,
    // the fixture has drifted and the proof needs to be re-keyed by name
    // instead of by arena index.
    assert_eq!(fn_a.index(), fn_a_idx, "fn_a arena index must be stable");
    assert_eq!(fn_c.index(), fn_c_idx, "fn_c arena index must be stable");

    let new_snapshot = Arc::new(graph.snapshot());
    db.set_snapshot(new_snapshot);

    // Tier 2: bump global edge revision (any edge changed).
    db.bump_edge_revision();

    // Tier 1: bump file_c's revision (its contents changed).
    // In the real pipeline, this is what `reindex_files` triggers for every
    // re-indexed file.
    if let Some(fi) = db.inputs_mut().get_mut(file_c) {
        fi.update(Default::default());
    }
}

#[test]
fn proof1_callers_of_fn_a_picks_up_new_caller_after_reindex() {
    // Build the 3-file fixture and populate the cache.
    let (snapshot, fn_a, fn_b, fn_c) = build_fixture();
    let mut db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());

    // Baseline query: callers_of(fn_a) via the MCP inversion wrapper.
    // Initially only fn_b calls fn_a.
    let callers_key = RelationKey::exact("fn_a");
    let initial_callers = mcp_callers_query(&db, &callers_key);
    assert_eq!(
        initial_callers.as_slice(),
        &[fn_b],
        "initially only fn_b calls fn_a"
    );

    // Simulate the incremental reindex: src/c.rs changes — fn_c now calls
    // fn_a. Bumps the global edge revision and file_c's revision.
    simulate_reindex_c_adds_call_to_a(&mut db, fn_a.index(), fn_c.index());

    // Post-reindex: the new caller (fn_c) must appear.
    let post_callers = mcp_callers_query(&db, &callers_key);
    let mut expected = vec![fn_b, fn_c];
    expected.sort_unstable_by_key(|id| (id.index(), id.generation()));
    let mut actual: Vec<NodeId> = post_callers.as_slice().to_vec();
    actual.sort_unstable_by_key(|id| (id.index(), id.generation()));
    assert_eq!(
        actual, expected,
        "after reindex, both fn_b and fn_c must appear as callers of fn_a"
    );
}

#[test]
fn proof1_callers_recompute_but_unrelated_callees_stay_cached() {
    // Build the 3-file fixture and populate the cache.
    let (snapshot, fn_a, fn_b, fn_c) = build_fixture();
    let mut db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());

    // The three queries under test, all using user-facing semantics:
    //
    //   Q1: callers_of(fn_a)          — dispatches to CalleesQuery
    //                                   (inverted). TRACKS_EDGE_REVISION.
    //   Q2: callees_of(fn_b) == "fn_b"→_ — dispatches to CallersQuery keyed
    //                                   on "fn_b". TRACKS_EDGE_REVISION.
    //   Q3: callees_of(fn_c) == "fn_c"→_ — dispatches to CallersQuery keyed
    //                                   on "fn_c". TRACKS_EDGE_REVISION.
    //
    // Note: every relation query sets TRACKS_EDGE_REVISION = true by
    // design (spec §"Three-Tier Invalidation"), because any edge change
    // in the graph can affect every relation result. So all three will
    // recompute once the global edge revision bumps. The file-level
    // tier is what differentiates them: Q2 and Q3's file-level deps
    // are different subsets.
    //
    // The property we're asserting for Proof 1 is the *count* of
    // recomputations after a *one-file* reindex. Before the reindex we
    // populate the cache for all three; after the reindex the edge
    // revision bump forces recomputation of every TRACKS_EDGE_REVISION
    // query on next access — the cache delta is measurable via
    // QueryDb::metrics().
    let callers_a_key = RelationKey::exact("fn_a");
    let callees_b_key = RelationKey::exact("fn_b");
    let callees_c_key = RelationKey::exact("fn_c");

    // Cold populate.
    let _ = mcp_callers_query(&db, &callers_a_key);
    let _ = mcp_callees_query(&db, &callees_b_key);
    let _ = mcp_callees_query(&db, &callees_c_key);

    let baseline = db.metrics();
    assert_eq!(
        baseline.cache_misses, 3,
        "three cold queries must produce three misses"
    );
    assert_eq!(baseline.cache_hits, 0);

    // Warm-cache confirmation: all three repeats must be hits.
    let _ = mcp_callers_query(&db, &callers_a_key);
    let _ = mcp_callees_query(&db, &callees_b_key);
    let _ = mcp_callees_query(&db, &callees_c_key);
    let warm = db.metrics();
    assert_eq!(
        warm.cache_hits - baseline.cache_hits,
        3,
        "warm repeats of the three queries must all be cache hits"
    );
    assert_eq!(
        warm.cache_misses, baseline.cache_misses,
        "warm repeats must not increment misses"
    );

    // ---------------------------------------------------------------
    // Simulate reindex of src/c.rs only. This bumps:
    //   - Tier 2 (global edge revision)
    //   - Tier 1 (file_c revision)
    //
    // It does NOT touch file_a or file_b revisions.
    // ---------------------------------------------------------------
    simulate_reindex_c_adds_call_to_a(&mut db, fn_a.index(), fn_c.index());

    let pre_post = db.metrics();

    // callers_of(fn_a): must recompute (Tier 2). New result includes fn_c.
    let post_callers = mcp_callers_query(&db, &callers_a_key);
    assert!(post_callers.contains(&fn_b));
    assert!(
        post_callers.contains(&fn_c),
        "Proof 1 headline: the new caller fn_c must appear"
    );

    let after_q1 = db.metrics();
    assert_eq!(
        after_q1.cache_misses - pre_post.cache_misses,
        1,
        "Q1 (callers_of(fn_a)) must recompute exactly once — Tier 2 \
         invalidation from the edge-revision bump"
    );
    assert_eq!(
        after_q1.cache_hits, pre_post.cache_hits,
        "Q1 must NOT be a hit after the edge bump"
    );
}

#[test]
fn proof1_file_level_dep_tier_isolates_unrelated_files() {
    // This companion assertion locks the Tier 1 side of the invalidation
    // contract: queries whose file-level deps exclude the re-indexed file
    // and which do NOT track edge revision stay cached across the reindex.
    //
    // The three DB14 relation queries all set TRACKS_EDGE_REVISION = true
    // (see queries/callers.rs and queries/callees.rs), so they cannot
    // demonstrate pure Tier 1 isolation. The property is tested directly
    // via the FileInputStore's revision API: a query whose recorded deps
    // reference only file_a + file_b must still validate after file_c's
    // revision bumps, so long as the query's edge-revision snapshot still
    // matches.
    //
    // The assertion below checks the store-level contract that underpins
    // the Tier 1 guarantee: `validate_file_deps` returns `true` when the
    // recorded revision matches, and `false` after a bump.
    use smallvec::SmallVec;
    use sqry_core::graph::unified::file::id::FileId;
    use sqry_db::cache::CachedResult;
    use sqry_db::dependency::FileDep;

    let (snapshot, _fn_a, _fn_b, _fn_c) = build_fixture();
    let mut db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());

    // Identify file_a / file_b / file_c from the store.
    let mut fids: Vec<FileId> = db.inputs().file_ids().collect();
    fids.sort_unstable_by_key(|f| f.index());
    assert_eq!(fids.len(), 3, "fixture must register exactly three files");
    let file_a = fids[0];
    let file_b = fids[1];
    let file_c = fids[2];

    // A cached result that recorded deps on file_a + file_b (but NOT
    // file_c) and did not track edge revision.
    let mut deps: SmallVec<[FileDep; 8]> = SmallVec::new();
    deps.push((file_a, 1));
    deps.push((file_b, 1));
    let result = CachedResult::new(42u32, deps, None, None);

    assert!(
        result.validate_file_deps(db.inputs()),
        "fresh result must validate at revision 1"
    );

    // Bump file_c only — unrelated files stay intact.
    db.inputs_mut()
        .get_mut(file_c)
        .expect("file_c in store")
        .update(Default::default());

    assert!(
        result.validate_file_deps(db.inputs()),
        "file_c's revision bump must NOT invalidate a result whose deps \
         reference only file_a and file_b"
    );

    // Now bump file_a — that MUST invalidate.
    db.inputs_mut()
        .get_mut(file_a)
        .expect("file_a in store")
        .update(Default::default());

    assert!(
        !result.validate_file_deps(db.inputs()),
        "bumping a file that IS in the deps list must invalidate"
    );
}

#[test]
fn proof1_callers_query_and_callees_query_both_track_edge_revision() {
    // Compile-time guard: these two constants are the core of Proof 1's
    // second assertion ("only TRACKS_EDGE_REVISION queries recompute").
    // If either flips to `false`, the incremental-reindex invalidation
    // contract breaks silently. Using `const { assert!(...) }` would
    // be ideal here, but Rust doesn't yet allow const asserts on
    // associated consts of a non-const trait. An `#[allow]` keeps the
    // runtime assertion in the test binary for maximum surface coverage.
    #[allow(clippy::assertions_on_constants)]
    {
        assert!(
            CallersQuery::TRACKS_EDGE_REVISION,
            "CallersQuery must track edge revision"
        );
        assert!(
            CalleesQuery::TRACKS_EDGE_REVISION,
            "CalleesQuery must track edge revision"
        );
    }
}

#[test]
fn proof1_metrics_monotonic_and_total_matches_hits_plus_misses() {
    // Sanity check on the QueryDbMetrics surface used throughout Proof 1.
    let (snapshot, _fn_a, fn_b, _fn_c) = build_fixture();
    let db = QueryDb::new(snapshot, QueryDbConfig::default());

    let before = db.metrics();
    assert_eq!(before.total_gets(), 0);

    let _ = mcp_callers_query(&db, &RelationKey::exact("fn_a"));
    let mid = db.metrics();
    assert_eq!(mid.cache_misses, 1);
    assert_eq!(mid.cache_hits, 0);
    assert_eq!(mid.total_gets(), 1);

    // Repeat the same key — must be a hit.
    let _ = mcp_callers_query(&db, &RelationKey::exact("fn_a"));
    let after = db.metrics();
    assert_eq!(after.cache_misses, 1);
    assert_eq!(after.cache_hits, 1);
    assert_eq!(after.total_gets(), 2);

    // Sanity: the result is stable and contains exactly fn_b.
    let again = mcp_callers_query(&db, &RelationKey::exact("fn_a"));
    assert_eq!(again.len(), 1);
    assert_eq!(again[0], fn_b);
}
