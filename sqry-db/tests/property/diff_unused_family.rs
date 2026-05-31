//! WS1 differential test family — unused queries (`UnusedQuery`,
//! `IsNodeUnusedQuery`). DAG unit `U_WS1_8_DIFF_UNUSED`, DESIGN §2.3 of
//! `docs/development/graph-fidelity-planner-correctness/02_DESIGN-graph-fidelity-planner-correctness.md`.
//!
//! For every proptest-generated well-formed `CodeGraph` (DAG unit
//! `U_WS1_3_GRAPH_GEN`, DESIGN §2.2) this test runs the production sqry-db
//! planner — `QueryDb::get::<UnusedQuery>` / `QueryDb::get::<IsNodeUnusedQuery>`
//! — and the WS1 baseline oracle (`sqry_db::baseline::unused` /
//! `is_node_unused`, DAG unit `U_WS1_2_BASELINE`) over the **same**
//! `GraphSnapshot`, then asserts the outputs are byte-identical.
//!
//! # Tier coverage
//!
//! Both queries declare `TRACKS_EDGE_REVISION = true` (Tier 2) **and**
//! `TRACKS_METADATA_REVISION = true` (Tier 3) — see
//! `sqry-db/src/queries/unused.rs` lines 197–198 (`UnusedQuery`) and 290–291
//! (`IsNodeUnusedQuery`). Tier 2 is exercised because every generated graph
//! emits non-`Defines` edges, several of which (`Calls`, `References`,
//! `Imports`, `Inherits`, `Implements`, `TypeOf`) are the reachability set
//! the baseline's `reachable_from_entry_points` walks; the production
//! `ReachableFromEntryPointsQuery` (which `UnusedQuery` depends on)
//! consults the same edge slice. Tier 3 is exercised because entry-point
//! detection (`queries::unused::is_entry_point`) consults `NodeEntry::kind`
//! (`Export` / `Test` variants), `NodeEntry::name` (string-interner ids),
//! and the per-snapshot string interner — all of which are metadata-tier
//! state the generator populates per case. Both planner-side and
//! baseline-side functions read the same `NodeEntry` fields, so any
//! divergence in how the planner observes Tier-2/Tier-3 state surfaces
//! as a differential failure here.
//!
//! Cross-snapshot cache invalidation (the *runtime* Tier-2/Tier-3
//! invalidation paths) is the WS1.5 cache-invalidation suite's
//! responsibility (DESIGN §2.5), not this family. This family validates
//! the planner/baseline observation contract on a single snapshot.
//!
//! # Execution
//!
//! ```text
//! # PR-tier (default 1024 cases, scalable):
//! cargo test -p sqry-db --test diff_unused_family
//!
//! # Nightly-tier (10k cases, release profile):
//! PROPTEST_CASES=10000 cargo test -p sqry-db --test diff_unused_family --release
//! ```
//!
//! `PROPTEST_CASES` is the env var DESIGN §2.3 prescribes — PR CI runs the
//! default count, the `proptest-deep` nightly job sets 100000.

// The graph generator + invariant checker live in `tests/property/graph_gen.rs`.
// `baseline_spot_check`-style `#[path]` inclusion keeps this test target a
// single compilation unit so cargo discovers the `#[test]` functions below.
#[path = "graph_gen.rs"]
#[allow(unused_imports)] // The generator module re-exports helpers other diff
// family files (U_WS1_4..U_WS1_9) will pull in; keeping the wildcard import
// quiet here lets each family file stay self-contained.
mod graph_gen;

use std::sync::Arc;

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::query::UnusedScope;
use sqry_db::baseline;
use sqry_db::queries::unused::{IsNodeUnusedKey, IsNodeUnusedQuery, UnusedKey, UnusedQuery};
use sqry_db::{QueryDb, QueryDbConfig};

use graph_gen::{GeneratedGraph, well_formed_graph};

// ---------------------------------------------------------------------------
// Proptest tuning
// ---------------------------------------------------------------------------

/// Reads `PROPTEST_CASES` from the environment, defaulting to 1024 for the
/// PR-tier `cargo test` invocation. Nightly CI sets 10000 (DESIGN §2.3).
fn cases_from_env() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1024)
}

/// Scopes enumerated by `UnusedScope`. Every proptest case sweeps the full
/// set so each per-scope branch in `scope_matches` is exercised on every
/// generated graph.
const ALL_SCOPES: &[UnusedScope] = &[
    UnusedScope::All,
    UnusedScope::Public,
    UnusedScope::Private,
    UnusedScope::Function,
    UnusedScope::Struct,
];

