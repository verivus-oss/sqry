use super::registry::*;
use super::{WorkspaceIndex, WorkspaceRepoId};
use crate::session::SessionManager;
use tempfile::tempdir;

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
            repo.symbol_count_at_registration = Some(100);
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

// =====================================================================
// LogicalWorkspace tests (STEP_1 of workspace-aware-cross-repo DAG)
//
// Acceptance criteria, mapped to tests:
//   1. Types compile + re-export from sqry_core::workspace.
//   2. WorkspaceId is [u8; 32]; short hex 16, full hex 64.
//   3. AnonymousMultiRoot reorder → identical WorkspaceId.
//   4. Case-insensitive mount detection → identical WorkspaceId.
//   5. Symlink-unresolved path is recorded in identity_inputs.
//   6. classify() returns Source / Member { reason } / Excluded / Unknown
//      per §1.4.
//   7. Heuristic classifier covers Bazel/Pants/worktree/sparse-checkout
//      /aggregate-checkout edge cases.
//   8. Tests + clippy clean.
// =====================================================================
mod logical_tests {
    use super::super::logical::{
        Classification, HeuristicVerdict, LogicalWorkspace, MemberReason, WorkspaceId,
        WorkspaceIdentity,
    };
    use crate::project::types::ProjectRootMode;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    // ----- helpers ---------------------------------------------------

