//! PR-R6: speculation trusted as fact.

use sqry_core::graph::unified::NodeKind;

use super::{chain, filtered_scan, path_to_check, references_to, scan_name};
use crate::dsl::RuleDefinition;
use crate::rules::{RuleVariant, ShippedRule};

const VARIANTS: &[RuleVariant] = &[
    RuleVariant::Chain,
    RuleVariant::NodeScan,
    RuleVariant::ReferencesAt,
    RuleVariant::PathQuery,
    RuleVariant::Filter,
];

/// Builds PR-R6 from bbnty methodology recipe R6.
#[must_use]
pub fn rule() -> ShippedRule {
    let speculation_source = scan_name(Some(NodeKind::Function), "Assume");
    let trusted_decision = scan_name(Some(NodeKind::Function), "CanElide");
    let guard = scan_name(Some(NodeKind::Function), "deopt");
    let plan = chain(vec![
        references_to(speculation_source.clone()),
        path_to_check(speculation_source, trusted_decision, 7),
        filtered_scan(Some(NodeKind::Function), "speculat"),
        references_to(guard),
    ]);

    ShippedRule {
        definition: RuleDefinition::new("bbnty.pr_r6.speculation_trust", plan),
        title: "PR-R6 Speculation Trust",
        methodology: "sqry-vulnerability-hunting-methodology.md §3 Recipe R6 lines 220-243",
        seed_finding: Some("rustc-rmeta-truncated-metadata-ice.md"),
        variants: VARIANTS,
        requires_beside_cache: false,
        requires_trace_path: true,
        baseline_ms_floor: 1,
    }
}
