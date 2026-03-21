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
    resolve_label_budget_config,
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

fn has_fresh_analysis(storage: &GraphStorage) -> bool {
    let manifest_hash = compute_manifest_hash(storage.manifest_path()).ok();
    manifest_hash.is_some_and(|hash| {
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
    })
}

fn collect_analysis_stats(
    analyses: &GraphAnalyses,
    build_time: std::time::Duration,
) -> AnalysisStats {
    AnalysisStats {
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
    }
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
#[allow(clippy::too_many_lines)] // Sequential CLI orchestration across index discovery, graph loading, analysis execution, persistence, and user-facing output.
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
    if !force && has_fresh_analysis(&storage) {
        streams.write_diagnostic(
            "Analysis files already exist and match current index. Use --force to rebuild.",
        )?;
        return Ok(());
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

    let stats = collect_analysis_stats(&analyses, build_time);

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

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::analysis::condensation::{
        CondensationDag, ReachabilityStrategy,
    };
    use sqry_core::graph::unified::analysis::csr::CsrAdjacency;
    use sqry_core::graph::unified::analysis::persistence::{
        AnalysisIdentity, persist_condensation, persist_scc,
    };
    use sqry_core::graph::unified::analysis::scc::SccData;
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::persistence::GraphStorage;
    use std::time::Duration;

    /// Create a minimal `SccData` for a given edge kind.
    fn make_scc(edge_kind: EdgeKind, scc_count: u32) -> SccData {
        SccData {
            edge_kind,
            node_count: 10,
            scc_count,
            non_trivial_count: if scc_count > 1 { 1 } else { 0 },
            max_scc_size: if scc_count > 1 { 3 } else { 1 },
            node_to_scc: vec![0; 10],
            scc_offsets: vec![0, 10],
            scc_members: (0..10).collect(),
            has_self_loop: vec![false],
        }
    }

    /// Create a minimal `CondensationDag` for a given edge kind.
    fn make_cond(edge_kind: EdgeKind) -> CondensationDag {
        CondensationDag {
            edge_kind,
            scc_count: 1,
            edge_count: 0,
            row_offsets: vec![0, 0],
            col_indices: vec![],
            topo_order: vec![0],
            label_out_offsets: vec![0, 0],
            label_out_data: vec![],
            label_in_offsets: vec![0, 0],
            label_in_data: vec![],
            strategy: ReachabilityStrategy::DagBfs,
        }
    }

    /// Create the four edge kinds used by analysis in canonical order.
    fn analysis_edge_kinds() -> Vec<(&'static str, EdgeKind)> {
        vec![
            (
                "calls",
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
            ),
            (
                "imports",
                EdgeKind::Imports {
                    alias: None,
                    is_wildcard: false,
                },
            ),
            ("references", EdgeKind::References),
            ("inherits", EdgeKind::Inherits),
        ]
    }

    /// Write a manifest.json and all 8 analysis files (4 SCC + 4 condensation)
    /// using the given manifest hash.
    fn write_analysis_files(root: &std::path::Path, manifest_hash: &str) {
        let storage = GraphStorage::new(root);
        let identity = AnalysisIdentity::new(manifest_hash.to_string(), [0u8; 32]);
        std::fs::create_dir_all(storage.analysis_dir()).unwrap();

        for (kind_str, edge_kind) in analysis_edge_kinds() {
            let scc = make_scc(edge_kind.clone(), 5);
            persist_scc(&scc, &identity, &storage.analysis_scc_path(kind_str)).unwrap();

            let cond = make_cond(edge_kind);
            persist_condensation(&cond, &identity, &storage.analysis_cond_path(kind_str)).unwrap();
        }
    }

    /// Write a manifest.json file with given content and return its SHA-256 hash.
    fn write_manifest(root: &std::path::Path, content: &str) -> String {
        let storage = GraphStorage::new(root);
        std::fs::create_dir_all(storage.graph_dir()).unwrap();
        std::fs::write(storage.manifest_path(), content).unwrap();
        compute_manifest_hash(storage.manifest_path()).unwrap()
    }

    // ========================================================================
    // has_fresh_analysis tests
    // ========================================================================

    #[test]
    fn has_fresh_analysis_false_when_no_files_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Write only a manifest, but no analysis files.
        write_manifest(root, r#"{"version":"1.0"}"#);

        let storage = GraphStorage::new(root);
        assert!(!has_fresh_analysis(&storage));
    }

    #[test]
    fn has_fresh_analysis_false_when_no_manifest_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // No manifest at all — compute_manifest_hash should fail.
        let storage = GraphStorage::new(root);
        assert!(!has_fresh_analysis(&storage));
    }

    #[test]
    fn has_fresh_analysis_true_when_all_files_match() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let hash = write_manifest(root, r#"{"version":"1.0"}"#);
        write_analysis_files(root, &hash);

        let storage = GraphStorage::new(root);
        assert!(has_fresh_analysis(&storage));
    }

    #[test]
    fn has_fresh_analysis_false_when_manifest_hash_mismatches() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Write analysis files with one hash, then change the manifest so the
        // hash no longer matches.
        let _old_hash = write_manifest(root, r#"{"version":"1.0"}"#);
        write_analysis_files(root, "stale_hash_that_wont_match");

        let storage = GraphStorage::new(root);
        assert!(!has_fresh_analysis(&storage));
    }

    #[test]
    fn has_fresh_analysis_false_when_one_scc_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let hash = write_manifest(root, r#"{"version":"1.0"}"#);
        write_analysis_files(root, &hash);

        // Remove one SCC file to simulate partial corruption.
        let storage = GraphStorage::new(root);
        std::fs::remove_file(storage.analysis_scc_path("imports")).unwrap();

        assert!(!has_fresh_analysis(&storage));
    }

    #[test]
    fn has_fresh_analysis_false_when_one_cond_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let hash = write_manifest(root, r#"{"version":"1.0"}"#);
        write_analysis_files(root, &hash);

        // Remove one condensation file.
        let storage = GraphStorage::new(root);
        std::fs::remove_file(storage.analysis_cond_path("references")).unwrap();

        assert!(!has_fresh_analysis(&storage));
    }

    // ========================================================================
    // collect_analysis_stats tests
    // ========================================================================

    #[test]
    fn collect_analysis_stats_populated() {
        let calls_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let imports_kind = EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        };

        let analyses = GraphAnalyses {
            adjacency: CsrAdjacency {
                node_count: 42,
                edge_count: 100,
                row_offsets: vec![],
                col_indices: vec![],
                edge_kinds: vec![],
            },
            scc_calls: make_scc(calls_kind.clone(), 10),
            scc_imports: make_scc(imports_kind.clone(), 5),
            scc_references: make_scc(EdgeKind::References, 3),
            scc_inherits: make_scc(EdgeKind::Inherits, 0),
            cond_calls: make_cond(calls_kind),
            cond_imports: make_cond(imports_kind),
            cond_references: make_cond(EdgeKind::References),
            cond_inherits: make_cond(EdgeKind::Inherits),
        };

        let duration = Duration::from_millis(1234);
        let stats = collect_analysis_stats(&analyses, duration);

        assert_eq!(stats.node_count, 42);
        assert_eq!(stats.edge_count, 100);
        assert_eq!(stats.scc_stats.len(), 4);

        // Verify each edge kind is represented correctly.
        assert_eq!(stats.scc_stats[0].edge_kind, "calls");
        assert_eq!(stats.scc_stats[0].scc_count, 10);
        assert_eq!(stats.scc_stats[0].non_trivial_count, 1);
        assert_eq!(stats.scc_stats[0].max_scc_size, 3);

        assert_eq!(stats.scc_stats[1].edge_kind, "imports");
        assert_eq!(stats.scc_stats[1].scc_count, 5);

        assert_eq!(stats.scc_stats[2].edge_kind, "references");
        assert_eq!(stats.scc_stats[2].scc_count, 3);

        assert_eq!(stats.scc_stats[3].edge_kind, "inherits");
        assert_eq!(stats.scc_stats[3].scc_count, 0);
        assert_eq!(stats.scc_stats[3].non_trivial_count, 0);
        assert_eq!(stats.scc_stats[3].max_scc_size, 1);

        // Build time should be faithfully captured.
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(stats.build_time_secs, 1.234);
        }
    }

    // ========================================================================
    // format_stats_text tests
    // ========================================================================

    #[test]
    fn format_stats_text_contains_expected_labels() {
        let calls_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let imports_kind = EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        };
        let stats = AnalysisStats {
            node_count: 10,
            edge_count: 20,
            build_time_secs: 0.5,
            scc_stats: vec![
                SccStats {
                    edge_kind: "calls".to_string(),
                    scc_count: 3,
                    non_trivial_count: 1,
                    max_scc_size: 5,
                },
                SccStats {
                    edge_kind: "imports".to_string(),
                    scc_count: 2,
                    non_trivial_count: 0,
                    max_scc_size: 1,
                },
            ],
        };

        let tmp = tempfile::tempdir().unwrap();
        let analysis_dir = tmp.path().join("analysis");
        std::fs::create_dir_all(&analysis_dir).unwrap();

        let output = format_stats_text(&stats, &analysis_dir);

        assert!(
            output.contains("Graph analysis complete"),
            "Expected completion marker: {output}"
        );
        assert!(output.contains("10 nodes"), "Expected node count: {output}");
        assert!(output.contains("20 edges"), "Expected edge count: {output}");
        assert!(output.contains("0.50s"), "Expected build time: {output}");
        assert!(
            output.contains("calls"),
            "Expected calls SCC stats: {output}"
        );
        assert!(
            output.contains("imports"),
            "Expected imports SCC stats: {output}"
        );
        assert!(output.contains("3 SCCs"), "Expected SCC count: {output}");
        assert!(
            output.contains("max size: 5"),
            "Expected max SCC size: {output}"
        );
        assert!(
            output.contains(analysis_dir.to_string_lossy().as_ref()),
            "Expected analysis dir path: {output}"
        );
        // Suppress unused variable warnings in test helper context
        let _ = calls_kind;
        let _ = imports_kind;
    }

    #[test]
    fn format_stats_text_empty_scc_stats() {
        let stats = AnalysisStats {
            node_count: 0,
            edge_count: 0,
            build_time_secs: 0.0,
            scc_stats: vec![],
        };
        let tmp = tempfile::tempdir().unwrap();
        let output = format_stats_text(&stats, tmp.path());

        assert!(
            output.contains("Graph analysis complete"),
            "Missing header: {output}"
        );
        assert!(output.contains("0 nodes"), "Expected 0 nodes: {output}");
        assert!(output.contains("0 edges"), "Expected 0 edges: {output}");
    }

    #[test]
    fn collect_analysis_stats_empty_graph() {
        let calls_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let imports_kind = EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        };

        let empty_scc = |kind: EdgeKind| SccData {
            edge_kind: kind,
            node_count: 0,
            scc_count: 0,
            non_trivial_count: 0,
            max_scc_size: 0,
            node_to_scc: vec![],
            scc_offsets: vec![0],
            scc_members: vec![],
            has_self_loop: vec![],
        };

        let analyses = GraphAnalyses {
            adjacency: CsrAdjacency {
                node_count: 0,
                edge_count: 0,
                row_offsets: vec![0],
                col_indices: vec![],
                edge_kinds: vec![],
            },
            scc_calls: empty_scc(calls_kind.clone()),
            scc_imports: empty_scc(imports_kind.clone()),
            scc_references: empty_scc(EdgeKind::References),
            scc_inherits: empty_scc(EdgeKind::Inherits),
            cond_calls: make_cond(calls_kind),
            cond_imports: make_cond(imports_kind),
            cond_references: make_cond(EdgeKind::References),
            cond_inherits: make_cond(EdgeKind::Inherits),
        };

        let duration = Duration::from_secs(0);
        let stats = collect_analysis_stats(&analyses, duration);

        assert_eq!(stats.node_count, 0);
        assert_eq!(stats.edge_count, 0);
        for scc_stat in &stats.scc_stats {
            assert_eq!(scc_stat.scc_count, 0);
            assert_eq!(scc_stat.non_trivial_count, 0);
            assert_eq!(scc_stat.max_scc_size, 0);
        }
    }
}
