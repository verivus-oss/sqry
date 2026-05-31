//! Self-tests for the WS1 well-formed `CodeGraph` generator (DAG unit
//! `U_WS1_3_GRAPH_GEN`, DESIGN §2.2).
//!
//! Acceptance criteria (from the DAG):
//!
//! 1. **`generated_graphs_are_well_formed`** — every graph produced by
//!    `well_formed_graph()` passes `check_well_formed`. The harness runs
//!    `PROPTEST_CASES` (default 1024, scalable via env to 10k or 100k) cases
//!    in `proptest!` form so failures shrink. The required "1000 generated
//!    graphs all pass" check is additionally pinned by
//!    `thousand_sample_graphs_are_well_formed` which drives the strategy
//!    1000 times via [`crate::property::graph_gen::sample_graphs`].
//! 2. **`shrink_synthetic_counter_example`** — a synthetic property that
//!    fails on every graph runs through the shrinker; the final witness
//!    must have `≤ 12` nodes and `≤ 20` edges, found in `≤ 10 000`
//!    shrink iterations.
//! 3. **`all_edge_kinds_emitted`** — across a 2048-graph sample the
//!    generator hits every one of the 38 `EdgeKind` discriminants.
//!
//! Run as:
//!
//! ```text
//! cargo test -p sqry-db --test graph_gen_self_test
//! cargo test -p sqry-db --test graph_gen_self_test -- --include-ignored \
//!     shrink_synthetic_counter_example
//! ```
//!
//! `PROPTEST_CASES=10000 cargo test ...` matches the PR-CI default in the
//! WS1 plan (DESIGN §2.3); the daily-CI `nightly-proptest` job sets 100000.

#[path = "graph_gen.rs"]
#[allow(unused_imports)] // The self-test file is a runnable test target on its own; including the
// generator module via `#[path]` keeps it a single compilation unit so cargo
// discovers the `#[test]` functions below as part of the `graph_gen_self_test`
// target.
mod graph_gen;

use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use proptest::prelude::*;
use proptest::test_runner::{Config, TestError, TestRunner};

use graph_gen::{
    ALL_EDGE_KIND_TAGS, GeneratedGraph, check_well_formed, observed_edge_tags, sample_graphs,
    well_formed_graph,
};

/// Reads `PROPTEST_CASES` from the environment, defaulting to 1024.
///
/// PR CI sets 10000 in `.github/workflows/...`; nightly sets 100000.
fn cases_from_env() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1024)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases_from_env(),
        // The generator is deterministic for any given seed and never produces
        // ill-formed graphs by construction; shrinking is purely for diagnostic
        // surfaces if a *future* invariant is added that this generator
        // accidentally violates.
        max_shrink_iters: 10_000,
        ..ProptestConfig::default()
    })]

    /// Acceptance criterion 1 — every generated graph is well-formed.
    #[test]
    fn generated_graphs_are_well_formed(graph in well_formed_graph()) {
        prop_assert!(check_well_formed(&graph).is_ok(),
            "generator emitted ill-formed graph: {:?}", check_well_formed(&graph));
    }
}

/// Pin the DAG's literal "1000 graphs all pass" acceptance — independent of
/// `PROPTEST_CASES` overrides, so the gate cannot be disabled by environment.
#[test]
fn thousand_sample_graphs_are_well_formed() {
    let graphs = sample_graphs(1000, 0xC0DE_F00D);
    for (i, g) in graphs.iter().enumerate() {
        if let Err(e) = check_well_formed(g) {
            panic!(
                "graph #{i} of 1000 violated well-formedness: {e:?} (recipe: {:#?})",
                g.recipe
            );
        }
    }
    // Sanity floor — each generated graph must have at least one node.
    assert!(graphs.iter().all(|g| !g.recipe.nodes.is_empty()));
}

/// Acceptance criterion 3 — every `EdgeKind` discriminant is emitted across
/// a 2048-graph sample.
#[test]
fn all_edge_kinds_emitted() {
    let graphs = sample_graphs(2048, 0x5EED_5EED);
    let mut seen: BTreeSet<&'static str> = BTreeSet::new();
    for g in &graphs {
        seen.extend(observed_edge_tags(g).into_iter());
        if seen.len() == ALL_EDGE_KIND_TAGS.len() {
            break;
        }
    }
    let expected: BTreeSet<&'static str> = ALL_EDGE_KIND_TAGS.iter().copied().collect();
    let missing: Vec<&&str> = expected.difference(&seen).collect();
    assert!(
        missing.is_empty(),
        "the generator failed to emit these EdgeKind variants in 2048 samples: {missing:?}"
    );
    assert_eq!(seen.len(), ALL_EDGE_KIND_TAGS.len());
}

