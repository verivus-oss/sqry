//! Graph export handler for LSP.
//!
//! Exports dependency graphs in various formats (JSON, DOT, D2, Mermaid).

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::traversal::EdgeClassification;
use sqry_core::graph::unified::{
    EdgeFilter, TraversalConfig, TraversalDirection, TraversalLimits, traverse,
};
use sqry_core::visualization::unified::{
    D2Config, DotConfig, MermaidConfig, UnifiedD2Exporter, UnifiedDotExporter,
    UnifiedMermaidExporter,
};

use crate::handlers::graph_common::find_nodes_by_name;
use crate::protocol::{SqryGraphEdge, SqryGraphExportParams, SqryGraphExportResult, SqryGraphNode};
use crate::session::SessionManager;

/// Default maximum depth for traversal
const DEFAULT_MAX_DEPTH: usize = 2;

/// Default maximum results
const DEFAULT_MAX_RESULTS: usize = 1000;

/// Execute graph export.
///
/// Exports dependency graph around seed symbols in various formats.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, inputs are invalid,
/// or the graph is unavailable.
pub fn execute(
    session: &SessionManager,
    params: &SqryGraphExportParams,
) -> Result<SqryGraphExportResult> {
    let root = session.resolve_path(params.path.as_deref())?;

    // Validate that we have at least one seed
    if params.file_path.is_none() && params.symbol_name.is_none() {
        anyhow::bail!("Either file_path or symbol_name must be provided");
    }

    let max_depth = params.max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
    let max_results = params.max_results.unwrap_or(DEFAULT_MAX_RESULTS);
    let include_calls = params.include_calls.unwrap_or(true);
    let include_imports = params.include_imports.unwrap_or(false);
    let verbose = params.verbose.unwrap_or(false);

    log::debug!(
        "Exporting graph: file={:?}, symbol={:?}, format={}, max_depth={}, root={}",
        params.file_path,
        params.symbol_name,
        params.format,
        max_depth,
        root.display()
    );

    // Get graph snapshot
    let graph = session
        .graph()?
        .ok_or_else(|| anyhow::anyhow!("No graph available. Run `sqry index` first."))?;

    let snapshot = graph.snapshot();

    // Collect seed nodes
    let seeds = collect_seeds(&snapshot, params, &root)?;

    // Collect nodes and edges via BFS traversal
    let (nodes, edges, truncated) = collect_graph_data(
        &snapshot,
        &seeds,
        max_depth,
        max_results,
        include_calls,
        include_imports,
    );

    let total_nodes = nodes.len();
    let total_edges = edges.len();

    // Render if non-JSON format requested
    let rendered = render_graph(&params.format, &graph, verbose);

    Ok(SqryGraphExportResult {
        nodes,
        edges,
        total_nodes,
        total_edges,
        rendered,
        truncated,
    })
}

/// Collect seed node IDs from file path or symbol name.
fn collect_seeds(
    snapshot: &GraphSnapshot,
    params: &SqryGraphExportParams,
    workspace_root: &Path,
) -> Result<Vec<NodeId>> {
    let mut seeds = Vec::new();
    let files = snapshot.files();

    // Collect from file_path
    if let Some(ref file_path) = params.file_path {
        let target_path = workspace_root.join(file_path);
        let relative_path = target_path
            .strip_prefix(workspace_root)
            .unwrap_or(&target_path);

        for (node_id, entry) in snapshot.iter_nodes() {
            // Gate 0d iter-2 fix: skip unified losers. See
            // `NodeEntry::is_unified_loser`.
            if entry.is_unified_loser() {
                continue;
            }
            if let Some(node_file) = files.resolve(entry.file)
                && node_file.as_ref() == relative_path
            {
                seeds.push(node_id);
            }
        }
    }

    // Collect from symbol_name
    if let Some(ref symbol_name) = params.symbol_name {
        let matches = find_nodes_by_name(snapshot, symbol_name);
        seeds.extend(matches);
    }

    if seeds.is_empty() {
        anyhow::bail!("No seed symbols found for graph export");
    }

    Ok(seeds)
}

