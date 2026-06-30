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
    let cancel = sqry_core::query::cancellation::CancellationToken::new();
    let tool_timeout = std::time::Duration::from_secs(ctx.config.tool_timeout_secs);
    let result = tokio::time::timeout(
        tool_timeout,
        tokio::task::spawn_blocking(move || {
            let exec = execute_semantic_search_for_daemon(&wctx, &args, &cancel)?;
            tool_response_json(exec).map_err(|err| anyhow::anyhow!("response build: {err:?}"))
        }),
    )
    .await
    .map_err(|_| {
        MethodError::Daemon(crate::error::DaemonError::ToolTimeout {
            root: root.clone(),
            secs: tool_timeout.as_secs(),
            deadline_ms: u64::try_from(tool_timeout.as_millis()).unwrap_or(u64::MAX),
        })
    })?
    .map_err(MethodError::JoinError)?
    .map_err(MethodError::Internal)?;

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
