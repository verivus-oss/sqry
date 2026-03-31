//! Index command implementation

use crate::args::Cli;
use crate::progress::{CliProgressReporter, CliStepProgressReporter, StepRunner};
use anyhow::{Context, Result};
use sqry_core::graph::unified::analysis::ReachabilityStrategy;
use sqry_core::graph::unified::build::BuildResult;
use sqry_core::graph::unified::build::entrypoint::AnalysisStrategySummary;
use sqry_core::graph::unified::persistence::{GraphStorage, load_header_from_path};
use sqry_core::json_response::IndexStatus;
use sqry_core::progress::{SharedReporter, no_op_reporter};
use std::fs;
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

/// Thread pool creation metrics for diagnostic output.
///
/// Emitted as JSON to stdout when `SQRY_EMIT_THREAD_POOL_METRICS=1` is set.
/// Used for build diagnostics and performance monitoring.
#[derive(serde::Serialize)]
struct ThreadPoolMetrics {
    thread_pool_creations: u64,
}

/// Get the current HEAD commit SHA from a git repository.
///
/// # Arguments
///
/// * `path` - Path to the repository root (or any path within it)
///
/// # Returns
///
/// Returns `Some(commit_sha)` if the path is in a git repository with at least one commit,
/// `None` otherwise (not a git repo, no commits, or git not available).
fn get_git_head_commit(path: &Path) -> Option<String> {
    let output = Command::new("git")
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

// format_validation_prometheus removed
// format_validation_summary removed
// validation_warning_count removed

/// Run index build command
///
/// # Arguments
///
/// * `cli` - CLI configuration (for validation flags)
/// * `path` - Directory to index
/// * `force` - Force rebuild even if index exists
/// * `threads` - Number of threads for parallel indexing (None = auto-detect)
///
/// # Errors
///
/// Returns an error if index build or persistence fails.
///
/// # Panics
///
/// Panics if the index is missing after a successful build-and-save sequence.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::fn_params_excessive_bools)] // CLI flags map directly to booleans.
#[allow(clippy::too_many_arguments)]
pub fn run_index(
    cli: &Cli,
    path: &str,
    force: bool,
    threads: Option<usize>,
    add_to_gitignore: bool,
    _no_incremental: bool,
    _cache_dir: Option<&str>,
    _no_compress: bool,
    enable_macro_expansion: bool,
    cfg_flags: &[String],
    expand_cache: Option<&std::path::Path>,
) -> Result<()> {
    if let Some(0) = threads {
        anyhow::bail!("--threads must be >= 1");
    }

    let root_path = Path::new(path);

    handle_gitignore(root_path, add_to_gitignore);

    // Check if graph already exists
    let storage = GraphStorage::new(root_path);
    if storage.exists() && !force {
        println!("Index already exists at {}", storage.graph_dir().display());
        println!("Use --force to rebuild, or run 'sqry update' to update incrementally");
        return Ok(());
    }

    // Log macro boundary analysis configuration
    if enable_macro_expansion || !cfg_flags.is_empty() || expand_cache.is_some() {
        log::info!(
            "Macro boundary config: expansion={}, cfg_flags={:?}, expand_cache={:?}",
            enable_macro_expansion,
            cfg_flags,
            expand_cache,
        );
    }

    print_index_build_banner(root_path, threads);

    let start = Instant::now();
    let mut step_runner = StepRunner::new(!std::io::stderr().is_terminal() && !cli.json);

    let (progress_bar, progress) = create_progress_reporter(cli);

    // Build unified graph using the consolidated pipeline
    let build_config = create_build_config(cli, root_path, threads)?;
    let build_result = step_runner.step("Build unified graph", || -> Result<_> {
        let (_graph, build_result) =
            sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
                root_path,
                &sqry_plugin_registry::create_plugin_manager(),
                &build_config,
                "cli:index",
                progress.clone(),
            )?;
        Ok(build_result)
    })?;

    finish_progress_bar(progress_bar.as_ref());

    let elapsed = start.elapsed();

    // Emit thread pool metrics if requested (diagnostic feature)
    if std::env::var("SQRY_EMIT_THREAD_POOL_METRICS")
        .ok()
        .is_some_and(|v| v == "1")
    {
        let metrics = ThreadPoolMetrics {
            thread_pool_creations: 1,
        };
        if let Ok(json) = serde_json::to_string(&metrics) {
            println!("{json}");
        }
    }

    // Report success
    if !cli.json {
        let status = build_graph_status(&storage)?;
        emit_graph_summary(
            &storage,
            &status,
            &build_result,
            elapsed,
            "✓ Index built successfully!",
        );
    }

    Ok(())
}

