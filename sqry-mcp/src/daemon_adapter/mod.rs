//! Daemon adapter for in-memory MCP tool dispatch.
//!
//! The sqryd daemon holds each workspace's `Arc<CodeGraph>` and a shared
//! `Arc<QueryExecutor>` in long-lived memory. When a daemon IPC client
//! calls an MCP-style tool, dispatch goes through `execute_*_for_daemon`
//! which consume a [`WorkspaceContext`] and forward to the same
//! `inner::execute_*` functions used by the `SqryServer` HTTP/stdio path —
//! guaranteeing identical behaviour between transports.
//!
//! Task 4 (Phase 8b) populates this module with:
//!   - 14 `execute_*_for_daemon` wrappers that accept `(ctx, args)` and
//!     forward to `crate::execution::tools::*_inner::execute_*`.
//!   - [`tool_response_json`], the daemon-path renderer that delegates to
//!     the shared [`crate::response::build_tool_response`] with
//!     `include_version=false` (the daemon transport supplies its own
//!     protocol version in `ResponseEnvelope::meta`).
//!
//! All wrappers return `anyhow::Result<ToolExecution<T>>` — the IPC layer
//! in sqry-daemon is responsible for mapping errors to the daemon
//! wire-error envelope. `tool_response_json` returns `rmcp::ErrorData`
//! because that is what the shared response builder yields; the daemon
//! maps that at its own boundary.

// The `_for_daemon` wrappers and `tool_response_json` are `pub` on the
// sqry-mcp library so sqry-daemon can consume them in Task 7. Inside the
// `sqry-mcp` binary itself (`main.rs`) nothing calls them yet, so the
// `bin` compilation would flag them as dead code. Dead-code analysis runs
// per compilation root, so `lib.rs` and `main.rs` each wrap the module
// declaration in `#[allow(dead_code)]` independently.

/// Name-keyed dispatch helper used by the Phase 8c daemon MCP host
/// (U8) so it does not duplicate Phase 8b's 14-arm tool-name match.
/// See [`dispatch::dispatch_by_name`].
pub mod dispatch;

/// Per-request thread-local workspace-override binder (issue #469 U04).
///
/// The sqryd daemon binds the reconstructed [`sqry_core::workspace::LogicalWorkspace`]
/// on its blocking tool thread through this seam so
/// `canonicalize_in_workspace_enforced` and the `workspace_scope`
/// entrypoints see the same per-request exclusions in daemon-hosted mode
/// that the standalone `SqryServer` binds. The reading side
/// (`current_logical_workspace`) stays crate-internal to the enforced
/// canonicalization entry, so the daemon only needs the write seam.
pub use crate::workspace_session::with_workspace_override;

/// On-disk `LogicalWorkspace` reconstruction seam (issue #469 U04).
///
/// Reads `<workspace_root>/.sqry-workspace` when present (carrying the
/// exclusion set), otherwise falls back to a single-root workspace. The
/// daemon re-runs this discovery per read-only tool dispatch to
/// reconstruct the workspace it holds only by root, keeping its
/// exclusion resolution byte-identical to the standalone server's.
pub use crate::workspace_session::resolve_logical_workspace_for_root;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rmcp::ErrorData as McpError;
use serde::Serialize;

use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::query::executor::QueryExecutor;

use crate::engine::canonicalize_in_workspace_enforced;
use crate::execution::ToolExecution;
use crate::execution::types::{
    ComplexityMetricsData, DependencyGraphData, DependencyImpactData, DirectCalleesData,
    DirectCallersData, FindCyclesData, FindUnusedData, NodeInCycleData, RelationQueryData,
    SemanticDiffData, SemanticSearchData, StructuralSimilarData, TracePathData,
};
use crate::execution::{
    analysis_inner, graph_inner, introspection_inner, relations_inner, search_inner, trace_inner,
};
use crate::tools::{
    ComplexityMetricsArgs, DependencyImpactArgs, DirectCalleesArgs, DirectCallersArgs,
    ExportGraphArgs, FindCyclesArgs, FindUnusedArgs, IsNodeInCycleArgs, RelationQueryArgs,
    SemanticDiffArgs, SemanticSearchArgs, ShowDependenciesArgs, StructuralSimilarArgs,
    SubgraphArgs, TracePathArgs,
};

/// Caller-supplied, pre-resolved workspace state consumed by every
/// `inner::execute_*` function.
///
/// The daemon supplies `graph` and `executor` directly from its per-
/// workspace memory; the rmcp `SqryServer` path builds a `WorkspaceContext`
/// ad-hoc by cloning the `Arc`s out of the request-scoped `Engine`.
///
/// `Clone` is cheap (three `Arc` clones + a `PathBuf` clone) and lets a
/// dispatcher hand identical contexts to concurrent tool evaluations.
#[derive(Clone)]
pub struct WorkspaceContext {
    pub workspace_root: PathBuf,
    pub graph: Arc<CodeGraph>,
    pub executor: Arc<QueryExecutor>,
}

