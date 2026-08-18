//! TC1: `sqry-rules` crate isolation and public API surface.
//!
//! These integration tests link only against the published `sqry-rules`
//! surface plus the public `sqry-core` / `sqry-db` types it deliberately
//! re-exports or accepts at its adapter boundary. They fail to compile if a
//! documented public re-export is dropped, which makes them the crate's
//! API-surface snapshot. The production-source "no private core module"
//! guard lives in `src/lib.rs`; this file proves the positive surface is
//! sufficient to drive a rule end to end.

mod common;

use sqry_core::graph::unified::NodeKind;
use sqry_db::planner::{Predicate, StringPattern};
use sqry_rules::{
    EdgeClassification, RuleBuilder, RuleEngine, RuleNode, RuleOutput, RulePlan, RuleStep,
    SqryDbRuleBackend, shipped_rules,
};

#[test]
fn public_re_exports_drive_a_rule_end_to_end() {
    let plan: RulePlan = RuleBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::MatchesName(StringPattern::contains("main")))
        .build()
        .expect("public builder surface produces a plan");

    let db = common::empty_query_db();
    let backend = SqryDbRuleBackend::new(&db);
    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("engine runs entirely through the public surface");

    assert!(matches!(run.output, RuleOutput::Nodes(_)));
    assert!(
        run.witness
            .steps
            .iter()
            .any(|step| matches!(step, RuleStep::RuleFired { .. })),
        "a completed run emits a terminal RuleFired witness step"
    );
}

#[test]
fn rule_layer_exposes_classification_not_storage_discriminants() {
    // `EdgeClassification` is the single graph-layer type the rule crate
    // intentionally re-exports; rule IR speaks `RuleEdgeClass`, never the
    // storage `EdgeKind` discriminant. These bounds are the compile-time
    // snapshot of the rule-facing public types.
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<EdgeClassification>();
    assert_send_sync::<RuleNode>();
    assert_send_sync::<RulePlan>();
}

#[test]
fn shipped_rule_catalog_is_reachable_through_public_surface() {
    let rules = shipped_rules();

    assert_eq!(
        rules.len(),
        13,
        "seven bbnty recipes, five standard intake rules, and one security detector are published"
    );
    assert!(rules.iter().all(|rule| !rule.id().is_empty()));
}
