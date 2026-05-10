//! Index tools execution.
//!
//! This module implements index-related tools:
//! - `index_status`: Reports on the current state of the sqry unified graph
//! - `rebuild_index`: Triggers a rebuild of the unified graph index

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use sqry_core::graph::unified::build::BuildConfig;
use sqry_core::graph::unified::persistence::{GraphStorage, Manifest, load_header_from_path};
use sqry_plugin_registry::create_plugin_manager;

use crate::engine::{canonicalize_in_workspace, engine_for_workspace};
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
fn resolve_workspace_path(path: &str) -> Option<PathBuf> {
    if path == "." {
        None
    } else {
        Some(PathBuf::from(path))
    }
}
pub fn execute_index_status(args: &GetIndexStatusArgs) -> Result<ToolExecution<IndexStatusData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let target = canonicalize_in_workspace(&args.path, &workspace_root)?;

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

/// Execute the `rebuild_index` tool to rebuild the unified graph index.
///
/// This triggers a full rebuild of the unified graph (`.sqry/graph/`) for the given path.
/// The operation scans all source files and rebuilds the graph from scratch.
#[allow(clippy::too_many_lines)]
pub fn execute_rebuild_index(args: &RebuildIndexArgs) -> Result<ToolExecution<RebuildIndexData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let target = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Determine the root directory for indexing
    let root_path = if target.is_dir() {
        target.clone()
    } else if let Some(parent) = target.parent() {
        parent.to_path_buf()
    } else {
        workspace_root.clone()
    };

    tracing::info!(path = %root_path.display(), force = args.force, "Executing rebuild_index tool");

    // Check if index exists and we're not forcing rebuild
    let storage = GraphStorage::new(&root_path);
    if storage.exists() && !args.force {
        // Load existing manifest to return current status
        let manifest = storage
            .load_manifest()
            .context("Index exists but manifest is unreadable")?;

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
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(
                engine.workspace_root(),
            ),
        });
    }

    // Cluster-E §E.3 — refuse to create a nested `.sqry/` if an outer
    // project already has its own graph. MCP exposes the `force` field
    // as the explicit opt-in: the user has already declared intent to
    // create/rebuild here, so when `force=true` we proceed even if a
    // parent project graph exists. Without `force`, we surface the
    // recovery text from `NestedIndexError` so the caller can route to
    // the correct workspace.
    if !storage.exists()
        && let Err(e) = sqry_core::workspace::assert_no_ancestor_graph(&root_path, args.force)
    {
        anyhow::bail!("{e}");
    }

    // Build, persist, and analyze using the shared pipeline
    let plugins = create_plugin_manager();
    let build_config = BuildConfig::default();
    let (_graph, build_result) = sqry_core::graph::unified::build::build_and_persist_graph(
        &root_path,
        &plugins,
        &build_config,
        "mcp:rebuild_index",
    )
    .context("Failed to build and persist unified graph")?;

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
            root_path: crate::execution::symbol_utils::path_to_forward_slash(&root_path),
            node_count,
            edge_count,
            files_indexed,
            built_at: build_result.built_at,
            message: Some("Index rebuilt successfully.".to_string()),
        },
        used_index: false,
        used_graph: true,
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
