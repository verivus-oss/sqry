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

/// `daemon/rebuild` success result payload.
///
/// Serialised under the `result` field of [`ResponseEnvelope`]. Reports
/// post-rebuild graph statistics and the wall-clock duration of the rebuild.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RebuildResult {
    /// The canonicalised workspace root path that was rebuilt.
    pub root: std::path::PathBuf,
    /// Wall-clock time the rebuild took, in milliseconds.
    pub duration_ms: u64,
    /// Node count of the freshly published graph.
    pub nodes: u64,
    /// Edge count of the freshly published graph.
    pub edges: u64,
    /// Number of source files indexed in the freshly published graph.
    pub files_indexed: u64,
    /// `true` when the rebuild was a full (non-incremental) rebuild.
    pub was_full: bool,
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
}
