//! Integration coverage for `STEP_4` — `LogicalWorkspace` integration
//! in the LSP layer.
//!
//! Covers the §1.3 5-step resolution order, the §1.4 `index_status`
//! aggregate / per-source-root / excluded / not-found contract, the
//! `sqry/workspaceStatus` JSON-RPC method, and the auto-index source-root
//! enumeration acceptance criterion.
//!
//! Each `resolve_step_*` integration test exercises one branch of the
//! resolver so a regression in any branch is unambiguous in CI output.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use sqry_core::workspace::{
    HeuristicVerdict, LogicalWorkspace, MemberFolder, MemberReason, SourceRoot, WorkspaceRegistry,
    WorkspaceRepoId, WorkspaceRepository,
};
use sqry_lsp::handlers::index::index_status;
use sqry_lsp::handlers::workspace_status::{
    SqryWorkspaceStatusParams, workspace_status as workspace_status_handler,
};
use sqry_lsp::session::{
    ResolutionBranch, SessionManager, WorkspaceResolutionInputs, default_workspace_heuristic,
    resolve_logical_workspace, resolve_step_1, resolve_step_2, resolve_step_3, resolve_step_4,
    resolve_step_5,
};
use sqry_lsp::{LspOptions, build_test_service};
use tempfile::TempDir;
use tower::Service;
use tower::ServiceExt;
use tower::buffer::Buffer;
use tower_lsp::jsonrpc::Request;
use tower_lsp::lsp_types::{InitializeParams, Url, WorkspaceFolder};

type TestLspFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = std::result::Result<
                    Option<tower_lsp::jsonrpc::Response>,
                    tower_lsp::ExitedError,
                >,
            > + Send,
    >,
>;
type TestLspBuffer = Buffer<Request, TestLspFuture>;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_session(index_root: PathBuf) -> SessionManager {
    SessionManager::new(LspOptions {
        stdio: false,
        socket: None,
        index_root: Some(index_root),
        log_level: "warn".into(),
        config: None,
        allow_public_bind: false,
        daemon: false,
        daemon_socket: None,
        workspace: None,
    })
}

/// Write a v2 `.sqry-workspace` registry into `dir`. The registry has a
/// single source-root entry pointing at `dir` itself so loaders can
/// canonicalize a real path.
fn write_sqry_workspace(dir: &Path, name: &str) -> PathBuf {
    let mut registry = WorkspaceRegistry::new(Some(name.to_string()));
    let id = WorkspaceRepoId::new(".");
    let root = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let index_path = root.join(".sqry-index");
    let repo = WorkspaceRepository::new(id, name.to_string(), root, index_path, None);
    registry
        .upsert_repo(repo)
        .expect("upsert source root into fixture registry");
    let path = dir.join(".sqry-workspace");
    registry.save(&path).expect("save fixture registry");
    path
}

/// Synthesize a `.code-workspace` JSON file with the given folder paths.
fn write_code_workspace(dir: &Path, folders: &[&Path]) -> PathBuf {
    let mut entries = Vec::new();
    for folder in folders {
        entries.push(json!({"path": folder.to_string_lossy()}));
    }
    let json_doc = json!({"folders": entries});
    let path = dir.join("project.code-workspace");
    fs::write(&path, serde_json::to_string_pretty(&json_doc).unwrap())
        .expect("write code-workspace");
    path
}

/// Synthesize a `.code-workspace` JSON file whose folders are explicit
/// sqry source roots. This mirrors the user-facing VS Code workspace
/// shape and bypasses heuristic ambiguity in regression tests.
fn write_source_code_workspace(dir: &Path, folders: &[(&Path, &str)]) -> PathBuf {
    let entries: Vec<_> = folders
        .iter()
        .map(|(folder, name)| {
            json!({
                "path": folder.to_string_lossy(),
                "name": name,
                "sqry.role": "source"
            })
        })
        .collect();
    let json_doc = json!({"folders": entries});
    let path = dir.join("project.code-workspace");
    fs::write(&path, serde_json::to_string_pretty(&json_doc).unwrap())
        .expect("write source code-workspace");
    path
}

