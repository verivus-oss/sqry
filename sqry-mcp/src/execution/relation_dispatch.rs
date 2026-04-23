//! Bridge from MCP relation tools to `sqry-db` derived relation queries.
//!
//! # Why this module exists
//!
//! Prior to DB18 this module owned `make_query_db` and the graph_eval-style
//! inversion wrappers (`mcp_callers_query`, `mcp_callees_query`, etc.).
//! DB18 lifted those helpers to `sqry-db::queries::dispatch` so the CLI
//! (and any future transport) can share the same dispatch table. This
//! module now re-exports the sqry-db helpers to preserve existing MCP call
//! sites and adds the `RelationType`-keyed dispatch map, which is
//! MCP-specific (it depends on the MCP [`crate::tools::RelationType`]
//! enum) and so cannot move into sqry-db.
//!
//! See [`sqry_db::queries::dispatch`] module docs for the full rationale
//! behind the graph_eval-style inversion and the direction crib sheet.
//!
//! # Which MCP handlers route through this module?
//!
//! `direct_callers` and `direct_callees` use `mcp_callers_query` and
//! `mcp_callees_query` — these tools take a user-supplied symbol name as
//! the predicate value, which is exactly what sqry-db's name-keyed
//! relation queries are designed for.
//!
//! `relation_query` does NOT route through this module. The post-DB15
//! Codex review caught a multi-hop bug where a stripped-name dispatch
//! leaked unrelated same-named chains into the BFS frontier; the
//! structural fix was to enumerate Calls edges directly from the
//! `find_nodes_by_name`-resolved start nodes (a NodeId-anchored
//! operation, not a name-keyed predicate). See
//! `tools/relations.rs::collect_call_relation_via_db` for the rationale.

use std::sync::Arc;

use sqry_core::graph::unified::node::NodeId;
use sqry_db::QueryDb;
use sqry_db::queries::RelationKey;

// Re-export the shared dispatch helpers from sqry-db so existing MCP call
// sites (`crate::execution::relation_dispatch::mcp_callers_query`, etc.)
// keep compiling unchanged.
pub(crate) use sqry_db::queries::dispatch::{mcp_callees_query, mcp_callers_query};
#[allow(unused_imports)]
pub(crate) use sqry_db::queries::dispatch::{
    mcp_exports_query, mcp_imports_query, mcp_references_query,
};

// `make_query_db` (legacy, load-only construction — no cold rehydration) is
// retained for use inside this module's own unit tests; production MCP
// handlers now route through `sqry_db::queries::dispatch::make_query_db_cold`
// (Codex final-review finding closed on 2026-04-16).
#[cfg(test)]
use sqry_db::queries::dispatch::make_query_db;

use crate::tools::RelationType;

/// Route a MCP [`RelationType`] to the corresponding `sqry-db` query and
/// return the matching endpoint `NodeId`s.
///
/// Reserved for callers that want a single-call dispatch (handlers that
/// project a flat node-set rather than per-edge metadata). The current
/// MCP handlers in [`crate::execution::tools::relations`] and
/// [`crate::execution::tools::analysis`] call the per-relation wrappers
/// directly because they need to thread relation-specific keys and
/// behaviour through different code paths. `Returns` returns an empty
/// set here because it is not yet modelled as an edge in the unified
/// graph.
///
/// This function stays in `sqry-mcp` (rather than moving to sqry-db with
/// the per-query wrappers) because it depends on the MCP-local
/// [`RelationType`] enum; sqry-db does not (and should not) know about
/// MCP's transport-layer enums.
#[must_use]
#[allow(dead_code)]
pub(crate) fn relation_endpoints_for_mcp(
    db: &QueryDb,
    relation: RelationType,
    symbol: &str,
) -> Arc<Vec<NodeId>> {
    let key = RelationKey::exact(symbol);
    match relation {
        RelationType::Callers => mcp_callers_query(db, &key),
        RelationType::Callees => mcp_callees_query(db, &key),
        RelationType::Imports => mcp_imports_query(db, &key),
        RelationType::Exports => mcp_exports_query(db, &key),
        RelationType::Returns => Arc::new(Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::sync::Arc;

    use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::NodeEntry;

    /// Builds: `main --Calls--> helper`, `isolated` has no edges. Returns
    /// `(snapshot, main_id, helper_id, isolated_id)`.
    fn build_caller_graph() -> (Arc<GraphSnapshot>, NodeId, NodeId, NodeId) {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("lib.rs")).unwrap();
        let main_name = graph.strings_mut().intern("main").unwrap();
        let helper_name = graph.strings_mut().intern("helper").unwrap();
        let isolated_name = graph.strings_mut().intern("isolated").unwrap();

        let main_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, main_name, file_id)
                    .with_qualified_name(main_name),
            )
            .unwrap();
        let helper_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, helper_name, file_id)
                    .with_qualified_name(helper_name),
            )
            .unwrap();
        let isolated_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, isolated_name, file_id)
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
            file_id,
        );

        (Arc::new(graph.snapshot()), main_id, helper_id, isolated_id)
    }

    #[test]
    fn mcp_dispatch_routes_relation_type_to_correct_query() {
        let (snapshot, main_id, helper_id, _isolated_id) = build_caller_graph();
        let db = make_query_db(snapshot);

        let callers_set = relation_endpoints_for_mcp(&db, RelationType::Callers, "helper");
        assert!(callers_set.contains(&main_id));

        let callees_set = relation_endpoints_for_mcp(&db, RelationType::Callees, "main");
        assert!(callees_set.contains(&helper_id));

        let returns_set = relation_endpoints_for_mcp(&db, RelationType::Returns, "anything");
        assert!(returns_set.is_empty(), "Returns is not modelled as an edge");
    }
}
