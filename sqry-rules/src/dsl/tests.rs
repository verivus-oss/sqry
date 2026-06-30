use sqry_core::graph::unified::{EdgeKind, NodeId, NodeKind, ResolvedVia};
use sqry_db::planner::{Direction, Predicate, QueryBuilder, SetOperation, StringPattern};

use super::{RuleBuilder, RuleDefinition, RulePack, load_rule_pack_str, load_rule_plan_str};
use crate::backend::SnapshotId;
use crate::ir::{
    ComplexityMetric, EntrypointExtension, PathKind, RelationEdgeKind, RuleCycleBounds,
    RuleEdgeClass, RuleEndpoint, RuleNode, RuleSimilarityKind,
};

fn node(index: u32) -> NodeId {
    NodeId::new(index, 1)
}

#[test]
fn rust_builder_constructs_all_extension_variants() {
    let endpoint = RuleEndpoint::Nodes(vec![node(1)]);
    let plans = [
        RuleBuilder::path_query(
            endpoint.clone(),
            endpoint.clone(),
            PathKind::Calls,
            3,
            Some(2),
        )
        .build()
        .expect("path query"),
        RuleBuilder::subgraph_extract(
            endpoint.clone(),
            vec![RuleEdgeClass::Call],
            Direction::Forward,
            2,
        )
        .build()
        .expect("subgraph extract"),
        RuleBuilder::relation_edges(endpoint.clone(), RelationEdgeKind::Callees, true)
            .build()
            .expect("relation edges"),
        RuleBuilder::cycle_witness(RuleEdgeClass::Call, RuleCycleBounds::default())
            .build()
            .expect("cycle witness"),
        RuleBuilder::references_at(endpoint.clone())
            .build()
            .expect("references at"),
        RuleBuilder::complexity_aggregate(Some(NodeKind::Function), ComplexityMetric::NodeCount)
            .build()
            .expect("complexity aggregate"),
        RuleBuilder::cross_snapshot_diff(
            SnapshotId {
                edge_revision: 1,
                metadata_revision: 1,
            },
            SnapshotId {
                edge_revision: 2,
                metadata_revision: 2,
            },
            false,
        )
        .build()
        .expect("cross snapshot diff"),
        RuleBuilder::entry_point_union(vec![EntrypointExtension::Name(StringPattern::exact(
            "main",
        ))])
        .build()
        .expect("entry point union"),
        RuleBuilder::similar_to(endpoint.clone(), None, RuleSimilarityKind::Duplicate)
            .build()
            .expect("similar to"),
    ];

    assert_eq!(plans.len(), 9);
}

#[test]
fn rust_builder_reuses_set_only_query_plan_shapes() {
    let plan = RuleBuilder::new()
        .scan(NodeKind::Function)
        .filter(Predicate::MatchesName(StringPattern::prefix("handle")))
        .traverse(Direction::Forward, Some(RuleEdgeClass::Call), 1)
        .build()
        .expect("set-only chain");

    let RuleNode::Chain { steps } = plan.root() else {
        panic!("multi-step builder emits chain");
    };

    assert_eq!(steps.len(), 3);
}

#[test]
fn rust_builder_preserves_planner_resolved_via() {
    let planner_plan = QueryBuilder::new()
        .scan_all()
        .traverse_with_resolved_via(
            Direction::Forward,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            Some(ResolvedVia::BindingPlane),
            1,
        )
        .build()
        .expect("planner call traversal");
    let plan = RuleBuilder::from_query_plan(planner_plan)
        .build()
        .expect("rule plan conversion");

    let RuleNode::Chain { steps } = plan.root() else {
        panic!("planner plan conversion emits chain");
    };
    let RuleNode::EdgeTraversal { resolved_via, .. } = &steps[1] else {
        panic!("second step is traversal");
    };

    assert_eq!(*resolved_via, Some(ResolvedVia::BindingPlane));
}

#[test]
fn rust_dsl_and_toml_loader_roundtrip_to_identical_ir() {
    let rust_plan = RuleBuilder::cross_snapshot_diff(
        SnapshotId {
            edge_revision: 11,
            metadata_revision: 12,
        },
        SnapshotId {
            edge_revision: 21,
            metadata_revision: 22,
        },
        true,
    )
    .build()
    .expect("rust DSL plan");
    let pack = RulePack::new(vec![RuleDefinition::new("demo.diff", rust_plan.clone())]);
    let source = toml::to_string(&pack).expect("serialize rule pack");
    let loaded = load_rule_plan_str(&source).expect("load generated TOML");

    assert_eq!(loaded, rust_plan);
}

#[test]
fn fixture_rule_pack_loads_through_toml_schema() {
    let loaded = load_rule_plan_str(include_str!("../../tests/fixtures/round_trip.toml"))
        .expect("load fixture");
    let expected = RuleBuilder::cross_snapshot_diff(
        SnapshotId {
            edge_revision: 11,
            metadata_revision: 12,
        },
        SnapshotId {
            edge_revision: 21,
            metadata_revision: 22,
        },
        true,
    )
    .build()
    .expect("expected fixture plan");

    assert_eq!(loaded, expected);
}

#[test]
fn toml_schema_rejects_unknown_fields_and_empty_packs() {
    let unknown = r#"
schema_version = 1
unknown = true
rules = []
"#;
    assert!(load_rule_pack_str(unknown).is_err());

    let empty = r#"
schema_version = 1
rules = []
"#;
    assert!(load_rule_pack_str(empty).is_err());
}

#[test]
fn builder_rejects_zero_depth_traversals() {
    let result = RuleBuilder::new()
        .scan_all()
        .traverse(Direction::Forward, Some(RuleEdgeClass::Call), 0)
        .build();

    assert!(result.is_err());
}

#[test]
fn set_op_builder_wraps_existing_rule_plans() {
    let left = RuleBuilder::new()
        .scan(NodeKind::Function)
        .build()
        .expect("left plan");
    let right = RuleBuilder::new()
        .scan(NodeKind::Method)
        .build()
        .expect("right plan");
    let plan = RuleBuilder::set_op(SetOperation::Union, left, right)
        .build()
        .expect("set op");

    assert!(matches!(plan.root(), RuleNode::SetOp { .. }));
}
