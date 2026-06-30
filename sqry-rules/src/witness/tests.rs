use std::collections::HashSet;

use sqry_core::graph::unified::{NodeId, NodeKind};
use sqry_db::planner::{Direction, SetOperation};

use super::{
    CitationSpan, DiffEntryKind, PathBudgetReason, RuleCitation, RulePredicateKind, RuleSeverity,
    RuleStep, RuleWitness,
};
use crate::ir::{RuleEdgeClass, RuleSimilarityKind};

fn node(index: u32) -> NodeId {
    NodeId::new(index, 1)
}

fn all_step_variants() -> Vec<RuleStep> {
    vec![
        RuleStep::NodeScanMatched {
            kind: Some(NodeKind::Function),
            visibility: None,
            name_pattern: None,
            match_count: 2,
        },
        RuleStep::EdgeTraversed {
            from: node(1),
            to: node(2),
            direction: Direction::Forward,
            edge_classification: RuleEdgeClass::Call,
            depth: 1,
        },
        RuleStep::PredicateApplied {
            predicate_kind: RulePredicateKind::Name,
            inputs: 2,
            outputs: 1,
        },
        RuleStep::SetOpEvaluated {
            op: SetOperation::Union,
            lhs_card: 1,
            rhs_card: 1,
            result_card: 2,
        },
        RuleStep::PathConstructed {
            from: node(1),
            to: node(3),
            length: 2,
            edge_classes: vec![RuleEdgeClass::Call],
            nodes: vec![node(1), node(2), node(3)],
        },
        RuleStep::PathBudgetExhausted {
            reason: PathBudgetReason::MaxPaths,
        },
        RuleStep::RelationEdgeEmitted {
            from: node(1),
            to: node(2),
            kind: RuleEdgeClass::Reference,
            with_metadata: true,
        },
        RuleStep::CycleDetected {
            component_id: 7,
            length: 2,
            nodes: vec![node(1), node(2)],
        },
        RuleStep::ReferenceLocated {
            source: node(1),
            target: node(2),
            citation_index: 0,
        },
        RuleStep::MetricComputed {
            metric: "outgoing_calls".into(),
            value: 3,
            node_count: 1,
        },
        RuleStep::DiffEntryEmitted {
            kind: DiffEntryKind::Modified,
            base: Some(node(1)),
            head: Some(node(2)),
        },
        RuleStep::EntryPointClassified {
            classifier: "bin_main".into(),
            node: node(1),
        },
        RuleStep::SimilarityMatchEmitted {
            seed: node(1),
            matched: node(2),
            score: 9_500,
            similarity_kind: RuleSimilarityKind::Similar,
        },
        RuleStep::RuleFired {
            rule_id: "rule.demo".into(),
            severity: RuleSeverity::Warning,
        },
        RuleStep::WitnessTruncated {
            dropped: 4,
            cap: 10,
        },
    ]
}

#[test]
fn rule_step_declares_fifteen_variants() {
    let names: HashSet<_> = all_step_variants()
        .iter()
        .map(RuleStep::variant_name)
        .collect();

    assert_eq!(names.len(), 15);
    assert!(names.contains("SimilarityMatchEmitted"));
    assert!(names.contains("WitnessTruncated"));
}

#[test]
fn every_fr2_rule_ir_family_has_a_witness_step() {
    let coverage = [
        ("NodeScan", "NodeScanMatched"),
        ("EdgeTraversal", "EdgeTraversed"),
        ("Filter", "PredicateApplied"),
        ("SetOp", "SetOpEvaluated"),
        ("Chain", "RuleFired"),
        ("PathQuery", "PathConstructed"),
        ("SubgraphExtract", "EdgeTraversed"),
        ("RelationEdges", "RelationEdgeEmitted"),
        ("CycleWitness", "CycleDetected"),
        ("ReferencesAt", "ReferenceLocated"),
        ("ComplexityAggregate", "MetricComputed"),
        ("CrossSnapshotDiff", "DiffEntryEmitted"),
        ("EntryPointUnion", "EntryPointClassified"),
        ("SimilarTo", "SimilarityMatchEmitted"),
    ];
    let names: HashSet<_> = all_step_variants()
        .iter()
        .map(RuleStep::variant_name)
        .collect();

    for (ir_variant, step_variant) in coverage {
        assert!(
            names.contains(step_variant),
            "{ir_variant} must have witness step {step_variant}"
        );
    }
}

#[test]
fn rule_witness_truncates_with_explicit_marker() {
    let steps = all_step_variants();
    let witness = RuleWitness::with_step_cap(steps, Vec::new(), 4);

    assert!(witness.truncated);
    assert_eq!(witness.steps.len(), 4);
    assert_eq!(
        witness.steps.last(),
        Some(&RuleStep::WitnessTruncated {
            dropped: 12,
            cap: 4
        })
    );
}

#[test]
fn rule_witness_and_steps_roundtrip_through_postcard() {
    for step in all_step_variants() {
        let bytes = postcard::to_allocvec(&step).expect("serialize step");
        let decoded: RuleStep = postcard::from_bytes(&bytes).expect("deserialize step");
        assert_eq!(decoded, step);
    }

    let citation = RuleCitation::new("src/lib.rs")
        .with_span(CitationSpan::new(1, 0, 1, 10))
        .with_label("demo");
    let witness = RuleWitness::with_default_cap(all_step_variants(), vec![citation]);
    let bytes = postcard::to_allocvec(&witness).expect("serialize witness");
    let decoded: RuleWitness = postcard::from_bytes(&bytes).expect("deserialize witness");

    assert_eq!(decoded, witness);
}
