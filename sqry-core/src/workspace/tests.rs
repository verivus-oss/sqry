use super::{WorkspaceIndex, WorkspaceRepoId, discover_repositories};
use super::{discovery::DiscoveryMode, registry::*};
use crate::session::SessionManager;
use tempfile::tempdir;

use std::fs;
use std::path::Path;

#[test]
fn workspace_registry_round_trip() {
    let temp = tempdir().unwrap();
    let registry_path = temp.path().join(".sqry-workspace");

    let mut registry = WorkspaceRegistry::new(Some("Test Workspace".into()));
    let repo_id = WorkspaceRepoId::new("service-a");
    let repo = WorkspaceRepository::new(
        repo_id.clone(),
        "service-a".into(),
        temp.path().join("service-a"),
        temp.path().join("service-a/.sqry-index"),
        None,
    );
    registry.upsert_repo(repo).unwrap();
    registry.save(&registry_path).unwrap();

    let loaded = WorkspaceRegistry::load(&registry_path).unwrap();

    assert_eq!(loaded.metadata.version, WORKSPACE_REGISTRY_VERSION);
    assert_eq!(
        loaded.metadata.workspace_name,
        Some("Test Workspace".into())
    );
    assert_eq!(loaded.repositories.len(), 1);
    let loaded_repo = &loaded.repositories[0];
    assert_eq!(loaded_repo.id, repo_id);
    assert_eq!(loaded_repo.name, "service-a");
    assert_eq!(loaded_repo.root, temp.path().join("service-a"));
}

#[test]
fn discovery_modes_find_expected_repos() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    create_repo(root, "service-a", true);
    create_repo(root, "service-b", false);
    create_repo(root, "legacy", true);

    let index_results = discover_repositories(root, DiscoveryMode::IndexFiles).unwrap();
    let ids: Vec<_> = index_results.iter().map(|repo| repo.id.as_str()).collect();
    assert_eq!(ids, vec!["legacy", "service-a", "service-b"]);

    let git_results = discover_repositories(root, DiscoveryMode::GitRoots).unwrap();
    let git_ids: Vec<_> = git_results.iter().map(|repo| repo.id.as_str()).collect();
    assert_eq!(git_ids, vec!["legacy", "service-a"]);
}

fn create_repo(root: &Path, name: &str, with_git: bool) {
    let repo_dir = root.join(name);
    fs::create_dir_all(&repo_dir).unwrap();
    if with_git {
        fs::create_dir_all(repo_dir.join(".git")).unwrap();
    }
    fs::write(repo_dir.join(".sqry-index"), b"{}").unwrap();
}

#[test]
fn workspace_index_stats() {
    let temp = tempdir().unwrap();
    let _registry_path = temp.path().join(".sqry-workspace");

    let mut registry = WorkspaceRegistry::new(Some("Test Workspace".into()));

    // Add 3 repos, 2 indexed
    for (name, indexed) in [("repo-a", true), ("repo-b", true), ("repo-c", false)] {
        let repo_id = WorkspaceRepoId::new(name);
        let mut repo = WorkspaceRepository::new(
            repo_id,
            name.into(),
            temp.path().join(name),
            temp.path().join(name).join(".sqry-index"),
            None,
        );
        if indexed {
            repo.symbol_count = Some(100);
            repo.last_indexed_at = Some(std::time::SystemTime::now());
        }
        registry.upsert_repo(repo).unwrap();
    }

    let session = SessionManager::new().unwrap();
    let index = WorkspaceIndex::new(temp.path(), registry, session);

    let stats = index.stats();

    assert_eq!(stats.total_repos, 3);
    assert_eq!(stats.indexed_repos, 2);
    assert_eq!(stats.total_symbols, 200);
}

#[test]
fn workspace_index_repo_filter() {
    let temp = tempdir().unwrap();
    let _registry_path = temp.path().join(".sqry-workspace");

    let mut registry = WorkspaceRegistry::new(Some("Test Workspace".into()));

    // Add repos: backend-api, backend-core, frontend
    for name in ["backend-api", "backend-core", "frontend"] {
        let repo_id = WorkspaceRepoId::new(name);
        let repo = WorkspaceRepository::new(
            repo_id,
            name.into(),
            temp.path().join(name),
            temp.path().join(name).join(".sqry-index"),
            None,
        );
        registry.upsert_repo(repo).unwrap();
    }

    let session = SessionManager::new().unwrap();
    let mut index = WorkspaceIndex::new(temp.path(), registry, session);

    // Query with repo filter (should only match backend-*)
    let results = index.query("repo:backend-*").unwrap();

    // We expect 0 results because the repos don't have actual indexes,
    // but we can verify the query executed without error
    assert_eq!(results.len(), 0);
}

