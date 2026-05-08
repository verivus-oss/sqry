//! Convert [`DaemonError`] to rmcp [`McpError`] using the SAME canonical
//! envelope shape as standalone `sqry-mcp` (`sqry-mcp/src/server.rs`'s
//! `rpc_error_to_mcp`).
//!
//! Every emitted envelope is
//! `{kind, retryable, retry_after_ms, details}` with daemon-specific
//! fields placed under `details`, per Codex iter-2 §O.3 MCP wire
//! parity. This is what lets a client consume daemon-path and
//! direct-path MCP responses with a single parser.
//!
//! Call-site awareness: `ToolTimeout` carries a `details.tool` slot
//! that is `null` when the error is constructed without a tool name in
//! scope. The MCP host (`DaemonMcpHandler::call_tool`, Phase 8c U8)
//! uses [`daemon_err_to_mcp_with_tool`] so the method name from the
//! inbound `tools/call` populates that slot in a SINGLE pass (no
//! post-hoc JSON mutation — Codex iter-3 NIT-1 contract).
//!
//! The JSON-RPC path already produces an equivalent payload via
//! [`crate::error::DaemonError::error_data`]. This module is the MCP
//! twin; any field change on one side must be mirrored on the other.
//!
//! # Wire parity with standalone sqry-mcp
//!
//! The outer 4-key envelope `{kind, retryable, retry_after_ms, details}`
//! matches standalone sqry-mcp's `rpc_error_to_mcp` at
//! `sqry-mcp/src/server.rs:1329-1343` exactly. Differences:
//!
//! - **`ToolTimeout`** adds `details.root` (the workspace path) which
//!   standalone's `RpcError::deadline_exceeded` does not include — the
//!   daemon serves multiple workspaces so this context is useful;
//!   MCP clients that parse by `kind` ignore it.
//! - **`ToolTimeout.details.tool`** is populated by
//!   [`daemon_err_to_mcp_with_tool`]`(e, tool_name)` at the call site;
//!   the non-site-aware [`daemon_err_to_mcp`] emits `null` placeholder.
//! - **Text-payload parity:** [`crate::mcp_host::DaemonMcpHandler::call_tool`]
//!   renders `content[0].text` via `serde_json::to_string_pretty(&payload)`
//!   (matching standalone's `success_result` at
//!   `sqry-mcp/src/server.rs:355-360`), so `content[0].text` is
//!   byte-identical across daemon-hosted and standalone modes.

use std::path::Path;

use rmcp::ErrorData as McpError;
use serde_json::{Value, json};
use sqry_nl::NlError;

use crate::error::DaemonError;

// Shared kind constants — mirrored from sqry-mcp's `RpcError` kinds for
// cross-path parity. If sqry-mcp renames these, update the daemon side
// to match (wire parity is a co-ordinated contract).
const KIND_DEADLINE_EXCEEDED: &str = "deadline_exceeded";
const KIND_VALIDATION_ERROR: &str = "validation_error";
const KIND_WORKSPACE_NOT_READY: &str = "workspace_not_ready";
const KIND_WORKSPACE_STALE_EXPIRED: &str = "workspace_stale_expired";
/// SGA04 Gate-A major #5 — distinct kind tag for the
/// path-policy / compatibility error class. Mirrors sqry-mcp's
/// `RpcError::workspace_incompatible_graph` envelope name (a
/// co-ordinated wire contract).
const KIND_WORKSPACE_INCOMPATIBLE_GRAPH: &str = "workspace_incompatible_graph";
const KIND_INTERNAL: &str = "internal";
/// NL08 — kind tag for the ONNX-Runtime-missing condition. Mirrored
/// across daemon-host and standalone sqry-mcp envelopes.
pub(crate) const KIND_ONNX_RUNTIME_MISSING: &str = "ONNX_RUNTIME_MISSING";

/// NL08 — convert an [`NlError::OnnxRuntimeMissing`] to a canonical
/// MCP envelope using `internal_error` (-32603) per the design (DAG
/// `[units.NL08]` + design §8). Mirrors the standalone sqry-mcp
/// `RpcError::onnx_runtime_missing` envelope shape so daemon-hosted
/// and standalone responses parse the same way.
///
/// Returns `Some(McpError)` when the input is the missing-dylib
/// variant; returns `None` otherwise so the caller can fall through to
/// the generic `daemon_err_to_mcp` mapping (typically wrapped as
/// `WorkspaceBuildFailed`).
#[must_use]
pub fn try_onnx_runtime_missing_to_mcp(err: &NlError) -> Option<McpError> {
    match err {
        NlError::OnnxRuntimeMissing { hint } => Some(onnx_runtime_missing_mcp(hint)),
        _ => None,
    }
}

