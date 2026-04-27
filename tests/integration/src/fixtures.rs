//! Shared on-disk fixture builders for the cross-surface
//! integration tests. Every fixture writes a real `.sqry-workspace`
//! file (v2 schema) under a `tempfile::TempDir`, so the resulting
//! `LogicalWorkspace` round-trips through the same code path that
//! production callers exercise.
//!
//! The fixtures intentionally avoid coupling to any one surface
//! (CLI/LSP/MCP/daemon/extension): they hand back the [`TempDir`] +
//! the constructed [`LogicalWorkspace`] only. Each test crate decides
//! which surface(s) to drive against the fixture.

use std::fs;
use std::path::{Path, PathBuf};

use sqry_core::workspace::{
    LogicalWorkspace, MemberReason, WorkspaceMemberFolder, WorkspaceRegistry, WorkspaceRepoId,
    WorkspaceRepository,
};
use tempfile::TempDir;

/// Result of [`build_two_source_one_member_one_excluded`].
pub struct TwoPlusOnePlusOne {
    /// The owning temporary directory; drop to clean up.
    pub tmp: TempDir,
    /// Logical workspace constructed from the registry written into
    /// `tmp/.sqry-workspace`.
    pub logical: LogicalWorkspace,
    /// Canonical path to the first source root (e.g. `frontend`).
    pub source_a: PathBuf,
    /// Canonical path to the second source root (e.g. `backend`).
    pub source_b: PathBuf,
    /// Canonical path to the operational member folder (e.g.
    /// `tools/operational`). Member folders are part of the workspace
    /// but not auto-indexed.
    pub member: PathBuf,
    /// Canonical path to the excluded folder (e.g. `node_modules`).
    pub excluded: PathBuf,
    /// Path to the registry file written under `tmp/.sqry-workspace`.
    pub registry_path: PathBuf,
}

/// Build a workspace fixture with **two source roots, one operational
/// member folder, and one excluded folder**. Used by every cross-surface
/// integration test.
///
/// Layout produced (canonical paths):
///
/// ```text
/// <tmp>/
///   .sqry-workspace                 # v2 registry
///   frontend/                       # source root A
///     src/main.ts
///   backend/                        # source root B
///     src/main.rs
///   tools/operational/              # member (operational) folder
///     deploy.sh
///   node_modules/                   # excluded
///     pkg/index.js
/// ```
///
/// # Errors
///
/// Returns an error string if any filesystem operation or the
/// `LogicalWorkspace` construction fails.
pub fn build_two_source_one_member_one_excluded() -> Result<TwoPlusOnePlusOne, String> {
    let tmp = TempDir::new().map_err(|err| format!("create tempdir: {err}"))?;
    let root = tmp.path().to_path_buf();

    // Materialize the four directories with at least one file each so
    // canonicalize_path succeeds and the workspace registry sees them
    // as live folders.
    let source_a = root.join("frontend");
    let source_b = root.join("backend");
    let member = root.join("tools").join("operational");
    let excluded = root.join("node_modules");

    fs::create_dir_all(source_a.join("src")).map_err(|err| format!("mkdir source_a/src: {err}"))?;
    fs::write(
        source_a.join("src").join("main.ts"),
        "export const x = 1;\n",
    )
    .map_err(|err| format!("write source_a/src/main.ts: {err}"))?;

    fs::create_dir_all(source_b.join("src")).map_err(|err| format!("mkdir source_b/src: {err}"))?;
    fs::write(source_b.join("src").join("main.rs"), "fn main() {}\n")
        .map_err(|err| format!("write source_b/src/main.rs: {err}"))?;

    fs::create_dir_all(&member).map_err(|err| format!("mkdir member: {err}"))?;
    fs::write(
        member.join("deploy.sh"),
        "#!/usr/bin/env bash\necho deploying\n",
    )
    .map_err(|err| format!("write member/deploy.sh: {err}"))?;

    fs::create_dir_all(excluded.join("pkg")).map_err(|err| format!("mkdir excluded/pkg: {err}"))?;
    fs::write(
        excluded.join("pkg").join("index.js"),
        "module.exports = {};\n",
    )
    .map_err(|err| format!("write excluded/pkg/index.js: {err}"))?;

    // Build a v2 registry pointing at the four directories. The two
    // source roots live in `repositories`, the operational folder in
    // `member_folders` (with reason `OperationalFolder`), and the
    // excluded folder in `exclusions`.
    let mut registry = WorkspaceRegistry::new(Some("step-11-fixture".to_string()));
    let canon_source_a = canonicalize_strict(&source_a)?;
    let canon_source_b = canonicalize_strict(&source_b)?;
    let canon_member = canonicalize_strict(&member)?;
    let canon_excluded = canonicalize_strict(&excluded)?;

    registry
        .upsert_repo(WorkspaceRepository::new(
            WorkspaceRepoId::new("frontend"),
            "frontend".to_string(),
            canon_source_a.clone(),
            canon_source_a
                .join(".sqry")
                .join("graph")
                .join("manifest.json"),
            None,
        ))
        .map_err(|err| format!("upsert frontend: {err}"))?;
    registry
        .upsert_repo(WorkspaceRepository::new(
            WorkspaceRepoId::new("backend"),
            "backend".to_string(),
            canon_source_b.clone(),
            canon_source_b
                .join(".sqry")
                .join("graph")
                .join("manifest.json"),
            None,
        ))
        .map_err(|err| format!("upsert backend: {err}"))?;
    registry.member_folders.push(WorkspaceMemberFolder::new(
        WorkspaceRepoId::new("tools/operational"),
        canon_member.clone(),
        MemberReason::OperationalFolder,
    ));
    registry.exclusions.push(canon_excluded.clone());

    let registry_path = root.join(".sqry-workspace");
    registry
        .save(&registry_path)
        .map_err(|err| format!("save registry: {err}"))?;

    let logical = LogicalWorkspace::from_sqry_workspace(&registry_path)
        .map_err(|err| format!("build logical workspace: {err}"))?;

    Ok(TwoPlusOnePlusOne {
        tmp,
        logical,
        source_a: canon_source_a,
        source_b: canon_source_b,
        member: canon_member,
        excluded: canon_excluded,
        registry_path,
    })
}

