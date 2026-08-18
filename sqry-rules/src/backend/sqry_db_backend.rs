//! `sqry-db` backed [`RuleBackend`](super::RuleBackend) implementation.

use std::collections::HashSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sqry_core::graph::unified::bind::BindingPlane;
use sqry_core::graph::unified::edge::kind::{FfiConvention, TypeOfContext};
use sqry_core::graph::unified::{
    DbQueryType, EdgeFilter, EdgeKind, ExportKind, GraphSnapshot, NodeId, ResolvedVia,
    SimplePathStrategy, TraversalConfig, TraversalDirection, TraversalLimits, TraversalResult,
    traverse,
};
use sqry_core::query::CircularType;
use sqry_db::dependency::record_file_dep;
use sqry_db::planner::{QueryPlan, execute_plan};
use sqry_db::queries::{
    CondensationQuery, CondensationValue, CycleBounds, CyclesKey, CyclesQuery, CyclesValue,
    EntryPointsQuery, IsInCycleKey, IsInCycleQuery, ReachabilityKey, ReachabilityQuery,
    ReachableFromEntryPointsQuery, ReachableSet, RelationKey, SccQuery, SccValue, UnusedKey,
    UnusedQuery, UnusedValue, mcp_callees_query, mcp_callers_query, mcp_exports_query,
    mcp_imports_query, mcp_references_query,
};
use sqry_db::{ComparativeQueryDb, QueryDb};

use super::{
    RuleBackend, RulePath, RuleReachabilityKey, RuleStructuralNeighbor, RuleTopologyKey,
    SnapshotId, TracePathKey,
};
use crate::ir::{RelationEdgeKind, RuleEdgeClass};
use crate::{RuleError, RuleResult};

/// Cycle edge families exposed at the rule boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleClass {
    /// Call graph cycles.
    Calls,
    /// Import graph cycles.
    Imports,
    /// Module import graph cycles.
    Modules,
}

impl From<CycleClass> for CircularType {
    fn from(value: CycleClass) -> Self {
        match value {
            CycleClass::Calls => Self::Calls,
            CycleClass::Imports => Self::Imports,
            CycleClass::Modules => Self::Modules,
        }
    }
}

/// Default backend that delegates to `sqry-db` and the public graph kernel.
pub struct SqryDbRuleBackend<'db> {
    db: &'db QueryDb,
}

impl<'db> SqryDbRuleBackend<'db> {
    /// Creates a backend over an existing `QueryDb`.
    #[must_use]
    pub const fn new(db: &'db QueryDb) -> Self {
        Self { db }
    }

    /// Returns the underlying database.
    #[must_use]
    pub const fn db(&self) -> &'db QueryDb {
        self.db
    }

    fn snapshot(&self) -> &GraphSnapshot {
        self.db.snapshot()
    }

    fn record_traversal_deps(&self, result: &TraversalResult) {
        for node in &result.nodes {
            self.record_node_dep(node.node_id);
        }
    }

    fn record_node_dep(&self, node: NodeId) {
        if let Some(entry) = self.snapshot().nodes().get(node) {
            record_file_dep(entry.file);
        }
    }

    fn relation_from_node_direct(&self, node: NodeId, kind: RelationEdgeKind) -> Arc<Vec<NodeId>> {
        self.record_node_dep(node);
        let mut related = match kind {
            RelationEdgeKind::Callers => self
                .snapshot()
                .edges()
                .edges_to(node)
                .into_iter()
                .filter(|edge| matches!(edge.kind, EdgeKind::Calls { .. }))
                .map(|edge| {
                    record_file_dep(edge.file);
                    self.record_node_dep(edge.source);
                    edge.source
                })
                .collect::<Vec<_>>(),
            RelationEdgeKind::Callees => self
                .snapshot()
                .edges()
                .edges_from(node)
                .into_iter()
                .filter(|edge| matches!(edge.kind, EdgeKind::Calls { .. }))
                .map(|edge| {
                    record_file_dep(edge.file);
                    self.record_node_dep(edge.target);
                    edge.target
                })
                .collect::<Vec<_>>(),
            RelationEdgeKind::Imports => self
                .snapshot()
                .edges()
                .edges_from(node)
                .into_iter()
                .filter(|edge| matches!(edge.kind, EdgeKind::Imports { .. }))
                .map(|edge| {
                    record_file_dep(edge.file);
                    self.record_node_dep(edge.target);
                    edge.target
                })
                .collect::<Vec<_>>(),
            RelationEdgeKind::Exports => self
                .snapshot()
                .edges()
                .edges_from(node)
                .into_iter()
                .chain(self.snapshot().edges().edges_to(node))
                .filter(|edge| matches!(edge.kind, EdgeKind::Exports { .. }))
                .filter_map(|edge| {
                    record_file_dep(edge.file);
                    let related = if edge.source == node {
                        edge.target
                    } else if edge.target == node {
                        edge.source
                    } else {
                        return None;
                    };
                    if related == node {
                        return None;
                    }
                    self.record_node_dep(related);
                    Some(related)
                })
                .collect::<Vec<_>>(),
            RelationEdgeKind::References => self
                .snapshot()
                .edges()
                .edges_to(node)
                .into_iter()
                .filter(|edge| {
                    matches!(
                        edge.kind,
                        EdgeKind::Calls { .. }
                            | EdgeKind::References
                            | EdgeKind::Imports { .. }
                            | EdgeKind::FfiCall { .. }
                    )
                })
                .map(|edge| {
                    record_file_dep(edge.file);
                    self.record_node_dep(edge.source);
                    edge.source
                })
                .collect::<Vec<_>>(),
            RelationEdgeKind::Implements => self
                .snapshot()
                .edges()
                .edges_from(node)
                .into_iter()
                .filter(|edge| matches!(edge.kind, EdgeKind::Implements))
                .map(|edge| {
                    record_file_dep(edge.file);
                    self.record_node_dep(edge.target);
                    edge.target
                })
                .collect::<Vec<_>>(),
        };
        related.sort_unstable_by_key(|node| (node.index(), node.generation()));
        related.dedup();
        Arc::new(related)
    }
}

