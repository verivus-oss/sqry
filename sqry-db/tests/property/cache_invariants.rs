//! WS1 cache-invalidation property suite (DAG unit `U_WS1_10_CACHE_INVARIANTS`).
//!
//! One property test per registered `DerivedQuery`. For each query Q with
//! declared tracked-tier set R, the test:
//!
//! 1. Generates a well-formed `CodeGraph` via [`well_formed_graph`].
//! 2. Builds a `QueryDb` and queries Q once (warm path → cache).
//! 3. Applies a proptest-generated [`Edit`] to the graph.
//! 4. Queries Q again on the post-edit snapshot.
//! 5. Asks the independent [`affects_revisions`] oracle whether the edit
//!    touches R.
//! 6. Asserts the two-way invariant:
//!
//!    - **No spurious invalidation**: if the oracle says "no tier touched"
//!      then `result_before == result_after` AND the second `db.get::<Q>` was
//!      a cache hit. A spurious invalidation here indicates a production
//!      `TRACKS_*_REVISION` constant is TOO WIDE (set to `true` when the
//!      edit cannot possibly change the result).
//!
//!    - **No missing invalidation**: if `result_before != result_after`,
//!      then the oracle says "at least one tier touched". A counter-example
//!      indicates a production `TRACKS_*_REVISION` constant is TOO NARROW
//!      (set to `false` despite the edit changing the result).
//!
//! Note: there is no fully-precise oracle for "non-result-changing edits
//! must miss the cache". The cache may legitimately invalidate on edits the
//! query *could* be sensitive to even if the specific input does not change
//! the answer (over-invalidation is allowed by the spec; under-invalidation
//! is not). So the cache-hit assertion runs only on the strict-noop edit
//! channel (`tier_touches == TierSet::NONE`).
//!
//! ## DAG contract
//!
//! - DAG file: `docs/superpowers/plans/2026-05-25-graph-fidelity-planner-correctness-dag.toml`
//! - DAG ref: `02_DESIGN-graph-fidelity-planner-correctness.md` §2.5
//! - 17 queries covered, matching `register_builtin_queries` in
//!   `sqry-db/src/lib.rs`.
//!
//! ## Hard rules carried from the DAG unit
//!
//! - The oracle is independent: the harness's `tier_set_for_<query>` helpers
//!   declare R themselves; they do NOT reference `Q::TRACKS_*_REVISION`.
//!   Codex review reconciles the two.
//! - 17 tests, one per query.
//! - PR-tier sweep: `PROPTEST_CASES=10000` per DAG; default 256 for the
//!   developer loop.

#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::too_many_lines)]

use std::collections::BTreeSet;
use std::sync::Arc;

use proptest::prelude::*;
use proptest::test_runner::Config;

use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::query::{CircularType, UnusedScope};

use sqry_db::DerivedQuery;
use sqry_db::queries::cycles::{CycleBounds, CyclesKey, IsInCycleKey};
use sqry_db::queries::reachability::ReachabilityKey;
use sqry_db::queries::unused::{IsNodeUnusedKey, UnusedKey};
use sqry_db::queries::{
    AddressTakenQuery, CalleesQuery, CallersQuery, CallsitePromiscuousQuery, CondensationQuery,
    CyclesQuery, EntryPointsQuery, ExportsQuery, ImplementsQuery, ImportsQuery, IsInCycleQuery,
    IsNodeUnusedQuery, ReachabilityQuery, ReachableFromEntryPointsQuery, ReferencesQuery,
    RelationKey, SccQuery, UnusedQuery,
};

#[path = "edit_oracle.rs"]
mod edit_oracle;

use edit_oracle::{
    AppliedEdit, Edit, EditContext, GeneratedGraph, TierSet, affects_revisions, apply_edit,
    arbitrary_edit, fresh_db_and_graph, well_formed_graph,
};

