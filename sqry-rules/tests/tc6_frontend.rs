//! TC6: rule front ends (typed Rust builder + TOML rule packs).
//!
//! Per DD4 the rule layer ships both a typed Rust builder and a TOML pack
//! loader. These tests cover builder lowering, TOML schema validation,
//! malformed-input rejection (schema version, empty packs, unknown fields),
//! and a load/serialize/load semantic round-trip.

use sqry_rules::ir::{RelationEdgeKind, RuleEndpoint};
use sqry_rules::{
    RULE_PACK_SCHEMA_VERSION, RuleBuilder, RuleDefinition, RuleError, RuleNode, RulePack, RulePlan,
    load_rule_pack_str, load_rule_plan_str,
};

const ROUND_TRIP_TOML: &str = include_str!("fixtures/round_trip.toml");

#[test]
fn typed_builder_lowers_relation_rule_to_expected_ir() {
    let plan = RuleBuilder::relation_edges(
        RuleEndpoint::Nodes(Vec::new()),
        RelationEdgeKind::Callers,
        true,
    )
    .build()
    .expect("builder produces a relation plan");

    assert_eq!(
        plan,
        RulePlan::new(RuleNode::RelationEdges {
            from: RuleEndpoint::Nodes(Vec::new()),
            kind: RelationEdgeKind::Callers,
            with_metadata: true,
        })
    );
}

#[test]
fn empty_builder_is_rejected() {
    let error = RuleBuilder::new()
        .build()
        .expect_err("an empty builder is invalid rule source");
    assert!(matches!(error, RuleError::InvalidRuleSource { .. }));
}

#[test]
fn valid_toml_pack_loads() {
    let pack = load_rule_pack_str(ROUND_TRIP_TOML).expect("valid pack loads");
    assert_eq!(pack.schema_version, 1);
    assert_eq!(pack.rules.len(), 1);
    assert_eq!(pack.rules[0].id, "demo.diff");
}

#[test]
fn single_rule_pack_yields_one_plan() {
    let plan = load_rule_plan_str(ROUND_TRIP_TOML).expect("single-rule pack yields one plan");
    assert!(matches!(plan.root(), RuleNode::CrossSnapshotDiff { .. }));
}

#[test]
fn toml_pack_survives_load_serialize_load_round_trip() {
    let pack = load_rule_pack_str(ROUND_TRIP_TOML).expect("initial load");
    let serialized = toml::to_string(&pack).expect("serialize pack to TOML");
    let reloaded = load_rule_pack_str(&serialized).expect("reload serialized pack");
    assert_eq!(pack, reloaded);
}

#[test]
fn unsupported_schema_version_is_rejected() {
    // Above CURRENT (2) is rejected. schema_version 2 is now valid (L0-P3
    // metadata), so the out-of-range case uses 3.
    let too_new = ROUND_TRIP_TOML.replace("schema_version = 1", "schema_version = 3");
    assert_ne!(
        too_new, ROUND_TRIP_TOML,
        "replacement must change the source"
    );
    let error = load_rule_pack_str(&too_new).expect_err("schema version above CURRENT is rejected");
    assert!(matches!(error, RuleError::InvalidRuleSource { .. }));

    // Zero is below the supported range `1..=CURRENT` and is rejected.
    let zero = ROUND_TRIP_TOML.replace("schema_version = 1", "schema_version = 0");
    let error = load_rule_pack_str(&zero).expect_err("schema version 0 is rejected");
    assert!(matches!(error, RuleError::InvalidRuleSource { .. }));
}

#[test]
fn schema_version_2_is_accepted() {
    // CURRENT is 2. A v2 pack (optional metadata fields absent here) loads.
    let v2 = ROUND_TRIP_TOML.replace("schema_version = 1", "schema_version = 2");
    assert_ne!(v2, ROUND_TRIP_TOML, "replacement must change the source");
    load_rule_pack_str(&v2).expect("schema version 2 (CURRENT) loads");
}

#[test]
fn empty_rule_pack_is_rejected() {
    let error = load_rule_pack_str("schema_version = 1\nrules = []\n")
        .expect_err("a pack with no rules is rejected");
    assert!(matches!(error, RuleError::InvalidRuleSource { .. }));
}

#[test]
fn unknown_top_level_field_is_rejected() {
    let source = "schema_version = 1\nunexpected_field = true\n";
    let error = load_rule_pack_str(source).expect_err("deny_unknown_fields rejects extra keys");
    // Unknown-field rejection surfaces as a TOML deserialization failure
    // wrapped in the analysis-infrastructure error variant.
    assert!(matches!(error, RuleError::Analysis(_)));
}

#[test]
fn unknown_field_under_rule_is_rejected() {
    // deny_unknown_fields also guards the per-rule table: serialize a valid pack,
    // then inject an unknown key inside the `[[rules]]` entry (after its id).
    let plan = RulePlan::new(RuleNode::NodeScan {
        kind: None,
        visibility: None,
        name_pattern: None,
    });
    let pack = RulePack::new(vec![RuleDefinition::new("x", plan)]);
    let source = toml::to_string(&pack).expect("serialize valid pack");
    let tampered = source.replace("id = \"x\"", "id = \"x\"\nbogus_rule_field = true");
    assert_ne!(tampered, source, "replacement must inject a rule-level key");

    let error = load_rule_pack_str(&tampered)
        .expect_err("deny_unknown_fields rejects a rule-level unknown key");
    assert!(matches!(error, RuleError::Analysis(_)));
}

#[test]
fn rule_definition_constructor_round_trips_into_a_pack() {
    let definition = RuleDefinition::new(
        "demo.scan",
        RulePlan::new(RuleNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        }),
    );
    let pack = RulePack::new(vec![definition.clone()]);

    assert_eq!(pack.schema_version, RULE_PACK_SCHEMA_VERSION);
    assert_eq!(pack.rules, vec![definition]);
}
