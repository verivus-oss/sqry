//! Shared fixtures for the Phase 5 TC1-TC10 workspace regression suite.
//!
//! Cargo compiles files in `tests/` subdirectories as plain modules, not as
//! standalone test binaries, so this module is shared by the `tcN_*`
//! integration tests through `mod common;` without producing an extra test
//! target. The crate-level `dead_code` allowance covers helpers that any
//! single test binary legitimately leaves unused.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::file::FileId;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::graph::unified::{
    CodeGraph, EdgeFilter, GraphSnapshot, NodeId, NodeKind, TraversalDirection, TraversalLimits,
};
use sqry_core::query::CircularType;
use sqry_db::planner::{Direction, PlanNode, Predicate, QueryPlan};
use sqry_db::queries::{CycleBounds, CyclesKey};
use sqry_db::{QueryDb, QueryDbConfig};
use sqry_rules::ir::{
    ComplexityMetric, EntrypointExtension, PathKind, RelationEdgeKind, RuleCycleBounds,
    RuleEdgeClass, RuleEndpoint, RuleNode, RuleSimilarityKind, TraversalEmit,
};
use sqry_rules::rules::ShippedRule;
use sqry_rules::{CycleClass, RuleBackend, TracePathKey, requires_unsupported_beside_cache};

/// A two-function call fixture: `main` calls `helper`, both in `src/lib.rs`.
pub struct CallFixture {
    /// Immutable snapshot of the fixture graph.
    pub snapshot: Arc<GraphSnapshot>,
    /// File the two functions live in (used for Tier-1 revision bumps).
    pub file: FileId,
    /// `main`, the caller.
    pub main: NodeId,
    /// `helper`, the callee.
    pub helper: NodeId,
}

/// Builds the canonical `main -> helper` call graph reused by TC5/TC8/TC10.
#[must_use]
pub fn two_node_call_fixture() -> CallFixture {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register(Path::new("src/lib.rs"))
        .expect("register fixture file");
    let main_name = graph.strings_mut().intern("main").expect("intern main");
    let helper_name = graph.strings_mut().intern("helper").expect("intern helper");
    let main = graph
        .nodes_mut()
        .alloc(NodeEntry::new(NodeKind::Function, main_name, file).with_qualified_name(main_name))
        .expect("allocate main");
    let helper = graph
        .nodes_mut()
        .alloc(
            NodeEntry::new(NodeKind::Function, helper_name, file).with_qualified_name(helper_name),
        )
        .expect("allocate helper");
    graph.edges_mut().add_edge(
        main,
        helper,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        file,
    );
    CallFixture {
        snapshot: Arc::new(graph.snapshot()),
        file,
        main,
        helper,
    }
}

/// A star fixture for cross-boundary traversal: `root` reaches an FFI target
/// (cross-boundary), a DB target (cross-boundary, different classification),
/// and an intra-language call target.
pub struct CrossBoundaryFixture {
    /// Immutable snapshot of the fixture graph.
    pub snapshot: Arc<GraphSnapshot>,
    /// `root`, the traversal seed.
    pub root: NodeId,
    /// Target reached via an `FfiCall` edge (cross-boundary).
    pub ffi_target: NodeId,
    /// Target reached via a `DbQuery` edge (cross-boundary).
    pub db_target: NodeId,
    /// Target reached via a plain `Calls` edge (intra-language).
    pub intra_target: NodeId,
}

