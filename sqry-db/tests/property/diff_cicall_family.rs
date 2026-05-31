//! WS1 differential test family — C indirect-call precision (DAG unit
//! `U_WS1_8B_DIFF_CICALL`).
//!
//! Pins planner output against the baseline-executor oracle
//! (`sqry_db::baseline`) for the two Phase A marker-flag derived queries:
//!
//! | DerivedQuery                  | Baseline                             |
//! |-------------------------------|--------------------------------------|
//! | [`AddressTakenQuery`]         | [`baseline::address_taken`]          |
//! | [`CallsitePromiscuousQuery`]  | [`baseline::callsite_promiscuous`]   |
//!
//! Both queries are `()`-keyed (whole-snapshot scope) and return
//! `Arc<Vec<NodeId>>` sorted by `(NodeId::index, NodeId::generation)`.
//!
//! # Origin
//!
//! The two queries land with Phase A of the C indirect-call precision
//! workstream (DESIGN §9.2 / §9.3 of
//! `docs/development/c-semantic-phase-a-icall-precision/`). They surface
//! the `NodeFlags::ADDRESS_TAKEN` and `NodeFlags::CALLSITE_PROMISCUOUS`
//! bits owned by the `NodeMetadataStore` (Tier-3 metadata revision) and
//! materialised by `pass5b_c_indirect` during graph commit.
//!
//! Non-vacuous differential coverage depends on the WS1 graph generator
//! (DAG unit `U_WS1_3B`, `tests/property/graph_gen.rs`) emitting both
//! flags at `FLAG_PROB_ADDRESS_TAKEN` / `FLAG_PROB_CALLSITE_PROMISCUOUS`
//! ≈ 0.15 each per node. The companion self-test
//! `nodeflags_coverage_is_non_vacuous` in `graph_gen_self_test.rs`
//! guards against generator regressions that would silently empty this
//! family — without it, both differentials would compare empty-vs-empty
//! and find no bugs.
//!
//! # Tier coverage
//!
//! Both queries declare `TRACKS_METADATA_REVISION = true` (Tier 3) **and**
//! `TRACKS_EDGE_REVISION = true` (Tier 2 — set conservatively because
//! pass5b applies marks in the same commit pass that adds new `Calls`
//! edges; see `address_taken.rs:14-23` and `callsite_promiscuous.rs:14-22`
//! for the rationale). Both sides of the differential read the same
//! `snapshot.macro_metadata()` accessor backed by the
//! `NodeMetadataStore`, so any drift in how the planner observes the
//! Tier-3 flag bits surfaces as a property failure.
//!
//! Cross-snapshot cache invalidation belongs to the WS1.5 cache
//! suite — this family validates single-snapshot observation parity.
//!
//! # Running
//!
//! ```text
//! # PR-tier (default 1024 cases, scalable via PROPTEST_CASES):
//! cargo test -p sqry-db --features baseline --test diff_cicall_family
//!
//! # Nightly-tier (10k cases, release profile):
//! PROPTEST_CASES=10000 cargo test -p sqry-db --features baseline \
//!     --test diff_cicall_family --release
//! ```

// The graph generator + invariant checker live in `tests/property/graph_gen.rs`.
// `#[path]` inclusion keeps this test target a single compilation unit so cargo
// discovers the `#[test]` functions below.
#[path = "graph_gen.rs"]
#[allow(unused_imports)] // Generator module re-exports helpers other family files use.
mod graph_gen;

use std::sync::Arc;

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

use sqry_core::graph::unified::concurrent::GraphSnapshot;

use sqry_db::QueryDb;
use sqry_db::baseline;
use sqry_db::config::QueryDbConfig;
use sqry_db::queries::{AddressTakenQuery, CallsitePromiscuousQuery};

use graph_gen::{GeneratedGraph, well_formed_graph};

// ---------------------------------------------------------------------------
// Proptest tuning
// ---------------------------------------------------------------------------

/// Reads `PROPTEST_CASES` from the environment, defaulting to 1024 for the
/// PR-tier `cargo test` invocation. Nightly CI sets 10000 (DESIGN §2.3).
/// Matches the convention established by `diff_unused_family.rs`.
fn cases_from_env() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(1024)
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

// ---------------------------------------------------------------------------
// AddressTakenQuery — set differential
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases_from_env(),
        // Shrinker budget mirrors the WS1 generator self-test (DAG
        // acceptance for U_WS1_3_GRAPH_GEN: ≤ 10 000 iterations).
        max_shrink_iters: 10_000,
        ..ProptestConfig::default()
    })]

    /// `AddressTakenQuery::execute` and `baseline::address_taken` must
    /// produce byte-identical `Vec<NodeId>` for every well-formed graph.
    ///
    /// Both implementations:
    ///
    /// 1. Walk `snapshot.nodes().iter()` in arena (= index) order.
    /// 2. Skip Phase 4c-prime unified losers (`NodeEntry::is_unified_loser`).
    /// 3. Probe `snapshot.macro_metadata().is_address_taken(node_id)`.
    /// 4. Sort by `(NodeId::index, NodeId::generation)` and return.
    ///
    /// Any drift between the two surfaces here as a property failure with a
    /// shrunk graph recipe.
    #[test]
    fn address_taken_planner_equals_baseline(graph in well_formed_graph()) {
        let (db, snapshot) = build_db(&graph);
        let planner = db.get::<AddressTakenQuery>(&());
        let baseline_out = baseline::address_taken(&snapshot);
        prop_assert_eq!(
            planner.as_ref().as_slice(),
            baseline_out.as_slice(),
            "AddressTakenQuery diverged from baseline::address_taken\n  nodes = {}\n  edges = {}",
            graph.recipe.nodes.len(),
            graph.recipe.edges.len()
        );
    }
}

