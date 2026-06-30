//! shape-match command — body-shape structural-neighbour search (U08).
//!
//! Routes through the sqry-db `StructuralNeighborsQuery` LSH index (U06) via the
//! shared `structural_neighbors` helper. Unlike `sqry similar` (fuzzy name
//! matching), this matches on the identifier-blind body-shape descriptor, so it
//! surfaces rename-and-relocate twins. Each match carries the AC-4 two-number
//! output: exact `shape_hash` identity and approximate `MinHash` Jaccard.

use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use serde::Serialize;

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;

#[derive(Debug, Serialize)]
struct ShapeMatchOutput {
    reference: NodeRef,
    neighbors: Vec<ShapeNeighbor>,
    stats: ShapeStats,
}

#[derive(Debug, Serialize)]
struct NodeRef {
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
    line: u32,
}

#[derive(Debug, Serialize)]
struct ShapeNeighbor {
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
    line: u32,
    /// True when the neighbour's structural `shape_hash` is byte-identical to
    /// the probe's (an exact rename/relocate-invariant match).
    shape_hash_exact: bool,
    /// Approximate `MinHash` Jaccard similarity (0.0–1.0).
    jaccard: f32,
}

#[derive(Debug, Serialize)]
struct ShapeStats {
    total_found: usize,
    similarity_floor: f64,
}

/// Run the `shape-match` command.
///
/// # Errors
/// Returns an error if the index is missing, the graph cannot be loaded, or the
/// probe symbol cannot be resolved to a function carrying a shape descriptor.
pub fn run_shape_match(
    cli: &Cli,
    symbol_name: &str,
    file: Option<&str>,
    path: Option<&str>,
    threshold: f64,
    max_results: usize,
) -> Result<()> {
    let mut streams = OutputStreams::new();

    let search_path = path.map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        std::path::PathBuf::from,
    );

    let Some(loc) = find_nearest_index(&search_path) else {
        streams
            .write_diagnostic("No .sqry-index found. Run 'sqry index' first to build the index.")?;
        return Ok(());
    };

    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&loc.index_root, &config, cli, no_op_reporter())
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    let snapshot = Arc::new(graph.snapshot());
    let strings = snapshot.strings();
    let files_registry = snapshot.files();
    let descriptors = snapshot.macro_metadata().shape_descriptors();

    // Resolve the probe: first Function/Method node matching `symbol_name` (by
    // simple or qualified name), optionally constrained to `file`, that carries
    // a shape descriptor.
    let probe = snapshot
        .nodes()
        .iter()
        .find(|(node_id, entry)| {
            if entry.is_unified_loser() {
                return false;
            }
            if !descriptors.contains_key(node_id) {
                return false;
            }
            if let Some(want_file) = file {
                let matches_file = files_registry.resolve(entry.file).is_some_and(|p| {
                    p.as_ref() == std::path::Path::new(want_file) || p.ends_with(want_file)
                });
                if !matches_file {
                    return false;
                }
            }
            let name_matches = strings
                .resolve(entry.name)
                .is_some_and(|n| n.as_ref() == symbol_name);
            let qname_matches = entry
                .qualified_name
                .and_then(|id| strings.resolve(id))
                .is_some_and(|q| q.as_ref() == symbol_name);
            name_matches || qname_matches
        })
        .map(|(id, _)| id);

    let Some(probe_id) = probe else {
        return Err(anyhow!(
            "No function/method named '{symbol_name}' with a body-shape descriptor was found{}. \
             (Tiny bodies carry no descriptor; try a larger function.)",
            file.map_or_else(String::new, |f| format!(" in '{f}'"))
        ));
    };

    let reference = node_ref(&snapshot, probe_id);

    // PN3 CLIENT_LOAD: opportunistic cold-load from the workspace companion.
    let db = sqry_db::queries::dispatch::make_query_db_cold(Arc::clone(&snapshot), &loc.index_root);

    #[allow(clippy::cast_possible_truncation)]
    let floor = threshold as f32;
    let matches = sqry_db::queries::structural_neighbors(
        &db,
        snapshot.as_ref(),
        probe_id,
        floor,
        max_results,
    );

    let neighbors: Vec<ShapeNeighbor> = matches
        .into_iter()
        .map(|m| {
            let r = node_ref(&snapshot, m.node);
            ShapeNeighbor {
                name: r.name,
                qualified_name: r.qualified_name,
                kind: r.kind,
                file: r.file,
                line: r.line,
                shape_hash_exact: m.shape_hash_exact,
                jaccard: m.jaccard,
            }
        })
        .collect();

    let stats = ShapeStats {
        total_found: neighbors.len(),
        similarity_floor: threshold,
    };
    let output = ShapeMatchOutput {
        reference,
        neighbors,
        stats,
    };

    if cli.json {
        let json = serde_json::to_string_pretty(&output).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        streams.write_result(&format_text(&output))?;
    }
    Ok(())
}

/// Build a [`NodeRef`] from a resolved node id.
fn node_ref(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node_id: sqry_core::graph::unified::node::id::NodeId,
) -> NodeRef {
    let strings = snapshot.strings();
    let files_registry = snapshot.files();
    let Some(entry) = snapshot.nodes().get(node_id) else {
        return NodeRef {
            name: String::new(),
            qualified_name: String::new(),
            kind: String::new(),
            file: String::new(),
            line: 0,
        };
    };
    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let qualified_name = entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| name.clone(), |s| s.to_string());
    let file = files_registry
        .resolve(entry.file)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    NodeRef {
        name,
        qualified_name,
        kind: format!("{:?}", entry.kind),
        file,
        line: entry.start_line,
    }
}

/// Human-readable rendering.
fn format_text(output: &ShapeMatchOutput) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Structural neighbours of {} ({}:{})",
        output.reference.qualified_name, output.reference.file, output.reference.line
    );
    let _ = writeln!(
        out,
        "  floor {:.2}, {} match(es)",
        output.stats.similarity_floor, output.stats.total_found
    );
    if output.neighbors.is_empty() {
        out.push_str("  (no structural neighbours above the floor)\n");
        return out;
    }
    for n in &output.neighbors {
        let exact = if n.shape_hash_exact { " [exact]" } else { "" };
        let _ = writeln!(
            out,
            "  {:.3}{}  {}  {}:{}",
            n.jaccard, exact, n.qualified_name, n.file, n.line
        );
    }
    out
}
