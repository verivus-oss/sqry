//! `BidirectionalEdgeStore`: Forward + reverse edge storage.
//!
//! This module implements bidirectional edge storage for efficient traversal
//! in both directions (callers and callees). It maintains two synchronized
//! `EdgeStore` instances - one for forward edges and one for reverse edges.
//!
//! # Design
//!
//! - **Forward store**: Maps source → target (e.g., "who does A call?")
//! - **Reverse store**: Maps target → source (e.g., "who calls A?")
//!
//! Both stores are kept in sync through atomic add/remove operations.
//! Concurrent access is protected by `RwLock`.
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::edge::bidirectional::BidirectionalEdgeStore;
//!
//! let store = BidirectionalEdgeStore::new();
//!
//! // Add edge (updates both forward and reverse)
//! store.add_edge(source, target, EdgeKind::Calls { argument_count: 0, is_async: false }, file);
//!
//! // Query forward (callers)
//! let callees = store.edges_from(source);
//!
//! // Query reverse (callees)
//! let callers = store.edges_to(target);
//! ```

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use super::super::file::FileId;
use super::super::node::NodeId;
use super::super::storage::CsrGraph;
use super::delta::DeltaEdge;
use super::kind::EdgeKind;
#[cfg(test)]
use super::kind::ResolvedVia;
use super::store::{EdgeStore, EdgeStoreStats, StoreEdgeRef};

/// Bidirectional edge store with forward and reverse indices.
///
/// Maintains two synchronized `EdgeStore` instances for efficient
/// bidirectional traversal. Uses `RwLock` for concurrent read/write access.
///
/// # Thread Safety
///
/// Multiple readers can access the store concurrently. Writers get exclusive
/// access. All operations that modify state (add, remove) acquire write locks.
#[derive(Debug, Serialize, Deserialize)]
pub struct BidirectionalEdgeStore {
    /// Forward edges: source → target
    #[serde(with = "rwlock_edge_store_serde")]
    forward: RwLock<EdgeStore>,
    /// Reverse edges: target → source (inverted view)
    #[serde(with = "rwlock_edge_store_serde")]
    reverse: RwLock<EdgeStore>,
}

