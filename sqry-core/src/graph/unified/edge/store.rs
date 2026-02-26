//! `EdgeStore`: Two-tier edge storage combining CSR and `DeltaBuffer`.
//!
//! This module implements the two-tier edge storage system for the unified graph:
//! - **Tier 1 (CSR)**: Stable, read-optimized compressed sparse row format
//! - **Tier 2 (`DeltaBuffer`)**: Mutable, write-optimized storage with sequence numbers
//!
//! # Design (FR-30, FR-31)
//!
//! Queries merge both tiers:
//! - CSR edges filtered by tombstone bitmap
//! - Delta edges filtered by `op != Remove`
//! - Union of both sets
//!
//! Writes go to the delta buffer. Periodically, compaction merges deltas
//! into a new CSR and clears the buffer.
//!
//! # Tombstone Management (CP-9, CP-10)
//!
//! When removing an edge that exists in CSR:
//! - A tombstone is set in the bitmap
//! - A Remove delta is also pushed (for cross-replica consistency)
//!
//! When removing an edge that only exists in delta:
//! - A Remove delta is pushed (shadows the Add)

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::super::file::FileId;
use super::super::node::NodeId;
use super::super::storage::CsrGraph;
use super::delta::{DeltaBuffer, DeltaEdge, DeltaOp, EdgeKey};
use super::kind::EdgeKind;

/// Error returned when edge operations fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EdgeStoreError {
    /// Attempted to access an invalid node.
    InvalidNode(NodeId),
    /// CSR graph error.
    CsrError(String),
    /// Delta buffer full.
    DeltaBufferFull {
        /// Current byte usage.
        current_bytes: usize,
        /// Requested bytes.
        requested_bytes: usize,
        /// Maximum allowed bytes.
        limit: usize,
    },
    /// Edge size exceeds reservation (FR-58).
    ///
    /// Returned when `push_committed()` receives edges whose total byte size
    /// exceeds the originally reserved amount.
    EdgeSizeExceeded {
        /// Actual bytes of the edges being pushed.
        edge_bytes: usize,
        /// Reserved bytes from the reservation.
        reservation_bytes: usize,
    },
}

impl std::fmt::Display for EdgeStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidNode(id) => write!(f, "invalid node: {id:?}"),
            Self::CsrError(msg) => write!(f, "CSR error: {msg}"),
            Self::DeltaBufferFull {
                current_bytes,
                requested_bytes,
                limit,
            } => write!(
                f,
                "delta buffer full: {current_bytes} + {requested_bytes} > {limit} bytes"
            ),
            Self::EdgeSizeExceeded {
                edge_bytes,
                reservation_bytes,
            } => write!(
                f,
                "edge size {edge_bytes} exceeds reservation {reservation_bytes} bytes"
            ),
        }
    }
}

impl std::error::Error for EdgeStoreError {}

/// An edge reference returned by `EdgeStore` queries.
///
/// Combines edges from both CSR and delta buffer into a unified view.
#[derive(Debug, Clone, PartialEq)]
pub struct StoreEdgeRef {
    /// Source node (full `NodeId` with generation for edge key matching).
    pub source: NodeId,
    /// Target node.
    pub target: NodeId,
    /// Edge kind.
    pub kind: EdgeKind,
    /// Sequence number (0 for CSR edges without explicit seq).
    pub seq: u64,
    /// File that the edge belongs to (for correct Remove delta partitioning).
    pub file: FileId,
    /// Source spans of the edge (e.g., call-site locations for LSP call hierarchy).
    /// Multiple spans when the same edge has multiple call sites.
    pub spans: Vec<crate::graph::node::Span>,
}

type DeltaFromEntry = (
    u64,
    bool,
    NodeId,
    EdgeKind,
    FileId,
    Vec<crate::graph::node::Span>,
);
type DeltaToEntry = (u64, bool, NodeId, FileId, Vec<crate::graph::node::Span>);