/// Builds a `root -> {ffi_target, db_target, intra_target}` star exercising the
/// cross-boundary edge discriminator through the production backend.
#[must_use]
pub fn cross_boundary_fixture() -> CrossBoundaryFixture {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register(Path::new("src/lib.rs"))
        .expect("register fixture file");

    let function = |graph: &mut CodeGraph, name: &str| {
        let name_id = graph.strings_mut().intern(name).expect("intern name");
        let node_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name_id, file).with_qualified_name(name_id))
            .expect("allocate function");
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, name_id, Some(name_id), file);
        node_id
    };

    let root = function(&mut graph, "root");
    let ffi_target = function(&mut graph, "ffi_target");
    let db_target = function(&mut graph, "db_target");
    let intra_target = function(&mut graph, "intra_target");

    graph.edges_mut().add_edge(
        root,
        ffi_target,
        EdgeKind::FfiCall {
            convention: sqry_core::graph::unified::edge::kind::FfiConvention::C,
        },
        file,
    );
    graph.edges_mut().add_edge(
        root,
        db_target,
        EdgeKind::DbQuery {
            query_type: sqry_core::graph::unified::edge::kind::DbQueryType::Select,
            table: None,
        },
        file,
    );
    graph.edges_mut().add_edge(
        root,
        intra_target,
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        file,
    );

    CrossBoundaryFixture {
        snapshot: Arc::new(graph.snapshot()),
        root,
        ffi_target,
        db_target,
        intra_target,
    }
}

/// Builds an empty `QueryDb` backed by an empty graph snapshot.
#[must_use]
pub fn empty_query_db() -> QueryDb {
    QueryDb::new(
        Arc::new(CodeGraph::new().snapshot()),
        QueryDbConfig::default(),
    )
}

/// Builds a `QueryDb` over an existing snapshot.
#[must_use]
pub fn query_db_for(snapshot: Arc<GraphSnapshot>) -> QueryDb {
    QueryDb::new(snapshot, QueryDbConfig::default())
}

/// Parses the package-relative `Cargo.toml` `[dependencies]` table into a list
/// of crate names, used by the dependency-allowlist regression guards.
#[must_use]
pub fn manifest_dependency_names(manifest: &str) -> Vec<&str> {
    let mut in_dependencies = false;
    let mut names = Vec::new();

    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed == "[dependencies]" {
            in_dependencies = true;
            continue;
        }
        if in_dependencies && trimmed.starts_with('[') {
            break;
        }
        if in_dependencies
            && !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && let Some((name, _value)) = trimmed.split_once('=')
        {
            let package_name = name
                .trim()
                .split_once('.')
                .map_or_else(|| name.trim(), |(package_name, _field)| package_name.trim());
            names.push(package_name);
        }
    }

    names
}

/// Returns true when any node in the rule subtree routes through beside-cache
/// coordination the engine cannot yet run (cross-snapshot), mirroring the
/// recursive check the CLI / MCP apply before reporting a rule as unsupported.
/// Since L2a, SimilarTo runs in-engine and is no longer flagged here.
#[must_use]
pub fn contains_beside_cache_route(node: &RuleNode) -> bool {
    requires_unsupported_beside_cache(node)
        || beside_child_nodes(node)
            .iter()
            .any(|child| contains_beside_cache_route(child))
}

/// Returns true when a shipped rule must route through beside-cache
/// coordination the engine cannot run yet, derived solely from the plan (a
/// stale declared flag must not gate a rule the engine can run).
#[must_use]
pub fn requires_beside_cache(rule: &ShippedRule) -> bool {
    contains_beside_cache_route(rule.definition.plan.root())
}

fn beside_child_nodes(node: &RuleNode) -> Vec<&RuleNode> {
    match node {
        RuleNode::SetOp { left, right, .. } => vec![left.as_ref(), right.as_ref()],
        RuleNode::Chain { steps } => steps.iter().collect(),
        RuleNode::PathQuery {
            from, to, avoid, ..
        } => {
            let mut children = beside_endpoint_children([from, to]);
            if let Some(avoid) = avoid {
                children.extend(beside_endpoint_children([avoid]));
            }
            children
        }
        RuleNode::SubgraphExtract { seeds, .. } => beside_endpoint_children([seeds]),
        RuleNode::RelationEdges { from, .. } => beside_endpoint_children([from]),
        RuleNode::ReferencesAt { target } => beside_endpoint_children([target]),
        RuleNode::SimilarTo { seed, scope, .. } => {
            let mut children = beside_endpoint_children([seed]);
            if let Some(scope) = scope {
                children.extend(beside_endpoint_children([scope]));
            }
            children
        }
        RuleNode::NodeScan { .. }
        | RuleNode::EdgeTraversal { .. }
        | RuleNode::Filter { .. }
        | RuleNode::CycleWitness { .. }
        | RuleNode::ComplexityAggregate { .. }
        | RuleNode::CrossSnapshotDiff { .. }
        | RuleNode::EntryPointUnion { .. } => Vec::new(),
    }
}

