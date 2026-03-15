//! Build entrypoint for unified graph.
//!
//! This module provides the top-level API for building a unified graph from source files.
//! It orchestrates file discovery and delegates to the 5-pass build pipeline.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use rayon::prelude::*;

use crate::graph::GraphBuilderError;
use crate::graph::unified::analysis::LabelBudgetConfig;
use crate::graph::unified::analysis::ReachabilityStrategy;
use crate::graph::unified::build::StagingGraph;
use crate::graph::unified::build::parallel_commit::{
    GlobalOffsets, pending_edges_to_delta, phase2_assign_ranges, phase3_parallel_commit,
    phase4_apply_global_remap,
};
use crate::graph::unified::build::pass3_intra::PendingEdge;
use crate::graph::unified::build::progress::GraphBuildProgressTracker;
use crate::graph::unified::concurrent::CodeGraph;
use crate::plugin::PluginManager;
use crate::plugin::error::ParseError;
use crate::progress::{SharedReporter, no_op_reporter};
use crate::project::path_utils::normalize_path_components;

/// Result of a successful build-and-persist operation.
///
/// Contains all metadata about the completed graph build, including
/// canonical (deduplicated) edge counts, file counts by language, and
/// provenance information.
#[derive(Debug, Clone)]
pub struct BuildResult {
    /// Number of nodes in the graph.
    pub node_count: usize,
    /// Number of deduplicated edges (from analysis CSR, after merge/compaction).
    /// This is the canonical edge count.
    pub edge_count: usize,
    /// Number of raw edges in the graph (CSR + delta buffer, before dedup).
    /// Available for diagnostics; NOT the canonical count.
    pub raw_edge_count: usize,
    /// Number of indexed files, by language (e.g., `{"rust": 150, "python": 30}`).
    ///
    /// Counts files that entered the graph indexing pipeline and were
    /// successfully parsed by a plugin. Not the same as "scanned files"
    /// (all files walked by the directory scanner).
    pub file_count: std::collections::HashMap<String, usize>,
    /// Total number of indexed files.
    pub total_files: usize,
    /// ISO 8601 timestamp when the build completed.
    pub built_at: String,
    /// Root path that was indexed.
    pub root_path: String,
    /// Number of threads used for parallel file processing.
    ///
    /// Reflects the effective thread count from the rayon pool, not the
    /// CLI-requested value. Useful for build diagnostics.
    pub thread_count: usize,

    /// Reachability strategy used by each persisted analysis kind.
    pub analysis_strategies: Vec<AnalysisStrategySummary>,
}

/// Persisted analysis strategy summary for one edge kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisStrategySummary {
    /// Stable edge-kind label (`calls`, `imports`, `references`, `inherits`).
    pub edge_kind: &'static str,
    /// Reachability strategy persisted for the edge kind.
    pub strategy: ReachabilityStrategy,
}

/// Default staging memory limit per batch: 512 MB.
///
/// When the accumulated `StagingGraph` memory exceeds this threshold, the
/// current batch is committed before parsing the next chunk. Override via
/// `SQRY_STAGING_MEMORY_LIMIT_MB` or [`BuildConfig::staging_memory_limit`].
const DEFAULT_STAGING_MEMORY_LIMIT: usize = 512 * 1024 * 1024;

/// Configuration for building the unified graph.
#[derive(Debug, Clone)]
pub struct BuildConfig {
    /// Maximum directory depth to traverse (None = unlimited).
    pub max_depth: Option<usize>,

    /// Follow symbolic links.
    pub follow_links: bool,

    /// Include hidden files and directories.
    pub include_hidden: bool,

    /// Number of threads for parallel building (None = use default based on CPU count).
    pub num_threads: Option<usize>,

    /// Maximum staging memory (bytes) to accumulate before committing a batch.
    ///
    /// Controls the parse-commit chunking watermark. When the sum of all
    /// in-flight `StagingGraph` buffers exceeds this limit, the batch is
    /// committed to the graph before the next chunk of files is parsed.
    ///
    /// Defaults to 512 MB. Override via
    /// `SQRY_STAGING_MEMORY_LIMIT_MB` environment variable.
    pub staging_memory_limit: usize,

    /// Configuration for the 2-hop label budget used during analysis.
    ///
    /// Controls the maximum number of intervals per edge kind and what
    /// to do when the budget is exceeded (fail or degrade to BFS).
    pub label_budget: LabelBudgetConfig,
}

