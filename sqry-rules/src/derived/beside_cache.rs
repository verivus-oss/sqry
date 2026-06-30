//! Beside-cache routing metadata for non-single-snapshot rule primitives.

use serde::{Deserialize, Serialize};

use crate::ir::{RuleNode, RuleSimilarityKind};

/// Beside-cache primitive selected for a non-`DerivedQuery` rule variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BesideCachePrimitive {
    /// Cross-snapshot semantic diff through `ComparativeQueryDb`.
    ComparativeDiff,
    /// Duplicate search via the MCP-layer duplicate executor.
    FindDuplicates,
    /// Similar-neighbour search via the MCP-layer similarity executor.
    FindSimilar,
}

/// Route metadata for rule variants that cannot be represented as a
/// single-snapshot `DerivedQuery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BesideCacheRoute {
    /// Primitive the higher-level coordinator must call.
    pub primitive: BesideCachePrimitive,
    /// Stable rule IR variant name.
    pub variant: &'static str,
    /// Whether this route spans multiple graph snapshots.
    pub is_cross_snapshot: bool,
}

/// Returns the beside-cache route for non-cacheable rule variants.
#[must_use]
pub fn beside_cache_route_for(node: &RuleNode) -> Option<BesideCacheRoute> {
    match node {
        RuleNode::CrossSnapshotDiff { .. } => Some(BesideCacheRoute {
            primitive: BesideCachePrimitive::ComparativeDiff,
            variant: "cross_snapshot_diff",
            is_cross_snapshot: true,
        }),
        RuleNode::SimilarTo {
            similarity_kind, ..
        } => Some(BesideCacheRoute {
            primitive: match similarity_kind {
                RuleSimilarityKind::Duplicate => BesideCachePrimitive::FindDuplicates,
                RuleSimilarityKind::Similar => BesideCachePrimitive::FindSimilar,
            },
            variant: "similar_to",
            is_cross_snapshot: false,
        }),
        RuleNode::NodeScan { .. }
        | RuleNode::EdgeTraversal { .. }
        | RuleNode::Filter { .. }
        | RuleNode::SetOp { .. }
        | RuleNode::Chain { .. }
        | RuleNode::PathQuery { .. }
        | RuleNode::SubgraphExtract { .. }
        | RuleNode::RelationEdges { .. }
        | RuleNode::CycleWitness { .. }
        | RuleNode::ReferencesAt { .. }
        | RuleNode::ComplexityAggregate { .. }
        | RuleNode::EntryPointUnion { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SnapshotId;
    use crate::ir::{RuleEndpoint, RuleSimilarityKind};
    use sqry_core::graph::unified::NodeId;

    #[test]
    fn cross_snapshot_and_similarity_routes_are_beside_cache_only() {
        let diff = RuleNode::CrossSnapshotDiff {
            base: SnapshotId {
                edge_revision: 1,
                metadata_revision: 1,
            },
            head: SnapshotId {
                edge_revision: 2,
                metadata_revision: 2,
            },
            include_unchanged: false,
        };
        let duplicate = RuleNode::SimilarTo {
            seed: RuleEndpoint::Nodes(vec![NodeId::new(1, 1)]),
            scope: None,
            similarity_kind: RuleSimilarityKind::Duplicate,
        };
        let similar = RuleNode::SimilarTo {
            seed: RuleEndpoint::Nodes(vec![NodeId::new(1, 1)]),
            scope: None,
            similarity_kind: RuleSimilarityKind::Similar,
        };

        assert_eq!(
            beside_cache_route_for(&diff).map(|route| route.primitive),
            Some(BesideCachePrimitive::ComparativeDiff)
        );
        assert_eq!(
            beside_cache_route_for(&duplicate).map(|route| route.primitive),
            Some(BesideCachePrimitive::FindDuplicates)
        );
        assert_eq!(
            beside_cache_route_for(&similar).map(|route| route.primitive),
            Some(BesideCachePrimitive::FindSimilar)
        );
    }
}
