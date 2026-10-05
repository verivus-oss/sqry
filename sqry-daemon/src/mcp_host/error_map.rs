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

use crate::error::DaemonError;
use sqry_mcp::error::RpcError;

// Shared kind constants — mirrored from sqry-mcp's `RpcError` kinds for
// cross-path parity. If sqry-mcp renames these, update the daemon side
// to match (wire parity is a co-ordinated contract).
const KIND_DEADLINE_EXCEEDED: &str = "deadline_exceeded";
#[cfg(test)]
const KIND_VALIDATION_ERROR: &str = "validation_error";
const KIND_WORKSPACE_NOT_READY: &str = "workspace_not_ready";
const KIND_WORKSPACE_STALE_EXPIRED: &str = "workspace_stale_expired";
const KIND_INTERNAL: &str = "internal";
/// PB-1 — wire-stable kind tag for the cost-gate rejection on the
/// daemon-hosted MCP path. Mirror of `sqry-mcp::error::KIND_QUERY_TOO_BROAD`
/// (and `sqry-daemon::error::KIND_QUERY_TOO_BROAD`) for byte-identical
/// envelopes across the standalone and daemon-hosted MCP transports.
///
/// Source: `B_cost_gate.md` §3 + `00_contracts.md` §3.CC-2. The envelope
/// itself is now the shared constructor's; the tests pin the tag.
#[cfg(test)]
const KIND_QUERY_TOO_BROAD: &str = "query_too_broad";