/// Two-tier edge storage combining CSR (stable) and `DeltaBuffer` (mutable).
///
/// `EdgeStore` provides read-write access to edges with efficient queries
/// and incremental updates. The CSR tier is immutable and optimized for
/// range queries. The delta buffer accumulates mutations until compaction.
///
/// # Query Algorithm
///
/// Edges for a node are computed as: `(CSR_edges - tombstones) ∪ (delta_adds)`
///
/// # Example
///
/// ```rust,ignore
/// use sqry_core::graph::unified::edge::store::EdgeStore;
///
/// let mut store = EdgeStore::new();
///
/// // Add an edge (goes to delta buffer)
/// store.add_edge(source, target, EdgeKind::Calls { argument_count: 0, is_async: false }, file)?;
///
/// // Query edges (merges CSR + delta)
/// for edge in store.edges_from(source) {
///     println!("{:?} -> {:?}", edge.source, edge.target);
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeStore {
    /// Tier 1: Stable CSR storage (read-optimized)
    csr: Option<CsrGraph>,

    /// CSR tombstone bitmap - marks deleted edges in CSR by global edge index.
    csr_tombstones: Vec<bool>,

    /// CSR version for MVCC
    csr_version: u64,

    /// Tier 2: Delta buffer (write-optimized)
    delta: DeltaBuffer,
}