fn beside_endpoint_children<const N: usize>(endpoints: [&RuleEndpoint; N]) -> Vec<&RuleNode> {
    endpoints
        .into_iter()
        .filter_map(|endpoint| match endpoint {
            RuleEndpoint::Nodes(_) => None,
            RuleEndpoint::Query(node) => Some(node.as_ref()),
        })
        .collect()
}

/// Builds a non-trivial analysis fixture (a ~114-node graph with a call chain,
/// a back-edge cycle, and a fan of reference edges, with several functions
/// named to match the shipped recipe / intake rule patterns). Used by TC8 so
/// node scans, traversals, and relation queries do work proportional to a real
/// graph rather than a two-node toy.
#[must_use]
pub fn analysis_fixture() -> Arc<GraphSnapshot> {
    let mut graph = CodeGraph::new();
    let file = graph
        .files_mut()
        .register(Path::new("src/analysis.rs"))
        .expect("register analysis fixture file");

    let mut nodes = Vec::new();
    let named = [
        "main",
        "test_alpha",
        "handle_request",
        "Validate",
        "Recv",
        "Assume",
        "CanElide",
        "deopt",
        "prototype_chain",
        "PrototypeChainValidityCell",
        "IsJSObjectThatCanBeTrackedAsPrototype",
        "localStorage",
        "kFileSystemRead",
        "speculation_guard",
    ];
    for name in named {
        nodes.push(alloc_function(&mut graph, file, name));
    }
    for index in 0..100 {
        nodes.push(alloc_function(&mut graph, file, &format!("fn_{index}")));
    }

    // Call chain across every node.
    for window in nodes.windows(2) {
        graph
            .edges_mut()
            .add_edge(window[0], window[1], calls_edge(), file);
    }
    // A small back-edge cycle among the first three nodes.
    graph
        .edges_mut()
        .add_edge(nodes[2], nodes[0], calls_edge(), file);
    // A fan of reference edges from `main`.
    for &target in nodes.iter().skip(1).take(20) {
        graph
            .edges_mut()
            .add_edge(nodes[0], target, EdgeKind::References, file);
    }

    Arc::new(graph.snapshot())
}

fn alloc_function(graph: &mut CodeGraph, file: FileId, name: &str) -> NodeId {
    let interned = graph.strings_mut().intern(name).expect("intern node name");
    graph
        .nodes_mut()
        .alloc(NodeEntry::new(NodeKind::Function, interned, file).with_qualified_name(interned))
        .expect("allocate fixture node")
}

fn calls_edge() -> EdgeKind {
    EdgeKind::Calls {
        argument_count: 0,
        is_async: false,
        resolved_via: ResolvedVia::Direct,
    }
}

