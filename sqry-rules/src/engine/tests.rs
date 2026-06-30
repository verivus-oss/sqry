use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::sync::Arc;

use sqry_core::graph::unified::bind::BindingPlane;
use sqry_core::graph::unified::{
    CodeGraph, EdgeClassification, EdgeFilter, GraphSnapshot, MaterializedEdge, MaterializedNode,
    NodeId, TraversalDirection, TraversalLimits, TraversalMetadata, TraversalResult,
};
use sqry_db::ComparativeQueryDb;
use sqry_db::planner::{Direction, QueryPlan, SetOperation, StringPattern};
use sqry_db::queries::{
    CachedCondensation, CachedSccData, CondensationKey, CondensationValue, CyclesKey, CyclesValue,
    ReachableSet, RelationKey, SccValue, UnusedKey, UnusedValue,
};

use crate::backend::{
    CycleClass, RuleBackend, RulePath, RuleReachabilityKey, RuleTopologyKey, SnapshotId,
    TracePathKey,
};
use crate::engine::{
    RuleCancellationToken, RuleEngine, RuleEngineConfig, RuleOutput, RuleRelationRows,
};
use crate::ir::{
    ComplexityMetric, EntrypointExtension, PathKind, RelationEdgeKind, RuleCycleBounds,
    RuleEdgeClass, RuleEndpoint, RuleNode, RulePlan, RuleSimilarityKind,
};
use crate::witness::{RuleSeverity, RuleStep};
use crate::{RuleError, RuleResult};

const NODE_A: NodeId = NodeId::new(1, 1);
const NODE_B: NodeId = NodeId::new(2, 1);

#[test]
fn node_scan_routes_through_run_plan_and_returns_witness() {
    let backend = RecordingBackend::new(vec![NODE_A]);
    let plan = RulePlan::new(RuleNode::NodeScan {
        kind: None,
        visibility: None,
        name_pattern: Some(StringPattern::exact("alpha")),
    });

    let run = RuleEngine::new().run(&backend, &plan).expect("node scan");

    assert_eq!(backend.run_plan_calls.get(), 1);
    assert_eq!(run.output, RuleOutput::Nodes(vec![NODE_A]));
    assert!(
        run.witness
            .steps
            .iter()
            .any(|step| matches!(step, RuleStep::NodeScanMatched { match_count: 1, .. }))
    );
}

#[test]
fn named_run_records_rule_metadata_in_witness() {
    let backend = RecordingBackend::new(vec![NODE_A]);
    let plan = RulePlan::new(RuleNode::NodeScan {
        kind: None,
        visibility: None,
        name_pattern: Some(StringPattern::exact("alpha")),
    });

    let run = RuleEngine::new()
        .run_named(&backend, &plan, "rule.demo", RuleSeverity::Warning)
        .expect("named rule run");

    assert!(run.witness.steps.iter().any(|step| {
        matches!(
            step,
            RuleStep::RuleFired {
                rule_id,
                severity: RuleSeverity::Warning,
            } if rule_id == "rule.demo"
        )
    }));
}

#[test]
fn set_op_routes_through_planner_backend() {
    let backend = RecordingBackend::new(vec![NODE_A]);
    let scan = || RuleNode::NodeScan {
        kind: None,
        visibility: None,
        name_pattern: Some(StringPattern::exact("alpha")),
    };
    let plan = RulePlan::new(RuleNode::SetOp {
        op: SetOperation::Union,
        left: Box::new(scan()),
        right: Box::new(scan()),
    });

    let run = RuleEngine::new().run(&backend, &plan).expect("set op");

    assert_eq!(backend.run_plan_calls.get(), 1);
    assert_eq!(backend.traverse_calls.get(), 0);
    assert_eq!(run.output, RuleOutput::Nodes(vec![NODE_A]));
}

