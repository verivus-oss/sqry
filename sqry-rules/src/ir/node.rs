//! Rule IR node vocabulary.

use serde::{Deserialize, Serialize};
use sqry_core::graph::unified::{EdgeClassification, NodeId, NodeKind, ResolvedVia};
use sqry_core::schema::Visibility;
use sqry_db::planner::{Direction, PathPattern, PlanNode, Predicate, SetOperation, StringPattern};

use crate::backend::SnapshotId;

/// Serializable edge class vocabulary used by rule source and IR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleEdgeClass {
    /// Function/method calls and cross-boundary calls.
    Call,
    /// Imports and exports.
    Import,
    /// References.
    Reference,
    /// Inheritance and implementation edges.
    Inheritance,
    /// Containment and definition edges.
    Structural,
    /// Type annotation / association edges.
    Type,
    /// Database access edges.
    Database,
    /// Service interaction edges.
    Service,
}

impl RuleEdgeClass {
    /// Returns whether this rule class accepts a materialized edge
    /// classification.
    #[must_use]
    pub const fn accepts(self, classification: EdgeClassification) -> bool {
        matches!(
            (self, classification),
            (Self::Call, EdgeClassification::Call { .. })
                | (
                    Self::Import,
                    EdgeClassification::Import { .. } | EdgeClassification::Export { .. },
                )
                | (Self::Reference, EdgeClassification::Reference)
                | (
                    Self::Inheritance,
                    EdgeClassification::Inherits | EdgeClassification::Implements,
                )
                | (
                    Self::Structural,
                    EdgeClassification::Contains | EdgeClassification::Defines,
                )
                | (Self::Type, EdgeClassification::TypeOf)
                | (Self::Database, EdgeClassification::DatabaseAccess)
                | (Self::Service, EdgeClassification::ServiceInteraction)
        )
    }
}

impl From<EdgeClassification> for RuleEdgeClass {
    fn from(value: EdgeClassification) -> Self {
        match value {
            EdgeClassification::Call { .. } => Self::Call,
            EdgeClassification::Import { .. } | EdgeClassification::Export { .. } => Self::Import,
            EdgeClassification::Reference => Self::Reference,
            EdgeClassification::Inherits | EdgeClassification::Implements => Self::Inheritance,
            EdgeClassification::Contains | EdgeClassification::Defines => Self::Structural,
            EdgeClassification::TypeOf => Self::Type,
            EdgeClassification::DatabaseAccess => Self::Database,
            EdgeClassification::ServiceInteraction => Self::Service,
        }
    }
}

/// Endpoint used by path, references, and similarity operators.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleEndpoint {
    /// Explicit node IDs.
    Nodes(Vec<NodeId>),
    /// Nodes produced by a nested rule node.
    Query(Box<RuleNode>),
}

/// Path traversal intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    /// Any edge class selected by the backend default.
    Any,
    /// Calls only.
    Calls,
    /// Dependency-impact edge set.
    Dependency,
}

/// Relation-edge families emitted by `RelationEdges`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationEdgeKind {
    /// Calls incoming to a symbol.
    Callers,
    /// Calls outgoing from a symbol.
    Callees,
    /// Imports.
    Imports,
    /// Exports.
    Exports,
    /// References.
    References,
    /// Implements.
    Implements,
}

/// Complexity metric computed by `ComplexityAggregate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComplexityMetric {
    /// Number of outgoing calls per selected node.
    OutgoingCalls,
    /// Number of incoming calls per selected node.
    IncomingCalls,
    /// Total selected node count.
    NodeCount,
}

/// Additional entry-point classifier used by `EntryPointUnion`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntrypointExtension {
    /// Match nodes by symbol name.
    Name(StringPattern),
    /// Match nodes by file path.
    Path(PathPattern),
    /// Include explicit node IDs.
    Nodes(Vec<NodeId>),
}

/// Similarity family used by `SimilarTo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSimilarityKind {
    /// Structural duplicates.
    Duplicate,
    /// Near-neighbour similarity.
    Similar,
}

/// Fixed-width cycle bounds for persisted rule IR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleCycleBounds {
    /// Minimum cycle depth to report.
    pub min_depth: u32,
    /// Maximum cycle depth to report.
    pub max_depth: Option<u32>,
    /// Maximum number of cycles to return.
    pub max_results: u32,
    /// Whether size-1 self loops count as cycles.
    pub should_include_self_loops: bool,
}

impl Default for RuleCycleBounds {
    fn default() -> Self {
        Self {
            min_depth: 2,
            max_depth: None,
            max_results: 100,
            should_include_self_loops: false,
        }
    }
}

