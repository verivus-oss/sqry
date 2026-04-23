//! Thread-local dependency recording with RAII cleanup.
//!
//! During query execution, every node/edge read records the owning `FileId` in
//! a thread-local vector. The [`DependencyRecorderGuard`] ensures the vector is
//! cleared on drop (including on panic unwind) to prevent stale state leaks.
//!
//! # Usage
//!
//! ```rust,ignore
//! let guard = DependencyRecorderGuard::new();
//! // ... query execution that calls record_file_dep() ...
//! let deps = guard.finish(&input_store);
//! ```

use std::cell::RefCell;

use smallvec::SmallVec;

use sqry_core::graph::unified::file::id::FileId;

use crate::input::FileInputStore;

/// File dependency pair: `(FileId, revision_at_read_time)`.
pub type FileDep = (FileId, u64);

thread_local! {
    /// Thread-local accumulator for file dependencies during query execution.
    ///
    /// Cleared by `DependencyRecorderGuard::drop` to prevent cross-query leaks.
    static FILE_DEPS: RefCell<Vec<FileId>> = const { RefCell::new(Vec::new()) };
}

/// Records a file dependency from within a query's `execute` method.
///
/// Call this whenever a query reads a node or edge belonging to `file_id`.
/// The guard will collect these at the end and pair them with revision numbers.
///
/// # Panics
///
/// Panics if called outside a `DependencyRecorderGuard` scope (the thread-local
/// is empty but functional — the dep simply won't be captured).
pub fn record_file_dep(file_id: FileId) {
    if file_id == FileId::INVALID {
        return;
    }
    FILE_DEPS.with(|deps| {
        deps.borrow_mut().push(file_id);
    });
}

/// RAII guard that initializes the thread-local dependency recorder and
/// clears it on drop (including on panic unwind).
///
/// # Design
///
/// The guard model ensures no stale file IDs leak between query executions,
/// even if a query panics mid-execution. Create one guard per `QueryDb::get`
/// call, before invoking `Q::execute`.
pub struct DependencyRecorderGuard {
    /// Marker to prevent `Send` (thread-local is per-thread).
    _not_send: std::marker::PhantomData<*const ()>,
}

impl DependencyRecorderGuard {
    /// Creates a new guard, clearing any stale state in the thread-local.
    #[must_use]
    pub fn new() -> Self {
        FILE_DEPS.with(|deps| deps.borrow_mut().clear());
        Self {
            _not_send: std::marker::PhantomData,
        }
    }

    /// Consumes the guard, pairing recorded `FileId`s with their current
    /// revision from the input store. Deduplicates file IDs.
    ///
    /// Returns a `SmallVec` with capacity 8, matching the design doc's
    /// `SmallVec<[(FileId, u64); 8]>` for the common case of queries
    /// touching 8 or fewer files.
    #[must_use]
    pub fn finish(self, inputs: &FileInputStore) -> SmallVec<[FileDep; 8]> {
        let raw = FILE_DEPS.with(|deps| std::mem::take(&mut *deps.borrow_mut()));

        // Deduplicate: sort + dedup (cheaper than a HashSet for small N)
        let mut unique: Vec<FileId> = raw;
        unique.sort_unstable();
        unique.dedup();

        unique
            .into_iter()
            .filter_map(|fid| inputs.revision(fid).map(|rev| (fid, rev)))
            .collect()
    }
}

impl Default for DependencyRecorderGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DependencyRecorderGuard {
    fn drop(&mut self) {
        FILE_DEPS.with(|deps| deps.borrow_mut().clear());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_clears_on_drop() {
        // Record some deps without a guard to verify the thread-local works
        FILE_DEPS.with(|deps| {
            deps.borrow_mut().push(FileId::new(1));
            deps.borrow_mut().push(FileId::new(2));
        });

        // Guard creation should clear stale state
        let guard = DependencyRecorderGuard::new();
        FILE_DEPS.with(|deps| {
            assert!(deps.borrow().is_empty(), "guard should clear on creation");
        });

        // Record new deps
        record_file_dep(FileId::new(10));
        record_file_dep(FileId::new(20));
        record_file_dep(FileId::new(10)); // duplicate

        // Drop without finish — should clear
        drop(guard);
        FILE_DEPS.with(|deps| {
            assert!(deps.borrow().is_empty(), "guard should clear on drop");
        });
    }

    #[test]
    fn finish_deduplicates_and_pairs_revisions() {
        let mut store = FileInputStore::new();
        store.insert(
            FileId::new(1),
            crate::input::FileInput::new(Default::default()),
        );
        store.insert(
            FileId::new(2),
            crate::input::FileInput::new(Default::default()),
        );

        let guard = DependencyRecorderGuard::new();
        record_file_dep(FileId::new(1));
        record_file_dep(FileId::new(2));
        record_file_dep(FileId::new(1)); // duplicate
        record_file_dep(FileId::INVALID); // should be ignored

        let deps = guard.finish(&store);
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0], (FileId::new(1), 1));
        assert_eq!(deps[1], (FileId::new(2), 1));
    }

    #[test]
    fn guard_clears_on_panic_unwind() {
        // Simulate a panic during query execution
        let result = std::panic::catch_unwind(|| {
            let _guard = DependencyRecorderGuard::new();
            record_file_dep(FileId::new(99));
            panic!("simulated query panic");
        });
        assert!(result.is_err());

        // Thread-local should be clean
        FILE_DEPS.with(|deps| {
            assert!(
                deps.borrow().is_empty(),
                "guard should clear on panic unwind"
            );
        });
    }
}
