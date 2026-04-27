//! STEP_11_4 (workspace-aware-cross-repo, 2026-04-26) — daemon
//! `workspaceStatus` per-source-root classpath presence test.
//!
//! Asserts that a logical workspace mixing two source roots — one with
//! `<root>/.sqry/classpath/` present, one without — produces a
//! [`sqry_daemon_protocol::WorkspaceIndexStatus`] aggregate whose
//! per-source-root [`sqry_daemon_protocol::WorkspaceSourceRootStatus`]
//! entries report the correct `classpath_present` flag for each.
//!
//! The probe runs at status time inside
//! `WorkspaceManager::workspace_index_status` (see
//! `sqry-daemon/src/workspace/manager.rs::probe_classpath_present`).

use std::{path::PathBuf, sync::Arc};

use sqry_core::project::ProjectRootMode;
use sqry_daemon::{
    DaemonConfig, WorkspaceKey, WorkspaceManager, WorkspaceState, workspace::LoadedWorkspace,
};
use sqry_daemon_protocol::WorkspaceId;

fn make_manager() -> Arc<WorkspaceManager> {
    let config = Arc::new(DaemonConfig::default());
    WorkspaceManager::new_without_reaper(config)
}

fn insert_workspace(
    mgr: &WorkspaceManager,
    key: &WorkspaceKey,
    state: WorkspaceState,
    bytes: usize,
) -> Arc<LoadedWorkspace> {
    mgr.insert_workspace_for_test_with_bytes(key.clone(), state, false, bytes)
}

#[test]
fn daemon_workspace_status_reports_per_source_root_classpath_presence() {
    // Two source roots inside one logical workspace:
    //   source-with-classpath/  →  contains .sqry/classpath/  →  classpath_present == true
    //   source-without-classpath/  →  no .sqry/classpath/    →  classpath_present == false
    //
    // The test fixture is built on the real filesystem so the
    // production probe in `WorkspaceManager::workspace_index_status`
    // exercises its actual `fs::metadata` code path, not a mock.
    let temp = tempfile::tempdir().expect("tempdir");
    let with_classpath = temp.path().join("source-with-classpath");
    let without_classpath = temp.path().join("source-without-classpath");
    std::fs::create_dir_all(with_classpath.join(".sqry").join("classpath"))
        .expect("create classpath dir");
    std::fs::create_dir_all(&without_classpath).expect("create plain source root");

    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0x42; 32]);
    let key_with = WorkspaceKey::with_workspace_id(
        id,
        with_classpath.clone(),
        ProjectRootMode::GitRoot,
        0xdeadbeef,
    );
    let key_without = WorkspaceKey::with_workspace_id(
        id,
        without_classpath.clone(),
        ProjectRootMode::GitRoot,
        0xdeadbeef,
    );

    let _ = insert_workspace(&mgr, &key_with, WorkspaceState::Loaded, 1_000);
    let _ = insert_workspace(&mgr, &key_without, WorkspaceState::Loaded, 2_000);

    let aggregate = mgr.workspace_index_status(&id).expect("aggregate present");
    assert_eq!(aggregate.workspace_id, id);
    assert_eq!(
        aggregate.source_roots.len(),
        2,
        "aggregate must report one row per source root"
    );

    // Lookup by source-root path so the assertions are independent of
    // the manager's lexical sort order.
    let row_with = aggregate
        .source_roots
        .iter()
        .find(|r| r.source_root == with_classpath)
        .expect("source-with-classpath row missing");
    let row_without = aggregate
        .source_roots
        .iter()
        .find(|r| r.source_root == without_classpath)
        .expect("source-without-classpath row missing");

    assert!(
        row_with.classpath_present,
        "source root with .sqry/classpath/ must report classpath_present == true \
         (row was {row_with:?})"
    );
    assert!(
        !row_without.classpath_present,
        "source root without .sqry/classpath/ must report classpath_present == false \
         (row was {row_without:?})"
    );
}

#[test]
fn daemon_workspace_status_classpath_probe_handles_nondirectory() {
    // Defensive: a `.sqry/classpath` *file* (not directory) must not
    // be reported as classpath_present == true. Catches a regression
    // where the probe accepted any `metadata` Ok regardless of
    // file-vs-directory shape.
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("source-bad-classpath");
    std::fs::create_dir_all(source.join(".sqry")).expect("create .sqry dir");
    std::fs::write(source.join(".sqry").join("classpath"), b"not a dir")
        .expect("write classpath as file");

    let mgr = make_manager();
    let id = WorkspaceId::from_bytes([0x43; 32]);
    let key =
        WorkspaceKey::with_workspace_id(id, source.clone(), ProjectRootMode::GitRoot, 0xdead_beef);
    let _ = insert_workspace(&mgr, &key, WorkspaceState::Loaded, 100);

    let aggregate = mgr.workspace_index_status(&id).expect("aggregate present");
    let row = &aggregate.source_roots[0];
    assert!(
        !row.classpath_present,
        "non-directory at .sqry/classpath must NOT be reported as classpath_present \
         (row was {row:?})"
    );
    assert_eq!(row.source_root, source);

    // Drop borrow so we can drop fixture cleanly.
    let _ = PathBuf::from(temp.path());
}