/// Build the canonical `ToolTimeout` MCP envelope — single source of
/// truth, byte-identical to the standalone
/// `RpcError::deadline_exceeded` envelope (cluster-A iter-2 BLOCKER 1
/// fix; design pack RC-2 / CC-1).
///
/// `tool_name` is `None` when called without call-site context (the
/// envelope emits `details.tool: null`) and `Some(name)` when
/// populated by the MCP `call_tool` wrapper.
///
/// **Wire-identity contract.** The standalone path's
/// `RpcError::deadline_exceeded` emits `retry_after_ms = 500` (the
/// in-process `SqryServer` default) and `details = { tool, deadline_ms }`.
/// The daemon path keeps the workspace root in the message text for
/// operator diagnostics but MUST NOT add it to `details` (that would
/// diverge the wire shape from standalone). The shape is checked by
/// `mcp_host::error_map::tests` plus the iter-1 `RpcError` parity
/// tests in `sqry-mcp/src/error.rs`.
/// The MCP envelope of [`DaemonError::ToolTimeout`]. `deadline_ms` is the
/// variant's own field, used as it is: a deadline below a second (the CPU
/// executor's queue wait is one, with `secs: 0`) must not be reported as
/// `secs * 1000`, which reads 0 ms.
fn mcp_timeout_error(root: &Path, deadline_ms: u64, tool_name: Option<&str>) -> McpError {
    let tool_value = match tool_name {
        Some(name) => Value::String(name.to_owned()),
        None => Value::Null,
    };
    let data = json!({
        "kind": KIND_DEADLINE_EXCEEDED,
        "retryable": true,
        // 500 ms matches the standalone `SqryServer` default
        // (`sqry-mcp/src/server.rs:94`). Operators that need a
        // different value should configure both sides identically.
        "retry_after_ms": 500,
        "details": {
            "tool": tool_value,
            "deadline_ms": deadline_ms,
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

fn internal_daemon_error(err: &anyhow::Error) -> McpError {
    let data = json!({
        "kind": KIND_INTERNAL,
        "retryable": false,
        "retry_after_ms": Value::Null,
        "details": Value::Null,
    });
    McpError::internal_error(format!("internal error: {err}"), Some(data))
}

fn workspace_stale_expired_error(
    message: String,
    root: &Path,
    age_hours: u64,
    cap_hours: u32,
    last_good_at: Option<std::time::SystemTime>,
    last_error: Option<&str>,
) -> McpError {
    let last_good_at_str = last_good_at.map(|t| {
        chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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
    McpError::internal_error(message, Some(data))
}

/// The absent index ([`DaemonError::WorkspaceNotIndexed`]): the same MCP
/// code and kind as the unreadable manifest (`internal_error`,
/// `workspace_not_ready`), not retryable because a retry finds the same
/// absent file until the repair runs, and the absent file and both repairs
/// (the CLI command and the `rebuild_index` tool) under `details`. The
/// message is the variant's Display.
fn workspace_not_indexed_error(
    message: String,
    root: &Path,
    missing_path: &Path,
    repair_command: &str,
) -> McpError {
    let details = json!({
        "root": root.display().to_string(),
        "missing_path": missing_path.display().to_string(),
        "reason": message,
        "repair_command": repair_command,
        "repair_tool": crate::error::REPAIR_TOOL_REBUILD_INDEX,
    });
    let data = json!({
        "kind": KIND_WORKSPACE_NOT_READY,
        "retryable": false,
        "retry_after_ms": Value::Null,
        "details": details,
    });
    McpError::internal_error(message, Some(data))
}

/// An evicted workspace whose bounded reload failed
/// ([`DaemonError::WorkspaceReloadFailed`]): `workspace_not_ready`, not
/// retryable (the query already ran the one reload it is allowed), with the
/// reload's failure under `details.reload_failure`. Before this arm the
/// eviction reached MCP through the catch-all as `kind: internal` with null
/// details. The message is the variant's Display.
fn workspace_reload_failed_error(message: String, root: &Path, reload_failure: &str) -> McpError {
    let details = json!({
        "root": root.display().to_string(),
        "reload_failure": reload_failure,
    });
    let data = json!({
        "kind": KIND_WORKSPACE_NOT_READY,
        "retryable": false,
        "retry_after_ms": Value::Null,
        "details": details,
    });
    McpError::internal_error(message, Some(data))
}

/// Convert [`DaemonError`] to [`McpError`] using the canonical 4-key
/// envelope.
///
/// Errors with `kind: "deadline_exceeded"` have `details.tool: null`
/// unless the caller has a tool name in scope — use
/// [`daemon_err_to_mcp_with_tool`] for call-site-aware mapping.
#[must_use]
pub fn daemon_err_to_mcp(e: DaemonError) -> McpError {
    match e {
        DaemonError::ToolTimeout {
            root, deadline_ms, ..
        } => mcp_timeout_error(&root, deadline_ms, None),

        // The rebuild continues; the data says so and names the read that
        // gives its outcome, as on IPC.
        err @ DaemonError::RebuildOutcomeTimeout { deadline_ms, .. } => McpError::internal_error(
            err.to_string(),
            Some(crate::error::rebuild_outcome_timeout_data(
                deadline_ms,
                Some("rebuild_index"),
            )),
        ),

        // The refusals the standalone server also gives are built through
        // its constructors (`sqry_mcp::error::RpcError`), so the text and
        // the envelope exist once and both hosts answer them byte for byte.
        DaemonError::InvalidArgument { reason } => {
            sqry_mcp::error::rpc_error_to_mcp(RpcError::invalid_argument(reason))
        }

        // Cluster-C iter-3: render the preserved `sqry_mcp::RpcError`
        // through the same selector the standalone path uses
        // (`sqry-mcp/src/server.rs::rpc_error_to_mcp`). This produces
        // a byte-identical wire envelope:
        //   - code -32602 → `McpError::invalid_params`
        //   - any other code → `McpError::internal_error`
        // and the `data` block carries the inner kind/retryable/
        // retry_after_ms/details verbatim.
        DaemonError::RpcErrorPreserved(rpc) => sqry_mcp::error::rpc_error_to_mcp(rpc),

        DaemonError::Internal(err) => internal_daemon_error(&err),

        // A persist refused because the index directory was removed after
        // it took the lock (decision D-i8-6) is a refusal, not a retryable
        // failed build: the envelope the standalone server gives it.
        DaemonError::WorkspaceBuildFailed { root, reason }
            if reason == crate::rebuild::INDEX_REMOVED_DURING_PERSIST
                || reason == crate::rebuild::INDEX_REMOVED_DURING_PERSIST_WAIT =>
        {
            sqry_mcp::error::rpc_error_to_mcp(RpcError::workspace_index_removed(&root, &reason))
        }
        DaemonError::WorkspaceBuildFailed { root, reason } => {
            sqry_mcp::error::rpc_error_to_mcp(RpcError::workspace_build_failed(&root, &reason))
        }

        DaemonError::WorkspaceStaleExpired {
            ref root,
            age_hours,
            cap_hours,
            last_good_at,
            ref last_error,
        } => workspace_stale_expired_error(
            e.to_string(),
            root,
            age_hours,
            cap_hours,
            last_good_at,
            last_error.as_deref(),
        ),

        // SGA04 Gate-A major #5 — keep `WorkspaceIncompatibleGraph`
        // distinct from the catch-all so MCP clients receive a
        // dedicated `kind` tag and the `reason` string is preserved
        // verbatim in `details.reason` (no collapse to "Internal").
        DaemonError::WorkspaceIncompatibleGraph { root, reason } => {
            sqry_mcp::error::rpc_error_to_mcp(RpcError::workspace_incompatible_graph(
                &root, &reason,
            ))
        }

        // PB-1 — pre-flight cost gate rejection. The CC-2 7-key
        // `details` value is supplied by the caller and round-tripped
        // verbatim. Wire envelope (kind / retryable / retry_after_ms /
        // details) is byte-identical to the standalone
        // `RpcError::query_too_broad` shape.
        DaemonError::QueryTooBroad { reason, details } => {
            sqry_mcp::error::rpc_error_to_mcp(RpcError::query_too_broad(reason, details))
        }

        // Surface parity W1 (D5): a refused narrowing rebuild is a
        // precondition failure the caller can act on (run the restore
        // command), so it maps to invalid_params with the ids and the
        // command under `details`, never to the generic Internal shape.
        DaemonError::RebuildWouldNarrowSelection {
            root,
            missing_plugin_ids,
            restore_command,
        } => {
            let message = DaemonError::RebuildWouldNarrowSelection {
                root: root.clone(),
                missing_plugin_ids: missing_plugin_ids.clone(),
                restore_command: restore_command.clone(),
            }
            .to_string();
            let data = json!({
                "kind": crate::error::KIND_REBUILD_WOULD_NARROW_SELECTION,
                "retryable": false,
                "retry_after_ms": Value::Null,
                "details": {
                    "root": crate::error::path_text(&root),
                    "missing_plugin_ids": missing_plugin_ids,
                    "restore_command": restore_command,
                },
            });
            McpError::invalid_params(message, Some(data))
        }

        // Surface parity W4 (W4-D7), S10 (round 7): an unusable expand
        // cache is the standalone server's envelope, built through its
        // constructor (the core's reason for the directory's shape, the MCP
        // remedy that fits where the directory came from, and `details`
        // naming the root, the directory and the origin), so both hosts
        // answer the same request byte for byte.
        DaemonError::RebuildMacroOptionsUnavailable {
            root,
            expand_cache_dir,
            origin,
        } => sqry_mcp::error::rpc_error_to_mcp(
            sqry_mcp::error::RpcError::rebuild_macro_options_unavailable(
                &root,
                &expand_cache_dir,
                origin,
            ),
        ),

        // Surface parity W1 round 2 (D9, D12): an unreadable manifest is
        // not retryable (a retry reads the same bytes); the details name
        // the file and the repair command. Same MCP code as
        // `WorkspaceBuildFailed` (internal_error), no new wire code.
        DaemonError::WorkspaceManifestUnreadable {
            root,
            manifest_path,
            reason,
        } => sqry_mcp::error::rpc_error_to_mcp(RpcError::workspace_manifest_unreadable(
            &root,
            &manifest_path,
            &reason,
        )),

        DaemonError::WorkspaceNotIndexed {
            ref root,
            ref missing_path,
            ref repair_command,
        } => workspace_not_indexed_error(e.to_string(), root, missing_path, repair_command),

        DaemonError::WorkspaceReloadFailed {
            ref root,
            ref reload_failure,
        } => workspace_reload_failed_error(e.to_string(), root, reload_failure),

        // A snapshot that cannot be loaded: `workspace_not_ready`, not
        // retryable (a retry reads the same bytes), naming the file and both
        // repairs, as the absent index does.
        DaemonError::WorkspaceSnapshotUnreadable {
            ref root,
            ref snapshot_path,
            ..
        } => {
            let message = e.to_string();
            let data = json!({
                "kind": KIND_WORKSPACE_NOT_READY,
                "retryable": false,
                "retry_after_ms": Value::Null,
                "details": {
                    "root": crate::error::path_text(root),
                    "snapshot_path": crate::error::path_text(snapshot_path),
                    "reason": message,
                    "repair_command": format!("sqry index --force {}", root.display()),
                    "repair_tool": crate::error::REPAIR_TOOL_REBUILD_INDEX,
                },
            });
            McpError::internal_error(message, Some(data))
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
#[must_use]
pub fn daemon_err_to_mcp_with_tool(e: DaemonError, tool_name: &str) -> McpError {
    match e {
        DaemonError::ToolTimeout {
            root, deadline_ms, ..
        } => mcp_timeout_error(&root, deadline_ms, Some(tool_name)),
        other => daemon_err_to_mcp(other),
    }
}

/// The `rebuild_index` call sites' wrapper: [`daemon_err_to_mcp_with_tool`]
/// for the tool, except that an unusable expand cache echoes the request's
/// own `path` argument, as the caller gave it, in
/// `details.reset_arguments.path` ([`RpcError::with_reset_path`]), as the
/// standalone server answers the same request. The remedy for a recorded
/// directory can then be sent back as it stands; redaction keeps
/// `reset_arguments` as given ([`sqry_mcp::error::redact_mcp_error`]). A
/// requested directory carries no `reset_arguments`, so its envelope is
/// unchanged.
#[must_use]
pub fn daemon_err_to_mcp_for_rebuild_index(e: DaemonError, requested_path: &str) -> McpError {
    match e {
        DaemonError::RebuildMacroOptionsUnavailable {
            root,
            expand_cache_dir,
            origin,
        } => sqry_mcp::error::rpc_error_to_mcp(
            RpcError::rebuild_macro_options_unavailable(&root, &expand_cache_dir, origin)
                .with_reset_path(requested_path),
        ),
        other => daemon_err_to_mcp_with_tool(other, "rebuild_index"),
    }
}

#[cfg(test)]
mod tests {
    /// A deadline below a second keeps its milliseconds on the wire: the
    /// CPU executor's `ToolTimeout { secs: 0, deadline_ms: 30 }` was
    /// reported as `deadline_ms: 0` and "0ms" (from `secs * 1000`), on both
    /// the plain mapping and the call-site wrapper.
    #[test]
    fn a_sub_second_tool_timeout_reports_its_own_deadline_ms() {
        let err = || DaemonError::ToolTimeout {
            root: std::path::PathBuf::from("/repo"),
            secs: 0,
            deadline_ms: 30,
        };
        for (route, mapped) in [
            ("daemon_err_to_mcp", daemon_err_to_mcp(err())),
            (
                "daemon_err_to_mcp_with_tool",
                daemon_err_to_mcp_with_tool(err(), "semantic_search"),
            ),
        ] {
            let data = mapped.data.clone().expect("data");
            assert_eq!(data["details"]["deadline_ms"], json!(30), "{route}: {data}");
            assert!(
                mapped.message.contains("30ms"),
                "{route}: the message names the 30 ms deadline: {}",
                mapped.message
            );
        }
    }

    use std::path::PathBuf;

    use super::*;

    /// The MCP side of `error_data_renders_a_non_utf8_path`: every
    /// path-naming variant maps to an envelope without a panic, and an
    /// envelope that names the path names it lossily. The narrowing arm
    /// built its `details.root` with `json!` of the `PathBuf`, which
    /// panicked on such a root.
    #[cfg(unix)]
    #[test]
    fn every_envelope_renders_a_non_utf8_path() {
        let errors = crate::error::tests::path_bearing_errors_with_a_non_utf8_path();
        let total = errors.len();
        let mut named = 0;
        for err in errors {
            let label = format!("{err:?}");
            let mcp =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| daemon_err_to_mcp(err)))
                    .unwrap_or_else(|_| panic!("daemon_err_to_mcp panicked for {label}"));
            let text = format!("{} {}", mcp.message, mcp.data.unwrap_or_default());
            if text.contains("/repo/") {
                assert!(text.contains('\u{fffd}'), "{label}: {text}");
                named += 1;
            }
        }
        println!("{named} of {total} envelopes name the path");
        assert!(named > 0);
    }

    /// Cluster-A iter-2 BLOCKER 1: the daemon-hosted envelope MUST be
    /// byte-identical to the standalone `RpcError::deadline_exceeded`
    /// envelope. The standalone shape is
    /// `{ kind, retryable, retry_after_ms, details: { tool, deadline_ms } }`
    /// — no `root` field in `details`. This test pins the shape.
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
        assert_eq!(data["retry_after_ms"], 500);
        let details = data["details"].as_object().unwrap();
        assert!(details["tool"].is_null());
        assert_eq!(details["deadline_ms"], 60_000);
        // No `root` in details — would diverge from the standalone shape.
        assert!(
            !details.contains_key("root"),
            "details must not include `root`; the standalone envelope omits it"
        );
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
        assert_eq!(data["details"]["deadline_ms"], 60_000);
        assert_eq!(data["kind"], KIND_DEADLINE_EXCEEDED);
        assert_eq!(data["retryable"], true);
        assert_eq!(data["retry_after_ms"], 500);
    }

    /// Surface parity W1 round 2, verifier pin (battery row V9, deviation
    /// D-12): an unreadable manifest reaches the daemon-hosted MCP wire as
    /// `internal_error` with `kind = workspace_not_ready`, `retryable =
    /// false` (a retry reads the same bytes), no `retry_after_ms`, and
    /// details naming the root, the manifest, the parse reason and the
    /// repair command; the message is the variant's Display verbatim.
    #[test]
    fn unreadable_manifest_envelope_is_not_retryable_and_names_the_repair() {
        let err = DaemonError::WorkspaceManifestUnreadable {
            root: PathBuf::from("/tmp/ws"),
            manifest_path: PathBuf::from("/tmp/ws/.sqry/graph/manifest.json"),
            reason: "missing field `schema_version`".into(),
        };
        let expected_message = err.to_string();
        let mcp_err = daemon_err_to_mcp(err);
        assert_eq!(mcp_err.code.0, -32603);
        assert_eq!(mcp_err.message, expected_message);
        let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
        assert_eq!(data["kind"], KIND_WORKSPACE_NOT_READY);
        assert_eq!(data["retryable"], false);
        assert!(data["retry_after_ms"].is_null());
        assert_eq!(data["details"]["root"], "/tmp/ws");
        assert_eq!(
            data["details"]["manifest_path"],
            "/tmp/ws/.sqry/graph/manifest.json"
        );
        assert_eq!(data["details"]["reason"], "missing field `schema_version`");
        assert_eq!(
            data["details"]["repair_command"],
            "sqry index --force /tmp/ws"
        );
    }

    /// The absent index on the daemon-hosted MCP wire: `internal_error`
    /// (the code the unreadable manifest uses), `workspace_not_ready`, not
    /// retryable, the message the variant's Display, and the absent file
    /// and both repairs under `details`.
    #[test]
    fn not_indexed_envelope_names_the_absent_file_and_both_repairs() {
        let err = DaemonError::workspace_not_indexed(
            Path::new("/tmp/ws"),
            PathBuf::from("/tmp/ws/.sqry/graph/manifest.json"),
        );
        let expected_message = err.to_string();
        let mcp_err = daemon_err_to_mcp(err);
        assert_eq!(mcp_err.code.0, -32603);
        assert_eq!(mcp_err.message, expected_message);
        let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
        assert_eq!(data.len(), 4, "the canonical four keys");
        assert_eq!(data["kind"], KIND_WORKSPACE_NOT_READY);
        assert_eq!(data["retryable"], false);
        assert!(data["retry_after_ms"].is_null());
        assert_eq!(data["details"]["root"], "/tmp/ws");
        assert_eq!(
            data["details"]["missing_path"],
            "/tmp/ws/.sqry/graph/manifest.json"
        );
        assert_eq!(data["details"]["reason"], expected_message);
        assert_eq!(data["details"]["repair_command"], "sqry index /tmp/ws");
        assert_eq!(data["details"]["repair_tool"], "rebuild_index");
    }

    /// A failed reload after an eviction on the daemon-hosted MCP wire:
    /// `workspace_not_ready` with the failure under `details`, where the
    /// bare eviction falls into the catch-all (`internal`, null details).
    #[test]
    fn reload_failed_envelope_carries_the_failure() {
        let err = DaemonError::WorkspaceReloadFailed {
            root: PathBuf::from("/tmp/ws"),
            reload_failure: "workspace /tmp/ws build failed: snapshot load failed".into(),
        };
        let expected_message = err.to_string();
        let mcp_err = daemon_err_to_mcp(err);
        assert_eq!(mcp_err.code.0, -32603);
        assert_eq!(mcp_err.message, expected_message);
        let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
        assert_eq!(data.len(), 4, "the canonical four keys");
        assert_eq!(data["kind"], KIND_WORKSPACE_NOT_READY);
        assert_eq!(data["retryable"], false);
        assert!(data["retry_after_ms"].is_null());
        assert_eq!(data["details"]["root"], "/tmp/ws");
        assert_eq!(
            data["details"]["reload_failure"],
            "workspace /tmp/ws build failed: snapshot load failed"
        );
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

    /// Cluster-C iter-3 regression: a typed `RpcError` validation
    /// failure (e.g. `validate_budget_rows({Some(0)})`) must reach
    /// the daemon-hosted MCP wire as `invalid_params` (-32602) with
    /// the standalone path's exact data shape, not as
    /// `internal_error` (-32603).
    #[test]
    fn rpc_error_preserved_validation_emits_invalid_params() {
        let rpc = sqry_mcp::error::RpcError::validation_with_data(
            "budget_rows must be > 0".to_string(),
            json!({
                "kind": "validation",
                "constraint": "range",
                "field": "budget_rows",
                "min": 1,
                "actual": 0,
            }),
        );
        let err = DaemonError::RpcErrorPreserved(rpc);
        let mcp_err = daemon_err_to_mcp(err);
        // -32602 InvalidParams, NOT -32603 Internal.
        assert_eq!(mcp_err.code.0, -32602);
        let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
        // RpcError.kind survives through the wrapper.
        assert_eq!(data["kind"], "validation_error");
        assert_eq!(data["retryable"], false);
        assert!(data["retry_after_ms"].is_null());
        // The structured details from the standalone path round-trip.
        let details = data["details"].as_object().unwrap();
        assert_eq!(details["field"], "budget_rows");
        assert_eq!(details["constraint"], "range");
        assert_eq!(details["min"], 1);
        assert_eq!(details["actual"], 0);
        assert_eq!(mcp_err.message, "budget_rows must be > 0");
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

    /// Decision D-i8-6 (fifth audit, item 3): the daemon's refusal for an
    /// index removed during the persist, or while the persist waited for
    /// its lock, is not retryable on the daemon-hosted MCP, as on the
    /// standalone server; any other failed build keeps its retryable
    /// envelope.
    #[test]
    fn an_index_removed_during_the_persist_is_not_retryable() {
        for reason in [
            crate::rebuild::INDEX_REMOVED_DURING_PERSIST,
            crate::rebuild::INDEX_REMOVED_DURING_PERSIST_WAIT,
        ] {
            let mcp_err = daemon_err_to_mcp(DaemonError::WorkspaceBuildFailed {
                root: PathBuf::from("/repo"),
                reason: reason.to_string(),
            });
            let data = mcp_err.data.as_ref().unwrap().as_object().unwrap();
            assert_eq!(data["kind"], KIND_WORKSPACE_NOT_READY, "{reason}");
            assert_eq!(data["retryable"], false, "{reason}");
            assert!(data["retry_after_ms"].is_null(), "{reason}");
            assert_eq!(data["details"]["root"], "/repo");
        }
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

    /// `B_cost_gate.md` §6 + `00_contracts.md` §3.CC-2: the daemon
    /// envelope for a cost-gate rejection MUST be the canonical 4-key
    /// shape (`kind`, `retryable`, `retry_after_ms`, `details`) with
    /// `kind == "query_too_broad"`, `retryable == false`,
    /// `retry_after_ms == null`, and `details` round-tripping the
    /// caller-supplied CC-2 7-key payload verbatim. Pinning this
    /// here keeps the standalone (`sqry-mcp::RpcError::query_too_broad`)
    /// and daemon paths byte-identical on the wire.
    #[test]
    fn query_too_broad_envelope_has_canonical_4_key_shape() {
        let details = serde_json::json!({
            "source": "static_estimate",
            "kind": "query_too_broad",
            "estimated_visited_nodes": 312_487,
            "limit": 312_487,
            "predicate_shape": "name~=/.*_set$/",
            "suggested_predicates": ["kind", "lang", "language", "path", "file"],
            "doc_url": "https://docs.verivus.dev/sqry/query-cost-gate",
        });
        let err = DaemonError::QueryTooBroad {
            reason: "rejected: predicate `name~=/.*_set$/` is unbounded".into(),
            details: details.clone(),
        };
        let mcp_err = daemon_err_to_mcp(err);
        let data = mcp_err
            .data
            .as_ref()
            .expect("QueryTooBroad must carry data")
            .as_object()
            .expect("data must be a JSON object");

        let keys: std::collections::BTreeSet<&str> = data.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> =
            ["kind", "retryable", "retry_after_ms", "details"]
                .iter()
                .copied()
                .collect();
        assert_eq!(
            keys, expected,
            "envelope must have exactly the 4 canonical keys, got: {keys:?}"
        );
        assert_eq!(data["kind"], KIND_QUERY_TOO_BROAD);
        assert_eq!(data["kind"], "query_too_broad");
        assert_eq!(data["retryable"], false);
        assert!(data["retry_after_ms"].is_null());

        // `details` must round-trip verbatim — the daemon does not
        // mutate the caller-supplied CC-2 7-key payload.
        assert_eq!(data["details"], details);
        // Quick spot-check on each canonical CC-2 key.
        assert_eq!(data["details"]["source"], "static_estimate");
        assert_eq!(data["details"]["kind"], "query_too_broad");
        assert_eq!(data["details"]["limit"], 312_487);
        assert!(data["details"]["suggested_predicates"].is_array());
        assert_eq!(
            data["details"]["doc_url"],
            "https://docs.verivus.dev/sqry/query-cost-gate"
        );
    }
}
