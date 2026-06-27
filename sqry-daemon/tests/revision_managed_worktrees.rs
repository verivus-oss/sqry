//! RWS12 managed worktree policy and isolation tests.

mod support;

use std::fs;

use sqry_daemon::{
    DaemonError,
    config::ManagedWorktreeConfig,
    workspace::revision::{
        ManagedWorktreeCreateOptions, ManagedWorktreeKind, ManagedWorktreeRegistry, RawGitSource,
        RawGitSourceOptions, VirtualSourceKind, VirtualSourceReader,
    },
};
use support::{git, revision_git_repo};
use tempfile::TempDir;

#[test]
fn agent_branches_are_unique_isolated_and_reuse_is_refused_without_stash() {
    let repo = revision_git_repo();
    let managed = TempDir::new().expect("managed root");
    let registry = ManagedWorktreeRegistry::new(ManagedWorktreeConfig {
        root: Some(managed.path().to_path_buf()),
        ..ManagedWorktreeConfig::default()
    });

    let mut first_options = ManagedWorktreeCreateOptions::agent_branch(repo.path(), "HEAD", None);
    first_options.agent_id = Some("agent".to_owned());
    first_options.task_id = Some("task".to_owned());
    first_options.lock_reason = Some("rws12 first".to_owned());
    let first = registry.create(&first_options).expect("first worktree");

    let mut second_options = ManagedWorktreeCreateOptions::agent_branch(repo.path(), "HEAD", None);
    second_options.agent_id = Some("agent".to_owned());
    second_options.task_id = Some("task".to_owned());
    second_options.lock_reason = Some("rws12 second".to_owned());
    let second = registry.create(&second_options).expect("second worktree");

    assert_eq!(first.kind, ManagedWorktreeKind::AgentBranch);
    assert_eq!(second.kind, ManagedWorktreeKind::AgentBranch);
    assert_ne!(first.branch_name, second.branch_name);
    assert_ne!(first.path, second.path);

    fs::write(
        first.path.join("first-only.rs"),
        b"pub fn first_only() {}\n",
    )
    .expect("write first");
    assert!(!second.path.join("first-only.rs").exists());

    let reused =
        ManagedWorktreeCreateOptions::agent_branch(repo.path(), "HEAD", first.branch_name.clone());
    let err = registry
        .create(&reused)
        .expect_err("branch reuse must be rejected");
    assert!(
        matches!(err, DaemonError::ManagedWorktreeInUse { .. }),
        "expected ManagedWorktreeInUse, got {err:?}"
    );

    registry
        .remove(repo.path(), &first.path)
        .expect("remove first");
    registry
        .remove(repo.path(), &second.path)
        .expect("remove second");

    let source = fs::read_to_string("src/workspace/revision/worktree_registry.rs")
        .expect("read registry source");
    assert!(
        !source.contains("\"stash\""),
        "managed worktree implementation must not use git stash for isolation"
    );
}

#[test]
fn protected_and_unapproved_agent_branches_are_rejected() {
    let repo = revision_git_repo();
    let managed = TempDir::new().expect("managed root");
    let registry = ManagedWorktreeRegistry::new(ManagedWorktreeConfig {
        root: Some(managed.path().to_path_buf()),
        ..ManagedWorktreeConfig::default()
    });

    for branch in ["main", "feature/manual"] {
        let options = ManagedWorktreeCreateOptions::agent_branch(
            repo.path(),
            "HEAD",
            Some(branch.to_owned()),
        );
        let err = registry
            .create(&options)
            .expect_err("unsafe branch must be rejected");
        assert!(
            matches!(err, DaemonError::ManagedWorktreeInUse { .. }),
            "expected ManagedWorktreeInUse for {branch}, got {err:?}"
        );
    }
}

#[test]
fn submodule_gitlink_is_reported_by_git_and_not_silently_materialized() {
    let repo = revision_git_repo();
    git(
        repo.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &"1".repeat(40),
            "vendor/submodule",
        ],
    );
    git(repo.path(), &["commit", "-m", "gitlink"]);

    let mode = git(repo.path(), &["ls-tree", "HEAD", "vendor/submodule"]);

    assert!(
        mode.starts_with("160000 commit "),
        "fixture must contain an explicit submodule gitlink, got {mode:?}"
    );

    let tree_oid = git(repo.path(), &["rev-parse", "HEAD^{tree}"]);
    let source =
        RawGitSource::open(RawGitSourceOptions::new(repo.path(), tree_oid)).expect("raw source");
    let entry = source
        .entries()
        .iter()
        .find(|entry| entry.path.display_lossy() == "vendor/submodule")
        .expect("gitlink entry");

    assert!(matches!(
        &entry.kind,
        VirtualSourceKind::Gitlink { oid } if oid == &"1".repeat(40)
    ));

    assert!(matches!(
        source.read_entry_bytes(entry),
        Err(DaemonError::SubmoduleUnavailable {
            gitlink_oid: Some(oid),
            ..
        }) if oid == "1".repeat(40)
    ));
}

#[test]
fn missing_raw_git_tree_returns_explicit_missing_object_without_fetch() {
    let repo = revision_git_repo();
    let missing_oid = "f".repeat(40);

    let err = RawGitSource::open(RawGitSourceOptions::new(repo.path(), missing_oid.clone()))
        .expect_err("missing tree must be a typed source error");

    assert!(
        matches!(err, DaemonError::RevisionObjectMissing { ref object, .. } if object == &missing_oid),
        "expected RevisionObjectMissing, got {err:?}"
    );
}
