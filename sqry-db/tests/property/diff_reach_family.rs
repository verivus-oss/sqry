//! WS1 differential test family — reach (DAG unit `U_WS1_7_DIFF_REACH`,
//! DESIGN §2.3).
//!
//! Pins planner output against the baseline-executor oracle
//! (`sqry_db::baseline`) for the three reach-flavoured derived queries:
//!
//! | DerivedQuery                       | Baseline                                  |
//! |------------------------------------|-------------------------------------------|
//! | [`ReachabilityQuery`]              | [`baseline::reachability`]                |
//! | [`EntryPointsQuery`]               | [`baseline::entry_points`]                |
//! | [`ReachableFromEntryPointsQuery`]  | [`baseline::reachable_from_entry_points`] |
//!
//! Each proptest builds a well-formed `CodeGraph` via the shared generator
//! (`property::graph_gen::well_formed_graph`), spins up a fresh `QueryDb`
//! over its snapshot, and asserts the planner-driven `db.get::<Q>(key)`
//! result is `==` the baseline output.
//!
//! # Entry-point coverage caveat
//!
//! `EntryPointsQuery` (and therefore `ReachableFromEntryPointsQuery`) fires
//! when *any* of four predicate disjuncts hold on a node:
//!
//! 1. `visibility ∈ {"public", "pub"}`.
//! 2. `name == "main" || starts_with("test_") || ends_with("_test")`.
//! 3. `NodeKind::Export`.
//! 4. `NodeKind::Test`.
//!
//! The current well-formed graph generator (see DESIGN §2.2 +
//! `property::graph_gen`) does **not** set `visibility` and emits synthetic
//! node names (`n{i}_{kind}_{byte_offset}`) that never match the
//! `main` / `test_*` / `*_test` patterns, so disjuncts (1) and (2) are not
//! exercised. Disjuncts (3) and (4) ARE exercised — `NodeKind::Export` and
//! `NodeKind::Test` both appear in the generator's `node_kind_strategy`
//! curated set (verified at `property::graph_gen` lines 1194 and 1199).
//!
//! Differential equivalence still holds: baseline and production code both
//! evaluate the *same* predicate, so any kind-driven failure shows up here
//! identically in both sides of the comparison. The proptest exercises the
//! shared kind-driven path. Broader visibility / name coverage is a SPEC-
//! level generator extension (not in scope for `U_WS1_7_DIFF_REACH`).
//!
//! # Running
//!
//! ```text
//! cargo test -p sqry-db --test diff_reach_family
//! PROPTEST_CASES=10000 cargo test -p sqry-db --test diff_reach_family --release
//! ```
//!
//! `PROPTEST_CASES` follows the WS1 convention: default 1024 for local,
//! 10 000 for PR CI, 100 000 for nightly (DESIGN §2.3).

// The `property` module pulled in via `#[path]` is shared with the other WS1
// diff-family test targets. Some helpers from `graph_gen.rs` are referenced
// only by sibling units (e.g. `sample_graphs`, `observed_edge_tags`); silence
// the unused-import lint here since the module surface is single-sourced.
#[path = "graph_gen.rs"]
#[allow(unused_imports)] // `dead_code` is already silenced at the module level inside graph_gen.rs.
mod graph_gen;

use std::collections::BTreeSet;
use std::sync::Arc;

use proptest::prelude::*;
use proptest::test_runner::Config as ProptestConfig;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia, TypeOfContext};
use sqry_core::graph::unified::node::id::NodeId;

use sqry_db::QueryDb;
use sqry_db::baseline;
use sqry_db::config::QueryDbConfig;
use sqry_db::queries::{
    EntryPointsQuery, ReachabilityKey, ReachabilityQuery, ReachableFromEntryPointsQuery,
};

use graph_gen::{GeneratedGraph, well_formed_graph};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Reads `PROPTEST_CASES` from the environment, defaulting to 256.
///
/// 256 is the WS1 family-file default (DESIGN §2.3 calls out three case
/// tiers: 256 local, 10 000 PR-CI, 100 000 nightly). Each generated graph
/// drives three queries here; running them at 256 cases keeps a clean
/// `cargo test` run well under the test-time budget while still surfacing
/// regressions across edge-kind variety.
fn cases_from_env() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(256)
}

fn proptest_config() -> ProptestConfig {
    ProptestConfig {
        cases: cases_from_env(),
        // The generator never produces ill-formed graphs (verified by
        // `graph_gen_self_test::generated_graphs_are_well_formed`), so we
        // keep the default shrinking budget for any planner-vs-baseline
        // divergence the shrinker has to chase.
        max_shrink_iters: 10_000,
        ..ProptestConfig::default()
    }
}

