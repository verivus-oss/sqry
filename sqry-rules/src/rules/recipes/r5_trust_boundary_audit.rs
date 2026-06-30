//! PR-R5: trust-boundary enumeration and validation audit.

use sqry_core::graph::unified::NodeKind;

use super::{chain, entry_points_named, path_to_check, relation_from, scan_name};
use crate::dsl::RuleDefinition;
use crate::ir::RelationEdgeKind;
use crate::rules::{RuleVariant, ShippedRule};

const VARIANTS: &[RuleVariant] = &[
    RuleVariant::Chain,
    RuleVariant::NodeScan,
    RuleVariant::EntryPointUnion,
    RuleVariant::RelationEdges,
    RuleVariant::PathQuery,
];

/// Builds PR-R5 from bbnty methodology recipe R5.
#[must_use]
pub fn rule() -> ShippedRule {
    let boundary_entries = entry_points_named("Recv");
    let boundary_callsite = scan_name(Some(NodeKind::Method), "Recv");
    let validator = scan_name(Some(NodeKind::Function), "Validate");
    let plan = chain(vec![
        boundary_entries,
        relation_from(boundary_callsite.clone(), RelationEdgeKind::References),
        path_to_check(boundary_callsite, validator, 6),
    ]);

    ShippedRule {
        definition: RuleDefinition::new("bbnty.pr_r5.trust_boundary_audit", plan),
        title: "PR-R5 Trust Boundary Audit",
        methodology: "sqry-vulnerability-hunting-methodology.md §3 Recipe R5 lines 201-218",
        seed_finding: Some("neon-proxy-jwks-ssrf.md"),
        variants: VARIANTS,
        requires_beside_cache: false,
        requires_trace_path: true,
        baseline_ms_floor: 1,
    }
}
