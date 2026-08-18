//! Dispatcher implementation for executing rule IR through [`RuleBackend`].

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use sqry_core::graph::unified::{
    EdgeFilter, NodeId, TraversalDirection, TraversalLimits, TraversalResult,
};
use sqry_db::planner::{Direction, PathPattern, PlanNode, Predicate, QueryPlan, StringPattern};
use sqry_db::queries::CyclesKey;

use crate::backend::{CycleClass, RuleBackend, RulePath, SnapshotId, TracePathKey};
use crate::ir::{
    ComplexityMetric, EntrypointExtension, PathKind, RelationEdgeKind, RuleCycleBounds,
    RuleEdgeClass, RuleEndpoint, RuleNode, RulePlan, RuleSimilarityKind, TraversalEmit,
};
use crate::witness::{DiffEntryKind, PathBudgetReason, RuleSeverity, RuleStep, RuleWitness};
use crate::{RuleError, RuleResult};

/// Jaccard floor for approximate structural neighbours (`SimilarTo::Similar`).
const SIMILAR_FLOOR: f32 = 0.7;
/// Per-seed cap on structural neighbours requested from the backend.
const MAX_SIMILAR_RESULTS: usize = 50;

/// Caller-provided cancellation hook.
pub trait RuleCancellationToken {
    /// Returns true when execution should stop promptly.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation token that never cancels.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopCancellationToken;

impl RuleCancellationToken for NoopCancellationToken {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Rule engine execution configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleEngineConfig {
    /// Maximum witness steps retained per rule firing.
    pub witness_step_cap: usize,
}

impl Default for RuleEngineConfig {
    fn default() -> Self {
        Self {
            witness_step_cap: crate::witness::DEFAULT_RULE_WITNESS_STEP_CAP,
        }
    }
}

/// Stateless rule engine.
#[derive(Debug, Clone, Default)]
pub struct RuleEngine {
    config: RuleEngineConfig,
}

impl RuleEngine {
    /// Creates an engine with default configuration.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            config: RuleEngineConfig {
                witness_step_cap: crate::witness::DEFAULT_RULE_WITNESS_STEP_CAP,
            },
        }
    }

    /// Creates an engine with explicit configuration.
    #[must_use]
    pub const fn with_config(config: RuleEngineConfig) -> Self {
        Self { config }
    }

    /// Executes a rule plan with a non-cancelling token.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError`] when the rule is malformed, the selected backend
    /// does not support a required primitive, cancellation is requested, or
    /// downstream analysis fails.
    pub fn run<B: RuleBackend>(&self, backend: &B, plan: &RulePlan) -> RuleResult<RuleRun> {
        self.run_named_with_cancellation(
            backend,
            plan,
            "anonymous",
            RuleSeverity::Info,
            &NoopCancellationToken,
        )
    }

    /// Executes a named rule plan with a non-cancelling token.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError`] when the rule is malformed, the selected backend
    /// does not support a required primitive, cancellation is requested, or
    /// downstream analysis fails.
    pub fn run_named<B: RuleBackend>(
        &self,
        backend: &B,
        plan: &RulePlan,
        rule_id: &str,
        severity: RuleSeverity,
    ) -> RuleResult<RuleRun> {
        self.run_named_with_cancellation(backend, plan, rule_id, severity, &NoopCancellationToken)
    }

    /// Executes a rule plan with a caller-provided cancellation token.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError`] when the rule is malformed, the selected backend
    /// does not support a required primitive, cancellation is requested, or
    /// downstream analysis fails.
    pub fn run_with_cancellation<B: RuleBackend, C: RuleCancellationToken>(
        &self,
        backend: &B,
        plan: &RulePlan,
        cancellation: &C,
    ) -> RuleResult<RuleRun> {
        self.run_named_with_cancellation(
            backend,
            plan,
            "anonymous",
            RuleSeverity::Info,
            cancellation,
        )
    }

    /// Executes a named rule plan with a caller-provided cancellation token.
    ///
    /// # Errors
    ///
    /// Returns [`RuleError`] when the rule is malformed, the selected backend
    /// does not support a required primitive, cancellation is requested, or
    /// downstream analysis fails.
    pub fn run_named_with_cancellation<B: RuleBackend, C: RuleCancellationToken>(
        &self,
        backend: &B,
        plan: &RulePlan,
        rule_id: &str,
        severity: RuleSeverity,
        cancellation: &C,
    ) -> RuleResult<RuleRun> {
        let mut steps = Vec::new();
        let output = self.execute_node(backend, plan.root(), cancellation, &mut steps)?;
        steps.push(RuleStep::RuleFired {
            rule_id: rule_id.to_string(),
            severity,
        });
        let witness = RuleWitness::with_step_cap(steps, Vec::new(), self.config.witness_step_cap);
        Ok(RuleRun { output, witness })
    }

    fn execute_node<B: RuleBackend, C: RuleCancellationToken>(
        &self,
        backend: &B,
        node: &RuleNode,
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        check_cancelled(cancellation)?;

        match node {
            RuleNode::NodeScan { .. } | RuleNode::SetOp { .. } => {
                Self::execute_planner_node(backend, node, steps)
            }
            RuleNode::Filter { .. } | RuleNode::EdgeTraversal { .. } => {
                Err(RuleError::InvalidRuleSource {
                    reason: "filter and edge traversal nodes require chain input",
                })
            }
            RuleNode::Chain { steps: chain_steps } => {
                self.execute_chain(backend, chain_steps, cancellation, steps)
            }
            RuleNode::PathQuery {
                from,
                to,
                kind,
                max_depth,
                max_paths,
                avoid,
            } => self.execute_path_query(
                backend,
                from,
                to,
                *kind,
                *max_depth,
                *max_paths,
                avoid.as_ref(),
                cancellation,
                steps,
            ),
            RuleNode::SubgraphExtract {
                seeds,
                edge_classes,
                direction,
                max_depth,
            } => self.execute_subgraph(
                backend,
                seeds,
                edge_classes,
                *direction,
                *max_depth,
                cancellation,
                steps,
            ),
            RuleNode::RelationEdges {
                from,
                kind,
                with_metadata,
            } => self.execute_relation_edges(
                backend,
                from,
                *kind,
                *with_metadata,
                cancellation,
                steps,
            ),
            RuleNode::CycleWitness { edge_class, bounds } => {
                Self::execute_cycle_witness(backend, *edge_class, *bounds, cancellation, steps)
            }
            RuleNode::ReferencesAt { target } => {
                self.execute_references_at(backend, target, cancellation, steps)
            }
            RuleNode::ComplexityAggregate {
                node_kind_filter,
                metric,
            } => Self::execute_complexity(backend, *node_kind_filter, *metric, cancellation, steps),
            RuleNode::CrossSnapshotDiff {
                base,
                head,
                include_unchanged,
            } => Self::execute_cross_snapshot_diff(
                backend,
                *base,
                *head,
                *include_unchanged,
                cancellation,
                steps,
            ),
            RuleNode::EntryPointUnion { extensions } => {
                Self::execute_entry_point_union(backend, extensions, cancellation, steps)
            }
            RuleNode::SimilarTo {
                seed,
                scope,
                similarity_kind,
            } => self.execute_similar_to(
                backend,
                seed,
                scope.as_ref(),
                *similarity_kind,
                cancellation,
                steps,
            ),
        }
    }

    fn execute_planner_node<B: RuleBackend>(
        backend: &B,
        node: &RuleNode,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        let plan = lower_context_free_plan(node)?;
        let nodes = backend.run_plan(&plan)?;
        push_planner_witness(node, nodes.len(), steps);
        Ok(RuleOutput::Nodes((*nodes).clone()))
    }

    fn execute_chain<B: RuleBackend, C: RuleCancellationToken>(
        &self,
        backend: &B,
        chain_steps: &[RuleNode],
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        check_cancelled(cancellation)?;
        if chain_steps.is_empty() {
            return Ok(RuleOutput::Nodes(Vec::new()));
        }

        if let Some(plan) = lower_plan_node(&RuleNode::Chain {
            steps: chain_steps.to_vec(),
        }) {
            let nodes = backend.run_plan(&QueryPlan::new(plan))?;
            steps.push(RuleStep::NodeScanMatched {
                kind: None,
                visibility: None,
                name_pattern: None,
                match_count: saturating_u32(nodes.len()),
            });
            return Ok(RuleOutput::Nodes((*nodes).clone()));
        }

        let Some((first, rest)) = chain_steps.split_first() else {
            return Ok(RuleOutput::Nodes(Vec::new()));
        };

        if !rest
            .iter()
            .all(|step| matches!(step, RuleNode::EdgeTraversal { .. }))
        {
            let mut outputs = Vec::with_capacity(chain_steps.len());
            for step in chain_steps {
                check_cancelled(cancellation)?;
                outputs.push(self.execute_node(backend, step, cancellation, steps)?);
            }
            return Ok(RuleOutput::Sequence(outputs));
        }

        let RuleOutput::Nodes(mut current) =
            self.execute_node(backend, first, cancellation, steps)?
        else {
            return Err(RuleError::InvalidRuleSource {
                reason: "edge-traversal chains require a node-producing first step",
            });
        };

        for step in rest {
            check_cancelled(cancellation)?;
            match step {
                RuleNode::EdgeTraversal {
                    direction,
                    edge_class,
                    max_depth,
                    cross_boundary,
                    emit,
                    ..
                } => {
                    if *max_depth == 0 {
                        return Err(RuleError::InvalidRuleSource {
                            reason: "edge traversal max_depth must be greater than zero",
                        });
                    }
                    let mut filter = edge_class.map_or_else(EdgeFilter::all, edge_filter_for_class);
                    filter.cross_boundary = *cross_boundary;
                    let result = backend.traverse(
                        &current,
                        traversal_direction(*direction),
                        filter,
                        TraversalLimits {
                            max_depth: *max_depth,
                            max_nodes: None,
                            max_edges: None,
                            max_paths: None,
                        },
                    )?;
                    for edge in &result.edges {
                        let from = result.nodes[edge.source_idx].node_id;
                        let to = result.nodes[edge.target_idx].node_id;
                        steps.push(RuleStep::EdgeTraversed {
                            from,
                            to,
                            direction: *direction,
                            edge_classification: RuleEdgeClass::from(edge.classification),
                            depth: edge.depth,
                        });
                    }
                    current = emit_traversal_nodes(&result, *emit);
                }
                _ => unreachable!("non-edge traversal steps are handled by sequence mode"),
            }
        }

        Ok(RuleOutput::Nodes(current))
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_path_query<B: RuleBackend, C: RuleCancellationToken>(
        &self,
        backend: &B,
        from: &RuleEndpoint,
        to: &RuleEndpoint,
        kind: PathKind,
        max_depth: u32,
        max_paths: Option<u32>,
        avoid: Option<&RuleEndpoint>,
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        if max_depth == 0 {
            return Err(RuleError::InvalidRuleSource {
                reason: "path query max_depth must be greater than zero",
            });
        }
        let sources = self.endpoint_nodes(backend, from, cancellation, steps)?;
        let targets = self.endpoint_nodes(backend, to, cancellation, steps)?;
        // Resolve the avoid set once. A path survives only if it does not
        // traverse any avoid node, expressing "reachable without passing through
        // the avoid set" (e.g. a sink reachable without a guard).
        let avoid_nodes: HashSet<NodeId> = match avoid {
            Some(endpoint) => self
                .endpoint_nodes(backend, endpoint, cancellation, steps)?
                .iter()
                .copied()
                .collect(),
            None => HashSet::new(),
        };
        let mut paths = Vec::new();
        for target in targets {
            check_cancelled(cancellation)?;
            let key = TracePathKey {
                sources: sources.clone(),
                target,
                direction: TraversalDirection::Outgoing,
                edge_filter: edge_filter_for_path(kind),
                limits: TraversalLimits {
                    max_depth,
                    max_nodes: None,
                    max_edges: None,
                    max_paths: max_paths.map(|value| value as usize),
                },
                min_confidence_bps: 0,
                allow_cross_language: true,
            };
            let returned_paths = backend.trace_path(key)?;
            for path in returned_paths.iter() {
                if !avoid_nodes.is_empty()
                    && path.nodes.iter().any(|node| avoid_nodes.contains(node))
                {
                    // Path passes through the avoid set: not a guard-avoiding
                    // path, so it is neither witnessed nor collected.
                    continue;
                }
                if let (Some(from_node), Some(to_node)) =
                    (path.nodes.first().copied(), path.nodes.last().copied())
                {
                    steps.push(RuleStep::PathConstructed {
                        from: from_node,
                        to: to_node,
                        length: saturating_u32(path.nodes.len().saturating_sub(1)),
                        edge_classes: vec![edge_class_for_path(kind)],
                        nodes: path.nodes.clone(),
                    });
                }
                paths.push(path.clone());
            }
        }
        if max_paths.is_some_and(|limit| paths.len() >= limit as usize) {
            steps.push(RuleStep::PathBudgetExhausted {
                reason: PathBudgetReason::MaxPaths,
            });
        }
        Ok(RuleOutput::Paths(paths))
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_subgraph<B: RuleBackend, C: RuleCancellationToken>(
        &self,
        backend: &B,
        seeds: &RuleEndpoint,
        edge_classes: &[RuleEdgeClass],
        direction: Direction,
        max_depth: u32,
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        if max_depth == 0 {
            return Err(RuleError::InvalidRuleSource {
                reason: "subgraph max_depth must be greater than zero",
            });
        }
        let seed_nodes = self.endpoint_nodes(backend, seeds, cancellation, steps)?;
        let result = backend.traverse(
            &seed_nodes,
            traversal_direction(direction),
            edge_filter_for_classes(edge_classes),
            TraversalLimits {
                max_depth,
                max_nodes: None,
                max_edges: None,
                max_paths: None,
            },
        )?;
        for edge in &result.edges {
            let from = result.nodes[edge.source_idx].node_id;
            let to = result.nodes[edge.target_idx].node_id;
            steps.push(RuleStep::EdgeTraversed {
                from,
                to,
                direction,
                edge_classification: RuleEdgeClass::from(edge.classification),
                depth: edge.depth,
            });
        }
        Ok(RuleOutput::Subgraph {
            nodes: result.nodes.iter().map(|node| node.node_id).collect(),
            edge_count: saturating_u32(result.edges.len()),
        })
    }

    fn execute_relation_edges<B: RuleBackend, C: RuleCancellationToken>(
        &self,
        backend: &B,
        from: &RuleEndpoint,
        kind: RelationEdgeKind,
        with_metadata: bool,
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        check_cancelled(cancellation)?;
        let source_nodes = self.endpoint_nodes(backend, from, cancellation, steps)?;
        let mut rows = Vec::new();
        for source in source_nodes {
            check_cancelled(cancellation)?;
            let targets = backend.relation_from_node(source, kind)?;
            for target in targets.iter().copied() {
                check_cancelled(cancellation)?;
                rows.push(target);
                steps.push(RuleStep::RelationEdgeEmitted {
                    from: source,
                    to: target,
                    kind: relation_edge_class(kind),
                    with_metadata,
                });
            }
        }
        Ok(RuleOutput::Relations(RuleRelationRows {
            kind,
            nodes: rows,
            with_metadata,
        }))
    }

    fn execute_cycle_witness<B: RuleBackend, C: RuleCancellationToken>(
        backend: &B,
        edge_class: RuleEdgeClass,
        bounds: RuleCycleBounds,
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        check_cancelled(cancellation)?;
        let key = CyclesKey {
            circular_type: cycle_class(edge_class)?.into(),
            bounds: sqry_db::queries::CycleBounds {
                min_depth: bounds.min_depth as usize,
                max_depth: bounds.max_depth.map(|value| value as usize),
                max_results: bounds.max_results as usize,
                should_include_self_loops: bounds.should_include_self_loops,
            },
        };
        let cycles = backend.cycles(key)?;
        for (component_id, cycle) in cycles.iter().enumerate() {
            check_cancelled(cancellation)?;
            steps.push(RuleStep::CycleDetected {
                component_id: saturating_u32(component_id),
                length: saturating_u32(cycle.len()),
                nodes: cycle.clone(),
            });
        }
        Ok(RuleOutput::Cycles((*cycles).clone()))
    }

    fn execute_references_at<B: RuleBackend, C: RuleCancellationToken>(
        &self,
        backend: &B,
        target: &RuleEndpoint,
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        check_cancelled(cancellation)?;
        let target_nodes = self.endpoint_nodes(backend, target, cancellation, steps)?;
        let mut references = Vec::new();
        for target_node in target_nodes {
            check_cancelled(cancellation)?;
            let returned = backend.relation_from_node(target_node, RelationEdgeKind::References)?;
            for source in returned.iter().copied() {
                check_cancelled(cancellation)?;
                steps.push(RuleStep::ReferenceLocated {
                    source,
                    target: target_node,
                    citation_index: 0,
                });
                references.push(source);
            }
        }
        Ok(RuleOutput::References(references))
    }

    fn execute_complexity<B: RuleBackend, C: RuleCancellationToken>(
        backend: &B,
        node_kind_filter: Option<sqry_core::graph::unified::NodeKind>,
        metric: ComplexityMetric,
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        check_cancelled(cancellation)?;
        let plan = QueryPlan::new(PlanNode::NodeScan {
            kind: node_kind_filter,
            visibility: None,
            name_pattern: None,
        });
        let nodes = backend.run_plan(&plan)?;
        let value = match metric {
            ComplexityMetric::NodeCount => nodes.len() as u64,
            ComplexityMetric::OutgoingCalls => {
                let mut total = 0_u64;
                for node in nodes.iter() {
                    check_cancelled(cancellation)?;
                    total += backend
                        .relation_from_node(*node, RelationEdgeKind::Callees)?
                        .len() as u64;
                }
                total
            }
            ComplexityMetric::IncomingCalls => {
                let mut total = 0_u64;
                for node in nodes.iter() {
                    check_cancelled(cancellation)?;
                    total += backend
                        .relation_from_node(*node, RelationEdgeKind::Callers)?
                        .len() as u64;
                }
                total
            }
        };
        steps.push(RuleStep::MetricComputed {
            metric: format!("{metric:?}"),
            value,
            node_count: saturating_u32(nodes.len()),
        });
        Ok(RuleOutput::Metrics(vec![RuleMetricValue {
            metric,
            value,
            node_count: saturating_u32(nodes.len()),
        }]))
    }

    fn execute_cross_snapshot_diff<B: RuleBackend, C: RuleCancellationToken>(
        backend: &B,
        base: SnapshotId,
        head: SnapshotId,
        include_unchanged: bool,
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        check_cancelled(cancellation)?;
        backend.comparative(base, head)?;
        if include_unchanged {
            steps.push(RuleStep::DiffEntryEmitted {
                kind: DiffEntryKind::Unchanged,
                base: None,
                head: None,
            });
        }
        Ok(RuleOutput::DiffEntries(Vec::new()))
    }

    fn execute_entry_point_union<B: RuleBackend, C: RuleCancellationToken>(
        backend: &B,
        extensions: &[EntrypointExtension],
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        check_cancelled(cancellation)?;
        let mut entry_points: BTreeSet<NodeId> = backend.entry_points()?.iter().copied().collect();
        for node in &entry_points {
            steps.push(RuleStep::EntryPointClassified {
                classifier: "builtin".to_string(),
                node: *node,
            });
        }
        for extension in extensions {
            check_cancelled(cancellation)?;
            let (classifier, nodes) = match extension {
                EntrypointExtension::Name(pattern) => (
                    "name".to_string(),
                    Self::execute_planner_endpoint(
                        backend,
                        Some(pattern.clone()),
                        None,
                        cancellation,
                    )?,
                ),
                EntrypointExtension::Path(pattern) => (
                    "path".to_string(),
                    Self::execute_path_extension(backend, pattern.clone(), cancellation)?,
                ),
                EntrypointExtension::Nodes(nodes) => ("nodes".to_string(), nodes.clone()),
            };
            for node in nodes {
                check_cancelled(cancellation)?;
                entry_points.insert(node);
                steps.push(RuleStep::EntryPointClassified {
                    classifier: classifier.clone(),
                    node,
                });
            }
        }
        Ok(RuleOutput::EntryPoints(entry_points.into_iter().collect()))
    }

    fn execute_similar_to<B: RuleBackend, C: RuleCancellationToken>(
        &self,
        backend: &B,
        seed: &RuleEndpoint,
        scope: Option<&RuleEndpoint>,
        similarity_kind: RuleSimilarityKind,
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<RuleOutput> {
        let seed_nodes = self.endpoint_nodes(backend, seed, cancellation, steps)?;
        let scope_allow: Option<HashSet<NodeId>> = match scope {
            Some(scope_endpoint) => Some(
                self.endpoint_nodes(backend, scope_endpoint, cancellation, steps)?
                    .into_iter()
                    .collect(),
            ),
            None => None,
        };

        // Duplicate = exact structural identity (shape_hash match); Similar =
        // approximate neighbour above the Jaccard floor.
        let (floor, exact_only) = match similarity_kind {
            RuleSimilarityKind::Duplicate => (0.0_f32, true),
            RuleSimilarityKind::Similar => (SIMILAR_FLOOR, false),
        };

        let mut rows = Vec::new();
        for seed_node in seed_nodes {
            check_cancelled(cancellation)?;
            let neighbours = backend.structural_neighbors(seed_node, floor, MAX_SIMILAR_RESULTS)?;
            for neighbour in neighbours {
                if neighbour.node == seed_node {
                    continue;
                }
                if exact_only && !neighbour.shape_hash_exact {
                    continue;
                }
                if let Some(allow) = &scope_allow
                    && !allow.contains(&neighbour.node)
                {
                    continue;
                }
                let score = match similarity_kind {
                    RuleSimilarityKind::Duplicate => 10_000u16,
                    RuleSimilarityKind::Similar => {
                        (neighbour.jaccard * 10_000.0).round().clamp(0.0, 10_000.0) as u16
                    }
                };
                rows.push(RuleSimilarityMatch {
                    seed: seed_node,
                    matched: neighbour.node,
                    score,
                    similarity_kind,
                });
                steps.push(RuleStep::SimilarityMatchEmitted {
                    seed: seed_node,
                    matched: neighbour.node,
                    score,
                    similarity_kind,
                });
            }
        }

        Ok(RuleOutput::SimilarityMatches(rows))
    }

    fn endpoint_nodes<B: RuleBackend, C: RuleCancellationToken>(
        &self,
        backend: &B,
        endpoint: &RuleEndpoint,
        cancellation: &C,
        steps: &mut Vec<RuleStep>,
    ) -> RuleResult<Vec<NodeId>> {
        check_cancelled(cancellation)?;
        match endpoint {
            RuleEndpoint::Nodes(nodes) => Ok(nodes.clone()),
            RuleEndpoint::Query(query) => {
                match self.execute_node(backend, query, cancellation, steps)? {
                    RuleOutput::Nodes(nodes) | RuleOutput::EntryPoints(nodes) => Ok(nodes),
                    _ => Err(RuleError::InvalidRuleSource {
                        reason: "endpoint query must produce a node set",
                    }),
                }
            }
        }
    }

    fn execute_planner_endpoint<B: RuleBackend, C: RuleCancellationToken>(
        backend: &B,
        name_pattern: Option<StringPattern>,
        path_pattern: Option<PathPattern>,
        cancellation: &C,
    ) -> RuleResult<Vec<NodeId>> {
        check_cancelled(cancellation)?;
        let root = match path_pattern {
            None => PlanNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern,
            },
            Some(path) => PlanNode::Chain {
                steps: vec![
                    PlanNode::NodeScan {
                        kind: None,
                        visibility: None,
                        name_pattern,
                    },
                    PlanNode::Filter {
                        predicate: Predicate::InFile(path),
                    },
                ],
            },
        };
        Ok((*backend.run_plan(&QueryPlan::new(root))?).clone())
    }

    fn execute_path_extension<B: RuleBackend, C: RuleCancellationToken>(
        backend: &B,
        path_pattern: PathPattern,
        cancellation: &C,
    ) -> RuleResult<Vec<NodeId>> {
        Self::execute_planner_endpoint(backend, None, Some(path_pattern), cancellation)
    }
}

/// Rule execution output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleOutput {
    /// Node-set output.
    Nodes(Vec<NodeId>),
    /// Path output.
    Paths(Vec<RulePath>),
    /// Bounded subgraph output.
    Subgraph {
        /// Nodes included in the subgraph.
        nodes: Vec<NodeId>,
        /// Number of materialized edges.
        edge_count: u32,
    },
    /// Relation rows.
    Relations(RuleRelationRows),
    /// Cycle components.
    Cycles(Vec<Vec<NodeId>>),
    /// Reference source nodes.
    References(Vec<NodeId>),
    /// Complexity metric values.
    Metrics(Vec<RuleMetricValue>),
    /// Cross-snapshot diff rows.
    DiffEntries(Vec<RuleDiffEntry>),
    /// Entry-point union.
    EntryPoints(Vec<NodeId>),
    /// Similarity matches.
    SimilarityMatches(Vec<RuleSimilarityMatch>),
    /// Ordered outputs from a heterogeneous rule sequence.
    Sequence(Vec<RuleOutput>),
}

