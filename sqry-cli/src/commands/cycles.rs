//! Cycles command implementation
//!
//! Provides CLI interface for finding circular dependencies in the codebase.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use serde::Serialize;
use sqry_core::query::{CircularConfig, CircularType, find_all_cycles_graph};

/// Cycle for output
#[derive(Debug, Serialize)]
struct Cycle {
    /// Cycle depth (number of nodes)
    depth: usize,
    /// Nodes in the cycle (forms a ring: last connects to first)
    nodes: Vec<String>,
}

/// Run the cycles command.
///
/// # Errors
/// Returns an error if the graph cannot be loaded or cycles cannot be found.
pub fn run_cycles(
    cli: &Cli,
    path: Option<&str>,
    cycle_type: &str,
    min_depth: usize,
    max_depth: Option<usize>,
    include_self: bool,
    max_results: usize,
) -> Result<()> {
    let mut streams = OutputStreams::new();

    // Parse cycle type
    let circular_type = CircularType::try_parse(cycle_type).with_context(|| {
        format!("Invalid cycle type: {cycle_type}. Use: calls, imports, modules")
    })?;

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

    // Load unified graph
    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&loc.index_root, &config, cli)
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    // Build config
    let circular_config = CircularConfig {
        min_depth,
        max_depth,
        max_results,
        should_include_self_loops: include_self,
    };

    // Find cycles using graph-based detection
    let all_cycles = find_all_cycles_graph(circular_type, &graph, &circular_config);

    // Convert to output format
    let output_cycles: Vec<Cycle> = all_cycles
        .into_iter()
        .take(max_results)
        .map(|nodes| Cycle {
            depth: nodes.len(),
            nodes,
        })
        .collect();

    // Output
    if cli.json {
        let json =
            serde_json::to_string_pretty(&output_cycles).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        // Text output
        let output = format_cycles_text(&output_cycles, circular_type);
        streams.write_result(&output)?;
    }

    Ok(())
}

/// Format cycles as human-readable text
fn format_cycles_text(cycles: &[Cycle], cycle_type: CircularType) -> String {
    let mut lines = Vec::new();

    let type_name = match cycle_type {
        CircularType::Calls => "call",
        CircularType::Imports => "import",
        CircularType::Modules => "module",
    };

    lines.push(format!("Found {} {} cycles", cycles.len(), type_name));
    lines.push(String::new());

    for (i, cycle) in cycles.iter().enumerate() {
        lines.push(format!("Cycle {} (depth {}):", i + 1, cycle.depth));

        // Format as chain: A → B → C → A
        let mut chain = cycle.nodes.join(" → ");
        if let Some(first) = cycle.nodes.first() {
            chain.push_str(" → ");
            chain.push_str(first);
        }
        lines.push(format!("  {chain}"));
        lines.push(String::new());
    }

    if cycles.is_empty() {
        lines.push("No cycles found.".to_string());
    }

    lines.join("\n")
}
