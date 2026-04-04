//! Workspace graph loader for CLI graph commands.
//!
//! This module loads a unified `CodeGraph` either from a persisted snapshot or
//! by invoking the core `build_unified_graph` entrypoint with the shared plugin
//! registry.

use crate::args::Cli;
use crate::plugin_defaults::{self, PluginSelectionMode};
use anyhow::{Context, Result, bail};
use sqry_core::graph::CodeGraph;
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph_with_progress};
use sqry_core::graph::unified::persistence::{GraphStorage, load_from_path};
use sqry_core::progress::{SharedReporter, no_op_reporter};
use std::path::Path;

/// Loader configuration derived from CLI flags.
#[derive(Debug, Clone, Default)]
pub struct GraphLoadConfig {
    pub include_hidden: bool,
    pub follow_symlinks: bool,
    pub max_depth: Option<usize>,
    /// Force building from source files, even if a snapshot exists.
    /// Used by the index command to always rebuild.
    pub force_build: bool,
}

/// Load a unified code graph using the new Arena+CSR storage architecture.
///
/// This is the preferred entry point for CLI graph operations. It loads a graph
/// either from a persisted snapshot or by building from source files.
///
/// # Loading Strategy
///
/// 1. First tries to load from persisted snapshot (`.sqry/graph/snapshot.sqry`)
/// 2. If no snapshot exists, builds from source files using language plugins
///
/// # Arguments
/// * `root` - Root directory to scan for source files
/// * `config` - Configuration for file walking (hidden files, symlinks, depth)
///
/// # Returns
/// A `CodeGraph` populated with nodes and edges from all supported languages
///
/// # Errors
/// Returns an error if the path is missing, the snapshot is invalid, or the graph build fails.
///
/// # Example
/// ```ignore
/// use std::path::Path;
/// use sqry_cli::commands::graph::loader::{load_unified_graph, GraphLoadConfig};
///
/// let config = GraphLoadConfig::default();
/// let graph = load_unified_graph(Path::new("."), &config)?;
/// # Ok::<(), anyhow::Error>(())
/// ```
#[allow(dead_code)]
pub fn load_unified_graph(root: &Path, config: &GraphLoadConfig) -> Result<CodeGraph> {
    load_unified_graph_with_progress_and_plugins(
        root,
        config,
        &sqry_plugin_registry::create_plugin_manager_all(),
        no_op_reporter(),
    )
}

/// CLI-aware graph loader that enforces manifest-backed plugin semantics.
///
/// # Errors
///
/// Returns an error if the workspace path is invalid, manifest-selected plugins
/// cannot load the persisted snapshot, or the fallback source build fails.
pub fn load_unified_graph_for_cli(
    root: &Path,
    config: &GraphLoadConfig,
    cli: &Cli,
) -> Result<CodeGraph> {
    let resolved_plugins =
        plugin_defaults::resolve_plugin_selection(cli, root, PluginSelectionMode::ReadOnly)?;
    load_unified_graph_with_progress_and_plugins(
        root,
        config,
        &resolved_plugins.plugin_manager,
        no_op_reporter(),
    )
}

/// Load a unified code graph with progress reporting.
///
/// Same as [`load_unified_graph`] but accepts a progress reporter for tracking
/// build progress when loading from source files.
///
/// # Arguments
/// * `root` - Root directory to scan for source files
/// * `config` - Configuration for file walking (hidden files, symlinks, depth)
/// * `progress` - Progress reporter for build status updates
///
/// # Returns
/// A `CodeGraph` populated with nodes and edges from all supported languages
///
/// # Errors
/// Returns an error if the path is missing, the snapshot is invalid, or the graph build fails.
#[allow(dead_code)]
pub fn load_unified_graph_with_progress(
    root: &Path,
    config: &GraphLoadConfig,
    progress: SharedReporter,
) -> Result<CodeGraph> {
    load_unified_graph_with_progress_and_plugins(
        root,
        config,
        &sqry_plugin_registry::create_plugin_manager_all(),
        progress,
    )
}

fn load_unified_graph_with_progress_and_plugins(
    root: &Path,
    config: &GraphLoadConfig,
    plugins: &sqry_core::plugin::PluginManager,
    progress: SharedReporter,
) -> Result<CodeGraph> {
    if !root.exists() {
        bail!("Path {} does not exist", root.display());
    }

    // Try to load from persisted snapshot first (unless force_build is set)
    if config.force_build {
        log::info!("Force build enabled, skipping snapshot load");
    } else {
        let storage = GraphStorage::new(root);
        if storage.exists() {
            log::info!(
                "Loading unified graph from snapshot: {}",
                storage.snapshot_path().display()
            );
            match load_from_path(storage.snapshot_path(), Some(plugins)) {
                Ok(mut graph) => {
                    log::info!("Loaded graph from snapshot");

                    // Restore confidence metadata from manifest if available
                    // The snapshot binary format doesn't include confidence,
                    // so we load it separately from the manifest JSON.
                    if let Ok(manifest) = storage.load_manifest()
                        && !manifest.confidence.is_empty()
                    {
                        log::debug!(
                            "Restoring confidence metadata for {} languages",
                            manifest.confidence.len()
                        );
                        graph.set_confidence(manifest.confidence);
                    }

                    return Ok(graph);
                }
                Err(e) => {
                    // Manifest present but snapshot missing/corrupt → corruption
                    // Do not silently rebuild; user should run `sqry index --force`
                    bail!(
                        "Index at {} is corrupted or incomplete ({}). Run `sqry index --force` to rebuild.",
                        root.display(),
                        e
                    );
                }
            }
        }
    }

    // Build from source files
    log::info!(
        "Building unified graph from source files in {}",
        root.display()
    );

    let build_config = BuildConfig {
        include_hidden: config.include_hidden,
        follow_links: config.follow_symlinks,
        max_depth: config.max_depth,
        num_threads: None,
        ..BuildConfig::default()
    };

    let graph = build_unified_graph_with_progress(root, plugins, &build_config, progress)
        .context("Failed to build unified graph")?;

    log::info!("Built unified graph with {} nodes", graph.node_count());
    Ok(graph)
}
