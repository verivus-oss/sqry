//! TC7: CLI / MCP surface contract owned by `sqry-rules`.
//!
//! The `sqry rules run` CLI command and the MCP `rules_run` tool both wrap the
//! same `sqry-rules` contract: resolve a shipped pack or rule, run the
//! non-beside-cache rules through `RuleEngine` + `SqryDbRuleBackend`, serialize
//! the witness-bearing `RuleRun` for transport, and report beside-cache rules
//! as unsupported. `sqry-rules` cannot depend on the CLI / MCP crates, so this
//! file pins that shared contract. The end-to-end JSON transport itself is
//! covered in `sqry-cli/src/commands/rules/tests.rs` and
//! `sqry-mcp/src/execution/tools/rules/tests.rs`.

mod common;

use sqry_rules::rules::{ShippedRule, intake, recipes, security};
use sqry_rules::{RuleEngine, RuleRun, RuleStep, SqryDbRuleBackend, shipped_rules};

#[test]
fn shipped_pack_selectors_resolve_to_expected_rule_sets() {
    assert_eq!(recipes::bbnty_recipe_rules().len(), 7);
    assert_eq!(intake::standard_intake_rules().len(), 5);
    assert_eq!(security::security_rules().len(), 1);
    assert_eq!(shipped_rules().len(), 13);
}

#[test]
fn non_beside_cache_rules_emit_serializable_witness_bearing_runs() {
    let fixture = common::two_node_call_fixture();
    let db = common::query_db_for(fixture.snapshot);
    let backend = SqryDbRuleBackend::new(&db);
    let engine = RuleEngine::new();

    let mut executed = 0_usize;
    for rule in shipped_rules() {
        if common::requires_beside_cache(&rule) {
            continue;
        }

        let run = engine
            .run(&backend, &rule.definition.plan)
            .unwrap_or_else(|error| panic!("{} should run: {error}", rule.id()));
        assert!(
            run.witness
                .steps
                .iter()
                .any(|step| matches!(step, RuleStep::RuleFired { .. })),
            "{} must emit a RuleFired witness step",
            rule.id()
        );

        // This is exactly the payload the CLI / MCP serialize for transport.
        let json = serde_json::to_string(&run).expect("RuleRun serializes for transport");
        let decoded: RuleRun = serde_json::from_str(&json).expect("RuleRun round-trips from JSON");
        assert_eq!(decoded, run);
        executed += 1;
    }

    assert!(executed > 0, "at least one non-beside-cache rule executes");
}

#[test]
fn beside_cache_rules_are_detected_so_surfaces_report_unsupported() {
    let beside_cache_rules: Vec<ShippedRule> = shipped_rules()
        .into_iter()
        .filter(common::requires_beside_cache)
        .collect();

    assert!(
        !beside_cache_rules.is_empty(),
        "the shipped catalog includes beside-cache rules"
    );
    for rule in &beside_cache_rules {
        assert!(
            common::contains_beside_cache_route(rule.definition.plan.root()),
            "{} is flagged beside-cache and must route through a beside-cache primitive",
            rule.id()
        );
    }
}
