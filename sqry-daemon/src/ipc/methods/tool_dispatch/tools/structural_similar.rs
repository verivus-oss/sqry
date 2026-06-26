//! Daemon IPC wrapper for the `structural_similar` MCP tool method (U07).

use serde_json::Value;

use sqry_mcp::daemon_adapter::execute_structural_similar_for_daemon;
use sqry_mcp::daemon_params::params_to_structural_similar_args;

use crate::ipc::methods::tool_dispatch::{classify_and_build, rpc_error_to_method_error};
use crate::ipc::methods::{HandlerContext, MethodError};

pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let args = params_to_structural_similar_args(params).map_err(rpc_error_to_method_error)?;
    let path = args.path.clone();
    classify_and_build(ctx, "structural_similar", &path, move |wctx, _cancel| {
        execute_structural_similar_for_daemon(wctx, &args)
    })
    .await
}
