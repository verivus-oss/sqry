//! Edge types for the unified graph architecture.
//!
//! This module provides the core edge-related types:
//! - [`EdgeId`]: Opaque index handle for edges
//! - [`EdgeKind`]: Enumeration of relationship types
//! - [`DeltaBuffer`]: Mutable edge storage with sequence numbers
//! - [`DeltaEdge`]: Individual edge mutation entry
//! - [`EdgeStore`]: Two-tier edge storage (CSR + `DeltaBuffer`)
//! - [`BidirectionalEdgeStore`]: Forward + reverse edge storage
//!
//! # Design Principles
//!
//! - **Comprehensive taxonomy**: `EdgeKind` covers structural, reference, OOP,
//!   cross-language, and extensible relationship types
//! - **Cross-boundary support**: First-class support for HTTP, gRPC, FFI, and
//!   message queue edges for service-oriented analysis
//! - **Two-tier storage**: Delta buffer for writes, CSR for reads
//! - **Bidirectional traversal**: Efficient callers/callees queries

pub mod bidirectional;
pub mod delta;
pub mod id;
pub mod kind;
pub mod store;

pub use bidirectional::{BidirectionalEdgeStore, BidirectionalEdgeStoreStats};
pub use delta::{DeltaBuffer, DeltaBufferStats, DeltaEdge, DeltaOp, EdgeKey};
pub use id::EdgeId;
pub use kind::{
    DbQueryType, EdgeKind, ExportKind, FfiConvention, HttpMethod, LifetimeConstraintKind,
    MacroExpansionKind, MqProtocol, ResolvedVia, TableWriteOp, WrapKind,
};
pub use store::{EdgeStore, EdgeStoreError, EdgeStoreStats, StoreEdgeRef};
