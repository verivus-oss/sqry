//! Execution module for MCP tool handlers.
//!
//! This module orchestrates tool execution and re-exports handlers from submodules.

pub(crate) mod diff_comparator;
pub(crate) mod git_worktree;
pub(crate) mod graph_builders;
pub(crate) mod graph_cache;
pub mod hierarchical;
pub(crate) mod symbol_utils;
mod tools;
pub(crate) mod types;
pub(crate) mod utils;

// Re-export cache initialization functions for server initialization (binary)
#[doc(hidden)]
#[allow(unused_imports)] // Used by binary (main.rs), not library
pub use graph_cache::{init_subgraph_cache, init_trace_path_cache};

pub use hierarchical::execute_hierarchical_search;
pub use tools::{
    execute_call_hierarchy,
    execute_complexity_metrics,
    execute_cross_language_edges,
    execute_dependency_impact,
    execute_direct_callees,
    // New graph-based tools
    execute_direct_callers,
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
    execute_semantic_diff,
    execute_semantic_search,
    execute_sqry_ask,
    execute_subgraph,
    execute_trace_path,
};

// Re-export types for external use (public API)
pub use types::{
    CodeContext, DiffSummary, NlDisambiguationOption, NlTranslationData, NodeChange, NodeRefData,
    PositionData, RangeData, ToolExecution,
};