async fn drive_initialize_with_workspace_file(
    buffered: &mut TestLspBuffer,
    workspace_file: &Path,
    folders: &[(&Path, &str)],
) {
    let workspace_folders: Vec<WorkspaceFolder> = folders
        .iter()
        .map(|(folder, name)| WorkspaceFolder {
            uri: Url::from_file_path(folder).expect("workspace folder file url"),
            name: (*name).to_string(),
        })
        .collect();
    let params = InitializeParams {
        root_uri: Some(Url::from_file_path(folders[0].0).expect("root file url")),
        workspace_folders: Some(workspace_folders),
        initialization_options: Some(json!({
            "sqry": {
                "workspaceFile": workspace_file
            }
        })),
        ..Default::default()
    };
    let initialize = Request::build("initialize")
        .params(serde_json::to_value(params).expect("initialize params serialize"))
        .id(0i64)
        .finish();
    buffered
        .ready()
        .await
        .expect("service ready for initialize")
        .call(initialize)
        .await
        .expect("initialize request succeeds");

    let initialized = Request::build("initialized").finish();
    buffered
        .ready()
        .await
        .expect("service ready for initialized")
        .call(initialized)
        .await
        .expect("initialized notification succeeds");
}

async fn custom_request_json(buffered: &mut TestLspBuffer, method: &str) -> serde_json::Value {
    let request = Request::build(method.to_string())
        .params(json!({}))
        .id(1i64)
        .finish();
    let response = buffered
        .ready()
        .await
        .expect("service ready for custom request")
        .call(request)
        .await
        .expect("custom request succeeds")
        .expect("custom request returns response");
    let (_, body) = response.into_parts();
    body.expect("custom request has success body")
}

fn fixture_logical_workspace_with_classification()
-> (TempDir, LogicalWorkspace, PathBuf, PathBuf, PathBuf) {
    // Layout:
    //   <tmp>/repo_a            → source root
    //   <tmp>/build_scripts     → member folder (operational)
    //   <tmp>/excluded          → exclusion
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    let repo_a = root.join("repo_a");
    let build_scripts = root.join("build_scripts");
    let excluded = root.join("excluded");
    fs::create_dir_all(&repo_a).unwrap();
    fs::create_dir_all(&build_scripts).unwrap();
    fs::create_dir_all(&excluded).unwrap();

    let canonical_repo = repo_a.canonicalize().unwrap();
    let canonical_build = build_scripts.canonicalize().unwrap();
    let canonical_excl = excluded.canonicalize().unwrap();

    // anonymous_multi_root makes ALL folders source roots; we want a
    // mixed shape (one source root + one member + one exclusion), so we
    // use `from_code_workspace` with explicit `sqry.role` annotations.
    let code_ws_path = root.join("mixed.code-workspace");
    let json_doc = json!({
        "folders": [
            {"path": canonical_repo.to_string_lossy(), "sqry.role": "source"},
            {"path": canonical_build.to_string_lossy(), "sqry.role": "operational"},
            {"path": canonical_excl.to_string_lossy(), "sqry.role": "excluded"}
        ]
    });
    fs::write(
        &code_ws_path,
        serde_json::to_string_pretty(&json_doc).unwrap(),
    )
    .unwrap();
    let ws = LogicalWorkspace::from_code_workspace(&code_ws_path, &|_p| HeuristicVerdict::Unknown)
        .expect("from_code_workspace");

    (tmp, ws, canonical_repo, canonical_build, canonical_excl)
}

// ---------------------------------------------------------------------------
// §1.3 resolution order — one test per branch (acceptance criterion 2)
// ---------------------------------------------------------------------------

