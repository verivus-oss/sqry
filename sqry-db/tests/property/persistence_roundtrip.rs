//! WS1 persistence round-trip property test.
//!
//! Implements DAG unit `U_WS1_12_PERSIST_RT` of the
//! `graph-fidelity-planner-correctness` plan (DESIGN §2.7 of
//! `docs/development/graph-fidelity-planner-correctness/02_DESIGN-graph-fidelity-planner-correctness.md`).
//!
//! # Invariant
//!
//! For every well-formed `CodeGraph` produced by the WS1 generator
//! (`property::graph_gen::well_formed_graph`, DESIGN §2.2 / `U_WS1_3`):
//!
//! ```text
//!     before = canonical_arena(&graph)
//!     bytes  = save_to_path(&graph)
//!     g'     = load_from_path(bytes)
//!     after  = canonical_arena(&g')
//!     ⇒ before == after
//! ```
//!
//! Equality is normalised by [`sqry_core::graph::unified::test_helpers::
//! canonical_arena`] — i.e. permutation of the `StringInterner`'s table or
//! of arena slot indices does not break the comparison; any genuine
//! semantic divergence (a dropped node, a flipped flag bit, a re-pointed
//! edge, a renamed symbol) does.
//!
//! # V11 scope (Phase α)
//!
//! The persistence layer's current on-disk magic is `SQRY_GRAPH_V11`. The
//! V12 schema lands with Plan B's `U_WS2_2_V12_SCHEMA` in Phase β; until
//! then this test exercises V11 directly. The V11 → V12 upconvert
//! round-trip is **deferred** to Phase β per the orchestrator brief:
//! "for Phase α run it against V11 and rebase on V12 in Phase β
//! integration."
//!
//! # Acceptance criteria (DAG verbatim)
//!
//! * Round-trip identity on 10 000 PR / 100 000 nightly cases.
//! * `canonical_arena()` normalises interner permutation.
//! * V11-to-V12 upconvert round-trip — **deferred** (see above).
//!
//! Default case count is 256 (matches the other WS1 property tests). PR
//! CI sets `PROPTEST_CASES=10000`, nightly sets `100000`.

#![allow(clippy::needless_pass_by_value)]

use std::sync::Arc;

use proptest::prelude::*;
use proptest::test_runner::Config;
use tempfile::TempDir;

use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::persistence::{load_from_path, save_to_path};
use sqry_core::graph::unified::test_helpers::{CanonicalArena, canonical_arena};

#[path = "graph_gen.rs"]
#[allow(unused_imports)]
mod graph_gen;

use graph_gen::{GeneratedGraph, well_formed_graph};

// ---------------------------------------------------------------------------
// Config — env-tunable case count matching the WS1 differential family
// ---------------------------------------------------------------------------

/// Reads `PROPTEST_CASES` from the environment, defaulting to 256.
///
/// Default is conservative so `cargo test -p sqry-db --features
/// persistence-roundtrip` stays fast on developer machines. The PR-tier
/// `PROPTEST_CASES=10000` and nightly `PROPTEST_CASES=100000` settings
/// drive the DAG's acceptance budget.
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
// Round-trip helpers
// ---------------------------------------------------------------------------

/// Saves the graph to a temp directory, loads it back, and returns the
/// reloaded `CodeGraph`. The `TempDir` is held until the function returns
/// so the snapshot file survives the load.
fn save_and_reload(graph: &CodeGraph) -> (TempDir, CodeGraph) {
    let tempdir = TempDir::new().expect("create tempdir");
    let path = tempdir.path().join("snapshot.sqry");
    save_to_path(graph, &path).expect("save_to_path V11");
    let reloaded = load_from_path(&path, None).expect("load_from_path V11");
    (tempdir, reloaded)
}

/// Runs the round-trip for one `GeneratedGraph` and returns the (before,
/// after) canonical-arena pair. Separated from the proptest body so the
/// diagnostic message can print recipe details on failure.
fn round_trip_arenas(graph: &GeneratedGraph) -> (CanonicalArena, CanonicalArena) {
    let before = canonical_arena(&graph.graph);
    let (_tempdir, reloaded) = save_and_reload(&graph.graph);
    let after = canonical_arena(&reloaded);
    (before, after)
}

// ---------------------------------------------------------------------------
// Property bodies
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest_config())]

    /// V11 round-trip identity on arbitrary well-formed graphs.
    ///
    /// This is the headline acceptance criterion from the DAG: every
    /// graph produced by `well_formed_graph()` must survive a V11 save →
    /// load cycle without any observable semantic change.
    #[test]
    fn snapshot_roundtrip_arena_canonical(graph in well_formed_graph()) {
        let (before, after) = round_trip_arenas(&graph);
        prop_assert_eq!(
            before,
            after,
            "V11 save/load lost or mutated semantic data. recipe={recipe:#?}",
            recipe = graph.recipe,
        );
    }

    /// Idempotency check: a *second* round-trip starting from the
    /// already-reloaded graph must produce the same canonical arena.
    ///
    /// Catches a class of bugs where the load path silently normalises in
    /// a way the save path does not (so the first round-trip looks
    /// stable, but the on-disk representation drifts on every cycle).
    #[test]
    fn snapshot_roundtrip_is_idempotent(graph in well_formed_graph()) {
        let before = canonical_arena(&graph.graph);
        let (_t1, reloaded_once) = save_and_reload(&graph.graph);
        let mid = canonical_arena(&reloaded_once);
        let (_t2, reloaded_twice) = save_and_reload(&reloaded_once);
        let after = canonical_arena(&reloaded_twice);
        prop_assert_eq!(&before, &mid,
            "first round-trip diverged. recipe={recipe:#?}",
            recipe = graph.recipe,
        );
        prop_assert_eq!(&mid, &after,
            "second round-trip diverged. recipe={recipe:#?}",
            recipe = graph.recipe,
        );
    }

    /// The canonical-arena value is stable under repeated calls on the
    /// same `CodeGraph` — i.e. `canonical_arena` is deterministic and
    /// purely a function of the graph's semantic state.
    ///
    /// Without this guard a non-deterministic interner traversal would
    /// flake the headline property at random; pinning it here keeps the
    /// helper itself out of the search space for any test failure.
    #[test]
    fn canonical_arena_is_deterministic(graph in well_formed_graph()) {
        let a = canonical_arena(&graph.graph);
        let b = canonical_arena(&graph.graph);
        prop_assert_eq!(a, b);
    }
}

// ---------------------------------------------------------------------------
// Smoke tests — fixed inputs so a generator regression cannot mask
// a persistence-layer regression and vice versa.
// ---------------------------------------------------------------------------

#[test]
fn empty_graph_roundtrips() {
    let graph = Arc::new(CodeGraph::new());
    let before = canonical_arena(&graph);
    let (_tempdir, reloaded) = save_and_reload(&graph);
    let after = canonical_arena(&reloaded);
    assert_eq!(before, after);
}
