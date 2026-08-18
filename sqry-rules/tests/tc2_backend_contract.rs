//! TC2: `RuleBackend` trait contract.
//!
//! The crate ships a `#[cfg(test)]`-only `FakeBackend` for its own unit
//! tests, so this integration test defines its own external implementation.
//! That proves the trait is implementable from outside the crate using only
//! the published surface (the whole point of the FR4 adapter boundary), and
//! that the engine round-trips against a fake without panicking on malformed
//! rule sources.

use std::collections::HashSet;
use std::sync::Arc;

use sqry_core::graph::unified::bind::BindingPlane;
use sqry_core::graph::unified::{
    CodeGraph, EdgeFilter, GraphSnapshot, NodeId, TraversalDirection, TraversalLimits,
    TraversalMetadata, TraversalResult,
};
use sqry_db::ComparativeQueryDb;
use sqry_db::planner::{Predicate, QueryPlan};
use sqry_db::queries::{
    CachedCondensation, CachedSccData, CondensationKey, CondensationValue, CyclesKey, CyclesValue,
    ReachableSet, RelationKey, SccValue, UnusedKey, UnusedValue,
};

use sqry_rules::ir::{RelationEdgeKind, RuleEndpoint};
use sqry_rules::{
    CycleClass, RULE_BACKEND_METHODS, RuleBackend, RuleEngine, RuleError, RuleNode, RuleOutput,
    RulePath, RulePlan, RuleReachabilityKey, RuleRelationRows, RuleResult, RuleStructuralNeighbor,
    RuleTopologyKey, SnapshotId, TracePathKey,
};

const NODE_A: NodeId = NodeId::new(1, 1);
const NODE_B: NodeId = NodeId::new(2, 1);

/// Minimal external `RuleBackend` returning a fixed node set for relation
/// queries and empty results everywhere else.
struct FakeBackend {
    snapshot: Arc<GraphSnapshot>,
    nodes: Arc<Vec<NodeId>>,
    set: Arc<HashSet<NodeId>>,
}

impl FakeBackend {
    fn with_nodes(nodes: Vec<NodeId>) -> Self {
        Self {
            snapshot: Arc::new(CodeGraph::new().snapshot()),
            nodes: Arc::new(nodes),
            set: Arc::new(HashSet::new()),
        }
    }
}

impl RuleBackend for FakeBackend {
    fn snapshot_id(&self) -> SnapshotId {
        SnapshotId {
            edge_revision: 3,
            metadata_revision: 5,
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
            reason: "fake backend carries a single snapshot",
        })
    }

    fn structural_neighbors(
        &self,
        _probe: NodeId,
        _similarity_floor: f32,
        _max_results: usize,
    ) -> RuleResult<Vec<RuleStructuralNeighbor>> {
        Ok(Vec::new())
    }
}

#[test]
fn external_backend_round_trips_a_relation_rule() {
    let backend = FakeBackend::with_nodes(vec![NODE_A, NODE_B]);
    let plan = RulePlan::new(RuleNode::RelationEdges {
        from: RuleEndpoint::Nodes(vec![NODE_A]),
        kind: RelationEdgeKind::Callers,
        with_metadata: true,
    });

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("relation rule round-trips against the external backend");

    assert_eq!(
        run.output,
        RuleOutput::Relations(RuleRelationRows {
            kind: RelationEdgeKind::Callers,
            nodes: vec![NODE_A, NODE_B],
            with_metadata: true,
        })
    );
}

#[test]
fn malformed_rule_source_returns_typed_error_without_panicking() {
    let backend = FakeBackend::with_nodes(Vec::new());
    // A bare `Filter` root has no chain input, which the engine must reject as
    // a typed error rather than panicking.
    let plan = RulePlan::new(RuleNode::Filter {
        predicate: Predicate::HasCaller,
    });

    let error = RuleEngine::new()
        .run(&backend, &plan)
        .expect_err("filter root is invalid rule source");

    assert!(matches!(error, RuleError::InvalidRuleSource { .. }));
}

#[test]
fn rule_backend_method_set_has_fr4_shape() {
    assert_eq!(RULE_BACKEND_METHODS.len(), 21);
    assert_eq!(RULE_BACKEND_METHODS[0], "snapshot_id");
    assert_eq!(
        RULE_BACKEND_METHODS[RULE_BACKEND_METHODS.len() - 1],
        "structural_neighbors"
    );
    assert!(RULE_BACKEND_METHODS.contains(&"comparative"));
    assert!(RULE_BACKEND_METHODS.contains(&"relation_from_node"));
}
