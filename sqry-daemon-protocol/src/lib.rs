//! `sqry-daemon-protocol` — sqryd daemon wire types + framing codec.
//!
//! This is a **leaf crate** with no dependencies on any other `sqry-*`
//! crate. Both the sqryd daemon itself (`sqry-daemon`) and the
//! client-side crates that need to speak the wire format
//! (`sqry-daemon-client`, `sqry-lsp`, `sqry-mcp`) depend on this crate
//! without creating a dependency cycle.
//!
//! # Module layout
//!
//! - [`framing`] — 4-byte little-endian length-prefix codec
//!   ([`framing::read_frame`] / [`framing::write_frame`] plus typed
//!   JSON wrappers and [`framing::FrameError`]).
//! - [`protocol`] — wire types: [`DaemonHello`], [`DaemonHelloResponse`],
//!   [`ShimProtocol`], [`ShimRegister`], [`ShimRegisterAck`],
//!   [`ResponseEnvelope`], [`ResponseMeta`], [`WorkspaceState`],
//!   [`JsonRpcVersion`], [`JsonRpcId`], [`JsonRpcRequest`],
//!   [`JsonRpcResponse`], [`JsonRpcPayload`], [`JsonRpcError`].
//!
//! # First-frame discrimination
//!
//! The router in `sqry-daemon` shape-discriminates the very first
//! frame between [`DaemonHello`] (CLI / JSON-RPC clients) and
//! [`ShimRegister`] (Phase 8c shim byte-pump clients). All four
//! first-frame structs — [`DaemonHello`], [`DaemonHelloResponse`],
//! [`ShimRegister`], [`ShimRegisterAck`] — carry
//! `#[serde(deny_unknown_fields)]` so that a first frame matching
//! neither shape is rejected with `-32600 Invalid Request` rather than
//! being silently routed to the wrong path.

pub mod framing;
pub mod protocol;

pub use protocol::{
    CancelRebuildResult, DaemonHello, DaemonHelloResponse, ENVELOPE_VERSION, JsonRpcError,
    JsonRpcId, JsonRpcPayload, JsonRpcRequest, JsonRpcResponse, JsonRpcVersion, LoadResult,
    LogicalWorkspaceWire, RebuildResult, RebuildStatus, ResponseEnvelope, ResponseMeta, SearchItem,
    SearchMode, SearchRequest, SearchResult, ShimProtocol, ShimRegister, ShimRegisterAck,
    SourceRootBinding, WorkspaceId, WorkspaceIndexStatus, WorkspaceSourceRootStatus,
    WorkspaceState,
};
