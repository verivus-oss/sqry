//! TC4: Rule witness vocabulary, serde envelope, and truncation marker.
//!
//! Constructs every `RuleStep` variant, asserts the witness envelope rejects
//! unknown fields (mirroring Phase 2's `SymbolResolutionWitness` contract),
//! and verifies the truncation marker fires past the configured step cap.

use sqry_core::graph::unified::NodeId;
use sqry_db::planner::{Direction, SetOperation};

use sqry_rules::ir::{RuleEdgeClass, RuleSimilarityKind};
use sqry_rules::witness::{DiffEntryKind, PathBudgetReason, RulePredicateKind, RuleSeverity};
use sqry_rules::{RuleStep, RuleWitness};

const NODE_A: NodeId = NodeId::new(1, 1);
const NODE_B: NodeId = NodeId::new(2, 1);

fn all_rule_steps() -> Vec<RuleStep> {
    vec![
        RuleStep::NodeScanMatched {
            kind: None,
            visibility: None,
            name_pattern: None,
            match_count: 3,
        },
        RuleStep::EdgeTraversed {
            from: NODE_A,
            to: NODE_B,
            direction: Direction::Forward,
            edge_classification: RuleEdgeClass::Call,
            depth: 1,
        },
        RuleStep::PredicateApplied {
            predicate_kind: RulePredicateKind::Name,
            inputs: 4,
            outputs: 2,
        },
        RuleStep::SetOpEvaluated {
            op: SetOperation::Union,
            lhs_card: 1,
            rhs_card: 1,
            result_card: 2,
        },
        RuleStep::PathConstructed {
            from: NODE_A,
            to: NODE_B,
            length: 1,
            edge_classes: vec![RuleEdgeClass::Call],
            nodes: vec![NODE_A, NODE_B],
        },
        RuleStep::PathBudgetExhausted {
            reason: PathBudgetReason::MaxPaths,
        },
        RuleStep::RelationEdgeEmitted {
            from: NODE_A,
            to: NODE_B,
            kind: RuleEdgeClass::Call,
            with_metadata: true,
        },
        RuleStep::CycleDetected {
            component_id: 0,
            length: 2,
            nodes: vec![NODE_A, NODE_B],
        },
        RuleStep::ReferenceLocated {
            source: NODE_A,
            target: NODE_B,
            citation_index: 0,
        },
        RuleStep::MetricComputed {
            metric: "NodeCount".to_string(),
            value: 7,
            node_count: 7,
        },
        RuleStep::DiffEntryEmitted {
            kind: DiffEntryKind::Added,
            base: None,
            head: Some(NODE_B),
        },
        RuleStep::EntryPointClassified {
            classifier: "name".to_string(),
            node: NODE_A,
        },
        RuleStep::SimilarityMatchEmitted {
            seed: NODE_A,
            matched: NODE_B,
            score: 9_000,
            similarity_kind: RuleSimilarityKind::Similar,
        },
        RuleStep::RuleFired {
            rule_id: "demo".to_string(),
            severity: RuleSeverity::Info,
        },
        RuleStep::WitnessTruncated { dropped: 1, cap: 1 },
    ]
}

#[test]
fn every_witness_step_variant_is_constructible() {
    let steps = all_rule_steps();
    let distinct: std::collections::BTreeSet<&'static str> =
        steps.iter().map(RuleStep::variant_name).collect();

    assert_eq!(steps.len(), 15);
    assert_eq!(distinct.len(), 15, "every variant has a unique stable name");
}

#[test]
fn witness_envelope_rejects_unknown_fields() {
    let witness = RuleWitness::new(
        vec![RuleStep::RuleFired {
            rule_id: "demo".to_string(),
            severity: RuleSeverity::Info,
        }],
        Vec::new(),
    );

    let json = serde_json::to_string(&witness).expect("witness serializes");
    let round_trip: RuleWitness = serde_json::from_str(&json).expect("witness deserializes");
    assert_eq!(round_trip, witness);

    let tampered = json.replace(
        "\"truncated\":false",
        "\"truncated\":false,\"unexpected_field\":true",
    );
    assert_ne!(tampered, json, "tamper string must actually inject a field");
    let error = serde_json::from_str::<RuleWitness>(&tampered);
    assert!(
        error.is_err(),
        "deny_unknown_fields must reject the injected witness field"
    );
}

#[test]
fn truncation_marker_fires_past_the_step_cap() {
    let steps = vec![
        RuleStep::NodeScanMatched {
            kind: None,
            visibility: None,
            name_pattern: None,
            match_count: 1,
        },
        RuleStep::NodeScanMatched {
            kind: None,
            visibility: None,
            name_pattern: None,
            match_count: 2,
        },
        RuleStep::NodeScanMatched {
            kind: None,
            visibility: None,
            name_pattern: None,
            match_count: 3,
        },
    ];

    let witness = RuleWitness::with_step_cap(steps, Vec::new(), 2);

    assert!(witness.truncated);
    assert_eq!(witness.steps.len(), 2);
    assert!(matches!(
        witness.steps.last(),
        Some(RuleStep::WitnessTruncated { dropped: 2, cap: 2 })
    ));
}