/// Executes the equivalent hand-authored ad-hoc `sqry-db` composition for a
/// rule plan: it issues the same `RuleBackend` primitive calls the engine
/// would, but without building the witness / `RuleOutput` envelopes. TC8
/// compares the rule engine against this baseline to bound the rule-layer
/// overhead at <= 2x (the reconciled NFR4 budget). The bbnty-audit baselines
/// referenced in 01_SPEC.md are external and not reproducible in CI; this is
/// the local, reproducible analog of that hand composition.
pub fn hand_compose<B: RuleBackend>(backend: &B, node: &RuleNode) -> Vec<NodeId> {
    match node {
        RuleNode::NodeScan { .. }
        | RuleNode::SetOp { .. }
        | RuleNode::Filter { .. }
        | RuleNode::EdgeTraversal { .. } => run_lowered(backend, node),
        RuleNode::Chain { steps } => hand_chain(backend, steps),
        RuleNode::PathQuery {
            from,
            to,
            kind,
            max_depth,
            max_paths,
            avoid: _,
        } => {
            let sources = endpoint_nodes(backend, from);
            for target in endpoint_nodes(backend, to) {
                let key = TracePathKey {
                    sources: sources.clone(),
                    target,
                    direction: TraversalDirection::Outgoing,
                    edge_filter: path_edge_filter(*kind),
                    limits: TraversalLimits {
                        max_depth: *max_depth,
                        max_nodes: None,
                        max_edges: None,
                        max_paths: max_paths.map(|value| value as usize),
                    },
                    min_confidence_bps: 0,
                    allow_cross_language: true,
                };
                let _ = backend.trace_path(key).expect("hand trace_path");
            }
            Vec::new()
        }
        RuleNode::SubgraphExtract {
            seeds,
            edge_classes,
            direction,
            max_depth,
        } => {
            let seed_nodes = endpoint_nodes(backend, seeds);
            let result = backend
                .traverse(
                    &seed_nodes,
                    traversal_direction(*direction),
                    classes_edge_filter(edge_classes),
                    TraversalLimits {
                        max_depth: *max_depth,
                        max_nodes: None,
                        max_edges: None,
                        max_paths: None,
                    },
                )
                .expect("hand traverse");
            result.nodes.iter().map(|node| node.node_id).collect()
        }
        RuleNode::RelationEdges { from, kind, .. } => {
            let mut rows = Vec::new();
            for source in endpoint_nodes(backend, from) {
                let targets = backend
                    .relation_from_node(source, *kind)
                    .expect("hand relation");
                rows.extend(targets.iter().copied());
            }
            rows
        }
        RuleNode::CycleWitness { edge_class, bounds } => {
            let Some(cycle_class) = cycle_class_for(*edge_class) else {
                return Vec::new();
            };
            let key = CyclesKey {
                circular_type: CircularType::from(cycle_class),
                bounds: cycle_bounds(*bounds),
            };
            backend
                .cycles(key)
                .expect("hand cycles")
                .iter()
                .flatten()
                .copied()
                .collect()
        }
        RuleNode::ReferencesAt { target } => {
            let mut references = Vec::new();
            for target_node in endpoint_nodes(backend, target) {
                references.extend(
                    backend
                        .relation_from_node(target_node, RelationEdgeKind::References)
                        .expect("hand references")
                        .iter()
                        .copied(),
                );
            }
            references
        }
        RuleNode::ComplexityAggregate {
            node_kind_filter,
            metric,
        } => {
            let nodes = backend
                .run_plan(&QueryPlan::new(PlanNode::NodeScan {
                    kind: *node_kind_filter,
                    visibility: None,
                    name_pattern: None,
                }))
                .expect("hand complexity scan");
            for node in nodes.iter() {
                match metric {
                    ComplexityMetric::OutgoingCalls => {
                        let _ = backend.relation_from_node(*node, RelationEdgeKind::Callees);
                    }
                    ComplexityMetric::IncomingCalls => {
                        let _ = backend.relation_from_node(*node, RelationEdgeKind::Callers);
                    }
                    ComplexityMetric::NodeCount => {}
                }
            }
            (*nodes).clone()
        }
        RuleNode::EntryPointUnion { extensions } => {
            let mut nodes: Vec<NodeId> = backend
                .entry_points()
                .expect("hand entry points")
                .iter()
                .copied()
                .collect();
            for extension in extensions {
                match extension {
                    EntrypointExtension::Name(pattern) => nodes.extend(
                        backend
                            .run_plan(&QueryPlan::new(PlanNode::NodeScan {
                                kind: None,
                                visibility: None,
                                name_pattern: Some(pattern.clone()),
                            }))
                            .expect("hand name scan")
                            .iter()
                            .copied(),
                    ),
                    EntrypointExtension::Path(pattern) => nodes.extend(
                        backend
                            .run_plan(&QueryPlan::new(PlanNode::Chain {
                                steps: vec![
                                    PlanNode::NodeScan {
                                        kind: None,
                                        visibility: None,
                                        name_pattern: None,
                                    },
                                    PlanNode::Filter {
                                        predicate: Predicate::InFile(pattern.clone()),
                                    },
                                ],
                            }))
                            .expect("hand path scan")
                            .iter()
                            .copied(),
                    ),
                    EntrypointExtension::Nodes(explicit) => nodes.extend(explicit.iter().copied()),
                }
            }
            nodes
        }
        // SimilarTo runs the structural-neighbour primitive in-engine since L2a,
        // so the baseline exercises the same backend call for a meaningful
        // wall-clock comparison (mirrors the dispatcher's execute_similar_to).
        RuleNode::SimilarTo {
            seed,
            similarity_kind,
            ..
        } => {
            let (floor, exact_only) = match similarity_kind {
                RuleSimilarityKind::Duplicate => (0.0_f32, true),
                RuleSimilarityKind::Similar => (0.7_f32, false),
            };
            let mut matched = Vec::new();
            for seed_node in endpoint_nodes(backend, seed) {
                for neighbour in backend
                    .structural_neighbors(seed_node, floor, 50)
                    .expect("hand structural_neighbors")
                {
                    if neighbour.node != seed_node && (!exact_only || neighbour.shape_hash_exact) {
                        matched.push(neighbour.node);
                    }
                }
            }
            matched
        }
        // CrossSnapshotDiff still has no single-snapshot backend primitive.
        RuleNode::CrossSnapshotDiff { .. } => Vec::new(),
    }
}

