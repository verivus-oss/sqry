//! Watch mode command for real-time index updates

use crate::args::Cli;
use crate::commands::index::{
    ClasspathCliOptions, UpdateStatsReport, build_and_persist_with_optional_classpath,
    compute_update_stats, create_build_config, create_progress_reporter, print_update_stats,
};
#[cfg(feature = "jvm-classpath")]
use crate::commands::index::{CliInputResolver, publish_with_current_inputs};
#[cfg(feature = "jvm-classpath")]
use crate::commands::index::{inject_classpath_into_graph, run_classpath_pipeline_only};
use crate::plugin_defaults::{self, PluginSelectionMode};
use anyhow::{Context, Result};
use sqry_core::graph::unified::build::{BuildResult, MacroOptionsRequest, UnreadableManifestRule};
use sqry_core::graph::unified::persistence::{GraphHeader, GraphStorage, load_header_from_path};
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
    show_stats: bool,
    build_if_missing: bool,
    classpath: bool,
    classpath_depth: crate::args::ClasspathDepthArg,
    classpath_file: Option<PathBuf>,
    build_system: Option<String>,
    force_classpath: bool,
    no_build_tool: bool,
) -> Result<()> {
    let root_path = resolve_path(path)?;
    let storage = GraphStorage::new(&root_path);
    // `sqry watch` carries no macro flags of its own, so every build reuses
    // the options the manifest records (surface parity W4, W4-D7). At
    // startup an unresolvable request is an error and the process exits
    // non-zero. The loop below resolves again on every iteration (W4-D10),
    // because the recorded expand cache directory can disappear while the
    // watcher runs.
    let (startup_build_config, _macro_options) = create_build_config(
        cli,
        &root_path,
        threads,
        &MacroOptionsRequest::empty(),
        watch_manifest_rule(&storage),
    )?;
    let classpath_opts = ClasspathCliOptions {
        enabled: classpath,
        depth: classpath_depth,
        classpath_file: classpath_file.as_deref(),
        build_system: build_system.as_deref(),
        force_classpath,
        no_build_tool,
    };
    #[cfg(feature = "jvm-classpath")]
    let mut classpath_cache = None;

    // Check if graph exists
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
                    &startup_build_config,
                    &|| {
                        resolve_watch_inputs(
                            cli,
                            &root_path,
                            threads,
                            PluginSelectionMode::FreshWrite,
                        )
                    },
                    "cli:watch",
                    progress,
                    Some(&classpath_opts),
                    &mut classpath_cache,
                    &[],
                    cli.json,
                )?;
            }
            #[cfg(not(feature = "jvm-classpath"))]
            {
                let _build_result = build_and_persist_with_optional_classpath(
                    &root_path,
                    &resolved_plugins,
                    &startup_build_config,
                    &|| {
                        resolve_watch_inputs(
                            cli,
                            &root_path,
                            threads,
                            PluginSelectionMode::FreshWrite,
                        )
                    },
                    "cli:watch",
                    progress,
                    Some(&classpath_opts),
                    None,
                    cli.json,
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
    // C004: platform-aware debounce default (matches the CLI help text).
    // The `SQRY_LIMITS__WATCH__DEBOUNCE_MS` env override wins over the
    // platform default but is itself overridden by an explicit `--debounce`.
    let debounce_duration = debounce.map_or_else(default_watch_debounce, Duration::from_millis);

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

        // Resolve the build configuration from the tree this iteration is
        // about to build (surface parity W4, W4-D10). A configuration that
        // cannot be resolved now (the recorded expand cache directory was
        // deleted, or the manifest became unreadable) is reported, nothing
        // is written, and the watcher keeps watching; the next iteration
        // resolves again, so restoring the directory resumes without a
        // restart.
        let build_config = match create_build_config(
            cli,
            &root_path,
            threads,
            &MacroOptionsRequest::empty(),
            watch_manifest_rule(&storage),
        ) {
            Ok((build_config, _macro_options)) => build_config,
            Err(e) => {
                eprintln!("❌ Error: {e:#}");
                eprintln!("   Nothing was written for these changes; still watching.");
                println!();
                continue;
            }
        };

        let start = std::time::Instant::now();
        // `--stats` (surface parity W4, W4-D4): the header the iteration
        // replaces, read before the build so the deltas compare header to
        // header the way `sqry update --stats` does.
        let pre_iteration_header = if show_stats {
            load_header_from_path(storage.snapshot_path()).ok()
        } else {
            None
        };

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
            &|| resolve_watch_inputs(cli, &root_path, threads, PluginSelectionMode::ExistingWrite),
            "cli:watch",
            progress.clone(),
            Some(&classpath_opts),
            &mut classpath_cache,
            &changes,
            cli.json,
        );
        #[cfg(not(feature = "jvm-classpath"))]
        let build_result = build_and_persist_with_optional_classpath(
            &root_path,
            &resolved_plugins,
            &build_config,
            &|| resolve_watch_inputs(cli, &root_path, threads, PluginSelectionMode::ExistingWrite),
            "cli:watch",
            progress.clone(),
            Some(&classpath_opts),
            None,
            cli.json,
        );
        match build_result {
            Ok(build_result) => {
                let elapsed = start.elapsed();
                println!("✓ Graph updated in {:.2}s", elapsed.as_secs_f64());
                if show_stats {
                    render_watch_iteration_stats(
                        cli,
                        &storage,
                        &build_result,
                        pre_iteration_header.as_ref(),
                        elapsed,
                    );
                }
            }
            Err(e) => {
                eprintln!("❌ Error updating graph: {e:#}");
            }
        }
        println!();
    }
}

