//! `daemon/status` handler.
//!
//! Returns a snapshot of
//! [`crate::workspace::WorkspaceManager::status`] wrapped in a
//! [`super::super::protocol::ResponseEnvelope`] with
//! [`super::super::protocol::ResponseMeta::management`] metadata
//! (the method is not bound to a specific workspace).

use serde::Deserialize;
use serde_json::Value;

use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::{HandlerContext, MethodError};

/// `daemon/status` params — currently empty. Accepts either an empty
/// object `{}` or no params field at all.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StatusParams {}

/// Handle one `daemon/status` request. Returns the
/// [`ResponseEnvelope<DaemonStatus>`] serialised as a `Value` ready
/// for the JSON-RPC `result` field.
pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let _params: StatusParams = match params {
        Value::Null => StatusParams::default(),
        other => serde_json::from_value(other).map_err(MethodError::InvalidParams)?,
    };
    let status = ctx.manager.status();
    let envelope = ResponseEnvelope {
        result: status,
        meta: ResponseMeta::management(ctx.daemon_version),
    };
    serde_json::to_value(&envelope)
        .map_err(|e| MethodError::Internal(anyhow::anyhow!("serialise daemon/status: {e}")))
}
