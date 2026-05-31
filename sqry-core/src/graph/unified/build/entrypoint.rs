//! Build entrypoint for unified graph.
//!
//! This module provides the top-level API for building a unified graph from source files.
//! It orchestrates file discovery and delegates to the 5-pass build pipeline.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use rayon::prelude::*;

use crate::graph::GraphBuilderError;
use crate::graph::error::GraphResult;
use crate::graph::unified::analysis::LabelBudgetConfig;
use crate::graph::unified::analysis::ReachabilityStrategy;
use crate::graph::unified::build::StagingGraph;
use crate::graph::unified::build::cancellation::CancellationToken;
use crate::graph::unified::build::parallel_commit::{
    GlobalOffsets, PhaseCIndirectDrain, phase2_assign_ranges, phase3_parallel_commit,
    phase4_apply_global_remap, phase4c_prime_unify_cross_file_nodes, phase4d_bulk_insert_edges,
};
use crate::graph::unified::build::pass3_intra::PendingEdge;
use crate::graph::unified::build::progress::GraphBuildProgressTracker;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::storage::c_indirect::{
    BindingEntry, CIndirectSideTables, IndirectCallsite,
};
use crate::io::FileReader;
use crate::plugin::PluginManager;
use crate::plugin::error::ParseError;
use crate::plugin::{SafeParser, SafeParserConfig};
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

    /// Deterministic ordered built-in plugin ids active during the build.
    pub active_plugin_ids: Vec<String>,

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

const BYTES_PER_MIB: usize = 1024 * 1024;
const INDEX_WORKER_STACK_SIZE_ENV: &str = "SQRY_INDEX_WORKER_STACK_MB";
const DEFAULT_INDEX_WORKER_STACK_SIZE_MB: usize = 32;
const MIN_INDEX_WORKER_STACK_SIZE_MB: usize = 8;
const MAX_INDEX_WORKER_STACK_SIZE_MB: usize = 256;

/// Directory names skipped by default when discovering first-party source files.
///
/// These are dependency, build output, editor cache, or CI runner cache roots
/// that routinely contain generated code or vendored third-party dependencies.
/// The indexer still honors `.gitignore` and related ignore files; this list
/// protects editor-triggered indexing when those files are absent or incomplete.
/// Set `SQRY_INCLUDE_DEFAULT_EXCLUDED_DIRS=1` to disable these built-in
/// excludes for repositories that intentionally keep first-party code in one
/// of these directories.
const DEFAULT_EXCLUDED_SOURCE_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".cache",
    ".next",
    ".nuxt",
    ".sqry",
    ".turbo",
    ".venv",
    "__pycache__",
    "_actions",
    "_update",
    "_work",
    "build",
    "dist",
    "node_modules",
    "target",
    "vendor",
    "venv",
];

const DEFAULT_EXCLUDED_SOURCE_DIR_PREFIXES: &[&str] = &["externals."];

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
        .stack_size(index_worker_stack_size_bytes())
        .build()
        .context("Failed to create rayon thread pool for graph indexing")
}

#[must_use]
fn index_worker_stack_size_bytes() -> usize {
    let env_value = std::env::var(INDEX_WORKER_STACK_SIZE_ENV).ok();
    index_worker_stack_size_bytes_from_value(env_value.as_deref())
}

#[must_use]
fn index_worker_stack_size_bytes_from_value(value: Option<&str>) -> usize {
    let size_mb = value
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_INDEX_WORKER_STACK_SIZE_MB)
        .clamp(
            MIN_INDEX_WORKER_STACK_SIZE_MB,
            MAX_INDEX_WORKER_STACK_SIZE_MB,
        );
    size_mb * BYTES_PER_MIB
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
    build_unified_graph_cancellable(root, plugins, config, &CancellationToken::default())
        .map_err(anyhow::Error::from)
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
) -> Result<(CodeGraph, usize)> {
    build_unified_graph_with_progress_cancellable(
        root,
        plugins,
        config,
        progress,
        &CancellationToken::default(),
    )
    .map_err(anyhow::Error::from)
}

/// Build a unified graph with cooperative cancellation.
///
/// Behaves identically to [`build_unified_graph`] except that the
/// `cancellation` token is polled at every pass boundary. A cancelled
/// token causes the pipeline to return [`GraphBuilderError::Cancelled`]
/// at the next boundary.
///
/// Used by the sqryd daemon's rebuild dispatcher to abort in-flight
/// full rebuilds when a workspace is evicted mid-build.
///
/// # Errors
///
/// Returns [`GraphBuilderError::Cancelled`] if the token is cancelled
/// at any pass boundary; otherwise the same error modes as
/// [`build_unified_graph`] (lifted from `anyhow::Error` into
/// [`GraphBuilderError::Internal`]).
pub fn build_unified_graph_cancellable(
    root: &Path,
    plugins: &PluginManager,
    config: &BuildConfig,
    cancellation: &CancellationToken,
) -> GraphResult<CodeGraph> {
    let (graph, _effective_threads) =
        build_unified_graph_inner(root, plugins, config, no_op_reporter(), cancellation)?;
    Ok(graph)
}

/// Build a unified graph with cooperative cancellation AND a progress
/// reporter.
///
/// Combines [`build_unified_graph_cancellable`] + the progress
/// reporter variant.
///
/// # Errors
///
/// Same as [`build_unified_graph_cancellable`].
pub fn build_unified_graph_with_progress_cancellable(
    root: &Path,
    plugins: &PluginManager,
    config: &BuildConfig,
    progress: SharedReporter,
    cancellation: &CancellationToken,
) -> GraphResult<(CodeGraph, usize)> {
    build_unified_graph_inner(root, plugins, config, progress, cancellation)
}