fn run_lowered<B: RuleBackend>(backend: &B, node: &RuleNode) -> Vec<NodeId> {
    match lower_plan(node) {
        Some(plan) => (*backend
            .run_plan(&QueryPlan::new(plan))
            .expect("hand run_plan"))
        .clone(),
        None => Vec::new(),
    }
}

fn hand_chain<B: RuleBackend>(backend: &B, steps: &[RuleNode]) -> Vec<NodeId> {
    if let Some(plan) = lower_plan(&RuleNode::Chain {
        steps: steps.to_vec(),
    }) {
        return (*backend
            .run_plan(&QueryPlan::new(plan))
            .expect("hand planner chain"))
        .clone();
    }

    if let Some((first, rest)) = steps.split_first()
        && !rest.is_empty()
        && rest
            .iter()
            .all(|step| matches!(step, RuleNode::EdgeTraversal { .. }))
    {
        let mut current = hand_compose(backend, first);
        for step in rest {
            if let RuleNode::EdgeTraversal {
                direction,
                edge_class,
                max_depth,
                cross_boundary,
                emit,
                ..
            } = step
            {
                let mut filter =
                    edge_class.map_or_else(EdgeFilter::all, |class| classes_edge_filter(&[class]));
                filter.cross_boundary = *cross_boundary;
                let result = backend
                    .traverse(
                        &current,
                        traversal_direction(*direction),
                        filter,
                        TraversalLimits {
                            max_depth: *max_depth,
                            max_nodes: None,
                            max_edges: None,
                            max_paths: None,
                        },
                    )
                    .expect("hand chain traverse");
                current = match emit {
                    TraversalEmit::ReachedNodes => {
                        result.nodes.iter().map(|node| node.node_id).collect()
                    }
                    TraversalEmit::EdgeSources => hand_edge_endpoints(&result, true),
                    TraversalEmit::EdgeTargets => hand_edge_endpoints(&result, false),
                };
            }
        }
        return current;
    }

    let mut last = Vec::new();
    for step in steps {
        last = hand_compose(backend, step);
    }
    last
}