impl BidirectionalEdgeStore {
    /// Creates a new empty bidirectional edge store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            forward: RwLock::new(EdgeStore::new()),
            reverse: RwLock::new(EdgeStore::new()),
        }
    }

    /// Construct a `BidirectionalEdgeStore` from already-rehydrated forward
    /// and reverse `EdgeStore` halves.
    ///
    /// Reserved for the V10 → V11 upconvert path in
    /// [`crate::graph::unified::persistence::legacy_v10`]. `pub(crate)`
    /// because only that module needs to assemble a bidirectional store
    /// from out-of-band halves; production code constructs a fresh empty
    /// store and uses `add_edge_*` so forward / reverse stay in sync.
    #[must_use]
    pub(crate) fn from_parts_v10_upconvert(forward: EdgeStore, reverse: EdgeStore) -> Self {
        Self {
            forward: RwLock::new(forward),
            reverse: RwLock::new(reverse),
        }
    }

    /// Adds an edge to both forward and reverse stores.
    ///
    /// The edge is added atomically to both stores:
    /// - Forward: source → target
    /// - Reverse: target → source (with source and target swapped)
    ///
    /// Returns the delta edge from the forward store.
    /// For edges with span information, use [`add_edge_with_spans`](Self::add_edge_with_spans).
    pub fn add_edge(
        &self,
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        file: FileId,
    ) -> DeltaEdge {
        self.add_edge_with_spans(source, target, kind, file, Vec::new())
    }

    /// Adds an edge to both forward and reverse stores with span information.
    ///
    /// The edge is added atomically to both stores:
    /// - Forward: source → target
    /// - Reverse: target → source (with source and target swapped)
    ///
    /// The spans represent source locations of the edge (e.g., call sites for CALLS edges).
    /// Returns the delta edge from the forward store.
    pub fn add_edge_with_spans(
        &self,
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        file: FileId,
        spans: Vec<crate::graph::node::Span>,
    ) -> DeltaEdge {
        // Add to forward store
        let forward_edge = self.forward.write().add_edge_with_spans(
            source,
            target,
            kind.clone(),
            file,
            spans.clone(),
        );

        // Add to reverse store (swapped source/target)
        // Note: We preserve the spans even in reverse direction for querying call-site locations
        self.reverse
            .write()
            .add_edge_with_spans(target, source, kind, file, spans);

        forward_edge
    }

    /// Removes an edge from both forward and reverse stores.
    ///
    /// The edge is removed atomically from both stores.
    /// Returns the delta edge from the forward store.
    pub fn remove_edge(
        &self,
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        file: FileId,
    ) -> DeltaEdge {
        // Remove from forward store
        let forward_edge = self
            .forward
            .write()
            .remove_edge(source, target, kind.clone(), file);

        // Remove from reverse store (swapped source/target)
        self.reverse.write().remove_edge(target, source, kind, file);

        forward_edge
    }

    /// Returns outgoing edges from a source node (forward traversal).
    ///
    /// Answers: "What does `source` connect to?"
    pub fn edges_from(&self, source: NodeId) -> Vec<StoreEdgeRef> {
        self.forward.read().edges_from(source)
    }

    /// Returns every live forward edge in the store in a single pass.
    ///
    /// Thin wrapper over [`EdgeStore::all_live_forward_edges`] on the
    /// forward store. The reverse store is intentionally not walked
    /// (it mirrors the forward store; iterating both would double-count
    /// every edge). Use this when you need a graph-wide edge-kind filter
    /// — e.g. Pass 5's HTTP request collector or FFI declaration scan —
    /// rather than a per-source query.
    ///
    /// Asymptotic cost is `O(|csr| + |delta|)` on both pre- and
    /// post-compaction graphs. Calling [`edges_from`](Self::edges_from)
    /// in a loop across every node is instead `O(N * |delta|)` because
    /// each call rebuilds a per-source delta LWW map — this helper
    /// exists specifically to avoid that regression.
    ///
    /// See [`EdgeStore::all_live_forward_edges`] for correctness and
    /// determinism details.
    pub fn all_live_forward_edges(&self) -> Vec<StoreEdgeRef> {
        self.forward.read().all_live_forward_edges()
    }

    /// Returns incoming edges to a target node (reverse traversal).
    ///
    /// Answers: "What connects to `target`?"
    ///
    /// Note: The returned edges have swapped source/target from the reverse
    /// store's perspective, so they are re-mapped to the original orientation.
    /// The source `NodeId` preserves the full generation for edge key matching.
    pub fn edges_to(&self, target: NodeId) -> Vec<StoreEdgeRef> {
        // Query reverse store for edges FROM target (which are TO target in original)
        let reverse_edges = self.reverse.read().edges_from(target);

        // Remap: In reverse store, source=target and target=original_source
        // So we swap back to get edges TO the target
        // e.target is the original source (full NodeId with generation)
        // e.file is the original source file for correct Remove delta partitioning
        reverse_edges
            .into_iter()
            .map(|e| StoreEdgeRef {
                source: e.target, // Original source (full NodeId with generation) stored as target in reverse
                target,           // The query target
                kind: e.kind,
                seq: e.seq,
                file: e.file,   // Original source file for Remove delta partitioning
                spans: e.spans, // Preserve spans from reverse store
            })
            .collect()
    }

    /// Checks if an edge exists (in forward direction).
    pub fn has_edge(&self, source: NodeId, target: NodeId, kind: &EdgeKind) -> bool {
        self.forward.read().has_edge(source, target, kind)
    }

    /// Returns statistics for both stores.
    pub fn stats(&self) -> BidirectionalEdgeStoreStats {
        let forward_stats = self.forward.read().stats();
        let reverse_stats = self.reverse.read().stats();
        BidirectionalEdgeStoreStats {
            forward: forward_stats,
            reverse: reverse_stats,
        }
    }

    /// Returns the forward store's CSR version.
    #[must_use]
    pub fn csr_version(&self) -> u64 {
        self.forward.read().csr_version()
    }

    /// Clears all edges from a specific file in both stores.
    pub fn clear_file(&self, file: FileId) -> usize {
        let forward_cleared = self.forward.write().clear_file(file);
        let _reverse_cleared = self.reverse.write().clear_file(file);
        forward_cleared
    }

    /// Clears all delta buffer data from both stores.
    pub fn clear_delta(&self) {
        self.forward.write().clear_delta();
        self.reverse.write().clear_delta();
    }

    /// Atomically swaps CSR graphs into both stores and clears both deltas.
    ///
    /// This is the build-time compaction entry point. Both CSRs must be fully
    /// built offline before calling this method. The mutation phase is kept
    /// short: swap both, then clear both.
    ///
    /// # Lock Ordering
    ///
    /// Acquires forward write lock first, then reverse, to match existing
    /// lock ordering conventions in the codebase.
    pub fn swap_csrs_and_clear_deltas(&self, forward_csr: CsrGraph, reverse_csr: CsrGraph) {
        let mut forward = self.forward.write();
        let mut reverse = self.reverse.write();

        forward.swap_csr(forward_csr);
        reverse.swap_csr(reverse_csr);

        forward.clear_delta();
        reverse.clear_delta();
    }

    /// Drops the CSR caches in **both** directions so the next read path
    /// rebuilds them from the (compacted) delta.
    ///
    /// Called by `RebuildGraph::finalize()` step 9 (A2 §H). After the
    /// preceding `retain_nodes` pass has filtered the delta to live
    /// endpoints, the cached CSRs still store column-indices into the
    /// pre-compaction arena layout and therefore cannot be reused. Per
    /// §K row K.A9 ("CSR adjacency is derived state; rebuilt from
    /// compacted edges — never mutated in place"), the correct operation
    /// is to drop both CSRs; the read path regenerates them lazily.
    pub fn reset_csr_caches(&mut self) {
        self.forward.get_mut().reset_csr();
        self.reverse.get_mut().reset_csr();
    }

    /// Test-only: rewrite every [`EdgeKind::Calls`] across both the
    /// forward and reverse stores so its `resolved_via` field collapses
    /// to [`ResolvedVia::Direct`].
    ///
    /// Used exclusively by `sqry-core/tests/snapshot_size_phase_a.rs`
    /// (U19) to materialize a "Phase-A-free" baseline edge set for the
    /// +10% snapshot-size gate.
    ///
    /// [`ResolvedVia::Direct`]: crate::graph::unified::edge::ResolvedVia::Direct
    #[cfg(any(test, feature = "test-support"))]
    pub fn normalize_calls_resolved_via_for_test(&mut self) {
        self.forward
            .get_mut()
            .normalize_calls_resolved_via_for_test();
        self.reverse
            .get_mut()
            .normalize_calls_resolved_via_for_test();
    }

    /// Rewrite every `StringId` payload carried by every committed `EdgeKind`
    /// (in both forward and reverse stores, across both CSR and delta tiers)
    /// through the canonical-StringId `remap`.
    ///
    /// Called exclusively by [`RebuildGraph::finalize`] step 1 (plan §H
    /// lines 658–707, §K row **K.B1**) after
    /// [`StringInterner::build_dedup_table`] canonicalises duplicate
    /// interner slots. Without this rewrite, committed edges still
    /// reference the pre-dedup `StringId`s, leaving dangling keys once
    /// [`StringInterner::recycle_unreferenced`] frees the collapsed
    /// slots. This is the edge-store counterpart to
    /// `NodeArena` / `AuxiliaryIndices` / `FileRegistry` /
    /// `AliasTable` / `ShadowTable` remaps already wired in
    /// `RebuildGraph::finalize` step 1.
    ///
    /// Semantics (matches `build_unified_graph_inner::phase4_apply_global_remap`
    /// in [`parallel_commit::remap_edge_kind_string_ids`]):
    ///
    /// * CSR tier: walks every entry of `edge_kind` via
    ///   [`CsrGraph::edge_kind_mut`]. `row_ptr`, `col_idx`, `edge_seq`,
    ///   and `edge_spans` are unaffected; only the `EdgeKind` payloads
    ///   are rewritten in place.
    /// * Delta tier: walks every `DeltaEdge` via
    ///   [`DeltaBuffer::iter_mut`] and rewrites `edge.kind` in place.
    ///   Sequence numbers and per-file partitioning are unaffected.
    /// * Both tiers receive the same exhaustive `match` on
    ///   [`EdgeKind`] via `remap_edge_kind_string_ids`; new variants
    ///   carrying `StringId`s force a compile error until the match arm
    ///   is added there.
    ///
    /// No deduplication is performed here. Edges whose `EdgeKind`
    /// payloads collapse onto a canonical representation after remap
    /// remain distinct `DeltaEdge`s: the read path's LWW merge in
    /// [`EdgeStore::edges_from`] / [`EdgeStore::edges_to`] naturally
    /// resolves them by `EdgeKey` (source, target, kind), keeping the
    /// edge with the highest `seq`. Byte sizes are preserved because
    /// [`EdgeKind::estimated_size`] is variant-only and does not depend
    /// on `StringId` values; `DeltaBuffer::byte_size` therefore stays
    /// accurate without recomputation.
    ///
    /// # Lock ordering
    ///
    /// Acquires `get_mut()` on both `RwLock<EdgeStore>`s — no contention
    /// possible because finalize holds the rebuild writer exclusively.
    ///
    /// `pub(crate)` because the only legitimate caller is
    /// [`RebuildGraph::finalize`] step 1 inside `sqry-core`. External
    /// crates (including `sqry-daemon` with `rebuild-internals` enabled)
    /// must never reach into committed edge storage directly — the
    /// finalize contract is the single publish path. See Gate 0c plan
    /// §H "Type-enforced publish path" and iter-4 blocker.
    #[allow(clippy::implicit_hasher)]
    // Live in the default build: the consumer is `RebuildGraph::finalize()`
    // step 1, reached from the ungated public
    // `build::incremental::incremental_rebuild` -> `finalize` path.
    pub(crate) fn rewrite_edge_kind_string_ids_through_remap(
        &mut self,
        remap: &std::collections::HashMap<
            crate::graph::unified::string::id::StringId,
            crate::graph::unified::string::id::StringId,
        >,
    ) {
        if remap.is_empty() {
            return;
        }
        // Forward store: CSR edge_kind + delta edges
        {
            let forward = self.forward.get_mut();
            if let Some(csr) = forward.csr_mut() {
                for kind in csr.edge_kind_mut() {
                    crate::graph::unified::build::parallel_commit::remap_edge_kind_string_ids(
                        kind, remap,
                    );
                }
            }
            for edge in forward.delta_mut().iter_mut() {
                crate::graph::unified::build::parallel_commit::remap_edge_kind_string_ids(
                    &mut edge.kind,
                    remap,
                );
            }
        }
        // Reverse store: same treatment
        {
            let reverse = self.reverse.get_mut();
            if let Some(csr) = reverse.csr_mut() {
                for kind in csr.edge_kind_mut() {
                    crate::graph::unified::build::parallel_commit::remap_edge_kind_string_ids(
                        kind, remap,
                    );
                }
            }
            for edge in reverse.delta_mut().iter_mut() {
                crate::graph::unified::build::parallel_commit::remap_edge_kind_string_ids(
                    &mut edge.kind,
                    remap,
                );
            }
        }
    }

    /// Tombstone every committed CSR edge, and drop every delta-buffer
    /// edge, whose source or target slot index is in `dead`, across both
    /// forward and reverse stores.
    ///
    /// This is the §F.2 invalidation primitive behind
    /// [`super::super::concurrent::CodeGraph::remove_file`] and
    /// [`super::super::rebuild::rebuild_graph::RebuildGraph::remove_file`].
    /// The caller has already tombstoned the affected arena slots (via
    /// [`NodeArena::remove`]); this helper ensures no CSR or delta edge
    /// remains pointing at those now-freed slots.
    ///
    /// Returns the total number of CSR edges newly tombstoned across
    /// both stores. Delta drops are not counted here — callers that
    /// need the delta count can read `stats()` before and after.
    ///
    /// # Lock ordering
    ///
    /// Acquires `get_mut` on the forward store's `RwLock` first, then
    /// the reverse store's `RwLock`, matching the convention used by
    /// [`swap_csrs_and_clear_deltas`](Self::swap_csrs_and_clear_deltas)
    /// and
    /// [`rewrite_edge_kind_string_ids_through_remap`](Self::rewrite_edge_kind_string_ids_through_remap).
    /// Safe against concurrent readers because the caller holds
    /// exclusive `&mut self`.
    ///
    /// `pub(crate)` because the only legitimate callers are
    /// `CodeGraph::remove_file` / `RebuildGraph::remove_file` inside
    /// sqry-core. External crates must go through those higher-level
    /// entry points (which in turn gate the rebuild via the
    /// `rebuild-internals` feature or the explicit `CodeGraph::remove_file`
    /// pub(crate) API on the full-rebuild path).
    ///
    /// [`NodeArena::remove`]: super::super::storage::arena::NodeArena::remove
    #[allow(clippy::implicit_hasher)]
    // Live in the default build: the consumers are `CodeGraph::remove_file`
    // and `RebuildGraph::remove_file` (the §F.2 invalidation primitive),
    // the latter reached from the ungated public
    // `build::incremental::incremental_rebuild` -> `remove_closure_from_rebuild`
    // -> `remove_file` path (the unit tests below exercise it too). Lives
    // here so the primitive is reviewable independently of the
    // higher-level entry points.
    pub(crate) fn tombstone_edges_for_nodes(
        &mut self,
        dead: &std::collections::HashSet<super::super::node::NodeId>,
    ) -> usize {
        if dead.is_empty() {
            return 0;
        }
        let forward_tombstoned = self.forward.get_mut().tombstone_edges_for_nodes(dead);
        let reverse_tombstoned = self.reverse.get_mut().tombstone_edges_for_nodes(dead);
        forward_tombstoned + reverse_tombstoned
    }

    /// Returns a read lock on the forward store.
    pub fn forward(&self) -> parking_lot::RwLockReadGuard<'_, EdgeStore> {
        self.forward.read()
    }

    /// Returns a read lock on the reverse store.
    pub fn reverse(&self) -> parking_lot::RwLockReadGuard<'_, EdgeStore> {
        self.reverse.read()
    }

    /// Returns a write lock on the forward store.
    pub fn forward_mut(&self) -> parking_lot::RwLockWriteGuard<'_, EdgeStore> {
        self.forward.write()
    }

    /// Returns a write lock on the reverse store.
    pub fn reverse_mut(&self) -> parking_lot::RwLockWriteGuard<'_, EdgeStore> {
        self.reverse.write()
    }

    /// Bulk insert edges with pre-assigned sequence numbers.
    ///
    /// Inserts all edges from `file_edge_vecs` (one `Vec<DeltaEdge>` per file) into
    /// both forward and reverse stores. Edges must already have their `seq` fields
    /// assigned with monotonically increasing values across all vecs.
    ///
    /// After insertion, the sequence counters on both stores are advanced past the
    /// highest inserted sequence number to ensure subsequent incremental operations
    /// receive higher sequence numbers.
    ///
    /// # Arguments
    ///
    /// * `file_edge_vecs` - Ordered slices of edges grouped by file. Each inner
    ///   `Vec` contains edges for one file with pre-assigned sequence numbers.
    /// * `expected_total` - Expected total number of edges across all vecs.
    ///   Used as a consistency check.
    ///
    /// # Panics
    ///
    /// Panics if the actual total edge count does not match `expected_total`.
    pub fn add_edges_bulk_ordered(&self, file_edge_vecs: &[Vec<DeltaEdge>], expected_total: u64) {
        // Validate expected total
        let actual_total: u64 = file_edge_vecs.iter().map(|v| v.len() as u64).sum();
        assert_eq!(
            actual_total, expected_total,
            "add_edges_bulk_ordered: actual edge count {actual_total} != expected {expected_total}"
        );

        // Lock both stores for the entire bulk operation to maintain consistency
        let mut forward = self.forward.write();
        let mut reverse = self.reverse.write();

        // Validate that all incoming edge seqs are >= current store seq counters.
        // This prevents insertion of stale edges that could break merge ordering.
        let current_forward_seq = forward.seq_counter();
        let current_reverse_seq = reverse.seq_counter();

        let mut prev_seq: Option<u64> = None;

        for file_edges in file_edge_vecs {
            for edge in file_edges {
                // Validate monotonic ordering: each edge.seq must be >= previous
                if let Some(prev) = prev_seq {
                    assert!(
                        edge.seq >= prev,
                        "add_edges_bulk_ordered: non-monotonic seq: {} follows {prev}",
                        edge.seq
                    );
                }

                // Validate edge.seq >= current store counters to prevent stale insertion
                assert!(
                    edge.seq >= current_forward_seq,
                    "add_edges_bulk_ordered: edge seq {} < forward store counter {current_forward_seq}",
                    edge.seq
                );
                assert!(
                    edge.seq >= current_reverse_seq,
                    "add_edges_bulk_ordered: edge seq {} < reverse store counter {current_reverse_seq}",
                    edge.seq
                );

                prev_seq = Some(edge.seq);

                // Push to forward delta buffer
                forward.delta_mut().push(edge.clone());

                // Create reversed edge (swap source/target) and push to reverse
                let reverse_edge = DeltaEdge::with_spans(
                    edge.target,
                    edge.source,
                    edge.kind.clone(),
                    edge.seq,
                    edge.op,
                    edge.file,
                    edge.spans.clone(),
                );
                reverse.delta_mut().push(reverse_edge);
            }
        }

        // Advance sequence counters past the highest inserted seq
        if let Some(max) = prev_seq {
            forward.delta_mut().advance_seq_to(max + 1);
            reverse.delta_mut().advance_seq_to(max + 1);
        }
    }
}