/// Build the canonical MCP envelope for the missing-dylib condition.
///
/// Wire shape (placed inside `details`, mirroring the rest of the
/// daemon's 4-key envelope):
///
/// ```json
/// {
///   "kind": "ONNX_RUNTIME_MISSING",
///   "retryable": false,
///   "retry_after_ms": null,
///   "details": {
///     "code": "ONNX_RUNTIME_MISSING",
///     "message": "<hint>",
///     "retriable": false
///   }
/// }
/// ```
///
/// `McpError::internal_error` carries IPC code -32603, matching the
/// daemon's existing internal-error code per design §8.
#[must_use]
pub fn onnx_runtime_missing_mcp(hint: &str) -> McpError {
    let data = json!({
        "kind": KIND_ONNX_RUNTIME_MISSING,
        "retryable": false,
        "retry_after_ms": Value::Null,
        "details": {
            "code": KIND_ONNX_RUNTIME_MISSING,
            "message": hint,
            "retriable": false,
        },
    });
    McpError::internal_error(format!("ONNX Runtime not found: {hint}"), Some(data))
}

/// Build the canonical `ToolTimeout` MCP envelope — single source of
/// truth. `tool_name` is `None` when called without call-site context
/// (the envelope emits `details.tool: null`) and `Some(name)` when
/// populated by the MCP `call_tool` wrapper.
fn mcp_timeout_error(root: &Path, secs: u64, tool_name: Option<&str>) -> McpError {
    let deadline_ms = secs.saturating_mul(1000);
    let tool_value = match tool_name {
        Some(name) => Value::String(name.to_owned()),
        None => Value::Null,
    };
    let data = json!({
        "kind": KIND_DEADLINE_EXCEEDED,
        "retryable": true,
        "retry_after_ms": 1000,
        "details": {
            "tool": tool_value,
            "deadline_ms": deadline_ms,
            "root": root.display().to_string(),
        }
    });
    McpError::internal_error(
        format!(
            "tool invocation exceeded deadline of {deadline_ms}ms for workspace {}",
            root.display()
        ),
        Some(data),
    )
}

/// Convert [`DaemonError`] to [`McpError`] using the canonical 4-key
/// envelope.
///
/// Errors with `kind: "deadline_exceeded"` have `details.tool: null`
/// unless the caller has a tool name in scope — use
/// [`daemon_err_to_mcp_with_tool`] for call-site-aware mapping.
pub fn daemon_err_to_mcp(e: DaemonError) -> McpError {
    match e {
        DaemonError::ToolTimeout { root, secs, .. } => mcp_timeout_error(&root, secs, None),

        DaemonError::InvalidArgument { reason } => {
            let data = json!({
                "kind": KIND_VALIDATION_ERROR,
                "retryable": false,
                "retry_after_ms": Value::Null,
                "details": { "reason": reason.clone() },
            });
            McpError::invalid_params(format!("invalid argument: {reason}"), Some(data))
        }

        DaemonError::Internal(err) => {
            let data = json!({
                "kind": KIND_INTERNAL,
                "retryable": false,
                "retry_after_ms": Value::Null,
                "details": Value::Null,
            });
            McpError::internal_error(format!("internal error: {err}"), Some(data))
        }

        DaemonError::WorkspaceBuildFailed { root, reason } => {
            let data = json!({
                "kind": KIND_WORKSPACE_NOT_READY,
                "retryable": true,
                "retry_after_ms": 2000,
                "details": {
                    "root": root.display().to_string(),
                    "reason": reason.clone(),
                },
            });
            McpError::internal_error(format!("workspace build failed: {reason}"), Some(data))
        }

        DaemonError::WorkspaceStaleExpired {
            root,
            age_hours,
            cap_hours,
            last_good_at,
            last_error,
        } => {
            let last_good_at_str = last_good_at.map(|t| {
                chrono::DateTime::<chrono::Utc>::from(t)
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            });
            let data = json!({
                "kind": KIND_WORKSPACE_STALE_EXPIRED,
                "retryable": false,
                "retry_after_ms": Value::Null,
                "details": {
                    "root": root.display().to_string(),
                    "age_hours": age_hours,
                    "cap_hours": cap_hours,
                    "last_good_at": last_good_at_str,
                    "last_error": last_error,
                },
            });
            McpError::internal_error(
                format!(
                    "workspace {} stale ({age_hours}h > {cap_hours}h cap)",
                    root.display()
                ),
                Some(data),
            )
        }

        // SGA04 Gate-A major #5 — keep `WorkspaceIncompatibleGraph`
        // distinct from the catch-all so MCP clients receive a
        // dedicated `kind` tag and the `reason` string is preserved
        // verbatim in `details.reason` (no collapse to "Internal").
        DaemonError::WorkspaceIncompatibleGraph { root, reason } => {
            let data = json!({
                "kind": KIND_WORKSPACE_INCOMPATIBLE_GRAPH,
                "retryable": false,
                "retry_after_ms": Value::Null,
                "details": {
                    "root": root.display().to_string(),
                    "reason": reason.clone(),
                },
            });
            McpError::internal_error(
                format!(
                    "workspace {} graph is incompatible with this binary: {reason}",
                    root.display()
                ),
                Some(data),
            )
        }

        // Server-lifecycle errors (Config, Io, MemoryBudgetExceeded,
        // WorkspaceEvicted). If these reach MCP the daemon is likely
        // shutting down or the workspace raced; map generically.
        other => {
            let data = json!({
                "kind": KIND_INTERNAL,
                "retryable": false,
                "retry_after_ms": Value::Null,
                "details": Value::Null,
            });
            McpError::internal_error(format!("{other}"), Some(data))
        }
    }
}

