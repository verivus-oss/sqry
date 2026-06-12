//! Per-file segment table mapping `FileId` to contiguous node ranges.
//!
//! `FileSegmentTable` records the `(start_slot, slot_count)` pair for each
//! file's nodes in the `NodeArena`. This is populated during the initial full
//! build's Phase 3 parallel commit and persisted in V10+ snapshots.
//!
//! # Design
//!
//! During the full build pipeline, `phase2_assign_ranges` computes a disjoint
//! `node_range` for each file in each chunk. `FileSegmentTable::record_range`
//! is called once per file after Phase 3 parallel commit to store these ranges.
//!
//! For incremental re-indexing (`reindex_files`), the segment table is used to
//! identify which node slots belong to a file so they can be tombstoned before
//! the file is re-parsed and committed at a new append-only range.

use serde::{Deserialize, Serialize};

use crate::graph::unified::file::id::FileId;

/// Per-file segment in the node arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSegment {
    /// First node slot index in the arena for this file.
    pub start_slot: u32,
    /// Number of contiguous node slots assigned to this file.
    pub slot_count: u32,
}

impl FileSegment {
    /// Returns the exclusive end of the slot range.
    #[inline]
    #[must_use]
    pub fn end_slot(&self) -> u32 {
        self.start_slot.saturating_add(self.slot_count)
    }

    /// Returns `true` if the segment is empty (no nodes).
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slot_count == 0
    }
}

/// Maps `FileId` to contiguous node ranges in the `NodeArena`.
///
/// Indexed by `FileId` (as `u32`). Files without segments have `None` entries.
/// The table is populated during the initial full build and updated during
/// incremental re-indexing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileSegmentTable {
    /// Segments indexed by `FileId.as_u32()`. `None` entries indicate files
    /// that have been removed or were never indexed.
    segments: Vec<Option<FileSegment>>,
}

impl FileSegmentTable {
    /// Creates an empty segment table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// Creates a table pre-sized for the given number of files.
    #[must_use]
    pub fn with_capacity(file_count: usize) -> Self {
        Self {
            segments: vec![None; file_count],
        }
    }

    /// Records a segment for a file. Grows the table if necessary.
    pub fn record_range(&mut self, file_id: FileId, start_slot: u32, slot_count: u32) {
        let idx = file_id.index() as usize;
        if idx >= self.segments.len() {
            self.segments.resize(idx + 1, None);
        }
        self.segments[idx] = Some(FileSegment {
            start_slot,
            slot_count,
        });
    }

    /// Returns the segment for a file, or `None` if unknown.
    #[inline]
    #[must_use]
    pub fn get(&self, file_id: FileId) -> Option<&FileSegment> {
        let idx = file_id.index() as usize;
        self.segments.get(idx).and_then(|opt| opt.as_ref())
    }

    /// Removes a file's segment entry (used during incremental re-indexing
    /// when a file's nodes are tombstoned).
    pub fn remove(&mut self, file_id: FileId) {
        let idx = file_id.index() as usize;
        if idx < self.segments.len() {
            self.segments[idx] = None;
        }
    }

    /// Returns the number of files with recorded segments.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments.iter().filter(|s| s.is_some()).count()
    }

    /// Returns the total number of node slots tracked across all segments.
    #[must_use]
    pub fn total_slots(&self) -> u64 {
        self.segments
            .iter()
            .filter_map(|s| s.as_ref())
            .map(|s| u64::from(s.slot_count))
            .sum()
    }

    /// Returns an iterator over all `(FileId, &FileSegment)` pairs.
    ///
    /// # Panics
    ///
    /// Panics if the segment table length exceeds `u32::MAX`, which would make
    /// the segment position unrepresentable as a [`FileId`].
    pub fn iter(&self) -> impl Iterator<Item = (FileId, &FileSegment)> + '_ {
        self.segments.iter().enumerate().filter_map(|(idx, opt)| {
            opt.as_ref().map(|seg| {
                (
                    FileId::new(u32::try_from(idx).expect("file segment index fits u32")),
                    seg,
                )
            })
        })
    }

    /// Returns the backing storage length (for diagnostics).
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.segments.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_retrieve() {
        let mut table = FileSegmentTable::new();
        table.record_range(FileId::new(0), 0, 10);
        table.record_range(FileId::new(5), 10, 20);

        let seg0 = table.get(FileId::new(0)).unwrap();
        assert_eq!(seg0.start_slot, 0);
        assert_eq!(seg0.slot_count, 10);
        assert_eq!(seg0.end_slot(), 10);

        let seg5 = table.get(FileId::new(5)).unwrap();
        assert_eq!(seg5.start_slot, 10);
        assert_eq!(seg5.slot_count, 20);

        assert!(table.get(FileId::new(3)).is_none());
        assert_eq!(table.segment_count(), 2);
        assert_eq!(table.total_slots(), 30);
    }

    #[test]
    fn remove_segment() {
        let mut table = FileSegmentTable::new();
        table.record_range(FileId::new(1), 0, 5);
        assert!(table.get(FileId::new(1)).is_some());

        table.remove(FileId::new(1));
        assert!(table.get(FileId::new(1)).is_none());
        assert_eq!(table.segment_count(), 0);
    }

    #[test]
    fn overwrite_segment() {
        let mut table = FileSegmentTable::new();
        table.record_range(FileId::new(1), 0, 5);
        table.record_range(FileId::new(1), 100, 15);

        let seg = table.get(FileId::new(1)).unwrap();
        assert_eq!(seg.start_slot, 100);
        assert_eq!(seg.slot_count, 15);
    }

    #[test]
    fn iter_over_segments() {
        let mut table = FileSegmentTable::new();
        table.record_range(FileId::new(0), 0, 10);
        table.record_range(FileId::new(2), 10, 5);

        let entries: Vec<_> = table.iter().collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, FileId::new(0));
        assert_eq!(entries[1].0, FileId::new(2));
    }

    #[test]
    fn empty_segment() {
        let seg = FileSegment {
            start_slot: 0,
            slot_count: 0,
        };
        assert!(seg.is_empty());
        assert_eq!(seg.end_slot(), 0);
    }
}
