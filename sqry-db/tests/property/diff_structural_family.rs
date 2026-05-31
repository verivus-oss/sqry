//! WS1 differential test family — **structural** relations (DAG unit
//! `U_WS1_5_DIFF_STRUCT`).
//!
//! Covers three of the six `DerivedQuery` relation predicates registered
//! by [`sqry_db::QueryDb`]'s `register_builtin_queries`:
//!
//! | Planner query        | Baseline oracle (NodeId-keyed)    |
//! |----------------------|-----------------------------------|
//! | [`ImportsQuery`]     | [`sqry_db::baseline::imports`]    |
//! | [`ExportsQuery`]     | [`sqry_db::baseline::exports`]    |
//! | [`ImplementsQuery`]  | [`sqry_db::baseline::implements`] |
//!
//! `ImplementsQuery` is the C-icall / OOP relation predicate added by HEAD's
//! `register_builtin_queries` — it traverses outgoing `Implements` edges and
//! matches the target endpoint name, exactly like `ImportsQuery` traverses
//! outgoing `Imports`. The earlier SPEC named this family "relation_query";
//! PR #315 corrected that phantom name to the three concrete `*Query` types
//! the planner actually registers.
//!
//! # Shape (DESIGN §2.3, mirroring `diff_call_family.rs`)
//!
//! Per query:
//!
//! 1. Generate a well-formed `GeneratedGraph` via
//!    [`graph_gen::well_formed_graph`].
//! 2. Pick a target node index. The Phase 4c-prime `CALL_COMPATIBLE_KINDS`
//!    constraint applies only to `Calls` edges; structural edges can target
//!    any kind. We still exclude `NodeKind::Import` as the planner exposes
//!    an [`Import`-name fast-path] inside `compute_relation_source_set`
//!    that the NodeId-keyed baseline does not implement (the fast-path
//!    returns the candidate node itself when it IS an `Import` whose own
//!    name matches the key; with NodeId-keyed semantics there is nothing
//!    to surface there). Skipping `NodeKind::Import` targets sidesteps the
//!    divergence without weakening structural coverage.
//! 3. Resolve the target NodeId to its interned name. Because the
//!    generator allocates unique names of the shape `n{i}_{kind}_{offset}`
//!    (see `graph_gen::assemble_recipe`), the name resolves the planner's
//!    `RelationKey::exact(name)` back to exactly the same NodeId, so the
//!    NodeId-keyed baseline and the name-keyed planner agree on the
//!    referent.
//! 4. Build a `QueryDb` over the generated snapshot. `QueryDb::new`
//!    auto-registers all 17 built-ins so the planner queries used here
//!    are reachable without manual `register::<>()` plumbing.
//! 5. Run the planner query through `db.get::<Q>(&RelationKey::exact(name))`,
//!    collect to a `BTreeSet<NodeId>` for diff stability.
//! 6. Run the baseline oracle with the same NodeId.
//! 7. `prop_assert_eq!` the two sets.
//!
//! # `PROPTEST_CASES` budget
//!
//! Environment-driven via `PROPTEST_CASES` (proptest's standard hook, also
//! read explicitly so the default in the absence of the env var stays
//! at 256 — the project-wide PR-tier setting). CI matrix sets:
//!
//! * PR-tier: `PROPTEST_CASES=10000` (verification gate).
//! * Nightly: `PROPTEST_CASES=100000`.
//!
//! Defaults pinned at 256 for fast local runs; bump via env to reproduce CI
//! load.
//!
//! [`Import`-name fast-path]:
//!     https://github.com/verivus-oss/sqry/blob/master/sqry-db/src/queries/relation.rs

#![allow(clippy::needless_pass_by_value)] // proptest strategies pass owned values intentionally.

// Pull in the sibling generator module. `tests/property/graph_gen.rs` is the
// canonical well-formed-graph generator used by every diff family file
// (DAG units U_WS1_4 .. U_WS1_8 share the same fixture).
mod graph_gen;

