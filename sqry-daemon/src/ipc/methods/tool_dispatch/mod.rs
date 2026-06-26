//! Phase 8b Task 7 — tool-method dispatch.
//!
//! Routes the 14 MCP tool methods over the daemon IPC. Each call is
//! gated on [`crate::workspace::WorkspaceManager::classify_for_serve`],
//! which returns one of:
//!
//! - [`crate::workspace::ServeVerdict::Fresh`] — serve from the graph,
//!   emit a `ResponseMeta::fresh_from(state, daemon_version)` envelope.
//! - [`crate::workspace::ServeVerdict::Stale`] — serve from the
//!   last-good graph, splice a `_stale_warning` into the inner JSON
//!   object, and emit `ResponseMeta::stale_from(...)`.
//! - [`crate::workspace::ServeVerdict::NotReady`] — error with
//!   [`DaemonError::WorkspaceBuildFailed`] (JSON-RPC code -32001).
//!
//! Per-method parameter deserialisation + Args conversion lives in
//! `tools/<method>.rs` alongside a thin `handle` wrapper that calls
//! [`classify_and_build`].
//!
//! # Phase 8c U6 refactor
//!
//! The classify + execute + stale-warning core now lives in
//! [`crate::ipc::tool_core`]. This module retains a thin,
//! transport-specific [`classify_and_build`] wrapper that:
//!
//! 1. Delegates to `tool_core::classify_and_execute` (which handles
//!    `tokio::task::spawn_blocking` + `tokio::time::timeout` +
//!    `DaemonError::ToolTimeout` / `DaemonError::InvalidArgument`).
//! 2. Converts the typed `ToolExecution<T>` produced by
//!    `sqry_mcp::daemon_adapter::execute_*_for_daemon` into a
//!    `serde_json::Value` via `tool_response_json` inside the
//!    `spawn_blocking` closure.
//! 3. Wraps the returned [`super::super::tool_core::ExecuteVerdict`]
//!    in a `ResponseEnvelope` with the daemon's JSON-RPC
//!    `ResponseMeta::fresh_from` / `ResponseMeta::stale_from` meta
//!    shape, splicing `_stale_warning` on the Stale arm.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sqry_mcp::daemon_adapter::WorkspaceContext;
use sqry_mcp::execution::ToolExecution;

use crate::ipc::methods::{HandlerContext, MethodError};
use crate::ipc::protocol::{ResponseEnvelope, ResponseMeta};
use crate::ipc::tool_core;

pub(crate) mod tools;

/// Dispatch one of the 14 registered tool methods. Called from
/// [`crate::ipc::methods::dispatch`] via the match arm introduced in
/// Task 7 Work Item D.
pub(crate) async fn dispatch_tool(
    ctx: &HandlerContext,
    method: &str,
    params: Value,
) -> Result<Value, MethodError> {
    match method {
        "semantic_search" => tools::semantic_search::handle(ctx, params).await,
        "relation_query" => tools::relation_query::handle(ctx, params).await,
        "direct_callers" => tools::direct_callers::handle(ctx, params).await,
        "direct_callees" => tools::direct_callees::handle(ctx, params).await,
        "find_unused" => tools::find_unused::handle(ctx, params).await,
        "find_cycles" => tools::find_cycles::handle(ctx, params).await,
        "is_node_in_cycle" => tools::is_node_in_cycle::handle(ctx, params).await,
        "trace_path" => tools::trace_path::handle(ctx, params).await,
        "subgraph" => tools::subgraph::handle(ctx, params).await,
        "export_graph" => tools::export_graph::handle(ctx, params).await,
        "complexity_metrics" => tools::complexity_metrics::handle(ctx, params).await,
        "semantic_diff" => tools::semantic_diff::handle(ctx, params).await,
        "dependency_impact" => tools::dependency_impact::handle(ctx, params).await,
        "show_dependencies" => tools::show_dependencies::handle(ctx, params).await,
        "structural_similar" => tools::structural_similar::handle(ctx, params).await,
        // `rebuild_index` is intentionally NOT routed through this
        // table — it is a mutating workspace-loading operation served
        // by the daemon MCP host's `handle_rebuild_index` (and the
        // dedicated `daemon/rebuild` JSON-RPC method). Routing it
        // through `tool_dispatch` would bypass the explicit
        // `MutatingRebuild` guard on `DaemonGraphProvider::acquire`,
        // so the JSON-RPC method table reports it as unknown here.
        other => Err(MethodError::MethodNotFound(other.to_owned())),
    }
}