impl Default for BuildConfig {
    fn default() -> Self {
        let limit = std::env::var("SQRY_STAGING_MEMORY_LIMIT_MB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map_or(DEFAULT_STAGING_MEMORY_LIMIT, |mb| mb * 1024 * 1024);

        let label_budget = LabelBudgetConfig {
            budget_per_kind: 15_000_000,
            on_exceeded: crate::graph::unified::analysis::BudgetExceededPolicy::Degrade,
            density_gate_threshold: 64,
            skip_labels: false,
        };

        Self {
            max_depth: None,
            follow_links: false,
            include_hidden: false,
            num_threads: None,
            staging_memory_limit: limit,
            label_budget,
        }
    }
}

/// Create a rayon thread pool sized by `BuildConfig::num_threads`.
fn create_thread_pool(config: &BuildConfig) -> Result<rayon::ThreadPool> {
    let mut builder = rayon::ThreadPoolBuilder::new();
    if let Some(n) = config.num_threads {
        builder = builder.num_threads(n);
    }
    builder
        .build()
        .context("Failed to create rayon thread pool for parallel indexing")
}

/// Compute chunk boundaries for memory-bounded parallel parse batches.
///
/// Splits `files` into non-overlapping ranges where each chunk's estimated
/// staging memory stays within `memory_limit`. Uses source file size as a
/// proxy for staging buffer size (multiplied by an expansion factor to
/// account for AST node/edge/string overhead).
///
/// Returns at least one chunk even if the first file alone exceeds the limit.
fn compute_parse_chunks(
    files: &[PathBuf],
    _pool: &rayon::ThreadPool,
    _plugins: &PluginManager,
    memory_limit: usize,
) -> Vec<std::ops::Range<usize>> {
    // Expansion factor: staging buffers are typically 2-8x the source file
    // size due to AST nodes, edges, and interned strings. Use 4x as a
    // conservative middle ground.
    const EXPANSION_FACTOR: usize = 4;

    let mut chunks = Vec::new();
    let mut chunk_start = 0;
    let mut chunk_estimate = 0usize;

    for (i, path) in files.iter().enumerate() {
        #[allow(clippy::cast_possible_truncation)] // File sizes always fit usize on 32/64-bit.
        let file_size = std::fs::metadata(path)
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        let estimated_staging = file_size * EXPANSION_FACTOR;

        // If adding this file would exceed the limit and we already have
        // files in the chunk, finalize the current chunk first.
        if chunk_estimate + estimated_staging > memory_limit && i > chunk_start {
            chunks.push(chunk_start..i);
            chunk_start = i;
            chunk_estimate = 0;
        }
        chunk_estimate += estimated_staging;
    }

    // Final chunk (always push — handles single-chunk and trailing files)
    if chunk_start < files.len() {
        chunks.push(chunk_start..files.len());
    }

    if chunks.len() > 1 {
        log::info!(
            "Memory-bounded chunking: {} batches for {} files (limit: {} MB)",
            chunks.len(),
            files.len(),
            memory_limit / (1024 * 1024),
        );
    }

    chunks
}

/// Phase name for file processing during graph build.
pub const GRAPH_FILE_PROCESSING_PHASE: &str = "File processing";

/// Build a unified graph from source files.
///
/// This function:
/// 1. Walks the file tree starting at `root`
/// 2. For each file, extracts symbols using the appropriate language plugin
/// 3. Runs the 5-pass build pipeline to populate the graph
/// 4. Returns the completed `CodeGraph`
///
/// # Arguments
///
/// * `root` - Root directory to scan for source files
/// * `plugins` - Plugin manager for language-specific extraction
/// * `config` - Build configuration
///
/// # Returns
///
/// A `CodeGraph` containing the populated graph.
///
/// # Errors
///
/// Returns an error if:
/// - The root path does not exist
/// - No graph builders are registered
/// - All eligible files fail to build (per-file failures are logged and skipped)
///
/// # Example
///
/// ```ignore
/// use sqry_core::graph::unified::build::{build_unified_graph, BuildConfig};
/// use sqry_core::plugin::PluginManager;
/// use std::path::Path;
///
/// let plugins = sqry_plugin_registry::create_plugin_manager();
/// let config = BuildConfig::default();
/// let graph = build_unified_graph(Path::new("src"), &plugins, &config)?;
/// println!("Created graph with {} nodes", graph.node_count());
/// ```
pub fn build_unified_graph(
    root: &Path,
    plugins: &PluginManager,
    config: &BuildConfig,
) -> Result<CodeGraph> {
    let (graph, _effective_threads) =
        build_unified_graph_inner(root, plugins, config, no_op_reporter())?;
    Ok(graph)
}

/// Build a unified graph from source files with progress reporting.
///
/// This is the same as [`build_unified_graph`] but accepts a progress reporter
/// for tracking build progress.
///
/// # Arguments
///
/// * `root` - Root directory to scan for source files
/// * `plugins` - Plugin manager for language-specific extraction
/// * `config` - Build configuration
/// * `progress` - Progress reporter for build status updates
///
/// # Returns
///
/// A `CodeGraph` containing the populated graph.
///
/// # Errors
///
/// Returns an error if the path is missing, no graph builders are registered,
/// or all eligible files fail to build.
pub fn build_unified_graph_with_progress(
    root: &Path,
    plugins: &PluginManager,
    config: &BuildConfig,
    progress: SharedReporter,
) -> Result<CodeGraph> {
    let (graph, _effective_threads) = build_unified_graph_inner(root, plugins, config, progress)?;
    Ok(graph)
}

/// Internal implementation that returns the effective thread count alongside the graph.
///
/// Used by [`build_and_persist_graph_with_progress`] to propagate the thread count
/// into `BuildResult` without exposing it in the public API.
#[allow(clippy::too_many_lines)] // Complex 5-pass build pipeline requires sequential flow
fn build_unified_graph_inner(
    root: &Path,
    plugins: &PluginManager,
    config: &BuildConfig,
    progress: SharedReporter,
) -> Result<(CodeGraph, usize)> {
    if !root.exists() {
        anyhow::bail!("Path {} does not exist", root.display());
    }

    log::info!(
        "Building unified graph from source files in {}",
        root.display()
    );

    let has_graph_builders = plugins
        .plugins()
        .iter()
        .any(|plugin| plugin.graph_builder().is_some());
    if !has_graph_builders {
        anyhow::bail!("No graph builders registered – cannot build code graph");
    }

    // Create progress tracker for this build
    let tracker = GraphBuildProgressTracker::new(progress);

    // 1. Find source files
    let mut files = find_source_files(root, config);
    sort_files_for_build(root, &mut files);

    // 2. Create the unified graph
    let mut graph = CodeGraph::new();

    // 3. Create scoped thread pool for parallel parse
    let pool = create_thread_pool(config)?;
    let effective_threads = pool.current_num_threads();
    log::info!("Parallel indexing: using {effective_threads} threads");

    // Chunked parallel-parse / parallel-commit pipeline.
    //
    // Files are processed in memory-bounded batches (chunks). Each chunk:
    //   Phase 1: Parse files in parallel (rayon thread pool)
    //   Phase 2: Count + prefix-sum range assignment
    //   Phase 3: Parallel commit into disjoint pre-allocated arena/interner ranges
    //   Phase 4: After ALL chunks — string dedup, global remap, index build, edge bulk insert
    //
    // The batch boundary is determined by `staging_memory_limit`: once the
    // accumulated staging buffer size exceeds the watermark, the current
    // batch is committed before more files are parsed. This prevents OOM
    // on large repositories where holding all StagingGraphs simultaneously
    // would exhaust available RAM.
    let total_files = files.len();
    tracker.start_phase(
        1,
        "Chunked structural indexing (parse -> range-plan -> semantic commit)",
        total_files,
    );

    let (mut succeeded, mut parse_errors, mut skipped) = (0usize, 0usize, 0usize);
    let mut total_staging_bytes = 0usize;
    let mut peak_chunk_staging_bytes = 0usize;
    let mut max_file_staging_bytes = 0usize;

    // Global offsets track running positions across chunks.
    // For a fresh graph: node arena starts at 0 slots, string interner at 1 (sentinel).
    let initial_string_offset = graph.strings_mut().alloc_range(0).unwrap_or(1);
    let mut offsets = GlobalOffsets {
        node_offset: u32::try_from(graph.nodes().slot_count()).unwrap_or(0),
        string_offset: initial_string_offset,
    };
    // Collect all edges across chunks for Phase 4 bulk insert.
    let mut all_edges: Vec<Vec<PendingEdge>> = Vec::new();

    let chunks = compute_parse_chunks(&files, &pool, plugins, config.staging_memory_limit);
    for chunk_range in chunks {
        let chunk_files = &files[chunk_range];

        // Phase 1: Parallel parse this chunk
        let staged_results: Vec<(PathBuf, Result<Option<ParsedFile>>)> = pool.install(|| {
            chunk_files
                .par_iter()
                .map(|path| {
                    let result = parse_file(path.as_path(), plugins);
                    tracker.increment_progress();
                    (path.clone(), result)
                })
                .collect()
        });

        // Separate successful parses from errors/skips
        let mut chunk_parsed: Vec<(PathBuf, ParsedFile)> = Vec::new();
        let mut chunk_staging_bytes = 0usize;
        for (path, result) in staged_results {
            match result {
                Ok(Some(parsed)) => {
                    let file_bytes = parsed.staging.estimated_byte_size();
                    total_staging_bytes += file_bytes;
                    chunk_staging_bytes += file_bytes;
                    if file_bytes > max_file_staging_bytes {
                        max_file_staging_bytes = file_bytes;
                    }
                    chunk_parsed.push((path, parsed));
                }
                Ok(None) => skipped += 1,
                Err(e) => {
                    parse_errors += 1;
                    log::warn!("Failed to parse {}: {e}", path.display());
                }
            }
        }
        if chunk_staging_bytes > peak_chunk_staging_bytes {
            peak_chunk_staging_bytes = chunk_staging_bytes;
        }

        if chunk_parsed.is_empty() {
            continue;
        }

        // Register files in batch
        let file_info: Vec<_> = chunk_parsed
            .iter()
            .map(|(path, parsed)| (path.clone(), Some(parsed.language)))
            .collect();
        let file_ids = graph
            .files_mut()
            .register_batch(&file_info)
            .map_err(|e| anyhow::anyhow!("Failed to register files: {e}"))?;

        // Phase 2: Count + range assignment (fast, no progress needed)
        let staging_refs: Vec<_> = chunk_parsed.iter().map(|(_, p)| &p.staging).collect();
        let plan = phase2_assign_ranges(&staging_refs, &file_ids, &offsets);

        // Pre-allocate arena and interner ranges for Phase 3.
        let placeholder = crate::graph::unified::storage::NodeEntry::new(
            crate::graph::unified::node::NodeKind::Other,
            crate::graph::unified::string::StringId::new(0),
            crate::graph::unified::file::FileId::new(0),
        );
        graph
            .nodes_mut()
            .alloc_range(plan.total_nodes, &placeholder)
            .map_err(|e| anyhow::anyhow!("Failed to alloc node range: {e:?}"))?;
        graph
            .strings_mut()
            .alloc_range(plan.total_strings)
            .map_err(|e| anyhow::anyhow!("Failed to alloc string range: {e}"))?;

        // Phase 3: Parallel commit into disjoint pre-allocated ranges.
        // Use pool.install to respect BuildConfig::num_threads for rayon par_iter.
        let (arena, interner) = graph.nodes_and_strings_mut();
        let phase3 = pool.install(|| phase3_parallel_commit(&plan, &staging_refs, arena, interner));

        // Validate written counts match plan. A mismatch indicates a bug in
        // StagingGraph counting — abort the build to prevent phantom entries
        // and inconsistent file registry state.
        let expected_nodes = plan.total_nodes as usize;
        let expected_strings = plan.total_strings as usize;
        let expected_edges = usize::try_from(plan.total_edges)
            .unwrap_or_else(|_| unreachable!("edge count does not fit usize"));
        if phase3.total_nodes_written != expected_nodes
            || phase3.total_strings_written != expected_strings
            || phase3.total_edges_collected != expected_edges
        {
            anyhow::bail!(
                "Phase 3 count mismatch: nodes {}/{expected_nodes}, strings {}/{expected_strings}, \
                 edges {}/{expected_edges}. This indicates a bug in StagingGraph counting.",
                phase3.total_nodes_written,
                phase3.total_strings_written,
                phase3.total_edges_collected,
            );
        }

        succeeded += chunk_parsed.len();

        // Merge confidence metadata from parsed files
        for (_path, parsed) in &mut chunk_parsed {
            if let Some(confidence) = parsed.staging.take_confidence() {
                let language_name = parsed.language.to_string();
                graph.merge_confidence(&language_name, confidence);
            }
        }

        // Update global offsets for next chunk
        offsets.node_offset += plan.total_nodes;
        offsets.string_offset += plan.total_strings;

        // Accumulate edges for Phase 4
        all_edges.extend(phase3.per_file_edges);
    }
    tracker.complete_phase();

    // Phase 4: Post-chunk finalization
    tracker.start_phase(4, "Finalizing graph", 4);

    // Phase 4a: Global string dedup
    let string_remap = graph.strings_mut().build_dedup_table();
    if !string_remap.is_empty() {
        log::debug!(
            "Phase 4a: dedup removed {} duplicate string(s)",
            string_remap.len()
        );

        // Phase 4b: Apply dedup remap to all nodes and pending edges
        phase4_apply_global_remap(graph.nodes_mut(), &mut all_edges, &string_remap);
    }
    tracker.increment_progress(); // 4a+4b done

    // Phase 4c: Build indices from finalized arena.
    // Uses build_from_arena() which is O(n log n) — no per-element duplicate check.
    graph.rebuild_indices();
    tracker.increment_progress(); // 4c done

    // Phase 4d: Bulk insert edges via deterministic DeltaEdge conversion.
    // Start seq numbering from the edge store's current counter to support non-empty graphs.
    let edge_seq_start = graph.edges().forward().seq_counter();
    let (delta_edge_vecs, _final_seq) = pending_edges_to_delta(&all_edges, edge_seq_start);
    let total_edge_count: u64 = delta_edge_vecs.iter().map(|v| v.len() as u64).sum();
    if total_edge_count > 0 {
        graph
            .edges()
            .add_edges_bulk_ordered(&delta_edge_vecs, total_edge_count);
    }
    tracker.increment_progress(); // 4d done
    tracker.complete_phase();

    log::info!(
        "Parallel indexing complete: {succeeded} committed, {skipped} skipped, \
         {parse_errors} parse errors, \
         ~{} MB total staged, ~{} MB peak chunk (max single file: ~{} KB)",
        total_staging_bytes / (1024 * 1024),
        peak_chunk_staging_bytes / (1024 * 1024),
        max_file_staging_bytes / 1024,
    );

    let attempted = succeeded + parse_errors;

    if attempted == 0 {
        log::warn!(
            "No eligible source files found for graph build in {}",
            root.display()
        );
    }

    if attempted > 0 && succeeded == 0 {
        anyhow::bail!("All graph builds failed");
    }

    // Pass 5: Cross-language linking (FFI declarations → C/C++ functions, HTTP requests → endpoints)
    tracker.start_phase(5, "Cross-language linking", 1);
    let pass5_stats = super::pass5_cross_language::link_cross_language_edges(&mut graph);
    if pass5_stats.total_edges_created > 0 {
        log::info!(
            "Pass 5: {} cross-language edges created ({} FFI, {} HTTP)",
            pass5_stats.total_edges_created,
            pass5_stats.ffi_edges_created,
            pass5_stats.http_endpoints_matched,
        );
    }
    tracker.increment_progress(); // pass 5 done
    tracker.complete_phase();

    log::info!("Built unified graph with {} nodes", graph.node_count());
    Ok((graph, effective_threads))
}

/// Build unified graph, persist snapshot + manifest, and run analysis pipeline.
///
/// Convenience wrapper that uses a no-op progress reporter.
/// See [`build_and_persist_graph_with_progress`] for full documentation.
///
/// # Errors
///
/// Returns an error if graph building, persistence, or analysis fails.
pub fn build_and_persist_graph(
    root: &Path,
    plugins: &PluginManager,
    config: &BuildConfig,
    build_command: &str,
) -> Result<(CodeGraph, BuildResult)> {
    build_and_persist_graph_with_progress(root, plugins, config, build_command, no_op_reporter())
}

/// Build unified graph with progress, persist snapshot + manifest, and run analysis.
///
/// This is the single entry point for building a complete graph index. It combines:
/// 1. Graph building from source files (with progress reporting)
/// 2. Snapshot persistence (binary format)
/// 3. Analysis pipeline (CSR + SCC + Condensation DAG + labels/fallback) — strict, fails on error
/// 4. Manifest creation with deduplicated edge count (JSON metadata, written LAST)
///
/// The manifest is the "commit point" — written last, only after all other artifacts
/// succeed. Consumers check `storage.exists()` (manifest-based) for index readiness.
///
/// # Arguments
///
/// * `root` - Root directory to scan for source files
/// * `plugins` - Plugin manager for language-specific extraction
/// * `config` - Build configuration
/// * `build_command` - Provenance string (e.g., `"cli:index"`, `"mcp:rebuild_index"`)
/// * `progress` - Progress reporter for build status updates
///
/// # Errors
///
/// Returns an error if graph building, persistence, or analysis fails.
/// Analysis failure is strict — no fallback to raw edge counts.
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
pub fn build_and_persist_graph_with_progress(
    root: &Path,
    plugins: &PluginManager,
    config: &BuildConfig,
    build_command: &str,
    progress: SharedReporter,
) -> Result<(CodeGraph, BuildResult)> {
    use crate::graph::unified::analysis::csr::CsrAdjacency;
    use crate::graph::unified::analysis::{AnalysisIdentity, GraphAnalyses, compute_node_id_hash};
    use crate::graph::unified::compaction::snapshot_edges;
    use crate::graph::unified::persistence::manifest::write_manifest_bytes_atomic;
    use crate::graph::unified::persistence::{
        BuildProvenance, GraphStorage, MANIFEST_SCHEMA_VERSION, Manifest, SNAPSHOT_FORMAT_VERSION,
        save_to_path,
    };
    use crate::progress::IndexProgress;
    use chrono::Utc;
    use sha2::{Digest, Sha256};

    // Step 1: Build the unified graph
    let (graph, effective_threads) =
        build_unified_graph_inner(root, plugins, config, progress.clone())?;

    // Step 2: Ensure storage directories exist and remove old manifest
    // Removing the manifest BEFORE writing the new snapshot ensures that
    // readers see `storage.exists() == false` during the rebuild window.
    // Without this, an interrupted rebuild (crash after snapshot write but
    // before manifest write) would leave the old manifest paired with a
    // new, potentially incompatible snapshot — violating the commit-point
    // contract.
    let storage = GraphStorage::new(root);
    fs::create_dir_all(storage.graph_dir())
        .with_context(|| format!("Failed to create {}", storage.graph_dir().display()))?;

    if storage.exists() {
        // Remove old manifest so readers don't see stale readiness.
        // This MUST succeed before we overwrite the snapshot — otherwise a
        // crash between snapshot write and manifest write leaves stale
        // readiness (old manifest + new snapshot).  NotFound is harmless
        // (race or already cleaned up); any other error is fatal.
        match fs::remove_file(storage.manifest_path()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "Failed to remove old manifest at {} — rebuild cannot proceed safely",
                        storage.manifest_path().display()
                    )
                });
            }
        }
    }

    // Step 3: Save binary snapshot
    progress.report(IndexProgress::SavingStarted {
        component_name: "unified graph",
    });
    let save_start = std::time::Instant::now();

    save_to_path(&graph, storage.snapshot_path()).with_context(|| {
        format!(
            "Failed to save snapshot to {}",
            storage.snapshot_path().display()
        )
    })?;

    progress.report(IndexProgress::SavingCompleted {
        component_name: "unified graph",
        save_duration: save_start.elapsed(),
    });

    // Step 4: Compute snapshot checksum
    let snapshot_content =
        fs::read(storage.snapshot_path()).context("Failed to read snapshot for checksum")?;
    let snapshot_sha256 = hex::encode(Sha256::digest(&snapshot_content));

    // Step 5: Build CSR adjacency for canonical edge counts and downstream analyses
    progress.report(IndexProgress::StageStarted {
        stage_name: "Preparing analysis snapshot",
    });
    let analysis_snapshot_start = std::time::Instant::now();

    let snapshot = graph.snapshot();
    let edges = snapshot.edges();
    let forward_store = edges.forward();
    let node_count = snapshot.nodes().len();
    let compaction_snapshot = snapshot_edges(&forward_store, node_count);
    drop(forward_store); // Release lock before heavy computation

    let adjacency = CsrAdjacency::build_from_snapshot(&compaction_snapshot)
        .context("Failed to build CSR adjacency")?;

    progress.report(IndexProgress::StageCompleted {
        stage_name: "Preparing analysis snapshot",
        stage_duration: analysis_snapshot_start.elapsed(),
    });

    let dedup_edge_count = adjacency.edge_count as usize;
    let raw_edge_count = graph.edge_count();

    // Step 6: Build full analyses using the configured label budget.
    progress.report(IndexProgress::StageStarted {
        stage_name: "Computing graph analyses",
    });
    let analysis_start = std::time::Instant::now();

    let analyses = if let Some(thread_count) = config.num_threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(thread_count)
            .build()
            .context("Failed to create rayon thread pool for graph analysis")?
            .install(|| {
                GraphAnalyses::build_all_with_budget(&compaction_snapshot, &config.label_budget)
            })
    } else {
        GraphAnalyses::build_all_with_budget(&compaction_snapshot, &config.label_budget)
    }
    .context("Failed to build graph analyses")?;

    progress.report(IndexProgress::StageCompleted {
        stage_name: "Computing graph analyses",
        stage_duration: analysis_start.elapsed(),
    });

    let analysis_strategies = vec![
        AnalysisStrategySummary {
            edge_kind: "calls",
            strategy: analyses.cond_calls.strategy,
        },
        AnalysisStrategySummary {
            edge_kind: "imports",
            strategy: analyses.cond_imports.strategy,
        },
        AnalysisStrategySummary {
            edge_kind: "references",
            strategy: analyses.cond_references.strategy,
        },
        AnalysisStrategySummary {
            edge_kind: "inherits",
            strategy: analyses.cond_inherits.strategy,
        },
    ];

    // Step 7: Count files by language using plugin detection
    let mut file_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (_file_id, file_path) in graph.indexed_files() {
        let language = plugins
            .plugin_for_path(file_path)
            .map_or_else(|| "unknown".to_string(), |p| p.metadata().id.to_string());
        *file_counts.entry(language).or_insert(0) += 1;
    }
    let total_files: usize = file_counts.values().sum();

    // Step 8: Construct Manifest in memory (with dedup edge count from analysis)
    let built_at = Utc::now().to_rfc3339();

    let manifest = Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        snapshot_format_version: SNAPSHOT_FORMAT_VERSION,
        built_at: built_at.clone(),
        root_path: root.to_string_lossy().to_string(),
        node_count,
        edge_count: dedup_edge_count,
        raw_edge_count: Some(raw_edge_count),
        snapshot_sha256,
        build_provenance: BuildProvenance {
            sqry_version: env!("CARGO_PKG_VERSION").to_string(),
            build_timestamp: built_at.clone(),
            build_command: build_command.to_string(),
            plugin_hashes: std::collections::HashMap::default(),
        },
        file_count: file_counts.clone(),
        languages: Vec::default(),
        config: std::collections::HashMap::default(),
        confidence: graph.confidence().clone(),
        last_indexed_commit: get_git_head_commit(root),
    };

    // Step 9: Serialize manifest to bytes and compute hash
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).context("Failed to serialize manifest")?;

    let manifest_hash = {
        let mut hasher = Sha256::new();
        hasher.update(&manifest_bytes);
        hex::encode(hasher.finalize())
    };

    // Step 10: Construct AnalysisIdentity and persist all analyses
    let node_id_hash = compute_node_id_hash(&snapshot);
    let identity = AnalysisIdentity::new(manifest_hash, node_id_hash);

    fs::create_dir_all(storage.analysis_dir()).with_context(|| {
        format!(
            "Failed to create analysis directory at {}",
            storage.analysis_dir().display()
        )
    })?;

    progress.report(IndexProgress::SavingStarted {
        component_name: "graph analyses",
    });

    analyses
        .persist_all(&storage, &identity)
        .context("Failed to persist graph analyses")?;

    log::info!(
        "Graph analyses persisted to {}",
        storage.analysis_dir().display()
    );

    progress.report(IndexProgress::SavingCompleted {
        component_name: "graph analyses",
        save_duration: analysis_start.elapsed(),
    });

    // Step 11: Write manifest bytes to disk LAST (commit point)
    write_manifest_bytes_atomic(storage.manifest_path(), &manifest_bytes).with_context(|| {
        format!(
            "Failed to save manifest to {}",
            storage.manifest_path().display()
        )
    })?;

    log::info!(
        "Manifest saved to {} (dedup edges: {}, raw edges: {})",
        storage.manifest_path().display(),
        dedup_edge_count,
        raw_edge_count
    );

    let build_result = BuildResult {
        node_count,
        edge_count: dedup_edge_count,
        raw_edge_count,
        file_count: file_counts,
        total_files,
        built_at,
        root_path: root.to_string_lossy().to_string(),
        thread_count: effective_threads,
        analysis_strategies,
    };

    Ok((graph, build_result))
}

