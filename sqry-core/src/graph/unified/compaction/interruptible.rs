//! Interruptible Compaction: Chunk-based compaction with yield points.
//!
//! This module implements FR-50 (Interruptible Compaction) to ensure LSP
//! responsiveness during compaction operations.
//!
//! # Design (FR-50)
//!
//! - **Chunk-based processing**: Process edges in chunks of 10K (configurable)
//! - **Yield points**: Callback invoked between chunks for cooperative scheduling
//! - **Cancellation support**: Atomic flag to request compaction abort
//! - **Progress tracking**: Monitor compaction progress
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::compaction::interruptible::{
//!     CancellationToken, InterruptibleConfig, compact_interruptible,
//! };
//!
//! let token = CancellationToken::new();
//! let config = InterruptibleConfig::default();
//!
//! // In another thread, can cancel:
//! // token.cancel();
//!
//! let result = compact_interruptible(
//!     &snapshot,
//!     node_count,
//!     &token,
//!     &config,
//!     |progress| {
//!         // Yield point - release locks, check interrupts
//!         println!("Progress: {}%", progress.percent_complete());
//!     },
//! );
//! ```

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::build::CompactionSnapshot;
use super::errors::{CompactionError, InterruptReason};
use super::merge::{MergeStats, MergedEdge, merge_delta_edges};
use crate::graph::unified::edge::DeltaEdge;

/// Default chunk size: 10,000 edges per chunk.
pub const DEFAULT_CHUNK_SIZE: usize = 10_000;

/// Cancellation token for interruptible operations.
///
/// Thread-safe flag that signals compaction should be cancelled.
/// Can be cloned and shared across threads.
#[derive(Debug, Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// Creates a new cancellation token (not cancelled).
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Checks if cancellation was requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Resets the token to non-cancelled state.
    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::Release);
    }
}

/// Configuration for interruptible compaction.
#[derive(Debug, Clone, Copy)]
pub struct InterruptibleConfig {
    /// Number of edges to process per chunk.
    /// Default: 10,000 (10K)
    pub chunk_size: usize,

    /// Whether to check cancellation between chunks.
    /// Default: true
    pub check_cancellation: bool,
}

impl Default for InterruptibleConfig {
    fn default() -> Self {
        Self {
            chunk_size: DEFAULT_CHUNK_SIZE,
            check_cancellation: true,
        }
    }
}

impl InterruptibleConfig {
    /// Creates config with custom chunk size.
    #[must_use]
    pub fn with_chunk_size(chunk_size: usize) -> Self {
        Self {
            chunk_size: chunk_size.max(1), // At least 1
            ..Default::default()
        }
    }

    /// Disables cancellation checking (for benchmarking).
    #[must_use]
    pub fn without_cancellation_check(mut self) -> Self {
        self.check_cancellation = false;
        self
    }
}

/// Progress information passed to yield callback.
#[derive(Debug, Clone, Copy)]
pub struct CompactionProgress {
    /// Total number of edges to process.
    pub total_edges: usize,
    /// Number of edges processed so far.
    pub edges_processed: usize,
    /// Current chunk number (1-indexed).
    pub current_chunk: usize,
    /// Total number of chunks.
    pub total_chunks: usize,
}

impl CompactionProgress {
    /// Returns completion percentage (0-100).
    #[must_use]
    pub fn percent_complete(&self) -> u8 {
        if self.total_edges == 0 {
            return 100;
        }
        let pct = (self.edges_processed.saturating_mul(100)) / self.total_edges;
        u8::try_from(pct.min(100)).unwrap_or(u8::MAX)
    }

    /// Returns true if compaction is complete.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.edges_processed >= self.total_edges
    }
}

impl fmt::Display for CompactionProgress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "chunk {}/{}: {}/{} edges ({}%)",
            self.current_chunk,
            self.total_chunks,
            self.edges_processed,
            self.total_edges,
            self.percent_complete()
        )
    }
}

/// Result of interruptible compaction.
#[derive(Debug)]
pub struct InterruptibleResult {
    /// Merged edges (surviving Add operations).
    pub merged_edges: Vec<MergedEdge>,
    /// Statistics about the merge process.
    pub merge_stats: MergeStats,
    /// Number of chunks processed.
    pub chunks_processed: usize,
    /// Whether compaction was cancelled.
    pub was_cancelled: bool,
}

