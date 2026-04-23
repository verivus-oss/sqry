//! Re-exports from [`sqry_daemon_protocol::framing`].
//!
//! Phase 8c U1 extracted the framing codec into the
//! `sqry-daemon-protocol` leaf crate. Keeping this module as a thin
//! re-export shim lets every Phase 8a/8b call site inside `sqry-daemon`
//! (`ipc::framing::X`) continue to compile unchanged.

pub use sqry_daemon_protocol::framing::{
    FrameError, MAX_FRAME_BYTES, read_frame, read_frame_json, write_frame, write_frame_json,
};
