//! Subgraph command implementation
//!
//! Provides CLI interface for extracting a focused subgraph around symbols.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::{
    EdgeFilter, TraversalConfig, TraversalDirection, TraversalLimits, traverse,
};
use std::collections::HashSet;

/// Subgraph output
#[derive(Debug, Serialize)]
struct SubgraphOutput {
    /// Seed symbols
    seeds: Vec<String>,
    /// Nodes in the subgraph
    nodes: Vec<SubgraphNode>,
    /// Edges in the subgraph
    edges: Vec<SubgraphEdge>,
    /// Statistics
    stats: SubgraphStats,
}

#[derive(Debug, Clone, Serialize)]
struct SubgraphNode {
    id: String,
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
    line: u32,
    language: String,
    /// Whether this is a seed node
    is_seed: bool,
    /// Depth from nearest seed
    depth: usize,
}

#[derive(Debug, Clone, Serialize)]
struct SubgraphEdge {
    source: String,
    target: String,
    kind: String,
}

#[derive(Debug, Serialize)]
struct SubgraphStats {
    node_count: usize,
    edge_count: usize,
    max_depth_reached: usize,
}

/// Find seed nodes in the graph matching the given symbol names.
fn find_seed_nodes(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    symbols: &[String],
) -> Vec<NodeId> {
    let strings = graph.strings();
    let mut seed_nodes: Vec<NodeId> = Vec::new();

    for symbol in symbols {
        let found = graph.nodes().iter().find(|(_, entry)| {
            // Gate 0d iter-2 fix: skip unified losers from CLI
            // `subgraph` seed lookup. See `NodeEntry::is_unified_loser`.
            if entry.is_unified_loser() {
                return false;
            }
            // Check qualified name
            if let Some(qn_id) = entry.qualified_name
                && let Some(qn) = strings.resolve(qn_id)
                && (qn.as_ref() == symbol.as_str() || qn.contains(symbol.as_str()))
            {
                return true;
            }
            // Check simple name
            if let Some(name) = strings.resolve(entry.name)
                && name.as_ref() == symbol.as_str()
            {
                return true;
            }
            false
        });

        if let Some((node_id, _)) = found {
            seed_nodes.push(node_id);
        }
    }

    seed_nodes
}

/// Result of BFS subgraph collection.
struct SubgraphBfsResult {
    visited: HashSet<NodeId>,
    node_depths: std::collections::HashMap<NodeId, usize>,
    collected_edges: Vec<(NodeId, NodeId, String)>,
    max_depth_reached: usize,
}

/// Collect a subgraph via BFS from seed nodes, following callers and/or callees.
///
/// Uses the traversal kernel with configurable direction and edge filter.
/// Converts the kernel's `TraversalResult` into the `SubgraphBfsResult`
/// expected by downstream code.
///
/// # Frontier invariant (DB19)
///
/// `seed_nodes` is a `&[NodeId]` — seeds are resolved once at the
/// handler boundary. The [`traverse`] kernel walks the NodeId-keyed
/// graph and never re-resolves names at depth ≥ 1. This is the same
/// invariant that DB17/DB18 locked for `trace_path`, `call-chain-depth`,
/// and `dependency-tree`: a same-simple-name node at depth 1 never
/// broadens the frontier because the kernel operates on
/// generational-indexed `NodeId`s, not names.
///
/// Regressions that reintroduced DB15-class same-name broadening would
/// surface in the CLI `migration_golden_cli_test` golden tests.
#[allow(clippy::similar_names)]
fn collect_subgraph_bfs(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    seed_nodes: &[NodeId],
    max_depth: usize,
    max_nodes: usize,
    include_callers: bool,
    include_callees: bool,
    include_imports: bool,
) -> SubgraphBfsResult {
    let snapshot = graph.snapshot();

    let direction = match (include_callers, include_callees) {
        (true, true) => TraversalDirection::Both,
        (true, false) => TraversalDirection::Incoming,
        #[allow(clippy::match_same_arms)] // Subgraph mode arms intentionally separate
        (false, true) => TraversalDirection::Outgoing,
        // If neither is selected, default to outgoing (no edges will match anyway)
        (false, false) => TraversalDirection::Outgoing,
    };

    let edge_filter = if include_imports {
        EdgeFilter::calls_and_imports()
    } else {
        EdgeFilter::calls_only()
    };

    let config = TraversalConfig {
        direction,
        edge_filter,
        limits: TraversalLimits {
            max_depth: u32::try_from(max_depth).unwrap_or(u32::MAX),
            max_nodes: Some(max_nodes),
            max_edges: None,
            max_paths: None,
        },
    };

    let result = traverse(&snapshot, seed_nodes, &config, None);

    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut node_depths: std::collections::HashMap<NodeId, usize> =
        std::collections::HashMap::new();
    let mut collected_edges: Vec<(NodeId, NodeId, String)> = Vec::new();
    let mut max_depth_reached: usize = 0;

    // Populate visited set and node depths from traversal result
    for (idx, mat_node) in result.nodes.iter().enumerate() {
        visited.insert(mat_node.node_id);

        // Seed nodes have depth 0; others get depth from the first edge
        let depth = if seed_nodes.contains(&mat_node.node_id) {
            0
        } else {
            result
                .edges
                .iter()
                .filter(|e| e.source_idx == idx || e.target_idx == idx)
                .map(|e| e.depth as usize)
                .min()
                .unwrap_or(0)
        };

        node_depths.insert(mat_node.node_id, depth);
        max_depth_reached = max_depth_reached.max(depth);
    }

    // Convert edges to (source_id, target_id, kind_str) tuples
    for edge in &result.edges {
        let source_id = result.nodes[edge.source_idx].node_id;
        let target_id = result.nodes[edge.target_idx].node_id;
        let kind_str = format!("{:?}", edge.raw_kind);
        collected_edges.push((source_id, target_id, kind_str));
    }

    SubgraphBfsResult {
        visited,
        node_depths,
        collected_edges,
        max_depth_reached,
    }
}

