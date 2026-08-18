//! TC3: Rule IR construction, serde round-trip, and DSL structural equality.
//!
//! Constructs every `RuleNode` variant, postcard round-trips each as a
//! persisted `RulePlan`, and asserts a DSL-emitted plan is structurally equal
//! to the hand-built IR it lowers to.

use sqry_core::graph::unified::{NodeKind, ResolvedVia};
use sqry_db::planner::{Direction, Predicate, QueryBuilder, SetOperation, StringPattern};

use sqry_rules::ir::{
    ComplexityMetric, EntrypointExtension, PathKind, RelationEdgeKind, RuleCycleBounds,
    RuleEdgeClass, RuleEndpoint, RuleSimilarityKind,
};
use sqry_rules::{RuleBuilder, RuleNode, RulePlan, SnapshotId};

fn all_rule_nodes() -> Vec<RuleNode> {
    let endpoint = RuleEndpoint::Nodes(Vec::new());
    let snapshot = SnapshotId {
        edge_revision: 1,
        metadata_revision: 2,
    };
    vec![
        RuleNode::NodeScan {
            kind: Some(NodeKind::Function),
            visibility: None,
            name_pattern: Some(StringPattern::contains("parse")),
        },
        RuleNode::EdgeTraversal {
            direction: Direction::Forward,
            edge_class: Some(RuleEdgeClass::Call),
            max_depth: 2,
            resolved_via: Some(ResolvedVia::Direct),
            cross_boundary: Some(true),
            emit: sqry_rules::ir::TraversalEmit::EdgeSources,
        },
        RuleNode::Filter {
            predicate: Predicate::IsUnused,
        },
        RuleNode::SetOp {
            op: SetOperation::Union,
            left: Box::new(RuleNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern: Some(StringPattern::exact("a")),
            }),
            right: Box::new(RuleNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern: Some(StringPattern::exact("b")),
            }),
        },
        RuleNode::Chain {
            steps: vec![RuleNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern: None,
            }],
        },
        RuleNode::PathQuery {
            from: endpoint.clone(),
            to: endpoint.clone(),
            kind: PathKind::Calls,
            max_depth: 5,
            max_paths: Some(3),
            avoid: Some(endpoint.clone()),
        },
        RuleNode::SubgraphExtract {
            seeds: endpoint.clone(),
            edge_classes: vec![RuleEdgeClass::Call, RuleEdgeClass::Import],
            direction: Direction::Both,
            max_depth: 3,
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
            base: snapshot,
            head: snapshot,
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
    ]
}

#[test]
fn every_rule_node_variant_is_constructible() {
    assert_eq!(
        all_rule_nodes().len(),
        14,
        "the full RuleNode vocabulary is fourteen variants"
    );
}

#[test]
fn every_rule_plan_round_trips_through_postcard() {
    for node in all_rule_nodes() {
        let plan = RulePlan::new(node);
        let bytes = postcard::to_allocvec(&plan).expect("serialize rule plan");
        let decoded: RulePlan = postcard::from_bytes(&bytes).expect("deserialize rule plan");
        assert_eq!(decoded, plan);
    }
}

#[test]
fn dsl_emitted_plan_is_structurally_equal_to_hand_built_ir() {
    let emitted = RuleBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::MatchesName(StringPattern::prefix("handle")))
        .build()
        .expect("builder emits a valid plan");

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
fn set_only_query_plan_lowers_into_rule_ir() {
    let plan = QueryBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::MatchesName(StringPattern::prefix("handle")))
        .build()
        .expect("planner builds a set-only plan");

    let lowered = RulePlan::from_query_plan(plan);
    assert!(matches!(lowered.root(), RuleNode::Chain { .. }));
}
