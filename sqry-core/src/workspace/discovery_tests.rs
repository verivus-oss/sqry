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
use crate::graph::unified::persistence::{BuildProvenance, Manifest};

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

/// Create an indexed repository fixture whose `manifest.json` is a real,
/// fully-parseable [`Manifest`] reporting `node_count` symbols. Used by the
/// issue #515 regression coverage below, which needs discovery to read a
/// real symbol count out of the sidecar rather than the placeholder `{}`
/// body [`create_indexed_repo`] writes (which `Manifest::load` cannot
/// parse, since `node_count` and friends are required fields).
fn create_indexed_repo_with_manifest(root: &Path, name: &str, node_count: usize) {
    let repo_dir = root.join(name);
    let graph_dir = repo_dir.join(".sqry/graph");
    fs::create_dir_all(&graph_dir).unwrap();
    let manifest = Manifest::new(
        repo_dir.display().to_string(),
        node_count,
        node_count * 2,
        "test-sha256",
        BuildProvenance::new("test", "test"),
    );
    manifest.save(graph_dir.join("manifest.json")).unwrap();
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

// ───────────────────────────────────────────────────────────────────────
// Issue #515 regression: `discover_repositories` must populate
// `symbol_count` from each member's `.sqry/graph/manifest.json` instead of
// leaving it `None` forever. Before the fix, `WorkspaceRepository::new`
// always defaulted `symbol_count` to `None` and nothing downstream ever
// set it, so `sqry workspace stats` summed zero symbols across every
// member even though `sqry workspace query` returned real hits from the
// same repositories (the query path loads each member's graph directly
// via `SessionManager`; `stats` never touched the graph at all).
// ───────────────────────────────────────────────────────────────────────

#[test]
fn discover_repositories_populates_symbol_count_from_manifest() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    create_indexed_repo_with_manifest(root, "service-a", 42);
    create_indexed_repo_with_manifest(root, "service-b", 17);

    let repos = discover_repositories(root, DiscoveryMode::IndexFiles).unwrap();
    assert_eq!(repos.len(), 2);

    let service_a = repos
        .iter()
        .find(|r| r.id.as_str() == "service-a")
        .expect("service-a discovered");
    assert_eq!(
        service_a.symbol_count_at_registration,
        Some(42),
        "discovery must read the real node_count out of manifest.json, not leave it None"
    );

    let service_b = repos
        .iter()
        .find(|r| r.id.as_str() == "service-b")
        .expect("service-b discovered");
    assert_eq!(service_b.symbol_count_at_registration, Some(17));
}

#[test]
fn discover_repositories_leaves_symbol_count_none_for_unparseable_manifest() {
    let temp = tempdir().unwrap();
    let root = temp.path();

    // `create_indexed_repo` writes a placeholder `{}` manifest that is
    // structurally present (discovery still registers the repo, its
    // `last_indexed_at` mtime is set) but not a parseable `Manifest`
    // (missing required fields). This must surface as "unknown", never
    // as a silent zero.
    create_indexed_repo(root, "corrupt-manifest", false);

    let repos = discover_repositories(root, DiscoveryMode::IndexFiles).unwrap();
    assert_eq!(repos.len(), 1);
    assert!(repos[0].last_indexed_at.is_some());
    assert_eq!(
        repos[0].symbol_count_at_registration, None,
        "unparseable manifest.json must leave symbol_count unknown (None), not Some(0)"
    );
}
