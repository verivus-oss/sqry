//! `DeltaBuffer`: Mutable edge storage with sequence numbers.
//!
//! This module implements the mutable tier of the two-tier edge storage system.
//! New edges are written to the delta buffer before being compacted into CSR.
//!
//! # Design
//!
//! - **Sequence-numbered operations**: Each operation gets a monotonic sequence
//!   number for deterministic merge ordering during compaction
//! - **File-partitioned storage**: Edges are partitioned by source file for
//!   efficient incremental updates
//! - **Byte tracking**: Accurate byte accounting for back-pressure admission
//!
//! # Thread Safety
//!
//! The `DeltaBuffer` requires external synchronization (e.g., via `RwLock`) for
//! concurrent access. The sequence counter is atomic for safe sequence allocation
//! from multiple threads.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::super::file::FileId;
use super::super::node::NodeId;
use super::kind::EdgeKind;
use crate::graph::node::Span;

/// Delta operation type.
///
/// Indicates whether the delta entry is an addition or removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum DeltaOp {
    /// Add a new edge
    #[default]
    Add,
    /// Remove an existing edge
    Remove,
}

impl fmt::Display for DeltaOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Add => write!(f, "add"),
            Self::Remove => write!(f, "remove"),
        }
    }
}

/// A delta edge entry with sequence number for merge ordering.
///
/// `DeltaEdge` represents a single edge mutation in the delta buffer.
/// The sequence number enables deterministic last-writer-wins merging
/// during compaction.
///
/// # Memory Layout
///
/// Fixed overhead per edge: ~34 bytes (plus spans Vec)
/// - source: `NodeId` (8 bytes: u32 index + u64 generation, but packed)
/// - target: `NodeId` (8 bytes)
/// - kind: `EdgeKind` (variable, typically 1-16 bytes)
/// - seq: u64 (8 bytes)
/// - op: `DeltaOp` (1 byte)
/// - file: `FileId` (4 bytes)
/// - spans: `Vec<Span>` (24 bytes overhead + span data)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeltaEdge {
    /// Source node of the edge
    pub source: NodeId,
    /// Target node of the edge
    pub target: NodeId,
    /// Edge relationship type
    pub kind: EdgeKind,
    /// Monotonic sequence number for merge ordering
    pub seq: u64,
    /// Operation type (add or remove)
    pub op: DeltaOp,
    /// Source file for this edge
    pub file: FileId,
    /// Source spans of the edge (e.g., call-site locations for LSP call hierarchy).
    /// Multiple spans when the same edge has multiple call sites.
    pub spans: Vec<Span>,
}

impl DeltaEdge {
    /// Creates a new delta edge without span data.
    #[must_use]
    pub fn new(
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        seq: u64,
        op: DeltaOp,
        file: FileId,
    ) -> Self {
        Self {
            source,
            target,
            kind,
            seq,
            op,
            file,
            spans: Vec::new(),
        }
    }

    /// Creates a new delta edge with span data.
    #[must_use]
    pub fn with_spans(
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        seq: u64,
        op: DeltaOp,
        file: FileId,
        spans: Vec<Span>,
    ) -> Self {
        Self {
            source,
            target,
            kind,
            seq,
            op,
            file,
            spans,
        }
    }

    /// Returns the edge key for deduplication.
    ///
    /// The edge key consists of (source, target, kind), excluding the
    /// sequence number and operation type. Used for last-writer-wins
    /// merge during compaction.
    #[must_use]
    pub fn edge_key(&self) -> EdgeKey {
        EdgeKey {
            source: self.source,
            target: self.target,
            kind: self.kind.clone(),
        }
    }

    /// Returns true if this is an add operation.
    #[must_use]
    #[inline]
    pub fn is_add(&self) -> bool {
        self.op == DeltaOp::Add
    }

    /// Returns true if this is a remove operation.
    #[must_use]
    #[inline]
    pub fn is_remove(&self) -> bool {
        self.op == DeltaOp::Remove
    }

