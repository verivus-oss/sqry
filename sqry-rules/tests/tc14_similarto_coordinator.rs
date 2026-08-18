//! TC14: L2a SimilarTo beside-cache coordinator.
//!
//! Proves two things through the published surface:
//!  1. `SimilarTo` now executes in-engine (structural neighbour query on the
//!     current snapshot) and yields `RuleOutput::SimilarityMatches`, so it is no
//!     longer a beside-cache "unsupported" primitive.
//!  2. `CrossSnapshotDiff` stays flagged by `requires_unsupported_beside_cache`,
//!     so L2a did not accidentally un-gate the cross-snapshot path.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sqry_core::graph::unified::bind::BindingPlane;
use sqry_core::graph::unified::{
    CodeGraph, EdgeFilter, GraphSnapshot, NodeId, TraversalDirection, TraversalLimits,
    TraversalMetadata, TraversalResult,
};
use sqry_db::ComparativeQueryDb;
use sqry_db::planner::QueryPlan;
use sqry_db::queries::{
    CachedCondensation, CachedSccData, CondensationKey, CondensationValue, CyclesKey, CyclesValue,
    ReachableSet, RelationKey, SccValue, UnusedKey, UnusedValue,
};

use sqry_rules::backend::SnapshotId;
use sqry_rules::derived::requires_unsupported_beside_cache;
use sqry_rules::ir::{RelationEdgeKind, RuleEndpoint, RuleSimilarityKind};
use sqry_rules::{
    CycleClass, RuleBackend, RuleEngine, RuleError, RuleNode, RuleOutput, RulePath, RulePlan,
    RuleReachabilityKey, RuleResult, RuleStructuralNeighbor, RuleTopologyKey, TracePathKey,
};

const SEED: NodeId = NodeId::new(1, 1);
const EXACT: NodeId = NodeId::new(2, 1);
const NEAR: NodeId = NodeId::new(3, 1);

/// External `RuleBackend` with an injectable structural-neighbour map; every
/// other method returns empty. Proves the L2a primitive is reachable from
/// outside the crate.
struct NeighbourBackend {
    snapshot: Arc<GraphSnapshot>,
    empty: Arc<Vec<NodeId>>,
    set: Arc<HashSet<NodeId>>,
    neighbours: HashMap<NodeId, Vec<RuleStructuralNeighbor>>,
}

impl NeighbourBackend {
    fn new(probe: NodeId, neighbours: Vec<RuleStructuralNeighbor>) -> Self {
        let mut map = HashMap::new();
        map.insert(probe, neighbours);
        Self {
            snapshot: Arc::new(CodeGraph::new().snapshot()),
            empty: Arc::new(Vec::new()),
            set: Arc::new(HashSet::new()),
            neighbours: map,
        }
    }
}

impl RuleBackend for NeighbourBackend {
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
        Ok(Arc::clone(&self.empty))
    }

    fn callees(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.empty))
    }

    fn imports(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.empty))
    }

    fn exports(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.empty))
    }

    fn references(&self, _key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.empty))
    }

    fn relation_from_node(
        &self,
        _node: NodeId,
        _kind: RelationEdgeKind,
    ) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::clone(&self.empty))
    }

    fn cycles(&self, _key: CyclesKey) -> RuleResult<CyclesValue> {
        Ok(Arc::new(Vec::new()))
    }

    fn is_in_cycle(&self, _node: NodeId, _cycle_class: CycleClass) -> RuleResult<bool> {
        Ok(false)
    }

    fn unused(&self, _key: UnusedKey) -> RuleResult<UnusedValue> {
        Ok(Arc::clone(&self.empty))
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
        Ok(Arc::clone(&self.empty))
    }

    fn comparative(
        &self,
        _base: SnapshotId,
        _head: SnapshotId,
    ) -> RuleResult<Arc<ComparativeQueryDb>> {
        Err(RuleError::UnsupportedPrimitive {
            backend: "fake",
            primitive: "comparative",
            reason: "single snapshot",
        })
    }

    fn structural_neighbors(
        &self,
        probe: NodeId,
        similarity_floor: f32,
        max_results: usize,
    ) -> RuleResult<Vec<RuleStructuralNeighbor>> {
        Ok(self
            .neighbours
            .get(&probe)
            .map(|hits| {
                hits.iter()
                    .copied()
                    .filter(|hit| hit.jaccard >= similarity_floor)
                    .take(max_results)
                    .collect()
            })
            .unwrap_or_default())
    }
}