impl InterruptibleResult {
    /// Returns true if compaction completed fully.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.was_cancelled
    }
}

/// Statistics tracker for interruptible operations.
#[derive(Debug, Default)]
pub struct InterruptibleStats {
    /// Number of compactions started.
    pub started: AtomicU64,
    /// Number of compactions completed.
    pub completed: AtomicU64,
    /// Number of compactions cancelled.
    pub cancelled: AtomicU64,
    /// Total chunks processed across all compactions.
    pub total_chunks: AtomicU64,
}

impl InterruptibleStats {
    /// Creates new stats tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a compaction start.
    pub fn record_start(&self) {
        self.started.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a compaction completion.
    pub fn record_complete(&self, chunks: usize) {
        self.completed.fetch_add(1, Ordering::Relaxed);
        self.total_chunks
            .fetch_add(chunks as u64, Ordering::Relaxed);
    }

    /// Records a compaction cancellation.
    pub fn record_cancel(&self, chunks: usize) {
        self.cancelled.fetch_add(1, Ordering::Relaxed);
        self.total_chunks
            .fetch_add(chunks as u64, Ordering::Relaxed);
    }

    /// Returns snapshot of current stats.
    #[must_use]
    pub fn snapshot(&self) -> InterruptibleStatsSnapshot {
        InterruptibleStatsSnapshot {
            started: self.started.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            total_chunks: self.total_chunks.load(Ordering::Relaxed),
        }
    }
}

/// Snapshot of interruptible stats.
#[derive(Debug, Clone, Copy, Default)]
pub struct InterruptibleStatsSnapshot {
    /// Number of compactions started.
    pub started: u64,
    /// Number of compactions completed.
    pub completed: u64,
    /// Number of compactions cancelled.
    pub cancelled: u64,
    /// Total chunks processed.
    pub total_chunks: u64,
}

impl InterruptibleStatsSnapshot {
    /// Returns completion rate as percentage.
    #[must_use]
    pub fn completion_rate(&self) -> u8 {
        if self.started == 0 {
            return 100;
        }
        let rate = (self.completed.saturating_mul(100)) / self.started;
        rate.min(100) as u8
    }