    /// Calculates the byte size of this delta edge.
    ///
    /// Used for byte-level admission control.
    #[must_use]
    pub fn byte_size(&self) -> usize {
        // Fixed overhead: source(12) + target(12) + seq(8) + op(1) + file(4) = 37
        // Plus Vec<Span> overhead: ~24 bytes (ptr + len + cap)
        // Plus span data: N * size_of::<Span>()
        // Plus EdgeKind size (estimated, varies by variant)
        const FIXED_OVERHEAD: usize = 37;
        const VEC_OVERHEAD: usize = std::mem::size_of::<Vec<crate::graph::node::Span>>();
        let span_data_size = self.spans.len() * std::mem::size_of::<crate::graph::node::Span>();
        FIXED_OVERHEAD + VEC_OVERHEAD + span_data_size + self.kind.estimated_size()
    }
}

/// Edge key for deduplication during merge.
///
/// Two edges with the same key are considered the same edge, and the
/// one with the higher sequence number wins during merge.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EdgeKey {
    /// Source node
    pub source: NodeId,
    /// Target node
    pub target: NodeId,
    /// Edge kind
    pub kind: EdgeKind,
}

/// File-partitioned mutable edge storage with sequence numbering.
///
/// `DeltaBuffer` is the write-optimized tier of the two-tier edge storage.
/// Edges are partitioned by source file for efficient incremental updates.
///
/// # Thread Safety
///
/// The buffer requires external synchronization for most operations, but
/// the sequence counter is atomic for safe allocation across threads.
///
/// # Example
///
/// ```rust,ignore
/// use sqry_core::graph::unified::edge::delta::{DeltaBuffer, DeltaOp};
///
/// let mut buffer = DeltaBuffer::new();
///
/// // Push an edge
/// let seq = buffer.next_seq();
/// buffer.push(DeltaEdge::new(source, target, EdgeKind::Calls { argument_count: 0, is_async: false }, seq, DeltaOp::Add, file));
///
/// // Query stats
/// assert_eq!(buffer.len(), 1);
/// ```
#[derive(Debug, Serialize, Deserialize)]
pub struct DeltaBuffer {
    /// File-partitioned edge storage
    edges: HashMap<FileId, Vec<DeltaEdge>>,
    /// Total number of edges across all files
    edge_count: usize,
    /// Total byte size of all edges
    byte_size: usize,
    /// Monotonic sequence counter for deterministic merge
    #[serde(with = "atomic_u64_serde")]
    seq_counter: AtomicU64,
}

