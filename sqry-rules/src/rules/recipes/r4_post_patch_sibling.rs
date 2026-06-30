//! PR-R4: post-patch sibling hunt.

use sqry_core::graph::unified::NodeKind;

use super::{chain, diff, filtered_scan, scan_name, similar_to};
use crate::dsl::RuleDefinition;
use crate::ir::RuleSimilarityKind;
use crate::rules::{RuleVariant, ShippedRule};

const VARIANTS: &[RuleVariant] = &[
    RuleVariant::Chain,
    RuleVariant::CrossSnapshotDiff,
    RuleVariant::SimilarTo,
    RuleVariant::NodeScan,
    RuleVariant::Filter,
];

/// Builds PR-R4 from bbnty methodology recipe R4.
#[must_use]
pub fn rule() -> ShippedRule {
    let patched_body = scan_name(Some(NodeKind::Function), "HandleToProcess");
    let ipc_scope = scan_name(Some(NodeKind::Method), "Recv");
    let plan = chain(vec![
        diff(),
        similar_to(patched_body, Some(ipc_scope), RuleSimilarityKind::Similar),
        filtered_scan(Some(NodeKind::Method), "validate"),
    ]);

    ShippedRule {
        definition: RuleDefinition::new("bbnty.pr_r4.post_patch_sibling", plan),
        title: "PR-R4 Post Patch Sibling",
        methodology: "sqry-vulnerability-hunting-methodology.md §3 Recipe R4 lines 180-199",
        seed_finding: Some("firecracker-pci-snapshot-restore-panic-followup-draft.md"),
        variants: VARIANTS,
        requires_beside_cache: true,
        requires_trace_path: false,
        baseline_ms_floor: 1,
    }
}
