//! Impact command implementation
//!
//! Provides CLI interface for analyzing what would break if a symbol changes.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::traversal::EdgeClassification;
use sqry_core::graph::unified::{
    EdgeFilter, TraversalConfig, TraversalDirection, TraversalLimits, traverse,
};
use std::collections::{HashMap, HashSet};

/// Impact analysis output
#[derive(Debug, Serialize)]
struct ImpactOutput {
    /// Symbol being analyzed
    symbol: String,
    /// Direct dependents (depth 1)
    direct: Vec<ImpactSymbol>,
    /// Indirect dependents (depth > 1)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    indirect: Vec<ImpactSymbol>,
    /// Affected files
    #[serde(skip_serializing_if = "Vec::is_empty")]
    affected_files: Vec<String>,
    /// Statistics
    stats: ImpactStats,
}

#[derive(Debug, Serialize)]
struct ImpactSymbol {
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
    line: u32,
    /// How this symbol depends on the target
    relation: String,
    /// Depth from target symbol
    depth: usize,
}

#[derive(Debug, Serialize)]
struct ImpactStats {
    direct_count: usize,
    indirect_count: usize,
    total_affected: usize,
    affected_files_count: usize,
    max_depth: usize,
}

/// Result of BFS traversal collecting dependents.
struct BfsResult {
    visited: HashSet<NodeId>,
    node_depths: HashMap<NodeId, usize>,
    node_relations: HashMap<NodeId, String>,
    max_depth_reached: usize,
}

/// Perform BFS to collect all reverse dependents of a target node.
///
/// Uses the traversal kernel with incoming direction and dependency edges
/// (calls, imports, references, inheritance). Converts the kernel's
/// `TraversalResult` into the `BfsResult` expected by downstream code.
fn collect_dependents_bfs(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    target_node_id: NodeId,
    effective_max_depth: usize,
) -> BfsResult {
    let snapshot = graph.snapshot();

    let config = TraversalConfig {
        direction: TraversalDirection::Incoming,
        edge_filter: EdgeFilter::dependency_edges(),
        limits: TraversalLimits {
            max_depth: u32::try_from(effective_max_depth).unwrap_or(u32::MAX),
            max_nodes: None,
            max_edges: None,
            max_paths: None,
        },
    };

    let result = traverse(&snapshot, &[target_node_id], &config, None);

    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut node_depths: HashMap<NodeId, usize> = HashMap::new();
    let mut node_relations: HashMap<NodeId, String> = HashMap::new();
    let mut actual_max_depth: usize = 0;

    for (idx, mat_node) in result.nodes.iter().enumerate() {
        // Skip the target node itself — we only want dependents
        if mat_node.node_id == target_node_id {
            continue;
        }

        visited.insert(mat_node.node_id);

        // Find the minimum depth edge leading to this node to determine its depth
        let depth = result
            .edges
            .iter()
            .filter(|e| e.source_idx == idx || e.target_idx == idx)
            .map(|e| e.depth as usize)
            .min()
            .unwrap_or(1);

        node_depths.insert(mat_node.node_id, depth);
        actual_max_depth = actual_max_depth.max(depth);

        // Determine relation type from the first edge classification reaching this node
        let relation = result
            .edges
            .iter()
            .find(|e| e.source_idx == idx || e.target_idx == idx)
            .map(|e| classify_relation(&e.classification))
            .unwrap_or_default();

        node_relations.insert(mat_node.node_id, relation);
    }

    BfsResult {
        visited,
        node_depths,
        node_relations,
        max_depth_reached: actual_max_depth,
    }
}

/// Map an `EdgeClassification` to a human-readable relation label.
#[allow(clippy::trivially_copy_pass_by_ref)] // API consistency with other command handlers
fn classify_relation(classification: &EdgeClassification) -> String {
    match classification {
        EdgeClassification::Call { .. } => "calls".to_string(),
        EdgeClassification::Import { .. } => "imports".to_string(),
        EdgeClassification::Reference => "references".to_string(),
        EdgeClassification::Inherits => "inherits".to_string(),
        EdgeClassification::Implements => "implements".to_string(),
        EdgeClassification::Export { .. } => "exports".to_string(),
        EdgeClassification::Contains => "contains".to_string(),
        EdgeClassification::Defines => "defines".to_string(),
        EdgeClassification::TypeOf => "type_of".to_string(),
        EdgeClassification::DatabaseAccess => "database_access".to_string(),
        EdgeClassification::ServiceInteraction => "service_interaction".to_string(),
    }
}

/// Categorized impact symbols after BFS traversal.
struct CategorizedImpact {
    direct: Vec<ImpactSymbol>,
    indirect: Vec<ImpactSymbol>,
    affected_files: HashSet<String>,
}

