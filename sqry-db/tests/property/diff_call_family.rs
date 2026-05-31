//! WS1 differential test family — call queries
//! (`CallersQuery`, `CalleesQuery`, `ReferencesQuery`).
//!
//! Implements DAG unit `U_WS1_4_DIFF_CALL` of the
//! `graph-fidelity-planner-correctness` plan (DESIGN §2.3 of
//! `docs/development/graph-fidelity-planner-correctness/02_DESIGN-graph-fidelity-planner-correctness.md`).
//!
//! # Pattern
//!
//! For each of the three queries we run a `proptest!` that:
//!
//! 1. Generates a well-formed `CodeGraph` via the
//!    [`property::graph_gen::well_formed_graph`] strategy (DESIGN §2.2 /
//!    `U_WS1_3_GRAPH_GEN`).
//! 2. Picks a target `NodeId` from the generated graph.
//! 3. Asks the production planner for the relation set keyed by the target
//!    node's name (`db.get::<Q>(&RelationKey::exact(name))`).
//! 4. Asks the baseline oracle ([`sqry_db::baseline`]) for the relation set
//!    keyed directly by the target `NodeId`.
//! 5. Asserts the two sets are equal.
//!
//! # Name-resolution adapter
//!
//! The production queries take a `RelationKey` (string pattern, segment-
//! aware language matching). The baseline oracle takes a `NodeId` directly
//! (see `sqry-db/src/baseline.rs` module docs).
//!
//! The adapter [`bridge_target_to_key`] in this file:
//!
//! 1. Resolves the target `NodeId`'s interned name through
//!    `snapshot.strings()`.
//! 2. Wraps it in an exact-mode [`RelationKey`].
//!
//! The well-formed-graph generator names every node `n{idx}_{kind}_{offset}`
//! — these names are unique by recipe index (see
//! `sqry-db/tests/property/graph_gen.rs` `assemble_recipe`). Uniqueness means
//! `RelationKey::exact(name)` matches exactly one node (no name collisions),
//! so the planner's name-keyed view of the relation reduces to the same set
//! the baseline computes from the target `NodeId`.
//!
//! Also: our generator never produces Phase 4c-prime unified losers (no
//! cross-file unification runs on synthetic recipes), so both the planner
//! and baseline iterate the same candidate node set.
//!
//! # Acceptance criteria (DAG verbatim)
//!
//! * PR-tier 10 000 cases pass for all three queries
//!   (`PROPTEST_CASES=10000 cargo test ...`).
//! * Nightly 100 000 cases pass.
//! * Any failure persists shrunken repro under `target/proptest-regressions/`
//!   (proptest's default behaviour).

#![allow(clippy::needless_pass_by_value)]

use std::collections::BTreeSet;
use std::sync::Arc;

use proptest::prelude::*;
use proptest::test_runner::Config;

use sqry_core::graph::unified::node::id::NodeId;

use sqry_db::QueryDb;
use sqry_db::QueryDbConfig;
use sqry_db::baseline;
use sqry_db::queries::{CalleesQuery, CallersQuery, ReferencesQuery, RelationKey};

#[path = "graph_gen.rs"]
#[allow(unused_imports)]
mod graph_gen;

use graph_gen::{GeneratedGraph, well_formed_graph};

// ---------------------------------------------------------------------------
// Adapter — bridge a NodeId target to a planner `RelationKey`
// ---------------------------------------------------------------------------

/// Resolves the target `NodeId`'s interned name into a planner
/// `RelationKey::exact(...)`.
///
/// Returns `None` if the node id does not resolve, or if its name is not in
/// the interner. In a well-formed graph this never happens, but the harness
/// reports `None` as "skip this case" rather than failing — the differential
/// invariant only makes sense when both sides see the same target.
fn bridge_target_to_key(graph: &GeneratedGraph, target: NodeId) -> Option<RelationKey> {
    let snapshot = graph.graph.snapshot();
    let entry = snapshot.nodes().get(target)?;
    let name = snapshot.strings().resolve(entry.name)?;
    Some(RelationKey::exact(name.as_ref().to_string()))
}

