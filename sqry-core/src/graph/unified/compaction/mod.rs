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

use crate::graph::unified::concurrent::CodeGraph;

/// Compact both edge stores of `graph` in place: merge each store's delta into
/// a fresh CSR and clear the deltas, so `edges_from` / `edges_to` become O(1)
/// per call (no per-source delta rescans). This is the same edge-compaction
/// sequence that `persist_durable_graph_transaction` runs before writing the
/// snapshot, hoisted out so callers that build a graph but do not persist it
/// (the daemon's cold-load path) can still serve a CSR-backed graph.
///
/// Idempotent: safe on an already-CSR graph (rebuilds the CSR from CSR+delta
/// and clears the empty delta). Compacts BOTH forward and reverse stores, which
/// is required because reverse traversals (`edges_to`) use the reverse store.
///
/// The CPU-heavy CSR builds run under `rayon::join` and hold no external locks
/// (only the brief per-store read while snapshotting and the atomic swap at the
/// end), matching the persist path's lock discipline.
///
/// # Errors
///
/// Returns the underlying [`CompactionError`] if CSR construction fails for
/// either direction. The atomic swap runs only after both CSRs build
/// successfully, so on error the graph's edge stores are left unmodified.
pub fn compact_edges_in_place(graph: &CodeGraph) -> Result<(), CompactionError> {
    let node_count = graph.node_count();

    let forward_snapshot = {
        let forward_store = graph.edges().forward();
        snapshot_edges(&forward_store, node_count)
    };
    let reverse_snapshot = {
        let reverse_store = graph.edges().reverse();
        snapshot_edges(&reverse_store, node_count)
    };

    let (forward_result, reverse_result) = rayon::join(
        || build_compacted_csr(&forward_snapshot, Direction::Forward),
        || build_compacted_csr(&reverse_snapshot, Direction::Reverse),
    );
    let (forward_csr, _forward_stats) = forward_result?;
    let (reverse_csr, _reverse_stats) = reverse_result?;

    graph
        .edges()
        .swap_csrs_and_clear_deltas(forward_csr, reverse_csr);
    Ok(())
}

#[cfg(test)]
mod compact_in_place_tests {
    use super::compact_edges_in_place;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
    use crate::graph::unified::node::id::NodeId;
    use crate::graph::unified::node::kind::NodeKind;
    use crate::graph::unified::storage::arena::NodeEntry;
    use std::path::Path;

    fn alloc_fn(g: &mut CodeGraph, name: &str) -> NodeId {
        let file = g.files_mut().register(Path::new("lib.rs")).unwrap();
        let n = g.strings_mut().intern(name).unwrap();
        g.nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, n, file).with_qualified_name(n))
            .unwrap()
    }

    fn calls(g: &mut CodeGraph, s: NodeId, t: NodeId) {
        let file = g.nodes().get(s).unwrap().file;
        g.edges_mut().add_edge(
            s,
            t,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
    }

    fn sorted_live_edges(g: &CodeGraph) -> Vec<(u32, u32)> {
        let mut e: Vec<(u32, u32)> = g
            .edges()
            .all_live_forward_edges()
            .iter()
            .map(|x| (x.source.index(), x.target.index()))
            .collect();
        e.sort_unstable();
        e
    }

    #[test]
    fn compact_edges_in_place_moves_delta_to_csr_preserving_live_edges() {
        let mut g = CodeGraph::new();
        let a = alloc_fn(&mut g, "a");
        let b = alloc_fn(&mut g, "b");
        let c = alloc_fn(&mut g, "c");
        calls(&mut g, a, b);
        calls(&mut g, a, c);
        calls(&mut g, b, c);
        calls(&mut g, c, c); // self-loop

        // A freshly built graph is delta-backed (this is the daemon's state).
        assert!(
            g.edges().forward().csr().is_none(),
            "expected delta-backed before compaction"
        );
        assert!(g.edges().stats().forward.delta_edge_count > 0);
        let before = sorted_live_edges(&g);
        assert_eq!(before.len(), 4);

        compact_edges_in_place(&g).unwrap();

        // Now CSR-backed with empty deltas on BOTH stores, same live edges.
        assert!(
            g.edges().forward().csr().is_some(),
            "forward must be CSR-backed after compaction"
        );
        assert!(
            g.edges().reverse().csr().is_some(),
            "reverse must be compacted too"
        );
        assert_eq!(g.edges().stats().forward.delta_edge_count, 0);
        assert_eq!(g.edges().stats().reverse.delta_edge_count, 0);
        assert_eq!(
            sorted_live_edges(&g),
            before,
            "live edges must be preserved by compaction"
        );

        // Idempotent: compacting an already-CSR graph preserves edges.
        compact_edges_in_place(&g).unwrap();
        assert!(g.edges().forward().csr().is_some());
        assert_eq!(g.edges().stats().forward.delta_edge_count, 0);
        assert_eq!(
            sorted_live_edges(&g),
            before,
            "idempotent compaction preserves edges"
        );
    }
}
