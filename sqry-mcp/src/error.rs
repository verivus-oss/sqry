use serde_json::{Map, Number, Value};
use std::fmt;

const KIND_VALIDATION: &str = "validation_error";
const KIND_DEADLINE_EXCEEDED: &str = "deadline_exceeded";

/// Wire-stable `kind` tag for the cost-gate rejection
/// (`B_cost_gate.md` §3 + `00_contracts.md` §3.CC-2). Surfaced both
/// from the standalone `sqry-mcp` `RpcError::query_too_broad`
/// constructor and the daemon-hosted `DaemonError::QueryTooBroad`
/// match arm. Clients pattern-match on this string to distinguish
/// pre-flight cost rejections (non-retryable; rewrite the query) from
/// `deadline_exceeded` (transient; retry possible).
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

/// `kind` of a rebuild refused because the expand cache directory its
/// macro options name is not a directory it can use: missing, a file, a
/// dangling link, empty, or not anchorable
/// (`DaemonError::RebuildMacroOptionsUnavailable` on the daemon-hosted MCP;
/// surface parity W4, design W4-D7).
pub const KIND_REBUILD_MACRO_OPTIONS_UNAVAILABLE: &str = "rebuild_macro_options_unavailable";

/// Where the expand cache directory of a refused rebuild came from, which
/// decides the remedy: a directory the request named is fixed by naming one
/// that exists (dropping the record would not help), one the index manifest
/// records also by dropping the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpandCacheOrigin {
    /// The request named it (`expand_cache`).
    Requested,
    /// The index manifest records it, and the request kept it.
    Recorded,
}

impl ExpandCacheOrigin {
    /// The origin of the directory a refusal of `request` names: an
    /// explicit `expand_cache` replaces the record, so the refused
    /// directory is the requested one; otherwise it is the recorded one.
    #[must_use]
    pub fn of(request: &sqry_core::graph::unified::build::MacroOptionsRequest) -> Self {
        if request.expand_cache_dir.is_some() {
            Self::Requested
        } else {
            Self::Recorded
        }
    }

    /// Stable wire spelling, for `details.origin`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Recorded => "recorded",
        }
    }
}

/// `kind` of a workspace that cannot be brought up: a failed build, or a
/// manifest that cannot be read (`DaemonError::WorkspaceBuildFailed` and
/// `DaemonError::WorkspaceManifestUnreadable` on the daemon-hosted MCP).
pub const KIND_WORKSPACE_NOT_READY: &str = "workspace_not_ready";

/// `kind` of a manifest naming a plugin id this binary did not compile
/// (`DaemonError::WorkspaceIncompatibleGraph` on the daemon-hosted MCP).
pub const KIND_WORKSPACE_INCOMPATIBLE_GRAPH: &str = "workspace_incompatible_graph";

/// The `details` key of the `rebuild_index` arguments that drop a recorded
/// expand cache ([`RpcError::rebuild_macro_options_unavailable`]). It holds
/// only the request's own arguments ([`RpcError::with_reset_path`]) and two
/// booleans, so [`redact_mcp_error`] leaves it as the caller sent it.
const RESET_ARGUMENTS: &str = "reset_arguments";

/// Render `err` and every cause in its chain on one line, as `{:#}` does,
/// skipping a cause whose text the line already carries (an error whose
/// `Display` names its source would otherwise print it twice).
///
/// Every place that turns an `anyhow::Error` into client-visible text uses
/// this, so a context added on the way up (`.context("Failed to ...")`)
/// can never hide the refusal it wraps behind its own outer message.
#[must_use]
pub fn render_error_chain(err: &anyhow::Error) -> String {
    let mut rendered = String::new();
    for cause in err.chain() {
        let text = cause.to_string();
        if text.is_empty() || rendered.contains(&text) {
            continue;
        }
        if !rendered.is_empty() {
            rendered.push_str(": ");
        }
        rendered.push_str(&text);
    }
    rendered
}

/// A path as JSON text. A path that is not valid UTF-8 is rendered lossily
/// rather than failing the error it describes.
fn path_text(path: &std::path::Path) -> String {
    path.display().to_string()
}

