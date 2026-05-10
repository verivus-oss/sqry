use serde_json::{Map, Number, Value};
use std::fmt;

const KIND_VALIDATION: &str = "validation_error";
const KIND_DEADLINE_EXCEEDED: &str = "deadline_exceeded";
/// NL08 — code returned in the structured MCP envelope for the
/// "ONNX Runtime not installed" condition. Wire-stable; consumers may
/// pattern-match on this string.
pub const CODE_ONNX_RUNTIME_MISSING: &str = "ONNX_RUNTIME_MISSING";

/// Wire-stable `kind` tag for the cost-gate rejection
/// (`B_cost_gate.md` §3 + `00_contracts.md` §3.CC-2). Surfaced both
/// from the standalone `sqry-mcp` `RpcError::query_too_broad`
/// constructor and the daemon-hosted `DaemonError::QueryTooBroad`
/// match arm. Clients pattern-match on this string to distinguish
/// pre-flight cost rejections (non-retryable; rewrite the query) from
/// deadline_exceeded (transient; retry possible).
///
/// Foundation-only export: this constant is consumed by cluster-B
/// Layer-2 (`IMP-B`). The binary target sees no caller until then,
/// hence `#[allow(dead_code)]`.
#[allow(dead_code)]
pub const KIND_QUERY_TOO_BROAD: &str = "query_too_broad";

/// Documentation URL surfaced in the canonical `details.doc_url`
/// field of the `query_too_broad` envelope. Wire-stable across
/// releases — both the static-estimate path (B) and the runtime-budget
/// path (C, via `details.source = "runtime_budget"`) reference this
/// URL so MCP clients can deep-link straight to the recovery doc.
///
/// Foundation-only export: see [`KIND_QUERY_TOO_BROAD`] note.
#[allow(dead_code)]
pub const QUERY_TOO_BROAD_DOC_URL: &str = "https://docs.verivus.dev/sqry/query-cost-gate";

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

    /// Cost-gate rejection (P0-1 mitigation per `B_cost_gate.md` §3).
    ///
    /// Non-retryable: the caller must rewrite the query (add a scope
    /// filter, anchor the regex, or shorten the operation). The wire
    /// envelope uses JSON-RPC code `-32602` (`invalid_params`) so the
    /// existing `rpc_error_to_mcp` bridge maps it to
    /// `McpError::invalid_params` without modification.
    ///
    /// `details` MUST follow the canonical CC-2 schema documented in
    /// `00_contracts.md` §3.CC-2:
    ///
    /// ```jsonc
    /// {
    ///   "source": "static_estimate" | "runtime_budget",   // discriminator
    ///   "kind":   "query_too_broad",
    ///   "estimated_visited_nodes": <u64?>, // present when source == "static_estimate"
    ///   "limit":  <u64>,                   // node-limit (static) OR row-budget (runtime)
    ///   "examined": <u64?>,                // present when source == "runtime_budget"
    ///   "predicate_shape": <string?>,      // C's Expr::shape_summary() (≤256 bytes)
    ///   "suggested_predicates": [<string>, ...],
    ///   "doc_url": "https://docs.verivus.dev/sqry/query-cost-gate"
    /// }
    /// ```
    ///
    /// Callers are responsible for assembling the `details` value with
    /// the canonical seven keys; this constructor only owns the
    /// outer envelope. The accompanying
    /// [`crate::error::QUERY_TOO_BROAD_DOC_URL`] constant is the
    /// canonical `details.doc_url` value.
    /// Foundation-only export: see [`KIND_QUERY_TOO_BROAD`] note.
    #[must_use]
    #[allow(dead_code)]
    pub fn query_too_broad(message: impl Into<String>, details: Value) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            kind: KIND_QUERY_TOO_BROAD.to_string(),
            retryable: false,
            retry_after_ms: None,
            details: Some(details),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `B_cost_gate.md` §6 row 1: the standalone envelope must surface
    /// `code = -32602`, `kind = "query_too_broad"`, `retryable = false`,
    /// `retry_after_ms = None`, and the caller-supplied `details`
    /// value verbatim. Pinning the four envelope invariants prevents
    /// drift between the standalone (`sqry-mcp`) and the daemon
    /// (`DaemonError::QueryTooBroad`) parity arms.
    #[test]
    fn query_too_broad_envelope_has_canonical_kind_and_code() {
        let details = serde_json::json!({
            "source": "static_estimate",
            "kind": "query_too_broad",
            "limit": 50_000,
            "doc_url": QUERY_TOO_BROAD_DOC_URL,
        });
        let err = RpcError::query_too_broad("rejected: scope filter required", details);
        assert_eq!(err.code, -32602);
        assert_eq!(err.kind, KIND_QUERY_TOO_BROAD);
        assert_eq!(err.kind, "query_too_broad");
        assert!(!err.retryable);
        assert!(err.retry_after_ms.is_none());
        let payload = err.details.expect("query_too_broad must carry details");
        assert_eq!(payload["source"], "static_estimate");
        assert_eq!(payload["kind"], "query_too_broad");
        assert_eq!(payload["limit"], 50_000);
        assert_eq!(payload["doc_url"], QUERY_TOO_BROAD_DOC_URL);
    }
}