#[test]
fn planner_only_chain_routes_through_run_plan_without_manual_traverse() {
    let backend = RecordingBackend::new(vec![NODE_A]);
    let plan = RulePlan::new(RuleNode::Chain {
        steps: vec![
            RuleNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern: Some(StringPattern::exact("alpha")),
            },
            RuleNode::EdgeTraversal {
                direction: Direction::Forward,
                edge_class: None,
                max_depth: 1,
                resolved_via: None,
            },
        ],
    });

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("planner chain");

    assert_eq!(backend.run_plan_calls.get(), 1);
    assert_eq!(backend.traverse_calls.get(), 0);
    assert_eq!(run.output, RuleOutput::Nodes(vec![NODE_A]));
}

#[test]
fn path_query_routes_through_trace_path_and_records_path_witness() {
    let backend =
        RecordingBackend::new(Vec::new()).with_paths(vec![RulePath::new(vec![NODE_A, NODE_B])]);
    let plan = RulePlan::new(RuleNode::PathQuery {
        from: RuleEndpoint::Nodes(vec![NODE_A]),
        to: RuleEndpoint::Nodes(vec![NODE_B]),
        kind: PathKind::Calls,
        max_depth: 4,
        max_paths: Some(3),
    });

    let run = RuleEngine::new().run(&backend, &plan).expect("path query");

    assert_eq!(backend.trace_path_calls.get(), 1);
    assert!(matches!(run.output, RuleOutput::Paths(paths) if paths.len() == 1));
    assert!(run.witness.steps.iter().any(|step| match step {
        RuleStep::PathConstructed {
            from, to, length, ..
        } => *from == NODE_A && *to == NODE_B && *length == 1,
        _ => false,
    }));
}

#[test]
fn relation_edges_route_to_relation_backend() {
    let backend = RecordingBackend::new(vec![NODE_B]);
    let plan = RulePlan::new(RuleNode::RelationEdges {
        from: RuleEndpoint::Query(Box::new(RuleNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: Some(StringPattern::exact("alpha")),
        })),
        kind: RelationEdgeKind::Callers,
        with_metadata: true,
    });

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("relation edges");

    assert_eq!(backend.relation_from_node_calls.get(), 1);
    assert_eq!(
        run.output,
        RuleOutput::Relations(RuleRelationRows {
            kind: RelationEdgeKind::Callers,
            nodes: vec![NODE_B],
            with_metadata: true,
        })
    );
}

#[test]
fn relation_edges_route_each_relation_kind_to_expected_backend_method() {
    let backend = RecordingBackend::new(vec![NODE_B]);
    for kind in [
        RelationEdgeKind::Callees,
        RelationEdgeKind::Imports,
        RelationEdgeKind::Exports,
        RelationEdgeKind::References,
        RelationEdgeKind::Implements,
    ] {
        let plan = RulePlan::new(RuleNode::RelationEdges {
            from: RuleEndpoint::Query(Box::new(RuleNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern: Some(StringPattern::exact("alpha")),
            })),
            kind,
            with_metadata: false,
        });

        let run = RuleEngine::new()
            .run(&backend, &plan)
            .expect("relation edge kind");

        assert_eq!(
            run.output,
            RuleOutput::Relations(RuleRelationRows {
                kind,
                nodes: vec![NODE_B],
                with_metadata: false,
            })
        );
    }

    assert_eq!(backend.relation_from_node_calls.get(), 5);
    assert_eq!(
        backend.references_calls.get(),
        0,
        "implements must not route through the references backend"
    );
}

#[test]
fn explicit_node_relation_witness_preserves_source_and_target() {
    let backend = RecordingBackend::new(vec![NODE_B]);
    let plan = RulePlan::new(RuleNode::RelationEdges {
        from: RuleEndpoint::Nodes(vec![NODE_A]),
        kind: RelationEdgeKind::Callees,
        with_metadata: true,
    });

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("node relation edge");

    assert!(run.witness.steps.iter().any(|step| {
        matches!(
            step,
            RuleStep::RelationEdgeEmitted {
                from,
                to,
                kind: RuleEdgeClass::Call,
                with_metadata: true,
            } if *from == NODE_A && *to == NODE_B
        )
    }));
}

