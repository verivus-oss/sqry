//! Cycles command implementation.
//!
//! Provides the CLI interface for finding circular dependencies in the
//! codebase.
//!
//! # Dispatch path (DB19)
//!
//! `cycles` is a **name-keyed predicate** under the Phase 3C dispatch
//! taxonomy: the question is "which strongly connected components match
//! this edge kind and these bounds", which is the planner-canonical
//! contract that sqry-db's [`sqry_db::queries::CyclesQuery`] caches
//! (keyed on [`sqry_db::queries::CyclesKey`]). The CLI handler acquires
//! a per-call [`sqry_db::QueryDb`] via
//! [`sqry_db::queries::dispatch::make_query_db`], dispatches
//! `CyclesQuery`, and materializes the returned
//! [`sqry_core::graph::unified::node::NodeId`] vectors into the
//! qualified-name shape the JSON / text output uses.
//!
//! This mirrors the MCP `execute_find_cycles` pattern exactly so CLI
//! and MCP share one cache behavior on the same snapshot. The legacy
//! `find_all_cycles_graph` call path was removed in DB19 (2026-04-15).

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use serde::Serialize;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::query::CircularType;
use std::sync::Arc;

/// Cycle for output.
#[derive(Debug, Serialize)]
struct Cycle {
    /// Cycle depth (number of nodes).
    depth: usize,
    /// Nodes in the cycle (forms a ring: last connects to first).
    nodes: Vec<String>,
}

/// Materialize a cycle `NodeId` list into the qualified-name strings
/// the JSON / text output uses.
///
/// Mirrors `sqry_mcp::execution::tools::analysis::materialize_cycle_node_ids`.
/// Qualified names are preferred; nodes without a qualified name fall back
/// to their simple name. Nodes whose entries cannot be resolved (stale
/// `NodeId`s post-tombstone) are skipped silently — they are never in a
/// live cycle because sqry-db's Tarjan walk only visits arena-live nodes
/// per `SccQuery`.
fn materialize_cycle_node_ids(
    cycles: &[Vec<NodeId>],
    snapshot: &GraphSnapshot,
) -> Vec<Vec<String>> {
    let strings = snapshot.strings();
    cycles
        .iter()
        .map(|cycle| {
            cycle
                .iter()
                .filter_map(|&node_id| {
                    snapshot.get_node(node_id).and_then(|entry| {
                        entry
                            .qualified_name
                            .and_then(|sid| strings.resolve(sid))
                            .or_else(|| strings.resolve(entry.name))
                            .map(|s| s.to_string())
                    })
                })
                .collect()
        })
        .filter(|cycle: &Vec<String>| !cycle.is_empty())
        .collect()
}

/// Run the cycles command.
///
/// # Errors
/// Returns an error if the graph cannot be loaded.
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

    // Parse cycle type.
    let circular_type = CircularType::try_parse(cycle_type).with_context(|| {
        format!("Invalid cycle type: {cycle_type}. Use: calls, imports, modules")
    })?;

    // Find index.
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

    // Load unified graph.
    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&loc.index_root, &config, cli)
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    // Route through sqry-db: `CyclesQuery` is the name-keyed cycle
    // predicate in the planner taxonomy, cached per-snapshot.
    // `--include-self` maps directly onto
    // `CycleBounds::should_include_self_loops`, matching the pre-DB19
    // `CircularConfig::should_include_self_loops` semantic exactly
    // (`include_self` at the CLI ⇒ size-1 SCCs with a self-edge count).
    let snapshot = Arc::new(graph.snapshot());
    // PN3 CLIENT_LOAD: opportunistic cold-load from workspace companion file.
    let db = sqry_db::queries::dispatch::make_query_db_cold(Arc::clone(&snapshot), &loc.index_root);
    let key = sqry_db::queries::CyclesKey {
        circular_type,
        bounds: sqry_db::queries::CycleBounds {
            min_depth,
            max_depth,
            max_results,
            should_include_self_loops: include_self,
        },
    };
    let cycle_node_ids = db.get::<sqry_db::queries::CyclesQuery>(&key);
    let cycles = materialize_cycle_node_ids(&cycle_node_ids, snapshot.as_ref());

    // Convert to output format.
    let output_cycles: Vec<Cycle> = cycles
        .into_iter()
        .take(max_results)
        .map(|nodes| Cycle {
            depth: nodes.len(),
            nodes,
        })
        .collect();

    // Output.
    if cli.json {
        let json =
            serde_json::to_string_pretty(&output_cycles).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        // Text output.
        let output = format_cycles_text(&output_cycles, circular_type);
        streams.write_result(&output)?;
    }

    Ok(())
}

/// Format cycles as human-readable text.
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
