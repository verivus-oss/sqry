//! Graph statistics handler for LSP.
//!
//! Provides statistics about the unified code graph.

use std::collections::HashMap;

use anyhow::{Context, Result};
use sqry_core::graph::unified::persistence::{GraphStorage, load_from_path};

use crate::protocol::{SqryGraphStatsParams, SqryGraphStatsResult};
use crate::session::SessionManager;

/// Execute graph stats query.
///
/// Returns statistics about the unified code graph.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved or no graph snapshot
/// is available.
pub fn execute(
    session: &SessionManager,
    params: &SqryGraphStatsParams,
) -> Result<SqryGraphStatsResult> {
    let root = session.resolve_path(params.path.as_deref())?;

    log::debug!("Executing graph stats for root={}", root.display());

    // Load the graph
    let storage = GraphStorage::new(&root);
    if !storage.exists() {
        anyhow::bail!(
            "No graph available at {}. Run `sqry index` first.",
            root.display()
        );
    }

    let graph = load_from_path(
        storage.snapshot_path(),
        Some(session.executor().plugin_manager()),
    )
    .with_context(|| {
        format!(
            "failed to load graph from {}",
            storage.snapshot_path().display()
        )
    })?;

    // Gather statistics
    let total_nodes = graph.node_count() as u64;
    let total_edges = graph.edge_count() as u64;
    let total_files = graph.files().len() as u64;
    let graph_epoch = graph.epoch();

    // Count nodes by kind
    let mut nodes_by_kind: HashMap<String, u64> = HashMap::new();
    for (kind, count) in graph.indices().iter_kinds() {
        let kind_str = format!("{kind:?}");
        nodes_by_kind.insert(kind_str, count as u64);
    }

    // Count files by language
    let mut files_by_language: HashMap<String, u64> = HashMap::new();
    for (_file_id, _path, language) in graph.files().iter_with_language() {
        let lang_str = language.map_or_else(|| "unknown".to_string(), |l| l.to_string());
        *files_by_language.entry(lang_str).or_insert(0) += 1;
    }

    Ok(SqryGraphStatsResult {
        total_nodes,
        total_edges,
        total_files,
        nodes_by_kind,
        files_by_language,
        graph_epoch,
    })
}