impl std::fmt::Debug for WorkspaceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceContext")
            .field("workspace_root", &self.workspace_root)
            .field("graph", &"Arc<CodeGraph>")
            .field("executor", &"Arc<QueryExecutor>")
            .finish()
    }
}

// ---------------------------------------------------------------------
// 14 daemon-path tool wrappers.
//
// Every wrapper forwards to the matching `inner::execute_*` function used
// by the rmcp `SqryServer` path. The daemon supplies the
// [`WorkspaceContext`] directly from its per-workspace state; the SqryServer
// path constructs one ad-hoc from an `Engine`. Both transports end up in
// the same inner body, so behaviour (and therefore test surface) is
// identical.
//
// Timing: each wrapper samples `Instant::now()` itself before calling the
// inner fn, EXCEPT `relation_query`, whose inner body preserves its legacy
// anchor taken immediately before the snapshot acquisition (see
// `relations_inner::execute_relation_query`). The rmcp path also samples
// the instant immediately before the inner call (see `execute_semantic_search`
// etc. in the per-tool modules), so the two transports measure the same
// execution window.
// ---------------------------------------------------------------------

/// Daemon-path wrapper for `semantic_search`.
///
/// Internally canonicalizes `args.path` against `ctx.workspace_root`
/// to produce the `search_root` the inner body requires, matching the
/// workspace-boundary enforcement performed by the rmcp entrypoint.
///
/// # Errors
///
/// Returns an error if the requested path escapes the workspace or if the
/// delegated semantic-search executor fails.
pub fn execute_semantic_search_for_daemon(
    ctx: &WorkspaceContext,
    args: &SemanticSearchArgs,
    cancel: &sqry_core::query::cancellation::CancellationToken,
) -> Result<ToolExecution<SemanticSearchData>> {
    let search_root = canonicalize_in_workspace_enforced(&args.path, &ctx.workspace_root)?;
    let start = Instant::now();
    search_inner::execute_semantic_search(ctx, args, &search_root, start, cancel)
}

/// Daemon-path wrapper for `relation_query`.
///
/// The inner body takes its own `Instant::now()` (legacy timing anchor);
/// this wrapper forwards `(ctx, args)` verbatim.
///
/// # Errors
///
/// Returns an error if relation-query validation or graph execution fails.
pub fn execute_relation_query_for_daemon(
    ctx: &WorkspaceContext,
    args: &RelationQueryArgs,
) -> Result<ToolExecution<RelationQueryData>> {
    relations_inner::execute_relation_query(ctx, args)
}

/// Daemon-path wrapper for `structural_similar` (body-shape descriptor, U07).
///
/// # Errors
///
/// Returns an error if a supplied `file_path` escapes the workspace or the probe
/// symbol has no body-shape descriptor.
pub fn execute_structural_similar_for_daemon(
    ctx: &WorkspaceContext,
    args: &StructuralSimilarArgs,
) -> Result<ToolExecution<StructuralSimilarData>> {
    let start = Instant::now();
    search_inner::execute_structural_similar(ctx, args, start)
}

/// Daemon-path wrapper for `direct_callers`.
///
/// # Errors
///
/// Returns an error if direct-caller analysis cannot be executed for `args`.
pub fn execute_direct_callers_for_daemon(
    ctx: &WorkspaceContext,
    args: &DirectCallersArgs,
) -> Result<ToolExecution<DirectCallersData>> {
    let start = Instant::now();
    analysis_inner::execute_direct_callers(ctx, args, start)
}

/// Daemon-path wrapper for `direct_callees`.
///
/// # Errors
///
/// Returns an error if direct-callee analysis cannot be executed for `args`.
pub fn execute_direct_callees_for_daemon(
    ctx: &WorkspaceContext,
    args: &DirectCalleesArgs,
) -> Result<ToolExecution<DirectCalleesData>> {
    let start = Instant::now();
    analysis_inner::execute_direct_callees(ctx, args, start)
}

/// Daemon-path wrapper for `find_unused`.
///
/// # Errors
///
/// Returns an error if unused-symbol analysis cannot be executed for `args`.
pub fn execute_find_unused_for_daemon(
    ctx: &WorkspaceContext,
    args: &FindUnusedArgs,
) -> Result<ToolExecution<FindUnusedData>> {
    let start = Instant::now();
    analysis_inner::execute_find_unused(ctx, args, start)
}

/// Daemon-path wrapper for `find_cycles`.
///
/// # Errors
///
/// Returns an error if cycle analysis cannot be executed for `args`.
#[must_use]
pub fn execute_find_cycles_for_daemon(
    ctx: &WorkspaceContext,
    args: &FindCyclesArgs,
) -> ToolExecution<FindCyclesData> {
    let start = Instant::now();
    analysis_inner::execute_find_cycles(ctx, args, start)
}