#[test]
fn resolve_step_1_initialization_options_sqry_workspace() {
    // Build a canonical LogicalWorkspace, serialize it, hand it to
    // step 1. The resolver should reconstruct the same workspace_id.
    let tmp = TempDir::new().unwrap();
    let canonical = tmp.path().canonicalize().unwrap();
    let original = LogicalWorkspace::single_root(canonical).expect("construct");
    let payload = serde_json::to_value(&original).expect("serialize");

    let resolved = resolve_step_1(Some(&payload))
        .expect("step 1 should accept a valid LogicalWorkspace payload")
        .expect("step 1 should produce Some when payload is present");
    assert_eq!(
        resolved.workspace_id().as_full_hex(),
        original.workspace_id().as_full_hex()
    );

    // None payload yields Ok(None).
    let none_resolved = resolve_step_1(None).expect("absent option is not an error");
    assert!(none_resolved.is_none());

    // Malformed payload errors.
    let bad = json!({"not_a_workspace": true});
    let err = resolve_step_1(Some(&bad)).expect_err("malformed payload must error");
    assert!(err.to_string().contains("LogicalWorkspace"));
}

/// STEP_5 codex iter1 MAJOR 2 — when the extension sends the parsed
/// `.code-workspace` classification hint (`folders` array +
/// `classification` key), `resolve_step_1` must soft-miss (`Ok(None)`)
/// so the resolver falls through to branch 4 (`workspaceFile` path).
/// This is the contract that lets `extension.ts` send the parsed object
/// under `initializationOptions.sqry.workspace` without breaking the
/// LSP's strict-deserialize behaviour for genuine `LogicalWorkspace`
/// payloads.
#[test]
fn resolve_step_1_falls_through_for_extension_classification_hint() {
    // Hint with a populated classification block — soft miss.
    let hint_with_block = json!({
        "folders": [
            { "path": "./repo-a", "name": "Repo A" },
            { "path": "./repo-b" }
        ],
        "classification": {
            "sourceRoots": ["./repo-a"],
            "memberFolders": ["./repo-b"],
            "projectRootMode": "gitRoot"
        }
    });
    let resolved = resolve_step_1(Some(&hint_with_block))
        .expect("classification hint must NOT error — it soft-misses");
    assert!(
        resolved.is_none(),
        "extension classification hint should soft-miss so branch 4 takes over"
    );

    // Hint with classification = null (no sqry.workspace block in the file).
    let hint_no_block = json!({
        "folders": [{ "path": "./only" }],
        "classification": null
    });
    let resolved = resolve_step_1(Some(&hint_no_block))
        .expect("classification hint with null block must soft-miss too");
    assert!(resolved.is_none());

    // Hint with empty folders + classification = null (default payload
    // sent when the workspace file does not exist).
    let empty_hint = json!({ "folders": [], "classification": null });
    let resolved = resolve_step_1(Some(&empty_hint)).expect("empty hint must soft-miss");
    assert!(resolved.is_none());

    // Defence: a payload that looks like a hint but is missing the
    // `classification` key MUST still error (we don't want a malformed
    // LogicalWorkspace JSON that happens to have `folders` to be silently
    // dropped).
    let only_folders = json!({ "folders": [] });
    let err = resolve_step_1(Some(&only_folders))
        .expect_err("payload without `classification` is not a hint and must error");
    assert!(err.to_string().contains("LogicalWorkspace"));
}

#[test]
fn resolve_step_2_index_root_with_sqry_workspace() {
    let tmp = TempDir::new().unwrap();
    let _registry_path = write_sqry_workspace(tmp.path(), "step2");

    let resolved = resolve_step_2(Some(tmp.path()))
        .expect("step 2 should not error on valid registry")
        .expect("step 2 should produce Some when .sqry-workspace exists in --index-root");
    assert!(!resolved.source_roots().is_empty());

    // Without index_root, step 2 yields Ok(None).
    let none_resolved = resolve_step_2(None).expect("None index_root is not an error");
    assert!(none_resolved.is_none());

    // index_root without .sqry-workspace yields Ok(None).
    let bare = TempDir::new().unwrap();
    let bare_resolved = resolve_step_2(Some(bare.path())).expect("bare dir is not an error");
    assert!(bare_resolved.is_none());
}

