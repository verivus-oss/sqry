//! Wire types for the sqryd daemon IPC.
//!
//! Every type in this module serialises as UTF-8 JSON through serde.
//! The wire format is versioned via the `envelope_version` field on
//! [`DaemonHelloResponse`] / [`ShimRegisterAck`]; clients negotiate
//! compatibility during the handshake before issuing any JSON-RPC
//! request or entering the shim byte-pump.
//!
//! # JSON-RPC 2.0 conformance
//!
//! - Requests and responses carry the mandatory `"jsonrpc": "2.0"` tag
//!   enforced by [`JsonRpcVersion`]'s manual serde impls.
//! - Response ids follow the spec exactly: a response to a request
//!   with a missing/invalid id MUST carry `id: null`; `Option<JsonRpcId>`
//!   on [`JsonRpcResponse::id`] is NOT marked `skip_serializing_if`, so
//!   `None` serialises as JSON `null` instead of being omitted.
//! - Batches are implemented in the sqry-daemon router; this module
//!   only provides the single-request envelope types.
//!
//! # `shim/register`
//!
//! [`ShimRegister`] / [`ShimProtocol`] / [`ShimRegisterAck`] are the
//! Phase 8c shim handshake wire types. The router in sqry-daemon
//! discriminates on the very first frame:
//!
//! - If the frame object has both `protocol` + `pid` keys (shim-shaped),
//!   the router enters the shim path and deserialises as [`ShimRegister`]
//!   with `deny_unknown_fields`. On deserialisation failure (e.g. extra
//!   keys from the hello shape, or an unknown `protocol` variant) the
//!   server writes [`ShimRegisterAck`]`{ accepted: false, reason: Some(..) }`
//!   and closes. **Not** a JSON-RPC `-32600` — the shim client expects a
//!   [`ShimRegisterAck`] as the first response, so the wire-form stays
//!   coherent.
//! - Otherwise the router falls through to the [`DaemonHello`] path
//!   (JSON-RPC). A frame with neither shape is rejected with
//!   `-32600 Invalid Request` and `id: null`.

use std::marker::PhantomData;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

// ---------------------------------------------------------------------------
// WorkspaceId — protocol-side wire wrapper for sqry-core's WorkspaceId.
// ---------------------------------------------------------------------------

/// 32-byte stable identity for a logical workspace, byte-identical to
/// `sqry_core::workspace::WorkspaceId`.
///
/// Defined here in the leaf protocol crate so the daemon wire types
/// (`DaemonHello.logical_workspace`, `daemon/load.logical_workspace`,
/// `daemon/workspaceStatus.workspace_id`) can carry the identity without
/// the protocol crate taking a `sqry-core` dependency. The `sqry-daemon`
/// binary owns the `From`/`Into` bridge against the canonical
/// `sqry_core::workspace::WorkspaceId` type — both use the same 32-byte
/// representation, so the bridge is a zero-cost newtype unwrap.
///
/// STEP_6 (workspace-aware-cross-repo DAG) introduced this type. Older
/// daemon clients that send `DaemonHello` without `logical_workspace`
/// continue to work because the field is `#[serde(default)]` — they
/// reproduce today's per-source-root semantics, with `workspace_id =
/// None` on the matching [`crate::WorkspaceState`] entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceId([u8; 32]);

impl WorkspaceId {
    /// Construct from raw 32 bytes. Callers in `sqry-daemon` use this
    /// to bridge from `sqry_core::workspace::WorkspaceId::as_bytes()`.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Borrow the 32-byte digest. Callers cross the bridge by feeding
    /// these bytes back into `sqry_core::workspace::WorkspaceId`.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// First 16 hex characters. Suitable for log lines / short
    /// identifiers; **not** sufficient for cross-process identity.
    #[must_use]
    pub fn as_short_hex(&self) -> String {
        let full = self.as_full_hex();
        full[..16].to_string()
    }

    /// Full 64-character hex digest. Use this for any identity
    /// comparison.
    #[must_use]
    pub fn as_full_hex(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::with_capacity(64);
        for byte in &self.0 {
            // `write!` to a `String` is infallible.
            let _ = write!(s, "{byte:02x}");
        }
        s
    }
}

impl std::fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_short_hex())
    }
}

// ---------------------------------------------------------------------------
// LogicalWorkspaceWire — daemon-IPC wire form of sqry-core's LogicalWorkspace.
// ---------------------------------------------------------------------------

/// Wire-form summary of a `LogicalWorkspace`, attached to
/// [`DaemonHello`] / `daemon/load` payloads. Carries the workspace
/// identity plus the canonical source-root paths the client wants the
/// daemon to bind under a single grouping `workspace_id`.
///
/// `member_folders` and `exclusions` are explicitly **not** carried on
/// this wire shape — they are MCP / redaction-side concerns (Step 7 of
/// the workspace-aware-cross-repo plan), not daemon admission concerns.
/// The daemon only needs `workspace_id` + the source-root list to build
/// one [`crate::WorkspaceState`]-keyed entry per source root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LogicalWorkspaceWire {
    /// 32-byte BLAKE3-256 identity of the logical workspace.
    pub workspace_id: WorkspaceId,
    /// Canonical absolute source-root paths. The daemon constructs one
    /// `WorkspaceKey { workspace_id: Some(this id), source_root: <p>, .. }`
    /// per entry, all sharing the same `workspace_id` for grouping.
    pub source_roots: Vec<std::path::PathBuf>,
    /// STEP_11_4 — per-source-root bindings. Each entry's `path` MUST
    /// appear in [`Self::source_roots`]; the binding's
    /// `config_fingerprint` overrides the workspace-level default for
    /// that root only. Empty in the common case so the wire stays
    /// pre-STEP_11_4-compatible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_root_bindings: Vec<SourceRootBinding>,
    /// STEP_11_4 — workspace-level config fingerprint applied to any
    /// source root that does not carry its own
    /// [`SourceRootBinding::config_fingerprint`] override. `0` is the
    /// "fingerprint not set" sentinel.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub workspace_config_fingerprint: u64,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// STEP_11_4 — per-source-root binding inside a [`LogicalWorkspaceWire`].
