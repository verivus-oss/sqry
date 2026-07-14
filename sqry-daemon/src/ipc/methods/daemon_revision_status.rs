//! Revision status/list daemon methods.

use std::path::Path;

use serde_json::Value;
use sqry_daemon_protocol::{ListRevisionsRequest, ListRevisionsResult, RevisionStatusRequest};

use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError};

/// Handle `daemon/listRevisions`.
pub(crate) fn handle_list(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let req: ListRevisionsRequest = match params {
        Value::Null => ListRevisionsRequest::default(),
        other => serde_json::from_value(other).map_err(MethodError::InvalidParams)?,
    };
    // #566: the optional `root` filter is a user-supplied path; reject a
    // relative one rather than canonicalizing it against the daemon's own CWD.
    if let Some(root) = req.root.as_deref() {
        crate::ipc::path_policy::ensure_absolute_workspace_path(root)
            .map_err(|reason| crate::error::DaemonError::InvalidArgument { reason })?;
    }
    let root = req.root.as_deref().map(canonical_or_original);
    let result = ListRevisionsResult {
        revisions: ctx
            .manager
            .resident_revision_statuses(root.as_deref(), req.include_unloaded),
    };
    let envelope = ResponseEnvelope {
        result,
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope).map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
}

/// Handle `daemon/revisionStatus`.
pub(crate) fn handle_status(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let req: RevisionStatusRequest = match params {
        Value::Null => {
            return Err(MethodError::InvalidParams(serde::de::Error::custom(
                "daemon/revisionStatus requires params",
            )));
        }
        other => serde_json::from_value(other).map_err(MethodError::InvalidParams)?,
    };
    let Some(handle) = ctx.manager.resident_revisions().get(&req.revision_id) else {
        return Err(crate::error::DaemonError::RevisionSourceUnavailable {
            reason: format!("resident revision {} is not loaded", req.revision_id.0),
            path: None,
        }
        .into());
    };
    let envelope = ResponseEnvelope {
        result: handle.status(),
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope).map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
}

fn canonical_or_original(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::handle_list;
    use crate::RebuildDispatcher;
    use crate::config::DaemonConfig;
    use crate::ipc::methods::{HandlerContext, MethodError};
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

    #[test]
    fn list_revisions_rejects_relative_root_filter() {
        // #566: the optional `root` filter is a user path; a relative one must
        // be rejected, not canonicalized against the daemon CWD.
        let ctx = make_ctx();
        let err = handle_list(&ctx, json!({ "root": "relative/dir" }))
            .expect_err("relative root filter must be rejected");
        match err {
            MethodError::Daemon(crate::error::DaemonError::InvalidArgument { reason }) => assert!(
                reason.contains("absolute"),
                "reason must mention the absolute-path requirement: {reason}"
            ),
            other => panic!("expected InvalidArgument, got {other:?}"),
        }
    }
}