impl DeltaBuffer {
    /// Creates a new empty delta buffer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            edges: HashMap::default(),
            edge_count: 0,
            byte_size: 0,
            seq_counter: AtomicU64::new(0),
        }
    }

    /// Creates a new delta buffer with the specified capacity.
    #[must_use]
    pub fn with_capacity(file_count: usize) -> Self {
        Self {
            edges: HashMap::with_capacity(file_count),
            edge_count: 0,
            byte_size: 0,
            seq_counter: AtomicU64::new(0),
        }
    }

    /// Returns the total number of delta edges.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.edge_count
    }

    /// Returns true if the buffer is empty.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.edge_count == 0
    }

    /// Returns the total byte size of all edges.
    #[must_use]
    #[inline]
    pub fn byte_size(&self) -> usize {
        self.byte_size
    }

    /// Returns the number of files with delta edges.
    #[must_use]
    #[inline]
    pub fn file_count(&self) -> usize {
        self.edges.len()
    }

    /// Returns the current sequence counter value.
    #[must_use]
    pub fn current_seq(&self) -> u64 {
        self.seq_counter.load(Ordering::Acquire)
    }

    /// Allocates and returns the next sequence number.
    ///
    /// Thread-safe: uses atomic `fetch_add`.
    pub fn next_seq(&self) -> u64 {
        self.seq_counter.fetch_add(1, Ordering::AcqRel)
    }

    /// Pushes a delta edge to the buffer.
    ///
    /// The edge is added to the partition for its source file.
    pub fn push(&mut self, edge: DeltaEdge) {
        let size = edge.byte_size();
        let file = edge.file;

        self.edges.entry(file).or_default().push(edge);
        self.edge_count += 1;
        self.byte_size += size;
    }

    /// Pushes multiple delta edges to the buffer.
    pub fn push_many(&mut self, edges: impl IntoIterator<Item = DeltaEdge>) {
        for edge in edges {
            self.push(edge);
        }
    }

    /// Returns an iterator over all delta edges.
    pub fn iter(&self) -> impl Iterator<Item = &DeltaEdge> {
        self.edges.values().flatten()
    }

    /// Returns a mutable iterator over all delta edges.
    ///
    /// Used by [`RebuildGraph::finalize`] step 1 to rewrite `StringId`
    /// payloads inside each committed delta `EdgeKind` through the
    /// canonical-StringId remap produced by
    /// `StringInterner::build_dedup_table`. Intentionally does not allow
    /// the caller to mutate `source`, `target`, `seq`, `op`, or `file` —
    /// only the `kind` field requires rewriting. The mutable-reference
    /// borrow is scoped to the iterator's lifetime and does not bypass
    /// `DeltaBuffer`'s invariants: `edge_count`, `byte_size`, and
    /// `seq_counter` remain untouched because `EdgeKind`'s in-place
    /// remap preserves every edge's byte size.
    ///
    /// `pub(crate)` because the only legitimate caller is
    /// [`BidirectionalEdgeStore::rewrite_edge_kind_string_ids_through_remap`]
    /// inside `sqry-core`. External crates must not mutate the delta
    /// buffer's edge payloads — they go through `RebuildGraph::finalize()`
    /// which invokes the remap helper internally. See Gate 0c plan §H
    /// and iter-4 blocker.
    #[allow(dead_code)] // Only reachable through the rebuild-internals-gated path.
    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut DeltaEdge> {
        self.edges.values_mut().flatten()
    }

    /// Returns an iterator over delta edges for a specific file.
    pub fn iter_file(&self, file: FileId) -> impl Iterator<Item = &DeltaEdge> {
        self.edges.get(&file).into_iter().flat_map(|v| v.iter())
    }

    /// Returns delta edges for a specific file as a slice.
    #[must_use]
    pub fn edges_for_file(&self, file: FileId) -> &[DeltaEdge] {
        self.edges.get(&file).map_or(&[], |v| v.as_slice())
    }

    /// Returns all file IDs that have delta edges.
    pub fn files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.edges.keys().copied()
    }

    /// Clears all delta edges for a specific file.
    ///
    /// Returns the number of edges removed.
    pub fn clear_file(&mut self, file: FileId) -> usize {
        if let Some(edges) = self.edges.remove(&file) {
            let count = edges.len();
            let size: usize = edges.iter().map(DeltaEdge::byte_size).sum();
            self.edge_count -= count;
            self.byte_size -= size;
            count
        } else {
            0
        }
    }

    /// Clears all delta edges.
    pub fn clear(&mut self) {
        self.edges.clear();
        self.edge_count = 0;
        self.byte_size = 0;
        // Note: sequence counter is NOT reset on clear to maintain monotonicity
    }

    /// Resets the sequence counter to a specific value.
    ///
    /// # Safety Considerations
    ///
    /// This should only be called when the buffer is empty or during
    /// initialization. Resetting while edges exist can break merge ordering.
    pub fn reset_seq(&mut self, value: u64) {
        self.seq_counter.store(value, Ordering::Release);
    }

    /// Advances the sequence counter to at least the given value.
    ///
    /// If the current counter is already >= value, this is a no-op.
    /// Returns the new counter value.
    ///
    /// This is used by `push_committed` to ensure the counter stays ahead
    /// of externally-sequenced edges ().
    pub fn advance_seq_to(&self, min_value: u64) -> u64 {
        loop {
            let current = self.seq_counter.load(Ordering::Acquire);
            if current >= min_value {
                return current;
            }
            // Try to advance to min_value
            if self
                .seq_counter
                .compare_exchange_weak(current, min_value, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return min_value;
            }
        }
    }

    /// Retain only delta edges for which `keep` returns `true`, updating
    /// `edge_count` and `byte_size` accordingly. Files whose bucket
    /// becomes empty are garbage-collected from the inner map so the
    /// `files()` iterator only surfaces populated files.
    ///
    /// Used by the Gate 0b [`NodeIdBearing`] impl on
    /// [`super::BidirectionalEdgeStore`] to drop delta edges whose
    /// source or target has been tombstoned
    /// (`sqry-core/src/graph/unified/rebuild/coverage.rs`). Exposed at
    /// `pub(crate)` scope because only the rebuild pipeline needs
    /// predicate-based filtering; external callers use the targeted
    /// [`EdgeStore::remove_edge`](super::store::EdgeStore::remove_edge)
    /// entry point.
    ///
    /// The sequence counter is preserved — surviving edges keep their
    /// original seq values so merge ordering stays correct after
    /// tombstone compaction.
    ///
    /// `#[allow(dead_code)]` is present because Gate 0b delivers only
    /// scaffolding; the call site in the Gate 0c `RebuildGraph::finalize()`
    /// step 3 lands in a follow-up commit. Unit coverage in
    /// `sqry-core/src/graph/unified/rebuild/coverage.rs::tests` already
    /// exercises this helper through the [`NodeIdBearing::retain_nodes`]
    /// impl on `BidirectionalEdgeStore`.
    ///
    /// [`NodeIdBearing`]: crate::graph::unified::rebuild::coverage::NodeIdBearing
    /// [`NodeIdBearing::retain_nodes`]: crate::graph::unified::rebuild::coverage::NodeIdBearing::retain_nodes
    #[allow(dead_code)]
    pub(crate) fn retain_if<F>(&mut self, mut keep: F)
    where
        F: FnMut(&DeltaEdge) -> bool,
    {
        let mut dropped_count: usize = 0;
        let mut dropped_bytes: usize = 0;
        self.edges.retain(|_file, bucket| {
            bucket.retain(|edge| {
                let retain = keep(edge);
                if !retain {
                    dropped_count += 1;
                    dropped_bytes += edge.byte_size();
                }
                retain
            });
            !bucket.is_empty()
        });
        self.edge_count -= dropped_count;
        self.byte_size -= dropped_bytes;
    }

    /// Takes all edges from the buffer, leaving it empty.
    ///
    /// Returns the edges grouped by file. The sequence counter is preserved.
    pub fn take_all(&mut self) -> HashMap<FileId, Vec<DeltaEdge>> {
        self.edge_count = 0;
        self.byte_size = 0;
        std::mem::take(&mut self.edges)
    }

    /// Returns statistics about the buffer.
    #[must_use]
    pub fn stats(&self) -> DeltaBufferStats {
        DeltaBufferStats {
            edge_count: self.edge_count,
            byte_size: self.byte_size,
            file_count: self.edges.len(),
            current_seq: self.current_seq(),
        }
    }
}