impl Default for BidirectionalEdgeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for BidirectionalEdgeStore {
    fn clone(&self) -> Self {
        // Clone by acquiring read locks and cloning the inner data
        let forward_data = self.forward.read().clone();
        let reverse_data = self.reverse.read().clone();
        Self {
            forward: RwLock::new(forward_data),
            reverse: RwLock::new(reverse_data),
        }
    }
}

/// Statistics for a bidirectional edge store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BidirectionalEdgeStoreStats {
    /// Forward store statistics.
    pub forward: EdgeStoreStats,
    /// Reverse store statistics.
    pub reverse: EdgeStoreStats,
}

impl crate::graph::unified::memory::GraphMemorySize for BidirectionalEdgeStore {
    fn heap_bytes(&self) -> usize {
        let fwd = crate::graph::unified::memory::GraphMemorySize::heap_bytes(&*self.forward.read());
        let rev = crate::graph::unified::memory::GraphMemorySize::heap_bytes(&*self.reverse.read());
        fwd + rev
    }
}

#[cfg(test)]
mod tests {
    use super::super::delta::DeltaOp;
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_bidirectional_new() {
        let store = BidirectionalEdgeStore::new();
        assert_eq!(store.csr_version(), 0);
    }

    #[test]
    fn test_default() {
        let store: BidirectionalEdgeStore = BidirectionalEdgeStore::default();
        assert_eq!(store.csr_version(), 0);
    }

