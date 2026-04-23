//! `daemon/unload` handler.
//!
//! Evicts the named workspace from the manager. The operation is
//! idempotent: calling `daemon/unload` on a never-loaded or already-
//! unloaded workspace is a no-op that returns
//! `was_loaded: false`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqry_core::project::ProjectRootMode;

use crate::workspace::WorkspaceKey;

use super::super::path_policy::resolve_index_root;
use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError};

/// `daemon/unload` params.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnloadParams {
    pub index_root: PathBuf,
    #[serde(default)]
    pub root_mode: Option<ProjectRootMode>,
    #[serde(default)]
    pub config_fingerprint: Option<u64>,
}

/// `daemon/unload` result payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnloadResult {
    pub root: PathBuf,
    pub was_loaded: bool,
}

pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let params: UnloadParams =
        serde_json::from_value(params).map_err(MethodError::InvalidParams)?;
    let canonical = resolve_index_root(&params.index_root)?;
    let key = WorkspaceKey::new(
        canonical.clone(),
        params.root_mode.unwrap_or_default(),
        params.config_fingerprint.unwrap_or(0),
    );
    let was_loaded = ctx.manager.unload(&key);
    let envelope = ResponseEnvelope {
        result: UnloadResult {
            root: canonical,
            was_loaded,
        },
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope)
        .map_err(|e| MethodError::Internal(anyhow::anyhow!("serialise daemon/unload: {e}")))
}
