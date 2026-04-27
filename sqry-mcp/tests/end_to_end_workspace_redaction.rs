//! `STEP_7` end-to-end response-redaction coverage.
//!
//! Drives the live `sqry-mcp` binary against a temporary
//! `SQRY_MCP_WORKSPACE_ROOT` containing a v2 `.sqry-workspace` registry
//! with two source roots, one member folder, and one exclusion. Asserts
//! the wire form of `path` fields in the `workspace_status` MCP response
//! against the **exact** workspace-aware tokens specified by acceptance
//! criteria 3-6, recomputed in-test from the same canonical
//! [`compute_source_root_id`] / `workspace_id_short` derivations the
//! redactor uses at runtime — not generic shape probes.
//!
//! Coverage matrix (acceptance criteria → test):
//!
//! | Criterion | Preset    | Field surface                                    | Test                                                                    |
//! |-----------|-----------|--------------------------------------------------|-------------------------------------------------------------------------|
//! | 4         | `minimal` | `aggregate.source_root_statuses[*].path`         | `minimal_preset_renders_exact_source_root_ids`                          |
//! | 5         | `minimal` | `member_folders[*].path` (registry-listed)       | `minimal_preset_renders_member_folder_with_workspace_id_short_prefix`   |
//! | 3 + 6     | `none`    | `aggregate.source_root_statuses[*].path` (cleartext) + `exclusions[]` (excluded → opaque hash) | `none_preset_redacts_excluded_paths_to_opaque_hash_but_passes_others`   |
//!
//! Pre-iter4 the minimal-preset assertion only checked for "some 8-hex
//! prefix" and the `none`-preset test only proved cleartext absolute
//! paths returned (because `create_redactor("none")` returned `None`,
//! so no `LogicalWorkspaceView` was ever bound under passthrough).
//! Iter4 strengthens the minimal-preset assertions to byte-exact
//! `<source_root_id>` / `<workspace_id_short>/<rel>` strings and adds
//! the criterion-6 path under `none` (now wired via the new
//! `create_redactor("none") → Some(passthrough Redactor)` plus the
//! existing `redact_excluded_in_passthrough` walker hook).

mod common;

use anyhow::Result;
use common::{McpTestClient, StderrMode, unwrap_mcp_content};
use serde_json::json;
use sqry_core::workspace::{
    LogicalWorkspace, MemberReason, WorkspaceMemberFolder, WorkspaceMetadata, WorkspaceRegistry,
    WorkspaceRepoId, WorkspaceRepository,
};
use sqry_mcp_redaction::compute_source_root_id;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Test fixture handle returned by [`write_workspace_fixture`].
///
/// Carries the canonicalized paths for the source roots, member folder
/// and exclusion, plus the `LogicalWorkspace` loaded from the same
/// `.sqry-workspace` file the live MCP server consumes — so each test
/// can compute the **exact** `source_root_id` / `workspace_id_short`
/// tokens the redactor will emit at runtime.
struct WorkspaceFixture {
    workspace_root_str: String,
    repo_a: PathBuf,
    repo_b: PathBuf,
    member_folder: PathBuf,
    excluded: PathBuf,
    workspace: LogicalWorkspace,
}

/// Layout knob — controls whether a source root itself is registered as
/// an exclusion. The `none`-preset criterion-6 test needs an excluded
/// path that surfaces through a `path`-keyed JSON field; the cleanest
/// surface is to mark `repo_a` as both a source root **and** an
/// exclusion, which causes
/// `aggregate.source_root_statuses[*].path` for `repo_a` to flow
/// through the `redact_excluded_in_passthrough` walker hook (the
/// parent field name is `path`, which `whitelist::is_path_field`
/// recognizes). The minimal-preset / member-folder tests use the
/// "non-overlapping" layout where `exclusions` lives strictly under a
/// child directory and never collides with a source root.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExclusionLayout {
    /// `exclusions = [<repo_a>/secrets]` — used by the minimal-preset
    /// and member-folder tests where `aggregate.source_root_statuses`
    /// must render workspace-aware tokens for *non-excluded* source
    /// roots only.
    NonOverlapping,
    /// `exclusions = [<repo_a>]` — used by the `none`-preset
    /// criterion-6 test so the excluded path surfaces through
    /// `aggregate.source_root_statuses[*].path` (a `path`-keyed field
    /// whose value matches an exclusion).
    SourceRootIsExcluded,
}