/// Materialise a fresh `Arc<GraphSnapshot>` from the generated graph.
///
/// The generator hands back an `Arc<CodeGraph>`; the snapshot must live in
/// its own `Arc` to satisfy `QueryDb::new`'s ownership contract.
fn snapshot_of(g: &GeneratedGraph) -> Arc<GraphSnapshot> {
    Arc::new(g.graph.snapshot())
}

/// Canonical list of probe `EdgeKind` discriminants for the
/// `ReachabilityQuery` differential. Mirrors the discriminator-driven
/// matching the production query implements
/// (`sqry-db/src/queries/reachability.rs` — `std::mem::discriminant`
/// comparison). Includes both reachability-edge kinds (Calls / References
/// / Imports / Inherits / Implements / TypeOf) and one non-reachability
/// kind (`Defines`) so the differential exercises both branches.
///
/// Metadata fields are filled with canonical defaults; the production
/// matcher ignores them by design.
fn reachability_probe_kinds() -> Vec<EdgeKind> {
    vec![
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        EdgeKind::References,
        EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        },
        EdgeKind::Inherits,
        EdgeKind::Implements,
        EdgeKind::TypeOf {
            context: Some(TypeOfContext::Parameter),
            index: None,
            name: None,
        },
        EdgeKind::Defines,
    ]
}

/// Pick a probe kind by index modulo the canonical list. Cloned per use
/// so the proptest input space stays small (u8) and deterministic.
fn pick_probe_kind(idx: u8) -> EdgeKind {
    let kinds = reachability_probe_kinds();
    kinds[(idx as usize) % kinds.len()].clone()
}

/// Pick up to `n` recipe-local node indices for the reachability root set,
/// modulo the live node count. Returns the deduplicated set of
/// corresponding live `NodeId`s.
///
/// The proptest hands us `(u8, u8, u8)` raw seeds; mapping them through
/// modulo arithmetic keeps the input vector trivially shrinkable while
/// still covering single-root, 2-root, and 3-root call patterns.
fn pick_roots(g: &GeneratedGraph, raw: &[u8]) -> Vec<NodeId> {
    let n = g.node_ids.len();
    if n == 0 {
        return Vec::new();
    }
    let mut roots: Vec<NodeId> = raw
        .iter()
        .map(|seed| g.node_ids[(*seed as usize) % n])
        .collect();
    // Deduplicate while preserving first-occurrence order so the proptest
    // log prints a minimal root set on shrink.
    roots.sort_by_key(|id| (id.index(), id.generation()));
    roots.dedup();
    roots
}

// ---------------------------------------------------------------------------
// 1. ReachabilityQuery
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest_config())]

    /// `ReachabilityQuery` ≡ `baseline::reachability` over every well-formed
    /// graph + (root-set, edge-kind) input. The production query keys on
    /// `ReachabilityKey { roots, edge_kind }` and matches edges by
    /// discriminant; the baseline mirrors that contract via
    /// `same_kind(&edge.kind, edge_kind)`.
    #[test]
    fn reachability_planner_equals_baseline(
        graph in well_formed_graph(),
        root_seeds in proptest::collection::vec(any::<u8>(), 1usize..=3),
        kind_seed in any::<u8>(),
    ) {
        let snapshot = snapshot_of(&graph);
        let roots = pick_roots(&graph, &root_seeds);
        // Skip degenerate inputs — well_formed_graph() can yield empty
        // node sets only at the absolute generator floor (1 node minimum,
        // see `graph_gen_self_test::thousand_sample_graphs_are_well_formed`
        // sanity floor), but be defensive anyway.
        prop_assume!(!roots.is_empty());
        let edge_kind = pick_probe_kind(kind_seed);

        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        let key = ReachabilityKey {
            roots: roots.clone(),
            edge_kind: edge_kind.clone(),
        };
        let planner = db.get::<ReachabilityQuery>(&key);
        let planner_set: BTreeSet<NodeId> = planner.reachable.iter().copied().collect();

        let baseline_set: BTreeSet<NodeId> =
            baseline::reachability(&snapshot, &roots, &edge_kind);

        prop_assert_eq!(
            planner_set,
            baseline_set,
            "ReachabilityQuery divergence: roots={:?} edge_kind={:?}",
            roots,
            edge_kind
        );
    }
}