/// Collect graph nodes and edges via BFS traversal using the kernel.
fn collect_graph_data(
    snapshot: &GraphSnapshot,
    seeds: &[NodeId],
    max_depth: usize,
    max_results: usize,
    include_calls: bool,
    include_imports: bool,
) -> (Vec<SqryGraphNode>, Vec<SqryGraphEdge>, bool) {
    let config = TraversalConfig {
        direction: TraversalDirection::Outgoing,
        edge_filter: EdgeFilter {
            include_calls,
            include_imports,
            include_references: false,
            include_inheritance: false,
            include_structural: false,
            include_type_edges: false,
            include_database: false,
            include_service: false,
            cross_boundary: None,
        },
        limits: TraversalLimits {
            max_depth: u32::try_from(max_depth).unwrap_or(u32::MAX),
            max_nodes: None,
            max_edges: Some(max_results),
            max_paths: None,
        },
    };

    let result = traverse(snapshot, seeds, &config, None);
    let truncated = result.metadata.truncation.is_some();

    // Convert MaterializedNode → SqryGraphNode
    let mut node_vec: Vec<SqryGraphNode> = result
        .nodes
        .iter()
        .map(|mat| SqryGraphNode {
            name: mat.name.clone(),
            qualified_name: mat.qualified_name.clone(),
            kind: mat.kind.clone(),
            language: mat.language.clone(),
            file_path: mat.file_path.clone(),
            start_line: mat.start_line,
            end_line: mat.end_line,
        })
        .collect();
    node_vec.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));

    // Convert MaterializedEdge → SqryGraphEdge using qualified names from nodes
    let edges: Vec<SqryGraphEdge> = result
        .edges
        .iter()
        .map(|mat_edge| {
            let from = result.nodes[mat_edge.source_idx].qualified_name.clone();
            let to = result.nodes[mat_edge.target_idx].qualified_name.clone();
            let edge_type = classify_edge_type(mat_edge.classification);
            SqryGraphEdge {
                from,
                to,
                edge_type: edge_type.to_string(),
                depth: mat_edge.depth,
            }
        })
        .collect();

    (node_vec, edges, truncated)
}

/// Map an `EdgeClassification` to a human-readable edge type string.
fn classify_edge_type(classification: EdgeClassification) -> &'static str {
    match classification {
        EdgeClassification::Call { .. } => "call",
        EdgeClassification::Import { .. } => "import",
        EdgeClassification::Export { .. } => "export",
        EdgeClassification::Reference => "reference",
        EdgeClassification::Inherits => "inherits",
        EdgeClassification::Implements => "implements",
        EdgeClassification::Contains => "contains",
        EdgeClassification::Defines => "defines",
        EdgeClassification::TypeOf => "type_of",
        EdgeClassification::DatabaseAccess => "database",
        EdgeClassification::ServiceInteraction => "service",
    }
}

/// Render graph in the specified format.
fn render_graph(format: &str, graph: &Arc<CodeGraph>, verbose: bool) -> Option<String> {
    if format == "json" {
        return None;
    }

    let snapshot = graph.snapshot();
    match format {
        "dot" => {
            let config = DotConfig::default()
                .with_cross_language_highlight(true)
                .with_details(verbose)
                .with_edge_labels(verbose);
            let exporter = UnifiedDotExporter::with_config(&snapshot, config);
            Some(exporter.export())
        }
        "d2" => {
            let config = D2Config::default()
                .with_cross_language_highlight(true)
                .with_details(verbose)
                .with_edge_labels(verbose);
            let exporter = UnifiedD2Exporter::with_config(&snapshot, config);
            Some(exporter.export())
        }
        "mermaid" => {
            let config = MermaidConfig::default()
                .with_cross_language_highlight(true)
                .with_edge_labels(verbose);
            let exporter = UnifiedMermaidExporter::with_config(&snapshot, config);
            Some(exporter.export())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── DEFAULT_* constants ───────────────────────────────────────────────────

    #[test]
    fn default_max_depth_is_two() {
        assert_eq!(DEFAULT_MAX_DEPTH, 2);
    }

    #[test]
    fn default_max_results_is_1000() {
        assert_eq!(DEFAULT_MAX_RESULTS, 1000);
    }
}
