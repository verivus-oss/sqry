//! RWS12 revision artifact and managed worktree recovery tests.

mod support;

use std::fs;

use sqry_daemon::{
    config::ManagedWorktreeConfig,
    workspace::revision::{
        ManagedWorktreeRegistry, RevisionArtifactStore, recover_managed_worktrees,
    },
};
use sqry_daemon_protocol::ArtifactId;
use support::revision_git_repo;
use tempfile::TempDir;

#[test]
fn startup_artifact_recovery_removes_partial_artifacts_idempotently() {
    let tmp = TempDir::new().expect("tmp");
    let store = RevisionArtifactStore::new(tmp.path().join("revision-graphs"));
    let partial = store
        .artifact_dir("repo", &ArtifactId("artifact".to_owned()))
        .expect("artifact dir");
    fs::create_dir_all(&partial).expect("partial dir");
    fs::write(partial.join("graph.bin"), b"partial").expect("partial graph");

    let first = store.remove_partial_artifacts().expect("first recovery");
    let second = store.remove_partial_artifacts().expect("second recovery");

    assert_eq!(first.len(), 1);
    assert!(second.is_empty());
    assert!(!partial.exists());
}

#[test]
fn managed_worktree_crash_recovery_and_remove_are_idempotent() {
    let repo = revision_git_repo();
    let managed = TempDir::new().expect("managed root");
    let registry = ManagedWorktreeRegistry::new(ManagedWorktreeConfig {
        root: Some(managed.path().to_path_buf()),
        ..ManagedWorktreeConfig::default()
    });
    let managed_root = registry
        .managed_repo_dir(repo.path())
        .expect("managed repo dir");
    let orphan = managed_root.join("orphaned");
    fs::create_dir_all(&orphan).expect("orphan dir");

    let first = recover_managed_worktrees(&registry, repo.path()).expect("first recovery");
    let second = recover_managed_worktrees(&registry, repo.path()).expect("second recovery");

    assert_eq!(first.orphaned_worktree_dirs_removed, vec![orphan.clone()]);
    assert!(second.orphaned_worktree_dirs_removed.is_empty());
    assert!(!orphan.exists());

    registry
        .remove(repo.path(), &orphan)
        .expect("idempotent remove");
}