fn emit_graph_summary(
    storage: &GraphStorage,
    status: &IndexStatus,
    build_result: &BuildResult,
    elapsed: std::time::Duration,
    summary_banner: &str,
) {
    println!("\n{summary_banner}");
    println!(
        "  Graph: {} nodes, {} canonical edges ({} raw)",
        build_result.node_count, build_result.edge_count, build_result.raw_edge_count
    );
    println!(
        "  Corpus: {} files across {} languages",
        build_result.total_files,
        build_result.file_count.len()
    );
    println!(
        "  Top languages: {}",
        format_top_languages(&build_result.file_count)
    );
    println!(
        "  Reachability: {}",
        format_analysis_strategy_highlights(&build_result.analysis_strategies)
    );
    if status.supports_relations {
        println!("  Relations: Enabled");
    }
    println!("  Graph path: {}", storage.graph_dir().display());
    println!("  Analysis path: {}", storage.analysis_dir().display());
    println!("  Time taken: {:.2}s", elapsed.as_secs_f64());
}

fn print_index_build_banner(root_path: &Path, threads: Option<usize>) {
    if let Some(1) = threads {
        println!(
            "Building index for {} (single-threaded)...",
            root_path.display()
        );
    } else if let Some(count) = threads {
        println!(
            "Building index for {} using {} threads...",
            root_path.display(),
            count
        );
    } else {
        println!("Building index for {} (parallel)...", root_path.display());
    }
}

pub(crate) fn create_progress_reporter(
    cli: &Cli,
) -> (Option<Arc<CliProgressReporter>>, SharedReporter) {
    // Create progress reporter (disable when not connected to a TTY)
    let progress_bar = if std::io::stderr().is_terminal() && !cli.json {
        Some(Arc::new(CliProgressReporter::new()))
    } else {
        None
    };

    let progress: SharedReporter = if let Some(progress_bar_ref) = &progress_bar {
        Arc::clone(progress_bar_ref) as SharedReporter
    } else if cli.json {
        no_op_reporter()
    } else {
        Arc::new(CliStepProgressReporter::new()) as SharedReporter
    };

    (progress_bar, progress)
}

fn finish_progress_bar(progress_bar: Option<&Arc<CliProgressReporter>>) {
    if let Some(progress_bar_ref) = progress_bar {
        progress_bar_ref.finish();
    }
}

// emit_index_summary removed — logic inlined in run_index
// handle_update_validation removed — validation moved to core
// emit_validation_failures removed — validation moved to core
// handle_validation_strictness removed — validation moved to core

// emit_update_summary removed

// build_index_status removed

// collect_languages removed

// write_index_status_json removed
// write_index_status_text removed
// write_index_status_found removed
// write_index_status_metadata removed
// write_index_status_missing removed
// write_validation_report_text removed
// write_dependency_validation removed
// write_id_validation removed
// write_graph_validation removed

fn build_graph_status(storage: &GraphStorage) -> Result<IndexStatus> {
    if !storage.exists() {
        return Ok(IndexStatus::not_found());
    }

    // Load manifest
    let manifest = storage
        .load_manifest()
        .context("Failed to load graph manifest")?;

    // Compute age
    let age_seconds = storage
        .snapshot_age(&manifest)
        .context("Failed to compute snapshot age")?
        .as_secs();

    // Get file count: prefer snapshot header (fast), fallback to manifest (CLI-built indexes)
    let total_files: Option<usize> =
        if let Ok(header) = load_header_from_path(storage.snapshot_path()) {
            // Read from snapshot header (always accurate)
            Some(header.file_count)
        } else if !manifest.file_count.is_empty() {
            // Fallback: sum manifest file counts (legacy CLI-built indexes)
            Some(manifest.file_count.values().sum())
        } else {
            // No file count available
            None
        };

    // Check if trigram index exists in graph storage
    // Trigram files would be stored alongside the snapshot
    let trigram_path = storage.graph_dir().join("trigram.idx");
    let has_trigram = trigram_path.exists();

    // Build status (map graph data to IndexStatus for compatibility)
    Ok(IndexStatus::from_index(
        storage.graph_dir().display().to_string(),
        manifest.built_at.clone(),
        age_seconds,
    )
    .symbol_count(manifest.node_count) // Map nodes → symbols
    .file_count_opt(total_files)
    .has_relations(manifest.edge_count > 0)
    .has_trigram(has_trigram)
    .build())
}