///
/// `path` MUST appear in the parent [`LogicalWorkspaceWire::source_roots`]
/// vector; the daemon matches bindings to source roots by canonical path
/// equality. A binding whose `path` is not in `source_roots` is silently
/// ignored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceRootBinding {
    /// Canonical absolute path of the source root this binding applies to.
    pub path: std::path::PathBuf,
    /// Per-source-root override of the config fingerprint. `0` means
    /// "use the workspace-level fingerprint"; non-zero overrides for
    /// this source root only.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub config_fingerprint: u64,
    /// Optional pre-resolved classpath directory for this source root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classpath_dir: Option<std::path::PathBuf>,
}

// ---------------------------------------------------------------------------
// WorkspaceIndexStatus — daemon/workspaceStatus result payload.
// ---------------------------------------------------------------------------

/// Aggregate status of a single source root inside a logical workspace.
/// Mirrors the per-source-root subset of `WorkspaceStatus` so cross-repo
/// MCP / LSP queries can render a per-source-root state without paying
/// the cost of the full `daemon/status` snapshot.
///
/// STEP_11_4 (workspace-aware-cross-repo, 2026-04-26) — adds the
/// `classpath_present` flag so consumers of `daemon/workspaceStatus`
/// know which source roots have JVM classpath analysis available
/// (`<source_root>/.sqry/classpath/` exists) without having to make a
/// separate filesystem probe. The flag is per-source-root, never
/// aggregated, so a workspace mixing JVM and non-JVM source roots
/// reports accurate per-root granularity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceSourceRootStatus {
    /// Canonical absolute path to the source root.
    pub source_root: std::path::PathBuf,
    /// Per-source-root lifecycle state. `Evicted` is a valid (and
    /// useful — partial eviction is observable here) value for a
    /// source root that has been LRU'd out while sibling source roots
    /// remain `Loaded`.
    pub state: WorkspaceState,
    /// Live graph size for this source root, in bytes.
    pub current_bytes: u64,
    /// STEP_11_4 — `true` when the daemon observed
    /// `<source_root>/.sqry/classpath/` as a directory at status time.
    /// `false` when the directory is absent or the probe failed (the
    /// daemon never blocks status on a classpath probe; failures
    /// surface through the LSP-side `WorkspaceIndexStatus.warnings`
    /// channel instead).
    ///
    /// `#[serde(default)]` so v1 IPC payloads (which never carried the
    /// flag) round-trip into `false`. `skip_serializing_if = ...` is
    /// deliberately NOT applied — the flag must be serialised even
    /// when `false` so consumers can distinguish "JVM-aware daemon
    /// reporting no classpath" from "older daemon that does not yet
    /// surface the flag".
    #[serde(default)]
    pub classpath_present: bool,
}

/// Aggregate status of a logical workspace, returned by
/// `daemon/workspaceStatus { workspace_id }`.
///
/// The daemon walks every `WorkspaceKey` whose `workspace_id` matches
/// the request and aggregates them into this view. A workspace is
/// "partially evicted" when at least one source root reports
/// [`WorkspaceState::Evicted`] but at least one other reports any
/// non-Evicted state — see [`Self::partially_evicted`].
///
/// STEP_12 (workspace-aware-cross-repo, 2026-04-26) introduced the
/// hex-string telemetry fields `workspace_id_short` (16 hex chars,
/// display) and `workspace_id_full` (64 hex chars, machine identity).
/// Scripts consuming this payload should key on `workspace_id_full` —
/// the 32-byte `workspace_id` is the canonical bytewise identity but
/// the hex string is what humans / shell tooling read. The two hex
/// fields are derived from `workspace_id`; they are NOT independent
/// inputs — they exist purely for ergonomic JSON consumption.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceIndexStatus {
    /// Identity the request matched against.
    pub workspace_id: WorkspaceId,
    /// STEP_12 — short (16 hex) form of `workspace_id`, suitable for
    /// CLI columns and human-scale log lines. Display only.
    pub workspace_id_short: String,
    /// STEP_12 — full (64 hex) form of `workspace_id`. Machine
    /// identity. Cross-process script consumers MUST key on this
    /// rather than the short form to avoid the (remote, non-zero)
    /// possibility of short-hex collisions across hundreds of
    /// thousands of distinct workspaces.
    pub workspace_id_full: String,
    /// Per-source-root status rows, sorted by `source_root` for
    /// deterministic CLI / test output.
    pub source_roots: Vec<WorkspaceSourceRootStatus>,
}

impl WorkspaceIndexStatus {
    /// Whether at least one source root is in [`WorkspaceState::Evicted`]
    /// while at least one other is not. `false` for fully-loaded or
    /// fully-evicted aggregates.
    #[must_use]
    pub fn partially_evicted(&self) -> bool {
        let any_evicted = self
            .source_roots
            .iter()
            .any(|r| matches!(r.state, WorkspaceState::Evicted));
        let any_alive = self
            .source_roots
            .iter()
            .any(|r| !matches!(r.state, WorkspaceState::Evicted));
        any_evicted && any_alive
    }
}

// ---------------------------------------------------------------------------
// Wire envelope version.
// ---------------------------------------------------------------------------

/// Version of the daemon wire envelope ([`DaemonHelloResponse::envelope_version`],
/// [`ShimRegisterAck::envelope_version`]).
///
/// Bumped when the [`ResponseEnvelope`] schema changes in an incompatible way.
/// Kept at `1` per the Amendment-2 2026-04-09 freeze.
///
/// This constant lives in the leaf wire-type crate (`sqry-daemon-protocol`) so
/// every consumer of the wire format — the daemon itself, the daemon client
/// (`sqry-daemon-client`), and the shim-mode callers inside `sqry-lsp` /
/// `sqry-mcp` — validates against exactly one source of truth. Clients MUST
/// reject a response whose `envelope_version` differs from this constant
/// rather than proceed on a mismatched wire format.
pub const ENVELOPE_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// WorkspaceState — moved here from sqry-daemon/src/workspace/state.rs
// ---------------------------------------------------------------------------

