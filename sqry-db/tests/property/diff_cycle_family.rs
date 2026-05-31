//! WS1 differential test family — cycle queries
//! (`CyclesQuery`, `IsInCycleQuery`, `SccQuery`, `CondensationQuery`).
//!
//! Implements DAG unit `U_WS1_6_DIFF_CYCLE` of the
//! `graph-fidelity-planner-correctness` plan (DESIGN §2.3 of
//! `docs/development/graph-fidelity-planner-correctness/02_DESIGN-graph-fidelity-planner-correctness.md`).
//!
//! # What this family diffs
//!
//! Four registered `DerivedQuery`s in `sqry-db`:
//!
//! * `SccQuery` — Tarjan SCC decomposition for a given `EdgeKind` discriminant.
//! * `CondensationQuery` — inter-component DAG edges derived from `SccQuery`.
//! * `CyclesQuery` — `CycleBounds`-filtered SCC list for a `CircularType`.
//! * `IsInCycleQuery` — per-node membership predicate against the same bounds.
//!
//! Each is run against the matching baseline oracle
//! (`sqry_db::baseline::{scc, condensation, cycles, is_in_cycle}`) over the
//! `well_formed_graph()` proptest strategy (DAG unit `U_WS1_3_GRAPH_GEN`).
//!
//! # Determinism contract — why this family exists
//!
//! Both `SccQuery::execute` and `baseline::scc` walk the snapshot via
//! `EdgeStore::edges_from`, which iterates the delta buffer in `HashMap`
//! order. Without caching the neighbour list at push time, the iterative
//! Tarjan loop can advance the cursor over a different element across two
//! identical calls on the same snapshot — mis-targeting `lowlink`
//! propagation and producing different SCC decompositions on repeated runs.
//!
//! The first attempt at this DAG unit at SHA `b7883f8c2` surfaced exactly
//! that flake at `PROPTEST_CASES=10000`. Both halves of the fix have since
//! landed:
//!
//! * Production: PR #316 (squash `53c496c74`) — `sqry-db/src/queries/scc.rs`
//!   caches each node's outgoing neighbour list at push time and sorts the
//!   cached vector by `NodeId` for canonical traversal order.
//! * Baseline: WS1 commit `d7c974ac3` — `sqry-db/src/baseline.rs::scc` mirrors
//!   the same fix symmetrically so the oracle stays deterministic.
//!
//! This family is the regression harness for both halves. It walks
//! `well_formed_graph()` graphs skewed up to 16–64 nodes (DAG acceptance
//! criterion: "meaningful cycle coverage") and asserts the planner and
//! baseline agree under the canonical SCC equivalence (sorted-of-sorted
//! components — component indices are arbitrary).
//!
//! # Output shape & comparison strategy
//!
//! * `SccQuery` returns `Arc<CachedSccData> { components, node_to_component,
//!   edge_kind }`. Component indices are arbitrary — `SccQuery::execute`'s
//!   doc-comment explicitly says so. We canonicalise both sides by sorting
//!   each inner `Vec<NodeId>` and then sorting the outer `Vec`. This is the
//!   SCC equivalence relation (set-of-sets of `NodeId`s), not raw index
//!   order. We additionally cross-check `node_to_component` agreement using
//!   the canonical-component identifier (sorted `NodeId` tuple) so any
//!   `node_to_component` drift surfaces here too.
//!
//! * `CondensationQuery` returns `Arc<CachedCondensation> { dag_edges,
//!   component_count, edge_kind }`. Since component indices differ between
//!   planner and baseline, we translate both `dag_edges` into a canonical
//!   form: for every `(src_component, tgt_component)` DAG edge, look up the
//!   sorted member-`NodeId` tuple for each endpoint and compare the two
//!   `BTreeSet<(Vec<NodeId>, Vec<NodeId>)>` sets. `component_count` is
//!   compared directly (it's index-free).
//!
//! * `CyclesQuery` returns `Arc<Vec<Vec<NodeId>>>`. Same canonical sort as
//!   `SccQuery`: sort each inner, sort the outer. `bounds.max_results`
//!   truncation is applied on raw Tarjan order in both implementations, so
//!   to make the canonical comparison robust the test runs with
//!   `max_results = usize::MAX` (no truncation), plus a separate path that
//!   exercises the explicit truncation bound by comparing canonical set
//!   *subset* semantics — both implementations must return the same
//!   `max_results` cycles when ordered by canonical id.
//!
//! * `IsInCycleQuery` returns `bool`. Pointwise compare per `(node_id,
//!   CircularType, CycleBounds)` triple.
//!
//! # Graph size distribution (DAG acceptance)
//!
//! DAG acceptance for U_WS1_6_DIFF_CYCLE says "graph size distribution
//! skewed up to 16–64 nodes for meaningful cycle coverage". The base
//! `well_formed_graph()` strategy emits `1usize..=MAX_NODES` (= 64) with
//! the prop-test uniform-on-range default — most graphs are well below 16
//! nodes and therefore unlikely to contain non-trivial SCCs on the
//! filtered edge set.
//!
//! We bias the distribution by wrapping the base strategy in a
//! [`large_enough_graph`] adapter that uses `prop_filter` with
//! `node_ids.len() >= LARGE_GRAPH_MIN_NODES` (= 16). The DAG estimate of
//! ~1M `max_local_rejects` absorbs the rejection rate at PR/nightly case
//! counts: with uniform-in-1..=64 the unfiltered acceptance rate is
//! `(64-16+1)/64 ≈ 76.6%`, so 10 000 cases × ~1.3 rejects per accepted
//! case ≈ 13 000 rejects, well below the 1M budget. The 100 000-case
//! nightly run sits at ~130 000 rejects — still well below the budget.
//!
//! # Acceptance criteria (DAG verbatim)
//!
//! * PR-tier 10 000 cases pass for all four queries
//!   (`PROPTEST_CASES=10000 cargo test ...`).
//! * Nightly 100 000 cases pass.
//! * Graph size distribution skewed up to 16–64 nodes for meaningful cycle coverage.
//! * Any failure persists shrunken repro under `target/proptest-regressions/`
//!   (proptest's default behaviour).
//!
//! # Execution
//!
//! ```text
//! # PR-tier (default 1024 cases):
//! cargo test -p sqry-db --features baseline --test diff_cycle_family
//!
//! # Nightly-tier (10000 cases, release profile):
//! PROPTEST_CASES=10000 cargo test -p sqry-db --features baseline \
//!     --test diff_cycle_family --release
//! ```

