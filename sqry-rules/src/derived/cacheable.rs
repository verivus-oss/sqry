//! Cacheable `DerivedQuery` adapters for single-snapshot rule primitives.

use serde::{Deserialize, Serialize};
use sqry_core::graph::unified::NodeKind;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_db::QueryDb;
use sqry_db::planner::Direction;
use sqry_db::query::DerivedQuery;

use crate::RuleError;
use crate::backend::SqryDbRuleBackend;
use crate::engine::{RuleEngine, RuleRun};
use crate::ir::{
    ComplexityMetric, EntrypointExtension, PathKind, RelationEdgeKind, RuleCycleBounds,
    RuleEdgeClass, RuleEndpoint, RuleNode, RulePlan,
};

const PATH_QUERY_TYPE_ID: u32 = 0x1000;
const SUBGRAPH_QUERY_TYPE_ID: u32 = 0x1001;
const RELATION_EDGES_QUERY_TYPE_ID: u32 = 0x1002;
const CYCLE_WITNESS_QUERY_TYPE_ID: u32 = 0x1003;
const REFERENCES_AT_QUERY_TYPE_ID: u32 = 0x1004;
const COMPLEXITY_QUERY_TYPE_ID: u32 = 0x1005;
const ENTRY_POINT_UNION_QUERY_TYPE_ID: u32 = 0x1006;

/// Cacheable FR5 rule variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheableRuleVariant {
    /// `PathQuery`.
    PathQuery,
    /// `SubgraphExtract`.
    SubgraphExtract,
    /// `RelationEdges`.
    RelationEdges,
    /// `CycleWitness`.
    CycleWitness,
    /// `ReferencesAt`.
    ReferencesAt,
    /// `ComplexityAggregate`.
    ComplexityAggregate,
    /// `EntryPointUnion`.
    EntryPointUnion,
}

/// Static registration and invalidation metadata for one cacheable rule query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheableRuleQuery {
    /// Cacheable rule variant.
    pub variant: CacheableRuleVariant,
    /// Stable `DerivedQuery::QUERY_TYPE_ID`.
    pub query_type_id: u32,
    /// `DerivedQuery::TRACKS_EDGE_REVISION`.
    pub tracks_edge_revision: bool,
    /// `DerivedQuery::TRACKS_METADATA_REVISION`.
    pub tracks_metadata_revision: bool,
    /// `DerivedQuery::PERSISTENT`.
    pub persistent: bool,
}

/// Serializable query result for cacheable rule execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleQueryOutcome {
    /// Rule executed successfully.
    Ok(RuleRun),
    /// Rule execution returned a typed failure.
    Err(RuleQueryFailure),
}

impl RuleQueryOutcome {
    fn from_result(result: Result<RuleRun, RuleError>) -> Self {
        match result {
            Ok(run) => Self::Ok(run),
            Err(error) => Self::Err(RuleQueryFailure::from_error(&error)),
        }
    }
}

/// Serializable rule-query execution failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleQueryFailure {
    /// Stable error class.
    pub kind: RuleQueryFailureKind,
    /// User-facing error message.
    pub message: String,
}

impl RuleQueryFailure {
    fn from_error(error: &RuleError) -> Self {
        let kind = match error {
            RuleError::NotInitialized { .. } => RuleQueryFailureKind::NotInitialized,
            RuleError::UnsupportedPrimitive { .. } => RuleQueryFailureKind::UnsupportedPrimitive,
            RuleError::InvalidRuleSource { .. } => RuleQueryFailureKind::InvalidRuleSource,
            RuleError::ExecutionCancelled => RuleQueryFailureKind::ExecutionCancelled,
            RuleError::Analysis(_) => RuleQueryFailureKind::Analysis,
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}

/// Stable serializable error class for cacheable rule-query failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleQueryFailureKind {
    /// Requested component is not initialized.
    NotInitialized,
    /// Backend does not support the requested primitive.
    UnsupportedPrimitive,
    /// Rule source is invalid.
    InvalidRuleSource,
    /// Caller cancelled execution.
    ExecutionCancelled,
    /// Downstream analysis failed.
    Analysis,
}

/// Path-query derived key.
///
/// This cacheable adapter does NOT support the L1 Primitive B `PathQuery.avoid`
/// guard-avoiding filter: the key has no `avoid` field and the reconstruction
/// below hardcodes `avoid: None`. Nothing converts an avoid-carrying `PathQuery`
/// into this key (the `missing_guard` / `trust_boundary` detectors run through
/// the full `RulePlan` via `RuleEngine`, not this derived path), so there is no
/// silent drop; guard-avoiding path queries are simply not cacheable through
/// this surface.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathRuleQueryKey {
    /// Source endpoint.
    pub from: RuleEndpoint,
    /// Target endpoint.
    pub to: RuleEndpoint,
    /// Path family.
    pub kind: PathKind,
    /// Maximum path depth.
    pub max_depth: u32,
    /// Optional maximum path count.
    pub max_paths: Option<u32>,
}

