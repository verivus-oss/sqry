//! Typed Rust builder for declarative rule plans.

use sqry_core::graph::unified::{NodeId, NodeKind, ResolvedVia};
use sqry_core::schema::Visibility;
use sqry_db::planner::{
    Direction, PathPattern, Predicate, QueryBuilder, SetOperation, StringPattern,
};

use crate::backend::SnapshotId;
use crate::ir::{
    ComplexityMetric, EntrypointExtension, PathKind, RelationEdgeKind, RuleCycleBounds,
    RuleEdgeClass, RuleEndpoint, RuleNode, RulePlan, RuleSimilarityKind, TraversalEmit,
};
use crate::{RuleError, RuleResult};

/// Typed builder that lowers Rust rule source into canonical `RulePlan` IR.
#[derive(Debug, Default, Clone)]
pub struct RuleBuilder {
    steps: Vec<RuleNode>,
}

impl RuleBuilder {
    /// Constructs an empty builder.
    #[must_use]
    pub const fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Starts from an unfiltered node scan.
    #[must_use]
    pub fn scan_all(mut self) -> Self {
        self.steps.push(RuleNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        });
        self
    }

    /// Starts from a node-kind scan.
    #[must_use]
    pub fn scan(mut self, kind: NodeKind) -> Self {
        self.steps.push(RuleNode::NodeScan {
            kind: Some(kind),
            visibility: None,
            name_pattern: None,
        });
        self
    }

    /// Starts from a fully specified node scan.
    #[must_use]
    pub fn scan_with(
        mut self,
        kind: Option<NodeKind>,
        visibility: Option<Visibility>,
        name_pattern: Option<StringPattern>,
    ) -> Self {
        self.steps.push(RuleNode::NodeScan {
            kind,
            visibility,
            name_pattern,
        });
        self
    }

    /// Appends a planner predicate filter.
    #[must_use]
    pub fn filter(mut self, predicate: Predicate) -> Self {
        self.steps.push(RuleNode::Filter { predicate });
        self
    }

    /// Appends a rule-level edge traversal.
    #[must_use]
    pub fn traverse(
        mut self,
        direction: Direction,
        edge_class: Option<RuleEdgeClass>,
        max_depth: u32,
    ) -> Self {
        self.steps.push(RuleNode::EdgeTraversal {
            direction,
            edge_class,
            max_depth,
            resolved_via: None,
            cross_boundary: None,
            emit: TraversalEmit::ReachedNodes,
        });
        self
    }

    /// Appends a rule-level call traversal with resolution provenance.
    #[must_use]
    pub fn traverse_with_resolved_via(
        mut self,
        direction: Direction,
        edge_class: Option<RuleEdgeClass>,
        resolved_via: Option<ResolvedVia>,
        max_depth: u32,
    ) -> Self {
        self.steps.push(RuleNode::EdgeTraversal {
            direction,
            edge_class,
            max_depth,
            resolved_via,
            cross_boundary: None,
            emit: TraversalEmit::ReachedNodes,
        });
        self
    }

    /// Appends a rule-level edge traversal restricted by cross-boundary status.
    ///
    /// `cross_boundary` is `Some(true)` to keep only cross-boundary (FFI /
    /// cross-language / service) edges, `Some(false)` to keep only
    /// intra-language edges, or `None` to ignore boundary status (equivalent
    /// to [`RuleBuilder::traverse`]). A set `Some(_)` forces the
    /// witness-bearing backend traversal path; the sqry-db planner has no
    /// cross-boundary concept and refuses to lower it.
    #[must_use]
    pub fn traverse_cross_boundary(
        mut self,
        direction: Direction,
        edge_class: Option<RuleEdgeClass>,
        max_depth: u32,
        cross_boundary: Option<bool>,
    ) -> Self {
        self.steps.push(RuleNode::EdgeTraversal {
            direction,
            edge_class,
            max_depth,
            resolved_via: None,
            cross_boundary,
            emit: TraversalEmit::ReachedNodes,
        });
        self
    }

    /// Appends a rule-level edge traversal that emits a chosen node set.
    ///
    /// `emit` selects the step output: [`TraversalEmit::ReachedNodes`] (seeds +
    /// reached, the default), [`TraversalEmit::EdgeSources`] (nodes with a
    /// qualifying out-edge), or [`TraversalEmit::EdgeTargets`] (nodes reached by
    /// a qualifying edge). A non-default `emit` (like a set `cross_boundary`)
    /// forces the witness-bearing backend traversal path; the sqry-db planner
    /// has no emit concept and refuses to lower it.
    #[must_use]
    pub fn traverse_emitting(
        mut self,
        direction: Direction,
        edge_class: Option<RuleEdgeClass>,
        max_depth: u32,
        cross_boundary: Option<bool>,
        emit: TraversalEmit,
    ) -> Self {
        self.steps.push(RuleNode::EdgeTraversal {
            direction,
            edge_class,
            max_depth,
            resolved_via: None,
            cross_boundary,
            emit,
        });
        self
    }

    /// Adopts an existing set-only planner plan.
    #[must_use]
    pub fn from_query_plan(plan: sqry_db::planner::QueryPlan) -> Self {
        Self {
            steps: vec![RulePlan::from_query_plan(plan).root],
        }
    }

    /// Builds a single path-query plan.
    #[must_use]
    pub fn path_query(
        from: RuleEndpoint,
        to: RuleEndpoint,
        kind: PathKind,
        max_depth: u32,
        max_paths: Option<u32>,
    ) -> Self {
        Self {
            steps: vec![RuleNode::PathQuery {
                from,
                to,
                kind,
                max_depth,
                max_paths,
                avoid: None,
            }],
        }
    }

    /// Builds a path query that excludes paths passing through the `avoid`
    /// endpoint: "reachable from `from` to `to` WITHOUT traversing `avoid`".
    #[must_use]
    pub fn path_query_avoiding(
        from: RuleEndpoint,
        to: RuleEndpoint,
        avoid: RuleEndpoint,
        kind: PathKind,
        max_depth: u32,
        max_paths: Option<u32>,
    ) -> Self {
        Self {
            steps: vec![RuleNode::PathQuery {
                from,
                to,
                kind,
                max_depth,
                max_paths,
                avoid: Some(avoid),
            }],
        }
    }

    /// Builds a single subgraph-extract plan.
    #[must_use]
    pub fn subgraph_extract(
        seeds: RuleEndpoint,
        edge_classes: Vec<RuleEdgeClass>,
        direction: Direction,
        max_depth: u32,
    ) -> Self {
        Self {
            steps: vec![RuleNode::SubgraphExtract {
                seeds,
                edge_classes,
                direction,
                max_depth,
            }],
        }
    }

    /// Builds a single relation-edge plan.
    #[must_use]
    pub fn relation_edges(from: RuleEndpoint, kind: RelationEdgeKind, with_metadata: bool) -> Self {
        Self {
            steps: vec![RuleNode::RelationEdges {
                from,
                kind,
                with_metadata,
            }],
        }
    }

    /// Builds a single cycle-witness plan.
    #[must_use]
    pub fn cycle_witness(edge_class: RuleEdgeClass, bounds: RuleCycleBounds) -> Self {
        Self {
            steps: vec![RuleNode::CycleWitness { edge_class, bounds }],
        }
    }

    /// Builds a single references-at plan.
    #[must_use]
    pub fn references_at(target: RuleEndpoint) -> Self {
        Self {
            steps: vec![RuleNode::ReferencesAt { target }],
        }
    }

    /// Builds a single complexity-aggregate plan.
    #[must_use]
    pub fn complexity_aggregate(
        node_kind_filter: Option<NodeKind>,
        metric: ComplexityMetric,
    ) -> Self {
        Self {
            steps: vec![RuleNode::ComplexityAggregate {
                node_kind_filter,
                metric,
            }],
        }
    }

    /// Builds a single cross-snapshot diff plan.
    #[must_use]
    pub fn cross_snapshot_diff(
        base: SnapshotId,
        head: SnapshotId,
        include_unchanged: bool,
    ) -> Self {
        Self {
            steps: vec![RuleNode::CrossSnapshotDiff {
                base,
                head,
                include_unchanged,
            }],
        }
    }

    /// Builds a single entry-point union plan.
    #[must_use]
    pub fn entry_point_union(extensions: Vec<EntrypointExtension>) -> Self {
        Self {
            steps: vec![RuleNode::EntryPointUnion { extensions }],
        }
    }

    /// Builds a single similarity plan.
    #[must_use]
    pub fn similar_to(
        seed: RuleEndpoint,
        scope: Option<RuleEndpoint>,
        similarity_kind: RuleSimilarityKind,
    ) -> Self {
        Self {
            steps: vec![RuleNode::SimilarTo {
                seed,
                scope,
                similarity_kind,
            }],
        }
    }

    /// Combines two plans through a set operation.
    #[must_use]
    pub fn set_op(op: SetOperation, left: RulePlan, right: RulePlan) -> Self {
        Self {
            steps: vec![RuleNode::SetOp {
                op,
                left: Box::new(left.root),
                right: Box::new(right.root),
            }],
        }
    }

    /// Builds the canonical rule plan.
    ///
    /// # Errors
    ///
    /// Returns `RuleError::InvalidRuleSource` when no steps were added or an
    /// edge traversal has zero depth.
    pub fn build(self) -> RuleResult<RulePlan> {
        if self.steps.is_empty() {
            return Err(RuleError::InvalidRuleSource {
                reason: "rule builder contains no steps",
            });
        }

        for step in &self.steps {
            reject_zero_depth(step)?;
        }

        let mut steps = self.steps;
        let root = if steps.len() == 1 {
            steps.remove(0)
        } else {
            RuleNode::Chain { steps }
        };

        Ok(RulePlan::new(root))
    }

    /// Builds a set-only `sqry-db` planner scan and converts it to rule IR.
    ///
    /// # Errors
    ///
    /// Returns the underlying planner build error wrapped as analysis
    /// infrastructure failure.
    pub fn scan_query(kind: NodeKind, name: StringPattern) -> RuleResult<RulePlan> {
        let plan = QueryBuilder::new()
            .scan(kind)
            .filter(Predicate::MatchesName(name))
            .build()
            .map_err(anyhow::Error::from)?;
        Ok(RulePlan::from_query_plan(plan))
    }

    /// Helper for tests and examples.
    #[must_use]
    pub fn node_endpoint(nodes: Vec<NodeId>) -> RuleEndpoint {
        RuleEndpoint::Nodes(nodes)
    }

    /// Helper for tests and examples.
    #[must_use]
    pub fn path_extension(pattern: impl Into<PathPattern>) -> EntrypointExtension {
        EntrypointExtension::Path(pattern.into())
    }
}

fn reject_zero_depth(node: &RuleNode) -> RuleResult<()> {
    match node {
        RuleNode::EdgeTraversal { max_depth, .. }
        | RuleNode::PathQuery { max_depth, .. }
        | RuleNode::SubgraphExtract { max_depth, .. }
            if *max_depth == 0 =>
        {
            Err(RuleError::InvalidRuleSource {
                reason: "rule traversal depth must be greater than zero",
            })
        }
        RuleNode::SetOp { left, right, .. } => {
            reject_zero_depth(left)?;
            reject_zero_depth(right)
        }
        RuleNode::Chain { steps } => {
            for step in steps {
                reject_zero_depth(step)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
