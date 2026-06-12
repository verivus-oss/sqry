//! `daemon/stop` handler.
//!
//! Flips the shared [`tokio_util::sync::CancellationToken`] that
//! [`super::super::server::IpcServer::run`] observes in its biased
//! accept-loop `tokio::select!`. The server drains active connections
//! (bounded by
//! [`crate::config::DaemonConfig::ipc_shutdown_drain_secs`]) and then
//! returns.
//!
//! The handler responds `{ scheduled: true }` immediately after
//! flipping the token. The client receives the response frame, then
//! typically reads EOF as the server drops the per-connection writer
//! during the drain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError};

/// `daemon/stop` params — empty for Phase 8a.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StopParams {}

/// `daemon/stop` result payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StopResult {
    pub scheduled: bool,
}

pub(crate) fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let _params: StopParams = match params {
        Value::Null => StopParams::default(),
        other => serde_json::from_value(other).map_err(MethodError::InvalidParams)?,
    };
    ctx.shutdown.cancel();
    let envelope = ResponseEnvelope {
        result: StopResult { scheduled: true },
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope)
        .map_err(|e| MethodError::Internal(anyhow::anyhow!("serialise daemon/stop: {e}")))
}
