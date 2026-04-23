//! Incremental re-indexing entrypoint: `reindex_files`.
//!
//! When files change on disk, `reindex_files` tombstones old nodes/edges and
//! parses + commits fresh data at an append-only arena range.
//!
//! # Ordering invariant (Codex H1)
//!
//! Edge tombstoning MUST precede node slot freeing. The CSR edge store indexes
//! edges by `source.index()` and drops generation. Tombstone ordering ensures
//! `build_delta_lww_from_source()` shadows stale CSR entries before the node
//! slots are freed.
//!
//! # Always-append allocation (Codex H1')
//!
//! `alloc_range()` is strictly append-only. Old segments enter the free list
//! for compaction only.

use std::path::PathBuf;

use log::debug;

use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::edge::kind::EdgeKind;
use crate::graph::unified::file::id::FileId;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::storage::segment::FileSegment;

/// Statistics returned by `reindex_files`.
#[derive(Debug, Default, Clone)]
pub struct ReindexStats {
    /// Number of files re-indexed.
    pub files_reindexed: usize,
    /// Number of old nodes tombstoned.
    pub nodes_tombstoned: usize,
    /// Number of old edges tombstoned (delta Remove entries).
    pub edges_tombstoned: usize,
    /// Number of new nodes committed.
    pub nodes_committed: usize,
    /// Number of new edges inserted.
    pub edges_inserted: usize,
    /// Files skipped (not in registry, no segment, or parse failure).
    pub files_skipped: usize,
}

/// Incrementally re-indexes the given files by tombstoning old data and
/// parsing + committing fresh data.
///
/// # Arguments
///
/// * `graph` — Mutable reference to the live `CodeGraph`.
/// * `changed_paths` — Absolute paths of files that changed on disk.
///
/// # Flow per file
///
/// 1. Resolve `FileId` from the file registry.
/// 2. Look up old `FileSegment` from the segment table.
/// 3. **Tombstone old edges** (delta Remove entries for all forward/reverse edges).
/// 4. **Tombstone old nodes** (`arena.remove(id)` for each slot in segment).
/// 5. **Allocate new segment** (append-only `alloc_range`).
/// 6. **Commit new nodes** into the new arena range.
/// 7. **Insert new edges** via `BidirectionalEdgeStore::add_edge`.
/// 8. **Update `FileSegmentTable`** with the new range.
///
/// # Returns
///
/// `ReindexStats` summarizing the work done.
pub fn reindex_files(graph: &mut CodeGraph, changed_paths: &[PathBuf]) -> ReindexStats {
    let mut stats = ReindexStats::default();

    for path in changed_paths {
        // Step 1: Resolve FileId
        let file_id = match graph.files().get(path) {
            Some(fid) => fid,
            None => {
                debug!(
                    "reindex: skipping {} — not in file registry",
                    path.display()
                );
                stats.files_skipped += 1;
                continue;
            }
        };

        // Step 2: Look up old segment
        let old_segment = match graph.file_segments().get(file_id).copied() {
            Some(seg) => seg,
            None => {
                debug!(
                    "reindex: skipping {} (FileId {:?}) — no segment",
                    path.display(),
                    file_id
                );
                stats.files_skipped += 1;
                continue;
            }
        };

        // Step 3: Tombstone old edges (BEFORE nodes — ordering invariant)
        let edges_tombstoned = tombstone_old_edges(graph, &old_segment);

        // Step 4: Tombstone old node slots
        let nodes_tombstoned = tombstone_old_nodes(graph, &old_segment);

        // Step 5-8: Handled by the caller after parsing
        // The actual parsing, node commit, and edge insertion are done by the
        // caller (via the build pipeline or a dedicated re-index path).
        // This function handles the tombstone side; commit is separate because
        // it requires the plugin manager and tree-sitter, which belong in
        // a higher-level orchestrator.

        // Clear the old segment entry
        graph.file_segments_mut().remove(file_id);

        // Drop the file's per_file_nodes bucket — the caller's
        // subsequent parse + commit pass will repopulate it via
        // `FileRegistry::record_node`. Leaving the old bucket in place
        // would leak now-tombstoned NodeIds into the Gate 0c bucket-
        // bijection invariant.
        let _ = graph.files_mut().take_nodes(file_id);

        stats.files_reindexed += 1;
        stats.nodes_tombstoned += nodes_tombstoned;
        stats.edges_tombstoned += edges_tombstoned;
    }

    stats
}

