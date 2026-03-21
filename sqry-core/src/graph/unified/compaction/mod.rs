//! Compaction components for the unified graph architecture.
//!
//! This module provides compaction utilities:
//! - [`merge_delta_edges`]: Sequence-numbered last-writer-wins merge (Step 13)
//! - [`CompactionCheckpoint`]: State snapshot for rollback (Step 14)
//! - [`CompactionError`]: Error types with failure state guarantees (Step 14b)
//! - [`build_compacted_csr`]: Offline CSR construction (Step 15a)
//! - [`swap_bidirectional_csr`]: Two-phase atomic CSR swap (Step 15b)
//! - [`CompactionScheduler`]: Threshold-based compaction scheduling (Step 16)
//! - [`compact_interruptible`]: Chunk-based compaction with yield points (Step 17)
//!
//! # Design Principles
//!
//! - **Deterministic merge**: Highest sequence number wins per edge key
//! - **Remove filtering**: Removed edges are excluded from output
//! - **Stable ordering**: Edges with same key maintain sequence-based order
//! - **Atomic rollback**: Checkpoints capture complete state for recovery
//! - **Documented failures**: Each error variant has explicit state guarantees
//! - **Lock-free build**: CSR construction without holding locks
//! - **Threshold scheduling**: Automatic compaction based on operation count and tombstone ratio
//! - **Interruptible**: Compaction can be cancelled between chunks

pub mod build;
pub mod checkpoint;
pub mod errors;
pub mod interruptible;
pub mod merge;
pub mod scheduler;
pub mod swap;

pub use build::{
    BuildStats, CompactionSnapshot, build_compacted_csr, build_csr_from_edges, snapshot_edges,
};
pub use checkpoint::{
    CheckpointStats, CompactionCheckpoint, CounterCheckpoint, EdgeStoreCheckpoint,
};
pub use errors::{
    BuildFailureReason, CompactionError, CompactionPhase, ComponentState, CounterReconcileState,
    Direction, InterruptReason, PostErrorState, SwapFailureReason, SwapPreconditionError,
    SwapPreconditions,
};
pub use interruptible::{
    CancellationToken, CompactionProgress, DEFAULT_CHUNK_SIZE, InterruptibleConfig,
    InterruptibleResult, InterruptibleStats, InterruptibleStatsSnapshot, compact_interruptible,
};
pub use merge::{MergeStats, MergedEdge, merge_delta_edges};
pub use scheduler::{CompactionScheduler, CompactionThresholds, CompactionTrigger, SchedulerStats};
pub use swap::{SwapInput, SwapResult, swap_bidirectional_csr, swap_single_csr};
