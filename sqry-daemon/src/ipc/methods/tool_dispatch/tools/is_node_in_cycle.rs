//! Daemon IPC wrapper for the `is_node_in_cycle` MCP tool method.

use serde_json::Value;

use sqry_mcp::daemon_adapter::execute_is_node_in_cycle_for_daemon;
use sqry_mcp::daemon_params::params_to_is_node_in_cycle_args;

use crate::ipc::methods::tool_dispatch::{classify_and_build, rpc_error_to_method_error};
use crate::ipc::methods::{HandlerContext, MethodError};

pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let args = params_to_is_node_in_cycle_args(params).map_err(rpc_error_to_method_error)?;
    let path = args.path.clone();
    classify_and_build(ctx, "is_node_in_cycle", &path, move |wctx| {
        execute_is_node_in_cycle_for_daemon(wctx, &args)
    })
    .await
}
