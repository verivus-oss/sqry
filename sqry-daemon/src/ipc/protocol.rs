//! Re-exports from [`sqry_daemon_protocol::protocol`].
//!
//! Phase 8c U1 extracted the wire types into the `sqry-daemon-protocol`
//! leaf crate so the client-side crates (`sqry-daemon-client`,
//! `sqry-lsp`, `sqry-mcp`) can depend on the wire format without
//! pulling in the full daemon and creating a dep cycle. Keeping this
//! module as a thin re-export shim lets every Phase 8a/8b call site
//! inside `sqry-daemon` (`ipc::protocol::X`) continue to compile
//! unchanged.

pub use sqry_daemon_protocol::protocol::{
    CancelRebuildResult, DaemonHello, DaemonHelloResponse, JsonRpcError, JsonRpcId, JsonRpcPayload,
    JsonRpcRequest, JsonRpcResponse, JsonRpcVersion, RebuildResult, ResponseEnvelope, ResponseMeta,
    ShimProtocol, ShimRegister, ShimRegisterAck, WorkspaceState,
};
