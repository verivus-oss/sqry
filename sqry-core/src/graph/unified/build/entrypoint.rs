//! Build entrypoint for unified graph.
//!
//! This module provides the top-level API for building a unified graph from source files.
//! It orchestrates file discovery and delegates to the 5-pass build pipeline.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;

use crate::graph::GraphBuilderError;
use crate::graph::unified::build::StagingGraph;
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
}

/// Configuration for building the unified graph.
#[derive(Debug, Clone, Default)]
pub struct BuildConfig {
    /// Maximum directory depth to traverse (None = unlimited).
    pub max_depth: Option<usize>,

    /// Follow symbolic links.
    pub follow_links: bool,

    /// Include hidden files and directories.
    pub include_hidden: bool,

    /// Number of threads for parallel building (None = use default based on CPU count).
    pub num_threads: Option<usize>,
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
    build_unified_graph_with_progress(root, plugins, config, no_op_reporter())
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

    // 3. Start file processing phase and process each file
    let total_files = files.len();
    tracker.start_phase(1, GRAPH_FILE_PROCESSING_PHASE, total_files);

    let mut attempted = 0usize;
    let mut succeeded = 0usize;
    for path in files {
        match process_file(path.as_path(), plugins, &mut graph) {
            Ok(ProcessOutcome::Skipped) => {
                // Skipped files still count toward progress
                tracker.increment_progress();
            }
            Ok(ProcessOutcome::Built) => {
                attempted += 1;
                succeeded += 1;
                tracker.increment_progress();
            }
            Err(e) => {
                attempted += 1;
                log::warn!("Failed to process {}: {}", path.display(), e);
                tracker.increment_progress();
            }
        }
    }

    // Complete file processing phase
    tracker.complete_phase();

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
    let pass5_stats = super::pass5_cross_language::link_cross_language_edges(&mut graph);
    if pass5_stats.total_edges_created > 0 {
        log::info!(
            "Pass 5: {} cross-language edges created ({} FFI, {} HTTP)",
            pass5_stats.total_edges_created,
            pass5_stats.ffi_edges_created,
            pass5_stats.http_endpoints_matched,
        );
    }

    log::info!("Built unified graph with {} nodes", graph.node_count());
    Ok(graph)
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
/// 3. Analysis pipeline (CSR + SCC + Condensation DAG) — strict, fails on error
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
    let graph = build_unified_graph_with_progress(root, plugins, config, progress.clone())?;

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

    // Step 5: Build analysis artifacts (to get dedup edge count)
    let snapshot = graph.snapshot();
    let edges = snapshot.edges();
    let forward_store = edges.forward();
    let node_count = snapshot.nodes().len();
    let compaction_snapshot = snapshot_edges(&forward_store, node_count);
    drop(forward_store); // Release lock before heavy computation

    let analyses = GraphAnalyses::build_all(&compaction_snapshot)
        .context("Failed to build analysis artifacts")?;

    let dedup_edge_count = analyses.adjacency.edge_count as usize;
    let raw_edge_count = graph.edge_count();

    // Step 6: Count files by language using plugin detection
    let mut file_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (_file_id, file_path) in graph.indexed_files() {
        let language = plugins
            .plugin_for_path(file_path)
            .map_or_else(|| "unknown".to_string(), |p| p.metadata().id.to_string());
        *file_counts.entry(language).or_insert(0) += 1;
    }
    let total_files: usize = file_counts.values().sum();

    // Step 7: Construct Manifest in memory (with dedup edge count from analysis)
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

    // Step 8: Serialize manifest to bytes and compute hash
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).context("Failed to serialize manifest")?;

    let manifest_hash = {
        let mut hasher = Sha256::new();
        hasher.update(&manifest_bytes);
        hex::encode(hasher.finalize())
    };

    // Step 9: Construct AnalysisIdentity and persist analysis artifacts
    let node_id_hash = compute_node_id_hash(&snapshot);
    let identity = AnalysisIdentity::new(manifest_hash, node_id_hash);

    analyses
        .persist_all(&storage, &identity)
        .context("Failed to persist analysis artifacts")?;

    log::info!(
        "Analysis artifacts persisted to {}",
        storage.analysis_dir().display()
    );

    // Step 10: Write manifest bytes to disk LAST (commit point)
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

/// Process a single file and add it to the unified graph.
enum ProcessOutcome {
    Skipped,
    Built,
}