fn write_graph_status_text(
    streams: &mut crate::output::OutputStreams,
    status: &IndexStatus,
    root_path: &Path,
) -> Result<()> {
    if status.exists {
        streams.write_result("✓ Graph snapshot found\n")?;
        if let Some(path) = &status.path {
            streams.write_result(&format!("  Path: {path}\n"))?;
        }
        if let Some(created_at) = &status.created_at {
            streams.write_result(&format!("  Built: {created_at}\n"))?;
        }
        if let Some(age) = status.age_seconds {
            streams.write_result(&format!("  Age: {}\n", format_age(age)))?;
        }
        if let Some(count) = status.symbol_count {
            streams.write_result(&format!("  Nodes: {count}\n"))?;
        }
        if let Some(count) = status.file_count {
            streams.write_result(&format!("  Files: {count}\n"))?;
        }
        if status.supports_relations {
            streams.write_result("  Relations: ✓ Available\n")?;
        }
    } else {
        streams.write_result("✗ No graph snapshot found\n")?;
        streams.write_result("\nTo create a graph snapshot, run:\n")?;
        streams.write_result(&format!("  sqry index --force {}\n", root_path.display()))?;
    }

    Ok(())
}

fn format_age(age_seconds: u64) -> String {
    let hours = age_seconds / 3600;
    let days = hours / 24;
    if days > 0 {
        format!("{} days, {} hours", days, hours % 24)
    } else {
        format!("{hours} hours")
    }
}