/// Canonicalize a path or return a descriptive error string. We use
/// `std::fs::canonicalize` here rather than `sqry-core`'s helper to keep
/// the test crate dependency surface minimal.
fn canonicalize_strict(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|err| format!("canonicalize {}: {err}", path.display()))
}

/// Result of [`build_logical_workspace_view`]. The returned view is
/// what the MCP redactor would receive at runtime when the session
/// holds the fixture's `LogicalWorkspace`.
pub struct ViewWithFixture {
    /// The owning fixture (kept alive for the duration of the test).
    pub fixture: TwoPlusOnePlusOne,
    /// The redaction-side projection of the fixture's logical
    /// workspace. Mirrors the shape STEP_7's MCP wiring constructs.
    pub view: sqry_mcp_redaction::LogicalWorkspaceView,
}

/// Lift the `TwoPlusOnePlusOne` fixture's `LogicalWorkspace` into the
/// MCP-side `LogicalWorkspaceView` that STEP_7 binds to the redactor.
///
/// This is the projection STEP_7 ships in `sqry-mcp/src/server.rs`
/// when a `LogicalWorkspace` is resolved at session start. Reproducing
/// the projection here (rather than calling into `sqry-mcp`) keeps the
/// integration test crate decoupled from the MCP entry-point binary.
///
/// # Errors
///
/// Propagates fixture-construction errors.
pub fn build_logical_workspace_view() -> Result<ViewWithFixture, String> {
    let fixture = build_two_source_one_member_one_excluded()?;

    let workspace_id_short = fixture.logical.workspace_id().as_short_hex();
    let source_roots = fixture
        .logical
        .source_roots()
        .iter()
        .map(|root| {
            let id = sqry_mcp_redaction::compute_source_root_id(&workspace_id_short, &root.path);
            (id, root.path.clone())
        })
        .collect();
    let member_folders = fixture
        .logical
        .member_folders()
        .iter()
        .map(|m| m.path.clone())
        .collect();
    let exclusions = fixture.logical.exclusions().to_vec();

    let view = sqry_mcp_redaction::LogicalWorkspaceView {
        workspace_id_short,
        source_roots,
        member_folders,
        exclusions,
    };

    Ok(ViewWithFixture { fixture, view })
}
