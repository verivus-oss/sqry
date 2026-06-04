//! `STEP_7` end-to-end response-redaction coverage.
//!
//! Drives the live `sqry-mcp` binary against a temporary
//! `SQRY_MCP_WORKSPACE_ROOT` containing a v2 `.sqry-workspace` registry
//! with two source roots, one member folder, and one exclusion. Asserts
//! the wire form of the `workspace_status` MCP response — the
//! `source_root_id` aggregate identifiers (#299), `member_folders[*].path`
//! redaction, and the `source_roots[]` cleartext carrier — against the
//! **exact** workspace-aware tokens specified by acceptance criteria 3-6,
//! recomputed in-test from the same canonical [`compute_source_root_id`] /
//! `workspace_id_short` derivations the redactor uses at runtime — not
//! generic shape probes.
//!
//! Coverage matrix (acceptance criteria → test):
//!
//! | Criterion | Preset    | Field surface                                    | Test                                                                    |
//! |-----------|-----------|--------------------------------------------------|-------------------------------------------------------------------------|
//! | 4 + #299 AC-1/AC-3 | `minimal` | `aggregate.source_root_statuses[*].source_root_id` | `minimal_preset_renders_exact_source_root_ids`                  |
//! | 5         | `minimal` | `member_folders[*].path` (registry-listed)       | `minimal_preset_renders_member_folder_with_workspace_id_short_prefix`   |
//! | 3 + 6     | `none`    | `member_folders[*].path` (excluded → opaque hash) + `source_roots[]` (cleartext carrier) | `none_preset_redacts_excluded_member_folder_to_opaque_hash`  |
//! | 3 + #299 FR-7 | `none` | `aggregate.source_root_statuses[*].source_root_id` + `source_roots[]` (cleartext carrier) | `none_preset_keeps_source_root_id_aggregate_and_cleartext_source_roots` |
//! | #299 FR-7 | `none`    | `source_roots[]` carrier for an EXCLUDED source root | `none_preset_source_roots_carrier_ignores_exclusions`               |
//!
//! #299 changed the `workspace_status` aggregate from the core
//! `WorkspaceIndexStatus` (whose per-root `path` field the redactor
//! rewrote into an opaque token that clients mistook for a path prefix)
//! to an MCP-local projection whose per-root entries carry an explicit
//! opaque `source_root_id` and NO `path` field, under every preset.
//! Consequences for this file:
//!
//! - The minimal-preset source-root assertion targets
//!   `source_root_id` (still the byte-exact `compute_source_root_id`
//!   token) and additionally asserts the `path` field is gone and no
//!   `path`-named field carries the opaque ID. The test FAILS on the
//!   old path-only shape.
//! - The aggregate no longer carries a path-keyed value, so the
//!   criterion-6 (exclusions override passthrough) e2e surface for this
//!   tool moved to `member_folders[*].path` — exercised by excluding
//!   the registry-listed member folder.
//! - Under `SQRY_REDACTION_PRESET=none` the documented cleartext
//!   source-root carrier is top-level `data.source_roots[]`: the walker
//!   only rewrites path-keyed fields (`source_roots` is not in
//!   `whitelist::PATH_FIELDS` / `WORKSPACE_FIELDS`) and the `none`
//!   preset disables in-string path detection, so canonical absolute
//!   source-root paths flow through verbatim — including (pre-existing
//!   behavior) for source roots that are also registered as exclusions.

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