/// Six-state workspace lifecycle per plan Task 6 Step 1 and Amendment 2 §G.5 /
/// §G.7.
///
/// The `#[repr(u8)]` is load-bearing: `sqry-daemon`'s `LoadedWorkspace::state`
/// is an `AtomicU8`, and the conversions [`Self::from_u8`] / [`Self::as_u8`]
/// serialise the state machine without allocation. Values are deliberately
/// contiguous from 0 so adding a variant stays backwards-compatible with
/// persisted telemetry.
///
/// This type lives in the leaf wire-type crate so [`ResponseMeta`] can
/// carry a canonical workspace_state string on every successful tool
/// response without the leaf crate taking a dep on `sqry-daemon` itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum WorkspaceState {
    /// Workspace entry exists but no graph has been loaded yet.
    Unloaded = 0,

    /// Initial load is in progress — a single blocking read from disk or
    /// a full rebuild with no prior snapshot.
    Loading = 1,

    /// Graph is loaded, idle, and ready to serve queries.
    Loaded = 2,

    /// A rebuild (incremental or full) is actively running on the
    /// dispatcher's background task. Queries keep serving the prior
    /// `ArcSwap<CodeGraph>` snapshot until `publish_and_retain` swaps
    /// the new graph in.
    Rebuilding = 3,

    /// Workspace was LRU-evicted or explicitly unloaded. The entry is
    /// REMOVED from the manager map — the next query must re-load via
    /// `get_or_load`. This discriminant exists for the short window
    /// between `execute_eviction` storing the state and
    /// `workspaces.remove(key)` completing (both under
    /// `workspaces.write()`); external observers routed through
    /// `WorkspaceManager::classify_for_serve` see the map-missing arm
    /// first and get `DaemonError::WorkspaceEvicted` regardless.
    Evicted = 4,

    /// The most recent rebuild failed. Queries are served from the last
    /// good snapshot with `meta.stale = true`; if the
    /// `stale_serve_max_age_hours` cap is exceeded, queries receive the
    /// JSON-RPC `-32002 workspace_stale_expired` error instead.
    Failed = 5,
}

impl WorkspaceState {
    /// Round-trip the state to its discriminant.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parse a discriminant back to a state. Returns `None` on any value
    /// outside the current enum range — callers should treat this as a
    /// telemetry corruption rather than silently map to `Unloaded`.
    #[must_use]
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Unloaded),
            1 => Some(Self::Loading),
            2 => Some(Self::Loaded),
            3 => Some(Self::Rebuilding),
            4 => Some(Self::Evicted),
            5 => Some(Self::Failed),
            _ => None,
        }
    }

    /// Canonical display string. Used by `daemon/status` output and
    /// tracing spans.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unloaded => "unloaded",
            Self::Loading => "loading",
            Self::Loaded => "loaded",
            Self::Rebuilding => "rebuilding",
            Self::Evicted => "evicted",
            Self::Failed => "failed",
        }
    }

    /// Whether the workspace can still serve queries in this state.
    ///
    /// `true` for [`Self::Loaded`], [`Self::Rebuilding`] (old snapshot
    /// still served), and [`Self::Failed`] (stale-serve subject to the
    /// age cap). `false` for [`Self::Unloaded`], [`Self::Loading`],
    /// and [`Self::Evicted`].
    #[must_use]
    pub const fn is_serving(self) -> bool {
        matches!(self, Self::Loaded | Self::Rebuilding | Self::Failed)
    }
}

impl std::fmt::Display for WorkspaceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Handshake types.
// ---------------------------------------------------------------------------

/// Pre-handshake header sent as the very first frame by a CLI client.
/// The server responds with [`DaemonHelloResponse`] before the
/// JSON-RPC request loop begins.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonHello {
    /// Free-form client identifier (`env!("CARGO_PKG_VERSION")` plus
    /// user-agent suffix). Informational only.
    pub client_version: String,

    /// Wire protocol version. Phase 8a accepts exactly `1`.
    pub protocol_version: u32,

    /// Optional logical-workspace binding hint (STEP_6 of the
    /// workspace-aware-cross-repo plan). When present, every
    /// subsequent `daemon/load` on this connection that does not
    /// itself supply `logical_workspace` inherits this binding —
    /// keeping today's anonymous behaviour for clients that do not
    /// set the hint.
    ///
    /// `#[serde(default)]` so older clients (and the standalone
    /// `sqry-mcp` / `sqry-lsp` shims that have not yet learned about
    /// logical workspaces) keep working with `None`. The daemon
    /// router synthesises one `WorkspaceKey` per source root with
    /// `workspace_id = Some(this id)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_workspace: Option<LogicalWorkspaceWire>,
}

/// Server's reply to [`DaemonHello`]. If `compatible` is `false` the
/// server closes the connection immediately after the frame is sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonHelloResponse {
    pub compatible: bool,
    pub daemon_version: String,
    pub envelope_version: u32,
}

// ---------------------------------------------------------------------------
// Shim handshake (Phase 8c wire types).
// ---------------------------------------------------------------------------

/// Which client protocol the shim will pump bytes for. Phase 8c surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShimProtocol {
    Lsp,
    Mcp,
}

/// Shim registration header sent as the first frame by a
/// `sqry lsp --daemon` or `sqry mcp --daemon` process. The router in
/// sqry-daemon shape-discriminates between [`DaemonHello`] and this
/// type using `#[serde(deny_unknown_fields)]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShimRegister {
    pub protocol: ShimProtocol,
    pub pid: u32,
}

/// Server's reply to [`ShimRegister`]. If `accepted` is `false` the
/// server closes the connection after sending the ack and the shim
/// client surfaces `reason` to its parent process. When `accepted` is
/// `true`, `reason` is omitted from the wire form (skip-if-none).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShimRegisterAck {
    pub accepted: bool,
    pub daemon_version: String,
    /// Rejection reason. Omitted from the wire when accepted=true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub envelope_version: u32,
}

// ---------------------------------------------------------------------------
// ResponseEnvelope.
// ---------------------------------------------------------------------------