/// `max_results` cap values swept per case. Mixing the unbounded cap with
/// small caps exercises the early-break path in both `UnusedQuery::execute`
/// and `baseline::unused`. Picked so at least one cap is below typical
/// generated graph size (≤ 64 nodes).
const MAX_RESULTS_CAPS: &[usize] = &[3, 16, usize::MAX];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Builds a fresh `QueryDb` over the generated graph's snapshot. Each
/// proptest case gets its own DB so cache state never leaks between cases —
/// the differential contract is single-snapshot correctness.
fn build_db(
    graph: &GeneratedGraph,
) -> (
    QueryDb,
    Arc<sqry_core::graph::unified::concurrent::GraphSnapshot>,
) {
    let snapshot = Arc::new(graph.graph.snapshot());
    let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    (db, snapshot)
}

/// Compares two `Vec<NodeId>` for byte-identity, surfacing the divergence
/// in a `proptest`-friendly message that includes the recipe's node/edge
/// counts (small enough to fit in failure logs without dumping the whole
/// recipe).
fn assert_unused_equal(
    planner: &[NodeId],
    baseline_out: &[NodeId],
    scope: UnusedScope,
    max_results: usize,
    graph: &GeneratedGraph,
) -> Result<(), TestCaseError> {
    prop_assert_eq!(
        planner,
        baseline_out,
        "UnusedQuery diverged from baseline::unused\n  scope = {:?}\n  max_results = {}\n  nodes = {}\n  edges = {}",
        scope,
        max_results,
        graph.recipe.nodes.len(),
        graph.recipe.edges.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// UnusedQuery — set differential
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases_from_env(),
        // Shrinker budget mirrors the WS1 generator self-test (DAG
        // acceptance for U_WS1_3_GRAPH_GEN: ≤ 10 000 iterations).
        max_shrink_iters: 10_000,
        ..ProptestConfig::default()
    })]

    /// `UnusedQuery::execute` and `baseline::unused` must produce
    /// byte-identical `Vec<NodeId>` for every (graph, scope, max_results)
    /// triple.
    ///
    /// Both implementations:
    ///
    /// 1. Walk `snapshot.nodes().iter()` in arena (= index) order.
    /// 2. Skip unified losers (Phase 4c-prime tombstones — `NodeEntry::is_unified_loser`).
    /// 3. Apply the same `scope_matches` predicate.
    /// 4. Apply the same always-entry-point predicate (`Export` /
    ///    `Test` kinds, names == `main` / starting with `test_` /
    ///    ending with `_test`).
    /// 5. Cross-check against `ReachableFromEntryPointsQuery` /
    ///    `baseline::reachable_from_entry_points`.
    /// 6. Break after `max_results` entries.
    /// 7. Sort by `(NodeId::index, NodeId::generation)` and return.
    ///
    /// Any drift between the two surfaces here as a property failure with
    /// a shrunk graph recipe.
    #[test]
    fn unused_planner_equals_baseline(graph in well_formed_graph()) {
        let (db, snapshot) = build_db(&graph);
        for &scope in ALL_SCOPES {
            for &max_results in MAX_RESULTS_CAPS {
                let key = UnusedKey { scope, max_results };
                let planner_arc = db.get::<UnusedQuery>(&key);
                let baseline_out = baseline::unused(&snapshot, &key);
                assert_unused_equal(
                    planner_arc.as_slice(),
                    baseline_out.as_slice(),
                    scope,
                    max_results,
                    &graph,
                )?;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// IsNodeUnusedQuery — per-node differential
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases_from_env(),
        max_shrink_iters: 10_000,
        ..ProptestConfig::default()
    })]

    /// `IsNodeUnusedQuery::execute` and `baseline::is_node_unused` must
    /// agree on every node in the generated graph, swept against every
    /// `UnusedScope`. Tombstoned / out-of-range node ids are not generated
    /// (the recipe materialiser tracks live `NodeId`s in
    /// `GeneratedGraph::node_ids`).
    #[test]
    fn is_node_unused_planner_equals_baseline(graph in well_formed_graph()) {
        let (db, snapshot) = build_db(&graph);
        for &node_id in &graph.node_ids {
            for &scope in ALL_SCOPES {
                let key = IsNodeUnusedKey { node_id, scope };
                let planner_out = db.get::<IsNodeUnusedQuery>(&key);
                let baseline_out = baseline::is_node_unused(&snapshot, &key);
                prop_assert_eq!(
                    planner_out,
                    baseline_out,
                    "IsNodeUnusedQuery diverged from baseline::is_node_unused\n  node_id = {:?}\n  scope = {:?}\n  nodes = {}\n  edges = {}",
                    node_id,
                    scope,
                    graph.recipe.nodes.len(),
                    graph.recipe.edges.len()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cross-consistency: UnusedQuery set ⇔ IsNodeUnusedQuery per-node truth
// ---------------------------------------------------------------------------
//
// The planner exposes two surfaces over the same underlying definition.
// `UnusedQuery` returns the set of unused nodes (under a scope, capped at
// `max_results`); `IsNodeUnusedQuery` is the per-node membership predicate
// for the same scope. They must agree once the cap is removed.
//
// This complements the cross-baseline diff above: even if planner and
// baseline both miscomputed the predicate the same way, this property
// catches an inconsistency between the two planner surfaces themselves —
// a class of bugs the per-query differentials cannot see.

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases_from_env(),
        max_shrink_iters: 10_000,
        ..ProptestConfig::default()
    })]

    /// For every scope, `IsNodeUnusedQuery` over `graph.node_ids` must
    /// equal `UnusedQuery`'s membership set (with `max_results = usize::MAX`
    /// so nothing is truncated).
    #[test]
    fn unused_set_membership_matches_is_node_unused(graph in well_formed_graph()) {
        use std::collections::BTreeSet;
        let (db, _snapshot) = build_db(&graph);
        for &scope in ALL_SCOPES {
            let set_key = UnusedKey { scope, max_results: usize::MAX };
            let unused_set: BTreeSet<NodeId> =
                db.get::<UnusedQuery>(&set_key).iter().copied().collect();
            for &node_id in &graph.node_ids {
                let probe_key = IsNodeUnusedKey { node_id, scope };
                let probe = db.get::<IsNodeUnusedQuery>(&probe_key);
                prop_assert_eq!(
                    probe,
                    unused_set.contains(&node_id),
                    "Planner self-consistency: IsNodeUnusedQuery({:?}, {:?}) = {} but UnusedQuery membership = {}\n  nodes = {}\n  edges = {}",
                    node_id,
                    scope,
                    probe,
                    unused_set.contains(&node_id),
                    graph.recipe.nodes.len(),
                    graph.recipe.edges.len()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Anti-flake pin — deterministic 64-graph sample
// ---------------------------------------------------------------------------
//
// `PROPTEST_CASES` is environment-driven. We pin a small fixed-seed sample
// so the gate cannot be silently disabled by setting `PROPTEST_CASES=0` in
// CI, mirroring the `thousand_sample_graphs_are_well_formed` defence in
// `graph_gen_self_test.rs`. 64 graphs is the smallest sample that, with
// the generator's coverage of `NodeKind::Export` / `NodeKind::Test`,
// reliably produces at least one non-empty entry-point set per run.

/// Deterministically sample `count` graphs using a fixed RNG seed —
/// mirrors `property::graph_gen::sample_graphs` but inlined here so this
/// test target is self-contained (the helper is `pub` from `graph_gen` but
/// dynamically importing through the `#[path]` mod is enough).
fn sampled_graphs(count: usize, seed: u64) -> Vec<GeneratedGraph> {
    let mut seed_bytes = [0u8; 32];
    for (i, chunk) in seed_bytes.chunks_exact_mut(8).enumerate() {
        let folded = seed ^ ((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        chunk.copy_from_slice(&folded.to_le_bytes());
    }
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &seed_bytes);
    let mut runner = TestRunner::new_with_rng(Config::default(), rng);
    let strategy = well_formed_graph();
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let tree = strategy
            .new_tree(&mut runner)
            .expect("strategy should not fail to produce a tree");
        out.push(tree.current());
    }
    out
}

#[test]
fn fixed_seed_64_graphs_planner_matches_baseline() {
    let graphs = sampled_graphs(64, 0xD1FF_0008_0000_0000);
    for graph in &graphs {
        let snapshot = Arc::new(graph.graph.snapshot());
        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        for &scope in ALL_SCOPES {
            let key = UnusedKey {
                scope,
                max_results: usize::MAX,
            };
            let planner_arc = db.get::<UnusedQuery>(&key);
            let baseline_out = baseline::unused(&snapshot, &key);
            assert_eq!(
                planner_arc.as_slice(),
                baseline_out.as_slice(),
                "fixed-seed differential failed: scope={:?}, nodes={}, edges={}",
                scope,
                graph.recipe.nodes.len(),
                graph.recipe.edges.len(),
            );
        }
    }
}
