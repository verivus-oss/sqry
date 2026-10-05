//! Index tools execution.
//!
//! This module implements index-related tools:
//! - `index_status`: Reports on the current state of the sqry unified graph
//! - `rebuild_index`: Triggers a rebuild of the unified graph index

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use sqry_core::graph::unified::build::{BuildConfig, MacroOptionsError};
use sqry_core::graph::unified::persistence::{GraphStorage, Manifest, load_header_from_path};
use sqry_plugin_registry::{
    BuildWithRosterError, PluginSelectionConfig, PluginSelectionError, UnreadableManifestPolicy,
};

use crate::error::{ExpandCacheOrigin, RpcError};

use crate::engine::{canonicalize_in_workspace_enforced, engine_for_workspace, workspace_root_for};
use crate::tools::{GetIndexStatusArgs, RebuildIndexArgs};

use crate::execution::types::{IndexStatusData, RebuildIndexData, ToolExecution};
use crate::execution::utils::duration_to_ms;

/// Execute the `index_status` tool to report on graph state.
///
/// Reports on the unified graph (`.sqry/graph/`) status for the given path.
/// Resolve workspace path from args.path parameter.
///
/// If path is "." (default), returns None to trigger discovery.
/// Otherwise returns Some(path) for explicit workspace resolution.
fn resolve_workspace_path(path: &str) -> anyhow::Result<Option<PathBuf>> {
    // Issue #394: resolve a subdirectory `path` to its owning workspace instead
    // of failing as if it were a workspace root. Subtree scoping for list/scan
    // tools is applied separately via `resolve_workspace_scope`.
    crate::execution::workspace_scope::resolve_workspace_selector_enforced(path)
}
/// Execute the `get_index_status` tool.
///
/// # Errors
///
/// Returns an error if workspace resolution, path canonicalization, or index
/// status aggregation fails.
pub fn execute_index_status(args: &GetIndexStatusArgs) -> Result<ToolExecution<IndexStatusData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path)?;
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let target = canonicalize_in_workspace_enforced(&args.path, &workspace_root)?;

    let mut candidate_roots = Vec::new();
    if target.is_dir() {
        candidate_roots.push(target.clone());
    } else if let Some(parent) = target.parent() {
        candidate_roots.push(parent.to_path_buf());
    }
    if !candidate_roots
        .iter()
        .any(|p| p == workspace_root.as_path())
    {
        candidate_roots.push(workspace_root.clone());
    }

    tracing::debug!(path = %args.path, "Executing index_status tool");

    // Find the first directory with a unified graph
    let mut graph_state: Option<(PathBuf, Manifest)> = None;
    for root in candidate_roots {
        let storage = GraphStorage::new(&root);
        if !storage.exists() {
            continue;
        }
        if let Ok(manifest) = storage.load_manifest() {
            graph_state = Some((root, manifest));
            break;
        }
    }

    let (data, used_graph_flag) = match graph_state {
        Some((root, manifest)) => {
            // Get file count: prefer snapshot header (fast), fallback to manifest (CLI-built indexes)
            let storage = GraphStorage::new(&root);
            let files_indexed: Option<u64> =
                if let Ok(header) = load_header_from_path(storage.snapshot_path()) {
                    // Read from snapshot header (always accurate)
                    header.file_count.try_into().ok()
                } else if !manifest.file_count.is_empty() {
                    // Fallback: sum manifest file counts (CLI-built indexes)
                    manifest.file_count.values().sum::<usize>().try_into().ok()
                } else {
                    // No file count available
                    None
                };

            (
                IndexStatusData {
                    has_index: true,
                    root_path: Some(crate::execution::symbol_utils::path_to_forward_slash(&root)),
                    indexed_symbols: manifest.node_count.try_into().ok(),
                    files_indexed,
                    index_version: Some(format!(
                        "{}.{}",
                        manifest.schema_version, manifest.snapshot_format_version
                    )),
                    // built_at is already RFC3339 format
                    created_at: Some(manifest.built_at.clone()),
                    // Unified graphs are rebuilt, not incrementally updated
                    updated_at: Some(manifest.built_at),
                    has_relations: Some(manifest.edge_count > 0),
                },
                true,
            )
        }
        None => (
            IndexStatusData {
                has_index: false,
                root_path: Some(crate::execution::symbol_utils::path_to_forward_slash(
                    &target,
                )),
                indexed_symbols: None,
                files_indexed: None,
                index_version: None,
                created_at: None,
                updated_at: None,
                has_relations: None,
            },
            false,
        ),
    };

    tracing::debug!(has_index = data.has_index, "index_status tool completed");

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: used_graph_flag,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(1),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(
            engine.workspace_root(),
        ),
    })
}

