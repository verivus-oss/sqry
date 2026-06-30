//! Adapter boundary between declarative rules and sqry analysis storage.

use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqry_core::graph::unified::bind::BindingPlane;
use sqry_core::graph::unified::{
    EdgeFilter, GraphSnapshot, NodeId, TraversalDirection, TraversalLimits, TraversalResult,
};
use sqry_db::queries::{
    CondensationValue, CyclesKey, CyclesValue, ReachableSet, RelationKey, SccValue, UnusedKey,
    UnusedValue,
};
use sqry_db::{ComparativeQueryDb, QueryDb, planner::QueryPlan};

use crate::RuleResult;
use crate::ir::{RelationEdgeKind, RuleEdgeClass};

mod sqry_db_backend;

pub use sqry_db_backend::{CycleClass, SqryDbRuleBackend, edge_filter_to_edge_kind_probes};

/// Canonical `RuleBackend` method set from the Phase 5 FR4 contract.
pub const RULE_BACKEND_METHODS: [&str; 20] = [
    "snapshot_id",
    "binding",
    "traverse",
    "callers",
    "callees",
    "imports",
    "exports",
    "references",
    "relation_from_node",
    "cycles",
    "is_in_cycle",
    "unused",
    "entry_points",
    "reachable_from_entry_points",
    "reachability",
    "scc",
    "condensation",
    "trace_path",
    "run_plan",
    "comparative",
];

/// Stable facade identity for the snapshot view a rule backend exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotId {
    /// Global edge revision observed by the backend.
    pub edge_revision: u64,
    /// Global metadata revision observed by the backend.
    pub metadata_revision: u64,
}

impl SnapshotId {
    /// Builds a snapshot identity from the public `QueryDb` revision counters.
    #[must_use]
    pub fn from_query_db(db: &QueryDb) -> Self {
        Self {
            edge_revision: db.edge_revision(),
            metadata_revision: db.metadata_revision(),
        }
    }
}

/// A node path returned by rule-layer path queries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RulePath {
    /// Ordered node IDs from source to target.
    pub nodes: Vec<NodeId>,
}

impl RulePath {
    /// Creates a path from an ordered node list.
    #[must_use]
    pub fn new(nodes: Vec<NodeId>) -> Self {
        Self { nodes }
    }
}

/// Backend input for path enumeration.
#[derive(Debug, Clone)]
pub struct TracePathKey {
    /// Source nodes.
    pub sources: Vec<NodeId>,
    /// Target node.
    pub target: NodeId,
    /// Direction to traverse.
    pub direction: TraversalDirection,
    /// Edge classes to follow.
    pub edge_filter: EdgeFilter,
    /// Traversal limits.
    pub limits: TraversalLimits,
    /// Minimum followability confidence in basis points.
    pub min_confidence_bps: u16,
    /// Whether cross-language or cross-service edges may be followed.
    pub allow_cross_language: bool,
}

/// Rule-layer reachability query key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleReachabilityKey {
    /// Root nodes to start from.
    pub roots: Vec<NodeId>,
    /// Rule-level edge class to traverse.
    pub edge_class: RuleEdgeClass,
}

/// Rule-layer SCC / condensation key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleTopologyKey {
    /// Rule-level edge class to analyze.
    pub edge_class: RuleEdgeClass,
}

impl TracePathKey {
    /// Returns the minimum confidence as an executor ratio.
    #[must_use]
    pub fn min_confidence(&self) -> f64 {
        f64::from(self.min_confidence_bps) / 10_000.0
    }
}

/// Adapter trait consumed by the declarative rule engine.
///
/// The trait deliberately exposes `EdgeFilter` / edge-class-shaped inputs at
/// the rule layer and keeps storage discriminants inside concrete backend
/// adapters.
#[allow(
    clippy::missing_errors_doc,
    reason = "RuleBackend methods share one backend-specific error contract documented on the trait; individual implementors supply concrete failure classes."
)]
pub trait RuleBackend {
    /// Identity of the current snapshot view.
    fn snapshot_id(&self) -> SnapshotId;

    /// Binding-plane facade for the current snapshot.
    fn binding(&self) -> BindingPlane<'_>;

    /// Generic graph traversal through the public kernel facade.
    fn traverse(
        &self,
        seeds: &[NodeId],
        direction: TraversalDirection,
        edge_filter: EdgeFilter,
        limits: TraversalLimits,
    ) -> RuleResult<TraversalResult>;

    /// Transport-facing callers query.
    fn callers(&self, key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>>;

    /// Transport-facing callees query.
    fn callees(&self, key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>>;

    /// Import relation query.
    fn imports(&self, key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>>;

    /// Export relation query.
    fn exports(&self, key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>>;

    /// Reference relation query.
    fn references(&self, key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>>;

    /// NodeId-anchored relation query.
    ///
    /// This is the rule-layer primitive for explicit-node endpoints and for
    /// rule steps that first resolve a nested endpoint to concrete graph nodes.
    /// Unlike [`RelationKey`]-based methods, this never attempts to encode a
    /// `NodeId` as a synthetic symbol name.
    fn relation_from_node(
        &self,
        node: NodeId,
        kind: RelationEdgeKind,
    ) -> RuleResult<Arc<Vec<NodeId>>>;

    /// Cycle query.
    fn cycles(&self, key: CyclesKey) -> RuleResult<CyclesValue>;

    /// Checks whether a node is in a cycle for the supplied cycle class.
    fn is_in_cycle(&self, node: NodeId, cycle_class: CycleClass) -> RuleResult<bool>;

    /// Unused-node query.
    fn unused(&self, key: UnusedKey) -> RuleResult<UnusedValue>;

    /// Entry-point set.
    fn entry_points(&self) -> RuleResult<Arc<HashSet<NodeId>>>;

    /// Reachable-from-entry-points set.
    fn reachable_from_entry_points(&self) -> RuleResult<Arc<HashSet<NodeId>>>;

    /// Reachability query.
    fn reachability(&self, key: RuleReachabilityKey) -> RuleResult<Arc<ReachableSet>>;

    /// Strongly connected components.
    fn scc(&self, key: RuleTopologyKey) -> RuleResult<SccValue>;

    /// Condensation DAG.
    fn condensation(&self, key: RuleTopologyKey) -> RuleResult<CondensationValue>;

    /// Enumerates paths between source nodes and a target.
    fn trace_path(&self, key: TracePathKey) -> RuleResult<Arc<Vec<RulePath>>>;

    /// Executes a set-only planner query.
    fn run_plan(&self, plan: &QueryPlan) -> RuleResult<Arc<Vec<NodeId>>>;

    /// Builds a comparative database for two known snapshot IDs.
    fn comparative(
        &self,
        base: SnapshotId,
        head: SnapshotId,
    ) -> RuleResult<Arc<ComparativeQueryDb>>;
}

/// Returns a binding plane from a public snapshot reference.
#[must_use]
pub fn binding_plane(snapshot: &GraphSnapshot) -> BindingPlane<'_> {
    snapshot.binding_plane()
}

#[cfg(test)]
#[path = "tests/fake_backend.rs"]
pub mod fake_backend;