/// Subgraph derived key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubgraphRuleQueryKey {
    /// Seed endpoint.
    pub seeds: RuleEndpoint,
    /// Included edge classes.
    pub edge_classes: Vec<RuleEdgeClass>,
    /// Traversal direction.
    pub direction: Direction,
    /// Maximum traversal depth.
    pub max_depth: u32,
}

/// Relation-edge derived key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelationEdgesRuleQueryKey {
    /// Source endpoint.
    pub from: RuleEndpoint,
    /// Relation family.
    pub kind: RelationEdgeKind,
    /// Whether edge metadata is materialized.
    pub with_metadata: bool,
}

/// Cycle-witness derived key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CycleWitnessRuleQueryKey {
    /// Edge class to analyze.
    pub edge_class: RuleEdgeClass,
    /// Cycle bounds.
    pub bounds: RuleCycleBounds,
}

/// References-at derived key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferencesAtRuleQueryKey {
    /// Target endpoint.
    pub target: RuleEndpoint,
}

/// Complexity aggregate derived key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComplexityRuleQueryKey {
    /// Optional node kind filter.
    pub node_kind_filter: Option<NodeKind>,
    /// Metric to compute.
    pub metric: ComplexityMetric,
}

/// Entry-point union derived key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryPointUnionRuleQueryKey {
    /// Extension classifiers.
    pub extensions: Vec<EntrypointExtension>,
}

/// Derived query for `PathQuery`.
pub struct PathRuleQuery;
/// Derived query for `SubgraphExtract`.
pub struct SubgraphRuleQuery;
/// Derived query for `RelationEdges`.
pub struct RelationEdgesRuleQuery;
/// Derived query for `CycleWitness`.
pub struct CycleWitnessRuleQuery;
/// Derived query for `ReferencesAt`.
pub struct ReferencesAtRuleQuery;
/// Derived query for `ComplexityAggregate`.
pub struct ComplexityRuleQuery;
/// Derived query for `EntryPointUnion`.
pub struct EntryPointUnionRuleQuery;

macro_rules! impl_rule_query {
    ($query:ident, $key:ty, $id:expr, $tracks_metadata:expr, $plan:expr) => {
        impl DerivedQuery for $query {
            type Key = $key;
            type Value = RuleQueryOutcome;
            const QUERY_TYPE_ID: u32 = $id;
            const TRACKS_EDGE_REVISION: bool = true;
            const TRACKS_METADATA_REVISION: bool = $tracks_metadata;

            fn execute(key: &Self::Key, db: &QueryDb, _snapshot: &GraphSnapshot) -> Self::Value {
                let plan = $plan(key);
                execute_rule_plan(db, &plan)
            }
        }
    };
}

impl_rule_query!(
    PathRuleQuery,
    PathRuleQueryKey,
    PATH_QUERY_TYPE_ID,
    false,
    |key: &PathRuleQueryKey| RulePlan::new(RuleNode::PathQuery {
        from: key.from.clone(),
        to: key.to.clone(),
        kind: key.kind,
        max_depth: key.max_depth,
        max_paths: key.max_paths,
        avoid: None,
    })
);

impl_rule_query!(
    SubgraphRuleQuery,
    SubgraphRuleQueryKey,
    SUBGRAPH_QUERY_TYPE_ID,
    false,
    |key: &SubgraphRuleQueryKey| RulePlan::new(RuleNode::SubgraphExtract {
        seeds: key.seeds.clone(),
        edge_classes: key.edge_classes.clone(),
        direction: key.direction,
        max_depth: key.max_depth,
    })
);

impl_rule_query!(
    RelationEdgesRuleQuery,
    RelationEdgesRuleQueryKey,
    RELATION_EDGES_QUERY_TYPE_ID,
    false,
    |key: &RelationEdgesRuleQueryKey| RulePlan::new(RuleNode::RelationEdges {
        from: key.from.clone(),
        kind: key.kind,
        with_metadata: key.with_metadata,
    })
);