#[test]
fn subgraph_extract_routes_through_traverse_with_rule_edge_filter() {
    let backend = RecordingBackend::new(Vec::new());
    let plan = RulePlan::new(RuleNode::SubgraphExtract {
        seeds: RuleEndpoint::Nodes(vec![NODE_A]),
        edge_classes: vec![RuleEdgeClass::Call],
        direction: Direction::Forward,
        max_depth: 2,
    });

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("subgraph extract");

    assert_eq!(backend.traverse_calls.get(), 1);
    assert!(matches!(
        run.output,
        RuleOutput::Subgraph {
            nodes,
            edge_count: 1
        } if nodes == vec![NODE_A, NODE_B]
    ));
    assert!(
        backend
            .last_traversal_filter
            .borrow()
            .as_ref()
            .is_some_and(|filter| filter.include_calls && !filter.include_imports)
    );
}

#[test]
fn edge_chain_routes_rule_edge_traversal_through_traverse() {
    let backend = RecordingBackend::new(vec![NODE_A]);
    let plan = RulePlan::new(RuleNode::Chain {
        steps: vec![
            RuleNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern: Some(StringPattern::exact("alpha")),
            },
            RuleNode::EdgeTraversal {
                direction: Direction::Forward,
                edge_class: Some(RuleEdgeClass::Call),
                max_depth: 1,
                resolved_via: None,
            },
        ],
    });

    let run = RuleEngine::new().run(&backend, &plan).expect("chain");

    assert_eq!(backend.run_plan_calls.get(), 1);
    assert_eq!(backend.traverse_calls.get(), 1);
    assert_eq!(run.output, RuleOutput::Nodes(vec![NODE_A, NODE_B]));
}

#[test]
fn cycle_witness_routes_through_cycle_backend() {
    let backend = RecordingBackend::new(Vec::new());
    let plan = RulePlan::new(RuleNode::CycleWitness {
        edge_class: RuleEdgeClass::Call,
        bounds: RuleCycleBounds::default(),
    });

    let run = RuleEngine::new().run(&backend, &plan).expect("cycles");

    assert_eq!(backend.cycles_calls.get(), 1);
    assert_eq!(run.output, RuleOutput::Cycles(vec![vec![NODE_A, NODE_B]]));
    assert!(
        run.witness
            .steps
            .iter()
            .any(|step| matches!(step, RuleStep::CycleDetected { length: 2, .. }))
    );
}

#[test]
fn references_at_routes_through_reference_backend() {
    let backend = RecordingBackend::new(vec![NODE_B]);
    let plan = RulePlan::new(RuleNode::ReferencesAt {
        target: RuleEndpoint::Nodes(vec![NODE_A]),
    });

    let run = RuleEngine::new().run(&backend, &plan).expect("references");

    assert_eq!(backend.relation_from_node_calls.get(), 1);
    assert_eq!(run.output, RuleOutput::References(vec![NODE_B]));
    assert!(run.witness.steps.iter().any(|step| {
        matches!(
            step,
            RuleStep::ReferenceLocated { source, target, .. }
                if *source == NODE_B && *target == NODE_A
        )
    }));
}

#[test]
fn complexity_aggregate_routes_through_planner_and_relation_backend() {
    let backend = RecordingBackend::new(vec![NODE_A, NODE_B]);
    let plan = RulePlan::new(RuleNode::ComplexityAggregate {
        node_kind_filter: None,
        metric: ComplexityMetric::OutgoingCalls,
    });

    let run = RuleEngine::new().run(&backend, &plan).expect("complexity");

    assert_eq!(backend.run_plan_calls.get(), 1);
    assert_eq!(backend.relation_from_node_calls.get(), 2);
    assert!(matches!(
        run.output,
        RuleOutput::Metrics(metrics)
            if metrics.len() == 1
                && metrics[0].metric == ComplexityMetric::OutgoingCalls
                && metrics[0].value == 4
                && metrics[0].node_count == 2
    ));
}