    /// Returns cancellation rate as percentage.
    #[must_use]
    pub fn cancellation_rate(&self) -> u8 {
        if self.started == 0 {
            return 0;
        }
        let rate = (self.cancelled.saturating_mul(100)) / self.started;
        rate.min(100) as u8
    }
}

/// Performs interruptible compaction with yield points.
///
/// Processes delta edges in chunks, calling the yield callback between chunks.
/// This allows the caller to release locks, check for cancellation, or perform
/// other cooperative scheduling tasks.
///
/// # Arguments
///
/// * `snapshot` - Snapshot of delta edges to compact
/// * `token` - Cancellation token to check between chunks
/// * `config` - Interruptible compaction configuration
/// * `on_yield` - Callback invoked after each chunk with progress info
///
/// # Returns
///
/// Returns merged edges and stats, or error if compaction failed.
///
/// # Errors
///
/// Returns `CompactionError::Interrupted` if cancellation was requested.
pub fn compact_interruptible<F>(
    snapshot: &CompactionSnapshot,
    token: &CancellationToken,
    config: &InterruptibleConfig,
    mut on_yield: F,
) -> Result<InterruptibleResult, CompactionError>
where
    F: FnMut(&CompactionProgress),
{
    let total_edges = snapshot.delta_edges.len();
    let chunk_size = config.chunk_size;
    let total_chunks = if total_edges == 0 {
        0
    } else {
        total_edges.div_ceil(chunk_size)
    };

    // Check for early cancellation
    if config.check_cancellation && token.is_cancelled() {
        return Err(CompactionError::Interrupted {
            reason: InterruptReason::CancellationRequested,
            edges_processed: 0,
            edges_total: total_edges,
        });
    }

    // For small snapshots, process in one go
    if total_edges <= chunk_size {
        let (merged_edges, merge_stats) = merge_delta_edges(snapshot.delta_edges.clone());

        // Final yield with complete status
        let progress = CompactionProgress {
            total_edges,
            edges_processed: total_edges,
            current_chunk: 1,
            total_chunks: 1.max(total_chunks),
        };
        on_yield(&progress);

        return Ok(InterruptibleResult {
            merged_edges,
            merge_stats,
            chunks_processed: 1,
            was_cancelled: false,
        });
    }

    // Process in chunks - collect winning operations (preserving removes)
    // This fixes the cross-chunk remove bug: removes in later chunks
    // can now correctly cancel adds from earlier chunks.
    let mut all_winners: Vec<DeltaEdge> = Vec::new();
    let mut edges_processed = 0;
    let mut chunk_dedup_count = 0;

    for (chunk_idx, chunk) in snapshot.delta_edges.chunks(chunk_size).enumerate() {
        // Check cancellation before processing chunk
        if config.check_cancellation && token.is_cancelled() {
            return Err(CompactionError::Interrupted {
                reason: InterruptReason::CancellationRequested,
                edges_processed,
                edges_total: total_edges,
            });
        }

        // Deduplicate within chunk, preserving op types (including removes)
        let chunk_input_count = chunk.len();
        let chunk_winners = dedupe_chunk_preserving_ops(chunk.to_vec());
        chunk_dedup_count += chunk_input_count - chunk_winners.len();

        all_winners.extend(chunk_winners);
        edges_processed += chunk.len();

        // Yield point
        let progress = CompactionProgress {
            total_edges,
            edges_processed,
            current_chunk: chunk_idx + 1,
            total_chunks,
        };
        on_yield(&progress);
    }

    // Check cancellation before final global merge (which can be expensive)
    if config.check_cancellation && token.is_cancelled() {
        return Err(CompactionError::Interrupted {
            reason: InterruptReason::CancellationRequested,
            edges_processed,
            edges_total: total_edges,
        });
    }

    // Final global merge: apply LWW across all chunks, then filter removes
    // This correctly handles cross-chunk removes canceling adds
    let (final_merged, final_stats) = merge_winners_global(all_winners);

    Ok(InterruptibleResult {
        merged_edges: final_merged,
        merge_stats: MergeStats {
            input_count: total_edges,
            output_count: final_stats.output_count,
            deduplicated_count: chunk_dedup_count + final_stats.deduplicated_count,
            removed_count: final_stats.removed_count,
        },
        chunks_processed: total_chunks,
        was_cancelled: false,
    })
}

/// Deduplicates delta edges within a chunk, preserving operation types.
///
/// This function applies LWW within a chunk but does NOT filter removes.
/// The winning operation (Add or Remove) for each edge key is preserved
/// for later cross-chunk reconciliation.
///
/// # Arguments
///
/// * `edges` - Delta edges to deduplicate
///
/// # Returns
///
/// Vector of winning delta operations (including removes)
fn dedupe_chunk_preserving_ops(mut edges: Vec<DeltaEdge>) -> Vec<DeltaEdge> {
    use std::cmp::Ordering;

    if edges.is_empty() {
        return vec![];
    }

    // Sort by edge key, then by DESCENDING seq (highest seq first)
    edges.sort_by(|a, b| {
        let key_a = a.edge_key();
        let key_b = b.edge_key();

        // Compare by source
        match a.source.index().cmp(&b.source.index()) {
            Ordering::Equal => {} // Continue to next comparison
            other => return other,
        }
        match a.source.generation().cmp(&b.source.generation()) {
            Ordering::Equal => {} // Continue to next comparison
            other => return other,
        }

        // Compare by target
        match a.target.index().cmp(&b.target.index()) {
            Ordering::Equal => {} // Continue to next comparison
            other => return other,
        }
        match a.target.generation().cmp(&b.target.generation()) {
            Ordering::Equal => {} // Continue to next comparison
            other => return other,
        }

        // Compare by kind (using debug format for stability)
        match format!("{:?}", key_a.kind).cmp(&format!("{:?}", key_b.kind)) {
            Ordering::Equal => {} // Continue to final seq comparison
            other => return other,
        }

        // DESCENDING seq (highest first)
        b.seq.cmp(&a.seq)
    });

    // Dedup by edge key (keeps first = highest seq due to descending sort)
    edges.dedup_by(|a, b| a.edge_key() == b.edge_key());

    edges
}

/// Merges winning delta operations from all chunks with global LWW semantics.
///
/// This function performs the final cross-chunk merge, correctly handling
/// the case where a Remove in one chunk should cancel an Add from another chunk.
///
/// # Algorithm
///
/// 1. Deduplicate all winning ops by edge key (LWW - highest seq wins)
/// 2. Filter out removes (only Add operations survive)
/// 3. Convert to `MergedEdge` for output
///
/// # Arguments
///
/// * `all_winners` - Winning delta operations from all chunks
///
/// # Returns
///
/// Tuple of (merged edges, statistics)
fn merge_winners_global(all_winners: Vec<DeltaEdge>) -> (Vec<MergedEdge>, MergeStats) {
    // Just use merge_delta_edges which handles this correctly
    // It does: sort by key+desc_seq, dedup, filter removes
    merge_delta_edges(all_winners)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::edge::{DeltaEdge, DeltaOp, EdgeKind};
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeId;

    fn make_delta_edge(src: u32, tgt: u32, seq: u64, is_remove: bool) -> DeltaEdge {
        DeltaEdge {
            source: NodeId::new(src, 0),
            target: NodeId::new(tgt, 0),
            kind: EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            seq,
            op: if is_remove {
                DeltaOp::Remove
            } else {
                DeltaOp::Add
            },
            file: FileId::new(0),
            spans: vec![],
        }
    }

    fn make_snapshot(delta_edges: Vec<DeltaEdge>) -> CompactionSnapshot {
        CompactionSnapshot {
            csr_edges: Vec::new(),
            delta_edges,
            node_count: 100,
            csr_version: 0,
        }
    }

    #[test]
    fn test_cancellation_token_default() {
        let token = CancellationToken::default();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn test_cancellation_token_cancel() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());

        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn test_cancellation_token_reset() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(token.is_cancelled());

        token.reset();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn test_cancellation_token_clone() {
        let token1 = CancellationToken::new();
        let token2 = token1.clone();

        token1.cancel();
        assert!(token2.is_cancelled());
    }

    #[test]
    fn test_config_default() {
        let config = InterruptibleConfig::default();
        assert_eq!(config.chunk_size, DEFAULT_CHUNK_SIZE);
        assert!(config.check_cancellation);
    }

    #[test]
    fn test_config_custom_chunk_size() {
        let config = InterruptibleConfig::with_chunk_size(5000);
        assert_eq!(config.chunk_size, 5000);
    }

    #[test]
    fn test_config_min_chunk_size() {
        let config = InterruptibleConfig::with_chunk_size(0);
        assert_eq!(config.chunk_size, 1); // Minimum is 1
    }

    #[test]
    fn test_config_without_cancellation() {
        let config = InterruptibleConfig::default().without_cancellation_check();
        assert!(!config.check_cancellation);
    }

    #[test]
    fn test_progress_percent_empty() {
        let progress = CompactionProgress {
            total_edges: 0,
            edges_processed: 0,
            current_chunk: 1,
            total_chunks: 0,
        };
        assert_eq!(progress.percent_complete(), 100);
        assert!(progress.is_complete());
    }

    #[test]
    fn test_progress_percent_partial() {
        let progress = CompactionProgress {
            total_edges: 100,
            edges_processed: 50,
            current_chunk: 1,
            total_chunks: 2,
        };
        assert_eq!(progress.percent_complete(), 50);
        assert!(!progress.is_complete());
    }

    #[test]
    fn test_progress_display() {
        let progress = CompactionProgress {
            total_edges: 100,
            edges_processed: 50,
            current_chunk: 1,
            total_chunks: 2,
        };
        let display = format!("{progress}");
        assert!(display.contains("1/2"));
        assert!(display.contains("50/100"));
        assert!(display.contains("50%"));
    }

    #[test]
    fn test_compact_empty_snapshot() {
        let snapshot = make_snapshot(vec![]);
        let token = CancellationToken::new();
        let config = InterruptibleConfig::default();
        let mut yields = 0;

        let result = compact_interruptible(&snapshot, &token, &config, |_| {
            yields += 1;
        })
        .unwrap();

        assert!(result.is_complete());
        assert!(result.merged_edges.is_empty());
        assert_eq!(yields, 1); // One yield for empty
    }

    #[test]
    fn test_compact_small_snapshot() {
        let snapshot = make_snapshot(vec![
            make_delta_edge(0, 1, 1, false),
            make_delta_edge(1, 2, 2, false),
        ]);
        let token = CancellationToken::new();
        let config = InterruptibleConfig::default();
        let mut yields = 0;

        let result = compact_interruptible(&snapshot, &token, &config, |_| {
            yields += 1;
        })
        .unwrap();

        assert!(result.is_complete());
        assert_eq!(result.merged_edges.len(), 2);
        assert_eq!(yields, 1);
    }

    #[test]
    fn test_compact_with_chunks() {
        // Create more edges than default chunk size
        let mut edges = Vec::new();
        for i in 0..100 {
            edges.push(make_delta_edge(i, i + 1, u64::from(i), false));
        }

        let snapshot = make_snapshot(edges);
        let token = CancellationToken::new();
        let config = InterruptibleConfig::with_chunk_size(30); // 4 chunks of ~30
        let mut yield_count = 0;
        let mut last_progress = None;

        let result = compact_interruptible(&snapshot, &token, &config, |progress| {
            yield_count += 1;
            last_progress = Some(*progress);
        })
        .unwrap();

        assert!(result.is_complete());
        assert_eq!(result.merged_edges.len(), 100);
        assert!(yield_count >= 3); // At least 3 yields for 4 chunks
        assert!(last_progress.unwrap().is_complete());
    }

    #[test]
    fn test_compact_early_cancellation() {
        let snapshot = make_snapshot(vec![make_delta_edge(0, 1, 1, false)]);
        let token = CancellationToken::new();
        token.cancel(); // Cancel before start
        let config = InterruptibleConfig::default();

        let result = compact_interruptible(&snapshot, &token, &config, |_| {});

        match result {
            Err(CompactionError::Interrupted {
                reason: InterruptReason::CancellationRequested,
                edges_processed: 0,
                ..
            }) => {}
            _ => panic!("expected Interrupted error"),
        }
    }

    #[test]
    fn test_compact_mid_cancellation() {
        let mut edges = Vec::new();
        for i in 0..100 {
            edges.push(make_delta_edge(i, i + 1, u64::from(i), false));
        }

        let snapshot = make_snapshot(edges);
        let token = CancellationToken::new();
        let config = InterruptibleConfig::with_chunk_size(10);

        let result = compact_interruptible(&snapshot, &token, &config, |progress| {
            if progress.current_chunk == 2 {
                token.cancel();
            }
        });

        match result {
            Err(CompactionError::Interrupted {
                reason: InterruptReason::CancellationRequested,
                edges_processed,
                ..
            }) => {
                assert!(edges_processed > 0); // Some processed
                assert!(edges_processed < 100); // Not all
            }
            _ => panic!("expected Interrupted error"),
        }
    }

    #[test]
    fn test_compact_with_removes() {
        let snapshot = make_snapshot(vec![
            make_delta_edge(0, 1, 1, false), // Add
            make_delta_edge(0, 1, 2, true),  // Remove (wins)
            make_delta_edge(1, 2, 3, false), // Add
        ]);
        let token = CancellationToken::new();
        let config = InterruptibleConfig::default();

        let result = compact_interruptible(&snapshot, &token, &config, |_| {}).unwrap();

        // Only edge 1->2 should survive
        assert_eq!(result.merged_edges.len(), 1);
        assert_eq!(result.merged_edges[0].source.index(), 1);
        assert_eq!(result.merged_edges[0].target.index(), 2);
    }

    #[test]
    fn test_stats_tracking() {
        let stats = InterruptibleStats::new();
        assert_eq!(stats.snapshot().started, 0);

        stats.record_start();
        stats.record_start();
        assert_eq!(stats.snapshot().started, 2);

        stats.record_complete(5);
        assert_eq!(stats.snapshot().completed, 1);
        assert_eq!(stats.snapshot().total_chunks, 5);

        stats.record_cancel(3);
        assert_eq!(stats.snapshot().cancelled, 1);
        assert_eq!(stats.snapshot().total_chunks, 8);
    }

    #[test]
    fn test_stats_rates() {
        let mut snapshot = InterruptibleStatsSnapshot::default();
        assert_eq!(snapshot.completion_rate(), 100);
        assert_eq!(snapshot.cancellation_rate(), 0);

        snapshot.started = 10;
        snapshot.completed = 7;
        snapshot.cancelled = 3;

        assert_eq!(snapshot.completion_rate(), 70);
        assert_eq!(snapshot.cancellation_rate(), 30);
    }

    #[test]
    fn test_interruptible_result_is_complete() {
        let result = InterruptibleResult {
            merged_edges: vec![],
            merge_stats: MergeStats::default(),
            chunks_processed: 1,
            was_cancelled: false,
        };
        assert!(result.is_complete());

        let result = InterruptibleResult {
            merged_edges: vec![],
            merge_stats: MergeStats::default(),
            chunks_processed: 1,
            was_cancelled: true,
        };
        assert!(!result.is_complete());
    }

    #[test]
    fn test_no_cancellation_check() {
        let snapshot = make_snapshot(vec![make_delta_edge(0, 1, 1, false)]);
        let token = CancellationToken::new();
        token.cancel(); // Cancel, but won't be checked
        let config = InterruptibleConfig::default().without_cancellation_check();

        // Should complete despite cancellation
        let result = compact_interruptible(&snapshot, &token, &config, |_| {}).unwrap();
        assert!(result.is_complete());
    }

    /// Test that Remove operations in later chunks correctly cancel
    /// Add operations from earlier chunks (cross-chunk LWW semantics).
    ///
    /// This is a regression test for the critical bug where cross-chunk
    /// removes were dropped, causing deleted edges to be resurrected.
    #[test]
    fn test_cross_chunk_remove_cancels_add() {
        // Create edges that will be split across chunks:
        // - First 5 edges are regular adds (will go in chunk 1)
        // - Edge 0->1 appears again as Remove with higher seq (will go in chunk 2)
        let mut edges = Vec::new();

        // Chunk 1: Add edges 0->1, 1->2, 2->3, 3->4, 4->5
        for i in 0..5 {
            edges.push(make_delta_edge(i, i + 1, u64::from(i + 1), false));
        }

        // Chunk 2: Remove edge 0->1 with higher seq (should cancel the add)
        // Plus some more adds to fill the chunk
        edges.push(make_delta_edge(0, 1, 100, true)); // Remove with seq=100, higher than add seq=1
        for i in 10..14 {
            edges.push(make_delta_edge(i, i + 1, u64::from(i + 1), false));
        }

        let snapshot = make_snapshot(edges);
        let token = CancellationToken::new();
        // Use chunk size of 5 to ensure edges are split across chunks
        let config = InterruptibleConfig::with_chunk_size(5);

        let result = compact_interruptible(&snapshot, &token, &config, |_| {}).unwrap();

        assert!(result.is_complete());

        // Edge 0->1 should NOT be in the result (Remove won)
        let has_edge_0_1 = result
            .merged_edges
            .iter()
            .any(|e| e.source.index() == 0 && e.target.index() == 1);
        assert!(
            !has_edge_0_1,
            "Edge 0->1 should be removed by cross-chunk Remove operation"
        );

        // Other edges should survive
        // Edges 1->2, 2->3, 3->4, 4->5 from chunk 1
        // Edges 10->11, 11->12, 12->13, 13->14 from chunk 2
        assert_eq!(result.merged_edges.len(), 8);

        // Verify the removed_count reflects the Remove that won
        assert_eq!(result.merge_stats.removed_count, 1);
    }

    /// Test that Add operations in later chunks correctly win over
    /// Remove operations from earlier chunks (cross-chunk LWW semantics).
    #[test]
    fn test_cross_chunk_add_wins_over_remove() {
        let mut edges = Vec::new();

        // Chunk 1: Remove edge 0->1 with seq=1
        edges.push(make_delta_edge(0, 1, 1, true)); // Remove
        for i in 1..5 {
            edges.push(make_delta_edge(i, i + 1, u64::from(i + 1), false));
        }

        // Chunk 2: Add edge 0->1 with higher seq (should resurrect the edge)
        edges.push(make_delta_edge(0, 1, 100, false)); // Add with seq=100
        for i in 10..14 {
            edges.push(make_delta_edge(i, i + 1, u64::from(i + 1), false));
        }

        let snapshot = make_snapshot(edges);
        let token = CancellationToken::new();
        let config = InterruptibleConfig::with_chunk_size(5);

        let result = compact_interruptible(&snapshot, &token, &config, |_| {}).unwrap();

        // Edge 0->1 SHOULD be in the result (Add with higher seq won)
        let edge_0_1 = result
            .merged_edges
            .iter()
            .find(|e| e.source.index() == 0 && e.target.index() == 1);
        assert!(
            edge_0_1.is_some(),
            "Edge 0->1 should be present (Add with higher seq won)"
        );
        assert_eq!(edge_0_1.unwrap().seq, 100);
    }
}
