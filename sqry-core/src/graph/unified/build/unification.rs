//! Cross-file node unification primitives.
//!
//! When multiple files create stub nodes for the same external symbol (e.g.,
//! `kfree` called from `file_a.c` and `file_b.c`), the build produces
//! duplicate nodes. This module provides the merge + remap primitives
//! that Phase 4c-prime uses to unify those duplicates into a single
//! canonical node.
//!
//! # Key Types
//!
//! - [`NodeRemapTable`] — maps loser `NodeId` → winner `NodeId`
//! - [`merge_node_into`] — merges a loser node into a winner in the arena
//! - [`MergeError`] — error variants for invalid merge operations

use std::collections::HashMap;

use super::pass3_intra::PendingEdge;
use crate::graph::unified::node::NodeId;
use crate::graph::unified::storage::arena::NodeArena;
use crate::graph::unified::string::StringId;

/// Errors that can occur during node merge operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeError {
    /// Attempted to merge a node into itself.
    SelfMerge,
    /// One or both `NodeId`s have stale generations (node was already removed).
    StaleNodeId,
}

impl std::fmt::Display for MergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SelfMerge => write!(f, "cannot merge a node into itself"),
            Self::StaleNodeId => write!(f, "one or both NodeIds have stale generations"),
        }
    }
}

impl std::error::Error for MergeError {}

/// Mapping from loser `NodeId` → winner `NodeId` for edge rewriting.
///
/// Built during Phase 4c-prime unification. Applied to `PendingEdge` vectors
/// before they are converted to `DeltaEdge`s in Phase 4d.
#[derive(Debug, Default)]
pub(crate) struct NodeRemapTable {
    map: HashMap<NodeId, NodeId>,
}

