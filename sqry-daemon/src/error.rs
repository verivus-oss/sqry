//! Daemon-wide error type.
//!
//! Thin `thiserror` enum covering every fallible surface of the daemon:
//! config loading, workspace lifecycle, admission control, IPC transport,
//! rebuild dispatch, and lifecycle management (pidfile, signals, auto-start).
//! Tasks 6–10 extend this enum as each surface lands.
//! Every variant maps cleanly to a JSON-RPC error code when the error
//! crosses the IPC boundary (see [`DaemonError::jsonrpc_code`]).
//!
//! # Exit-code mapping (Task 9 U1)
//!
//! Variants that can be returned before the IPC server binds (lifecycle errors)
//! map to POSIX `sysexits.h` exit codes via [`DaemonError::exit_code`]:
//!
//! | Variant             | Exit code | `sysexits.h` constant  |
//! |---------------------|-----------|------------------------|
//! | `AlreadyRunning`    | 75        | `EX_TEMPFAIL`          |
//! | `AutoStartTimeout`  | 69        | `EX_UNAVAILABLE`       |
//! | `SignalSetup`       | 70        | `EX_SOFTWARE`          |
//! | `Config`            | 78        | `EX_CONFIG`            |
//! | `Io`                | 73        | `EX_CANTCREAT`         |
//! | Other variants      | 70        | `EX_SOFTWARE` (default)|

use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    time::SystemTime,
};

use sqry_core::graph::acquisition::GraphAcquisitionError;
use thiserror::Error;

use crate::{
    JSONRPC_INTERNAL_ERROR, JSONRPC_INVALID_PARAMS, JSONRPC_MEMORY_BUDGET_EXCEEDED,
    JSONRPC_QUERY_TOO_BROAD, JSONRPC_RESET_CANCELLATION_DISPATCHED, JSONRPC_RESET_WHILE_LOADING,
    JSONRPC_SOCKET_SETUP, JSONRPC_TOOL_TIMEOUT, JSONRPC_WORKSPACE_BUILD_FAILED,
    JSONRPC_WORKSPACE_EVICTED, JSONRPC_WORKSPACE_INCOMPATIBLE_GRAPH, JSONRPC_WORKSPACE_OVERSIZE,
    JSONRPC_WORKSPACE_PINNED, JSONRPC_WORKSPACE_STALE_EXPIRED,
};

/// Wire-stable `kind` tag for the cost-gate rejection on the
/// daemon-hosted MCP path. Mirror of
/// [`sqry_mcp::error::KIND_QUERY_TOO_BROAD`][1] for byte-identical
/// envelopes across the standalone and daemon-hosted MCP transports.
///
/// Source: `B_cost_gate.md` §3 + `00_contracts.md` §3.CC-2.
///
/// [1]: https://docs.rs/sqry-mcp/latest/sqry_mcp/error/constant.KIND_QUERY_TOO_BROAD.html
pub const KIND_QUERY_TOO_BROAD: &str = "query_too_broad";

fn f64_hours_to_u64(hours: f64) -> u64 {
    if !hours.is_finite() || hours <= 0.0 {
        return 0;
    }
    format!("{:.0}", hours.trunc())
        .parse::<u64>()
        .unwrap_or(u64::MAX)
}

/// Result alias for daemon operations.
pub type DaemonResult<T> = Result<T, DaemonError>;

/// All daemon-surface error variants.
#[derive(Debug, Error)]
pub enum DaemonError {
    /// Config file could not be read or parsed.
    #[error("config error at {path}: {source}")]
    Config {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },

    /// An `io::Error` occurred outside the config surface (socket bind,
    /// pidfile lock, filesystem probe, etc.).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Workspace load / rebuild failed with no prior-good graph to serve from.
    ///
    /// Maps to JSON-RPC `-32001`.
    #[error("workspace {root} build failed: {reason}")]
    WorkspaceBuildFailed { root: PathBuf, reason: String },

    /// Workspace is in the Failed state and the most recent successful build
    /// is older than the configured `stale_serve_max_age_hours` cap.
    ///
    /// Maps to JSON-RPC `-32002`.
    #[error("workspace {root} stale-serve window expired ({age_hours}h >= {cap_hours}h cap)")]
    WorkspaceStaleExpired {
        root: PathBuf,
        age_hours: u64,
        cap_hours: u32,
        /// Last successful build timestamp, if any. `None` when the workspace
        /// has never successfully built (edge case: should not reach
        /// `WorkspaceStaleExpired` in that case — `WorkspaceBuildFailed` is
        /// returned instead — but the type is permissive for future-proofing).
        last_good_at: Option<SystemTime>,
        /// Textual diagnostic from the most recent failed build, if any.
        last_error: Option<String>,
    },