/// Redact an error response the way a successful response is redacted:
/// the message as a string and the whole `data` object (`kind`, `details`
/// and every path a refusal names, `root`, `expand_cache_dir`,
/// `manifest_path`, the reason text). The standalone server sends every
/// error through here: its tool wrapper redacts what a tool returns with
/// the redactor bound to the request's workspace (a path under it reads
/// `<workspace>`, as in a success), and its `call_tool`, `get_prompt` and
/// `read_resource` boundaries redact everything with the server's own
/// redactor, which covers the errors raised before a workspace is bound.
/// Public so another MCP host serving the same tools redacts its errors the
/// same way. `None` (no redactor configured) returns `err` unchanged;
/// redacting an already redacted error changes nothing (a placeholder is
/// not a path). Two things are kept as they are: values that are not paths
/// (the redactor rewrites only paths, so `details.source` keeps
/// `static_estimate` or `runtime_budget`), and `details.reset_arguments`,
/// which echoes only the caller's own arguments so it can be sent back
/// ([`RpcError::with_reset_path`]).
#[must_use]
pub fn redact_mcp_error(
    redactor: Option<&sqry_mcp_redaction::Redactor>,
    mut err: rmcp::ErrorData,
) -> rmcp::ErrorData {
    let Some(redactor) = redactor else {
        return err;
    };
    let mut message = Value::String(err.message.into_owned());
    redactor.redact(&mut message);
    err.message = match message {
        Value::String(redacted) => redacted,
        other => other.to_string(),
    }
    .into();
    if let Some(data) = err.data.as_mut() {
        let reset = data
            .get_mut("details")
            .and_then(Value::as_object_mut)
            .and_then(|details| details.remove(RESET_ARGUMENTS));
        redactor.redact(data);
        if let Some(reset) = reset
            && let Some(details) = data.get_mut("details").and_then(Value::as_object_mut)
        {
            details.insert(RESET_ARGUMENTS.to_string(), reset);
        }
    }
    err
}

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

    /// A request argument refused before anything was done, in the
    /// envelope the daemon-hosted MCP gives `DaemonError::InvalidArgument`:
    /// code `-32602`, message `invalid argument: <reason>`, `details.reason`.
    #[must_use]
    pub fn invalid_argument(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            code: -32602,
            message: format!("invalid argument: {reason}"),
            kind: KIND_VALIDATION.to_string(),
            retryable: false,
            retry_after_ms: None,
            details: Some(serde_json::json!({ "reason": reason })),
        }
    }

    /// Macro arguments (`cfg_flags`, `expand_cache`, `reset_macro_options`)
    /// given beside `force=false` when a graph already exists at `root`:
    /// no build would honour them, so they are refused rather than dropped
    /// (surface parity W4, W4-D8). One message, true on both MCP hosts: the
    /// standalone server only meets an index on disk, the daemon-hosted one
    /// also a workspace it holds in memory with no index on disk (F3), and
    /// the text names both. It is the one constructor for the refusal: the
    /// standalone `rebuild_index` and the daemon-hosted one both build
    /// through it, so both hosts answer the shared case (an index on disk)
    /// byte for byte.
    #[must_use]
    pub fn macro_options_need_force(root: &std::path::Path) -> Self {
        Self::invalid_argument(format!(
            "rebuild_index: cfg_flags, expand_cache and reset_macro_options need force=true \
             when a graph already exists at {} (an index on disk or a workspace loaded in the \
             daemon); nothing was built",
            root.display()
        ))
    }

    /// A rebuild of `root` refused because the expand cache directory its
    /// macro options name (`expand_cache_dir`, anchored to the directory
    /// being indexed) is not a directory it can use (surface parity W4,
    /// design W4-D7; `DaemonError::RebuildMacroOptionsUnavailable` on the
    /// daemon-hosted MCP). Code `-32602`, not retryable.
    ///
    /// The reason is the core's, chosen by the directory's shape
    /// ([`sqry_core::graph::unified::build::expand_cache_missing_reason`]):
    /// a missing directory, a file and a dangling link are "does not exist
    /// or is not a directory", an empty path "names no directory". The
    /// remedy is for an MCP caller and fits `origin`: a requested directory
    /// is fixed by passing one that exists, a recorded one also by
    /// `reset_macro_options`; neither names a CLI command. `details` names
    /// the root, the directory and the origin, and for a recorded directory
    /// `reset_arguments`, the `rebuild_index` arguments that drop the record:
    /// `force` and `reset_macro_options`, and the request's own `path` once
    /// [`Self::with_reset_path`] echoes it. A host chains that call so the
    /// remedy can be sent back as it stands.
    #[must_use]
    pub fn rebuild_macro_options_unavailable(
        root: &std::path::Path,
        expand_cache_dir: &std::path::Path,
        origin: ExpandCacheOrigin,
    ) -> Self {
        let reason =
            sqry_core::graph::unified::build::expand_cache_missing_reason(expand_cache_dir);
        let remedy = match origin {
            ExpandCacheOrigin::Requested => {
                "the request named it: pass expand_cache naming a directory that exists, or omit \
                 expand_cache"
            }
            ExpandCacheOrigin::Recorded => {
                "the index manifest records it: drop the record with reset_macro_options: true \
                 and force: true, or pass expand_cache naming a directory that exists"
            }
        };
        let mut details = serde_json::json!({
            "root": path_text(root),
            "expand_cache_dir": path_text(expand_cache_dir),
            "origin": origin.as_str(),
        });
        if origin == ExpandCacheOrigin::Recorded {
            details[RESET_ARGUMENTS] = serde_json::json!({
                "force": true,
                "reset_macro_options": true,
            });
        }
        Self {
            code: -32602,
            message: format!("rebuild of {} refused: {reason}; {remedy}", root.display()),
            kind: KIND_REBUILD_MACRO_OPTIONS_UNAVAILABLE.to_string(),
            retryable: false,
            retry_after_ms: None,
            details: Some(details),
        }
    }

    /// Echo `requested_path`, the refused request's own `path` argument as
    /// the request gave it, in `details.reset_arguments.path`, so the remedy
    /// can be sent back as it stands. It is the caller's own input, which the
    /// caller already knows, so [`redact_mcp_error`] keeps it as given; the
    /// canonical root it replaces was redacted to a placeholder that names
    /// no workspace, which made the remedy unusable under the default
    /// preset. A refusal with no `reset_arguments` (a requested directory,
    /// or any other refusal) is returned unchanged.
    #[must_use]
    pub fn with_reset_path(mut self, requested_path: &str) -> Self {
        if let Some(reset) = self
            .details
            .as_mut()
            .and_then(|details| details.get_mut(RESET_ARGUMENTS))
            .and_then(Value::as_object_mut)
        {
            reset.insert(
                "path".to_string(),
                Value::String(requested_path.to_string()),
            );
        }
        self
    }

    /// `rebuild_index` with `force=false` refused because an index already
    /// exists at an ancestor of `requested` in the same project: a nested
    /// index would index the same files twice (cluster E, design E.3, which
    /// makes `force` the MCP opt-in). Code `-32602`; `details` name the
    /// requested directory, the ancestor's graph directory and the project
    /// boundary. It is the one constructor for this refusal: the standalone
    /// `rebuild_index` and the daemon-hosted one both check for an ancestor
    /// index and refuse through it.
    #[must_use]
    pub fn nested_index_refused(
        requested: &std::path::Path,
        ancestor_graph: &std::path::Path,
        boundary: &std::path::Path,
    ) -> Self {
        let reason = format!(
            "rebuild_index: refusing to build a nested index at {}: an index already exists at \
             {} for the project at {}, so its files would be indexed twice; pass force: true to \
             build the nested index anyway, or rebuild the project at {}; nothing was built",
            requested.display(),
            ancestor_graph.display(),
            boundary.display(),
            boundary.display()
        );
        Self {
            code: -32602,
            message: format!("invalid argument: {reason}"),
            kind: KIND_VALIDATION.to_string(),
            retryable: false,
            retry_after_ms: None,
            details: Some(serde_json::json!({
                "reason": reason,
                "root": path_text(requested),
                "ancestor_graph": path_text(ancestor_graph),
                "boundary": path_text(boundary),
            })),
        }
    }

    /// A request whose workspace cannot be resolved from its `path` (or its
    /// session): the path does not exist, is relative and names nothing
    /// under any root, the last workspace, or the workspace the server
    /// resolves without it, or the session has several roots and no path
    /// picks one. The request's argument is what is refused, so this is an
    /// invalid argument (code `-32602`, `validation_error`), as the
    /// daemon-hosted MCP refuses a missing or relative `path`; `tool`
    /// prefixes the reason and `cause` renders the whole resolution chain.
    /// A client whose own roots cannot be read is not refused through here:
    /// that is an invalid request (`-32600`), not the caller's argument.
    #[must_use]
    pub fn workspace_unresolved(tool: &str, cause: &anyhow::Error) -> Self {
        Self::invalid_argument(format!(
            "{tool}: cannot resolve the workspace for this request: {}",
            render_error_chain(cause)
        ))
    }

    /// A manifest at `root` that exists but cannot be read, in the envelope
    /// the daemon-hosted MCP gives `DaemonError::WorkspaceManifestUnreadable`
    /// (code `-32603`, not retryable, `details` naming the manifest and the
    /// repair command).
    #[must_use]
    pub fn workspace_manifest_unreadable(
        root: &std::path::Path,
        manifest_path: &std::path::Path,
        reason: &str,
    ) -> Self {
        Self {
            code: -32603,
            message: format!(
                "manifest at {} cannot be read ({reason}); repair with: sqry index --force {}",
                manifest_path.display(),
                root.display()
            ),
            kind: KIND_WORKSPACE_NOT_READY.to_string(),
            retryable: false,
            retry_after_ms: None,
            details: Some(serde_json::json!({
                "root": path_text(root),
                "manifest_path": path_text(manifest_path),
                "reason": reason,
                "repair_command": format!("sqry index --force {}", root.display()),
            })),
        }
    }

    /// A manifest at `root` naming a plugin id this binary did not compile,
    /// in the envelope the daemon-hosted MCP gives
    /// `DaemonError::WorkspaceIncompatibleGraph` (code `-32603`, not
    /// retryable, `details.reason` carrying the registry's message).
    #[must_use]
    pub fn workspace_incompatible_graph(root: &std::path::Path, reason: &str) -> Self {
        Self {
            code: -32603,
            message: format!(
                "workspace {} graph is incompatible with this binary: {reason}",
                root.display()
            ),
            kind: KIND_WORKSPACE_INCOMPATIBLE_GRAPH.to_string(),
            retryable: false,
            retry_after_ms: None,
            details: Some(serde_json::json!({
                "root": path_text(root),
                "reason": reason,
            })),
        }
    }

    /// A build of `root` that failed, in the envelope the daemon-hosted MCP
    /// gives `DaemonError::WorkspaceBuildFailed` (code `-32603`, retryable
    /// after 2000 ms, `details` naming the root and the reason).
    #[must_use]
    pub fn workspace_build_failed(root: &std::path::Path, reason: &str) -> Self {
        Self {
            code: -32603,
            message: format!("workspace build failed: {reason}"),
            kind: KIND_WORKSPACE_NOT_READY.to_string(),
            retryable: true,
            retry_after_ms: Some(2000),
            details: Some(serde_json::json!({
                "root": path_text(root),
                "reason": reason,
            })),
        }
    }

    /// A persist of `root` refused because its index directory was removed
    /// after the persist took the index's lock (decision D-i8-6 in
    /// `docs/development/surface-parity/04_PROGRESS-surface-parity.md`):
    /// nothing was written, and retrying would only recreate an index the
    /// user removed. Code `-32603`, kind `workspace_not_ready`, not
    /// retryable, `details` naming the root and the reason; the
    /// daemon-hosted MCP gives the same envelope for the daemon's refusal.
    #[must_use]
    pub fn workspace_index_removed(root: &std::path::Path, reason: &str) -> Self {
        Self {
            code: -32603,
            message: format!("rebuild of {} refused: {reason}", root.display()),
            kind: KIND_WORKSPACE_NOT_READY.to_string(),
            retryable: false,
            retry_after_ms: None,
            details: Some(serde_json::json!({
                "root": path_text(root),
                "reason": reason,
            })),
        }
    }

    #[must_use]
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

