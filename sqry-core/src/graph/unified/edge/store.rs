//! `EdgeStore`: Two-tier edge storage combining CSR and `DeltaBuffer`.
//!
//! This module implements the two-tier edge storage system for the unified graph:
//! - **Tier 1 (CSR)**: Stable, read-optimized compressed sparse row format
//! - **Tier 2 (`DeltaBuffer`)**: Mutable, write-optimized storage with sequence numbers
//!
//! # Design
//!
//! Queries merge both tiers:
//! - CSR edges filtered by tombstone bitmap
//! - Delta edges filtered by `op != Remove`
//! - Union of both sets
//!
//! Writes go to the delta buffer. Periodically, compaction merges deltas
//! into a new CSR and clears the buffer.
//!
//! # Tombstone Management
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
#[cfg(test)]
use super::kind::ResolvedVia;

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
    /// Edge size exceeds reservation.
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

    /// Construct an `EdgeStore` from raw V10 wire parts.
    ///
    /// Reserved for the V10 → V11 upconvert path in
    /// [`crate::graph::unified::persistence::legacy_v10`]. Mirrors the
    /// field layout exactly so a V10 snapshot's edge store can be
    /// rehydrated without going through `add_edge` (which would lose
    /// the persisted `csr_version` and per-edge sequence numbers).
    ///
    /// `pub(crate)` because only the `persistence/legacy_v10` module has a
    /// legitimate reason to construct an `EdgeStore` from out-of-band
    /// parts; external callers must go through `EdgeStore::new` or
    /// `with_csr`.
    #[must_use]
    pub(crate) fn from_parts_v10_upconvert(
        csr: Option<CsrGraph>,
        csr_tombstones: Vec<bool>,
        csr_version: u64,
        delta: DeltaBuffer,
    ) -> Self {
        Self {
            csr,
            csr_tombstones,
            csr_version,
            delta,
        }
    }

    /// Borrow the CSR tombstone bitmap.
    ///
    /// Used by the V11 → V10 test-only translator in
    /// [`crate::graph::unified::persistence::legacy_v10`] to construct a
    /// `EdgeStoreV10` mirror without exposing internal fields publicly.
    #[must_use]
    pub(crate) fn csr_tombstones_slice(&self) -> &[bool] {
        &self.csr_tombstones
    }

    /// Returns the CSR graph reference, if any.
    #[must_use]
    pub fn csr(&self) -> Option<&CsrGraph> {
        self.csr.as_ref()
    }

    /// Returns a mutable reference to the CSR graph, if any.
    ///
    /// Used exclusively by
    /// [`BidirectionalEdgeStore::rewrite_edge_kind_string_ids_through_remap`]
    /// so [`RebuildGraph::finalize`] step 1 can rewrite `StringId`
    /// payloads inside the CSR's `edge_kind` array in place without
    /// going through `swap_csr`. The CSR's structural invariants
    /// (`row_ptr`, `col_idx`, `edge_seq`) are not touched.
    ///
    /// `pub(crate)` because only the finalize-step-1 helper in
    /// `BidirectionalEdgeStore` (same crate) has a legitimate reason
    /// to mutate the committed CSR in place. External crates must go
    /// through `RebuildGraph::finalize()` or the regular `swap_csr`
    /// write path. See Gate 0c plan §H and iter-4 blocker.
    #[allow(dead_code)] // Only reachable through the rebuild-internals-gated path.
    #[must_use]
    pub(crate) fn csr_mut(&mut self) -> Option<&mut CsrGraph> {
        self.csr.as_mut()
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

    /// Tombstone every CSR edge whose source **or** target `NodeId` is in
    /// `dead`, and drop every delta-buffer edge (of any op) whose source
    /// or target is in `dead`.
    ///
    /// Called by [`super::super::concurrent::CodeGraph::remove_file`] and
    /// [`super::super::rebuild::rebuild_graph::RebuildGraph::remove_file`]
    /// when a file is deleted from the workspace. The caller has already
    /// tombstoned the relevant arena slots; this helper surgically marks
    /// edges that referenced any of those slots, preventing dangling
    /// references across both tiers.
    ///
    /// # Semantics
    ///
    /// * **CSR tier**: walks every row and every column entry in a single
    ///   `O(node_count + edge_count)` pass, setting
    ///   `csr_tombstones[idx] = true` whenever the edge at `idx` has
    ///   either endpoint in `dead`. We walk by source-row so that source
    ///   hits are `O(1)` per row (skip rows whose source `NodeId` is in
    ///   `dead` after setting all their edges), and target hits are
    ///   `O(edges)` scan.
    /// * **Delta tier**: [`DeltaBuffer::retain_if`] drops every edge (Add
    ///   or Remove) whose source or target is in `dead`. Remove deltas
    ///   against already-dead endpoints are moot — the CSR edge they
    ///   would have targeted is now tombstoned, and the Remove itself
    ///   would silently match against a freed slot.
    /// * `csr_version` is bumped once, regardless of how many edges were
    ///   tombstoned, so readers holding stale `csr_version()` markers
    ///   observe the change via the MVCC invalidation path.
    ///
    /// Returns the number of CSR edges newly tombstoned by this call.
    /// Delta-buffer drops are not counted — the caller tracks those via
    /// `stats()` if needed.
    ///
    /// Note: CSR tombstoning uses the slot *index* field of `NodeId` (not
    /// the full `(index, generation)` pair) because CSR column entries
    /// store the full `NodeId` — the generation stored in the CSR was
    /// captured at the most recent full rebuild and may not match the
    /// current arena's generation for a re-allocated slot. Set membership
    /// is performed against `dead`, which contains the `NodeIds` as they
    /// were *at the moment of tombstoning*; callers pass the `NodeIds`
    /// they drained from `FileRegistry::take_nodes` or the arena's live
    /// enumeration before calling `NodeArena::remove`.
    #[allow(dead_code)] // Consumer is
    // `BidirectionalEdgeStore::tombstone_edges_for_nodes` (Task 4
    // Steps 2–3) and the unit tests below.
    pub(crate) fn tombstone_edges_for_nodes(
        &mut self,
        dead: &std::collections::HashSet<NodeId>,
    ) -> usize {
        if dead.is_empty() {
            return 0;
        }
        // Pre-compute a dense set of the *slot indices* we need to kill.
        // CSR stores `NodeId` values in `col_idx` but the source axis is
        // the row index, which is a bare `u32` slot index without a
        // generation. Both axes collapse onto slot-index set membership
        // so each CSR edge check is O(1) amortised rather than
        // O(|dead|) per edge.
        let dead_slot_indices: std::collections::HashSet<u32> =
            dead.iter().map(|nid| nid.index()).collect();
        let mut newly_tombstoned: usize = 0;
        if let Some(ref csr) = self.csr {
            let node_count = csr.node_count();
            for slot_index in 0..node_count {
                let Ok(slot_u32) = u32::try_from(slot_index) else {
                    continue;
                };
                let source_slot_dead = dead_slot_indices.contains(&slot_u32);
                for edge_ref in csr.edges_of(slot_u32) {
                    if edge_ref.index >= self.csr_tombstones.len() {
                        continue;
                    }
                    if self.csr_tombstones[edge_ref.index] {
                        continue; // already tombstoned
                    }
                    // The semantic rule matches plan A2 §F.2: no live
                    // edge may reference any NodeId in the drained
                    // tombstone set after file removal. We kill on
                    // slot-index membership (not full NodeId) because
                    // the arena's slot generation advances on remove;
                    // any CSR column whose slot is being tombstoned is
                    // semantically dead regardless of the captured
                    // generation in col_idx.
                    let target_slot_dead = dead_slot_indices.contains(&edge_ref.target.index());
                    if source_slot_dead || target_slot_dead {
                        self.csr_tombstones[edge_ref.index] = true;
                        newly_tombstoned += 1;
                    }
                }
            }
        }
        // Delta tier: drop every edge with a dead endpoint. We match on
        // *slot index* (not full NodeId) for symmetry with the CSR pass
        // above, which matters because a node removed mid-build can
        // still have Add/Remove deltas queued against its pre-remove
        // generation — those deltas must die alongside the node.
        self.delta.retain_if(|edge| {
            !dead_slot_indices.contains(&edge.source.index())
                && !dead_slot_indices.contains(&edge.target.index())
        });
        if newly_tombstoned > 0 {
            self.csr_version = self.csr_version.wrapping_add(1);
        }
        newly_tombstoned
    }

    /// Pushes committed edges with size validation.
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

        // Validate edge_bytes <= reservation.bytes
        if edge_bytes > reservation_bytes {
            return Err(EdgeStoreError::EdgeSizeExceeded {
                edge_bytes,
                reservation_bytes,
            });
        }

        // /Find max sequence number to advance counter
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
    /// highest sequence number wins
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

    /// Returns every live forward edge in the store in a **single pass**,
    /// applying LWW across CSR and delta globally.
    ///
    /// Equivalent in output to concatenating [`edges_from`](Self::edges_from)
    /// over every source node, but strictly more efficient: `edges_from`
    /// rebuilds a per-source delta LWW map by scanning the full delta on
    /// every invocation, so calling it in a loop across N nodes is
    /// `O(N * |delta|)`. This helper scans the delta **once** AND folds
    /// the delta-Add ⇄ CSR suppression check into a `HashMap` lookup
    /// keyed on `(source_idx, target, kind)` populated during the CSR
    /// walk. The combined cost is:
    ///
    /// * Delta LWW build: `O(|delta|)`.
    /// * CSR walk: `O(|csr|)` — emits surviving edges and records them
    ///   in the suppression map in the same iteration.
    /// * Delta-Add emission: `O(|delta|)` — each delta key pays an
    ///   `O(1)` hash-map lookup against the suppression map.
    ///
    /// Total: `O(|csr| + |delta|)` — the same asymptotic cost as the
    /// legacy delta-only `forward.delta().iter()` scan in the full-build
    /// case, strictly correct for CSR-backed (post-compaction) graphs,
    /// and **no longer subject to the star-graph degeneracy** where a
    /// high-degree source shared by many delta keys used to produce
    /// `O(|csr| * |delta|)` work via per-key `csr.edges_of(source_idx)`
    /// scans (iter-2 Codex blocker — addressed here).
    ///
    /// # When to use this over [`edges_from`](Self::edges_from)
    ///
    /// Use this whenever you need a graph-wide view of forward edges
    /// filtered by [`EdgeKind`] (e.g. Pass 5's HTTP-request collection or
    /// FFI-declaration scan). `edges_from` remains the right surface for
    /// per-source queries where each source is visited at most a small,
    /// bounded number of times.
    ///
    /// # Correctness
    ///
    /// Every (source, target, kind) triple that appears in the emitted
    /// vector is a live edge — either a CSR entry not shadowed by a
    /// higher-seq delta op, or a delta Add whose key does not appear in
    /// CSR with a greater-or-equal seq. Tombstoned CSR edges and
    /// Add-followed-by-Remove delta sequences are filtered. The
    /// filtering logic mirrors `edges_from` exactly — the map-based
    /// suppression step produces the same decision as
    /// [`csr_has_edge_with_seq_at_least`](Self::csr_has_edge_with_seq_at_least)
    /// would, using the same generation-agnostic source-idx +
    /// target + kind triple as its equality relation.
    ///
    /// # Determinism
    ///
    /// Emission order is:
    /// 1. CSR edges in `(source_index, row_ptr)` order (dense, stable).
    /// 2. Delta Adds in `HashMap` iteration order (unordered), which is
    ///    non-deterministic across runs inside the delta-only segment.
    ///
    /// Consumers that need deterministic iteration (e.g. persistence
    /// encoders) must sort the result themselves. Pass 5's HTTP /
    /// FFI linkers already build lookup tables keyed by
    /// `(method, normalized_path)` / qualified name, so the emission
    /// order inside each tier is immaterial to their output.
    ///
    /// # Panics
    ///
    /// Panics if a source [`NodeId`](crate::graph::unified::node::NodeId)
    /// cannot be converted to its dense index. That would indicate corrupted
    /// graph storage because all CSR and delta entries must reference indexed
    /// nodes.
    pub fn all_live_forward_edges(&self) -> Vec<StoreEdgeRef> {
        // Build a GLOBAL delta LWW map keyed by (source, target, kind).
        // One pass over the delta buffer — shared across every source,
        // instead of `edges_from`'s per-source rebuild.
        let mut delta_lww: HashMap<EdgeKey, DeltaFromEntry> = HashMap::new();
        for edge in self.delta.iter() {
            Self::update_delta_lww_from_edge(&mut delta_lww, edge);
        }

        let mut result: Vec<StoreEdgeRef> = Vec::new();

        // Build a flat CSR-adjacency membership map during the single
        // CSR walk below, so the subsequent delta-Add suppression phase
        // can do an O(1) lookup per delta key instead of
        // `csr.edges_of(idx)` + linear scan per key.
        //
        // **Semantic equivalence with
        // [`csr_has_edge_with_seq_at_least`](Self::csr_has_edge_with_seq_at_least)
        // (iter-3 Codex blocker — addressed here).** The legacy
        // suppression check scans `csr.edges_of(source_idx)` *without*
        // filtering by tombstone or delta-shadow. We therefore populate
        // this map from **every** CSR adjacency entry — including ones
        // we will not emit because they are tombstoned or shadowed. Any
        // other choice (e.g. populating only from emitted edges) causes
        // `all_live_forward_edges` to diverge from `edges_from` on
        // shapes where a tombstoned CSR entry collides by
        // `(source_idx, target, kind)` with a delta Add, which the
        // iter-3 Codex repro demonstrates:
        //
        //   1. Seed CSR with `(0, 1, K, seq=S)`.
        //   2. `remove_edge(0, 1, K)` — tombstones the CSR entry and
        //      appends a `DeltaOp::Remove` at delta seq `R`.
        //   3. `add_edge(0, 1, K)` — appends `DeltaOp::Add` at delta
        //      seq `A`.
        //
        //   `edges_from(0)` on this state emits nothing: the CSR edge
        //   is tombstoned, but its raw adjacency still satisfies
        //   `seq >= A` (because CSR seqs and delta seqs live in
        //   overlapping integer space — a fresh `EdgeStore::with_csr`
        //   resets the delta seq counter to 0, so CSR seq 1 >= delta
        //   seq 1 is a real collision). Populating the map from the
        //   raw CSR adjacency reproduces this suppression exactly,
        //   which is what the equivalence claim in the helper's
        //   contract requires.
        //
        // The map key is `(source_idx: u32, target: NodeId, kind:
        // EdgeKind)` — generation-agnostic on the source side so it
        // matches `csr_has_edge_with_seq_at_least`, which takes a bare
        // `source_idx` and ignores the delta key's generation.
        //
        // Complexity: `O(|csr|)` time and `O(|csr_adjacency_keys|)`
        // space. Without this map, the delta-Add suppression step
        // would call `csr_has_edge_with_seq_at_least` per delta key,
        // which is `O(out_degree(source))` per call — degenerate on
        // star-shaped sources where one node has many outgoing CSR
        // edges AND many delta keys share that source, blowing total
        // work up to `O(|csr| * |delta|)`. The map flattens that to
        // `O(|csr| + |delta|)` overall.
        let mut csr_max_seq_by_key: HashMap<(u32, NodeId, EdgeKind), u64> = HashMap::new();

        // CSR edges: walk every node's adjacency slice once. Populate
        // the suppression map from EVERY raw adjacency entry (matching
        // `csr_has_edge_with_seq_at_least`'s unfiltered semantics) and,
        // in the same iteration, emit only the edges that survive the
        // tombstone + delta-shadow filters.
        if let Some(ref csr) = self.csr {
            let node_count = csr.node_count();
            for node_idx in 0..node_count {
                let node_idx_u32 = u32::try_from(node_idx)
                    .expect("CSR node index exceeds u32::MAX — invariant violated by builder");
                // CSR does not track generation; use generation 0 matching
                // the `append_csr_edges_from` convention in `edges_from`.
                let source = NodeId::new(node_idx_u32, 0);
                for edge_ref in csr.edges_of(node_idx_u32) {
                    // ALWAYS record the raw CSR adjacency so the
                    // delta-Add suppression phase sees what
                    // `csr_has_edge_with_seq_at_least` would see.
                    // `entry.and_modify` keeps the max seq if duplicate
                    // keys ever surface (post-compaction CSRs dedupe to
                    // one entry per EdgeKey, but `.max()` is a cheap
                    // and defensive merge for hand-built CSRs, snapshot
                    // reloads, and any future pre-compaction CSR shape).
                    csr_max_seq_by_key
                        .entry((node_idx_u32, edge_ref.target, edge_ref.kind.clone()))
                        .and_modify(|s| {
                            if edge_ref.seq > *s {
                                *s = edge_ref.seq;
                            }
                        })
                        .or_insert(edge_ref.seq);

                    // Separately, decide whether to emit this CSR edge.
                    // Tombstoned or shadow-by-delta entries are
                    // suppressed from the emission stream but remain
                    // recorded in `csr_max_seq_by_key` above.
                    if self.is_edge_tombstoned(edge_ref.index) {
                        continue;
                    }
                    if Self::csr_edge_shadowed_by_delta(source, &edge_ref, &delta_lww) {
                        continue;
                    }
                    result.push(StoreEdgeRef {
                        source,
                        target: edge_ref.target,
                        kind: edge_ref.kind,
                        seq: edge_ref.seq,
                        file: FileId::INVALID,
                        spans: edge_ref.spans,
                    });
                }
            }
        }

        // Delta Adds: yield every entry where the latest op is Add, and
        // no CSR edge with a higher-or-equal seq already satisfied it.
        // Suppression is an O(1) `HashMap::get` against the **raw**
        // `csr_max_seq_by_key` populated above, matching
        // `csr_has_edge_with_seq_at_least`'s exact semantics (compare
        // on `source_idx` + target + kind, ignoring source generation,
        // without pre-filtering CSR by tombstone or shadow).
        for (key, (seq, is_add, target, kind, file, spans)) in delta_lww {
            if !is_add {
                continue;
            }
            let suppression_key = (key.source.index(), target, kind.clone());
            if csr_max_seq_by_key
                .get(&suppression_key)
                .is_some_and(|csr_seq| *csr_seq >= seq)
            {
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

        result
    }

    /// Returns edges to a target node (from delta only).
    ///
    /// Note: Without a reverse CSR, this only scans delta edges.
    /// For production, use `BidirectionalEdgeStore` which maintains a reverse store.
    /// Applies LWW semantics to delta edges
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

    /// Drops the CSR cache so the next read path rebuilds it from the
    /// (now compacted) delta.
    ///
    /// Used by `RebuildGraph::finalize()` step 9 (A2 §H): the CSR is
    /// **derived** state — after a rebuild compacts tombstoned edges out
    /// of the delta tier, the prior CSR can still reference compacted
    /// arena slots via stale column-indices, so the correct operation is
    /// to drop it rather than mutate it in place. The CSR version is bumped
    /// so any readers holding `csr_version()` markers observe the change.
    pub fn reset_csr(&mut self) {
        self.csr = None;
        self.csr_tombstones.clear();
        self.csr_version = self.csr_version.wrapping_add(1);
    }

    /// Test-only: rewrite every [`EdgeKind::Calls`] in this store to carry
    /// `resolved_via = ResolvedVia::Direct`, leaving `argument_count` and
    /// `is_async` untouched.
    ///
    /// Used exclusively by `sqry-core/tests/snapshot_size_phase_a.rs`
    /// (U19) to materialize a "Phase-A-free" baseline edge set for the
    /// +10% snapshot-size gate. Walks both the CSR `edge_kind` slice
    /// (in place) and every `Add` delta still in the delta buffer.
    ///
    /// Gated behind `cfg(any(test, feature = "test-support"))` so the
    /// helper is invisible to production builds. Mutating `EdgeKind`s
    /// in place is safe because the CSR's structural invariants
    /// (`row_ptr`, `col_idx`, `edge_seq`) are not touched — only the
    /// metadata payload of the `Calls` variant changes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn normalize_calls_resolved_via_for_test(&mut self) {
        use crate::graph::unified::edge::ResolvedVia;

        if let Some(csr) = self.csr.as_mut() {
            for k in csr.edge_kind_mut() {
                if let EdgeKind::Calls { resolved_via, .. } = k {
                    *resolved_via = ResolvedVia::Direct;
                }
            }
        }
        for delta in self.delta.iter_mut() {
            if let EdgeKind::Calls { resolved_via, .. } = &mut delta.kind {
                *resolved_via = ResolvedVia::Direct;
            }
        }
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

impl crate::graph::unified::memory::GraphMemorySize for EdgeStore {
    fn heap_bytes(&self) -> usize {
        use crate::graph::unified::memory::GraphMemorySize;

        let csr_bytes = self.csr.as_ref().map_or(0, GraphMemorySize::heap_bytes);
        let tombstones = self.csr_tombstones.capacity() * std::mem::size_of::<bool>();
        let delta = self.delta.heap_bytes();
        csr_bytes + tombstones + delta
    }
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
                    resolved_via: ResolvedVia::Direct,
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
                    resolved_via: ResolvedVia::Direct,
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
                    resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.add_edge(
            source,
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(4, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        let remove = store.remove_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            }
        ));

        store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );

        assert!(store.has_edge(
            source,
            target,
            &EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
            },
            file1,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file2,
        );
        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
            },
            file1,
        );
        store.add_edge(
            NodeId::new(2, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.add_edge(
            NodeId::new(2, 0),
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(3, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );

        let edges_to_target = store.edges_to(target);
        assert_eq!(edges_to_target.len(), 2);
    }

    // Step 10b: Edge Size Validation tests

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
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
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

    // /LWW semantics tests

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
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.remove_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.remove_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.remove_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
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

    // ------------------------------------------------------------------
    // Task 4 Step 2 — EdgeStore::tombstone_edges_for_nodes (CSR path)
    // ------------------------------------------------------------------

    #[test]
    fn tombstone_edges_for_nodes_empty_dead_set_is_a_noop() {
        let mut store = EdgeStore::with_csr(make_csr());
        let prior_version = store.csr_version();
        let prior_tombstones = store.stats().tombstone_count;

        let newly = store.tombstone_edges_for_nodes(&std::collections::HashSet::new());
        assert_eq!(newly, 0);
        assert_eq!(store.csr_version(), prior_version);
        assert_eq!(store.stats().tombstone_count, prior_tombstones);
    }

    #[test]
    fn tombstone_edges_for_nodes_kills_csr_edges_with_dead_source() {
        // CSR: 0 -> 1, 0 -> 2, 1 -> 2. Mark node 0 dead → edges 0->1
        // and 0->2 must be tombstoned, but 1->2 must remain live.
        let mut store = EdgeStore::with_csr(make_csr());
        let dead: std::collections::HashSet<NodeId> = [NodeId::new(0, 0)].into_iter().collect();

        let newly = store.tombstone_edges_for_nodes(&dead);
        assert_eq!(newly, 2, "both 0->1 and 0->2 must tombstone");
        assert_eq!(store.stats().tombstone_count, 2);
        // Confirm the surviving edge 1->2 is still live.
        assert!(
            store.edges_from(NodeId::new(1, 0)).iter().any(|e| {
                e.target == NodeId::new(2, 0)
                    && matches!(
                        e.kind,
                        EdgeKind::Calls {
                            argument_count: 0,
                            is_async: false,
                            resolved_via: ResolvedVia::Direct,
                        }
                    )
            }),
            "edge 1->2 must survive when only node 0 is tombstoned"
        );
    }

    #[test]
    fn tombstone_edges_for_nodes_kills_csr_edges_with_dead_target() {
        // CSR: 0 -> 1, 0 -> 2, 1 -> 2. Mark node 2 dead → edges 0->2
        // and 1->2 must be tombstoned; 0->1 survives.
        let mut store = EdgeStore::with_csr(make_csr());
        let dead: std::collections::HashSet<NodeId> = [NodeId::new(2, 0)].into_iter().collect();

        let newly = store.tombstone_edges_for_nodes(&dead);
        assert_eq!(newly, 2, "both 0->2 and 1->2 must tombstone");
        assert_eq!(store.stats().tombstone_count, 2);
        // Confirm 0->1 survives.
        assert!(
            store.edges_from(NodeId::new(0, 0)).iter().any(|e| {
                e.target == NodeId::new(1, 0)
                    && matches!(
                        e.kind,
                        EdgeKind::Calls {
                            argument_count: 0,
                            is_async: false,
                            resolved_via: ResolvedVia::Direct,
                        }
                    )
            }),
            "edge 0->1 must survive when only node 2 is tombstoned"
        );
    }

    #[test]
    fn tombstone_edges_for_nodes_bumps_csr_version_when_work_done() {
        let mut store = EdgeStore::with_csr(make_csr());
        let prior = store.csr_version();
        let dead: std::collections::HashSet<NodeId> = [NodeId::new(0, 0)].into_iter().collect();

        let newly = store.tombstone_edges_for_nodes(&dead);
        assert!(newly > 0);
        assert_eq!(
            store.csr_version(),
            prior.wrapping_add(1),
            "csr_version must bump when any edge was tombstoned"
        );
    }

    #[test]
    fn tombstone_edges_for_nodes_does_not_bump_version_when_no_work() {
        // A dead node that does not appear as any endpoint — no edge
        // should tombstone, csr_version stays put.
        let mut store = EdgeStore::with_csr(make_csr());
        let prior = store.csr_version();
        let dead: std::collections::HashSet<NodeId> = [NodeId::new(9999, 0)].into_iter().collect();

        let newly = store.tombstone_edges_for_nodes(&dead);
        assert_eq!(newly, 0);
        assert_eq!(store.csr_version(), prior);
    }

    #[test]
    fn tombstone_edges_for_nodes_drops_delta_buffer_entries() {
        // Delta-only edges: ensure both source-dead and target-dead
        // delta entries are dropped, and an untouched entry survives.
        let mut store = EdgeStore::new();
        let alive = NodeId::new(100, 0);
        let dead_nid = NodeId::new(7, 0);
        store.add_edge(
            alive,
            dead_nid,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            FileId::new(1),
        );
        store.add_edge(dead_nid, alive, EdgeKind::References, FileId::new(1));
        store.add_edge(
            alive,
            NodeId::new(200, 0),
            EdgeKind::References,
            FileId::new(1),
        );
        assert_eq!(store.delta_count(), 3);

        let dead: std::collections::HashSet<NodeId> = [dead_nid].into_iter().collect();
        let newly_csr = store.tombstone_edges_for_nodes(&dead);
        assert_eq!(newly_csr, 0, "no CSR edges means no new CSR tombstones");
        assert_eq!(
            store.delta_count(),
            1,
            "both delta edges touching dead_nid must be gone, the third survives"
        );
    }

    // -------- all_live_forward_edges tests (Codex iter-2 blocker fix) --

    fn make_calls(arg_count: u8) -> EdgeKind {
        EdgeKind::Calls {
            argument_count: arg_count,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        }
    }

    #[test]
    fn all_live_forward_edges_csr_only_emits_every_live_edge() {
        // A CSR-backed store with no delta entries must emit every
        // non-tombstoned CSR edge exactly once, in `(source_index,
        // row_ptr)` order. This is the "post-compaction published
        // graph" shape Pass 5 sees on warm daemons.
        let csr = make_csr();
        let store = EdgeStore::with_csr(csr);

        let all = store.all_live_forward_edges();
        // CSR seeded by `make_csr()`: node 0 → {1, 2}, node 1 → {2}.
        assert_eq!(all.len(), 3, "three live CSR edges expected");
        // `edges_from` on every source must observe the same set.
        let via_edges_from: Vec<_> = (0u32..3)
            .flat_map(|idx| store.edges_from(NodeId::new(idx, 0)))
            .collect();
        assert_eq!(
            via_edges_from.len(),
            all.len(),
            "single-pass helper must agree with per-source edges_from aggregate"
        );
    }

    #[test]
    fn all_live_forward_edges_delta_only_emits_adds_and_skips_removes() {
        // A delta-only store (no CSR — matches the full-build state
        // when Pass 5 runs before `persist_and_analyze_graph`'s CSR
        // compaction). Two adds plus one remove; the remove must be
        // suppressed, both adds survive.
        let mut store = EdgeStore::new();
        let src = NodeId::new(1, 0);
        let t1 = NodeId::new(2, 0);
        let t2 = NodeId::new(3, 0);
        let file = FileId::new(7);
        store.add_edge(src, t1, make_calls(1), file);
        store.add_edge(src, t2, make_calls(2), file);
        // Remove only the first edge — the LWW state for (src,t1,kind)
        // is `Remove` at the highest seq.
        store.remove_edge(src, t1, make_calls(1), file);

        let all = store.all_live_forward_edges();
        assert_eq!(
            all.len(),
            1,
            "one surviving delta Add must be emitted; removed edge must be suppressed"
        );
        assert_eq!(all[0].target, t2);
    }

    #[test]
    fn all_live_forward_edges_suppresses_csr_shadow_by_newer_delta() {
        // CSR has (0,1,kind,seq=1). Delta adds a NEWER op for the
        // same key. Depending on the delta op, the CSR edge is either
        // shadowed (Remove) or redundant (Add with higher seq). In
        // either case each logical edge key surfaces at most once.
        let csr = make_csr();
        let mut store = EdgeStore::with_csr(csr);

        // Delta Remove with seq > CSR's seq — shadows the CSR entry.
        let src = NodeId::new(0, 0);
        let tgt = NodeId::new(1, 0);
        store.remove_edge(src, tgt, make_calls(0), FileId::new(1));

        let all = store.all_live_forward_edges();
        // `make_csr()` seeds three edges; one is now shadowed.
        assert_eq!(
            all.len(),
            2,
            "shadowed CSR edge must not be emitted when delta removed it"
        );
        // The surviving edges must NOT include (0 → 1) — that's the
        // shadowed pair.
        assert!(
            !all.iter().any(|e| e.source.index() == 0 && e.target == tgt),
            "shadowed CSR edge (0 → 1) must be absent"
        );
    }

    #[test]
    fn all_live_forward_edges_star_source_no_quadratic_duplicate_suppression() {
        // The Codex iter-2 star-source scenario: a single source with
        // many outgoing CSR edges AND a delta Add for (almost) every
        // same-key in CSR. Before the `csr_max_seq_by_key` optimisation
        // this was `O(|csr| * |delta|)` because every delta-Add
        // emission called `csr.edges_of(source_idx).any(...)`; now the
        // suppression is an O(1) lookup against the map built during
        // the CSR walk.
        //
        // We seed 5 CSR edges out of node 0 (seq 1..=5) and 5 delta
        // Add ops that RE-ADD each of those same (0, target, kind)
        // triples but with a LOWER seq than the CSR entry. Every
        // delta Add should be suppressed because CSR already emits
        // the edge at a higher seq. Result: exactly the 5 CSR edges,
        // no duplicates.
        let mut builder = CsrBuilder::new(6);
        for t in 1u32..=5 {
            builder
                .add_edge(
                    0,
                    NodeId::new(t, 0),
                    make_calls(t as u8),
                    u64::from(t),
                    vec![],
                )
                .unwrap();
        }
        let csr = builder.build().unwrap();
        let mut store = EdgeStore::with_csr(csr);

        // Reset seq counter low so our delta Adds land BELOW the CSR
        // seqs and must be suppressed.
        let src = NodeId::new(0, 0);
        for t in 1u32..=5 {
            store.add_edge(src, NodeId::new(t, 0), make_calls(t as u8), FileId::new(1));
        }

        let all = store.all_live_forward_edges();

        // No duplicate emission from delta + CSR for the same key.
        assert_eq!(
            all.len(),
            5,
            "suppression must yield exactly the 5 CSR edges, not 10"
        );
        for t in 1u32..=5 {
            let hits = all
                .iter()
                .filter(|e| e.source.index() == 0 && e.target == NodeId::new(t, 0))
                .count();
            assert_eq!(
                hits, 1,
                "every (0 → {t}, kind) must appear exactly once across CSR + delta"
            );
        }
    }

    #[test]
    fn all_live_forward_edges_matches_edges_from_on_tombstoned_csr_plus_delta_add() {
        // Codex iter-3 semantic-equivalence regression. The shrunk
        // repro Codex built against the iter-3 diff:
        //   1. Seed CSR with (0, 1, Calls{0,false}, seq=1).
        //   2. `remove_edge(...)` — tombstones the CSR entry and
        //      appends DeltaOp::Remove (delta seq 0).
        //   3. `add_edge(...)` — appends DeltaOp::Add (delta seq 1).
        //
        // `edges_from(0)` emits nothing on this state: the CSR entry
        // is tombstoned AND `csr_has_edge_with_seq_at_least(0, 1,
        // Calls, 1)` returns true (raw CSR adjacency has seq 1 >= 1),
        // suppressing the delta Add. Before the iter-3 map fix,
        // `all_live_forward_edges` diverged (emitted the delta Add
        // because the suppression map was only populated from
        // post-filter emissions). Lock the correct behaviour in so
        // any future refactor that re-opens the gap fails CI.
        let mut builder = CsrBuilder::new(2);
        builder
            .add_edge(0, NodeId::new(1, 0), make_calls(0), 1, vec![])
            .unwrap();
        let csr = builder.build().unwrap();
        let mut store = EdgeStore::with_csr(csr);

        let src = NodeId::new(0, 0);
        let tgt = NodeId::new(1, 0);
        store.remove_edge(src, tgt, make_calls(0), FileId::new(1));
        store.add_edge(src, tgt, make_calls(0), FileId::new(1));

        let via_edges_from = store.edges_from(src);
        let via_all = store.all_live_forward_edges();

        assert_eq!(
            via_edges_from.len(),
            via_all.len(),
            "edges_from and all_live_forward_edges must agree on tombstoned-CSR + delta-Add shapes"
        );
        // Both should be zero for this specific shape: delta seq counter
        // resets to 0 on `EdgeStore::with_csr`, so the delta Add's seq
        // collides with the CSR seq, triggering suppression.
        assert_eq!(
            via_all.len(),
            0,
            "tombstoned CSR entry + delta Add with colliding seq must suppress both emissions"
        );
    }

    #[test]
    fn all_live_forward_edges_delta_add_with_higher_seq_wins_over_csr() {
        // Inverse of the previous test: the delta Add carries a seq
        // STRICTLY higher than the CSR entry. That means the delta
        // wins (shadows CSR). The CSR entry must be suppressed via
        // `csr_edge_shadowed_by_delta`, and the delta Add must be
        // emitted. Each key surfaces exactly once.
        let mut builder = CsrBuilder::new(2);
        builder
            .add_edge(0, NodeId::new(1, 0), make_calls(0), 10, vec![])
            .unwrap();
        let csr = builder.build().unwrap();
        let mut store = EdgeStore::with_csr(csr);

        let src = NodeId::new(0, 0);
        let tgt = NodeId::new(1, 0);
        // Advance the delta seq counter so our Add lands at seq > 10.
        for _ in 0..20 {
            store.add_edge(src, tgt, make_calls(0), FileId::new(1));
            store.remove_edge(src, tgt, make_calls(0), FileId::new(1));
        }
        // One final Add that ends the LWW state on Add with seq > 10.
        store.add_edge(src, tgt, make_calls(0), FileId::new(1));

        let all = store.all_live_forward_edges();
        assert_eq!(
            all.len(),
            1,
            "delta winning over CSR must yield exactly one emission, not two"
        );
    }
}
