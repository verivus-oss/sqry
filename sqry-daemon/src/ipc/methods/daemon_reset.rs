//! `daemon/reset` handler — non-destructive workspace recovery
//! (cluster-G §3.2).
//!
//! Drops the in-memory graph + admission bytes for a single
//! workspace but **retains** its manager-map entry, `pinned` bit, and
//! `last_error`. The next `daemon/load` for the same path is cheap
//! because the workspace key, the `<root>/.sqry/` snapshot, and any
//! per-workspace plugin metadata survive. Files on disk are NEVER
//! touched — destructive cleanup is owned by `sqry workspace clean`
//! (cluster-E IMP-E.4).
//!
//! Wire contract:
//!
//! - **Method name:** `daemon/reset`
//! - **Params:** `{ "path": "<workspace-root>", "force": <bool?> }`.
//!   `path` is canonicalised by [`resolve_index_root`] before lookup
//!   so a relative path or `..` cannot target an unintended workspace.
//!   `force = true` is required to reset a `pinned` workspace.
//! - **Result:** `ResponseEnvelope<ResetResult>` where
//!   `ResetResult { root, reset }`. `reset = true` when the workspace
//!   was present and reset; `false` when the path matched no
//!   workspace.
//! - **Errors:**
//!   - [`-32004`] `WorkspaceNotLoaded` when the path is unknown.
//!   - [`-32008`] `ResetWhileLoading` when the workspace is currently
//!     `Loading`.
//!   - [`-32009`] `ResetCancellationDispatched` when a rebuild was in
//!     flight; the caller should retry after `retry_after_ms`.
//!   - [`-32010`] `WorkspacePinned` when the workspace is pinned and
//!     the caller did not pass `force = true`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

use super::super::path_policy::resolve_index_root;
use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError};
use crate::error::DaemonError;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetParams {
    pub path: PathBuf,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResetResult {
    /// Canonicalised workspace root.
    pub root: PathBuf,
    /// `true` when the workspace was present and reset; `false` when
    /// the path matched no registered workspace.
    pub reset: bool,
}

pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let p: ResetParams = serde_json::from_value(params).map_err(MethodError::InvalidParams)?;
    let canonical = resolve_index_root(&p.path).map_err(|e| {
        MethodError::Daemon(DaemonError::InvalidArgument {
            reason: format!("cannot resolve path {}: {e}", p.path.display()),
        })
    })?;
    let Some((key, _ws)) = ctx.manager.find_key_and_workspace_by_path(&canonical) else {
        return Err(MethodError::Daemon(DaemonError::WorkspaceNotLoaded {
            root: canonical,
        }));
    };
    let reset = ctx
        .manager
        .reset(&key, p.force)
        .map_err(MethodError::Daemon)?;
    let envelope = ResponseEnvelope {
        result: ResetResult {
            root: canonical,
            reset,
        },
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope)
        .map_err(|e| MethodError::Internal(anyhow::anyhow!("serialise daemon/reset: {e}")))
}