/// Build the v2 `.sqry-workspace` registry on disk under `root`. Encodes
/// the multi-root + member-folder + exclusion setup the tests exercise.
///
/// `layout` selects the exclusion target — see [`ExclusionLayout`].
///
/// Each source root receives a stub `.sqry/graph/` directory so the
/// `aggregate_workspace_index_status` helper classifies it as `Missing`
/// (no `snapshot.sqry` on disk) rather than `Error` — the status string
/// never appears in the assertions, but the directories must exist for
/// the workspace registry's canonicalization to succeed.
///
/// Uses the [`WorkspaceRegistry`] Rust API so the on-disk JSON shape
/// stays locked to whatever serde defines, rather than hand-crafting
/// the JSON (which would silently rot if the schema evolves).
fn write_workspace_fixture(root: &Path, layout: ExclusionLayout) -> Result<WorkspaceFixture> {
    let repo_a = root.join("repo_a");
    let repo_b = root.join("repo_b");
    let member_folder = root.join("scripts");
    let secrets = repo_a.join("secrets");
    fs::create_dir_all(repo_a.join(".sqry/graph"))?;
    fs::create_dir_all(repo_b.join(".sqry/graph"))?;
    fs::create_dir_all(&member_folder)?;
    fs::create_dir_all(&secrets)?;
    fs::create_dir_all(root.join(".sqry/graph"))?;

    // Canonicalize so the registry stores the same identity that
    // `LogicalWorkspace::from_sqry_workspace` will canonicalize to at
    // load time — without this the source-root prefix matching in the
    // redactor's workspace-aware path renderer would fail when the
    // tempdir lives behind a symlink (e.g. `/var → /private/var` on
    // macOS).
    let canonical_workspace = root.canonicalize()?;
    let repo_a = repo_a.canonicalize()?;
    let repo_b = repo_b.canonicalize()?;
    let member_folder = member_folder.canonicalize()?;
    let secrets = secrets.canonicalize()?;

    let excluded = match layout {
        ExclusionLayout::NonOverlapping => secrets,
        ExclusionLayout::SourceRootIsExcluded => repo_a.clone(),
    };

    let mut registry = WorkspaceRegistry {
        metadata: WorkspaceMetadata {
            version: 2,
            workspace_name: Some("step7-iter4-fixture".to_string()),
            default_discovery_mode: None,
            created_at: std::time::SystemTime::now(),
            updated_at: std::time::SystemTime::now(),
        },
        repositories: vec![
            WorkspaceRepository::new(
                WorkspaceRepoId::new("repo_a"),
                "repo_a".to_string(),
                repo_a.clone(),
                repo_a.join(".sqry-index"),
                None,
            ),
            WorkspaceRepository::new(
                WorkspaceRepoId::new("repo_b"),
                "repo_b".to_string(),
                repo_b.clone(),
                repo_b.join(".sqry-index"),
                None,
            ),
        ],
        member_folders: vec![WorkspaceMemberFolder::new(
            WorkspaceRepoId::new("scripts"),
            member_folder.clone(),
            MemberReason::OperationalFolder,
        )],
        exclusions: vec![excluded.clone()],
        project_root_mode: Default::default(),
    };
    let registry_path = canonical_workspace.join(".sqry-workspace");
    registry.save(&registry_path)?;

    // Load the canonical LogicalWorkspace so each test can compute the
    // exact `workspace_id_short` / `source_root_id` tokens the redactor
    // will emit. The MCP server constructs its own
    // LogicalWorkspace from the same `.sqry-workspace` path at request
    // time, so the WorkspaceId / source-root-id derivations are
    // byte-identical.
    let workspace = LogicalWorkspace::from_sqry_workspace(&registry_path)?;

    let workspace_root_str = canonical_workspace.to_string_lossy().into_owned();
    Ok(WorkspaceFixture {
        workspace_root_str,
        repo_a,
        repo_b,
        member_folder,
        excluded,
        workspace,
    })
}

/// Build the `envs` slice forwarded to [`McpTestClient`] for the given
/// preset. Centralized so the three tests below stay in lockstep on
/// `SQRY_MCP_WORKSPACE_ROOT` + `SQRY_REDACTION_PRESET`.
fn envs_for(fixture: &WorkspaceFixture, preset: &str) -> Vec<(String, String)> {
    vec![
        (
            "SQRY_MCP_WORKSPACE_ROOT".to_string(),
            fixture.workspace_root_str.clone(),
        ),
        ("SQRY_REDACTION_PRESET".to_string(), preset.to_string()),
    ]
}

