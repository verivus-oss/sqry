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
    JSONRPC_ARTIFACT_KEY_MISMATCH, JSONRPC_CHECKOUT_FILTER_UNSUPPORTED,
    JSONRPC_DIRTY_SNAPSHOT_CHANGED, JSONRPC_INTERNAL_ERROR, JSONRPC_INVALID_PARAMS,
    JSONRPC_MANAGED_WORKTREE_IN_USE, JSONRPC_MEMORY_BUDGET_EXCEEDED, JSONRPC_QUERY_TOO_BROAD,
    JSONRPC_REBUILD_MACRO_OPTIONS_UNAVAILABLE, JSONRPC_REBUILD_WOULD_NARROW_SELECTION,
    JSONRPC_RESET_CANCELLATION_DISPATCHED, JSONRPC_RESET_WHILE_LOADING,
    JSONRPC_REVISION_DISK_BUDGET_EXCEEDED, JSONRPC_REVISION_OBJECT_MISSING,
    JSONRPC_REVISION_QUERY_REQUIRES_EXPLICIT_SELECTOR, JSONRPC_REVISION_SELECTOR_AMBIGUOUS,
    JSONRPC_REVISION_SOURCE_UNAVAILABLE, JSONRPC_SOCKET_SETUP, JSONRPC_SUBMODULE_UNAVAILABLE,
    JSONRPC_TOOL_TIMEOUT, JSONRPC_WORKSPACE_BUILD_FAILED, JSONRPC_WORKSPACE_EVICTED,
    JSONRPC_WORKSPACE_INCOMPATIBLE_GRAPH, JSONRPC_WORKSPACE_OVERSIZE, JSONRPC_WORKSPACE_PINNED,
    JSONRPC_WORKSPACE_STALE_EXPIRED,
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

/// Wire-stable `kind` tag for [`DaemonError::RebuildWouldNarrowSelection`]
/// on both the JSON-RPC `error.data` payload and the MCP envelope.
pub const KIND_REBUILD_WOULD_NARROW_SELECTION: &str = "rebuild_would_narrow_selection";

/// Wire-stable `kind` tag for [`DaemonError::RebuildMacroOptionsUnavailable`]
/// (surface parity W4, design W4-D7).
pub const KIND_REBUILD_MACRO_OPTIONS_UNAVAILABLE: &str = "rebuild_macro_options_unavailable";

/// A path as JSON text, rendered lossily when it is not valid UTF-8 (as the
/// standalone server renders the paths its refusals name). `json!` of a
/// `PathBuf` panics on such a path: its `Serialize` fails and the macro
/// unwraps. Every path an error's data names goes through here.
pub(crate) fn path_text(path: &std::path::Path) -> String {
    path.display().to_string()
}