// ---------------------------------------------------------------------------
// Test-side independent tier declarations.
//
// CRITICAL: these constants are authored separately from the production
// `TRACKS_*_REVISION` flags. Do NOT replace these with `Q::TRACKS_*_REVISION`
// reads — the whole point of WS1 is to compare the two by black-box behaviour.
// Codex review confirms the table here matches the table in production.
// ---------------------------------------------------------------------------

/// Test-side R for each of the 17 registered queries. Anchored to the
/// per-query module docstrings:
/// - `SccQuery`, `CondensationQuery`, `ReachabilityQuery`: pure edge topology → EDGE.
/// - `CallersQuery`/`CalleesQuery`/`ImportsQuery`/`ExportsQuery`/
///   `ReferencesQuery`/`ImplementsQuery`: name-keyed relation queries over the
///   edge set → EDGE.
/// - `CyclesQuery`/`IsInCycleQuery`: SCC over the edge set → EDGE.
/// - `EntryPointsQuery`/`ReachableFromEntryPointsQuery`/`UnusedQuery`/
///   `IsNodeUnusedQuery`: read NodeKind + visibility (metadata) AND traverse
///   edges (edge) → EDGE | METADATA.
/// - `AddressTakenQuery`/`CallsitePromiscuousQuery`: read flag bits on
///   `NodeMetadataStore`; documented to conservatively also track edges → EDGE | METADATA.
mod test_oracle {
    use super::TierSet;

    pub const SCC: TierSet = TierSet::EDGE;
    pub const CONDENSATION: TierSet = TierSet::EDGE;
    pub const REACHABILITY: TierSet = TierSet::EDGE;
    pub const CALLERS: TierSet = TierSet::EDGE;
    pub const CALLEES: TierSet = TierSet::EDGE;
    pub const IMPORTS: TierSet = TierSet::EDGE;
    pub const EXPORTS: TierSet = TierSet::EDGE;
    pub const REFERENCES: TierSet = TierSet::EDGE;
    pub const IMPLEMENTS: TierSet = TierSet::EDGE;
    pub const CYCLES: TierSet = TierSet::EDGE;
    pub const IS_IN_CYCLE: TierSet = TierSet::EDGE;
    pub const ENTRY_POINTS: TierSet = TierSet::EDGE.union(TierSet::METADATA);
    pub const REACHABLE_FROM_ENTRY: TierSet = TierSet::EDGE.union(TierSet::METADATA);
    pub const UNUSED: TierSet = TierSet::EDGE.union(TierSet::METADATA);
    pub const IS_NODE_UNUSED: TierSet = TierSet::EDGE.union(TierSet::METADATA);
    pub const ADDRESS_TAKEN: TierSet = TierSet::EDGE.union(TierSet::METADATA);
    pub const CALLSITE_PROMISCUOUS: TierSet = TierSet::EDGE.union(TierSet::METADATA);
}

// ---------------------------------------------------------------------------
// Test config
// ---------------------------------------------------------------------------

fn cases_from_env() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(256)
}

fn proptest_config() -> Config {
    Config {
        cases: cases_from_env(),
        max_shrink_iters: 10_000,
        ..Config::default()
    }
}

// ---------------------------------------------------------------------------
// Shared assertion routine
//
// `before` / `after` are query outputs, compared via `PartialEq`. `tracked`
// is the test-side R. The invariants:
//
//   (a) result_before != result_after  ⇒  oracle says "tier touched"
//   (b) tier_touches == NONE           ⇒  result unchanged AND cache hit
//
// (b)'s "cache hit" check measures the metrics delta around the post-edit
// `db.get::<Q>(...)` call.
// ---------------------------------------------------------------------------

