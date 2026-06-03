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

use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::materialize::find_nodes_by_name;
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
/// behaviour through different code paths.
///
/// `Returns` walks `EdgeKind::TypeOf { context: Some(TypeOfContext::Return), .. }`
/// edges from the candidate function/method nodes resolved out of `symbol`
/// and returns the target type-node IDs. This mirrors the planner's
/// `Predicate::Returns` evaluator at
/// [`sqry_db::planner::execute`]'s `node_returns_type` for cross-engine
/// consistency: both surfaces consult the unified-graph edge plane (the
/// `TypeOf { Return }` edges produced by `B2_PLANNER`'s `GraphBuildHelper`
/// integration) rather than parsing `NodeEntry.signature` text.
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
        RelationType::Returns => returns_targets_for_symbol(db, symbol),
        // `Wraps` is dispatched directly inside the relation_query
        // handler (collect_wraps_relation in relations.rs) — it walks
        // outbound `EdgeKind::Wraps` from resolved start nodes
        // rather than going through a name-keyed `DerivedQuery`. This
        // matches the dispatch taxonomy (CLAUDE.md §"sqry-db crate"
        // §"Dispatch taxonomy"): name-resolution + NodeId-anchored
        // edge walk, no derived-query backing yet.
        RelationType::Wraps => Arc::new(Vec::new()),
        // `ChannelPeers` / `Instantiations` (T2.4 / T2.5) are dispatched
        // directly inside the relation_query handler
        // (collect_channel_peer_relation / collect_instantiates_relation in
        // relations.rs) — they body-expand the resolved start nodes and walk
        // `EdgeKind::ChannelPeer` / `EdgeKind::Instantiates` edges, rather
        // than going through a name-keyed `DerivedQuery`. Same dispatch
        // taxonomy as `Wraps`: name-resolution + NodeId-anchored edge walk.
        RelationType::ChannelPeers | RelationType::Instantiations => Arc::new(Vec::new()),
    }
}

/// Resolve `symbol` to candidate function/method nodes and collect the
/// target `NodeId`s of every outgoing
/// `EdgeKind::TypeOf { context: Some(TypeOfContext::Return), .. }` edge.
///
/// # Inline snapshot walk vs. derived-query cache
///
/// We walk `snapshot.edges().edges_from(...)` directly rather than backing
/// this with a sqry-db [`DerivedQuery`][sqry_db::query::DerivedQuery]. The
/// planner's `Predicate::Returns` evaluator at
/// `sqry_db::planner::execute::node_returns_type` makes the same choice
/// for the same reason: `Returns` lacks a name-keyed cache entry in
/// sqry-db today, and adding one is out of scope for `B2_MCP` (the DAG's
/// `critical_decisions` only require shape parity with
/// Callers/Callees/Imports/Exports for client-side consistency, not cache
/// reuse). A future unit can introduce a `ReturnsQuery` derived-query
/// type and route both this function and the planner through it; at that
/// point this body becomes a one-liner like `db.get::<ReturnsQuery>(key)`
/// in the same shape as [`mcp_callers_query`]. Until then the inline walk
/// keeps the MCP `relation_query relation:returns` surface honest about
/// the underlying edge plane.
///
/// Deduplication preserves first-seen order across candidate nodes so the
/// surface is deterministic across runs.
#[must_use]
fn returns_targets_for_symbol(db: &QueryDb, symbol: &str) -> Arc<Vec<NodeId>> {
    let snapshot = db.snapshot();
    let candidates = find_nodes_by_name(snapshot, symbol);

    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<NodeId> = Vec::new();

    for candidate in candidates {
        for edge in snapshot.edges().edges_from(candidate) {
            if matches!(
                edge.kind,
                EdgeKind::TypeOf {
                    context: Some(TypeOfContext::Return),
                    ..
                }
            ) && seen.insert(edge.target)
            {
                out.push(edge.target);
            }
        }
    }

    Arc::new(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::Path;
    use std::sync::Arc;

    use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
    use sqry_core::graph::unified::edge::EdgeKind;
    #[cfg(test)]
    use sqry_core::graph::unified::edge::ResolvedVia;
    use sqry_core::graph::unified::edge::kind::TypeOfContext;
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
                resolved_via: ResolvedVia::Direct,
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

        // `build_caller_graph` has no `TypeOf{Return}` edges, so the Returns
        // dispatch over an unrelated symbol must still come back empty —
        // confirming that we did not regress to a "match anything" walk.
        let returns_set = relation_endpoints_for_mcp(&db, RelationType::Returns, "anything");
        assert!(
            returns_set.is_empty(),
            "Returns dispatch must yield empty when no TypeOf{{Return}} edges exist"
        );
    }

    /// Builds: `parseConfig --TypeOf{Return}--> error_type`. Returns
    /// `(snapshot, parse_config_id, error_type_id)`.
    fn build_returns_graph() -> (Arc<GraphSnapshot>, NodeId, NodeId) {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("lib.rs")).unwrap();
        let parse_name = graph.strings_mut().intern("parseConfig").unwrap();
        let error_name = graph.strings_mut().intern("error_type").unwrap();

        let parse_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, parse_name, file_id)
                    .with_qualified_name(parse_name),
            )
            .unwrap();
        let error_id = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Type, error_name, file_id).with_qualified_name(error_name),
            )
            .unwrap();

        graph.edges_mut().add_edge(
            parse_id,
            error_id,
            EdgeKind::TypeOf {
                context: Some(TypeOfContext::Return),
                index: None,
                name: None,
            },
            file_id,
        );

        // Rebuild auxiliary indices so `find_nodes_by_name` can resolve
        // the symbol; the production build pipeline does this after every
        // chunk in `phase4c_rebuild_indices`.
        graph.rebuild_indices();

        (Arc::new(graph.snapshot()), parse_id, error_id)
    }

    #[test]
    fn mcp_dispatch_returns_walks_typeof_return_edges() {
        let (snapshot, _parse_id, error_id) = build_returns_graph();
        let db = make_query_db(snapshot);

        let returns_set = relation_endpoints_for_mcp(&db, RelationType::Returns, "parseConfig");
        assert!(
            returns_set.contains(&error_id),
            "Returns dispatch must surface the target of the TypeOf{{Return}} edge \
             (got {returns_set:?}, expected to contain {error_id:?})",
        );
    }

    #[test]
    fn mcp_dispatch_returns_empty_for_unknown_symbol() {
        let (snapshot, _parse_id, _error_id) = build_returns_graph();
        let db = make_query_db(snapshot);

        let returns_set =
            relation_endpoints_for_mcp(&db, RelationType::Returns, "no_such_function");
        assert!(
            returns_set.is_empty(),
            "Unknown symbols must dispatch to an empty Returns set, not panic or return all targets"
        );
    }
}