    /// Admission control could not satisfy a reservation after evicting every
    /// non-pinned workspace.
    ///
    /// Maps to JSON-RPC `-32003`.
    #[error(
        "memory budget exceeded: requested {requested_bytes} B, \
         {current_bytes} B loaded + {reserved_bytes} B reserved + \
         {retained_bytes} B retained / {limit_bytes} B limit"
    )]
    MemoryBudgetExceeded {
        limit_bytes: u64,
        current_bytes: u64,
        reserved_bytes: u64,
        retained_bytes: u64,
        requested_bytes: u64,
    },

    /// Workspace was evicted or removed between a rebuild dispatch and its
    /// admission / publish commit. Signals the Task 7b2 watcher task and any
    /// direct `handle_changes` caller to terminate their per-workspace loop —
    /// subsequent dispatches on the same `WorkspaceKey` must route through a
    /// fresh `get_or_load` first.
    ///
    /// Surfaced by `RebuildDispatcher::handle_changes`' top-of-drain-loop
    /// eviction gate AND by `WorkspaceManager::reserve_rebuild`'s Phase-1
    /// `workspaces.read()` membership + cancellation check (both paths use
    /// this typed variant so 7b2 can match on it without string parsing).
    ///
    /// Maps to JSON-RPC `-32004`.
    #[error("workspace {root} evicted mid-rebuild")]
    WorkspaceEvicted { root: PathBuf },

    /// Caller requested `daemon/rebuild` or `daemon/cancel_rebuild` for a
    /// path that is not currently registered in the `WorkspaceManager`.
    ///
    /// Shares the JSON-RPC `-32004` code with [`Self::WorkspaceEvicted`].
    /// The `error_data` `"hint"` field distinguishes the two situations on
    /// the wire.
    ///
    /// Maps to JSON-RPC `-32004`.
    #[error("workspace {root} is not loaded")]
    WorkspaceNotLoaded { root: PathBuf },

    /// On-disk graph snapshot or manifest is incompatible with this binary
    /// (unknown plugin ids in the manifest, or a snapshot format the
    /// runtime cannot parse). SGA02 / SGA04 mandate this stay distinct
    /// from [`Self::WorkspaceBuildFailed`] so clients can route
    /// "rebuild" vs. "upgrade binary" vs. "wait" responses correctly.
    ///
    /// `reason` is a human-readable rendering of the underlying
    /// [`sqry_core::graph::acquisition::PluginSelectionStatus`] — the
    /// `From<GraphAcquisitionError>` impl below preserves the variant
    /// faithfully so no information is lost on the wire.
    ///
    /// Maps to JSON-RPC `-32005`.
    #[error("workspace {root} graph is incompatible with this binary: {reason}")]
    WorkspaceIncompatibleGraph { root: PathBuf, reason: String },

    /// Tool invocation exceeded [`DaemonConfig::tool_timeout_secs`].
    /// Emitted by `tool_core::classify_and_execute` (Task 8 Phase 8c U6)
    /// when the `tokio::time::timeout(tool_timeout, spawn_blocking(run))`
    /// outer timer fires. The detached [`tokio::task::JoinHandle`] is
    /// dropped — the OS thread may continue executing the tool closure
    /// but its result is discarded.
    ///
    /// The `deadline_ms` field is the canonical wire value (populated by
    /// the constructor as `secs * 1000`) so `error_data` does not have
    /// to re-derive it on every call and serialised payloads remain
    /// byte-for-byte identical regardless of constructor shape.
    ///
    /// Maps to JSON-RPC `-32000`.
    ///
    /// [`DaemonConfig::tool_timeout_secs`]: crate::config::DaemonConfig
    #[error(
        "tool invocation exceeded deadline of {deadline_ms}ms for workspace {}",
        root.display()
    )]
    ToolTimeout {
        root: PathBuf,
        secs: u64,
        /// Derived: `secs * 1000`. Stored explicitly to avoid
        /// re-calculating inside `error_data` / `Display` impls and to
        /// give the MCP-path wrapper (`daemon_err_to_mcp`, Phase 8c U8)
        /// a single field to read.
        deadline_ms: u64,
    },

    /// Argument validation failure surfaced by `tool_core` BEFORE any
    /// workspace classification runs. Used for `resolve_index_root`
    /// failures, missing `path` arguments in MCP tool args, and any
    /// other precondition violation that must be rejected with a
    /// JSON-RPC `-32602` "Invalid params" response.
    ///
    /// Maps to JSON-RPC `-32602`.
    #[error("invalid argument: {reason}")]
    InvalidArgument { reason: String },

    /// Typed `sqry_mcp::error::RpcError` preserved through the
    /// daemon-hosted MCP path so the wire envelope is byte-identical
    /// to the standalone MCP response (cluster-C iter-3, codex PR
    /// review recommendation).
    ///
    /// The daemon adapter (`sqry-mcp/src/daemon_adapter/dispatch.rs`)
    /// previously rewrapped param-parsing failures with
    /// `anyhow!("invalid arguments: {e}")`, which destroyed the typed
    /// `RpcError` root before [`crate::ipc::tool_core::execute_with_timeout`]
    /// could downcast it. The downstream `daemon_err_to_mcp`
    /// then mapped through `DaemonError::Internal` →
    /// `McpError::internal_error` (`-32603`) regardless of the
    /// `RpcError`'s actual `code`. This variant is the dedicated
    /// pass-through: the inner `RpcError` carries the correct
    /// `code` (`-32602` for validation failures, etc.), `kind`,
    /// `retryable`, `retry_after_ms`, and `details`, and
    /// [`daemon_err_to_mcp`][1] renders them through the same
    /// `invalid_params` / `internal_error` selector the standalone
    /// path uses.
    ///
    /// [1]: crate::mcp_host::error_map::daemon_err_to_mcp
    #[error("{0}")]
    RpcErrorPreserved(sqry_mcp::error::RpcError),

    /// Catch-all for errors surfaced by
    /// [`sqry_mcp::daemon_adapter`][1] tool execution that do not map
    /// to a more specific `DaemonError` variant. The wrapped
    /// `anyhow::Error` is flattened into a string on the wire via the
    /// `Display`/`#[source]` chain.
    ///
    /// Maps to JSON-RPC `-32603`.
    ///
    /// [1]: https://docs.rs/sqry-mcp/latest/sqry_mcp/daemon_adapter/index.html
    #[error("internal error: {0}")]
    Internal(#[source] anyhow::Error),

    // ── Task 9 U1 — lifecycle error variants ─────────────────────────────
    /// A sqryd process already holds the exclusive flock on `lock` and has
    /// written its PID to `pidfile`.  The caller should surface this to the
    /// user with the owner PID (if legible) and exit `EX_TEMPFAIL` (75).
    ///
    /// This error fires before [`IpcServer::bind`] and therefore before any
    /// workspace is registered; it should never be stored in the workspace
    /// `last_error` field.  [`crate::workspace::manager::clone_err`] maps it
    /// to `WorkspaceBuildFailed` as a defensive fallback.
    ///
    /// [`IpcServer::bind`]: crate::ipc::IpcServer
    #[error(
        "sqryd is already running (pid={}) on socket {} (lock: {})",
        owner_pid.map_or_else(|| "?".to_owned(), |p| p.to_string()),
        socket.display(),
        lock.display()
    )]
    AlreadyRunning {
        /// The IPC socket path that the running daemon owns.
        socket: PathBuf,
        /// The flock file that proves ownership.
        lock: PathBuf,
        /// PID of the owner process, if the pidfile was legible.
        owner_pid: Option<u32>,
    },

    /// The daemon did not become ready within `timeout_secs` seconds.
    /// Used by both the `--detach` parent wait loop and the
    /// `lifecycle::start_detached` auto-spawn helper (Task 10).
    ///
    /// Callers should exit `EX_UNAVAILABLE` (69).
    #[error(
        "daemon did not become ready within {timeout_secs}s on socket {}",
        socket.display()
    )]
    AutoStartTimeout {
        /// How long we waited.
        timeout_secs: u64,
        /// The socket we polled.
        socket: PathBuf,
    },

    /// Installing OS signal handlers failed (e.g. `sigaction` returned
    /// `ENOSYS` in a highly-restricted container, or tokio's signal
    /// registration failed).
    ///
    /// Callers should exit `EX_SOFTWARE` (70).
    #[error("failed to install signal handlers: {source}")]
    SignalSetup {
        #[source]
        source: std::io::Error,
    },

    // ── sqry-mcp flakiness P0-1 / P1 admission + recovery variants ───────
    /// The freshly-built graph exceeds the daemon's memory budget by
    /// itself — even if every other workspace were evicted, the
    /// daemon could not host it. Returned by
    /// `WorkspaceManager::publish_and_retain` AFTER the build
    /// completes but BEFORE the new graph is exposed to readers.
    ///
    /// Wire code: `-32006`. Distinct from `MemoryBudgetExceeded`
    /// (`-32003`), which is a *projected* admission failure on a
    /// pre-build estimate.
    ///
    /// Source: `G_daemon_control_plane.md` §1.4 hand-off G4.
    #[error(
        "workspace {} oversize: {measured_bytes} > {limit_bytes} (after eviction headroom; current loaded: {current_loaded_bytes})",
        root.display()
    )]
    WorkspaceOversize {
        root: PathBuf,
        measured_bytes: u64,
        limit_bytes: u64,
        current_loaded_bytes: u64,
    },

    /// `daemon/reset` was invoked on a pinned workspace and the
    /// caller did not pass `force = true`. Pinning is the operator
    /// opt-in for "do not LRU-evict this workspace"; resetting it
    /// has the same drop-graph effect as eviction and is therefore
    /// gated behind the same explicit override.
    ///
    /// Wire code: `-32010`.
    ///
    /// Source: `G_daemon_control_plane.md` §3.2 hand-off G4.
    #[error("workspace {} is pinned; pass force=true to reset", root.display())]
    WorkspacePinned { root: PathBuf },

    /// `daemon/reset` was invoked on a workspace whose state is
    /// `Loading`. Cancelling a load mid-flight is structurally
    /// unsafe (reservation accounting + admission state would
    /// drift). Caller must wait for the load to settle (success or
    /// `Failed`) and retry.
    ///
    /// Wire code: `-32008`.
    ///
    /// Source: `G_daemon_control_plane.md` §3.2 hand-off G4.
    #[error("workspace {} is currently loading; retry once load settles", root.display())]
    ResetWhileLoading { root: PathBuf },

    /// `daemon/reset` was invoked on a workspace whose state is
    /// `Rebuilding`. The reset has dispatched a cancellation token
    /// to the runner; the caller should retry after `retry_after_ms`
    /// for the runner to finish its drain pass and the workspace to
    /// transition to `Failed` (which is then idempotently reset on
    /// the next call).
    ///
    /// Wire code: `-32009`.
    ///
    /// Source: `G_daemon_control_plane.md` §3.2 hand-off G4.
    #[error(
        "workspace {} rebuild cancellation dispatched; retry after {retry_after_ms}ms",
        root.display()
    )]
    ResetCancellationDispatched { root: PathBuf, retry_after_ms: u64 },

    /// Socket parent directory cannot be created or is not writable.
    /// Surfaced before `IpcServer::bind` so the failure mode is
    /// distinguishable from a generic `EACCES` (which would otherwise
    /// be wrapped as `Io`).
    ///
    /// Wire code: `-32007`. Note this is not normally observed on
    /// the wire because it fires before the IPC server binds; the
    /// JSON-RPC mapping exists for the rare case where the daemon
    /// surface re-emits this through IPC during a hot-reload of the
    /// socket configuration.
    ///
    /// Source: `G_daemon_control_plane.md` §5.2 hand-off G4.
    #[error("socket setup failed at {}: {reason}", path.display())]
    SocketSetup { path: PathBuf, reason: String },

    /// Pre-flight cost gate rejected a query (per `B_cost_gate.md`
    /// §3, daemon-hosted MCP parity arm). The wire envelope mirrors
    /// the standalone `RpcError::query_too_broad` exactly so MCP
    /// clients can use a single parser regardless of which transport
    /// the request flowed through.
    ///
    /// Wire code: `-32602` (the existing `invalid_params` slot;
    /// `kind = "query_too_broad"` is the discriminator).
    ///
    /// Source: `B_cost_gate.md` §3 + `00_contracts.md` §3.CC-2.
    #[error("query rejected by cost gate: {reason}")]
    QueryTooBroad {
        reason: String,
        details: serde_json::Value,
    },
}