/// Acceptance criterion 2 — synthetic counter-example shrinks within
/// budget. The synthetic property "every generated graph has at most one
/// edge" is *intentionally* false for any non-trivial input. The shrinker
/// must drive it down to `≤ 12` nodes / `≤ 20` edges within `≤ 10 000`
/// iterations.
#[test]
fn shrink_synthetic_counter_example() {
    let config = Config {
        max_shrink_iters: 10_000,
        // Only need one failure; bound the search budget.
        cases: 32,
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);

    let strategy = well_formed_graph();

    // Drive the failure search by hand so we can inspect the shrunk witness.
    let iter_count = AtomicUsize::new(0);
    let final_witness: Mutex<Option<GeneratedGraph>> = Mutex::new(None);

    let result = runner.run(&strategy, |graph: GeneratedGraph| {
        iter_count.fetch_add(1, Ordering::Relaxed);
        // Synthetic property: assert the graph has fewer than 2 edges. By
        // construction the generator will eventually produce a graph with
        // ≥ 2 edges; that case becomes the shrink seed.
        let snapshot = graph.graph.snapshot();
        let edge_count = snapshot.iter_edges().count();
        // Record the latest failure-witness so the assertions can inspect
        // the shrunk minimum after the run.
        if edge_count >= 2 {
            *final_witness.lock().unwrap() = Some(graph.clone());
        }
        prop_assert!(
            edge_count < 2,
            "synthetic counter-example fires once edges ≥ 2 (count = {edge_count})"
        );
        Ok(())
    });

    // Expect a failure — that's the whole point of the synthetic property.
    let err = result.expect_err("synthetic property must fail so the shrinker runs");

    // Pull the minimal witness proptest reports. proptest stores the
    // shrunk value inside the TestError::Fail variant.
    let TestError::Fail(_, shrunk) = err else {
        panic!("expected TestError::Fail, got: {err:?}");
    };

    let shrunk_snapshot = shrunk.graph.snapshot();
    let node_count = shrunk_snapshot.nodes().len();
    let edge_count = shrunk_snapshot.iter_edges().count();

    // The shrunk witness must still violate the synthetic property — i.e.
    // have ≥ 2 edges — but be much smaller than the original.
    assert!(
        edge_count >= 2,
        "shrunk witness should still violate the synthetic property; \
         edge_count={edge_count}"
    );
    // Acceptance bound: ≤ 12 nodes and ≤ 20 edges.
    assert!(
        node_count <= 12,
        "shrinker should reduce nodes to ≤ 12; got {node_count}. recipe={:#?}",
        shrunk.recipe
    );
    assert!(
        edge_count <= 20,
        "shrinker should reduce edges to ≤ 20; got {edge_count}. recipe={:#?}",
        shrunk.recipe
    );
    // Iteration budget.
    let iters = iter_count.load(Ordering::Relaxed);
    assert!(
        iters <= 10_000,
        "shrinker exceeded iteration budget: {iters} > 10000"
    );

    // The shrunk witness must itself be well-formed.
    check_well_formed(&shrunk).expect("shrunk graph must remain well-formed");
}

