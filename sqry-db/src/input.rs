//! File-level input storage with per-file revision counters.
//!
//! `FileInputStore` maps each `FileId` to a [`FileInput`] record containing the
//! set of node/edge/scope IDs belonging to that file and a monotonic revision
//! counter. When a file is re-indexed, its revision bumps, causing all queries
//! that recorded a dependency on that file to be invalidated on next access.

use std::collections::HashMap;

use smallvec::SmallVec;

use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::file::id::FileId;
use sqry_core::graph::unified::node::id::NodeId;

/// Per-file fact set with revision counter.
///
/// The revision counter starts at 1 for the initial build and is bumped every
/// time the file is re-indexed via `reindex_files`.
#[derive(Debug, Clone)]
pub struct FileInput {
    /// Node IDs belonging to this file (from the arena segment).
    pub node_ids: SmallVec<[NodeId; 16]>,
    /// Monotonic revision counter for this file's content.
    ///
    /// Starts at 1. Bumped on every re-index. Queries cache the revision
    /// at read time and are invalidated when the stored revision differs.
    pub revision: u64,
}

impl FileInput {
    /// Creates a new `FileInput` with the given node IDs at revision 1.
    #[must_use]
    pub fn new(node_ids: SmallVec<[NodeId; 16]>) -> Self {
        Self {
            node_ids,
            revision: 1,
        }
    }

    /// Returns the current revision.
    #[inline]
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Bumps the revision and replaces node IDs atomically.
    pub fn update(&mut self, new_node_ids: SmallVec<[NodeId; 16]>) {
        self.revision = self.revision.saturating_add(1);
        self.node_ids = new_node_ids;
    }
}

/// Storage for per-file input facts, indexed by `FileId`.
///
/// This is the Tier 1 dependency source. Queries record the `FileId` + revision
/// of every file they read during execution. On cache validation, each recorded
/// `(FileId, revision)` pair is checked against the current store.
#[derive(Debug, Clone)]
pub struct FileInputStore {
    /// Maps `FileId` (as u32 index) to per-file input.
    entries: HashMap<FileId, FileInput>,
}

impl FileInputStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Populates the store from a graph snapshot by scanning the node arena
    /// for file ownership.
    ///
    /// This is called once during `QueryDb` construction from a full build.
    /// Each file gets revision 1.
    #[must_use]
    pub fn from_snapshot(snapshot: &GraphSnapshot) -> Self {
        let mut file_nodes: HashMap<FileId, SmallVec<[NodeId; 16]>> = HashMap::new();
        let arena = snapshot.nodes();

        for (idx, slot) in arena.slots().iter().enumerate() {
            if let Some(entry) = slot.get() {
                let file_id = entry.file;
                if file_id != FileId::INVALID
                    && let Ok(node_index) = u32::try_from(idx)
                {
                    let node_id = NodeId::new(node_index, slot.generation());
                    file_nodes.entry(file_id).or_default().push(node_id);
                }
            }
        }

        let entries = file_nodes
            .into_iter()
            .map(|(fid, nodes)| (fid, FileInput::new(nodes)))
            .collect();

        Self { entries }
    }

    /// Returns the current revision for a file, or `None` if unknown.
    #[inline]
    #[must_use]
    pub fn revision(&self, file_id: FileId) -> Option<u64> {
        self.entries.get(&file_id).map(|fi| fi.revision)
    }

    /// Returns the file input for a file, or `None` if unknown.
    #[inline]
    #[must_use]
    pub fn get(&self, file_id: FileId) -> Option<&FileInput> {
        self.entries.get(&file_id)
    }

    /// Returns a mutable reference to the file input.
    #[inline]
    pub fn get_mut(&mut self, file_id: FileId) -> Option<&mut FileInput> {
        self.entries.get_mut(&file_id)
    }

    /// Inserts or replaces a file input entry.
    pub fn insert(&mut self, file_id: FileId, input: FileInput) {
        self.entries.insert(file_id, input);
    }

    /// Removes a file input entry, returning it if it existed.
    pub fn remove(&mut self, file_id: FileId) -> Option<FileInput> {
        self.entries.remove(&file_id)
    }

    /// Returns the total number of tracked files.
    #[inline]
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.entries.len()
    }

    /// Returns an iterator over all tracked file IDs.
    pub fn file_ids(&self) -> impl Iterator<Item = FileId> + '_ {
        self.entries.keys().copied()
    }

    /// Returns an iterator over all entries.
    pub fn iter(&self) -> impl Iterator<Item = (FileId, &FileInput)> + '_ {
        self.entries.iter().map(|(&fid, fi)| (fid, fi))
    }

    /// Returns a snapshot of all per-file revision counters as a `Vec`.
    ///
    /// Each element is `(FileId, revision)`. The order is unspecified (`HashMap`
    /// iteration order). Used by the `SAVE_PATH` persistence unit to capture
    /// the Tier 1 dependency state into [`DerivedHeader::file_revisions`].
    #[must_use]
    pub fn all_revisions(&self) -> Vec<(FileId, u64)> {
        self.entries
            .iter()
            .map(|(&fid, fi)| (fid, fi.revision))
            .collect()
    }

    /// Overwrites the stored revision counter for each `(FileId, revision)`
    /// pair in `revisions`.
    ///
    /// Files not present in `revisions` are left untouched. Files in
    /// `revisions` that are not currently tracked in the store are inserted
    /// with empty node IDs at the given revision — this ensures that cold-load
    /// cache entries for files whose nodes haven't been scanned yet can still
    /// have their Tier 1 revisions validated correctly.
    ///
    /// **Infallible by construction**: uses only `HashMap::insert` on `&self`
    /// via interior mutability through the `FileInput` revision field. Called
    /// exclusively from [`QueryDb::commit_staged_load`] — the single infallible
    /// commit boundary in `LOAD_PATH`.
    ///
    /// # Note on `&self`
    ///
    /// `FileInputStore` holds a `HashMap` directly without interior mutability
    /// wrappers — callers must have `&mut FileInputStore`. This method takes
    /// `&mut self` to mutate entries directly. Infallibility is preserved
    /// because `HashMap::insert` and field assignment cannot fail.
    pub(crate) fn restore_revisions(&mut self, revisions: &[(FileId, u64)]) {
        for &(fid, saved_rev) in revisions {
            if let Some(fi) = self.entries.get_mut(&fid) {
                // Overwrite the revision with the saved value.
                fi.revision = saved_rev;
            } else {
                // File not yet tracked — insert with the saved revision and
                // empty node IDs so Tier 1 validation can match it.
                let mut fi = FileInput::new(smallvec::SmallVec::new());
                fi.revision = saved_rev;
                self.entries.insert(fid, fi);
            }
        }
    }
}

impl Default for FileInputStore {
    fn default() -> Self {
        Self::new()
    }
}
