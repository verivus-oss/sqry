//! Analyze command implementation
//!
//! Builds precomputed graph analyses (Pass 5) for fast query-time performance.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use serde::Serialize;
use sqry_core::graph::unified::analysis::{
    AnalysisIdentity, GraphAnalyses, compute_manifest_hash, compute_node_id_hash,
};
use sqry_core::graph::unified::compaction::snapshot_edges;
use sqry_core::graph::unified::persistence::GraphStorage;
use std::time::Instant;

/// Analysis statistics for output
#[derive(Debug, Serialize)]
struct AnalysisStats {
    /// Total nodes in graph
    node_count: u32,
    /// Total edges in graph
    edge_count: u32,
    /// SCC statistics per edge kind
    scc_stats: Vec<SccStats>,
    /// Analysis build time in seconds
    build_time_secs: f64,
}

#[derive(Debug, Serialize)]
struct SccStats {
    edge_kind: String,
    scc_count: u32,
    non_trivial_count: u32,
    max_scc_size: u32,
}

/// Run the analyze command.
///
/// Builds precomputed graph analyses (CSR, SCC, Condensation DAG, 2-hop labels)
/// and persists them to .sqry/analysis/ for fast query-time performance.
///
/// # Errors
/// Returns an error if the graph cannot be loaded or analyses cannot be built.
pub fn run_analyze(
    cli: &Cli,
    path: Option<&str>,
    force: bool,
    threads: Option<usize>,
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

    streams.write_diagnostic("Building graph analyses...")?;

    // Load unified graph
    let config = GraphLoadConfig::default();
    let graph = load_unified_graph(&loc.index_root, &config)
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    // Check if analysis files already exist
    let storage = GraphStorage::new(&loc.index_root);
    let analysis_dir = storage.analysis_dir();
    if analysis_dir.exists() && !force {
        streams.write_diagnostic("Analysis files already exist. Use --force to rebuild.")?;
        return Ok(());
    }

    // Build compaction snapshot from graph
    streams.write_diagnostic("Creating compaction snapshot...")?;
    let graph_snapshot = graph.snapshot();
    let edges = graph_snapshot.edges();
    let forward_store = edges.forward();
    let node_count = graph_snapshot.nodes().len();
    let snapshot = snapshot_edges(&forward_store, node_count);

    let manifest_hash = compute_manifest_hash(storage.manifest_path())
        .context("Failed to compute manifest hash for analysis identity")?;
    let node_id_hash = compute_node_id_hash(&graph_snapshot);
    let identity = AnalysisIdentity::new(manifest_hash, node_id_hash);

    // Build all analyses
    streams.write_diagnostic("Computing analyses (CSR + SCC + Condensation + 2-hop labels)...")?;
    let start = Instant::now();
    let analyses = if let Some(n) = threads {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .context("Failed to create rayon thread pool for analysis")?;
        pool.install(|| GraphAnalyses::build_all(&snapshot))
            .context("Failed to build graph analyses")?
    } else {
        GraphAnalyses::build_all(&snapshot).context("Failed to build graph analyses")?
    };
    let build_time = start.elapsed();

    // Persist to disk
    streams.write_diagnostic("Persisting analyses to disk...")?;
    analyses
        .persist_all(&storage, &identity)
        .context("Failed to persist analyses")?;

    // Collect statistics
    let stats = AnalysisStats {
        node_count: analyses.adjacency.node_count,
        edge_count: analyses.adjacency.edge_count,
        scc_stats: vec![
            SccStats {
                edge_kind: "calls".to_string(),
                scc_count: analyses.scc_calls.scc_count,
                non_trivial_count: analyses.scc_calls.non_trivial_count,
                max_scc_size: analyses.scc_calls.max_scc_size,
            },
            SccStats {
                edge_kind: "imports".to_string(),
                scc_count: analyses.scc_imports.scc_count,
                non_trivial_count: analyses.scc_imports.non_trivial_count,
                max_scc_size: analyses.scc_imports.max_scc_size,
            },
            SccStats {
                edge_kind: "references".to_string(),
                scc_count: analyses.scc_references.scc_count,
                non_trivial_count: analyses.scc_references.non_trivial_count,
                max_scc_size: analyses.scc_references.max_scc_size,
            },
            SccStats {
                edge_kind: "inherits".to_string(),
                scc_count: analyses.scc_inherits.scc_count,
                non_trivial_count: analyses.scc_inherits.non_trivial_count,
                max_scc_size: analyses.scc_inherits.max_scc_size,
            },
        ],
        build_time_secs: build_time.as_secs_f64(),
    };

    // Output
    if cli.json {
        let json = serde_json::to_string_pretty(&stats).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        let output = format_stats_text(&stats, analysis_dir);
        streams.write_result(&output)?;
    }

    Ok(())
}

/// Format analysis statistics as human-readable text
fn format_stats_text(stats: &AnalysisStats, analysis_dir: &std::path::Path) -> String {
    let mut lines = Vec::new();

    lines.push("✓ Graph analysis complete".to_string());
    lines.push(String::new());

    lines.push(format!(
        "Graph: {} nodes, {} edges",
        stats.node_count, stats.edge_count
    ));
    lines.push(format!("Build time: {:.2}s", stats.build_time_secs));
    lines.push(String::new());

    lines.push("SCC Analysis:".to_string());
    for scc_stat in &stats.scc_stats {
        lines.push(format!(
            "  {}: {} SCCs ({} non-trivial, max size: {})",
            scc_stat.edge_kind,
            scc_stat.scc_count,
            scc_stat.non_trivial_count,
            scc_stat.max_scc_size
        ));
    }
    lines.push(String::new());

    lines.push(format!(
        "Analysis files written to: {}",
        analysis_dir.display()
    ));
    lines.push("  - adjacency.csr (CSR adjacency matrix)".to_string());
    lines.push(
        "  - scc_calls.scc, scc_imports.scc, scc_references.scc, scc_inherits.scc".to_string(),
    );
    lines.push(
        "  - cond_calls.dag, cond_imports.dag, cond_references.dag, cond_inherits.dag".to_string(),
    );

    lines.join("\n")
}
