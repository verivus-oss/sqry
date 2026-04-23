//! `daemon/load` handler.
//!
//! Canonicalises `index_root`, constructs a
//! [`crate::workspace::WorkspaceKey`], and drives
//! [`crate::workspace::WorkspaceManager::get_or_load`] on the blocking
//! pool via [`tokio::task::spawn_blocking`].
//!
//! Panics inside the blocking closure are translated into
//! [`crate::error::DaemonError::WorkspaceBuildFailed`] (`-32001`) so
//! clients always see structured daemon errors, never a transport-
//! level `-32603`.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;
use sqry_core::graph::unified::GraphMemorySize;
use sqry_core::project::ProjectRootMode;
use sqry_daemon_protocol::LoadResult;

use crate::config::{DaemonConfig, WORKING_SET_MULTIPLIER};
use crate::error::DaemonError;
use crate::workspace::{WorkspaceKey, WorkspaceState};

use super::super::path_policy::resolve_index_root;
use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError, format_panic_payload};

/// `daemon/load` params.
///
/// Pinning is a config concern — see
/// [`crate::config::WorkspaceConfig::pinned`]; clients cannot mark a
/// workspace as pinned at `daemon/load` time. To pin a workspace, add
/// it to `daemon.toml` and restart the daemon (or call a future
/// `daemon/pin` method when Task 9 exposes it).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadParams {
    pub index_root: PathBuf,
    #[serde(default)]
    pub root_mode: Option<ProjectRootMode>,
    #[serde(default)]
    pub config_fingerprint: Option<u64>,
}

/// Conservative initial working-set estimate for a cold `get_or_load`.
/// Calibration is deferred to Task 13 / 14 per the plan; this value is
/// the same 2 MiB floor the design document specifies.
const INITIAL_WORKING_SET_BYTES: u64 = 2 * 1024 * 1024;

/// Multiply the initial working-set estimate by the conservative
/// factor used everywhere else in admission accounting (§G.6).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn working_set_estimate_for_initial(_cfg: &DaemonConfig) -> u64 {
    #[allow(clippy::cast_precision_loss)]
    let scaled = (INITIAL_WORKING_SET_BYTES as f64) * WORKING_SET_MULTIPLIER;
    scaled as u64
}

/// Handle one `daemon/load` request.
pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let params: LoadParams = serde_json::from_value(params).map_err(MethodError::InvalidParams)?;

    let canonical_root = resolve_index_root(&params.index_root)?;

    let key = WorkspaceKey::new(
        canonical_root.clone(),
        params.root_mode.unwrap_or_default(),
        params.config_fingerprint.unwrap_or(0),
    );

    let manager = Arc::clone(&ctx.manager);
    let builder = Arc::clone(&ctx.workspace_builder);
    let estimate = working_set_estimate_for_initial(&ctx.config);
    let key_for_task = key.clone();

    let graph = match tokio::task::spawn_blocking(move || {
        manager.get_or_load(&key_for_task, &*builder, estimate)
    })
    .await
    {
        Ok(Ok(graph)) => graph,
        Ok(Err(daemon_err)) => return Err(MethodError::Daemon(daemon_err)),
        Err(join_err) if join_err.is_panic() => {
            let reason = format_panic_payload(join_err);
            return Err(MethodError::Daemon(DaemonError::WorkspaceBuildFailed {
                root: key.index_root.clone(),
                reason: format!("workspace builder panicked: {reason}"),
            }));
        }
        Err(join_err) => return Err(MethodError::JoinError(join_err)),
    };

    let current_bytes = graph.heap_bytes() as u64;
    let envelope = ResponseEnvelope {
        result: LoadResult {
            root: key.index_root.clone(),
            current_bytes,
            state: WorkspaceState::Loaded,
        },
        meta: ResponseMeta::loaded(ctx.daemon_version),
    };
    serde_json::to_value(&envelope)
        .map_err(|e| MethodError::Internal(anyhow::anyhow!("serialise daemon/load: {e}")))
}