/// Strategy: pick an in-range NodeId from a generated graph. Returns the
/// (graph, target) pair so subsequent steps see the same graph instance.
fn graph_with_target() -> impl Strategy<Value = (GeneratedGraph, NodeId)> {
    well_formed_graph().prop_flat_map(|graph: GeneratedGraph| {
        let n = graph.node_ids.len();
        assert!(n > 0, "well_formed_graph guarantees ≥ 1 node");
        (Just(graph), 0usize..n).prop_map(|(g, idx)| {
            let target = g.node_ids[idx];
            (g, target)
        })
    })
}

/// Reads `PROPTEST_CASES` from the environment, defaulting to 256.
///
/// Matches the WS1_3 self-test convention (DESIGN §2.3). PR CI sets 10 000,
/// nightly 100 000.
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
// Planner ↔ baseline runners
//
// Each function performs one differential comparison and returns a
// `prop_assert_eq!`-compatible `Result<(), TestCaseError>`. The proptest body
// just forwards to one of these so the three families share assertion
// shape + diagnostic message form.
// ---------------------------------------------------------------------------

fn build_db(graph: &GeneratedGraph) -> QueryDb {
    let snapshot = Arc::new(graph.graph.snapshot());
    QueryDb::new(snapshot, QueryDbConfig::default())
}

fn run_callers_diff(graph: &GeneratedGraph, target: NodeId) -> Result<(), TestCaseError> {
    let Some(key) = bridge_target_to_key(graph, target) else {
        return Ok(());
    };

    let db = build_db(graph);
    let planner_vec: Arc<Vec<NodeId>> = db.get::<CallersQuery>(&key);
    let planner: BTreeSet<NodeId> = planner_vec.iter().copied().collect();

    let snapshot = graph.graph.snapshot();
    let baseline: BTreeSet<NodeId> = baseline::callers(&snapshot, target);

    prop_assert_eq!(
        planner,
        baseline,
        "CallersQuery planner ≠ baseline for target={target:?} (key={key:?}). \
         recipe={recipe:#?}",
        target = target,
        key = key,
        recipe = graph.recipe,
    );
    Ok(())
}

fn run_callees_diff(graph: &GeneratedGraph, target: NodeId) -> Result<(), TestCaseError> {
    let Some(key) = bridge_target_to_key(graph, target) else {
        return Ok(());
    };

    let db = build_db(graph);
    let planner_vec: Arc<Vec<NodeId>> = db.get::<CalleesQuery>(&key);
    let planner: BTreeSet<NodeId> = planner_vec.iter().copied().collect();

    let snapshot = graph.graph.snapshot();
    let baseline: BTreeSet<NodeId> = baseline::callees(&snapshot, target);

    prop_assert_eq!(
        planner,
        baseline,
        "CalleesQuery planner ≠ baseline for target={target:?} (key={key:?}). \
         recipe={recipe:#?}",
        target = target,
        key = key,
        recipe = graph.recipe,
    );
    Ok(())
}

fn run_references_diff(graph: &GeneratedGraph, target: NodeId) -> Result<(), TestCaseError> {
    let Some(key) = bridge_target_to_key(graph, target) else {
        return Ok(());
    };

    let db = build_db(graph);
    let planner_vec: Arc<Vec<NodeId>> = db.get::<ReferencesQuery>(&key);
    let planner: BTreeSet<NodeId> = planner_vec.iter().copied().collect();

    let snapshot = graph.graph.snapshot();
    let baseline: BTreeSet<NodeId> = baseline::references(&snapshot, target);

    prop_assert_eq!(
        planner,
        baseline,
        "ReferencesQuery planner ≠ baseline for target={target:?} (key={key:?}). \
         recipe={recipe:#?}",
        target = target,
        key = key,
        recipe = graph.recipe,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Proptest bodies — one per query
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest_config())]

    /// `CallersQuery` planner output equals baseline oracle for every
    /// well-formed graph + target pair.
    #[test]
    fn callers_planner_equals_baseline(
        (graph, target) in graph_with_target(),
    ) {
        run_callers_diff(&graph, target)?;
    }

    /// `CalleesQuery` planner output equals baseline oracle for every
    /// well-formed graph + target pair.
    #[test]
    fn callees_planner_equals_baseline(
        (graph, target) in graph_with_target(),
    ) {
        run_callees_diff(&graph, target)?;
    }

    /// `ReferencesQuery` planner output equals baseline oracle for every
    /// well-formed graph + target pair.
    #[test]
    fn references_planner_equals_baseline(
        (graph, target) in graph_with_target(),
    ) {
        run_references_diff(&graph, target)?;
    }
}
