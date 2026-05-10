//! JSON-RPC method dispatch for Phase 8a.
//!
//! Phase 8a exposes four daemon management methods:
//!
//! - `daemon/status` — [`daemon_status`]
//! - `daemon/load` — [`daemon_load`]
//! - `daemon/unload` — [`daemon_unload`]
//! - `daemon/stop` — [`daemon_stop`]
//!
//! MCP Robustness (CLI_REBUILD_2) adds:
//!
//! - `daemon/rebuild` — [`daemon_rebuild`]
//! - `daemon/cancel_rebuild` — [`daemon_cancel_rebuild`]
//!
//! STEP_6 of the workspace-aware-cross-repo plan adds:
//!
//! - `daemon/workspaceStatus` — [`daemon_workspace_status`]
//!
//! Phase 8b will add 14 MCP tool methods on the same router; the
//! [`MethodError`] enum below already carries every variant the tool
//! methods will need.

pub mod daemon_active_artifacts;
pub mod daemon_cancel_rebuild;
pub mod daemon_load;
pub mod daemon_rebuild;
pub mod daemon_reset;
pub mod daemon_status;
pub mod daemon_stop;
pub mod daemon_unload;
pub mod daemon_workspace_status;
pub(crate) mod tool_dispatch;

use std::sync::Arc;

use serde_json::json;
use sqry_core::query::executor::QueryExecutor;
use thiserror::Error;
use tokio::task::JoinError;
use tokio_util::sync::CancellationToken;

use crate::config::DaemonConfig;
use crate::error::DaemonError;
use crate::rebuild::RebuildDispatcher;
use crate::workspace::{WorkspaceBuilder, WorkspaceManager};

use super::protocol::{JsonRpcId, JsonRpcPayload, JsonRpcRequest, JsonRpcResponse};
use super::shim_registry::ShimRegistry;
use sqry_daemon_protocol::LogicalWorkspaceWire;

/// Shared context each per-connection task + per-method handler needs.
///
/// `pub(crate)` because the only callers are the router + method
/// handlers within this crate. External test code constructs it via
/// [`IpcServer::bind`] and the test helper
/// [`super::super::ipc::server::IpcServer::spawn_for_tests`].
#[derive(Clone)]
pub(crate) struct HandlerContext {
    pub manager: Arc<WorkspaceManager>,
    #[allow(dead_code)] // held so Phase 8b can drive rebuilds from tool-method paths
    pub dispatcher: Arc<RebuildDispatcher>,
    pub workspace_builder: Arc<dyn WorkspaceBuilder>,
    /// Shared query executor used by Task 7's tool-dispatch handlers.
    /// Task 4's `WorkspaceContext` captures a clone of this `Arc` and hands it
    /// to the `inner::execute_*` functions in `sqry-mcp::daemon_adapter`.
    #[allow(dead_code)] // consumed by Phase 8b Task 7's tool-dispatch handlers
    pub tool_executor: Arc<QueryExecutor>,
    /// Shim-connection registry. U10's router consumes this on every
    /// `ShimRegister` first frame via
    /// [`ShimRegistry::try_register_bounded`] under
    /// `ctx.config.max_shim_connections`; the resulting `ShimHandle`
    /// lives for the byte-pump host call and its RAII `Drop` removes
    /// the entry when the host returns.
    pub shim_registry: Arc<ShimRegistry>,
    pub shutdown: CancellationToken,
    pub config: Arc<DaemonConfig>,
    pub daemon_version: &'static str,
}

impl std::fmt::Debug for HandlerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandlerContext")
            .field("daemon_version", &self.daemon_version)
            .finish_non_exhaustive()
    }
}

/// Per-connection state captured at handshake time and inherited by
/// every subsequent request on the same connection.
///
/// STEP_6 (workspace-aware-cross-repo, 2026-04-26) iter-2 BLOCK fix:
/// `DaemonHello.logical_workspace` documented a connection-level
/// binding contract — every later `daemon/load` that omits
/// `logical_workspace` from its own params should inherit the
/// connection's binding. The pre-fix router parsed the field on the
/// hello frame and discarded it. This struct gives every per-frame
/// dispatch path a place to read the inherited binding.
///
/// Precedence rule:
///   1. Per-request `LoadParams.logical_workspace` if present.
///   2. Otherwise the connection-level
///      `connection_logical_workspace` recorded from `DaemonHello`.
///   3. Otherwise `None` — anonymous (per-source-root) semantics,
///      bit-for-bit pre-STEP_6 behaviour.
///
/// The struct is per-connection (NOT shared across connections), so
/// every connection has its own binding. `run_hello_connection` in
/// `super::super::router` constructs it once from the parsed
/// `DaemonHello` and passes it by reference into the per-frame
/// dispatch chain.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConnectionState {
    /// Connection-level logical-workspace binding from `DaemonHello`.
    /// `None` for connections that do not bind a logical workspace.
    pub connection_logical_workspace: Option<LogicalWorkspaceWire>,
}