/// Tombstones all edges for nodes in the old segment by calling
/// `remove_edge` on the bidirectional edge store (which appends Remove
/// deltas to both forward and reverse stores and sets CSR tombstones).
fn tombstone_old_edges(graph: &mut CodeGraph, segment: &FileSegment) -> usize {
    let start = segment.start_slot;
    let end = segment.end_slot();
    let mut count = 0usize;

    // Collect edge keys to remove. We must collect first to avoid holding
    // both read and write borrows on the edge store.
    let mut edges_to_remove: Vec<(NodeId, NodeId, EdgeKind, FileId)> = Vec::new();

    for idx in start..end {
        if let Some(slot) = graph.nodes().slot(idx)
            && slot.is_occupied()
        {
            let node_id = NodeId::new(idx, slot.generation());

            // Forward edges from this node
            for edge_ref in &graph.edges().edges_from(node_id) {
                edges_to_remove.push((
                    edge_ref.source,
                    edge_ref.target,
                    edge_ref.kind.clone(),
                    edge_ref.file,
                ));
            }

            // Reverse edges to this node
            for edge_ref in &graph.edges().edges_to(node_id) {
                edges_to_remove.push((
                    edge_ref.source,
                    edge_ref.target,
                    edge_ref.kind.clone(),
                    edge_ref.file,
                ));
            }
        }
    }

    // Deduplicate (forward and reverse traversal may find the same edge)
    edges_to_remove.sort_by(|a, b| {
        a.0.index()
            .cmp(&b.0.index())
            .then_with(|| a.1.index().cmp(&b.1.index()))
    });
    edges_to_remove.dedup_by(|a, b| {
        a.0.index() == b.0.index()
            && a.1.index() == b.1.index()
            && std::mem::discriminant(&a.2) == std::mem::discriminant(&b.2)
    });

    for (source, target, kind, file) in edges_to_remove {
        graph.edges_mut().remove_edge(source, target, kind, file);
        count += 1;
    }

    count
}

/// Tombstones old node slots by calling `arena.remove(id)`.
fn tombstone_old_nodes(graph: &mut CodeGraph, segment: &FileSegment) -> usize {
    let start = segment.start_slot;
    let end = segment.end_slot();
    let mut count = 0usize;

    for idx in start..end {
        if let Some(slot) = graph.nodes().slot(idx)
            && slot.is_occupied()
        {
            let generation = slot.generation();
            let node_id = NodeId::new(idx, generation);
            graph.nodes_mut().remove(node_id);
            count += 1;
        }
    }

    count
}

/// Allocates a new append-only segment in the arena and updates the segment table.
///
/// Returns the start slot index of the new segment.
///
/// # Errors
///
/// Returns an error if the arena capacity is exceeded.
pub fn allocate_new_segment(
    graph: &mut CodeGraph,
    file_id: FileId,
    node_count: u32,
) -> Result<u32, crate::graph::unified::storage::arena::ArenaError> {
    let placeholder = crate::graph::unified::storage::NodeEntry::new(
        crate::graph::unified::node::NodeKind::Other,
        crate::graph::unified::string::id::StringId::new(0),
        file_id,
    );
    let start = graph.nodes_mut().alloc_range(node_count, &placeholder)?;
    graph
        .file_segments_mut()
        .record_range(file_id, start, node_count);
    Ok(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reindex_stats_default() {
        let stats = ReindexStats::default();
        assert_eq!(stats.files_reindexed, 0);
        assert_eq!(stats.nodes_tombstoned, 0);
        assert_eq!(stats.edges_tombstoned, 0);
        assert_eq!(stats.nodes_committed, 0);
        assert_eq!(stats.edges_inserted, 0);
        assert_eq!(stats.files_skipped, 0);
    }
}
