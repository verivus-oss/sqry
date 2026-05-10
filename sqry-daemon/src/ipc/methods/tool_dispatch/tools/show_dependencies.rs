//! Daemon IPC wrapper for the `show_dependencies` MCP tool method.
//!
//! The underlying inner sqry-mcp body is historically named
//! `graph_inner::execute_get_dependencies`; only the wire-facing name
//! was renamed. The daemon wrapper therefore calls
//! `execute_show_dependencies_for_daemon` — the Task 4 facade that
//! dispatches to the legacy inner body.

use serde_json::Value;

use sqry_mcp::daemon_adapter::execute_show_dependencies_for_daemon;
use sqry_mcp::daemon_params::params_to_show_dependencies_args;

use crate::ipc::methods::tool_dispatch::{classify_and_build, rpc_error_to_method_error};
use crate::ipc::methods::{HandlerContext, MethodError};

pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let args = params_to_show_dependencies_args(params).map_err(rpc_error_to_method_error)?;
    let path = args.path.clone();
    classify_and_build(ctx, "show_dependencies", &path, move |wctx, _cancel| {
        execute_show_dependencies_for_daemon(wctx, &args)
    })
    .await
}