// ---------------------------------------------------------------------------
// 2. EntryPointsQuery
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest_config())]

    /// `EntryPointsQuery` ≡ `baseline::entry_points` over every well-formed
    /// graph. The production query iterates `snapshot.nodes()` filtering by
    /// the (`is_public` ∨ `is_main_or_test` ∨ `NodeKind::Export` ∨
    /// `NodeKind::Test`) disjunction (`queries/unused.rs::is_entry_point`);
    /// the baseline mirrors that via `entry_point_predicate`.
    ///
    /// See module docs for the entry-point coverage caveat — only the
    /// kind-driven disjuncts fire under the current generator.
    #[test]
    fn entry_points_planner_equals_baseline(graph in well_formed_graph()) {
        let snapshot = snapshot_of(&graph);

        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        let planner = db.get::<EntryPointsQuery>(&());
        let planner_set: BTreeSet<NodeId> = planner.iter().copied().collect();

        let baseline_set: BTreeSet<NodeId> = baseline::entry_points(&snapshot);

        prop_assert_eq!(
            planner_set,
            baseline_set,
            "EntryPointsQuery divergence on {} nodes",
            graph.recipe.nodes.len()
        );
    }
}

// ---------------------------------------------------------------------------
// 3. ReachableFromEntryPointsQuery
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest_config())]

    /// `ReachableFromEntryPointsQuery` ≡ `baseline::reachable_from_entry_points`
    /// over every well-formed graph. Both walk the entry-point set under
    /// the reachability-edge predicate (`Calls` ∨ `References` ∨ `Imports`
    /// ∨ `Inherits` ∨ `Implements` ∨ `TypeOf`) — see
    /// `queries/unused.rs::is_reachability_edge` and
    /// `baseline::is_reachability_edge`.
    #[test]
    fn reachable_from_entry_points_planner_equals_baseline(
        graph in well_formed_graph()
    ) {
        let snapshot = snapshot_of(&graph);

        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        let planner = db.get::<ReachableFromEntryPointsQuery>(&());
        let planner_set: BTreeSet<NodeId> = planner.iter().copied().collect();

        let baseline_set: BTreeSet<NodeId> =
            baseline::reachable_from_entry_points(&snapshot);

        prop_assert_eq!(
            planner_set,
            baseline_set,
            "ReachableFromEntryPointsQuery divergence on {} nodes / {} edges",
            graph.recipe.nodes.len(),
            graph.recipe.edges.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Sanity smoke tests (non-proptest)
// ---------------------------------------------------------------------------
//
// These pin the contract on a tiny hand-built input drawn from the
// generator's deterministic-seed path, so a `cargo test` invocation with
// `PROPTEST_CASES=0` (or any environment that suppresses proptest) still
// surfaces wiring regressions (wrong feature flag, missing baseline import,
// changed query signature).

#[test]
fn sanity_reachability_planner_equals_baseline_on_sample() {
    let graphs = graph_gen::sample_graphs(8, 0xCAFE_BABE);
    for (i, g) in graphs.iter().enumerate() {
        let snapshot = snapshot_of(g);
        if g.node_ids.is_empty() {
            continue;
        }
        let roots = vec![g.node_ids[0]];
        for edge_kind in reachability_probe_kinds() {
            let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
            let key = ReachabilityKey {
                roots: roots.clone(),
                edge_kind: edge_kind.clone(),
            };
            let planner: BTreeSet<NodeId> = db
                .get::<ReachabilityQuery>(&key)
                .reachable
                .iter()
                .copied()
                .collect();
            let baseline_set = baseline::reachability(&snapshot, &roots, &edge_kind);
            assert_eq!(
                planner, baseline_set,
                "sanity divergence on graph #{i} edge_kind={edge_kind:?}"
            );
        }
    }
}

#[test]
fn sanity_entry_points_planner_equals_baseline_on_sample() {
    let graphs = graph_gen::sample_graphs(8, 0xCAFE_F00D);
    for (i, g) in graphs.iter().enumerate() {
        let snapshot = snapshot_of(g);
        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        let planner: BTreeSet<NodeId> = db.get::<EntryPointsQuery>(&()).iter().copied().collect();
        let baseline_set = baseline::entry_points(&snapshot);
        assert_eq!(planner, baseline_set, "sanity divergence on graph #{i}");
    }
}

#[test]
fn sanity_reachable_from_entry_points_planner_equals_baseline_on_sample() {
    let graphs = graph_gen::sample_graphs(8, 0xDEAD_BEEF);
    for (i, g) in graphs.iter().enumerate() {
        let snapshot = snapshot_of(g);
        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        let planner: BTreeSet<NodeId> = db
            .get::<ReachableFromEntryPointsQuery>(&())
            .iter()
            .copied()
            .collect();
        let baseline_set = baseline::reachable_from_entry_points(&snapshot);
        assert_eq!(planner, baseline_set, "sanity divergence on graph #{i}");
    }
}
