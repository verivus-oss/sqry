//! Daemon IPC wrapper for the `find_unused` MCP tool method.

use serde_json::Value;

use sqry_mcp::daemon_adapter::execute_find_unused_for_daemon;
use sqry_mcp::daemon_params::params_to_find_unused_args;

use crate::ipc::methods::tool_dispatch::{classify_and_build, rpc_error_to_method_error};
use crate::ipc::methods::{HandlerContext, MethodError};

pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let args = params_to_find_unused_args(params).map_err(rpc_error_to_method_error)?;
    let path = args.path.clone();
    classify_and_build(ctx, "find_unused", &path, move |wctx, _cancel| {
        execute_find_unused_for_daemon(wctx, &args)
    })
    .await
}