/// Uniform successful-response wrapper. Every successful method
/// response is serialised as `ResponseEnvelope<T>` at the JSON-RPC
/// `result` field — clients can rely on the [`ResponseMeta`] shape
/// being present on every successful reply regardless of method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope<T> {
    pub result: T,
    pub meta: ResponseMeta,
}

/// Metadata attached to every successful response. For Phase 8a
/// management methods the staleness fields are always absent
/// (`stale = false`, no last_good_at, no last_error,
/// `workspace_state = None`). Phase 8b populates them from the
/// server-side `ServeVerdict` for tool-method responses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResponseMeta {
    pub stale: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_good_at: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,

    /// Canonical workspace state string (serde form of
    /// [`WorkspaceState`]). `None` for methods not tied to a workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_state: Option<WorkspaceState>,

    pub daemon_version: String,
}

impl ResponseMeta {
    /// Construct the [`ResponseMeta`] used by daemon management methods
    /// (`daemon/status`, `daemon/unload`, `daemon/stop` — the ones not
    /// bound to a specific workspace).
    #[must_use]
    pub fn management(daemon_version: &str) -> Self {
        Self {
            stale: false,
            last_good_at: None,
            last_error: None,
            workspace_state: None,
            daemon_version: daemon_version.to_owned(),
        }
    }

    /// Construct the [`ResponseMeta`] for a successful `daemon/load`.
    /// Phase 8b adds `fresh_from` / `stale_from` constructors for
    /// MCP tool-method responses that route through `classify_for_serve`.
    #[must_use]
    pub fn loaded(daemon_version: &str) -> Self {
        Self {
            stale: false,
            last_good_at: None,
            last_error: None,
            workspace_state: Some(WorkspaceState::Loaded),
            daemon_version: daemon_version.to_owned(),
        }
    }

    /// Construct [`ResponseMeta`] for a tool-method response served from a
    /// Fresh workspace verdict (`WorkspaceState::Loaded` or `Rebuilding`).
    ///
    /// Phase 8b Task 7 — populated by the `tool_dispatch` helper when
    /// the daemon's `WorkspaceManager::classify_for_serve` returns
    /// `ServeVerdict::Fresh`. `stale` is `false` and both `last_good_at`
    /// and `last_error` are absent from the wire form (they are skipped
    /// by `serde(skip_serializing_if = "Option::is_none")`).
    #[must_use]
    pub fn fresh_from(state: WorkspaceState, daemon_version: &str) -> Self {
        Self {
            stale: false,
            last_good_at: None,
            last_error: None,
            workspace_state: Some(state),
            daemon_version: daemon_version.to_owned(),
        }
    }

    /// Construct [`ResponseMeta`] for a tool-method response served from a
    /// Stale verdict. `last_good_at` is rendered as RFC3339 UTC-Zulu via
    /// `chrono::DateTime::<Utc>::from(SystemTime) -> to_rfc3339_opts(Secs, true)`.
    ///
    /// `workspace_state` is fixed at [`WorkspaceState::Failed`] because
    /// `WorkspaceManager::classify_for_serve` only emits a Stale verdict
    /// when the observed state is `Failed`. Keeping this constructor
    /// intentionally rigid (no caller-supplied state) prevents the wire
    /// form from claiming `stale = true` with a workspace_state the
    /// classifier could never have produced.
    #[must_use]
    pub fn stale_from(
        last_good_at: std::time::SystemTime,
        last_error: Option<String>,
        daemon_version: &str,
    ) -> Self {
        use chrono::{DateTime, SecondsFormat, Utc};
        let rfc3339 =
            DateTime::<Utc>::from(last_good_at).to_rfc3339_opts(SecondsFormat::Secs, true);
        Self {
            stale: true,
            last_good_at: Some(rfc3339),
            last_error,
            workspace_state: Some(WorkspaceState::Failed),
            daemon_version: daemon_version.to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// daemon/load result wire type.
// ---------------------------------------------------------------------------

/// `daemon/load` success result payload.
///
/// Serialised under the `result` field of [`ResponseEnvelope`]. Living
/// in the leaf protocol crate lets both the daemon (writer) and
/// [`sqry-daemon-client`][] (reader) share a single typed definition —
/// clients can `serde_json::from_value::<ResponseEnvelope<LoadResult>>`
/// and get compile-time schema checking instead of stringly-typed
/// `serde_json::Value::get` lookups.
///
/// [`sqry-daemon-client`]: ../../sqry-daemon-client/index.html
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LoadResult {
    /// The canonicalised workspace root path that the daemon loaded.
    pub root: std::path::PathBuf,

    /// Resident graph memory footprint for the loaded workspace, in
    /// bytes. Matches `LoadedWorkspace::heap_bytes()` at the moment of
    /// the response.
    pub current_bytes: u64,

    /// The canonical workspace lifecycle state after the load
    /// completes. Always [`WorkspaceState::Loaded`] on the successful
    /// `daemon/load` path — the field is typed so clients do not have
    /// to re-parse the string.
    pub state: WorkspaceState,
}

/// Status of a `daemon/rebuild` invocation (cluster-G §2.4).
///
/// Distinguishes the four outcomes the dispatcher can produce so a
/// `--timeout 0` (fire-and-forget) caller can distinguish "started in
/// the background" from "actually completed in this call". Pre-§2.4
/// callers received only the `Completed` shape and a missing field
/// here is interpreted as `Completed` for backward compatibility.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RebuildStatus {
    /// The rebuild ran to completion in this call. `duration_ms`,
    /// `nodes`, `edges`, `files_indexed`, and `was_full` are all
    /// populated.
    #[default]
    Completed,
    /// `--timeout 0` (fire-and-forget): the runner-role was acquired
    /// and the rebuild is running in the background. The stat fields
    /// are absent. The caller should poll `daemon/status` to observe
    /// completion.
    Started,
    /// Another runner is active; this request was coalesced into the
    /// pending lane. The stat fields reflect the runner's *previous*
    /// publish if known, or are absent.
    Coalesced,
    /// Reservation failed before the pipeline started (e.g.
    /// `MemoryBudgetExceeded`, `WorkspaceOversize`). The stat fields
    /// are absent.
    Rejected,
}

/// `daemon/rebuild` success result payload (schema_version 2 — see
/// cluster-G §2.4).
///
/// Serialised under the `result` field of [`ResponseEnvelope`]. The
/// stat fields are `Option`-typed because `--timeout 0` callers
/// receive them populated only when `status == Completed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RebuildResult {
    /// The canonicalised workspace root path that was rebuilt.
    pub root: std::path::PathBuf,
    /// Outcome of the dispatch. New in cluster-G §2.4. Older clients
    /// that pre-date the schema bump are tolerated by the
    /// `#[serde(default)]` here — they read the field as `Completed`
    /// and continue to work because pre-§2.4 daemons only ever
    /// produced the completed shape.
    #[serde(default)]
    pub status: RebuildStatus,
    /// Wall-clock time the rebuild took, in milliseconds. Populated
    /// only when `status == Completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Node count of the freshly published graph. Populated only
    /// when `status == Completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<u64>,
    /// Edge count of the freshly published graph. Populated only
    /// when `status == Completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edges: Option<u64>,
    /// Number of source files indexed in the freshly published
    /// graph. Populated only when `status == Completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_indexed: Option<u64>,
    /// `true` when the rebuild was a full (non-incremental) rebuild.
    /// Populated only when `status == Completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was_full: Option<bool>,
}

/// `daemon/cancel_rebuild` success result payload.
///
/// Serialised under the `result` field of [`ResponseEnvelope`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CancelRebuildResult {
    /// The canonicalised workspace root path whose rebuild was signalled for
    /// cancellation.
    pub root: std::path::PathBuf,
    /// `true` when a rebuild was actually in flight at the moment the
    /// cancellation signal was dispatched.
    pub cancelled: bool,
}