/// Issue the `workspace_status` tool call against the live MCP binary
/// with `path: "."` and return the parsed `data` payload. Centralized
/// so individual tests focus on the assertion shape, not the wire
/// boilerplate.
fn fetch_workspace_status_payload(
    fixture: &WorkspaceFixture,
    preset: &str,
) -> Result<serde_json::Value> {
    let envs = envs_for(fixture, preset);
    let mut client = McpTestClient::new_with_env_and_stderr_mode(&envs, StderrMode::Null)?;
    let _ = client.initialize()?;

    let response = client.call(
        "tools/call",
        json!({
            "name": "workspace_status",
            "arguments": { "path": "." }
        }),
        1,
    )?;

    unwrap_mcp_content(&response)
}

/// `STEP_7` codex iter4 MAJOR 1 fix — assert the live MCP response
/// renders `aggregate.source_root_statuses[*].path` as the **exact**
/// `<source_root_id>` token derived by `compute_source_root_id`, not
/// merely "some 8-hex prefix" as iter3 did. The iter3 shape probe could
/// have passed even if the redactor emitted a different 8-hex digest
/// (e.g. a leaked workspace_id prefix), leaving the criterion-4
/// contract uncovered end-to-end.
#[test]
fn minimal_preset_renders_exact_source_root_ids() -> Result<()> {
    let workspace = TempDir::new()?;
    let fixture = write_workspace_fixture(workspace.path(), ExclusionLayout::NonOverlapping)?;

    let workspace_id_short = fixture.workspace.workspace_id().as_short_hex();
    let expected_repo_a = compute_source_root_id(&workspace_id_short, &fixture.repo_a);
    let expected_repo_b = compute_source_root_id(&workspace_id_short, &fixture.repo_b);

    let payload = fetch_workspace_status_payload(&fixture, "minimal")?;

    let aggregate = payload
        .get("data")
        .and_then(|v| v.get("aggregate"))
        .and_then(|v| v.get("source_root_statuses"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("data.aggregate.source_root_statuses missing in {payload:?}")
        })?;

    assert_eq!(
        aggregate.len(),
        2,
        "fixture has two source roots; got {aggregate:?}"
    );

    // Each rendered path must exactly match `<expected_source_root_id>`
    // (the path IS the source root → empty relative). Unordered match —
    // the aggregate is sorted by source-root path order which is not
    // contractually guaranteed across platforms.
    let mut rendered: Vec<String> = aggregate
        .iter()
        .map(|entry| {
            entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("source_root_statuses[*].path missing in {entry:?}"))
        })
        .collect::<Result<_>>()?;
    rendered.sort();

    let mut expected = vec![expected_repo_a.clone(), expected_repo_b.clone()];
    expected.sort();

    assert_eq!(
        rendered,
        expected,
        "source-root paths must render as exact compute_source_root_id tokens; \
         got {rendered:?}, expected {expected:?} \
         (workspace_id_short={workspace_id_short}, \
         repo_a={}, repo_b={})",
        fixture.repo_a.display(),
        fixture.repo_b.display(),
    );

    // Defence-in-depth: each rendered token must be 8 lowercase hex
    // chars (the digest format `compute_source_root_id` emits) and
    // must NOT leak the underlying workspace path.
    for token in &rendered {
        assert_eq!(
            token.len(),
            8,
            "source_root_id is 8 hex chars; got {token:?}"
        );
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "source_root_id is lowercase hex; got {token:?}"
        );
        assert!(
            !token.contains(fixture.workspace_root_str.as_str()) && !token.starts_with('/'),
            "rendered token must not leak absolute workspace path: {token:?}"
        );
    }

    Ok(())
}