#[test]
fn complexity_aggregate_routes_all_metric_arms() {
    let backend = RecordingBackend::new(vec![NODE_A, NODE_B]);

    let node_count_run = RuleEngine::new()
        .run(
            &backend,
            &RulePlan::new(RuleNode::ComplexityAggregate {
                node_kind_filter: None,
                metric: ComplexityMetric::NodeCount,
            }),
        )
        .expect("node count");
    let incoming_run = RuleEngine::new()
        .run(
            &backend,
            &RulePlan::new(RuleNode::ComplexityAggregate {
                node_kind_filter: None,
                metric: ComplexityMetric::IncomingCalls,
            }),
        )
        .expect("incoming calls");

    assert!(matches!(
        node_count_run.output,
        RuleOutput::Metrics(metrics)
            if metrics.len() == 1
                && metrics[0].metric == ComplexityMetric::NodeCount
                && metrics[0].value == 2
    ));
    assert!(matches!(
        incoming_run.output,
        RuleOutput::Metrics(metrics)
            if metrics.len() == 1
                && metrics[0].metric == ComplexityMetric::IncomingCalls
                && metrics[0].value == 4
    ));
    assert_eq!(backend.run_plan_calls.get(), 2);
    assert_eq!(backend.relation_from_node_calls.get(), 2);
}

#[test]
fn cross_snapshot_diff_routes_through_comparative_backend() {
    let backend = RecordingBackend::new(Vec::new());
    let plan = RulePlan::new(RuleNode::CrossSnapshotDiff {
        base: backend.snapshot_id(),
        head: backend.snapshot_id(),
        include_unchanged: true,
    });

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("cross snapshot diff");

    assert_eq!(backend.comparative_calls.get(), 1);
    assert_eq!(run.output, RuleOutput::DiffEntries(Vec::new()));
    assert!(run.witness.steps.iter().any(|step| {
        matches!(
            step,
            RuleStep::DiffEntryEmitted {
                kind: crate::witness::DiffEntryKind::Unchanged,
                ..
            }
        )
    }));
}

#[test]
fn entry_point_union_routes_through_entry_points_and_extensions() {
    let backend = RecordingBackend::new(Vec::new()).with_entry_points([NODE_A]);
    let plan = RulePlan::new(RuleNode::EntryPointUnion {
        extensions: vec![EntrypointExtension::Nodes(vec![NODE_B])],
    });

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("entry points");

    assert_eq!(backend.entry_points_calls.get(), 1);
    assert!(matches!(run.output, RuleOutput::EntryPoints(nodes) if nodes == vec![NODE_A, NODE_B]));
}

#[test]
fn cancellation_is_checked_before_backend_dispatch() {
    let backend = RecordingBackend::new(vec![NODE_A]);
    let plan = RulePlan::new(RuleNode::NodeScan {
        kind: None,
        visibility: None,
        name_pattern: None,
    });
    let cancellation = AlwaysCancelled;

    let error = RuleEngine::new()
        .run_with_cancellation(&backend, &plan, &cancellation)
        .expect_err("cancelled");

    assert!(matches!(error, RuleError::ExecutionCancelled));
    assert_eq!(backend.run_plan_calls.get(), 0);
}

#[test]
fn cancellation_is_rechecked_inside_relation_dispatch_before_backend_work() {
    let backend = RecordingBackend::new(vec![NODE_B]);
    let cancellation = CancelsAfterFirstCheck::default();
    let plan = RulePlan::new(RuleNode::RelationEdges {
        from: RuleEndpoint::Nodes(vec![NODE_A]),
        kind: RelationEdgeKind::Callers,
        with_metadata: false,
    });

    let error = RuleEngine::new()
        .run_with_cancellation(&backend, &plan, &cancellation)
        .expect_err("cancelled after root check");

    assert!(matches!(error, RuleError::ExecutionCancelled));
    assert_eq!(backend.callers_calls.get(), 0);
}

#[test]
fn cancellation_is_checked_before_entry_point_backend_work() {
    let backend = RecordingBackend::new(Vec::new()).with_entry_points([NODE_A]);
    let cancellation = CancelsAfterFirstCheck::default();
    let plan = RulePlan::new(RuleNode::EntryPointUnion {
        extensions: vec![EntrypointExtension::Nodes(vec![NODE_B])],
    });

    let error = RuleEngine::new()
        .run_with_cancellation(&backend, &plan, &cancellation)
        .expect_err("cancelled before entry points");

    assert!(matches!(error, RuleError::ExecutionCancelled));
    assert_eq!(backend.entry_points_calls.get(), 0);
}

