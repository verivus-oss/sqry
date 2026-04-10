//! Watch mode command for real-time index updates

use crate::args::Cli;
use crate::commands::index::{
    ClasspathCliOptions, build_and_persist_with_optional_classpath, create_build_config,
    create_progress_reporter,
};
#[cfg(feature = "jvm-classpath")]
use crate::commands::index::{inject_classpath_into_graph, run_classpath_pipeline_only};
use crate::plugin_defaults::{self, PluginSelectionMode};
use anyhow::{Context, Result};
use sqry_core::graph::unified::persistence::GraphStorage;
#[cfg(feature = "jvm-classpath")]
use sqry_core::watch::FileChange;
use sqry_core::watch::FileWatcher;
use std::path::PathBuf;
use std::time::Duration;

/// Execute the watch command.
///
/// # Errors
/// Returns an error if the index cannot be loaded or watch mode fails.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
#[allow(clippy::fn_params_excessive_bools)] // CLI flags map directly to booleans.
#[allow(clippy::needless_pass_by_value)] // CLI owned args are forwarded and cached directly
pub fn execute(
    cli: &Cli,
    path: Option<String>,
    threads: Option<usize>,
    debounce: Option<u64>,
    _show_stats: bool, // Detailed stats not supported for unified graph yet
    build_if_missing: bool,
    classpath: bool,
    _no_classpath: bool,
    classpath_depth: crate::args::ClasspathDepthArg,
    classpath_file: Option<PathBuf>,
    build_system: Option<String>,
    force_classpath: bool,
) -> Result<()> {
    let root_path = resolve_path(path)?;
    let build_config = create_build_config(cli, &root_path, threads)?;
    let classpath_opts = ClasspathCliOptions {
        enabled: classpath,
        depth: classpath_depth,
        classpath_file: classpath_file.as_deref(),
        build_system: build_system.as_deref(),
        force_classpath,
    };
    #[cfg(feature = "jvm-classpath")]
    let mut classpath_cache = None;

    // Check if graph exists
    let storage = GraphStorage::new(&root_path);
    if !storage.exists() {
        if build_if_missing {
            println!("🔨 Building initial graph...");
            let (_, progress) = create_progress_reporter(cli);
            let resolved_plugins = plugin_defaults::resolve_plugin_selection(
                cli,
                &root_path,
                PluginSelectionMode::FreshWrite,
            )?;
            #[cfg(feature = "jvm-classpath")]
            {
                let _build_result = build_and_persist_watch_iteration(
                    &root_path,
                    &resolved_plugins,
                    &build_config,
                    "cli:watch",
                    progress,
                    Some(&classpath_opts),
                    &mut classpath_cache,
                    &[],
                )?;
            }
            #[cfg(not(feature = "jvm-classpath"))]
            {
                let _build_result = build_and_persist_with_optional_classpath(
                    &root_path,
                    &resolved_plugins,
                    &build_config,
                    "cli:watch",
                    progress,
                    Some(&classpath_opts),
                )?;
            }
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
        let resolved_plugins = plugin_defaults::resolve_plugin_selection(
            cli,
            &root_path,
            PluginSelectionMode::ExistingWrite,
        )?;
        #[cfg(feature = "jvm-classpath")]
        let build_result = build_and_persist_watch_iteration(
            &root_path,
            &resolved_plugins,
            &build_config,
            "cli:watch",
            progress.clone(),
            Some(&classpath_opts),
            &mut classpath_cache,
            &changes,
        );
        #[cfg(not(feature = "jvm-classpath"))]
        let build_result = build_and_persist_with_optional_classpath(
            &root_path,
            &resolved_plugins,
            &build_config,
            "cli:watch",
            progress.clone(),
            Some(&classpath_opts),
        );
        match build_result {
            Ok(_build_result) => {
                println!("✓ Graph updated in {:.2}s", start.elapsed().as_secs_f64());
            }
            Err(e) => {
                eprintln!("❌ Error updating graph: {e}");
            }
        }
        println!();
    }
}

#[cfg(feature = "jvm-classpath")]
const CLASSPATH_INVALIDATION_FILE_NAMES: &[&str] = &[
    "build.gradle",
    "build.gradle.kts",
    "gradle.properties",
    "settings.gradle",
    "settings.gradle.kts",
    "pom.xml",
    "build.sbt",
    "WORKSPACE",
    "WORKSPACE.bazel",
    "MODULE.bazel",
    "gradle-wrapper.properties",
];

#[cfg(feature = "jvm-classpath")]
fn classpath_inputs_changed(
    root_path: &std::path::Path,
    changes: &[FileChange],
    classpath_opts: &ClasspathCliOptions<'_>,
) -> bool {
    if classpath_opts.force_classpath {
        return true;
    }

    let manual_classpath = classpath_opts.classpath_file.map(|path| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            root_path.join(path)
        }
    });

    changes.iter().any(|change| {
        let path = match change {
            FileChange::Created(path) | FileChange::Modified(path) | FileChange::Deleted(path) => {
                path
            }
        };

        if manual_classpath
            .as_ref()
            .is_some_and(|cp_file| path == cp_file)
        {
            return true;
        }

        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| CLASSPATH_INVALIDATION_FILE_NAMES.contains(&name))
    })
}