impl ConnectionState {
    /// Construct a connection state with no binding. Equivalent to
    /// `Default::default()`. Useful for tests that synthesise a
    /// dispatch path without a real handshake; production code
    /// always builds via [`Self::from_hello`] from a parsed
    /// `DaemonHello.logical_workspace`.
    #[allow(dead_code)] // exposed for unit tests / future synthetic dispatch paths
    pub fn empty() -> Self {
        Self::default()
    }

    /// Construct a connection state pre-populated with a
    /// `DaemonHello`-supplied logical-workspace binding.
    pub fn from_hello(binding: Option<LogicalWorkspaceWire>) -> Self {
        Self {
            connection_logical_workspace: binding,
        }
    }
}

/// Method-layer error. Converts into a JSON-RPC error response via
/// [`MethodError::into_jsonrpc_response`].
#[derive(Debug, Error)]
pub enum MethodError {
    /// `-32601` — method name not registered in the dispatcher.
    #[error("method not found: {0}")]
    MethodNotFound(String),

    /// `-32602` — params failed typed deserialisation or semantic
    /// validation (e.g., `index_root` is a file).
    #[error("invalid params: {0}")]
    InvalidParams(#[source] serde_json::Error),

    /// `-32602` — structured validation error carrying the rmcp
    /// [`sqry_mcp::error::RpcError`] payload verbatim. Used by the
    /// Phase 8b tool-dispatch path so the daemon emits a JSON-RPC
    /// error whose `message` + `data` fields match the rmcp
    /// `SqryServer` wire form bit-for-bit.
    #[error("invalid params: {message}")]
    InvalidParamsStructured {
        message: String,
        kind: String,
        retryable: bool,
        retry_after_ms: Option<u64>,
        details: Option<serde_json::Value>,
    },

    /// `-32600` — typically surfaces from inner validators that
    /// discover a request-shape error after the top-level validator
    /// has already accepted the request. Phase 8b/8c surface this
    /// when tool-method params fail cross-field validation.
    #[error("invalid request: {0}")]
    #[allow(dead_code)] // consumed by Phase 8b
    InvalidRequest(String),

    /// Daemon-specific error. Code derived from
    /// [`DaemonError::jsonrpc_code`] + [`DaemonError::error_data`].
    #[error(transparent)]
    Daemon(#[from] DaemonError),

    /// `-32603` — unexpected internal failure (serialisation, plugin
    /// registry unavailable, etc.).
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),

    /// `-32603` — a blocking builder task failed to join (non-panic
    /// cancellation). Panics are mapped to a structured
    /// [`DaemonError::WorkspaceBuildFailed`] upstream of this variant.
    #[error("spawn_blocking join error: {0}")]
    JoinError(#[from] JoinError),
}

impl MethodError {
    /// Build the wire-level JSON-RPC response for this error.
    #[must_use]
    pub fn into_jsonrpc_response(self, id: Option<JsonRpcId>) -> JsonRpcResponse {
        let (code, message, data) = match self {
            Self::MethodNotFound(ref m) => (
                -32601,
                "Method not found".to_string(),
                Some(json!({ "method": m })),
            ),
            Self::InvalidParams(e) => (
                -32602,
                "Invalid params".to_string(),
                Some(json!({ "reason": e.to_string() })),
            ),
            Self::InvalidParamsStructured {
                message,
                kind,
                retryable,
                retry_after_ms,
                details,
            } => (
                -32602,
                message,
                // Mirrors the rmcp `rpc_error_to_mcp` data envelope at
                // `sqry-mcp/src/server.rs:1330-1343` so both transports emit
                // identical `error.data` payloads for -32602 responses.
                Some(json!({
                    "kind": kind,
                    "retryable": retryable,
                    "retry_after_ms": retry_after_ms,
                    "details": details,
                })),
            ),
            Self::InvalidRequest(ref reason) => (
                -32600,
                "Invalid Request".to_string(),
                Some(json!({ "reason": reason })),
            ),
            Self::Daemon(d) => match d.jsonrpc_code() {
                Some(code) => (code, d.to_string(), d.error_data()),
                None => (
                    -32603,
                    "Internal error".to_string(),
                    Some(json!({ "reason": d.to_string() })),
                ),
            },
            Self::Internal(e) => (
                -32603,
                "Internal error".to_string(),
                Some(json!({ "reason": e.to_string() })),
            ),
            Self::JoinError(e) => (
                -32603,
                "Internal error".to_string(),
                Some(json!({ "reason": format!("blocking task join: {e}") })),
            ),
        };
        JsonRpcResponse::error(id, code, message, data)
    }
}

/// Extract a human-readable reason string from a panicked
/// [`JoinError`]. Consumes the error because `into_panic` consumes it.
/// Returns `"<non-string panic payload>"` when the panic payload is
/// not a `&str`/`String`.
#[must_use]
pub fn format_panic_payload(join_err: JoinError) -> String {
    if join_err.is_panic() {
        let any = join_err.into_panic();
        if let Some(s) = any.downcast_ref::<&'static str>() {
            return (*s).to_owned();
        }
        if let Some(s) = any.downcast_ref::<String>() {
            return s.clone();
        }
        return "<non-string panic payload>".to_owned();
    }
    // Cancellation or other non-panic join failure.
    format!("task join failure: {join_err}")
}

/// Dispatch a validated, typed [`JsonRpcRequest`]. Returns `None` for
/// notifications (no response expected).
///
/// `conn` carries per-connection state captured at handshake time —
/// today only the optional `DaemonHello.logical_workspace` binding,
/// inherited by `daemon/load` calls that omit their own
/// `logical_workspace` param (STEP_6 iter-2 BLOCK fix).
pub(crate) async fn dispatch(
    ctx: &HandlerContext,
    conn: &ConnectionState,
    req: JsonRpcRequest,
) -> Option<JsonRpcResponse> {
    let id = req.id.clone()?;

    let result = match req.method.as_str() {
        "daemon/status" => daemon_status::handle(ctx, req.params).await,
        "daemon/load" => daemon_load::handle(ctx, conn, req.params).await,
        "daemon/unload" => daemon_unload::handle(ctx, req.params).await,
        "daemon/stop" => daemon_stop::handle(ctx, req.params).await,
        "daemon/rebuild" => daemon_rebuild::handle(ctx, req.params).await,
        "daemon/cancel_rebuild" => daemon_cancel_rebuild::handle(ctx, req.params).await,
        // STEP_6 (workspace-aware-cross-repo): aggregate
        // per-source-root rollup of every WorkspaceKey grouped under
        // the requested `workspace_id`. See
        // `daemon_workspace_status` module docs for the contract.
        "daemon/workspaceStatus" => daemon_workspace_status::handle(ctx, req.params).await,
        // sqry-mcp flakiness P1 (`E_p1_cluster.md` §E.4 + `00_contracts.md`
        // §3.CC-4): read-only enumeration of `.sqry/graph` directories
        // belonging to every live workspace. Consumed by
        // `sqry workspace clean` (cluster E Layer-2) with a 250ms caller
        // budget; fallback path is `<root>/.sqry/graph/active.lock`.
        "daemon/active-artifacts" => daemon_active_artifacts::handle(ctx, req.params).await,
        // Cluster-G §3.2: non-destructive workspace recovery. Drops the
        // in-memory graph + admission bytes but preserves the manager-
        // map entry, `pinned` bit, and `last_error`. Files on disk are
        // never touched (cluster-E §E.4 owns the destructive path).
        "daemon/reset" => daemon_reset::handle(ctx, req.params).await,
        // Phase 8b Task 7 — the 14 MCP tool methods. Each is gated on
        // `WorkspaceManager::classify_for_serve` inside
        // `tool_dispatch::classify_and_build`; NotReady verdicts surface
        // as -32001 `WorkspaceBuildFailed`, Stale verdicts carry a
        // `_stale_warning` plus `meta.stale = true`, Fresh verdicts
        // serve from the live (or rebuild-in-flight) graph.
        "semantic_search" | "relation_query" | "direct_callers" | "direct_callees"
        | "find_unused" | "find_cycles" | "is_node_in_cycle" | "trace_path" | "subgraph"
        | "export_graph" | "complexity_metrics" | "semantic_diff" | "dependency_impact"
        | "show_dependencies" => {
            tool_dispatch::dispatch_tool(ctx, req.method.as_str(), req.params).await
        }
        other => Err(MethodError::MethodNotFound(other.to_owned())),
    };

    Some(match result {
        Ok(envelope_json) => JsonRpcResponse {
            jsonrpc: super::protocol::JsonRpcVersion,
            id: Some(id),
            payload: JsonRpcPayload::Success {
                result: envelope_json,
            },
        },
        Err(e) => e.into_jsonrpc_response(Some(id)),
    })
}

/// Helper for the batch path — builds an internal-error response with
/// structured context. Not a method handler; exposed here so the
/// batch loop in [`super::router`] can use it.
#[must_use]
pub(crate) fn internal_error_response(id: Option<JsonRpcId>, reason: &str) -> JsonRpcResponse {
    JsonRpcResponse::error(
        id,
        -32603,
        "Internal error",
        Some(json!({ "reason": reason })),
    )
}