fn process_file(
    path: &Path,
    plugins: &PluginManager,
    graph: &mut CodeGraph,
) -> Result<ProcessOutcome> {
    // 1. Identify language by extension or special filename routing.
    let plugin = plugins.plugin_for_path(path);
    let Some(plugin) = plugin else {
        return Ok(ProcessOutcome::Skipped); // Skip unsupported extensions/filenames
    };

    let Some(builder) = plugin.graph_builder() else {
        return Ok(ProcessOutcome::Skipped); // Skip plugins without graph builder
    };

    // 2. Parse and build graph
    let content = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;

    let tree = plugin
        .parse_ast(&content)
        .map_err(|err| map_parse_error(path, err))?;

    // Build into staging graph
    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, &content, path, &mut staging)
        .map_err(|err| map_builder_error(path, &err))?;

    // 2b. Attach body hashes to staged nodes
    // This computes 128-bit hashes for hashable symbol kinds (functions, methods, etc.)
    // enabling duplicate code detection.
    staging.attach_body_hashes(&content);

    // 3. Commit to unified graph

    // Register file and get its ID
    let file_id = graph
        .files_mut()
        .register_with_language(path, Some(builder.language()))
        .map_err(|e| anyhow::anyhow!("Failed to register file: {e}"))?;

    // Staging graphs are built per-file, so normalize any placeholder FileIds
    // in staged operations to the committed graph's FileId.
    staging.apply_file_id(file_id);

    // Commit flow: commit_strings -> apply_string_remap -> commit_nodes -> commit edges
    // Step 1: Commit strings and get the local->global StringId remap table
    let string_remap = staging
        .commit_strings(graph.strings_mut())
        .map_err(|e| anyhow::anyhow!("Failed to commit strings: {e}"))?;

    // Step 2: Apply string remap to all staged nodes and edges
    staging
        .apply_string_remap(&string_remap)
        .map_err(|e| anyhow::anyhow!("Failed to apply string remap: {e}"))?;

    // Step 3: Commit staged nodes to the arena
    let node_id_mapping = staging
        .commit_nodes(graph.nodes_mut())
        .map_err(|e| anyhow::anyhow!("Failed to commit nodes: {e}"))?;

    // Step 3b: Update indices with committed nodes
    // This is critical for query operations that rely on indices for efficient lookups
    // Collect node data first to avoid borrow conflicts
    let index_entries: Vec<_> = node_id_mapping
        .values()
        .filter_map(|&actual_id| {
            graph.nodes().get(actual_id).map(|entry| {
                (
                    actual_id,
                    entry.kind,
                    entry.name,
                    entry.qualified_name,
                    entry.file,
                )
            })
        })
        .collect();
    let mut duplicate_count = 0;
    for (node_id, kind, name, qualified_name, file) in index_entries {
        let added = graph
            .indices_mut()
            .add(node_id, kind, name, qualified_name, file);
        if !added {
            duplicate_count += 1;
            log::debug!(
                "Index duplicate detected: {node_id:?} kind={kind:?} already in indices (file={file:?})"
            );
        }
    }
    if duplicate_count > 0 {
        log::warn!(
            "Detected {} duplicate node(s) during index population for {}",
            duplicate_count,
            path.display()
        );
    }

    // Step 4: Get remapped edges and add to graph
    // Use the actual file_id from registration, not the staging file_id
    let edges = staging.get_remapped_edges(&node_id_mapping);
    for edge in edges {
        graph.edges_mut().add_edge_with_spans(
            edge.source,
            edge.target,
            edge.kind.clone(),
            file_id,
            edge.spans.clone(),
        );
    }

    // Step 5: Extract and merge confidence metadata from staging
    // This is set by language plugins (e.g., Rust) during graph building
    if let Some(confidence) = staging.take_confidence() {
        let language_name = builder.language().to_string();
        graph.merge_confidence(&language_name, confidence);
    }

    Ok(ProcessOutcome::Built)
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
    fn test_process_file_matches_uppercase_extension() {
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

        let outcome = process_file(&file_path, &plugins, &mut graph).expect("process file");
        assert!(matches!(outcome, ProcessOutcome::Built));
    }

    #[test]
    fn test_process_file_matches_dotless_filename() {
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

        let outcome = process_file(&file_path, &plugins, &mut graph).expect("process file");
        assert!(matches!(outcome, ProcessOutcome::Built));
    }

    #[test]
    fn test_process_file_matches_pulumi_stack_filename() {
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

        let outcome = process_file(&file_path, &plugins, &mut graph).expect("process file");
        assert!(matches!(outcome, ProcessOutcome::Built));
    }

    // ========================================================================
    // Build pipeline consolidation regression tests (Step 10)
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

    /// Regression test (Step 10, #1): `build_and_persist_graph` returns populated `BuildResult`.
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

    /// Regression test (Step 10, #2): `edge_count` <= `raw_edge_count`.
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

    /// Regression test (Step 10, #4): File counts use plugin detection.
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

    /// Regression test (Step 10, #5): Manifest `edge_count` matches `BuildResult` (deduplicated).
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

    /// Regression test (Step 10, #8): Build command provenance in manifest.
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

    /// Regression test (Step 10, #9): Analysis identity hash matches manifest bytes hash.
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
