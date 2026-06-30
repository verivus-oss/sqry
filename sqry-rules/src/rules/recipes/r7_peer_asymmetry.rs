//! PR-R7: asymmetric peer analysis.

use sqry_core::graph::unified::NodeKind;

use super::{chain, difference, relation_from, scan_name, similar_to, subgraph_from};
use crate::dsl::RuleDefinition;
use crate::ir::{RelationEdgeKind, RuleEdgeClass, RuleSimilarityKind};
use crate::rules::{RuleVariant, ShippedRule};

const VARIANTS: &[RuleVariant] = &[
    RuleVariant::SetOp,
    RuleVariant::NodeScan,
    RuleVariant::SubgraphExtract,
    RuleVariant::SimilarTo,
    RuleVariant::RelationEdges,
];

/// Builds PR-R7 from bbnty methodology recipe R7.
#[must_use]
pub fn rule() -> ShippedRule {
    let checked_peer = scan_name(Some(NodeKind::Method), "CopyScratchSpace");
    let peer_scope = scan_name(Some(NodeKind::Method), "Read");
    let relation_scan = scan_name(Some(NodeKind::Method), "Read");
    let unchecked_peer = scan_name(Some(NodeKind::Method), "Slug");
    let plan = chain(vec![
        subgraph_from(peer_scope.clone(), vec![RuleEdgeClass::Call], 2),
        similar_to(
            checked_peer,
            Some(peer_scope),
            RuleSimilarityKind::Duplicate,
        ),
        relation_from(relation_scan, RelationEdgeKind::Callees),
        difference(scan_name(Some(NodeKind::Method), "Read"), unchecked_peer),
    ]);

    ShippedRule {
        definition: RuleDefinition::new("bbnty.pr_r7.peer_asymmetry", plan),
        title: "PR-R7 Peer Asymmetry",
        methodology: "sqry-vulnerability-hunting-methodology.md §3 Recipe R7 lines 245-263",
        seed_finding: Some("rustc-rmeta-truncated-metadata-ice.md"),
        variants: VARIANTS,
        requires_beside_cache: true,
        requires_trace_path: false,
        baseline_ms_floor: 1,
    }
}
