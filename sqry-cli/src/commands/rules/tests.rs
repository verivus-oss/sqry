use clap::Parser;
use sqry_core::graph::unified::NodeId;
use sqry_rules::ir::{RuleEndpoint, RuleNode, RulePlan, RuleSimilarityKind};

use crate::large_stack_test;

use super::*;

#[test]
fn shipped_recipe_pack_loads_all_recipe_rules() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let loaded = load_rules("bbnty.recipes", workspace.path()).expect("recipe pack should load");

    assert_eq!(loaded.source, "bbnty.recipes");
    assert_eq!(loaded.rules.len(), 7);
    assert!(
        loaded
            .rules
            .iter()
            .any(|rule| rule.definition.id == "bbnty.pr_r5.trust_boundary_audit")
    );
}

#[test]
fn shipped_single_rule_loads_by_stable_id() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let loaded = load_rules("bbnty.pr_r5.trust_boundary_audit", workspace.path())
        .expect("single shipped rule should load");

    assert_eq!(loaded.rules.len(), 1);
    assert_eq!(
        loaded.rules[0].definition.id,
        "bbnty.pr_r5.trust_boundary_audit"
    );
}

large_stack_test! {
#[test]
fn output_format_rejects_conflicting_global_json_and_text_format() {
    let cli = Cli::parse_from(["sqry", "--json", "rules", "run", "bbnty.recipes"]);

    let err = resolve_rules_output_format(&cli, RulesOutputFormat::Text)
        .expect_err("global JSON plus text format should be rejected");

    assert!(err.to_string().contains("--json conflicts"));
}
}

large_stack_test! {
#[test]
fn output_format_accepts_global_json_with_json_format() {
    let cli = Cli::parse_from([
        "sqry",
        "--json",
        "rules",
        "run",
        "bbnty.recipes",
        "--format",
        "json",
    ]);

    let format =
        resolve_rules_output_format(&cli, RulesOutputFormat::Json).expect("JSON should resolve");

    assert_eq!(format, RulesOutputFormat::Json);
}
}

#[test]
fn beside_cache_detection_finds_nested_similarity_rules() {
    let nested = RuleNode::PathQuery {
        from: RuleEndpoint::Query(Box::new(RuleNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        })),
        to: RuleEndpoint::Query(Box::new(RuleNode::SimilarTo {
            seed: RuleEndpoint::Nodes(vec![NodeId::new(1, 1)]),
            scope: None,
            similarity_kind: RuleSimilarityKind::Duplicate,
        })),
        kind: sqry_rules::ir::PathKind::Calls,
        max_depth: 3,
        max_paths: Some(8),
    };

    assert!(contains_beside_cache_route(&nested));
}

#[test]
fn toml_pack_loads_from_fixture_path() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace parent")
        .join("sqry-rules/tests/fixtures/trust-boundary.toml");

    let loaded = load_rules(
        fixture
            .to_str()
            .expect("fixture path should be valid UTF-8 for test"),
        fixture.parent().expect("fixture parent"),
    )
    .expect("fixture TOML rule pack should load");

    assert_eq!(loaded.rules.len(), 1);
    assert_eq!(
        loaded.rules[0].definition.id,
        "bbnty.pr_r5.trust_boundary_audit"
    );
}

#[test]
fn toml_pack_loads_relative_to_workspace_path() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace parent")
        .join("sqry-rules/tests/fixtures/trust-boundary.toml");
    let workspace_pack = workspace.path().join("trust-boundary.toml");
    std::fs::copy(&fixture, &workspace_pack).expect("copy fixture into workspace");

    let loaded = load_rules("trust-boundary.toml", workspace.path())
        .expect("workspace-relative TOML rule pack should load");

    assert_eq!(loaded.rules.len(), 1);
    assert_eq!(
        loaded.rules[0].definition.id,
        "bbnty.pr_r5.trust_boundary_audit"
    );
}

#[test]
fn toml_pack_rejects_workspace_escape() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::NamedTempFile::new().expect("outside fixture");

    let error = load_rules(
        outside
            .path()
            .to_str()
            .expect("outside path should be UTF-8"),
        workspace.path(),
    )
    .expect_err("absolute TOML path outside workspace must be rejected");

    assert!(error.to_string().contains("does not resolve"));
}

