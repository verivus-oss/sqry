//! `sqry-daemon` — long-lived code-graph service.
//!
//! The daemon (`sqryd` binary) owns one or more loaded code graphs in memory,
//! watches source trees for changes, and serves CLI / LSP / MCP clients over a
//! shared Unix-domain socket (named pipe on Windows). The goal is to amortise
//! graph-load cost across every sqry invocation on a machine while preserving
//! the semantic guarantees of direct-mode sqry (bijective per-file buckets,
//! tombstone compaction, `ArcSwap` publish, etc.).
//!
//! # Architecture at a glance
//!
//! - [`config`] — parses `~/.config/sqry/daemon.toml` into a [`config::DaemonConfig`]
//!   with every tuning knob from the Amendment-2 design (memory limits, working-set
//!   multipliers, stale-serve age cap, debounce timing, interner compaction
//!   threshold, log rotation, socket path).
//! - [`workspace`] *(Task 6)* — `WorkspaceManager` owns `LoadedWorkspace` state
//!   (§G admission accounting, §H rebuild plumbing, §F bijection check).
//! - [`rebuild`] *(Task 7)* — per-workspace rebuild lane + coalescing (§J).
//! - [`ipc`] *(Task 8)* — JSON-RPC over UDS with a standard response envelope.
//! - [`lifecycle`] *(Task 9)* — pidfile locking, signal handling, service
//!   unit generators.
//! - [`client`] *(Task 10)* — client library used by `sqry-cli` /
//!   `sqry-lsp --daemon` / `sqry-mcp --daemon` to connect to a running daemon
//!   and auto-start one if necessary.
//!
//! Only [`config`] and the public error type [`DaemonError`] are in the surface
//! today; later tasks in this plan land the other modules in order.

pub mod config;
/// Task 9 U10 — production sqryd binary entry point.
///
/// Owns the clap CLI (`SqrydCli`), the ordered startup / shutdown lifecycle
/// (`run()`), and every `run_start` / `run_stop` / `run_status` /
/// `run_install_*` / `run_print_config` dispatcher.  `main.rs` calls
/// `sqry_daemon::entrypoint::main_impl()` which parses the CLI, builds the
/// tokio runtime, and maps every error to a POSIX `sysexits.h` exit code via
/// `DaemonError::exit_code()`.
pub mod entrypoint;
pub mod error;
pub mod ipc;
/// Task 9 — daemon binary lifecycle: pidfile locking, signal handling, service
/// unit generators, log rotation, and auto-spawn primitives.
///
/// The module is built up incrementally across Task 9 units. Only the units
/// that have landed so far are present; later units (U3–U10) add submodules as
/// they are implemented.
pub mod lifecycle;
/// Phase 8c U8 — in-daemon MCP host.
///
/// Hosts an rmcp `ServerHandler` in-process for each MCP shim
/// byte-pump connection (see [`mcp_host::host_mcp_on_streams`]),
/// routing every `tools/call` through Phase 8b's
/// `daemon_adapter::execute_*_for_daemon` path via the shared
/// [`ipc::tool_core::classify_and_execute`] pipeline. MCP tool
/// behaviour is bit-identical to direct sqryd JSON-RPC tool dispatch.
pub mod mcp_host;
pub mod rebuild;
pub mod workspace;

pub use config::{
    DEFAULT_IPC_SHUTDOWN_DRAIN_SECS, DaemonConfig, ESTIMATE_FINAL_PER_FILE_BYTES,
    ESTIMATE_STAGING_PER_FILE_BYTES, INTERNER_BUILDER_OVERHEAD_RATIO, SocketConfig,
    WORKING_SET_MULTIPLIER, WorkspaceConfig,
};
pub use error::{DaemonError, DaemonResult};
pub use ipc::{
    CancelRebuildResult, DaemonHello, DaemonHelloResponse, IpcServer, JsonRpcError, JsonRpcId,
    JsonRpcPayload, JsonRpcRequest, JsonRpcResponse, JsonRpcVersion, RebuildResult,
    ResponseEnvelope, ResponseMeta,
};
pub use rebuild::{
    CapturedIteration, RebuildDispatcher, RebuildMode, TestCapture, TestGate, decide_mode,
};
pub use workspace::{
    BACKOFF_SCHEDULE, DaemonStatus, EmptyGraphBuilder, FailingGraphBuilder, LoadedWorkspace,
    MemoryStatus, NoOpHook, OldGraphToken, PendingRebuild, RealWorkspaceBuilder,
    RebuildReservation, ServeVerdict, SharedHook, SqrydHook, StalenessVerdict, WorkingSetInputs,
    WorkspaceBuilder, WorkspaceKey, WorkspaceManager, WorkspaceState, WorkspaceStatus,
    backoff_delay_for, classify_staleness, noop_hook, spawn_hook, working_set_estimate,
};