/// The `daemon/rebuild` text of
/// [`DaemonError::RebuildMacroOptionsUnavailable`]: the core's reason for
/// the directory's shape, then the remedy that fits where it came from,
/// naming each wire field beside the `sqry daemon rebuild` flag.
fn rebuild_macro_options_unavailable_message(
    root: &std::path::Path,
    expand_cache_dir: &std::path::Path,
    origin: sqry_mcp::error::ExpandCacheOrigin,
) -> String {
    let reason = sqry_core::graph::unified::build::expand_cache_missing_reason(expand_cache_dir);
    let remedy = match origin {
        sqry_mcp::error::ExpandCacheOrigin::Requested => {
            "the request named it: pass expand_cache (--expand-cache) naming a directory that \
             exists, or omit it"
                .to_string()
        }
        sqry_mcp::error::ExpandCacheOrigin::Recorded => format!(
            "the index manifest records it: drop the record with reset_macro_options (sqry daemon \
             rebuild --no-macro-options {}), or pass expand_cache (--expand-cache) naming a \
             directory that exists",
            root.display()
        ),
    };
    format!("rebuild of {} refused: {reason}; {remedy}", root.display())
}

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
    /// The text is the standalone server's for the same failure
    /// (`RpcError::workspace_build_failed`), so the IPC and the MCP
    /// surfaces word it alike (F8, round 7); the root is `error.data.root`
    /// on both.
    ///
    /// Maps to JSON-RPC `-32001`.
    #[error("workspace build failed: {reason}")]
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

    /// A `daemon/rebuild` or daemon-hosted `rebuild_index` call did not
    /// receive the outcome of its own rebuild within
    /// `RebuildDispatcher::outcome_wait` (integration round 7). Unlike
    /// [`Self::ToolTimeout`] nothing was abandoned: the rebuild continues
    /// in the daemon and publishes (or records its failure) when it ends.
    /// Retrying the call would queue another rebuild, so the data says
    /// `retryable: false` and names the read that gives the outcome:
    /// `daemon/status` on IPC (the workspace leaves `Rebuilding`, with
    /// `last_good_at` or `last_error`), `rebuild_index` with
    /// `force: false` on MCP (it answers from the resident graph without
    /// building).
    ///
    /// Maps to JSON-RPC `-32000`, as [`Self::ToolTimeout`] does.
    #[error(
        "rebuild of {} did not answer within {deadline_ms}ms; the rebuild continues in the \
         daemon, so read its outcome rather than retry (a retry queues another rebuild)",
        root.display()
    )]
    RebuildOutcomeTimeout {
        root: PathBuf,
        secs: u64,
        /// Derived: `secs * 1000`, the bound the caller waited.
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
    ///
    /// The IPC message is the inner message alone, as the MCP surfaces send
    /// it (F8, round 7); `RpcError`'s own `Display` appends the kind, which
    /// is `error.data.kind` on both.
    #[error("{}", .0.message)]
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

    /// `daemon/reset` was invoked on a workspace whose rebuild runner
    /// holds the runner role (read under the rebuild lane, decision
    /// D-i7-3), so nothing was reset: the reset dispatched a cancellation
    /// to the runner, which consumes it, answers its parked requests
    /// `-32004` and releases the role. The caller retries after
    /// `retry_after_ms`, and the retry resets the workspace. Also answered,
    /// with nothing dispatched, when the lane stays held for
    /// `RESET_LANE_WAIT`.
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
    ///
    /// The text is the gate's own (`query rejected: ...`), as the
    /// standalone server sends it; a prefix here repeated it (F8, round 7).
    #[error("{reason}")]
    QueryTooBroad {
        reason: String,
        details: serde_json::Value,
    },

    /// Revision selector resolved to more than one candidate.
    ///
    /// Wire code: `-32011`.
    #[error("revision selector {selector} is ambiguous: {matches:?}")]
    RevisionSelectorAmbiguous {
        selector: String,
        matches: Vec<String>,
    },

    /// Required Git object is not present in the local object database.
    ///
    /// Wire code: `-32012`.
    #[error("revision object {object} is missing locally")]
    RevisionObjectMissing {
        object: String,
        path: Option<PathBuf>,
    },

    /// Revision source bytes cannot be read from the selected local source.
    ///
    /// Wire code: `-32013`.
    #[error("revision source unavailable: {reason}")]
    RevisionSourceUnavailable {
        reason: String,
        path: Option<PathBuf>,
    },

    /// Checkout-byte mode encountered a filter that cannot be reproduced.
    ///
    /// Wire code: `-32014`.
    #[error("checkout filter unsupported: {filter}")]
    CheckoutFilterUnsupported {
        filter: String,
        path: Option<PathBuf>,
    },

    /// Submodule gitlink cannot be indexed safely for this revision.
    ///
    /// Wire code: `-32015`.
    #[error("submodule unavailable at {}", path.display())]
    SubmoduleUnavailable {
        path: PathBuf,
        gitlink_oid: Option<String>,
    },

    /// Dirty snapshot changed during capture and failed the retry policy.
    ///
    /// Wire code: `-32016`.
    #[error("dirty snapshot changed during capture at {}", root.display())]
    DirtySnapshotChanged { root: PathBuf },

    /// Artifact id does not match manifest-verifiable inputs.
    ///
    /// Wire code: `-32017`.
    #[error("artifact key mismatch for {artifact_id}: {reason}")]
    ArtifactKeyMismatch { artifact_id: String, reason: String },

    /// Managed worktree is already leased or unsafe to reuse.
    ///
    /// Wire code: `-32018`.
    #[error("managed worktree {} is in use: {reason}", worktree.display())]
    ManagedWorktreeInUse { worktree: PathBuf, reason: String },

    /// Revision artifact disk budget would be exceeded.
    ///
    /// Wire code: `-32019`.
    #[error(
        "revision disk budget exceeded: requested {requested_bytes} B, current {current_bytes} B / limit {limit_bytes} B"
    )]
    RevisionDiskBudgetExceeded {
        limit_bytes: u64,
        requested_bytes: u64,
        current_bytes: u64,
    },

    /// Query route requires an explicit revision selector.
    ///
    /// Wire code: `-32020`.
    /// A rebuild was refused before anything was written because the plugin
    /// selection it would record drops ids the workspace manifest already
    /// records (surface parity W1, D5), or, with the manifest gone, ids the
    /// resident graph built from it records (integration round 7, S4). The
    /// resident graph and the on-disk index are untouched; `restore_command`
    /// is the `sqry index` invocation that rebuilds with the recorded
    /// selection.
    ///
    /// Maps to JSON-RPC `-32021`.
    #[error(
        "rebuild of {} refused: it would drop plugins [{}] the recorded plugin selection names \
         (the manifest's, or the resident graph's when the manifest is gone or unreadable); restore with: \
         {restore_command}",
        root.display(),
        missing_plugin_ids.join(", ")
    )]
    RebuildWouldNarrowSelection {
        root: PathBuf,
        missing_plugin_ids: Vec<String>,
        restore_command: String,
    },

    /// A rebuild was refused before anything was written because the macro
    /// build options it would run with (the manifest's record overlaid by
    /// the request) name an expand cache directory it cannot use: missing,
    /// a file, a dangling link (surface parity W4, design W4-D7). Building
    /// without it would silently drop every macro-generated symbol, so the
    /// resident graph keeps serving and the on-disk index is untouched.
    ///
    /// The reason is the core's, chosen by the directory's shape
    /// ([`sqry_core::graph::unified::build::expand_cache_missing_reason`]),
    /// and the remedy fits `origin` (S10, round 7): a directory the request
    /// named is fixed by naming one that exists, a recorded one also by
    /// dropping the record (`reset_macro_options`, `sqry daemon rebuild
    /// --no-macro-options`). The message names the wire fields MCP and IPC
    /// callers send beside the `sqry daemon rebuild` flags the CLI
    /// translates them from; the daemon-hosted MCP sends the standalone
    /// server's envelope instead
    /// ([`sqry_mcp::error::RpcError::rebuild_macro_options_unavailable`]).
    ///
    /// Maps to JSON-RPC `-32022`.
    #[error("{}", rebuild_macro_options_unavailable_message(root, expand_cache_dir, *origin))]
    RebuildMacroOptionsUnavailable {
        root: PathBuf,
        expand_cache_dir: PathBuf,
        /// Whether the request named the directory or the manifest records
        /// it ([`sqry_mcp::error::ExpandCacheOrigin::of`] the request).
        origin: sqry_mcp::error::ExpandCacheOrigin,
    },

    #[error("revision query requires an explicit selector: {reason}")]
    RevisionQueryRequiresExplicitSelector { reason: String },

    /// The workspace has an index but its manifest cannot be read, so the
    /// recorded plugin selection is unknown and the daemon refuses to bring
    /// the workspace up or to write over the file (surface parity W1 round
    /// 2, design D9 and D12). Nothing was written; the resident graph, if
    /// any, is intact. `manifest_path` names the file and the message names
    /// the repair, `sqry index --force <root>`.
    ///
    /// Shares JSON-RPC `-32001` with [`Self::WorkspaceBuildFailed`] ("the
    /// workspace cannot be brought up"); no new wire code. The variant is
    /// distinct so the rebuild dispatcher can return a refused workspace to
    /// the state it entered from instead of recording a build failure.
    #[error(
        "manifest at {} cannot be read ({reason}); repair with: sqry index --force {}",
        manifest_path.display(),
        root.display()
    )]
    WorkspaceManifestUnreadable {
        root: PathBuf,
        manifest_path: PathBuf,
        reason: String,
    },

    /// A read-only query reached a workspace the daemon does not hold, and
    /// the workspace has no persisted graph to load: its index manifest is
    /// absent (it was never indexed), or the manifest is present and the
    /// snapshot it describes is absent. `missing_path` names the absent
    /// file and `repair_command` the `sqry index` invocation that creates
    /// it (`--force` when a manifest is already there, because without it
    /// `sqry index` reports the existing index and writes nothing); an MCP
    /// caller repairs with the `rebuild_index` tool. Nothing was written.
    ///
    /// Shares JSON-RPC `-32001` with [`Self::WorkspaceBuildFailed`] and
    /// [`Self::WorkspaceManifestUnreadable`] ("the workspace cannot be
    /// brought up"), as the unreadable manifest does (surface parity W1
    /// round 3, design D15); the variant is distinct so the wire carries
    /// the missing file and the repair as their own `error.data` keys.
    #[error(
        "workspace {} is not indexed: {} is absent; index it with: {repair_command} \
         (MCP clients: the rebuild_index tool)",
        root.display(),
        missing_path.display()
    )]
    WorkspaceNotIndexed {
        root: PathBuf,
        missing_path: PathBuf,
        repair_command: String,
    },

    /// The workspace's snapshot exists but cannot be loaded: corrupt,
    /// truncated, failing its integrity check or unreadable (integration
    /// round 7, DAEMON_FOLLOWUP). The load (or the reload after an
    /// eviction) refused it before anything was published. Typed so the
    /// acquirer treats it as a refusal of the on-disk index that a repair
    /// on disk clears: once `sqry index --force <root>` has rewritten the
    /// snapshot, the next query loads it, where before the recorded
    /// `WorkspaceBuildFailed` was answered until a `daemon/load`.
    ///
    /// Shares JSON-RPC `-32001` with [`Self::WorkspaceBuildFailed`].
    #[error(
        "workspace {} snapshot {} cannot be loaded ({reason}); repair with: sqry index --force {} \
         (MCP clients: the rebuild_index tool with force: true)",
        root.display(),
        snapshot_path.display(),
        root.display()
    )]
    WorkspaceSnapshotUnreadable {
        root: PathBuf,
        snapshot_path: PathBuf,
        reason: String,
    },

    /// A workspace the daemon had loaded was evicted, and the bounded
    /// read-only reload a query runs to bring it back failed.
    /// `reload_failure` is that reload's own error, rendered.
    ///
    /// Shares JSON-RPC `-32004` with [`Self::WorkspaceEvicted`]: the
    /// workspace is not resident and the caller must load it again
    /// (`daemon/load`, or `rebuild_index` from MCP). The variant is
    /// distinct so the reason the automatic reload failed reaches the wire
    /// (`error.data.reload_failure`) instead of the bare "evicted
    /// mid-rebuild" that names no cause.
    #[error(
        "workspace {} was evicted and its reload from the persisted graph failed: {reload_failure}",
        root.display()
    )]
    WorkspaceReloadFailed {
        root: PathBuf,
        reload_failure: String,
    },
}

