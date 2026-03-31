//! Unified Graph Architecture for sqry.
//!
//! This module implements sqry's high-performance graph architecture for
//! cross-language code analysis, as specified in:
//! - `docs/architecture/UNIFIED_GRAPH_ARCHITECTURE.md`
//! - `docs/development/graph-architecture-reconcile-*/01_SPEC.md`
//!
//! # Architecture Overview
//!
//! The unified graph architecture provides:
//!
//! - **Arena + CSR storage**: Cache-friendly node storage with compressed sparse row
//!   edge format for O(1) traversal
//! - **Generational indices**: `NodeId` uses generation counters to detect stale
//!   references after deletion
//! - **Two-tier edge storage**: Stable CSR tier for reads, delta buffer for writes
//! - **MVCC concurrency**: Single-writer, multi-reader with epoch-based snapshots
//! - **Back-pressure admission**: Reservation-based admission control for delta buffers
//!
//! # Module Structure
//!
//! ```text
//! unified/
//! ├── node/           - Node handle and kind types
//! │   ├── id.rs       - NodeId with generational index
//! │   └── kind.rs     - NodeKind enumeration
//! ├── edge/           - Edge handle and kind types
//! │   ├── id.rs       - EdgeId opaque handle
//! │   └── kind.rs     - EdgeKind enumeration
//! ├── string/         - String interning
//! │   └── id.rs       - StringId handle
//! ├── file/           - File registry
//! │   └── id.rs       - FileId handle
//! └── admission/      - Back-pressure control
//!     ├── state.rs    - SharedBufferState
//!     └── reservation.rs - ReservationGuard RAII
//! ```
//!
//! # Usage
//!
//! The unified architecture is enabled via the `unified-graph` feature flag.
//! When enabled, it provides the new Arena+CSR storage. The legacy DashMap
//! storage remains available via the `legacy-graph` feature for fallback.
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::{NodeId, NodeKind, EdgeKind};
//!
//! // Create a node reference
//! let node_id = NodeId::new(42, 1);  // index 42, generation 1
//!
//! // Check if still valid
//! if !node_id.is_invalid() {
//!     // Use in graph operations
//! }
//! ```
//!
//! # Design Principles
//!
//! 1. **Type safety**: Opaque handles prevent index confusion
//! 2. **Memory efficiency**: String interning, compressed storage
//! 3. **Concurrency safety**: RAII guards, atomic counters, epoch versioning
//! 4. **Correctness first**: Generational indices, reservation validation

// Foundation handle types (Step 1)
pub mod edge;
pub mod file;
pub mod node;
pub mod string;

// Admission control (Steps 3-4)
pub mod admission;

// Storage components (Steps 5-7)
pub mod storage;

// Compaction components (Steps 13-14)
pub mod compaction;

// Concurrency wrappers (Step 18)
pub mod concurrent;

// Shared symbol resolution
pub mod resolution;

// Transaction API (Workstream A)
pub mod txn;

// Query adapter (Workstream C)
pub mod query_adapter;

// Build pipeline (Steps 19-21)
pub mod build;

// Persistence layer (Workstream H)
pub mod persistence;

// Graph analysis precomputation (Pass 5)
pub mod analysis;

// Loom concurrency tests (Steps 28-30)
#[cfg(all(test, feature = "loom"))]
pub mod loom_tests;

// Re-exports for convenience
pub use admission::{
    AdmissionController, AdmissionControllerStats, AdmissionError, BufferStateSnapshot,
    CommitError, Reservation, ReservationGuard, SharedBufferState,
};
pub use build::{
    ExportMap, GraphBuildHelper, HelperStats, IdentityIndex, IdentityKey, IncrementalStats,
    IntraFileReference, Pass3Stats, Pass4Stats, PendingEdge, StagingGraph, StagingOp,
    add_edge_incremental, pass3_intra_edges, pass4_cross_file, remove_file_nodes,
};
pub use compaction::{
    BuildFailureReason, CheckpointStats, CompactionCheckpoint, CompactionError, CompactionPhase,
    ComponentState, CounterCheckpoint, CounterReconcileState, Direction, EdgeStoreCheckpoint,
    InterruptReason, MergeStats, MergedEdge, PostErrorState, SwapFailureReason,
    SwapPreconditionError, SwapPreconditions, merge_delta_edges,
};
pub use concurrent::{
    ChannelError, ChannelStats, CodeGraph, ConcurrentCodeGraph, GraphSnapshot, GraphUpdate,
    UpdateChannel, UpdateReceiver,
};
pub use edge::{
    BidirectionalEdgeStore, BidirectionalEdgeStoreStats, DbQueryType, DeltaBuffer,
    DeltaBufferStats, DeltaEdge, DeltaOp, EdgeId, EdgeKey, EdgeKind, EdgeStore, EdgeStoreError,
    EdgeStoreStats, ExportKind, FfiConvention, HttpMethod, LifetimeConstraintKind,
    MacroExpansionKind, MqProtocol, StoreEdgeRef, TableWriteOp,
};
pub use file::FileId;
pub use node::{GenerationOverflowError, NodeId, NodeKind};
pub use query_adapter::GraphQueryAdapter;
pub use resolution::{
    FileScope, FileScopeError, NormalizedSymbolQuery, ResolutionMode, ResolvedFileScope,
    SymbolCandidateOutcome, SymbolQuery, SymbolResolutionOutcome,
};
pub use storage::{
    AuxiliaryIndices, CsrBuilder, CsrError, CsrGraph, CsrStats, EdgeRef, FileRegistry,
    IndicesStats, InternerStats, MacroNodeMetadata, NodeArena, NodeEntry, NodeMetadataStore,
    ProcMacroFunctionKind, RegistryError, RegistryStats, ResolveError, Slot, SlotState,
    StringInterner,
};
pub use string::StringId;
pub use txn::GraphWriteTxn;
