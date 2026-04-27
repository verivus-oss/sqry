//! `mcp__sqry__workspace_status` tool — `STEP_7`.
//!
//! Returns the aggregate [`sqry_core::workspace::WorkspaceIndexStatus`]
//! for the current MCP-resolved workspace, plus the workspace identity
//! surfaces (short and full hex). Every other MCP tool resolves a single
//! per-call workspace via the session registry; `workspace_status`
//! reports the **same** resolved workspace, so a client that issued a
//! `workspace_id`-bearing tool call can call this immediately after to
//! verify the identity it observed (acceptance criterion 1: tools
//! accept optional `workspace_id`; 2: `workspace_status` returns the
//! aggregate).
//!
//! The tool reads the resolved [`LogicalWorkspace`] from the per-request
//! thread-local override populated in
//! [`crate::workspace_session::with_workspace_override`]. The MCP server
//! resolves the LogicalWorkspace once per request (registry-discovery on
//! the resolved workspace_root, with single-root fallback) and binds it
//! to the thread before dispatching. This MUST mirror the LSP
//! `sqry/workspaceStatus` wire shape one-for-one — see
//! `sqry-lsp/src/handlers/workspace_status.rs` and
//! `sqry-lsp/src/session.rs::build_workspace_status_info`.
//!
//! When the override is unbound (legacy single-root MCP entry path or
//! `LogicalWorkspace` construction failure), the tool falls back to
//! synthesising a single-source-root view from the resolved
//! `workspace_root` so the wire shape stays uniform — clients parse
//! exactly one envelope.
//!
//! # Wiring
//!
//! - Params: [`WorkspaceStatusParams`] (declared in `tools/params.rs`).
//! - Args: [`WorkspaceStatusArgs`] (declared in `tools/validation.rs`).
//! - Server hook: [`crate::server::SqryServer::workspace_status`] (added
//!   in this PR).
//! - Execution: [`execute_workspace_status`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use serde::Serialize;
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_core::workspace::{
    LogicalWorkspace, MemberFolder, MemberReason, SourceRootIndexState, SourceRootStatus,
    WorkspaceIndexStatus,
};

use crate::engine::engine_for_workspace;
use crate::execution::types::ToolExecution;
use crate::execution::utils::duration_to_ms;
use crate::workspace_session::current_logical_workspace;

/// Wire-side payload for the `workspace_status` tool.
///
/// Mirrors the LSP `sqry/workspaceStatus` shape one-for-one
/// (`workspace_id_short`, `workspace_id_full`, `aggregate`,
/// `project_root_mode`, `source_roots`, `member_folders`,
/// `exclusions`) so a single client can dual-route between LSP and MCP
/// without translating identity / aggregate fields.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceStatusData {
    /// Short BLAKE3 prefix for human-readable surfaces.
    pub workspace_id_short: String,
    /// Full BLAKE3 hex digest. Use this for any identity comparison.
    pub workspace_id_full: String,
    /// Per-source-root + summary counters (acceptance criterion 2).
    pub aggregate: WorkspaceIndexStatus,
    /// Workspace-level `project_root_mode` (string-form).
    pub project_root_mode: String,
    /// Source root paths (canonical absolute).
    pub source_roots: Vec<PathBuf>,
    /// Member folder paths + reason.
    pub member_folders: Vec<MemberFolderInfo>,
    /// Excluded paths (canonical absolute).
    pub exclusions: Vec<PathBuf>,
    /// Echoes the optional `workspace_id` request parameter, when
    /// supplied. Lets clients sanity-check the identity they expect
    /// against the identity the server resolved.
    pub requested_workspace_id: Option<String>,
}

/// Wire-side member-folder projection.
#[derive(Debug, Clone, Serialize)]
pub struct MemberFolderInfo {
    /// Canonical absolute path.
    pub path: PathBuf,
    /// Why the folder was classified as a member.
    pub reason: MemberReason,
}

/// Validated arguments for `execute_workspace_status`. Currently only
/// the optional `workspace_id` from the request hints — the workspace
/// itself is resolved by `WorkspaceSessionRegistry::resolve_for_request`
/// before this function is called, identical to every other MCP tool.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceStatusArgs {
    /// Optional client-supplied workspace identity (full 64-char hex).
    /// Currently echoed back as `requested_workspace_id` so the client
    /// can detect mismatches; future work may switch the resolver to
    /// honour it for cross-workspace tool routing.
    pub workspace_id: Option<String>,
    /// Workspace path hint, defaulting to ".". Forwarded to the engine
    /// resolver so the standard MCP path-based session resolution
    /// applies.
    pub path: String,
}

/// Execute the `workspace_status` tool against the current resolved
/// workspace. Returns a [`ToolExecution`] envelope so the standard MCP
/// metadata fields (`execution_ms`, `workspace_path`) are populated.
///
/// # Errors
///
/// Returns an error if the engine cannot resolve a workspace from the
/// current session context.
pub fn execute_workspace_status(
    args: &WorkspaceStatusArgs,
) -> Result<ToolExecution<WorkspaceStatusData>> {
    let start = Instant::now();
    let resolved = if args.path == "." {
        None
    } else {
        Some(PathBuf::from(&args.path))
    };
    let engine = engine_for_workspace(resolved.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    // STEP_7 MAJOR 2 fix: read the resolved LogicalWorkspace from the
    // per-request thread-local set by `with_workspace_override`. This
    // surfaces the real multi-root structure (.sqry-workspace registry
    // when present, single_root fallback otherwise), mirroring the LSP
    // `sqry/workspaceStatus` shape rather than fabricating a single-root
    // view from the engine's workspace_root alone. When the override is
    // unbound (legacy entry paths or single_root construction failure)
    // we synthesize the same single-root view as before so the wire
    // contract holds.
    let workspace_arc = match current_logical_workspace() {
        Some(arc) => arc,
        None => Arc::new(
            LogicalWorkspace::single_root(workspace_root.clone()).map_err(|err| {
                anyhow::anyhow!(
                    "Failed to build single-root LogicalWorkspace for {}: {err}",
                    workspace_root.display()
                )
            })?,
        ),
    };

    let data = build_status(workspace_arc.as_ref(), args.workspace_id.clone());

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: false,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(1),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(&workspace_root),
    })
}

