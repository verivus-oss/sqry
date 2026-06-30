//! TC5: default `sqry-db` backend integration and cache invalidation.
//!
//! Runs rule primitives against a populated fixture through the production
//! `SqryDbRuleBackend`, asserts the results match the hand-verified graph
//! facts, and proves Tier-1 file-revision bumps invalidate a registered
//! cacheable rule query.

mod common;

use std::sync::Arc;

use sqry_db::planner::StringPattern;

use sqry_rules::ir::{RelationEdgeKind, RuleEndpoint};
use sqry_rules::{
    RelationEdgesRuleQuery, RelationEdgesRuleQueryKey, RuleEngine, RuleNode, RuleOutput, RulePlan,
    RuleQueryOutcome, RuleRun, SqryDbRuleBackend, register_rule_queries,
};

#[test]
fn relation_rule_matches_hand_verified_call_facts() {
    let fixture = common::two_node_call_fixture();
    let db = common::query_db_for(Arc::clone(&fixture.snapshot));
    let backend = SqryDbRuleBackend::new(&db);

    let plan = RulePlan::new(RuleNode::RelationEdges {
        from: RuleEndpoint::Query(Box::new(RuleNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: Some(StringPattern::exact("main")),
        })),
        kind: RelationEdgeKind::Callees,
        with_metadata: false,
    });

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("relation rule runs against the default backend");

    match run.output {
        RuleOutput::Relations(rows) => {
            assert_eq!(
                rows.nodes,
                vec![fixture.helper],
                "callees of main is exactly helper"
            );
        }
        other => panic!("expected relation rows, got {other:?}"),
    }
}

#[test]
fn node_scan_rule_returns_every_node_in_the_fixture() {
    let fixture = common::two_node_call_fixture();
    let db = common::query_db_for(Arc::clone(&fixture.snapshot));
    let backend = SqryDbRuleBackend::new(&db);

    let plan = RulePlan::new(RuleNode::NodeScan {
        kind: None,
        visibility: None,
        name_pattern: None,
    });

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("node scan runs");

    match run.output {
        RuleOutput::Nodes(mut nodes) => {
            nodes.sort_by_key(|node| node.index());
            assert_eq!(nodes, vec![fixture.main, fixture.helper]);
        }
        other => panic!("expected node set, got {other:?}"),
    }
}

#[test]
fn tier_one_file_revision_bump_invalidates_cacheable_rule_query() {
    let fixture = common::two_node_call_fixture();
    let mut db = common::query_db_for(Arc::clone(&fixture.snapshot));
    register_rule_queries(&mut db);

    let key = RelationEdgesRuleQueryKey {
        from: RuleEndpoint::Query(Box::new(RuleNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: Some(StringPattern::exact("main")),
        })),
        kind: RelationEdgeKind::Callees,
        with_metadata: false,
    };

    let first = db.get::<RelationEdgesRuleQuery>(&key);
    assert!(matches!(
        first,
        RuleQueryOutcome::Ok(RuleRun {
            output: RuleOutput::Relations(ref rows),
            ..
        }) if rows.nodes == vec![fixture.helper]
    ));

    let baseline = db.metrics();
    let _ = db.get::<RelationEdgesRuleQuery>(&key);
    let warm = db.metrics();
    assert_eq!(warm.cache_hits, baseline.cache_hits + 1);
    assert_eq!(warm.cache_misses, baseline.cache_misses);

    db.inputs_mut()
        .get_mut(fixture.file)
        .expect("fixture file input is tracked")
        .update(Default::default());

    let _ = db.get::<RelationEdgesRuleQuery>(&key);
    let after_bump = db.metrics();
    assert!(
        after_bump.cache_misses > warm.cache_misses,
        "a Tier-1 file revision bump must invalidate the cacheable rule query"
    );
}