impl DaemonError {
    /// Map to the stable JSON-RPC error code used on the wire.
    ///
    /// Returns `None` for errors that have no public JSON-RPC code — these
    /// are serialised as `-32603 "Internal error"` per the JSON-RPC 2.0 spec
    /// at the IPC boundary (wired in Task 8).
    ///
    /// The Task 9 lifecycle variants (`AlreadyRunning`, `AutoStartTimeout`,
    /// `SignalSetup`) fire before `IpcServer::bind` so they never cross the
    /// IPC boundary directly; `None` is returned for them here.  They are
    /// only surfaced to human users via `exit_code()` and process exit.
    #[must_use]
    pub const fn jsonrpc_code(&self) -> Option<i32> {
        match self {
            Self::WorkspaceBuildFailed { .. } => Some(JSONRPC_WORKSPACE_BUILD_FAILED),
            Self::WorkspaceStaleExpired { .. } => Some(JSONRPC_WORKSPACE_STALE_EXPIRED),
            Self::MemoryBudgetExceeded { .. } => Some(JSONRPC_MEMORY_BUDGET_EXCEEDED),
            Self::WorkspaceEvicted { .. } | Self::WorkspaceNotLoaded { .. } => {
                Some(JSONRPC_WORKSPACE_EVICTED)
            }
            Self::WorkspaceIncompatibleGraph { .. } => Some(JSONRPC_WORKSPACE_INCOMPATIBLE_GRAPH),
            Self::ToolTimeout { .. } => Some(JSONRPC_TOOL_TIMEOUT),
            Self::InvalidArgument { .. } => Some(JSONRPC_INVALID_PARAMS),
            // Cluster-C iter-3: pass-through preserves the inner
            // RpcError's JSON-RPC code (typically -32602 for
            // validation failures emitted by `validate_budget_rows`
            // and similar validators).
            Self::RpcErrorPreserved(rpc) => Some(rpc.code),
            Self::Internal(_) => Some(JSONRPC_INTERNAL_ERROR),
            Self::WorkspaceOversize { .. } => Some(JSONRPC_WORKSPACE_OVERSIZE),
            Self::WorkspacePinned { .. } => Some(JSONRPC_WORKSPACE_PINNED),
            Self::ResetWhileLoading { .. } => Some(JSONRPC_RESET_WHILE_LOADING),
            Self::ResetCancellationDispatched { .. } => Some(JSONRPC_RESET_CANCELLATION_DISPATCHED),
            Self::SocketSetup { .. } => Some(JSONRPC_SOCKET_SETUP),
            Self::QueryTooBroad { .. } => Some(JSONRPC_QUERY_TOO_BROAD),
            // Lifecycle errors don't cross the IPC boundary.
            Self::AlreadyRunning { .. }
            | Self::AutoStartTimeout { .. }
            | Self::SignalSetup { .. }
            | Self::Config { .. }
            | Self::Io(_) => None,
        }
    }

