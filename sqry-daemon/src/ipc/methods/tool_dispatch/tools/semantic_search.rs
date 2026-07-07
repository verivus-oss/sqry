//! Daemon IPC wrapper for the `semantic_search` MCP tool method.

use serde_json::Value;

use sqry_daemon_protocol::{RevisionId, RevisionQueryTarget, RevisionSelector};
use sqry_mcp::daemon_adapter::{
    WorkspaceContext, execute_semantic_search_for_daemon, tool_response_json,
};
use sqry_mcp::daemon_params::params_to_semantic_search_args;
use sqry_mcp::tool_args::SemanticSearchArgs;

use crate::ipc::methods::daemon_load_revision;
use crate::ipc::methods::tool_dispatch::{classify_and_build, rpc_error_to_method_error};
use crate::ipc::methods::{HandlerContext, MethodError};
use crate::ipc::protocol::{ResponseEnvelope, ResponseMeta};

/// Deserialise params, classify the workspace, route through
/// [`execute_semantic_search_for_daemon`], and build the response envelope.
///
/// Phase 8c U6: `run` is moved into the `spawn_blocking` closure
/// invoked by `classify_and_build`, so `args` must be owned (`move`)
/// and the closure must be `'static`.
pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let args = params_to_semantic_search_args(params).map_err(rpc_error_to_method_error)?;
    if let Some(target) = revision_target_from_args(&args)? {
        return handle_revision_semantic_search(ctx, args, target).await;
    }
    let path = args.path.clone();
    classify_and_build(ctx, "semantic_search", &path, move |wctx, cancel| {
        execute_semantic_search_for_daemon(wctx, &args, cancel)
    })
    .await
}

async fn handle_revision_semantic_search(
    ctx: &HandlerContext,
    args: SemanticSearchArgs,
    target: RevisionQueryTarget,
) -> Result<Value, MethodError> {
    let root = std::path::PathBuf::from(&args.path)
        .canonicalize()
        .map_err(|err| {
            MethodError::Daemon(crate::error::DaemonError::InvalidArgument {
                reason: format!(
                    "semantic_search revision target path {} could not be canonicalized: {err}",
                    args.path
                ),
            })
        })?;
    let (revision_id, _metadata) = daemon_load_revision::resolve_query_target(ctx, &root, &target)?;
    let guard = ctx.manager.acquire_resident_query(&revision_id)?;
    let graph = guard.graph().ok_or_else(|| {
        MethodError::Daemon(crate::error::DaemonError::RevisionSourceUnavailable {
            reason: format!("resident revision {} had no loaded graph", revision_id.0),
            path: Some(root.clone()),
        })
    })?;
    drop(guard);

    let wctx = WorkspaceContext {
        workspace_root: root.clone(),
        graph,
        executor: std::sync::Arc::clone(&ctx.tool_executor),
    };
    // issue #503 Phase 2: submit on the shared dedicated CPU pool. Phase 1
    // fixed the moved-token bug here; Phase 2 replaces the hand-rolled
    // spawn_blocking + timeout + flip with `CpuExecutor::run`, which owns and
    // flips the token on deadline (fire-and-forget) and maps the result
    // through the shared ladder. The wire envelope is unchanged.
    let tool_timeout = std::time::Duration::from_secs(ctx.config.tool_timeout_secs);
    let result = ctx
        .cpu_executor
        .run(tool_timeout, &root, move |cancel| {
            let exec = execute_semantic_search_for_daemon(&wctx, &args, cancel)?;
            tool_response_json(exec).map_err(|err| anyhow::anyhow!("response build: {err:?}"))
        })
        .await
        .map_err(MethodError::Daemon)?;

    let envelope = ResponseEnvelope {
        result,
        meta: ResponseMeta::fresh_from(
            crate::workspace::WorkspaceState::Loaded,
            ctx.daemon_version,
        ),
    };
    serde_json::to_value(&envelope).map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
}

fn revision_target_from_args(
    args: &SemanticSearchArgs,
) -> Result<Option<RevisionQueryTarget>, MethodError> {
    if let Some(revision_id) = &args.revision.id {
        return Ok(Some(RevisionQueryTarget::RevisionId {
            revision_id: RevisionId(revision_id.clone()),
        }));
    }
    let selector_count = usize::from(args.revision.git_ref.is_some())
        + usize::from(args.revision.commit.is_some())
        + usize::from(args.revision.tree.is_some())
        + usize::from(args.revision.dirty);
    if selector_count > 1 {
        return Err(MethodError::InvalidRequest(
            "semantic_search accepts only one revision selector".to_owned(),
        ));
    }
    let selector = if let Some(name) = &args.revision.git_ref {
        Some(RevisionSelector::Ref { name: name.clone() })
    } else if let Some(oid) = &args.revision.commit {
        Some(RevisionSelector::Commit { oid: oid.clone() })
    } else if let Some(oid) = &args.revision.tree {
        Some(RevisionSelector::Tree { oid: oid.clone() })
    } else if args.revision.dirty {
        Some(RevisionSelector::Dirty {
            include_untracked: args.revision.include_untracked,
            include_ignored: args.revision.include_ignored,
        })
    } else {
        None
    };
    Ok(selector.map(|selector| RevisionQueryTarget::Selector { selector }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use crate::RebuildDispatcher;
    use crate::config::DaemonConfig;
    use crate::ipc::methods::HandlerContext;
    use crate::ipc::methods::tool_dispatch::dispatch_tool;
    use crate::ipc::shim_registry::ShimRegistry;
    use crate::workspace::{EmptyGraphBuilder, WorkspaceManager};
    use sqry_core::plugin::PluginManager;

    fn make_ctx() -> HandlerContext {
        let config = Arc::new(DaemonConfig::default());
        let manager = WorkspaceManager::new_without_reaper(Arc::clone(&config));
        let plugins = Arc::new(PluginManager::default());
        let dispatcher = RebuildDispatcher::new(Arc::clone(&manager), Arc::clone(&config), plugins);
        let executor = Arc::new(sqry_core::query::executor::QueryExecutor::default());
        HandlerContext {
            manager,
            dispatcher,
            workspace_builder: Arc::new(EmptyGraphBuilder),
            tool_executor: executor,
            cpu_executor: crate::ipc::tool_core::cpu_executor::CpuExecutor::with_threads(1),
            shim_registry: ShimRegistry::new(),
            shutdown: CancellationToken::new(),
            config,
            daemon_version: "test",
        }
    }

    /// Two revision selectors on a single `semantic_search` dispatch must
    /// surface as `MethodError::InvalidRequest`, which maps to the JSON-RPC
    /// `-32600 "Invalid Request"` envelope. The cross-field selector check
    /// runs before any workspace/graph acquisition, so an unloaded manager
    /// is enough to exercise it end to end through `dispatch_tool`.
    #[tokio::test]
    async fn two_revision_selectors_dispatch_to_invalid_request() {
        let ctx = make_ctx();
        let params = json!({
            "query": "needle",
            "path": "/tmp/does-not-need-to-exist",
            "revision_ref": "main",
            "revision_commit": "0123456789abcdef0123456789abcdef01234567",
        });

        let err = dispatch_tool(&ctx, "semantic_search", params)
            .await
            .expect_err("two revision selectors must be rejected");

        let resp = err.into_jsonrpc_response(None);
        let body = serde_json::to_value(&resp).expect("response to_value");
        let code = body
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_i64())
            .expect("error.code present");
        let message = body
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .expect("error.message present");
        assert_eq!(code, -32600, "two-selector rejection is JSON-RPC -32600");
        assert_eq!(message, "Invalid Request");
    }
}
