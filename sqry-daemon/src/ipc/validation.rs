//! JSON-RPC 2.0 request validator.
//!
//! Separates transport-level parse/validate failures (which produce
//! `-32700`/`-32600` responses with `id: null`) from method-level
//! dispatch errors (`-32601`/`-32602`/daemon-specific codes).
//!
//! The validator operates on `serde_json::Value` so the batch router
//! can parse the top-level array/object split once and pass individual
//! elements through here. See [`super::router`] for the batch flow.

use serde_json::json;
use thiserror::Error;

use super::protocol::{JsonRpcRequest, JsonRpcResponse};

/// Transport-level validation failure.
#[derive(Debug, Error)]
pub enum ValidationError {
    /// `-32700 Parse error` — the raw frame bytes did not deserialise
    /// as JSON. The connection is closed after the response is sent
    /// because the stream is in an indeterminate state.
    #[error("parse error: {0}")]
    ParseError(#[from] serde_json::Error),

    /// `-32600 Invalid Request` — JSON parsed but violated the
    /// JSON-RPC 2.0 request object shape rules.
    #[error("invalid request: {reason}")]
    InvalidRequest {
        reason: &'static str,
        context: Option<String>,
    },
}

impl ValidationError {
    /// Whether this error should close the connection immediately
    /// after the response frame is written. Parse errors leave the
    /// framing state indeterminate; invalid-request errors do not.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::ParseError(_))
    }

    /// Translate to the wire-level JSON-RPC response. Both variants
    /// carry `id: null` per spec.
    #[must_use]
    pub fn into_jsonrpc_response(self) -> JsonRpcResponse {
        match self {
            Self::ParseError(e) => JsonRpcResponse::error(
                None,
                -32700,
                "Parse error",
                Some(json!({ "reason": e.to_string() })),
            ),
            Self::InvalidRequest { reason, context } => {
                let mut data = json!({ "reason": reason });
                if let Some(ctx) = context {
                    data["context"] = serde_json::Value::String(ctx);
                }
                JsonRpcResponse::error(None, -32600, "Invalid Request", Some(data))
            }
        }
    }
}

/// Validate a single request `Value` and convert it into a typed
/// [`JsonRpcRequest`]. The caller is responsible for the outer
/// batch-vs-single split and for producing the top-level parse-error
/// response on failing `serde_json::from_slice`.
///
/// Rejects at `-32600`:
/// - root value not an object
/// - missing or non-string `jsonrpc` field
/// - `jsonrpc` value not exactly `"2.0"`
/// - missing or empty `method`
/// - `id` that is not `null`, a string, an `i64`, or a `u64`
///   (explicitly rejects fractional, exponent-form, boolean, object,
///   and array ids)
/// - `params` that is not an object, array, or null
///
/// # Errors
///
/// Returns [`ValidationError::InvalidRequest`] when any JSON-RPC envelope
/// invariant above is violated.
pub fn validate_request_value(value: serde_json::Value) -> Result<JsonRpcRequest, ValidationError> {
    let serde_json::Value::Object(obj) = &value else {
        return Err(ValidationError::InvalidRequest {
            reason: "request must be an object",
            context: None,
        });
    };

    match obj.get("jsonrpc").and_then(|v| v.as_str()) {
        Some("2.0") => {}
        Some(other) => {
            return Err(ValidationError::InvalidRequest {
                reason: "jsonrpc must be exactly \"2.0\"",
                context: Some(other.to_owned()),
            });
        }
        None => {
            return Err(ValidationError::InvalidRequest {
                reason: "missing jsonrpc field",
                context: None,
            });
        }
    }

    match obj.get("method").and_then(|v| v.as_str()) {
        Some(m) if !m.is_empty() => {}
        Some(_) => {
            return Err(ValidationError::InvalidRequest {
                reason: "method must be non-empty",
                context: None,
            });
        }
        None => {
            return Err(ValidationError::InvalidRequest {
                reason: "missing method field",
                context: None,
            });
        }
    }

    if let Some(id) = obj.get("id") {
        let ok = id.is_null() || id.is_string() || id.is_i64() || id.is_u64();
        if !ok {
            return Err(ValidationError::InvalidRequest {
                reason: "id must be null, string, or an integer number \
                         (no fractional parts, no exponent form)",
                context: Some(id.to_string()),
            });
        }
    }

    if let Some(params) = obj.get("params")
        && !(params.is_object() || params.is_array() || params.is_null())
    {
        return Err(ValidationError::InvalidRequest {
            reason: "params must be object, array, or null",
            context: None,
        });
    }

    let req: JsonRpcRequest =
        serde_json::from_value(value).map_err(|e| ValidationError::InvalidRequest {
            reason: "request failed schema decode after validation",
            context: Some(e.to_string()),
        })?;
    Ok(req)
}