#[test]
fn cancellation_is_rechecked_before_cycle_reference_complexity_and_diff_backend_work() {
    let cycle_backend = RecordingBackend::new(Vec::new());
    let error = RuleEngine::new()
        .run_with_cancellation(
            &cycle_backend,
            &RulePlan::new(RuleNode::CycleWitness {
                edge_class: RuleEdgeClass::Call,
                bounds: RuleCycleBounds::default(),
            }),
            &CancelsAfterFirstCheck::default(),
        )
        .expect_err("cycle cancellation");
    assert!(matches!(error, RuleError::ExecutionCancelled));
    assert_eq!(cycle_backend.cycles_calls.get(), 0);

    let references_backend = RecordingBackend::new(vec![NODE_B]);
    let error = RuleEngine::new()
        .run_with_cancellation(
            &references_backend,
            &RulePlan::new(RuleNode::ReferencesAt {
                target: RuleEndpoint::Nodes(vec![NODE_A]),
            }),
            &CancelsAfterFirstCheck::default(),
        )
        .expect_err("references cancellation");
    assert!(matches!(error, RuleError::ExecutionCancelled));
    assert_eq!(references_backend.references_calls.get(), 0);

    let complexity_backend = RecordingBackend::new(vec![NODE_A]);
    let error = RuleEngine::new()
        .run_with_cancellation(
            &complexity_backend,
            &RulePlan::new(RuleNode::ComplexityAggregate {
                node_kind_filter: None,
                metric: ComplexityMetric::OutgoingCalls,
            }),
            &CancelsAfterFirstCheck::default(),
        )
        .expect_err("complexity cancellation");
    assert!(matches!(error, RuleError::ExecutionCancelled));
    assert_eq!(complexity_backend.run_plan_calls.get(), 0);
    assert_eq!(complexity_backend.callees_calls.get(), 0);

    let diff_backend = RecordingBackend::new(Vec::new());
    let error = RuleEngine::new()
        .run_with_cancellation(
            &diff_backend,
            &RulePlan::new(RuleNode::CrossSnapshotDiff {
                base: diff_backend.snapshot_id(),
                head: diff_backend.snapshot_id(),
                include_unchanged: false,
            }),
            &CancelsAfterFirstCheck::default(),
        )
        .expect_err("cross-snapshot cancellation");
    assert!(matches!(error, RuleError::ExecutionCancelled));
    assert_eq!(diff_backend.comparative_calls.get(), 0);
}

#[test]
fn malformed_filter_root_returns_error_instead_of_panicking() {
    let backend = RecordingBackend::new(Vec::new());
    let plan = RulePlan::new(RuleNode::Filter {
        predicate: sqry_db::planner::Predicate::HasCaller,
    });

    let error = RuleEngine::new().run(&backend, &plan).expect_err("invalid");

    assert!(matches!(error, RuleError::InvalidRuleSource { .. }));
}