/// Relation row output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleRelationRows {
    /// Relation family.
    pub kind: RelationEdgeKind,
    /// Matched nodes returned by the backend relation primitive.
    pub nodes: Vec<NodeId>,
    /// Whether metadata was requested.
    pub with_metadata: bool,
}

/// Metric output row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleMetricValue {
    /// Metric family.
    pub metric: ComplexityMetric,
    /// Computed integer value.
    pub value: u64,
    /// Number of nodes included in the aggregate.
    pub node_count: u32,
}

/// Diff output row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDiffEntry {
    /// Diff row family.
    pub kind: DiffEntryKind,
    /// Base node, when present.
    pub base: Option<NodeId>,
    /// Head node, when present.
    pub head: Option<NodeId>,
}

/// Similarity output row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSimilarityMatch {
    /// Seed node.
    pub seed: NodeId,
    /// Matched node.
    pub matched: NodeId,
    /// Similarity score in basis points.
    pub score: u16,
    /// Similarity family.
    pub similarity_kind: RuleSimilarityKind,
}

/// Complete rule run output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleRun {
    /// Rule output payload.
    pub output: RuleOutput,
    /// Witness explaining how the output was produced.
    pub witness: RuleWitness,
}

fn check_cancelled<C: RuleCancellationToken>(cancellation: &C) -> RuleResult<()> {
    if cancellation.is_cancelled() {
        return Err(RuleError::ExecutionCancelled);
    }
    Ok(())
}