impl EdgeStore {
    /// Creates a new empty edge store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            csr: None,
            csr_tombstones: Vec::new(),
            csr_version: 0,
            delta: DeltaBuffer::new(),
        }
    }

    /// Creates an edge store with an initial CSR graph.
    #[must_use]
    pub fn with_csr(csr: CsrGraph) -> Self {
        let edge_count = csr.edge_count();
        Self {
            csr: Some(csr),
            csr_tombstones: vec![false; edge_count],
            csr_version: 1,
            delta: DeltaBuffer::new(),
        }
    }

    /// Returns the CSR graph reference, if any.
    #[must_use]
    pub fn csr(&self) -> Option<&CsrGraph> {
        self.csr.as_ref()
    }

    /// Returns the delta buffer reference.
    #[must_use]
    pub fn delta(&self) -> &DeltaBuffer {
        &self.delta
    }

    /// Returns the mutable delta buffer.
    pub fn delta_mut(&mut self) -> &mut DeltaBuffer {
        &mut self.delta
    }

    /// Returns the current CSR version.
    #[must_use]
    pub fn csr_version(&self) -> u64 {
        self.csr_version
    }

    /// Returns the number of edges in the delta buffer.
    #[must_use]
    pub fn delta_count(&self) -> usize {
        self.delta.len()
    }

    /// Returns the current sequence counter value.
    #[must_use]
    pub fn seq_counter(&self) -> u64 {
        self.delta.current_seq()
    }

    /// Returns the total number of edges (CSR - tombstones + delta adds).
    ///
    /// Note: This is an approximation that may count some edges twice
    /// if they exist in both CSR and delta with conflicting ops.
    #[must_use]
    pub fn edge_count_approx(&self) -> usize {
        let csr_edges = self
            .csr
            .as_ref()
            .map_or(0, super::super::storage::csr::CsrGraph::edge_count);
        let tombstones = self.csr_tombstones.iter().filter(|&&t| t).count();
        let delta_adds = self.delta.iter().filter(|e| e.is_add()).count();

        csr_edges.saturating_sub(tombstones) + delta_adds
    }

    /// Adds an edge to the store.
    ///
    /// The edge is added to the delta buffer with a new sequence number.
    /// For edges with span information, use [`add_edge_with_spans`](Self::add_edge_with_spans).
    pub fn add_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        file: FileId,
    ) -> DeltaEdge {
        self.add_edge_with_spans(source, target, kind, file, Vec::new())
    }

    /// Adds an edge to the store with span information.
    ///
    /// The edge is added to the delta buffer with a new sequence number.
    /// The spans represent source locations of the edge (e.g., call sites for CALLS edges).
    pub fn add_edge_with_spans(
        &mut self,
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        file: FileId,
        spans: Vec<crate::graph::node::Span>,
    ) -> DeltaEdge {
        let seq = self.delta.next_seq();
        let edge = DeltaEdge::with_spans(source, target, kind, seq, DeltaOp::Add, file, spans);
        self.delta.push(edge.clone());
        edge
    }

    /// Removes an edge from the store.
    ///
    /// If the edge exists in CSR, sets the tombstone bit.
    /// Always pushes a Remove delta for consistency.
    pub fn remove_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        file: FileId,
    ) -> DeltaEdge {
        self.tombstone_csr_edge(source, target, &kind);

        // Push remove delta
        let seq = self.delta.next_seq();
        let edge = DeltaEdge::new(source, target, kind, seq, DeltaOp::Remove, file);
        self.delta.push(edge.clone());
        edge
    }

    fn tombstone_csr_edge(&mut self, source: NodeId, target: NodeId, kind: &EdgeKind) {
        let Some(ref csr) = self.csr else {
            return;
        };

        for edge_ref in csr.edges_of(source.index()) {
            if edge_ref.target == target && edge_ref.kind == *kind {
                if edge_ref.index < self.csr_tombstones.len() {
                    self.csr_tombstones[edge_ref.index] = true;
                }
                break;
            }
        }
    }

    /// Pushes committed edges with size validation (FR-58).
    ///
    /// Validates that the total byte size of `edges` does not exceed
    /// `reservation_bytes`. This is used by the admission controller to
    /// ensure that actual edge data stays within the reserved capacity.
    ///
    /// # Errors
    ///
    /// Returns [`EdgeStoreError::EdgeSizeExceeded`] if the total byte size
    /// of the edges exceeds the reservation.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Reservation was made for 1000 bytes
    /// let reservation_bytes = 1000;
    /// let edges = vec![edge1, edge2, edge3];
    ///
    /// // This validates and pushes if within reservation
    /// store.push_committed(edges, reservation_bytes)?;
    /// ```
    pub fn push_committed(
        &mut self,
        edges: Vec<DeltaEdge>,
        reservation_bytes: usize,
    ) -> Result<usize, EdgeStoreError> {
        // Calculate total byte size of edges
        let edge_bytes: usize = edges.iter().map(DeltaEdge::byte_size).sum();

        // FR-58: Validate edge_bytes <= reservation.bytes
        if edge_bytes > reservation_bytes {
            return Err(EdgeStoreError::EdgeSizeExceeded {
                edge_bytes,
                reservation_bytes,
            });
        }

        // FR-43/FR-44: Find max sequence number to advance counter
        let max_seq = edges.iter().map(|e| e.seq).max();

        // Push all edges to delta buffer
        for edge in edges {
            self.delta.push(edge);
        }

        // Advance sequence counter to stay ahead of pushed edges
        if let Some(max) = max_seq {
            self.delta.advance_seq_to(max + 1);
        }

        Ok(edge_bytes)
    }

    fn update_delta_lww_from_edge(
        delta_lww: &mut HashMap<EdgeKey, DeltaFromEntry>,
        edge: &DeltaEdge,
    ) {
        let key = edge.edge_key();
        if delta_lww
            .get(&key)
            .is_some_and(|(existing_seq, _, _, _, _, _)| *existing_seq >= edge.seq)
        {
            return;
        }

        delta_lww.insert(
            key,
            (
                edge.seq,
                edge.is_add(),
                edge.target,
                edge.kind.clone(),
                edge.file,
                edge.spans.clone(),
            ),
        );
    }

    fn update_delta_lww_to_edge(delta_lww: &mut HashMap<EdgeKey, DeltaToEntry>, edge: &DeltaEdge) {
        let key = edge.edge_key();
        if delta_lww
            .get(&key)
            .is_some_and(|(existing_seq, _, _, _, _)| *existing_seq >= edge.seq)
        {
            return;
        }

        delta_lww.insert(
            key,
            (
                edge.seq,
                edge.is_add(),
                edge.source,
                edge.file,
                edge.spans.clone(),
            ),
        );
    }

    fn build_delta_lww_from_source(&self, source_idx: u32) -> HashMap<EdgeKey, DeltaFromEntry> {
        let mut delta_lww = HashMap::new();
        for edge in self.delta.iter() {
            if edge.source.index() == source_idx {
                Self::update_delta_lww_from_edge(&mut delta_lww, edge);
            }
        }
        delta_lww
    }

    fn build_delta_lww_to_target(&self, target: NodeId) -> HashMap<EdgeKey, DeltaToEntry> {
        let mut delta_lww = HashMap::new();
        for edge in self.delta.iter() {
            if edge.target == target {
                Self::update_delta_lww_to_edge(&mut delta_lww, edge);
            }
        }
        delta_lww
    }

    fn csr_edge_shadowed_by_delta(
        source: NodeId,
        edge_ref: &super::super::storage::csr::EdgeRef,
        delta_lww: &HashMap<EdgeKey, DeltaFromEntry>,
    ) -> bool {
        let key = EdgeKey {
            source,
            target: edge_ref.target,
            kind: edge_ref.kind.clone(),
        };
        delta_lww
            .get(&key)
            .is_some_and(|(delta_seq, _, _, _, _, _)| *delta_seq > edge_ref.seq)
    }

    fn csr_has_edge_with_seq_at_least(
        &self,
        source_idx: u32,
        target: NodeId,
        kind: &EdgeKind,
        seq: u64,
    ) -> bool {
        self.csr.as_ref().is_some_and(|csr| {
            csr.edges_of(source_idx).any(|edge_ref| {
                edge_ref.target == target && edge_ref.kind == *kind && edge_ref.seq >= seq
            })
        })
    }

    fn append_csr_edges_from(
        &self,
        source: NodeId,
        source_idx: u32,
        delta_lww: &HashMap<EdgeKey, DeltaFromEntry>,
        result: &mut Vec<StoreEdgeRef>,
    ) {
        let Some(ref csr) = self.csr else {
            return;
        };

        for edge_ref in csr.edges_of(source_idx) {
            if self.is_edge_tombstoned(edge_ref.index) {
                continue;
            }

            if Self::csr_edge_shadowed_by_delta(source, &edge_ref, delta_lww) {
                continue;
            }

            result.push(StoreEdgeRef {
                source, // Full NodeId with generation
                target: edge_ref.target,
                kind: edge_ref.kind,
                seq: edge_ref.seq,
                file: FileId::INVALID,
                spans: edge_ref.spans.clone(),
            });
        }
    }

    fn append_delta_edges_from(
        &self,
        delta_lww: HashMap<EdgeKey, DeltaFromEntry>,
        result: &mut Vec<StoreEdgeRef>,
    ) {
        for (key, (seq, is_add, target, kind, file, spans)) in delta_lww {
            if !is_add {
                continue;
            }

            if self.csr_has_edge_with_seq_at_least(key.source.index(), target, &kind, seq) {
                continue;
            }

            result.push(StoreEdgeRef {
                source: key.source,
                target,
                kind,
                seq,
                file,
                spans,
            });
        }
    }

    /// Returns edges from a source node.
    ///
    /// Merges CSR edges (minus tombstones) with delta, applying LWW semantics.
    /// For each unique edge (source, target, kind), the operation with the
    /// highest sequence number wins (FR-30, FR-43, FR-44).
    pub fn edges_from(&self, source: NodeId) -> Vec<StoreEdgeRef> {
        let source_idx = source.index();

        // Build LWW map from delta: EdgeKey -> (highest_seq, is_add, edge_data, file)
        // This tells us the final state of each edge in delta
        let delta_lww = self.build_delta_lww_from_source(source_idx);

        let mut result = Vec::new();

        // CSR edges filtered by tombstones AND delta removes
        self.append_csr_edges_from(source, source_idx, &delta_lww, &mut result);

        // Add delta edges where latest op is Add
        self.append_delta_edges_from(delta_lww, &mut result);

        result
    }

    /// Returns edges to a target node (from delta only).
    ///
    /// Note: Without a reverse CSR, this only scans delta edges.
    /// For production, use `BidirectionalEdgeStore` which maintains a reverse store.
    /// Applies LWW semantics to delta edges (FR-43, FR-44).
    pub fn edges_to(&self, target: NodeId) -> Vec<StoreEdgeRef> {
        // Build LWW map from delta: EdgeKey -> (highest_seq, is_add, source, file, spans)
        let delta_lww = self.build_delta_lww_to_target(target);

        // Return only edges where latest op is Add
        delta_lww
            .into_iter()
            .filter_map(|(key, (seq, is_add, source, file, spans))| {
                if is_add {
                    Some(StoreEdgeRef {
                        source, // Full NodeId with generation from delta buffer
                        target,
                        kind: key.kind,
                        seq,
                        file, // File for correct Remove delta partitioning
                        spans,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Returns true if an edge exists between source and target.
    pub fn has_edge(&self, source: NodeId, target: NodeId, kind: &EdgeKind) -> bool {
        // Check delta first (most recent)
        if let Some(exists) = self.check_delta_for_edge(source, target, kind) {
            return exists;
        }

        // Check CSR
        self.check_csr_for_edge(source, target, kind)
    }

    /// Returns true if there's a delta operation for this edge.
    /// Returns Some(true) if latest is Add, Some(false) if Remove, None if not in delta.
    fn check_delta_for_edge(
        &self,
        source: NodeId,
        target: NodeId,
        kind: &EdgeKind,
    ) -> Option<bool> {
        let mut latest_seq: Option<u64> = None;
        let mut latest_is_add = false;

        for edge in self.delta.iter() {
            if Self::delta_edge_matches(edge, source, target, kind)
                && Self::should_update_latest_seq(latest_seq, edge.seq)
            {
                latest_seq = Some(edge.seq);
                latest_is_add = edge.is_add();
            }
        }

        latest_seq.map(|_| latest_is_add)
    }

    fn delta_edge_matches(
        edge: &DeltaEdge,
        source: NodeId,
        target: NodeId,
        kind: &EdgeKind,
    ) -> bool {
        edge.source == source && edge.target == target && &edge.kind == kind
    }

    fn should_update_latest_seq(latest_seq: Option<u64>, candidate_seq: u64) -> bool {
        latest_seq.is_none_or(|latest| candidate_seq > latest)
    }

    /// Checks if edge exists in CSR (considering tombstones).
    fn check_csr_for_edge(&self, source: NodeId, target: NodeId, kind: &EdgeKind) -> bool {
        let Some(ref csr) = self.csr else {
            return false;
        };

        for edge_ref in csr.edges_of(source.index()) {
            if Self::csr_edge_matches(&edge_ref, target, kind) && self.csr_edge_is_live(&edge_ref) {
                return true;
            }
        }

        false
    }

    fn csr_edge_matches(
        edge_ref: &super::super::storage::csr::EdgeRef,
        target: NodeId,
        kind: &EdgeKind,
    ) -> bool {
        edge_ref.target == target && &edge_ref.kind == kind
    }

    fn csr_edge_is_live(&self, edge_ref: &super::super::storage::csr::EdgeRef) -> bool {
        edge_ref.index < self.csr_tombstones.len() && !self.csr_tombstones[edge_ref.index]
    }

    /// Clears all edges for a specific file.
    ///
    /// Returns the number of delta edges cleared.
    pub fn clear_file(&mut self, file: FileId) -> usize {
        self.delta.clear_file(file)
    }

    /// Returns statistics about the store.
    #[must_use]
    pub fn stats(&self) -> EdgeStoreStats {
        EdgeStoreStats {
            csr_edge_count: self
                .csr
                .as_ref()
                .map_or(0, super::super::storage::csr::CsrGraph::edge_count),
            csr_version: self.csr_version,
            tombstone_count: self.csr_tombstones.iter().filter(|&&t| t).count(),
            delta_edge_count: self.delta.len(),
            delta_byte_size: self.delta.byte_size(),
            delta_file_count: self.delta.file_count(),
        }
    }

    /// Checks if a CSR edge at the given index is tombstoned.
    ///
    /// Returns `true` if the edge is tombstoned (deleted), `false` if it's live
    /// or if the index is out of bounds.
    #[must_use]
    pub fn is_edge_tombstoned(&self, edge_index: usize) -> bool {
        self.csr_tombstones
            .get(edge_index)
            .copied()
            .unwrap_or(false)
    }

    /// Swaps in a new CSR graph (used during compaction).
    ///
    /// The old CSR is replaced, tombstones are cleared, and version is bumped.
    pub fn swap_csr(&mut self, new_csr: CsrGraph) {
        let edge_count = new_csr.edge_count();
        self.csr = Some(new_csr);
        self.csr_tombstones = vec![false; edge_count];
        self.csr_version += 1;
    }

    /// Swaps in a new CSR graph and returns the old CSR and tombstones.
    ///
    /// Used during two-phase compaction to enable rollback on failure.
    /// Returns `(old_csr, old_tombstones, new_version)`.
    pub fn swap_csr_returning_old(
        &mut self,
        new_csr: CsrGraph,
    ) -> (Option<CsrGraph>, Vec<bool>, u64) {
        let edge_count = new_csr.edge_count();
        let old_csr = self.csr.replace(new_csr);
        let old_tombstones = std::mem::replace(&mut self.csr_tombstones, vec![false; edge_count]);
        self.csr_version += 1;
        (old_csr, old_tombstones, self.csr_version)
    }

    /// Restores a CSR from a rollback checkpoint.
    ///
    /// Used during two-phase compaction to restore the CSR on failure.
    /// Decrements the version to maintain consistency.
    pub fn restore_csr(&mut self, old_csr: Option<CsrGraph>, old_tombstones: Vec<bool>) {
        self.csr = old_csr;
        self.csr_tombstones = old_tombstones;
        // Decrement version to undo the swap's increment
        self.csr_version = self.csr_version.saturating_sub(1);
    }

    /// Clears the delta buffer.
    ///
    /// Called after successful compaction.
    pub fn clear_delta(&mut self) {
        self.delta.clear();
    }

    /// Takes all delta edges for compaction.
    pub fn take_delta(&mut self) -> HashMap<FileId, Vec<DeltaEdge>> {
        self.delta.take_all()
    }
}

impl Default for EdgeStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about an `EdgeStore`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeStoreStats {
    /// Number of edges in CSR.
    pub csr_edge_count: usize,
    /// Current CSR version.
    pub csr_version: u64,
    /// Number of tombstoned edges.
    pub tombstone_count: usize,
    /// Number of edges in delta buffer.
    pub delta_edge_count: usize,
    /// Byte size of delta buffer.
    pub delta_byte_size: usize,
    /// Number of files in delta buffer.
    pub delta_file_count: usize,
}

#[cfg(test)]
mod tests {
    use super::super::super::storage::CsrBuilder;
    use super::*;

    fn make_csr() -> CsrGraph {
        // Build a simple CSR: node 0 -> [1, 2], node 1 -> [2]
        // We have 3 nodes: 0, 1, 2
        let mut builder = CsrBuilder::new(3);

        builder
            .add_edge(
                0,
                NodeId::new(1, 0),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                1,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                0,
                NodeId::new(2, 0),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                2,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                1,
                NodeId::new(2, 0),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                3,
                vec![],
            )
            .unwrap();

        builder.build().unwrap()
    }

    #[test]
    fn test_edge_store_new() {
        let store = EdgeStore::new();
        assert!(store.csr().is_none());
        assert_eq!(store.delta().len(), 0);
        assert_eq!(store.csr_version(), 0);
    }

    #[test]
    fn test_edge_store_with_csr() {
        let csr = make_csr();
        let edge_count = csr.edge_count();
        let store = EdgeStore::with_csr(csr);

        assert!(store.csr().is_some());
        assert_eq!(store.csr_version(), 1);
        assert_eq!(store.stats().csr_edge_count, edge_count);
        assert_eq!(store.stats().tombstone_count, 0);
    }

    #[test]
    fn test_add_edge() {
        let mut store = EdgeStore::new();
        let source = NodeId::new(1, 0);
        let target = NodeId::new(2, 0);
        let file = FileId::new(10);

        let edge = store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        assert_eq!(edge.source, source);
        assert_eq!(edge.target, target);
        assert!(edge.is_add());
        assert_eq!(store.delta().len(), 1);
    }

    #[test]
    fn test_add_multiple_edges() {
        let mut store = EdgeStore::new();
        let file = FileId::new(10);

        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::References,
            file,
        );

        assert_eq!(store.delta().len(), 3);
    }

    #[test]
    fn test_edges_from_delta_only() {
        let mut store = EdgeStore::new();
        let source = NodeId::new(1, 0);
        let file = FileId::new(10);

        store.add_edge(
            source,
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            source,
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(4, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        let edges = store.edges_from(source);
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn test_remove_edge_delta_only() {
        let mut store = EdgeStore::new();
        let source = NodeId::new(1, 0);
        let target = NodeId::new(2, 0);
        let file = FileId::new(10);

        // Add then remove
        store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        let remove = store.remove_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        assert!(remove.is_remove());
        assert_eq!(store.delta().len(), 2);

        // has_edge should check delta and find the remove shadows the add
        // (Most recent is Remove, so edge doesn't exist)
        assert!(!store.has_edge(
            source,
            target,
            &EdgeKind::Calls {
                argument_count: 0,
                is_async: false
            }
        ));
    }

    #[test]
    fn test_has_edge_in_delta() {
        let mut store = EdgeStore::new();
        let source = NodeId::new(1, 0);
        let target = NodeId::new(2, 0);
        let file = FileId::new(10);

        assert!(!store.has_edge(
            source,
            target,
            &EdgeKind::Calls {
                argument_count: 0,
                is_async: false
            }
        ));

        store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        assert!(store.has_edge(
            source,
            target,
            &EdgeKind::Calls {
                argument_count: 0,
                is_async: false
            }
        ));
        assert!(!store.has_edge(source, target, &EdgeKind::References));
    }

    #[test]
    fn test_clear_file() {
        let mut store = EdgeStore::new();
        let file1 = FileId::new(10);
        let file2 = FileId::new(20);

        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file1,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file2,
        );
        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file1,
        );

        assert_eq!(store.delta().len(), 3);

        let removed = store.clear_file(file1);
        assert_eq!(removed, 2);
        assert_eq!(store.delta().len(), 1);
    }

    #[test]
    fn test_stats() {
        let mut store = EdgeStore::new();
        let file = FileId::new(10);

        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        let stats = store.stats();
        assert_eq!(stats.csr_edge_count, 0);
        assert_eq!(stats.csr_version, 0);
        assert_eq!(stats.tombstone_count, 0);
        assert_eq!(stats.delta_edge_count, 2);
        assert!(stats.delta_byte_size > 0);
        assert_eq!(stats.delta_file_count, 1);
    }

    #[test]
    fn test_swap_csr() {
        let mut store = EdgeStore::new();
        assert_eq!(store.csr_version(), 0);

        let csr = make_csr();
        store.swap_csr(csr);

        assert_eq!(store.csr_version(), 1);
        assert!(store.csr().is_some());
        assert_eq!(store.stats().tombstone_count, 0);
    }

    #[test]
    fn test_clear_delta() {
        let mut store = EdgeStore::new();
        let file = FileId::new(10);

        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        assert_eq!(store.delta().len(), 2);

        store.clear_delta();

        assert_eq!(store.delta().len(), 0);
    }

    #[test]
    fn test_take_delta() {
        let mut store = EdgeStore::new();
        let file1 = FileId::new(10);
        let file2 = FileId::new(20);

        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file1,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file2,
        );

        let taken = store.take_delta();

        assert_eq!(taken.len(), 2); // Two files
        assert_eq!(store.delta().len(), 0);
    }

    #[test]
    fn test_default() {
        let store: EdgeStore = EdgeStore::default();
        assert!(store.csr().is_none());
    }

    #[test]
    fn test_edge_store_error_display() {
        let err1 = EdgeStoreError::InvalidNode(NodeId::new(42, 1));
        assert!(format!("{err1}").contains("invalid node"));

        let err2 = EdgeStoreError::CsrError("test error".to_string());
        assert!(format!("{err2}").contains("CSR error"));

        let err3 = EdgeStoreError::DeltaBufferFull {
            current_bytes: 100,
            requested_bytes: 50,
            limit: 120,
        };
        assert!(format!("{err3}").contains("delta buffer full"));

        let err4 = EdgeStoreError::EdgeSizeExceeded {
            edge_bytes: 200,
            reservation_bytes: 100,
        };
        assert!(format!("{err4}").contains("edge size"));
        assert!(format!("{err4}").contains("exceeds reservation"));
    }

    #[test]
    fn test_edges_to() {
        let mut store = EdgeStore::new();
        let file = FileId::new(10);
        let target = NodeId::new(5, 0);

        store.add_edge(
            NodeId::new(1, 0),
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            NodeId::new(2, 0),
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        let edges_to_target = store.edges_to(target);
        assert_eq!(edges_to_target.len(), 2);
    }

    // Step 10b: Edge Size Validation tests (FR-58)

    #[test]
    fn test_push_committed_validates_size() {
        let mut store = EdgeStore::new();
        let file = FileId::new(10);

        // Create edges
        let edge1 = DeltaEdge::new(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            1,
            DeltaOp::Add,
            file,
        );
        let edge2 = DeltaEdge::new(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            2,
            DeltaOp::Add,
            file,
        );

        let edge_batch = vec![edge1.clone(), edge2.clone()];
        let actual_size = edge1.byte_size() + edge2.byte_size();

        // Reservation smaller than actual - should reject
        let result = store.push_committed(edge_batch, actual_size - 1);
        assert!(result.is_err());

        // Verify nothing was pushed
        assert_eq!(store.delta().len(), 0);
    }

    #[test]
    fn test_push_committed_accepts_valid() {
        let mut store = EdgeStore::new();
        let file = FileId::new(10);

        // Create edges
        let edge1 = DeltaEdge::new(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            1,
            DeltaOp::Add,
            file,
        );
        let edge2 = DeltaEdge::new(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            2,
            DeltaOp::Add,
            file,
        );

        let actual_size = edge1.byte_size() + edge2.byte_size();
        let edge_batch = vec![edge1, edge2];

        // Exact size - should accept
        let result = store.push_committed(edge_batch.clone(), actual_size);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), actual_size);
        assert_eq!(store.delta().len(), 2);

        // Larger reservation - should also accept
        let mut store2 = EdgeStore::new();
        let result2 = store2.push_committed(edge_batch, actual_size + 100);
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), actual_size);
    }

    #[test]
    fn test_edge_exceeds_reservation_error() {
        let mut store = EdgeStore::new();
        let file = FileId::new(10);

        let edge = DeltaEdge::new(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            1,
            DeltaOp::Add,
            file,
        );

        let edge_bytes = edge.byte_size();
        let reservation_bytes = edge_bytes - 1; // Less than needed

        let result = store.push_committed(vec![edge], reservation_bytes);

        // Verify correct error type with correct values
        assert!(matches!(
            result,
            Err(EdgeStoreError::EdgeSizeExceeded {
                edge_bytes: eb,
                reservation_bytes: rb,
            }) if eb == edge_bytes && rb == reservation_bytes
        ));
    }

    // FR-43/FR-44: LWW semantics tests

    #[test]
    fn test_edges_from_applies_lww_removes() {
        // Test that edges_from correctly excludes removed edges
        let mut store = EdgeStore::new();
        let source = NodeId::new(1, 0);
        let target = NodeId::new(2, 0);
        let file = FileId::new(10);

        // Add then remove the same edge
        store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.remove_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        // edges_from should NOT return the removed edge
        let edges = store.edges_from(source);
        assert!(
            edges.is_empty(),
            "edges_from should not return removed edges, got {edges:?}"
        );
    }

    #[test]
    fn test_edges_from_lww_add_after_remove() {
        // Test that re-adding after removal works
        let mut store = EdgeStore::new();
        let source = NodeId::new(1, 0);
        let target = NodeId::new(2, 0);
        let file = FileId::new(10);

        // Add -> Remove -> Add
        store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.remove_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        // edges_from should return the edge (latest op is Add)
        let edges = store.edges_from(source);
        assert_eq!(edges.len(), 1, "should have 1 edge after add->remove->add");
    }

    #[test]
    fn test_edges_to_applies_lww_removes() {
        // Test that edges_to correctly excludes removed edges
        let mut store = EdgeStore::new();
        let source = NodeId::new(1, 0);
        let target = NodeId::new(2, 0);
        let file = FileId::new(10);

        // Add then remove
        store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );
        store.remove_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            file,
        );

        // edges_to should NOT return the removed edge
        let edges = store.edges_to(target);
        assert!(
            edges.is_empty(),
            "edges_to should not return removed edges, got {edges:?}"
        );
    }

    #[test]
    fn test_push_committed_advances_seq_counter() {
        let mut store = EdgeStore::new();
        let file = FileId::new(10);

        // Create edges with high sequence numbers
        let edge1 = DeltaEdge::new(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            100, // seq = 100
            DeltaOp::Add,
            file,
        );
        let edge2 = DeltaEdge::new(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            50, // seq = 50
            DeltaOp::Add,
            file,
        );

        let edge_batch = vec![edge1.clone(), edge2.clone()];
        let reservation = edge1.byte_size() + edge2.byte_size() + 100;

        // Counter should start at 0
        assert_eq!(store.delta().current_seq(), 0);

        // Push committed edges
        store.push_committed(edge_batch, reservation).unwrap();

        // Counter should be advanced to max(seq) + 1 = 101
        assert_eq!(
            store.delta().current_seq(),
            101,
            "seq counter should be advanced to max(pushed_seq) + 1"
        );

        // Next seq should be 101
        let next = store.delta().next_seq();
        assert_eq!(next, 101, "next_seq should return the advanced value");
    }

    #[test]
    fn test_push_committed_empty_preserves_counter() {
        let mut store = EdgeStore::new();

        // Generate some sequence numbers
        store.delta().next_seq(); // 0
        store.delta().next_seq(); // 1
        assert_eq!(store.delta().current_seq(), 2);

        // Push empty list
        store.push_committed(vec![], 1000).unwrap();

        // Counter should be unchanged
        assert_eq!(
            store.delta().current_seq(),
            2,
            "empty push should not change counter"
        );
    }
}
