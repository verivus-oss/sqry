//! Graph export handler for LSP.
//!
//! Exports dependency graphs in various formats (JSON, DOT, D2, Mermaid).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::visualization::unified::{
    D2Config, DotConfig, MermaidConfig, UnifiedD2Exporter, UnifiedDotExporter,
    UnifiedMermaidExporter,
};

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

/// Find nodes by name (simple or qualified) in the graph.
fn find_nodes_by_name(snapshot: &GraphSnapshot, name: &str) -> Vec<NodeId> {
    let strings = snapshot.strings();

    // First try exact match by simple name
    let mut matches = snapshot.nodes_by_symbol(name);

    // Also search by qualified name if no simple name matches
    if matches.is_empty() {
        for (node_id, entry) in snapshot.iter_nodes() {
            if let Some(qname_id) = entry.qualified_name
                && let Some(qname) = strings.resolve(qname_id)
                && (qname.as_ref() == name || qname.ends_with(&format!("::{name}")))
            {
                matches.push(node_id);
            }
        }
    }

    matches
}

/// Build a `SqryGraphNode` from a node entry, returning its qualified name.
///
/// Returns `None` if the node has an empty qualified name.
fn build_graph_node(snapshot: &GraphSnapshot, node_id: NodeId) -> Option<(SqryGraphNode, String)> {
    let strings = snapshot.strings();
    let files = snapshot.files();
    let entry = snapshot.get_node(node_id)?;

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let qualified_name = entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| name.clone(), |s| s.to_string());

    if qualified_name.is_empty() {
        return None;
    }

    let kind = format!("{:?}", entry.kind).to_lowercase();

    let language = files
        .language_for_file(entry.file)
        .map_or("unknown".to_string(), |l| {
            l.to_string().to_ascii_lowercase()
        });

    let file_path = files
        .resolve(entry.file)
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let node = SqryGraphNode {
        name,
        qualified_name: qualified_name.clone(),
        kind,
        language,
        file_path,
        start_line: entry.start_line,
        end_line: entry.end_line,
    };

    Some((node, qualified_name))
}

/// Process outgoing edges for a node during graph export BFS.
///
/// Returns `true` if the edge limit was reached (truncated).
#[allow(clippy::too_many_arguments)]
fn collect_outgoing_edges(
    snapshot: &GraphSnapshot,
    current_id: NodeId,
    qualified_name: &str,
    depth: usize,
    max_depth: usize,
    max_results: usize,
    include_calls: bool,
    include_imports: bool,
    visited: &HashSet<NodeId>,
    edges: &mut Vec<SqryGraphEdge>,
    queue: &mut VecDeque<(NodeId, usize)>,
) -> bool {
    let strings = snapshot.strings();

    for edge in snapshot.edges().edges_from(current_id) {
        if edges.len() >= max_results {
            return true;
        }

        let (edge_type, should_traverse) = match &edge.kind {
            EdgeKind::Calls { .. } if include_calls => ("call", true),
            EdgeKind::Imports { .. } if include_imports => ("import", false),
            _ => continue,
        };

        let target_id = edge.target;
        let Some(target_entry) = snapshot.get_node(target_id) else {
            continue;
        };

        let target_qname = target_entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .map_or_else(String::new, |s| s.to_string());

        if target_qname.is_empty() {
            continue;
        }

        edges.push(SqryGraphEdge {
            from: qualified_name.to_string(),
            to: target_qname,
            edge_type: edge_type.to_string(),
            depth: u32::try_from(depth).unwrap_or(u32::MAX),
        });

        // Traverse call edges
        if should_traverse && depth < max_depth && !visited.contains(&target_id) {
            queue.push_back((target_id, depth + 1));
        }
    }

    false
}

/// Collect graph nodes and edges via BFS traversal.
fn collect_graph_data(
    snapshot: &GraphSnapshot,
    seeds: &[NodeId],
    max_depth: usize,
    max_results: usize,
    include_calls: bool,
    include_imports: bool,
) -> (Vec<SqryGraphNode>, Vec<SqryGraphEdge>, bool) {
    let mut nodes: HashMap<NodeId, SqryGraphNode> = HashMap::new();
    let mut edges: Vec<SqryGraphEdge> = Vec::new();
    let mut visited = HashSet::new();
    let mut queue: VecDeque<(NodeId, usize)> = seeds.iter().map(|&id| (id, 0usize)).collect();
    let mut truncated = false;

    while let Some((current_id, depth)) = queue.pop_front() {
        if depth > max_depth || visited.contains(&current_id) {
            continue;
        }
        visited.insert(current_id);

        let Some((node, qualified_name)) = build_graph_node(snapshot, current_id) else {
            continue;
        };

        nodes.entry(current_id).or_insert(node);

        // Process outgoing edges
        if collect_outgoing_edges(
            snapshot,
            current_id,
            &qualified_name,
            depth,
            max_depth,
            max_results,
            include_calls,
            include_imports,
            &visited,
            &mut edges,
            &mut queue,
        ) {
            truncated = true;
            break;
        }

        if edges.len() >= max_results {
            truncated = true;
            break;
        }
    }

    let mut node_vec: Vec<SqryGraphNode> = nodes.into_values().collect();
    node_vec.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));

    (node_vec, edges, truncated)
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
