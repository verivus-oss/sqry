//! `mcp__sqry__workspace_status` tool — `STEP_7`.
//!
//! Returns an MCP-local aggregate projection
//! ([`WorkspaceStatusAggregate`]) for the current MCP-resolved
//! workspace, plus the workspace identity surfaces (short and full
//! hex). Every other MCP tool resolves a single per-call workspace via
//! the session registry; `workspace_status` reports the **same**
//! resolved workspace, so a client that issued a `workspace_id`-bearing
//! tool call can call this immediately after to verify the identity it
//! observed (acceptance criterion 1: tools accept optional
//! `workspace_id`; 2: `workspace_status` returns the aggregate).
//!
//! The tool reads the resolved [`LogicalWorkspace`] from the per-request
//! thread-local override populated in
//! [`crate::workspace_session::with_workspace_override`]. The MCP server
//! resolves the LogicalWorkspace once per request (registry-discovery on
//! the resolved workspace_root, with single-root fallback) and binds it
//! to the thread before dispatching.
//!
//! # Relationship to the LSP `sqry/workspaceStatus` shape
//!
//! Identity, `project_root_mode`, `source_roots`, `member_folders`, and
//! `exclusions` stay field-for-field compatible with the LSP envelope
//! (`sqry-lsp/src/handlers/workspace_status.rs`,
//! `sqry-lsp/src/session.rs::build_workspace_status_info`). The
//! `aggregate` field intentionally diverges (#299): MCP projects the
//! core [`sqry_core::workspace::WorkspaceIndexStatus`] into the
//! MCP-local [`WorkspaceStatusAggregate`], whose per-root entries carry
//! an explicit opaque [`WorkspaceStatusSourceRoot::source_root_id`] and
//! no `path` field. The core type stays the LSP/CLI wire shape.
//! Embedding it here was the #299 root cause: the output redactor
//! rewrote the per-root `path` field into an opaque source-root ID, and
//! clients reasonably treated that token as a path prefix for follow-up
//! tool calls. Counts, `generated_at`, `warnings`, per-root `status`,
//! `last_indexed_at`, `symbol_count`, and `classpath_dir` survive the
//! projection unchanged, including their wire encodings.
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
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Serialize, Serializer};
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_core::workspace::{
    LogicalWorkspace, MemberFolder, MemberReason, SourceRootIndexState, SourceRootStatus,
    WorkspaceIndexStatus, WorkspaceWarning,
};
use sqry_mcp_redaction::compute_source_root_id;

use crate::engine::engine_for_workspace;
use crate::execution::types::ToolExecution;
use crate::execution::utils::duration_to_ms;
use crate::workspace_session::current_logical_workspace;

/// Wire-side payload for the `workspace_status` tool.
///
/// Identity and structure fields (`workspace_id_short`,
/// `workspace_id_full`, `project_root_mode`, `source_roots`,
/// `member_folders`, `exclusions`) mirror the LSP
/// `sqry/workspaceStatus` shape, so a client can dual-route between
/// LSP and MCP for those surfaces. The `aggregate` field is the
/// deliberate exception (#299): MCP serves the local
/// [`WorkspaceStatusAggregate`] projection, which identifies each
/// source root by an opaque [`WorkspaceStatusSourceRoot::source_root_id`]
/// rather than the core per-root `path` field the LSP/CLI aggregate
/// carries.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceStatusData {
    /// Short BLAKE3 prefix for human-readable surfaces.
    pub workspace_id_short: String,
    /// Full BLAKE3 hex digest. Use this for any identity comparison.
    pub workspace_id_full: String,
    /// MCP-local per-source-root + summary counters (acceptance
    /// criterion 2). Projected from the core
    /// [`WorkspaceIndexStatus`]; see [`WorkspaceStatusAggregate`].
    pub aggregate: WorkspaceStatusAggregate,
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

/// MCP-local aggregate projection of the core
/// [`WorkspaceIndexStatus`] (#299).
///
/// Counts, `generated_at`, and `warnings` are copied verbatim from the
/// core aggregate (same wire encodings, including the millisecond
/// timestamp form and the skip-empty `warnings` behaviour). The
/// per-root entries are projected into [`WorkspaceStatusSourceRoot`],
/// which replaces the core `path` field with an explicit opaque
/// `source_root_id`. The core type remains the LSP/CLI wire shape;
/// only the MCP response uses this projection.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceStatusAggregate {
    /// Per-source-root entries, preserving the core aggregate's
    /// deterministic order (sorted by the underlying source-root path).
    pub source_root_statuses: Vec<WorkspaceStatusSourceRoot>,
    /// Number of source roots whose snapshot file is missing.
    pub missing_count: u32,
    /// Number of source roots currently being rebuilt.
    pub building_count: u32,
    /// Number of source roots reporting `ok`.
    pub ok_count: u32,
    /// Number of source roots reporting `error`.
    pub error_count: u32,
    /// Wall-clock time at which the core aggregate was computed.
    /// Serialized as milliseconds since the UNIX epoch, the same
    /// encoding the core `WorkspaceIndexStatus` emits.
    #[serde(serialize_with = "serialize_millis")]
    pub generated_at: SystemTime,
    /// Non-fatal warnings copied from the core aggregate. Mirrors the
    /// core serde shape: absent from the wire form when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<WorkspaceWarning>,
}

