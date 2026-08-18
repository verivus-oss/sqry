//! TC8: proof-rule suite wall-clock budget (FR6 / NFR4).
//!
//! Each shipped proof recipe (PR-R1..PR-R7) plus the standard intake pack must
//! execute within 2x the wall-clock of the equivalent hand-authored ad-hoc
//! `sqry-db` composition (the reconciled NFR4 bound for rules that exercise the
//! new IR variants). The bbnty-audit baselines referenced in 01_SPEC.md are
//! external and not reproducible in CI, so this test measures the local,
//! reproducible analog: for each executable rule it times the rule engine and
//! `common::hand_compose` (the same `RuleBackend` primitive calls without the
//! witness / `RuleOutput` envelopes) over many iterations on a non-trivial
//! ~114-node fixture, and asserts engine_time <= 2 * hand_time.
//!
//! Since L2a, SimilarTo rules (PR-R7 and the duplicates intake rule) run
//! in-engine via the structural-neighbour primitive, so they ARE wall-clock
//! compared here (the hand baseline exercises the same `structural_neighbors`
//! call). Only the cross-snapshot rules (PR-R3, PR-R4) remain exempt: their
//! `CrossSnapshotDiff` path needs a prior snapshot the single-snapshot engine
//! cannot source yet, so they are budgeted by confirming they route through a
//! beside-cache primitive. Measured ratios are recorded in 06_TEST_EXECUTION.md.

mod common;

use std::time::{Duration, Instant};

use sqry_rules::{RuleEngine, SqryDbRuleBackend, shipped_rules};

/// Iterations per measurement: enough that the aggregate is comfortably above
/// scheduler noise on shared CI hosts, so the 2x ratio (not an absolute floor)
/// is the operative gate.
const ITERATIONS: u32 = 64;

#[test]
fn executable_proof_rules_run_within_two_x_hand_composition() {
    let snapshot = common::analysis_fixture();
    let db = common::query_db_for(snapshot);
    let backend = SqryDbRuleBackend::new(&db);
    let engine = RuleEngine::new();

    let mut executed = 0_usize;
    for rule in shipped_rules() {
        if common::requires_beside_cache(&rule) {
            assert!(
                common::contains_beside_cache_route(rule.definition.plan.root()),
                "{} is beside-cache but has no beside-cache route",
                rule.id()
            );
            continue;
        }

        let plan = &rule.definition.plan;

        // Warm both paths so the comparison measures steady-state work
        // (built-in relation facts are cached after the first touch).
        engine
            .run(&backend, plan)
            .unwrap_or_else(|error| panic!("{} should execute: {error}", rule.id()));
        let _ = common::hand_compose(&backend, plan.root());

        let rule_start = Instant::now();
        for _ in 0..ITERATIONS {
            engine
                .run(&backend, plan)
                .unwrap_or_else(|error| panic!("{} should execute: {error}", rule.id()));
        }
        let rule_time = rule_start.elapsed();

        let hand_start = Instant::now();
        for _ in 0..ITERATIONS {
            let _ = common::hand_compose(&backend, plan.root());
        }
        let hand_time = hand_start.elapsed();

        // 2x of the hand composition, plus a 2ms aggregate epsilon to absorb
        // measurement jitter when both timings are tiny. The 2x ratio is the
        // operative gate once timings are above noise.
        let budget = hand_time.saturating_mul(2) + Duration::from_millis(2);
        assert!(
            rule_time <= budget,
            "{} engine {rule_time:?} exceeded 2x hand-composition {hand_time:?} (budget {budget:?})",
            rule.id()
        );
        executed += 1;
    }

    assert!(
        executed >= 4,
        "the executable recipe + intake rules are wall-clock compared"
    );
}