/// The wire refusal for a plugin roster `root`'s manifest cannot give, in
/// the envelope the daemon-hosted MCP gives the same refusal (the daemon's
/// `map_selection_error` then `daemon_err_to_mcp`): an id this binary did
/// not compile is `workspace_incompatible_graph`, an unreadable manifest is
/// `workspace_not_ready` naming the file and the repair command, and any
/// other selection failure is a failed build.
fn roster_refusal(root: &Path, err: &PluginSelectionError) -> RpcError {
    #[allow(deprecated)]
    match err {
        PluginSelectionError::UnknownPluginIdsCtx { .. }
        | PluginSelectionError::UnknownPluginIds { .. } => {
            RpcError::workspace_incompatible_graph(root, &err.to_string())
        }
        PluginSelectionError::ManifestUnreadable {
            manifest_path,
            reason,
        } => RpcError::workspace_manifest_unreadable(root, manifest_path, reason),
        other => {
            RpcError::workspace_build_failed(root, &format!("plugin selection failed: {other}"))
        }
    }
}

/// The wire refusal for macro options `root` cannot resolve, in the
/// envelope the daemon-hosted MCP gives the same refusal (the daemon's
/// `map_macro_options_err` then `daemon_err_to_mcp`): a directory it
/// cannot use is `rebuild_macro_options_unavailable`, described by its
/// shape with the remedy for where it came from (`origin`: the request or
/// the record), whose reset arguments echo `requested_path`, the request's
/// own `path`; an unrecordable or empty requested directory is an invalid
/// argument carrying the resolver's message; an unreadable manifest is
/// `workspace_not_ready`.
fn macro_options_refusal(
    root: &Path,
    err: MacroOptionsError,
    origin: ExpandCacheOrigin,
    requested_path: &str,
) -> RpcError {
    match err {
        MacroOptionsError::ExpandCacheMissing { dir } => {
            RpcError::rebuild_macro_options_unavailable(root, &dir, origin)
                .with_reset_path(requested_path)
        }
        refused @ (MacroOptionsError::ExpandCachePathNotUtf8 { .. }
        | MacroOptionsError::ExpandCacheEmpty
        | MacroOptionsError::RecordedCfgFlagInvalid { .. }) => {
            RpcError::invalid_argument(format!("rebuild of {} refused: {refused}", root.display()))
        }
        MacroOptionsError::ManifestUnreadable {
            manifest_path,
            reason,
        } => RpcError::workspace_manifest_unreadable(root, &manifest_path, &reason),
    }
}

/// The wire refusal for a build the registry helper did not produce. A
/// refused request (the roster, the request check, the macro options)
/// carries its own envelope; a build or persistence failure is a failed
/// build whose reason renders the whole error chain, so no context hides
/// the cause.
fn rebuild_refusal(
    root: &Path,
    err: BuildWithRosterError,
    origin: ExpandCacheOrigin,
    requested_path: &str,
) -> RpcError {
    match err {
        BuildWithRosterError::Selection(err) => roster_refusal(root, &err),
        BuildWithRosterError::MacroRequest(err) => {
            RpcError::invalid_argument(format!("rebuild of {} refused: {err}", root.display()))
        }
        BuildWithRosterError::MacroOptions(err) => {
            macro_options_refusal(root, err, origin, requested_path)
        }
        // An index removed after the persist took its lock is a refusal
        // (decision D-i8-6), not a retryable failed build.
        BuildWithRosterError::Build(err)
            if err.chain().any(|cause| {
                cause.is::<sqry_core::graph::unified::persistence::IndexRemovedDuringPersist>()
            }) =>
        {
            RpcError::workspace_index_removed(root, &crate::error::render_error_chain(&err))
        }
        BuildWithRosterError::Build(err) => {
            RpcError::workspace_build_failed(root, &crate::error::render_error_chain(&err))
        }
    }
}