/// Build a `WorkspaceStatusData` from a `LogicalWorkspace` reference,
/// computing the on-disk aggregate fresh on every call. Symbol counts
/// are intentionally `None` — surfaces that need them route through
/// `get_index_status` for the per-root manifest read.
fn build_status(workspace: &LogicalWorkspace, requested: Option<String>) -> WorkspaceStatusData {
    let aggregate = aggregate_workspace_index_status(workspace);
    let source_roots = workspace
        .source_roots()
        .iter()
        .map(|r| r.path.clone())
        .collect();
    let member_folders = workspace
        .member_folders()
        .iter()
        .map(|m: &MemberFolder| MemberFolderInfo {
            path: m.path.clone(),
            reason: m.reason,
        })
        .collect();

    WorkspaceStatusData {
        workspace_id_short: workspace.workspace_id().as_short_hex(),
        workspace_id_full: workspace.workspace_id().as_full_hex(),
        aggregate,
        project_root_mode: workspace.project_root_mode().to_string(),
        source_roots,
        member_folders,
        exclusions: workspace.exclusions().to_vec(),
        requested_workspace_id: requested,
    }
}

/// Per-source-root aggregate computation, byte-for-byte identical to
/// `sqry_lsp::session::aggregate_workspace_index_status`. Inlined here
/// because the LSP helper is not on a shared dependency path; folding
/// it into `sqry-core` is a follow-on refactor outside `STEP_7`'s scope.
fn aggregate_workspace_index_status(workspace: &LogicalWorkspace) -> WorkspaceIndexStatus {
    let mut entries = Vec::with_capacity(workspace.source_roots().len());
    for source_root in workspace.source_roots() {
        let storage = GraphStorage::new(&source_root.path);
        let snapshot_path = storage.snapshot_path();
        let lock_path = source_root.path.join(".sqry/graph/build.lock");
        let lock_present = lock_path.is_file();

        let (status, last_indexed_at) = if lock_present {
            (SourceRootIndexState::Building, None)
        } else if !storage.exists() || !storage.snapshot_exists() {
            (SourceRootIndexState::Missing, None)
        } else {
            match std::fs::metadata(snapshot_path) {
                Ok(meta) => (SourceRootIndexState::Ok, meta.modified().ok()),
                Err(_) => (SourceRootIndexState::Error, None),
            }
        };

        entries.push(SourceRootStatus {
            path: source_root.path.clone(),
            status,
            last_indexed_at,
            symbol_count: None,
            // STEP_11_4 — surface auto-populated
            // `SourceRoot.classpath_dir` through MCP per-root status.
            classpath_dir: source_root.classpath_dir.clone(),
        });
    }
    WorkspaceIndexStatus::from_source_root_statuses(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn build_status_matches_logical_workspace_identity() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".sqry/graph")).unwrap();
        let workspace = LogicalWorkspace::single_root(tmp.path().to_path_buf()).unwrap();
        let data = build_status(&workspace, Some("client-supplied".to_string()));

        assert_eq!(data.workspace_id_short.len(), 16);
        assert_eq!(data.workspace_id_full.len(), 64);
        assert!(
            data.workspace_id_full
                .starts_with(data.workspace_id_short.as_str())
        );
        assert_eq!(
            data.requested_workspace_id.as_deref(),
            Some("client-supplied")
        );
        assert_eq!(data.source_roots.len(), 1);
        assert_eq!(data.member_folders.len(), 0);
        assert_eq!(data.exclusions.len(), 0);
        assert_eq!(data.aggregate.source_root_statuses.len(), 1);
    }

    /// `STEP_7` MAJOR 2 fix coverage: a multi-root LogicalWorkspace
    /// (the shape the LSP `sqry/workspaceStatus` reports) MUST surface
    /// every source root in the response. Pre-fix, `workspace_status`
    /// fabricated `LogicalWorkspace::single_root(workspace_root)` and
    /// always reported one source root. The fix threads the resolved
    /// LogicalWorkspace through the per-request thread-local; this
    /// test pins `build_status` against an explicit multi-root input
    /// and asserts the full structure (source roots count, aggregate
    /// per-source-root statuses) survives the rendering.
    #[test]
    fn build_status_surfaces_multi_root_structure() {
        let tmp = TempDir::new().unwrap();
        let root_a = tmp.path().join("repo_a");
        let root_b = tmp.path().join("repo_b");
        std::fs::create_dir_all(root_a.join(".sqry/graph")).unwrap();
        std::fs::create_dir_all(root_b.join(".sqry/graph")).unwrap();
        let workspace =
            LogicalWorkspace::anonymous_multi_root(vec![root_a.clone(), root_b.clone()]).unwrap();

        let data = build_status(&workspace, None);

        assert_eq!(
            data.source_roots.len(),
            2,
            "multi-root workspace must surface every source root"
        );
        assert_eq!(
            data.aggregate.source_root_statuses.len(),
            2,
            "aggregate WorkspaceIndexStatus must report one entry per source root"
        );
        assert!(data.workspace_id_full.starts_with(&data.workspace_id_short));
    }
}