#[cfg(feature = "jvm-classpath")]
fn build_and_persist_watch_iteration(
    root_path: &std::path::Path,
    resolved_plugins: &crate::plugin_defaults::ResolvedPluginManager,
    build_config: &sqry_core::graph::unified::build::BuildConfig,
    build_command: &str,
    progress: sqry_core::progress::SharedReporter,
    classpath_opts: Option<&ClasspathCliOptions<'_>>,
    classpath_cache: &mut Option<sqry_classpath::pipeline::ClasspathPipelineResult>,
    changes: &[FileChange],
) -> Result<sqry_core::graph::unified::build::BuildResult> {
    if let Some(classpath_opts) = classpath_opts.filter(|opts| opts.enabled) {
        let should_refresh = classpath_cache.is_none()
            || classpath_inputs_changed(root_path, changes, classpath_opts);
        if should_refresh {
            *classpath_cache = Some(run_classpath_pipeline_only(root_path, classpath_opts)?);
        }

        let (mut graph, effective_threads) =
            sqry_core::graph::unified::build::build_unified_graph_with_progress(
                root_path,
                &resolved_plugins.plugin_manager,
                build_config,
                progress.clone(),
            )?;

        if let Some(classpath_result) = classpath_cache.as_ref() {
            inject_classpath_into_graph(&mut graph, classpath_result)?;
        }

        let (_graph, build_result) = sqry_core::graph::unified::build::persist_and_analyze_graph(
            graph,
            root_path,
            &resolved_plugins.plugin_manager,
            build_config,
            build_command,
            resolved_plugins.persisted_selection.clone(),
            progress,
            effective_threads,
        )?;
        return Ok(build_result);
    }

    build_and_persist_with_optional_classpath(
        root_path,
        resolved_plugins,
        build_config,
        build_command,
        progress,
        None,
    )
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

#[cfg(all(test, feature = "jvm-classpath"))]
mod tests {
    use super::*;

    fn classpath_opts<'a>(classpath_file: Option<&'a std::path::Path>) -> ClasspathCliOptions<'a> {
        ClasspathCliOptions {
            enabled: true,
            depth: crate::args::ClasspathDepthArg::Full,
            classpath_file,
            build_system: None,
            force_classpath: false,
        }
    }

    #[test]
    fn classpath_invalidation_includes_gradle_property_files() {
        let root = std::path::Path::new("/repo");
        let changes = vec![
            FileChange::Modified(root.join("gradle.properties")),
            FileChange::Modified(root.join("gradle/wrapper/gradle-wrapper.properties")),
        ];

        assert!(
            classpath_inputs_changed(root, &changes[..1], &classpath_opts(None)),
            "gradle.properties should invalidate the classpath cache"
        );
        assert!(
            classpath_inputs_changed(root, &changes[1..], &classpath_opts(None)),
            "gradle-wrapper.properties should invalidate the classpath cache"
        );
    }

    #[test]
    fn classpath_invalidation_ignores_regular_source_files() {
        let root = std::path::Path::new("/repo");
        let changes = vec![FileChange::Modified(root.join("src/Main.java"))];
        assert!(
            !classpath_inputs_changed(root, &changes, &classpath_opts(None)),
            "ordinary source edits should reuse the cached classpath result"
        );
    }
}
