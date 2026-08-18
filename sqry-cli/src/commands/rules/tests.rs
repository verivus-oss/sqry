use clap::Parser;
use sqry_core::graph::unified::NodeId;
use sqry_rules::ir::{RuleEndpoint, RuleNode, RulePlan, RuleSimilarityKind};

use crate::large_stack_test;

use super::*;

/// A minimal `CrossSnapshotDiff` node: the one remaining primitive the gate
/// still reports as unsupported.
fn cross_snapshot_diff() -> RuleNode {
    RuleNode::CrossSnapshotDiff {
        base: sqry_rules::SnapshotId {
            edge_revision: 1,
            metadata_revision: 1,
        },
        head: sqry_rules::SnapshotId {
            edge_revision: 2,
            metadata_revision: 2,
        },
        include_unchanged: false,
    }
}

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
fn shipped_security_pack_loads_the_universal_detector() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let loaded = load_rules("bbnty.security", workspace.path()).expect("security pack should load");

    assert_eq!(loaded.source, "bbnty.security");
    assert_eq!(loaded.rules.len(), 1);
    assert_eq!(
        loaded.rules[0].definition.id,
        "bbnty.security.unsafe_ffi_reach"
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
fn unsupported_detection_finds_nested_cross_snapshot_but_not_similar_to() {
    // Since L2a, a nested SimilarTo is engine-executable, so it must NOT flag the
    // rule as unsupported; a nested CrossSnapshotDiff still must.
    let nested_similar = RuleNode::PathQuery {
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
        avoid: None,
    };
    assert!(!contains_unsupported_beside_cache(&nested_similar));

    let nested_diff = RuleNode::PathQuery {
        from: RuleEndpoint::Query(Box::new(RuleNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        })),
        to: RuleEndpoint::Query(Box::new(cross_snapshot_diff())),
        kind: sqry_rules::ir::PathKind::Calls,
        max_depth: 3,
        max_paths: Some(8),
        avoid: None,
    };
    assert!(contains_unsupported_beside_cache(&nested_diff));
}

#[test]
fn unsupported_detection_walks_the_path_query_avoid_endpoint() {
    // A CrossSnapshotDiff nested under PathQuery.avoid must still be detected, so
    // the rule is correctly reported Unsupported rather than run as a plain query.
    let with_avoid = RuleNode::PathQuery {
        from: RuleEndpoint::Query(Box::new(RuleNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        })),
        to: RuleEndpoint::Query(Box::new(RuleNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        })),
        kind: sqry_rules::ir::PathKind::Calls,
        max_depth: 3,
        max_paths: Some(8),
        avoid: Some(RuleEndpoint::Query(Box::new(cross_snapshot_diff()))),
    };

    assert!(contains_unsupported_beside_cache(&with_avoid));
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

    // A cross-snapshot rule is reported as `unsupported` with no witness and the
    // cross-snapshot explanation, never executed on the single-snapshot graph.
    let beside_rule = LoadedRule {
        requires_beside_cache: false,
        definition: RuleDefinition::new(
            "test.beside.cross_snapshot",
            RulePlan::new(cross_snapshot_diff()),
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
            .contains("cross-snapshot"),
        "unsupported result explains the cross-snapshot requirement"
    );
}