    /// Map to a POSIX process exit code following the BSD `sysexits.h`
    /// conventions used for daemon CLI errors (Task 9 U1).
    ///
    /// | Code | Symbol        | Semantics                                   |
    /// |------|---------------|---------------------------------------------|
    /// | 0    | `EX_OK`       | Success (not an error; included for completeness) |
    /// | 69   | `EX_UNAVAILABLE` | Service unavailable (timeout, not-ready)  |
    /// | 70   | `EX_SOFTWARE` | Internal software error                     |
    /// | 73   | `EX_CANTCREAT`| IO error / cannot create required file      |
    /// | 75   | `EX_TEMPFAIL` | Try again (e.g. another instance is running)|
    /// | 78   | `EX_CONFIG`   | Configuration error                         |
    ///
    /// For variants that only occur inside the IPC / workspace layer
    /// (not at process-startup time) the JSON-RPC code's sign-flipped
    /// magnitude is used as a proxy, falling back to `70` (`EX_SOFTWARE`)
    /// for anything not covered.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            // BSD sysexits.h (man 3 sysexits) exit codes for lifecycle errors.
            // 75 EX_TEMPFAIL: another process already owns the socket/lock.
            Self::AlreadyRunning { .. } => 75,
            // 69 EX_UNAVAILABLE: daemon didn't start in time.
            Self::AutoStartTimeout { .. } => 69,
            // 70 EX_SOFTWARE: internal OS-level failure (signal registration).
            // 78 EX_CONFIG: malformed or unreadable config file.
            Self::Config { .. } => 78,
            // 73 EX_CANTCREAT: I/O failure (pidfile write, socket bind, etc.).
            Self::Io(_) => 73,
            // IPC-layer errors that escape to the CLI surface default to 70.
            Self::SignalSetup { .. }
            | Self::WorkspaceBuildFailed { .. }
            | Self::WorkspaceStaleExpired { .. }
            | Self::MemoryBudgetExceeded { .. }
            | Self::WorkspaceEvicted { .. }
            | Self::WorkspaceNotLoaded { .. }
            | Self::WorkspaceIncompatibleGraph { .. }
            | Self::ToolTimeout { .. }
            | Self::InvalidArgument { .. }
            | Self::RpcErrorPreserved(_)
            | Self::Internal(_)
            | Self::WorkspaceOversize { .. }
            | Self::WorkspacePinned { .. }
            | Self::ResetWhileLoading { .. }
            | Self::ResetCancellationDispatched { .. }
            | Self::SocketSetup { .. }
            | Self::QueryTooBroad { .. } => 70,
        }
    }

    /// Build the `error.data` JSON payload surfaced alongside the JSON-RPC
    /// error code. Returns `None` when no structured payload should be
    /// attached (typically `Io`/`Config` errors routed through `-32603`).
    ///
    /// Task 8 Phase 8a. The IPC method dispatch consumes this to populate
    /// `JsonRpcError.data` so clients can render actionable diagnostics
    /// without parsing the free-form `message` string.
    #[must_use]
    pub fn error_data(&self) -> Option<serde_json::Value> {
        use serde_json::json;
        match self {
            Self::MemoryBudgetExceeded {
                limit_bytes,
                current_bytes,
                reserved_bytes,
                retained_bytes,
                requested_bytes,
            } => Some(json!({
                "limit_bytes": limit_bytes,
                "current_bytes": current_bytes,
                "reserved_bytes": reserved_bytes,
                "retained_bytes": retained_bytes,
                "requested_bytes": requested_bytes,
            })),
            Self::WorkspaceStaleExpired {
                root,
                age_hours,
                cap_hours,
                last_good_at,
                last_error,
            } => Some(workspace_stale_data(
                root,
                *age_hours,
                *cap_hours,
                *last_good_at,
                last_error.as_deref(),
            )),
            Self::WorkspaceBuildFailed { root, reason }
            | Self::WorkspaceIncompatibleGraph { root, reason } => Some(json!({
                    "root": root,
                    "reason": reason,
            })),
            Self::WorkspaceEvicted { root } => Some(json!({ "root": root })),
            Self::WorkspaceNotLoaded { root } => Some(json!({
                "root": root,
                "hint": "use daemon/load to load the workspace before calling daemon/rebuild",
            })),
            // Phase 8c §O canonical 4-key envelope
            // `{kind, retryable, retry_after_ms, details}` matching
            // standalone `sqry-mcp::rpc_error_to_mcp` shape so clients
            // can handle daemon-path and direct-path errors with a
            // single parser.
            Self::ToolTimeout {
                root: _,
                secs: _,
                deadline_ms,
            } => Some(tool_timeout_data(*deadline_ms)),
            Self::InvalidArgument { reason } => Some(json!({
                "kind": "validation_error",
                "retryable": false,
                "retry_after_ms": serde_json::Value::Null,
                "details": {
                    "reason": reason,
                },
            })),
            // Cluster-C iter-3: preserve the inner RpcError's wire
            // shape verbatim so the daemon-hosted MCP envelope is
            // byte-identical to the standalone path's
            // `rpc_error_to_mcp` output.
            Self::RpcErrorPreserved(rpc) => Some(rpc_error_data(rpc)),
            Self::Internal(_) => Some(json!({
                "kind": "internal",
                "retryable": false,
                "retry_after_ms": serde_json::Value::Null,
                "details": serde_json::Value::Null,
            })),
            Self::Io(_)
            | Self::Config { .. }
            | Self::AlreadyRunning { .. }
            | Self::AutoStartTimeout { .. }
            | Self::SignalSetup { .. } => None,
            // Lifecycle errors don't cross the IPC boundary; no structured
            // payload is needed.
            Self::WorkspaceOversize {
                root,
                measured_bytes,
                limit_bytes,
                current_loaded_bytes,
            } => Some(json!({
                "root": root,
                "measured_bytes": measured_bytes,
                "limit_bytes": limit_bytes,
                "current_loaded_bytes": current_loaded_bytes,
            })),
            Self::WorkspacePinned { root } => Some(json!({
                "root": root,
                "hint": "pass force=true to reset a pinned workspace",
            })),
            Self::ResetWhileLoading { root } => Some(json!({
                "root": root,
                "hint": "wait for the load to settle, then retry",
            })),
            Self::ResetCancellationDispatched {
                root,
                retry_after_ms,
            } => Some(json!({
                "root": root,
                "retry_after_ms": retry_after_ms,
            })),
            Self::SocketSetup { path, reason } => Some(json!({
                "path": path,
                "reason": reason,
            })),
            // Phase 8c §O canonical 4-key envelope. The standalone
            // `sqry-mcp::RpcError::query_too_broad` envelope shape is
            // mirrored byte-for-byte (`B_cost_gate.md` §3 +
            // `00_contracts.md` §3.CC-2). The caller assembles the
            // CC-2 seven-key `details` value (source, kind, limit,
            // estimated_visited_nodes / examined / predicate_shape /
            // suggested_predicates / doc_url) and hands it to this
            // arm verbatim — this layer only owns the 4-key
            // envelope.
            Self::QueryTooBroad { details, .. } => Some(query_too_broad_data(details)),
        }
    }
}