#[test]
fn resolve_step_3_workspace_folders_with_sqry_workspace() {
    // Two workspace folders: only the second has a .sqry-workspace.
    // Resolver should pick the second.
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    let _ = write_sqry_workspace(tmp_b.path(), "step3");

    let folders = vec![tmp_a.path().to_path_buf(), tmp_b.path().to_path_buf()];
    let resolved = resolve_step_3(&folders)
        .expect("step 3 should not error on valid registry")
        .expect("step 3 should produce Some when any folder has .sqry-workspace");
    assert!(!resolved.source_roots().is_empty());

    // Empty list yields None.
    let empty_resolved = resolve_step_3(&[]).expect("empty folders is not an error");
    assert!(empty_resolved.is_none());

    // Folders without registries yield None.
    let bare = TempDir::new().unwrap();
    let bare_resolved =
        resolve_step_3(&[bare.path().to_path_buf()]).expect("bare folder is not an error");
    assert!(bare_resolved.is_none());
}

#[test]
fn resolve_step_4_sibling_code_workspace() {
    let tmp = TempDir::new().unwrap();
    let folder_a = tmp.path().join("folder_a");
    fs::create_dir_all(&folder_a).unwrap();
    let canonical_a = folder_a.canonicalize().unwrap();
    let workspace_file = write_code_workspace(tmp.path(), &[&canonical_a]);

    let resolved = resolve_step_4(Some(&workspace_file), &|_p| HeuristicVerdict::Source)
        .expect("step 4 should accept a valid .code-workspace")
        .expect("step 4 should produce Some when workspaceFile is provided");
    // With Source heuristic, every folder becomes a source root.
    assert!(!resolved.source_roots().is_empty());

    // None workspace_file yields None.
    let none_resolved =
        resolve_step_4(None, &|_p| HeuristicVerdict::Unknown).expect("None is not an error");
    assert!(none_resolved.is_none());

    // Missing file errors.
    let missing = tmp.path().join("missing.code-workspace");
    let err = resolve_step_4(Some(&missing), &|_p| HeuristicVerdict::Unknown)
        .expect_err("missing file must error");
    assert!(err.to_string().contains("branch 4"));
}

#[test]
fn resolve_step_5_anonymous_multi_root_fallback() {
    let tmp_a = TempDir::new().unwrap();
    let tmp_b = TempDir::new().unwrap();
    let folders = vec![tmp_a.path().to_path_buf(), tmp_b.path().to_path_buf()];

    let resolved = resolve_step_5(folders.clone(), tmp_a.path()).expect("step 5 is the fallback");
    assert_eq!(resolved.source_roots().len(), 2);

    // Empty folders -> single source root at fallback_root.
    let resolved_empty = resolve_step_5(Vec::new(), tmp_a.path())
        .expect("empty folders falls back to fallback_root");
    assert_eq!(resolved_empty.source_roots().len(), 1);
}

// Composed resolver — verifies short-circuit ordering.
#[test]
fn resolve_logical_workspace_short_circuits_in_documented_order() {
    let tmp = TempDir::new().unwrap();
    let folder = tmp.path().to_path_buf();
    let _registry = write_sqry_workspace(tmp.path(), "compose");

    // Step 2 should win when both steps 2 and 3 are eligible.
    let inputs = WorkspaceResolutionInputs {
        workspace_folders: vec![folder.clone()],
        index_root: Some(folder.clone()),
        init_options_workspace: None,
        init_options_workspace_file: None,
    };
    let heuristic = default_workspace_heuristic();
    let (_workspace, branch) = resolve_logical_workspace(&inputs, &folder, &heuristic).unwrap();
    assert_eq!(branch, ResolutionBranch::IndexRootSqryWorkspace);

    // Without index_root, step 3 should fire.
    let inputs2 = WorkspaceResolutionInputs {
        workspace_folders: vec![folder.clone()],
        index_root: None,
        init_options_workspace: None,
        init_options_workspace_file: None,
    };
    let (_workspace2, branch2) = resolve_logical_workspace(&inputs2, &folder, &heuristic).unwrap();
    assert_eq!(branch2, ResolutionBranch::WorkspaceFolderSqryWorkspace);

    // Without any registry, step 5 fires.
    let bare = TempDir::new().unwrap();
    let inputs3 = WorkspaceResolutionInputs {
        workspace_folders: vec![bare.path().to_path_buf()],
        index_root: None,
        init_options_workspace: None,
        init_options_workspace_file: None,
    };
    let (_workspace3, branch3) =
        resolve_logical_workspace(&inputs3, bare.path(), &heuristic).unwrap();
    assert_eq!(branch3, ResolutionBranch::AnonymousMultiRoot);
}