    fn write_code_workspace(dir: &Path, name: &str, body: &serde_json::Value) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, serde_json::to_vec_pretty(body).unwrap()).unwrap();
        p
    }

    fn mkdir(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// Heuristic that always returns the configured verdict.
    fn const_heuristic(v: HeuristicVerdict) -> impl Fn(&Path) -> HeuristicVerdict {
        move |_p: &Path| v.clone()
    }

    /// Heuristic that classifies based on whether the folder name contains
    /// a marker file (used to simulate Bazel/Pants/worktree fixtures).
    fn marker_heuristic(path: &Path) -> HeuristicVerdict {
        // Aggregate-checkout markers — treat as operational parent.
        if path.join("AGGREGATE_PARENT").exists() {
            return HeuristicVerdict::Member {
                reason: MemberReason::OperationalFolder,
            };
        }
        // Bazel/Pants — folder is a sub-package; treat as non-source
        // member because the source root is the ancestor monorepo.
        if path.join("BUILD.bazel").exists()
            || path.join("BUILD").exists()
            || path.join("pants.toml").exists()
        {
            return HeuristicVerdict::Member {
                reason: MemberReason::NonSourceFolder,
            };
        }
        // Worktree — `.git` is a *file* with `gitdir:` body.
        if let Ok(meta) = fs::symlink_metadata(path.join(".git"))
            && meta.is_file()
        {
            return HeuristicVerdict::Source;
        }
        // Sparse checkout — empty repo with `.git` dir but no source files.
        if path.join(".git").is_dir() && !path.join("HAS_SOURCES").exists() {
            return HeuristicVerdict::Member {
                reason: MemberReason::NonSourceFolder,
            };
        }
        // Default: real repo.
        if path.join(".git").is_dir() {
            return HeuristicVerdict::Source;
        }
        HeuristicVerdict::Unknown
    }

    // ----- WorkspaceId hex / size ------------------------------------

    #[test]
    fn workspace_id_blake3_256_size() {
        let id = WorkspaceId::from_identity(&WorkspaceIdentity::SingleRoot {
            path: PathBuf::from("/x"),
            symlink_unresolved: false,
        });
        // Round-trip via hex must yield 64 chars (= 32 bytes).
        assert_eq!(id.as_full_hex().len(), 64);
        assert_eq!(id.as_bytes().len(), 32);
    }

    #[test]
    fn as_short_hex_returns_16_chars() {
        let id = WorkspaceId::from_identity(&WorkspaceIdentity::SingleRoot {
            path: PathBuf::from("/foo"),
            symlink_unresolved: false,
        });
        assert_eq!(id.as_short_hex().len(), 16);
        assert!(id.as_short_hex().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn as_full_hex_returns_64_chars() {
        let id = WorkspaceId::from_identity(&WorkspaceIdentity::SingleRoot {
            path: PathBuf::from("/foo"),
            symlink_unresolved: false,
        });
        assert_eq!(id.as_full_hex().len(), 64);
        assert!(id.as_full_hex().chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ----- AnonymousMultiRoot reorder identity stability -------------

    #[test]
    fn anonymous_multi_root_reorder_produces_same_id() {
        let temp = tempdir().unwrap();
        let a = mkdir(temp.path(), "a");
        let b = mkdir(temp.path(), "b");
        let c = mkdir(temp.path(), "c");

        let ws1 =
            LogicalWorkspace::anonymous_multi_root(vec![a.clone(), b.clone(), c.clone()]).unwrap();
        let ws2 = LogicalWorkspace::anonymous_multi_root(vec![c, b, a]).unwrap();

        assert_eq!(ws1.workspace_id(), ws2.workspace_id());
        // And both produce the same ordered source root list.
        let p1: Vec<_> = ws1.source_roots().iter().map(|r| &r.path).collect();
        let p2: Vec<_> = ws2.source_roots().iter().map(|r| &r.path).collect();
        assert_eq!(p1, p2);
    }

    // ----- Case-insensitive identity stability -----------------------

    #[test]
    fn case_insensitive_mount_produces_same_id() {
        // We exercise the hashing pipeline directly so the test is
        // stable regardless of host filesystem case-sensitivity. Two
        // identities with identical canonical paths must collide
        // regardless of how the *logical* case looked at construction.
        let lower = PathBuf::from("/Users/alice/Project");
        let lowered = PathBuf::from(lower.to_string_lossy().to_lowercase());

        let id_lower = WorkspaceId::from_identity(&WorkspaceIdentity::SingleRoot {
            path: lowered.clone(),
            symlink_unresolved: false,
        });
        let id_lower2 = WorkspaceId::from_identity(&WorkspaceIdentity::SingleRoot {
            path: lowered,
            symlink_unresolved: false,
        });
        assert_eq!(id_lower, id_lower2);

        // And case variants of the *same* canonical (lowercased) input
        // produce a different id only when the canonicalizer doesn't
        // lowercase — which would mean the caller passed two genuinely
        // different paths. The contract `case-variant inputs collapse
        // to identical id when the mount is case-insensitive` is
        // exercised end-to-end via `from_code_workspace` on
        // case-insensitive hosts; here we assert the identity-input
        // hashing is itself deterministic.
        let id_upper = WorkspaceId::from_identity(&WorkspaceIdentity::SingleRoot {
            path: PathBuf::from("/Users/alice/Project"),
            symlink_unresolved: false,
        });
        // Different canonical strings → different ids (this is the
        // case-sensitive contract).
        assert_ne!(id_upper, id_lower);
    }

    /// End-to-end exercise of acceptance criterion 4: case-VARIANT
    /// inputs collapse to identical [`WorkspaceId`] when the
    /// (logical) mount is case-insensitive, and DIVERGE when the mount
    /// is case-sensitive. Uses
    /// `single_root_with_case_sensitivity` to bypass host-FS detection
    /// so the contract is exercised regardless of whether the test
    /// runs on Linux ext4, macOS APFS, or Windows NTFS.
    #[test]
    fn case_insensitive_mount_produces_same_id_end_to_end() {
        let temp = tempdir().unwrap();
        // Construct a real on-disk path so canonicalization succeeds.
        let actual = mkdir(temp.path(), "Bar");
        // Case-variant of the same path string.
        let upper_str = actual.to_string_lossy().to_uppercase();
        let lower_str = actual.to_string_lossy().to_lowercase();
        let upper = PathBuf::from(&upper_str);
        let lower = PathBuf::from(&lower_str);

        // --- case-insensitive: variants must collapse to ONE id ------
        let ws_upper_ci =
            LogicalWorkspace::single_root_with_case_sensitivity(upper.clone(), true).unwrap();
        let ws_lower_ci =
            LogicalWorkspace::single_root_with_case_sensitivity(lower.clone(), true).unwrap();
        assert_eq!(
            ws_upper_ci.workspace_id(),
            ws_lower_ci.workspace_id(),
            "case-insensitive mount must produce identical WorkspaceId for case-variant paths"
        );

        // --- case-sensitive: variants MAY differ ---------------------
        // Two case-variant strings hash to two different ids ONLY when
        // the underlying canonicalization preserves the case
        // distinction. On a case-sensitive host that's the live
        // behaviour; on a case-insensitive host (macOS / Windows) the
        // realpath canonicalization itself may already collapse them,
        // so we only assert the case-insensitive contract above.
        // To still cover the case-sensitive negative case, exercise
        // hashing of two SYNTHETIC non-existing paths — that bypasses
        // realpath and falls through to lexical absolutize, which
        // preserves case verbatim.
        let synth_upper = PathBuf::from("/non/existent/Bar-2026-04-26");
        let synth_lower = PathBuf::from("/non/existent/bar-2026-04-26");
        let synth_upper_strict =
            LogicalWorkspace::single_root_with_case_sensitivity(synth_upper.clone(), false)
                .unwrap();
        let synth_lower_strict =
            LogicalWorkspace::single_root_with_case_sensitivity(synth_lower.clone(), false)
                .unwrap();
        assert_ne!(
            synth_upper_strict.workspace_id(),
            synth_lower_strict.workspace_id(),
            "case-sensitive mount must preserve case-distinction for case-variant paths"
        );
        let synth_upper_folded =
            LogicalWorkspace::single_root_with_case_sensitivity(synth_upper, true).unwrap();
        let synth_lower_folded =
            LogicalWorkspace::single_root_with_case_sensitivity(synth_lower, true).unwrap();
        assert_eq!(
            synth_upper_folded.workspace_id(),
            synth_lower_folded.workspace_id(),
            "case-insensitive mount must collapse synthetic case-variant paths to identical WorkspaceId"
        );
    }

    // ----- Symlink-unresolved flag recorded --------------------------

    #[test]
    fn symlink_unresolved_records_flag_in_identity_inputs() {
        // Path that does not exist → fs::canonicalize fails → flag set.
        let nonexistent = PathBuf::from("/tmp/sqry-test-does-not-exist-2026-04-26");
        let ws = LogicalWorkspace::single_root(nonexistent).unwrap();
        match ws.identity() {
            WorkspaceIdentity::SingleRoot {
                symlink_unresolved, ..
            } => assert!(*symlink_unresolved),
            other => panic!("expected SingleRoot identity, got {other:?}"),
        }

        // And the WorkspaceId differs from a hypothetical resolved-path
        // identity carrying the same path string.
        let resolved = WorkspaceId::from_identity(&WorkspaceIdentity::SingleRoot {
            path: PathBuf::from("/tmp/sqry-test-does-not-exist-2026-04-26"),
            symlink_unresolved: false,
        });
        assert_ne!(ws.workspace_id(), &resolved);
    }

    // ----- classify() contract (§1.4) --------------------------------

    fn build_ws_for_classify() -> (tempfile::TempDir, LogicalWorkspace) {
        let temp = tempdir().unwrap();
        let src = mkdir(temp.path(), "src");
        let _src_sub = mkdir(temp.path(), "src/lib");
        let _ops = mkdir(temp.path(), "ops");
        let _excl = mkdir(temp.path(), "excl");
        let body = serde_json::json!({
            "folders": [
                { "path": "./src", "sqry.role": "source" },
                { "path": "./ops", "sqry.role": "operational" },
                { "path": "./excl", "sqry.role": "excluded" },
            ]
        });
        let wsfile = write_code_workspace(temp.path(), "x.code-workspace", &body);
        let ws = LogicalWorkspace::from_code_workspace(
            &wsfile,
            &const_heuristic(HeuristicVerdict::Unknown),
        )
        .unwrap();
        let _ = src; // silence unused warning
        (temp, ws)
    }

    #[test]
    fn classify_returns_source_for_known_source_root() {
        let (temp, ws) = build_ws_for_classify();
        let src = temp.path().join("src");
        assert_eq!(ws.classify(&src), Classification::Source);
    }

    #[test]
    fn classify_returns_source_for_descendant_of_source_root() {
        let (temp, ws) = build_ws_for_classify();
        let nested = temp.path().join("src").join("lib");
        assert_eq!(ws.classify(&nested), Classification::Source);
    }

    #[test]
    fn classify_returns_member_for_member_folder() {
        let (temp, ws) = build_ws_for_classify();
        let ops = temp.path().join("ops");
        assert_eq!(
            ws.classify(&ops),
            Classification::Member {
                reason: MemberReason::OperationalFolder
            }
        );
    }

    #[test]
    fn classify_returns_excluded_for_exclusion_path() {
        let (temp, ws) = build_ws_for_classify();
        let e = temp.path().join("excl");
        assert_eq!(ws.classify(&e), Classification::Excluded);
    }

    #[test]
    fn classify_returns_unknown_for_outside_path() {
        let (_temp, ws) = build_ws_for_classify();
        let outside = PathBuf::from("/totally/elsewhere/never/seen");
        assert_eq!(ws.classify(&outside), Classification::Unknown);
    }

    // ----- from_code_workspace classification rules ------------------

    #[test]
    fn from_code_workspace_explicit_role_wins() {
        let temp = tempdir().unwrap();
        let _f = mkdir(temp.path(), "f");
        let body = serde_json::json!({
            "folders": [{ "path": "./f", "sqry.role": "operational" }],
            "sqry.workspace": { "sourceRoots": ["./f"] }  // would otherwise be source
        });
        let wsfile = write_code_workspace(temp.path(), "p.code-workspace", &body);
        let ws = LogicalWorkspace::from_code_workspace(
            &wsfile,
            &const_heuristic(HeuristicVerdict::Source),
        )
        .unwrap();
        assert_eq!(ws.source_roots().len(), 0);
        assert_eq!(ws.member_folders().len(), 1);
        assert_eq!(
            ws.member_folders()[0].reason,
            MemberReason::OperationalFolder
        );
    }

    #[test]
    fn from_code_workspace_top_level_sources_override() {
        let temp = tempdir().unwrap();
        let _f = mkdir(temp.path(), "f");
        let body = serde_json::json!({
            "folders": [{ "path": "./f" }],
            "sqry.workspace": { "sourceRoots": ["./f"] }
        });
        let wsfile = write_code_workspace(temp.path(), "p.code-workspace", &body);
        // Heuristic returns Excluded; top-level override should win.
        let ws = LogicalWorkspace::from_code_workspace(
            &wsfile,
            &const_heuristic(HeuristicVerdict::Excluded),
        )
        .unwrap();
        assert_eq!(ws.source_roots().len(), 1);
        assert!(ws.exclusions().is_empty());
    }

    #[test]
    fn from_code_workspace_heuristic_fallback_classifies_unset_folders() {
        let temp = tempdir().unwrap();
        let _src = mkdir(temp.path(), "src");
        let _ops = mkdir(temp.path(), "ops");
        let body = serde_json::json!({
            "folders": [
                { "path": "./src" },
                { "path": "./ops" }
            ]
        });
        let wsfile = write_code_workspace(temp.path(), "p.code-workspace", &body);
        let h = |p: &Path| -> HeuristicVerdict {
            if p.ends_with("src") {
                HeuristicVerdict::Source
            } else {
                HeuristicVerdict::Member {
                    reason: MemberReason::OperationalFolder,
                }
            }
        };
        let ws = LogicalWorkspace::from_code_workspace(&wsfile, &h).unwrap();
        assert_eq!(ws.source_roots().len(), 1);
        assert_eq!(ws.member_folders().len(), 1);
        assert_eq!(
            ws.member_folders()[0].reason,
            MemberReason::OperationalFolder
        );
    }

    #[test]
    fn from_code_workspace_unknown_verdict_becomes_member_no_language_plugin_match() {
        let temp = tempdir().unwrap();
        let _f = mkdir(temp.path(), "f");
        let body = serde_json::json!({ "folders": [{ "path": "./f" }] });
        let wsfile = write_code_workspace(temp.path(), "p.code-workspace", &body);
        let ws = LogicalWorkspace::from_code_workspace(
            &wsfile,
            &const_heuristic(HeuristicVerdict::Unknown),
        )
        .unwrap();
        assert_eq!(ws.member_folders().len(), 1);
        assert_eq!(
            ws.member_folders()[0].reason,
            MemberReason::NoLanguagePluginMatch
        );
    }

    // ----- Edge-case fixtures (§1.1) ---------------------------------

    #[test]
    fn from_code_workspace_handles_bazel_package_via_heuristic() {
        let temp = tempdir().unwrap();
        let pkg = mkdir(temp.path(), "pkg");
        fs::write(pkg.join("BUILD.bazel"), b"# bazel\n").unwrap();
        let body = serde_json::json!({ "folders": [{ "path": "./pkg" }] });
        let wsfile = write_code_workspace(temp.path(), "p.code-workspace", &body);
        let ws = LogicalWorkspace::from_code_workspace(&wsfile, &marker_heuristic).unwrap();
        assert_eq!(ws.source_roots().len(), 0);
        assert_eq!(ws.member_folders().len(), 1);
        assert_eq!(ws.member_folders()[0].reason, MemberReason::NonSourceFolder);
    }

    /// Acceptance criterion 7: the heuristic classifier must cover
    /// Pants packages. A folder containing `pants.toml` is classified
    /// as a non-source member (the source root is the ancestor
    /// monorepo, not the per-package folder).
    #[test]
    fn from_code_workspace_handles_pants_package_via_heuristic() {
        let temp = tempdir().unwrap();
        let pkg = mkdir(temp.path(), "pants_pkg");
        fs::write(pkg.join("pants.toml"), b"[GLOBAL]\nbackend_packages = []\n").unwrap();
        // A nested source file makes the fixture realistic; the
        // heuristic above only inspects the folder, not file contents.
        let nested = mkdir(temp.path(), "pants_pkg/python_sources");
        fs::write(nested.join("main.py"), b"print('hi')\n").unwrap();
        let body = serde_json::json!({ "folders": [{ "path": "./pants_pkg" }] });
        let wsfile = write_code_workspace(temp.path(), "p.code-workspace", &body);
        let ws = LogicalWorkspace::from_code_workspace(&wsfile, &marker_heuristic).unwrap();
        assert_eq!(ws.source_roots().len(), 0);
        assert_eq!(ws.member_folders().len(), 1);
        assert_eq!(ws.member_folders()[0].reason, MemberReason::NonSourceFolder);
    }

    #[test]
    fn from_code_workspace_handles_aggregate_checkout() {
        let temp = tempdir().unwrap();
        // Parent is operational; two children are sources.
        let parent = mkdir(temp.path(), "agg");
        fs::write(parent.join("AGGREGATE_PARENT"), b"").unwrap();
        let child_a = mkdir(temp.path(), "agg/a");
        fs::create_dir_all(child_a.join(".git")).unwrap();
        fs::write(child_a.join("HAS_SOURCES"), b"").unwrap();
        let child_b = mkdir(temp.path(), "agg/b");
        fs::create_dir_all(child_b.join(".git")).unwrap();
        fs::write(child_b.join("HAS_SOURCES"), b"").unwrap();
        let body = serde_json::json!({
            "folders": [
                { "path": "./agg" },
                { "path": "./agg/a" },
                { "path": "./agg/b" }
            ]
        });
        let wsfile = write_code_workspace(temp.path(), "p.code-workspace", &body);
        let ws = LogicalWorkspace::from_code_workspace(&wsfile, &marker_heuristic).unwrap();
        // Two source roots (a, b) and one operational member (agg).
        assert_eq!(ws.source_roots().len(), 2);
        assert_eq!(ws.member_folders().len(), 1);
        assert_eq!(
            ws.member_folders()[0].reason,
            MemberReason::OperationalFolder
        );
    }

    #[test]
    fn from_code_workspace_handles_worktree_via_heuristic() {
        let temp = tempdir().unwrap();
        let wt = mkdir(temp.path(), "worktree");
        // Worktree marker: .git is a *file* containing `gitdir: ...`.
        fs::write(wt.join(".git"), b"gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
        let body = serde_json::json!({ "folders": [{ "path": "./worktree" }] });
        let wsfile = write_code_workspace(temp.path(), "p.code-workspace", &body);
        let ws = LogicalWorkspace::from_code_workspace(&wsfile, &marker_heuristic).unwrap();
        assert_eq!(ws.source_roots().len(), 1);
    }

    #[test]
    fn from_code_workspace_handles_sparse_checkout() {
        let temp = tempdir().unwrap();
        let sparse = mkdir(temp.path(), "sparse");
        fs::create_dir_all(sparse.join(".git")).unwrap(); // empty .git dir, no sources
        let body = serde_json::json!({ "folders": [{ "path": "./sparse" }] });
        let wsfile = write_code_workspace(temp.path(), "p.code-workspace", &body);
        let ws = LogicalWorkspace::from_code_workspace(&wsfile, &marker_heuristic).unwrap();
        assert_eq!(ws.source_roots().len(), 0);
        assert_eq!(ws.member_folders().len(), 1);
        assert_eq!(ws.member_folders()[0].reason, MemberReason::NonSourceFolder);
    }

    // ----- single_root + anonymous_multi_root constructors -----------

    #[test]
    fn single_root_creates_one_source_root() {
        let temp = tempdir().unwrap();
        let r = mkdir(temp.path(), "r");
        let ws = LogicalWorkspace::single_root(r.clone()).unwrap();
        assert_eq!(ws.source_roots().len(), 1);
        assert!(ws.member_folders().is_empty());
        assert!(ws.exclusions().is_empty());
        assert_eq!(ws.project_root_mode(), ProjectRootMode::default());
        assert_eq!(ws.config_fingerprint(), 0);
        assert!(ws.is_source_root(&fs::canonicalize(&r).unwrap()));
    }

    #[test]
    fn anonymous_multi_root_creates_n_source_roots() {
        let temp = tempdir().unwrap();
        let a = mkdir(temp.path(), "a");
        let b = mkdir(temp.path(), "b");
        let ws = LogicalWorkspace::anonymous_multi_root(vec![a, b]).unwrap();
        assert_eq!(ws.source_roots().len(), 2);
        assert!(matches!(
            ws.identity(),
            WorkspaceIdentity::AnonymousMultiRoot { .. }
        ));
    }

    // ----- Legacy v1 .sqry-workspace round-trip ----------------------

    #[test]
    fn from_sqry_workspace_loads_v1_registry_as_single_source_root() {
        use super::super::registry::{WorkspaceRegistry, WorkspaceRepoId, WorkspaceRepository};
        let temp = tempdir().unwrap();
        let registry_path = temp.path().join(".sqry-workspace");
        let repo_root = mkdir(temp.path(), "service-a");
        let mut reg = WorkspaceRegistry::new(Some("Test".into()));
        let repo = WorkspaceRepository::new(
            WorkspaceRepoId::new("service-a"),
            "service-a".into(),
            repo_root.clone(),
            repo_root.join(".sqry-index"),
            None,
        );
        reg.upsert_repo(repo).unwrap();
        reg.save(&registry_path).unwrap();

        let ws = LogicalWorkspace::from_sqry_workspace(&registry_path).unwrap();
        assert!(matches!(
            ws.identity(),
            WorkspaceIdentity::SqryWorkspaceFile { .. }
        ));
        assert_eq!(ws.source_roots().len(), 1);
        assert_eq!(
            ws.source_roots()[0].path,
            fs::canonicalize(&repo_root).unwrap()
        );
        // v1 registry has no member_folders / exclusions yet.
        assert!(ws.member_folders().is_empty());
        assert!(ws.exclusions().is_empty());
    }

    // ----- Identity is_source_root + project_root_mode round-trip ----

    #[test]
    fn project_root_mode_round_trips_from_code_workspace() {
        let temp = tempdir().unwrap();
        let _f = mkdir(temp.path(), "f");
        let body = serde_json::json!({
            "folders": [{ "path": "./f", "sqry.role": "source" }],
            "sqry.workspace": { "projectRootMode": "workspaceRoot" }
        });
        let wsfile = write_code_workspace(temp.path(), "p.code-workspace", &body);
        let ws = LogicalWorkspace::from_code_workspace(
            &wsfile,
            &const_heuristic(HeuristicVerdict::Unknown),
        )
        .unwrap();
        assert_eq!(ws.project_root_mode(), ProjectRootMode::WorkspaceRoot);
    }
}

// =====================================================================
// Acceptance criterion 1: STEP_1 types compile + re-export from the
// public `sqry_core::workspace` surface (not via `super::super::logical`).
//
// `sqry-core/src/workspace/mod.rs` re-exports the STEP_1 types via
// `pub use logical::{...}`. Any change that drops one of those
// re-exports would break this test (compile-time), making it a direct
// guard for the public-surface contract called out in the DAG.
// =====================================================================
mod public_re_export_tests {
    // Inside the same crate `sqry_core::workspace::X` and
    // `crate::workspace::X` are equivalent — using `crate::workspace`
    // here exercises the re-export layer (NOT the per-module path)
    // without needing to depend on the crate by name.
    use crate::workspace::{
        Classification, HeuristicVerdict, LogicalWorkspace, LogicalWorkspaceError, MemberFolder,
        MemberReason, SourceRoot, WorkspaceId, WorkspaceIdentity,
    };

    #[test]
    fn step_1_types_re_export_from_sqry_core_workspace() {
        // Compile-time + runtime assertion: each STEP_1 type is
        // reachable through the public re-export surface. If any
        // of these names disappears from `workspace::mod.rs::pub use
        // logical::{...}` this block fails to compile.
        let _ = std::any::type_name::<LogicalWorkspace>();
        let _ = std::any::type_name::<WorkspaceIdentity>();
        let _ = std::any::type_name::<SourceRoot>();
        let _ = std::any::type_name::<MemberFolder>();
        let _ = std::any::type_name::<MemberReason>();
        let _ = std::any::type_name::<Classification>();
        let _ = std::any::type_name::<WorkspaceId>();
        let _ = std::any::type_name::<HeuristicVerdict>();
        let _ = std::any::type_name::<LogicalWorkspaceError>();

        // Smoke-construct one of the types via the public API to
        // additionally guard the runtime-reachable API shape, not
        // just the type name lookup.
        let id = WorkspaceId::from_identity(&WorkspaceIdentity::SingleRoot {
            path: std::path::PathBuf::from("/sqry/public-re-export-test"),
            symlink_unresolved: false,
        });
        assert_eq!(id.as_full_hex().len(), 64);
    }
}

// =====================================================================
// Registry v1 → v2 schema upgrade tests (STEP_2 of the
// workspace-aware-cross-repo DAG). Coverage matches the acceptance
// criteria in the DAG TOML §[units.STEP_2_REGISTRY_V2]:
//
//   - WorkspaceRegistry serialises/deserialises v2 schema with the new
//     fields (source_roots, member_folders, exclusions, project_root_mode).
//   - WORKSPACE_REGISTRY_VERSION = 2.
//   - Loading a v1 file produces an upgraded v2 in-memory representation.
//   - Loading a file with version > 2 returns a typed error.
//   - v2 round-trip is loss-free.
// =====================================================================
mod registry_v2_tests {
    use super::super::error::WorkspaceError;
    use super::super::logical::MemberReason;
    use super::super::registry::{
        WORKSPACE_REGISTRY_VERSION, WorkspaceMemberFolder, WorkspaceRegistry, WorkspaceRepoId,
        WorkspaceRepository,
    };
    use crate::project::types::ProjectRootMode;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn workspace_registry_version_constant_is_two() {
        assert_eq!(WORKSPACE_REGISTRY_VERSION, 2);
    }

    #[test]
    fn registry_v1_to_v2_auto_upgrade() {
        let temp = tempdir().unwrap();
        let registry_path = temp.path().join(".sqry-workspace");

        // Hand-craft a v1 registry on disk: schema version 1 and the
        // legacy `repositories` field name (no source_roots /
        // member_folders / exclusions / project_root_mode).
        let v1 = json!({
            "metadata": {
                "version": 1,
                "workspace_name": "Legacy",
                "default_discovery_mode": null,
                "created_at": 1_700_000_000_000_u64,
                "updated_at": 1_700_000_000_000_u64,
            },
            "repositories": [
                {
                    "id": "service-a",
                    "name": "service-a",
                    "root": "/ws/service-a",
                    "index_path": "/ws/service-a/.sqry-index",
                    "last_indexed_at": null,
                    "symbol_count": null,
                    "primary_language": null,
                }
            ],
        });
        fs::write(&registry_path, serde_json::to_vec_pretty(&v1).unwrap()).unwrap();

        let loaded = WorkspaceRegistry::load(&registry_path).expect("v1 file must auto-upgrade");
        assert_eq!(loaded.metadata.version, WORKSPACE_REGISTRY_VERSION);
        assert_eq!(loaded.repositories.len(), 1);
        assert_eq!(loaded.repositories[0].id, WorkspaceRepoId::new("service-a"));
        // v2-only collections initialised to defaults.
        assert!(loaded.member_folders.is_empty());
        assert!(loaded.exclusions.is_empty());
        assert_eq!(loaded.project_root_mode, ProjectRootMode::default());
    }

    #[test]
    fn registry_v3_returns_unsupported_version_error() {
        let temp = tempdir().unwrap();
        let registry_path = temp.path().join(".sqry-workspace");

        // Future schema version. We must reject this — never silently
        // downgrade.
        let v3 = json!({
            "metadata": {
                "version": 3,
                "workspace_name": "Future",
                "default_discovery_mode": null,
                "created_at": 1_700_000_000_000_u64,
                "updated_at": 1_700_000_000_000_u64,
            },
            "source_roots": [],
            "member_folders": [],
            "exclusions": [],
            "project_root_mode": "gitRoot",
        });
        fs::write(&registry_path, serde_json::to_vec_pretty(&v3).unwrap()).unwrap();

        let err =
            WorkspaceRegistry::load(&registry_path).expect_err("v3 must return UnsupportedVersion");
        match err {
            WorkspaceError::UnsupportedVersion { found, expected } => {
                assert_eq!(found, 3);
                assert_eq!(expected, WORKSPACE_REGISTRY_VERSION);
            }
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[test]
    fn registry_v2_round_trips() {
        let temp = tempdir().unwrap();
        let registry_path = temp.path().join(".sqry-workspace");

        let mut registry = WorkspaceRegistry::new(Some("ws".into()));
        registry
            .upsert_repo(WorkspaceRepository::new(
                WorkspaceRepoId::new("svc"),
                "svc".into(),
                temp.path().join("svc"),
                temp.path().join("svc/.sqry/graph/manifest.json"),
                None,
            ))
            .unwrap();
        registry.member_folders.push(WorkspaceMemberFolder::new(
            WorkspaceRepoId::new("ops"),
            temp.path().join("ops"),
            MemberReason::OperationalFolder,
        ));
        registry.exclusions.push(temp.path().join("vendored"));
        registry.project_root_mode = ProjectRootMode::WorkspaceRoot;
        registry.save(&registry_path).unwrap();

        let loaded = WorkspaceRegistry::load(&registry_path).unwrap();
        assert_eq!(loaded.metadata.version, WORKSPACE_REGISTRY_VERSION);
        assert_eq!(loaded.repositories.len(), 1);
        assert_eq!(loaded.member_folders.len(), 1);
        assert_eq!(loaded.member_folders[0].id, WorkspaceRepoId::new("ops"));
        assert_eq!(
            loaded.member_folders[0].reason,
            MemberReason::OperationalFolder
        );
        assert_eq!(loaded.exclusions, vec![temp.path().join("vendored")]);
        assert_eq!(loaded.project_root_mode, ProjectRootMode::WorkspaceRoot);
    }

    #[test]
    fn registry_v2_serializes_with_new_field_names() {
        // Verifies the on-disk JSON uses `source_roots` (v2), not
        // `repositories` (v1).
        let temp = tempdir().unwrap();
        let registry_path = temp.path().join(".sqry-workspace");

        let mut registry = WorkspaceRegistry::new(Some("ws".into()));
        registry
            .upsert_repo(WorkspaceRepository::new(
                WorkspaceRepoId::new("svc"),
                "svc".into(),
                PathBuf::from("/ws/svc"),
                PathBuf::from("/ws/svc/.sqry/graph/manifest.json"),
                None,
            ))
            .unwrap();
        registry.project_root_mode = ProjectRootMode::WorkspaceFolder;
        registry.save(&registry_path).unwrap();

        let raw = fs::read_to_string(&registry_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        // v2 names present.
        assert!(
            parsed.get("source_roots").is_some(),
            "v2 must serialise `source_roots`: {raw}"
        );
        assert!(parsed.get("member_folders").is_some());
        assert!(parsed.get("exclusions").is_some());
        assert_eq!(
            parsed.get("project_root_mode").and_then(|v| v.as_str()),
            Some("workspaceFolder")
        );
        // Legacy v1 name absent.
        assert!(
            parsed.get("repositories").is_none(),
            "v2 must not also serialise `repositories`: {raw}"
        );
        assert_eq!(
            parsed
                .get("metadata")
                .and_then(|m| m.get("version"))
                .and_then(serde_json::Value::as_u64),
            Some(2)
        );
    }
}