#[test]
fn text_summary_for_sequence_keeps_child_shapes() {
    let output = RuleOutput::Sequence(vec![
        RuleOutput::Nodes(vec![NodeId::new(1, 1)]),
        RuleOutput::EntryPoints(vec![NodeId::new(1, 2), NodeId::new(1, 3)]),
    ]);

    let summary = summarize_output(&output);

    assert!(summary.contains("2 sequence output"));
    assert!(summary.contains("1 node"));
    assert!(summary.contains("2 entry-point"));
}

#[test]
fn text_witness_steps_are_display_capped() {
    let steps = (0..(MAX_TEXT_WITNESS_STEPS + 3))
        .map(|index| RuleStep::RuleFired {
            rule_id: format!("test.rule.{index}"),
            severity: RuleSeverity::Info,
        })
        .collect();
    let witness = RuleWitness::new(steps, Vec::new());

    let lines = format_witness_step_lines(&witness);

    assert_eq!(lines.len(), MAX_TEXT_WITNESS_STEPS + 1);
    assert!(lines[0].contains("test.rule.0"));
    assert!(lines[MAX_TEXT_WITNESS_STEPS - 1].contains("test.rule.19"));
    assert!(
        lines[MAX_TEXT_WITNESS_STEPS]
            .contains("3 additional witness step(s) omitted from text output")
    );
    assert!(lines[MAX_TEXT_WITNESS_STEPS].contains("--format json"));
}

#[test]
fn execute_loaded_rule_emits_witness_for_ok_and_unsupported_for_beside_cache() {
    use std::sync::Arc;

    use sqry_core::graph::unified::CodeGraph;
    use sqry_db::planner::StringPattern;
    use sqry_db::{QueryDb, QueryDbConfig};

    let snapshot = Arc::new(CodeGraph::new().snapshot());
    let db = QueryDb::new(snapshot, QueryDbConfig::default());
    let backend = SqryDbRuleBackend::new(&db);
    let engine = RuleEngine::new();

    // An executable (non-beside-cache) rule yields status `ok` with a
    // witness-bearing result that ends in a RuleFired step.
    let ok_rule = LoadedRule {
        requires_beside_cache: false,
        definition: RuleDefinition::new(
            "test.ok.scan",
            RulePlan::new(RuleNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern: Some(StringPattern::contains("x")),
            }),
        ),
    };
    let ok = execute_loaded_rule(&engine, &backend, &ok_rule);
    assert!(matches!(ok.status, RuleRunStatus::Ok));
    assert!(ok.error.is_none());
    assert!(ok.output.is_some());
    let witness = ok.witness.expect("ok rule emits a witness");
    assert!(
        witness
            .steps
            .iter()
            .any(|step| matches!(step, RuleStep::RuleFired { .. })),
        "an ok rule result carries a witness ending in RuleFired"
    );

    // A beside-cache rule is reported as `unsupported` with no witness and the
    // beside-cache explanation, never executed on the single-snapshot graph.
    let beside_rule = LoadedRule {
        requires_beside_cache: true,
        definition: RuleDefinition::new(
            "test.beside.similar",
            RulePlan::new(RuleNode::SimilarTo {
                seed: RuleEndpoint::Nodes(Vec::new()),
                scope: None,
                similarity_kind: RuleSimilarityKind::Similar,
            }),
        ),
    };
    let unsupported = execute_loaded_rule(&engine, &backend, &beside_rule);
    assert!(matches!(unsupported.status, RuleRunStatus::Unsupported));
    assert!(unsupported.witness.is_none());
    assert!(unsupported.output.is_none());
    assert!(
        unsupported
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("beside-cache"),
        "unsupported beside-cache result explains the beside-cache requirement"
    );
}

#[test]
fn toml_loaded_rule_marks_beside_cache_from_plan_shape() {
    let definition = RuleDefinition::new(
        "test.beside",
        RulePlan::new(RuleNode::SimilarTo {
            seed: RuleEndpoint::Nodes(vec![NodeId::new(1, 1)]),
            scope: None,
            similarity_kind: RuleSimilarityKind::Similar,
        }),
    );

    let loaded = LoadedRule {
        requires_beside_cache: contains_beside_cache_route(definition.plan.root()),
        definition,
    };

    assert!(loaded.requires_beside_cache);
}