/// The unreadable-manifest rule for a watch build, decided from the tree as
/// it stands when the build is about to run. It follows the roster's rule: a
/// fresh `--build` over no index treats an unreadable manifest as no record,
/// a rebuild over an existing index refuses.
fn watch_manifest_rule(storage: &GraphStorage) -> UnreadableManifestRule {
    if storage.exists() {
        UnreadableManifestRule::Refuse
    } else {
        UnreadableManifestRule::TreatAsNoRecord
    }
}

/// The statistics of one watch iteration, the figures `sqry update --stats`
/// reports (surface parity W4, design W4-D4): the iteration's
/// [`BuildResult`] with node, edge and registered-file deltas against the
/// header the iteration replaced, the post-iteration header read from the
/// freshly written snapshot. The mode column reports hash-based: the watch
/// loop rebuilds on file events, never on git state.
pub(crate) fn watch_iteration_stats(
    storage: &GraphStorage,
    build_result: &BuildResult,
    pre_iteration_header: Option<&GraphHeader>,
    elapsed: Duration,
) -> UpdateStatsReport {
    let post_iteration_header = load_header_from_path(storage.snapshot_path()).ok();
    compute_update_stats(
        build_result,
        pre_iteration_header,
        post_iteration_header.as_ref(),
        elapsed,
        false,
    )
}