fn lower_context_free_plan(node: &RuleNode) -> RuleResult<QueryPlan> {
    let Some(plan) = lower_plan_node(node) else {
        return Err(RuleError::UnsupportedPrimitive {
            backend: "rule-engine",
            primitive: "rule_edge_class_to_planner_edge_kind",
            reason: "rule-edge-class traversals must dispatch through RuleBackend::traverse",
        });
    };
    if !plan.is_context_free() {
        return Err(RuleError::InvalidRuleSource {
            reason: "planner-compatible rule root must be context-free",
        });
    }
    Ok(QueryPlan::new(plan))
}

fn lower_plan_node(node: &RuleNode) -> Option<PlanNode> {
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
            // sqry-db's planner IR has no cross-boundary or emit concept, and
            // its separate `run_traversal` BFS filters by discriminant only,
            // with no `EdgeFilter` and always emits reached nodes. Lowering a
            // cross-boundary or non-default-emit traversal would silently drop
            // the criterion, so refuse it and force the witness-bearing backend
            // traversal path.
            if edge_class.is_some()
                || cross_boundary.is_some()
                || *emit != TraversalEmit::ReachedNodes
            {
                return None;
            }
            Some(PlanNode::EdgeTraversal {
                direction: *direction,
                edge_kind: None,
                max_depth: *max_depth,
                resolved_via: *resolved_via,
            })
        }
        RuleNode::Filter { predicate } => Some(PlanNode::Filter {
            predicate: predicate.clone(),
        }),
        RuleNode::SetOp { op, left, right } => Some(PlanNode::SetOp {
            op: *op,
            left: Box::new(lower_plan_node(left)?),
            right: Box::new(lower_plan_node(right)?),
        }),
        RuleNode::Chain { steps } => Some(PlanNode::Chain {
            steps: steps
                .iter()
                .map(lower_plan_node)
                .collect::<Option<Vec<_>>>()?,
        }),
        RuleNode::PathQuery { .. }
        | RuleNode::SubgraphExtract { .. }
        | RuleNode::RelationEdges { .. }
        | RuleNode::CycleWitness { .. }
        | RuleNode::ReferencesAt { .. }
        | RuleNode::ComplexityAggregate { .. }
        | RuleNode::CrossSnapshotDiff { .. }
        | RuleNode::EntryPointUnion { .. }
        | RuleNode::SimilarTo { .. } => None,
    }
}

