//! Ordered step vocabulary for rule witnesses.

use serde::{Deserialize, Serialize};
use sqry_core::graph::unified::{NodeId, NodeKind};
use sqry_core::schema::Visibility;
use sqry_db::planner::{Direction, SetOperation, StringPattern};

use crate::ir::{RuleEdgeClass, RuleSimilarityKind};

use super::RuleCitation;

/// Default maximum number of witness steps retained per rule firing.
pub const DEFAULT_RULE_WITNESS_STEP_CAP: usize = 1024;

/// Predicate families recorded in witness steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RulePredicateKind {
    /// Name predicate.
    Name,
    /// Kind predicate.
    Kind,
    /// Visibility predicate.
    Visibility,
    /// File/path predicate.
    File,
    /// Relation predicate.
    Relation,
    /// Boolean combinator predicate.
    Boolean,
    /// Other bounded planner predicate.
    Other,
}

/// Path-budget exhaustion reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathBudgetReason {
    /// Maximum hop count reached.
    MaxHops,
    /// Maximum path count reached.
    MaxPaths,
    /// Caller cancelled the rule run.
    Cancelled,
}

/// Diff row family emitted by cross-snapshot rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffEntryKind {
    /// Added symbol or edge.
    Added,
    /// Removed symbol or edge.
    Removed,
    /// Modified symbol or edge.
    Modified,
    /// Unchanged entry included by request.
    Unchanged,
}

/// Rule firing severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSeverity {
    /// Informational rule result.
    Info,
    /// Warning rule result.
    Warning,
    /// Error rule result.
    Error,
}

/// One ordered step in a rule witness.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleStep {
    /// A node scan matched zero or more nodes.
    NodeScanMatched {
        /// Optional node kind filter.
        kind: Option<NodeKind>,
        /// Optional visibility filter.
        visibility: Option<Visibility>,
        /// Optional name pattern.
        name_pattern: Option<StringPattern>,
        /// Number of matched nodes.
        match_count: u32,
    },
    /// A graph edge was traversed.
    EdgeTraversed {
        /// Source node.
        from: NodeId,
        /// Target node.
        to: NodeId,
        /// Direction used by the rule.
        direction: Direction,
        /// Rule-level edge class.
        edge_classification: RuleEdgeClass,
        /// Traversal depth.
        depth: u32,
    },
    /// A predicate was applied to an input set.
    PredicateApplied {
        /// Predicate family.
        predicate_kind: RulePredicateKind,
        /// Input cardinality.
        inputs: u32,
        /// Output cardinality.
        outputs: u32,
    },
    /// A set operation was evaluated.
    SetOpEvaluated {
        /// Set operation.
        op: SetOperation,
        /// Left-hand cardinality.
        lhs_card: u32,
        /// Right-hand cardinality.
        rhs_card: u32,
        /// Result cardinality.
        result_card: u32,
    },
    /// A path was constructed.
    PathConstructed {
        /// Source node.
        from: NodeId,
        /// Target node.
        to: NodeId,
        /// Number of hops.
        length: u32,
        /// Edge classes used by the path.
        edge_classes: Vec<RuleEdgeClass>,
        /// Full node sequence.
        nodes: Vec<NodeId>,
    },
    /// Path search exhausted a configured budget.
    PathBudgetExhausted {
        /// Exhaustion reason.
        reason: PathBudgetReason,
    },
    /// A relation edge row was emitted.
    RelationEdgeEmitted {
        /// Source node.
        from: NodeId,
        /// Target node.
        to: NodeId,
        /// Relation edge kind.
        kind: RuleEdgeClass,
        /// Whether metadata was emitted.
        with_metadata: bool,
    },
    /// A cycle witness was emitted.
    CycleDetected {
        /// Stable component ordinal inside the rule result.
        component_id: u32,
        /// Cycle length.
        length: u32,
        /// Ordered cycle nodes.
        nodes: Vec<NodeId>,
    },
    /// A reference source location was emitted.
    ReferenceLocated {
        /// Referencing node.
        source: NodeId,
        /// Target node.
        target: NodeId,
        /// Citation index in the enclosing witness.
        citation_index: u32,
    },
    /// A metric was computed.
    MetricComputed {
        /// Stable metric name.
        metric: String,
        /// Computed integer value.
        value: u64,
        /// Number of nodes included in the aggregate.
        node_count: u32,
    },
    /// A cross-snapshot diff row was emitted.
    DiffEntryEmitted {
        /// Diff row family.
        kind: DiffEntryKind,
        /// Base node, when present.
        base: Option<NodeId>,
        /// Head node, when present.
        head: Option<NodeId>,
    },
    /// A node was classified as an entry point.
    EntryPointClassified {
        /// Stable classifier label.
        classifier: String,
        /// Classified node.
        node: NodeId,
    },
    /// A similarity match was emitted.
    SimilarityMatchEmitted {
        /// Seed node.
        seed: NodeId,
        /// Matched node.
        matched: NodeId,
        /// Similarity score in basis points.
        score: u16,
        /// Similarity family.
        similarity_kind: RuleSimilarityKind,
    },
    /// Rule terminal firing event.
    RuleFired {
        /// Stable rule identifier.
        rule_id: String,
        /// Rule severity.
        severity: RuleSeverity,
    },
    /// Witness step list was truncated.
    WitnessTruncated {
        /// Number of original steps dropped.
        dropped: u32,
        /// Configured step cap after zero is normalized to one.
        cap: u32,
    },
}

