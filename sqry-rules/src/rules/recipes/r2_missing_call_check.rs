//! PR-R2: missing-call safety-check elision.

use sqry_core::graph::unified::NodeKind;

use super::{chain, difference, entry_points_named, exact_name, path_to_check, scan_name};
use crate::dsl::RuleDefinition;
use crate::rules::{RuleVariant, ShippedRule};

const VARIANTS: &[RuleVariant] = &[
    RuleVariant::Chain,
    RuleVariant::NodeScan,
    RuleVariant::EntryPointUnion,
    RuleVariant::PathQuery,
    RuleVariant::SetOp,
];

/// Builds PR-R2 from bbnty methodology recipe R2.
#[must_use]
pub fn rule() -> ShippedRule {
    let entry_points = entry_points_named("localStorage");
    let dangerous_use = scan_name(Some(NodeKind::Function), "localStorage");
    let permission_check = exact_name(Some(NodeKind::Function), "kFileSystemRead");
    let plan = chain(vec![
        entry_points.clone(),
        path_to_check(dangerous_use.clone(), permission_check.clone(), 5),
        difference(dangerous_use, permission_check),
    ]);

    ShippedRule {
        definition: RuleDefinition::new("bbnty.pr_r2.missing_call_check", plan),
        title: "PR-R2 Missing Call Check",
        methodology: "sqry-vulnerability-hunting-methodology.md §3 Recipe R2 lines 135-156",
        seed_finding: Some("nodejs-localstorage-file-permission-bypass.md"),
        variants: VARIANTS,
        requires_beside_cache: false,
        requires_trace_path: true,
        baseline_ms_floor: 1,
    }
}