/// Call-site-aware wrapper that builds the tool name into
/// `details.tool` for [`DaemonError::ToolTimeout`]. For all other
/// variants this is equivalent to [`daemon_err_to_mcp`].
pub fn daemon_err_to_mcp_with_tool(e: DaemonError, tool_name: &str) -> McpError {
    match e {
        DaemonError::ToolTimeout { root, secs, .. } => {
            mcp_timeout_error(&root, secs, Some(tool_name))
        }
        other => daemon_err_to_mcp(other),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn tool_timeout_envelope_has_canonical_shape() {
        let err = DaemonError::ToolTimeout {
            root: PathBuf::from("/tmp/ws"),
            secs: 60,
            deadline_ms: 60_000,
        };
        let mcp_err = daemon_err_to_mcp(err);
        let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
        assert_eq!(data["kind"], KIND_DEADLINE_EXCEEDED);
        assert_eq!(data["retryable"], true);
        assert_eq!(data["retry_after_ms"], 1000);
        let details = data["details"].as_object().unwrap();
        assert!(details["tool"].is_null());
        assert_eq!(details["deadline_ms"], 60_000);
        assert_eq!(details["root"], "/tmp/ws");
    }

    #[test]
    fn tool_timeout_with_tool_populates_details() {
        let err = DaemonError::ToolTimeout {
            root: PathBuf::from("/tmp/ws"),
            secs: 60,
            deadline_ms: 60_000,
        };
        let mcp_err = daemon_err_to_mcp_with_tool(err, "semantic_search");
        let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
        assert_eq!(data["details"]["tool"], "semantic_search");
        // Other fields unchanged:
        assert_eq!(data["details"]["deadline_ms"], 60_000);
        assert_eq!(data["details"]["root"], "/tmp/ws");
        assert_eq!(data["kind"], KIND_DEADLINE_EXCEEDED);
        assert_eq!(data["retryable"], true);
        assert_eq!(data["retry_after_ms"], 1000);
    }

    #[test]
    fn invalid_argument_envelope_canonical() {
        let err = DaemonError::InvalidArgument {
            reason: "missing path".into(),
        };
        let mcp_err = daemon_err_to_mcp(err);
        let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
        assert_eq!(data["kind"], KIND_VALIDATION_ERROR);
        assert_eq!(data["retryable"], false);
        assert!(data["retry_after_ms"].is_null());
        assert_eq!(data["details"]["reason"], "missing path");
        // `invalid_params` carries the standard JSON-RPC code -32602;
        // verify the rmcp error code matches so wire parity with
        // sqry-mcp's `rpc_error_to_mcp` is preserved.
        assert_eq!(mcp_err.code.0, -32602);
    }

    #[test]
    fn internal_envelope_has_null_details() {
        let err = DaemonError::Internal(anyhow::anyhow!("boom"));
        let mcp_err = daemon_err_to_mcp(err);
        let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
        assert_eq!(data["kind"], KIND_INTERNAL);
        assert_eq!(data["retryable"], false);
        assert!(data["retry_after_ms"].is_null());
        assert!(data["details"].is_null());
        assert!(mcp_err.message.contains("boom"));
    }

    #[test]
    fn workspace_build_failed_envelope() {
        let err = DaemonError::WorkspaceBuildFailed {
            root: PathBuf::from("/repo"),
            reason: "plugin panic".into(),
        };
        let mcp_err = daemon_err_to_mcp(err);
        let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
        assert_eq!(data["kind"], KIND_WORKSPACE_NOT_READY);
        assert_eq!(data["retryable"], true);
        assert_eq!(data["retry_after_ms"], 2000);
        assert_eq!(data["details"]["root"], "/repo");
        assert_eq!(data["details"]["reason"], "plugin panic");
    }

    #[test]
    fn workspace_stale_expired_envelope_with_last_good_emits_rfc3339() {
        use std::time::{Duration, UNIX_EPOCH};
        // 2025-10-09T09:33:20Z — arbitrary past instant.
        let last_good = UNIX_EPOCH + Duration::from_secs(1_760_000_000);
        let err = DaemonError::WorkspaceStaleExpired {
            root: PathBuf::from("/repo"),
            age_hours: 48,
            cap_hours: 24,
            last_good_at: Some(last_good),
            last_error: Some("parse error".into()),
        };
        let mcp_err = daemon_err_to_mcp(err);
        let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
        assert_eq!(data["kind"], KIND_WORKSPACE_STALE_EXPIRED);
        assert_eq!(data["retryable"], false);
        assert!(data["retry_after_ms"].is_null());
        assert_eq!(data["details"]["age_hours"], 48);
        assert_eq!(data["details"]["cap_hours"], 24);
        assert_eq!(data["details"]["last_error"], "parse error");
        let last_good_str = data["details"]["last_good_at"].as_str().unwrap();
        // `to_rfc3339_opts(Secs, true)` always emits UTC-Zulu form.
        assert!(
            last_good_str.ends_with('Z'),
            "expected RFC3339 UTC-Zulu form, got: {last_good_str}"
        );
    }

    #[test]
    fn envelope_has_exactly_four_top_level_keys() {
        use std::collections::BTreeSet;
        let errs = vec![
            DaemonError::ToolTimeout {
                root: PathBuf::from("/"),
                secs: 1,
                deadline_ms: 1000,
            },
            DaemonError::InvalidArgument { reason: "x".into() },
            DaemonError::Internal(anyhow::anyhow!("y")),
            DaemonError::WorkspaceBuildFailed {
                root: PathBuf::from("/repo"),
                reason: "z".into(),
            },
            DaemonError::WorkspaceStaleExpired {
                root: PathBuf::from("/repo"),
                age_hours: 48,
                cap_hours: 24,
                last_good_at: None,
                last_error: None,
            },
        ];
        let expected: BTreeSet<String> = ["kind", "retryable", "retry_after_ms", "details"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        for err in errs {
            let label = format!("{err:?}");
            let mcp_err = daemon_err_to_mcp(err);
            let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
            let keys: BTreeSet<String> = data.keys().cloned().collect();
            assert_eq!(
                keys, expected,
                "envelope for {label} must be exactly the 4 canonical keys"
            );
        }
    }

    #[test]
    fn server_lifecycle_errors_map_to_internal_kind() {
        // `MemoryBudgetExceeded` / `WorkspaceEvicted` can only reach
        // the MCP host during shutdown races. They must still map to a
        // canonical 4-key envelope so clients don't need a separate
        // parser; the fallback arm is `KIND_INTERNAL` with null
        // details.
        let errs = [
            DaemonError::MemoryBudgetExceeded {
                limit_bytes: 1,
                current_bytes: 0,
                reserved_bytes: 0,
                retained_bytes: 0,
                requested_bytes: 2,
            },
            DaemonError::WorkspaceEvicted {
                root: PathBuf::from("/repo"),
            },
        ];
        for err in errs {
            let mcp_err = daemon_err_to_mcp(err);
            let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
            assert_eq!(data["kind"], KIND_INTERNAL);
            assert!(data["details"].is_null());
        }
    }
}