impl_rule_query!(
    CycleWitnessRuleQuery,
    CycleWitnessRuleQueryKey,
    CYCLE_WITNESS_QUERY_TYPE_ID,
    false,
    |key: &CycleWitnessRuleQueryKey| RulePlan::new(RuleNode::CycleWitness {
        edge_class: key.edge_class,
        bounds: key.bounds,
    })
);

impl_rule_query!(
    ReferencesAtRuleQuery,
    ReferencesAtRuleQueryKey,
    REFERENCES_AT_QUERY_TYPE_ID,
    false,
    |key: &ReferencesAtRuleQueryKey| RulePlan::new(RuleNode::ReferencesAt {
        target: key.target.clone(),
    })
);

impl_rule_query!(
    ComplexityRuleQuery,
    ComplexityRuleQueryKey,
    COMPLEXITY_QUERY_TYPE_ID,
    false,
    |key: &ComplexityRuleQueryKey| RulePlan::new(RuleNode::ComplexityAggregate {
        node_kind_filter: key.node_kind_filter,
        metric: key.metric,
    })
);

impl_rule_query!(
    EntryPointUnionRuleQuery,
    EntryPointUnionRuleQueryKey,
    ENTRY_POINT_UNION_QUERY_TYPE_ID,
    true,
    |key: &EntryPointUnionRuleQueryKey| RulePlan::new(RuleNode::EntryPointUnion {
        extensions: key.extensions.clone(),
    })
);

/// Registers all cacheable rule queries with the supplied `QueryDb`.
pub fn register_rule_queries(db: &mut QueryDb) {
    db.register::<PathRuleQuery>();
    db.register::<SubgraphRuleQuery>();
    db.register::<RelationEdgesRuleQuery>();
    db.register::<CycleWitnessRuleQuery>();
    db.register::<ReferencesAtRuleQuery>();
    db.register::<ComplexityRuleQuery>();
    db.register::<EntryPointUnionRuleQuery>();
}

/// Returns static FR5 metadata for all cacheable rule queries.
#[must_use]
pub const fn cacheable_rule_query_specs() -> [CacheableRuleQuery; 7] {
    [
        CacheableRuleQuery {
            variant: CacheableRuleVariant::PathQuery,
            query_type_id: PathRuleQuery::QUERY_TYPE_ID,
            tracks_edge_revision: PathRuleQuery::TRACKS_EDGE_REVISION,
            tracks_metadata_revision: PathRuleQuery::TRACKS_METADATA_REVISION,
            persistent: PathRuleQuery::PERSISTENT,
        },
        CacheableRuleQuery {
            variant: CacheableRuleVariant::SubgraphExtract,
            query_type_id: SubgraphRuleQuery::QUERY_TYPE_ID,
            tracks_edge_revision: SubgraphRuleQuery::TRACKS_EDGE_REVISION,
            tracks_metadata_revision: SubgraphRuleQuery::TRACKS_METADATA_REVISION,
            persistent: SubgraphRuleQuery::PERSISTENT,
        },
        CacheableRuleQuery {
            variant: CacheableRuleVariant::RelationEdges,
            query_type_id: RelationEdgesRuleQuery::QUERY_TYPE_ID,
            tracks_edge_revision: RelationEdgesRuleQuery::TRACKS_EDGE_REVISION,
            tracks_metadata_revision: RelationEdgesRuleQuery::TRACKS_METADATA_REVISION,
            persistent: RelationEdgesRuleQuery::PERSISTENT,
        },
        CacheableRuleQuery {
            variant: CacheableRuleVariant::CycleWitness,
            query_type_id: CycleWitnessRuleQuery::QUERY_TYPE_ID,
            tracks_edge_revision: CycleWitnessRuleQuery::TRACKS_EDGE_REVISION,
            tracks_metadata_revision: CycleWitnessRuleQuery::TRACKS_METADATA_REVISION,
            persistent: CycleWitnessRuleQuery::PERSISTENT,
        },
        CacheableRuleQuery {
            variant: CacheableRuleVariant::ReferencesAt,
            query_type_id: ReferencesAtRuleQuery::QUERY_TYPE_ID,
            tracks_edge_revision: ReferencesAtRuleQuery::TRACKS_EDGE_REVISION,
            tracks_metadata_revision: ReferencesAtRuleQuery::TRACKS_METADATA_REVISION,
            persistent: ReferencesAtRuleQuery::PERSISTENT,
        },
        CacheableRuleQuery {
            variant: CacheableRuleVariant::ComplexityAggregate,
            query_type_id: ComplexityRuleQuery::QUERY_TYPE_ID,
            tracks_edge_revision: ComplexityRuleQuery::TRACKS_EDGE_REVISION,
            tracks_metadata_revision: ComplexityRuleQuery::TRACKS_METADATA_REVISION,
            persistent: ComplexityRuleQuery::PERSISTENT,
        },
        CacheableRuleQuery {
            variant: CacheableRuleVariant::EntryPointUnion,
            query_type_id: EntryPointUnionRuleQuery::QUERY_TYPE_ID,
            tracks_edge_revision: EntryPointUnionRuleQuery::TRACKS_EDGE_REVISION,
            tracks_metadata_revision: EntryPointUnionRuleQuery::TRACKS_METADATA_REVISION,
            persistent: EntryPointUnionRuleQuery::PERSISTENT,
        },
    ]
}