impl RuleStep {
    /// Stable variant name for completeness tests and diagnostics.
    #[must_use]
    pub const fn variant_name(&self) -> &'static str {
        match self {
            Self::NodeScanMatched { .. } => "NodeScanMatched",
            Self::EdgeTraversed { .. } => "EdgeTraversed",
            Self::PredicateApplied { .. } => "PredicateApplied",
            Self::SetOpEvaluated { .. } => "SetOpEvaluated",
            Self::PathConstructed { .. } => "PathConstructed",
            Self::PathBudgetExhausted { .. } => "PathBudgetExhausted",
            Self::RelationEdgeEmitted { .. } => "RelationEdgeEmitted",
            Self::CycleDetected { .. } => "CycleDetected",
            Self::ReferenceLocated { .. } => "ReferenceLocated",
            Self::MetricComputed { .. } => "MetricComputed",
            Self::DiffEntryEmitted { .. } => "DiffEntryEmitted",
            Self::EntryPointClassified { .. } => "EntryPointClassified",
            Self::SimilarityMatchEmitted { .. } => "SimilarityMatchEmitted",
            Self::RuleFired { .. } => "RuleFired",
            Self::WitnessTruncated { .. } => "WitnessTruncated",
        }
    }
}

/// Witness-bearing rule result envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleWitness {
    /// Ordered rule execution steps.
    pub steps: Vec<RuleStep>,
    /// Source citations referenced by steps.
    pub citations: Vec<RuleCitation>,
    /// Whether steps were truncated to the configured cap.
    pub truncated: bool,
}

impl RuleWitness {
    /// Creates an untruncated witness.
    #[must_use]
    pub const fn new(steps: Vec<RuleStep>, citations: Vec<RuleCitation>) -> Self {
        Self {
            steps,
            citations,
            truncated: false,
        }
    }

    /// Creates a witness with the default step cap.
    #[must_use]
    pub fn with_default_cap(steps: Vec<RuleStep>, citations: Vec<RuleCitation>) -> Self {
        Self::with_step_cap(steps, citations, DEFAULT_RULE_WITNESS_STEP_CAP)
    }

    /// Creates a witness with a bounded step list and truncation marker.
    #[must_use]
    pub fn with_step_cap(
        mut steps: Vec<RuleStep>,
        citations: Vec<RuleCitation>,
        step_cap: usize,
    ) -> Self {
        let effective_cap = step_cap.max(1);
        if steps.len() <= effective_cap {
            return Self::new(steps, citations);
        }

        let retained_before_marker = effective_cap - 1;
        let dropped = steps.len() - retained_before_marker;
        steps.truncate(retained_before_marker);
        steps.push(RuleStep::WitnessTruncated {
            dropped: u32::try_from(dropped).unwrap_or(u32::MAX),
            cap: u32::try_from(effective_cap).unwrap_or(u32::MAX),
        });

        Self {
            steps,
            citations,
            truncated: true,
        }
    }
}