// ---------------------------------------------------------------------------
// `daemon/search` wire types (verivus-oss/sqry#238 Tier 2).
//
// Mirror the relevant arguments of `commands::search::run_search` so the
// CLI shim at `sqry-cli/src/commands/search.rs` can attempt the daemon
// path before falling through to in-process load. Field shape follows the
// established protocol-crate convention: leaf-only (no upstream sqry deps,
// no tower-lsp types), flat fields rather than nested LSP `Location`
// wrappers — the CLI converts to `DisplaySymbol` for output formatting,
// keeping parity at the formatter layer rather than the wire layer.
// ---------------------------------------------------------------------------

/// `daemon/search` request payload.
///
/// Submitted as the `params` field of a [`JsonRpcRequest`] with
/// `method == "daemon/search"`. Wire-compatible with `--exact`, regex, and
/// fuzzy search modes; the daemon handler dispatches to the same
/// `find_by_exact_name` / regex / fuzzy logic the in-process CLI uses
/// (verifiable by the `DAEMON_SEARCH_TESTS` parity assertions).
///
/// Errors: a request whose `search_path` does not resolve to a
/// daemon-loaded workspace returns `WorkspaceEvicted` (-32004) or
/// `WorkspaceNotLoaded`; an incompatible plugin selection returns
/// `WorkspaceIncompatibleGraph` (-32005). The CLI shim treats either as
/// a soft failure and falls through to the in-process path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    /// Wire envelope version. Defaults to [`ENVELOPE_VERSION`] for
    /// forward-compatible serde when the field is absent.
    #[serde(default = "default_envelope_version")]
    pub envelope_version: u32,
    /// Pattern to search for. Interpreted per `mode`.
    pub pattern: String,
    /// Workspace root the search should resolve against. The daemon
    /// normalises and looks this up via `WorkspaceManager`.
    pub search_path: String,
    /// Search interpretation mode. Maps to the CLI's `--exact` / `--fuzzy`
    /// flags; everything else falls into [`SearchMode::Regex`].
    pub mode: SearchMode,
    /// Optional kind filter (e.g. `"function"`, `"class"`). Matches
    /// the in-process `Cli::kind` semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Optional language filter (e.g. `"rust"`, `"python"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Maximum result count. `None` lets the daemon apply its default
    /// (mirrors `Cli::limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Mirror of the CLI's `--include-generated` flag (default in-process:
    /// `false` — macro-generated symbols are dropped). When `false` the
    /// daemon applies the same `filter_nodes_by_macro_boundary` contract
    /// as `sqry-cli/src/commands/search.rs::run_regular_search` so the
    /// daemon route does not surface macro-generated symbols the
    /// in-process path would have dropped.
    ///
    /// Serde default is `true` for backward compatibility — a request
    /// body that omits the field gets the same "no filter" behaviour
    /// the tier-2 daemon handler shipped with originally (the
    /// `DAEMON_SEARCH_HANDLER` unit's approved tests rely on that
    /// default and continue to pass without modification).
    #[serde(default = "default_include_generated")]
    pub include_generated: bool,
}

fn default_envelope_version() -> u32 {
    ENVELOPE_VERSION
}

fn default_include_generated() -> bool {
    // True keeps the pre-`include_generated` wire-shape behaviour: the
    // daemon returns every candidate, including macro-generated ones.
    // Callers that need parity with the CLI's default exact search must
    // set this to `false` so the daemon drops `macro_generated == Some(true)`
    // nodes before serialising the result set.
    true
}

/// Search interpretation mode for [`SearchRequest::mode`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    /// Regex pattern over interned symbol names. Default behaviour for
    /// `sqry search <pat>` and `sqry <pat>`.
    Regex,
    /// Literal byte-exact symbol-name match (`--exact` / planner
    /// `name:<literal>` contract).
    Exact,
    /// Trigram fuzzy match (`--fuzzy`).
    Fuzzy,
}