/// Canonical declarative rule IR.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleNode {
    /// Reused set-only `PlanNode::NodeScan` semantics.
    NodeScan {
        /// Optional node kind.
        kind: Option<NodeKind>,
        /// Optional visibility.
        visibility: Option<Visibility>,
        /// Optional name pattern.
        name_pattern: Option<StringPattern>,
    },
    /// Reused set-only `PlanNode::EdgeTraversal` semantics with rule edge
    /// classes instead of storage discriminants.
    EdgeTraversal {
        /// Direction to traverse.
        direction: Direction,
        /// Optional rule edge class.
        edge_class: Option<RuleEdgeClass>,
        /// Maximum traversal depth.
        max_depth: u32,
        /// Optional call-resolution provenance filter preserved from planner IR.
        resolved_via: Option<ResolvedVia>,
    },
    /// Reused set-only `PlanNode::Filter` semantics.
    Filter {
        /// Planner predicate.
        predicate: Predicate,
    },
    /// Reused set-only `PlanNode::SetOp` semantics.
    SetOp {
        /// Set operation.
        op: SetOperation,
        /// Left operand.
        left: Box<RuleNode>,
        /// Right operand.
        right: Box<RuleNode>,
    },
    /// Ordered rule sequence. Planner-only chains preserve
    /// `PlanNode::Chain` semantics; heterogeneous chains execute each rule
    /// primitive in order and return sequence output.
    Chain {
        /// Ordered rule steps.
        steps: Vec<RuleNode>,
    },
    /// Witness-bearing path query.
    PathQuery {
        /// Source endpoint.
        from: RuleEndpoint,
        /// Target endpoint.
        to: RuleEndpoint,
        /// Path intent.
        kind: PathKind,
        /// Maximum hops.
        max_depth: u32,
        /// Maximum paths to emit.
        max_paths: Option<u32>,
    },
    /// Extracts a bounded subgraph.
    SubgraphExtract {
        /// Seed node expression.
        seeds: RuleEndpoint,
        /// Edge classes to include.
        edge_classes: Vec<RuleEdgeClass>,
        /// Direction to traverse.
        direction: Direction,
        /// Maximum hops.
        max_depth: u32,
    },
    /// Emits relation edge rows.
    RelationEdges {
        /// Source node expression.
        from: RuleEndpoint,
        /// Relation family.
        kind: RelationEdgeKind,
        /// Whether edge metadata should be materialized.
        with_metadata: bool,
    },
    /// Emits a cycle witness.
    CycleWitness {
        /// Edge class to analyze.
        edge_class: RuleEdgeClass,
        /// Cycle bounds.
        bounds: RuleCycleBounds,
    },
    /// Emits references to a target expression.
    ReferencesAt {
        /// Target node expression.
        target: RuleEndpoint,
    },
    /// Aggregates a graph complexity metric.
    ComplexityAggregate {
        /// Optional kind filter.
        node_kind_filter: Option<NodeKind>,
        /// Metric to compute.
        metric: ComplexityMetric,
    },
    /// Cross-snapshot semantic diff. Beside-cache per FR5.
    CrossSnapshotDiff {
        /// Base snapshot.
        base: SnapshotId,
        /// Head snapshot.
        head: SnapshotId,
        /// Whether unchanged nodes are included.
        include_unchanged: bool,
    },
    /// Entry-point set extended with additional classifiers.
    EntryPointUnion {
        /// Extension classifiers.
        extensions: Vec<EntrypointExtension>,
    },
    /// Similarity query. Beside-cache per FR5.
    SimilarTo {
        /// Seed expression.
        seed: RuleEndpoint,
        /// Optional scope expression.
        scope: Option<RuleEndpoint>,
        /// Similarity family.
        similarity_kind: RuleSimilarityKind,
    },
}

impl From<PlanNode> for RuleNode {
    fn from(value: PlanNode) -> Self {
        match value {
            PlanNode::NodeScan {
                kind,
                visibility,
                name_pattern,
            } => Self::NodeScan {
                kind,
                visibility,
                name_pattern,
            },
            PlanNode::EdgeTraversal {
                direction,
                edge_kind,
                max_depth,
                resolved_via,
            } => Self::EdgeTraversal {
                direction,
                edge_class: edge_kind
                    .as_ref()
                    .map(EdgeClassification::from)
                    .map(RuleEdgeClass::from),
                max_depth,
                resolved_via,
            },
            PlanNode::Filter { predicate } => Self::Filter { predicate },
            PlanNode::SetOp { op, left, right } => Self::SetOp {
                op,
                left: Box::new(Self::from(*left)),
                right: Box::new(Self::from(*right)),
            },
            PlanNode::Chain { steps } => Self::Chain {
                steps: steps.into_iter().map(Self::from).collect(),
            },
        }
    }
}

impl RuleNode {
    /// Returns true for the two FR5 beside-cache variants.
    #[must_use]
    pub const fn is_beside_cache(&self) -> bool {
        matches!(
            self,
            Self::CrossSnapshotDiff { .. } | Self::SimilarTo { .. }
        )
    }
}