fn execute_rule_plan(db: &QueryDb, plan: &RulePlan) -> RuleQueryOutcome {
    let backend = SqryDbRuleBackend::new(db);
    RuleQueryOutcome::from_result(RuleEngine::new().run(&backend, plan))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derived::{BesideCachePrimitive, beside_cache_route_for};
    use crate::engine::RuleOutput;
    use crate::ir::{RuleEndpoint, RuleSimilarityKind};
    use sqry_core::graph::unified::CodeGraph;
    use sqry_core::graph::unified::NodeId;
    use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
    use sqry_core::graph::unified::file::FileId;
    use sqry_core::graph::unified::storage::arena::NodeEntry;
    use sqry_db::QueryDbConfig;
    use sqry_db::planner::StringPattern;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::Arc;

    fn two_node_call_fixture() -> (Arc<GraphSnapshot>, FileId, NodeId, NodeId) {
        let mut graph = CodeGraph::new();
        let file = graph
            .files_mut()
            .register(Path::new("src/lib.rs"))
            .expect("register fixture file");
        let main_name = graph.strings_mut().intern("main").expect("intern main");
        let helper_name = graph.strings_mut().intern("helper").expect("intern helper");
        let main = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, main_name, file).with_qualified_name(main_name),
            )
            .expect("allocate main");
        let helper = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, helper_name, file)
                    .with_qualified_name(helper_name),
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
        (Arc::new(graph.snapshot()), file, main, helper)
    }

    #[test]
    fn cacheable_rule_queries_have_reserved_unique_type_ids() {
        let specs = cacheable_rule_query_specs();
        let ids: BTreeSet<u32> = specs.iter().map(|spec| spec.query_type_id).collect();

        assert_eq!(ids.len(), specs.len());
        assert_eq!(ids.first().copied(), Some(0x1000));
        assert_eq!(ids.last().copied(), Some(0x1006));
        assert!(ids.iter().all(|id| (0x1000..=0xFFFF).contains(id)));
    }

    #[test]
    fn cacheable_rule_queries_match_fr5_tier_mapping() {
        let specs = cacheable_rule_query_specs();

        assert_eq!(specs.len(), 7);
        assert!(specs.iter().all(|spec| spec.tracks_edge_revision));
        assert!(specs.iter().all(|spec| spec.persistent));
        assert_eq!(
            specs
                .iter()
                .filter(|spec| spec.tracks_metadata_revision)
                .map(|spec| spec.variant)
                .collect::<Vec<_>>(),
            vec![CacheableRuleVariant::EntryPointUnion]
        );
    }

    #[test]
    fn register_rule_queries_enables_query_db_execution() {
        let snapshot = Arc::new(CodeGraph::new().snapshot());
        let mut db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        register_rule_queries(&mut db);

        let outcome = db.get::<ComplexityRuleQuery>(&ComplexityRuleQueryKey {
            node_kind_filter: None,
            metric: ComplexityMetric::NodeCount,
        });

        assert!(matches!(outcome, RuleQueryOutcome::Ok(_)));
    }

    #[test]
    fn nested_relation_rule_query_deps_invalidate_on_file_revision_bump() {
        let (snapshot, file, _main, helper) = two_node_call_fixture();
        let mut db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        register_rule_queries(&mut db);

        let key = RelationEdgesRuleQueryKey {
            from: RuleEndpoint::Query(Box::new(RuleNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern: Some(StringPattern::exact("main")),
            })),
            kind: RelationEdgeKind::Callees,
            with_metadata: false,
        };

        let first = db.get::<RelationEdgesRuleQuery>(&key);
        assert!(matches!(
            first,
            RuleQueryOutcome::Ok(RuleRun {
                output: RuleOutput::Relations(ref rows),
                ..
            }) if rows.nodes == vec![helper]
        ));

        let after_first = db.metrics();
        let _ = db.get::<RelationEdgesRuleQuery>(&key);
        let after_warm = db.metrics();
        assert_eq!(after_warm.cache_hits, after_first.cache_hits + 1);
        assert_eq!(after_warm.cache_misses, after_first.cache_misses);

        db.inputs_mut()
            .get_mut(file)
            .expect("fixture file input")
            .update(Default::default());

        let _ = db.get::<RelationEdgesRuleQuery>(&key);
        let after_bump = db.metrics();
        assert!(
            after_bump.cache_misses > after_warm.cache_misses,
            "file revision bump must invalidate the outer rule query and may also invalidate nested relation entries"
        );
    }

    #[test]
    fn direct_traversal_rule_query_deps_invalidate_on_file_revision_bump() {
        let (snapshot, file, main, helper) = two_node_call_fixture();
        let mut db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        register_rule_queries(&mut db);

        let key = SubgraphRuleQueryKey {
            seeds: RuleEndpoint::Nodes(vec![main]),
            edge_classes: vec![RuleEdgeClass::Call],
            direction: Direction::Forward,
            max_depth: 1,
        };

        let first = db.get::<SubgraphRuleQuery>(&key);
        assert!(matches!(
            first,
            RuleQueryOutcome::Ok(RuleRun {
                output: RuleOutput::Subgraph { ref nodes, .. },
                ..
            }) if nodes == &vec![main, helper]
        ));

        let after_first = db.metrics();
        let _ = db.get::<SubgraphRuleQuery>(&key);
        let after_warm = db.metrics();
        assert_eq!(after_warm.cache_hits, after_first.cache_hits + 1);
        assert_eq!(after_warm.cache_misses, after_first.cache_misses);

        db.inputs_mut()
            .get_mut(file)
            .expect("fixture file input")
            .update(Default::default());

        let _ = db.get::<SubgraphRuleQuery>(&key);
        let after_bump = db.metrics();
        assert_eq!(after_bump.cache_misses, after_warm.cache_misses + 1);
    }

    #[test]
    fn beside_cache_variants_are_absent_from_cacheable_specs() {
        let variants: BTreeSet<_> = cacheable_rule_query_specs()
            .iter()
            .map(|spec| spec.variant)
            .collect();
        assert_eq!(variants.len(), 7);

        let diff = RuleNode::CrossSnapshotDiff {
            base: crate::backend::SnapshotId {
                edge_revision: 1,
                metadata_revision: 1,
            },
            head: crate::backend::SnapshotId {
                edge_revision: 2,
                metadata_revision: 2,
            },
            include_unchanged: false,
        };
        let similar = RuleNode::SimilarTo {
            seed: RuleEndpoint::Nodes(vec![NodeId::new(1, 1)]),
            scope: None,
            similarity_kind: RuleSimilarityKind::Similar,
        };

        assert_eq!(
            beside_cache_route_for(&diff).map(|route| route.primitive),
            Some(BesideCachePrimitive::ComparativeDiff)
        );
        assert_eq!(
            beside_cache_route_for(&similar).map(|route| route.primitive),
            Some(BesideCachePrimitive::FindSimilar)
        );
    }

    #[test]
    fn invalid_cacheable_rule_execution_serializes_failure() {
        let snapshot = Arc::new(CodeGraph::new().snapshot());
        let mut db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        register_rule_queries(&mut db);

        let outcome = db.get::<CycleWitnessRuleQuery>(&CycleWitnessRuleQueryKey {
            edge_class: RuleEdgeClass::Reference,
            bounds: RuleCycleBounds::default(),
        });

        assert!(matches!(
            outcome,
            RuleQueryOutcome::Err(RuleQueryFailure {
                kind: RuleQueryFailureKind::UnsupportedPrimitive,
                ..
            })
        ));
    }
}
