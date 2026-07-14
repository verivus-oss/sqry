//! Name-keyed dispatch into the Phase 8b `execute_*_for_daemon`
//! wrappers, used by the Phase 8c MCP host (`sqry-daemon::mcp_host`,
//! U8) to avoid duplicating the 14-arm match that
//! `sqry-daemon::ipc::methods::tool_dispatch::dispatch_tool` already
//! has. Returns raw JSON `Value` (not wrapped in envelope) — the
//! caller builds the rmcp `CallToolResult` or sqryd
//! `ResponseEnvelope`.
//!
//! # SGA05 — acquisition boundary contract
//!
//! `dispatch_by_name` runs **inside** the `run` closure passed to
//! `sqry-daemon::ipc::tool_core::acquire_and_execute`, AFTER the
//! shared `DaemonGraphProvider` has acquired a graph through the
//! SGA02/SGA04 boundary. It does NOT itself touch
//! `WorkspaceManager::classify_for_serve` or load any graph — the
//! provider has already done that, including the bounded one-shot
//! reload on `WorkspaceEvicted`. Adding a new daemon-supported
//! read-only tool means (a) registering it here and in
//! `sqry-daemon/src/ipc/methods/tool_dispatch/mod.rs::dispatch_tool`,
//! and (b) ensuring its `*_for_daemon` body consumes the
//! pre-acquired graph from the [`WorkspaceContext`] argument
//! rather than reloading the workspace on its own.
//!
//! Single source of truth for name-keyed tool routing, shared by U8's
//! MCP host and future Task 10 `DaemonClient` tool paths. Every arm
//! mirrors the corresponding handler in
//! `sqry-daemon/src/ipc/methods/tool_dispatch/tools/*.rs` — keep the
//! two in sync.
//!
//! Argument conversion goes through [`crate::daemon_params`] rather
//! than `serde_json::from_value` directly: the internal `*Args`
//! structs in [`crate::tools::validation`] do not implement
//! `Deserialize` (they are post-validation types), whereas the
//! external `*Params` structs in [`crate::tools::params`] do. The
//! `params_to_*_args` converters lower the external schema into the
//! internal types with identical bounds checking to the rmcp
//! `SqryServer` path.

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::{
    WorkspaceContext, execute_complexity_metrics_for_daemon, execute_dependency_impact_for_daemon,
    execute_direct_callees_for_daemon, execute_direct_callers_for_daemon,
    execute_export_graph_for_daemon, execute_find_cycles_for_daemon,
    execute_find_unused_for_daemon, execute_generate_overview_for_daemon,
    execute_is_node_in_cycle_for_daemon, execute_relation_query_for_daemon,
    execute_semantic_diff_for_daemon, execute_semantic_search_for_daemon,
    execute_show_dependencies_for_daemon, execute_structural_similar_for_daemon,
    execute_subgraph_for_daemon, execute_trace_path_for_daemon, tool_response_json,
};

use crate::daemon_params::{
    params_to_complexity_metrics_args, params_to_dependency_impact_args,
    params_to_direct_callees_args, params_to_direct_callers_args, params_to_export_graph_args,
    params_to_find_cycles_args, params_to_find_unused_args, params_to_generate_overview_args,
    params_to_is_node_in_cycle_args, params_to_relation_query_args, params_to_semantic_diff_args,
    params_to_semantic_search_args, params_to_show_dependencies_args,
    params_to_structural_similar_args, params_to_subgraph_args, params_to_trace_path_args,
};

