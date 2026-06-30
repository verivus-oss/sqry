//! PR-R3: new feature / new IR-node coverage.

use sqry_core::graph::unified::NodeKind;

use super::{chain, diff, filtered_scan, references_to, scan_name};
use crate::dsl::RuleDefinition;
use crate::rules::{RuleVariant, ShippedRule};

const VARIANTS: &[RuleVariant] = &[
    RuleVariant::Chain,
    RuleVariant::CrossSnapshotDiff,
    RuleVariant::ReferencesAt,
    RuleVariant::NodeScan,
    RuleVariant::Filter,
];

/// Builds PR-R3 from bbnty methodology recipe R3.
#[must_use]
pub fn rule() -> ShippedRule {
    let new_variant = scan_name(Some(NodeKind::EnumVariant), "opcode");
    let plan = chain(vec![
        diff(),
        references_to(new_variant),
        filtered_scan(Some(NodeKind::Function), "case"),
    ]);

    ShippedRule {
        definition: RuleDefinition::new("bbnty.pr_r3.new_feature_coverage", plan),
        title: "PR-R3 New Feature Coverage",
        methodology: "sqry-vulnerability-hunting-methodology.md §3 Recipe R3 lines 158-178",
        seed_finding: Some("firefox-webtransport-capsule-length-truncation.md"),
        variants: VARIANTS,
        requires_beside_cache: true,
        requires_trace_path: false,
        baseline_ms_floor: 1,
    }
}