fn workspace_stale_data(
    root: &Path,
    age_hours: u64,
    cap_hours: u32,
    last_good_at: Option<SystemTime>,
    last_error: Option<&str>,
) -> serde_json::Value {
    use serde_json::json;
    let last_good_rfc3339 = last_good_at.map(|t| {
        chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });
    json!({
        "root": root,
        "age_hours": age_hours,
        "cap_hours": cap_hours,
        "last_good_at": last_good_rfc3339,
        "last_error": last_error,
    })
}

fn tool_timeout_data(deadline_ms: u64) -> serde_json::Value {
    use serde_json::json;
    json!({
        "kind": "deadline_exceeded",
        "retryable": true,
        "retry_after_ms": 500,
        "details": {
            "tool": serde_json::Value::Null,
            "deadline_ms": deadline_ms,
        },
    })
}

fn rpc_error_data(rpc: &sqry_mcp::error::RpcError) -> serde_json::Value {
    use serde_json::json;
    json!({
        "kind": rpc.kind,
        "retryable": rpc.retryable,
        "retry_after_ms": rpc.retry_after_ms,
        "details": rpc.details,
    })
}

fn query_too_broad_data(details: &serde_json::Value) -> serde_json::Value {
    use serde_json::json;
    json!({
        "kind": KIND_QUERY_TOO_BROAD,
        "retryable": false,
        "retry_after_ms": serde_json::Value::Null,
        "details": details,
    })
}