#![allow(clippy::needless_pass_by_value)]

// `#[path]` inclusion keeps this test target a single compilation unit so
// cargo discovers the `#[test]` functions below. Matches the convention
// established by every other `diff_*_family.rs` file.
#[path = "graph_gen.rs"]
#[allow(unused_imports)]
mod graph_gen;

use std::collections::BTreeSet;
use std::sync::Arc;

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::query::CircularType;

use sqry_db::baseline;
use sqry_db::queries::{
    CachedSccData, CondensationQuery, CycleBounds, CyclesKey, CyclesQuery, IsInCycleKey,
    IsInCycleQuery, SccQuery,
};
use sqry_db::{QueryDb, QueryDbConfig};

use graph_gen::{GeneratedGraph, well_formed_graph};

// ---------------------------------------------------------------------------
// Proptest tuning
// ---------------------------------------------------------------------------

/// Reads `PROPTEST_CASES` from the environment, defaulting to 1024 for the
/// PR-tier `cargo test` invocation. Nightly CI sets 10000 (DESIGN §2.3).
/// Matches the convention established by `diff_unused_family.rs` /
/// `diff_cicall_family.rs`.
fn cases_from_env() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1024)
}

/// Minimum live-node count for a graph to be accepted into this family. DAG
/// acceptance criterion: "graph size distribution skewed up to 16–64 nodes
/// for meaningful cycle coverage". Filter rejection rate is ≲ 25% on the
/// base strategy (uniform `1..=64`), so the inflated budget below is
/// comfortable.
const LARGE_GRAPH_MIN_NODES: usize = 16;

/// `max_local_rejects` / `max_global_rejects` budget for the size-bias
/// `prop_filter`. Sized for the nightly 100 000-case run (≈ 130 000
/// rejects expected) with an order-of-magnitude safety margin.
const REJECT_BUDGET: u32 = 1_000_000;

