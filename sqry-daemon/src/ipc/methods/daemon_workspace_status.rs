//! `daemon/workspaceStatus` handler.
//!
//! `STEP_6` of the workspace-aware-cross-repo plan (2026-04-26):
//! returns the aggregate per-source-root rollup of every
//! [`crate::workspace::WorkspaceKey`] in the manager whose
//! `workspace_id` matches the request payload.
//!
//! Aggregation rules:
//!
//! - Source roots are sorted lexically by absolute path so the wire
//!   form is deterministic across runs (CLI output / golden tests
//!   assume this ordering).
//! - Eviction is per-source-root; the aggregate reports
//!   [`sqry_daemon_protocol::WorkspaceState::Evicted`] for any source
//!   root that has been LRU'd out while siblings remain loaded
//!   (acceptance criterion #4 / #5 from the `STEP_6` brief).
//! - "Workspace not found" is surfaced as
//!   [`crate::error::DaemonError::WorkspaceNotLoaded`]; the daemon
//!   never returns an empty aggregate.

use serde::Deserialize;
use serde_json::Value;
use sqry_daemon_protocol::{ResponseEnvelope, ResponseMeta, WorkspaceId, WorkspaceIndexStatus};

use crate::error::DaemonError;

use super::{HandlerContext, MethodError};

/// `daemon/workspaceStatus` params — a single 32-byte
/// [`WorkspaceId`]. The daemon walks every loaded workspace whose
/// `workspace_id` matches and rolls up a [`WorkspaceIndexStatus`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceStatusParams {
    pub workspace_id: WorkspaceId,
}

/// Handle one `daemon/workspaceStatus` request. Returns the
/// [`ResponseEnvelope<WorkspaceIndexStatus>`] serialised as a `Value`
/// ready for the JSON-RPC `result` field.
pub(crate) fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let params: WorkspaceStatusParams =
        serde_json::from_value(params).map_err(MethodError::InvalidParams)?;

    let aggregate = ctx
        .manager
        .workspace_index_status(&params.workspace_id)
        .ok_or_else(|| {
            // The daemon does not retain a registry of historical
            // workspace_ids — if no entry currently carries this id we
            // surface `WorkspaceNotLoaded`. Use `<unknown-workspace-id>`
            // as the synthetic root because the wire-form
            // `WorkspaceNotLoaded` envelope is per-path, not per-id;
            // this preserves the existing -32004 contract while still
            // signalling "no such logical workspace right now".
            MethodError::Daemon(DaemonError::WorkspaceNotLoaded {
                root: std::path::PathBuf::from(format!(
                    "<workspace_id={}>",
                    params.workspace_id.as_short_hex()
                )),
            })
        })?;

    let envelope: ResponseEnvelope<WorkspaceIndexStatus> = ResponseEnvelope {
        result: aggregate,
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope).map_err(|e| {
        MethodError::Internal(anyhow::anyhow!("serialise daemon/workspaceStatus: {e}"))
    })
}
