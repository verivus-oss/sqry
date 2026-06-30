use super::{
    BesideCachePrimitive, CacheableRuleVariant, beside_cache_route_for, cacheable_rule_query_specs,
};
use crate::backend::SnapshotId;
use crate::ir::{RuleEndpoint, RuleNode, RuleSimilarityKind};
use sqry_core::graph::unified::NodeId;

#[test]
fn p5u07_public_metadata_separates_cacheable_and_beside_cache_variants() {
    let cacheable_variants: Vec<_> = cacheable_rule_query_specs()
        .iter()
        .map(|spec| spec.variant)
        .collect();
    assert_eq!(
        cacheable_variants,
        vec![
            CacheableRuleVariant::PathQuery,
            CacheableRuleVariant::SubgraphExtract,
            CacheableRuleVariant::RelationEdges,
            CacheableRuleVariant::CycleWitness,
            CacheableRuleVariant::ReferencesAt,
            CacheableRuleVariant::ComplexityAggregate,
            CacheableRuleVariant::EntryPointUnion,
        ]
    );

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
        beside_cache_route_for(&similar).map(|route| route.primitive),
        Some(BesideCachePrimitive::FindSimilar)
    );
}
