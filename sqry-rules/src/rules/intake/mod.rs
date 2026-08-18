//! Standard first-run intake pack for bbnty targets.

use sqry_core::graph::unified::NodeKind;
use sqry_db::planner::{Predicate, StringPattern};

use crate::dsl::RuleDefinition;
use crate::ir::{
    ComplexityMetric, EntrypointExtension, RuleCycleBounds, RuleEdgeClass, RuleEndpoint, RuleNode,
    RulePlan, RuleSimilarityKind,
};
use crate::rules::{RuleVariant, ShippedRule};

const CYCLE_VARIANTS: &[RuleVariant] = &[RuleVariant::CycleWitness];
const ENTRY_VARIANTS: &[RuleVariant] = &[RuleVariant::EntryPointUnion, RuleVariant::NodeScan];
const UNUSED_VARIANTS: &[RuleVariant] = &[
    RuleVariant::Chain,
    RuleVariant::NodeScan,
    RuleVariant::Filter,
];
const DUPLICATE_VARIANTS: &[RuleVariant] = &[RuleVariant::SimilarTo, RuleVariant::NodeScan];
const COMPLEXITY_VARIANTS: &[RuleVariant] = &[RuleVariant::ComplexityAggregate];

/// Returns the standard intake pack from the `TiKV` Phase 2/3 methodology.
#[must_use]
pub fn standard_intake_rules() -> Vec<ShippedRule> {
    vec![
        cycle_intake_rule(),
        entrypoint_intake_rule(),
        unused_classification_intake_rule(),
        duplicate_intake_rule(),
        complexity_intake_rule(),
    ]
}

fn cycle_intake_rule() -> ShippedRule {
    ShippedRule {
        definition: RuleDefinition::new(
            "bbnty.intake.cycles.calls",
            RulePlan::new(RuleNode::CycleWitness {
                edge_class: RuleEdgeClass::Call,
                bounds: RuleCycleBounds {
                    min_depth: 2,
                    max_depth: Some(8),
                    max_results: 500,
                    should_include_self_loops: false,
                },
            }),
        ),
        title: "Standard Intake Call Cycles",
        methodology: "tikv-analysis-methodology.md §Phase 3 find_cycles lines 44-95",
        seed_finding: None,
        variants: CYCLE_VARIANTS,
        requires_beside_cache: false,
        requires_trace_path: false,
        baseline_ms_floor: 1,
    }
}

fn entrypoint_intake_rule() -> ShippedRule {
    ShippedRule {
        definition: RuleDefinition::new(
            "bbnty.intake.entrypoints",
            RulePlan::new(RuleNode::EntryPointUnion {
                extensions: vec![
                    EntrypointExtension::Name(StringPattern::exact("main")),
                    EntrypointExtension::Name(StringPattern::contains("test")),
                ],
            }),
        ),
        title: "Standard Intake Entry Points",
        methodology: "tikv-analysis-methodology.md §Phase 3 get_graph_stats/find_unused lines 44-111",
        seed_finding: None,
        variants: ENTRY_VARIANTS,
        requires_beside_cache: false,
        requires_trace_path: false,
        baseline_ms_floor: 1,
    }
}

fn unused_classification_intake_rule() -> ShippedRule {
    ShippedRule {
        definition: RuleDefinition::new(
            "bbnty.intake.unused.nodes",
            RulePlan::new(RuleNode::Chain {
                steps: vec![
                    RuleNode::NodeScan {
                        kind: None,
                        visibility: None,
                        name_pattern: None,
                    },
                    RuleNode::Filter {
                        predicate: Predicate::IsUnused,
                    },
                ],
            }),
        ),
        title: "Standard Intake Unused Nodes",
        methodology: "tikv-analysis-methodology.md §Phase 3 find_unused lines 97-111",
        seed_finding: None,
        variants: UNUSED_VARIANTS,
        requires_beside_cache: false,
        requires_trace_path: false,
        baseline_ms_floor: 1,
    }
}

fn duplicate_intake_rule() -> ShippedRule {
    let seed = RuleNode::NodeScan {
        kind: Some(NodeKind::Function),
        visibility: None,
        name_pattern: Some(StringPattern::contains("handle")),
    };
    ShippedRule {
        definition: RuleDefinition::new(
            "bbnty.intake.duplicates.body",
            RulePlan::new(RuleNode::SimilarTo {
                seed: RuleEndpoint::Query(Box::new(seed)),
                scope: None,
                similarity_kind: RuleSimilarityKind::Duplicate,
            }),
        ),
        title: "Standard Intake Body Duplicates",
        methodology: "tikv-analysis-methodology.md §Phase 3 find_duplicates lines 113-145",
        seed_finding: None,
        variants: DUPLICATE_VARIANTS,
        // SimilarTo runs in-engine since L2a; only cross-snapshot needs coordination.
        requires_beside_cache: false,
        requires_trace_path: false,
        baseline_ms_floor: 1,
    }
}

fn complexity_intake_rule() -> ShippedRule {
    ShippedRule {
        definition: RuleDefinition::new(
            "bbnty.intake.complexity.functions",
            RulePlan::new(RuleNode::ComplexityAggregate {
                node_kind_filter: Some(NodeKind::Function),
                metric: ComplexityMetric::OutgoingCalls,
            }),
        ),
        title: "Standard Intake Function Complexity",
        methodology: "tikv-analysis-methodology.md §Phase 3 complexity_metrics lines 147-263",
        seed_finding: None,
        variants: COMPLEXITY_VARIANTS,
        requires_beside_cache: false,
        requires_trace_path: false,
        baseline_ms_floor: 1,
    }
}