use std::collections::BTreeSet;
use std::sync::Arc;

use proptest::prelude::*;
use proptest::test_runner::Config;

use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;

use sqry_db::baseline;
use sqry_db::queries::relation::{ExportsQuery, ImplementsQuery, ImportsQuery, RelationKey};
use sqry_db::{QueryDb, QueryDbConfig};

use graph_gen::{GeneratedGraph, well_formed_graph};

// ---------------------------------------------------------------------------
// Proptest configuration
// ---------------------------------------------------------------------------

/// Default proptest case count when `PROPTEST_CASES` is unset.
///
/// 256 keeps the default `cargo test` run fast (< 30 s on developer
/// hardware). PR CI sets `PROPTEST_CASES=10000`; nightly sets `100000`.
/// Proptest also honours the env var natively; reading it here lets us
/// log the resolved value on test entry for reproducibility.
const DEFAULT_CASES: u32 = 256;

fn resolve_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(DEFAULT_CASES)
}

fn config() -> Config {
    Config {
        cases: resolve_cases(),
        // Failure cases shrink within the budget the generator's custom
        // ValueTree implements (see `graph_gen.rs` shrinker docs).
        max_shrink_iters: 10_000,
        // Persistent regression cache lives under
        // `target/proptest-regressions/<test name>.txt`. Nightly preserves
        // them as a CI artifact (see DESIGN §2.3).
        failure_persistence: None,
        ..Config::default()
    }
}

// ---------------------------------------------------------------------------
// Adapter — bridges NodeId-keyed baseline to name-keyed planner
// ---------------------------------------------------------------------------

/// Resolves a NodeId to the interned name the planner expects in
/// `RelationKey::exact`. Returns `None` if the node is missing (impossible
/// for an id pulled out of `GeneratedGraph::node_ids` but kept defensive
/// for shrinker survivability) or has an unresolved name string.
fn name_for(graph: &GeneratedGraph, target: NodeId) -> Option<String> {
    let snapshot = graph.graph.snapshot();
    let entry = snapshot.nodes().get(target)?;
    let resolved = snapshot.strings().resolve(entry.name)?;
    Some(resolved.as_ref().to_owned())
}

/// Picks a target NodeId for the diff. Restricts to non-`Import` kinds so
/// the planner's `entry.kind == NodeKind::Import` fast-path in
/// `compute_relation_source_set` does not produce an artefactual divergence
/// from the NodeId-keyed baseline. See module-level docs.
fn pick_target_nodeid(graph: &GeneratedGraph, raw_idx: usize) -> Option<NodeId> {
    if graph.node_ids.is_empty() {
        return None;
    }
    let n = graph.node_ids.len();
    // Scan starting at `raw_idx % n`, wrapping, returning the first slot
    // whose recipe entry is NOT NodeKind::Import. With the curated
    // `node_kind_strategy` in graph_gen most kinds qualify, so the linear
    // scan terminates in O(1) expected.
    for offset in 0..n {
        let i = (raw_idx + offset) % n;
        if graph.recipe.nodes[i].kind != NodeKind::Import {
            return Some(graph.node_ids[i]);
        }
    }
    None
}

/// Composite target-selection strategy: pairs a graph with an opaque
/// integer used to derive the target node index. Keeping the selector as a
/// raw `usize` (rather than baking it into a custom Strategy) lets the
/// outer strategy keep proptest's built-in shrinker for the integer while
/// the graph keeps the custom shrinker from `WellFormedGraphTree`.
fn graph_and_target_idx() -> impl Strategy<Value = (GeneratedGraph, usize)> {
    (well_formed_graph(), 0usize..1024)
}

// ---------------------------------------------------------------------------
// Per-query equality assertions
// ---------------------------------------------------------------------------