fn push_planner_witness(node: &RuleNode, output_len: usize, steps: &mut Vec<RuleStep>) {
    match node {
        RuleNode::NodeScan {
            kind,
            visibility,
            name_pattern,
        } => steps.push(RuleStep::NodeScanMatched {
            kind: *kind,
            visibility: *visibility,
            name_pattern: name_pattern.clone(),
            match_count: saturating_u32(output_len),
        }),
        RuleNode::SetOp { op, left, right } => steps.push(RuleStep::SetOpEvaluated {
            op: *op,
            lhs_card: lower_plan_node(left).map_or(0, |_| saturating_u32(output_len)),
            rhs_card: lower_plan_node(right).map_or(0, |_| saturating_u32(output_len)),
            result_card: saturating_u32(output_len),
        }),
        _ => {}
    }
}

fn traversal_direction(direction: Direction) -> TraversalDirection {
    match direction {
        Direction::Forward => TraversalDirection::Outgoing,
        Direction::Reverse => TraversalDirection::Incoming,
        Direction::Both => TraversalDirection::Both,
    }
}

fn edge_filter_for_class(edge_class: RuleEdgeClass) -> EdgeFilter {
    edge_filter_for_classes(&[edge_class])
}

/// Selects the node set an edge traversal emits from its materialized result.
///
/// `ReachedNodes` returns seeds plus every reached node (historical behavior);
/// `EdgeSources` / `EdgeTargets` return the distinct source / target endpoints
/// of the edges that passed the filter, in first-seen order.
fn emit_traversal_nodes(result: &TraversalResult, emit: TraversalEmit) -> Vec<NodeId> {
    match emit {
        TraversalEmit::ReachedNodes => result.nodes.iter().map(|node| node.node_id).collect(),
        TraversalEmit::EdgeSources => collect_edge_endpoints(result, true),
        TraversalEmit::EdgeTargets => collect_edge_endpoints(result, false),
    }
}

