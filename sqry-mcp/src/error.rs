use serde_json::{Map, Number, Value};
use std::fmt;

const KIND_VALIDATION: &str = "validation_error";
const KIND_DEADLINE_EXCEEDED: &str = "deadline_exceeded";
/// NL08 — code returned in the structured MCP envelope for the
/// "ONNX Runtime not installed" condition. Wire-stable; consumers may
/// pattern-match on this string.
pub const CODE_ONNX_RUNTIME_MISSING: &str = "ONNX_RUNTIME_MISSING";

#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    pub kind: String,
    pub retryable: bool,
    pub retry_after_ms: Option<u64>,
    pub details: Option<Value>,
}

impl RpcError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            kind: KIND_VALIDATION.to_string(),
            retryable: false,
            retry_after_ms: None,
            details: None,
        }
    }

    /// Validation error with structured data payload.
    ///
    /// Used by custom validators to include field/constraint context.
    pub fn validation_with_data(message: impl Into<String>, data: Value) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            kind: KIND_VALIDATION.to_string(),
            retryable: false,
            retry_after_ms: None,
            details: Some(data),
        }
    }

    /// NL08: ONNX Runtime missing — produces the structured MCP
    /// envelope `{ code: "ONNX_RUNTIME_MISSING", message: <hint>,
    /// retriable: false }` that every NL08-aware client can pattern
    /// match against. The wire envelope is rendered by
    /// [`crate::server::rpc_error_to_mcp`] (server.rs) by reading the
    /// `details` field below — the envelope's three top-level fields
    /// `code` / `message` / `retriable` live INSIDE `details` because
    /// the existing server-wide envelope shape (`kind` / `retryable` /
    /// `retry_after_ms` / `details`) reserves those names. Per the
    /// NL08 design (DAG `[units.NL08]`) the user-facing 3-field
    /// envelope is what consumers parse; this implementation places it
    /// in `details` while keeping the existing server-wide envelope
    /// structurally intact for parity with other tools.
    #[must_use]
    pub fn onnx_runtime_missing(hint: impl Into<String>) -> Self {
        let hint_str = hint.into();
        let mut detail_map = Map::new();
        detail_map.insert(
            "code".to_string(),
            Value::String(CODE_ONNX_RUNTIME_MISSING.to_string()),
        );
        detail_map.insert("message".to_string(), Value::String(hint_str.clone()));
        detail_map.insert("retriable".to_string(), Value::Bool(false));
        Self {
            // -32603 (Internal error) — the daemon-side mapping uses
            // the same code so wire parity holds across both surfaces.
            code: -32603,
            message: format!("ONNX Runtime not found: {hint_str}"),
            kind: CODE_ONNX_RUNTIME_MISSING.to_string(),
            retryable: false,
            retry_after_ms: None,
            details: Some(Value::Object(detail_map)),
        }
    }

    pub fn deadline_exceeded(tool: &str, deadline_ms: u64, retry_delay_ms: u64) -> Self {
        let mut detail_map = Map::new();
        detail_map.insert("tool".to_string(), Value::String(tool.to_string()));
        detail_map.insert(
            "deadline_ms".to_string(),
            Value::Number(Number::from(deadline_ms)),
        );

        Self {
            code: -32000,
            message: format!("Tool '{tool}' exceeded deadline of {deadline_ms}ms"),
            kind: KIND_DEADLINE_EXCEEDED.to_string(),
            retryable: true,
            retry_after_ms: Some(retry_delay_ms),
            details: Some(Value::Object(detail_map)),
        }
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.kind)
    }
}

impl std::error::Error for RpcError {}
