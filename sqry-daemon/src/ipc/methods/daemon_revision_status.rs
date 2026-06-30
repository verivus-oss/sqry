//! Revision status/list daemon methods.

use std::path::Path;

use serde_json::Value;
use sqry_daemon_protocol::{ListRevisionsRequest, ListRevisionsResult, RevisionStatusRequest};

use super::{HandlerContext, MethodError};

/// Handle `daemon/listRevisions`.
pub(crate) fn handle_list(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let req: ListRevisionsRequest = match params {
        Value::Null => ListRevisionsRequest::default(),
        other => serde_json::from_value(other).map_err(MethodError::InvalidParams)?,
    };
    let root = req.root.as_deref().map(canonical_or_original);
    let result = ListRevisionsResult {
        revisions: ctx
            .manager
            .resident_revision_statuses(root.as_deref(), req.include_unloaded),
    };
    serde_json::to_value(result).map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
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
    serde_json::to_value(handle.status())
        .map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
}

fn canonical_or_original(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