fn lower_plan(node: &RuleNode) -> Option<PlanNode> {
    match node {
        RuleNode::NodeScan {
            kind,
            visibility,
            name_pattern,
        } => Some(PlanNode::NodeScan {
            kind: *kind,
            visibility: *visibility,
            name_pattern: name_pattern.clone(),
        }),
        RuleNode::EdgeTraversal {
            direction,
            edge_class,
            max_depth,
            resolved_via,
            cross_boundary,
            emit,
        } => {
            if edge_class.is_some()
                || cross_boundary.is_some()
                || *emit != TraversalEmit::ReachedNodes
            {
                None
            } else {
                Some(PlanNode::EdgeTraversal {
                    direction: *direction,
                    edge_kind: None,
                    max_depth: *max_depth,
                    resolved_via: *resolved_via,
                })
            }
        }
        RuleNode::Filter { predicate } => Some(PlanNode::Filter {
            predicate: predicate.clone(),
        }),
        RuleNode::SetOp { op, left, right } => Some(PlanNode::SetOp {
            op: *op,
            left: Box::new(lower_plan(left)?),
            right: Box::new(lower_plan(right)?),
        }),
        RuleNode::Chain { steps } => Some(PlanNode::Chain {
            steps: steps.iter().map(lower_plan).collect::<Option<Vec<_>>>()?,
        }),
        _ => None,
    }
}

fn endpoint_nodes<B: RuleBackend>(backend: &B, endpoint: &RuleEndpoint) -> Vec<NodeId> {
    match endpoint {
        RuleEndpoint::Nodes(nodes) => nodes.clone(),
        RuleEndpoint::Query(query) => hand_compose(backend, query),
    }
}

fn path_edge_filter(kind: PathKind) -> EdgeFilter {
    match kind {
        PathKind::Any => EdgeFilter::all(),
        PathKind::Calls => EdgeFilter::calls_only(),
        PathKind::Dependency => EdgeFilter::dependency_edges(),
    }
}

/// Mirror of the dispatcher's edge-endpoint collection for `emit` modes:
/// distinct source (or target) endpoints of the traversed edges, first-seen.
fn hand_edge_endpoints(
    result: &sqry_core::graph::unified::TraversalResult,
    sources: bool,
) -> Vec<NodeId> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for edge in &result.edges {
        let idx = if sources {
            edge.source_idx
        } else {
            edge.target_idx
        };
        let node_id = result.nodes[idx].node_id;
        if seen.insert(node_id) {
            out.push(node_id);
        }
    }
    out
}

fn classes_edge_filter(classes: &[RuleEdgeClass]) -> EdgeFilter {
    if classes.is_empty() {
        return EdgeFilter::all();
    }
    EdgeFilter {
        include_calls: classes.contains(&RuleEdgeClass::Call),
        include_imports: classes.contains(&RuleEdgeClass::Import),
        include_references: classes.contains(&RuleEdgeClass::Reference),
        include_inheritance: classes.contains(&RuleEdgeClass::Inheritance),
        include_structural: classes.contains(&RuleEdgeClass::Structural),
        include_type_edges: classes.contains(&RuleEdgeClass::Type),
        include_database: classes.contains(&RuleEdgeClass::Database),
        include_service: classes.contains(&RuleEdgeClass::Service),
        cross_boundary: None,
    }
}

fn traversal_direction(direction: Direction) -> TraversalDirection {
    match direction {
        Direction::Forward => TraversalDirection::Outgoing,
        Direction::Reverse => TraversalDirection::Incoming,
        Direction::Both => TraversalDirection::Both,
    }
}

fn cycle_class_for(edge_class: RuleEdgeClass) -> Option<CycleClass> {
    match edge_class {
        RuleEdgeClass::Call => Some(CycleClass::Calls),
        RuleEdgeClass::Import => Some(CycleClass::Imports),
        RuleEdgeClass::Structural => Some(CycleClass::Modules),
        _ => None,
    }
}

fn cycle_bounds(bounds: RuleCycleBounds) -> CycleBounds {
    CycleBounds {
        min_depth: bounds.min_depth as usize,
        max_depth: bounds.max_depth.map(|value| value as usize),
        max_results: bounds.max_results as usize,
        should_include_self_loops: bounds.should_include_self_loops,
    }
}
