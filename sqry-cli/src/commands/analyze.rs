//! Analyze command implementation
//!
//! Builds precomputed graph analyses (Pass 5) for fast query-time performance.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use sqry_core::config::{ConfigPersistence, GraphConfigStore};
use sqry_core::graph::unified::analysis::{
    AnalysisIdentity, BudgetExceededPolicy, GraphAnalyses, LabelBudgetConfig,
    compute_manifest_hash, compute_node_id_hash,
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
/// Analysis settings are resolved with precedence: CLI args > config file > env vars > compiled defaults.
///
/// # Errors
/// Returns an error if the graph cannot be loaded or analyses cannot be built.
#[allow(clippy::too_many_arguments)]
pub fn run_analyze(
    cli: &Cli,
    path: Option<&str>,
    force: bool,
    threads: Option<usize>,
    label_budget: Option<u64>,
    density_threshold: Option<u64>,
    budget_exceeded_policy: Option<&str>,
    no_labels: bool,
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

    // Check if full analysis artifacts already exist AND match the current manifest.
    // After `sqry index --force`, stale SCC/DAG files from a previous analysis may
    // remain on disk. We validate against the manifest hash to detect this.
    let storage = GraphStorage::new(&loc.index_root);
    let analysis_dir = storage.analysis_dir();
    if !force {
        let manifest_hash = compute_manifest_hash(storage.manifest_path()).ok();
        let has_valid_analysis = manifest_hash.is_some_and(|hash| {
            ["calls", "imports", "references", "inherits"]
                .iter()
                .all(|kind| {
                    let scc_path = storage.analysis_scc_path(kind);
                    let cond_path = storage.analysis_cond_path(kind);
                    scc_path.exists()
                        && cond_path.exists()
                        && sqry_core::graph::unified::analysis::persistence::load_scc_manifest_checked(
                            &scc_path, &hash,
                        )
                        .is_ok()
                        && sqry_core::graph::unified::analysis::persistence::load_condensation_manifest_checked(
                            &cond_path, &hash,
                        )
                        .is_ok()
                })
        });
        if has_valid_analysis {
            streams.write_diagnostic(
                "Analysis files already exist and match current index. Use --force to rebuild.",
            )?;
            return Ok(());
        }
    }

    // Resolve analysis settings: CLI args > config file > env vars > compiled defaults
    let label_budget_config = resolve_label_budget_config(
        &loc.index_root,
        label_budget,
        density_threshold,
        budget_exceeded_policy,
        no_labels,
    )
    .context("Failed to resolve analysis budget configuration")?;

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
    let phase_desc = if label_budget_config.skip_labels {
        "CSR + SCC + Condensation (labels skipped)"
    } else {
        "CSR + SCC + Condensation + 2-hop labels"
    };
    streams.write_diagnostic(&format!("Computing analyses ({phase_desc})..."))?;
    let start = Instant::now();
    let analyses = if let Some(n) = threads {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .context("Failed to create rayon thread pool for analysis")?;
        pool.install(|| GraphAnalyses::build_all_with_budget(&snapshot, &label_budget_config))
            .context("Failed to build graph analyses")?
    } else {
        GraphAnalyses::build_all_with_budget(&snapshot, &label_budget_config)
            .context("Failed to build graph analyses")?
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

/// Resolve label budget config with precedence: CLI args > config file > env vars > compiled defaults.
///
/// # Errors
///
/// Returns an error if a u64 value cannot be converted to usize (e.g. on 32-bit platforms).
fn resolve_label_budget_config(
    index_root: &std::path::Path,
    cli_label_budget: Option<u64>,
    cli_density_threshold: Option<u64>,
    cli_policy: Option<&str>,
    cli_no_labels: bool,
) -> Result<LabelBudgetConfig> {
    // Layer 1: Compiled defaults
    let mut budget_per_kind: usize = 15_000_000;
    let mut on_exceeded = BudgetExceededPolicy::Degrade;
    let mut density_gate_threshold: usize = 64;
    let mut skip_labels = false;

    // Layer 2: Environment variables
    if let Some(val) = std::env::var("SQRY_LABEL_BUDGET")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        budget_per_kind = val;
    }
    if std::env::var("SQRY_LABEL_BUDGET_FAIL")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        on_exceeded = BudgetExceededPolicy::Fail;
    }
    if let Some(val) = std::env::var("SQRY_DENSITY_GATE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    {
        density_gate_threshold = val;
    }
    if std::env::var("SQRY_NO_LABELS")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        skip_labels = true;
    }

    // Layer 3: Config file (using canonical loader with recovery support)
    if let Ok(store) = GraphConfigStore::new(index_root)
        && store.is_initialized()
    {
        let persistence = ConfigPersistence::new(&store);
        if let Ok((config, report)) = persistence.load() {
            for warning in &report.warnings {
                log::warn!("Config load: {warning}");
            }
            budget_per_kind = usize::try_from(config.config.limits.analysis_label_budget_per_kind)
                .context("analysis_label_budget_per_kind exceeds usize range")?;
            density_gate_threshold =
                usize::try_from(config.config.limits.analysis_density_gate_threshold)
                    .context("analysis_density_gate_threshold exceeds usize range")?;
            match config
                .config
                .limits
                .analysis_budget_exceeded_policy
                .as_str()
            {
                "fail" => on_exceeded = BudgetExceededPolicy::Fail,
                "degrade" => on_exceeded = BudgetExceededPolicy::Degrade,
                other => {
                    log::warn!(
                        "Unknown analysis_budget_exceeded_policy '{other}' in config, ignoring"
                    );
                    // Don't override: preserve lower-precedence value (env var or default)
                }
            }
        }
    }

    // Layer 4: CLI args (highest precedence)
    if let Some(val) = cli_label_budget {
        budget_per_kind =
            usize::try_from(val).context("--label-budget value exceeds usize range")?;
    }
    if let Some(val) = cli_density_threshold {
        density_gate_threshold =
            usize::try_from(val).context("--density-threshold value exceeds usize range")?;
    }
    if cli_no_labels {
        skip_labels = true;
    }
    if let Some(policy) = cli_policy {
        on_exceeded = match policy {
            "fail" => BudgetExceededPolicy::Fail,
            "degrade" => BudgetExceededPolicy::Degrade,
            // clap PossibleValuesParser already rejects invalid values, but be defensive
            _ => bail!("Invalid --budget-exceeded-policy: '{policy}' (expected: degrade or fail)"),
        };
    }

    Ok(LabelBudgetConfig {
        budget_per_kind,
        on_exceeded,
        density_gate_threshold,
        skip_labels,
    })
}