// ---------------------------------------------------------------------------
// §1.4 index_status contract (acceptance criterion 3)
// ---------------------------------------------------------------------------

#[test]
fn index_status_for_source_root_returns_per_source_root() {
    // Source root with no graph snapshot -> not_found (existing behaviour
    // preserved for the Source-classified branch).
    let tmp = TempDir::new().unwrap();
    let session = make_session(tmp.path().to_path_buf());

    // Default LogicalWorkspace at session init is AnonymousMultiRoot
    // over [index_root], so the index_root path classifies as Source.
    let status = index_status(&session, None).expect("index_status should succeed");
    assert!(!status.exists, "no snapshot -> not exists");
    // No `stale` partial flag because we're on a Source path, not an
    // aggregate.
    assert!(status.stale.is_none());
}

#[test]
fn index_status_for_member_folder_returns_aggregate() {
    // Construct a session whose LogicalWorkspace contains a member
    // folder; ask index_status about that folder; assert the response
    // shape matches the §1.4 aggregate contract — full
    // `WorkspaceIndexStatus` (per-source-root statuses + summary
    // counters + generated_at) carried in `IndexStatus.aggregate`,
    // plus a dedicated `partial` flag when any source root is
    // missing / error.
    let (tmp, workspace, src, member, _excl) = fixture_logical_workspace_with_classification();
    let session = make_session(tmp.path().to_path_buf());
    session.set_logical_workspace(std::sync::Arc::new(workspace));

    let status = index_status(&session, Some(member.to_str().unwrap()))
        .expect("index_status for member folder must succeed");

    // path is set to the canonical member path.
    assert_eq!(
        status.path.as_deref(),
        Some(member.display().to_string().as_str())
    );

    // No snapshot exists at any source root -> exists=false, partial=true.
    assert!(!status.exists, "no source-root snapshot -> exists=false");
    assert_eq!(
        status.partial,
        Some(true),
        "missing source roots set partial=true"
    );
    // The aggregate path uses `partial`, not `stale` (the latter is
    // reserved for the >24h freshness bit on per-source-root responses).
    assert!(status.stale.is_none(), "aggregate path leaves stale unset");

    // The full WorkspaceIndexStatus must be present, not flattened away.
    let aggregate = status
        .aggregate
        .as_ref()
        .expect("member-folder branch must carry the WorkspaceIndexStatus aggregate");

    // Summary counters mirror the source-root vector's contents.
    assert_eq!(
        aggregate.missing_count, 1,
        "single missing source root -> missing_count=1"
    );
    assert_eq!(aggregate.ok_count, 0, "no source root has a snapshot");
    assert_eq!(aggregate.building_count, 0, "no rebuild in progress");
    assert_eq!(aggregate.error_count, 0, "no I/O errors");
    assert_eq!(
        aggregate.total(),
        1,
        "fixture has exactly one source root in the aggregate"
    );

    // Per-source-root entries are visible — the codex BLOCK called out
    // that flattening these into a single IndexStatus discarded the
    // contract, so the test now asserts the vector survives.
    assert_eq!(
        aggregate.source_root_statuses.len(),
        1,
        "one source root in the per-source-root vector"
    );
    let entry = &aggregate.source_root_statuses[0];
    assert_eq!(
        entry.path, src,
        "source-root path round-trips into aggregate"
    );
    assert_eq!(
        entry.status,
        sqry_core::workspace::SourceRootIndexState::Missing,
        "source root with no snapshot is reported as Missing"
    );

    // `generated_at` is a real wall-clock timestamp set by the
    // aggregator (sanity-check it's non-epoch).
    assert!(
        aggregate
            .generated_at
            .duration_since(std::time::UNIX_EPOCH)
            .is_ok(),
        "generated_at must be a post-epoch SystemTime"
    );
}