/// The MCP wire error for an [`RpcError`]: code `-32602` is
/// `invalid_params`, every other code `internal_error`, and `data` is the
/// canonical `{kind, retryable, retry_after_ms, details}` envelope. Public
/// so a test (or another host) can derive the exact response the standalone
/// server sends for a refusal without running the transport.
#[must_use]
pub fn rpc_error_to_mcp(err: RpcError) -> rmcp::ErrorData {
    let data = serde_json::json!({
        "kind": err.kind,
        "retryable": err.retryable,
        "retry_after_ms": err.retry_after_ms,
        "details": err.details,
    });

    // Map error codes: -32602 = invalid params, everything else = internal error
    match err.code {
        -32602 => rmcp::ErrorData::invalid_params(err.message, Some(data)),
        _ => rmcp::ErrorData::internal_error(err.message, Some(data)),
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

    /// S10 (round 7): the reason is the core's, by shape, and the remedy
    /// fits the origin and an MCP caller: a requested directory is fixed by
    /// one that exists, a recorded one also by the reset, whose arguments
    /// `details` carries; neither names a CLI command.
    #[test]
    fn rebuild_macro_options_unavailable_fits_the_origin() {
        let root = std::path::Path::new("/ws");
        let requested = RpcError::rebuild_macro_options_unavailable(
            root,
            std::path::Path::new("/ws/gone"),
            ExpandCacheOrigin::Requested,
        );
        assert_eq!(requested.code, -32602);
        assert_eq!(requested.kind, KIND_REBUILD_MACRO_OPTIONS_UNAVAILABLE);
        assert!(!requested.retryable && requested.retry_after_ms.is_none());
        assert_eq!(
            requested.message,
            "rebuild of /ws refused: expand cache directory /ws/gone does not exist or is not a \
             directory; the request named it: pass expand_cache naming a directory that exists, \
             or omit expand_cache"
        );
        assert_eq!(
            requested.details,
            Some(serde_json::json!({
                "root": "/ws",
                "expand_cache_dir": "/ws/gone",
                "origin": "requested",
            }))
        );

        let recorded = RpcError::rebuild_macro_options_unavailable(
            root,
            std::path::Path::new(""),
            ExpandCacheOrigin::Recorded,
        );
        assert_eq!(
            recorded.message,
            "rebuild of /ws refused: the expand cache directory is empty, so it names no \
             directory; the index manifest records it: drop the record with \
             reset_macro_options: true and force: true, or pass expand_cache naming a directory \
             that exists"
        );
        assert_eq!(
            recorded
                .details
                .as_ref()
                .map(|details| details["reset_arguments"].clone()),
            Some(serde_json::json!({ "force": true, "reset_macro_options": true })),
            "no path until the host echoes the request's"
        );
        let echoed = recorded.clone().with_reset_path("ws/../ws");
        assert_eq!(
            echoed
                .details
                .as_ref()
                .map(|details| details["reset_arguments"].clone()),
            Some(
                serde_json::json!({ "path": "ws/../ws", "force": true, "reset_macro_options": true })
            ),
            "the request's path as it gave it, not the root"
        );
        assert_eq!(
            requested.clone().with_reset_path("/ws").details,
            requested.details,
            "a requested directory carries no reset arguments to echo into"
        );
        for err in [&requested, &recorded] {
            assert!(!err.message.contains("sqry "), "{}", err.message);
            assert!(!err.message.contains("--"), "{}", err.message);
        }
    }

    #[test]
    fn expand_cache_origin_follows_the_request() {
        use sqry_core::graph::unified::build::MacroOptionsRequest;
        let named = MacroOptionsRequest {
            expand_cache_dir: Some(std::path::PathBuf::from("c")),
            ..MacroOptionsRequest::empty()
        };
        assert_eq!(ExpandCacheOrigin::of(&named), ExpandCacheOrigin::Requested);
        let kept = MacroOptionsRequest {
            cfg_flags: Some(vec!["test".to_string()]),
            ..MacroOptionsRequest::empty()
        };
        assert_eq!(ExpandCacheOrigin::of(&kept), ExpandCacheOrigin::Recorded);
        assert_eq!(ExpandCacheOrigin::Requested.as_str(), "requested");
        assert_eq!(ExpandCacheOrigin::Recorded.as_str(), "recorded");
    }

    /// The `force=false` refusal is one invalid argument whose text holds
    /// on both hosts (an index on disk, or a graph the daemon holds).
    #[test]
    fn macro_options_need_force_is_one_invalid_argument() {
        let err = RpcError::macro_options_need_force(std::path::Path::new("/ws"));
        let reason = "rebuild_index: cfg_flags, expand_cache and reset_macro_options need \
                      force=true when a graph already exists at /ws (an index on disk or a \
                      workspace loaded in the daemon); nothing was built";
        assert_eq!(err.code, -32602);
        assert_eq!(err.kind, KIND_VALIDATION);
        assert_eq!(err.message, format!("invalid argument: {reason}"));
        assert_eq!(err.details, Some(serde_json::json!({ "reason": reason })));
    }

    /// S5 (round 7): the nested-index refusal names `force`, not the CLI's
    /// `--allow-nested`, and the three directories.
    #[test]
    fn nested_index_refused_names_force_and_the_ancestor() {
        let err = RpcError::nested_index_refused(
            std::path::Path::new("/ws/sub"),
            std::path::Path::new("/ws/.sqry/graph"),
            std::path::Path::new("/ws"),
        );
        assert_eq!(err.code, -32602);
        assert_eq!(err.kind, KIND_VALIDATION);
        assert!(
            err.message.starts_with(
                "invalid argument: rebuild_index: refusing to build a nested index at /ws/sub: \
                 an index already exists at /ws/.sqry/graph for the project at /ws"
            ) && err.message.contains("pass force: true")
                && !err.message.contains("--allow-nested"),
            "{}",
            err.message
        );
        let details = err.details.expect("details");
        assert_eq!(details["root"], "/ws/sub");
        assert_eq!(details["ancestor_graph"], "/ws/.sqry/graph");
        assert_eq!(details["boundary"], "/ws");
    }

    /// S5 (round 7): an unresolvable workspace is an invalid argument whose
    /// reason renders the whole chain after the tool's name.
    #[test]
    fn workspace_unresolved_is_an_invalid_argument_with_the_chain() {
        let cause =
            anyhow::anyhow!("No such file or directory").context("`path` \"x\" names no workspace");
        let err = RpcError::workspace_unresolved("rebuild_index", &cause);
        assert_eq!(err.code, -32602);
        assert_eq!(err.kind, KIND_VALIDATION);
        assert_eq!(
            err.message,
            "invalid argument: rebuild_index: cannot resolve the workspace for this request: \
             `path` \"x\" names no workspace: No such file or directory"
        );
    }

    /// An error whose `Display` names its source.
    #[derive(Debug, thiserror::Error)]
    #[error("refused: {0}")]
    struct NamesItsSource(#[source] Cause);

    #[derive(Debug, thiserror::Error)]
    #[error("the cause")]
    struct Cause;

    /// S9 (round 7): the whole chain on one line, a cause the line already
    /// carries skipped (an error whose `Display` names its source would
    /// print it twice), and an empty cause skipped; the other side, a cause
    /// the line does not carry, is kept.
    #[test]
    fn render_error_chain_renders_each_cause_once() {
        let err = anyhow::Error::new(NamesItsSource(Cause)).context("outer");
        assert_eq!(render_error_chain(&err), "outer: refused: the cause");
        let plain = anyhow::Error::new(Cause).context("outer").context("");
        assert_eq!(render_error_chain(&plain), "outer: the cause");
    }
}