fn neighbours() -> Vec<RuleStructuralNeighbor> {
    vec![
        // The self-match is always dropped.
        RuleStructuralNeighbor {
            node: SEED,
            shape_hash_exact: true,
            jaccard: 1.0,
        },
        RuleStructuralNeighbor {
            node: EXACT,
            shape_hash_exact: true,
            jaccard: 0.8,
        },
        RuleStructuralNeighbor {
            node: NEAR,
            shape_hash_exact: false,
            jaccard: 0.9,
        },
    ]
}

#[test]
fn similar_kind_runs_end_to_end_and_scales_scores() {
    let backend = NeighbourBackend::new(SEED, neighbours());
    let plan = RulePlan::new(RuleNode::SimilarTo {
        seed: RuleEndpoint::Nodes(vec![SEED]),
        scope: None,
        similarity_kind: RuleSimilarityKind::Similar,
    });

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("SimilarTo executes through the public engine");

    let RuleOutput::SimilarityMatches(matches) = run.output else {
        panic!("expected SimilarityMatches, got {:?}", run.output);
    };
    // Self-match dropped; both neighbours kept (jaccard >= 0.7 floor), exact first.
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].matched, EXACT);
    assert_eq!(matches[0].score, 8_000);
    assert_eq!(matches[1].matched, NEAR);
    assert_eq!(matches[1].score, 9_000);
    assert!(matches.iter().all(|m| m.seed == SEED));
}

#[test]
fn duplicate_kind_keeps_only_exact_shape_matches() {
    let backend = NeighbourBackend::new(SEED, neighbours());
    let plan = RulePlan::new(RuleNode::SimilarTo {
        seed: RuleEndpoint::Nodes(vec![SEED]),
        scope: None,
        similarity_kind: RuleSimilarityKind::Duplicate,
    });

    let run = RuleEngine::new()
        .run(&backend, &plan)
        .expect("duplicate runs");

    let RuleOutput::SimilarityMatches(matches) = run.output else {
        panic!("expected SimilarityMatches, got {:?}", run.output);
    };
    // Only EXACT survives (NEAR is not shape_hash_exact); score pinned at 10000.
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].matched, EXACT);
    assert_eq!(matches[0].score, 10_000);
}

#[test]
fn scope_restricts_matches_to_allow_set() {
    let backend = NeighbourBackend::new(SEED, neighbours());
    let plan = RulePlan::new(RuleNode::SimilarTo {
        seed: RuleEndpoint::Nodes(vec![SEED]),
        scope: Some(RuleEndpoint::Nodes(vec![NEAR])),
        similarity_kind: RuleSimilarityKind::Similar,
    });

    let run = RuleEngine::new().run(&backend, &plan).expect("scoped runs");

    let RuleOutput::SimilarityMatches(matches) = run.output else {
        panic!("expected SimilarityMatches, got {:?}", run.output);
    };
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].matched, NEAR);
}

#[test]
fn empty_neighbour_set_is_ok_not_unsupported() {
    let backend = NeighbourBackend::new(SEED, Vec::new());
    let plan = RulePlan::new(RuleNode::SimilarTo {
        seed: RuleEndpoint::Nodes(vec![SEED]),
        scope: None,
        similarity_kind: RuleSimilarityKind::Similar,
    });

    let run = RuleEngine::new().run(&backend, &plan).expect("empty is ok");
    assert_eq!(run.output, RuleOutput::SimilarityMatches(Vec::new()));
}

#[test]
fn gate_flags_cross_snapshot_but_not_similar_to() {
    let similar = RuleNode::SimilarTo {
        seed: RuleEndpoint::Nodes(vec![SEED]),
        scope: None,
        similarity_kind: RuleSimilarityKind::Similar,
    };
    let diff = RuleNode::CrossSnapshotDiff {
        base: SnapshotId {
            edge_revision: 1,
            metadata_revision: 1,
        },
        head: SnapshotId {
            edge_revision: 2,
            metadata_revision: 2,
        },
        include_unchanged: false,
    };

    assert!(
        !requires_unsupported_beside_cache(&similar),
        "SimilarTo is engine-executable since L2a"
    );
    assert!(
        requires_unsupported_beside_cache(&diff),
        "CrossSnapshotDiff still lacks a snapshot-sourcing coordinator"
    );
}
