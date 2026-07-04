//! Bridge from MCP relation tools to `sqry-db` derived relation queries.
//!
//! # Why this module exists
//!
//! Prior to DB18 this module owned `make_query_db` and the graph_eval-style
//! inversion wrappers (`mcp_callers_query`, `mcp_callees_query`, etc.).
//! DB18 lifted those helpers to `sqry-db::queries::dispatch` so the CLI
//! (and any future transport) can share the same dispatch table. This
//! module now re-exports the sqry-db helpers to preserve existing MCP call
//! sites.
//!
//! See [`sqry_db::queries::dispatch`] module docs for the full rationale
//! behind the graph_eval-style inversion and the direction crib sheet.
//!
//! # Which MCP handlers route through this module?
//!
//! `direct_callers` and `direct_callees` use `mcp_callers_query` and
//! `mcp_callees_query`: these tools take a user-supplied symbol name as
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

// Re-export the shared dispatch helpers from sqry-db so existing MCP call
// sites (`crate::execution::relation_dispatch::mcp_callers_query`, etc.)
// keep compiling unchanged.
pub(crate) use sqry_db::queries::dispatch::{mcp_callees_query, mcp_callers_query};
#[allow(unused_imports)]
pub(crate) use sqry_db::queries::dispatch::{
    mcp_exports_query, mcp_imports_query, mcp_references_query,
};