/// `STEP_7` codex iter4 MAJOR 1 fix (member-folder half) — assert the
/// live MCP response renders `member_folders[*].path` as the **exact**
/// `<workspace_id_short>` token derived from `WorkspaceId::as_short_hex`.
/// The member-folder list reports the folder root itself, so the relative
/// remainder is empty and the rendered form collapses to just
/// `<workspace_id_short>` (criterion 5, empty-relative case).
#[test]
fn minimal_preset_renders_member_folder_with_workspace_id_short_prefix() -> Result<()> {
    let workspace = TempDir::new()?;
    let fixture = write_workspace_fixture(workspace.path(), ExclusionLayout::NonOverlapping)?;
    let workspace_id_short = fixture.workspace.workspace_id().as_short_hex();

    let payload = fetch_workspace_status_payload(&fixture, "minimal")?;

    let member_folders = payload
        .get("data")
        .and_then(|v| v.get("member_folders"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("data.member_folders missing in {payload:?}"))?;

    assert_eq!(
        member_folders.len(),
        1,
        "fixture has one member folder (`<root>/scripts`); got {member_folders:?}"
    );

    let entry = &member_folders[0];
    let path_value = entry
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("member_folders[0].path missing in {entry:?}"))?;

    assert_eq!(
        path_value,
        workspace_id_short,
        "member_folder path must render as the bound workspace_id_short \
         (criterion 5, empty-relative case): got {path_value:?}, expected {workspace_id_short:?} \
         (member_folder={})",
        fixture.member_folder.display(),
    );

    // Must not contain any leak of the on-disk workspace location.
    assert!(
        !path_value.starts_with('/') && !path_value.contains(fixture.workspace_root_str.as_str()),
        "member_folder path must not leak the absolute workspace path: {path_value:?}"
    );

    Ok(())
}

/// `STEP_7` codex iter4 MAJOR 2 fix — under `SQRY_REDACTION_PRESET=none`
/// the live MCP server now constructs a passthrough `Redactor`
/// (criterion 3: non-excluded paths flow through verbatim) **bound to
/// the resolved [`LogicalWorkspace`]** so the
/// `redact_excluded_in_passthrough` walker hook can rewrite excluded
/// paths to the opaque-hash form (criterion 6 — exclusions take
/// precedence regardless of preset).
///
/// Pre-iter4 `create_redactor("none")` returned [`None`], so no redactor
/// was ever constructed and no `LogicalWorkspaceView` was ever bound
/// under passthrough — the criterion-6 contract was unenforceable end-
/// to-end. The iter3 e2e test only proved cleartext absolute paths came
/// back from `workspace_status`, which any plausible regression would
/// also satisfy.
///
/// Test setup uses [`ExclusionLayout::SourceRootIsExcluded`]: `repo_a`
/// is registered as both a source root **and** an exclusion. This puts
/// the excluded path on a surface (`aggregate.source_root_statuses[*].path`)
/// the redaction walker actually traverses — `path` is a known
/// path-bearing field whose value flows through
/// `redact_excluded_in_passthrough`. (`data.exclusions[]` is a bare
/// string array under a parent field name — `exclusions` — that is not
/// in `whitelist::PATH_FIELDS`, so the walker would not enter the
/// passthrough-exclusion branch for that surface; we deliberately do
/// not assert on `data.exclusions[]` here.)
///
/// Asserts both halves in a single MCP response:
///   1. `aggregate.source_root_statuses[*].path` for `repo_b` =
///      absolute path (criterion 3 — non-excluded paths under `none`
///      flow verbatim).
///   2. `aggregate.source_root_statuses[*].path` for `repo_a` =
///      `<excluded>/[<hash>]` (criterion 6 — excluded paths rewritten
///      to opaque hash regardless of preset).
#[test]
fn none_preset_redacts_excluded_paths_to_opaque_hash_but_passes_others() -> Result<()> {
    let workspace = TempDir::new()?;
    let fixture = write_workspace_fixture(workspace.path(), ExclusionLayout::SourceRootIsExcluded)?;
    assert_eq!(
        fixture.excluded, fixture.repo_a,
        "fixture invariant: SourceRootIsExcluded layout marks repo_a as the exclusion"
    );

    let payload = fetch_workspace_status_payload(&fixture, "none")?;

    let aggregate = payload
        .get("data")
        .and_then(|v| v.get("aggregate"))
        .and_then(|v| v.get("source_root_statuses"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("data.aggregate.source_root_statuses missing in {payload:?}")
        })?;

    assert_eq!(aggregate.len(), 2, "fixture has two source roots");

    // Bin the rendered paths into "excluded form" and "absolute
    // cleartext". The aggregate ordering is not contractually
    // guaranteed, so we classify by shape.
    let rendered_paths: Vec<String> = aggregate
        .iter()
        .map(|entry| {
            entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("source_root_statuses[*].path missing in {entry:?}"))
        })
        .collect::<Result<_>>()?;

    let excluded_form: Vec<&String> = rendered_paths
        .iter()
        .filter(|p| p.starts_with("<excluded>/["))
        .collect();
    let absolute_form: Vec<&String> = rendered_paths
        .iter()
        .filter(|p| p.starts_with('/') || p.contains(':'))
        .collect();

    assert_eq!(
        excluded_form.len(),
        1,
        "criterion 6: exactly one source-root path (the excluded one, repo_a) must render \
         as `<excluded>/[<hash>]` under preset=none; got rendered_paths={rendered_paths:?}"
    );
    assert_eq!(
        absolute_form.len(),
        1,
        "criterion 3: exactly one source-root path (the non-excluded one, repo_b) must \
         flow through as an absolute path under preset=none; got rendered_paths={rendered_paths:?}"
    );

    // Half 2 — criterion 6 shape + leak check on the excluded entry.
    let rendered_excluded = excluded_form[0];
    assert!(
        rendered_excluded.ends_with(']'),
        "criterion 6: excluded path render must terminate with `]`; got {rendered_excluded:?}"
    );
    assert!(
        !rendered_excluded.contains("repo_a"),
        "criterion 6: excluded source-root leaf `repo_a` must not survive in cleartext; \
         got {rendered_excluded:?}"
    );
    let excluded_path_str = fixture.excluded.to_string_lossy();
    assert!(
        !rendered_excluded.contains(excluded_path_str.as_ref()),
        "criterion 6: excluded absolute path must not survive in cleartext; \
         got {rendered_excluded:?} (excluded={})",
        fixture.excluded.display(),
    );
    assert!(
        !rendered_excluded.contains(fixture.workspace_root_str.as_str()),
        "criterion 6: workspace root must not leak into the rendered excluded form: \
         {rendered_excluded:?}"
    );

    // Half 1 — criterion 3 verbatim check on the non-excluded entry.
    let rendered_absolute = absolute_form[0];
    assert_eq!(
        rendered_absolute,
        &fixture.repo_b.to_string_lossy().into_owned(),
        "criterion 3: non-excluded source-root path must flow through verbatim under \
         preset=none with a bound LogicalWorkspaceView; got {rendered_absolute:?}, \
         expected {}",
        fixture.repo_b.display()
    );

    Ok(())
}