#[test]
fn workspace_index_open_and_query() {
    let temp = tempdir().unwrap();
    let registry_path = temp.path().join(".sqry-workspace");

    // Create a registry with one repo
    let mut registry = WorkspaceRegistry::new(Some("Test Workspace".into()));
    let repo_id = WorkspaceRepoId::new("test-repo");
    let repo = WorkspaceRepository::new(
        repo_id,
        "test-repo".into(),
        temp.path().join("test-repo"),
        temp.path().join("test-repo/.sqry-index"),
        None,
    );
    registry.upsert_repo(repo).unwrap();
    registry.save(&registry_path).unwrap();

    // Open the workspace index
    let mut index = WorkspaceIndex::open(temp.path(), &registry_path).unwrap();

    // Query should work (even if no results due to missing actual index files)
    let results = index.query("kind:function").unwrap();

    // Expect 0 results since there's no actual index file
    assert_eq!(results.len(), 0);
}

#[test]
fn workspace_index_repo_only_query() {
    // Test repo-only query edge case: "repo:backend" should filter repos but execute empty query
    let temp = tempdir().unwrap();
    let _registry_path = temp.path().join(".sqry-workspace");

    let mut registry = WorkspaceRegistry::new(Some("Test Workspace".into()));

    // Add repos: backend, frontend
    for name in ["backend", "frontend"] {
        let repo_id = WorkspaceRepoId::new(name);
        let repo = WorkspaceRepository::new(
            repo_id,
            name.into(),
            temp.path().join(name),
            temp.path().join(name).join(".sqry-index"),
            None,
        );
        registry.upsert_repo(repo).unwrap();
    }

    let session = SessionManager::new().unwrap();
    let mut index = WorkspaceIndex::new(temp.path(), registry, session);

    // Repo-only query should parse successfully and filter repos
    // The normalized query will be empty, which should match all symbols in filtered repos
    let results = index.query("repo:backend").unwrap();

    // We expect 0 results because the repos don't have actual indexes,
    // but the query should execute without error
    assert_eq!(results.len(), 0);
}

#[test]
fn workspace_index_boolean_query_with_repo_filter() {
    // Test boolean query with repo filter: "repo:backend AND kind:function"
    let temp = tempdir().unwrap();
    let _registry_path = temp.path().join(".sqry-workspace");

    let mut registry = WorkspaceRegistry::new(Some("Test Workspace".into()));

    // Add repos: backend-api, backend-core, frontend
    for name in ["backend-api", "backend-core", "frontend"] {
        let repo_id = WorkspaceRepoId::new(name);
        let repo = WorkspaceRepository::new(
            repo_id,
            name.into(),
            temp.path().join(name),
            temp.path().join(name).join(".sqry-index"),
            None,
        );
        registry.upsert_repo(repo).unwrap();
    }

    let session = SessionManager::new().unwrap();
    let mut index = WorkspaceIndex::new(temp.path(), registry, session);

    // Boolean query with repo filter should parse and execute
    // Should filter to backend-* repos and execute "kind:function" against them
    let results = index.query("repo:backend-* AND kind:function").unwrap();

    // We expect 0 results because the repos don't have actual indexes,
    // but the query should execute without error
    assert_eq!(results.len(), 0);
}

#[test]
fn workspace_index_complex_boolean_query() {
    // Test complex boolean query at workspace level
    let temp = tempdir().unwrap();
    let _registry_path = temp.path().join(".sqry-workspace");

    let mut registry = WorkspaceRegistry::new(Some("Test Workspace".into()));

    let repo_id = WorkspaceRepoId::new("test-repo");
    let repo = WorkspaceRepository::new(
        repo_id,
        "test-repo".into(),
        temp.path().join("test-repo"),
        temp.path().join("test-repo/.sqry-index"),
        None,
    );
    registry.upsert_repo(repo).unwrap();

    let session = SessionManager::new().unwrap();
    let mut index = WorkspaceIndex::new(temp.path(), registry, session);

    // Complex boolean query with OR and NOT
    let results = index
        .query("(kind:function OR kind:class) AND NOT name~=/^test/")
        .unwrap();

    // We expect 0 results because the repo doesn't have an actual index,
    // but the query should parse and execute without error
    assert_eq!(results.len(), 0);
}

// Test workspace_index_repo_only_query_with_symbols was removed:
// It depended on the legacy index, which has been removed in favor of CodeGraph.