/// Get the current HEAD commit SHA from a git repository.
fn get_git_head_commit(path: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;

    if output.status.success() {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(sha);
        }
    }
    None
}

/// Find source files in the given directory.
///
/// Uses the `ignore` crate to respect `.gitignore` files and standard ignore patterns.
fn find_source_files(root: &Path, config: &BuildConfig) -> Vec<std::path::PathBuf> {
    let mut builder = WalkBuilder::new(root);

    builder
        .follow_links(config.follow_links)
        .hidden(!config.include_hidden)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true);

    if let Some(depth) = config.max_depth {
        builder.max_depth(Some(depth));
    }

    if let Some(threads) = config.num_threads {
        builder.threads(threads);
    }

    let mut files = Vec::new();

    for entry in builder.build() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                log::warn!("Failed to read directory entry: {err}");
                continue;
            }
        };

        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            files.push(entry.into_path());
        }
    }

    files
}

fn sort_files_for_build(root: &Path, files: &mut [PathBuf]) {
    let normalized_root = normalize_path_components(root);
    files.sort_by(|left, right| {
        let left_key = file_sort_key(&normalized_root, left);
        let right_key = file_sort_key(&normalized_root, right);
        left_key.cmp(&right_key).then_with(|| left.cmp(right))
    });
}

