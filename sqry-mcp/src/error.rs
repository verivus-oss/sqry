use serde_json::{Map, Number, Value};
use std::fmt;

const KIND_VALIDATION: &str = "validation_error";
const KIND_DEADLINE_EXCEEDED: &str = "deadline_exceeded";

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
