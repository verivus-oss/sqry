//! IPC surface of the sqryd daemon.
//!
//! Task 8 Phase 8a wires the wire format, framing, version handshake,
//! request validator, and the first four daemon management methods
//! (`daemon/status`, `daemon/load`, `daemon/unload`, `daemon/stop`) that
//! together give a CLI client a fully usable remote-control channel.
//!
//! # Module layout
//!
//! - [`framing`] — 4-byte little-endian length-prefix codec.
//! - [`protocol`] — wire types: [`protocol::DaemonHello`],
//!   [`protocol::DaemonHelloResponse`], [`protocol::ResponseEnvelope`],
//!   [`protocol::ResponseMeta`], [`protocol::JsonRpcRequest`],
//!   [`protocol::JsonRpcResponse`], [`protocol::JsonRpcError`],
//!   [`protocol::JsonRpcId`].
//! - [`validation`] — JSON-RPC 2.0 request validator.
//! - [`path_policy`] — canonicalisation policy for
//!   [`crate::workspace::WorkspaceKey`] construction from user paths.
//! - [`shim_registry`] — shim-connection registry (Phase 8c public surface).
//! - [`server`] — [`server::IpcServer`] accept loop (UDS + named pipe).
//! - [`router`] — connection router + batch dispatch.
//! - [`methods`] — one module per JSON-RPC method handler.
//!
//! # Concurrency
//!
//! The accept loop and every per-connection task are spawned on the
//! current Tokio runtime. The router holds no locks across `.await`
//! points; [`crate::workspace::WorkspaceManager`]-mutating method
//! handlers use [`tokio::task::spawn_blocking`] when they call sync
//! long-running operations (e.g. [`crate::workspace::WorkspaceManager::get_or_load`]).
//! §J.4 lock order (`workspaces → rebuild_lane → admission`) is
//! preserved: no IPC-owned lock is ever acquired.
//!
//! # Shutdown
//!
//! IPC server construction takes a
//! [`tokio_util::sync::CancellationToken`]. The `daemon/stop` method
//! and Task 9's signal handler both flip the token; the accept loop
//! observes cancellation in a biased `tokio::select!` arm and drains
//! active connections for up to
//! [`crate::config::DaemonConfig::ipc_shutdown_drain_secs`] before
//! returning.

pub mod framing;
pub(crate) mod methods;
pub(crate) mod path_policy;
pub mod protocol;
pub(crate) mod router;
pub mod server;
pub mod shim_registry;
pub mod tool_core;
pub mod validation;

pub use protocol::{
    CancelRebuildResult, DaemonHello, DaemonHelloResponse, JsonRpcError, JsonRpcId, JsonRpcPayload,
    JsonRpcRequest, JsonRpcResponse, JsonRpcVersion, RebuildResult, ResponseEnvelope, ResponseMeta,
};
pub use server::IpcServer;
pub use shim_registry::{ShimConnEntry, ShimConnId, ShimHandle, ShimRegistry};