#[test]
fn malformed_ir_returns_typed_errors_for_dispatch_boundaries() {
    let backend = RecordingBackend::new(vec![NODE_A]);

    assert!(matches!(
        RuleEngine::new()
            .run(
                &backend,
                &RulePlan::new(RuleNode::EdgeTraversal {
                    direction: Direction::Forward,
                    edge_class: None,
                    max_depth: 1,
                    resolved_via: None,
                }),
            )
            .expect_err("root edge traversal"),
        RuleError::InvalidRuleSource { .. }
    ));
    assert!(matches!(
        RuleEngine::new()
            .run(
                &backend,
                &RulePlan::new(RuleNode::PathQuery {
                    from: RuleEndpoint::Nodes(vec![NODE_A]),
                    to: RuleEndpoint::Nodes(vec![NODE_B]),
                    kind: PathKind::Calls,
                    max_depth: 0,
                    max_paths: None,
                }),
            )
            .expect_err("zero depth path"),
        RuleError::InvalidRuleSource { .. }
    ));
    assert!(matches!(
        RuleEngine::new()
            .run(
                &backend,
                &RulePlan::new(RuleNode::SubgraphExtract {
                    seeds: RuleEndpoint::Nodes(vec![NODE_A]),
                    edge_classes: vec![RuleEdgeClass::Call],
                    direction: Direction::Forward,
                    max_depth: 0,
                }),
            )
            .expect_err("zero depth subgraph"),
        RuleError::InvalidRuleSource { .. }
    ));
    assert!(matches!(
        RuleEngine::new()
            .run(
                &backend,
                &RulePlan::new(RuleNode::Chain {
                    steps: vec![
                        RuleNode::NodeScan {
                            kind: None,
                            visibility: None,
                            name_pattern: Some(StringPattern::exact("alpha")),
                        },
                        RuleNode::EdgeTraversal {
                            direction: Direction::Forward,
                            edge_class: Some(RuleEdgeClass::Call),
                            max_depth: 0,
                            resolved_via: None,
                        },
                    ],
                }),
            )
            .expect_err("zero depth edge traversal"),
        RuleError::InvalidRuleSource { .. }
    ));
    assert!(matches!(
        RuleEngine::new()
            .run(
                &backend,
                &RulePlan::new(RuleNode::RelationEdges {
                    from: RuleEndpoint::Query(Box::new(RuleNode::ComplexityAggregate {
                        node_kind_filter: None,
                        metric: ComplexityMetric::NodeCount,
                    })),
                    kind: RelationEdgeKind::Callers,
                    with_metadata: false,
                }),
            )
            .expect_err("invalid relation endpoint"),
        RuleError::InvalidRuleSource { .. }
    ));
    assert!(matches!(
        RuleEngine::new()
            .run(
                &backend,
                &RulePlan::new(RuleNode::CycleWitness {
                    edge_class: RuleEdgeClass::Reference,
                    bounds: RuleCycleBounds::default(),
                }),
            )
            .expect_err("unsupported cycle class"),
        RuleError::UnsupportedPrimitive {
            primitive: "cycle_witness",
            ..
        }
    ));
    assert!(matches!(
        RuleEngine::new()
            .run(
                &backend,
                &RulePlan::new(RuleNode::PathQuery {
                    from: RuleEndpoint::Query(Box::new(RuleNode::PathQuery {
                        from: RuleEndpoint::Nodes(vec![NODE_A]),
                        to: RuleEndpoint::Nodes(vec![NODE_B]),
                        kind: PathKind::Calls,
                        max_depth: 1,
                        max_paths: None,
                    })),
                    to: RuleEndpoint::Nodes(vec![NODE_B]),
                    kind: PathKind::Calls,
                    max_depth: 1,
                    max_paths: None,
                }),
            )
            .expect_err("non-node endpoint query"),
        RuleError::InvalidRuleSource { .. }
    ));
}

#[test]
fn similar_to_reports_explicit_unsupported_primitive_until_beside_cache_lands() {
    let backend = RecordingBackend::new(vec![NODE_A]);
    let plan = RulePlan::new(RuleNode::SimilarTo {
        seed: RuleEndpoint::Nodes(vec![NODE_A]),
        scope: None,
        similarity_kind: RuleSimilarityKind::Similar,
    });

    let error = RuleEngine::new()
        .run(&backend, &plan)
        .expect_err("unsupported");

    assert!(matches!(
        error,
        RuleError::UnsupportedPrimitive {
            primitive: "similar_to",
            ..
        }
    ));
}

#[test]
fn witness_step_cap_is_applied_to_engine_output() {
    let backend = RecordingBackend::new(Vec::new()).with_paths(vec![
        RulePath::new(vec![NODE_A, NODE_B]),
        RulePath::new(vec![NODE_A, NODE_B]),
        RulePath::new(vec![NODE_A, NODE_B]),
    ]);
    let engine = RuleEngine::with_config(RuleEngineConfig {
        witness_step_cap: 2,
    });
    let plan = RulePlan::new(RuleNode::PathQuery {
        from: RuleEndpoint::Nodes(vec![NODE_A]),
        to: RuleEndpoint::Nodes(vec![NODE_B]),
        kind: PathKind::Calls,
        max_depth: 4,
        max_paths: None,
    });

    let run = engine.run(&backend, &plan).expect("path query");

    assert!(run.witness.truncated);
    assert_eq!(run.witness.steps.len(), 2);
    assert!(matches!(
        run.witness.steps.last(),
        Some(RuleStep::WitnessTruncated { .. })
    ));
}

