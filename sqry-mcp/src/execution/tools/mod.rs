//! Tool execution handlers for MCP.
//!
//! This module contains the individual tool handlers extracted from
//! the main execution module for better organization.

mod analysis;
mod context_propagation;
mod explain;
mod graph;
mod index;
mod introspection;
mod navigation;
mod planner_query;
mod relations;
mod rules;
mod search;
mod trace;

pub use analysis::{
    execute_cross_language_edges,
    execute_dependency_impact,
    execute_direct_callees,
    // New graph-based tools
    execute_direct_callers,
    execute_find_cycles,
    execute_find_duplicates,
    execute_find_unused,
    execute_is_node_in_cycle,
    execute_pattern_search,
    execute_semantic_diff,
};
pub use context_propagation::execute_context_propagation;
pub use explain::execute_explain_code;
pub use graph::{execute_export_graph, execute_get_dependencies, execute_subgraph};
pub use index::{execute_index_status, execute_rebuild_index};
pub use introspection::{
    execute_complexity_metrics, execute_expand_cache_status, execute_get_graph_stats,
    execute_get_insights, execute_list_files, execute_list_symbols,
};
pub use navigation::{
    execute_get_definition, execute_get_document_symbols, execute_get_hover_info,
    execute_get_references, execute_get_workspace_symbols,
};
pub use planner_query::execute_sqry_query;
pub use relations::{execute_call_hierarchy, execute_relation_query};
pub use rules::execute_rules_run;
pub use search::{execute_find_similar, execute_semantic_search, execute_structural_similar};
pub use trace::execute_trace_path;

// Phase 8b Task 3: inner:: submodules used by daemon_adapter for
// cross-transport dispatch. The `_for_daemon` wrappers land in Task 4.
#[allow(unused_imports)]
pub(crate) use analysis::inner as analysis_inner;
#[allow(unused_imports)]
pub(crate) use graph::inner as graph_inner;
#[allow(unused_imports)]
pub(crate) use introspection::inner as introspection_inner;
#[allow(unused_imports)]
pub(crate) use relations::inner as relations_inner;
#[allow(unused_imports)]
pub(crate) use search::inner as search_inner;
#[allow(unused_imports)]
pub(crate) use trace::inner as trace_inner;