fn file_sort_key(root: &Path, path: &Path) -> String {
    let normalized_path = normalize_path_components(path);
    let relative = normalized_path
        .strip_prefix(root)
        .unwrap_or(normalized_path.as_path());
    let mut key = relative.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        key = key.to_ascii_lowercase();
    }
    key
}

/// Result of successfully parsing a single file (parallel-safe, no shared state).
struct ParsedFile {
    /// Language identifier for file counting and confidence merging.
    language: crate::graph::Language,
    /// Staged graph operations ready for serial commit.
    staging: StagingGraph,
}

/// Parse a single file into a `StagingGraph` without touching the shared graph.
///
/// This function is safe to call from multiple threads — it creates its own
/// parser, reads the file, and builds a self-contained staging graph.
///
/// Returns `Ok(None)` if the file has no matching plugin or graph builder.
fn parse_file(path: &Path, plugins: &PluginManager) -> Result<Option<ParsedFile>> {
    let plugin = plugins.plugin_for_path(path);
    let Some(plugin) = plugin else {
        return Ok(None);
    };

    let Some(builder) = plugin.graph_builder() else {
        return Ok(None);
    };

    let content = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;

    let tree = plugin
        .parse_ast(&content)
        .map_err(|err| map_parse_error(path, err))?;

    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, &content, path, &mut staging)
        .map_err(|err| map_builder_error(path, &err))?;

    staging.attach_body_hashes(&content);

    Ok(Some(ParsedFile {
        language: builder.language(),
        staging,
    }))
}