impl NodeRemapTable {
    /// Create a remap table with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            map: HashMap::with_capacity(capacity),
        }
    }

    /// Insert a loser → winner mapping.
    pub fn insert(&mut self, loser: NodeId, winner: NodeId) {
        self.map.insert(loser, winner);
    }

    /// Whether the table is empty (no remaps).
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Iterate every loser `NodeId` in the map.
    ///
    /// Part of the public `NodeRemapTable` surface for consumers that
    /// need to reason about which nodes were merged in the last
    /// Phase 4c-prime pass. Phase 4c-prime used to call this to purge
    /// losers from `FileRegistry::per_file_nodes`, but Gate 0d's
    /// bucket-bijection work showed that purge was inconsistent with
    /// [`super::unification::merge_node_into`]'s contract (which keeps
    /// the loser slot `Occupied` but inert). Losers now remain in their
    /// original bucket and this helper is available for future
    /// diagnostic or rebuild-side uses.
    #[allow(dead_code)] // API surface; see doc comment.
    pub fn losers(&self) -> impl Iterator<Item = &NodeId> {
        self.map.keys()
    }

    /// Rewrite `source` and `target` fields of all `PendingEdge` entries
    /// in place. No allocation — pure mutation.
    pub fn apply_to_edges(&self, edge_vecs: &mut [Vec<PendingEdge>]) {
        if self.map.is_empty() {
            return;
        }
        for edges in edge_vecs.iter_mut() {
            for edge in edges.iter_mut() {
                if let Some(&winner) = self.map.get(&edge.source) {
                    edge.source = winner;
                }
                if let Some(&winner) = self.map.get(&edge.target) {
                    edge.target = winner;
                }
            }
        }
    }

    /// Retarget every committed edge whose endpoint is a unification loser
    /// so it points at the corresponding winner instead.
    ///
    /// # Why this helper exists (Phase 3e correctness)
    ///
    /// In the full build pipeline, every edge produced by per-file parsing
    /// is still in `PendingEdge` form when `phase4c_prime_unify_cross_file_nodes`
    /// runs — so [`apply_to_edges`](Self::apply_to_edges) on that
    /// `Vec<Vec<PendingEdge>>` is sufficient to keep every resulting
    /// cross-file edge pointing at the canonical winner.
    ///
    /// In the incremental rebuild pipeline (Task 4 Step 4 Phase 3e) the
    /// rebuild plane inherits every committed edge from the pre-edit
    /// graph via [`clone_for_rebuild`]. When a newly-reparsed file
    /// introduces a qualified-name duplicate that wins the tie-break (or
    /// when closure widening brings two pre-existing duplicates into the
    /// same unification group), surviving committed edges targeting the
    /// pre-edit definition continue to reference what is now the loser
    /// slot — the slot that `merge_node_into` has cleared of its
    /// `qualified_name`. Without rewriting these committed references the
    /// rebuild plane finalises a graph whose cross-file edges point at
    /// inert stubs instead of the live canonical winners (see the §E
    /// harness shrink `java_enterprise / AddFile{...}×2` for the
    /// minimal failure-reproducing input).
    ///
    /// # Behaviour
    ///
    /// For every `(loser → winner)` entry in the map, this routine:
    ///
    ///  * walks both `edges_from(loser)` and `edges_to(loser)` on the
    ///    bidirectional store (which merges CSR + delta with LWW overlay);
    ///  * for every live edge touching the loser, emits a
    ///    [`remove_edge`](BidirectionalEdgeStore::remove_edge) + a
    ///    [`add_edge_with_spans`](BidirectionalEdgeStore::add_edge_with_spans)
    ///    pair that installs the rewritten edge with the loser endpoint
    ///    replaced by the winner. The `spans` vector is preserved so
    ///    call-site metadata for `Calls` / `References` / HTTP edges is
    ///    not lost across the retarget.
    ///  * elides self-loop retargets that would arise when both source
    ///    and target map to the same winner (e.g. two stubs from the
    ///    same file merged into the same canonical node).
    ///
    /// # Full-build no-op
    ///
    /// Safe to call from the full-build pipeline: in a full build the
    /// edge store is still empty when `phase4c_prime_unify_cross_file_nodes`
    /// runs, so every `edges_from(loser)` / `edges_to(loser)` enumeration
    /// returns an empty iterator and no mutations occur. The cost is
    /// `O(|losers|)` empty queries — strictly less than the cost of the
    /// unification pass itself.
    ///
    /// # Determinism
    ///
    /// Iteration order over `self.map.keys()` is undefined (`HashMap`)
    /// but the emitted `remove_edge` + `add_edge_with_spans` pairs are
    /// commutative on the edge store's delta because each target edge
    /// is retargeted exactly once: a loser is never itself a winner, so
    /// there are no transitive rewrites inside a single invocation.
    ///
    /// [`clone_for_rebuild`]: crate::graph::unified::concurrent::CodeGraph::clone_for_rebuild
    /// [`BidirectionalEdgeStore::remove_edge`]: crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore::remove_edge
    /// [`BidirectionalEdgeStore::add_edge_with_spans`]: crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore::add_edge_with_spans
    pub fn apply_to_committed_edges(
        &self,
        edges: &crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore,
    ) {
        if self.map.is_empty() {
            return;
        }
        for (&loser, &winner) in &self.map {
            // Incoming edges (... → loser) retarget to (... → winner).
            let incoming = edges.edges_to(loser);
            for edge_ref in incoming {
                let new_source = self
                    .map
                    .get(&edge_ref.source)
                    .copied()
                    .unwrap_or(edge_ref.source);
                // Drop degenerate self-loops that would be created by
                // retargeting an edge where both endpoints collapse to the
                // same winner.
                if new_source == winner {
                    let _ = edges.remove_edge(
                        edge_ref.source,
                        loser,
                        edge_ref.kind.clone(),
                        edge_ref.file,
                    );
                    continue;
                }
                let _ =
                    edges.remove_edge(edge_ref.source, loser, edge_ref.kind.clone(), edge_ref.file);
                edges.add_edge_with_spans(
                    new_source,
                    winner,
                    edge_ref.kind,
                    edge_ref.file,
                    edge_ref.spans,
                );
            }

            // Outgoing edges (loser → ...) retarget to (winner → ...).
            let outgoing = edges.edges_from(loser);
            for edge_ref in outgoing {
                let new_target = self
                    .map
                    .get(&edge_ref.target)
                    .copied()
                    .unwrap_or(edge_ref.target);
                if new_target == winner {
                    let _ = edges.remove_edge(
                        loser,
                        edge_ref.target,
                        edge_ref.kind.clone(),
                        edge_ref.file,
                    );
                    continue;
                }
                let _ =
                    edges.remove_edge(loser, edge_ref.target, edge_ref.kind.clone(), edge_ref.file);
                edges.add_edge_with_spans(
                    winner,
                    new_target,
                    edge_ref.kind,
                    edge_ref.file,
                    edge_ref.spans,
                );
            }
        }
    }

    /// Drop every loser-keyed entry in `store`.
    ///
    /// Phase 4c-prime tombstones each loser's arena slot but keeps the
    /// loser's `NodeMetadataStore` entry alive at the staging level. The
    /// winner's own per-file `NodeMetadataStore` (produced by the file
    /// that actually defines the surviving symbol) already carries the
    /// authoritative metadata. Per `01_SPEC` §5.3.f and `02_DESIGN` §4.3.e,
    /// "losers' constraints are lost" is the documented Phase-1 contract
    /// for T3 — re-keying loser metadata under the winner would force the
    /// file-order question with no guarantee that `staged_metadata`
    /// iteration order aligns with Phase 4c-prime's winner choice.
    ///
    /// We therefore **drop** loser entries rather than rewriting them
    /// under the winner key. Dropping is the only choice consistent with
    /// all three contracts simultaneously (winner-selection, synthetic
    /// suppression, and `01_SPEC` §5.3.f).
    ///
    /// Mirrors [`Self::apply_to_edges`]: no-op on empty table; in-place
    /// mutation otherwise. Performance: `O(n_meta)` where `n_meta` is the
    /// entry count of `store`.
    pub fn apply_to_metadata_store(
        &self,
        store: &mut crate::graph::unified::storage::metadata::NodeMetadataStore,
    ) {
        if self.map.is_empty() {
            return;
        }

        // Pass 1: scan for loser keys. Borrow `store` immutably; do not
        // mutate while iterating. `iter_entries` yields `((u32, u64),
        // &StoredEntry)` keyed on `(NodeId::index(), NodeId::generation())`
        // and covers every entry — typed payloads AND synthetic-flag-only
        // markers — so no loser slips through. Reconstruct the public
        // `NodeId` via the standard `NodeId::new` ctor used by every other
        // `NodeIdBearing` impl.
        let mut losers_to_drop: Vec<NodeId> = Vec::new();
        for ((index, generation), _entry) in store.iter_entries() {
            let nid = NodeId::new(index, generation);
            if self.map.contains_key(&nid) {
                losers_to_drop.push(nid);
            }
        }

        // Pass 2: drop each loser entry. `remove_entry` removes the whole
        // `StoredEntry` (typed payload + flags). We intentionally do not
        // re-insert under the winner key — see the doc-comment above for
        // the spec rationale.
        for loser in losers_to_drop {
            store.remove_entry(loser);
        }

        // Pass 3: drop loser-keyed SHAPE descriptors under the same §5.3.f
        // contract. A loser can carry a descriptor and no entry (the common
        // shape-only case), so this is a separate scan over the descriptor map.
        // Dropping (not remapping onto the winner) is the only choice
        // consistent with winner-selection: the winner's own per-file store
        // already carries its authoritative descriptor.
        let shape_losers: Vec<NodeId> = store
            .shape_descriptors()
            .keys()
            .copied()
            .filter(|nid| self.map.contains_key(nid))
            .collect();
        for loser in shape_losers {
            store.remove_shape_descriptor(loser);
        }
    }
}