/// The `rebuild_index` tool, the MCP caller's repair for
/// [`DaemonError::WorkspaceNotIndexed`]. Carried as
/// `error.data.repair_tool` and `details.repair_tool`.
pub const REPAIR_TOOL_REBUILD_INDEX: &str = "rebuild_index";

impl DaemonError {
    /// [`Self::WorkspaceNotIndexed`] for a root with no index manifest:
    /// the repair is `sqry index <root>`.
    #[must_use]
    pub fn workspace_not_indexed(root: &Path, manifest_path: PathBuf) -> Self {
        Self::WorkspaceNotIndexed {
            root: root.to_path_buf(),
            missing_path: manifest_path,
            repair_command: format!("sqry index {}", root.display()),
        }
    }

    /// [`Self::WorkspaceNotIndexed`] for a root whose manifest is present
    /// and whose snapshot is absent: the repair is
    /// `sqry index --force <root>`, because `sqry index` without `--force`
    /// reports the manifest as an existing index and builds nothing.
    #[must_use]
    pub fn workspace_snapshot_missing(root: &Path, snapshot_path: PathBuf) -> Self {
        Self::WorkspaceNotIndexed {
            root: root.to_path_buf(),
            missing_path: snapshot_path,
            repair_command: format!("sqry index --force {}", root.display()),
        }
    }
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
            Self::WorkspaceBuildFailed { .. } | Self::WorkspaceManifestUnreadable { .. } => {
                Some(JSONRPC_WORKSPACE_BUILD_FAILED)
            }
            Self::WorkspaceNotIndexed { .. } | Self::WorkspaceSnapshotUnreadable { .. } => {
                Some(JSONRPC_WORKSPACE_BUILD_FAILED)
            }
            Self::WorkspaceReloadFailed { .. } => Some(JSONRPC_WORKSPACE_EVICTED),
            Self::WorkspaceStaleExpired { .. } => Some(JSONRPC_WORKSPACE_STALE_EXPIRED),
            Self::MemoryBudgetExceeded { .. } => Some(JSONRPC_MEMORY_BUDGET_EXCEEDED),
            Self::WorkspaceEvicted { .. } | Self::WorkspaceNotLoaded { .. } => {
                Some(JSONRPC_WORKSPACE_EVICTED)
            }
            Self::WorkspaceIncompatibleGraph { .. } => Some(JSONRPC_WORKSPACE_INCOMPATIBLE_GRAPH),
            Self::ToolTimeout { .. } | Self::RebuildOutcomeTimeout { .. } => {
                Some(JSONRPC_TOOL_TIMEOUT)
            }
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
            Self::RevisionSelectorAmbiguous { .. } => Some(JSONRPC_REVISION_SELECTOR_AMBIGUOUS),
            Self::RevisionObjectMissing { .. } => Some(JSONRPC_REVISION_OBJECT_MISSING),
            Self::RevisionSourceUnavailable { .. } => Some(JSONRPC_REVISION_SOURCE_UNAVAILABLE),
            Self::CheckoutFilterUnsupported { .. } => Some(JSONRPC_CHECKOUT_FILTER_UNSUPPORTED),
            Self::SubmoduleUnavailable { .. } => Some(JSONRPC_SUBMODULE_UNAVAILABLE),
            Self::DirtySnapshotChanged { .. } => Some(JSONRPC_DIRTY_SNAPSHOT_CHANGED),
            Self::ArtifactKeyMismatch { .. } => Some(JSONRPC_ARTIFACT_KEY_MISMATCH),
            Self::ManagedWorktreeInUse { .. } => Some(JSONRPC_MANAGED_WORKTREE_IN_USE),
            Self::RevisionDiskBudgetExceeded { .. } => Some(JSONRPC_REVISION_DISK_BUDGET_EXCEEDED),
            Self::RevisionQueryRequiresExplicitSelector { .. } => {
                Some(JSONRPC_REVISION_QUERY_REQUIRES_EXPLICIT_SELECTOR)
            }
            Self::RebuildWouldNarrowSelection { .. } => {
                Some(JSONRPC_REBUILD_WOULD_NARROW_SELECTION)
            }
            Self::RebuildMacroOptionsUnavailable { .. } => {
                Some(JSONRPC_REBUILD_MACRO_OPTIONS_UNAVAILABLE)
            }
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
            | Self::RebuildOutcomeTimeout { .. }
            | Self::InvalidArgument { .. }
            | Self::RpcErrorPreserved(_)
            | Self::Internal(_)
            | Self::WorkspaceOversize { .. }
            | Self::WorkspacePinned { .. }
            | Self::ResetWhileLoading { .. }
            | Self::ResetCancellationDispatched { .. }
            | Self::SocketSetup { .. }
            | Self::QueryTooBroad { .. }
            | Self::RevisionSelectorAmbiguous { .. }
            | Self::RevisionObjectMissing { .. }
            | Self::RevisionSourceUnavailable { .. }
            | Self::CheckoutFilterUnsupported { .. }
            | Self::SubmoduleUnavailable { .. }
            | Self::DirtySnapshotChanged { .. }
            | Self::ArtifactKeyMismatch { .. }
            | Self::ManagedWorktreeInUse { .. }
            | Self::RevisionDiskBudgetExceeded { .. }
            | Self::RevisionQueryRequiresExplicitSelector { .. }
            | Self::RebuildWouldNarrowSelection { .. }
            | Self::RebuildMacroOptionsUnavailable { .. }
            | Self::WorkspaceManifestUnreadable { .. }
            | Self::WorkspaceNotIndexed { .. }
            | Self::WorkspaceSnapshotUnreadable { .. }
            | Self::WorkspaceReloadFailed { .. } => 70,
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
        match self {
            Self::QueryTooBroad { .. }
            | Self::RevisionSelectorAmbiguous { .. }
            | Self::RevisionObjectMissing { .. }
            | Self::RevisionSourceUnavailable { .. }
            | Self::CheckoutFilterUnsupported { .. }
            | Self::SubmoduleUnavailable { .. }
            | Self::DirtySnapshotChanged { .. }
            | Self::ArtifactKeyMismatch { .. }
            | Self::ManagedWorktreeInUse { .. }
            | Self::RevisionDiskBudgetExceeded { .. }
            | Self::RevisionQueryRequiresExplicitSelector { .. } => revision_error_data(self),
            _ => workspace_error_data(self),
        }
    }
}

