//! Watch mode command for real-time index updates

use crate::args::Cli;
use crate::commands::index::{create_build_config, create_progress_reporter};
use anyhow::{Context, Result};
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_core::watch::FileWatcher;
use std::path::PathBuf;
use std::time::Duration;

/// Execute the watch command.
///
/// # Errors
/// Returns an error if the index cannot be loaded or watch mode fails.
pub fn execute(
    cli: &Cli,
    path: Option<String>,
    threads: Option<usize>,
    debounce: Option<u64>,
    _show_stats: bool, // Detailed stats not supported for unified graph yet
    build_if_missing: bool,
) -> Result<()> {
    let root_path = resolve_path(path)?;
    let build_config = create_build_config(cli, threads);
    let plugins = sqry_plugin_registry::create_plugin_manager();

    // Check if graph exists
    let storage = GraphStorage::new(&root_path);
    if !storage.exists() {
        if build_if_missing {
            println!("🔨 Building initial graph...");
            let (_, progress) = create_progress_reporter(cli);
            sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
                &root_path,
                &plugins,
                &build_config,
                "cli:watch",
                progress,
            )?;
        } else {
            anyhow::bail!(
                "No index found at {}. Use --build to create one, or run 'sqry index' first.",
                root_path.display()
            );
        }
    }

    // Create watcher
    let watcher = FileWatcher::new(&root_path)?;
    let debounce_duration = debounce.map_or(Duration::from_millis(500), Duration::from_millis);

    println!("🔍 Watch mode started");
    println!("📂 Monitoring: {}", root_path.display());
    println!("⏱️  Debounce: {}ms", debounce_duration.as_millis());
    println!();
    println!("Press Ctrl+C to stop...");
    println!();

    let (_, progress) = create_progress_reporter(cli);

    loop {
        // Wait for changes
        let changes = watcher.wait_with_debounce(debounce_duration)?;

        if changes.is_empty() {
            continue;
        }

        println!(
            "📝 Detected {} file changes, updating graph...",
            changes.len()
        );
        let start = std::time::Instant::now();

        // Full rebuild using consolidated pipeline
        match sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
            &root_path,
            &plugins,
            &build_config,
            "cli:watch",
            progress.clone(),
        ) {
            Ok((_graph, _build_result)) => {
                println!("✓ Graph updated in {:.2}s", start.elapsed().as_secs_f64());
            }
            Err(e) => {
                eprintln!("❌ Error updating graph: {e}");
            }
        }
        println!();
    }
}

/// Resolve path argument to absolute `PathBuf`
fn resolve_path(path: Option<String>) -> Result<PathBuf> {
    let path_str = path.unwrap_or_else(|| ".".to_string());
    let path = PathBuf::from(path_str);

    if path.exists() {
        path.canonicalize().context("Failed to resolve path")
    } else {
        anyhow::bail!("Path does not exist: {}", path.display());
    }
}