#[test]
fn index_status_for_excluded_path_returns_excluded() {
    let (tmp, workspace, _src, _member, excluded) = fixture_logical_workspace_with_classification();
    let session = make_session(tmp.path().to_path_buf());
    session.set_logical_workspace(std::sync::Arc::new(workspace));

    let status = index_status(&session, Some(excluded.to_str().unwrap()))
        .expect("index_status for excluded path must succeed");
    assert!(!status.exists);
    assert_eq!(
        status.path.as_deref(),
        Some(excluded.display().to_string().as_str())
    );
    // Excluded != stale.
    assert!(status.stale.is_none());
}

#[test]
fn index_status_for_outside_path_returns_not_found() {
    // Outside paths are rejected by --index-root (acceptance criterion 7).
    // resolve_path returns an error, which we surface as not_found
    // rather than leaking the boundary violation back to the client.
    let tmp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let session = make_session(tmp.path().to_path_buf());

    let status = index_status(&session, Some(outside.path().to_str().unwrap()))
        .expect("outside path must not bubble up an error");
    assert!(!status.exists);
    assert!(status.path.is_none());
}

// ---------------------------------------------------------------------------
// Auto-index iteration (acceptance criterion 4)
// ---------------------------------------------------------------------------

#[test]
fn auto_index_iterates_only_source_roots() {
    // We can't run the full LSP auto-index loop in a unit test (it
    // spawns tokio tasks that build real graphs), but we *can* verify
    // the central invariant: `logical_workspace.source_roots()` is the
    // exact list the loop uses, and member folders are absent from
    // that list.
    let (tmp, workspace, src, member, excluded) = fixture_logical_workspace_with_classification();
    let session = make_session(tmp.path().to_path_buf());
    session.set_logical_workspace(std::sync::Arc::new(workspace));

    let logical = session.logical_workspace();
    let source_paths: Vec<PathBuf> = logical
        .source_roots()
        .iter()
        .map(|r| r.path.clone())
        .collect();

    assert!(source_paths.contains(&src), "source root present");
    assert!(
        !source_paths.contains(&member),
        "member folder NOT in source_roots"
    );
    assert!(
        !source_paths.contains(&excluded),
        "excluded NOT in source_roots"
    );
}

// ---------------------------------------------------------------------------
// sqry/workspaceStatus (acceptance criterion 6)
// ---------------------------------------------------------------------------

#[test]
fn workspace_status_returns_workspace_id_short_and_full() {
    let (tmp, workspace, _src, _member, _excl) = fixture_logical_workspace_with_classification();
    let session = make_session(tmp.path().to_path_buf());
    let expected_short = workspace.workspace_id().as_short_hex();
    let expected_full = workspace.workspace_id().as_full_hex();
    session.set_logical_workspace(std::sync::Arc::new(workspace));

    let result = workspace_status_handler(&session, &SqryWorkspaceStatusParams::default())
        .expect("workspace_status should succeed");

    assert_eq!(result.info.workspace_id_short, expected_short);
    assert_eq!(result.info.workspace_id_full, expected_full);
    assert_eq!(result.info.source_roots.len(), 1);
    assert_eq!(result.info.member_folders.len(), 1);
    assert_eq!(
        result.info.member_folders[0].reason,
        MemberReason::OperationalFolder
    );
    assert_eq!(result.info.exclusions.len(), 1);
}