// ---------------------------------------------------------------------------
// SGA04 — `From<GraphAcquisitionError>` for `DaemonError`.
// ---------------------------------------------------------------------------
//
// Maps the transport-neutral acquisition taxonomy into the daemon's
// existing JSON-RPC-coded error variants. This is the boundary used by
// SGA05 dispatch wiring to surface acquisition failures through the
// JSON-RPC / MCP envelopes without losing the InvalidPath / Evicted /
// StaleExpired / IncompatibleGraph distinctions (per the SGA spec
// "Adapters must not collapse" rule).
impl From<GraphAcquisitionError> for DaemonError {
    fn from(err: GraphAcquisitionError) -> Self {
        match err {
            GraphAcquisitionError::InvalidPath { path, reason } => Self::InvalidArgument {
                reason: format!("invalid path {}: {reason}", path.display()),
            },
            GraphAcquisitionError::NoGraph { workspace_root } => Self::WorkspaceBuildFailed {
                root: workspace_root,
                reason: "no graph artifact for workspace".to_string(),
            },
            GraphAcquisitionError::LoadFailed {
                source_root,
                reason,
            } => Self::WorkspaceBuildFailed {
                root: source_root,
                reason: format!("graph load failed: {reason}"),
            },
            GraphAcquisitionError::IncompatibleGraph {
                source_root,
                status,
            } => {
                use sqry_core::graph::acquisition::PluginSelectionStatus;
                // Format the status losslessly into a user-facing reason
                // string. `Exact` should never reach this arm — the core
                // crate only constructs `IncompatibleGraph` for the two
                // negative verdicts — but we cover it defensively to
                // keep the conversion total.
                let reason = match status {
                    PluginSelectionStatus::IncompatibleUnknownPluginIds {
                        unknown_plugin_ids,
                        manifest_path,
                    } => {
                        let suggested =
                            sqry_plugin_registry::missing_features_for(&unknown_plugin_ids);
                        let mut buf =
                            format!("unknown plugin ids: [{}]", unknown_plugin_ids.join(", "),);
                        if let Some(p) = manifest_path.as_ref() {
                            let _ = write!(buf, " (manifest: {})", p.display());
                        }
                        if !suggested.is_empty() {
                            // Cluster-E iter-2: render the full
                            // copy-paste-ready cargo install command,
                            // matching the CLI / standalone-MCP shape.
                            let _ = write!(
                                buf,
                                " — rebuild this binary with: \
                                 cargo install --path sqry-cli --features {}",
                                suggested.join(","),
                            );
                        }
                        buf
                    }
                    PluginSelectionStatus::IncompatibleSnapshotFormat { reason } => {
                        format!("incompatible snapshot format: {reason}")
                    }
                    PluginSelectionStatus::Exact => {
                        // Defensive: should not happen.
                        "compatibility verdict reported Exact alongside IncompatibleGraph error"
                            .to_string()
                    }
                    other => format!("unrecognised plugin selection status: {other:?}"),
                };
                Self::WorkspaceIncompatibleGraph {
                    root: source_root,
                    reason,
                }
            }
            GraphAcquisitionError::NotReady {
                workspace_root,
                lifecycle,
            } => Self::WorkspaceBuildFailed {
                root: workspace_root,
                reason: format!("workspace not ready (lifecycle={lifecycle})"),
            },
            GraphAcquisitionError::Evicted {
                workspace_root,
                original_lifecycle,
                reload_failure,
            } => {
                // Preserve original-lifecycle + reload-failure context
                // by tracing it before collapsing into the daemon's
                // single-field WorkspaceEvicted variant. The wire shape
                // for `-32004` is fixed (`{"root": ...}`); diagnostic
                // detail rides on the daemon log channel.
                tracing::warn!(
                    workspace = %workspace_root.display(),
                    original_lifecycle = %original_lifecycle,
                    reload_failure = ?reload_failure,
                    "graph acquisition: workspace evicted, reload failed"
                );
                Self::WorkspaceEvicted {
                    root: workspace_root,
                }
            }
            GraphAcquisitionError::StaleExpired {
                workspace_root,
                age_hours,
            } => Self::WorkspaceStaleExpired {
                root: workspace_root,
                age_hours: age_hours.map_or(0, f64_hours_to_u64),
                cap_hours: 0,
                last_good_at: None,
                last_error: None,
            },
            GraphAcquisitionError::BuildFailed {
                workspace_root,
                reason,
            } => Self::WorkspaceBuildFailed {
                root: workspace_root,
                reason,
            },
            GraphAcquisitionError::Internal { reason } => {
                Self::Internal(anyhow::anyhow!("graph acquisition: {reason}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonrpc_code_covers_every_public_variant() {
        let mem = DaemonError::MemoryBudgetExceeded {
            limit_bytes: 2_048 * 1024 * 1024,
            current_bytes: 0,
            reserved_bytes: 0,
            retained_bytes: 0,
            requested_bytes: 4_096 * 1024 * 1024,
        };
        assert_eq!(mem.jsonrpc_code(), Some(JSONRPC_MEMORY_BUDGET_EXCEEDED));

        let stale = DaemonError::WorkspaceStaleExpired {
            root: PathBuf::from("/repo"),
            age_hours: 48,
            cap_hours: 24,
            last_good_at: None,
            last_error: None,
        };
        assert_eq!(stale.jsonrpc_code(), Some(JSONRPC_WORKSPACE_STALE_EXPIRED));

        let failed = DaemonError::WorkspaceBuildFailed {
            root: PathBuf::from("/repo"),
            reason: "plugin panic".into(),
        };
        assert_eq!(failed.jsonrpc_code(), Some(JSONRPC_WORKSPACE_BUILD_FAILED));

        let evicted = DaemonError::WorkspaceEvicted {
            root: PathBuf::from("/repo"),
        };
        assert_eq!(evicted.jsonrpc_code(), Some(JSONRPC_WORKSPACE_EVICTED));
    }

    // -----------------------------------------------------------------
    // SGA04 Gate-A major #5 — IncompatibleGraph mapping tests
    // -----------------------------------------------------------------
    //
    // The acquisition taxonomy distinguishes path-policy /
    // compatibility errors from generic build failures so MCP / IPC
    // clients can react differently (rebuild vs. upgrade vs. retry).
    // These tests pin that the `From<GraphAcquisitionError>` impl
    // routes IncompatibleGraph to the dedicated
    // `WorkspaceIncompatibleGraph` variant — NOT to
    // `WorkspaceBuildFailed`.

    #[test]
    fn from_graph_acquisition_incompatible_unknown_plugins_maps_to_incompatible_graph() {
        use sqry_core::graph::acquisition::{GraphAcquisitionError, PluginSelectionStatus};

        let err = GraphAcquisitionError::IncompatibleGraph {
            source_root: PathBuf::from("/repo"),
            status: PluginSelectionStatus::IncompatibleUnknownPluginIds {
                unknown_plugin_ids: vec!["plugin-a".to_string(), "plugin-b".to_string()],
                manifest_path: Some(PathBuf::from("/repo/.sqry/graph/manifest.json")),
            },
        };
        let de: DaemonError = err.into();
        match de {
            DaemonError::WorkspaceIncompatibleGraph { root, reason } => {
                assert_eq!(root, PathBuf::from("/repo"));
                assert!(
                    reason.contains("plugin-a") && reason.contains("plugin-b"),
                    "reason must list every unknown plugin id losslessly, got: {reason}"
                );
                assert!(
                    reason.contains("unknown plugin ids"),
                    "reason must surface the plugin-id verdict, got: {reason}"
                );
            }
            other => panic!(
                "GraphAcquisitionError::IncompatibleGraph(IncompatibleUnknownPluginIds) \
                 must map to DaemonError::WorkspaceIncompatibleGraph, got {other:?}"
            ),
        }
    }

    #[test]
    fn from_graph_acquisition_incompatible_snapshot_format_maps_to_incompatible_graph() {
        use sqry_core::graph::acquisition::{GraphAcquisitionError, PluginSelectionStatus};

        let err = GraphAcquisitionError::IncompatibleGraph {
            source_root: PathBuf::from("/repo"),
            status: PluginSelectionStatus::IncompatibleSnapshotFormat {
                reason: "V99 magic, this binary supports up to V10".to_string(),
            },
        };
        let de: DaemonError = err.into();
        match de {
            DaemonError::WorkspaceIncompatibleGraph { root, reason } => {
                assert_eq!(root, PathBuf::from("/repo"));
                assert!(
                    reason.contains("incompatible snapshot format") && reason.contains("V99 magic"),
                    "reason must preserve the snapshot-format detail, got: {reason}"
                );
            }
            other => panic!(
                "GraphAcquisitionError::IncompatibleGraph(IncompatibleSnapshotFormat) \
                 must map to DaemonError::WorkspaceIncompatibleGraph, got {other:?}"
            ),
        }
    }

    #[test]
    fn workspace_incompatible_graph_has_dedicated_jsonrpc_code() {
        let err = DaemonError::WorkspaceIncompatibleGraph {
            root: PathBuf::from("/repo"),
            reason: "unknown plugin ids: [a, b]".to_string(),
        };
        assert_eq!(
            err.jsonrpc_code(),
            Some(JSONRPC_WORKSPACE_INCOMPATIBLE_GRAPH),
            "WorkspaceIncompatibleGraph must carry the dedicated -32005 code"
        );
        assert_eq!(err.jsonrpc_code(), Some(-32005));
        // Distinct from -32001.
        assert_ne!(err.jsonrpc_code(), Some(JSONRPC_WORKSPACE_BUILD_FAILED));

        let data = err
            .error_data()
            .expect("WorkspaceIncompatibleGraph must emit error_data");
        assert_eq!(data["root"], "/repo");
        assert_eq!(data["reason"], "unknown plugin ids: [a, b]");
    }

    #[test]
    fn jsonrpc_code_is_none_for_internal_variants() {
        let io = DaemonError::Io(std::io::Error::other("boom"));
        assert!(io.jsonrpc_code().is_none());

        let cfg = DaemonError::Config {
            path: PathBuf::from("/etc/sqry.toml"),
            source: anyhow::anyhow!("malformed"),
        };
        assert!(cfg.jsonrpc_code().is_none());
    }

    // -----------------------------------------------------------------
    // Task 8 Phase 8c U5 — Tool-dispatch error variants
    // -----------------------------------------------------------------
    //
    // These tests pin the stable wire contract defined in the design
    // doc §O for `ToolTimeout` / `InvalidArgument` / `Internal`. Any
    // change to the JSON-RPC codes or the `{kind, retryable,
    // retry_after_ms, details}` envelope shape will fail at least one
    // of these tests and force a matching update to the MCP-path
    // wrapper (`daemon_err_to_mcp`) so daemon-path and direct-path
    // MCP responses stay byte-identical.

    #[test]
    fn tool_timeout_has_jsonrpc_code_32000_and_deadline_exceeded_kind() {
        let err = DaemonError::ToolTimeout {
            root: PathBuf::from("/tmp/workspace"),
            secs: 60,
            deadline_ms: 60_000,
        };
        assert_eq!(err.jsonrpc_code(), Some(JSONRPC_TOOL_TIMEOUT));
        assert_eq!(err.jsonrpc_code(), Some(-32000));
        let data = err.error_data().expect("ToolTimeout must emit data");
        assert_eq!(data["kind"], "deadline_exceeded");
        assert_eq!(data["retryable"], true);
        // Cluster-A iter-2 BLOCKER 1: aligned with the standalone
        // `RpcError::deadline_exceeded` envelope (500 ms).
        assert_eq!(data["retry_after_ms"], 500);
        assert_eq!(data["details"]["deadline_ms"], 60_000);
        // Cluster-A iter-2 BLOCKER 1: `details.root` removed for
        // wire-identity with the standalone shape.
        assert!(
            data["details"].get("root").is_none(),
            "details.root must be absent post-iter-2"
        );
        // Placeholder for the MCP-path wrapper (Phase 8c U8) to
        // overwrite with the inbound method name.
        assert!(data["details"]["tool"].is_null());
    }

    #[test]
    fn invalid_argument_has_jsonrpc_code_32602_and_validation_error_kind() {
        let err = DaemonError::InvalidArgument {
            reason: "missing path argument".into(),
        };
        assert_eq!(err.jsonrpc_code(), Some(JSONRPC_INVALID_PARAMS));
        assert_eq!(err.jsonrpc_code(), Some(-32602));
        let data = err.error_data().expect("InvalidArgument must emit data");
        assert_eq!(data["kind"], "validation_error");
        assert_eq!(data["retryable"], false);
        assert!(data["retry_after_ms"].is_null());
        assert_eq!(data["details"]["reason"], "missing path argument");
    }

    #[test]
    fn internal_has_jsonrpc_code_32603_and_internal_kind() {
        let err = DaemonError::Internal(anyhow::anyhow!("something blew up"));
        assert_eq!(err.jsonrpc_code(), Some(JSONRPC_INTERNAL_ERROR));
        assert_eq!(err.jsonrpc_code(), Some(-32603));
        let data = err.error_data().expect("Internal must emit data");
        assert_eq!(data["kind"], "internal");
        assert_eq!(data["retryable"], false);
        assert!(data["retry_after_ms"].is_null());
        assert!(data["details"].is_null());
    }

    #[test]
    fn error_data_envelope_shape_is_canonical_for_tool_dispatch_variants() {
        // All 3 new Phase 8c U5 variants must emit EXACTLY the 4
        // canonical top-level keys and no others — this is the
        // contract documented in the design doc §O.3 and is what
        // the MCP-path wrapper relies on to avoid renaming / reshaping
        // fields.
        let expected: std::collections::BTreeSet<String> =
            ["kind", "retryable", "retry_after_ms", "details"]
                .iter()
                .map(|s| (*s).to_string())
                .collect();

        let errs = [
            DaemonError::ToolTimeout {
                root: PathBuf::from("/tmp"),
                secs: 10,
                deadline_ms: 10_000,
            },
            DaemonError::InvalidArgument { reason: "x".into() },
            DaemonError::Internal(anyhow::anyhow!("y")),
        ];
        for err in errs {
            let data = err.error_data().expect("variant must emit data");
            let obj = data
                .as_object()
                .expect("error_data envelope must be a JSON object");
            let keys: std::collections::BTreeSet<String> = obj.keys().cloned().collect();
            assert_eq!(
                keys, expected,
                "error_data envelope for {err:?} must be exactly the 4 canonical keys"
            );
        }
    }

    // -----------------------------------------------------------------
    // Task 9 U1 — DaemonError lifecycle variant tests
    // -----------------------------------------------------------------

    /// `AlreadyRunning` must have no JSON-RPC code (it never reaches the wire)
    /// and must exit with code 75 (`EX_TEMPFAIL`).
    #[test]
    fn already_running_has_no_jsonrpc_code_and_exit_75() {
        let err = DaemonError::AlreadyRunning {
            owner_pid: Some(12345),
            socket: PathBuf::from("/run/user/1000/sqryd.sock"),
            lock: PathBuf::from("/run/user/1000/sqryd.lock"),
        };
        assert!(
            err.jsonrpc_code().is_none(),
            "AlreadyRunning must not carry a JSON-RPC code"
        );
        assert_eq!(
            err.exit_code(),
            75,
            "AlreadyRunning must exit with EX_TEMPFAIL (75)"
        );
        assert!(
            err.error_data().is_none(),
            "AlreadyRunning must not carry IPC error_data"
        );
    }

    /// `AlreadyRunning` with `owner_pid = None` must render `pid=?` in Display.
    #[test]
    fn already_running_owner_pid_none_display_contains_pid_question_mark() {
        let err = DaemonError::AlreadyRunning {
            owner_pid: None,
            socket: PathBuf::from("/tmp/sqryd.sock"),
            lock: PathBuf::from("/tmp/sqryd.lock"),
        };
        assert_eq!(err.exit_code(), 75);
        assert!(err.jsonrpc_code().is_none());
        let msg = err.to_string();
        assert!(
            msg.contains("pid=?"),
            "Display for owner_pid=None must contain 'pid=?', got: {msg}"
        );
    }

    /// `AutoStartTimeout` must have no JSON-RPC code and must exit with code
    /// 69 (`EX_UNAVAILABLE`). The design doc iter-0 m5 explicitly changed this
    /// from 73 (`EX_CANTCREAT`) to 69 (`EX_UNAVAILABLE`) — this test pins that
    /// decision and guards against accidental reversion.
    #[test]
    fn auto_start_timeout_has_no_jsonrpc_code_and_exit_69_not_73() {
        let err = DaemonError::AutoStartTimeout {
            timeout_secs: 10,
            socket: PathBuf::from("/run/user/1000/sqryd.sock"),
        };
        assert!(
            err.jsonrpc_code().is_none(),
            "AutoStartTimeout must not carry a JSON-RPC code"
        );
        assert_eq!(
            err.exit_code(),
            69,
            "AutoStartTimeout must exit with EX_UNAVAILABLE (69), NOT EX_CANTCREAT (73)"
        );
        assert!(
            err.error_data().is_none(),
            "AutoStartTimeout must not carry IPC error_data"
        );
    }

    /// `SignalSetup` must have no JSON-RPC code and must exit with code 70
    /// (`EX_SOFTWARE`).
    #[test]
    fn signal_setup_has_no_jsonrpc_code_and_exit_70() {
        let err = DaemonError::SignalSetup {
            source: std::io::Error::other("SIGTERM handler failed"),
        };
        assert!(
            err.jsonrpc_code().is_none(),
            "SignalSetup must not carry a JSON-RPC code"
        );
        assert_eq!(
            err.exit_code(),
            70,
            "SignalSetup must exit with EX_SOFTWARE (70)"
        );
        assert!(
            err.error_data().is_none(),
            "SignalSetup must not carry IPC error_data"
        );
    }

    /// `Config` must exit with code 78 (`EX_CONFIG`).
    #[test]
    fn config_exits_with_78() {
        let err = DaemonError::Config {
            path: PathBuf::from("/etc/sqry/daemon.toml"),
            source: anyhow::anyhow!("invalid TOML"),
        };
        assert_eq!(err.exit_code(), 78, "Config must exit with EX_CONFIG (78)");
        assert!(err.jsonrpc_code().is_none());
    }

    /// `Io` must exit with code 73 (`EX_CANTCREAT`).
    #[test]
    fn io_error_exits_with_73() {
        let err = DaemonError::Io(std::io::Error::other("socket bind failed"));
        assert_eq!(err.exit_code(), 73, "Io must exit with EX_CANTCREAT (73)");
        assert!(err.jsonrpc_code().is_none());
    }

    /// All IPC-path variants must have a defined exit code of 70 (the
    /// `EX_SOFTWARE` default). They should never reach process exit, but the
    /// method must be exhaustive.
    #[test]
    fn ipc_path_variants_exit_with_70_default() {
        let cases: &[DaemonError] = &[
            DaemonError::WorkspaceBuildFailed {
                root: PathBuf::from("/repo"),
                reason: "build failed".into(),
            },
            DaemonError::WorkspaceStaleExpired {
                root: PathBuf::from("/repo"),
                age_hours: 48,
                cap_hours: 24,
                last_good_at: None,
                last_error: None,
            },
            DaemonError::MemoryBudgetExceeded {
                limit_bytes: 1024 * 1024 * 1024,
                current_bytes: 512 * 1024 * 1024,
                reserved_bytes: 0,
                retained_bytes: 0,
                requested_bytes: 4 * 1024 * 1024 * 1024,
            },
            DaemonError::WorkspaceEvicted {
                root: PathBuf::from("/repo"),
            },
            DaemonError::WorkspaceIncompatibleGraph {
                root: PathBuf::from("/repo"),
                reason: "unknown plugin ids: [a]".into(),
            },
            DaemonError::ToolTimeout {
                root: PathBuf::from("/tmp/ws"),
                secs: 60,
                deadline_ms: 60_000,
            },
            DaemonError::InvalidArgument {
                reason: "missing path".into(),
            },
            DaemonError::Internal(anyhow::anyhow!("internal error")),
        ];
        for err in cases {
            assert_eq!(
                err.exit_code(),
                70,
                "IPC-path variant {err:?} must default to EX_SOFTWARE (70)"
            );
        }
    }

    /// `clone_err` must handle all three Task 9 lifecycle variants without
    /// panicking. All three collapse to `WorkspaceBuildFailed` (matching the
    /// pattern for `Config`/`Io`) because they fire before `IpcServer::bind`
    /// and should never reach workspace state storage — but the collapse must
    /// preserve the human-readable message.
    #[test]
    fn clone_err_handles_lifecycle_variants_without_panic() {
        use crate::workspace::manager::clone_err;

        let ar = DaemonError::AlreadyRunning {
            owner_pid: Some(42),
            socket: PathBuf::from("/tmp/sqryd.sock"),
            lock: PathBuf::from("/tmp/sqryd.lock"),
        };
        let cloned = clone_err(&ar);
        assert!(
            cloned.to_string().contains("sqryd.sock"),
            "clone_err for AlreadyRunning must preserve socket path, got: {cloned}"
        );

        // Must not panic with owner_pid=None.
        let ar_none = DaemonError::AlreadyRunning {
            owner_pid: None,
            socket: PathBuf::from("/tmp/sqryd.sock"),
            lock: PathBuf::from("/tmp/sqryd.lock"),
        };
        let _ = clone_err(&ar_none);

        let at = DaemonError::AutoStartTimeout {
            timeout_secs: 15,
            socket: PathBuf::from("/run/user/1000/sqryd.sock"),
        };
        let cloned = clone_err(&at);
        assert!(
            cloned.to_string().contains("15"),
            "clone_err for AutoStartTimeout must preserve timeout_secs, got: {cloned}"
        );

        let ss = DaemonError::SignalSetup {
            source: std::io::Error::other("SIGTERM handler failed"),
        };
        let cloned = clone_err(&ss);
        assert!(
            cloned.to_string().contains("SIGTERM handler failed"),
            "clone_err for SignalSetup must preserve the source message via Display, got: {cloned}"
        );
    }

    #[test]
    fn clone_err_round_trips_tool_dispatch_variants() {
        // `clone_err` lives in `workspace::manager` so it can be used
        // by `classify_for_serve` to reproduce the stored
        // `last_error` on every read path. The helper is
        // `pub(crate)` so we exercise it directly from inside the
        // daemon crate — Phase 8c U5 must keep all new variants
        // round-trippable or `classify_for_serve` will collapse them
        // into the generic `WorkspaceBuildFailed` fallback.
        use crate::workspace::manager::clone_err;

        let tt = DaemonError::ToolTimeout {
            root: PathBuf::from("/tmp/workspace"),
            secs: 60,
            deadline_ms: 60_000,
        };
        let cloned = clone_err(&tt);
        match cloned {
            DaemonError::ToolTimeout {
                root,
                secs,
                deadline_ms,
            } => {
                assert_eq!(root, PathBuf::from("/tmp/workspace"));
                assert_eq!(secs, 60);
                assert_eq!(deadline_ms, 60_000);
            }
            other => panic!("expected ToolTimeout round-trip, got {other:?}"),
        }

        let ia = DaemonError::InvalidArgument {
            reason: "missing path argument".into(),
        };
        let cloned = clone_err(&ia);
        match cloned {
            DaemonError::InvalidArgument { reason } => {
                assert_eq!(reason, "missing path argument");
            }
            other => panic!("expected InvalidArgument round-trip, got {other:?}"),
        }

        let inner = DaemonError::Internal(anyhow::anyhow!("something blew up"));
        let cloned = clone_err(&inner);
        match cloned {
            DaemonError::Internal(err) => {
                // `anyhow::Error` is not `Clone`; `clone_err`
                // re-creates it from the `Display` representation so
                // the user-facing message survives round-trips.
                assert!(
                    err.to_string().contains("something blew up"),
                    "cloned Internal error must preserve the Display text, got: {err}"
                );
            }
            other => panic!("expected Internal round-trip, got {other:?}"),
        }
    }
}