fn assert_cache_invariants<V: PartialEq + std::fmt::Debug>(
    query_name: &'static str,
    before: &V,
    after: &V,
    applied: &AppliedEdit,
    tracked: TierSet,
    cache_miss_delta: u64,
) -> Result<(), TestCaseError> {
    let oracle_says_invalidate = affects_revisions(applied, tracked);
    let result_changed = before != after;

    // (a) Missing invalidation: result changed but the oracle says no.
    if result_changed {
        prop_assert!(
            oracle_says_invalidate,
            "[{query_name}] MISSING INVALIDATION: result changed but oracle says no \
             tier touched. \n  edit={edit:?}\n  tier_touches={tt:?}\n  tracked={tr:?}\n  \
             before={before:?}\n  after={after:?}",
            query_name = query_name,
            edit = applied.edit,
            tt = applied.tier_touches,
            tr = tracked,
            before = before,
            after = after,
        );
    }

    // (b) Spurious invalidation: edit touched nothing, but the second query
    //     missed the cache. Only enforced for the strict-noop channel because
    //     for any tier-touching edit production is allowed to over-invalidate
    //     conservatively (and we don't want to encode "predict the exact
    //     production over-invalidation" into the oracle).
    if applied.tier_touches == TierSet::NONE {
        prop_assert!(
            !result_changed,
            "[{query_name}] result drifted on a strict-noop edit — \
             generator non-determinism? \n  edit={edit:?}\n  before={before:?}\n  after={after:?}",
            query_name = query_name,
            edit = applied.edit,
            before = before,
            after = after,
        );
        prop_assert_eq!(
            cache_miss_delta,
            0,
            "[{query_name}] SPURIOUS INVALIDATION: cache missed on a strict-noop \
             edit (no tier touched). edit={edit:?}",
            query_name = query_name,
            edit = applied.edit.clone(),
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Test runners — one per query
//
// Pattern shared by all 17:
//   1. Build (CodeGraph clone, QueryDb) from the generated graph.
//   2. Warm-path query: `db.get::<Q>(&key)`.
//   3. Apply edit; this also installs the new snapshot and bumps tiers.
//   4. Measure cache_miss delta around the post-edit `db.get::<Q>(&key)`.
//   5. Call `assert_cache_invariants(...)`.
// ---------------------------------------------------------------------------

fn pick_target(generated: &GeneratedGraph, idx: usize) -> Option<NodeId> {
    if generated.node_ids.is_empty() {
        None
    } else {
        Some(generated.node_ids[idx % generated.node_ids.len()])
    }
}

fn name_of(generated: &GeneratedGraph, node: NodeId) -> Option<String> {
    let snap = generated.graph.snapshot();
    let entry = snap.nodes().get(node)?;
    snap.strings()
        .resolve(entry.name)
        .map(|s| s.as_ref().to_string())
}

fn run_query_and_count_misses<Q: DerivedQuery>(
    db: &sqry_db::QueryDb,
    key: &Q::Key,
) -> (Q::Value, u64) {
    let before = db.metrics().cache_misses;
    let val = db.get::<Q>(key);
    let after = db.metrics().cache_misses;
    (val, after - before)
}

// ----- 1. SccQuery -----
fn run_scc_test(generated: GeneratedGraph, edit: Edit) -> Result<(), TestCaseError> {
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let key = EdgeKind::Calls {
        argument_count: 0,
        is_async: false,
        resolved_via: ResolvedVia::Direct,
    };
    let before = db.get::<SccQuery>(&key);
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<SccQuery>(&db, &key);
    let normalized_before: BTreeSet<BTreeSet<NodeId>> = before
        .components
        .iter()
        .map(|c| c.iter().copied().collect())
        .collect();
    let normalized_after: BTreeSet<BTreeSet<NodeId>> = after
        .components
        .iter()
        .map(|c| c.iter().copied().collect())
        .collect();
    assert_cache_invariants(
        "SccQuery",
        &normalized_before,
        &normalized_after,
        &applied,
        test_oracle::SCC,
        miss_delta,
    )
}

// ----- 2. CondensationQuery -----
fn run_condensation_test(generated: GeneratedGraph, edit: Edit) -> Result<(), TestCaseError> {
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let key = EdgeKind::Calls {
        argument_count: 0,
        is_async: false,
        resolved_via: ResolvedVia::Direct,
    };
    let before = db.get::<CondensationQuery>(&key);
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<CondensationQuery>(&db, &key);
    // Condensation produces an `Arc<CachedCondensation>` — compare via
    // a normalised edge set flattened from the `dag_edges` adjacency map.
    let flatten = |c: &sqry_db::queries::condensation::CachedCondensation| -> BTreeSet<(u32, u32)> {
        let mut out = BTreeSet::new();
        for (src, tgts) in &c.dag_edges {
            for tgt in tgts {
                out.insert((*src, *tgt));
            }
        }
        out
    };
    let edges_before = flatten(&before);
    let edges_after = flatten(&after);
    assert_cache_invariants(
        "CondensationQuery",
        &edges_before,
        &edges_after,
        &applied,
        test_oracle::CONDENSATION,
        miss_delta,
    )
}

// ----- 3. ReachabilityQuery -----
fn run_reachability_test(
    generated: GeneratedGraph,
    edit: Edit,
    target_idx: usize,
) -> Result<(), TestCaseError> {
    let Some(root) = pick_target(&generated, target_idx) else {
        return Ok(());
    };
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let key = ReachabilityKey {
        roots: vec![root],
        edge_kind: EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
    };
    let before = db.get::<ReachabilityQuery>(&key);
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<ReachabilityQuery>(&db, &key);
    let before_set: BTreeSet<NodeId> = before.reachable.iter().copied().collect();
    let after_set: BTreeSet<NodeId> = after.reachable.iter().copied().collect();
    assert_cache_invariants(
        "ReachabilityQuery",
        &before_set,
        &after_set,
        &applied,
        test_oracle::REACHABILITY,
        miss_delta,
    )
}

// ----- 4-9. Relation queries -----
fn run_relation_test<Q: DerivedQuery<Key = RelationKey, Value = Arc<Vec<NodeId>>>>(
    name: &'static str,
    tracked: TierSet,
    generated: GeneratedGraph,
    edit: Edit,
    target_idx: usize,
) -> Result<(), TestCaseError> {
    let Some(target) = pick_target(&generated, target_idx) else {
        return Ok(());
    };
    let Some(name_str) = name_of(&generated, target) else {
        return Ok(());
    };
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let key = RelationKey::exact(name_str);
    let before = db.get::<Q>(&key);
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<Q>(&db, &key);
    let before_set: BTreeSet<NodeId> = before.iter().copied().collect();
    let after_set: BTreeSet<NodeId> = after.iter().copied().collect();
    assert_cache_invariants(name, &before_set, &after_set, &applied, tracked, miss_delta)
}

// ----- 10. CyclesQuery -----
fn run_cycles_test(generated: GeneratedGraph, edit: Edit) -> Result<(), TestCaseError> {
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let key = CyclesKey {
        circular_type: CircularType::Calls,
        bounds: CycleBounds::default(),
    };
    let before = db.get::<CyclesQuery>(&key);
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<CyclesQuery>(&db, &key);
    let before_set: BTreeSet<BTreeSet<NodeId>> =
        before.iter().map(|c| c.iter().copied().collect()).collect();
    let after_set: BTreeSet<BTreeSet<NodeId>> =
        after.iter().map(|c| c.iter().copied().collect()).collect();
    assert_cache_invariants(
        "CyclesQuery",
        &before_set,
        &after_set,
        &applied,
        test_oracle::CYCLES,
        miss_delta,
    )
}

// ----- 11. IsInCycleQuery -----
fn run_is_in_cycle_test(
    generated: GeneratedGraph,
    edit: Edit,
    target_idx: usize,
) -> Result<(), TestCaseError> {
    let Some(node) = pick_target(&generated, target_idx) else {
        return Ok(());
    };
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let key = IsInCycleKey {
        node_id: node,
        circular_type: CircularType::Calls,
        bounds: CycleBounds::default(),
    };
    let before = db.get::<IsInCycleQuery>(&key);
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<IsInCycleQuery>(&db, &key);
    assert_cache_invariants(
        "IsInCycleQuery",
        &before,
        &after,
        &applied,
        test_oracle::IS_IN_CYCLE,
        miss_delta,
    )
}

// ----- 12. EntryPointsQuery -----
fn run_entry_points_test(generated: GeneratedGraph, edit: Edit) -> Result<(), TestCaseError> {
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let before = db.get::<EntryPointsQuery>(&());
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<EntryPointsQuery>(&db, &());
    let before_set: BTreeSet<NodeId> = before.iter().copied().collect();
    let after_set: BTreeSet<NodeId> = after.iter().copied().collect();
    assert_cache_invariants(
        "EntryPointsQuery",
        &before_set,
        &after_set,
        &applied,
        test_oracle::ENTRY_POINTS,
        miss_delta,
    )
}

// ----- 13. ReachableFromEntryPointsQuery -----
fn run_reachable_from_ep_test(generated: GeneratedGraph, edit: Edit) -> Result<(), TestCaseError> {
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let before = db.get::<ReachableFromEntryPointsQuery>(&());
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<ReachableFromEntryPointsQuery>(&db, &());
    let before_set: BTreeSet<NodeId> = before.iter().copied().collect();
    let after_set: BTreeSet<NodeId> = after.iter().copied().collect();
    assert_cache_invariants(
        "ReachableFromEntryPointsQuery",
        &before_set,
        &after_set,
        &applied,
        test_oracle::REACHABLE_FROM_ENTRY,
        miss_delta,
    )
}

// ----- 14. UnusedQuery -----
fn run_unused_test(generated: GeneratedGraph, edit: Edit) -> Result<(), TestCaseError> {
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let key = UnusedKey {
        scope: UnusedScope::All,
        max_results: 1_000,
    };
    let before = db.get::<UnusedQuery>(&key);
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<UnusedQuery>(&db, &key);
    let before_vec: Vec<NodeId> = before.as_ref().clone();
    let after_vec: Vec<NodeId> = after.as_ref().clone();
    assert_cache_invariants(
        "UnusedQuery",
        &before_vec,
        &after_vec,
        &applied,
        test_oracle::UNUSED,
        miss_delta,
    )
}

// ----- 15. IsNodeUnusedQuery -----
fn run_is_node_unused_test(
    generated: GeneratedGraph,
    edit: Edit,
    target_idx: usize,
) -> Result<(), TestCaseError> {
    let Some(node) = pick_target(&generated, target_idx) else {
        return Ok(());
    };
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let key = IsNodeUnusedKey {
        node_id: node,
        scope: UnusedScope::All,
    };
    let before = db.get::<IsNodeUnusedQuery>(&key);
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<IsNodeUnusedQuery>(&db, &key);
    assert_cache_invariants(
        "IsNodeUnusedQuery",
        &before,
        &after,
        &applied,
        test_oracle::IS_NODE_UNUSED,
        miss_delta,
    )
}

// ----- 16. AddressTakenQuery -----
fn run_address_taken_test(generated: GeneratedGraph, edit: Edit) -> Result<(), TestCaseError> {
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let before = db.get::<AddressTakenQuery>(&());
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<AddressTakenQuery>(&db, &());
    let before_vec: Vec<NodeId> = before.as_ref().clone();
    let after_vec: Vec<NodeId> = after.as_ref().clone();
    assert_cache_invariants(
        "AddressTakenQuery",
        &before_vec,
        &after_vec,
        &applied,
        test_oracle::ADDRESS_TAKEN,
        miss_delta,
    )
}

// ----- 17. CallsitePromiscuousQuery -----
fn run_callsite_promiscuous_test(
    generated: GeneratedGraph,
    edit: Edit,
) -> Result<(), TestCaseError> {
    let (mut graph, mut db) = fresh_db_and_graph(&generated);
    let ctx = EditContext::from_generated(&generated);
    let before = db.get::<CallsitePromiscuousQuery>(&());
    let applied = apply_edit(&mut graph, &mut db, edit, &ctx);
    let (after, miss_delta) = run_query_and_count_misses::<CallsitePromiscuousQuery>(&db, &());
    let before_vec: Vec<NodeId> = before.as_ref().clone();
    let after_vec: Vec<NodeId> = after.as_ref().clone();
    assert_cache_invariants(
        "CallsitePromiscuousQuery",
        &before_vec,
        &after_vec,
        &applied,
        test_oracle::CALLSITE_PROMISCUOUS,
        miss_delta,
    )
}

// ---------------------------------------------------------------------------
// proptest! bodies — 17 tests, one per registered query
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest_config())]

    // 1. SccQuery
    #[test]
    fn scc_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
    ) {
        run_scc_test(generated, edit)?;
    }

    // 2. CondensationQuery
    #[test]
    fn condensation_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
    ) {
        run_condensation_test(generated, edit)?;
    }

    // 3. ReachabilityQuery
    #[test]
    fn reachability_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
        target_idx in 0usize..256,
    ) {
        run_reachability_test(generated, edit, target_idx)?;
    }

    // 4. CallersQuery
    #[test]
    fn callers_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
        target_idx in 0usize..256,
    ) {
        run_relation_test::<CallersQuery>(
            "CallersQuery", test_oracle::CALLERS, generated, edit, target_idx,
        )?;
    }

    // 5. CalleesQuery
    #[test]
    fn callees_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
        target_idx in 0usize..256,
    ) {
        run_relation_test::<CalleesQuery>(
            "CalleesQuery", test_oracle::CALLEES, generated, edit, target_idx,
        )?;
    }

    // 6. ImportsQuery
    #[test]
    fn imports_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
        target_idx in 0usize..256,
    ) {
        run_relation_test::<ImportsQuery>(
            "ImportsQuery", test_oracle::IMPORTS, generated, edit, target_idx,
        )?;
    }

    // 7. ExportsQuery
    #[test]
    fn exports_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
        target_idx in 0usize..256,
    ) {
        run_relation_test::<ExportsQuery>(
            "ExportsQuery", test_oracle::EXPORTS, generated, edit, target_idx,
        )?;
    }

    // 8. ReferencesQuery
    #[test]
    fn references_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
        target_idx in 0usize..256,
    ) {
        run_relation_test::<ReferencesQuery>(
            "ReferencesQuery", test_oracle::REFERENCES, generated, edit, target_idx,
        )?;
    }

    // 9. ImplementsQuery
    #[test]
    fn implements_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
        target_idx in 0usize..256,
    ) {
        run_relation_test::<ImplementsQuery>(
            "ImplementsQuery", test_oracle::IMPLEMENTS, generated, edit, target_idx,
        )?;
    }

    // 10. CyclesQuery
    #[test]
    fn cycles_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
    ) {
        run_cycles_test(generated, edit)?;
    }

    // 11. IsInCycleQuery
    #[test]
    fn is_in_cycle_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
        target_idx in 0usize..256,
    ) {
        run_is_in_cycle_test(generated, edit, target_idx)?;
    }

    // 12. EntryPointsQuery
    #[test]
    fn entry_points_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
    ) {
        run_entry_points_test(generated, edit)?;
    }

    // 13. ReachableFromEntryPointsQuery
    #[test]
    fn reachable_from_entry_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
    ) {
        run_reachable_from_ep_test(generated, edit)?;
    }

    // 14. UnusedQuery
    #[test]
    fn unused_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
    ) {
        run_unused_test(generated, edit)?;
    }

    // 15. IsNodeUnusedQuery
    #[test]
    fn is_node_unused_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
        target_idx in 0usize..256,
    ) {
        run_is_node_unused_test(generated, edit, target_idx)?;
    }

    // 16. AddressTakenQuery
    #[test]
    fn address_taken_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
    ) {
        run_address_taken_test(generated, edit)?;
    }

    // 17. CallsitePromiscuousQuery
    #[test]
    fn callsite_promiscuous_cache_minimal_sufficient(
        generated in well_formed_graph(),
        edit in arbitrary_edit(),
    ) {
        run_callsite_promiscuous_test(generated, edit)?;
    }
}
