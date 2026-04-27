//! Unit tests for [`super::discovery::discover_repositories`].
//!
//! Owned by `STEP_3` of the workspace-aware-cross-repo DAG: validates that the
//! discovery walker recognises the canonical `.sqry/graph/manifest.json`
//! marker emitted by `build_unified_graph_inner` (see
//! `graph/unified/persistence/mod.rs`'s `GRAPH_DIR_NAME` /
//! `MANIFEST_FILE_NAME` constants), and not the dead `.sqry-index` legacy
//! placeholder that no part of the live build pipeline writes.

use std::fs;
use std::path::Path;

use tempfile::tempdir;

use super::discovery::{DiscoveryMode, discover_repositories};

/// Create an indexed repository fixture by writing the canonical marker
/// (`.sqry/graph/manifest.json`) under `root/name`. Optionally seeds a
/// `.git/` directory so the same fixture can drive `GitRoots` mode tests.
fn create_indexed_repo(root: &Path, name: &str, with_git: bool) {
    let repo_dir = root.join(name);
    fs::create_dir_all(repo_dir.join(".sqry/graph")).unwrap();
    fs::write(repo_dir.join(".sqry/graph/manifest.json"), b"{}").unwrap();
    if with_git {
        fs::create_dir_all(repo_dir.join(".git")).unwrap();
    }
}

#[test]
fn discover_repositories_finds_indexed_fixture() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    create_indexed_repo(root, "service-a", false);

    let repos = discover_repositories(root, DiscoveryMode::IndexFiles).unwrap();

    assert_eq!(repos.len(), 1, "expected exactly one discovered repo");
    let repo = &repos[0];
    assert_eq!(repo.id.as_str(), "service-a");
    assert_eq!(repo.root, root.join("service-a"));
    assert_eq!(
        repo.index_path,
        root.join("service-a/.sqry/graph/manifest.json"),
        "discovered index_path must point at the canonical marker"
    );
}

#[test]
fn discover_repositories_returns_empty_for_empty_directory() {
    let temp = tempdir().unwrap();

    let repos = discover_repositories(temp.path(), DiscoveryMode::IndexFiles).unwrap();

    assert!(
        repos.is_empty(),
        "empty directory must yield no repositories, got {repos:?}"
    );
}

#[test]
fn discover_repositories_ignores_default_ignored_dirs() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    // Indexed repo nested under node_modules — must be skipped by the walker.
    create_indexed_repo(&root.join("node_modules"), "foo", false);
    // Legitimate top-level repo — must still be found.
    create_indexed_repo(root, "service-a", false);

    let repos = discover_repositories(root, DiscoveryMode::IndexFiles).unwrap();
    let ids: Vec<_> = repos.iter().map(|r| r.id.as_str().to_string()).collect();

    assert_eq!(
        ids,
        vec!["service-a".to_string()],
        "node_modules subtree must be excluded from discovery"
    );
}

#[test]
fn discovery_modes_find_expected_repos() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    create_indexed_repo(root, "service-a", true);
    create_indexed_repo(root, "service-b", false);
    create_indexed_repo(root, "legacy", true);

    let index_results = discover_repositories(root, DiscoveryMode::IndexFiles).unwrap();
    let ids: Vec<_> = index_results.iter().map(|repo| repo.id.as_str()).collect();
    assert_eq!(ids, vec!["legacy", "service-a", "service-b"]);

    let git_results = discover_repositories(root, DiscoveryMode::GitRoots).unwrap();
    let git_ids: Vec<_> = git_results.iter().map(|repo| repo.id.as_str()).collect();
    assert_eq!(git_ids, vec!["legacy", "service-a"]);
}
