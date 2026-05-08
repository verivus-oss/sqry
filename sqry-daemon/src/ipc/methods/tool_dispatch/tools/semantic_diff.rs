//! Daemon IPC wrapper for the `semantic_diff` MCP tool method.

use serde_json::Value;

use sqry_mcp::daemon_adapter::execute_semantic_diff_for_daemon;
use sqry_mcp::daemon_params::params_to_semantic_diff_args;

use crate::ipc::methods::tool_dispatch::{classify_and_build, rpc_error_to_method_error};
use crate::ipc::methods::{HandlerContext, MethodError};

pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let args = params_to_semantic_diff_args(params).map_err(rpc_error_to_method_error)?;
    let path = args.path.clone();
    classify_and_build(ctx, "semantic_diff", &path, move |wctx| {
        execute_semantic_diff_for_daemon(wctx, &args)
    })
    .await
}