impl RuleBackend for SqryDbRuleBackend<'_> {
    fn snapshot_id(&self) -> SnapshotId {
        SnapshotId::from_query_db(self.db)
    }

    fn binding(&self) -> BindingPlane<'_> {
        self.snapshot().binding_plane()
    }

    fn traverse(
        &self,
        seeds: &[NodeId],
        direction: TraversalDirection,
        edge_filter: EdgeFilter,
        limits: TraversalLimits,
    ) -> RuleResult<TraversalResult> {
        let config = TraversalConfig {
            direction,
            edge_filter,
            limits,
        };
        let result = traverse(self.snapshot(), seeds, &config, None);
        self.record_traversal_deps(&result);
        Ok(result)
    }

    fn callers(&self, key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(mcp_callers_query(self.db, &key))
    }

    fn callees(&self, key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(mcp_callees_query(self.db, &key))
    }

    fn imports(&self, key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(mcp_imports_query(self.db, &key))
    }

    fn exports(&self, key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(mcp_exports_query(self.db, &key))
    }

    fn references(&self, key: RelationKey) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(mcp_references_query(self.db, &key))
    }

    fn relation_from_node(
        &self,
        node: NodeId,
        kind: RelationEdgeKind,
    ) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(self.relation_from_node_direct(node, kind))
    }

    fn cycles(&self, key: CyclesKey) -> RuleResult<CyclesValue> {
        Ok(self.db.get::<CyclesQuery>(&key))
    }

    fn is_in_cycle(&self, node: NodeId, cycle_class: CycleClass) -> RuleResult<bool> {
        let key = IsInCycleKey {
            node_id: node,
            circular_type: cycle_class.into(),
            bounds: CycleBounds::default(),
        };
        Ok(self.db.get::<IsInCycleQuery>(&key))
    }

    fn unused(&self, key: UnusedKey) -> RuleResult<UnusedValue> {
        Ok(self.db.get::<UnusedQuery>(&key))
    }

    fn entry_points(&self) -> RuleResult<Arc<HashSet<NodeId>>> {
        Ok(self.db.get::<EntryPointsQuery>(&()))
    }

    fn reachable_from_entry_points(&self) -> RuleResult<Arc<HashSet<NodeId>>> {
        Ok(self.db.get::<ReachableFromEntryPointsQuery>(&()))
    }

    fn reachability(&self, key: RuleReachabilityKey) -> RuleResult<Arc<ReachableSet>> {
        let db_key = ReachabilityKey {
            roots: key.roots,
            edge_kind: edge_class_to_edge_kind_probe(key.edge_class),
        };
        Ok(self.db.get::<ReachabilityQuery>(&db_key))
    }

    fn scc(&self, key: RuleTopologyKey) -> RuleResult<SccValue> {
        let edge_kind = edge_class_to_edge_kind_probe(key.edge_class);
        Ok(self.db.get::<SccQuery>(&edge_kind))
    }

    fn condensation(&self, key: RuleTopologyKey) -> RuleResult<CondensationValue> {
        let edge_kind = edge_class_to_edge_kind_probe(key.edge_class);
        Ok(self.db.get::<CondensationQuery>(&edge_kind))
    }

    fn trace_path(&self, key: TracePathKey) -> RuleResult<Arc<Vec<RulePath>>> {
        let min_confidence = key.min_confidence();
        let config = TraversalConfig {
            direction: key.direction,
            edge_filter: key.edge_filter,
            limits: key.limits,
        };
        let mut strategy =
            SimplePathStrategy::new(key.target, min_confidence, key.allow_cross_language);
        let result = traverse(self.snapshot(), &key.sources, &config, Some(&mut strategy));
        self.record_traversal_deps(&result);
        let paths = result
            .paths
            .unwrap_or_default()
            .into_iter()
            .map(|path| {
                let nodes = path
                    .into_iter()
                    .filter_map(|idx| result.nodes.get(idx).map(|node| node.node_id))
                    .collect();
                RulePath::new(nodes)
            })
            .collect();
        Ok(Arc::new(paths))
    }

    fn run_plan(&self, plan: &QueryPlan) -> RuleResult<Arc<Vec<NodeId>>> {
        Ok(Arc::new(execute_plan(plan, self.db)))
    }

    fn comparative(
        &self,
        base: SnapshotId,
        head: SnapshotId,
    ) -> RuleResult<Arc<ComparativeQueryDb>> {
        let current = self.snapshot_id();
        if base == current && head == current {
            let snapshot = self.db.snapshot_arc();
            return Ok(Arc::new(ComparativeQueryDb::new(
                Arc::clone(&snapshot),
                snapshot,
            )));
        }

        Err(RuleError::UnsupportedPrimitive {
            backend: "sqry-db",
            primitive: "comparative",
            reason: "the default backend only owns the current single-snapshot QueryDb; cross-snapshot lookup is provided by a higher-level coordinator",
        })
    }

    fn structural_neighbors(
        &self,
        probe: NodeId,
        similarity_floor: f32,
        max_results: usize,
    ) -> RuleResult<Vec<RuleStructuralNeighbor>> {
        let snapshot = self.db.snapshot();
        let hits = sqry_db::queries::structural_neighbors(
            self.db,
            snapshot,
            probe,
            similarity_floor,
            max_results,
        );
        Ok(hits
            .into_iter()
            .map(|neighbour| RuleStructuralNeighbor {
                node: neighbour.node,
                shape_hash_exact: neighbour.shape_hash_exact,
                jaccard: neighbour.jaccard,
            })
            .collect())
    }
}

