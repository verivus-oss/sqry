//! Integration tests for `DependencyRecorderGuard` and file dependency tracking.

use sqry_core::graph::unified::file::id::FileId;
use sqry_db::dependency::{DependencyRecorderGuard, record_file_dep};
use sqry_db::input::{FileInput, FileInputStore};

#[test]
fn recorder_captures_deps_and_pairs_with_revisions() {
    let mut store = FileInputStore::new();
    store.insert(FileId::new(10), FileInput::new(Default::default()));
    store.insert(FileId::new(20), FileInput::new(Default::default()));
    store.insert(FileId::new(30), FileInput::new(Default::default()));

    let guard = DependencyRecorderGuard::new();

    record_file_dep(FileId::new(10));
    record_file_dep(FileId::new(20));
    record_file_dep(FileId::new(30));
    record_file_dep(FileId::new(10)); // duplicate

    let deps = guard.finish(&store);

    // Should be deduplicated: 3 unique files
    assert_eq!(deps.len(), 3);

    // All at revision 1
    for &(_fid, rev) in &deps {
        assert_eq!(rev, 1);
    }
}

#[test]
fn recorder_ignores_invalid_file_id() {
    let store = FileInputStore::new();
    let guard = DependencyRecorderGuard::new();

    record_file_dep(FileId::INVALID);
    record_file_dep(FileId::INVALID);

    let deps = guard.finish(&store);
    assert!(deps.is_empty(), "INVALID file IDs should be ignored");
}

#[test]
fn recorder_skips_unknown_files() {
    let store = FileInputStore::new(); // empty store

    let guard = DependencyRecorderGuard::new();
    record_file_dep(FileId::new(999)); // not in store

    let deps = guard.finish(&store);
    assert!(deps.is_empty(), "files not in store should be filtered out");
}

#[test]
fn recorder_handles_bumped_revision() {
    let mut store = FileInputStore::new();
    store.insert(FileId::new(1), FileInput::new(Default::default()));

    // Bump revision
    store
        .get_mut(FileId::new(1))
        .unwrap()
        .update(Default::default());

    let guard = DependencyRecorderGuard::new();
    record_file_dep(FileId::new(1));

    let deps = guard.finish(&store);
    assert_eq!(deps.len(), 1);
    assert_eq!(
        deps[0],
        (FileId::new(1), 2),
        "should capture revision 2 after bump"
    );
}

#[test]
fn sequential_guards_are_isolated() {
    let mut store = FileInputStore::new();
    store.insert(FileId::new(1), FileInput::new(Default::default()));
    store.insert(FileId::new(2), FileInput::new(Default::default()));

    // First guard
    {
        let guard = DependencyRecorderGuard::new();
        record_file_dep(FileId::new(1));
        let deps = guard.finish(&store);
        assert_eq!(deps.len(), 1);
    }

    // Second guard should not see deps from first
    {
        let guard = DependencyRecorderGuard::new();
        record_file_dep(FileId::new(2));
        let deps = guard.finish(&store);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].0, FileId::new(2));
    }
}

#[test]
fn nested_finished_guards_propagate_deps_to_parent_scope() {
    let mut store = FileInputStore::new();
    store.insert(FileId::new(1), FileInput::new(Default::default()));
    store.insert(FileId::new(2), FileInput::new(Default::default()));

    let outer = DependencyRecorderGuard::new();
    record_file_dep(FileId::new(1));

    {
        let inner = DependencyRecorderGuard::new();
        record_file_dep(FileId::new(2));
        let inner_deps = inner.finish(&store);
        assert_eq!(inner_deps.as_slice(), &[(FileId::new(2), 1)]);
    }

    let outer_deps = outer.finish(&store);
    assert_eq!(
        outer_deps.as_slice(),
        &[(FileId::new(1), 1), (FileId::new(2), 1)]
    );
}