/// Collects the distinct source (or target) endpoints of the traversed edges in
/// first-seen order.
fn collect_edge_endpoints(result: &TraversalResult, sources: bool) -> Vec<NodeId> {
    let mut seen = HashSet::new();
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

fn edge_filter_for_classes(edge_classes: &[RuleEdgeClass]) -> EdgeFilter {
    if edge_classes.is_empty() {
        return EdgeFilter::all();
    }
    let mut selected = HashSet::new();
    for edge_class in edge_classes {
        selected.insert(*edge_class);
    }
    EdgeFilter {
        include_calls: selected.contains(&RuleEdgeClass::Call),
        include_imports: selected.contains(&RuleEdgeClass::Import),
        include_references: selected.contains(&RuleEdgeClass::Reference),
        include_inheritance: selected.contains(&RuleEdgeClass::Inheritance),
        include_structural: selected.contains(&RuleEdgeClass::Structural),
        include_type_edges: selected.contains(&RuleEdgeClass::Type),
        include_database: selected.contains(&RuleEdgeClass::Database),
        include_service: selected.contains(&RuleEdgeClass::Service),
        cross_boundary: None,
    }
}

fn edge_filter_for_path(kind: PathKind) -> EdgeFilter {
    match kind {
        PathKind::Any => EdgeFilter::all(),
        PathKind::Calls => EdgeFilter::calls_only(),
        PathKind::Dependency => EdgeFilter::dependency_edges(),
    }
}

fn edge_class_for_path(kind: PathKind) -> RuleEdgeClass {
    match kind {
        PathKind::Any | PathKind::Dependency => RuleEdgeClass::Structural,
        PathKind::Calls => RuleEdgeClass::Call,
    }
}

fn relation_edge_class(kind: RelationEdgeKind) -> RuleEdgeClass {
    match kind {
        RelationEdgeKind::Callers | RelationEdgeKind::Callees => RuleEdgeClass::Call,
        RelationEdgeKind::Imports | RelationEdgeKind::Exports => RuleEdgeClass::Import,
        RelationEdgeKind::References => RuleEdgeClass::Reference,
        RelationEdgeKind::Implements => RuleEdgeClass::Inheritance,
    }
}

fn cycle_class(edge_class: RuleEdgeClass) -> RuleResult<CycleClass> {
    match edge_class {
        RuleEdgeClass::Call => Ok(CycleClass::Calls),
        RuleEdgeClass::Import => Ok(CycleClass::Imports),
        RuleEdgeClass::Structural => Ok(CycleClass::Modules),
        RuleEdgeClass::Reference
        | RuleEdgeClass::Inheritance
        | RuleEdgeClass::Type
        | RuleEdgeClass::Database
        | RuleEdgeClass::Service => Err(RuleError::UnsupportedPrimitive {
            backend: "rule-engine",
            primitive: "cycle_witness",
            reason: "cycle witnesses support call, import, and structural module cycles in the current backend contract",
        }),
    }
}

fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