#[test]
fn real_lsp_workspace_status_is_authoritative_for_multi_root_code_workspace() {
    // Regression guard for the VS Code pane bug reported on 2026-04-28:
    // the extension must consume `sqry/workspaceStatus` for aggregate
    // workspace state. No-path `sqry/indexStatus` is a path status
    // surface; in a multi-root `.code-workspace` it may legitimately
    // report `exists=false` for the fallback root and must never be
    // synthesized into a fake one-entry "missing" workspace aggregate.
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    let repo_a = root.join("repo_a");
    let repo_b = root.join("repo_b");
    fs::create_dir_all(repo_a.join("src")).expect("repo_a dirs");
    fs::create_dir_all(repo_b.join("src")).expect("repo_b dirs");
    fs::write(
        repo_a.join("src/lib.rs"),
        "pub fn alpha_symbol() -> usize { 1 }\n",
    )
    .expect("write repo_a source");
    fs::write(
        repo_b.join("src/lib.rs"),
        "pub fn beta_symbol() -> usize { 2 }\n",
    )
    .expect("write repo_b source");

    let repo_a = repo_a.canonicalize().expect("repo_a canonical");
    let repo_b = repo_b.canonicalize().expect("repo_b canonical");
    let workspace_file =
        write_source_code_workspace(root, &[(&repo_a, "repo_a"), (&repo_b, "repo_b")]);

    let build_session = make_session(root.to_path_buf());
    let reporter = sqry_core::progress::no_op_reporter();
    sqry_lsp::handlers::index::rebuild_index(&build_session, &repo_a, &reporter, true)
        .expect("repo_a real index builds");
    sqry_lsp::handlers::index::rebuild_index(&build_session, &repo_b, &reporter, true)
        .expect("repo_b real index builds");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let session = make_session(root.to_path_buf());
        let service = build_test_service(&session);
        let mut buffered = Buffer::new(service, 4);
        drive_initialize_with_workspace_file(
            &mut buffered,
            &workspace_file,
            &[(&repo_a, "repo_a"), (&repo_b, "repo_b")],
        )
        .await;

        let workspace_status = custom_request_json(&mut buffered, "sqry/workspaceStatus").await;
        assert_eq!(
            workspace_status["aggregate"]["ok_count"], 2,
            "real workspaceStatus must see both prebuilt source-root indexes as ok: {workspace_status:#}"
        );
        assert_eq!(
            workspace_status["aggregate"]["missing_count"], 0,
            "real workspaceStatus must not re-prompt indexed source roots: {workspace_status:#}"
        );
        let source_roots = workspace_status["aggregate"]["source_root_statuses"]
            .as_array()
            .expect("source_root_statuses array");
        assert_eq!(source_roots.len(), 2, "two source-root statuses");
        assert!(
            source_roots.iter().all(|entry| entry["status"] == "ok"),
            "all source roots should be ok: {source_roots:#?}"
        );

        let no_path_index_status = custom_request_json(&mut buffered, "sqry/indexStatus").await;
        assert_eq!(
            no_path_index_status["status"]["exists"], false,
            "no-path indexStatus remains a fallback-root path status in this fixture"
        );
        assert!(
            no_path_index_status["status"].get("aggregate").is_none(),
            "this regression guard intentionally proves no-path indexStatus is not the workspace aggregate"
        );
    });
}

// ---------------------------------------------------------------------------
// SessionManager: replaceable Arc<LogicalWorkspace> (acceptance criterion 1)
// ---------------------------------------------------------------------------

#[test]
fn session_manager_owns_replaceable_arc_logical_workspace() {
    let tmp = TempDir::new().unwrap();
    let session = make_session(tmp.path().to_path_buf());

    let original = session.logical_workspace();
    let original_id = original.workspace_id().as_full_hex();

    // Construct a new workspace and swap it in.
    let other_tmp = TempDir::new().unwrap();
    let new_workspace =
        LogicalWorkspace::single_root(other_tmp.path().to_path_buf()).expect("single_root");
    let new_id = new_workspace.workspace_id().as_full_hex();
    session.set_logical_workspace(std::sync::Arc::new(new_workspace));

    let after = session.logical_workspace();
    assert_eq!(after.workspace_id().as_full_hex(), new_id);
    assert_ne!(original_id, new_id);

    // The original Arc snapshot is stable (acceptance criterion 1's
    // "replaceable via sqry/workspaceUpdate" implies pre-swap readers
    // keep their view).
    assert_eq!(original.workspace_id().as_full_hex(), original_id);
}

// Suppress unused-imports warnings for helpers used only in cfg-gated paths.
#[allow(dead_code)]
fn _unused_imports_anchor(_: SourceRoot, _: MemberFolder) {}