/// Layout knob — controls which registry path is registered as an
/// exclusion. The `none`-preset criterion-6 test needs an excluded path
/// that surfaces through a `path`-keyed JSON field; since #299 removed
/// the per-root `path` field from `aggregate.source_root_statuses`, the
/// remaining `path`-keyed surface in the `workspace_status` response is
/// `member_folders[*].path`, so the criterion-6 layout marks the
/// registry-listed member folder as the exclusion. The minimal-preset /
/// member-folder tests use the "non-overlapping" layout where
/// `exclusions` lives strictly under a child directory.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExclusionLayout {
    /// `exclusions = [<repo_a>/secrets]` — used by the minimal-preset
    /// and member-folder tests where no registry-listed surface is
    /// excluded.
    NonOverlapping,
    /// `exclusions = [<repo_a>]` — used by the `none`-preset FR-7
    /// carrier test documenting that top-level `source_roots[]` flows
    /// through in cleartext even when the source root is also an
    /// exclusion (the walker rewrites only `path`-keyed fields).
    SourceRootIsExcluded,
    /// `exclusions = [<member_folder>]` — used by the `none`-preset
    /// criterion-6 test so the excluded path surfaces through
    /// `member_folders[*].path` (a `path`-keyed field whose value
    /// matches an exclusion) and flows through the
    /// `redact_excluded_in_passthrough` walker hook.
    MemberFolderIsExcluded,
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
    // #299 — give repo_b a classpath directory so the aggregate entry
    // carries `classpath_dir`, the one legitimate `path`-substring field
    // in `source_root_statuses[*]`. The no-opaque-ID-under-a-path-name
    // assertions must distinguish this real filesystem path from an
    // opaque source-root ID.
    fs::create_dir_all(repo_b.join(".sqry/classpath"))?;
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
        ExclusionLayout::MemberFolderIsExcluded => member_folder.clone(),
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

/// Extract `data.aggregate.source_root_statuses` from a
/// `workspace_status` payload.
fn aggregate_entries(payload: &serde_json::Value) -> Result<&Vec<serde_json::Value>> {
    payload
        .get("data")
        .and_then(|v| v.get("aggregate"))
        .and_then(|v| v.get("source_root_statuses"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            anyhow::anyhow!("data.aggregate.source_root_statuses missing in {payload:?}")
        })
}

/// #299 shared response-shape assertion — under EVERY preset the
/// per-root aggregate entries must:
///
/// 1. carry `source_root_id` equal to the exact
///    [`compute_source_root_id`] token for the underlying root
///    (unordered match across the fixture's two roots);
/// 2. be 8 lowercase hex chars and leak no absolute path;
/// 3. serialize NO `path` field (this is the assertion that FAILS on
///    the pre-#299 shape, where the opaque ID appeared only under
///    `path`);
/// 4. carry no field whose name contains `path` and whose value is one
///    of the opaque source-root IDs. `classpath_dir` (populated for
///    `repo_b` by the fixture) is the legitimate `path`-substring field
///    the assertion distinguishes by value.
fn assert_aggregate_uses_source_root_ids(
    payload: &serde_json::Value,
    fixture: &WorkspaceFixture,
) -> Result<()> {
    let workspace_id_short = fixture.workspace.workspace_id().as_short_hex();
    let id_repo_a = compute_source_root_id(&workspace_id_short, &fixture.repo_a);
    let id_repo_b = compute_source_root_id(&workspace_id_short, &fixture.repo_b);
    let mut expected = vec![id_repo_a.clone(), id_repo_b.clone()];
    expected.sort();

    let entries = aggregate_entries(payload)?;
    assert_eq!(
        entries.len(),
        2,
        "fixture has two source roots; got {entries:?}"
    );

    let mut rendered: Vec<String> = Vec::with_capacity(entries.len());
    // Track the `classpath_dir` presence per source root so we can prove
    // (below) that the one legitimate `path`-substring field actually
    // survives serialization on the live binary path — the fixture gives
    // repo_b a `.sqry/classpath/` directory and leaves repo_a without one
    // (Grok U03 LOW: the no-opaque-ID loop alone would pass even if
    // `classpath_dir` were dropped).
    let mut repo_b_classpath_present = false;
    let mut repo_a_classpath_present = false;
    for entry in entries {
        let obj = entry.as_object().ok_or_else(|| {
            anyhow::anyhow!("source_root_statuses entry is not an object: {entry:?}")
        })?;

        // (3) The pre-#299 shape carried the opaque ID under `path`;
        // the projected shape must not serialize `path` at all.
        assert!(
            !obj.contains_key("path"),
            "#299: source_root_statuses entries must not carry a `path` field; got {entry:?}"
        );

        let id = obj
            .get("source_root_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!("source_root_statuses[*].source_root_id missing in {entry:?}")
            })?;

        // (2) Shape + leak checks.
        assert_eq!(id.len(), 8, "source_root_id is 8 hex chars; got {id:?}");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "source_root_id is lowercase hex; got {id:?}"
        );
        assert!(
            !id.contains(fixture.workspace_root_str.as_str()) && !id.starts_with('/'),
            "source_root_id must not leak absolute workspace path: {id:?}"
        );

        // (4) No `path`-named field may carry an opaque source-root ID.
        // `classpath_dir` is allowed to exist; its value must be a real
        // (possibly redacted) filesystem path string, never one of the
        // opaque IDs.
        for (key, value) in obj {
            if key.contains("path")
                && let Some(text) = value.as_str()
            {
                assert!(
                    !expected.contains(&text.to_string()),
                    "#299: field {key:?} must not carry an opaque source-root ID; got {text:?}"
                );
            }
        }

        // (5) Record whether this entry carries a non-null
        // `classpath_dir` (the legitimate `path`-substring field).
        let classpath_present = obj.get("classpath_dir").is_some_and(|v| !v.is_null());
        if id == id_repo_b {
            repo_b_classpath_present = classpath_present;
        } else if id == id_repo_a {
            repo_a_classpath_present = classpath_present;
        }

        rendered.push(id.to_string());
    }
    rendered.sort();

    // (1) Byte-exact ID match, unordered across roots.
    assert_eq!(
        rendered,
        expected,
        "source_root_id values must be exact compute_source_root_id tokens; \
         got {rendered:?}, expected {expected:?} \
         (workspace_id_short={workspace_id_short}, repo_a={}, repo_b={})",
        fixture.repo_a.display(),
        fixture.repo_b.display(),
    );

    // (5 cont.) The fixture wires `repo_b/.sqry/classpath/`, so the
    // repo_b entry MUST carry a non-null `classpath_dir` on the wire
    // (proving the legitimate `path`-substring field survives the MCP
    // projection + redaction live), while repo_a (no classpath dir) must
    // omit it (`skip_serializing_if = "Option::is_none"`).
    assert!(
        repo_b_classpath_present,
        "repo_b has a .sqry/classpath dir; its aggregate entry must carry a non-null classpath_dir"
    );
    assert!(
        !repo_a_classpath_present,
        "repo_a has no classpath dir; its aggregate entry must omit classpath_dir"
    );

    Ok(())
}