// ---------------------------------------------------------------------------
// CallsitePromiscuousQuery — set differential
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: cases_from_env(),
        max_shrink_iters: 10_000,
        ..ProptestConfig::default()
    })]

    /// `CallsitePromiscuousQuery::execute` and
    /// `baseline::callsite_promiscuous` must produce byte-identical
    /// `Vec<NodeId>` for every well-formed graph. Same observation
    /// contract as `AddressTakenQuery` above, with the flag probe
    /// swapped for `is_callsite_promiscuous`.
    #[test]
    fn callsite_promiscuous_planner_equals_baseline(graph in well_formed_graph()) {
        let (db, snapshot) = build_db(&graph);
        let planner = db.get::<CallsitePromiscuousQuery>(&());
        let baseline_out = baseline::callsite_promiscuous(&snapshot);
        prop_assert_eq!(
            planner.as_ref().as_slice(),
            baseline_out.as_slice(),
            "CallsitePromiscuousQuery diverged from baseline::callsite_promiscuous\n  nodes = {}\n  edges = {}",
            graph.recipe.nodes.len(),
            graph.recipe.edges.len()
        );
    }
}

// ---------------------------------------------------------------------------
// Anti-flake pin — deterministic 64-graph sample
// ---------------------------------------------------------------------------
//
// `PROPTEST_CASES` is environment-driven. Pin a small fixed-seed sample so
// the gate cannot be silently disabled by setting `PROPTEST_CASES=0` in CI,
// mirroring `diff_unused_family::fixed_seed_64_graphs_planner_matches_baseline`.
// 64 graphs is large enough that, with the generator emitting both flags at
// ~15% per node, both flag populations are non-empty across the sample with
// overwhelming probability.

/// Deterministically sample `count` graphs using a fixed RNG seed —
/// mirrors `diff_unused_family::sampled_graphs`.
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
fn fixed_seed_64_graphs_planner_matches_baseline_address_taken() {
    let graphs = sampled_graphs(64, 0xD1FF_0008_B000_0000);
    let mut graphs_with_marks = 0usize;
    let mut total_marked_nodes = 0usize;
    for graph in &graphs {
        let snapshot = Arc::new(graph.graph.snapshot());
        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        let planner = db.get::<AddressTakenQuery>(&());
        let baseline_out = baseline::address_taken(&snapshot);
        assert_eq!(
            planner.as_ref().as_slice(),
            baseline_out.as_slice(),
            "fixed-seed address-taken differential failed: nodes={}, edges={}",
            graph.recipe.nodes.len(),
            graph.recipe.edges.len(),
        );
        if !planner.is_empty() {
            graphs_with_marks += 1;
            total_marked_nodes += planner.len();
        }
    }
    // Non-vacuity guard. With FLAG_PROB_ADDRESS_TAKEN = 0.15 per node and
    // typical graph size > 0, the probability of zero ADDRESS_TAKEN marks
    // across 64 graphs is vanishingly small (the U_WS1_3B coverage assertion
    // observes 92/100 in practice). If this trips, the generator regressed.
    assert!(
        graphs_with_marks > 0,
        "vacuous coverage: 0/64 fixed-seed graphs carried ADDRESS_TAKEN — \
         generator regression (expected ≈92/100 per U_WS1_3B coverage); \
         total_marked_nodes={total_marked_nodes}"
    );
}

#[test]
fn fixed_seed_64_graphs_planner_matches_baseline_callsite_promiscuous() {
    let graphs = sampled_graphs(64, 0xD1FF_0008_B000_0001);
    let mut graphs_with_marks = 0usize;
    let mut total_marked_nodes = 0usize;
    for graph in &graphs {
        let snapshot = Arc::new(graph.graph.snapshot());
        let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        let planner = db.get::<CallsitePromiscuousQuery>(&());
        let baseline_out = baseline::callsite_promiscuous(&snapshot);
        assert_eq!(
            planner.as_ref().as_slice(),
            baseline_out.as_slice(),
            "fixed-seed callsite-promiscuous differential failed: nodes={}, edges={}",
            graph.recipe.nodes.len(),
            graph.recipe.edges.len(),
        );
        if !planner.is_empty() {
            graphs_with_marks += 1;
            total_marked_nodes += planner.len();
        }
    }
    // Non-vacuity guard — mirrors the ADDRESS_TAKEN check above; expected
    // ≈90/100 per U_WS1_3B coverage.
    assert!(
        graphs_with_marks > 0,
        "vacuous coverage: 0/64 fixed-seed graphs carried CALLSITE_PROMISCUOUS — \
         generator regression (expected ≈90/100 per U_WS1_3B coverage); \
         total_marked_nodes={total_marked_nodes}"
    );
}