/// One search hit. Flat shape (no LSP `Location` wrapper) so the protocol
/// crate stays leaf-only; the CLI shim converts to `DisplaySymbol` for
/// output formatting.
///
/// Line/column semantics match `DisplaySymbol`:
///   - `start_line` / `end_line` are 1-based
///   - `start_column` / `end_column` are 0-based byte offsets
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearchItem {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub language: String,
    pub file_path: String,
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
    /// Fuzzy match score. Absent for regex / exact hits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

/// `daemon/search` success result payload.
///
/// Serialised under the `result` field of [`ResponseEnvelope`]. The CLI
/// shim reads `total` + `truncated` separately so it can emit the same
/// "Showing N of M matches" banner the in-process path does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearchResult {
    /// The hits returned for this query, post-limit.
    pub items: Vec<SearchItem>,
    /// Pre-truncation match count. When `truncated == false` this equals
    /// `items.len()`; when `truncated == true` it is at least `limit + 1`
    /// (lower-bound sentinel, matching the LSP `list_unused_symbols` /
    /// `list_circular_dependencies` convention).
    pub total: u64,
    /// True when the result was capped by `SearchRequest::limit`.
    pub truncated: bool,
    /// Reserved for future cursor-based pagination. Tier-2 always returns
    /// `None`; clients should not assume any specific format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 envelope types.
// ---------------------------------------------------------------------------

/// JSON-RPC `"2.0"` version tag. Manual serde impls enforce exact
/// string match on the wire so malformed requests never leak into the
/// method dispatcher.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Vis(PhantomData<JsonRpcVersion>);
        impl<'de> de::Visitor<'de> for Vis {
            type Value = JsonRpcVersion;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("the string \"2.0\"")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                if v == "2.0" {
                    Ok(JsonRpcVersion)
                } else {
                    Err(E::invalid_value(de::Unexpected::Str(v), &"\"2.0\""))
                }
            }
        }
        d.deserialize_str(Vis(PhantomData))
    }
}

/// JSON-RPC id: `null`, integer (signed or unsigned), or string.
/// `I64` covers `i64::MIN..=i64::MAX`; `U64` covers
/// `i64::MAX + 1..=u64::MAX`. Serde's untagged deserialize tries
/// variants in order so `0..=i64::MAX` lands in `I64` and
/// `i64::MAX + 1..=u64::MAX` in `U64`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum JsonRpcId {
    /// Signed integer id.
    I64(i64),
    /// Unsigned integer id above `i64::MAX`.
    U64(u64),
    /// String id.
    Str(String),
}

/// JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: JsonRpcVersion,

    /// `None` ≙ notification (no response expected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<JsonRpcId>,

    pub method: String,

    #[serde(default)]
    pub params: serde_json::Value,
}

/// JSON-RPC 2.0 response. `id` is [`Option<JsonRpcId>`] with **no**
/// `skip_serializing_if` — the `None` case serialises as JSON `null`,
/// which is exactly what the spec demands for parse-error and
/// invalid-request responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: JsonRpcVersion,

    /// `null` on the wire when the server could not determine the
    /// originating request id (parse error, invalid request shape,
    /// batch element with un-parseable id).
    pub id: Option<JsonRpcId>,

    #[serde(flatten)]
    pub payload: JsonRpcPayload,
}

/// Tagged success-or-error payload. Serde `untagged` so the wire form
/// is `{... "result": ...}` or `{... "error": ...}`, never both.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcPayload {
    Success { result: serde_json::Value },
    Error { error: JsonRpcError },
}

