//! sqry-mcp library re-exports for fuzzing and testing.
//!
//! This module exposes internal components for fuzz testing. The MCP server
//! itself is the main binary; this library allows external test crates to
//! access validation and pagination functionality.
//!
//! Note: Many internal modules are only used by the binary (main.rs) and appear
//! unused from the library's perspective. The `dead_code` lint is suppressed
//! for these modules.

// These modules are used by the binary (main.rs) but not the library exports.
// Suppress dead_code warnings for modules that are only used by the binary.
// Engine module is public to allow integration tests.

// Phase 8c U12 — daemon shim-mode CLI helpers.  Exposed from the lib target
// so integration tests can call `sqry_mcp::daemon_shim::resolve_daemon_socket`
// and `sqry_mcp::daemon_shim::run_daemon_shim` directly without spawning a
// subprocess.
pub mod daemon_shim;

// Re-export key public symbols at the crate root for ergonomic access from
// integration tests (`use sqry_mcp::daemon_shim::parse_daemon_args;`).
// The `daemon_shim` module itself is already pub above; these re-exports
// are a convenience for callers that prefer flat imports.
pub use daemon_shim::ProbeOutcome;
pub use daemon_shim::parse_daemon_args;
pub use daemon_shim::probe_and_run_daemon_shim;
pub use daemon_shim::resolve_daemon_socket;

#[allow(dead_code)]
pub mod daemon_adapter;
#[allow(dead_code)]
pub mod daemon_params;
#[allow(dead_code)]
pub mod engine;
#[allow(dead_code)]
pub mod error;
#[allow(dead_code)]
pub mod execution;
#[allow(dead_code)]
mod feature_flags;
pub mod mcp_config;
/// Output truncation cap module — enforces `SQRY_MCP_MAX_OUTPUT_BYTES`
/// (default 50 000) at the `success_result` boundary in `server.rs`.
pub mod output_caps;
mod pagination;
#[allow(dead_code)]
mod path_resolver;
#[allow(dead_code)]
mod prompts;
#[allow(dead_code)]
mod resources;
#[allow(dead_code)]
mod response;
#[allow(dead_code)]
mod server;
#[allow(dead_code)]
mod tools;
#[allow(dead_code)]
pub mod tools_schema;
mod workspace_session;

pub use pagination::{decode_cursor, encode_cursor};

// Re-export mcp_config for testing
pub use mcp_config::McpConfig;

/// Re-export tool validation functions for fuzzing.
#[cfg(any(test, fuzzing))]
pub mod tool_validation {
    pub use crate::tools::{
        validate_cross_language_edges_args, validate_dependency_impact_args,
        validate_explain_code_args, validate_export_graph_args, validate_get_index_status_args,
        validate_relation_query_args, validate_search_similar_args, validate_semantic_diff_args,
        validate_semantic_search_args, validate_show_dependencies_args, validate_subgraph_args,
        validate_trace_path_args,
    };
}

/// Re-export the argument types used by the relation / caller / callee
/// / analysis / trace / graph MCP handlers. Integration tests in `tests/`
/// exercise these directly so they stay locked to the planner-canonical
/// semantics established by DB14 + DB15 + DB16 + DB17.
pub mod tool_args {
    pub use crate::tools::{
        ChangeType, ComplexityMetricsArgs, CycleType, DependencyImpactArgs, DirectCalleesArgs,
        DirectCallersArgs, ExportGraphArgs, FindCyclesArgs, FindUnusedArgs, GitVersionRef,
        IsNodeInCycleArgs, PaginationArgs, RelationQueryArgs, RelationType, SearchFilters,
        SemanticDiffArgs, SemanticDiffFilters, SemanticSearchArgs, ShowDependenciesArgs,
        SqryAskParams, SqryQueryParams, SubgraphArgs, TracePathArgs, UnusedScope,
        WorkspaceStatusArgs,
    };
}

/// Re-export the relation-family + analysis-family + trace-family execute
/// functions for the DB15 / DB16 / DB17 golden integration tests in
/// `tests/`. Public surface is intentionally narrow — only the handlers
/// migrated in DB15 / DB16 / DB17 plus the shared `ToolExecution`
/// envelope.
pub mod tool_handlers {
    pub use crate::execution::{
        execute_complexity_metrics, execute_dependency_impact, execute_direct_callees,
        execute_direct_callers, execute_export_graph, execute_find_cycles, execute_find_unused,
        execute_get_dependencies, execute_is_node_in_cycle, execute_relation_query,
        execute_semantic_diff, execute_sqry_ask, execute_sqry_query, execute_subgraph,
        execute_trace_path,
    };
    pub use crate::tools::execute_workspace_status;
}

// `STEP_7` MAJOR 2 test surface — re-exports of the workspace_status
// tool entry points so integration tests can drive the per-request
// thread-local LogicalWorkspace override path end-to-end. Mirrors what
// the rmcp tool dispatch wires at runtime (resolved_workspace ->
// with_workspace_override -> tool execution).
//
// Folded into `tool_args` and `tool_handlers` to avoid a name
// collision with the private `mod tools;` declaration above. We do not
// re-export under `pub mod tools` because that would shadow the
// private module path used everywhere else in the crate.

/// `STEP_7` MAJOR 2 test surface — public access to the per-request
/// thread-local workspace override so integration tests can bind a
/// `LogicalWorkspace` before invoking workspace-aware tools (mirroring
/// the runtime dispatch in `SqryServer::execute_tool_for_request`).
pub mod workspace_session_test_api {
    pub use crate::workspace_session::{current_logical_workspace, with_workspace_override};
}

/// Test-only setup helpers. Integration tests must initialize the
/// discovery / engine / graph caches before invoking workspace-resolved
/// tool handlers (the binary does this in `main`); this re-export keeps
/// the initialization surface narrow.
pub mod test_setup {
    pub use crate::engine::init_engine_cache;
    pub use crate::execution::{init_subgraph_cache, init_trace_path_cache};
    pub use crate::path_resolver::init_discovery_cache;
}

/// Iter2-B C029c — test-helper surface gated behind `feature = "test-helpers"`.
///
/// Exposes `SqryServer::new_for_tests` and
/// `SqryServer::build_success_result_for_tests` so integration tests in
/// `tests/output_caps_truncation_via_dispatch.rs` can drive the actual
/// `success_result` boundary that all `#[tool]`-decorated handlers route
/// through. This is strictly additive — the default build (the binary
/// shipped to users) does not enable this feature and therefore has no
/// new public surface.
#[cfg(feature = "test-helpers")]
pub mod server_test_helpers {
    pub use crate::server::SqryServer;
    pub use rmcp::model::CallToolResult;
}
