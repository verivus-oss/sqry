//! Daemon IPC wrapper for the `semantic_search` MCP tool method.

use serde_json::Value;

use sqry_mcp::daemon_adapter::execute_semantic_search_for_daemon;
use sqry_mcp::daemon_params::params_to_semantic_search_args;

use crate::ipc::methods::tool_dispatch::{classify_and_build, rpc_error_to_method_error};
use crate::ipc::methods::{HandlerContext, MethodError};

/// Deserialise params, classify the workspace, route through
/// [`execute_semantic_search_for_daemon`], and build the response envelope.
///
/// Phase 8c U6: `run` is moved into the `spawn_blocking` closure
/// invoked by `classify_and_build`, so `args` must be owned (`move`)
/// and the closure must be `'static`.
pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let args = params_to_semantic_search_args(params).map_err(rpc_error_to_method_error)?;
    let path = args.path.clone();
    classify_and_build(ctx, "semantic_search", &path, move |wctx| {
        execute_semantic_search_for_daemon(wctx, &args)
    })
    .await
}