/// Render one watch iteration's statistics the way `sqry update --stats`
/// renders an update: [`watch_iteration_stats`] printed through the shared
/// printer, so `--json` emits the same `update_stats` document
/// `sqry update --stats --json` emits.
pub(crate) fn render_watch_iteration_stats(
    cli: &Cli,
    storage: &GraphStorage,
    build_result: &BuildResult,
    pre_iteration_header: Option<&GraphHeader>,
    elapsed: Duration,
) {
    let report = watch_iteration_stats(storage, build_result, pre_iteration_header, elapsed);
    print_update_stats(cli, &report);
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

/// The inputs a watch build resolves, again, under the index's persist
/// lock (decision D-i8-4): the roster in `mode` and the build configuration
/// with the macro options the manifest records, under the unreadable-manifest
/// rule the tree implies now.
fn resolve_watch_inputs(
    cli: &Cli,
    root_path: &std::path::Path,
    threads: Option<usize>,
    mode: PluginSelectionMode,
) -> Result<(
    plugin_defaults::ResolvedPluginManager,
    sqry_core::graph::unified::build::BuildConfig,
)> {
    let storage = GraphStorage::new(root_path);
    let plugins = plugin_defaults::resolve_plugin_selection(cli, root_path, mode)?;
    let (config, _macro_options) = create_build_config(
        cli,
        root_path,
        threads,
        &MacroOptionsRequest::empty(),
        watch_manifest_rule(&storage),
    )?;
    Ok((plugins, config))
}

#[cfg(feature = "jvm-classpath")]
#[allow(clippy::too_many_arguments)]
fn build_and_persist_watch_iteration(
    root_path: &std::path::Path,
    resolved_plugins: &crate::plugin_defaults::ResolvedPluginManager,
    build_config: &sqry_core::graph::unified::build::BuildConfig,
    resolve: &CliInputResolver<'_>,
    build_command: &str,
    progress: sqry_core::progress::SharedReporter,
    classpath_opts: Option<&ClasspathCliOptions<'_>>,
    classpath_cache: &mut Option<sqry_classpath::pipeline::ClasspathPipelineResult>,
    changes: &[FileChange],
    json_output: bool,
) -> Result<sqry_core::graph::unified::build::BuildResult> {
    if let Some(classpath_opts) = classpath_opts.filter(|opts| opts.enabled) {
        let build_progress = progress.clone();
        return publish_with_current_inputs(
            root_path,
            (resolved_plugins, build_config),
            resolve,
            &mut |plugins, config| {
                let should_refresh = classpath_cache.is_none()
                    || classpath_inputs_changed(root_path, changes, classpath_opts);
                if should_refresh {
                    *classpath_cache =
                        run_classpath_pipeline_only(root_path, classpath_opts, json_output)?;
                }

                let (mut graph, effective_threads) =
                    sqry_core::graph::unified::build::build_unified_graph_with_progress(
                        root_path,
                        &plugins.plugin_manager,
                        config,
                        build_progress.clone(),
                    )?;

                if let Some(classpath_result) = classpath_cache.as_ref() {
                    inject_classpath_into_graph(&mut graph, classpath_result, json_output)?;
                }
                Ok((graph, effective_threads))
            },
            &mut |plugins, config, (graph, effective_threads)| {
                let (_graph, build_result) =
                    sqry_core::graph::unified::build::persist_and_analyze_graph(
                        graph,
                        root_path,
                        &plugins.plugin_manager,
                        config,
                        build_command,
                        plugins.persisted_selection.clone(),
                        progress.clone(),
                        effective_threads,
                    )?;
                Ok(build_result)
            },
        );
    }

    build_and_persist_with_optional_classpath(
        root_path,
        resolved_plugins,
        build_config,
        resolve,
        build_command,
        progress,
        None,
        None,
        json_output,
    )
}

/// Compute the default debounce duration for the file watcher.
///
/// Resolution order (highest priority first):
///
/// 1. `SQRY_LIMITS__WATCH__DEBOUNCE_MS` env var (parsed as u64 milliseconds).
/// 2. Platform-specific default: 400 ms on macOS (`FSEvents` coalescing
///    latency), 100 ms on Linux/Windows (inotify / `ReadDirectoryChangesW`
///    react more quickly so a tighter debounce keeps interactivity).
fn default_watch_debounce() -> Duration {
    if let Ok(raw) = std::env::var("SQRY_LIMITS__WATCH__DEBOUNCE_MS")
        && let Ok(ms) = raw.trim().parse::<u64>()
    {
        return Duration::from_millis(ms);
    }
    if cfg!(target_os = "macos") {
        Duration::from_millis(400)
    } else {
        Duration::from_millis(100)
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

#[cfg(test)]
mod debounce_tests {
    use super::default_watch_debounce;
    use std::time::Duration;

    /// Serialise the env-mutating debounce tests so the two tests do not
    /// race on `SQRY_LIMITS__WATCH__DEBOUNCE_MS`. `cargo test` parallelises
    /// by default; without this guard the platform-default test would
    /// observe the override test's set/unset window and flap.
    static ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn default_watch_debounce_honours_env_override() {
        // C004: explicit env override wins over platform default. Use a
        // sentinel value (777ms) that can't collide with either platform
        // default (100 / 400). Reset after the assertion to avoid leaking
        // process state into other tests.
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::set_var("SQRY_LIMITS__WATCH__DEBOUNCE_MS", "777");
        }
        assert_eq!(default_watch_debounce(), Duration::from_millis(777));
        unsafe {
            std::env::remove_var("SQRY_LIMITS__WATCH__DEBOUNCE_MS");
        }
    }

    #[test]
    fn default_watch_debounce_falls_back_to_platform_default() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        unsafe {
            std::env::remove_var("SQRY_LIMITS__WATCH__DEBOUNCE_MS");
        }
        let expected = if cfg!(target_os = "macos") {
            Duration::from_millis(400)
        } else {
            Duration::from_millis(100)
        };
        assert_eq!(default_watch_debounce(), expected);
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
            no_build_tool: false,
        }
    }

    #[test]
    fn classpath_invalidation_includes_gradle_property_files() {
        let root = std::path::Path::new("/repo");
        let changes = [
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

    /// T4 (surface parity W4, design W4-D4): the per-iteration report
    /// carries the iteration's `BuildResult` figures and the deltas against
    /// the header the iteration replaced, exactly as `sqry update --stats`
    /// computes them; the mode column is hash-based.
    #[test]
    fn watch_iteration_stats_report_the_iterations_build_result() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("lib.rs"),
            "pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
        )
        .expect("write fixture");
        let plugins = sqry_plugin_registry::create_plugin_manager();
        let (_graph, build_result) =
            sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
                tmp.path(),
                &plugins,
                &sqry_core::graph::unified::build::BuildConfig::default(),
                "test:watch_stats",
                None,
                sqry_core::progress::no_op_reporter(),
            )
            .expect("fixture persists");
        let storage = GraphStorage::new(tmp.path());
        let header = load_header_from_path(storage.snapshot_path()).expect("header readable");
        assert!(build_result.node_count > 0, "fixture precondition: nodes");

        let report =
            watch_iteration_stats(&storage, &build_result, None, Duration::from_millis(1500));
        println!(
            "report: nodes {} edges {} raw {} files {} registered {:?}",
            report.nodes,
            report.canonical_edges,
            report.raw_edges,
            report.workspace_files_indexed,
            report.registered_files
        );
        assert_eq!(report.nodes, build_result.node_count);
        assert_eq!(report.canonical_edges, build_result.edge_count);
        assert_eq!(report.raw_edges, build_result.raw_edge_count);
        assert_eq!(report.workspace_files_indexed, build_result.total_files);
        assert_eq!(report.registered_files, Some(header.file_count));
        assert_eq!(report.nodes_delta, None, "no header to diff against");
        assert!(!report.using_git_mode, "the watch loop is hash-based");
        assert_eq!(report.threads_used, build_result.thread_count);
        assert_eq!(report.built_at, build_result.built_at);
        assert!((report.elapsed_seconds - 1.5).abs() < f64::EPSILON);

        // Against the header the iteration replaced, the deltas are zero
        // when nothing changed.
        let report = watch_iteration_stats(
            &storage,
            &build_result,
            Some(&header),
            Duration::from_millis(1500),
        );
        assert_eq!(report.nodes_delta, Some(0));
        assert_eq!(report.canonical_edges_delta, Some(0));
        assert_eq!(report.registered_files_delta, Some(0));
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