/// Internal implementation that returns the effective thread count alongside the graph.
///
/// Used by [`build_and_persist_graph_with_progress`] to propagate the thread count
/// into `BuildResult` without exposing it in the public API.
///
/// Accepts a [`CancellationToken`] which is polled at every pass
/// boundary. Callers that do not need cancellation pass
/// `&CancellationToken::default()` (via the `build_unified_graph` +
/// `build_unified_graph_with_progress` wrappers).
#[allow(clippy::too_many_lines)] // Complex 5-pass build pipeline requires sequential flow
fn build_unified_graph_inner(
    root: &Path,
    plugins: &PluginManager,
    config: &BuildConfig,
    progress: SharedReporter,
    cancellation: &CancellationToken,
) -> GraphResult<(CodeGraph, usize)> {
    if !root.exists() {
        return Err(GraphBuilderError::Internal {
            reason: format!("Path {} does not exist", root.display()),
        });
    }

    log::info!(
        "Building unified graph from source files in {}",
        root.display()
    );

    // 7c cancellation boundary 1: pre-build, after arg validation.
    cancellation.check()?;

    let has_graph_builders = plugins
        .plugins()
        .iter()
        .any(|plugin| plugin.graph_builder().is_some());
    if !has_graph_builders {
        return Err(GraphBuilderError::Internal {
            reason: "No graph builders registered – cannot build code graph".to_string(),
        });
    }

    // Create progress tracker for this build
    let tracker = GraphBuildProgressTracker::new(progress);

    // 1. Find source files
    let mut files = find_source_files(root, config);
    sort_files_for_build(root, &mut files);

    // 7c cancellation boundary 2: after file discovery, before thread
    // pool creation + graph allocation.
    cancellation.check()?;

    // 2. Create the unified graph
    let mut graph = CodeGraph::new();

    // 3. Create scoped thread pool for parallel parse
    let pool = create_thread_pool(config).map_err(|e| GraphBuilderError::Internal {
        reason: format!("thread pool: {e}"),
    })?;
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

    let (mut succeeded, mut parse_errors, mut skipped, mut timed_out) =
        (0usize, 0usize, 0usize, 0usize);
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
    // Accumulate per-chunk C indirect-call drains (DESIGN §8.2 / U11).
    // Stays at `default()` (empty) for non-C workspaces — only C plugin
    // Phase 1 walkers (U10) push into the per-file
    // `CIndirectStagingPayload`. Applied AFTER Phase 4c-prime cross-file
    // unification so the post-unification `by_qualified_name` index is in
    // its final canonical-winners-only state.
    let mut c_indirect_pending: PhaseCIndirectDrain = PhaseCIndirectDrain::default();

    let chunks = compute_parse_chunks(&files, &pool, plugins, config.staging_memory_limit);
    for chunk_range in chunks {
        // 7c cancellation boundary 3: top of each chunk iteration.
        cancellation.check()?;

        let chunk_files = &files[chunk_range];

        // 7c test hook: observation point fired at the top of each
        // chunk. Tests that need to flip the cancellation token
        // between chunks register a callback here. Production builds
        // compile this call out entirely.
        #[cfg(any(test, feature = "rebuild-internals"))]
        testing::fire_after_chunk_hook(cancellation);

        // Phase 1: Parallel parse this chunk
        let staged_results: Vec<(PathBuf, Result<ParsedFileOutcome>)> = pool.install(|| {
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
                Ok(ParsedFileOutcome::Parsed(parsed)) => {
                    let file_bytes = parsed.staging.estimated_byte_size();
                    total_staging_bytes += file_bytes;
                    chunk_staging_bytes += file_bytes;
                    if file_bytes > max_file_staging_bytes {
                        max_file_staging_bytes = file_bytes;
                    }
                    chunk_parsed.push((path, parsed));
                }
                Ok(ParsedFileOutcome::Skipped) => skipped += 1,
                Ok(ParsedFileOutcome::TimedOut {
                    file,
                    phase,
                    timeout_ms,
                }) => {
                    timed_out += 1;
                    log::warn!(
                        "Timed out building graph for {} during {} after {} ms",
                        file.display(),
                        phase,
                        timeout_ms,
                    );
                }
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
        let file_ids = graph.files_mut().register_batch(&file_info).map_err(|e| {
            GraphBuilderError::Internal {
                reason: format!("Failed to register files: {e}"),
            }
        })?;

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
            .map_err(|e| GraphBuilderError::Internal {
                reason: format!("Failed to alloc node range: {e:?}"),
            })?;
        graph
            .strings_mut()
            .alloc_range(plan.total_strings)
            .map_err(|e| GraphBuilderError::Internal {
                reason: format!("Failed to alloc string range: {e}"),
            })?;

        // Phase 3: Parallel commit into disjoint pre-allocated ranges.
        // Use pool.install to respect BuildConfig::num_threads for rayon par_iter.
        //
        // `phase3_parallel_commit` is generic over
        // `G: GraphMutationTarget` as of Task 4 Step 4 Phase 1; here
        // the inferred `G` is `CodeGraph`, and the helper reaches the
        // arena + interner via `graph.nodes_and_strings_mut()`
        // internally.
        let phase3 = pool.install(|| phase3_parallel_commit(&plan, &staging_refs, &mut graph));

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
            return Err(GraphBuilderError::Internal {
                reason: format!(
                    "Phase 3 count mismatch: nodes {}/{expected_nodes}, strings {}/{expected_strings}, edges {}/{expected_edges}. This indicates a bug in StagingGraph counting.",
                    phase3.total_nodes_written,
                    phase3.total_strings_written,
                    phase3.total_edges_collected,
                ),
            });
        }

        // Populate FileSegmentTable from the chunk's file plans.
        for fp in &plan.file_plans {
            let start = fp.node_range.start;
            let count = fp.node_range.end.saturating_sub(start);
            graph
                .file_segments_mut()
                .record_range(fp.file_id, start, count);
        }

        // Populate FileRegistry::per_file_nodes from Phase 3's
        // committed-NodeId vectors. This is the Gate 0c iter-2 B2 fix
        // (pulled base-plan Step 1 forward): each NodeId committed by
        // parallel-parse is bucketed by its owning FileId so the
        // bucket-bijection debug invariant at publish time can verify
        // arena ↔ bucket consistency against real data instead of a
        // vacuously-empty map.
        //
        // Iteration order matches `plan.file_plans`, which is
        // deterministic across runs. `per_file_node_ids[i]` is the
        // set of NodeIds committed for `plan.file_plans[i]`; the
        // registry's `record_node` is O(1) amortised per call.
        debug_assert_eq!(
            phase3.per_file_node_ids.len(),
            plan.file_plans.len(),
            "phase3 per-file node ID vector length must match plan length"
        );
        for (fp, node_ids) in plan.file_plans.iter().zip(phase3.per_file_node_ids.iter()) {
            for nid in node_ids {
                graph.files_mut().record_node(fp.file_id, *nid);
            }
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

        // 7c cancellation boundary 4: after chunk commit, before
        // accumulating edges for Phase 4.
        cancellation.check()?;

        // Accumulate edges for Phase 4
        all_edges.extend(phase3.per_file_edges);

        // Accumulate C indirect-call drain into the workspace-global
        // pending buffer (DESIGN §8.2 / U11). `None` for chunks with no C
        // files. Merge is a `Vec::append` for each inner vec — O(n).
        if let Some(drain) = phase3.c_indirect_drain {
            c_indirect_pending.merge(drain);
        }
    }
    tracker.complete_phase();

    // 7c test hook: observation point fired after the chunk loop exits
    // and before Phase 4 finalization. Tests that need to flip the
    // cancellation token at this boundary register a callback here.
    #[cfg(any(test, feature = "rebuild-internals"))]
    testing::fire_before_phase4_hook(cancellation);

    // Phase 4: Post-chunk finalization
    tracker.start_phase(4, "Finalizing graph", 5);

    // 7c cancellation boundary 5: pre-Phase-4a.
    cancellation.check()?;

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

    // 7c cancellation boundary 6: pre-Phase-4c (rebuild_indices).
    cancellation.check()?;

    // Phase 4c: Build indices from finalized arena.
    // Uses build_from_arena() which is O(n log n) — no per-element duplicate check.
    graph.rebuild_indices();
    tracker.increment_progress(); // 4c done

    // 7c cancellation boundary 7: pre-Phase-4c-prime
    // (phase4c_prime_unify_cross_file_nodes).
    cancellation.check()?;

    // Phase 4c-prime: Cross-file node unification.
    // Walk the arena for nodes sharing a qualified name and a call-compatible kind,
    // merge duplicates into a single canonical node, and rewrite PendingEdge targets.
    // Must run AFTER rebuild_indices (uses by_qualified_name) and BEFORE Phase 4d
    // (operates on PendingEdge, not committed DeltaEdge).
    let unification_stats = phase4c_prime_unify_cross_file_nodes(&mut graph, &mut all_edges);
    if unification_stats.nodes_merged > 0 {
        log::info!(
            "Phase 4c-prime: unified {} duplicate nodes ({} candidate groups examined, \
             {} edges rewritten, {} ms)",
            unification_stats.nodes_merged,
            unification_stats.candidate_pairs_examined,
            unification_stats.edges_rewritten,
            unification_stats.elapsed_ms,
        );
        // 7c cancellation boundary 7b: post-4c-prime, before the
        // optional second rebuild_indices. Codex iter-0 MAJOR: without
        // this check, a cancellation observed after the unification
        // walk still pays another O(n log n) index rebuild.
        cancellation.check()?;
        // Rebuild indices after tombstoning loser nodes
        graph.rebuild_indices();
    }
    tracker.increment_progress(); // 4c-prime done

    // Phase 4c-prime-post (U11 / DESIGN §8.3): apply deferred C
    // indirect-call side-tables now that the qualified-name index reflects
    // post-unification canonical winners.
    //
    // Runs AFTER Phase 4c-prime (the indices are coherent — losers are
    // tombstoned, name/qualified_name index buckets contain only canonical
    // winners; verified at `concurrent/graph.rs:1948-1950, 2076`) and
    // BEFORE Phase 4d (bulk edge insert) — Phase 4d does not touch the
    // `c_indirect_tables` slot or the metadata store, so this ordering
    // keeps both Phase 4c-prime invariants (indices coherent) and Phase 4d
    // invariants (edge store untouched until bulk insert) intact.
    //
    // Per the U11 plan iter-2 correction (TRACEABILITY:IMP:c-icall-precision-011),
    // resolution goes through `graph.indices().by_qualified_name(str_id)`
    // (with a `by_name` fallback for languages whose canonical qualified
    // name equals the semantic name and therefore leaves
    // `NodeEntry::qualified_name` unset — e.g. C) — NOT through Phase
    // 4c-prime's internal `NodeRemapTable`, which is `pub(crate)` and
    // consumed inside `phase4c_prime_unify_cross_file_nodes` (see
    // `parallel_commit.rs:884-885`).
    if !c_indirect_pending.is_empty() {
        let stats = apply_deferred_address_taken_marks(&mut graph, c_indirect_pending);
        log::info!(
            target: "sqry_core::build",
            "Phase 4c-prime-post: applied C indirect side-tables — {} address-taken marks, \
             {} struct field signatures, {} bindings (resolved {} entries), \
             {} pending callsites, {} local-scope indices, {} unresolved names",
            stats.address_taken_marks_applied,
            stats.struct_field_signatures_inserted,
            stats.binding_entries_inserted,
            stats.bindings_resolved,
            stats.indirect_callsites_inserted,
            stats.local_scope_indices_inserted,
            stats.unresolved_names,
        );
    }

    // 7c cancellation boundary 8: pre-Phase-4d (bulk edge insert).
    cancellation.check()?;

    // Phase 4d: Bulk insert edges via deterministic DeltaEdge conversion.
    // Wraps the pure pending_edges_to_delta + add_edges_bulk_ordered pair
    // behind phase4d_bulk_insert_edges so the incremental rebuild path
    // (Task 4 Step 4 Phase 3) can reuse the same helper against a
    // RebuildGraph. The helper carries forward the edge store's current
    // seq counter so non-empty graphs advance deterministically.
    let _final_edge_seq = phase4d_bulk_insert_edges(&mut graph, &all_edges);
    tracker.increment_progress(); // 4d done
    tracker.complete_phase();

    log::info!(
        "Parallel indexing complete: {succeeded} committed, {skipped} skipped, \
         {timed_out} timed out, {parse_errors} parse errors, \
         ~{} MB total staged, ~{} MB peak chunk (max single file: ~{} KB)",
        total_staging_bytes / (1024 * 1024),
        peak_chunk_staging_bytes / (1024 * 1024),
        max_file_staging_bytes / 1024,
    );

    let attempted = succeeded + parse_errors + timed_out;

    if attempted == 0 {
        log::warn!(
            "No eligible source files found for graph build in {}",
            root.display()
        );
    }

    if attempted > 0 && succeeded == 0 {
        return Err(GraphBuilderError::Internal {
            reason: "All graph builds failed".to_string(),
        });
    }

    // 7c cancellation boundary 9: pre-Phase-4e (binding plane).
    cancellation.check()?;

    // ------------------------------------------------------------------
    // Phase 4e — Binding plane derivation.
    //
    // Runs between Phase 4d (bulk edge insert) and Pass 5 (cross-language
    // linking). Consumes only the language-local edge kinds Contains,
    // Defines, Imports, Exports. Populates CodeGraph::scope_arena (P2U03),
    // CodeGraph::alias_table (P2U04), CodeGraph::shadow_table (P2U05), and
    // CodeGraph::scope_provenance_store (P2U11) in one pass.
    // ------------------------------------------------------------------
    tracker.start_phase(5, "Binding plane derivation", 1);
    let binding_stats = super::phase4e_binding::derive_binding_plane(&mut graph);
    log::info!(
        target: "sqry_core::build",
        "Phase 4e: {} scopes, {} aliases, {} shadows derived",
        binding_stats.scopes,
        binding_stats.aliases,
        binding_stats.shadows,
    );
    tracker.increment_progress();
    tracker.complete_phase();

    // ------------------------------------------------------------------
    // Pass 5b — C indirect-call resolution (Phase A, U12).
    //
    // Runs AFTER Phase 4e (binding-plane derivation) and BEFORE Pass 5
    // (cross-language linking). It also runs before the Go T1 method-set
    // satisfaction pass so Go sees the same pre-Pass-5 graph plus any C
    // indirect-call precision edges. Consumes CodeGraph::c_indirect_tables
    // populated by U10/U11 and rewrites synthetic indirect-call Calls edges
    // into precise binding-plane / type-match candidates.
    //
    // Deliberately placed BEFORE `fire_before_pass5_hook` so existing
    // test hooks observe a graph that already has resolved indirect
    // calls and Go method-set edges — preserving the hook's "before
    // cross-language linking" semantic. See IMPL_PLAN §"U12 —
    // pass5b_c_indirect_resolve".
    // ------------------------------------------------------------------
    cancellation.check()?;
    tracker.start_phase(6, "C indirect-call resolution", 1);
    let pass5b_stats = super::pass5b_c_indirect::resolve_c_indirect_calls(&mut graph);
    log::info!(
        target: "sqry_core::build",
        "Pass 5b: binding={}, typematch={}, cap_exceeded={}, fallback={}",
        pass5b_stats.binding_resolved,
        pass5b_stats.typematch_resolved,
        pass5b_stats.cap_exceeded,
        pass5b_stats.stub_fallback,
    );
    tracker.increment_progress();
    tracker.complete_phase();

    // Go T1 method-set satisfaction pass (Cluster E1 wiring).
    //
    // Runs between Phase A pass 5b (C indirect-call resolution) and Pass
    // 5 (cross-language linking). Full-build plane: `changed_files =
    // None` because no prior pass-owned state exists; the pass walks the
    // entire graph and emits T1.2 promoted methods, T1.1 implicit
    // `Implements`, and T1.3 function-signature `Implements`. The
    // tombstone-before-emit step (02_DESIGN §3.6) is skipped on this plane.
    //
    // No-op on workspaces with zero Go nodes — the pass body
    // short-circuits via the `embeddings.is_empty()` check before
    // touching the graph (see `pass_go_method_set.rs` step 1).
    // ------------------------------------------------------------------
    cancellation.check()?;
    tracker.start_phase(7, "Go method-set satisfaction", 1);
    let go_method_set_stats =
        super::pass_go_method_set::run_go_method_set_satisfaction(&mut graph, None);
    log::info!(
        target: "sqry_core::build",
        "Go method-set: {} value-form Implements, {} pointer-form Implements, \
         {} signature Implements, {} promoted methods, {} shadow Calls/References, \
         elapsed_ms={}",
        go_method_set_stats.implements_edges_value,
        go_method_set_stats.implements_edges_pointer,
        go_method_set_stats.signature_implements_edges,
        go_method_set_stats.promoted_method_nodes,
        go_method_set_stats.promoted_back_reference_edges,
        go_method_set_stats.elapsed_ms,
    );
    tracker.increment_progress();
    tracker.complete_phase();

    // 7c test hook: observation point fired before Pass 5. Tests that
    // need to flip the cancellation token at this boundary register a
    // callback here (fires BEFORE the check below so a hook that flips
    // the token is observed by the subsequent check).
    #[cfg(any(test, feature = "rebuild-internals"))]
    testing::fire_before_pass5_hook(cancellation);

    // 7c cancellation boundary 10: pre-Pass-5 (cross-language linking).
    cancellation.check()?;

    // Pass 5: Cross-language linking (FFI declarations → C/C++ functions, HTTP requests → endpoints)
    tracker.start_phase(8, "Cross-language linking", 1);
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

    // Publish-boundary invariants (A2 §F / Task 4 Gate 0d).
    //
    // This is the canonical "full rebuild end" call site named in plan
    // §F.3. Full rebuilds have no tombstoned NodeIds to carry forward,
    // so the §F.2 residue check does not run here — per plan §H step
    // 14, the residue check has EXACTLY ONE call site
    // (`RebuildGraph::finalize` step 14) against the drained tombstone
    // set. Full rebuilds run the §F.1 bucket bijection only, via
    // [`crate::graph::unified::publish::assert_publish_bijection`]:
    // every parallel-commit chunk populates per-file buckets via
    // `FileRegistry::record_node`, and the bijection proves no file
    // ended up with a dead / duplicate / misfiled / missing node.
    //
    // In release builds the helper is a no-op; see `publish.rs`.
    super::super::publish::assert_publish_bijection(&graph);

    Ok((graph, effective_threads))
}

/// Statistics returned by [`apply_deferred_address_taken_marks`].
///
/// Each counter is incremented exactly once per drained payload entry,
/// matching the per-vec input cardinalities in
/// [`PhaseCIndirectDrain`]. `unresolved_names` counts every staging name
/// for which neither `by_qualified_name` nor `by_name` produced a
/// CALL_COMPATIBLE_KINDS-filtered hit — typically address-taken names
/// referring to functions defined in files the plugin couldn't reach (a
/// forward declaration `void foo(void);` with no matching definition
/// anywhere in the workspace) or to the `cb_alpha` pattern when the
/// matching definition lives outside the indexed source tree.
#[derive(Debug, Default, Clone)]
pub(crate) struct DeferredCIndirectStats {
    /// Number of `mark_address_taken` calls successfully applied. Counts
    /// EACH match — a name resolving to N nodes contributes N to this
    /// counter, per SPEC §3.1.2 (mark every match on ambiguity).
    pub address_taken_marks_applied: usize,
    /// Number of `(struct_tag, field_name) → signature` entries inserted
    /// into `CIndirectSideTables::struct_field_fnptr`.
    pub struct_field_signatures_inserted: usize,
    /// Number of `BindingEntry` values inserted into
    /// `CIndirectSideTables::bindings_by_field` across all keys.
    pub binding_entries_inserted: usize,
    /// Number of bindings whose `instance_name` AND `target_fn_name` both
    /// resolved to a canonical NodeId. Bindings that fail to resolve
    /// either name are dropped (not inserted) because both NodeIds are
    /// required to construct a valid `BindingEntry`.
    pub bindings_resolved: usize,
    /// Number of `IndirectCallsite` entries pushed onto
    /// `CIndirectSideTables::pending_callsites`. A callsite whose
    /// `caller_qualified_name` fails to resolve is dropped — U12's
    /// resolver requires a real caller NodeId to rewrite the synthetic
    /// `Calls` edge.
    pub indirect_callsites_inserted: usize,
    /// Number of `(file_id, LocalScopeIndex)` pairs inserted. Last write
    /// wins on duplicate keys (the C plugin guarantees per-file
    /// uniqueness, but defensive duplicate handling lives at the HashMap
    /// layer).
    pub local_scope_indices_inserted: usize,
    /// Names that resolved to zero CALL_COMPATIBLE_KINDS nodes — see the
    /// struct-level doc for typical causes.
    pub unresolved_names: usize,
}

/// Apply deferred C indirect-call side-tables to the post-unification graph.
///
/// Runs once, sequentially, immediately after `phase4c_prime_unify_cross_file_nodes`
/// returns (and after the optional second `rebuild_indices` call that
/// keeps the by-name / by-qualified-name buckets coherent post-merge).
/// Drives U11 from the U10 staging payload through to the final
/// persisted [`CIndirectSideTables`] + [`NodeFlags::ADDRESS_TAKEN`]
/// state.
///
/// # Resolution algorithm (per DESIGN §8.2 / §8.3, iter-2-corrected)
///
/// For each entry that names a function by qualified name (`address_taken_names`,
/// `bindings.instance_name`, `bindings.target_fn_name`,
/// `indirect_callsites[].caller_qualified_name`):
///
/// 1. Intern the name through `graph.strings_mut().intern(...)` — this
///    interns into the **post-Phase-4a-dedup, post-Phase-4c** canonical
///    interner, so the resulting `StringId` is the canonical id every
///    AuxiliaryIndices bucket is keyed on.
/// 2. Look up matches via `graph.indices().by_qualified_name(str_id)`
///    first (handles languages whose canonical qualified name differs
///    from the semantic name — e.g. Rust `foo::bar::baz`), and union with
///    `by_name(str_id)` (handles languages whose canonical qualified
///    name equals the semantic name and therefore left
///    `NodeEntry::qualified_name = None` — the entire C plugin route,
///    where `cb_alpha` is its own qualified name; see
///    `sqry-lang-c/src/relations/graph_builder.rs:230`).
/// 3. Filter the union to `CALL_COMPATIBLE_KINDS` (`Function`, `Method`,
///    `Macro`, `Constant`, `LambdaTarget`) — the same kind-set Phase
///    4c-prime uses for unification. Deduplicate (a node may appear in
///    both buckets when `qualified_name == name`). Additionally
///    constrain the candidate set to nodes whose owning file's
///    language is `Language::C` (via
///    `graph.files().language_for_file(entry.file)`). This is required
///    by SPEC §3.1.2 line 163 ("Every C `NodeKind::Function`...") and
///    DESIGN §8.2 lines 1239-1241 — the by_name fallback is
///    workspace-global, so without this filter a Rust `fn cb_alpha`,
///    Python `def cb_alpha`, etc. sharing a bare name with a C symbol
///    would be erroneously marked address-taken (or inserted into
///    `bindings_by_field` / `pending_callsites`) by C-only semantics.
/// 4. For `address_taken_names`: call `mark_address_taken` on every
///    matched NodeId (SPEC §3.1.2 — "is this function ever
///    address-taken?" — every plausible target gets flagged).
///
/// Tombstoned losers are absent from both `by_name` and `by_qualified_name`
/// post-Phase-4c-prime — `merge_node_into` clears their name/qualified_name
/// fields to `StringId::INVALID` / `None`, and `build_from_arena`
/// (re-run from `rebuild_indices`) skips them. The marks therefore land
/// only on canonical winners.
///
/// Tier-3 metadata revision counter bumps automatically via the
/// `mark_address_taken` mutation (see DESIGN §9.2 / CLAUDE.md "Derived
/// Analysis DB").
pub(crate) fn apply_deferred_address_taken_marks(
    graph: &mut CodeGraph,
    pending: PhaseCIndirectDrain,
) -> DeferredCIndirectStats {
    use super::helper::CALL_COMPATIBLE_KINDS;

    let mut stats = DeferredCIndirectStats::default();

    // Local-scope indices need no name resolution. Drain them first so
    // the c_indirect_tables slot is materialised before we start interning
    // strings (which mutably borrow graph).
    if !pending.local_scope_indices.is_empty() {
        let tables = graph
            .c_indirect_tables_mut()
            .get_or_insert_with(CIndirectSideTables::new);
        for (file_id, scope_index) in pending.local_scope_indices {
            // Defensive: in-spec, the C plugin pushes exactly one scope
            // index per file. Last-write-wins on duplicate keys (the
            // HashMap semantics) preserves the most-recently-staged
            // index, matching the U09 wire-shape contract.
            tables.local_scope_indices.insert(file_id, scope_index);
            stats.local_scope_indices_inserted += 1;
        }
    }

    // Resolve every distinct name to its canonical NodeId set ONCE,
    // cached in a HashMap. The drain may contain duplicates (the same
    // address-taken name across multiple sites/files), and bindings'
    // target_fn_name + indirect_callsites' caller_qualified_name share
    // the same callable-only lookup surface as address_taken_names.
    // Caching avoids re-walking the indices for every duplicate.
    //
    // Resolution = intern name → by_qualified_name ∪ by_name → filter →
    // dedupe. Holds the resolved NodeId vector (possibly empty for
    // unresolved names).
    //
    // Two caches with DIFFERENT kind filters:
    //
    // * `name_to_node_ids`: filtered to `CALL_COMPATIBLE_KINDS`
    //   (Function/Method/Macro/Constant/LambdaTarget). Used for any
    //   name that must resolve to a callable target —
    //   `address_taken_names`, `bindings.target_fn_name`,
    //   `indirect_callsites.caller_qualified_name`.
    //
    // * `instance_to_node_ids`: filtered to `Variable` only. Used for
    //   `bindings.instance_name` — the ops-table variable that holds
    //   the binding. Without this second filter the instance lookup
    //   would always fail (Variable ∉ CALL_COMPATIBLE_KINDS), dropping
    //   every legal binding emitted by the U10 designated-initializer
    //   path (graph_builder.rs:971-982). The Variable filter is the
    //   correct kind constraint per DESIGN §7.1 ("the ops-table
    //   variable that holds this binding"); accepting other kinds
    //   (e.g. a Type sharing the name) would conflate them with the
    //   storage location.
    //
    // Both caches ALSO filter every candidate to nodes whose owning
    // file's language is `Language::C`. This enforces the C-scoped
    // contract from SPEC §3.1.2 line 163 ("Every C
    // `NodeKind::Function`...") and the DESIGN §8.2 mandate that the
    // deferred payload carry `(function_qualified_name, file_id)`. The
    // `by_name` workspace-global index (a single `BTreeMap<StringId,
    // Vec<NodeId>>` populated by every live node — see
    // `sqry-core/src/graph/unified/storage/indices.rs:70-75`) would
    // otherwise let a same-named Rust / Python / Go function
    // (`fn cb_alpha`, `def cb_alpha`) be marked address-taken or be
    // inserted into `bindings_by_field` / `pending_callsites` under
    // C-only semantics. The candidate-language filter (not the drain
    // entry's origin `file_id`) is the right constraint because
    // cross-TU address-takes are legal: `cb_alpha` may be defined in
    // `a.c` and have its address taken in `b.c`. Both candidate
    // definitions must be eligible so long as they originate from C
    // source.
    let callable_names_iter = pending
        .address_taken_names
        .iter()
        .map(|e| e.function_qualified_name.as_str())
        .chain(
            pending
                .bindings
                .iter()
                .map(|(_, b)| b.target_fn_name.as_str()),
        )
        .chain(
            pending
                .indirect_callsites
                .iter()
                .map(|(_, cs)| cs.caller_qualified_name.as_str()),
        );
    let mut name_to_node_ids: std::collections::HashMap<
        String,
        Vec<crate::graph::unified::node::NodeId>,
    > = std::collections::HashMap::new();
    for name in callable_names_iter {
        if name_to_node_ids.contains_key(name) {
            continue;
        }
        // `intern` returns `Result<StringId, InternError>`. On capacity
        // exhaustion (which would also have been visible at parse time),
        // we record an empty result for this name — propagating the
        // error would force a full build failure for a side-table
        // application that is informational rather than load-bearing for
        // graph correctness.
        let str_id = match graph.strings_mut().intern(name) {
            Ok(id) => id,
            Err(e) => {
                log::warn!(
                    "Phase 4c-prime-post: failed to intern C indirect name {name:?}: {e:?} — \
                     skipping every entry referencing this name"
                );
                name_to_node_ids.insert(name.to_owned(), Vec::new());
                continue;
            }
        };
        // Snapshot the resolved NodeIds into an owned Vec so the
        // immutable index borrow doesn't outlive this iteration —
        // subsequent loop bodies need mutable access to other
        // parts of the graph (metadata store, c_indirect_tables).
        let by_qn = graph.indices().by_qualified_name(str_id).to_vec();
        let by_nm = graph.indices().by_name(str_id).to_vec();

        // Filter by CALL_COMPATIBLE_KINDS, by C-language origin, and
        // deduplicate. A node whose qualified_name == name appears in
        // both buckets — dedupe via a HashSet on (index, generation).
        // We don't use NodeId directly as the HashSet key because Hash
        // isn't derived on NodeId in every code path historically;
        // (index, generation) is the canonical handle pair.
        //
        // The C-language filter consults `graph.files().language_for_file(...)`
        // for each candidate's owning file (see
        // `sqry-core/src/graph/unified/storage/registry.rs:598`). Per
        // SPEC §3.1.2 line 163 ("Every C `NodeKind::Function`...") and
        // DESIGN §8.2, address-taken marks, binding-target lookups, and
        // indirect-callsite caller lookups all operate over the C
        // symbol set only — Rust/Go/Python namesakes must NOT be
        // affected by U11 application.
        let mut seen: std::collections::HashSet<(u32, u64)> = std::collections::HashSet::new();
        let mut matches: Vec<crate::graph::unified::node::NodeId> = Vec::new();
        let arena = graph.nodes();
        let files = graph.files();
        for nid in by_qn.into_iter().chain(by_nm.into_iter()) {
            if !seen.insert((nid.index(), nid.generation())) {
                continue;
            }
            let Some(entry) = arena.get(nid) else {
                continue;
            };
            if !CALL_COMPATIBLE_KINDS.contains(&entry.kind) {
                continue;
            }
            // C-language scope guard. `language_for_file` returns
            // `None` for invalid `FileId`s or files whose language was
            // never set; both cases are conservatively rejected to
            // keep the U11 surface C-only.
            if files.language_for_file(entry.file) != Some(crate::graph::Language::C) {
                continue;
            }
            matches.push(nid);
        }

        if matches.is_empty() {
            stats.unresolved_names += 1;
        }
        name_to_node_ids.insert(name.to_owned(), matches);
    }

    // Second cache: bindings' `instance_name` resolves to a Variable
    // NodeId. Same intern → by_qualified_name ∪ by_name → dedupe
    // pipeline, different kind filter (Variable only). Built lazily —
    // skipped entirely when `pending.bindings` is empty (the common
    // non-C / non-vtable case).
    let mut instance_to_node_ids: std::collections::HashMap<
        String,
        Vec<crate::graph::unified::node::NodeId>,
    > = std::collections::HashMap::new();
    if !pending.bindings.is_empty() {
        for (_file_id, binding) in &pending.bindings {
            let name = binding.instance_name.as_str();
            if instance_to_node_ids.contains_key(name) {
                continue;
            }
            let str_id = match graph.strings_mut().intern(name) {
                Ok(id) => id,
                Err(e) => {
                    log::warn!(
                        "Phase 4c-prime-post: failed to intern C indirect instance name \
                         {name:?}: {e:?} — skipping every binding referencing this instance"
                    );
                    instance_to_node_ids.insert(name.to_owned(), Vec::new());
                    continue;
                }
            };
            let by_qn = graph.indices().by_qualified_name(str_id).to_vec();
            let by_nm = graph.indices().by_name(str_id).to_vec();
            let mut seen: std::collections::HashSet<(u32, u64)> = std::collections::HashSet::new();
            let mut matches: Vec<crate::graph::unified::node::NodeId> = Vec::new();
            let arena = graph.nodes();
            let files = graph.files();
            for nid in by_qn.into_iter().chain(by_nm.into_iter()) {
                if !seen.insert((nid.index(), nid.generation())) {
                    continue;
                }
                let Some(entry) = arena.get(nid) else {
                    continue;
                };
                // Variable is the storage kind the C plugin assigns to
                // ops-table instances (`helper.add_variable`,
                // graph_builder.rs:786 / 2069).
                if !matches!(entry.kind, crate::graph::unified::node::NodeKind::Variable) {
                    continue;
                }
                // C-language scope guard, same rationale as the
                // callable cache above — same-named non-C variables
                // (e.g. a Rust `static the_ops`) must NOT be inserted
                // into `bindings_by_field` as a C ops-table instance.
                if files.language_for_file(entry.file) != Some(crate::graph::Language::C) {
                    continue;
                }
                matches.push(nid);
            }
            if matches.is_empty() {
                stats.unresolved_names += 1;
            }
            instance_to_node_ids.insert(name.to_owned(), matches);
        }
    }

    // Apply address-taken marks (SPEC §3.1.2: mark every match).
    //
    // The cache `name_to_node_ids` already enforces the C-language
    // scope at lookup time — entries here are guaranteed to be C-only
    // canonical NodeIds. The drain entry's origin `file_id` is not
    // consulted directly during mark application because cross-TU
    // address-takes are legal (a `cb_alpha` defined in `a.c` may have
    // its address taken in `b.c`); the candidate's own owning-file
    // language is the correct filter.
    for entry in &pending.address_taken_names {
        if let Some(node_ids) = name_to_node_ids.get(&entry.function_qualified_name) {
            for &nid in node_ids {
                graph.macro_metadata_mut().mark_address_taken(nid);
                stats.address_taken_marks_applied += 1;
            }
        }
    }

    // Intern struct-field-signature triples and populate
    // `struct_field_fnptr`. Each leg interns through the same canonical
    // graph interner so downstream consumers can compare via single
    // `StringId::eq` (DESIGN §3.1 rationale: "amortises across functions
    // sharing signatures").
    if !pending.struct_field_signatures.is_empty() {
        // Intern legs first to avoid mutably-then-immutably borrowing the
        // graph in the same expression. Each `intern` mutably borrows the
        // interner, but we need a fresh borrow per call — collect the
        // interned triples into a Vec then insert into the side-table.
        let mut interned: Vec<(
            (
                crate::graph::unified::string::StringId,
                crate::graph::unified::string::StringId,
            ),
            crate::graph::unified::string::StringId,
        )> = Vec::with_capacity(pending.struct_field_signatures.len());
        let mut intern_failures = 0usize;
        for (struct_tag, field_name, signature) in &pending.struct_field_signatures {
            let strings = graph.strings_mut();
            let st = strings.intern(struct_tag);
            let fn_ = strings.intern(field_name);
            let sig = strings.intern(signature);
            match (st, fn_, sig) {
                (Ok(st), Ok(fn_), Ok(sig)) => {
                    interned.push(((st, fn_), sig));
                }
                _ => {
                    intern_failures += 1;
                }
            }
        }
        if intern_failures > 0 {
            log::warn!(
                "Phase 4c-prime-post: {intern_failures} struct-field-signature triple(s) \
                 failed to intern (capacity exhaustion?) — dropped"
            );
        }
        let tables = graph
            .c_indirect_tables_mut()
            .get_or_insert_with(CIndirectSideTables::new);
        for (key, value) in interned {
            // Last-write-wins on duplicate `(struct_tag, field_name)` —
            // C cannot legally redeclare the same struct field with a
            // different function-pointer type in the same translation
            // unit, so this only fires across files that include
            // conflicting headers (a real bug the user should fix; the
            // resolver tolerates the resulting signature mismatch by
            // surfacing it as a binding-plane-miss).
            tables.struct_field_fnptr.insert(key, value);
            stats.struct_field_signatures_inserted += 1;
        }
    }

    // Resolve bindings: both legs (`instance_name`, `target_fn_name`) must
    // resolve to a canonical NodeId. On either-side miss, the binding is
    // dropped — without both NodeIds the entry would be uninterpretable
    // by U12's resolver.
    //
    // For ambiguity on either leg (multiple canonical winners with the
    // same QN), per DESIGN §7.1 we emit ONE BindingEntry per
    // (instance_node, target_fn) pair — the Cartesian product. This
    // preserves SPEC §3.1.2 semantics ("is this function ever
    // address-taken?" — every plausible binding contributes).
    if !pending.bindings.is_empty() {
        let mut interned_bindings: Vec<(
            (
                crate::graph::unified::string::StringId,
                crate::graph::unified::string::StringId,
            ),
            BindingEntry,
        )> = Vec::with_capacity(pending.bindings.len());
        let mut intern_failures = 0usize;

        for (_file_id, binding) in &pending.bindings {
            // Intern struct_tag + field_name through the graph interner.
            // The origin `_file_id` is discarded here — both legs were
            // already resolved through the C-language-scoped caches
            // (`name_to_node_ids`, `instance_to_node_ids`) above, which
            // is the load-bearing constraint per DESIGN §8.2.
            let strings = graph.strings_mut();
            let st_id = match strings.intern(&binding.struct_tag) {
                Ok(id) => id,
                Err(_) => {
                    intern_failures += 1;
                    continue;
                }
            };
            let fn_id = match strings.intern(&binding.field_name) {
                Ok(id) => id,
                Err(_) => {
                    intern_failures += 1;
                    continue;
                }
            };

            let instances = instance_to_node_ids
                .get(&binding.instance_name)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let targets = name_to_node_ids
                .get(&binding.target_fn_name)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if instances.is_empty() || targets.is_empty() {
                continue;
            }
            stats.bindings_resolved += 1;

            for &instance_node in instances {
                for &target_fn in targets {
                    interned_bindings.push((
                        (st_id, fn_id),
                        BindingEntry {
                            instance_node,
                            target_fn,
                            site_kind: binding.site_kind,
                        },
                    ));
                }
            }
        }
        if intern_failures > 0 {
            log::warn!(
                "Phase 4c-prime-post: {intern_failures} binding key(s) failed to intern \
                 (capacity exhaustion?) — dropped"
            );
        }
        let tables = graph
            .c_indirect_tables_mut()
            .get_or_insert_with(CIndirectSideTables::new);
        for (key, entry) in interned_bindings {
            tables.bindings_by_field.entry(key).or_default().push(entry);
            stats.binding_entries_inserted += 1;
        }
    }

    // Resolve indirect callsites. Caller name MUST resolve — without a
    // caller NodeId, U12's resolver cannot retarget the synthetic Calls
    // edge. Drop callsites whose caller name doesn't resolve.
    //
    // Ambiguity: if the caller name resolves to multiple canonical
    // winners (rare; would imply the C plugin staged a callsite from a
    // function whose qualified name shadowed another), emit one
    // `IndirectCallsite` per matched NodeId so every plausible caller
    // gets its synthetic edge retargeted.
    if !pending.indirect_callsites.is_empty() {
        let mut callsites_to_push: Vec<IndirectCallsite> =
            Vec::with_capacity(pending.indirect_callsites.len());
        for (file_id, cs) in &pending.indirect_callsites {
            let Some(callers) = name_to_node_ids.get(&cs.caller_qualified_name) else {
                continue;
            };
            for &caller in callers {
                callsites_to_push.push(IndirectCallsite {
                    caller,
                    file_id: *file_id,
                    use_span: cs.use_span,
                    shape: cs.shape.clone(),
                    argument_count: cs.argument_count,
                    is_async: cs.is_async,
                });
            }
        }
        if !callsites_to_push.is_empty() {
            let tables = graph
                .c_indirect_tables_mut()
                .get_or_insert_with(CIndirectSideTables::new);
            for cs in callsites_to_push {
                tables.pending_callsites.push(cs);
                stats.indirect_callsites_inserted += 1;
            }
        }
    }

    stats
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
    build_and_persist_graph_with_progress(
        root,
        plugins,
        config,
        build_command,
        inferred_plugin_selection_manifest(plugins),
        no_op_reporter(),
    )
}

/// Infer a persisted plugin-selection manifest from the active plugin manager.
///
/// This is used by durable graph persistence callers that do not have CLI
/// plugin-selection arguments available but still need the manifest to record
/// which built-in plugins participated in the build.
#[must_use]
pub fn inferred_plugin_selection_manifest(
    plugins: &PluginManager,
) -> Option<crate::graph::unified::persistence::PluginSelectionManifest> {
    let active_plugin_ids = plugins
        .plugins()
        .iter()
        .map(|plugin| plugin.metadata().id.to_string())
        .collect::<Vec<_>>();
    if active_plugin_ids.is_empty() {
        return None;
    }

    Some(
        crate::graph::unified::persistence::PluginSelectionManifest {
            active_plugin_ids,
            high_cost_mode: None,
        },
    )
}

/// Inputs for the canonical durable graph persistence transaction.
pub struct DurableGraphPersistenceRequest<'a> {
    /// Root directory whose `.sqry/` artifacts are being committed.
    pub root: &'a Path,
    /// Plugin manager used for language/file-count metadata.
    pub plugins: &'a PluginManager,
    /// Build configuration used for analysis budgets and thread-pool sizing.
    pub config: &'a BuildConfig,
    /// Provenance string written into the manifest.
    pub build_command: &'a str,
    /// Optional plugin-selection metadata written into the manifest.
    pub plugin_selection: Option<crate::graph::unified::persistence::PluginSelectionManifest>,
    /// Progress reporter used for compaction, snapshot, and analysis stages.
    pub progress: SharedReporter,
    /// Effective worker thread count from the build phase.
    pub effective_threads: usize,
}

/// Persist a pre-built graph and run the analysis pipeline.
///
/// This is the persist+analysis portion of
/// [`build_and_persist_graph_with_progress`], extracted so callers can enrich
/// the graph between build and persist.
///
/// # Errors
///
/// Returns an error if persistence or analysis fails.
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
pub fn persist_and_analyze_graph(
    graph: CodeGraph,
    root: &Path,
    plugins: &PluginManager,
    config: &BuildConfig,
    build_command: &str,
    plugin_selection: Option<crate::graph::unified::persistence::PluginSelectionManifest>,
    progress: SharedReporter,
    effective_threads: usize,
) -> Result<(CodeGraph, BuildResult)> {
    persist_durable_graph_transaction(
        graph,
        DurableGraphPersistenceRequest {
            root,
            plugins,
            config,
            build_command,
            plugin_selection,
            progress,
            effective_threads,
        },
    )
}

/// Run the canonical manifest-as-commit-point graph persistence transaction.
///
/// Ordering is part of the durability contract:
///
/// 1. Remove any stale manifest first.
/// 2. Compact edges and write the canonical snapshot.
/// 3. Persist graph analyses for the new identity.
/// 4. Write `manifest.json` last as the commit point.
///
/// # Errors
///
/// Returns an error if any persistence, analysis, or manifest write step fails.
#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
pub fn persist_durable_graph_transaction(
    graph: CodeGraph,
    request: DurableGraphPersistenceRequest<'_>,
) -> Result<(CodeGraph, BuildResult)> {
    use crate::graph::unified::analysis::csr::CsrAdjacency;
    use crate::graph::unified::analysis::{AnalysisIdentity, GraphAnalyses, compute_node_id_hash};
    use crate::graph::unified::compaction::{Direction, build_compacted_csr, snapshot_edges};
    use crate::graph::unified::persistence::manifest::write_manifest_bytes_atomic;
    use crate::graph::unified::persistence::{
        BuildProvenance, GraphStorage, MANIFEST_SCHEMA_VERSION, Manifest, SNAPSHOT_FORMAT_VERSION,
        save_to_path,
    };
    use crate::progress::IndexProgress;
    use chrono::Utc;
    use sha2::{Digest, Sha256};

    let DurableGraphPersistenceRequest {
        root,
        plugins,
        config,
        build_command,
        plugin_selection,
        progress,
        effective_threads,
    } = request;

    // Step 1: Ensure storage directories exist and remove old manifest
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

    // Step 2: Capture raw edge count before compaction changes it
    let raw_edge_count = graph.edge_count();
    let node_count = graph.node_count();

    // Step 3: Compact edge stores into CSR before persistence
    //
    // The build pipeline inserts all edges into the DeltaBuffer (write-optimized).
    // Without compaction, the persisted snapshot stores edges in delta, causing
    // O(N) scans for every edges_from()/edges_to() call on load. Compacting to
    // CSR gives O(degree) lookups — critical for kernel-scale graphs (22M edges).
    progress.report(IndexProgress::StageStarted {
        stage_name: "Compacting edge stores for persistence",
    });
    let compaction_start = std::time::Instant::now();

    // Snapshot both edge stores (sequential — holds read locks briefly)
    let forward_compaction_snapshot = {
        let forward_store = graph.edges().forward();
        snapshot_edges(&forward_store, node_count)
    };
    let reverse_compaction_snapshot = {
        let reverse_store = graph.edges().reverse();
        snapshot_edges(&reverse_store, node_count)
    };

    // Build both CSRs in parallel (CPU-intensive, no locks held)
    let (forward_result, reverse_result) = rayon::join(
        || build_compacted_csr(&forward_compaction_snapshot, Direction::Forward),
        || build_compacted_csr(&reverse_compaction_snapshot, Direction::Reverse),
    );

    let (forward_csr, _forward_build_stats) =
        forward_result.context("Failed to build forward CSR for persistence compaction")?;
    let (reverse_csr, _reverse_build_stats) =
        reverse_result.context("Failed to build reverse CSR for persistence compaction")?;

    // Drop snapshots — no longer needed
    drop(forward_compaction_snapshot);
    drop(reverse_compaction_snapshot);

    // Build analysis adjacency from forward CSR before it's consumed by swap.
    // This replaces the expensive build_from_snapshot merge+sort (~11s on kernel).
    let adjacency = CsrAdjacency::from_csr_graph(&forward_csr);

    // Atomic mutation phase: swap both CSRs and clear both deltas
    graph
        .edges()
        .swap_csrs_and_clear_deltas(forward_csr, reverse_csr);

    progress.report(IndexProgress::StageCompleted {
        stage_name: "Compacting edge stores for persistence",
        stage_duration: compaction_start.elapsed(),
    });

    // Step 4: Save CSR-backed binary snapshot
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

    // Step 5: Compute snapshot checksum
    let snapshot_content =
        fs::read(storage.snapshot_path()).context("Failed to read snapshot for checksum")?;
    let snapshot_sha256 = hex::encode(Sha256::digest(&snapshot_content));

    // Step 6: Build full analyses from the prebuilt adjacency.
    // CsrAdjacency was already derived from the forward CsrGraph in Step 4,
    // eliminating the expensive re-merge from CompactionSnapshot.
    progress.report(IndexProgress::StageStarted {
        stage_name: "Computing graph analyses",
    });
    let analysis_start = std::time::Instant::now();

    let analysis_pool = create_thread_pool(config)
        .context("Failed to create rayon thread pool for graph analysis")?;
    let analyses = analysis_pool
        .install(|| {
            GraphAnalyses::build_all_from_adjacency_with_budget(adjacency, &config.label_budget)
        })
        .context("Failed to build graph analyses")?;

    progress.report(IndexProgress::StageCompleted {
        stage_name: "Computing graph analyses",
        stage_duration: analysis_start.elapsed(),
    });

    let dedup_edge_count = analyses.adjacency.edge_count as usize;

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

    // Step 7: Count workspace files by language using plugin detection
    let mut file_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (file_id, file_path) in graph.indexed_files() {
        if graph.files().is_external(file_id) {
            continue;
        }
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
        plugin_selection: plugin_selection.clone(),
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
    let snapshot = graph.snapshot();
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
        active_plugin_ids: plugin_selection
            .map_or_else(Vec::new, |selection| selection.active_plugin_ids),
        analysis_strategies,
    };

    Ok((graph, build_result))
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
    plugin_selection: Option<crate::graph::unified::persistence::PluginSelectionManifest>,
    progress: SharedReporter,
) -> Result<(CodeGraph, BuildResult)> {
    let (graph, effective_threads) = build_unified_graph_inner(
        root,
        plugins,
        config,
        progress.clone(),
        &CancellationToken::default(),
    )
    .map_err(anyhow::Error::from)?;
    persist_and_analyze_graph(
        graph,
        root,
        plugins,
        config,
        build_command,
        plugin_selection,
        progress,
        effective_threads,
    )
}

/// Get the current HEAD commit SHA from a git repository.
#[must_use]
pub fn get_git_head_commit(path: &Path) -> Option<String> {
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

    let root_for_filter = root.to_path_buf();
    builder.filter_entry(move |entry| {
        entry
            .file_type()
            .is_none_or(|file_type| !file_type.is_dir())
            || should_visit_source_dir(&root_for_filter, entry.path())
    });

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

fn should_visit_source_dir(root: &Path, path: &Path) -> bool {
    if path == root {
        return true;
    }

    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return true;
    };

    !is_default_excluded_source_dir(name)
}

fn is_default_excluded_source_dir(name: &str) -> bool {
    if std::env::var("SQRY_INCLUDE_DEFAULT_EXCLUDED_DIRS")
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
    {
        return false;
    }

    DEFAULT_EXCLUDED_SOURCE_DIRS.contains(&name)
        || DEFAULT_EXCLUDED_SOURCE_DIR_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
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
///
/// `pub(super)` so sibling modules in `crate::graph::unified::build`
/// (specifically [`super::incremental`] from Task 4 Step 4 Phase 3c onward)
/// can construct and consume `ParsedFile` values when driving the
/// parse → commit pipeline against a `RebuildGraph`. The type stays
/// crate-private: external callers still route through the higher-level
/// `build_unified_graph` / `incremental_rebuild` entrypoints.
#[derive(Debug)]
pub(super) struct ParsedFile {
    /// Language identifier for file counting and confidence merging.
    pub(super) language: crate::graph::Language,
    /// Staged graph operations ready for serial commit.
    pub(super) staging: StagingGraph,
}

/// Outcome of [`parse_file`]. `pub(super)` for the same reason as
/// [`ParsedFile`] — shared with [`super::incremental`]'s re-parse closure
/// driver in Phase 3c+. Still crate-private.
///
/// `ParsedFile` is the dominant variant by both size and frequency on
/// every real build — `Skipped` / `TimedOut` are tail paths. Boxing
/// `Parsed` would add an allocation per parsed file (the parse loop is
/// the hottest part of Phase 1), trading the dominant case's perf for
/// uniform variant size. We accept the size difference on the enum
/// rather than pay that cost; the lint is suppressed at the
/// declaration with this rationale.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub(super) enum ParsedFileOutcome {
    Parsed(ParsedFile),
    Skipped,
    TimedOut {
        file: PathBuf,
        phase: &'static str,
        timeout_ms: u64,
    },
}

/// Parse a single file into a `StagingGraph` without touching the shared graph.
///
/// This function is safe to call from multiple threads — it creates its own
/// parser, reads the file, and builds a self-contained staging graph.
///
/// Returns [`ParsedFileOutcome::Skipped`] if the file has no matching plugin or graph builder.
///
/// `pub(super)` as of Task 4 Step 4 Phase 3c so the sibling
/// [`super::incremental`] module can re-parse closure files against the
/// rebuild-local `GraphMutationTarget` plane during `incremental_rebuild`.
pub(super) fn parse_file(path: &Path, plugins: &PluginManager) -> Result<ParsedFileOutcome> {
    let plugin = plugins.plugin_for_path(path);
    let Some(plugin) = plugin else {
        return Ok(ParsedFileOutcome::Skipped);
    };

    let Some(builder) = plugin.graph_builder() else {
        return Ok(ParsedFileOutcome::Skipped);
    };

    let reader =
        FileReader::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let raw_content = reader.as_slice();

    let safe_parser = SafeParser::new(SafeParserConfig::new().with_max_input_size(
        usize::try_from(crate::config::buffers::max_source_file_size()).unwrap_or(usize::MAX),
    ));
    let prepared_content = plugin.preprocess(raw_content);
    let parse_content = prepared_content.as_ref();
    let parse_start = Instant::now();
    let tree = safe_parser
        .parse_file(&plugin.language(), parse_content, path)
        .map_err(|err| map_parse_error(path, err))?;
    let parse_duration = parse_start.elapsed();
    if parse_duration >= Duration::from_secs(2) {
        log::warn!("Slow parse ({parse_duration:.2?}): {}", path.display());
    }

    let mut staging = StagingGraph::new();
    let build_start = Instant::now();
    match builder.build_graph(&tree, parse_content, path, &mut staging) {
        Ok(()) => {}
        Err(GraphBuilderError::BuildTimedOut {
            phase, timeout_ms, ..
        }) => {
            return Ok(ParsedFileOutcome::TimedOut {
                file: path.to_path_buf(),
                phase,
                timeout_ms,
            });
        }
        Err(err) => return Err(map_builder_error(path, &err)),
    }
    let build_duration = build_start.elapsed();
    if build_duration >= Duration::from_secs(2) {
        log::warn!(
            "Slow graph build ({build_duration:.2?}): {}",
            path.display()
        );
    }

    staging.attach_body_hashes(raw_content);

    Ok(ParsedFileOutcome::Parsed(ParsedFile {
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
        ParseError::InputTooLarge { size, max, .. } => anyhow::anyhow!(
            "input too large for {}: {} bytes exceeds {} byte parser limit",
            path.display(),
            size,
            max
        ),
        ParseError::ParseTimedOut { timeout_micros, .. } => anyhow::anyhow!(
            "parse timed out for {} after {} ms",
            path.display(),
            timeout_micros / 1000
        ),
        ParseError::ParseCancelled { reason, .. } => {
            anyhow::anyhow!("parse cancelled for {}: {}", path.display(), reason)
        }
        _ => anyhow::anyhow!("parse error in {}: {:?}", path.display(), err),
    }
}

fn map_builder_error(path: &Path, err: &GraphBuilderError) -> anyhow::Error {
    anyhow::anyhow!("graph builder error in {}: {}", path.display(), err)
}

// ---------------------------------------------------------------------------
// Test-only hooks (Task 7 Phase 7c)
// ---------------------------------------------------------------------------
//
// Thread-local callbacks fired at pass boundaries inside
// `build_unified_graph_inner`. Tests that need to flip the
// `CancellationToken` between chunks / before Phase 4 / before Pass 5
// install a hook, trigger a rebuild, and observe the pipeline
// short-circuit.
//
// Follows the same pattern as [`incremental::testing`] (see
// `incremental.rs:1605`): the module is gated on
// `any(test, feature = "rebuild-internals")` and production builds
// compile every call site into `let _ = ...;` no-ops.
/// Test-only hooks exposed so `sqry-daemon` integration tests can
/// drive cancellation-boundary scenarios in `build_unified_graph_inner`
/// without reaching into private module state.
///
/// Gated on `any(test, feature = "rebuild-internals")`; production
/// builds compile the module out.
#[cfg(any(test, feature = "rebuild-internals"))]
pub mod testing {
    use super::CancellationToken;
    use std::cell::RefCell;

    /// Callback invoked at the top of each chunk iteration in
    /// `build_unified_graph_inner`, receiving the current cancellation
    /// token. Tests typically call `token.cancel()` after N chunks to
    /// assert the pipeline short-circuits at the next boundary.
    pub type AfterChunkHook = Box<dyn FnMut(&CancellationToken)>;
    /// Callback invoked once after the chunk loop exits and before
    /// Phase 4 finalization.
    pub type BeforePhase4Hook = Box<dyn FnMut(&CancellationToken)>;
    /// Callback invoked once before Pass 5 cross-language linking.
    pub type BeforePass5Hook = Box<dyn FnMut(&CancellationToken)>;

    thread_local! {
        static AFTER_CHUNK_HOOK: RefCell<Option<AfterChunkHook>> = const { RefCell::new(None) };
        static BEFORE_PHASE4_HOOK: RefCell<Option<BeforePhase4Hook>> = const { RefCell::new(None) };
        static BEFORE_PASS5_HOOK: RefCell<Option<BeforePass5Hook>> = const { RefCell::new(None) };
    }

    /// Install a callback that runs at the top of each chunk iteration.
    /// Replaces any previously-installed hook on the current thread.
    pub fn set_after_chunk_hook<F>(hook: F) -> Option<AfterChunkHook>
    where
        F: FnMut(&CancellationToken) + 'static,
    {
        AFTER_CHUNK_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove the currently-installed after-chunk hook. Idempotent.
    pub fn clear_after_chunk_hook() {
        AFTER_CHUNK_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Install a callback that runs after the chunk loop exits, before
    /// Phase 4 finalization. Replaces any previously-installed hook.
    pub fn set_before_phase4_hook<F>(hook: F) -> Option<BeforePhase4Hook>
    where
        F: FnMut(&CancellationToken) + 'static,
    {
        BEFORE_PHASE4_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove the currently-installed before-Phase-4 hook. Idempotent.
    pub fn clear_before_phase4_hook() {
        BEFORE_PHASE4_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Install a callback that runs before Pass 5 cross-language linking.
    /// Replaces any previously-installed hook.
    pub fn set_before_pass5_hook<F>(hook: F) -> Option<BeforePass5Hook>
    where
        F: FnMut(&CancellationToken) + 'static,
    {
        BEFORE_PASS5_HOOK.with(|cell| cell.replace(Some(Box::new(hook))))
    }

    /// Remove the currently-installed before-Pass-5 hook. Idempotent.
    pub fn clear_before_pass5_hook() {
        BEFORE_PASS5_HOOK.with(|cell| {
            let _ = cell.replace(None);
        });
    }

    /// Fire the installed after-chunk hook (if any). Called from
    /// `build_unified_graph_inner` at the top of every chunk iteration.
    pub(super) fn fire_after_chunk_hook(cancellation: &CancellationToken) {
        AFTER_CHUNK_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(cancellation);
            }
        });
    }

    /// Fire the installed before-Phase-4 hook (if any).
    pub(super) fn fire_before_phase4_hook(cancellation: &CancellationToken) {
        BEFORE_PHASE4_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(cancellation);
            }
        });
    }

    /// Fire the installed before-Pass-5 hook (if any).
    pub(super) fn fire_before_pass5_hook(cancellation: &CancellationToken) {
        BEFORE_PASS5_HOOK.with(|cell| {
            if let Some(hook) = cell.borrow_mut().as_mut() {
                hook(cancellation);
            }
        });
    }

    /// RAII guard that installs an after-chunk hook on construction
    /// and clears it on drop. Prevents a panic mid-test from leaking
    /// a hook into a sibling test on the same thread.
    pub struct AfterChunkHookGuard {
        _sealed: (),
    }

    impl AfterChunkHookGuard {
        /// Install `hook` as the thread-local after-chunk callback.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(&CancellationToken) + 'static,
        {
            let _previous = set_after_chunk_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for AfterChunkHookGuard {
        fn drop(&mut self) {
            clear_after_chunk_hook();
        }
    }

    /// RAII guard that installs a before-Phase-4 hook on construction
    /// and clears it on drop.
    pub struct BeforePhase4HookGuard {
        _sealed: (),
    }

    impl BeforePhase4HookGuard {
        /// Install `hook` as the thread-local before-Phase-4 callback.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(&CancellationToken) + 'static,
        {
            let _previous = set_before_phase4_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for BeforePhase4HookGuard {
        fn drop(&mut self) {
            clear_before_phase4_hook();
        }
    }

    /// RAII guard that installs a before-Pass-5 hook on construction
    /// and clears it on drop.
    pub struct BeforePass5HookGuard {
        _sealed: (),
    }

    impl BeforePass5HookGuard {
        /// Install `hook` as the thread-local before-Pass-5 callback.
        pub fn install<F>(hook: F) -> Self
        where
            F: FnMut(&CancellationToken) + 'static,
        {
            let _previous = set_before_pass5_hook(hook);
            Self { _sealed: () }
        }
    }

    impl Drop for BeforePass5HookGuard {
        fn drop(&mut self) {
            clear_before_pass5_hook();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Scope;
    use crate::graph::{GraphBuilder, GraphBuilderError, GraphResult, Language};
    use crate::plugin::error::{ParseError, ScopeError};
    use crate::plugin::{LanguageMetadata, LanguagePlugin};
    use serial_test::serial;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;
    use tree_sitter::{Parser, Tree};

    const RUST_TEST_EXTENSIONS: &[&str] = &["rs"];
    const FILENAME_MATCH_EXTENSIONS: &[&str] = &["rmd", "bash_profile"];

    #[test]
    fn index_worker_stack_size_uses_default_without_override() {
        assert_eq!(
            index_worker_stack_size_bytes_from_value(None),
            DEFAULT_INDEX_WORKER_STACK_SIZE_MB * BYTES_PER_MIB
        );
    }

    #[test]
    fn index_worker_stack_size_honors_valid_override() {
        assert_eq!(
            index_worker_stack_size_bytes_from_value(Some("64")),
            64 * BYTES_PER_MIB
        );
    }

    #[test]
    fn index_worker_stack_size_clamps_override_bounds() {
        assert_eq!(
            index_worker_stack_size_bytes_from_value(Some("1")),
            MIN_INDEX_WORKER_STACK_SIZE_MB * BYTES_PER_MIB
        );
        assert_eq!(
            index_worker_stack_size_bytes_from_value(Some("9999")),
            MAX_INDEX_WORKER_STACK_SIZE_MB * BYTES_PER_MIB
        );
    }

    #[test]
    fn index_worker_stack_size_ignores_invalid_override() {
        assert_eq!(
            index_worker_stack_size_bytes_from_value(Some("not-a-number")),
            DEFAULT_INDEX_WORKER_STACK_SIZE_MB * BYTES_PER_MIB
        );
    }

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

    fn expect_parsed_file(outcome: ParsedFileOutcome) -> ParsedFile {
        match outcome {
            ParsedFileOutcome::Parsed(parsed) => parsed,
            ParsedFileOutcome::Skipped => panic!("expected parsed file, got skipped outcome"),
            ParsedFileOutcome::TimedOut { file, phase, .. } => {
                panic!(
                    "expected parsed file, got timeout outcome for {} during {}",
                    file.display(),
                    phase,
                )
            }
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

    struct TimeoutGraphBuilder;

    impl GraphBuilder for TimeoutGraphBuilder {
        fn build_graph(
            &self,
            _tree: &Tree,
            _content: &[u8],
            file: &Path,
            _staging: &mut StagingGraph,
        ) -> GraphResult<()> {
            Err(GraphBuilderError::BuildTimedOut {
                file: file.to_path_buf(),
                phase: "test-timeout",
                timeout_ms: 42,
            })
        }

        fn language(&self) -> Language {
            Language::Rust
        }
    }

    struct SelectiveTimeoutGraphBuilder;

    impl GraphBuilder for SelectiveTimeoutGraphBuilder {
        fn build_graph(
            &self,
            _tree: &Tree,
            _content: &[u8],
            file: &Path,
            staging: &mut StagingGraph,
        ) -> GraphResult<()> {
            use crate::graph::unified::build::helper::GraphBuildHelper;

            let mut helper = GraphBuildHelper::new(staging, file, Language::Rust);
            let file_name = file
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();

            if file_name == "timeout.rs" {
                helper.add_function("timeout_partial", None, false, false);
                return Err(GraphBuilderError::BuildTimedOut {
                    file: file.to_path_buf(),
                    phase: "test-timeout",
                    timeout_ms: 42,
                });
            }

            helper.add_function("survivor_fn", None, false, false);
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
    #[serial]
    fn test_find_source_files_excludes_generated_dependency_roots() {
        let temp_dir = TempDir::new().expect("temp dir");
        let root = temp_dir.path();

        fs::write(root.join("src.rs"), "fn src() {}").expect("write source file");
        for dir in [
            "_work",
            "_actions",
            "_update",
            "externals.2.334.0",
            "node_modules",
            "target",
            "vendor",
        ] {
            let nested = root.join(dir).join("nested");
            fs::create_dir_all(&nested).expect("create excluded dir");
            fs::write(nested.join("ignored.rs"), "fn ignored() {}")
                .expect("write ignored source file");
        }
        for dir in ["external_tools", "vendorized"] {
            let nested = root.join(dir).join("nested");
            fs::create_dir_all(&nested).expect("create included sibling dir");
            fs::write(nested.join("included.rs"), "fn included() {}")
                .expect("write included source file");
        }

        let config = BuildConfig::default();
        let mut relative_files: Vec<_> = find_source_files(root, &config)
            .iter()
            .map(|path| path.strip_prefix(root).expect("strip root").to_path_buf())
            .collect();
        relative_files.sort();

        assert_eq!(
            relative_files,
            vec![
                PathBuf::from("external_tools/nested/included.rs"),
                PathBuf::from("src.rs"),
                PathBuf::from("vendorized/nested/included.rs"),
            ]
        );
    }

    #[test]
    #[serial]
    fn test_find_source_files_can_include_default_excluded_roots() {
        let temp_dir = TempDir::new().expect("temp dir");
        let root = temp_dir.path();
        let nested = root.join("vendor").join("first_party");
        fs::create_dir_all(&nested).expect("create vendor dir");
        fs::write(nested.join("included.rs"), "fn included() {}").expect("write included source");

        unsafe {
            std::env::set_var("SQRY_INCLUDE_DEFAULT_EXCLUDED_DIRS", "1");
        }
        let config = BuildConfig::default();
        let files = find_source_files(root, &config);
        unsafe {
            std::env::remove_var("SQRY_INCLUDE_DEFAULT_EXCLUDED_DIRS");
        }

        let relative_files: Vec<_> = files
            .iter()
            .map(|path| path.strip_prefix(root).expect("strip root").to_path_buf())
            .collect();

        assert_eq!(
            relative_files,
            vec![PathBuf::from("vendor/first_party/included.rs")]
        );
    }

    #[test]
    fn test_build_unified_graph_empty_registry_error() {
        let plugins = PluginManager::new();
        let config = BuildConfig::default();
        let root = std::path::Path::new(".");

        let result = build_unified_graph(root, &plugins, &config);
        let err = result.expect_err("empty registry must error");
        // Task 7 Phase 7c: the internal pipeline now returns
        // `GraphBuilderError::Internal { reason }` instead of a bare
        // `anyhow::bail!`. The legacy `build_unified_graph` wrapper
        // lifts through `anyhow::Error::from`, which prefixes the
        // reason with the `GraphBuilderError::Internal` `Display`
        // string (`Internal graph builder error: ...`).
        assert_eq!(
            err.to_string(),
            "Internal graph builder error: No graph builders registered – cannot build code graph"
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
        let err = result.expect_err("no graph builders must error");
        assert_eq!(
            err.to_string(),
            "Internal graph builder error: No graph builders registered – cannot build code graph"
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
        let err = result.expect_err("all-failures must error");
        assert_eq!(
            err.to_string(),
            "Internal graph builder error: All graph builds failed"
        );
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

        let parsed = expect_parsed_file(parse_file(&file_path, &plugins).expect("parse file"));
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

        let parsed = expect_parsed_file(parse_file(&file_path, &plugins).expect("parse file"));
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

        let parsed = expect_parsed_file(parse_file(&file_path, &plugins).expect("parse file"));
        commit_parsed_file_for_test(&file_path, parsed, &mut graph);
    }

    #[test]
    fn test_parse_file_returns_timed_out_outcome() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("timeout.rs");
        fs::write(&file_path, "fn main() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-timeout",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(TimeoutGraphBuilder)),
        )));

        let outcome = parse_file(&file_path, &plugins).expect("parse file");
        match outcome {
            ParsedFileOutcome::TimedOut {
                file,
                phase,
                timeout_ms,
            } => {
                assert_eq!(file, file_path);
                assert_eq!(phase, "test-timeout");
                assert_eq!(timeout_ms, 42);
            }
            other => panic!("expected timed out outcome, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_file_rejects_oversized_input() {
        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("oversized.rs");
        fs::write(&file_path, vec![b'a'; 1_048_577]).expect("write oversized file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-oversized",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(NoopGraphBuilder)),
        )));

        unsafe {
            std::env::set_var("SQRY_MAX_SOURCE_FILE_SIZE", "1048576");
        }
        let err = parse_file(&file_path, &plugins).expect_err("oversized file should fail");
        unsafe {
            std::env::remove_var("SQRY_MAX_SOURCE_FILE_SIZE");
        }

        let err_text = err.to_string();
        assert!(err_text.contains("oversized.rs"));
    }

    #[test]
    fn test_build_unified_graph_skips_timed_out_file_without_partial_commit() {
        let temp_dir = TempDir::new().expect("temp dir");
        let ok_path = temp_dir.path().join("ok.rs");
        let timeout_path = temp_dir.path().join("timeout.rs");
        fs::write(&ok_path, "fn ok() {}").expect("write ok file");
        fs::write(&timeout_path, "fn timeout() {}").expect("write timeout file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-selective-timeout",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(SelectiveTimeoutGraphBuilder)),
        )));
        let config = BuildConfig::default();

        let graph = build_unified_graph(temp_dir.path(), &plugins, &config)
            .expect("graph build should succeed with surviving files");
        let snapshot = graph.snapshot();

        assert_eq!(snapshot.find_by_pattern("survivor_fn").len(), 1);
        assert!(
            snapshot.find_by_pattern("timeout_partial").is_empty(),
            "timed out file staging must not be committed"
        );
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

    /// Wrapper-based builds infer plugin-selection provenance from the active
    /// plugin manager so non-CLI callers do not silently persist legacy-looking
    /// manifests.
    #[test]
    fn test_wrapper_infers_plugin_selection_from_manager() {
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

        let (_graph, build_result) =
            build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:wrapper_plugins")
                .expect("wrapper build should succeed");

        assert_eq!(
            build_result.active_plugin_ids,
            vec!["rust-simple".to_string()],
            "build result should expose the inferred active plugin ids"
        );

        let storage = GraphStorage::new(temp_dir.path());
        let manifest = storage.load_manifest().expect("manifest should load");
        let plugin_selection = manifest
            .plugin_selection
            .expect("wrapper should persist plugin selection metadata");
        assert_eq!(
            plugin_selection.active_plugin_ids,
            vec!["rust-simple".to_string()],
            "wrapper should persist the manager-derived plugin ids"
        );
        assert_eq!(
            plugin_selection.high_cost_mode, None,
            "wrapper-inferred plugin selection should keep high_cost_mode diagnostic-only"
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

    // ===== CSR Compaction Persistence Regression Tests =====

    /// Graph builder that creates duplicate edges to exercise `raw_edge_count` > `edge_count`.
    struct DuplicateCallsGraphBuilder;

    impl GraphBuilder for DuplicateCallsGraphBuilder {
        fn build_graph(
            &self,
            _tree: &Tree,
            _content: &[u8],
            file: &Path,
            staging: &mut StagingGraph,
        ) -> GraphResult<()> {
            use crate::graph::unified::build::helper::GraphBuildHelper;

            let mut helper = GraphBuildHelper::new(staging, file, Language::Rust);
            let fn1 = helper.add_function("main", None, false, false);
            let fn2 = helper.add_function("helper", None, false, false);

            // Add the same Calls edge twice to create a duplicate
            helper.add_call_edge(fn1, fn2);
            helper.add_call_edge(fn1, fn2);

            Ok(())
        }

        fn language(&self) -> Language {
            Language::Rust
        }
    }

    /// Persisted snapshot has CSR on both stores and empty deltas.
    #[test]
    fn test_persisted_snapshot_compacts_both_edge_stores_before_save() {
        use crate::graph::unified::persistence::{GraphStorage, load_from_path};

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

        let _result =
            build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:csr_compact")
                .expect("build should succeed");

        // Load the persisted snapshot and verify CSR state
        let storage = GraphStorage::new(temp_dir.path());
        let loaded = load_from_path(storage.snapshot_path(), None).expect("load should succeed");

        assert!(
            loaded.edges().forward().csr().is_some(),
            "Forward store must have CSR after persistence"
        );
        assert!(
            loaded.edges().reverse().csr().is_some(),
            "Reverse store must have CSR after persistence"
        );

        let stats = loaded.edges().stats();
        assert_eq!(
            stats.forward.delta_edge_count, 0,
            "Forward delta must be empty after persistence"
        );
        assert_eq!(
            stats.reverse.delta_edge_count, 0,
            "Reverse delta must be empty after persistence"
        );
    }

    /// Loaded snapshot supports reverse traversal (direct-callers / `edges_to`).
    #[test]
    fn test_loaded_snapshot_edges_to_works_after_round_trip() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::persistence::{GraphStorage, load_from_path};
        use crate::graph::unified::{
            FileScope, ResolutionMode, SymbolCandidateOutcome, SymbolQuery,
        };

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

        build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:round_trip")
            .expect("build should succeed");

        let storage = GraphStorage::new(temp_dir.path());
        let loaded = load_from_path(storage.snapshot_path(), None).expect("load should succeed");

        // Find main and helper node IDs through symbol resolution
        let snapshot = loaded.snapshot();

        let main_id = match snapshot.find_symbol_candidates(&SymbolQuery {
            symbol: "main",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        }) {
            SymbolCandidateOutcome::Candidates(ids) => ids[0],
            _ => panic!("main node must exist"),
        };

        let helper_id = match snapshot.find_symbol_candidates(&SymbolQuery {
            symbol: "helper",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        }) {
            SymbolCandidateOutcome::Candidates(ids) => ids[0],
            _ => panic!("helper node must exist"),
        };

        // Forward: main -> helper
        let forward_edges = loaded.edges().edges_from(main_id);
        let has_call = forward_edges
            .iter()
            .any(|e| e.target == helper_id && matches!(e.kind, EdgeKind::Calls { .. }));
        assert!(has_call, "Forward traversal: main should call helper");

        // Reverse: helper <- main (the critical regression check)
        let reverse_edges = loaded.edges().edges_to(helper_id);
        let has_caller = reverse_edges
            .iter()
            .any(|e| e.source == main_id && matches!(e.kind, EdgeKind::Calls { .. }));
        assert!(
            has_caller,
            "Reverse traversal: helper should have main as caller"
        );
    }

    /// `raw_edge_count` >= `edge_count` still holds after pre-save compaction.
    #[test]
    fn test_raw_edge_count_preserved_across_pre_save_compaction() {
        use crate::graph::unified::persistence::GraphStorage;

        let temp_dir = TempDir::new().expect("temp dir");
        let file_path = temp_dir.path().join("test.rs");
        fs::write(&file_path, "fn main() {} fn helper() {}").expect("write test file");

        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-dup",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(DuplicateCallsGraphBuilder)),
        )));
        let config = BuildConfig::default();

        let (_graph, build_result) =
            build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:raw_edge_count")
                .expect("build should succeed");

        assert!(
            build_result.raw_edge_count > build_result.edge_count,
            "raw_edge_count ({}) must be > edge_count ({}) for duplicate builder",
            build_result.raw_edge_count,
            build_result.edge_count
        );

        // Verify manifest matches
        let storage = GraphStorage::new(temp_dir.path());
        let manifest = storage.load_manifest().expect("manifest should load");

        assert_eq!(
            manifest.raw_edge_count,
            Some(build_result.raw_edge_count),
            "Manifest raw_edge_count must match build result"
        );
        assert_eq!(
            manifest.edge_count, build_result.edge_count,
            "Manifest edge_count must match build result"
        );
    }

    /// Full round-trip: build -> save -> load -> query produces correct results.
    #[test]
    fn test_build_save_load_query_round_trip_preserves_edge_queries() {
        use crate::graph::unified::edge::EdgeKind;
        use crate::graph::unified::persistence::{GraphStorage, load_from_path};
        use crate::graph::unified::{
            FileScope, ResolutionMode, SymbolCandidateOutcome, SymbolQuery,
        };

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

        let (_original_graph, build_result) =
            build_and_persist_graph(temp_dir.path(), &plugins, &config, "test:full_round_trip")
                .expect("build should succeed");

        // Load from disk
        let storage = GraphStorage::new(temp_dir.path());
        let loaded = load_from_path(storage.snapshot_path(), None).expect("load should succeed");

        // Edge count on loaded graph should match dedup count
        assert_eq!(
            loaded.edge_count(),
            build_result.edge_count,
            "Loaded graph edge count must match build result dedup count"
        );

        // Node count should match
        assert_eq!(
            loaded.node_count(),
            build_result.node_count,
            "Loaded graph node count must match build result"
        );

        // Verify edge queries work on loaded graph
        let snapshot = loaded.snapshot();

        let main_id = match snapshot.find_symbol_candidates(&SymbolQuery {
            symbol: "main",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        }) {
            SymbolCandidateOutcome::Candidates(ids) => {
                assert!(!ids.is_empty(), "main must exist");
                ids[0]
            }
            _ => panic!("main node must exist"),
        };

        let helper_id = match snapshot.find_symbol_candidates(&SymbolQuery {
            symbol: "helper",
            file_scope: FileScope::Any,
            mode: ResolutionMode::AllowSuffixCandidates,
        }) {
            SymbolCandidateOutcome::Candidates(ids) => {
                assert!(!ids.is_empty(), "helper must exist");
                ids[0]
            }
            _ => panic!("helper node must exist"),
        };

        // Forward query: main calls helper
        let fwd = loaded.edges().edges_from(main_id);
        let has_fwd_call = fwd
            .iter()
            .any(|e| e.target == helper_id && matches!(e.kind, EdgeKind::Calls { .. }));
        assert!(has_fwd_call, "edges_from(main) must include call to helper");

        // Reverse query: helper called by main
        let rev = loaded.edges().edges_to(helper_id);
        let has_rev_call = rev
            .iter()
            .any(|e| e.source == main_id && matches!(e.kind, EdgeKind::Calls { .. }));
        assert!(has_rev_call, "edges_to(helper) must include caller main");
    }

    // -----------------------------------------------------------------
    // Phase 7c cancellation wire-through tests (task 7 phase 7c)
    // -----------------------------------------------------------------
    //
    // The four cancellation-boundary tests below exercise the pipeline
    // at distinct points in `build_unified_graph_inner`:
    //
    //   1. preflight — token cancelled before the first boundary; no
    //      FS walk, no parse, no Phase 4 work.
    //   2. mid-chunk — token flipped after the first chunk commits via
    //      the AfterChunkHookGuard; second chunk never parses.
    //   3. pre-Phase-4 — token flipped after the chunk loop exits via
    //      the BeforePhase4HookGuard; Phase 4a+ never runs.
    //   4. pre-Pass-5 — token flipped before cross-language linking
    //      via the BeforePass5HookGuard; Pass 5 never runs.
    //
    // A fifth test confirms the backwards-compatible default path
    // (no cancellation arg) still returns a fully-built graph.

    fn build_rust_test_fixture(dir: &Path, file_count: usize) {
        for i in 0..file_count {
            let path = dir.join(format!("fixture_{i}.rs"));
            fs::write(&path, format!("pub fn fn_{i}() {{ let _ = {i}; }}")).expect("write fixture");
        }
    }

    fn make_rust_test_plugins() -> PluginManager {
        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(TestPlugin::new(
            "rust-noop-for-cancellation-tests",
            RUST_TEST_EXTENSIONS,
            Some(Box::new(NoopGraphBuilder)),
        )));
        plugins
    }

    #[test]
    fn build_unified_graph_cancellable_preflight_cancellation_returns_cancelled() {
        let tmp = TempDir::new().expect("tmp");
        build_rust_test_fixture(tmp.path(), 4);
        let plugins = make_rust_test_plugins();
        let config = BuildConfig::default();

        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = build_unified_graph_cancellable(tmp.path(), &plugins, &config, &cancel);
        let err = result.expect_err("pre-cancelled token must short-circuit");
        assert!(
            matches!(err, GraphBuilderError::Cancelled),
            "expected Cancelled, got: {err:?}"
        );
    }

    #[test]
    fn build_unified_graph_cancellable_mid_chunk_cancellation_returns_cancelled() {
        let tmp = TempDir::new().expect("tmp");
        // Force multiple chunks by setting a tiny staging_memory_limit.
        build_rust_test_fixture(tmp.path(), 8);
        let plugins = make_rust_test_plugins();
        // A very small memory limit forces ~1 file per chunk.
        let config = BuildConfig {
            staging_memory_limit: 1,
            ..BuildConfig::default()
        };

        let cancel = CancellationToken::new();

        // Install a hook that cancels after the FIRST chunk. The hook
        // fires at the TOP of every chunk iteration (including chunk 0
        // before cancelling). We cancel on the first call; the next
        // iteration's top-of-loop `cancellation.check()` short-circuits.
        let cancel_for_hook = cancel.clone();
        let mut call_count = 0u32;
        let _guard = testing::AfterChunkHookGuard::install(move |tok| {
            call_count += 1;
            if call_count >= 2 {
                cancel_for_hook.cancel();
                // `tok` is the same shared Arc under the hood.
                assert!(tok.is_cancelled());
            }
        });

        let result = build_unified_graph_cancellable(tmp.path(), &plugins, &config, &cancel);
        let err = result.expect_err("mid-chunk cancellation must short-circuit");
        assert!(
            matches!(err, GraphBuilderError::Cancelled),
            "expected Cancelled, got: {err:?}"
        );
    }

    #[test]
    fn build_unified_graph_cancellable_pre_phase4_cancellation_short_circuits() {
        let tmp = TempDir::new().expect("tmp");
        build_rust_test_fixture(tmp.path(), 4);
        let plugins = make_rust_test_plugins();
        let config = BuildConfig::default();

        let cancel = CancellationToken::new();
        let cancel_for_hook = cancel.clone();
        let _guard = testing::BeforePhase4HookGuard::install(move |_tok| {
            cancel_for_hook.cancel();
        });

        let result = build_unified_graph_cancellable(tmp.path(), &plugins, &config, &cancel);
        let err = result.expect_err("pre-Phase-4 cancellation must short-circuit");
        assert!(
            matches!(err, GraphBuilderError::Cancelled),
            "expected Cancelled, got: {err:?}"
        );
    }

    #[test]
    fn build_unified_graph_cancellable_pre_pass5_cancellation_short_circuits() {
        let tmp = TempDir::new().expect("tmp");
        build_rust_test_fixture(tmp.path(), 4);
        let plugins = make_rust_test_plugins();
        let config = BuildConfig::default();

        let cancel = CancellationToken::new();
        let cancel_for_hook = cancel.clone();
        let _guard = testing::BeforePass5HookGuard::install(move |_tok| {
            cancel_for_hook.cancel();
        });

        let result = build_unified_graph_cancellable(tmp.path(), &plugins, &config, &cancel);
        let err = result.expect_err("pre-Pass-5 cancellation must short-circuit");
        assert!(
            matches!(err, GraphBuilderError::Cancelled),
            "expected Cancelled, got: {err:?}"
        );
    }

    #[test]
    fn build_unified_graph_default_path_is_backwards_compatible() {
        let tmp = TempDir::new().expect("tmp");
        build_rust_test_fixture(tmp.path(), 3);
        let plugins = make_rust_test_plugins();
        let config = BuildConfig::default();

        // Legacy API: no cancellation parameter. Must return a
        // built graph without triggering cancellation short-circuits.
        // (The test plugin is a NoopGraphBuilder that produces zero
        // nodes; we only assert the success path returns Ok.)
        let _graph = build_unified_graph(tmp.path(), &plugins, &config)
            .expect("legacy path must still build successfully");
    }
}
