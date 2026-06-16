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

// Core node materialization and seed lookup
pub mod materialize;

// Shared traversal result types and EdgeClassification
pub mod traversal;

// Generic BFS kernel
pub mod kernel;

// Binding query facade
pub mod bind;

// Heap memory size reporting
pub mod memory;

// Shared symbol resolution
pub mod resolution;

// Transaction API (Workstream A)
pub mod txn;

// Build pipeline (Steps 19-21)
pub mod build;

// Mutation-plane trait that lets Pass 1-5 helpers operate uniformly on
// `CodeGraph` (full build) or `RebuildGraph` (incremental rebuild).
// See module docs for the Phase 1 / Phase 2 migration split; the trait
// itself is `pub(crate)` so external crates cannot name it.
pub(crate) mod mutation_target;

// Persistence layer (Workstream H)
pub mod persistence;

// Graph analysis precomputation (Pass 5)
pub mod analysis;

// Publish-boundary invariant helper (sqryd daemon, Task 4 Gate 0d).
// Single source of truth for bijection + tombstone-residue assertions.
// Every publish call path (full rebuild end, `RebuildGraph::finalize`
// step 13+14, Task 6's `WorkspaceManager::publish_graph`, §E harness)
// routes through `assert_publish_invariants`.
pub mod publish;

// Incremental rebuild pre-implementation gates (sqryd daemon, Task 4).
// Gate 0b — NodeIdBearing trait + §K master coverage matrix.
// Gate 0c/0d will extend this module with RebuildGraph + finalize() and
// the bijection / tombstone-residue invariants.
//
// Visibility: `pub(crate)` by default so the coverage matrix and the
// NodeIdBearing trait are an implementation detail; **`pub` under the
// `rebuild-internals` feature** so `sqry-daemon` (the only whitelisted
// consumer per `tests/rebuild_internals_whitelist.rs`) can reach
// [`rebuild::RebuildGraph`] and the public
// `CodeGraph::clone_for_rebuild` entry point. Even when `rebuild` is
// `pub`, the individual struct fields on `RebuildGraph` and the
// `__assemble_from_rebuild_parts_internal` constructor on `CodeGraph`
// remain `pub(crate)`, enforcing the A2 §H "Type-enforced publish
// path" invariant at the downstream-crate boundary.
#[cfg(not(feature = "rebuild-internals"))]
pub(crate) mod rebuild;

#[cfg(feature = "rebuild-internals")]
pub mod rebuild;

// Loom concurrency tests (Steps 28-30)
#[cfg(all(test, feature = "loom"))]
pub mod loom_tests;

// Test-only canonical-arena helper for WS1 round-trip property tests
// (DAG unit `U_WS1_12_PERSIST_RT`, DESIGN §2.7 of
// `docs/development/graph-fidelity-planner-correctness/02_DESIGN-graph-fidelity-planner-correctness.md`).
// Gated behind `cfg(any(test, feature = "test-support"))` so the
// symbols never link into release binaries — verified by the WS1
// gate `nm target/release/sqry | grep -c canonical_arena` returning 0.
#[cfg(any(test, feature = "test-support"))]
pub mod test_helpers;

// Re-exports for convenience
pub use admission::{
    AdmissionController, AdmissionControllerStats, AdmissionError, BufferStateSnapshot,
    CommitError, Reservation, ReservationGuard, SharedBufferState,
};
pub use bind::{BindingQuery, BindingResult, ResolvedBinding, SymbolClassification};
pub use build::{
    GraphBuildHelper, HelperStats, IdentityIndex, IdentityKey, IncrementalStats,
    IntraFileReference, Pass3Stats, PendingEdge, StagingGraph, StagingOp, add_edge_incremental,
    pass3_intra_edges, remove_file_nodes,
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
    MacroExpansionKind, MqProtocol, ResolvedVia, StoreEdgeRef, TableWriteOp,
};
pub use file::FileId;
pub use kernel::{
    EdgeFilter, FrontierMode, SccPathStrategy, SimplePathStrategy, TraversalConfig,
    TraversalDirection, TraversalLimits, TraversalStrategy, VisitedPolicy, is_followable_edge,
    traverse,
};
pub use materialize::{
    MaterializedNode, collect_symbol_seeds, display_entry_qualified_name, find_nodes_by_name,
    materialize_node, qualified_node_name,
};
pub use memory::{BTREEMAP_ENTRY_OVERHEAD, GraphMemorySize, HASHMAP_ENTRY_OVERHEAD};
pub use node::{GenerationOverflowError, NodeId, NodeKind};
// Re-export the public Go-plugin-facing receiver pointerness enum from
// `mutation_target`. The trait itself stays `pub(crate)` — only the
// public *data type* used by `GoEmbeddingHint::pointerness` is exposed
// here so external plugin crates (notably `sqry-lang-go`) can construct
// hint records during their Phase-1 parse.
pub use mutation_target::Receiver;
pub use resolution::{
    FileScope, FileScopeError, NormalizedSymbolQuery, ResolutionMode, ResolvedFileScope,
    SymbolCandidateBucket, SymbolCandidateOutcome, SymbolCandidateSearchWitness,
    SymbolCandidateWitness, SymbolQuery, SymbolResolutionOutcome, SymbolResolutionWitness,
};
pub use storage::{
    AuxiliaryIndices, ClasspathNodeMetadata, CsrBuilder, CsrError, CsrGraph, CsrStats, EdgeRef,
    FileRegistry, IndicesStats, InternerStats, MacroNodeMetadata, NodeArena, NodeEntry, NodeFlags,
    NodeMetadataStore, ProcMacroFunctionKind, RegistryError, RegistryStats, ResolveError, Slot,
    SlotState, StoredEntry, StringInterner, TypedMetadata,
};
pub use string::StringId;
pub use traversal::{
    EdgeClassification, MaterializedEdge, TraversalMetadata, TraversalResult, TruncationReason,
};
pub use txn::GraphWriteTxn;