/// Maps an edge filter to representative edge discriminants.
///
/// This is the only production `sqry-rules` chokepoint that names
/// `EdgeKind` directly. Rule source and trait signatures stay on
/// `EdgeFilter` / edge-class vocabulary.
#[must_use]
pub fn edge_filter_to_edge_kind_probes(filter: &EdgeFilter) -> Vec<EdgeKind> {
    let mut probes = Vec::new();
    if filter.include_calls {
        // Both an intra-language and a cross-boundary representative of the
        // `Call` classification, so the cross_boundary discriminator can select
        // the correct one in the `retain` below: a plain `Calls` edge is not
        // cross-boundary, while `FfiCall` is.
        probes.push(EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        });
        probes.push(EdgeKind::FfiCall {
            convention: FfiConvention::C,
        });
    }
    if filter.include_imports {
        probes.push(EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        });
        probes.push(EdgeKind::Exports {
            kind: ExportKind::Direct,
            alias: None,
        });
    }
    if filter.include_references {
        probes.push(EdgeKind::References);
    }
    if filter.include_inheritance {
        probes.push(EdgeKind::Inherits);
        probes.push(EdgeKind::Implements);
    }
    if filter.include_structural {
        probes.push(EdgeKind::Contains);
        probes.push(EdgeKind::Defines);
    }
    if filter.include_type_edges {
        probes.push(EdgeKind::TypeOf {
            context: Some(TypeOfContext::Variable),
            index: None,
            name: None,
        });
    }
    if filter.include_database {
        probes.push(EdgeKind::DbQuery {
            query_type: DbQueryType::Select,
            table: None,
        });
    }
    if filter.include_service {
        probes.push(EdgeKind::WebAssemblyCall);
    }
    // Every emitted probe must be consistent with the whole filter, including
    // the cross_boundary discriminator (P2). Without this, a `Some(true)` filter
    // could emit a plain `Calls` probe (not cross-boundary) and a `Some(false)`
    // filter could emit a `DbQuery` probe (cross-boundary), each violating the
    // filter contract. `cross_boundary: None` keeps every classification-selected
    // probe, preserving prior behavior exactly.
    probes.retain(|kind| filter.accepts_kind(kind));
    probes
}

