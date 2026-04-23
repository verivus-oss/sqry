//! `callees:X` derived query.
//!
//! Under the planner's relation convention (shared with the DB12 inline
//! `relation_matches` path), `callees:X` filters a node set to those nodes
//! whose **outgoing** `Calls` edges target an endpoint whose name matches
//! `X`. The returned set consists of callers of an `X`-named symbol.
//!
//! The real computation lives in
//! [`super::relation::compute_relation_source_set`].

use std::sync::Arc;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::id::NodeId;

use crate::QueryDb;
use crate::query::DerivedQuery;

use super::relation::{RelationKey, RelationKind, compute_relation_source_set};

/// `callees:X` — filter to nodes where `X` is one of the callees.
///
/// # Invalidation
///
/// `TRACKS_EDGE_REVISION = true`: any change in the global `Calls`
/// topology can introduce or remove callees of a given name.
pub struct CalleesQuery;

impl DerivedQuery for CalleesQuery {
    type Key = RelationKey;
    type Value = Arc<Vec<NodeId>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::CALLEES;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(key: &RelationKey, _db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<Vec<NodeId>> {
        compute_relation_source_set(RelationKind::Callees, key, snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueryDbConfig;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::edge::kind::EdgeKind;
    use sqry_core::graph::unified::node::kind::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;
    use std::path::Path;
    use std::sync::Arc;

    #[test]
    fn callees_query_returns_callers_of_named_target() {
        // main --Calls--> helper, main --Calls--> other, isolated_fn has no
        // edges. `callees:helper` under the planner convention = {main}.
        let mut graph = CodeGraph::new();
        let file = graph.files_mut().register(Path::new("lib.rs")).unwrap();
        let main_name = graph.strings_mut().intern("main").unwrap();
        let helper_name = graph.strings_mut().intern("helper").unwrap();
        let other_name = graph.strings_mut().intern("other").unwrap();
        let isolated_name = graph.strings_mut().intern("isolated").unwrap();

        let main_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, main_name, file).with_qualified_name(main_name),
            )
            .unwrap();
        let helper_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, helper_name, file)
                    .with_qualified_name(helper_name),
            )
            .unwrap();
        let other_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, other_name, file)
                    .with_qualified_name(other_name),
            )
            .unwrap();
        let _ = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, isolated_name, file)
                    .with_qualified_name(isolated_name),
            )
            .unwrap();

        graph.edges_mut().add_edge(
            main_id,
            helper_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        graph.edges_mut().add_edge(
            main_id,
            other_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        let snapshot = Arc::new(graph.snapshot());
        let mut db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());
        db.register::<CalleesQuery>();

        let matches = db.get::<CalleesQuery>(&RelationKey::exact("helper"));
        assert_eq!(matches.as_ref(), &vec![main_id]);
    }
}