/// Daemon-path wrapper for `is_node_in_cycle`.
///
/// # Errors
///
/// Returns an error if cycle-membership analysis cannot be executed for `args`.
pub fn execute_is_node_in_cycle_for_daemon(
    ctx: &WorkspaceContext,
    args: &IsNodeInCycleArgs,
) -> Result<ToolExecution<NodeInCycleData>> {
    let start = Instant::now();
    analysis_inner::execute_is_node_in_cycle(ctx, args, start)
}

/// Daemon-path wrapper for `trace_path`.
///
/// # Errors
///
/// Returns an error if path tracing cannot be executed for `args`.
pub fn execute_trace_path_for_daemon(
    ctx: &WorkspaceContext,
    args: &TracePathArgs,
) -> Result<ToolExecution<TracePathData>> {
    let start = Instant::now();
    trace_inner::execute_trace_path(ctx, args, start)
}

/// Daemon-path wrapper for `subgraph`.
///
/// # Errors
///
/// Returns an error if subgraph extraction cannot be executed for `args`.
pub fn execute_subgraph_for_daemon(
    ctx: &WorkspaceContext,
    args: &SubgraphArgs,
) -> Result<ToolExecution<DependencyGraphData>> {
    let start = Instant::now();
    graph_inner::execute_subgraph(ctx, args, start)
}

/// Daemon-path wrapper for `export_graph`.
///
/// # Errors
///
/// Returns an error if graph export cannot be executed for `args`.
pub fn execute_export_graph_for_daemon(
    ctx: &WorkspaceContext,
    args: &ExportGraphArgs,
) -> Result<ToolExecution<DependencyGraphData>> {
    let start = Instant::now();
    graph_inner::execute_export_graph(ctx, args, start)
}

/// Daemon-path wrapper for `complexity_metrics`.
///
/// # Errors
///
/// Returns an error if the requested subtree `path` is excluded by the logical
/// workspace policy (issue #469 Surface 2).
pub fn execute_complexity_metrics_for_daemon(
    ctx: &WorkspaceContext,
    args: &ComplexityMetricsArgs,
) -> Result<ToolExecution<ComplexityMetricsData>> {
    let start = Instant::now();
    introspection_inner::execute_complexity_metrics(ctx, args, start)
}

/// Daemon-path wrapper for `semantic_diff`.
///
/// `ctx.graph` is unused by the inner body (`semantic_diff` builds its
/// own per-git-ref graphs from worktrees) but the context is still
/// supplied for daemon-path symmetry.
///
/// # Errors
///
/// Returns an error if either git reference cannot be materialized or diffed.
pub fn execute_semantic_diff_for_daemon(
    ctx: &WorkspaceContext,
    args: &SemanticDiffArgs,
) -> Result<ToolExecution<SemanticDiffData>> {
    let start = Instant::now();
    analysis_inner::execute_semantic_diff(ctx, args, start)
}

/// Daemon-path wrapper for `dependency_impact`.
///
/// # Errors
///
/// Returns an error if dependency-impact analysis cannot be executed for `args`.
pub fn execute_dependency_impact_for_daemon(
    ctx: &WorkspaceContext,
    args: &DependencyImpactArgs,
) -> Result<ToolExecution<DependencyImpactData>> {
    let start = Instant::now();
    analysis_inner::execute_dependency_impact(ctx, args, start)
}

/// Daemon-path wrapper for the wire method `show_dependencies`.
///
/// The underlying inner fn is historically named
/// [`graph_inner::execute_get_dependencies`]; only the wire-facing name
/// changed. The wrapper is named after the wire method so the daemon's
/// `tool_dispatch` (Task 7) can look it up by the JSON-RPC method string.
///
/// # Errors
///
/// Returns an error if dependency extraction cannot be executed for `args`.
pub fn execute_show_dependencies_for_daemon(
    ctx: &WorkspaceContext,
    args: &ShowDependenciesArgs,
) -> Result<ToolExecution<DependencyGraphData>> {
    let start = Instant::now();
    graph_inner::execute_get_dependencies(ctx, args, start)
}

// ---------------------------------------------------------------------
// Shared response renderer for the daemon transport.
// ---------------------------------------------------------------------

/// Render a `ToolExecution<T>` to the daemon wire JSON format.
///
/// Calls the shared [`crate::response::build_tool_response`] with
/// `include_version=false` — the daemon's JSON-RPC envelope carries its
/// own protocol version in `ResponseEnvelope::meta`, so the inner
/// response object omits the MCP `"version"` key that `SqryServer`
/// emits.
///
/// The return type uses `rmcp::ErrorData` (aliased as `McpError`)
/// because that is what the shared builder produces. The daemon IPC
/// layer is responsible for mapping this error into its own wire-error
/// envelope.
///
/// # Errors
///
/// Returns an MCP error if the tool response cannot be serialized into the
/// shared response envelope.
pub fn tool_response_json<T: Serialize>(
    exec: ToolExecution<T>,
) -> Result<serde_json::Value, McpError> {
    crate::response::build_tool_response(exec, false)
}