/// Execute the `rebuild_index` tool to rebuild the unified graph index.
///
/// This triggers a full rebuild of the unified graph (`.sqry/graph/`) for the given path.
/// The operation scans all source files and rebuilds the graph from scratch.
/// Without `force`, an index on disk (its manifest and its snapshot) is
/// reported instead; a manifest whose snapshot is gone is no index, so it is
/// built, as the daemon host builds it.
///
/// Every refusal is an [`RpcError`] in the envelope (code, message and
/// `data`) the daemon-hosted MCP gives the same request, so a client sees
/// the same answer whichever host serves it: macro arguments beside
/// `force=false` over an existing index (manifest and snapshot), a manifest naming an uncompiled
/// plugin id or one that cannot be read on the `force=false` leg, and every
/// macro options refusal. A forced (or first) build over an unreadable
/// manifest is not a refusal: it falls back, logs, and records the fallback
/// (surface parity W1, design D9).
///
/// # Errors
///
/// Returns an error if workspace resolution, path canonicalization, graph
/// rebuilding, or persistence fails, and the [`RpcError`] refusals above.
#[allow(clippy::too_many_lines)]
pub fn execute_rebuild_index(args: &RebuildIndexArgs) -> Result<ToolExecution<RebuildIndexData>> {
    let start = Instant::now();
    // A `path` that names no workspace (missing, or relative and under no
    // root) is the request's argument refused, as the daemon-hosted MCP
    // refuses it: an invalid argument, not an internal error.
    let unresolved = |err: anyhow::Error| {
        RpcError::workspace_unresolved(
            "rebuild_index",
            &err.context(format!("`path` {:?} names no workspace", args.path)),
        )
    };
    let workspace_path = resolve_workspace_path(&args.path).map_err(unresolved)?;
    // The root, not an engine: a cached engine's freshness refresh would
    // refuse an unreadable manifest before the rebuild that may replace it.
    let workspace_root = workspace_root_for(workspace_path.as_ref()).map_err(unresolved)?;
    let target =
        canonicalize_in_workspace_enforced(&args.path, &workspace_root).map_err(unresolved)?;

    // Determine the root directory for indexing
    let root_path = if target.is_dir() {
        target.clone()
    } else if let Some(parent) = target.parent() {
        parent.to_path_buf()
    } else {
        workspace_root.clone()
    };

    tracing::info!(path = %root_path.display(), force = args.force, "Executing rebuild_index tool");

    // Check if index exists and we're not forcing rebuild. An index is its
    // manifest and its snapshot: a manifest whose snapshot is gone is no
    // index a load could read (the read paths refuse it as not indexed), so
    // it is not reported as one; the call goes on to build it, keeping the
    // recorded selection, as the daemon host does (round 7).
    let storage = GraphStorage::new(&root_path);
    if storage.exists() && !args.force {
        let snapshot_present = storage.snapshot_exists();
        // Surface parity W4 (W4-D8): a macro option given beside
        // `force=false` over an existing index would reach no build, so it
        // is refused instead of being dropped on the floor. Over a manifest
        // with no snapshot the build below runs and applies it.
        let macro_request = args.macro_request();
        if snapshot_present && !macro_request.is_empty() {
            return Err(RpcError::macro_options_need_force(&root_path).into());
        }
        // Surface parity W1 round 3 (design D17): classify the manifest
        // through the same resolver the read path uses before reporting
        // an existing index. A manifest naming an id this binary did not
        // compile is refused by name and an unreadable manifest by file
        // (the registry's own Display); the resolved roster is dropped,
        // since this leg builds nothing.
        sqry_plugin_registry::resolve_workspace_roster(
            &root_path,
            &PluginSelectionConfig::default(),
        )
        .map(drop)
        .map_err(|err| roster_refusal(&root_path, &err))?;
        // Load existing manifest to return current status. After the
        // resolve above this arm is reached only if the file changed
        // between the two reads; it is the defence, not the rule, and it
        // refuses the way the resolver does. Without `force` the build
        // below may replace only a manifest this call can read.
        let manifest = storage.load_manifest().map_err(|err| {
            RpcError::workspace_manifest_unreadable(
                &root_path,
                storage.manifest_path(),
                &err.to_string(),
            )
        })?;
        if !snapshot_present {
            return build_and_report(args, &root_path, &workspace_root, start);
        }

        // Get file count from snapshot header (fast, no full graph load)
        let files_indexed: u64 = if let Ok(header) = load_header_from_path(storage.snapshot_path())
        {
            header.file_count.try_into().unwrap_or(0)
        } else if !manifest.file_count.is_empty() {
            // Fallback: sum manifest file counts (CLI-built indexes)
            manifest
                .file_count
                .values()
                .sum::<usize>()
                .try_into()
                .unwrap_or(0)
        } else {
            0
        };

        return Ok(ToolExecution {
            data: RebuildIndexData {
                success: true,
                root_path: crate::execution::symbol_utils::path_to_forward_slash(&root_path),
                node_count: manifest.node_count.try_into().unwrap_or(0),
                edge_count: manifest.edge_count.try_into().unwrap_or(0),
                files_indexed,
                built_at: manifest.built_at,
                message: Some("Index already exists. Use force=true to rebuild.".to_string()),
            },
            used_index: false,
            used_graph: true,
            graph_metadata: None,
            execution_ms: duration_to_ms(start.elapsed()),
            next_page_token: None,
            total: Some(1),
            truncated: Some(false),
            candidates_scanned: None,
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(&workspace_root),
        });
    }

    build_and_report(args, &root_path, &workspace_root, start)
}

