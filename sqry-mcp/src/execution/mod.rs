//! Execution module for MCP tool handlers.
//!
//! This module orchestrates tool execution and re-exports handlers from submodules.

pub(crate) mod git_worktree;
pub(crate) mod graph_builders;
pub(crate) mod graph_cache;
pub mod hierarchical;
pub(crate) mod location;
pub(crate) mod relation_dispatch;
pub(crate) mod symbol_utils;
mod tools;
pub(crate) mod types;
pub(crate) mod utils;
pub(crate) mod workspace_scope;

// Re-export cache initialization functions for server initialization (binary)
#[doc(hidden)]
#[allow(unused_imports)] // Used by binary (main.rs), not library
pub use graph_cache::{init_subgraph_cache, init_trace_path_cache};

pub use hierarchical::execute_hierarchical_search;
pub use tools::{
    execute_call_hierarchy,
    execute_complexity_metrics,
    execute_context_propagation,
    execute_cross_language_edges,
    execute_dependency_impact,
    execute_direct_callees,
    // New graph-based tools
    execute_direct_callers,
    execute_expand_cache_status,
    execute_explain_code,
    execute_export_graph,
    execute_find_cycles,
    execute_find_duplicates,
    execute_find_similar,
    execute_find_unused,
    // Navigation tools
    execute_get_definition,
    execute_get_dependencies,
    execute_get_document_symbols,
    // Introspection tools
    execute_get_graph_stats,
    execute_get_hover_info,
    execute_get_insights,
    execute_get_references,
    execute_get_workspace_symbols,
    execute_index_status,
    execute_is_node_in_cycle,
    execute_list_files,
    execute_list_symbols,
    execute_pattern_search,
    // Index tools
    execute_rebuild_index,
    execute_relation_query,
    execute_rules_run,
    execute_semantic_diff,
    execute_semantic_search,
    execute_sqry_query,
    execute_structural_similar,
    execute_subgraph,
    execute_trace_path,
};

// Re-export types used inside the crate's own tests and hierarchical
// handler. The external (lib) public surface re-exports what integration
// tests need under `sqry_mcp::tool_handlers`; keeping this block narrow
// avoids dead re-exports flagged by `clippy -D warnings`.
#[allow(unused_imports)]
pub use types::{
    CodeContext, DiffSummary, NodeChange, NodeRefData, PositionData, RangeData, RebuildIndexData,
    StructuralNeighborData, StructuralSimilarData, ToolExecution,
};

// Phase 8b Task 4: surface the per-tool `*_inner` re-exports at
// `crate::execution::*` so the daemon adapter can reach the
// SqryServer-shared bodies without widening the private `tools` module.
pub(crate) use tools::{
    analysis_inner, graph_inner, introspection_inner, relations_inner, search_inner, trace_inner,
};