/// Build categorized impact symbols from BFS results.
fn build_impact_symbols(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    bfs: &BfsResult,
    include_indirect: bool,
    include_files: bool,
) -> CategorizedImpact {
    let strings = graph.strings();
    let files = graph.files();
    let mut direct: Vec<ImpactSymbol> = Vec::new();
    let mut indirect: Vec<ImpactSymbol> = Vec::new();
    let mut affected_files: HashSet<String> = HashSet::new();

    for &node_id in &bfs.visited {
        if let Some(entry) = graph.nodes().get(node_id) {
            let depth = *bfs.node_depths.get(&node_id).unwrap_or(&0);
            let relation = bfs
                .node_relations
                .get(&node_id)
                .cloned()
                .unwrap_or_default();

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

            let impact_sym = ImpactSymbol {
                name,
                qualified_name,
                kind: format!("{:?}", entry.kind),
                file: file_path.clone(),
                line: entry.start_line,
                relation,
                depth,
            };

            if include_files {
                affected_files.insert(file_path);
            }

            if depth == 1 {
                direct.push(impact_sym);
            } else if include_indirect {
                indirect.push(impact_sym);
            }
        }
    }

    CategorizedImpact {
        direct,
        indirect,
        affected_files,
    }
}

/// Run the impact command.
///
/// # Errors
/// Returns an error if the graph cannot be loaded or symbol cannot be found.
pub fn run_impact(
    cli: &Cli,
    symbol: &str,
    path: Option<&str>,
    max_depth: usize,
    max_results: usize,
    include_indirect: bool,
    include_files: bool,
) -> Result<()> {
    let mut streams = OutputStreams::new();

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

    let strings = graph.strings();

    // Find the target symbol by iterating over nodes
    let target_node_id = graph
        .nodes()
        .iter()
        .find(|(_, entry)| {
            // Check qualified name
            if let Some(qn_id) = entry.qualified_name
                && let Some(qn) = strings.resolve(qn_id)
                && (qn.as_ref() == symbol || qn.contains(symbol))
            {
                return true;
            }
            // Check simple name
            if let Some(name) = strings.resolve(entry.name)
                && name.as_ref() == symbol
            {
                return true;
            }
            false
        })
        .map(|(id, _)| id)
        .ok_or_else(|| anyhow!("Symbol '{symbol}' not found in graph"))?;

    // BFS to find all dependents (reverse dependency traversal)
    let effective_max_depth = if include_indirect { max_depth } else { 1 };
    let bfs = collect_dependents_bfs(&graph, target_node_id, effective_max_depth);

    // Build categorized output
    let mut impact = build_impact_symbols(&graph, &bfs, include_indirect, include_files);

    // Sort for determinism
    impact
        .direct
        .sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    impact.indirect.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then(a.qualified_name.cmp(&b.qualified_name))
    });

    // Apply limit
    impact.direct.truncate(max_results);
    impact
        .indirect
        .truncate(max_results.saturating_sub(impact.direct.len()));

    let mut files_vec: Vec<String> = impact.affected_files.into_iter().collect();
    files_vec.sort();

    let stats = ImpactStats {
        direct_count: impact.direct.len(),
        indirect_count: impact.indirect.len(),
        total_affected: impact.direct.len() + impact.indirect.len(),
        affected_files_count: files_vec.len(),
        max_depth: bfs.max_depth_reached,
    };

    let output = ImpactOutput {
        symbol: symbol.to_string(),
        direct: impact.direct,
        indirect: impact.indirect,
        affected_files: if include_files { files_vec } else { Vec::new() },
        stats,
    };

    // Output
    if cli.json {
        let json = serde_json::to_string_pretty(&output).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        let text = format_impact_text(&output);
        streams.write_result(&text)?;
    }

    Ok(())
}

/// Format direct dependents section for text output.
fn format_direct_dependents(lines: &mut Vec<String>, direct: &[ImpactSymbol]) {
    if !direct.is_empty() {
        lines.push("Direct dependents:".to_string());
        for sym in direct {
            lines.push(format!(
                "  {} [{}] ({} this)",
                sym.qualified_name, sym.kind, sym.relation
            ));
            lines.push(format!("    {}:{}", sym.file, sym.line));
        }
    }
}

/// Format indirect dependents section for text output.
fn format_indirect_dependents(lines: &mut Vec<String>, indirect: &[ImpactSymbol]) {
    if !indirect.is_empty() {
        lines.push(String::new());
        lines.push("Indirect dependents:".to_string());
        for sym in indirect {
            lines.push(format!(
                "  {} [{}] depth={} ({} chain)",
                sym.qualified_name, sym.kind, sym.depth, sym.relation
            ));
            lines.push(format!("    {}:{}", sym.file, sym.line));
        }
    }
}

fn format_impact_text(output: &ImpactOutput) -> String {
    let mut lines = Vec::new();

    lines.push(format!("Impact analysis for: {}", output.symbol));
    lines.push(format!(
        "Total affected: {} ({} direct, {} indirect)",
        output.stats.total_affected, output.stats.direct_count, output.stats.indirect_count
    ));
    if output.stats.affected_files_count > 0 {
        lines.push(format!(
            "Affected files: {}",
            output.stats.affected_files_count
        ));
    }
    lines.push(String::new());

    if output.direct.is_empty() && output.indirect.is_empty() {
        lines.push("No dependents found. This symbol appears to be unused.".to_string());
    } else {
        format_direct_dependents(&mut lines, &output.direct);
        format_indirect_dependents(&mut lines, &output.indirect);
    }

    if !output.affected_files.is_empty() {
        lines.push(String::new());
        lines.push("Affected files:".to_string());
        for file in &output.affected_files {
            lines.push(format!("  {file}"));
        }
    }

    lines.join("\n")
}