struct AlwaysCancelled;

impl RuleCancellationToken for AlwaysCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[derive(Default)]
struct CancelsAfterFirstCheck {
    checks: Cell<usize>,
}

impl RuleCancellationToken for CancelsAfterFirstCheck {
    fn is_cancelled(&self) -> bool {
        let checks = self.checks.get();
        self.checks.set(checks + 1);
        checks > 0
    }
}

struct RecordingBackend {
    snapshot: Arc<GraphSnapshot>,
    nodes: Arc<Vec<NodeId>>,
    set: Arc<HashSet<NodeId>>,
    paths: Arc<Vec<RulePath>>,
    run_plan_calls: Cell<usize>,
    traverse_calls: Cell<usize>,
    trace_path_calls: Cell<usize>,
    callers_calls: Cell<usize>,
    callees_calls: Cell<usize>,
    imports_calls: Cell<usize>,
    exports_calls: Cell<usize>,
    references_calls: Cell<usize>,
    relation_from_node_calls: Cell<usize>,
    cycles_calls: Cell<usize>,
    entry_points_calls: Cell<usize>,
    comparative_calls: Cell<usize>,
    last_traversal_filter: RefCell<Option<EdgeFilter>>,
}

impl RecordingBackend {
    fn new(nodes: Vec<NodeId>) -> Self {
        let graph = CodeGraph::new();
        Self {
            snapshot: Arc::new(graph.snapshot()),
            nodes: Arc::new(nodes),
            set: Arc::new(HashSet::new()),
            paths: Arc::new(Vec::new()),
            run_plan_calls: Cell::new(0),
            traverse_calls: Cell::new(0),
            trace_path_calls: Cell::new(0),
            callers_calls: Cell::new(0),
            callees_calls: Cell::new(0),
            imports_calls: Cell::new(0),
            exports_calls: Cell::new(0),
            references_calls: Cell::new(0),
            relation_from_node_calls: Cell::new(0),
            cycles_calls: Cell::new(0),
            entry_points_calls: Cell::new(0),
            comparative_calls: Cell::new(0),
            last_traversal_filter: RefCell::new(None),
        }
    }

    fn with_paths(mut self, paths: Vec<RulePath>) -> Self {
        self.paths = Arc::new(paths);
        self
    }

    fn with_entry_points<const N: usize>(mut self, nodes: [NodeId; N]) -> Self {
        self.set = Arc::new(nodes.into_iter().collect());
        self
    }
}

impl RuleBackend for RecordingBackend {
    fn snapshot_id(&self) -> SnapshotId {
        SnapshotId {
            edge_revision: 1,
            metadata_revision: 1,
        }
    }