impl Default for DeltaBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DeltaBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DeltaBuffer(edges={}, bytes={}, files={})",
            self.edge_count,
            self.byte_size,
            self.edges.len()
        )
    }
}

impl Clone for DeltaBuffer {
    fn clone(&self) -> Self {
        Self {
            edges: self.edges.clone(),
            edge_count: self.edge_count,
            byte_size: self.byte_size,
            // Clone the current value of the atomic counter
            seq_counter: AtomicU64::new(self.seq_counter.load(Ordering::SeqCst)),
        }
    }
}

/// Statistics about a `DeltaBuffer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaBufferStats {
    /// Total number of delta edges
    pub edge_count: usize,
    /// Total byte size of all edges
    pub byte_size: usize,
    /// Number of files with delta edges
    pub file_count: usize,
    /// Current sequence counter value
    pub current_seq: u64,
}

impl crate::graph::unified::memory::GraphMemorySize for DeltaBuffer {
    fn heap_bytes(&self) -> usize {
        use crate::graph::unified::memory::HASHMAP_ENTRY_OVERHEAD;

        let map_overhead = self.edges.capacity()
            * (std::mem::size_of::<crate::graph::unified::file::FileId>()
                + std::mem::size_of::<Vec<DeltaEdge>>()
                + HASHMAP_ENTRY_OVERHEAD);
        let vec_payloads: usize = self
            .edges
            .values()
            .map(|v| {
                let vec_cap = v.capacity() * std::mem::size_of::<DeltaEdge>();
                // Each DeltaEdge owns a Vec<Span>.
                let span_payloads: usize = v
                    .iter()
                    .map(|e| e.spans.capacity() * std::mem::size_of::<crate::graph::node::Span>())
                    .sum();
                vec_cap + span_payloads
            })
            .sum();
        map_overhead + vec_payloads
    }
}