#[test]
fn execute_loaded_rule_carries_authored_metadata_and_severity() {
    use std::sync::Arc;

    use sqry_core::graph::unified::CodeGraph;
    use sqry_db::planner::StringPattern;
    use sqry_db::{QueryDb, QueryDbConfig};

    let snapshot = Arc::new(CodeGraph::new().snapshot());
    let db = QueryDb::new(snapshot, QueryDbConfig::default());
    let backend = SqryDbRuleBackend::new(&db);
    let engine = RuleEngine::new();

    // An Ok rule authored with full metadata: every field rides the result row
    // and the authored severity reaches the RuleFired witness step (overriding
    // the Info default the caller would otherwise pass).
    let ok_rule = LoadedRule {
        requires_beside_cache: false,
        definition: RuleDefinition::new(
            "test.meta.scan",
            RulePlan::new(RuleNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern: Some(StringPattern::contains("x")),
            }),
        )
        .with_severity(RuleSeverity::Error)
        .with_cwe("CWE-242")
        .with_description("unsafe FFI usage")
        .with_remediation("audit the boundary"),
    };
    let ok = execute_loaded_rule(&engine, &backend, &ok_rule);
    assert!(matches!(ok.status, RuleRunStatus::Ok));
    assert_eq!(ok.severity, Some(RuleSeverity::Error));
    assert_eq!(ok.cwe.as_deref(), Some("CWE-242"));
    assert_eq!(ok.description.as_deref(), Some("unsafe FFI usage"));
    assert_eq!(ok.remediation.as_deref(), Some("audit the boundary"));
    let witness = ok.witness.expect("ok rule emits a witness");
    assert!(
        witness.steps.iter().any(|step| matches!(
            step,
            RuleStep::RuleFired {
                severity: RuleSeverity::Error,
                ..
            }
        )),
        "authored severity reaches the RuleFired witness step"
    );

    // Metadata also rides an Unsupported (beside-cache) row, which never executes.
    let beside_rule = LoadedRule {
        requires_beside_cache: true,
        definition: RuleDefinition::new(
            "test.meta.beside",
            RulePlan::new(RuleNode::SimilarTo {
                seed: RuleEndpoint::Nodes(Vec::new()),
                scope: None,
                similarity_kind: RuleSimilarityKind::Similar,
            }),
        )
        .with_severity(RuleSeverity::Warning)
        .with_cwe("CWE-000"),
    };
    let unsupported = execute_loaded_rule(&engine, &backend, &beside_rule);
    assert!(matches!(unsupported.status, RuleRunStatus::Unsupported));
    assert_eq!(unsupported.severity, Some(RuleSeverity::Warning));
    assert_eq!(unsupported.cwe.as_deref(), Some("CWE-000"));
}

#[test]
fn toml_loaded_rule_marks_cross_snapshot_from_plan_shape() {
    // CrossSnapshotDiff is flagged from plan shape; a SimilarTo-only plan is not.
    let diff_definition = RuleDefinition::new("test.diff", RulePlan::new(cross_snapshot_diff()));
    let diff_loaded = LoadedRule {
        requires_beside_cache: contains_unsupported_beside_cache(diff_definition.plan.root()),
        definition: diff_definition,
    };
    assert!(diff_loaded.requires_beside_cache);

    let similar_definition = RuleDefinition::new(
        "test.similar",
        RulePlan::new(RuleNode::SimilarTo {
            seed: RuleEndpoint::Nodes(vec![NodeId::new(1, 1)]),
            scope: None,
            similarity_kind: RuleSimilarityKind::Similar,
        }),
    );
    let similar_loaded = LoadedRule {
        requires_beside_cache: contains_unsupported_beside_cache(similar_definition.plan.root()),
        definition: similar_definition,
    };
    assert!(!similar_loaded.requires_beside_cache);
}

#[test]
fn shipped_rules_beside_cache_flag_matches_plan_shape() {
    // A shipped rule's hardcoded requires_beside_cache flag must agree with its
    // plan. A stale flag (SimilarTo rule declared true) silently regressed L2a;
    // this pins every declaration to plan reality across the whole catalog.
    for rule in rules::shipped_rules() {
        let plan_needs = contains_unsupported_beside_cache(rule.definition.plan.root());
        assert_eq!(
            rule.requires_beside_cache,
            plan_needs,
            "{} declares requires_beside_cache={} but its plan needs={plan_needs}",
            rule.id(),
            rule.requires_beside_cache,
        );
    }
}

#[test]
fn shipped_similarto_rules_load_ungated() {
    // The primary product surface: shipped SimilarTo rules must reach the engine
    // (not be short-circuited to Unsupported), while cross-snapshot rules stay gated.
    let similar_to_ids = ["bbnty.intake.duplicates.body", "bbnty.pr_r7.peer_asymmetry"];
    let mut seen = 0_usize;
    let mut any_gated = false;
    for rule in rules::shipped_rules() {
        let id = rule.id().to_string();
        let loaded = LoadedRule::from(rule);
        if similar_to_ids.contains(&id.as_str()) {
            seen += 1;
            assert!(
                !loaded.requires_beside_cache,
                "{id} is SimilarTo and must load ungated since L2a"
            );
        }
        if loaded.requires_beside_cache {
            any_gated = true;
        }
    }
    assert_eq!(
        seen,
        similar_to_ids.len(),
        "expected the shipped SimilarTo rules to be present"
    );
    assert!(
        any_gated,
        "cross-snapshot shipped rules must still load gated as unsupported"
    );
}