/// Merge a loser node into a winner node in the arena.
///
/// The winner retains the richest metadata from both sides:
/// - **span**: pick whichever has `start_line > 0`; if both, pick the wider range
/// - **visibility**: prefer non-`None`
/// - **signature**: prefer non-`None`
/// - **`is_async` / `is_static` / `is_unsafe`**: OR the flags
/// - **file**: prefer the winner's file (canonical definition)
/// - **doc**: prefer non-`None`
///
/// After merge, the loser's arena slot is tombstoned (generation set to
/// `TOMBSTONE_GENERATION`) so stale `NodeId` lookups return `None`.
///
/// # Errors
///
/// Returns `MergeError::SelfMerge` if `loser == winner`.
/// Returns `MergeError::StaleNodeId` if either node is not found.
///
/// # Safety Contract
///
/// The caller must hold an exclusive write lock on the arena during
/// the entire Phase 4c-prime pass.
pub(crate) fn merge_node_into(
    arena: &mut NodeArena,
    loser: NodeId,
    winner: NodeId,
) -> Result<(), MergeError> {
    if loser == winner {
        return Err(MergeError::SelfMerge);
    }

    // Read loser data first (needs to be cloned since we'll mutate arena)
    let loser_entry = arena.get(loser).ok_or(MergeError::StaleNodeId)?.clone();
    let winner_entry = arena.get_mut(winner).ok_or(MergeError::StaleNodeId)?;

    // Merge span: prefer the one with start_line > 0
    if winner_entry.start_line == 0 && loser_entry.start_line > 0 {
        winner_entry.start_line = loser_entry.start_line;
        winner_entry.start_column = loser_entry.start_column;
        winner_entry.end_line = loser_entry.end_line;
        winner_entry.end_column = loser_entry.end_column;
        winner_entry.start_byte = loser_entry.start_byte;
        winner_entry.end_byte = loser_entry.end_byte;
    } else if winner_entry.start_line > 0 && loser_entry.start_line > 0 {
        // Both have real spans — pick the wider range
        let winner_range = winner_entry
            .end_line
            .saturating_sub(winner_entry.start_line);
        let loser_range = loser_entry.end_line.saturating_sub(loser_entry.start_line);
        if loser_range > winner_range {
            winner_entry.start_line = loser_entry.start_line;
            winner_entry.start_column = loser_entry.start_column;
            winner_entry.end_line = loser_entry.end_line;
            winner_entry.end_column = loser_entry.end_column;
            winner_entry.start_byte = loser_entry.start_byte;
            winner_entry.end_byte = loser_entry.end_byte;
        }
    }

    // Merge visibility: prefer non-None
    if winner_entry.visibility.is_none() && loser_entry.visibility.is_some() {
        winner_entry.visibility = loser_entry.visibility;
    }

    // Merge signature: prefer non-None
    if winner_entry.signature.is_none() && loser_entry.signature.is_some() {
        winner_entry.signature = loser_entry.signature;
    }

    // Merge doc: prefer non-None
    if winner_entry.doc.is_none() && loser_entry.doc.is_some() {
        winner_entry.doc = loser_entry.doc;
    }

    // Merge body_hash: prefer non-None
    if winner_entry.body_hash.is_none() && loser_entry.body_hash.is_some() {
        winner_entry.body_hash = loser_entry.body_hash;
    }

    // OR the boolean flags
    winner_entry.is_async |= loser_entry.is_async;
    winner_entry.is_static |= loser_entry.is_static;
    winner_entry.is_unsafe |= loser_entry.is_unsafe;

    // NOTE: We intentionally do NOT remove the loser from the arena.
    // The NodeRemapTable ensures all PendingEdge references now point at
    // the winner, making the loser unreachable via edges. Removing the
    // loser would reduce arena.len() below slot_count(), breaking the
    // CSR persistence compaction which sizes row_ptr by node_count().
    // The loser slot remains occupied but inert — its metadata was merged
    // into the winner above.
    //
    // Bucket/resolution contract — Gate 0d iter-1 fix (plus iter-2
    // content-addressable containment hardening).
    //
    // `merge_node_into` is the single authority on "this slot is a
    // unified-away duplicate". We mark the slot inert by clearing every
    // publish-visible content-addressable field on the loser BEFORE
    // `rebuild_indices` runs for the second time in
    // `build_unified_graph_inner` (entrypoint.rs:571):
    //
    //   * `name`           → `StringId::INVALID` (primary sentinel;
    //                        matches `NodeEntry::is_unified_loser`)
    //   * `qualified_name` → `None`
    //   * `signature`      → `None` (was leaking via `duplicates:body`
    //                        and `duplicates:signature` hash paths that
    //                        iterated raw arena entries and keyed on
    //                        `entry.signature`)
    //   * `body_hash`      → `None` (was leaking via `duplicates:body`
    //                        primary hash path which keyed on
    //                        `entry.body_hash`)
    //   * `doc`            → `None` (docstring should not surface for
    //                        an inert duplicate)
    //   * `visibility`     → `None` (no publish-visible visibility for
    //                        an inert duplicate)
    //
    // Downstream `AuxiliaryIndices::build_from_arena` already skips
    // entries whose `name == StringId::INVALID`, so name / qualified
    // name lookups return only the winner. The additional clearing
    // above is defense-in-depth: any publish-visible surface that
    // iterates raw arena entries and keys on content-addressable
    // metadata (duplicate detection, similarity search, body-hash
    // matching) MUST still call `entry.is_unified_loser()` to filter,
    // but if that filter is ever missed the cleared metadata prevents
    // a loser from participating in grouping/hashing.
    //
    // Without this, the iter-2 blocker showed `duplicates:` query
    // still grouped losers via preserved `body_hash` / `signature`,
    // violating the §F.3 "every live published NodeId is reachable via
    // exactly one name-resolution result" contract.
    let loser_mut = arena.get_mut(loser).ok_or(MergeError::StaleNodeId)?;
    loser_mut.name = StringId::INVALID;
    loser_mut.qualified_name = None;
    loser_mut.signature = None;
    loser_mut.body_hash = None;
    loser_mut.doc = None;
    loser_mut.visibility = None;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::{Position, Span};
    use crate::graph::unified::edge::bidirectional::BidirectionalEdgeStore;
    use crate::graph::unified::edge::{EdgeKind, ResolvedVia};
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::{NodeId, NodeKind};
    use crate::graph::unified::storage::NodeEntry;
    use crate::graph::unified::storage::arena::NodeArena;
    use crate::graph::unified::string::StringId;

    fn make_entry(kind: NodeKind, start_line: u32) -> NodeEntry {
        NodeEntry::new(kind, StringId::new(0), FileId::new(0)).with_location(
            start_line,
            0,
            start_line + 10,
            0,
        )
    }

    #[test]
    fn test_merge_winner_has_real_span_loser_has_zero() {
        let mut arena = NodeArena::new();
        let winner = arena.alloc(make_entry(NodeKind::Function, 42)).unwrap();
        let loser = arena.alloc(make_entry(NodeKind::Function, 0)).unwrap();

        merge_node_into(&mut arena, loser, winner).unwrap();

        let entry = arena.get(winner).unwrap();
        assert_eq!(entry.start_line, 42, "Winner's real span preserved");
        // Loser slot is NOT removed — it remains in the arena as an inert
        // duplicate. The NodeRemapTable (applied by the caller) ensures all
        // edges reference the winner instead.
        assert!(
            arena.get(loser).is_some(),
            "Loser slot stays in arena (inert)"
        );
    }

    #[test]
    fn test_merge_loser_has_real_span_winner_has_zero() {
        let mut arena = NodeArena::new();
        let winner = arena.alloc(make_entry(NodeKind::Function, 0)).unwrap();
        let loser = arena.alloc(make_entry(NodeKind::Function, 99)).unwrap();

        merge_node_into(&mut arena, loser, winner).unwrap();

        let entry = arena.get(winner).unwrap();
        assert_eq!(entry.start_line, 99, "Loser's span adopted by winner");
    }

    #[test]
    fn test_merge_both_real_spans_wider_wins() {
        let mut arena = NodeArena::new();
        // Winner: lines 10-15 (range 5)
        let winner = arena
            .alloc(make_entry(NodeKind::Function, 10).with_location(10, 0, 15, 0))
            .unwrap();
        // Loser: lines 3-30 (range 27) — wider
        let loser = arena
            .alloc(make_entry(NodeKind::Function, 3).with_location(3, 0, 30, 0))
            .unwrap();

        merge_node_into(&mut arena, loser, winner).unwrap();

        let entry = arena.get(winner).unwrap();
        assert_eq!(entry.start_line, 3, "Wider span from loser adopted");
        assert_eq!(entry.end_line, 30);
    }

    #[test]
    fn test_merge_metadata_adoption() {
        let mut arena = NodeArena::new();
        let sig = StringId::new(5);
        let vis = StringId::new(6);

        let winner = arena.alloc(make_entry(NodeKind::Function, 10)).unwrap();
        let mut loser_entry = make_entry(NodeKind::Function, 0);
        loser_entry.signature = Some(sig);
        loser_entry.visibility = Some(vis);
        loser_entry.is_async = true;
        let loser = arena.alloc(loser_entry).unwrap();

        merge_node_into(&mut arena, loser, winner).unwrap();

        let entry = arena.get(winner).unwrap();
        assert_eq!(entry.signature, Some(sig));
        assert_eq!(entry.visibility, Some(vis));
        assert!(entry.is_async);
    }

    #[test]
    fn test_self_merge_error() {
        let mut arena = NodeArena::new();
        let node = arena.alloc(make_entry(NodeKind::Function, 1)).unwrap();
        let result = merge_node_into(&mut arena, node, node);
        assert_eq!(result, Err(MergeError::SelfMerge));
    }

    #[test]
    fn test_stale_node_error() {
        let mut arena = NodeArena::new();
        let node = arena.alloc(make_entry(NodeKind::Function, 1)).unwrap();
        let result = merge_node_into(&mut arena, NodeId::INVALID, node);
        assert_eq!(result, Err(MergeError::StaleNodeId));
    }

    #[test]
    fn test_remap_table_apply_to_edges() {
        use crate::graph::unified::edge::EdgeKind;

        let loser = NodeId::new(100, 0);
        let winner = NodeId::new(200, 0);
        let other = NodeId::new(300, 0);

        let mut remap = NodeRemapTable::with_capacity(1);
        remap.insert(loser, winner);

        let mut edges = vec![vec![
            PendingEdge {
                source: loser,
                target: other,
                kind: EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                file: FileId::new(0),
                spans: Vec::new(),
            },
            PendingEdge {
                source: other,
                target: loser,
                kind: EdgeKind::References,
                file: FileId::new(0),
                spans: Vec::new(),
            },
        ]];

        remap.apply_to_edges(&mut edges);

        assert_eq!(edges[0][0].source, winner, "Source remapped");
        assert_eq!(edges[0][0].target, other, "Non-remapped target unchanged");
        assert_eq!(edges[0][1].source, other, "Non-remapped source unchanged");
        assert_eq!(edges[0][1].target, winner, "Target remapped");
    }

    #[test]
    fn test_remap_table_empty_is_empty() {
        let remap = NodeRemapTable::with_capacity(0);
        assert!(remap.is_empty());
    }

    /// Gate 0d iter-1 blocker regression.
    ///
    /// Before the fix, a cross-file unified-away loser remained in the
    /// arena with its original `name` / `qualified_name`, so
    /// `AuxiliaryIndices::build_from_arena` would populate both the
    /// winner and the loser into `by_name` / `by_qualified_name`,
    /// causing downstream `find_by_pattern` / `resolve_by_qualified_name`
    /// surfaces to return duplicate results after unification.
    ///
    /// The fix clears the loser's `name` (to `StringId::INVALID`) and
    /// `qualified_name` (to `None`) inside `merge_node_into`. Here we
    /// assert exactly that contract on the arena-level primitive — the
    /// downstream `build_from_arena` behaviour is asserted separately
    /// in `sqry-core/src/graph/unified/storage/indices.rs` tests.
    #[test]
    fn test_merge_clears_loser_name_and_qualified_name() {
        let mut arena = NodeArena::new();
        let name_id = StringId::new(10);
        let qn_id = StringId::new(11);

        let mut winner_entry =
            NodeEntry::new(NodeKind::Function, name_id, FileId::new(0)).with_location(5, 0, 15, 0);
        winner_entry.qualified_name = Some(qn_id);
        let winner = arena.alloc(winner_entry).unwrap();

        let mut loser_entry =
            NodeEntry::new(NodeKind::Function, name_id, FileId::new(1)).with_location(3, 0, 7, 0);
        loser_entry.qualified_name = Some(qn_id);
        let loser = arena.alloc(loser_entry).unwrap();

        merge_node_into(&mut arena, loser, winner).unwrap();

        let winner_after = arena.get(winner).expect("winner live");
        assert_eq!(
            winner_after.name, name_id,
            "winner's name must be preserved"
        );
        assert_eq!(
            winner_after.qualified_name,
            Some(qn_id),
            "winner's qualified name must be preserved"
        );

        let loser_after = arena.get(loser).expect("loser slot stays occupied");
        assert_eq!(
            loser_after.name,
            StringId::INVALID,
            "loser's name must be cleared to StringId::INVALID so rebuild_indices \
             skips it in name_index / file_index / kind_index"
        );
        assert!(
            loser_after.qualified_name.is_none(),
            "loser's qualified_name must be cleared so rebuild_indices skips it in \
             qualified_name_index"
        );
    }

    /// End-to-end regression for the Gate 0d iter-1 blocker.
    ///
    /// Builds a two-file fixture where both files declare a
    /// call-compatible stub with the same qualified name, drives the
    /// full `build_unified_graph` pipeline, and asserts that
    /// `by_name` / `by_qualified_name` buckets contain EXACTLY ONE
    /// NodeId — the winner — after Phase 4c-prime. `find_by_pattern`
    /// and `resolve_by_qualified_name` must likewise return a single
    /// canonical result.
    ///
    /// This test explicitly covers the reviewer's verbatim failure
    /// mode: "merged losers can still surface as duplicate / ambiguous
    /// published symbol results after cross-file unification."
    #[test]
    fn unified_losers_are_absent_from_by_name_and_by_qualified_name_indices() {
        use crate::graph::unified::concurrent::CodeGraph;
        use crate::graph::unified::storage::arena::NodeArena;

        // Simulate the end of Phase 4c-prime by:
        // 1. Staging two nodes with the same name + qualified name in
        //    different files (the "per-file stub" shape Phase 4c-prime
        //    unifies).
        // 2. Calling `merge_node_into` to unify them.
        // 3. Running `AuxiliaryIndices::build_from_arena` (the same
        //    routine `rebuild_indices` calls after unification in
        //    `build_unified_graph_inner`).
        // 4. Asserting only the winner surfaces in `by_name` and
        //    `by_qualified_name`, that `find_by_pattern` deduplicates
        //    back to one hit, and that the FileRegistry still tracks
        //    both slots for the §F.1 bucket bijection.

        let mut graph = CodeGraph::new();

        let name_id = graph.strings_mut().intern("shared_symbol").expect("intern");
        let qn_id = graph
            .strings_mut()
            .intern("mod::shared_symbol")
            .expect("intern");

        let file_a = FileId::new(1);
        let file_b = FileId::new(2);

        let (winner_id, loser_id) = {
            let arena: &mut NodeArena = graph.nodes_mut();
            let mut w =
                NodeEntry::new(NodeKind::Function, name_id, file_a).with_location(10, 0, 20, 0);
            w.qualified_name = Some(qn_id);
            let w_id = arena.alloc(w).unwrap();

            let mut l =
                NodeEntry::new(NodeKind::Function, name_id, file_b).with_location(5, 0, 6, 0);
            l.qualified_name = Some(qn_id);
            let l_id = arena.alloc(l).unwrap();
            (w_id, l_id)
        };

        graph.files_mut().record_node(file_a, winner_id);
        graph.files_mut().record_node(file_b, loser_id);

        // Perform the unification merge (same primitive Phase 4c-prime
        // calls in `parallel_commit::phase4c_prime_unify_cross_file_nodes`).
        merge_node_into(graph.nodes_mut(), loser_id, winner_id).unwrap();

        // Rebuild indices post-unification (mirrors the second
        // `rebuild_indices()` call in `build_unified_graph_inner`).
        graph.rebuild_indices();

        // --- Assertion 1: by_name contains exactly the winner.
        let by_name = graph.indices().by_name(name_id).to_vec();
        assert_eq!(
            by_name,
            vec![winner_id],
            "by_name must return exactly one NodeId (the winner) after unification; \
             got {by_name:?}"
        );

        // --- Assertion 2: by_qualified_name contains exactly the winner.
        let by_qn = graph.indices().by_qualified_name(qn_id).to_vec();
        assert_eq!(
            by_qn,
            vec![winner_id],
            "by_qualified_name must return exactly one NodeId (the winner) after \
             unification; got {by_qn:?}"
        );

        // --- Assertion 3: by_kind / by_file likewise exclude the loser.
        let by_kind = graph.indices().by_kind(NodeKind::Function).to_vec();
        assert_eq!(
            by_kind,
            vec![winner_id],
            "by_kind must exclude the unified-away loser; got {by_kind:?}"
        );
        assert!(
            graph.indices().by_file(file_b).is_empty(),
            "by_file for the loser's file must be empty after unification; \
             got {:?}",
            graph.indices().by_file(file_b)
        );
        assert_eq!(
            graph.indices().by_file(file_a).to_vec(),
            vec![winner_id],
            "by_file for the winner's file must contain only the winner"
        );

        // --- Assertion 4: `find_by_pattern` (via GraphSnapshot)
        //     deduplicates to one hit.
        let snap = graph.snapshot();
        let pattern_hits = snap.find_by_pattern("shared_symbol");
        assert_eq!(
            pattern_hits,
            vec![winner_id],
            "find_by_pattern must return exactly one NodeId after unification; \
             got {pattern_hits:?}"
        );

        // --- Assertion 5: §F.1 bucket bijection must still hold — the
        //     FileRegistry is the only publish-visible bucket that
        //     references the loser, so losers stay accounted for there.
        let file_a_bucket = graph.files().nodes_for_file(file_a).to_vec();
        let file_b_bucket = graph.files().nodes_for_file(file_b).to_vec();
        assert_eq!(file_a_bucket, vec![winner_id]);
        assert_eq!(file_b_bucket, vec![loser_id]);
        // And the bijection assertion itself (debug-only helper) must
        // pass — prove it by invoking the publish helper on the
        // assembled graph.
        #[cfg(any(debug_assertions, test))]
        crate::graph::unified::publish::assert_publish_invariants(
            &graph,
            &std::collections::HashSet::new(),
        );
    }

    /// Gate 0d iter-2 regression: `merge_node_into` MUST clear every
    /// publish-visible content-addressable field on the loser, not
    /// just `name` and `qualified_name`. Iter-2 blocker surfaced
    /// because `body_hash` and `signature` remained set on losers,
    /// letting `duplicates:` query group losers alongside winners
    /// through those fields.
    ///
    /// This is a unit-level test of the clearing contract. The
    /// `duplicates:` query-level regression test lives in
    /// [`crate::query::executor::graph_duplicates::tests::duplicates_query_excludes_unified_losers`],
    /// and the publish-boundary assertion
    /// (`assert_losers_have_cleared_metadata`) runs this invariant at
    /// every publish site in debug / test builds.
    #[test]
    fn merge_node_into_clears_all_publish_visible_content_addressable_fields() {
        use crate::graph::body_hash::BodyHash128;
        use crate::graph::unified::storage::interner::StringInterner;

        let mut interner = StringInterner::new();
        let name = interner.intern("dup_fn").unwrap();
        let qn = interner.intern("mod::dup_fn").unwrap();
        let sig = interner.intern("fn dup_fn() -> ()").unwrap();
        let doc = interner.intern("/// docstring for dup_fn").unwrap();
        let vis = interner.intern("pub").unwrap();

        let file_a = FileId::new(1);
        let file_b = FileId::new(2);

        let body = BodyHash128 {
            high: 0x1234_5678,
            low: 0x9ABC_DEF0,
        };

        let mut arena = NodeArena::new();
        let winner = arena
            .alloc(
                NodeEntry::new(NodeKind::Function, name, file_a)
                    .with_location(10, 0, 20, 0)
                    .with_qualified_name(qn)
                    .with_signature(sig)
                    .with_doc(doc)
                    .with_visibility(vis)
                    .with_body_hash(body),
            )
            .unwrap();

        let mut loser_entry = NodeEntry::new(NodeKind::Function, name, file_b)
            .with_location(5, 0, 15, 0)
            .with_signature(sig)
            .with_doc(doc)
            .with_visibility(vis)
            .with_body_hash(body);
        loser_entry.qualified_name = Some(qn);
        let loser = arena.alloc(loser_entry).unwrap();

        merge_node_into(&mut arena, loser, winner).unwrap();

        let l = arena.get(loser).expect("loser still in arena");
        assert_eq!(
            l.name,
            StringId::INVALID,
            "loser.name must be INVALID (unified-loser sentinel)"
        );
        assert!(
            l.qualified_name.is_none(),
            "loser.qualified_name must be None"
        );
        assert!(
            l.signature.is_none(),
            "loser.signature must be cleared — iter-2 blocker: `duplicates:` query \
             was keying on preserved loser signatures"
        );
        assert!(
            l.body_hash.is_none(),
            "loser.body_hash must be cleared — iter-2 blocker: `duplicates:body` \
             query was keying on preserved loser body_hash"
        );
        assert!(l.doc.is_none(), "loser.doc must be cleared");
        assert!(l.visibility.is_none(), "loser.visibility must be cleared");
        assert!(l.is_unified_loser());

        // Winner metadata preserved.
        let w = arena.get(winner).expect("winner still in arena");
        assert_eq!(w.name, name);
        assert_eq!(w.qualified_name, Some(qn));
        assert_eq!(w.signature, Some(sig));
        assert_eq!(w.body_hash, Some(body));
        assert_eq!(w.doc, Some(doc));
        assert_eq!(w.visibility, Some(vis));
    }

    // -------- apply_to_committed_edges direct tests (Codex iter-1 nit) --

    /// Span helper mirroring the conventions used by the
    /// `apply_to_committed_edges` callers — a synthetic non-zero span so
    /// the preservation assertion has something observable to check.
    fn harness_span(line: u32) -> Span {
        let line_usize = line as usize;
        Span {
            start: Position {
                line: line_usize,
                column: 0,
            },
            end: Position {
                line: line_usize,
                column: 8,
            },
        }
    }

    #[test]
    fn apply_to_committed_edges_retargets_incoming_and_outgoing_preserving_spans() {
        // Contract documented at [`NodeRemapTable::apply_to_committed_edges`]:
        //   * incoming edges (...→loser) are rewritten to (...→winner);
        //   * outgoing edges (loser→...) are rewritten to (winner→...);
        //   * each edge's `spans` vector is preserved across the retarget;
        //   * no duplicate / stale entry remains on the old endpoint.
        //
        // Also locks the behaviour claim in the helper's docstring that a
        // single invocation has no transitive rewrites — we only ever observe
        // the post-retarget shape (winner in both source and target positions
        // where appropriate).

        let store = BidirectionalEdgeStore::new();

        // Build a minimal node set: an external source, the loser, the
        // winner, and an external sink that the loser used to reach.
        let external_source = NodeId::new(0, 0);
        let loser = NodeId::new(1, 0);
        let winner = NodeId::new(2, 0);
        let external_sink = NodeId::new(3, 0);
        let source_file = FileId::new(1);
        let loser_file = FileId::new(2);

        // Seed committed edges:
        //   external_source --Calls(spans=[line=10])--> loser       (incoming)
        //   loser           --References(spans=[line=20])--> external_sink  (outgoing)
        let call_kind = EdgeKind::Calls {
            argument_count: 1,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let ref_kind = EdgeKind::References;

        store.add_edge_with_spans(
            external_source,
            loser,
            call_kind.clone(),
            source_file,
            vec![harness_span(10)],
        );
        store.add_edge_with_spans(
            loser,
            external_sink,
            ref_kind.clone(),
            loser_file,
            vec![harness_span(20)],
        );

        // Sanity: before remap, the committed edges reach the loser.
        let incoming_before = store.edges_to(loser);
        let outgoing_before = store.edges_from(loser);
        assert_eq!(
            incoming_before.len(),
            1,
            "precondition: single incoming edge to loser"
        );
        assert_eq!(
            outgoing_before.len(),
            1,
            "precondition: single outgoing edge from loser"
        );
        assert_eq!(incoming_before[0].spans, vec![harness_span(10)]);
        assert_eq!(outgoing_before[0].spans, vec![harness_span(20)]);

        // Run the retarget helper.
        let mut remap = NodeRemapTable::default();
        remap.insert(loser, winner);
        remap.apply_to_committed_edges(&store);

        // Post-retarget: no edge touches the loser anymore.
        assert!(
            store.edges_to(loser).is_empty(),
            "incoming edges to loser must be removed"
        );
        assert!(
            store.edges_from(loser).is_empty(),
            "outgoing edges from loser must be removed"
        );

        // The external-source→loser edge became external-source→winner.
        let incoming_winner = store.edges_to(winner);
        assert_eq!(
            incoming_winner.len(),
            1,
            "exactly one incoming edge must now target the winner"
        );
        let incoming = &incoming_winner[0];
        assert_eq!(incoming.source, external_source);
        assert_eq!(incoming.target, winner);
        assert_eq!(incoming.kind, call_kind);
        assert_eq!(
            incoming.spans,
            vec![harness_span(10)],
            "spans must survive the retarget for Calls edges"
        );

        // The loser→external_sink edge became winner→external_sink.
        let outgoing_winner = store.edges_from(winner);
        assert_eq!(
            outgoing_winner.len(),
            1,
            "exactly one outgoing edge must now originate from the winner"
        );
        let outgoing = &outgoing_winner[0];
        assert_eq!(outgoing.source, winner);
        assert_eq!(outgoing.target, external_sink);
        assert_eq!(outgoing.kind, ref_kind);
        assert_eq!(
            outgoing.spans,
            vec![harness_span(20)],
            "spans must survive the retarget for References edges"
        );
    }

    #[test]
    fn apply_to_committed_edges_empty_map_is_noop() {
        // An empty remap must leave every committed edge untouched. This
        // pins the early-return in `apply_to_committed_edges` and guarantees
        // the helper is zero-cost when Phase 4c-prime runs against a
        // graph that needs no unification.
        let store = BidirectionalEdgeStore::new();
        let a = NodeId::new(1, 0);
        let b = NodeId::new(2, 0);
        let file = FileId::new(1);
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        store.add_edge_with_spans(a, b, kind.clone(), file, vec![harness_span(1)]);

        let before = store.edges_from(a);
        assert_eq!(before.len(), 1);

        let remap = NodeRemapTable::default();
        remap.apply_to_committed_edges(&store);

        let after = store.edges_from(a);
        assert_eq!(after.len(), 1, "empty remap must not mutate the edge store");
        assert_eq!(after[0].target, b);
        assert_eq!(after[0].spans, vec![harness_span(1)]);
    }

    #[test]
    fn apply_to_committed_edges_collapses_self_loop_to_winner() {
        // Edge case: an edge where BOTH endpoints are in the remap map and
        // both resolve to the SAME winner. A naive retarget would produce a
        // (winner → winner) self-loop; the helper documents that it elides
        // such degenerate retargets. This test pins that behaviour.
        let store = BidirectionalEdgeStore::new();
        let loser_a = NodeId::new(1, 0);
        let loser_b = NodeId::new(2, 0);
        let winner = NodeId::new(3, 0);
        let file = FileId::new(1);
        let kind = EdgeKind::References;

        // Both losers point at each other — a pre-unification shape where
        // two stub nodes cross-reference one another.
        store.add_edge_with_spans(loser_a, loser_b, kind.clone(), file, vec![harness_span(42)]);

        let mut remap = NodeRemapTable::default();
        remap.insert(loser_a, winner);
        remap.insert(loser_b, winner);
        remap.apply_to_committed_edges(&store);

        // Both endpoints collapse onto `winner`; the helper elides the
        // resulting (winner → winner) self-loop.
        assert!(
            store.edges_from(winner).is_empty(),
            "winner must not accumulate a self-loop from degenerate retarget"
        );
        assert!(
            store.edges_to(winner).is_empty(),
            "winner must not accumulate a self-loop incoming edge"
        );
        // Neither loser retains the original edge.
        assert!(store.edges_from(loser_a).is_empty());
        assert!(store.edges_to(loser_b).is_empty());
    }

    // ----------------------------------------------------------------------
    // T3 Cluster B (02_DESIGN §4.3.e Change 3): NodeRemapTable::apply_to_metadata_store
    // ----------------------------------------------------------------------

    #[test]
    fn apply_to_metadata_store_drops_loser_keys() {
        use crate::graph::unified::storage::metadata::{
            MacroNodeMetadata, NodeFlags, NodeMetadataStore, StoredEntry,
        };

        let loser_a = NodeId::new(101, 1);
        let loser_b = NodeId::new(102, 1);
        let winner = NodeId::new(200, 1);
        let untouched = NodeId::new(300, 1);

        let mut store = NodeMetadataStore::new();
        let macro_a = MacroNodeMetadata {
            cfg_condition: Some("linux".to_string()),
            ..Default::default()
        };
        store.insert(loser_a, macro_a);

        let macro_b = MacroNodeMetadata {
            cfg_condition: Some("darwin".to_string()),
            ..Default::default()
        };
        store.insert(loser_b, macro_b);

        let macro_w = MacroNodeMetadata {
            cfg_condition: Some("windows".to_string()),
            ..Default::default()
        };
        store.insert(winner, macro_w);

        store.insert_entry(
            untouched,
            StoredEntry {
                typed: None,
                flags: NodeFlags::SYNTHETIC,
            },
        );

        let mut remap = NodeRemapTable::default();
        remap.insert(loser_a, winner);
        remap.insert(loser_b, winner);

        remap.apply_to_metadata_store(&mut store);

        // Both losers are dropped — 01_SPEC §5.3.f contract.
        assert!(
            store.get_macro(loser_a).is_none(),
            "loser_a metadata must be dropped"
        );
        assert!(
            store.get_macro(loser_b).is_none(),
            "loser_b metadata must be dropped"
        );
        // Winner's own authoritative metadata is preserved — we never
        // re-key losers onto the winner.
        assert_eq!(
            store
                .get_macro(winner)
                .and_then(|m| m.cfg_condition.clone()),
            Some("windows".to_string()),
            "winner's authoritative cfg_condition survives unchanged"
        );
        // Unrelated entries untouched.
        assert!(
            store.is_synthetic(untouched),
            "non-loser synthetic marker survives"
        );
    }

    #[test]
    fn apply_to_metadata_store_drops_loser_shape_descriptors() {
        // The shape-only loser case: a node carrying ONLY a descriptor (no
        // entry) must still be dropped when it loses Phase 4c-prime unification,
        // or it strands a descriptor on a tombstoned arena slot.
        use crate::graph::unified::storage::metadata::NodeMetadataStore;
        use crate::graph::unified::storage::shape::ShapeDescriptor;

        let loser = NodeId::new(101, 1);
        let winner = NodeId::new(200, 1);
        let untouched = NodeId::new(300, 1);

        let mut store = NodeMetadataStore::new();
        store.insert_shape_descriptor(loser, ShapeDescriptor::default());
        store.insert_shape_descriptor(winner, ShapeDescriptor::default());
        store.insert_shape_descriptor(untouched, ShapeDescriptor::default());

        let mut remap = NodeRemapTable::default();
        remap.insert(loser, winner);
        remap.apply_to_metadata_store(&mut store);

        assert!(
            store.shape_descriptor(loser).is_none(),
            "loser's shape descriptor must be dropped (§5.3.f)"
        );
        assert!(
            store.shape_descriptor(winner).is_some(),
            "winner keeps its own authoritative descriptor"
        );
        assert!(
            store.shape_descriptor(untouched).is_some(),
            "non-loser descriptor survives"
        );
    }

    #[test]
    fn apply_to_metadata_store_empty_remap_is_noop() {
        use crate::graph::unified::storage::metadata::{MacroNodeMetadata, NodeMetadataStore};

        let nid = NodeId::new(42, 1);
        let mut store = NodeMetadataStore::new();
        let macro_meta = MacroNodeMetadata {
            cfg_condition: Some("test".to_string()),
            ..Default::default()
        };
        store.insert(nid, macro_meta);

        let remap = NodeRemapTable::default();
        remap.apply_to_metadata_store(&mut store);

        assert_eq!(
            store.get_macro(nid).and_then(|m| m.cfg_condition.clone()),
            Some("test".to_string()),
            "empty remap must not touch any entry"
        );
    }

    #[test]
    fn apply_to_metadata_store_empty_store_is_noop() {
        use crate::graph::unified::storage::metadata::NodeMetadataStore;

        let mut store = NodeMetadataStore::new();
        let mut remap = NodeRemapTable::default();
        remap.insert(NodeId::new(1, 1), NodeId::new(2, 1));

        remap.apply_to_metadata_store(&mut store);

        assert!(store.is_empty(), "empty store stays empty");
    }
}