#[cfg(test)]
mod tests {
    use super::super::kind::EdgeKind;
    use super::*;

    fn make_edge(source: u32, target: u32, seq: u64, op: DeltaOp, file: u32) -> DeltaEdge {
        DeltaEdge::new(
            NodeId::new(source, 0),
            NodeId::new(target, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            seq,
            op,
            FileId::new(file),
        )
    }

    #[test]
    fn test_delta_op_default() {
        assert_eq!(DeltaOp::default(), DeltaOp::Add);
    }

    #[test]
    fn test_delta_op_display() {
        assert_eq!(format!("{}", DeltaOp::Add), "add");
        assert_eq!(format!("{}", DeltaOp::Remove), "remove");
    }

    #[test]
    fn test_delta_edge_new() {
        let edge = make_edge(1, 2, 42, DeltaOp::Add, 10);
        assert_eq!(edge.source.index(), 1);
        assert_eq!(edge.target.index(), 2);
        assert_eq!(edge.seq, 42);
        assert_eq!(edge.op, DeltaOp::Add);
        assert_eq!(edge.file.index(), 10);
    }

    #[test]
    fn test_delta_edge_is_add_remove() {
        let add_edge = make_edge(1, 2, 0, DeltaOp::Add, 1);
        let remove_edge = make_edge(1, 2, 0, DeltaOp::Remove, 1);

        assert!(add_edge.is_add());
        assert!(!add_edge.is_remove());
        assert!(!remove_edge.is_add());
        assert!(remove_edge.is_remove());
    }

    #[test]
    fn test_delta_edge_edge_key() {
        let edge1 = make_edge(1, 2, 100, DeltaOp::Add, 1);
        let edge2 = make_edge(1, 2, 200, DeltaOp::Remove, 1);
        let edge3 = make_edge(1, 3, 100, DeltaOp::Add, 1);

        // Same source, target, kind -> same key
        assert_eq!(edge1.edge_key(), edge2.edge_key());
        // Different target -> different key
        assert_ne!(edge1.edge_key(), edge3.edge_key());
    }

    #[test]
    fn test_delta_edge_byte_size() {
        let edge = make_edge(1, 2, 0, DeltaOp::Add, 1);
        // Should have reasonable size
        assert!(edge.byte_size() > 30);
    }

    #[test]
    fn test_delta_buffer_new() {
        let buffer = DeltaBuffer::new();
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert_eq!(buffer.byte_size(), 0);
        assert_eq!(buffer.file_count(), 0);
    }

    #[test]
    fn test_delta_buffer_with_capacity() {
        let buffer = DeltaBuffer::with_capacity(100);
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn test_delta_buffer_push() {
        let mut buffer = DeltaBuffer::new();

        let seq = buffer.next_seq();
        let edge = make_edge(1, 2, seq, DeltaOp::Add, 10);
        let size = edge.byte_size();

        buffer.push(edge);

        assert_eq!(buffer.len(), 1);
        assert!(!buffer.is_empty());
        assert_eq!(buffer.byte_size(), size);
        assert_eq!(buffer.file_count(), 1);
    }

    #[test]
    fn test_delta_buffer_push_multiple_files() {
        let mut buffer = DeltaBuffer::new();

        buffer.push(make_edge(1, 2, buffer.next_seq(), DeltaOp::Add, 10));
        buffer.push(make_edge(3, 4, buffer.next_seq(), DeltaOp::Add, 20));
        buffer.push(make_edge(5, 6, buffer.next_seq(), DeltaOp::Add, 10));

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.file_count(), 2);
    }

    #[test]
    fn test_delta_buffer_sequence_monotonic() {
        let buffer = DeltaBuffer::new();

        let seq1 = buffer.next_seq();
        let seq2 = buffer.next_seq();
        let seq3 = buffer.next_seq();

        assert_eq!(seq1, 0);
        assert_eq!(seq2, 1);
        assert_eq!(seq3, 2);
        assert_eq!(buffer.current_seq(), 3);
    }

    #[test]
    fn test_delta_buffer_iter() {
        let mut buffer = DeltaBuffer::new();
        buffer.push(make_edge(1, 2, 0, DeltaOp::Add, 10));
        buffer.push(make_edge(3, 4, 1, DeltaOp::Add, 10));

        let edges: Vec<_> = buffer.iter().collect();
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn test_delta_buffer_iter_file() {
        let mut buffer = DeltaBuffer::new();
        buffer.push(make_edge(1, 2, 0, DeltaOp::Add, 10));
        buffer.push(make_edge(3, 4, 1, DeltaOp::Add, 20));
        buffer.push(make_edge(5, 6, 2, DeltaOp::Add, 10));

        let file10_edges: Vec<_> = buffer.iter_file(FileId::new(10)).collect();
        assert_eq!(file10_edges.len(), 2);

        let file20_edges: Vec<_> = buffer.iter_file(FileId::new(20)).collect();
        assert_eq!(file20_edges.len(), 1);

        let file30_edges: Vec<_> = buffer.iter_file(FileId::new(30)).collect();
        assert_eq!(file30_edges.len(), 0);
    }

    #[test]
    fn test_delta_buffer_edges_for_file() {
        let mut buffer = DeltaBuffer::new();
        buffer.push(make_edge(1, 2, 0, DeltaOp::Add, 10));
        buffer.push(make_edge(3, 4, 1, DeltaOp::Add, 10));

        let edges = buffer.edges_for_file(FileId::new(10));
        assert_eq!(edges.len(), 2);

        let empty = buffer.edges_for_file(FileId::new(99));
        assert!(empty.is_empty());
    }

    #[test]
    fn test_delta_buffer_files() {
        let mut buffer = DeltaBuffer::new();
        buffer.push(make_edge(1, 2, 0, DeltaOp::Add, 10));
        buffer.push(make_edge(3, 4, 1, DeltaOp::Add, 20));

        let files: Vec<_> = buffer.files().collect();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&FileId::new(10)));
        assert!(files.contains(&FileId::new(20)));
    }

