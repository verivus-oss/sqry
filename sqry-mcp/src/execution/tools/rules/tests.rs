//! Unit tests for the `rules_run` MCP tool (P5U10).
//!
//! These cover the pure selector-resolution and beside-cache-detection logic
//! plus the response-type serde contract. The full graph-backed execute path
//! is exercised end-to-end by `tests/installed_feature_surface_e2e.rs`, which
//! invokes `rules_run` through the real MCP transport against an indexed
//! fixture workspace.

use super::*;
use sqry_mcp_redaction::{RedactionConfig, Redactor};
use sqry_rules::rules::shipped_rules;
use tempfile::tempdir;

#[test]
fn shipped_pack_recipes_loads() {
    let dir = tempdir().expect("tempdir");
    let loaded = load_rules("bbnty.recipes", dir.path()).expect("recipes load");
    assert_eq!(loaded.source, "bbnty.recipes");
    assert!(!loaded.rules.is_empty(), "recipes pack must ship rules");
}

#[test]
fn shipped_pack_intake_loads() {
    let dir = tempdir().expect("tempdir");
    let loaded = load_rules("bbnty.intake", dir.path()).expect("intake load");
    assert_eq!(loaded.source, "bbnty.intake");
    assert!(!loaded.rules.is_empty(), "intake pack must ship rules");
}

#[test]
fn shipped_pack_security_loads_the_universal_detector() {
    let dir = tempdir().expect("tempdir");
    let loaded = load_rules("bbnty.security", dir.path()).expect("security load");
    assert_eq!(loaded.source, "bbnty.security");
    assert_eq!(loaded.rules.len(), 1);
    assert_eq!(
        loaded.rules[0].definition.id,
        "bbnty.security.unsafe_ffi_reach"
    );
}

#[test]
fn shipped_pack_all_unions_recipes_and_intake() {
    let dir = tempdir().expect("tempdir");
    let all = load_rules("bbnty.all", dir.path()).expect("all load");
    assert_eq!(all.source, "bbnty.all");
    assert_eq!(all.rules.len(), shipped_rules().len());
}

#[test]
fn exact_shipped_id_loads_single_rule() {
    let dir = tempdir().expect("tempdir");
    let first_id = shipped_rules()
        .first()
        .expect("at least one shipped rule")
        .id()
        .to_string();
    let loaded = load_rules(&first_id, dir.path()).expect("exact id load");
    assert_eq!(loaded.source, first_id);
    assert_eq!(loaded.rules.len(), 1);
    assert_eq!(loaded.rules[0].definition.id, first_id);
}

#[test]
fn unknown_selector_without_toml_file_errors() {
    let dir = tempdir().expect("tempdir");
    // Not a shipped id/pack and not an existing TOML file in the workspace.
    let result = load_rules("not-a-rule-or-pack", dir.path());
    assert!(
        result.is_err(),
        "an unresolved selector with no TOML file must error, got: {result:?}"
    );
}

#[test]
fn toml_path_escaping_workspace_is_rejected() {
    let dir = tempdir().expect("tempdir");
    // Path traversal must be rejected before any read attempt.
    let result = load_rules("../../etc/passwd", dir.path());
    assert!(
        result.is_err(),
        "a workspace-escaping TOML path must be rejected, got: {result:?}"
    );
}