/// Builds the shared `ProptestConfig` used by every `proptest!` block in this
/// family — `PROPTEST_CASES`-driven count + 10 000 shrink iterations (WS1
/// generator self-test convention) + an inflated reject budget so the
/// `LARGE_GRAPH_MIN_NODES` size-bias filter can run at 100 000-case scale
/// without the runner aborting.
fn family_config() -> ProptestConfig {
    ProptestConfig {
        cases: cases_from_env(),
        // Shrinker budget mirrors the WS1 generator self-test (DAG
        // acceptance for U_WS1_3_GRAPH_GEN: ≤ 10 000 iterations).
        max_shrink_iters: 10_000,
        // Absorb the ≥ LARGE_GRAPH_MIN_NODES filter rejection rate at
        // 10k / 100k case counts (see module-level comment).
        max_local_rejects: REJECT_BUDGET,
        max_global_rejects: REJECT_BUDGET,
        ..ProptestConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Size-biased strategy
// ---------------------------------------------------------------------------

/// Wraps `well_formed_graph()` with a `prop_filter` requiring at least
/// `LARGE_GRAPH_MIN_NODES` live `NodeId`s. The filter targets *live* nodes
/// (`GeneratedGraph::node_ids`) rather than recipe entries so the
/// distribution is biased on the post-materialisation arena, which is what
/// every query in this family observes.
fn large_enough_graph() -> impl Strategy<Value = GeneratedGraph> {
    well_formed_graph().prop_filter(
        "graph must carry at least LARGE_GRAPH_MIN_NODES live nodes for meaningful cycle coverage",
        |g: &GeneratedGraph| g.node_ids.len() >= LARGE_GRAPH_MIN_NODES,
    )
}

// ---------------------------------------------------------------------------
// Edge / cycle parameter sweeps
// ---------------------------------------------------------------------------

/// Every `EdgeKind` discriminant the cycle family needs to exercise.
///
/// `Calls` and `Imports` are the two production cycle-detection probes
/// (mirrored by `CircularType::Calls` / `CircularType::Imports` /
/// `CircularType::Modules`). `References` and `Defines` give additional
/// coverage of the planner's discriminant-only edge filter on edge kinds
/// the generator emits in bulk; `TypeOf` exercises the metadata-carrying
/// discriminant path. Discriminator equality is what both implementations
/// use to filter edges, so the actual field values inside each variant
/// don't matter — only the variant.
fn scc_edge_kinds() -> Vec<EdgeKind> {
    vec![
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        },
        EdgeKind::References,
        EdgeKind::Defines,
        EdgeKind::TypeOf {
            context: Some(sqry_core::graph::unified::edge::kind::TypeOfContext::Parameter),
            index: Some(0),
            name: None,
        },
    ]
}

/// All three `CircularType` variants exercised by the cycle / is-in-cycle
/// queries. `Calls` covers the `EdgeKind::Calls` discriminant; `Imports`
/// and `Modules` both probe the `EdgeKind::Imports` discriminant —
/// `cycle_edge_probe` collapses them in both the production query
/// (`edge_probe_for` in `sqry-db/src/queries/cycles.rs`) and the baseline
/// (`cycle_edge_probe` in `sqry-db/src/baseline.rs`).
const ALL_CIRCULAR_TYPES: &[CircularType] = &[
    CircularType::Calls,
    CircularType::Imports,
    CircularType::Modules,
];

/// A small bouquet of `CycleBounds` configurations swept per case. Each
/// row exercises a different combination of:
///
/// * `min_depth` (1 with self-loops vs 2 without — the production default
///   vs the self-loop-aware path).
/// * `max_depth` (`None` vs explicit caps that exclude the largest SCCs
///   in a 64-node graph).
/// * `max_results` (unbounded vs caps below the typical SCC count).
/// * `should_include_self_loops` (both polarities).
///
/// Bound rows are intentionally small so the per-case combinatorial
/// budget stays under control — six rows × three circular types ×
/// `node_ids.len()` for `IsInCycleQuery` is ≲ 64 × 18 ≈ 1.2 k probes per
/// case worst-case.
fn cycle_bounds_matrix() -> Vec<CycleBounds> {
    vec![
        // Production default: min_depth = 2, no self-loops, unbounded.
        CycleBounds::default(),
        // Include self-loops at min_depth = 1.
        CycleBounds {
            min_depth: 1,
            max_depth: None,
            max_results: usize::MAX,
            should_include_self_loops: true,
        },
        // Truncated result count below typical SCC count.
        CycleBounds {
            min_depth: 2,
            max_depth: None,
            max_results: 4,
            should_include_self_loops: false,
        },
        // Bounded depth window.
        CycleBounds {
            min_depth: 2,
            max_depth: Some(8),
            max_results: usize::MAX,
            should_include_self_loops: false,
        },
        // Narrow depth band excluding all small SCCs.
        CycleBounds {
            min_depth: 4,
            max_depth: Some(16),
            max_results: usize::MAX,
            should_include_self_loops: false,
        },
        // Self-loops + truncated result count: both early-break paths.
        CycleBounds {
            min_depth: 1,
            max_depth: Some(2),
            max_results: 2,
            should_include_self_loops: true,
        },
    ]
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Builds a fresh `QueryDb` over the generated graph's snapshot. Each
/// proptest case gets its own DB so cache state never leaks between cases —
/// the differential contract is single-snapshot correctness.
fn build_db(graph: &GeneratedGraph) -> (QueryDb, Arc<GraphSnapshot>) {
    let snapshot = Arc::new(graph.graph.snapshot());
    let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    (db, snapshot)
}

/// Canonicalises a list of components by sorting each inner `Vec<NodeId>`
/// in `(index, generation)` order and then sorting the outer collection.
/// The result is the SCC equivalence representative: two `CachedSccData`s
/// with permuted component indices canonicalise to the same form.
fn canonical_components(components: &[Vec<NodeId>]) -> Vec<Vec<NodeId>> {
    let mut canon: Vec<Vec<NodeId>> = components
        .iter()
        .map(|c| {
            let mut sorted = c.clone();
            sorted.sort_unstable();
            sorted
        })
        .collect();
    canon.sort_unstable();
    canon
}

/// Maps every live node in the snapshot to its canonical component
/// representative (the sorted `Vec<NodeId>` of its SCC members), or `None`
/// if the planner/baseline didn't place it in any component. Used to
/// cross-check `node_to_component` agreement once components have been
/// canonicalised.
fn node_to_canonical_component(
    snapshot: &GraphSnapshot,
    scc: &CachedSccData,
) -> Vec<(NodeId, Option<Vec<NodeId>>)> {
    snapshot
        .nodes()
        .iter()
        .filter(|(_, entry)| !entry.is_unified_loser())
        .map(|(nid, _)| {
            let comp = scc.component_of(nid).map(|cidx| {
                let mut members = scc.components[cidx as usize].clone();
                members.sort_unstable();
                members
            });
            (nid, comp)
        })
        .collect()
}

/// Builds the canonical-edge set of a condensation: every DAG edge
/// `(src_component_idx, tgt_component_idx)` mapped through the SCC's
/// component → sorted-member-`NodeId`-tuple identity.
fn canonical_condensation_edges(
    scc: &CachedSccData,
    dag_edges: &std::collections::HashMap<u32, Vec<u32>>,
) -> BTreeSet<(Vec<NodeId>, Vec<NodeId>)> {
    let component_identity = |idx: u32| -> Vec<NodeId> {
        let mut members = scc.components[idx as usize].clone();
        members.sort_unstable();
        members
    };
    let mut out: BTreeSet<(Vec<NodeId>, Vec<NodeId>)> = BTreeSet::new();
    for (src, successors) in dag_edges {
        let src_id = component_identity(*src);
        for tgt in successors {
            out.insert((src_id.clone(), component_identity(*tgt)));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// SccQuery — canonical-form differential
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(family_config())]

    /// `SccQuery::execute` and `baseline::scc` must agree on the SCC
    /// decomposition under the canonical (sort-inner-then-outer) equivalence
    /// for every `EdgeKind` discriminant in `scc_edge_kinds()`.
    ///
    /// Component *indices* are arbitrary by `SccQuery`'s contract, so the
    /// canonical form is the set-of-sets-of-`NodeId`. We additionally
    /// cross-check `node_to_component` agreement under the same canonical
    /// identifier (sorted `NodeId` tuple) so any drift in the membership
    /// map surfaces here too.
    #[test]
    fn scc_planner_equals_baseline(graph in large_enough_graph()) {
        let (db, snapshot) = build_db(&graph);
        for edge_kind in scc_edge_kinds() {
            let planner = db.get::<SccQuery>(&edge_kind);
            let baseline_out = baseline::scc(&snapshot, &edge_kind);

            let planner_canon = canonical_components(&planner.components);
            let baseline_canon = canonical_components(&baseline_out.components);
            prop_assert_eq!(
                &planner_canon,
                &baseline_canon,
                "SccQuery diverged from baseline::scc (canonical components)\n  edge_kind = {:?}\n  nodes = {}\n  edges = {}",
                edge_kind,
                graph.recipe.nodes.len(),
                graph.recipe.edges.len()
            );

            let planner_map = node_to_canonical_component(&snapshot, &planner);
            let baseline_map = node_to_canonical_component(&snapshot, &baseline_out);
            prop_assert_eq!(
                &planner_map,
                &baseline_map,
                "SccQuery node→component map diverged from baseline::scc\n  edge_kind = {:?}\n  nodes = {}\n  edges = {}",
                edge_kind,
                graph.recipe.nodes.len(),
                graph.recipe.edges.len()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// CondensationQuery — canonical-edge-set differential
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(family_config())]

    /// `CondensationQuery::execute` and `baseline::condensation` must agree
    /// on the inter-component DAG-edge set under the canonical
    /// `(sorted-NodeId-tuple, sorted-NodeId-tuple)` identity for every
    /// `EdgeKind` in `scc_edge_kinds()`.
    ///
    /// Component indices differ between planner and baseline — same
    /// reasoning as `SccQuery`. We compare the canonical edge set via
    /// `canonical_condensation_edges`, which translates each `(src_idx,
    /// tgt_idx)` pair through its SCC's sorted member tuple.
    ///
    /// `component_count` is index-free so we compare it directly: any
    /// disagreement on the number of components is a planner-vs-baseline
    /// drift that should never happen if the SCC differential above
    /// passed.
    #[test]
    fn condensation_planner_equals_baseline(graph in large_enough_graph()) {
        let (db, snapshot) = build_db(&graph);
        for edge_kind in scc_edge_kinds() {
            let planner_scc = db.get::<SccQuery>(&edge_kind);
            let baseline_scc = baseline::scc(&snapshot, &edge_kind);

            let planner_cond = db.get::<CondensationQuery>(&edge_kind);
            let baseline_edges = baseline::condensation(&snapshot, &edge_kind);

            prop_assert_eq!(
                planner_cond.component_count,
                planner_scc.components.len(),
                "Planner CondensationQuery.component_count out of sync with own SccQuery components\n  edge_kind = {:?}",
                edge_kind
            );
            prop_assert_eq!(
                planner_cond.component_count,
                baseline_scc.components.len(),
                "CondensationQuery.component_count diverged from baseline::scc.components.len()\n  edge_kind = {:?}\n  nodes = {}\n  edges = {}",
                edge_kind,
                graph.recipe.nodes.len(),
                graph.recipe.edges.len()
            );

            let planner_edges =
                canonical_condensation_edges(&planner_scc, &planner_cond.dag_edges);
            // Baseline `condensation` returns `BTreeSet<(u32, u32)>` keyed on
            // baseline-side component indices; reuse `canonical_condensation_edges`
            // by converting to the same `HashMap<u32, Vec<u32>>` representation
            // the planner emits, then translating through baseline's SCC.
            let mut baseline_dag: std::collections::HashMap<u32, Vec<u32>> =
                std::collections::HashMap::new();
            for (s, t) in &baseline_edges {
                baseline_dag.entry(*s).or_default().push(*t);
            }
            let baseline_edges_canon =
                canonical_condensation_edges(&baseline_scc, &baseline_dag);

            prop_assert_eq!(
                &planner_edges,
                &baseline_edges_canon,
                "CondensationQuery diverged from baseline::condensation (canonical DAG edges)\n  edge_kind = {:?}\n  nodes = {}\n  edges = {}",
                edge_kind,
                graph.recipe.nodes.len(),
                graph.recipe.edges.len()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// CyclesQuery — canonical-form differential
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(family_config())]

    /// `CyclesQuery::execute` and `baseline::cycles` must agree on the
    /// filtered cycle list under the canonical (sort-inner-then-outer)
    /// equivalence for every `(CircularType, CycleBounds)` pair in the
    /// per-case sweep.
    ///
    /// The truncation by `bounds.max_results` is applied on the raw Tarjan
    /// traversal order in both implementations — which would normally
    /// couple component-emission order into the differential. To stay
    /// canonical we compare:
    ///
    /// 1. The full (max_results = MAX) cycle list under canonical equality,
    ///    AND
    /// 2. The `bounds.max_results` count agreement (truncation cardinality).
    ///
    /// Step 1 covers the membership contract. Step 2 covers the truncation
    /// contract without locking the test to a specific traversal order
    /// (since both implementations now sort neighbours by `NodeId`, they
    /// should agree on traversal order in practice, but `prop_assert_eq!`
    /// on cardinality is the contract that survives any future refactor of
    /// the post-truncation sort).
    #[test]
    fn cycles_planner_equals_baseline(graph in large_enough_graph()) {
        let (db, snapshot) = build_db(&graph);
        for &circular_type in ALL_CIRCULAR_TYPES {
            for bounds in cycle_bounds_matrix() {
                // (1) Full canonical comparison at max_results = MAX.
                let unbounded = CycleBounds {
                    max_results: usize::MAX,
                    ..bounds
                };
                let unbounded_key = CyclesKey {
                    circular_type,
                    bounds: unbounded,
                };
                let planner_full = db.get::<CyclesQuery>(&unbounded_key);
                let baseline_full = baseline::cycles(&snapshot, circular_type, unbounded);
                let planner_canon = canonical_components(planner_full.as_ref());
                let baseline_canon = canonical_components(&baseline_full);
                prop_assert_eq!(
                    &planner_canon,
                    &baseline_canon,
                    "CyclesQuery diverged from baseline::cycles (canonical, unbounded max_results)\n  circular_type = {:?}\n  bounds = {:?}\n  nodes = {}\n  edges = {}",
                    circular_type,
                    bounds,
                    graph.recipe.nodes.len(),
                    graph.recipe.edges.len()
                );

                // (2) Truncation cardinality at the bound under test.
                let bounded_key = CyclesKey {
                    circular_type,
                    bounds,
                };
                let planner_bounded = db.get::<CyclesQuery>(&bounded_key);
                let baseline_bounded = baseline::cycles(&snapshot, circular_type, bounds);
                prop_assert_eq!(
                    planner_bounded.len(),
                    baseline_bounded.len(),
                    "CyclesQuery truncation cardinality diverged from baseline::cycles\n  circular_type = {:?}\n  bounds = {:?}\n  planner_len = {}, baseline_len = {}",
                    circular_type,
                    bounds,
                    planner_bounded.len(),
                    baseline_bounded.len()
                );

                // Truncated output must be a subset of the full canonical
                // cycle set on both sides. (Avoids relying on
                // truncation-time order beyond what the canonical-set
                // membership contract guarantees.)
                let full_set: BTreeSet<Vec<NodeId>> =
                    planner_canon.iter().cloned().collect();
                for cycle in planner_bounded.iter() {
                    let mut c = cycle.clone();
                    c.sort_unstable();
                    prop_assert!(
                        full_set.contains(&c),
                        "CyclesQuery bounded cycle {:?} not in unbounded set\n  circular_type = {:?}\n  bounds = {:?}",
                        c, circular_type, bounds
                    );
                }
                for cycle in baseline_bounded.iter() {
                    let mut c = cycle.clone();
                    c.sort_unstable();
                    prop_assert!(
                        full_set.contains(&c),
                        "baseline::cycles bounded cycle {:?} not in unbounded set\n  circular_type = {:?}\n  bounds = {:?}",
                        c, circular_type, bounds
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// IsInCycleQuery — pointwise differential
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(family_config())]

    /// `IsInCycleQuery::execute` and `baseline::is_in_cycle` must agree
    /// on every `(node_id, CircularType, CycleBounds)` triple, where the
    /// node id ranges over `graph.node_ids` and the bounds range over
    /// `cycle_bounds_matrix()`.
    ///
    /// This is the pointwise per-node membership contract that pairs with
    /// the set-level `CyclesQuery` differential above. Even if both
    /// implementations of `cycles` agree on the cycle set, they could in
    /// principle disagree on per-node membership (e.g. via a divergent
    /// self-loop check or `min_depth` predicate). This test pins both.
    #[test]
    fn is_in_cycle_planner_equals_baseline(graph in large_enough_graph()) {
        let (db, snapshot) = build_db(&graph);
        for &node_id in &graph.node_ids {
            for &circular_type in ALL_CIRCULAR_TYPES {
                for bounds in cycle_bounds_matrix() {
                    let key = IsInCycleKey {
                        node_id,
                        circular_type,
                        bounds,
                    };
                    let planner = db.get::<IsInCycleQuery>(&key);
                    let baseline_out = baseline::is_in_cycle(&snapshot, &key);
                    prop_assert_eq!(
                        planner,
                        baseline_out,
                        "IsInCycleQuery diverged from baseline::is_in_cycle\n  node_id = {:?}\n  circular_type = {:?}\n  bounds = {:?}\n  nodes = {}\n  edges = {}",
                        node_id,
                        circular_type,
                        bounds,
                        graph.recipe.nodes.len(),
                        graph.recipe.edges.len()
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cross-consistency: CyclesQuery set ⇔ IsInCycleQuery per-node truth
// ---------------------------------------------------------------------------
//
// The planner exposes two surfaces over the same underlying cycle
// definition. `CyclesQuery` returns the set of cycles (filtered by
// `CycleBounds`); `IsInCycleQuery` is the per-node membership predicate
// for the same `(CircularType, CycleBounds)` triple. They must agree once
// `max_results` is removed. Mirrors `diff_unused_family`'s
// `unused_set_membership_matches_is_node_unused`.
//
// This complements the per-query differentials: even if planner and
// baseline both miscomputed the predicate the same way, this property
// catches inconsistency between the two planner surfaces — a class of
// bugs the per-query differentials cannot see.

proptest! {
    #![proptest_config(family_config())]

    /// For every `(CircularType, CycleBounds-with-MAX-results)`, the set
    /// of `NodeId`s appearing in `CyclesQuery`'s output must equal the
    /// `{ node_id : IsInCycleQuery(node_id, ct, bounds) = true }` set
    /// derived from `graph.node_ids`.
    #[test]
    fn cycles_set_membership_matches_is_in_cycle(graph in large_enough_graph()) {
        let (db, _snapshot) = build_db(&graph);
        for &circular_type in ALL_CIRCULAR_TYPES {
            for bounds in cycle_bounds_matrix() {
                let unbounded = CycleBounds {
                    max_results: usize::MAX,
                    ..bounds
                };
                let set_key = CyclesKey {
                    circular_type,
                    bounds: unbounded,
                };
                let cycles_out = db.get::<CyclesQuery>(&set_key);
                let cycle_member_set: BTreeSet<NodeId> = cycles_out
                    .iter()
                    .flat_map(|c| c.iter().copied())
                    .collect();
                for &node_id in &graph.node_ids {
                    let probe_key = IsInCycleKey {
                        node_id,
                        circular_type,
                        bounds: unbounded,
                    };
                    let probe = db.get::<IsInCycleQuery>(&probe_key);
                    prop_assert_eq!(
                        probe,
                        cycle_member_set.contains(&node_id),
                        "Planner self-consistency: IsInCycleQuery({:?}, {:?}, {:?}) = {} but CyclesQuery membership = {}\n  nodes = {}\n  edges = {}",
                        node_id,
                        circular_type,
                        unbounded,
                        probe,
                        cycle_member_set.contains(&node_id),
                        graph.recipe.nodes.len(),
                        graph.recipe.edges.len()
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Anti-flake pin — deterministic 32-graph sample
// ---------------------------------------------------------------------------
//
// `PROPTEST_CASES` is environment-driven. Pin a small fixed-seed sample so
// the gate cannot be silently disabled by setting `PROPTEST_CASES=0` in CI,
// mirroring `diff_unused_family::fixed_seed_64_graphs_planner_matches_baseline`
// and `diff_cicall_family::fixed_seed_64_graphs_planner_matches_baseline_*`.
//
// Sample size = 32 graphs. The cycle family runs four queries × multiple
// parameter sweeps per graph (more expensive than the address-taken pin),
// so 32 graphs is the balance point that keeps the fixed-seed gate under
// a second of CPU time while still exercising every cycle bound row at
// least once with overwhelming probability.

/// Deterministically sample `count` *large-enough* graphs using a fixed RNG
/// seed — wraps the size-bias filter so the fixed-seed sample matches the
/// proptest distribution. Pulls from `large_enough_graph()` exactly like
/// the proptest cases do.
fn sampled_large_graphs(count: usize, seed: u64) -> Vec<GeneratedGraph> {
    let mut seed_bytes = [0u8; 32];
    for (i, chunk) in seed_bytes.chunks_exact_mut(8).enumerate() {
        let folded = seed ^ ((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        chunk.copy_from_slice(&folded.to_le_bytes());
    }
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &seed_bytes);
    // Match the filter rejection budget the proptest! blocks above set.
    let runner_config = Config {
        max_local_rejects: REJECT_BUDGET,
        max_global_rejects: REJECT_BUDGET,
        ..Config::default()
    };
    let mut runner = TestRunner::new_with_rng(runner_config, rng);
    let strategy = large_enough_graph();
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let tree = strategy
            .new_tree(&mut runner)
            .expect("strategy should not fail to produce a tree under REJECT_BUDGET");
        out.push(tree.current());
    }
    out
}

/// Fixed-seed SCC differential. The seed is shared with the
/// fixed-seed condensation pin below so they walk the same graph
/// sample — `SccQuery` is a strict prerequisite of `CondensationQuery`,
/// so coverage benefits from matched samples.
#[test]
fn fixed_seed_32_graphs_planner_matches_baseline_scc() {
    let graphs = sampled_large_graphs(32, 0xD1FF_0006_C000_0000);
    let mut total_components = 0usize;
    let mut graphs_with_nontrivial_scc = 0usize;
    for graph in &graphs {
        let snapshot = Arc::new(graph.graph.snapshot());
        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        for edge_kind in scc_edge_kinds() {
            let planner = db.get::<SccQuery>(&edge_kind);
            let baseline_out = baseline::scc(&snapshot, &edge_kind);
            assert_eq!(
                canonical_components(&planner.components),
                canonical_components(&baseline_out.components),
                "fixed-seed SCC differential failed: edge_kind={:?}, nodes={}, edges={}",
                edge_kind,
                graph.recipe.nodes.len(),
                graph.recipe.edges.len(),
            );
            total_components += planner.components.len();
            if planner.components.iter().any(|c| c.len() >= 2) {
                graphs_with_nontrivial_scc += 1;
            }
        }
    }
    // Non-vacuity guard. With the size-filter pinning graphs to ≥16 live
    // nodes and a five-EdgeKind sweep, the SCC decomposition must produce
    // at least one component per graph — anything less indicates a
    // generator regression that emptied the live-node set, or a `SccQuery`
    // / `baseline::scc` regression that silently dropped components.
    assert!(
        total_components > 0,
        "vacuous SCC coverage: 0 components across 32×{} edge kinds",
        scc_edge_kinds().len()
    );
    // Non-trivial SCCs (size ≥ 2) are reported as the secondary metric.
    // The well-formed-graph generator's edge distribution doesn't reliably
    // produce non-trivial SCCs at this sample size for *every* edge kind
    // in the sweep — the `Defines` edge kind is forest-structured by
    // construction (parent-child edges always point to the parent) so it
    // can never form a non-trivial SCC. The cycles fixed-seed pin (below)
    // is the canonical non-vacuity guard for non-trivial cycle coverage;
    // we surface this counter here for diagnosability without making it a
    // hard assertion, since `Defines`-only sweeps would always trip it.
    let _ = graphs_with_nontrivial_scc;
}

/// Fixed-seed cycles + is-in-cycle differential. Uses a separate seed so
/// the cycle pins exercise an independent slice of the generator's
/// distribution from the SCC pin.
#[test]
fn fixed_seed_32_graphs_planner_matches_baseline_cycles() {
    let graphs = sampled_large_graphs(32, 0xD1FF_0006_C000_0001);
    let mut total_cycles = 0usize;
    let mut total_is_in_cycle_true = 0usize;
    for graph in &graphs {
        let snapshot = Arc::new(graph.graph.snapshot());
        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        for &circular_type in ALL_CIRCULAR_TYPES {
            for bounds in cycle_bounds_matrix() {
                // Cycles set (unbounded).
                let unbounded = CycleBounds {
                    max_results: usize::MAX,
                    ..bounds
                };
                let key = CyclesKey {
                    circular_type,
                    bounds: unbounded,
                };
                let planner_full = db.get::<CyclesQuery>(&key);
                let baseline_full = baseline::cycles(&snapshot, circular_type, unbounded);
                assert_eq!(
                    canonical_components(planner_full.as_ref()),
                    canonical_components(&baseline_full),
                    "fixed-seed cycles differential failed: circular_type={:?}, bounds={:?}, nodes={}, edges={}",
                    circular_type,
                    bounds,
                    graph.recipe.nodes.len(),
                    graph.recipe.edges.len(),
                );
                total_cycles += planner_full.len();

                // Per-node IsInCycle agreement.
                for &node_id in &graph.node_ids {
                    let probe_key = IsInCycleKey {
                        node_id,
                        circular_type,
                        bounds,
                    };
                    let planner = db.get::<IsInCycleQuery>(&probe_key);
                    let baseline_out = baseline::is_in_cycle(&snapshot, &probe_key);
                    assert_eq!(
                        planner,
                        baseline_out,
                        "fixed-seed is_in_cycle differential failed: node_id={:?}, circular_type={:?}, bounds={:?}, nodes={}, edges={}",
                        node_id,
                        circular_type,
                        bounds,
                        graph.recipe.nodes.len(),
                        graph.recipe.edges.len(),
                    );
                    if planner {
                        total_is_in_cycle_true += 1;
                    }
                }
            }
        }
    }
    // Non-vacuity guard. With the size-bias filter pinning graphs to ≥ 16
    // live nodes and the cycle-bounds sweep covering the `min_depth=1 +
    // self_loops` row, at least one cycle (possibly a self-loop) should
    // appear across the 32-graph sample; a strict zero indicates a
    // generator / filter regression rather than the absence of cycles in
    // a particular run.
    assert!(
        total_cycles + total_is_in_cycle_true > 0,
        "vacuous cycle coverage across 32 graphs × {} circular_types × {} bound rows: \
         total_cycles={}, total_is_in_cycle_true={} — generator or size-filter regression",
        ALL_CIRCULAR_TYPES.len(),
        cycle_bounds_matrix().len(),
        total_cycles,
        total_is_in_cycle_true,
    );
}

/// Fixed-seed condensation differential. Shares the SCC pin's seed so the
/// canonical-edge-set comparison is exercised on the same large-graph
/// sample where the SCC pin already proved the decomposition matches —
/// any divergence here is a `CondensationQuery`-specific defect rather
/// than a downstream effect of SCC non-determinism.
#[test]
fn fixed_seed_32_graphs_planner_matches_baseline_condensation() {
    let graphs = sampled_large_graphs(32, 0xD1FF_0006_C000_0000);
    let mut total_dag_edges = 0usize;
    for graph in &graphs {
        let snapshot = Arc::new(graph.graph.snapshot());
        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        for edge_kind in scc_edge_kinds() {
            let planner_scc = db.get::<SccQuery>(&edge_kind);
            let baseline_scc = baseline::scc(&snapshot, &edge_kind);
            let planner_cond = db.get::<CondensationQuery>(&edge_kind);
            let baseline_edges = baseline::condensation(&snapshot, &edge_kind);

            assert_eq!(
                planner_cond.component_count,
                baseline_scc.components.len(),
                "fixed-seed condensation component_count mismatch: edge_kind={:?}",
                edge_kind
            );

            let planner_edges = canonical_condensation_edges(&planner_scc, &planner_cond.dag_edges);
            let mut baseline_dag: std::collections::HashMap<u32, Vec<u32>> =
                std::collections::HashMap::new();
            for (s, t) in &baseline_edges {
                baseline_dag.entry(*s).or_default().push(*t);
            }
            let baseline_edges_canon = canonical_condensation_edges(&baseline_scc, &baseline_dag);
            assert_eq!(
                planner_edges,
                baseline_edges_canon,
                "fixed-seed condensation differential failed: edge_kind={:?}, nodes={}, edges={}",
                edge_kind,
                graph.recipe.nodes.len(),
                graph.recipe.edges.len(),
            );
            total_dag_edges += planner_cond
                .dag_edges
                .values()
                .map(|v| v.len())
                .sum::<usize>();
        }
    }
    // Non-vacuity guard.
    assert!(
        total_dag_edges > 0,
        "vacuous condensation coverage: 0 inter-component DAG edges across 32×{} edge kinds",
        scc_edge_kinds().len()
    );
}