fn map_parse_error(path: &Path, err: ParseError) -> anyhow::Error {
    match err {
        ParseError::TreeSitterFailed => {
            anyhow::anyhow!("tree-sitter failed to parse {}", path.display())
        }
        ParseError::LanguageSetFailed(reason) => anyhow::anyhow!(
            "failed to configure tree-sitter for {}: {}",
            path.display(),
            reason
        ),
        _ => anyhow::anyhow!("parse error in {}: {:?}", path.display(), err),
    }
}

fn map_builder_error(path: &Path, err: &GraphBuilderError) -> anyhow::Error {
    anyhow::anyhow!("graph builder error in {}: {}", path.display(), err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Scope;
    use crate::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language};
    use crate::plugin::error::{ParseError, ScopeError};
    use crate::plugin::{LanguageMetadata, LanguagePlugin};
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;
    use tree_sitter::{Parser, Tree};

    const RUST_TEST_EXTENSIONS: &[&str] = &["rs"];
    const FILENAME_MATCH_EXTENSIONS: &[&str] = &["rmd", "bash_profile"];

    /// Test helper: commit a single parsed file to a graph using the serial path.
    ///
    /// This is only used in tests to verify parse-and-commit without running the
    /// full parallel pipeline. It replicates the old `commit_staged_file` logic.
    fn commit_parsed_file_for_test(path: &Path, mut parsed: ParsedFile, graph: &mut CodeGraph) {
        let file_id = graph
            .files_mut()
            .register_with_language(path, Some(parsed.language))
            .expect("register file");
        parsed.staging.apply_file_id(file_id);
        let string_remap = parsed
            .staging
            .commit_strings(graph.strings_mut())
            .expect("commit strings");
        parsed
            .staging
            .apply_string_remap(&string_remap)
            .expect("apply string remap");
        let node_id_mapping = parsed
            .staging
            .commit_nodes(graph.nodes_mut())
            .expect("commit nodes");
        let edges = parsed.staging.get_remapped_edges(&node_id_mapping);
        for edge in edges {
            graph.edges_mut().add_edge_with_spans(
                edge.source,
                edge.target,
                edge.kind.clone(),
                file_id,
                edge.spans.clone(),
            );
        }
    }

    fn parse_rust_ast(content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let language = tree_sitter_rust::LANGUAGE.into();
        parser
            .set_language(&language)
            .map_err(|err| ParseError::LanguageSetFailed(err.to_string()))?;
        parser
            .parse(content, None)
            .ok_or(ParseError::TreeSitterFailed)
    }

    struct TestPlugin {
        metadata: LanguageMetadata,
        extensions: &'static [&'static str],
        builder: Option<Box<dyn GraphBuilder>>,
    }

    impl TestPlugin {
        fn new(
            id: &'static str,
            extensions: &'static [&'static str],
            builder: Option<Box<dyn GraphBuilder>>,
        ) -> Self {
            Self {
                metadata: LanguageMetadata {
                    id,
                    name: "Rust",
                    version: "test",
                    author: "sqry-core tests",
                    description: "Test-only Rust plugin for unified graph entrypoint tests",
                    tree_sitter_version: "0.25",
                },
                extensions,
                builder,
            }
        }
    }

    impl LanguagePlugin for TestPlugin {
        fn metadata(&self) -> LanguageMetadata {
            self.metadata.clone()
        }

        fn extensions(&self) -> &'static [&'static str] {
            self.extensions
        }

        fn language(&self) -> tree_sitter::Language {
            tree_sitter_rust::LANGUAGE.into()
        }

        fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
            parse_rust_ast(content)
        }

        fn extract_scopes(
            &self,
            _tree: &Tree,
            _content: &[u8],
            _file_path: &Path,
        ) -> Result<Vec<Scope>, ScopeError> {
            Ok(Vec::new())
        }

        fn graph_builder(&self) -> Option<&dyn crate::graph::GraphBuilder> {
            self.builder.as_deref()
        }
    }

    struct FailingGraphBuilder;

    impl GraphBuilder for FailingGraphBuilder {
        fn build_graph(
            &self,
            _tree: &Tree,
            _content: &[u8],
            _file: &Path,
            _staging: &mut StagingGraph,
        ) -> GraphResult<()> {
            Err(GraphBuilderError::CrossLanguageError {
                reason: "forced failure".to_string(),
            })
        }

        fn language(&self) -> Language {
            Language::Rust
        }
    }

    struct NoopGraphBuilder;

    impl GraphBuilder for NoopGraphBuilder {
        fn build_graph(
            &self,
            _tree: &Tree,
            _content: &[u8],
            _file: &Path,
            _staging: &mut StagingGraph,
        ) -> GraphResult<()> {
            Ok(())
        }

        fn language(&self) -> Language {
            Language::Rust
        }
    }

    #[test]
    fn test_build_config_default() {
        let config = BuildConfig::default();
        assert_eq!(config.max_depth, None);
        assert!(!config.follow_links);
        assert!(!config.include_hidden);
        assert_eq!(config.num_threads, None);
    }

    #[test]
    fn test_build_unified_graph_empty_registry_error() {
        let plugins = PluginManager::new();
        let config = BuildConfig::default();
        let root = std::path::Path::new(".");

        let result = build_unified_graph(root, &plugins, &config);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "No graph builders registered – cannot build code graph"
        );
    }

    #[test]
    fn test_build_unified_graph_no_graph_builders_error() {
        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-no-graph-builder",
            RUST_TEST_EXTENSIONS,
            None,
        )));
        let config = BuildConfig::default();
        let root = std::path::Path::new(".");

        let result = build_unified_graph(root, &plugins, &config);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "No graph builders registered – cannot build code graph"
        );
    }

    #[test]
    fn test_build_unified_graph_all_failures_error() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("fail.rs");
        fs::write(&file_path, "fn main() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-failing-graph-builder",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(FailingGraphBuilder)),
        )));
        let config = BuildConfig::default();

        let result = build_unified_graph(temp_dir.path(), &plugins, &config);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "All graph builds failed");
    }

    #[test]
    fn test_parse_file_matches_uppercase_extension() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("report.Rmd");
        fs::write(&file_path, "fn main() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-filename-match",
            FILENAME_MATCH_EXTENSIONS,
            Some(Box::new(NoopGraphBuilder)),
        )));
        let mut graph = CodeGraph::new();

        let parsed = parse_file(&file_path, &plugins)
            .expect("parse file")
            .expect("should not be skipped");
        commit_parsed_file_for_test(&file_path, parsed, &mut graph);
    }

    #[test]
    fn test_parse_file_matches_dotless_filename() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("bash_profile");
        fs::write(&file_path, "fn main() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-filename-match",
            FILENAME_MATCH_EXTENSIONS,
            Some(Box::new(NoopGraphBuilder)),
        )));
        let mut graph = CodeGraph::new();

        let parsed = parse_file(&file_path, &plugins)
            .expect("parse file")
            .expect("should not be skipped");
        commit_parsed_file_for_test(&file_path, parsed, &mut graph);
    }

    #[test]
    fn test_parse_file_matches_pulumi_stack_filename() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("Pulumi.dev.yaml");
        fs::write(&file_path, "fn main() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "pulumi",
            &["pulumi.yaml"],
            Some(Box::new(NoopGraphBuilder)),
        )));
        let mut graph = CodeGraph::new();

        let parsed = parse_file(&file_path, &plugins)
            .expect("parse file")
            .expect("should not be skipped");
        commit_parsed_file_for_test(&file_path, parsed, &mut graph);
    }

    // ========================================================================
    // Build pipeline consolidation regression tests
    // ========================================================================

    /// A graph builder that creates a few nodes and edges for testing.
    struct SimpleGraphBuilder;

    impl GraphBuilder for SimpleGraphBuilder {
        fn build_graph(
            &self,
            _tree: &Tree,
            _content: &[u8],
            file: &Path,
            staging: &mut StagingGraph,
        ) -> GraphResult<()> {
            use crate::graph::unified::build::helper::GraphBuildHelper;

            let mut helper = GraphBuildHelper::new(staging, file, Language::Rust);

            // Create two function nodes
            let fn1 = helper.add_function("main", None, false, false);
            let fn2 = helper.add_function("helper", None, false, false);

            // Add a Calls edge from main -> helper
            helper.add_call_edge(fn1, fn2);

            Ok(())
        }

        fn language(&self) -> Language {
            Language::Rust
        }
    }

    /// `build_and_persist_graph` returns a populated `BuildResult`.
    #[test]
    fn test_build_and_persist_graph_returns_build_result() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("test.rs");
        fs::write(&file_path, "fn main() {} fn helper() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-simple",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(SimpleGraphBuilder)),
        )));
        let config = BuildConfig::default();

        let result =
            build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:build_result");
        assert!(result.is_ok(), "build_and_persist_graph should succeed");

        let (_graph, build_result) = result.unwrap();
        assert!(build_result.node_count > 0, "Should have nodes");
        assert!(build_result.total_files > 0, "Should have indexed files");
        assert!(!build_result.built_at.is_empty(), "Should have timestamp");
        assert!(!build_result.root_path.is_empty(), "Should have root path");
    }

    /// Deduplicated `edge_count` is always <= `raw_edge_count`.
    #[test]
    fn test_build_result_edge_count_le_raw() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("test.rs");
        fs::write(&file_path, "fn main() {} fn helper() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-simple",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(SimpleGraphBuilder)),
        )));
        let config = BuildConfig::default();

        let (_graph, build_result) =
            build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:edge_count").unwrap();

        assert!(
            build_result.edge_count <= build_result.raw_edge_count,
            "Deduplicated edge count ({}) should be <= raw edge count ({})",
            build_result.edge_count,
            build_result.raw_edge_count
        );
    }

    /// File counts use plugin detection (keyed by plugin ID).
    #[test]
    fn test_build_and_persist_graph_file_counts_use_plugins() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("test.rs");
        fs::write(&file_path, "fn main() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-simple",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(SimpleGraphBuilder)),
        )));
        let config = BuildConfig::default();

        let (_graph, build_result) =
            build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:file_counts")
                .unwrap();

        // File counts should include the plugin's ID as the language key
        assert!(
            !build_result.file_count.is_empty(),
            "File counts should not be empty"
        );
        assert!(
            build_result.file_count.contains_key("rust-simple"),
            "File counts should use plugin ID. Got: {:?}",
            build_result.file_count
        );
    }

    /// Manifest `edge_count` matches `BuildResult` (deduplicated).
    #[test]
    fn test_manifest_edge_count_is_deduplicated() {
        use crate::graph::unified::persistence::GraphStorage;

        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("test.rs");
        fs::write(&file_path, "fn main() {} fn helper() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-simple",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(SimpleGraphBuilder)),
        )));
        let config = BuildConfig::default();

        let (_graph, build_result) =
            build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:manifest_dedup")
                .unwrap();

        // Load manifest and verify edge counts match BuildResult
        let storage = GraphStorage::new(temp_dir.path());
        assert!(storage.exists(), "Manifest should exist after build");

        let manifest = storage.load_manifest().unwrap();
        assert_eq!(
            manifest.edge_count, build_result.edge_count,
            "Manifest edge_count should match BuildResult (deduplicated)"
        );
        assert_eq!(
            manifest.raw_edge_count,
            Some(build_result.raw_edge_count),
            "Manifest raw_edge_count should match BuildResult"
        );
    }

    /// Build command provenance is recorded in the manifest.
    #[test]
    fn test_build_command_provenance() {
        use crate::graph::unified::persistence::GraphStorage;

        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("test.rs");
        fs::write(&file_path, "fn main() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-simple",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(SimpleGraphBuilder)),
        )));
        let config = BuildConfig::default();

        build_and_persist_graph(temp_dir.path(), &plugins, &config, "cli:index").unwrap();

        let storage = GraphStorage::new(temp_dir.path());
        let manifest = storage.load_manifest().unwrap();
        assert_eq!(
            manifest.build_provenance.build_command, "cli:index",
            "Build command provenance should match"
        );
    }

    /// Analysis identity hash matches the on-disk manifest bytes hash.
    #[test]
    fn test_analysis_identity_matches_manifest_hash() {
        use crate::graph::unified::analysis::persistence::load_csr;
        use crate::graph::unified::persistence::GraphStorage;
        use sha2::{Digest, Sha256};

        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("test.rs");
        fs::write(&file_path, "fn main() {} fn helper() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-simple",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(SimpleGraphBuilder)),
        )));
        let config = BuildConfig::default();

        build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:identity").unwrap();

        let storage = GraphStorage::new(temp_dir.path());

        // Compute manifest hash from on-disk manifest bytes
        let manifest_bytes = std::fs::read(storage.manifest_path()).unwrap();
        let expected_hash = hex::encode(Sha256::digest(&manifest_bytes));

        // Load analysis identity from the CSR file (identity is embedded in each analysis file)
        let (_csr, identity) = load_csr(&storage.analysis_csr_path()).unwrap();

        assert_eq!(
            identity.manifest_hash, expected_hash,
            "On-disk manifest hash should equal analysis identity hash"
        );
    }

    /// Regression test: old manifest is removed at start of rebuild.
    ///
    /// Verifies that `build_and_persist_graph_with_progress()` removes any
    /// existing manifest before writing the new snapshot. This prevents the
    /// inconsistent state where an old manifest pairs with a new snapshot
    /// after an interrupted rebuild.
    #[test]
    fn test_old_manifest_removed_during_rebuild() {
        use crate::graph::unified::persistence::GraphStorage;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let src = temp_dir.path().join("lib.rs");
        std::fs::write(&src, "fn main() {}").unwrap();

        // Build an initial index
        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-simple",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(SimpleGraphBuilder)),
        )));
        let config = BuildConfig::default();
        build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:initial").unwrap();

        let storage = GraphStorage::new(temp_dir.path());
        assert!(
            storage.exists(),
            "Manifest should exist after initial build"
        );

        // Record the original manifest's built_at timestamp
        let original_manifest = storage.load_manifest().unwrap();
        let original_built_at = original_manifest.built_at.clone();

        // Rebuild — during the build, the old manifest should be removed first
        build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:rebuild").unwrap();

        // Verify the manifest was replaced (different built_at timestamp)
        let new_manifest = storage.load_manifest().unwrap();
        assert_ne!(
            original_built_at, new_manifest.built_at,
            "Manifest should have been replaced with new timestamp"
        );
        assert_eq!(
            new_manifest.build_provenance.build_command, "test:rebuild",
            "Manifest should reflect the rebuild provenance"
        );
    }

    /// Regression test: failed rebuild leaves index in non-ready state.
    ///
    /// Exercises the real pipeline by making the analysis directory
    /// non-writable after an initial build, then attempting a rebuild.
    /// The pipeline should:
    ///   1. Remove the old manifest (Step 2) — making `exists()` false.
    ///   2. Write the new snapshot (Step 3).
    ///   3. Fail at analysis persistence (Step 9) because the directory
    ///      is not writable.
    ///   4. Return an error — manifest is NEVER written.
    ///
    /// After the failed rebuild, `storage.exists()` must be false (old
    /// manifest removed), even though the snapshot file was updated.
    #[test]
    fn test_failed_rebuild_leaves_index_not_ready() {
        use crate::graph::unified::persistence::GraphStorage;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let src = temp_dir.path().join("lib.rs");
        std::fs::write(&src, "fn main() {}").unwrap();

        // Build an initial index (success)
        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-simple",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(SimpleGraphBuilder)),
        )));
        let config = BuildConfig::default();
        build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:initial").unwrap();

        let storage = GraphStorage::new(temp_dir.path());
        assert!(
            storage.exists(),
            "Manifest should exist after initial build"
        );

        // Replace the analysis directory with a regular file to force a
        // failure at Step 9 (analysis persistence). `create_dir_all` will
        // fail because a regular file exists where a directory is expected.
        // This simulates the real failure window between snapshot write
        // (Step 3) and manifest write (Step 10).
        let analysis_dir = storage.analysis_dir().to_path_buf();
        std::fs::remove_dir_all(&analysis_dir).unwrap();
        std::fs::write(&analysis_dir, b"blocker").unwrap();

        // Attempt rebuild — should fail at analysis persistence
        let result =
            build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:failed_rebuild");

        // Restore analysis dir so TempDir cleanup succeeds
        std::fs::remove_file(&analysis_dir).unwrap();
        std::fs::create_dir_all(&analysis_dir).unwrap();

        // The build should have failed
        assert!(
            result.is_err(),
            "Rebuild should fail when analysis dir is read-only"
        );

        // The old manifest should have been removed (Step 2 ran before failure)
        assert!(
            !storage.exists(),
            "After failed rebuild, manifest should have been removed — index is NOT ready"
        );

        // The snapshot was updated (Step 3 succeeded before failure)
        assert!(
            storage.snapshot_exists(),
            "Snapshot should still exist on disk (written before failure)"
        );
    }
}