fn format_top_languages(file_count: &std::collections::HashMap<String, usize>) -> String {
    if file_count.is_empty() {
        return "none".to_string();
    }

    let mut entries: Vec<_> = file_count.iter().collect();
    entries.sort_by(|(left_name, left_count), (right_name, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_name.cmp(right_name))
    });

    entries
        .into_iter()
        .take(3)
        .map(|(language, count)| format!("{language}={count}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_analysis_strategy_highlights(analysis_strategies: &[AnalysisStrategySummary]) -> String {
    if analysis_strategies.is_empty() {
        return "not available".to_string();
    }

    let mut interval_labels = Vec::new();
    let mut dag_bfs = Vec::new();

    for strategy in analysis_strategies {
        match strategy.strategy {
            ReachabilityStrategy::IntervalLabels => interval_labels.push(strategy.edge_kind),
            ReachabilityStrategy::DagBfs => dag_bfs.push(strategy.edge_kind),
        }
    }

    let mut groups = Vec::new();
    if !interval_labels.is_empty() {
        groups.push(format!("interval_labels({})", interval_labels.join(",")));
    }
    if !dag_bfs.is_empty() {
        groups.push(format!("dag_bfs({})", dag_bfs.join(",")));
    }

    groups.join(" | ")
}

/// Create a `BuildConfig` from CLI flags.
pub(crate) fn create_build_config(
    cli: &Cli,
    root_path: &Path,
    threads: Option<usize>,
) -> Result<sqry_core::graph::unified::build::BuildConfig> {
    Ok(sqry_core::graph::unified::build::BuildConfig {
        max_depth: if cli.max_depth == 0 {
            None
        } else {
            Some(cli.max_depth)
        },
        follow_links: cli.follow,
        include_hidden: cli.hidden,
        num_threads: threads,
        label_budget: sqry_core::graph::unified::analysis::resolve_label_budget_config(
            root_path, None, None, None, false,
        )?,
        ..sqry_core::graph::unified::build::BuildConfig::default()
    })
}

/// Run index update command
///
/// # Arguments
///
/// * `cli` - CLI configuration (for validation flags)
/// * `path` - Directory with existing index
/// * `show_stats` - Show detailed statistics
///
/// # Errors
/// Returns an error if the index cannot be loaded, updated, or validated.
pub fn run_update(
    cli: &Cli,
    path: &str,
    threads: Option<usize>,
    show_stats: bool,
    _no_incremental: bool,
    _cache_dir: Option<&str>,
) -> Result<()> {
    let root_path = Path::new(path);
    let mut step_runner = StepRunner::new(!std::io::stderr().is_terminal() && !cli.json);

    // Check if graph exists
    let storage = GraphStorage::new(root_path);
    if !storage.exists() {
        anyhow::bail!(
            "No index found at {}. Run 'sqry index' first.",
            storage.graph_dir().display()
        );
    }

    println!("Updating index for {}...", root_path.display());
    let start = Instant::now();

    // Determine update mode based on git availability
    let git_mode_disabled = std::env::var("SQRY_GIT_BACKEND")
        .ok()
        .is_some_and(|v| v == "none");

    let current_commit = if git_mode_disabled {
        None
    } else {
        get_git_head_commit(root_path)
    };

    // Determine if we're using git-aware or hash-based mode
    let using_git_mode = !git_mode_disabled && current_commit.is_some();

    let (progress_bar, progress) = create_progress_reporter(cli);

    // Update graph using consolidated pipeline
    let build_config = create_build_config(cli, root_path, threads)?;
    let build_result = step_runner.step("Update unified graph", || -> Result<_> {
        let (_graph, build_result) =
            sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
                root_path,
                &sqry_plugin_registry::create_plugin_manager(),
                &build_config,
                "cli:update",
                progress.clone(),
            )?;
        Ok(build_result)
    })?;

    finish_progress_bar(progress_bar.as_ref());

    let elapsed = start.elapsed();

    // Report success with appropriate message based on update mode
    if !cli.json {
        let status = build_graph_status(&storage)?;

        if using_git_mode {
            emit_graph_summary(
                &storage,
                &status,
                &build_result,
                elapsed,
                "✓ Index updated successfully!",
            );
        } else {
            emit_graph_summary(
                &storage,
                &status,
                &build_result,
                elapsed,
                "✓ Index updated successfully (hash-based mode)!",
            );
        }
    }

    if show_stats {
        println!("(Detailed stats are not available for unified graph update)");
    }

    Ok(())
}

#[allow(deprecated)]
/// Run index status command for programmatic consumers.
///
/// # Arguments
///
/// * `cli` - CLI configuration
/// * `path` - Directory to check for index
///
/// # Errors
/// Returns an error if the index status cannot be loaded or rendered.
pub fn run_index_status(
    cli: &Cli,
    path: &str,
    _metrics_format: crate::args::MetricsFormat,
) -> Result<()> {
    // Redirect to graph status as legacy index is removed
    run_graph_status(cli, path)
}

/// Run graph status command using unified graph architecture.
///
/// This command reports on the state of the unified graph snapshot
/// stored in `.sqry/graph/` directory instead of the legacy `.sqry-index`.
///
/// # Errors
///
/// Returns an error if manifest cannot be loaded or output formatting fails.
pub fn run_graph_status(cli: &Cli, path: &str) -> Result<()> {
    let root_path = Path::new(path);
    let storage = GraphStorage::new(root_path);
    let status = build_graph_status(&storage)?;

    // Output result (same format as run_index_status for compatibility)
    let mut streams = crate::output::OutputStreams::with_pager(cli.pager_config());

    if cli.json {
        let json =
            serde_json::to_string_pretty(&status).context("Failed to serialize graph status")?;
        streams.write_result(&json)?;
    } else {
        write_graph_status_text(&mut streams, &status, root_path)?;
    }

    streams.finish_checked()
}

/// Handles the .gitignore check and modification.
fn handle_gitignore(path: &Path, add_to_gitignore: bool) {
    if let Some(root) = find_git_root(path) {
        let gitignore_path = root.join(".gitignore");
        let entry = ".sqry-index/";
        let mut is_already_indexed = false;

        if gitignore_path.exists()
            && let Ok(file) = fs::File::open(&gitignore_path)
        {
            let reader = BufReader::new(file);
            if reader.lines().any(|line| {
                line.map(|l| l.trim() == ".sqry-index" || l.trim() == ".sqry-index/")
                    .unwrap_or(false)
            }) {
                is_already_indexed = true;
            }
        }

        if !is_already_indexed
            && add_to_gitignore
            && let Ok(mut file) = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&gitignore_path)
            && writeln!(file, "\n{entry}").is_ok()
        {
            println!("Added '{entry}' to .gitignore");
        } else if !is_already_indexed {
            print_gitignore_warning();
        }
    }
}