/// Build the top-level parse-error response for a frame whose bytes
/// could not be decoded as JSON.
#[must_use]
pub fn parse_error_response(err: serde_json::Error) -> JsonRpcResponse {
    // Round-trip through the `ValidationError` constructor so the
    // response shape (id: null, code: -32700, "Parse error") stays
    // consistent with the batch path.
    ValidationError::ParseError(err).into_jsonrpc_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_code(resp: &JsonRpcResponse) -> i32 {
        match &resp.payload {
            super::super::protocol::JsonRpcPayload::Error { error } => error.code,
            super::super::protocol::JsonRpcPayload::Success { .. } => panic!("not an error"),
        }
    }

    #[test]
    fn valid_request_roundtrips() {
        let v = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "daemon/status",
            "params": {},
        });
        let req = validate_request_value(v).unwrap();
        assert_eq!(req.method, "daemon/status");
    }

    #[test]
    fn missing_jsonrpc_rejected() {
        let v = serde_json::json!({
            "id": 1,
            "method": "x",
        });
        let e = validate_request_value(v).unwrap_err();
        let resp = e.into_jsonrpc_response();
        assert_eq!(err_code(&resp), -32600);
    }

    #[test]
    fn wrong_jsonrpc_version_rejected() {
        let v = serde_json::json!({
            "jsonrpc": "1.0",
            "id": 1,
            "method": "x",
        });
        let resp = validate_request_value(v)
            .unwrap_err()
            .into_jsonrpc_response();
        assert_eq!(err_code(&resp), -32600);
    }

    #[test]
    fn missing_method_rejected() {
        let v = serde_json::json!({"jsonrpc": "2.0", "id": 1});
        let resp = validate_request_value(v)
            .unwrap_err()
            .into_jsonrpc_response();
        assert_eq!(err_code(&resp), -32600);
    }

    #[test]
    fn empty_method_rejected() {
        let v = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": ""});
        let resp = validate_request_value(v)
            .unwrap_err()
            .into_jsonrpc_response();
        assert_eq!(err_code(&resp), -32600);
    }

    #[test]
    fn non_object_root_rejected() {
        let v = serde_json::json!("not an object");
        let resp = validate_request_value(v)
            .unwrap_err()
            .into_jsonrpc_response();
        assert_eq!(err_code(&resp), -32600);
    }

    #[test]
    fn numeric_id_shape_matrix() {
        // Accepted shapes
        for v in [
            serde_json::json!(0_i64),
            serde_json::json!(1_i64),
            serde_json::json!(-1_i64),
            serde_json::json!(i64::MAX),
            serde_json::json!(u64::MAX),
            serde_json::json!("abc"),
            serde_json::Value::Null,
        ] {
            let req = serde_json::json!({
                "jsonrpc": "2.0",
                "id": v,
                "method": "x",
            });
            validate_request_value(req).expect("valid id shape must pass");
        }
        // Rejected shapes
        let fractional = serde_json::Number::from_f64(1.5).unwrap();
        let rejected: &[serde_json::Value] = &[
            serde_json::Value::Number(fractional.clone()),
            serde_json::from_str(r#"1e3"#).unwrap(),
            serde_json::from_str(r#"42.0E0"#).unwrap(),
            serde_json::json!(true),
            serde_json::json!({}),
            serde_json::json!([]),
        ];
        for v in rejected {
            let req = serde_json::json!({
                "jsonrpc": "2.0",
                "id": v,
                "method": "x",
            });
            let resp = validate_request_value(req)
                .unwrap_err()
                .into_jsonrpc_response();
            assert_eq!(err_code(&resp), -32600, "id shape {v:?} should be -32600");
        }
    }

    #[test]
    fn params_must_be_object_array_or_null() {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "x",
            "params": "not-an-object",
        });
        let resp = validate_request_value(req)
            .unwrap_err()
            .into_jsonrpc_response();
        assert_eq!(err_code(&resp), -32600);
    }

    #[test]
    fn parse_error_response_has_id_null_and_32700() {
        let bad = b"{not valid";
        let err = serde_json::from_slice::<serde_json::Value>(bad).unwrap_err();
        let resp = parse_error_response(err);
        assert_eq!(err_code(&resp), -32700);
        assert!(resp.id.is_none());
    }
}