/// MCP-local per-source-root status entry (#299).
///
/// Identifies the source root by an opaque `source_root_id` instead of
/// the core `path` field. The ID is a display/correlation token, NOT a
/// filesystem path: clients must never prefix tool path arguments with
/// it. Path-taking MCP tools accept workspace-relative paths (for
/// example `src/lib.rs`) or normal filesystem paths.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceStatusSourceRoot {
    /// Opaque 8-lowercase-hex display identifier, derived via
    /// [`sqry_mcp_redaction::compute_source_root_id`] from
    /// `workspace_id_short` and the real source-root path. Matches the
    /// per-source-root prefix the minimal-preset redactor emits in
    /// other tools' redacted path fields, so clients can correlate the
    /// two surfaces.
    pub source_root_id: String,
    /// One-word machine-readable status, unchanged from the core
    /// per-root entry.
    pub status: SourceRootIndexState,
    /// Last-indexed timestamp for the source root, if available.
    /// Serialized as milliseconds since the UNIX epoch (or `null`),
    /// the same encoding the core entry emits.
    #[serde(serialize_with = "serialize_millis_option")]
    pub last_indexed_at: Option<SystemTime>,
    /// Cached symbol count for the source root, if available.
    pub symbol_count: Option<u64>,
    /// JVM classpath directory for this source root, if populated.
    /// This is a real filesystem path (not an opaque ID) and is
    /// redacted by the standard output redactor like any other path
    /// value. Mirrors the core serde shape: absent when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classpath_dir: Option<PathBuf>,
}

/// Serialize a [`SystemTime`] as milliseconds since the UNIX epoch,
/// byte-identical to `sqry-core`'s private `workspace::serde_time`
/// encoding. Duplicated locally (serialize half only) because the core
/// module is private and the #299 projection must not change the
/// `generated_at` / `last_indexed_at` wire forms.
fn serialize_millis<S>(time: &SystemTime, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let millis = time
        .duration_since(UNIX_EPOCH)
        .map_err(|err| serde::ser::Error::custom(format!("time occurs before UNIX_EPOCH: {err}")))?
        .as_millis();
    serializer.serialize_u128(millis)
}

/// Option-wrapping companion to [`serialize_millis`], mirroring
/// `sqry-core`'s `workspace::serde_time::option` encoding
/// (`Some(millis)` or `null`).
#[allow(clippy::ref_option)] // Serde `serialize_with` requires `&Option<T>`.
fn serialize_millis_option<S>(time: &Option<SystemTime>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match time {
        Some(value) => serializer.serialize_some(
            &value
                .duration_since(UNIX_EPOCH)
                .map_err(|err| {
                    serde::ser::Error::custom(format!("time occurs before UNIX_EPOCH: {err}"))
                })?
                .as_millis(),
        ),
        None => serializer.serialize_none(),
    }
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
    let workspace_id_short = workspace.workspace_id().as_short_hex();
    let aggregate = project_aggregate(
        &workspace_id_short,
        aggregate_workspace_index_status(workspace),
    );
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
        workspace_id_short,
        workspace_id_full: workspace.workspace_id().as_full_hex(),
        aggregate,
        project_root_mode: workspace.project_root_mode().to_string(),
        source_roots,
        member_folders,
        exclusions: workspace.exclusions().to_vec(),
        requested_workspace_id: requested,
    }
}

