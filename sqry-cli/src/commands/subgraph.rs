//! Subgraph command implementation
//!
//! Provides CLI interface for extracting a focused subgraph around symbols.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeId;
use std::collections::{HashSet, VecDeque};

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

/// Process outgoing edges (callees) from a node during BFS traversal.
#[allow(clippy::too_many_arguments)]
fn process_callee_edges(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    node_id: NodeId,
    include_imports: bool,
    collected_edges: &mut Vec<(NodeId, NodeId, String)>,
    visited: &mut HashSet<NodeId>,
    node_depths: &mut std::collections::HashMap<NodeId, usize>,
    queue: &mut VecDeque<(NodeId, usize)>,
    depth: usize,
    max_nodes: usize,
) {
    for edge_ref in graph.edges().edges_from(node_id) {
        let is_call = matches!(edge_ref.kind, EdgeKind::Calls { .. });
        let is_import = matches!(edge_ref.kind, EdgeKind::Imports { .. });

        if is_call || (include_imports && is_import) {
            let kind_str = format!("{:?}", edge_ref.kind);
            collected_edges.push((node_id, edge_ref.target, kind_str));

            if !visited.contains(&edge_ref.target) && visited.len() < max_nodes {
                visited.insert(edge_ref.target);
                node_depths.insert(edge_ref.target, depth + 1);
                queue.push_back((edge_ref.target, depth + 1));
            }
        }
    }
}

/// Process incoming edges (callers) to a node during BFS traversal.
fn process_caller_edges(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    node_id: NodeId,
    collected_edges: &mut Vec<(NodeId, NodeId, String)>,
    visited: &mut HashSet<NodeId>,
    node_depths: &mut std::collections::HashMap<NodeId, usize>,
    queue: &mut VecDeque<(NodeId, usize)>,
    depth: usize,
    max_nodes: usize,
) {
    for edge_ref in graph.edges().edges_to(node_id) {
        if matches!(edge_ref.kind, EdgeKind::Calls { .. }) {
            let kind_str = format!("{:?}", edge_ref.kind);
            collected_edges.push((edge_ref.source, node_id, kind_str));

            if !visited.contains(&edge_ref.source) && visited.len() < max_nodes {
                visited.insert(edge_ref.source);
                node_depths.insert(edge_ref.source, depth + 1);
                queue.push_back((edge_ref.source, depth + 1));
            }
        }
    }
}

/// Collect a subgraph via BFS from seed nodes, following callers and/or callees.
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
    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut node_depths: std::collections::HashMap<NodeId, usize> =
        std::collections::HashMap::new();
    let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();
    let mut collected_edges: Vec<(NodeId, NodeId, String)> = Vec::new();

    // Initialize with seeds
    for &seed in seed_nodes {
        visited.insert(seed);
        node_depths.insert(seed, 0);
        queue.push_back((seed, 0));
    }

    let mut max_depth_reached = 0;

    while let Some((node_id, depth)) = queue.pop_front() {
        if visited.len() >= max_nodes {
            break;
        }
        if depth >= max_depth {
            continue;
        }

        max_depth_reached = max_depth_reached.max(depth);

        // Process outgoing edges (callees)
        if include_callees {
            process_callee_edges(
                graph,
                node_id,
                include_imports,
                &mut collected_edges,
                &mut visited,
                &mut node_depths,
                &mut queue,
                depth,
                max_nodes,
            );
        }

        // Process incoming edges (callers)
        if include_callers {
            process_caller_edges(
                graph,
                node_id,
                &mut collected_edges,
                &mut visited,
                &mut node_depths,
                &mut queue,
                depth,
                max_nodes,
            );
        }
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
    let graph = load_unified_graph_for_cli(&loc.index_root, &config, cli)
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
