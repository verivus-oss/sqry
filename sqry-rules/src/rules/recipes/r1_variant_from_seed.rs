//! PR-R1: variant analysis from a seed CVE.

use sqry_core::graph::unified::NodeKind;

use super::{chain, exact_name, path_to_check, references_to, scan_name, subgraph_from};
use crate::dsl::RuleDefinition;
use crate::ir::RuleEdgeClass;
use crate::rules::{RuleVariant, ShippedRule};

const VARIANTS: &[RuleVariant] = &[
    RuleVariant::Chain,
    RuleVariant::NodeScan,
    RuleVariant::ReferencesAt,
    RuleVariant::SubgraphExtract,
    RuleVariant::PathQuery,
];

/// Builds PR-R1 from bbnty methodology recipe R1.
#[must_use]
pub fn rule() -> ShippedRule {
    let seed = scan_name(Some(NodeKind::Function), "prototype");
    let safety_check = exact_name(
        Some(NodeKind::Function),
        "IsJSObjectThatCanBeTrackedAsPrototype",
    );
    let validity_cell = scan_name(Some(NodeKind::Function), "PrototypeChainValidityCell");
    let plan = chain(vec![
        references_to(validity_cell.clone()),
        subgraph_from(
            seed.clone(),
            vec![RuleEdgeClass::Call, RuleEdgeClass::Reference],
            2,
        ),
        path_to_check(seed, safety_check, 6),
    ]);

    ShippedRule {
        definition: RuleDefinition::new("bbnty.pr_r1.variant_from_seed", plan),
        title: "PR-R1 Variant From Seed",
        methodology: "sqry-vulnerability-hunting-methodology.md §3 Recipe R1 lines 103-133",
        seed_finding: Some("firefox-webtransport-capsule-length-truncation.md"),
        variants: VARIANTS,
        requires_beside_cache: false,
        requires_trace_path: true,
        baseline_ms_floor: 1,
    }
}
