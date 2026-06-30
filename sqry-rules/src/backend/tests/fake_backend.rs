use std::collections::HashSet;
use std::sync::Arc;

use sqry_core::graph::unified::bind::BindingPlane;
use sqry_core::graph::unified::{
    CodeGraph, EdgeFilter, GraphSnapshot, NodeId, TraversalDirection, TraversalLimits,
    TraversalMetadata, TraversalResult,
};
use sqry_db::ComparativeQueryDb;
use sqry_db::planner::{QueryBuilder, QueryPlan};
use sqry_db::queries::{
    CachedCondensation, CachedSccData, CondensationKey, CondensationValue, CyclesKey, CyclesValue,
    ReachableSet, RelationKey, SccValue, UnusedKey, UnusedValue,
};

use super::{
    CycleClass, RULE_BACKEND_METHODS, RuleBackend, RulePath, RuleReachabilityKey, RuleTopologyKey,
    SnapshotId, TracePathKey,
};
use crate::ir::RelationEdgeKind;
use crate::{RuleError, RuleResult};

/// Public fake backend for sibling unit tests.
pub struct FakeBackend {
    snapshot: Arc<GraphSnapshot>,
    nodes: Arc<Vec<NodeId>>,
    set: Arc<HashSet<NodeId>>,
}

impl FakeBackend {
    /// Creates an empty fake backend.
    pub fn new() -> Self {
        let graph = CodeGraph::new();
        Self {
            snapshot: Arc::new(graph.snapshot()),
            nodes: Arc::new(Vec::new()),
            set: Arc::new(HashSet::new()),
        }
    }
}

impl Default for FakeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RuleBackend for FakeBackend {
    fn snapshot_id(&self) -> SnapshotId {
        SnapshotId {
            edge_revision: 7,
            metadata_revision: 11,
        }
    }

    fn binding(&self) -> BindingPlane<'_> {
        self.snapshot.binding_plane()
    }

    fn traverse(
        &self,
        _seeds: &[NodeId],
        _direction: TraversalDirection,
        _edge_filter: EdgeFilter,
        _limits: TraversalLimits,
    ) -> RuleResult<TraversalResult> {
        Ok(TraversalResult {
            nodes: Vec::new(),
            edges: Vec::new(),
            paths: None,
            metadata: TraversalMetadata {
                truncation: None,
                max_depth_reached: false,
                seed_count: 0,
                nodes_visited: 0,
                total_nodes: 0,
                total_edges: 0,
            },
        })
    }

    fn callers(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.nodes))
    }

    fn callees(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.nodes))
    }

    fn imports(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.nodes))
    }

    fn exports(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.nodes))
    }

    fn references(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.nodes))
    }

    fn relation_from_node(
        &self,
        _node: NodeId,
        _kind: RelationEdgeKind,
    ) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.nodes))
    }

    fn cycles(&self, _key: CyclesKey) -> RuleResult<CyclesValue> {
        Ok(Arc::new(Vec::new()))
    }

    fn is_in_cycle(&self, _node: NodeId, _cycle_class: CycleClass) -> RuleResult<bool> {
        Ok(false)
    }

    fn unused(&self, _key: UnusedKey) -> RuleResult<UnusedValue> {
        Ok(Arc::clone(&self.nodes))
    }

    fn entry_points(&self) -> RuleResult<Arc<HashSet<NodeId>>> {
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
        Ok(Arc::new(Vec::new()))
    }

    fn run_plan(&self, _plan: &QueryPlan) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.nodes))
    }

    fn comparative(
        &self,
        _base: SnapshotId,
        _head: SnapshotId,
    ) -> RuleResult<Arc<ComparativeQueryDb>> {
        Err(RuleError::UnsupportedPrimitive {
            backend: "fake",
            primitive: "comparative",
            reason: "fake backend does not carry multiple snapshots",
        })
    }
}

#[test]
fn rule_backend_method_set_has_fr4_shape() {
    assert_eq!(RULE_BACKEND_METHODS.len(), 20);
    assert_eq!(
        RULE_BACKEND_METHODS,
        [
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
        ]
    );
}

#[test]
fn fake_backend_is_usable_for_rule_unit_tests() {
    let backend = FakeBackend::new();
    let plan = QueryBuilder::new()
        .scan_all()
        .build()
        .expect("scan_all plan is valid");

    assert_eq!(backend.snapshot_id().edge_revision, 7);
    assert!(
        backend
            .run_plan(&plan)
            .expect("fake plan result")
            .is_empty()
    );
    assert!(
        backend
            .callers(RelationKey::exact("anything"))
            .expect("fake callers")
            .is_empty()
    );
}