fn edge_class_to_edge_kind_probe(edge_class: RuleEdgeClass) -> EdgeKind {
    match edge_class {
        RuleEdgeClass::Call => EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        },
        RuleEdgeClass::Import => EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        },
        RuleEdgeClass::Reference => EdgeKind::References,
        RuleEdgeClass::Inheritance => EdgeKind::Inherits,
        RuleEdgeClass::Structural => EdgeKind::Contains,
        RuleEdgeClass::Type => EdgeKind::TypeOf {
            context: Some(TypeOfContext::Variable),
            index: None,
            name: None,
        },
        RuleEdgeClass::Database => EdgeKind::DbQuery {
            query_type: DbQueryType::Select,
            table: None,
        },
        RuleEdgeClass::Service => EdgeKind::WebAssemblyCall,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_filter_mapping_covers_every_filter_group() {
        let probes = edge_filter_to_edge_kind_probes(&EdgeFilter::all());
        assert!(
            probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::Calls { .. }))
        );
        assert!(
            probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::Imports { .. }))
        );
        assert!(
            probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::Exports { .. }))
        );
        assert!(
            probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::References))
        );
        assert!(probes.iter().any(|kind| matches!(kind, EdgeKind::Inherits)));
        assert!(
            probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::Implements))
        );
        assert!(probes.iter().any(|kind| matches!(kind, EdgeKind::Contains)));
        assert!(probes.iter().any(|kind| matches!(kind, EdgeKind::Defines)));
        assert!(
            probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::TypeOf { .. }))
        );
        assert!(
            probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::DbQuery { .. }))
        );
        assert!(
            probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::WebAssemblyCall))
        );
    }

    #[test]
    fn every_probe_is_consistent_with_its_filter() {
        // The probe set must never emit an edge kind the filter would reject,
        // for any cross_boundary polarity.
        for cross_boundary in [None, Some(true), Some(false)] {
            let filter = EdgeFilter {
                cross_boundary,
                ..EdgeFilter::all()
            };
            let probes = edge_filter_to_edge_kind_probes(&filter);
            assert!(
                probes.iter().all(|kind| filter.accepts_kind(kind)),
                "every emitted probe must pass accepts_kind for cross_boundary={cross_boundary:?}"
            );
        }
    }

    #[test]
    fn cross_boundary_true_emits_only_cross_boundary_probes() {
        let filter = EdgeFilter {
            cross_boundary: Some(true),
            ..EdgeFilter::all()
        };
        let probes = edge_filter_to_edge_kind_probes(&filter);
        assert!(!probes.is_empty(), "cross-boundary calls/db/service remain");
        assert!(
            probes.iter().all(EdgeKind::is_cross_boundary),
            "Some(true) must drop the plain Calls / Imports / intra probes"
        );
        // The cross-boundary call representative survives even though plain
        // `Calls` is dropped.
        assert!(
            probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::FfiCall { .. }))
        );
        assert!(
            !probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::Calls { .. }))
        );
    }

    #[test]
    fn cross_boundary_false_emits_only_intra_language_probes() {
        let filter = EdgeFilter {
            cross_boundary: Some(false),
            ..EdgeFilter::all()
        };
        let probes = edge_filter_to_edge_kind_probes(&filter);
        assert!(
            probes.iter().all(|kind| !kind.is_cross_boundary()),
            "Some(false) must drop FfiCall / DbQuery / WebAssemblyCall"
        );
        assert!(
            probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::Calls { .. }))
        );
        assert!(
            !probes
                .iter()
                .any(|kind| matches!(kind, EdgeKind::DbQuery { .. }))
        );
    }
}