#[test]
fn shipped_similarto_rules_load_ungated() {
    // Since L2a, shipped SimilarTo rules must reach the engine on the MCP
    // surface instead of short-circuiting to `unsupported`; only cross-snapshot
    // rules stay gated. The From conversion derives the gate from plan shape.
    let similar_to_ids = ["bbnty.intake.duplicates.body", "bbnty.pr_r7.peer_asymmetry"];
    let mut seen = 0_usize;
    let mut any_gated = false;
    for rule in shipped_rules() {
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

#[test]
fn shipped_rules_beside_cache_flag_matches_plan_shape() {
    // Pin every shipped declaration to plan reality so a stale flag cannot
    // silently regress the gate (the L2a code-gate defect).
    for rule in shipped_rules() {
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
fn status_serializes_snake_case() {
    assert_eq!(
        serde_json::to_value(RulesRunStatus::Ok).expect("serialize ok"),
        serde_json::json!("ok")
    );
    assert_eq!(
        serde_json::to_value(RulesRunStatus::Unsupported).expect("serialize unsupported"),
        serde_json::json!("unsupported")
    );
    assert_eq!(
        serde_json::to_value(RulesRunStatus::Error).expect("serialize error"),
        serde_json::json!("error")
    );
}

#[test]
fn result_skips_absent_optional_fields() {
    let ok = RulesRunResultData {
        id: "rule.x".to_string(),
        status: RulesRunStatus::Ok,
        severity: None,
        cwe: None,
        description: None,
        remediation: None,
        output: None,
        witness: None,
        error: None,
    };
    let json = serde_json::to_value(&ok).expect("serialize result");
    assert_eq!(json.get("id").and_then(|v| v.as_str()), Some("rule.x"));
    assert_eq!(json.get("status").and_then(|v| v.as_str()), Some("ok"));
    assert!(json.get("error").is_none(), "absent error must be skipped");
    assert!(
        json.get("output").is_none(),
        "absent output must be skipped"
    );
    assert!(
        json.get("witness").is_none(),
        "absent witness must be skipped"
    );
    for field in ["severity", "cwe", "description", "remediation"] {
        assert!(
            json.get(field).is_none(),
            "absent metadata field {field} must be skipped"
        );
    }
}

#[test]
fn data_uses_camel_case_envelope() {
    let data = RulesRunData {
        selector: "bbnty.intake".to_string(),
        results: Vec::new(),
    };
    let json = serde_json::to_value(&data).expect("serialize data");
    // `selector` (not `source`) so the minimal redaction preset does not
    // rewrite the echoed selector through the path-field walker.
    assert_eq!(
        json.get("selector").and_then(|v| v.as_str()),
        Some("bbnty.intake")
    );
    assert!(
        json.get("source").is_none(),
        "must not use PATH_FIELDS `source`"
    );
    assert!(
        json.get("results").is_some(),
        "results array must serialize"
    );
}

#[test]
fn execute_loaded_rule_emits_witness_for_ok_and_unsupported_for_beside_cache() {
    use std::sync::Arc;

    use sqry_core::graph::unified::CodeGraph;
    use sqry_db::planner::StringPattern;
    use sqry_db::{QueryDb, QueryDbConfig};
    use sqry_rules::ir::RulePlan;
    use sqry_rules::witness::RuleStep;

    let snapshot = Arc::new(CodeGraph::new().snapshot());
    let db = QueryDb::new(snapshot, QueryDbConfig::default());
    let backend = SqryDbRuleBackend::new(&db);
    let engine = RuleEngine::new();

    // An executable rule returns status `ok` with a witness-bearing result.
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
    assert!(matches!(ok.status, RulesRunStatus::Ok));
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

    // A cross-snapshot rule is reported `unsupported` with the cross-snapshot
    // explanation and no witness, never executed on the single-snapshot graph.
    // The gate computes this from the plan shape (flag left false).
    let beside_rule = LoadedRule {
        requires_beside_cache: false,
        definition: RuleDefinition::new(
            "test.beside.cross_snapshot",
            RulePlan::new(RuleNode::CrossSnapshotDiff {
                base: sqry_rules::SnapshotId {
                    edge_revision: 1,
                    metadata_revision: 1,
                },
                head: sqry_rules::SnapshotId {
                    edge_revision: 2,
                    metadata_revision: 2,
                },
                include_unchanged: false,
            }),
        ),
    };
    let unsupported = execute_loaded_rule(&engine, &backend, &beside_rule);
    assert!(matches!(unsupported.status, RulesRunStatus::Unsupported));
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
fn selector_survives_minimal_redaction() {
    // Codex P5U10 finding: a field named `source` collides with a
    // sqry-mcp-redaction PATH_FIELDS key and would be rewritten by the
    // path walker under the default `minimal` preset. The `selector` rename
    // must survive a minimal-preset redaction round-trip verbatim.
    let mut json = serde_json::to_value(RulesRunData {
        selector: "bbnty.intake".to_string(),
        results: Vec::new(),
    })
    .expect("serialize data");

    let redactor = Redactor::new(RedactionConfig::minimal()).expect("minimal redactor");
    redactor.redact(&mut json);

    assert_eq!(
        json.get("selector").and_then(|v| v.as_str()),
        Some("bbnty.intake"),
        "selector echo must survive the minimal redaction preset unchanged: {json}"
    );
}