    fn binding(&self) -> BindingPlane<'_> {
        self.snapshot.binding_plane()
    }

    fn traverse(
        &self,
        _seeds: &[NodeId],
        _direction: TraversalDirection,
        edge_filter: EdgeFilter,
        _limits: TraversalLimits,
    ) -> RuleResult<TraversalResult> {
        self.traverse_calls.set(self.traverse_calls.get() + 1);
        self.last_traversal_filter.replace(Some(edge_filter));
        Ok(TraversalResult {
            nodes: vec![
                materialized_node(NODE_A, "alpha"),
                materialized_node(NODE_B, "beta"),
            ],
            edges: vec![MaterializedEdge {
                source_idx: 0,
                target_idx: 1,
                classification: EdgeClassification::Call {
                    is_async: false,
                    is_cross_boundary: false,
                },
                raw_kind: sqry_core::graph::unified::EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: sqry_core::graph::unified::ResolvedVia::Direct,
                },
                depth: 1,
            }],
            paths: None,
            metadata: TraversalMetadata {
                truncation: None,
                max_depth_reached: false,
                seed_count: 1,
                nodes_visited: 2,
                total_nodes: 2,
                total_edges: 1,
            },
        })
    }

    fn callers(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        self.callers_calls.set(self.callers_calls.get() + 1);
        Ok(Arc::clone(&self.nodes))
    }

    fn callees(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        self.callees_calls.set(self.callees_calls.get() + 1);
        Ok(Arc::clone(&self.nodes))
    }

    fn imports(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        self.imports_calls.set(self.imports_calls.get() + 1);
        Ok(Arc::clone(&self.nodes))
    }

    fn exports(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        self.exports_calls.set(self.exports_calls.get() + 1);
        Ok(Arc::clone(&self.nodes))
    }

    fn references(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        self.references_calls.set(self.references_calls.get() + 1);
        Ok(Arc::clone(&self.nodes))
    }

    fn relation_from_node(
        &self,
        _node: NodeId,
        _kind: RelationEdgeKind,
    ) -> RuleResult<Arc<Vec<NodeId>>> {
        self.relation_from_node_calls
            .set(self.relation_from_node_calls.get() + 1);
        Ok(Arc::clone(&self.nodes))
    }

    fn cycles(&self, _key: CyclesKey) -> RuleResult<CyclesValue> {
        self.cycles_calls.set(self.cycles_calls.get() + 1);
        Ok(Arc::new(vec![vec![NODE_A, NODE_B]]))
    }

    fn is_in_cycle(&self, _node: NodeId, _cycle_class: CycleClass) -> RuleResult<bool> {
        Ok(false)
    }

    fn unused(&self, _key: UnusedKey) -> RuleResult<UnusedValue> {
        Ok(Arc::clone(&self.nodes))
    }

    fn entry_points(&self) -> RuleResult<Arc<HashSet<NodeId>>> {
        self.entry_points_calls
            .set(self.entry_points_calls.get() + 1);
        Ok(Arc::clone(&self.set))
    }

    fn reachable_from_entry_points(&self) -> RuleResult<Arc<HashSet<NodeId>>> {
        Ok(Arc::clone(&self.set))
    }

    fn reachability(&self, _key: RuleReachabilityKey) -> RuleResult<Arc<ReachableSet>> {
        Ok(Arc::new(ReachableSet {
            reachable: HashSet::new(),
        }))
    }

    fn scc(&self, _key: RuleTopologyKey) -> RuleResult<SccValue> {
        Ok(Arc::new(CachedSccData {
            node_to_component: Default::default(),
            components: Vec::new(),
            edge_kind: CondensationKey::References,
        }))
    }

    fn condensation(&self, _key: RuleTopologyKey) -> RuleResult<CondensationValue> {
        Ok(Arc::new(CachedCondensation {
            dag_edges: Default::default(),
            component_count: 0,
            edge_kind: CondensationKey::References,
        }))
    }

    fn trace_path(&self, _key: TracePathKey) -> RuleResult<Arc<Vec<RulePath>>> {
        self.trace_path_calls.set(self.trace_path_calls.get() + 1);
        Ok(Arc::clone(&self.paths))
    }

    fn run_plan(&self, _plan: &QueryPlan) -> RuleResult<Arc<Vec<NodeId>>> {
        self.run_plan_calls.set(self.run_plan_calls.get() + 1);
        Ok(Arc::clone(&self.nodes))
    }

    fn comparative(
        &self,
        _base: SnapshotId,
        _head: SnapshotId,
    ) -> RuleResult<Arc<ComparativeQueryDb>> {
        self.comparative_calls.set(self.comparative_calls.get() + 1);
        Ok(Arc::new(ComparativeQueryDb::new(
            Arc::clone(&self.snapshot),
            Arc::clone(&self.snapshot),
        )))
    }
}

fn materialized_node(node_id: NodeId, name: &str) -> MaterializedNode {
    MaterializedNode {
        node_id,
        name: name.to_string(),
        qualified_name: name.to_string(),
        kind: "function".to_string(),
        language: "rust".to_string(),
        file_path: "src/lib.rs".to_string(),
        start_line: 1,
        end_line: 1,
    }
}