/// The build leg of [`execute_rebuild_index`]: refuse a nested index when
/// none exists at the root, then build, persist and report. The message
/// says "built" for a call without `force` and "rebuilt" for one with it,
/// as the daemon host says.
fn build_and_report(
    args: &RebuildIndexArgs,
    root_path: &Path,
    workspace_root: &Path,
    start: Instant,
) -> Result<ToolExecution<RebuildIndexData>> {
    let storage = GraphStorage::new(root_path);
    // Cluster-E §E.3 — refuse to create a nested `.sqry/` if an outer
    // project already has its own graph. MCP exposes the `force` field
    // as the explicit opt-in: the user has already declared intent to
    // create/rebuild here, so when `force=true` we proceed even if a
    // parent project graph exists. Without `force`, the refusal names the
    // ancestor index and `force` (not the CLI's `--allow-nested`), as an
    // invalid argument the caller can act on.
    if !storage.exists() {
        match sqry_core::workspace::assert_no_ancestor_graph(root_path, args.force) {
            Ok(()) => {}
            Err(sqry_core::workspace::NestedIndexError::AncestorExists {
                requested,
                ancestor_graph,
                boundary,
            }) => {
                return Err(
                    RpcError::nested_index_refused(&requested, &ancestor_graph, &boundary).into(),
                );
            }
        }
    }

    // Build, persist, and analyze through the one registry helper every
    // persisting site uses (surface parity W1 round 2, D8): the roster
    // comes from the workspace manifest (fast-path fallback for a
    // brand-new index), so a forced rebuild over an `include_all` index
    // keeps `json` and its recorded `high_cost_mode` instead of narrowing
    // the manifest to the fast path. This is an explicit rebuild (`force`,
    // or no index yet), so a manifest that exists but cannot be read
    // resolves to the fallback, is logged, and the fallback is recorded
    // (D9); a readable manifest naming an uncompiled plugin id is refused.
    let built = sqry_plugin_registry::build_and_persist_with_workspace_roster(
        root_path,
        &PluginSelectionConfig::default(),
        UnreadableManifestPolicy::FallBack,
        "mcp:rebuild_index",
        &BuildConfig::default(),
        &args.macro_request(),
        sqry_core::progress::no_op_reporter(),
    )
    .map_err(|err| {
        rebuild_refusal(
            root_path,
            err,
            ExpandCacheOrigin::of(&args.macro_request()),
            &args.path,
        )
    })?;
    if let Some(unreadable) = &built.roster.unreadable_manifest {
        tracing::warn!(
            manifest = %unreadable.manifest_path.display(),
            reason = %unreadable.reason,
            "manifest unreadable; rebuilt with the fallback roster and recorded it"
        );
    }
    let build_result = built.build_result;

    let node_count: u64 = build_result.node_count.try_into().unwrap_or(0);
    let edge_count: u64 = build_result.edge_count.try_into().unwrap_or(0);
    let files_indexed: u64 = build_result.total_files.try_into().unwrap_or(0);

    tracing::info!(
        node_count,
        edge_count,
        "rebuild_index tool completed successfully"
    );

    Ok(ToolExecution {
        data: RebuildIndexData {
            success: true,
            root_path: crate::execution::symbol_utils::path_to_forward_slash(root_path),
            node_count,
            edge_count,
            files_indexed,
            built_at: build_result.built_at,
            message: Some(
                if args.force {
                    "Index rebuilt successfully."
                } else {
                    "Index built successfully."
                }
                .to_string(),
            ),
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(1),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

#[cfg(test)]
mod index_removed_refusal_tests {
    use super::*;

    /// Decision D-i8-6 (fifth audit, item 3): a persist the core refuses
    /// because the index directory was removed after it took the lock
    /// (`IndexRemovedDuringPersist`, under whatever context the helper
    /// adds) is the same refusal the daemon-hosted MCP gives: code `-32603`,
    /// kind `workspace_not_ready`, not retryable. Before, it was a failed
    /// build, retryable after 2000 ms.
    #[test]
    fn an_index_removed_during_the_persist_is_a_refusal_not_a_failed_build() {
        let root = Path::new("/ws");
        let removed = anyhow::Error::new(
            sqry_core::graph::unified::persistence::IndexRemovedDuringPersist::new(
                &root.join(".sqry/graph"),
            ),
        )
        .context("durable graph persistence transaction failed");
        let rpc = rebuild_refusal(
            root,
            BuildWithRosterError::Build(removed),
            ExpandCacheOrigin::Recorded,
            "/ws",
        );
        assert_eq!(rpc.code, -32603);
        assert_eq!(rpc.kind, "workspace_not_ready");
        assert!(
            !rpc.retryable,
            "an index removed during the persist is not retryable: {rpc:?}"
        );
        assert_eq!(rpc.retry_after_ms, None);
        assert!(
            rpc.message.contains("was removed during the persist"),
            "{}",
            rpc.message
        );
    }
}