/// JSON-RPC error code: per-tool invocation exceeded
/// `DaemonConfig::tool_timeout_secs`. Emitted by
/// `tool_core::classify_and_execute` (Task 8 Phase 8c U6) when the
/// `tokio::time::timeout(tool_timeout, spawn_blocking(run))` outer
/// timer fires. The detached `JoinHandle` is dropped — the OS thread
/// may continue executing the tool closure but its result is
/// discarded.
///
/// Source: Task 8 Phase 8c design §O (iter-2 Codex-approved wire contract).
pub const JSONRPC_TOOL_TIMEOUT: i32 = -32000;

/// JSON-RPC error code: workspace build failed and no prior good graph exists.
///
/// Source: Amendment 1 §C, Amendment 2 §G.7.
pub const JSONRPC_WORKSPACE_BUILD_FAILED: i32 = -32001;

/// JSON-RPC error code: the workspace is serving a Failed state, but the last
/// successful build is older than `stale_serve_max_age_hours`.
///
/// Source: Amendment 1 §C.
pub const JSONRPC_WORKSPACE_STALE_EXPIRED: i32 = -32002;

/// JSON-RPC error code: admission control could not satisfy a reservation
/// after evicting every non-pinned workspace.
///
/// Source: Amendment 2 §G.1, §G.7.
pub const JSONRPC_MEMORY_BUDGET_EXCEEDED: i32 = -32003;

/// JSON-RPC error code: the workspace was evicted or removed between a
/// rebuild dispatch and its admission / publish commit. Callers must treat
/// this as a terminal signal on the affected `WorkspaceKey` — subsequent
/// dispatches require a fresh `get_or_load` first.
///
/// Source: Amendment 2 §J (same-workspace rebuild serialization), Task 7
/// Phase 7b1 (runner-role gate + `reserve_rebuild` eviction check).
///
/// # Daemon public JSON-RPC error codes (authoritative table)
///
/// | Code    | Variant                | Semantics                                                      |
/// |---------|------------------------|----------------------------------------------------------------|
/// | -32000  | `ToolTimeout`          | Per-tool `tool_timeout_secs` deadline elapsed (Phase 8c U6).   |
/// | -32001  | `WorkspaceBuildFailed` | Build failed, no prior good graph.                             |
/// | -32002  | `WorkspaceStaleExpired`| Stale-serve window exceeded `stale_serve_max_age_hours`.       |
/// | -32003  | `MemoryBudgetExceeded` | Admission cannot fit even after evicting all non-pinned.       |
/// | -32004  | `WorkspaceEvicted`     | Workspace gone mid-rebuild; caller must re-`get_or_load`.      |
/// | -32005  | `WorkspaceIncompatibleGraph` | On-disk graph cannot be used by this binary (plugin or format mismatch). |
/// | -32602  | `InvalidArgument`      | Tool-argument validation failure (JSON-RPC standard).          |
/// | -32603  | `Internal`             | Catch-all bubbled from `sqry_mcp::daemon_adapter` execution.   |
/// | n/a     | `AlreadyRunning`       | Another sqryd holds the pidfile lock (Task 9 U1). Exit 75.    |
/// | n/a     | `AutoStartTimeout`     | `start_detached` socket poll timed out (Task 9 U1). Exit 69.  |
/// | n/a     | `SignalSetup`          | `tokio::signal` handler install failed (Task 9 U1). Exit 70.  |
pub const JSONRPC_WORKSPACE_EVICTED: i32 = -32004;

/// JSON-RPC error code: the on-disk graph snapshot or manifest cannot be
/// loaded safely by this binary. Distinct from `WorkspaceBuildFailed`
/// because it represents a path-policy / compatibility verdict (unknown
/// plugin ids, unsupported snapshot format) rather than a transient
/// build failure — clients react differently (rebuild vs. upgrade vs.
/// retry).
///
/// SGA02 / SGA04 acceptance: "API carries path-policy errors distinctly
/// from load, stale, eviction, and corruption errors" — adapters must
/// not collapse this taxonomy class into the generic build-failed code.
pub const JSONRPC_WORKSPACE_INCOMPATIBLE_GRAPH: i32 = -32005;

/// JSON-RPC error code: the freshly-built graph exceeds the daemon's
/// memory budget *by itself* (post-build oversize) — even after every
/// other workspace would be evicted, the daemon cannot host this
/// graph. Distinct from `MemoryBudgetExceeded` (`-32003`), which is a
/// *projected* admission failure on a pre-build estimate.
///
/// Source: `G_daemon_control_plane.md` §1.4 (post-build heap check) +
/// `00_contracts.md` §3.CC-3 (admission boundary with DPA / DPC).
/// Returned by `WorkspaceManager::publish_and_retain` after the build
/// completes but before the new graph is exposed to readers.
pub const JSONRPC_WORKSPACE_OVERSIZE: i32 = -32006;

