use sqry_core::graph::unified::{EdgeKind, NodeKind, ResolvedVia};
use sqry_db::planner::{Predicate, QueryBuilder, StringPattern};

use super::{
    ComplexityMetric, EntrypointExtension, PathKind, RelationEdgeKind, RuleCycleBounds,
    RuleEdgeClass, RuleEndpoint, RuleNode, RulePlan, RuleSimilarityKind,
};
use crate::backend::SnapshotId;

#[test]
fn rule_ir_declares_nine_extension_variants() {
    let endpoint = RuleEndpoint::Nodes(Vec::new());
    let extension_variants = [
        RuleNode::PathQuery {
            from: endpoint.clone(),
            to: endpoint.clone(),
            kind: PathKind::Calls,
            max_depth: 5,
            max_paths: Some(3),
            avoid: None,
        },
        RuleNode::SubgraphExtract {
            seeds: endpoint.clone(),
            edge_classes: vec![RuleEdgeClass::Call],
            direction: sqry_db::planner::Direction::Forward,
            max_depth: 2,
        },
        RuleNode::RelationEdges {
            from: endpoint.clone(),
            kind: RelationEdgeKind::Callees,
            with_metadata: true,
        },
        RuleNode::CycleWitness {
            edge_class: RuleEdgeClass::Call,
            bounds: RuleCycleBounds::default(),
        },
        RuleNode::ReferencesAt {
            target: endpoint.clone(),
        },
        RuleNode::ComplexityAggregate {
            node_kind_filter: Some(NodeKind::Function),
            metric: ComplexityMetric::OutgoingCalls,
        },
        RuleNode::CrossSnapshotDiff {
            base: SnapshotId {
                edge_revision: 1,
                metadata_revision: 2,
            },
            head: SnapshotId {
                edge_revision: 3,
                metadata_revision: 4,
            },
            include_unchanged: false,
        },
        RuleNode::EntryPointUnion {
            extensions: vec![EntrypointExtension::Name(StringPattern::exact("main"))],
        },
        RuleNode::SimilarTo {
            seed: endpoint,
            scope: None,
            similarity_kind: RuleSimilarityKind::Similar,
        },
    ];

    let reusable_edge_traversal = RuleNode::EdgeTraversal {
        direction: sqry_db::planner::Direction::Forward,
        edge_class: Some(RuleEdgeClass::Call),
        max_depth: 1,
        resolved_via: Some(ResolvedVia::Direct),
        cross_boundary: None,
        emit: crate::ir::TraversalEmit::ReachedNodes,
    };

    assert_eq!(extension_variants.len(), 9);
    assert!(!reusable_edge_traversal.is_beside_cache());
    assert!(extension_variants[6].is_beside_cache());
    assert!(extension_variants[8].is_beside_cache());
}

#[test]
fn set_only_query_plan_converts_to_structurally_equal_rule_ir() {
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::MatchesName(StringPattern::prefix("handle")))
        .build()
        .expect("builder emits a valid plan");

    let emitted = RulePlan::from_query_plan(plan);
    let hand_built = RulePlan::new(RuleNode::Chain {
        steps: vec![
            RuleNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            RuleNode::Filter {
                predicate: Predicate::MatchesName(StringPattern::prefix("handle")),
            },
        ],
    });

    assert_eq!(emitted, hand_built);
}

#[test]
fn cacheable_rule_ir_variants_roundtrip_through_postcard() {
    let endpoint = RuleEndpoint::Nodes(Vec::new());
    let cacheable = [
        RuleNode::NodeScan {
            kind: Some(NodeKind::Function),
            visibility: None,
            name_pattern: Some(StringPattern::contains("parse")),
        },
        RuleNode::PathQuery {
            from: endpoint.clone(),
            to: endpoint.clone(),
            kind: PathKind::Dependency,
            max_depth: 4,
            max_paths: Some(8),
            avoid: None,
        },
        RuleNode::SubgraphExtract {
            seeds: endpoint.clone(),
            edge_classes: vec![RuleEdgeClass::Call, RuleEdgeClass::Import],
            direction: sqry_db::planner::Direction::Both,
            max_depth: 3,
        },
        RuleNode::EdgeTraversal {
            direction: sqry_db::planner::Direction::Forward,
            edge_class: Some(RuleEdgeClass::Call),
            max_depth: 1,
            resolved_via: Some(ResolvedVia::BindingPlane),
            cross_boundary: None,
            emit: crate::ir::TraversalEmit::ReachedNodes,
        },
        RuleNode::RelationEdges {
            from: endpoint.clone(),
            kind: RelationEdgeKind::References,
            with_metadata: false,
        },
        RuleNode::CycleWitness {
            edge_class: RuleEdgeClass::Call,
            bounds: RuleCycleBounds::default(),
        },
        RuleNode::ReferencesAt {
            target: endpoint.clone(),
        },
        RuleNode::ComplexityAggregate {
            node_kind_filter: None,
            metric: ComplexityMetric::NodeCount,
        },
        RuleNode::EntryPointUnion {
            extensions: vec![EntrypointExtension::Path("src/**/*.rs".into())],
        },
    ];

    for node in cacheable {
        let plan = RulePlan::new(node);
        let bytes = postcard::to_allocvec(&plan).expect("serialize rule plan");
        let decoded: RulePlan = postcard::from_bytes(&bytes).expect("deserialize rule plan");
        assert_eq!(decoded, plan);
        assert!(!decoded.root().is_beside_cache());
    }
}

#[test]
fn query_plan_conversion_preserves_traversal_resolved_via_filter() {
    let plan = QueryBuilder::new()
        .scan_all()
        .traverse_with_resolved_via(
            sqry_db::planner::Direction::Forward,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            Some(ResolvedVia::BindingPlane),
            1,
        )
        .build()
        .expect("builder emits a valid traversal plan");

    let emitted = RulePlan::from_query_plan(plan);
    let RuleNode::Chain { steps } = emitted.root() else {
        panic!("query plans convert to a chain");
    };
    let RuleNode::EdgeTraversal { resolved_via, .. } = &steps[1] else {
        panic!("second converted step is the traversal");
    };

    assert_eq!(*resolved_via, Some(ResolvedVia::BindingPlane));
}
