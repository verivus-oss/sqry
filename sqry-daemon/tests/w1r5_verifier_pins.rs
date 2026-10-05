//! Verifier pin for surface parity W1, round 5 (its texts and this bound
//! rewritten in round 6, design D32): the positive control of the
//! instrument T50 and T51 read. Both oracles take
//! `TryEvictOutcome::WouldBlock` from `WorkspaceManager::try_evict_for_test`
//! as the proof that the observation held a guard. Before this pin nothing
//! else in the tree showed the same hook answering `Evicted` when no guard
//! is held (bounded by
//! `git grep -n 'TryEvictOutcome::Evicted\|TryEvictOutcome::Absent' 9163ac824 -- 'sqry-daemon/**/*.rs'`:
//! 4 lines, the two doc comments on the variants and the two body
//! expressions of the hook), so a hook that answered `WouldBlock`
//! unconditionally would leave both oracles green; the battery's K39 and
//! K40 rows were the only record of the `Evicted` answer. This pin puts
//! that record in the tree: with no guard held the hook answers `Evicted`
//! and leaves the tombstone (an `Evicted` slot holding the placeholder, no
//! record), and an unknown key answers `Absent`. It establishes the
//! outcomes and the tombstone, not the lock: a hook that cloned the map
//! under a read guard would answer the same (battery row C63), and the lock
//! discipline is T54's (`try_evict_for_test_yields_to_a_held_read_guard` in
//! the manager's test module), which holds `workspaces_read()` on its own
//! thread and requires `WouldBlock`.

#![cfg(feature = "test-hooks")]

use std::sync::Arc;

use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::workspace::manager::TryEvictOutcome;
use sqry_daemon::{DaemonConfig, WorkspaceKey, WorkspaceManager, WorkspaceState};

#[test]
fn try_evict_for_test_evicts_when_no_guard_is_held() {
    let manager = WorkspaceManager::new_without_reaper(Arc::new(DaemonConfig::default()));
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = canonicalize_path(tmp.path()).expect("canonical root");
    let key = WorkspaceKey::new(root, ProjectRootMode::GitRoot, 0);

    let absent = manager.try_evict_for_test(&key);

    manager.insert_workspace_in_state_for_test(key.clone(), WorkspaceState::Loaded);
    let seeded = manager.lookup(&key).expect("the seeded slot is resident");
    let seeded_record = seeded.roster().is_some();
    let seeded_state = seeded.load_state();

    let evicted = manager.try_evict_for_test(&key);
    let after = manager
        .lookup(&key)
        .expect("the tombstone stays in the map");
    let after_state = after.load_state();
    let after_record = after.roster().is_some();
    println!(
        "R5V pin: absent={absent:?} seeded_state={seeded_state:?} seeded_record={seeded_record} \
         evicted={evicted:?} after_state={after_state:?} after_record={after_record}"
    );

    assert_eq!(
        absent,
        TryEvictOutcome::Absent,
        "an unknown key answers Absent (the write lock was free and no entry is keyed by it)"
    );
    assert_eq!(
        (seeded_state, seeded_record),
        (WorkspaceState::Loaded, true),
        "the seeded slot is Loaded with a record before the eviction"
    );
    assert_eq!(
        evicted,
        TryEvictOutcome::Evicted,
        "with no guard held the hook answers Evicted"
    );
    assert_eq!(
        (after_state, after_record),
        (WorkspaceState::Evicted, false),
        "the eviction left the tombstone: Evicted, holding the placeholder with no record"
    );
}