    #[test]
    fn test_delta_buffer_clear_file() {
        let mut buffer = DeltaBuffer::new();
        buffer.push(make_edge(1, 2, 0, DeltaOp::Add, 10));
        buffer.push(make_edge(3, 4, 1, DeltaOp::Add, 20));
        buffer.push(make_edge(5, 6, 2, DeltaOp::Add, 10));

        let initial_size = buffer.byte_size();

        let removed = buffer.clear_file(FileId::new(10));
        assert_eq!(removed, 2);
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.file_count(), 1);
        assert!(buffer.byte_size() < initial_size);
    }

    #[test]
    fn test_delta_buffer_clear() {
        let mut buffer = DeltaBuffer::new();
        buffer.push(make_edge(1, 2, buffer.next_seq(), DeltaOp::Add, 10));
        buffer.push(make_edge(3, 4, buffer.next_seq(), DeltaOp::Add, 20));

        let seq_before = buffer.current_seq();

        buffer.clear();

        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert_eq!(buffer.byte_size(), 0);
        assert_eq!(buffer.file_count(), 0);
        // Sequence counter should NOT be reset
        assert_eq!(buffer.current_seq(), seq_before);
    }

    #[test]
    fn test_delta_buffer_reset_seq() {
        let mut buffer = DeltaBuffer::new();
        buffer.next_seq();
        buffer.next_seq();
        assert_eq!(buffer.current_seq(), 2);

        buffer.reset_seq(100);
        assert_eq!(buffer.current_seq(), 100);
        assert_eq!(buffer.next_seq(), 100);
        assert_eq!(buffer.current_seq(), 101);
    }

    #[test]
    fn test_delta_buffer_take_all() {
        let mut buffer = DeltaBuffer::new();
        buffer.push(make_edge(1, 2, buffer.next_seq(), DeltaOp::Add, 10));
        buffer.push(make_edge(3, 4, buffer.next_seq(), DeltaOp::Add, 20));

        let seq_before = buffer.current_seq();
        let taken = buffer.take_all();

        assert_eq!(taken.len(), 2);
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        // Sequence counter preserved
        assert_eq!(buffer.current_seq(), seq_before);
    }

    #[test]
    fn test_delta_buffer_push_many() {
        let mut buffer = DeltaBuffer::new();

        let edges = vec![
            make_edge(1, 2, 0, DeltaOp::Add, 10),
            make_edge(3, 4, 1, DeltaOp::Add, 10),
            make_edge(5, 6, 2, DeltaOp::Add, 20),
        ];

        buffer.push_many(edges);

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.file_count(), 2);
    }

    #[test]
    fn test_delta_buffer_stats() {
        let mut buffer = DeltaBuffer::new();
        buffer.push(make_edge(1, 2, buffer.next_seq(), DeltaOp::Add, 10));
        buffer.push(make_edge(3, 4, buffer.next_seq(), DeltaOp::Add, 20));

        let stats = buffer.stats();
        assert_eq!(stats.edge_count, 2);
        assert!(stats.byte_size > 0);
        assert_eq!(stats.file_count, 2);
        assert_eq!(stats.current_seq, 2);
    }

    #[test]
    fn test_delta_buffer_display() {
        let mut buffer = DeltaBuffer::new();
        buffer.push(make_edge(1, 2, 0, DeltaOp::Add, 10));

        let display = format!("{buffer}");
        assert!(display.contains("DeltaBuffer"));
        assert!(display.contains("edges=1"));
    }

    #[test]
    fn test_delta_buffer_default() {
        let buffer: DeltaBuffer = DeltaBuffer::default();
        assert_eq!(buffer.len(), 0);
    }

    #[test]
    fn test_delta_buffer_advance_seq_to() {
        let buffer = DeltaBuffer::new();
        assert_eq!(buffer.current_seq(), 0);

        // Advance to 10
        let result = buffer.advance_seq_to(10);
        assert_eq!(result, 10);
        assert_eq!(buffer.current_seq(), 10);

        // Advance to 5 (should be no-op since already at 10)
        let result = buffer.advance_seq_to(5);
        assert_eq!(result, 10);
        assert_eq!(buffer.current_seq(), 10);

        // Advance to 15
        let result = buffer.advance_seq_to(15);
        assert_eq!(result, 15);
        assert_eq!(buffer.current_seq(), 15);

        // next_seq should return 15 and increment
        assert_eq!(buffer.next_seq(), 15);
        assert_eq!(buffer.current_seq(), 16);
    }
}

/// Custom serialization for `AtomicU64`.
mod atomic_u64_serde {
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(value: &AtomicU64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.load(Ordering::SeqCst).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<AtomicU64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Ok(AtomicU64::new(value))
    }
}