/// Extract top-level `data.source_roots` as strings.
fn source_roots_field(payload: &serde_json::Value) -> Result<Vec<String>> {
    payload
        .get("data")
        .and_then(|v| v.get("source_roots"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("data.source_roots missing in {payload:?}"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!("source_roots entry is not a string: {v:?}"))
        })
        .collect()
}

/// `STEP_7` codex iter4 MAJOR 1 fix, retargeted by #299 — assert the
/// live MCP response renders the **exact** `compute_source_root_id`
/// tokens under the explicit `source_root_id` field, with no `path`
/// field anywhere in the per-root entries. Fails on the pre-#299 shape
/// (opaque ID only under `path`) twice over: the `path` key is
/// forbidden and `source_root_id` is required.
#[test]
fn minimal_preset_renders_exact_source_root_ids() -> Result<()> {
    let workspace = TempDir::new()?;
    let fixture = write_workspace_fixture(workspace.path(), ExclusionLayout::NonOverlapping)?;

    let payload = fetch_workspace_status_payload(&fixture, "minimal")?;
    assert_aggregate_uses_source_root_ids(&payload, &fixture)?;

    // Under minimal redaction no cleartext source-root path may appear
    // in the per-root entries (NFR-4): every string value in each entry
    // must avoid the absolute fixture paths.
    for entry in aggregate_entries(&payload)? {
        let rendered = entry.to_string();
        assert!(
            !rendered.contains(fixture.workspace_root_str.as_str()),
            "minimal preset must not leak the workspace root in {rendered}"
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

/// `STEP_7` codex iter4 MAJOR 2 fix, retargeted by #299 — under
/// `SQRY_REDACTION_PRESET=none` the live MCP server constructs a
/// passthrough `Redactor` **bound to the resolved [`LogicalWorkspace`]**
/// so the `redact_excluded_in_passthrough` walker hook rewrites excluded
/// paths to the opaque-hash form (criterion 6 — exclusions take
/// precedence regardless of preset).
///
/// #299 removed the `path` field from `aggregate.source_root_statuses`,
/// so that surface no longer carries a path-keyed value the hook can
/// rewrite. The criterion-6 e2e surface for `workspace_status` is now
/// `member_folders[*].path` — a `path`-keyed field
/// (`whitelist::is_path_field`) whose value flows through the hook.
/// Test setup uses [`ExclusionLayout::MemberFolderIsExcluded`]: the
/// registry-listed member folder is also the exclusion.
///
/// Asserts in a single MCP response:
///   1. `member_folders[0].path` = `<excluded>/[<hash>]` (criterion 6 —
///      excluded paths rewritten to opaque hash regardless of preset).
///   2. Per-root aggregate entries still use exact `source_root_id`
///      tokens with no `path` field (#299 FR-7: preset-independent
///      aggregate shape).
///   3. Non-excluded source roots flow through `data.source_roots[]`
///      verbatim (criterion 3).
#[test]
fn none_preset_redacts_excluded_member_folder_to_opaque_hash() -> Result<()> {
    let workspace = TempDir::new()?;
    let fixture =
        write_workspace_fixture(workspace.path(), ExclusionLayout::MemberFolderIsExcluded)?;
    assert_eq!(
        fixture.excluded, fixture.member_folder,
        "fixture invariant: MemberFolderIsExcluded layout marks the member folder as the exclusion"
    );

    let payload = fetch_workspace_status_payload(&fixture, "none")?;

    // (1) Criterion 6 — the excluded member folder renders as
    // `<excluded>/[<hash>]` with no cleartext survivor.
    let member_folders = payload
        .get("data")
        .and_then(|v| v.get("member_folders"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("data.member_folders missing in {payload:?}"))?;
    assert_eq!(
        member_folders.len(),
        1,
        "fixture has one member folder; got {member_folders:?}"
    );
    let rendered_excluded = member_folders[0]
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("member_folders[0].path missing in {member_folders:?}"))?;
    assert!(
        rendered_excluded.starts_with("<excluded>/[") && rendered_excluded.ends_with(']'),
        "criterion 6: excluded member-folder path must render as `<excluded>/[<hash>]` \
         under preset=none; got {rendered_excluded:?}"
    );
    assert!(
        !rendered_excluded.contains("scripts"),
        "criterion 6: excluded member-folder leaf `scripts` must not survive in cleartext; \
         got {rendered_excluded:?}"
    );
    assert!(
        !rendered_excluded.contains(fixture.workspace_root_str.as_str()),
        "criterion 6: workspace root must not leak into the rendered excluded form: \
         {rendered_excluded:?}"
    );

    // (2) #299 FR-7 — the aggregate keeps the explicit source_root_id
    // shape under `none`, identical to the minimal-preset shape.
    assert_aggregate_uses_source_root_ids(&payload, &fixture)?;

    // (3) Criterion 3 — non-excluded source roots flow through the
    // top-level cleartext carrier verbatim.
    let mut roots = source_roots_field(&payload)?;
    roots.sort();
    let mut expected_roots = vec![
        fixture.repo_a.to_string_lossy().into_owned(),
        fixture.repo_b.to_string_lossy().into_owned(),
    ];
    expected_roots.sort();
    assert_eq!(
        roots, expected_roots,
        "criterion 3: non-excluded source roots must flow through data.source_roots[] \
         verbatim under preset=none"
    );

    Ok(())
}

/// #299 FR-7 carrier documentation — under `SQRY_REDACTION_PRESET=none`
/// the top-level `data.source_roots[]` carrier passes through in
/// cleartext EVEN when a source root is also registered as an
/// exclusion. The walker's passthrough-exclusion hook only fires for
/// `path`-keyed fields (`whitelist::PATH_FIELDS` / `WORKSPACE_FIELDS`),
/// and `source_roots` is neither; the `none` preset also disables
/// in-string path detection. This is pre-existing behavior that #299
/// does not change — this test pins it so the documented cleartext
/// carrier statement in the MCP docs stays true to the wire.
///
/// The aggregate, by contrast, stays on the preset-independent
/// `source_root_id` shape for both roots, INCLUDING the excluded one.
#[test]
fn none_preset_source_roots_carrier_ignores_exclusions() -> Result<()> {
    let workspace = TempDir::new()?;
    let fixture = write_workspace_fixture(workspace.path(), ExclusionLayout::SourceRootIsExcluded)?;
    assert_eq!(
        fixture.excluded, fixture.repo_a,
        "fixture invariant: SourceRootIsExcluded layout marks repo_a as the exclusion"
    );

    let payload = fetch_workspace_status_payload(&fixture, "none")?;

    // The aggregate shape is unaffected by the exclusion overlap: both
    // roots (excluded repo_a included) render as exact source_root_id
    // tokens with no `path` field.
    assert_aggregate_uses_source_root_ids(&payload, &fixture)?;

    // The cleartext carrier ignores exclusions: both canonical absolute
    // source-root paths flow through, including the excluded repo_a.
    let mut roots = source_roots_field(&payload)?;
    roots.sort();
    let mut expected_roots = vec![
        fixture.repo_a.to_string_lossy().into_owned(),
        fixture.repo_b.to_string_lossy().into_owned(),
    ];
    expected_roots.sort();
    assert_eq!(
        roots, expected_roots,
        "FR-7: data.source_roots[] is the cleartext source-root carrier under preset=none \
         and is not rewritten for excluded roots (pre-existing walker behavior)"
    );

    Ok(())
}

/// #299 FR-7 + criterion 3 — under `SQRY_REDACTION_PRESET=none` with a
/// strictly non-overlapping exclusion (excluded path under a source
/// root, not a registry-listed surface itself):
///
/// 1. The per-root aggregate entries keep the explicit
///    `source_root_id` shape — the aggregate is preset-independent and
///    never carries cleartext or a `path` field.
/// 2. The documented cleartext source-root carrier is top-level
///    `data.source_roots[]`, which flows through verbatim (criterion 3
///    passthrough; pairs with the iter1 unit-test
///    `passthrough_exclusion_paths::none_preset_preserves_non_excluded_absolute_path`).
#[test]
fn none_preset_keeps_source_root_id_aggregate_and_cleartext_source_roots() -> Result<()> {
    let workspace = TempDir::new()?;
    let fixture = write_workspace_fixture(workspace.path(), ExclusionLayout::NonOverlapping)?;

    let payload = fetch_workspace_status_payload(&fixture, "none")?;

    // (1) Preset-independent aggregate shape.
    assert_aggregate_uses_source_root_ids(&payload, &fixture)?;

    // (2) Cleartext carrier under `none` is data.source_roots[].
    let mut roots = source_roots_field(&payload)?;
    roots.sort();
    let mut expected = vec![
        fixture.repo_a.to_string_lossy().into_owned(),
        fixture.repo_b.to_string_lossy().into_owned(),
    ];
    expected.sort();
    assert_eq!(
        roots, expected,
        "criterion 3 / FR-7: source roots must flow through data.source_roots[] verbatim \
         under preset=none with a bound LogicalWorkspaceView; got {roots:?}, expected {expected:?}"
    );

    // (3) Positive classpath_dir wire proof (Grok U03 open question 1):
    // exactly one aggregate entry (repo_b, whose fixture carries
    // `.sqry/classpath/`) serializes `classpath_dir`, and under `none`
    // the value is the cleartext directory — a real filesystem path,
    // visibly distinct from the opaque 8-hex source_root_id the
    // shared assertion already validated.
    let with_classpath: Vec<&serde_json::Value> = aggregate_entries(&payload)?
        .iter()
        .filter(|entry| entry.get("classpath_dir").is_some())
        .collect();
    assert_eq!(
        with_classpath.len(),
        1,
        "exactly one source root (repo_b) carries a classpath_dir; got {with_classpath:?}"
    );
    let classpath_dir = with_classpath[0]
        .get("classpath_dir")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("classpath_dir is not a string"))?;
    assert_eq!(
        classpath_dir,
        fixture
            .repo_b
            .join(".sqry/classpath")
            .to_string_lossy()
            .as_ref(),
        "classpath_dir must be the cleartext directory under preset=none"
    );

    Ok(())
}