    #[test]
    fn test_add_updates_both_directions() {
        let store = BidirectionalEdgeStore::new();
        let source = NodeId::new(1, 0);
        let target = NodeId::new(2, 0);
        let file = FileId::new(10);

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

        // Forward: source -> target
        let forward_edges = store.edges_from(source);
        assert_eq!(forward_edges.len(), 1);
        assert_eq!(forward_edges[0].target, target);

        // Reverse: target <- source
        let reverse_edges = store.edges_to(target);
        assert_eq!(reverse_edges.len(), 1);
        assert_eq!(reverse_edges[0].source, source); // Full NodeId comparison
    }

    #[test]
    fn test_remove_updates_both_directions() {
        let store = BidirectionalEdgeStore::new();
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

        // Forward should have 0 after merge (add + remove cancel)
        // But in delta buffer we have both
        let forward_stats = store.stats().forward;
        assert_eq!(forward_stats.delta_edge_count, 2); // add + remove

        let reverse_stats = store.stats().reverse;
        assert_eq!(reverse_stats.delta_edge_count, 2); // add + remove
    }

    #[test]
    fn test_forward_reverse_consistency() {
        let store = BidirectionalEdgeStore::new();
        let file = FileId::new(10);

        // Create a graph: 1 -> 2 -> 3, 1 -> 3
        let n1 = NodeId::new(1, 0);
        let n2 = NodeId::new(2, 0);
        let n3 = NodeId::new(3, 0);

        store.add_edge(
            n1,
            n2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.add_edge(
            n2,
            n3,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );
        store.add_edge(
            n1,
            n3,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file,
        );

        // Forward from n1: should see n2 and n3
        let from_n1 = store.edges_from(n1);
        assert_eq!(from_n1.len(), 2);

        // Reverse to n3: should see n1 and n2
        let to_n3 = store.edges_to(n3);
        assert_eq!(to_n3.len(), 2);

        // Verify consistency: count of outgoing from all nodes == incoming to all nodes
        let total_outgoing =
            store.edges_from(n1).len() + store.edges_from(n2).len() + store.edges_from(n3).len();

        let total_incoming =
            store.edges_to(n1).len() + store.edges_to(n2).len() + store.edges_to(n3).len();

        assert_eq!(total_outgoing, total_incoming);
    }

    #[test]
    fn test_concurrent_access() {
        let store = Arc::new(BidirectionalEdgeStore::new());
        let file = FileId::new(10);

        let mut handles = vec![];

        // Spawn 4 writer threads
        for i in 0..4 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                for j in 0..100 {
                    let source = NodeId::new(i * 100 + j, 0);
                    let target = NodeId::new((i * 100 + j + 1) % 400, 0);
                    store_clone.add_edge(
                        source,
                        target,
                        EdgeKind::Calls {
                            argument_count: 0,
                            is_async: false,
                            resolved_via: ResolvedVia::Direct,
                        },
                        file,
                    );
                }
            });
            handles.push(handle);
        }

        // Spawn 4 reader threads
        for _ in 0..4 {
            let store_clone = Arc::clone(&store);
            let handle = thread::spawn(move || {
                for i in 0..100 {
                    let node = NodeId::new(i, 0);
                    let _ = store_clone.edges_from(node);
                    let _ = store_clone.edges_to(node);
                }
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().expect("Thread panicked");
        }

        // No assertion needed - success means no data races
        // But verify we have data
        let stats = store.stats();
        assert!(stats.forward.delta_edge_count > 0);
    }

    #[test]
    fn test_has_edge() {
        let store = BidirectionalEdgeStore::new();
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
        let store = BidirectionalEdgeStore::new();
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
            file1,
        );
        store.add_edge(
            NodeId::new(3, 0),
            NodeId::new(4, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file2,
        );

        let cleared = store.clear_file(file1);
        assert_eq!(cleared, 2);

        // file2 edges should remain
        let stats = store.stats();
        assert_eq!(stats.forward.delta_edge_count, 1);
    }

    #[test]
    fn test_clear_delta() {
        let store = BidirectionalEdgeStore::new();
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

        assert!(store.stats().forward.delta_edge_count > 0);

        store.clear_delta();

        assert_eq!(store.stats().forward.delta_edge_count, 0);
        assert_eq!(store.stats().reverse.delta_edge_count, 0);
    }

    #[test]
    fn test_stats() {
        let store = BidirectionalEdgeStore::new();
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

        let stats = store.stats();
        assert_eq!(stats.forward.delta_edge_count, 1);
        assert_eq!(stats.reverse.delta_edge_count, 1);
    }

    // --- Tests for add_edges_bulk_ordered ---

    fn make_delta_edge(source: u32, target: u32, seq: u64, file: u32) -> DeltaEdge {
        DeltaEdge::new(
            NodeId::new(source, 0),
            NodeId::new(target, 0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            seq,
            DeltaOp::Add,
            FileId::new(file),
        )
    }

    #[test]
    fn test_add_edges_bulk_ordered_basic() {
        let store = BidirectionalEdgeStore::new();

        // Two files with pre-assigned sequence numbers
        let file1_edges = vec![make_delta_edge(1, 2, 0, 10), make_delta_edge(3, 4, 1, 10)];
        let file2_edges = vec![make_delta_edge(5, 6, 2, 20), make_delta_edge(7, 8, 3, 20)];

        store.add_edges_bulk_ordered(&[file1_edges, file2_edges], 4);

        // Verify forward edges
        let stats = store.stats();
        assert_eq!(stats.forward.delta_edge_count, 4);
        assert_eq!(stats.reverse.delta_edge_count, 4);

        // Verify forward traversal
        let edges_from_1 = store.edges_from(NodeId::new(1, 0));
        assert_eq!(edges_from_1.len(), 1);
        assert_eq!(edges_from_1[0].target, NodeId::new(2, 0));

        let edges_from_5 = store.edges_from(NodeId::new(5, 0));
        assert_eq!(edges_from_5.len(), 1);
        assert_eq!(edges_from_5[0].target, NodeId::new(6, 0));

        // Verify reverse traversal
        let edges_to_2 = store.edges_to(NodeId::new(2, 0));
        assert_eq!(edges_to_2.len(), 1);
        assert_eq!(edges_to_2[0].source, NodeId::new(1, 0));

        let edges_to_8 = store.edges_to(NodeId::new(8, 0));
        assert_eq!(edges_to_8.len(), 1);
        assert_eq!(edges_to_8[0].source, NodeId::new(7, 0));

        // Verify sequence counter advanced past max seq (3) to 4
        let fwd_seq = store.forward().seq_counter();
        assert_eq!(fwd_seq, 4);
        let rev_seq = store.reverse().seq_counter();
        assert_eq!(rev_seq, 4);
    }

    #[test]
    fn test_add_edges_bulk_ordered_empty() {
        let store = BidirectionalEdgeStore::new();

        store.add_edges_bulk_ordered(&[], 0);

        let stats = store.stats();
        assert_eq!(stats.forward.delta_edge_count, 0);
        assert_eq!(stats.reverse.delta_edge_count, 0);
    }

    #[test]
    fn test_add_edges_bulk_ordered_single_file() {
        let store = BidirectionalEdgeStore::new();

        let edges = vec![
            make_delta_edge(10, 20, 0, 5),
            make_delta_edge(20, 30, 1, 5),
            make_delta_edge(30, 10, 2, 5),
        ];

        store.add_edges_bulk_ordered(&[edges], 3);

        // Verify forward edge count
        let stats = store.stats();
        assert_eq!(stats.forward.delta_edge_count, 3);
        assert_eq!(stats.reverse.delta_edge_count, 3);

        // Verify cycle: 10 -> 20 -> 30 -> 10
        let from_10 = store.edges_from(NodeId::new(10, 0));
        assert_eq!(from_10.len(), 1);
        assert_eq!(from_10[0].target, NodeId::new(20, 0));

        let from_20 = store.edges_from(NodeId::new(20, 0));
        assert_eq!(from_20.len(), 1);
        assert_eq!(from_20[0].target, NodeId::new(30, 0));

        let from_30 = store.edges_from(NodeId::new(30, 0));
        assert_eq!(from_30.len(), 1);
        assert_eq!(from_30[0].target, NodeId::new(10, 0));

        // Verify reverse: node 10 is targeted by node 30
        let to_10 = store.edges_to(NodeId::new(10, 0));
        assert_eq!(to_10.len(), 1);
        assert_eq!(to_10[0].source, NodeId::new(30, 0));

        // Verify seq counter advanced to 3
        assert_eq!(store.forward().seq_counter(), 3);
    }

    #[test]
    #[should_panic(expected = "actual edge count")]
    fn test_add_edges_bulk_ordered_wrong_expected_total() {
        let store = BidirectionalEdgeStore::new();
        let edges = vec![make_delta_edge(1, 2, 0, 1)];

        // Expected 5 but actually 1 — should panic
        store.add_edges_bulk_ordered(&[edges], 5);
    }

    #[test]
    #[should_panic(expected = "non-monotonic seq")]
    fn test_add_edges_bulk_ordered_non_monotonic_seq() {
        let store = BidirectionalEdgeStore::new();

        // seq goes 0, 5, 3 — not monotonic
        let edges = vec![
            make_delta_edge(1, 2, 0, 1),
            make_delta_edge(3, 4, 5, 1),
            make_delta_edge(5, 6, 3, 1),
        ];

        store.add_edges_bulk_ordered(&[edges], 3);
    }

    #[test]
    #[should_panic(expected = "forward store counter")]
    fn test_add_edges_bulk_ordered_stale_seq() {
        let store = BidirectionalEdgeStore::new();

        // First add some edges through normal API to advance the seq counter
        store.add_edge(
            NodeId::new(1, 0),
            NodeId::new(2, 0),
            EdgeKind::References,
            FileId::new(1),
        );
        // Seq counter is now at 1

        // Try to bulk insert with seq=0 (stale) — should panic
        let edges = vec![make_delta_edge(10, 20, 0, 5)];
        store.add_edges_bulk_ordered(&[edges], 1);
    }

    #[test]
    fn test_add_edges_bulk_ordered_seq_counter_allows_subsequent_ops() {
        let store = BidirectionalEdgeStore::new();

        let edges = vec![make_delta_edge(1, 2, 10, 1), make_delta_edge(3, 4, 20, 1)];
        store.add_edges_bulk_ordered(&[edges], 2);

        // Seq counter should be at 21 (max_seq=20, advanced to 21)
        assert_eq!(store.forward().seq_counter(), 21);

        // Now add an edge through normal API — it should get seq >= 21
        let added = store.add_edge(
            NodeId::new(100, 0),
            NodeId::new(200, 0),
            EdgeKind::References,
            FileId::new(1),
        );
        assert!(
            added.seq >= 21,
            "subsequent edge should have seq >= 21, got {}",
            added.seq
        );

        // Total should be 3 forward edges
        assert_eq!(store.stats().forward.delta_edge_count, 3);
    }

    #[test]
    fn test_swap_csrs_and_clear_deltas() {
        use super::super::super::compaction::{Direction, build_compacted_csr, snapshot_edges};

        let store = BidirectionalEdgeStore::new();
        let source = NodeId::new(0, 0);
        let target = NodeId::new(1, 0);
        let file = FileId::new(0);

        // Add an edge (goes into delta on both stores)
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

        // Verify edges are in delta, no CSR
        assert!(store.forward().csr().is_none());
        assert!(store.reverse().csr().is_none());
        assert!(store.stats().forward.delta_edge_count > 0);
        assert!(store.stats().reverse.delta_edge_count > 0);

        // Build real CSRs from the current delta state
        let node_count = 2;
        let fwd_snap = snapshot_edges(&store.forward(), node_count);
        let (forward_csr, _) = build_compacted_csr(&fwd_snap, Direction::Forward).unwrap();
        let rev_snap = snapshot_edges(&store.reverse(), node_count);
        let (reverse_csr, _) = build_compacted_csr(&rev_snap, Direction::Reverse).unwrap();

        // Swap and clear
        store.swap_csrs_and_clear_deltas(forward_csr, reverse_csr);

        // Both stores now have CSR and empty deltas
        assert!(store.forward().csr().is_some());
        assert!(store.reverse().csr().is_some());
        assert_eq!(store.stats().forward.delta_edge_count, 0);
        assert_eq!(store.stats().reverse.delta_edge_count, 0);

        // Reverse traversal still works through CSR after swap
        let reverse_edges = store.edges_to(target);
        assert!(!reverse_edges.is_empty(), "edges_to must return callers");
        let has_caller = reverse_edges
            .iter()
            .any(|e| e.source == source && matches!(e.kind, EdgeKind::Calls { .. }));
        assert!(has_caller, "Reverse traversal must find source as caller");
    }
}

/// Custom serialization for `RwLock<EdgeStore>`.
mod rwlock_edge_store_serde {
    use parking_lot::RwLock;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::EdgeStore;

    pub fn serialize<S>(value: &RwLock<EdgeStore>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.read().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<RwLock<EdgeStore>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let store = EdgeStore::deserialize(deserializer)?;
        Ok(RwLock::new(store))
    }
}