/// Project the core [`WorkspaceIndexStatus`] into the MCP-local
/// [`WorkspaceStatusAggregate`] (#299).
///
/// Pure in-memory translation: no graph or filesystem IO beyond the
/// aggregate build the caller already performed. Per-root entries keep
/// the core aggregate's deterministic order (sorted by source-root
/// path) and every count/status/timestamp/classpath field is copied
/// verbatim. The only change is identity: each entry's `path` is
/// replaced with the opaque `source_root_id` derived via
/// [`compute_source_root_id`], the same formula the redaction pipeline
/// uses for per-source-root path prefixes
/// (`crate::server::logical_workspace_to_view`).
fn project_aggregate(
    workspace_id_short: &str,
    aggregate: WorkspaceIndexStatus,
) -> WorkspaceStatusAggregate {
    let source_root_statuses = aggregate
        .source_root_statuses
        .into_iter()
        .map(|entry| WorkspaceStatusSourceRoot {
            source_root_id: compute_source_root_id(workspace_id_short, &entry.path),
            status: entry.status,
            last_indexed_at: entry.last_indexed_at,
            symbol_count: entry.symbol_count,
            classpath_dir: entry.classpath_dir,
        })
        .collect();
    WorkspaceStatusAggregate {
        source_root_statuses,
        missing_count: aggregate.missing_count,
        building_count: aggregate.building_count,
        ok_count: aggregate.ok_count,
        error_count: aggregate.error_count,
        generated_at: aggregate.generated_at,
        warnings: aggregate.warnings,
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
    use std::path::Path;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Craft a core per-root entry for pure `project_aggregate` tests.
    fn core_entry(
        path: &Path,
        status: SourceRootIndexState,
        last_indexed_at: Option<SystemTime>,
        symbol_count: Option<u64>,
        classpath_dir: Option<PathBuf>,
    ) -> SourceRootStatus {
        SourceRootStatus {
            path: path.to_path_buf(),
            status,
            last_indexed_at,
            symbol_count,
            classpath_dir,
        }
    }

    /// #299 U02 — every projected `source_root_id` must equal
    /// `compute_source_root_id(workspace_id_short, <real root path>)`,
    /// in the core aggregate's deterministic (path-sorted) order.
    #[test]
    fn projection_source_root_ids_match_compute_source_root_id() {
        let tmp = TempDir::new().unwrap();
        let root_a = tmp.path().join("repo_a");
        let root_b = tmp.path().join("repo_b");
        std::fs::create_dir_all(root_a.join(".sqry/graph")).unwrap();
        std::fs::create_dir_all(root_b.join(".sqry/graph")).unwrap();
        let workspace =
            LogicalWorkspace::anonymous_multi_root(vec![root_a.clone(), root_b.clone()]).unwrap();

        let data = build_status(&workspace, None);

        // Recompute the expected IDs from the core aggregate's sorted
        // order so the assertion also pins ordering determinism.
        let core = aggregate_workspace_index_status(&workspace);
        assert_eq!(core.source_root_statuses.len(), 2);
        let expected: Vec<String> = core
            .source_root_statuses
            .iter()
            .map(|entry| compute_source_root_id(&data.workspace_id_short, &entry.path))
            .collect();
        let projected: Vec<String> = data
            .aggregate
            .source_root_statuses
            .iter()
            .map(|entry| entry.source_root_id.clone())
            .collect();
        assert_eq!(
            projected, expected,
            "projected source_root_id values must match compute_source_root_id in core order"
        );
        for id in &projected {
            assert_eq!(id.len(), 8, "source_root_id is 8 hex chars: {id:?}");
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "source_root_id is lowercase hex: {id:?}"
            );
        }
    }

    /// #299 U01 — counts, statuses, timestamps, symbol counts,
    /// classpath metadata, and warnings survive the projection
    /// verbatim, and entries keep the core (path-sorted) order.
    #[test]
    fn projection_preserves_counts_statuses_and_warnings() {
        let indexed_at = UNIX_EPOCH + Duration::from_millis(1_717_000_000_123);
        // Deliberately unsorted input: from_source_root_statuses sorts
        // by path, and the projection must preserve that order.
        let entries = vec![
            core_entry(
                Path::new("/ws/zeta"),
                SourceRootIndexState::Missing,
                None,
                None,
                None,
            ),
            core_entry(
                Path::new("/ws/alpha"),
                SourceRootIndexState::Ok,
                Some(indexed_at),
                Some(42),
                Some(PathBuf::from("/ws/alpha/.sqry/classpath")),
            ),
            core_entry(
                Path::new("/ws/mid"),
                SourceRootIndexState::Building,
                None,
                None,
                None,
            ),
        ];
        let mut core = WorkspaceIndexStatus::from_source_root_statuses(entries);
        core.push_warning(WorkspaceWarning::ClasspathProbeFailed {
            source_root: PathBuf::from("/ws/alpha"),
            detail: "probe failed".to_string(),
        });
        let core_generated_at = core.generated_at;

        let projected = project_aggregate("0123456789abcdef", core.clone());

        assert_eq!(projected.missing_count, 1);
        assert_eq!(projected.building_count, 1);
        assert_eq!(projected.ok_count, 1);
        assert_eq!(projected.error_count, 0);
        assert_eq!(projected.generated_at, core_generated_at);
        assert_eq!(projected.warnings, core.warnings);
        assert_eq!(projected.source_root_statuses.len(), 3);

        // Core order is path-sorted: alpha, mid, zeta.
        let alpha = &projected.source_root_statuses[0];
        assert_eq!(
            alpha.source_root_id,
            compute_source_root_id("0123456789abcdef", Path::new("/ws/alpha"))
        );
        assert_eq!(alpha.status, SourceRootIndexState::Ok);
        assert_eq!(alpha.last_indexed_at, Some(indexed_at));
        assert_eq!(alpha.symbol_count, Some(42));
        assert_eq!(
            alpha.classpath_dir.as_deref(),
            Some(Path::new("/ws/alpha/.sqry/classpath"))
        );
        let mid = &projected.source_root_statuses[1];
        assert_eq!(mid.status, SourceRootIndexState::Building);
        let zeta = &projected.source_root_statuses[2];
        assert_eq!(zeta.status, SourceRootIndexState::Missing);
        assert_eq!(zeta.last_indexed_at, None);
        assert_eq!(zeta.symbol_count, None);
        assert_eq!(zeta.classpath_dir, None);
    }

    /// #299 FR-2 — the serialized per-root entry has no `path` field,
    /// and no field whose name contains `path` carries the opaque
    /// source-root ID. `classpath_dir` (a real filesystem path) is the
    /// legitimate `path`-substring field the assertion must
    /// distinguish from an opaque ID carrier.
    #[test]
    fn projection_serializes_no_path_named_opaque_id_field() {
        let entries = vec![core_entry(
            Path::new("/ws/alpha"),
            SourceRootIndexState::Ok,
            None,
            None,
            Some(PathBuf::from("/ws/alpha/.sqry/classpath")),
        )];
        let core = WorkspaceIndexStatus::from_source_root_statuses(entries);
        let projected = project_aggregate("0123456789abcdef", core);
        let json = serde_json::to_value(&projected).expect("aggregate serializes");

        let statuses = json
            .get("source_root_statuses")
            .and_then(serde_json::Value::as_array)
            .expect("source_root_statuses array");
        assert_eq!(statuses.len(), 1);
        for entry in statuses {
            let obj = entry.as_object().expect("entry is an object");
            assert!(
                !obj.contains_key("path"),
                "per-root entry must not serialize a `path` field: {entry:?}"
            );
            let id = obj
                .get("source_root_id")
                .and_then(serde_json::Value::as_str)
                .expect("source_root_id present");
            for (key, value) in obj {
                if key.contains("path") {
                    // Only legitimate filesystem-path fields may contain
                    // `path` in their name; they must not carry the
                    // opaque source-root ID.
                    assert_ne!(
                        value.as_str(),
                        Some(id),
                        "field {key:?} must not carry the opaque source_root_id"
                    );
                }
            }
        }
    }

    /// #299 NFR-3 — non-diverged wire encodings survive the
    /// projection: `generated_at` / `last_indexed_at` serialize as
    /// epoch-milliseconds numbers exactly like the core aggregate, and
    /// empty `warnings` stays absent from the wire form.
    #[test]
    fn projection_wire_encodings_match_core_aggregate() {
        let indexed_at = UNIX_EPOCH + Duration::from_millis(1_717_000_000_123);
        let entries = vec![
            core_entry(
                Path::new("/ws/alpha"),
                SourceRootIndexState::Ok,
                Some(indexed_at),
                None,
                None,
            ),
            core_entry(
                Path::new("/ws/zeta"),
                SourceRootIndexState::Missing,
                None,
                None,
                None,
            ),
        ];
        let core = WorkspaceIndexStatus::from_source_root_statuses(entries);
        let core_json = serde_json::to_value(&core).expect("core serializes");
        let projected = project_aggregate("0123456789abcdef", core);
        let projected_json = serde_json::to_value(&projected).expect("projection serializes");

        assert_eq!(
            projected_json.get("generated_at"),
            core_json.get("generated_at"),
            "generated_at wire form must match the core encoding"
        );
        assert_eq!(
            projected_json["source_root_statuses"][0]["last_indexed_at"],
            serde_json::json!(1_717_000_000_123_u64),
            "last_indexed_at wire form is epoch milliseconds"
        );
        assert_eq!(
            projected_json["source_root_statuses"][1]["last_indexed_at"],
            serde_json::Value::Null,
            "absent last_indexed_at serializes as null, matching core"
        );
        assert!(
            projected_json.get("warnings").is_none(),
            "empty warnings must be skipped on the wire, matching core"
        );
        assert!(
            core_json.get("warnings").is_none(),
            "core empty warnings are skipped (shape baseline)"
        );
    }

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
            "MCP aggregate projection must report one entry per source root"
        );
        assert!(data.workspace_id_full.starts_with(&data.workspace_id_short));
    }
}