/// JSON-RPC 2.0 error payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    /// Construct a successful response.
    #[must_use]
    pub fn success(id: Option<JsonRpcId>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id,
            payload: JsonRpcPayload::Success { result },
        }
    }

    /// Construct an error response.
    #[must_use]
    pub fn error(
        id: Option<JsonRpcId>,
        code: i32,
        message: impl Into<String>,
        data: Option<serde_json::Value>,
    ) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id,
            payload: JsonRpcPayload::Error {
                error: JsonRpcError {
                    code,
                    message: message.into(),
                    data,
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonrpc_version_roundtrip() {
        let wire = serde_json::to_string(&JsonRpcVersion).unwrap();
        assert_eq!(wire, r#""2.0""#);
        let back: JsonRpcVersion = serde_json::from_str(&wire).unwrap();
        assert_eq!(back, JsonRpcVersion);
    }

    #[test]
    fn jsonrpc_version_rejects_wrong_string() {
        let err = serde_json::from_str::<JsonRpcVersion>(r#""1.0""#)
            .expect_err("must reject non-\"2.0\"");
        assert!(err.to_string().contains("\"2.0\""));
    }

    #[test]
    fn jsonrpc_id_untagged_roundtrip() {
        let cases: &[(&str, JsonRpcId)] = &[
            ("0", JsonRpcId::I64(0)),
            ("-7", JsonRpcId::I64(-7)),
            (&i64::MAX.to_string(), JsonRpcId::I64(i64::MAX)),
            ("\"abc\"", JsonRpcId::Str("abc".into())),
        ];
        for (wire, expected) in cases {
            let parsed: JsonRpcId = serde_json::from_str(wire).expect(wire);
            assert_eq!(&parsed, expected, "round-trip failed for {wire}");
        }
        // i64::MAX + 1 routes to U64.
        let u: JsonRpcId = serde_json::from_str("9223372036854775808").unwrap();
        assert_eq!(u, JsonRpcId::U64(9_223_372_036_854_775_808));
    }

    #[test]
    fn response_id_none_serializes_as_json_null() {
        let resp = JsonRpcResponse::error(None, -32700, "Parse error", None);
        let wire = serde_json::to_string(&resp).unwrap();
        assert!(
            wire.contains(r#""id":null"#),
            "expected id:null in wire form, got: {wire}"
        );
    }

    #[test]
    fn response_id_some_serializes_as_value() {
        let resp = JsonRpcResponse::success(Some(JsonRpcId::I64(7)), serde_json::json!({}));
        let wire = serde_json::to_string(&resp).unwrap();
        assert!(wire.contains(r#""id":7"#));
    }

    #[test]
    fn response_meta_management_has_none_workspace_state() {
        let meta = ResponseMeta::management("8.0.6");
        let wire = serde_json::to_string(&meta).unwrap();
        assert!(!wire.contains("workspace_state"), "wire: {wire}");
        assert!(wire.contains(r#""stale":false"#));
        assert!(wire.contains(r#""daemon_version":"8.0.6""#));
    }

    #[test]
    fn response_meta_loaded_has_loaded_workspace_state() {
        let meta = ResponseMeta::loaded("8.0.6");
        let wire = serde_json::to_string(&meta).unwrap();
        assert!(
            wire.contains(r#""workspace_state":"Loaded""#),
            "wire: {wire}"
        );
    }

    #[test]
    fn response_meta_fresh_from_emits_state() {
        let meta = ResponseMeta::fresh_from(WorkspaceState::Loaded, "8.0.6");
        let wire = serde_json::to_string(&meta).unwrap();
        assert!(
            wire.contains(r#""workspace_state":"Loaded""#),
            "wire: {wire}"
        );
        assert!(wire.contains(r#""stale":false"#), "wire: {wire}");
        // `last_good_at` / `last_error` are omitted for a Fresh verdict.
        assert!(!wire.contains("last_good_at"), "wire: {wire}");
        assert!(!wire.contains("last_error"), "wire: {wire}");

        // Rebuilding is also a valid Fresh variant per `classify_for_serve`.
        let meta_rebuild = ResponseMeta::fresh_from(WorkspaceState::Rebuilding, "8.0.6");
        let wire_rebuild = serde_json::to_string(&meta_rebuild).unwrap();
        assert!(
            wire_rebuild.contains(r#""workspace_state":"Rebuilding""#),
            "wire: {wire_rebuild}"
        );
    }

    #[test]
    fn response_meta_stale_from_rfc3339_and_workspace_state() {
        let anchor =
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_760_000_000);
        let meta = ResponseMeta::stale_from(anchor, Some("boom".to_owned()), "8.0.6");
        let wire = serde_json::to_string(&meta).unwrap();
        assert!(wire.contains(r#""stale":true"#), "wire: {wire}");
        assert!(
            wire.contains(r#""workspace_state":"Failed""#),
            "wire: {wire}"
        );
        assert!(wire.contains(r#""last_error":"boom""#), "wire: {wire}");
        // RFC3339 UTC-Zulu — the rendered timestamp must terminate with `Z"`.
        let last_good_marker = r#""last_good_at":""#;
        let start = wire
            .find(last_good_marker)
            .unwrap_or_else(|| panic!("missing last_good_at in wire: {wire}"))
            + last_good_marker.len();
        let rest = &wire[start..];
        let end = rest
            .find('"')
            .expect("last_good_at must be a closed string");
        let rfc = &rest[..end];
        assert!(rfc.ends_with('Z'), "expected UTC-Zulu, got: {rfc}");
        assert!(
            rfc.contains('T'),
            "RFC3339 must carry a 'T' separator: {rfc}"
        );
    }

    // ------------------------------------------------------------------
    // ShimRegisterAck tests (Phase 8c U1 new surface).
    // ------------------------------------------------------------------

    #[test]
    fn shim_register_ack_accepted_omits_reason_on_wire() {
        let ack = ShimRegisterAck {
            accepted: true,
            daemon_version: "8.0.6".to_owned(),
            reason: None,
            envelope_version: 1,
        };
        let wire = serde_json::to_string(&ack).unwrap();
        assert!(!wire.contains("reason"), "wire: {wire}");
        assert!(wire.contains(r#""accepted":true"#), "wire: {wire}");
        assert!(wire.contains(r#""daemon_version":"8.0.6""#), "wire: {wire}");
        assert!(wire.contains(r#""envelope_version":1"#), "wire: {wire}");
    }

    #[test]
    fn shim_register_ack_rejected_includes_reason() {
        let ack = ShimRegisterAck {
            accepted: false,
            daemon_version: "8.0.6".to_owned(),
            reason: Some("cap".to_owned()),
            envelope_version: 1,
        };
        let wire = serde_json::to_string(&ack).unwrap();
        assert!(wire.contains(r#""reason":"cap""#), "wire: {wire}");
        assert!(wire.contains(r#""accepted":false"#), "wire: {wire}");
    }

    // ------------------------------------------------------------------
    // deny_unknown_fields verification (iter-1 M1 fix).
    // ------------------------------------------------------------------

    #[test]
    fn daemon_hello_rejects_unknown_fields() {
        let wire = r#"{"client_version":"x","protocol_version":1,"extra":true}"#;
        let err = serde_json::from_str::<DaemonHello>(wire)
            .expect_err("DaemonHello must reject unknown fields");
        // serde's `deny_unknown_fields` error message contains
        // "unknown field" — enough to assert without pinning exact phrasing.
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field"),
            "expected 'unknown field' in error, got: {msg}"
        );
    }

    #[test]
    fn shim_register_rejects_unknown_fields() {
        let wire = r#"{"protocol":"lsp","pid":1,"extra":true}"#;
        let err = serde_json::from_str::<ShimRegister>(wire)
            .expect_err("ShimRegister must reject unknown fields");
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field"),
            "expected 'unknown field' in error, got: {msg}"
        );
    }

    // ── daemon/search (verivus-oss/sqry#238 Tier 2) ─────────────────

    #[test]
    fn search_request_roundtrip_minimal() {
        let req = SearchRequest {
            envelope_version: ENVELOPE_VERSION,
            pattern: "foo".into(),
            search_path: "/tmp/ws".into(),
            mode: SearchMode::Exact,
            kind: None,
            lang: None,
            limit: None,
            include_generated: true,
        };
        let wire = serde_json::to_string(&req).expect("serialize");
        let back: SearchRequest = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back, req);
    }

    #[test]
    fn search_request_roundtrip_with_all_filters() {
        let req = SearchRequest {
            envelope_version: ENVELOPE_VERSION,
            pattern: "test_.*".into(),
            search_path: "/srv/repo".into(),
            mode: SearchMode::Regex,
            kind: Some("function".into()),
            lang: Some("rust".into()),
            limit: Some(50),
            include_generated: false,
        };
        let wire = serde_json::to_string(&req).expect("serialize");
        let back: SearchRequest = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back, req);
    }

    #[test]
    fn search_request_mode_lowercase_on_wire() {
        let req = SearchRequest {
            envelope_version: ENVELOPE_VERSION,
            pattern: "f".into(),
            search_path: ".".into(),
            mode: SearchMode::Fuzzy,
            kind: None,
            lang: None,
            limit: None,
            include_generated: true,
        };
        let wire = serde_json::to_string(&req).expect("serialize");
        // The `#[serde(rename_all = "lowercase")]` attribute on SearchMode
        // must surface the variant as `"fuzzy"`, not `"Fuzzy"`.
        assert!(
            wire.contains(r#""mode":"fuzzy""#),
            "expected mode=fuzzy lowercase; got: {wire}"
        );
    }

    #[test]
    fn search_request_rejects_unknown_field() {
        let wire =
            r#"{"envelope_version":1,"pattern":"f","search_path":"/","mode":"exact","bogus":true}"#;
        let err = serde_json::from_str::<SearchRequest>(wire)
            .expect_err("SearchRequest must reject unknown fields");
        assert!(
            err.to_string().contains("unknown field"),
            "expected unknown-field error, got: {err}"
        );
    }

    #[test]
    fn search_request_include_generated_defaults_when_absent() {
        // Forward-compat: a request body that predates the
        // `include_generated` wire field deserialises to the legacy
        // "no filter" behaviour (`true`), preserving the
        // `DAEMON_SEARCH_HANDLER` unit's approved tests after the
        // Codex round-1 CLI shim review fix.
        let wire = r#"{"pattern":"f","search_path":"/","mode":"exact"}"#;
        let req: SearchRequest =
            serde_json::from_str(wire).expect("deserialize w/o include_generated");
        assert!(req.include_generated);
    }

    #[test]
    fn search_request_include_generated_round_trips_when_false() {
        let wire = r#"{"pattern":"f","search_path":"/","mode":"exact","include_generated":false}"#;
        let req: SearchRequest =
            serde_json::from_str(wire).expect("deserialize include_generated=false");
        assert!(!req.include_generated);
        // Re-serialise and confirm the field is preserved.
        let back = serde_json::to_string(&req).expect("serialize");
        assert!(
            back.contains(r#""include_generated":false"#),
            "include_generated=false must survive a round-trip: {back}"
        );
    }

    #[test]
    fn search_request_envelope_version_defaults_when_absent() {
        // Forward-compat: older clients can omit `envelope_version` and
        // the daemon defaults it to ENVELOPE_VERSION.
        let wire = r#"{"pattern":"f","search_path":"/","mode":"exact"}"#;
        let req: SearchRequest =
            serde_json::from_str(wire).expect("deserialize w/o envelope_version");
        assert_eq!(req.envelope_version, ENVELOPE_VERSION);
    }

    #[test]
    fn search_result_roundtrip_empty() {
        let result = SearchResult {
            items: vec![],
            total: 0,
            truncated: false,
            cursor: None,
        };
        let wire = serde_json::to_string(&result).expect("serialize");
        let back: SearchResult = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back, result);
    }

    #[test]
    fn search_result_roundtrip_single_hit() {
        let result = SearchResult {
            items: vec![SearchItem {
                name: "start_kernel".into(),
                qualified_name: "kernel::start_kernel".into(),
                kind: "function".into(),
                language: "c".into(),
                file_path: "/linux/init/main.c".into(),
                start_line: 985,
                start_column: 0,
                end_line: 1100,
                end_column: 1,
                score: None,
            }],
            total: 1,
            truncated: false,
            cursor: None,
        };
        let wire = serde_json::to_string(&result).expect("serialize");
        let back: SearchResult = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back, result);
    }

    #[test]
    fn search_result_roundtrip_truncated_with_score() {
        let result = SearchResult {
            items: (0..3)
                .map(|i| SearchItem {
                    name: format!("hit_{i}"),
                    qualified_name: format!("crate::hit_{i}"),
                    kind: "function".into(),
                    language: "rust".into(),
                    file_path: "/repo/src/lib.rs".into(),
                    start_line: 10 + i,
                    start_column: 0,
                    end_line: 12 + i,
                    end_column: 1,
                    score: Some(0.5_f32 + (i as f32) * 0.1),
                })
                .collect(),
            total: 101,
            truncated: true,
            cursor: None,
        };
        let wire = serde_json::to_string(&result).expect("serialize");
        let back: SearchResult = serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back, result);
    }

    #[test]
    fn search_item_rejects_unknown_field() {
        let wire = r#"{"name":"f","qualified_name":"f","kind":"function","language":"rust","file_path":"/","start_line":1,"start_column":0,"end_line":1,"end_column":0,"bogus":1}"#;
        let err = serde_json::from_str::<SearchItem>(wire)
            .expect_err("SearchItem must reject unknown fields");
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn search_result_rejects_unknown_field() {
        let wire = r#"{"items":[],"total":0,"truncated":false,"bogus":1}"#;
        let err = serde_json::from_str::<SearchResult>(wire)
            .expect_err("SearchResult must reject unknown fields");
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn search_result_score_omitted_when_none() {
        // `skip_serializing_if = "Option::is_none"` on `score` keeps the
        // wire compact for regex/exact hits where no score is meaningful.
        let item = SearchItem {
            name: "f".into(),
            qualified_name: "f".into(),
            kind: "function".into(),
            language: "rust".into(),
            file_path: "/".into(),
            start_line: 1,
            start_column: 0,
            end_line: 1,
            end_column: 0,
            score: None,
        };
        let wire = serde_json::to_string(&item).expect("serialize");
        assert!(
            !wire.contains("\"score\""),
            "score must be omitted when None; got: {wire}"
        );
    }

    #[test]
    fn search_result_envelope_wraps_payload() {
        // SearchResult is intended to ride inside ResponseEnvelope<T>, just
        // like LoadResult / RebuildResult. Verify the wrap.
        let envelope = ResponseEnvelope {
            result: SearchResult {
                items: vec![],
                total: 0,
                truncated: false,
                cursor: None,
            },
            meta: ResponseMeta::management("test"),
        };
        let wire = serde_json::to_string(&envelope).expect("serialize");
        let back: ResponseEnvelope<SearchResult> =
            serde_json::from_str(&wire).expect("deserialize");
        assert_eq!(back.result, envelope.result);
    }
}
