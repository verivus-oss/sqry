//! `daemon/active-artifacts` handler — read-only enumeration of
//! `.sqry/graph` directories belonging to every workspace whose state
//! is `Loading | Loaded | Rebuilding`.
//!
//! Source: `00_contracts.md` §3.CC-4 + `E_p1_cluster.md` §E.4 (DPG
//! hand-off) + `G_daemon_control_plane.md` §7.1 hand-off G4. This is
//! the canonical mechanism that resolved the iter-1 mechanism
//! conflict between E (which required a dedicated IPC) and G (which
//! offered to filter `daemon/status` instead) — E's mechanism wins
//! because it gives `sqry workspace clean` a stable `() → Vec<PathBuf>`
//! contract independent of the heavier `DaemonStatus` schema.
//!
//! Wire contract:
//!
//! - **Method name:** `daemon/active-artifacts`
//! - **Params:** empty `{}` or `null` (no caller input).
//! - **Result:** `ResponseEnvelope<ActiveArtifactsResult>` where
//!   `ActiveArtifactsResult { artifacts: Vec<PathBuf> }`.
//! - **Latency budget (caller-side):** 250 ms — `sqry workspace clean`
//!   times the call out and falls back to "no daemon available" mode
//!   on miss.
//! - **Concurrency:** read-only (`WorkspaceManager::active_artifact_dirs`
//!   takes `self.workspaces.read()`).
//!
//! The fallback path described in `00_contracts.md` §3.CC-4 (an
//! advisory `<root>/.sqry/graph/active.lock` file written at publish
//! time) is owned by `IMP-G` Layer-2 and is not part of this method.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError};

/// `daemon/active-artifacts` params — currently empty. Accepts either an
/// empty object `{}` or no params field at all (for parity with
/// `daemon/status`'s param shape).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ActiveArtifactsParams {}

/// Wire result of `daemon/active-artifacts`. Always a single
/// stable-sorted `Vec<PathBuf>` — empty when the daemon has no live
/// workspaces.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveArtifactsResult {
    /// Absolute paths to the `.sqry/graph` directories owned by every
    /// workspace whose state is `Loading | Loaded | Rebuilding` at the
    /// instant the read lock was taken.
    pub artifacts: Vec<PathBuf>,
}

/// Handle one `daemon/active-artifacts` request.
pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let _params: ActiveArtifactsParams = match params {
        Value::Null => ActiveArtifactsParams::default(),
        other => serde_json::from_value(other).map_err(MethodError::InvalidParams)?,
    };
    let artifacts = ctx.manager.active_artifact_dirs();
    let envelope = ResponseEnvelope {
        result: ActiveArtifactsResult { artifacts },
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope).map_err(|e| {
        MethodError::Internal(anyhow::anyhow!("serialise daemon/active-artifacts: {e}"))
    })
}