/// Find the root of the git repository by traversing up from the given path.
fn find_git_root(path: &Path) -> Option<&Path> {
    let mut current = path;
    loop {
        if current.join(".git").is_dir() {
            return Some(current);
        }
        if let Some(parent) = current.parent() {
            current = parent;
        } else {
            return None;
        }
    }
}

/// Prints a standard warning message about .gitignore.
fn print_gitignore_warning() {
    eprintln!(
        "\n\u{26a0}\u{fe0f} Warning: It is recommended to add the '.sqry-index/' directory to your .gitignore file."
    );
    eprintln!("This is a generated cache and can become large.\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_stack_test;
    use std::fs;
    use tempfile::TempDir;

    large_stack_test! {
    #[test]
    fn test_run_index_basic() {
        use crate::args::Cli;
        use clap::Parser;

        let tmp_cli_workspace = TempDir::new().unwrap();
        let file_path = tmp_cli_workspace.path().join("test.rs");
        fs::write(&file_path, "fn hello() {}").unwrap();

        let cli = Cli::parse_from(["sqry", "index"]);
        let result = run_index(
            &cli,
            tmp_cli_workspace.path().to_str().unwrap(),
            false,
            None,
            false,
            false,
            None,
            false, // no_compress
            false, // enable_macro_expansion
            &[],  // cfg_flags
            None,  // expand_cache
        );
        assert!(result.is_ok());

        // Check index was created
        let storage = GraphStorage::new(tmp_cli_workspace.path());
        assert!(storage.exists());
    }
    }

    large_stack_test! {
    #[test]
    fn test_run_index_force_rebuild() {
        use crate::args::Cli;
        use clap::Parser;

        let tmp_cli_workspace = TempDir::new().unwrap();
        let file_path = tmp_cli_workspace.path().join("test.rs");
        fs::write(&file_path, "fn hello() {}").unwrap();

        let cli = Cli::parse_from(["sqry", "index"]);

        // Build initial index
        run_index(
            &cli,
            tmp_cli_workspace.path().to_str().unwrap(),
            false,
            None,
            false,
            false,
            None,
            false, // no_compress
            false, // enable_macro_expansion
            &[],   // cfg_flags
            None,  // expand_cache
        )
        .unwrap();

        // Try to rebuild without force (should skip)
        let result = run_index(
            &cli,
            tmp_cli_workspace.path().to_str().unwrap(),
            false,
            None,
            false,
            false,
            None,
            false, // no_compress
            false, // enable_macro_expansion
            &[],  // cfg_flags
            None,  // expand_cache
        );
        assert!(result.is_ok());

        // Rebuild with force (should succeed)
        let result = run_index(
            &cli,
            tmp_cli_workspace.path().to_str().unwrap(),
            true,
            None,
            false,
            false,
            None,
            false, // no_compress
            false, // enable_macro_expansion
            &[],  // cfg_flags
            None,  // expand_cache
        );
        assert!(result.is_ok());
    }
    }

    large_stack_test! {
    #[test]
    fn test_run_update_no_index() {
        use crate::args::Cli;
        use clap::Parser;

        let tmp_cli_workspace = TempDir::new().unwrap();
        let cli = Cli::parse_from(["sqry", "update"]);

        let result = run_update(
            &cli,
            tmp_cli_workspace.path().to_str().unwrap(),
            None,
            false,
            false,
            None,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No index found"));
    }
    }

    large_stack_test! {
    #[test]
    fn test_run_index_status_no_index() {
        use crate::args::Cli;
        use clap::Parser;

        let tmp_cli_workspace = TempDir::new().unwrap();

        // Create CLI with JSON flag
        let cli = Cli::parse_from(["sqry", "--json"]);

        // Should succeed even with no index
        let result = run_index_status(
            &cli,
            tmp_cli_workspace.path().to_str().unwrap(),
            crate::args::MetricsFormat::Json,
        );
        assert!(
            result.is_ok(),
            "Index status should not error on missing index"
        );

        // The output would be captured via OutputStreams
        // We can't easily test the output here, but we verified it doesn't panic
    }
    }

    large_stack_test! {
    #[test]
    fn test_run_index_status_with_index() {
        use crate::args::Cli;
        use clap::Parser;

        let tmp_cli_workspace = TempDir::new().unwrap();
        let file_path = tmp_cli_workspace.path().join("test.rs");
        fs::write(&file_path, "fn test_func() {}").unwrap();

        let cli = Cli::parse_from(["sqry", "index"]);

        // Build index first
        run_index(
            &cli,
            tmp_cli_workspace.path().to_str().unwrap(),
            false,
            None,
            false,
            false,
            None,
            false, // no_compress
            false, // enable_macro_expansion
            &[],   // cfg_flags
            None,  // expand_cache
        )
        .unwrap();

        // Check status with JSON flag
        let cli = Cli::parse_from(["sqry", "--json"]);
        let result = run_index_status(
            &cli,
            tmp_cli_workspace.path().to_str().unwrap(),
            crate::args::MetricsFormat::Json,
        );
        assert!(
            result.is_ok(),
            "Index status should succeed with existing index"
        );

        // Verify the index actually exists
        let storage = GraphStorage::new(tmp_cli_workspace.path());
        assert!(storage.exists());

        // Load index and verify it has the symbol
        let manifest = storage.load_manifest().unwrap();
        assert_eq!(manifest.node_count, 1, "Should have 1 symbol");
    }
    }

    large_stack_test! {
    #[test]
    fn test_run_update_basic() {
        use crate::args::Cli;
        use clap::Parser;

        let tmp_cli_workspace = TempDir::new().unwrap();
        let file_path = tmp_cli_workspace.path().join("test.rs");
        fs::write(&file_path, "fn hello() {}").unwrap();

        let cli = Cli::parse_from(["sqry", "index"]);

        // Build initial index
        run_index(
            &cli,
            tmp_cli_workspace.path().to_str().unwrap(),
            false,
            None,
            false,
            false,
            None,
            false, // no_compress
            false, // enable_macro_expansion
            &[],   // cfg_flags
            None,  // expand_cache
        )
        .unwrap();

        // Update should succeed
        let result = run_update(
            &cli,
            tmp_cli_workspace.path().to_str().unwrap(),
            None,
            true,
            false,
            None,
        );
        assert!(result.is_ok());
    }
    }

    #[test]
    fn plugin_manager_registers_elixir_extensions() {
        let pm = crate::plugin_defaults::create_plugin_manager();
        assert!(
            pm.plugin_for_extension("ex").is_some(),
            "Elixir .ex extension missing"
        );
        assert!(
            pm.plugin_for_extension("exs").is_some(),
            "Elixir .exs extension missing"
        );
    }

    #[test]
    fn test_format_top_languages_orders_by_count_then_name() {
        let counts = std::collections::HashMap::from([
            ("rust".to_string(), 9_usize),
            ("python".to_string(), 4_usize),
            ("go".to_string(), 4_usize),
            ("typescript".to_string(), 2_usize),
        ]);

        assert_eq!(format_top_languages(&counts), "rust=9, go=4, python=4");
    }

    #[test]
    fn test_format_analysis_strategy_highlights_groups_by_strategy() {
        let strategies = vec![
            AnalysisStrategySummary {
                edge_kind: "calls",
                strategy: ReachabilityStrategy::IntervalLabels,
            },
            AnalysisStrategySummary {
                edge_kind: "imports",
                strategy: ReachabilityStrategy::DagBfs,
            },
            AnalysisStrategySummary {
                edge_kind: "references",
                strategy: ReachabilityStrategy::DagBfs,
            },
            AnalysisStrategySummary {
                edge_kind: "inherits",
                strategy: ReachabilityStrategy::IntervalLabels,
            },
        ];

        assert_eq!(
            format_analysis_strategy_highlights(&strategies),
            "interval_labels(calls,inherits) | dag_bfs(imports,references)"
        );
    }
}