/// Dispatch a named tool call against a pre-resolved
/// [`WorkspaceContext`].
///
/// `name` must be one of
/// [`crate::tools_schema::DAEMON_SUPPORTED_TOOL_NAMES`]. `args_value`
/// is the raw tool arguments JSON (as received from rmcp
/// `CallToolRequestParam::arguments`).
///
/// Returns the rendered JSON payload via
/// [`super::tool_response_json`] — the caller wraps it in their
/// transport-specific envelope (rmcp `CallToolResult` or sqryd
/// `ResponseEnvelope`).
///
/// # Errors
///
/// - `anyhow::Error` wrapping [`crate::error::RpcError`] if args
///   deserialisation or bounds validation fails (the daemon IPC
///   boundary maps this to `DaemonError::InvalidArgument` / `-32602`).
/// - `anyhow::Error` propagated from the underlying
///   `execute_*_for_daemon` wrapper.
/// - `anyhow::Error::msg("dispatch_by_name: unknown tool name ...")`
///   if `name` is not in `DAEMON_SUPPORTED_TOOL_NAMES`.
pub fn dispatch_by_name(
    name: &str,
    wctx: &WorkspaceContext,
    args_value: &Value,
    cancel: &sqry_core::query::cancellation::CancellationToken,
) -> Result<Value> {
    // `A_cancellation.md` §A2 + §A3 — daemon-hosted MCP dispatch
    // accepts a per-request `&CancellationToken` so deadline-driven
    // cancellation flows through the same pass-boundary poll the
    // standalone path uses. The token is currently propagated only
    // through tools whose inner body routes through the executor's
    // `*_cancellable` overloads (semantic_search today; the rest of
    // the per-tool migration is tracked under IMP-A's deferred
    // follow-up — see the cluster-A test row 6 deferral marker).
    let _ = cancel; // suppresses unused-binding warnings on tool arms that
    // do not yet propagate cancellation.
    match name {
        "semantic_search" => {
            let args =
                params_to_semantic_search_args(args_value.clone()).map_err(anyhow::Error::from)?;
            let exec = execute_semantic_search_for_daemon(wctx, &args, cancel)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "relation_query" => {
            let args =
                params_to_relation_query_args(args_value.clone()).map_err(anyhow::Error::from)?;
            let exec = execute_relation_query_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "structural_similar" => {
            let args = params_to_structural_similar_args(args_value.clone())
                .map_err(anyhow::Error::from)?;
            let exec = execute_structural_similar_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "direct_callers" => {
            let args =
                params_to_direct_callers_args(args_value.clone()).map_err(anyhow::Error::from)?;
            let exec = execute_direct_callers_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "direct_callees" => {
            let args =
                params_to_direct_callees_args(args_value.clone()).map_err(anyhow::Error::from)?;
            let exec = execute_direct_callees_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "find_unused" => {
            let args =
                params_to_find_unused_args(args_value.clone()).map_err(anyhow::Error::from)?;
            let exec = execute_find_unused_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "find_cycles" => {
            let args =
                params_to_find_cycles_args(args_value.clone()).map_err(anyhow::Error::from)?;
            let exec = execute_find_cycles_for_daemon(wctx, &args);
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "is_node_in_cycle" => {
            let args =
                params_to_is_node_in_cycle_args(args_value.clone()).map_err(anyhow::Error::from)?;
            let exec = execute_is_node_in_cycle_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "trace_path" => {
            let args =
                params_to_trace_path_args(args_value.clone()).map_err(anyhow::Error::from)?;
            let exec = execute_trace_path_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "subgraph" => {
            let args = params_to_subgraph_args(args_value.clone()).map_err(anyhow::Error::from)?;
            let exec = execute_subgraph_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "export_graph" => {
            let args =
                params_to_export_graph_args(args_value.clone()).map_err(anyhow::Error::from)?;
            let exec = execute_export_graph_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "complexity_metrics" => {
            let args = params_to_complexity_metrics_args(args_value.clone())
                .map_err(anyhow::Error::from)?;
            let exec = execute_complexity_metrics_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "generate_overview" => {
            let args = params_to_generate_overview_args(args_value.clone())
                .map_err(anyhow::Error::from)?;
            let exec = execute_generate_overview_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "semantic_diff" => {
            let args =
                params_to_semantic_diff_args(args_value.clone()).map_err(anyhow::Error::from)?;
            let exec = execute_semantic_diff_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "dependency_impact" => {
            let args = params_to_dependency_impact_args(args_value.clone())
                .map_err(anyhow::Error::from)?;
            let exec = execute_dependency_impact_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        "show_dependencies" => {
            let args = params_to_show_dependencies_args(args_value.clone())
                .map_err(anyhow::Error::from)?;
            let exec = execute_show_dependencies_for_daemon(wctx, &args)?;
            tool_response_json(exec).map_err(|e| anyhow!("response build: {e:?}"))
        }
        // `rebuild_index` is in DAEMON_SUPPORTED_TOOL_NAMES but is NOT
        // routed through `dispatch_by_name`. It is a workspace-loading
        // operation handled by `DaemonMcpHandler::handle_rebuild_index`
        // before `dispatch_by_name` is reached.
        "rebuild_index" => Err(anyhow!(
            "dispatch_by_name: rebuild_index must be handled by the caller \
             (DaemonMcpHandler::handle_rebuild_index), not routed through dispatch_by_name"
        )),
        other => Err(anyhow!(
            "dispatch_by_name: unknown tool name {other:?} (not in DAEMON_SUPPORTED_TOOL_NAMES)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Constructing a real `WorkspaceContext` requires a built
    // `Arc<CodeGraph>` + `Arc<QueryExecutor>` which is non-trivial
    // from a pure unit-test context (happy-path exercise needs an
    // indexed workspace on disk). The happy-path 14 arms are covered
    // end-to-end by the U15 MCP-host integration tests in sqry-daemon;
    // here we only assert the unknown-name error path, which requires
    // no real workspace state — `match name` rejects before `wctx` is
    // dereferenced. We build a synthetic `WorkspaceContext` using
    // `CodeGraph::new` + `QueryExecutor::new` on an empty graph; the
    // unknown-name arm returns before any graph operation, so the
    // empty graph is harmless.
    #[test]
    fn dispatch_by_name_rejects_unknown_tool_name() {
        use std::path::PathBuf;
        use std::sync::Arc;

        use sqry_core::graph::unified::concurrent::CodeGraph;
        use sqry_core::query::executor::QueryExecutor;

        let graph = Arc::new(CodeGraph::new());
        let executor = Arc::new(QueryExecutor::new());
        let wctx = WorkspaceContext {
            workspace_root: PathBuf::from("/nonexistent/workspace"),
            graph,
            executor,
        };
        let err = dispatch_by_name(
            "not_a_real_tool",
            &wctx,
            &serde_json::json!({}),
            &sqry_core::query::cancellation::CancellationToken::new(),
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown tool name"),
            "error should mention 'unknown tool name', got: {msg}"
        );
        assert!(
            msg.contains("not_a_real_tool"),
            "error should include offending tool name, got: {msg}"
        );
        assert!(
            msg.contains("DAEMON_SUPPORTED_TOOL_NAMES"),
            "error should reference the constant, got: {msg}"
        );
    }

    /// The empty string is not a valid tool name either.
    #[test]
    fn dispatch_by_name_rejects_empty_name() {
        use std::path::PathBuf;
        use std::sync::Arc;

        use sqry_core::graph::unified::concurrent::CodeGraph;
        use sqry_core::query::executor::QueryExecutor;

        let graph = Arc::new(CodeGraph::new());
        let executor = Arc::new(QueryExecutor::new());
        let wctx = WorkspaceContext {
            workspace_root: PathBuf::from("/nonexistent/workspace"),
            graph,
            executor,
        };
        let err = dispatch_by_name(
            "",
            &wctx,
            &serde_json::json!({}),
            &sqry_core::query::cancellation::CancellationToken::new(),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("unknown tool name"),
            "empty name should error with 'unknown tool name', got: {err}"
        );
    }
}