/// Convert an [`sqry_mcp::error::RpcError`] into a [`MethodError`],
/// preserving the JSON-RPC error code and wire payload semantics.
///
/// Validation-kind errors (code `-32602`) map to
/// [`MethodError::InvalidParamsStructured`] so the outgoing JSON-RPC
/// response carries the `RpcError.message` verbatim as `error.message`
/// and the full `{kind, retryable, retry_after_ms, details}` envelope
/// as `error.data` — matching the rmcp `SqryServer` path at
/// `sqry-mcp/src/server.rs:1330-1343` bit-for-bit.
///
/// Everything else maps to [`MethodError::Internal`] (`-32603`).
pub(crate) fn rpc_error_to_method_error(e: sqry_mcp::error::RpcError) -> MethodError {
    if e.code == -32602 {
        MethodError::InvalidParamsStructured {
            message: e.message,
            kind: e.kind,
            retryable: e.retryable,
            retry_after_ms: e.retry_after_ms,
            details: e.details,
        }
    } else {
        MethodError::Internal(anyhow::anyhow!("{} ({})", e.message, e.kind))
    }
}

/// Shared verdict-handling helper used by every tool wrapper.
///
/// SGA05: delegates to [`tool_core::acquire_and_execute`], which
/// routes graph acquisition through the shared
/// [`DaemonGraphProvider`](crate::workspace::acquirer::DaemonGraphProvider)
/// boundary so read-only tool dispatch picks up the bounded one-shot
/// reload semantics on `WorkspaceEvicted`. The resulting
/// [`tool_core::ExecuteVerdict`] is wrapped in a JSON-RPC
/// [`ResponseEnvelope`] with [`ResponseMeta::fresh_from`] /
/// [`ResponseMeta::stale_from`] — the wire shape is unchanged.
///
/// The closure must be `Send + 'static` because it crosses a
/// `spawn_blocking` boundary.
pub(crate) async fn classify_and_build<T, F>(
    ctx: &HandlerContext,
    tool_name: &'static str,
    path: &str,
    run: F,
) -> Result<Value, MethodError>
where
    // `A_cancellation.md` §A3 + `00_contracts.md` §3.CC-1: typed
    // run-closure now receives the per-request `&CancellationToken`
    // alongside the `WorkspaceContext` so deadline-driven cancellation
    // flows into per-tool inner bodies. The closure is `Send + 'static`
    // because it crosses a `spawn_blocking` boundary.
    F: FnOnce(
            &WorkspaceContext,
            &sqry_core::query::cancellation::CancellationToken,
        ) -> anyhow::Result<ToolExecution<T>>
        + Send
        + 'static,
    T: serde::Serialize + Send + 'static,
{
    // Wrap the typed `run` closure to return `serde_json::Value` so it
    // fits the uniform `tool_core` signature. `tool_response_json`
    // runs inside `spawn_blocking` too — it is pure CPU/serialisation
    // work anchored to the same blocking context.
    let run_as_value = move |wctx: &WorkspaceContext,
                             cancel: &sqry_core::query::cancellation::CancellationToken|
          -> anyhow::Result<Value> {
        let exec = run(wctx, cancel)?;
        let inner = sqry_mcp::daemon_adapter::tool_response_json(exec)
            .map_err(|e| anyhow::anyhow!("response build: {e:?}"))?;
        Ok(inner)
    };

    let tool_timeout = Duration::from_secs(ctx.config.tool_timeout_secs);
    let verdict = tool_core::acquire_and_execute(
        Arc::clone(&ctx.manager),
        Arc::clone(&ctx.workspace_builder),
        Arc::clone(&ctx.tool_executor),
        tool_timeout,
        path,
        Some(tool_name),
        run_as_value,
    )
    .await
    .map_err(MethodError::Daemon)?;

    match verdict {
        tool_core::ExecuteVerdict::Fresh { inner, state } => {
            let envelope = ResponseEnvelope {
                result: inner,
                meta: ResponseMeta::fresh_from(state, ctx.daemon_version),
            };
            serde_json::to_value(&envelope)
                .map_err(|e| MethodError::Internal(anyhow::anyhow!("envelope serialise: {e}")))
        }
        tool_core::ExecuteVerdict::Stale {
            mut inner,
            stale_warning,
            last_good_at,
            last_error,
        } => {
            if let Value::Object(ref mut map) = inner {
                map.insert("_stale_warning".into(), Value::String(stale_warning));
            }
            let envelope = ResponseEnvelope {
                result: inner,
                meta: ResponseMeta::stale_from(last_good_at, last_error, ctx.daemon_version),
            };
            serde_json::to_value(&envelope)
                .map_err(|e| MethodError::Internal(anyhow::anyhow!("envelope serialise: {e}")))
        }
    }
}
