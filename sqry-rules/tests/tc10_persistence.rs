//! TC10: derived-cache persistence for rule execution (DD6).
//!
//! Rule primitives register as `DerivedQuery` implementations, so this test
//! exercises the SHA-256-gated, magic-prefixed `derived.sqry` format end to
//! end: a cacheable rule query is run, the cache is saved, and a fresh `QueryDb`
//! loads it cold-start without accepting reserved-range rule memo entries as
//! stable persisted ABI.
//!
//! Note on scope: `sqry-db`'s `load_derived` treats query-type IDs in the
//! reserved `0x1000..` range (which the rule-memo queries occupy) as
//! forward-compat skips. Node-id anchored relation rules execute against the
//! graph snapshot directly, so this path does not depend on a separate
//! name-keyed built-in relation cache rehydrating cold-start.

mod common;

use std::sync::Arc;

use sqry_db::persistence::{LoadError, LoadOutcome, load_derived, save_derived};
use sqry_db::planner::StringPattern;
use tempfile::TempDir;

use sqry_rules::ir::{RelationEdgeKind, RuleEndpoint};
use sqry_rules::{
    RelationEdgesRuleQuery, RelationEdgesRuleQueryKey, RuleNode, RuleOutput, RuleQueryOutcome,
    register_rule_queries,
};

fn callees_of_main_key() -> RelationEdgesRuleQueryKey {
    RelationEdgesRuleQueryKey {
        from: RuleEndpoint::Query(Box::new(RuleNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: Some(StringPattern::exact("main")),
        })),
        kind: RelationEdgeKind::Callees,
        with_metadata: false,
    }
}

#[test]
fn rule_derived_cache_round_trips_through_sha_gated_format() {
    let fixture = common::two_node_call_fixture();
    let mut warm = common::query_db_for(Arc::clone(&fixture.snapshot));
    register_rule_queries(&mut warm);

    // Running the cacheable rule warms its rule-memo entry. The direct
    // node-id relation execution path intentionally does not create a
    // name-keyed built-in relation cache dependency.
    let outcome = warm.get::<RelationEdgesRuleQuery>(&callees_of_main_key());
    assert!(matches!(outcome, RuleQueryOutcome::Ok(_)));

    let dir = TempDir::new().expect("temp workspace");
    let path = dir.path().join("derived.sqry");
    let snapshot_sha = [0x5Au8; 32];

    save_derived(&warm, snapshot_sha, &path, dir.path()).expect("derived cache writes atomically");
    assert!(path.exists(), "save_derived produced a derived.sqry file");

    let mut cold = common::query_db_for(Arc::clone(&fixture.snapshot));
    register_rule_queries(&mut cold);
    let loaded =
        load_derived(&mut cold, snapshot_sha, &path, dir.path()).expect("derived cache rehydrates");

    match loaded {
        LoadOutcome::Applied { entries } => assert_eq!(
            entries, 0,
            "reserved rule-memo entries are skipped and no built-in relation cache is required"
        ),
        LoadOutcome::Skipped(_) => panic!("a SHA-matched derived cache must not be skipped"),
    }

    // The rule-memo entry (query-type id 0x1002, in sqry-db's forward-compat
    // reserved range) is skipped by load_derived, so the rule query recomputes
    // against the graph snapshot and still returns the correct relation edge.
    let before_rule = cold.metrics();
    let outcome = cold.get::<RelationEdgesRuleQuery>(&callees_of_main_key());
    let after_rule = cold.metrics();
    match outcome {
        RuleQueryOutcome::Ok(run) => {
            match run.output {
                RuleOutput::Relations(rows) => {
                    assert_eq!(rows.nodes, vec![fixture.helper]);
                }
                other => panic!("expected relation rows after recompute, got {other:?}"),
            }
            assert!(run.witness.steps.iter().any(|step| {
                matches!(
                    step,
                    sqry_rules::witness::RuleStep::RelationEdgeEmitted { from, to, .. }
                        if *from == fixture.main && *to == fixture.helper
                )
            }));
        }
        other => panic!("expected relation rows after recompute, got {other:?}"),
    }
    assert!(
        after_rule.cache_misses > before_rule.cache_misses,
        "the rule-memo entry was not rehydrated (reserved 0x1000.. range)"
    );
}

#[test]
fn stale_snapshot_sha_discards_rule_derived_cache() {
    let fixture = common::two_node_call_fixture();
    let mut warm = common::query_db_for(Arc::clone(&fixture.snapshot));
    register_rule_queries(&mut warm);
    let _ = warm.get::<RelationEdgesRuleQuery>(&callees_of_main_key());

    let dir = TempDir::new().expect("temp workspace");
    let path = dir.path().join("derived.sqry");
    let saved_sha = [0x11u8; 32];
    let mismatched_sha = [0x22u8; 32];

    save_derived(&warm, saved_sha, &path, dir.path()).expect("derived cache writes");

    let mut cold = common::query_db_for(Arc::clone(&fixture.snapshot));
    let error = load_derived(&mut cold, mismatched_sha, &path, dir.path())
        .expect_err("a SHA mismatch must be rejected");

    assert!(matches!(error, LoadError::StaleSnapshot));
}