/// Belt-and-suspenders coverage — under `SQRY_REDACTION_PRESET=none`
/// with a strictly non-overlapping exclusion (excluded path under a
/// source root, not the source root itself), the source-root paths
/// emitted in `aggregate.source_root_statuses[*].path` MUST flow
/// through verbatim. Pairs with the iter1 unit-test
/// `passthrough_exclusion_paths::none_preset_preserves_non_excluded_absolute_path`
/// and asserts the symmetry holds end-to-end (criterion 3, no
/// regression from the iter4 wiring change that constructs a
/// passthrough redactor where iter3 returned `None`).
#[test]
fn none_preset_passes_through_source_root_paths_when_exclusion_is_unrelated() -> Result<()> {
    let workspace = TempDir::new()?;
    let fixture = write_workspace_fixture(workspace.path(), ExclusionLayout::NonOverlapping)?;

    let payload = fetch_workspace_status_payload(&fixture, "none")?;
    let aggregate = payload
        .get("data")
        .and_then(|v| v.get("aggregate"))
        .and_then(|v| v.get("source_root_statuses"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("data.aggregate.source_root_statuses missing in {payload:?}")
        })?;

    assert_eq!(aggregate.len(), 2, "fixture has two source roots");

    let mut rendered: Vec<String> = aggregate
        .iter()
        .map(|entry| {
            entry
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("source_root_statuses[*].path missing in {entry:?}"))
        })
        .collect::<Result<_>>()?;
    rendered.sort();

    let mut expected = vec![
        fixture.repo_a.to_string_lossy().into_owned(),
        fixture.repo_b.to_string_lossy().into_owned(),
    ];
    expected.sort();

    assert_eq!(
        rendered, expected,
        "criterion 3: non-excluded source-root paths must flow through verbatim under \
         preset=none with a bound LogicalWorkspaceView; got {rendered:?}, expected {expected:?}"
    );

    Ok(())
}