/// JSON-RPC error code: socket parent directory cannot be created or
/// is not writable by the daemon's uid. Surfaced before `IpcServer::bind`
/// so the failure mode is a precise diagnostic instead of a generic
/// `EACCES` from the eventual bind.
///
/// Source: `G_daemon_control_plane.md` §5.2.
pub const JSONRPC_SOCKET_SETUP: i32 = -32007;

/// JSON-RPC error code: `daemon/reset` was invoked on a workspace
/// whose state is `Loading` and cannot be safely interrupted yet.
/// Caller should retry once the load completes.
///
/// Source: `G_daemon_control_plane.md` §3.2.
pub const JSONRPC_RESET_WHILE_LOADING: i32 = -32008;

/// JSON-RPC error code: `daemon/reset` was invoked on a workspace
/// whose state is `Rebuilding`; cancellation has been dispatched and
/// the caller is expected to retry after `retry_after_ms` for the
/// state to settle into `Failed` / `Unloaded` (then a follow-up reset
/// completes).
///
/// Source: `G_daemon_control_plane.md` §3.2.
pub const JSONRPC_RESET_CANCELLATION_DISPATCHED: i32 = -32009;

/// JSON-RPC error code: `daemon/reset` refused because the targeted
/// workspace is pinned and the caller did not pass `force = true`.
/// Pinning is a per-workspace operator override; callers must opt in
/// explicitly to drop a pinned workspace.
///
/// Source: `G_daemon_control_plane.md` §3.2.
pub const JSONRPC_WORKSPACE_PINNED: i32 = -32010;

/// JSON-RPC error code: pre-flight cost gate rejected a query because
/// its evaluator cost is structurally unbounded (no scope filter, no
/// regex anchoring, predicate shape would scan the full arena). Wire
/// `kind` is always `"query_too_broad"`. Reuses `-32602` per the
/// existing wire-bridge convention; `kind` is the discriminator
/// (per `B_cost_gate.md` §3 "Why -32602, not a new -32xxx code").
///
/// Source: `B_cost_gate.md` §3 + `00_contracts.md` §3.CC-2.
pub const JSONRPC_QUERY_TOO_BROAD: i32 = JSONRPC_INVALID_PARAMS;

/// JSON-RPC 2.0 standard "Invalid params" error code.
///
/// Surfaced by `tool_core` argument validation (Phase 8c U6) BEFORE
/// workspace classification runs — e.g. `resolve_index_root` failures
/// and missing `path` arguments in MCP tool args.
pub const JSONRPC_INVALID_PARAMS: i32 = -32602;

/// JSON-RPC 2.0 standard "Internal error" code. Catch-all for errors
/// bubbling from `sqry_mcp::daemon_adapter` tool execution that don't
/// map to a more specific `DaemonError` variant.
pub const JSONRPC_INTERNAL_ERROR: i32 = -32603;

/// Version of the daemon wire envelope (`DaemonHelloResponse.envelope_version`).
///
/// Re-exported from `sqry-daemon-protocol` so callers that only depend on
/// `sqry-daemon` (or on `sqry-daemon-client`) both see the same single source
/// of truth. See [`sqry_daemon_protocol::ENVELOPE_VERSION`] for the canonical
/// definition and bump policy.
pub use sqry_daemon_protocol::ENVELOPE_VERSION;

// ---------------------------------------------------------------------------
// SGA07 parity test hooks (test-only re-exports)
// ---------------------------------------------------------------------------

/// SGA07 parity test hook — snapshot the process-wide counter that the
/// daemon graph provider bumps on every [`acquire`](workspace::acquirer)
/// call. Returns the current count without resetting it. Gated on
/// `#[cfg(any(test, feature = "test-hooks"))]` so the symbol is
/// unreachable in default release builds.
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub fn acquire_counter_snapshot() -> usize {
    workspace::acquirer::acquire_counter_snapshot()
}

/// SGA07 parity test hook — reset the process-wide acquisition counter
/// to zero. Returns the previous value so callers can sanity-check a
/// reset between dispatches. Production code MUST NOT call this.
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub fn acquire_counter_reset() -> usize {
    workspace::acquirer::acquire_counter_reset()
}

// ---------------------------------------------------------------------------
// Shared test-only ENV_LOCK
// ---------------------------------------------------------------------------

/// Single process-wide mutex for tests that manipulate `XDG_RUNTIME_DIR`.
///
/// Multiple test modules (`pidfile`, `detach`, `config`) run as threads in the
/// same binary.  Each module previously had its own `ENV_LOCK`, which allowed
/// concurrent `XDG_RUNTIME_DIR` mutations and produced flaky pidfile-PID
/// mismatches.  This shared lock serialises all env-var mutations across every
/// `#[cfg(test)]` module in the crate.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