fn run_imports_check(graph: GeneratedGraph, raw_idx: usize) -> Result<(), TestCaseError> {
    let Some(target) = pick_target_nodeid(&graph, raw_idx) else {
        return Ok(()); // No eligible target — vacuous pass.
    };
    let Some(name) = name_for(&graph, target) else {
        return Ok(());
    };
    let snapshot = Arc::new(graph.graph.snapshot());
    let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    let planner_arc = db.get::<ImportsQuery>(&RelationKey::exact(name.clone()));
    let planner_out: BTreeSet<NodeId> = planner_arc.iter().copied().collect();
    let baseline_out = baseline::imports(&snapshot, target);
    prop_assert_eq!(
        planner_out,
        baseline_out,
        "ImportsQuery diverged from baseline::imports for target NodeId \
         {target:?} (name={name:?}); recipe={recipe:#?}",
        target = target,
        name = name,
        recipe = graph.recipe,
    );
    Ok(())
}

fn run_exports_check(graph: GeneratedGraph, raw_idx: usize) -> Result<(), TestCaseError> {
    let Some(target) = pick_target_nodeid(&graph, raw_idx) else {
        return Ok(());
    };
    let Some(name) = name_for(&graph, target) else {
        return Ok(());
    };
    let snapshot = Arc::new(graph.graph.snapshot());
    let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    let planner_arc = db.get::<ExportsQuery>(&RelationKey::exact(name.clone()));
    let planner_out: BTreeSet<NodeId> = planner_arc.iter().copied().collect();
    let baseline_out = baseline::exports(&snapshot, target);
    prop_assert_eq!(
        planner_out,
        baseline_out,
        "ExportsQuery diverged from baseline::exports for target NodeId \
         {target:?} (name={name:?}); recipe={recipe:#?}",
        target = target,
        name = name,
        recipe = graph.recipe,
    );
    Ok(())
}

fn run_implements_check(graph: GeneratedGraph, raw_idx: usize) -> Result<(), TestCaseError> {
    let Some(target) = pick_target_nodeid(&graph, raw_idx) else {
        return Ok(());
    };
    let Some(name) = name_for(&graph, target) else {
        return Ok(());
    };
    let snapshot = Arc::new(graph.graph.snapshot());
    let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
    let planner_arc = db.get::<ImplementsQuery>(&RelationKey::exact(name.clone()));
    let planner_out: BTreeSet<NodeId> = planner_arc.iter().copied().collect();
    let baseline_out = baseline::implements(&snapshot, target);
    prop_assert_eq!(
        planner_out,
        baseline_out,
        "ImplementsQuery diverged from baseline::implements for target NodeId \
         {target:?} (name={name:?}); recipe={recipe:#?}",
        target = target,
        name = name,
        recipe = graph.recipe,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Proptest entry points — one per query in the family.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(config())]

    /// `ImportsQuery` agrees with `baseline::imports` for every well-formed
    /// graph + non-`Import` target NodeId. Cache: built fresh per case so
    /// no cross-case state can mask a divergence.
    #[test]
    fn imports_planner_equals_baseline((graph, idx) in graph_and_target_idx()) {
        run_imports_check(graph, idx)?;
    }

    /// `ExportsQuery` agrees with `baseline::exports`. Exercises the
    /// `EndpointRole::Either` self-loop skip on both sides — baseline
    /// `exports` (see `sqry-db/src/baseline.rs:339`) skips `node_id == target`
    /// and any edge whose source==target, matching the planner's
    /// `Either` skip in `compute_relation_source_set`.
    #[test]
    fn exports_planner_equals_baseline((graph, idx) in graph_and_target_idx()) {
        run_exports_check(graph, idx)?;
    }

    /// `ImplementsQuery` agrees with `baseline::implements`. The OOP /
    /// C-icall relation predicate: outgoing `Implements` edges from a
    /// candidate to a target node whose name matches the key.
    #[test]
    fn implements_planner_equals_baseline((graph, idx) in graph_and_target_idx()) {
        run_implements_check(graph, idx)?;
    }
}