/// Infer language display name from a file extension.
fn extension_to_display_language(ext: &str) -> &str {
    match ext {
        "rs" => "Rust",
        "py" => "Python",
        "js" => "JavaScript",
        "ts" => "TypeScript",
        "go" => "Go",
        "java" => "Java",
        "c" | "h" => "C",
        "cpp" | "hpp" | "cc" => "C++",
        "rb" => "Ruby",
        "swift" => "Swift",
        "kt" => "Kotlin",
        _ => ext,
    }
}

/// Build `SubgraphNode` list from visited node IDs in BFS results.
fn build_subgraph_nodes(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    bfs: &SubgraphBfsResult,
    seed_nodes: &[NodeId],
) -> Vec<SubgraphNode> {
    let strings = graph.strings();
    let files = graph.files();
    let seed_set: HashSet<_> = seed_nodes.iter().collect();

    let mut nodes: Vec<SubgraphNode> = bfs
        .visited
        .iter()
        .filter_map(|&node_id| {
            let entry = graph.nodes().get(node_id)?;
            let name = strings
                .resolve(entry.name)
                .map(|s| s.to_string())
                .unwrap_or_default();
            let qualified_name = entry
                .qualified_name
                .and_then(|id| strings.resolve(id))
                .map_or_else(|| name.clone(), |s| s.to_string());

            let file_path = files
                .resolve(entry.file)
                .map(|p| p.display().to_string())
                .unwrap_or_default();

            // Infer language from file extension
            let language = files.resolve(entry.file).map_or_else(
                || "Unknown".to_string(),
                |p| {
                    p.extension()
                        .and_then(|ext| ext.to_str())
                        .map_or("Unknown", extension_to_display_language)
                        .to_string()
                },
            );

            Some(SubgraphNode {
                id: qualified_name.clone(),
                name,
                qualified_name,
                kind: format!("{:?}", entry.kind),
                file: file_path,
                line: entry.start_line,
                language,
                is_seed: seed_set.contains(&node_id),
                depth: *bfs.node_depths.get(&node_id).unwrap_or(&0),
            })
        })
        .collect();

    // Sort for determinism
    nodes.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    nodes
}