fn workspace_error_data(err: &DaemonError) -> Option<serde_json::Value> {
    use serde_json::json;
    match err {
        DaemonError::MemoryBudgetExceeded {
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
        DaemonError::WorkspaceStaleExpired {
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
        DaemonError::WorkspaceBuildFailed { root, reason }
        | DaemonError::WorkspaceIncompatibleGraph { root, reason } => Some(json!({
                "root": path_text(root),
                "reason": reason,
        })),
        DaemonError::WorkspaceEvicted { root } => Some(json!({ "root": path_text(root) })),
        DaemonError::WorkspaceNotLoaded { root } => Some(json!({
            "root": path_text(root),
            "hint": "use daemon/load to load the workspace before calling daemon/rebuild",
        })),
        DaemonError::ToolTimeout { deadline_ms, .. } => Some(tool_timeout_data(*deadline_ms)),
        DaemonError::RebuildOutcomeTimeout { deadline_ms, .. } => {
            Some(rebuild_outcome_timeout_data(*deadline_ms, None))
        }
        DaemonError::InvalidArgument { reason } => Some(json!({
            "kind": "validation_error",
            "retryable": false,
            "retry_after_ms": serde_json::Value::Null,
            "details": {
                "reason": reason,
            },
        })),
        DaemonError::RpcErrorPreserved(rpc) => Some(rpc_error_data(rpc)),
        DaemonError::Internal(_) => Some(json!({
            "kind": "internal",
            "retryable": false,
            "retry_after_ms": serde_json::Value::Null,
            "details": serde_json::Value::Null,
        })),
        DaemonError::WorkspaceOversize {
            root,
            measured_bytes,
            limit_bytes,
            current_loaded_bytes,
        } => Some(json!({
            "root": path_text(root),
            "measured_bytes": measured_bytes,
            "limit_bytes": limit_bytes,
            "current_loaded_bytes": current_loaded_bytes,
        })),
        DaemonError::WorkspacePinned { root } => Some(json!({
            "root": path_text(root),
            "hint": "pass force=true to reset a pinned workspace",
        })),
        DaemonError::ResetWhileLoading { root } => Some(json!({
            "root": path_text(root),
            "hint": "wait for the load to settle, then retry",
        })),
        DaemonError::ResetCancellationDispatched {
            root,
            retry_after_ms,
        } => Some(json!({
            "root": path_text(root),
            "retry_after_ms": retry_after_ms,
        })),
        DaemonError::SocketSetup { path, reason } => Some(json!({
            "path": path_text(path),
            "reason": reason,
        })),
        DaemonError::RebuildWouldNarrowSelection {
            root,
            missing_plugin_ids,
            restore_command,
        } => Some(json!({
            "kind": KIND_REBUILD_WOULD_NARROW_SELECTION,
            "retryable": false,
            "root": path_text(root),
            "missing_plugin_ids": missing_plugin_ids,
            "restore_command": restore_command,
        })),
        DaemonError::RebuildMacroOptionsUnavailable {
            root,
            expand_cache_dir,
            origin,
        } => {
            let mut data = json!({
                "kind": KIND_REBUILD_MACRO_OPTIONS_UNAVAILABLE,
                "retryable": false,
                "root": path_text(root),
                "expand_cache_dir": path_text(expand_cache_dir),
                "origin": origin.as_str(),
            });
            // Dropping the record is a way out only for a recorded
            // directory; a requested one is fixed by naming one that exists.
            if *origin == sqry_mcp::error::ExpandCacheOrigin::Recorded {
                data["reset_command"] = json!(format!(
                    "sqry daemon rebuild --no-macro-options {}",
                    root.display()
                ));
            }
            Some(data)
        }
        // Same `{root, reason}` shape as `WorkspaceBuildFailed` (they share
        // `-32001`), with the manifest and the repair command as their own
        // keys so a client need not parse `reason`. `reason` is the full
        // sentence so a client that only reads that key still sees both.
        DaemonError::WorkspaceManifestUnreadable {
            root,
            manifest_path,
            ..
        } => Some(json!({
            "root": path_text(root),
            "reason": err.to_string(),
            "manifest_path": path_text(manifest_path),
            "repair_command": format!("sqry index --force {}", root.display()),
        })),
        // The D15 shape for the absent index: `{root, reason}` as every
        // `-32001` carries, then the absent file and the two repairs (the
        // CLI command and the MCP tool) as their own keys.
        DaemonError::WorkspaceNotIndexed {
            root,
            missing_path,
            repair_command,
        } => Some(json!({
            "root": path_text(root),
            "reason": err.to_string(),
            "missing_path": path_text(missing_path),
            "repair_command": repair_command,
            "repair_tool": REPAIR_TOOL_REBUILD_INDEX,
        })),
        // The `-32001` `{root, reason}` shape, with the snapshot and the
        // two repairs as their own keys, as `WorkspaceNotIndexed` carries.
        DaemonError::WorkspaceSnapshotUnreadable {
            root,
            snapshot_path,
            ..
        } => Some(json!({
            "root": path_text(root),
            "reason": err.to_string(),
            "snapshot_path": path_text(snapshot_path),
            "repair_command": format!("sqry index --force {}", root.display()),
            "repair_tool": REPAIR_TOOL_REBUILD_INDEX,
        })),
        // `{root}` as `WorkspaceEvicted` carries, plus the reload's failure.
        DaemonError::WorkspaceReloadFailed {
            root,
            reload_failure,
        } => Some(json!({
            "root": path_text(root),
            "reload_failure": reload_failure,
        })),
        _ => None,
    }
}

fn revision_error_data(err: &DaemonError) -> Option<serde_json::Value> {
    use serde_json::json;
    match err {
        DaemonError::QueryTooBroad { details, .. } => Some(query_too_broad_data(details)),
        DaemonError::RevisionSelectorAmbiguous { selector, matches } => Some(json!({
            "kind": "revision_selector_ambiguous",
            "retryable": false,
            "selector": selector,
            "matches": matches,
        })),
        DaemonError::RevisionObjectMissing { object, path } => Some(json!({
            "kind": "revision_object_missing",
            "retryable": false,
            "object": object,
            "path": path.as_deref().map(path_text),
            "hint": "fetch or provide the missing Git object explicitly; sqryd does not fetch implicitly",
        })),
        DaemonError::RevisionSourceUnavailable { reason, path } => Some(json!({
            "kind": "revision_source_unavailable",
            "retryable": false,
            "reason": reason,
            "path": path.as_deref().map(path_text),
        })),
        DaemonError::CheckoutFilterUnsupported { filter, path } => Some(json!({
            "kind": "checkout_filter_unsupported",
            "retryable": false,
            "filter": filter,
            "path": path.as_deref().map(path_text),
            "hint": "use raw_git_objects mode or configure a supported explicit checkout-byte source",
        })),
        DaemonError::SubmoduleUnavailable { path, gitlink_oid } => Some(json!({
            "kind": "submodule_unavailable",
            "retryable": false,
            "path": path_text(path),
            "gitlink_oid": gitlink_oid,
        })),
        DaemonError::DirtySnapshotChanged { root } => Some(json!({
            "kind": "dirty_snapshot_changed",
            "retryable": true,
            "root": path_text(root),
            "hint": "retry after file writes settle",
        })),
        DaemonError::ArtifactKeyMismatch {
            artifact_id,
            reason,
        } => Some(json!({
            "kind": "artifact_key_mismatch",
            "retryable": false,
            "artifact_id": artifact_id,
            "reason": reason,
        })),
        DaemonError::ManagedWorktreeInUse { worktree, reason } => Some(json!({
            "kind": "managed_worktree_in_use",
            "retryable": true,
            "worktree": path_text(worktree),
            "reason": reason,
        })),
        DaemonError::RevisionDiskBudgetExceeded {
            limit_bytes,
            requested_bytes,
            current_bytes,
        } => Some(json!({
            "kind": "revision_disk_budget_exceeded",
            "retryable": false,
            "limit_bytes": limit_bytes,
            "requested_bytes": requested_bytes,
            "current_bytes": current_bytes,
        })),
        DaemonError::RevisionQueryRequiresExplicitSelector { reason } => Some(json!({
            "kind": "revision_query_requires_explicit_selector",
            "retryable": false,
            "reason": reason,
        })),
        _ => None,
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
        "root": path_text(root),
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

/// The data of [`DaemonError::RebuildOutcomeTimeout`]: the deadline kind,
/// not retryable (a retry queues another rebuild), and the read that gives
/// the outcome of the rebuild that continues. `tool` is the MCP tool name,
/// `None` on IPC, where the read is `daemon/status`.
pub(crate) fn rebuild_outcome_timeout_data(
    deadline_ms: u64,
    tool: Option<&str>,
) -> serde_json::Value {
    use serde_json::json;
    let follow_with = if tool.is_some() {
        "rebuild_index with force: false (answers from the resident graph without building)"
    } else {
        "daemon/status (the workspace leaves Rebuilding, with last_good_at or last_error)"
    };
    json!({
        "kind": "deadline_exceeded",
        "retryable": false,
        "retry_after_ms": serde_json::Value::Null,
        "details": {
            "tool": tool,
            "deadline_ms": deadline_ms,
            "rebuild_continues": true,
            "follow_with": follow_with,
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
                    // Served with a warning on the acquisition path, so it
                    // reaches this arm only if a caller wraps it in
                    // `IncompatibleGraph` explicitly; render it losslessly.
                    PluginSelectionStatus::DivergesFromManifest {
                        missing_plugin_ids,
                        extra_plugin_ids,
                        manifest_path,
                    } => {
                        let mut buf = format!(
                            "resident graph diverges from manifest: missing [{}], extra [{}]",
                            missing_plugin_ids.join(", "),
                            extra_plugin_ids.join(", "),
                        );
                        if let Some(p) = manifest_path.as_ref() {
                            let _ = write!(buf, " (manifest: {})", p.display());
                        }
                        buf
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
                tracing::warn!(
                    workspace = %workspace_root.display(),
                    original_lifecycle = %original_lifecycle,
                    reload_failure = ?reload_failure,
                    "graph acquisition: workspace evicted, reload failed"
                );
                // The reload's failure is the reason the caller cannot be
                // served, so it reaches the wire: `-32004` with
                // `error.data.reload_failure`. Before this arm carried it,
                // the conversion kept only the root and the wire read
                // "evicted mid-rebuild" whatever the reload had said. An
                // eviction with no reload attempt keeps the bare variant.
                match reload_failure {
                    Some(reload_failure) => Self::WorkspaceReloadFailed {
                        root: workspace_root,
                        reload_failure,
                    },
                    None => Self::WorkspaceEvicted {
                        root: workspace_root,
                    },
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
pub(crate) mod tests {
    use super::*;

    /// Every `DaemonError` variant that names a path, each with a path that
    /// is not valid UTF-8. The set is the enum's path-typed fields as of
    /// round 7 (`PathBuf` and `Option<PathBuf>`), listed in the order the
    /// enum declares them.
    #[cfg(unix)]
    pub(crate) fn path_bearing_errors_with_a_non_utf8_path() -> Vec<DaemonError> {
        use std::os::unix::ffi::OsStringExt;
        let bad = |name: &str| {
            let mut bytes = format!("/repo/{name}-").into_bytes();
            bytes.push(0xff);
            PathBuf::from(std::ffi::OsString::from_vec(bytes))
        };
        vec![
            DaemonError::WorkspaceBuildFailed {
                root: bad("root"),
                reason: "r".into(),
            },
            DaemonError::WorkspaceStaleExpired {
                root: bad("root"),
                age_hours: 2,
                cap_hours: 1,
                last_good_at: None,
                last_error: None,
            },
            DaemonError::WorkspaceEvicted { root: bad("root") },
            DaemonError::WorkspaceNotLoaded { root: bad("root") },
            DaemonError::WorkspaceIncompatibleGraph {
                root: bad("root"),
                reason: "r".into(),
            },
            DaemonError::ToolTimeout {
                root: bad("root"),
                secs: 1,
                deadline_ms: 1000,
            },
            DaemonError::RebuildOutcomeTimeout {
                root: bad("root"),
                secs: 1,
                deadline_ms: 1000,
            },
            DaemonError::WorkspaceOversize {
                root: bad("root"),
                measured_bytes: 2,
                limit_bytes: 1,
                current_loaded_bytes: 0,
            },
            DaemonError::WorkspacePinned { root: bad("root") },
            DaemonError::ResetWhileLoading { root: bad("root") },
            DaemonError::ResetCancellationDispatched {
                root: bad("root"),
                retry_after_ms: 100,
            },
            DaemonError::SocketSetup {
                path: bad("socket"),
                reason: "r".into(),
            },
            DaemonError::RevisionObjectMissing {
                object: "o".into(),
                path: Some(bad("object")),
            },
            DaemonError::RevisionSourceUnavailable {
                reason: "r".into(),
                path: Some(bad("source")),
            },
            DaemonError::CheckoutFilterUnsupported {
                filter: "f".into(),
                path: Some(bad("filtered")),
            },
            DaemonError::SubmoduleUnavailable {
                path: bad("submodule"),
                gitlink_oid: None,
            },
            DaemonError::DirtySnapshotChanged { root: bad("root") },
            DaemonError::ManagedWorktreeInUse {
                worktree: bad("worktree"),
                reason: "r".into(),
            },
            DaemonError::RebuildWouldNarrowSelection {
                root: bad("root"),
                missing_plugin_ids: vec!["json".into()],
                restore_command: "c".into(),
            },
            DaemonError::RebuildMacroOptionsUnavailable {
                root: bad("root"),
                expand_cache_dir: bad("cache"),
                origin: sqry_mcp::error::ExpandCacheOrigin::Recorded,
            },
            DaemonError::WorkspaceManifestUnreadable {
                root: bad("root"),
                manifest_path: bad("manifest"),
                reason: "r".into(),
            },
            DaemonError::WorkspaceNotIndexed {
                root: bad("root"),
                missing_path: bad("snapshot"),
                repair_command: "c".into(),
            },
            DaemonError::WorkspaceSnapshotUnreadable {
                root: bad("root"),
                snapshot_path: bad("snapshot"),
                reason: "r".into(),
            },
            DaemonError::WorkspaceReloadFailed {
                root: bad("root"),
                reload_failure: "r".into(),
            },
        ]
    }

    /// A path that is not valid UTF-8 is rendered lossily in an error's
    /// `data`, never a panic: `json!` of a `PathBuf` unwraps a `Serialize`
    /// that fails on such a path, so the IPC error for a workspace whose
    /// root is not UTF-8 crashed the request's handler instead of answering
    /// it. Each variant above answers, and its data names the path.
    #[cfg(unix)]
    #[test]
    fn error_data_renders_a_non_utf8_path() {
        let errors = path_bearing_errors_with_a_non_utf8_path();
        let mut named = 0;
        for err in &errors {
            let label = format!("{err:?}");
            let data = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| err.error_data()))
                .unwrap_or_else(|_| panic!("error_data panicked for {label}"));
            let text = data.map(|value| value.to_string()).unwrap_or_default();
            if text.contains("/repo/") {
                assert!(
                    text.contains('\u{fffd}'),
                    "{label}: the path is rendered lossily: {text}"
                );
                named += 1;
            }
        }
        // The two timeouts' data is the deadline alone; every other variant
        // names its path.
        assert_eq!(
            named,
            errors.len() - 2,
            "every path-naming variant was rendered"
        );
    }

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

        // Surface parity W1 (D5): the narrowing refusal has its own code,
        // distinct from every neighbour, and a data payload that names the
        // ids and the restore command so the caller can act without
        // parsing `message`.
        let narrow = DaemonError::RebuildWouldNarrowSelection {
            root: PathBuf::from("/repo"),
            missing_plugin_ids: vec!["json".to_string()],
            restore_command: "sqry index --force --include-high-cost /repo".to_string(),
        };
        assert_eq!(
            narrow.jsonrpc_code(),
            Some(JSONRPC_REBUILD_WOULD_NARROW_SELECTION)
        );
        assert_eq!(narrow.jsonrpc_code(), Some(-32021));
        assert_ne!(
            narrow.jsonrpc_code(),
            Some(JSONRPC_REVISION_QUERY_REQUIRES_EXPLICIT_SELECTOR),
            "-32020 is already owned by the revision-selector error at this head"
        );
        assert_ne!(narrow.jsonrpc_code(), Some(JSONRPC_WORKSPACE_BUILD_FAILED));
        assert_ne!(narrow.jsonrpc_code(), Some(JSONRPC_INVALID_PARAMS));
        let data = narrow
            .error_data()
            .expect("RebuildWouldNarrowSelection must emit error_data");
        assert_eq!(data["kind"], KIND_REBUILD_WOULD_NARROW_SELECTION);
        assert_eq!(data["retryable"], false);
        assert_eq!(data["root"], "/repo");
        assert_eq!(data["missing_plugin_ids"], serde_json::json!(["json"]));
        assert_eq!(
            data["restore_command"],
            "sqry index --force --include-high-cost /repo"
        );
        assert_eq!(narrow.exit_code(), 70);
        let rendered = narrow.to_string();
        assert!(
            rendered.contains("[json]") && rendered.contains("--include-high-cost"),
            "Display must name the dropped ids and the restore command: {rendered}"
        );

        // Surface parity W4 (W4-D7): the macro-options refusal takes the
        // next unused code after -32021, distinct from every neighbour, and
        // its data names the directory and the reset command.
        let macro_refusal = DaemonError::RebuildMacroOptionsUnavailable {
            root: PathBuf::from("/repo"),
            expand_cache_dir: PathBuf::from("/repo/.expand-cache"),
            origin: sqry_mcp::error::ExpandCacheOrigin::Recorded,
        };
        assert_eq!(
            macro_refusal.jsonrpc_code(),
            Some(JSONRPC_REBUILD_MACRO_OPTIONS_UNAVAILABLE)
        );
        assert_eq!(macro_refusal.jsonrpc_code(), Some(-32022));
        assert_ne!(macro_refusal.jsonrpc_code(), narrow.jsonrpc_code());
        assert_ne!(
            macro_refusal.jsonrpc_code(),
            Some(JSONRPC_WORKSPACE_BUILD_FAILED)
        );
        assert_ne!(macro_refusal.jsonrpc_code(), Some(JSONRPC_INVALID_PARAMS));
        let data = macro_refusal
            .error_data()
            .expect("RebuildMacroOptionsUnavailable must emit error_data");
        assert_eq!(data["kind"], KIND_REBUILD_MACRO_OPTIONS_UNAVAILABLE);
        assert_eq!(data["retryable"], false);
        assert_eq!(data["root"], "/repo");
        assert_eq!(data["expand_cache_dir"], "/repo/.expand-cache");
        assert_eq!(
            data["reset_command"],
            "sqry daemon rebuild --no-macro-options /repo"
        );
        assert_eq!(macro_refusal.exit_code(), 70);
        let rendered = macro_refusal.to_string();
        assert!(
            rendered.contains("/repo/.expand-cache") && rendered.contains("--no-macro-options"),
            "Display must name the directory and the reset command: {rendered}"
        );
        assert_eq!(data["origin"], "recorded");
        // S10 (round 7): the reason is the core's, by shape, and a
        // directory the request named is fixed by naming one that exists,
        // so no reset is offered for it.
        assert!(
            rendered.starts_with(&format!(
                "rebuild of /repo refused: {};",
                sqry_core::graph::unified::build::expand_cache_missing_reason(
                    std::path::Path::new("/repo/.expand-cache")
                )
            )),
            "{rendered}"
        );
        let requested = DaemonError::RebuildMacroOptionsUnavailable {
            root: PathBuf::from("/repo"),
            expand_cache_dir: PathBuf::from("/repo/.expand-cache"),
            origin: sqry_mcp::error::ExpandCacheOrigin::Requested,
        };
        let data = requested.error_data().expect("error_data");
        assert_eq!(data["origin"], "requested");
        assert!(data.get("reset_command").is_none(), "{data}");
        let rendered = requested.to_string();
        assert!(
            rendered.contains("expand_cache (--expand-cache)")
                && !rendered.contains("--no-macro-options"),
            "{rendered}"
        );

        // Surface parity W1 round 2 (D9, D12): an unreadable manifest is
        // refused with the existing `-32001`, no new code; the Display and
        // the data payload both name the manifest and the repair command.
        let unreadable = DaemonError::WorkspaceManifestUnreadable {
            root: PathBuf::from("/repo"),
            manifest_path: PathBuf::from("/repo/.sqry/graph/manifest.json"),
            reason: "missing field `schema_version`".to_string(),
        };
        assert_eq!(
            unreadable.jsonrpc_code(),
            Some(JSONRPC_WORKSPACE_BUILD_FAILED)
        );
        assert_eq!(unreadable.jsonrpc_code(), Some(-32001));
        assert_ne!(
            unreadable.jsonrpc_code(),
            Some(JSONRPC_WORKSPACE_INCOMPATIBLE_GRAPH)
        );
        assert_eq!(unreadable.exit_code(), 70);
        let rendered = unreadable.to_string();
        assert_eq!(
            rendered,
            "manifest at /repo/.sqry/graph/manifest.json cannot be read (missing field `schema_version`); repair with: sqry index --force /repo"
        );
        let data = unreadable
            .error_data()
            .expect("WorkspaceManifestUnreadable must emit error_data");
        assert_eq!(data["root"], "/repo");
        assert_eq!(data["reason"], rendered);
        assert_eq!(data["manifest_path"], "/repo/.sqry/graph/manifest.json");
        assert_eq!(data["repair_command"], "sqry index --force /repo");
    }

    /// The absent index shares `-32001` with the build failure and the
    /// unreadable manifest (design D15's precedent), and its data carries
    /// the `{root, reason}` every `-32001` carries plus the absent file and
    /// both repairs; the snapshot-only shape names `--force`.
    #[test]
    fn workspace_not_indexed_is_32001_naming_the_absent_file_and_the_repair() {
        let root = PathBuf::from("/repo");
        let manifest = PathBuf::from("/repo/.sqry/graph/manifest.json");
        let err = DaemonError::workspace_not_indexed(&root, manifest.clone());
        assert_eq!(err.jsonrpc_code(), Some(JSONRPC_WORKSPACE_BUILD_FAILED));
        assert_eq!(err.jsonrpc_code(), Some(-32001));
        assert_ne!(err.jsonrpc_code(), Some(JSONRPC_WORKSPACE_EVICTED));
        assert_eq!(err.exit_code(), 70);
        let rendered = err.to_string();
        assert_eq!(
            rendered,
            "workspace /repo is not indexed: /repo/.sqry/graph/manifest.json is absent; \
             index it with: sqry index /repo (MCP clients: the rebuild_index tool)"
        );
        let data = err
            .error_data()
            .expect("WorkspaceNotIndexed must emit data");
        assert_eq!(data["root"], "/repo");
        assert_eq!(data["reason"], rendered);
        assert_eq!(data["missing_path"], "/repo/.sqry/graph/manifest.json");
        assert_eq!(data["repair_command"], "sqry index /repo");
        assert_eq!(data["repair_tool"], "rebuild_index");
        assert_eq!(data.as_object().map(serde_json::Map::len), Some(5));

        let snapshot = PathBuf::from("/repo/.sqry/graph/snapshot.sqry");
        let err = DaemonError::workspace_snapshot_missing(&root, snapshot);
        let data = err.error_data().expect("data");
        assert_eq!(data["missing_path"], "/repo/.sqry/graph/snapshot.sqry");
        assert_eq!(data["repair_command"], "sqry index --force /repo");
        assert!(err.to_string().contains("sqry index --force /repo"));
    }

    /// A failed reload after an eviction reaches the wire as `-32004`
    /// carrying the reload's failure; an eviction with no reload attempt
    /// keeps the bare `WorkspaceEvicted` and its `{root}` data.
    #[test]
    fn from_graph_acquisition_evicted_carries_the_reload_failure() {
        let carried: DaemonError = GraphAcquisitionError::Evicted {
            workspace_root: PathBuf::from("/repo"),
            original_lifecycle: "evicted".to_string(),
            reload_failure: Some("workspace /repo build failed: snapshot load failed".into()),
        }
        .into();
        assert_eq!(carried.jsonrpc_code(), Some(JSONRPC_WORKSPACE_EVICTED));
        assert_eq!(carried.exit_code(), 70);
        assert_eq!(
            carried.to_string(),
            "workspace /repo was evicted and its reload from the persisted graph failed: \
             workspace /repo build failed: snapshot load failed"
        );
        let data = carried.error_data().expect("data");
        assert_eq!(data["root"], "/repo");
        assert_eq!(
            data["reload_failure"],
            "workspace /repo build failed: snapshot load failed"
        );

        let bare: DaemonError = GraphAcquisitionError::Evicted {
            workspace_root: PathBuf::from("/repo"),
            original_lifecycle: "evicted".to_string(),
            reload_failure: None,
        }
        .into();
        assert!(
            matches!(bare, DaemonError::WorkspaceEvicted { .. }),
            "{bare:?}"
        );
        assert_eq!(
            bare.error_data().expect("data"),
            serde_json::json!({ "root": "/repo" })
        );
    }

    /// `clone_err` keeps both new variants (the reload records the absent
    /// index as `last_error`; the fallthrough for an unlisted variant is
    /// `unreachable!`).
    #[test]
    fn clone_err_round_trips_the_absent_index_and_the_failed_reload() {
        use crate::workspace::manager::clone_err;

        let not_indexed = DaemonError::workspace_snapshot_missing(
            Path::new("/repo"),
            PathBuf::from("/repo/.sqry/graph/snapshot.sqry"),
        );
        let cloned = clone_err(&not_indexed);
        assert!(matches!(cloned, DaemonError::WorkspaceNotIndexed { .. }));
        assert_eq!(cloned.to_string(), not_indexed.to_string());

        let reload = DaemonError::WorkspaceReloadFailed {
            root: PathBuf::from("/repo"),
            reload_failure: "x".into(),
        };
        let cloned = clone_err(&reload);
        assert!(matches!(cloned, DaemonError::WorkspaceReloadFailed { .. }));
        assert_eq!(cloned.to_string(), reload.to_string());
    }

    /// T11 (surface parity W1): the daemon renderer names the missing and
    /// extra ids of a `DivergesFromManifest` verdict instead of falling
    /// into the "unrecognised plugin selection status" arm.
    #[test]
    fn from_graph_acquisition_diverges_from_manifest_names_the_ids() {
        use sqry_core::graph::acquisition::{GraphAcquisitionError, PluginSelectionStatus};

        let err = GraphAcquisitionError::IncompatibleGraph {
            source_root: PathBuf::from("/repo"),
            status: PluginSelectionStatus::DivergesFromManifest {
                missing_plugin_ids: vec!["json".to_string()],
                extra_plugin_ids: vec!["terraform".to_string()],
                manifest_path: Some(PathBuf::from("/repo/.sqry/graph/manifest.json")),
            },
        };
        let de: DaemonError = err.into();
        match de {
            DaemonError::WorkspaceIncompatibleGraph { root, reason } => {
                assert_eq!(root, PathBuf::from("/repo"));
                assert!(
                    reason.contains("missing [json]") && reason.contains("extra [terraform]"),
                    "reason must name both id lists, got: {reason}"
                );
                assert!(
                    reason.contains("/repo/.sqry/graph/manifest.json"),
                    "reason must name the manifest, got: {reason}"
                );
                assert!(
                    !reason.contains("unrecognised"),
                    "the verdict must not fall into the catch-all arm: {reason}"
                );
            }
            other => panic!("expected WorkspaceIncompatibleGraph, got {other:?}"),
        }
    }

    #[test]
    fn revision_errors_have_dedicated_jsonrpc_codes_and_data() {
        let cases = [
            (
                DaemonError::RevisionSelectorAmbiguous {
                    selector: "main".to_owned(),
                    matches: vec!["refs/heads/main".to_owned(), "refs/tags/main".to_owned()],
                },
                JSONRPC_REVISION_SELECTOR_AMBIGUOUS,
                "revision_selector_ambiguous",
            ),
            (
                DaemonError::RevisionObjectMissing {
                    object: "a".repeat(40),
                    path: Some(PathBuf::from("/repo")),
                },
                JSONRPC_REVISION_OBJECT_MISSING,
                "revision_object_missing",
            ),
            (
                DaemonError::RevisionSourceUnavailable {
                    reason: "not a git repository".to_owned(),
                    path: Some(PathBuf::from("/repo")),
                },
                JSONRPC_REVISION_SOURCE_UNAVAILABLE,
                "revision_source_unavailable",
            ),
            (
                DaemonError::CheckoutFilterUnsupported {
                    filter: "lfs".to_owned(),
                    path: Some(PathBuf::from("large.bin")),
                },
                JSONRPC_CHECKOUT_FILTER_UNSUPPORTED,
                "checkout_filter_unsupported",
            ),
            (
                DaemonError::SubmoduleUnavailable {
                    path: PathBuf::from("vendor/lib"),
                    gitlink_oid: Some("b".repeat(40)),
                },
                JSONRPC_SUBMODULE_UNAVAILABLE,
                "submodule_unavailable",
            ),
            (
                DaemonError::DirtySnapshotChanged {
                    root: PathBuf::from("/repo"),
                },
                JSONRPC_DIRTY_SNAPSHOT_CHANGED,
                "dirty_snapshot_changed",
            ),
            (
                DaemonError::ArtifactKeyMismatch {
                    artifact_id: "artifact".to_owned(),
                    reason: "manifest digest mismatch".to_owned(),
                },
                JSONRPC_ARTIFACT_KEY_MISMATCH,
                "artifact_key_mismatch",
            ),
            (
                DaemonError::ManagedWorktreeInUse {
                    worktree: PathBuf::from("/repo-agent"),
                    reason: "leased by task-1".to_owned(),
                },
                JSONRPC_MANAGED_WORKTREE_IN_USE,
                "managed_worktree_in_use",
            ),
            (
                DaemonError::RevisionDiskBudgetExceeded {
                    limit_bytes: 100,
                    requested_bytes: 80,
                    current_bytes: 40,
                },
                JSONRPC_REVISION_DISK_BUDGET_EXCEEDED,
                "revision_disk_budget_exceeded",
            ),
            (
                DaemonError::RevisionQueryRequiresExplicitSelector {
                    reason: "cross-revision query disabled by default".to_owned(),
                },
                JSONRPC_REVISION_QUERY_REQUIRES_EXPLICIT_SELECTOR,
                "revision_query_requires_explicit_selector",
            ),
        ];

        for (err, code, kind) in cases {
            assert_eq!(err.jsonrpc_code(), Some(code), "{err:?}");
            let data = err.error_data().expect("revision error must emit data");
            assert_eq!(data["kind"], kind, "{data}");
            assert_eq!(err.exit_code(), 70, "{err:?}");
        }
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

    /// Round 7 note: a rebuild whose outcome did not arrive in time keeps
    /// `-32000` and the deadline kind, but says the rebuild continues, is
    /// not retryable, and names the read that gives the outcome.
    #[test]
    fn rebuild_outcome_timeout_says_the_rebuild_continues() {
        let err = DaemonError::RebuildOutcomeTimeout {
            root: PathBuf::from("/repo"),
            secs: 600,
            deadline_ms: 600_000,
        };
        assert_eq!(err.jsonrpc_code(), Some(JSONRPC_TOOL_TIMEOUT));
        let data = err.error_data().expect("data");
        assert_eq!(data["kind"], "deadline_exceeded");
        assert_eq!(data["retryable"], false);
        assert!(data["retry_after_ms"].is_null());
        assert_eq!(data["details"]["deadline_ms"], 600_000);
        assert_eq!(data["details"]["rebuild_continues"], true);
        assert!(
            data["details"]["follow_with"]
                .as_str()
                .is_some_and(|read| read.starts_with("daemon/status")),
            "{data}"
        );
        assert!(err.to_string().contains("the rebuild continues"), "{err}");
    }

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