/// Acceptance criterion 4 (U_WS1_3B_CICALL_FLAGS) — across a 100-graph
/// sample, the generator produces non-vacuous populations of both
/// `NodeFlags::ADDRESS_TAKEN` and `NodeFlags::CALLSITE_PROMISCUOUS`. Without
/// this coverage the `AddressTakenQuery` / `CallsitePromiscuousQuery`
/// baseline functions (`sqry-db/src/baseline.rs::address_taken` /
/// `::callsite_promiscuous`) would return empty on every graph and the
/// diff_cicall family becomes trivially equal — no differential signal.
///
/// The bound is "at least one graph in the sample carries each flag" — a
/// floor that protects against the regression "generator silently emits no
/// flags". At the calibrated ~15% per-node probability against the
/// 1..MAX_NODES distribution the expected hit rate per graph is well above
/// 95%; we keep the gate at >0 to stay robust against future graph-size
/// retuning.
#[test]
fn nodeflags_coverage_is_non_vacuous() {
    let graphs = sample_graphs(100, 0xF1A6_C0DE);
    let mut address_taken_graphs = 0usize;
    let mut callsite_promiscuous_graphs = 0usize;
    let mut address_taken_nodes = 0usize;
    let mut callsite_promiscuous_nodes = 0usize;
    let mut total_nodes = 0usize;
    for g in &graphs {
        let snap = g.graph.snapshot();
        let mut g_at = 0usize;
        let mut g_cp = 0usize;
        for (node_id, _) in snap.nodes().iter() {
            total_nodes += 1;
            if snap.macro_metadata().is_address_taken(node_id) {
                g_at += 1;
            }
            if snap.macro_metadata().is_callsite_promiscuous(node_id) {
                g_cp += 1;
            }
        }
        if g_at > 0 {
            address_taken_graphs += 1;
        }
        if g_cp > 0 {
            callsite_promiscuous_graphs += 1;
        }
        address_taken_nodes += g_at;
        callsite_promiscuous_nodes += g_cp;
    }
    assert!(
        address_taken_graphs > 0,
        "no graph in 100 samples carries an ADDRESS_TAKEN flag — generator regressed \
         to vacuous diff_cicall coverage; address_taken_nodes={address_taken_nodes} \
         / total_nodes={total_nodes}"
    );
    assert!(
        callsite_promiscuous_graphs > 0,
        "no graph in 100 samples carries a CALLSITE_PROMISCUOUS flag — generator \
         regressed to vacuous diff_cicall coverage; \
         callsite_promiscuous_nodes={callsite_promiscuous_nodes} \
         / total_nodes={total_nodes}"
    );
    // Stronger expectation at the calibrated rate: most graphs should
    // carry at least one of each. We assert a floor of 50/100 so transient
    // RNG variance never flakes — the calibrated mean is comfortably
    // above this floor (see `FLAG_PROB_*` rationale in graph_gen.rs).
    assert!(
        address_taken_graphs >= 50,
        "expected ≥50/100 graphs to carry ADDRESS_TAKEN at 15% per-node rate; \
         got {address_taken_graphs}"
    );
    assert!(
        callsite_promiscuous_graphs >= 50,
        "expected ≥50/100 graphs to carry CALLSITE_PROMISCUOUS at 15% per-node rate; \
         got {callsite_promiscuous_graphs}"
    );
}

/// Acceptance criterion 5 (U_WS1_3B_CICALL_FLAGS) — the shrinker can
/// reduce a counter-example so every `NodeFlags` bit it carries is
/// load-bearing. A synthetic property that fires whenever any node
/// carries `ADDRESS_TAKEN` must shrink to a witness with exactly one
/// such node (the minimum that still fails).
#[test]
fn shrinker_minimises_nodeflags() {
    use proptest::test_runner::{Config, TestError, TestRunner};

    let config = Config {
        max_shrink_iters: 10_000,
        cases: 64,
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    let strategy = well_formed_graph();
    let result = runner.run(&strategy, |graph: GeneratedGraph| {
        let snap = graph.graph.snapshot();
        let any_marked = snap
            .nodes()
            .iter()
            .any(|(id, _)| snap.macro_metadata().is_address_taken(id));
        prop_assert!(
            !any_marked,
            "synthetic property fails iff any node carries ADDRESS_TAKEN"
        );
        Ok(())
    });
    let err = result.expect_err("synthetic property must fail so the shrinker runs");
    let TestError::Fail(_, shrunk) = err else {
        panic!("expected TestError::Fail, got: {err:?}");
    };
    let snap = shrunk.graph.snapshot();
    let marked: usize = snap
        .nodes()
        .iter()
        .filter(|(id, _)| snap.macro_metadata().is_address_taken(*id))
        .count();
    assert_eq!(
        marked, 1,
        "shrinker should reduce ADDRESS_TAKEN-bearing nodes to exactly 1; \
         got {marked}. recipe={:#?}",
        shrunk.recipe
    );
    // Sanity: the shrunk witness must still be well-formed.
    check_well_formed(&shrunk).expect("shrunk graph must remain well-formed");
}

/// Smoke check that `sample_graphs` is deterministic for a given seed.
/// Differential tests rely on reproducibility for regression triage.
#[test]
fn sample_graphs_are_deterministic_for_seed() {
    let a = sample_graphs(16, 0xABCD);
    let b = sample_graphs(16, 0xABCD);
    assert_eq!(a.len(), b.len());
    for (ga, gb) in a.iter().zip(b.iter()) {
        assert_eq!(ga.recipe.files.len(), gb.recipe.files.len());
        assert_eq!(ga.recipe.nodes.len(), gb.recipe.nodes.len());
        assert_eq!(ga.recipe.edges.len(), gb.recipe.edges.len());
    }
}