/// Build `SubgraphEdge` list from collected edges, resolving node IDs to names.
fn build_subgraph_edges(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    bfs: &SubgraphBfsResult,
) -> Vec<SubgraphEdge> {
    let strings = graph.strings();

    // Create a map from NodeId to qualified_name for edge resolution
    let node_names: std::collections::HashMap<NodeId, String> = bfs
        .visited
        .iter()
        .filter_map(|&node_id| {
            let entry = graph.nodes().get(node_id)?;
            let name = strings
                .resolve(entry.name)
                .map(|s| s.to_string())
                .unwrap_or_default();
            let qn = entry
                .qualified_name
                .and_then(|id| strings.resolve(id))
                .map_or_else(|| name, |s| s.to_string());
            Some((node_id, qn))
        })
        .collect();

    let mut edges: Vec<SubgraphEdge> = bfs
        .collected_edges
        .iter()
        .filter(|(src, tgt, _)| bfs.visited.contains(src) && bfs.visited.contains(tgt))
        .filter_map(|(src, tgt, kind)| {
            let src_name = node_names.get(src)?.clone();
            let tgt_name = node_names.get(tgt)?.clone();
            Some(SubgraphEdge {
                source: src_name,
                target: tgt_name,
                kind: kind.clone(),
            })
        })
        .collect();

    // Deduplicate edges
    edges.sort_by(|a, b| (&a.source, &a.target, &a.kind).cmp(&(&b.source, &b.target, &b.kind)));
    edges.dedup_by(|a, b| a.source == b.source && a.target == b.target && a.kind == b.kind);
    edges
}

/// Run the subgraph command.
///
/// # Errors
/// Returns an error if the graph cannot be loaded or symbols cannot be found.
#[allow(clippy::similar_names)]
// Callers/callees naming mirrors CLI flag semantics.
pub fn run_subgraph(
    cli: &Cli,
    symbols: &[String],
    path: Option<&str>,
    max_depth: usize,
    max_nodes: usize,
    include_callers: bool,
    include_callees: bool,
    include_imports: bool,
) -> Result<()> {
    let mut streams = OutputStreams::new();

    if symbols.is_empty() {
        return Err(anyhow!("At least one seed symbol is required"));
    }

    // Find index
    let search_path = path.map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        std::path::PathBuf::from,
    );

    let index_location = find_nearest_index(&search_path);
    let Some(ref loc) = index_location else {
        streams
            .write_diagnostic("No .sqry-index found. Run 'sqry index' first to build the index.")?;
        return Ok(());
    };

    // Load graph
    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&loc.index_root, &config, cli, no_op_reporter())
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    // Find seed nodes
    let seed_nodes = find_seed_nodes(&graph, symbols);
    if seed_nodes.is_empty() {
        streams.write_diagnostic("No seed symbols found in the graph.")?;
        return Ok(());
    }

    // BFS to collect subgraph
    let bfs = collect_subgraph_bfs(
        &graph,
        &seed_nodes,
        max_depth,
        max_nodes,
        include_callers,
        include_callees,
        include_imports,
    );

    // Build output
    let nodes = build_subgraph_nodes(&graph, &bfs, &seed_nodes);
    let edges = build_subgraph_edges(&graph, &bfs);

    let stats = SubgraphStats {
        node_count: nodes.len(),
        edge_count: edges.len(),
        max_depth_reached: bfs.max_depth_reached,
    };

    let output = SubgraphOutput {
        seeds: symbols.to_vec(),
        nodes,
        edges,
        stats,
    };

    // Output
    if cli.json {
        let json = serde_json::to_string_pretty(&output).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        let text = format_subgraph_text(&output);
        streams.write_result(&text)?;
    }

    Ok(())
}

fn format_subgraph_text(output: &SubgraphOutput) -> String {
    let mut lines = Vec::new();

    lines.push(format!(
        "Subgraph around {} seed(s): {}",
        output.seeds.len(),
        output.seeds.join(", ")
    ));
    lines.push(format!(
        "Stats: {} nodes, {} edges, max depth {}",
        output.stats.node_count, output.stats.edge_count, output.stats.max_depth_reached
    ));
    lines.push(String::new());

    lines.push("Nodes:".to_string());
    for node in &output.nodes {
        let seed_marker = if node.is_seed { " [SEED]" } else { "" };
        lines.push(format!(
            "  {} [{}] depth={}{} ",
            node.qualified_name, node.kind, node.depth, seed_marker
        ));
        lines.push(format!("    {}:{}", node.file, node.line));
    }

    if !output.edges.is_empty() {
        lines.push(String::new());
        lines.push("Edges:".to_string());
        for edge in &output.edges {
            lines.push(format!(
                "  {} --[{}]--> {}",
                edge.source, edge.kind, edge.target
            ));
        }
    }

    lines.join("\n")
}
