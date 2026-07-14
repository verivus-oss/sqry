//! Index command implementation

use crate::args::Cli;
use crate::plugin_defaults::{self, PluginSelectionMode};
use crate::progress::{CliProgressReporter, CliStepProgressReporter, StepRunner};
use anyhow::{Context, Result};
use sqry_core::graph::unified::analysis::ReachabilityStrategy;
use sqry_core::graph::unified::build::BuildResult;
use sqry_core::graph::unified::build::entrypoint::{AnalysisStrategySummary, get_git_head_commit};
use sqry_core::graph::unified::persistence::{GraphStorage, load_header_from_path};
use sqry_core::json_response::IndexStatus;
use sqry_core::progress::{SharedReporter, no_op_reporter};
use std::fs;
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::Path;
#[cfg(feature = "jvm-classpath")]
use std::path::PathBuf;
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

#[cfg_attr(not(feature = "jvm-classpath"), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClasspathCliOptions<'a> {
    pub enabled: bool,
    pub depth: crate::args::ClasspathDepthArg,
    pub classpath_file: Option<&'a Path>,
    pub build_system: Option<&'a str>,
    pub force_classpath: bool,
}

#[cfg(feature = "jvm-classpath")]
pub(crate) fn run_classpath_pipeline_only(
    root_path: &Path,
    classpath_opts: &ClasspathCliOptions<'_>,
    json_output: bool,
) -> Result<Option<sqry_classpath::pipeline::ClasspathPipelineResult>> {
    use sqry_classpath::pipeline::{ClasspathConfig, ClasspathDepth};

    let depth = match classpath_opts.depth {
        crate::args::ClasspathDepthArg::Full => ClasspathDepth::Full,
        crate::args::ClasspathDepthArg::Shallow => ClasspathDepth::Shallow,
    };
    let config = ClasspathConfig {
        enabled: true,
        depth,
        build_system_override: classpath_opts.build_system.map(str::to_owned),
        classpath_file: classpath_opts.classpath_file.map(Path::to_path_buf),
        force: classpath_opts.force_classpath,
        timeout_secs: 60,
    };

    // Under `--json` this progress chatter must not pollute stdout (the caller
    // guarantees stdout is a single JSON document); route it to stderr.
    if json_output {
        eprintln!("Running JVM classpath analysis...");
    } else {
        println!("Running JVM classpath analysis...");
    }
    match sqry_classpath::pipeline::run_classpath_pipeline(root_path, &config) {
        Ok(result) => {
            if json_output {
                eprintln!(
                    "  Classpath: {} JARs scanned, {} classes parsed",
                    result.jars_scanned, result.classes_parsed
                );
            } else {
                println!(
                    "  Classpath: {} JARs scanned, {} classes parsed",
                    result.jars_scanned, result.classes_parsed
                );
            }
            Ok(Some(result))
        }
        Err(sqry_classpath::ClasspathError::DetectionFailed(message))
            if classpath_opts.build_system.is_none() && classpath_opts.classpath_file.is_none() =>
        {
            eprintln!(
                "WARNING: --classpath requested, but no JVM build system was detected; \
                 skipping classpath analysis. {message}"
            );
            Ok(None)
        }
        Err(error) => Err(error).context("Classpath pipeline failed"),
    }
}

#[cfg(feature = "jvm-classpath")]
struct ExistingClasspathImport {
    importer_id: sqry_core::graph::unified::node::NodeId,
    file_id: sqry_core::graph::unified::file::FileId,
    alias: Option<sqry_core::graph::unified::string::StringId>,
    is_wildcard: bool,
    import_name: Option<String>,
    importer_path: Option<PathBuf>,
}

#[cfg(feature = "jvm-classpath")]
fn create_workspace_classpath_import_edges(
    graph: &mut sqry_core::graph::unified::concurrent::CodeGraph,
    classpath_result: &sqry_classpath::pipeline::ClasspathPipelineResult,
    fqn_to_nodes: &std::collections::HashMap<
        String,
        Vec<sqry_classpath::graph::emitter::ClasspathNodeRef>,
    >,
) -> (usize, usize, usize, usize) {
    let package_index = build_classpath_package_index(classpath_result, fqn_to_nodes);
    let scoped_jars = build_scope_jar_sets(&classpath_result.provenance);
    let provenance_lookup = build_provenance_lookup(&classpath_result.provenance);
    let existing_imports = collect_existing_classpath_imports(graph);

    let mut created_edges = 0usize;
    let mut skipped_member_imports = 0usize;
    let mut skipped_unscoped_imports = 0usize;
    let mut skipped_ambiguous_imports = 0usize;

    for import in existing_imports {
        let Some(import_name) = import.import_name else {
            continue;
        };
        if import_name.starts_with("static ") {
            skipped_member_imports += 1;
            continue;
        }

        let Some(resolved) = resolve_allowed_jars(import.importer_path.as_deref(), &scoped_jars)
        else {
            skipped_unscoped_imports += 1;
            continue;
        };

        if import.is_wildcard || import_name.ends_with(".*") || import_name.ends_with("._") {
            let package_name = import_name
                .strip_suffix(".*")
                .or_else(|| import_name.strip_suffix("._"))
                .unwrap_or(import_name.as_str());
            if let Some(targets) = package_index.get(package_name) {
                let filtered_targets =
                    filter_scope_targets(targets.clone(), &resolved.allowed_jars);
                let grouped_targets = group_targets_by_fqn(filtered_targets);
                for target_group in grouped_targets.into_values() {
                    let reduced = prefer_direct_targets(
                        target_group,
                        resolved.matched_root.as_deref(),
                        &provenance_lookup,
                    );
                    if reduced.len() > 1 {
                        skipped_ambiguous_imports += 1;
                        continue;
                    }
                    add_classpath_import_edge(
                        graph,
                        import.importer_id,
                        reduced[0].node_id,
                        import.alias,
                        import.is_wildcard,
                        import.file_id,
                        &mut created_edges,
                    );
                }
            }
            continue;
        }

        if let Some(targets) = fqn_to_nodes.get(import_name.as_str()) {
            let filtered_targets =
                filter_scope_targets(targets.iter().collect(), &resolved.allowed_jars);
            let reduced = prefer_direct_targets(
                filtered_targets,
                resolved.matched_root.as_deref(),
                &provenance_lookup,
            );
            if reduced.len() > 1 {
                skipped_ambiguous_imports += 1;
                continue;
            }
            if let Some(target_ref) = reduced.first() {
                add_classpath_import_edge(
                    graph,
                    import.importer_id,
                    target_ref.node_id,
                    import.alias,
                    import.is_wildcard,
                    import.file_id,
                    &mut created_edges,
                );
            }
        }
    }

    (
        created_edges,
        skipped_member_imports,
        skipped_unscoped_imports,
        skipped_ambiguous_imports,
    )
}

#[cfg(feature = "jvm-classpath")]
fn add_classpath_import_edge(
    graph: &mut sqry_core::graph::unified::concurrent::CodeGraph,
    importer_id: sqry_core::graph::unified::node::NodeId,
    target_id: sqry_core::graph::unified::node::NodeId,
    alias: Option<sqry_core::graph::unified::string::StringId>,
    is_wildcard: bool,
    file_id: sqry_core::graph::unified::file::FileId,
    created_edges: &mut usize,
) {
    use sqry_core::graph::unified::edge::EdgeKind;

    let _delta = graph.edges().add_edge(
        importer_id,
        target_id,
        EdgeKind::Imports { alias, is_wildcard },
        file_id,
    );
    *created_edges += 1;
}

#[cfg(feature = "jvm-classpath")]
fn collect_existing_classpath_imports(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
) -> Vec<ExistingClasspathImport> {
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;

    let mut existing_imports = Vec::new();
    for (source_id, source_entry) in graph.nodes().iter() {
        if source_entry.is_unified_loser() {
            continue;
        }
        for edge in graph.edges().edges_from(source_id) {
            let EdgeKind::Imports { alias, is_wildcard } = edge.kind.clone() else {
                continue;
            };
            let Some(import_entry) = graph.nodes().get(edge.target) else {
                continue;
            };
            if import_entry.kind != NodeKind::Import || graph.files().is_external(import_entry.file)
            {
                continue;
            }
            let importer_path = graph
                .files()
                .resolve(edge.file)
                .map(|path| canonicalish_path(path.as_ref()));
            let import_name = import_entry
                .qualified_name
                .and_then(|id| graph.strings().resolve(id))
                .or_else(|| graph.strings().resolve(import_entry.name))
                .map(|value| value.to_string());
            existing_imports.push(ExistingClasspathImport {
                importer_id: source_id,
                file_id: edge.file,
                alias,
                is_wildcard,
                import_name,
                importer_path,
            });
        }
    }
    existing_imports
}

#[cfg(feature = "jvm-classpath")]
fn build_classpath_package_index<'a>(
    classpath_result: &'a sqry_classpath::pipeline::ClasspathPipelineResult,
    fqn_to_nodes: &'a std::collections::HashMap<
        String,
        Vec<sqry_classpath::graph::emitter::ClasspathNodeRef>,
    >,
) -> std::collections::HashMap<String, Vec<&'a sqry_classpath::graph::emitter::ClasspathNodeRef>> {
    let class_fqns: std::collections::HashSet<&str> = classpath_result
        .index
        .classes
        .iter()
        .map(|class_stub| class_stub.fqn.as_str())
        .collect();
    let mut package_index: std::collections::HashMap<
        String,
        Vec<&sqry_classpath::graph::emitter::ClasspathNodeRef>,
    > = std::collections::HashMap::new();
    for fqn in class_fqns {
        if let Some(node_refs) = fqn_to_nodes.get(fqn)
            && let Some((package_name, _)) = fqn.rsplit_once('.')
        {
            package_index
                .entry(package_name.to_owned())
                .or_default()
                .extend(node_refs.iter());
        }
    }
    package_index
}

#[cfg(feature = "jvm-classpath")]
pub(crate) fn inject_classpath_into_graph(
    graph: &mut sqry_core::graph::unified::concurrent::CodeGraph,
    classpath_result: &sqry_classpath::pipeline::ClasspathPipelineResult,
    json_output: bool,
) -> Result<()> {
    let emission_result = sqry_classpath::graph::emitter::emit_into_code_graph(
        &classpath_result.index,
        graph,
        &classpath_result.provenance,
    )
    .map_err(|e| anyhow::anyhow!("Classpath emission error: {e}"))?;

    let (
        import_edges_created,
        skipped_member_imports,
        skipped_unscoped_imports,
        skipped_ambiguous_imports,
    ) = create_workspace_classpath_import_edges(
        graph,
        classpath_result,
        &emission_result.fqn_to_nodes,
    );

    graph.rebuild_indices();
    // Under `--json` this enrichment summary goes to stderr so stdout stays a
    // single JSON document.
    if json_output {
        eprintln!(
            "  Graph enriched with {} classpath types, {} import edges ({} member/static, {} unscoped, {} ambiguous imports skipped)",
            classpath_result.index.classes.len(),
            import_edges_created,
            skipped_member_imports,
            skipped_unscoped_imports,
            skipped_ambiguous_imports,
        );
    } else {
        println!(
            "  Graph enriched with {} classpath types, {} import edges ({} member/static, {} unscoped, {} ambiguous imports skipped)",
            classpath_result.index.classes.len(),
            import_edges_created,
            skipped_member_imports,
            skipped_unscoped_imports,
            skipped_ambiguous_imports,
        );
    }
    Ok(())
}

#[cfg(feature = "jvm-classpath")]
fn build_scope_jar_sets(
    provenance: &[sqry_classpath::graph::provenance::ClasspathProvenance],
) -> Vec<(PathBuf, std::collections::HashSet<PathBuf>)> {
    let mut by_root: std::collections::HashMap<PathBuf, std::collections::HashSet<PathBuf>> =
        std::collections::HashMap::new();
    for entry in provenance {
        for scope in &entry.scopes {
            by_root
                .entry(canonicalish_path(&scope.module_root))
                .or_default()
                .insert(entry.jar_path.clone());
        }
    }

    let mut scopes: Vec<_> = by_root.into_iter().collect();
    scopes.sort_by(|a, b| {
        b.0.components()
            .count()
            .cmp(&a.0.components().count())
            .then_with(|| a.0.cmp(&b.0))
    });
    scopes
}

/// Result of scope resolution for an importer path.
#[cfg(feature = "jvm-classpath")]
struct ResolvedScope {
    allowed_jars: std::collections::HashSet<PathBuf>,
    matched_root: Option<PathBuf>,
}

#[cfg(feature = "jvm-classpath")]
fn resolve_allowed_jars(
    importer_path: Option<&Path>,
    scopes: &[(PathBuf, std::collections::HashSet<PathBuf>)],
) -> Option<ResolvedScope> {
    let importer_path = importer_path?;
    for (root, jars) in scopes {
        if importer_path.starts_with(root) {
            return Some(ResolvedScope {
                allowed_jars: jars.clone(),
                matched_root: Some(root.clone()),
            });
        }
    }
    if scopes.len() == 1 {
        return Some(ResolvedScope {
            allowed_jars: scopes[0].1.clone(),
            matched_root: Some(scopes[0].0.clone()),
        });
    }
    None
}

/// Builds a lookup from JAR path to its provenance entry for O(1) directness
/// checks during import resolution.
#[cfg(feature = "jvm-classpath")]
fn build_provenance_lookup(
    provenance: &[sqry_classpath::graph::provenance::ClasspathProvenance],
) -> std::collections::HashMap<PathBuf, &sqry_classpath::graph::provenance::ClasspathProvenance> {
    provenance
        .iter()
        .map(|entry| (entry.jar_path.clone(), entry))
        .collect()
}

/// Reduces candidates by preferring direct dependencies over transitive ones
/// within the matched scope. Returns the full set unchanged if all candidates
/// share the same directness or if no provenance/scope information is available.
#[cfg(feature = "jvm-classpath")]
fn prefer_direct_targets<'a>(
    targets: Vec<&'a sqry_classpath::graph::emitter::ClasspathNodeRef>,
    matched_root: Option<&Path>,
    provenance_lookup: &std::collections::HashMap<
        PathBuf,
        &sqry_classpath::graph::provenance::ClasspathProvenance,
    >,
) -> Vec<&'a sqry_classpath::graph::emitter::ClasspathNodeRef> {
    if targets.len() <= 1 {
        return targets;
    }

    let Some(root) = matched_root else {
        return targets;
    };

    let direct: Vec<_> = targets
        .iter()
        .copied()
        .filter(|target| {
            provenance_lookup.get(&target.jar_path).is_some_and(|prov| {
                prov.scopes
                    .iter()
                    .any(|scope| scope.module_root == root && scope.is_direct)
            })
        })
        .collect();

    if direct.is_empty() || direct.len() == targets.len() {
        // No differentiation possible — return the original set
        targets
    } else {
        direct
    }
}

#[cfg(feature = "jvm-classpath")]
fn filter_scope_targets<'a>(
    targets: Vec<&'a sqry_classpath::graph::emitter::ClasspathNodeRef>,
    allowed_jars: &std::collections::HashSet<PathBuf>,
) -> Vec<&'a sqry_classpath::graph::emitter::ClasspathNodeRef> {
    targets
        .into_iter()
        .filter(|target| allowed_jars.contains(&target.jar_path))
        .collect()
}

#[cfg(feature = "jvm-classpath")]
fn group_targets_by_fqn(
    targets: Vec<&sqry_classpath::graph::emitter::ClasspathNodeRef>,
) -> std::collections::HashMap<String, Vec<&sqry_classpath::graph::emitter::ClasspathNodeRef>> {
    let mut grouped = std::collections::HashMap::new();
    for target in targets {
        grouped
            .entry(target.fqn.clone())
            .or_insert_with(Vec::new)
            .push(target);
    }
    grouped
}

#[cfg(feature = "jvm-classpath")]
fn canonicalish_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[allow(unused_variables, unused_mut, clippy::too_many_arguments)]
pub(crate) fn build_and_persist_with_optional_classpath(
    root_path: &Path,
    resolved_plugins: &plugin_defaults::ResolvedPluginManager,
    build_config: &sqry_core::graph::unified::build::BuildConfig,
    build_command: &str,
    progress: SharedReporter,
    classpath_opts: Option<&ClasspathCliOptions<'_>>,
    cache_dir: Option<&Path>,
    json_output: bool,
) -> Result<BuildResult> {
    #[cfg(feature = "jvm-classpath")]
    let classpath_result = if let Some(classpath_opts) = classpath_opts.filter(|opts| opts.enabled)
    {
        run_classpath_pipeline_only(root_path, classpath_opts, json_output)?
    } else {
        None
    };

    #[cfg(not(feature = "jvm-classpath"))]
    {
        // `json_output` only gates the classpath progress lines, which are
        // compiled out without the `jvm-classpath` feature.
        let _ = json_output;
        if classpath_opts.is_some_and(|opts| opts.enabled) {
            eprintln!(
                "WARNING: --classpath flag requires the 'jvm-classpath' feature. \
                 Rebuild sqry-cli with: cargo build --features jvm-classpath"
            );
        }
    }

    let (mut graph, effective_threads) =
        sqry_core::graph::unified::build::build_unified_graph_with_progress(
            root_path,
            &resolved_plugins.plugin_manager,
            build_config,
            progress.clone(),
        )?;

    #[cfg(feature = "jvm-classpath")]
    if let Some(classpath_result) = &classpath_result {
        inject_classpath_into_graph(&mut graph, classpath_result, json_output)?;
    }

    // C001b-core: when `--cache-dir <DIR>` is supplied, persist a hash-index
    // snapshot covering every file the freshly built graph references. The
    // graph build itself is NOT short-circuited (the unified builder does not
    // yet support merging cached file segments), so the new graph is always
    // complete; this side-channel snapshot is what `sqry update` and the
    // forthcoming incremental-merge API will pick up next time.
    if let Some(dir) = cache_dir
        && let Err(err) = persist_hash_index_snapshot(&graph, dir)
    {
        log::warn!(
            "failed to persist hash index to {} ({err}); cache snapshot skipped",
            dir.display()
        );
    }

    let (_graph, build_result) =
        sqry_core::graph::unified::build::persist_durable_graph_transaction(
            graph,
            sqry_core::graph::unified::build::DurableGraphPersistenceRequest {
                root: root_path,
                plugins: &resolved_plugins.plugin_manager,
                config: build_config,
                build_command,
                plugin_selection: resolved_plugins.persisted_selection.clone(),
                progress,
                effective_threads,
            },
        )?;

    Ok(build_result)
}

/// Persist a fresh `HashIndex` capturing every parsed file from `graph` to
/// `cache_dir`. Read-side load + short-circuit is deferred (audit row
/// C001b-core); this is the save half of the pair.
fn persist_hash_index_snapshot(
    graph: &sqry_core::graph::unified::CodeGraph,
    cache_dir: &Path,
) -> Result<()> {
    use sqry_core::indexing::incremental::{FileHash, HashIndex};

    let mut index = HashIndex::new();
    let mut hashed = 0usize;
    let mut skipped = 0usize;
    for (_file_id, path) in graph.files().iter() {
        let path_ref: &Path = path.as_ref();
        match FileHash::compute(path_ref) {
            Ok(hash) => {
                index.update(path_ref.to_path_buf(), hash);
                hashed += 1;
            }
            Err(err) => {
                log::trace!(
                    "skipping hash for {} during cache snapshot: {err}",
                    path_ref.display()
                );
                skipped += 1;
            }
        }
    }
    index.save(cache_dir)?;
    log::debug!(
        "Persisted hash index snapshot to {}: {hashed} files hashed, {skipped} skipped",
        cache_dir.display()
    );
    Ok(())
}

/// Convert an [`IndexStatus`] snapshot into Prometheus / `OpenMetrics`-shaped
/// text so `sqry index --status --metrics-format prometheus` emits a payload
/// that monitoring systems can scrape.
///
/// The original (pre-v2.0) implementation operated against
/// `sqry_core::symbols::ValidationReport`, which the unified-graph migration
/// removed; this restoration projects the equivalent gauges from the surviving
/// [`IndexStatus`] surface (existence, freshness, node/file/relation counts).
fn format_validation_prometheus(status: &IndexStatus) -> String {
    use std::fmt::Write as _;

    let mut output = String::new();

    output.push_str("# HELP sqry_index_exists Whether the unified graph index exists on disk\n");
    output.push_str("# TYPE sqry_index_exists gauge\n");
    let _ = writeln!(output, "sqry_index_exists {}", u8::from(status.exists));

    output.push_str("# HELP sqry_index_supports_fuzzy Whether fuzzy search is enabled\n");
    output.push_str("# TYPE sqry_index_supports_fuzzy gauge\n");
    let _ = writeln!(
        output,
        "sqry_index_supports_fuzzy {}",
        u8::from(status.supports_fuzzy)
    );

    output
        .push_str("# HELP sqry_index_supports_relations Whether relation queries are supported\n");
    output.push_str("# TYPE sqry_index_supports_relations gauge\n");
    let _ = writeln!(
        output,
        "sqry_index_supports_relations {}",
        u8::from(status.supports_relations)
    );

    if let Some(symbols) = status.symbol_count {
        output.push_str("# HELP sqry_index_symbol_count Total number of indexed symbols\n");
        output.push_str("# TYPE sqry_index_symbol_count gauge\n");
        let _ = writeln!(output, "sqry_index_symbol_count {symbols}");
    }

    if let Some(files) = status.file_count {
        output.push_str("# HELP sqry_index_file_count Total number of indexed source files\n");
        output.push_str("# TYPE sqry_index_file_count gauge\n");
        let _ = writeln!(output, "sqry_index_file_count {files}");
    }

    if let Some(age) = status.age_seconds {
        output.push_str("# HELP sqry_index_age_seconds Index age in seconds since creation\n");
        output.push_str("# TYPE sqry_index_age_seconds gauge\n");
        let _ = writeln!(output, "sqry_index_age_seconds {age}");
    }

    if let Some(stale) = status.stale {
        output.push_str("# HELP sqry_index_stale Whether the index is considered stale\n");
        output.push_str("# TYPE sqry_index_stale gauge\n");
        let _ = writeln!(output, "sqry_index_stale {}", u8::from(stale));
    }

    if let Some(relations) = status.cross_language_relation_count {
        output.push_str(
            "# HELP sqry_index_cross_language_relation_count Total cross-language relations\n",
        );
        output.push_str("# TYPE sqry_index_cross_language_relation_count gauge\n");
        let _ = writeln!(
            output,
            "sqry_index_cross_language_relation_count {relations}"
        );
    }

    output
}

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
#[allow(clippy::fn_params_excessive_bools)] // CLI flags map directly to booleans.
#[allow(clippy::too_many_arguments)]
/// Build a fresh index for the given path.
///
/// `STEP_8` precedence: callers must resolve `path` via
/// [`crate::args::Cli::resolve_subcommand_path`] so that an explicit positional
/// `<path>` always wins over the global `--workspace` / `SQRY_WORKSPACE_FILE`
/// flag. This function trusts the caller to have applied that precedence.
pub fn run_index(
    cli: &Cli,
    path: &str,
    force: bool,
    threads: Option<usize>,
    add_to_gitignore: bool,
    no_incremental: bool,
    cache_dir: Option<&str>,
    classpath: bool,
    _no_classpath: bool,
    classpath_depth: crate::args::ClasspathDepthArg,
    classpath_file: Option<&Path>,
    build_system: Option<&str>,
    force_classpath: bool,
    allow_nested: bool,
    cfg_flags: &[String],
    expand_cache: Option<&Path>,
) -> Result<()> {
    if let Some(0) = threads {
        anyhow::bail!("--threads must be >= 1");
    }

    let root_path = Path::new(path);

    handle_gitignore(root_path, add_to_gitignore);

    // Check if graph already exists
    let storage = GraphStorage::new(root_path);
    // C001a: `--no-incremental` forces a full rebuild even when a snapshot
    // exists, so the early-exit gate honours it alongside `--force`.
    if storage.exists() && !force && !no_incremental {
        println!("Index already exists at {}", storage.graph_dir().display());
        println!("Use --force to rebuild, or run 'sqry update' to update incrementally");
        return Ok(());
    }

    // Cluster-E §E.3 — refuse to create a nested `.sqry/` inside an outer
    // project that already has its own graph. The guard fires only on
    // *fresh* creation: existing graphs at `root_path` (handled by the
    // exit-gate above) and `--allow-nested` opt-in are exempt.
    if !storage.exists()
        && let Err(e) = sqry_core::workspace::assert_no_ancestor_graph(root_path, allow_nested)
    {
        anyhow::bail!("{e}");
    }

    print_index_build_banner(root_path, threads);

    let start = Instant::now();
    let mut step_runner = StepRunner::new(!std::io::stderr().is_terminal() && !cli.json);

    let (progress_bar, progress) = create_progress_reporter(cli);

    // Build unified graph using the consolidated pipeline
    let build_config = create_build_config(cli, root_path, threads, cfg_flags, expand_cache)?;
    let resolved_plugins =
        plugin_defaults::resolve_plugin_selection(cli, root_path, PluginSelectionMode::FreshWrite)?;
    let classpath_opts = ClasspathCliOptions {
        enabled: classpath,
        depth: classpath_depth,
        classpath_file,
        build_system,
        force_classpath,
    };
    // C001b: surface `--cache-dir` to the build pipeline so the post-parse
    // hash-index snapshot lands in the operator-supplied directory.
    let cache_dir_path = cache_dir.map(Path::new);
    let build_result = step_runner.step("Build unified graph", || -> Result<_> {
        build_and_persist_with_optional_classpath(
            root_path,
            &resolved_plugins,
            &build_config,
            "cli:index",
            progress.clone(),
            Some(&classpath_opts),
            cache_dir_path,
            cli.json,
        )
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
    if !build_result.active_plugin_ids.is_empty() {
        println!(
            "  Active plugins: {}",
            build_result.active_plugin_ids.join(", ")
        );
    }
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
    let snapshot_exists = storage.snapshot_path().exists();
    let manifest_exists = storage.manifest_path().exists();

    match (snapshot_exists, manifest_exists) {
        (false, false) => return Ok(IndexStatus::not_found()),
        (true, false) => {
            // Cluster-G §4.3 — daemon-built snapshot. The daemon's
            // `QueryDbHook` writes `snapshot.sqry` without a manifest,
            // so `storage.exists()` (which checks the manifest) would
            // mis-report "no graph snapshot found". Read the
            // node/edge counts from the snapshot header instead.
            let header = match load_header_from_path(storage.snapshot_path()) {
                Ok(h) => h,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "snapshot present at {} but unreadable: {e}",
                        storage.snapshot_path().display()
                    ));
                }
            };
            // The snapshot header carries the file count but no
            // built_at timestamp; surface "unknown" so downstream
            // renderers can decide what to show.
            return Ok(IndexStatus::from_index(
                storage.graph_dir().display().to_string(),
                "unknown (daemon-built; no manifest)".to_string(),
                0,
            )
            .symbol_count(header.node_count)
            .file_count_opt(Some(header.file_count))
            .has_relations(header.edge_count > 0)
            .has_trigram(false)
            .build());
        }
        (false, true) => {
            // Manifest without a snapshot — corrupt half-built state.
            return Err(anyhow::anyhow!(
                "manifest at {} but no snapshot at {}; rebuild with `sqry index --force`",
                storage.manifest_path().display(),
                storage.snapshot_path().display()
            ));
        }
        (true, true) => {
            // Existing path: both files present, fall through.
        }
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
    cfg_flags: &[String],
    expand_cache: Option<&Path>,
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
        // Phase 1a/1b: thread `--cfg` predicate strings and the `--expand-cache`
        // directory into the Rust plugin's macro-boundary analysis. Empty/None
        // (the `sqry update` path) leaves today's behaviour unchanged.
        macro_options: sqry_core::graph::unified::build::MacroBuildOptions {
            cfg_flags: cfg_flags.to_vec(),
            expand_cache_dir: expand_cache.map(std::path::Path::to_path_buf),
        },
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
#[allow(clippy::too_many_arguments)]
#[allow(clippy::fn_params_excessive_bools)] // CLI flags map directly to booleans.
pub fn run_update(
    cli: &Cli,
    path: &str,
    threads: Option<usize>,
    show_stats: bool,
    _no_incremental: bool,
    cache_dir: Option<&str>,
    classpath: bool,
    _no_classpath: bool,
    classpath_depth: crate::args::ClasspathDepthArg,
    classpath_file: Option<&Path>,
    build_system: Option<&str>,
    force_classpath: bool,
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

    // Capture the pre-update graph counts so `--stats` can report real deltas
    // (nodes / edges / files added or removed by this update). Read straight
    // from the existing snapshot header; a soft failure just means the delta
    // columns are omitted, never that the update fails.
    let pre_update_header = if show_stats {
        load_header_from_path(storage.snapshot_path()).ok()
    } else {
        None
    };

    // Under `--json`, stdout must carry ONLY the machine-readable document, so
    // route the human-readable progress line to stderr (the `--stats --json`
    // output has to deserialize as a single JSON document with no preamble).
    if cli.json {
        eprintln!("Updating index for {}...", root_path.display());
    } else {
        println!("Updating index for {}...", root_path.display());
    }
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

    // Update graph using consolidated pipeline. Phase 1c: `sqry update` does not
    // yet carry `--cfg` / `--expand-cache`, so it rebuilds with defaults.
    let build_config = create_build_config(cli, root_path, threads, &[], None)?;
    let resolved_plugins = plugin_defaults::resolve_plugin_selection(
        cli,
        root_path,
        PluginSelectionMode::ExistingWrite,
    )?;
    let classpath_opts = ClasspathCliOptions {
        enabled: classpath,
        depth: classpath_depth,
        classpath_file,
        build_system,
        force_classpath,
    };
    let cache_dir_path = cache_dir.map(Path::new);
    let build_result = step_runner.step("Update unified graph", || -> Result<_> {
        build_and_persist_with_optional_classpath(
            root_path,
            &resolved_plugins,
            &build_config,
            "cli:update",
            progress.clone(),
            Some(&classpath_opts),
            cache_dir_path,
            cli.json,
        )
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
        // Load the freshly-written snapshot header so the file delta compares
        // like against like. `GraphHeader.file_count` counts REGISTERED files
        // (including external / dependency files) on BOTH sides, whereas
        // `BuildResult.total_files` counts only workspace (non-external) files;
        // mixing the two produced a wrong file delta on classpath / external
        // workspaces (#520 review). Header-to-header keeps the delta honest.
        let post_update_header = load_header_from_path(storage.snapshot_path()).ok();
        emit_update_stats(
            cli,
            &build_result,
            pre_update_header.as_ref(),
            post_update_header.as_ref(),
            elapsed,
            using_git_mode,
        );
    }

    Ok(())
}

/// Signed delta between a post-update count and its pre-update counterpart.
///
/// `None` means one side of the comparison was unavailable (no readable
/// pre-update snapshot header, or the post-update header could not be re-read),
/// so we cannot attribute a delta to this update. Rendered as `"n/a"`.
fn count_delta(new: Option<usize>, old: Option<usize>) -> Option<i64> {
    match (new, old) {
        (Some(new), Some(old)) => Some(new as i64 - old as i64),
        _ => None,
    }
}

/// Render a signed delta for text output: `+5`, `-3`, `0`, or `n/a`.
fn fmt_delta(delta: Option<i64>) -> String {
    match delta {
        Some(d) => format!("{d:+}"),
        None => "n/a".to_string(),
    }
}

/// Computed `sqry update --stats` figures.
///
/// Kept separate from rendering so the delta semantics (in particular the
/// apples-to-apples file delta) are unit-testable without spawning a process.
///
/// The critical distinction the #520 review flagged:
///
/// - `registered_files` / `registered_files_delta` come from the graph header
///   on BOTH sides (`GraphHeader.file_count`), which counts every registered
///   file INCLUDING external / dependency files. Because both sides use the
///   same definition, the delta is honest on classpath / external workspaces.
/// - `workspace_files_indexed` (and the per-language breakdown) come from
///   `BuildResult.total_files`, which counts only workspace (non-external)
///   files. It is reported as an absolute; it deliberately carries NO delta,
///   because a correct workspace-only pre-update count is not available from
///   the header (the header only stores the registered total).
struct UpdateStatsReport {
    using_git_mode: bool,
    nodes: usize,
    nodes_delta: Option<i64>,
    canonical_edges: usize,
    canonical_edges_delta: Option<i64>,
    raw_edges: usize,
    workspace_files_indexed: usize,
    registered_files: Option<usize>,
    registered_files_delta: Option<i64>,
    /// Workspace (non-external) files per language, sorted count-desc then name.
    files_by_language: Vec<(String, usize)>,
    threads_used: usize,
    active_plugins: Vec<String>,
    built_at: String,
    elapsed_seconds: f64,
}

/// Derive the `--stats` figures from the build result and the pre/post snapshot
/// headers. See [`UpdateStatsReport`] for the file-count semantics.
fn compute_update_stats(
    build_result: &BuildResult,
    pre_update_header: Option<&sqry_core::graph::unified::persistence::GraphHeader>,
    post_update_header: Option<&sqry_core::graph::unified::persistence::GraphHeader>,
    elapsed: std::time::Duration,
    using_git_mode: bool,
) -> UpdateStatsReport {
    let mut files_by_language: Vec<(String, usize)> = build_result
        .file_count
        .iter()
        .map(|(lang, count)| (lang.clone(), *count))
        .collect();
    files_by_language.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let registered_files = post_update_header.map(|h| h.file_count);

    UpdateStatsReport {
        using_git_mode,
        nodes: build_result.node_count,
        nodes_delta: count_delta(
            Some(build_result.node_count),
            pre_update_header.map(|h| h.node_count),
        ),
        canonical_edges: build_result.edge_count,
        canonical_edges_delta: count_delta(
            Some(build_result.edge_count),
            pre_update_header.map(|h| h.edge_count),
        ),
        raw_edges: build_result.raw_edge_count,
        workspace_files_indexed: build_result.total_files,
        registered_files,
        // Header-to-header: registered files on both sides, so the delta is
        // apples-to-apples even when external files are present.
        registered_files_delta: count_delta(
            registered_files,
            pre_update_header.map(|h| h.file_count),
        ),
        files_by_language,
        threads_used: build_result.thread_count,
        active_plugins: build_result.active_plugin_ids.clone(),
        built_at: build_result.built_at.clone(),
        elapsed_seconds: elapsed.as_secs_f64(),
    }
}

/// Emit real `sqry update --stats` statistics.
///
/// The update path rebuilds the whole unified graph and persists it, so the
/// [`BuildResult`] carries accurate absolute counts (nodes, canonical + raw
/// edges, workspace per-language file breakdown, thread count, active plugins),
/// and the freshly-written header carries the registered-file total. Node,
/// canonical-edge, and registered-file deltas are reported against the
/// pre-update header. Honours `--json`: emits a single structured object for
/// programmatic consumers (stdout stays a single JSON document), otherwise a
/// human-readable block.
fn emit_update_stats(
    cli: &Cli,
    build_result: &BuildResult,
    pre_update_header: Option<&sqry_core::graph::unified::persistence::GraphHeader>,
    post_update_header: Option<&sqry_core::graph::unified::persistence::GraphHeader>,
    elapsed: std::time::Duration,
    using_git_mode: bool,
) {
    let report = compute_update_stats(
        build_result,
        pre_update_header,
        post_update_header,
        elapsed,
        using_git_mode,
    );

    if cli.json {
        let files_by_language: serde_json::Map<String, serde_json::Value> = report
            .files_by_language
            .iter()
            .map(|(lang, count)| (lang.clone(), serde_json::json!(*count)))
            .collect();

        let payload = serde_json::json!({
            "update_stats": {
                "mode": if report.using_git_mode { "git" } else { "hash" },
                "nodes": report.nodes,
                "nodes_delta": report.nodes_delta,
                "canonical_edges": report.canonical_edges,
                "canonical_edges_delta": report.canonical_edges_delta,
                "raw_edges": report.raw_edges,
                "workspace_files_indexed": report.workspace_files_indexed,
                "registered_files": report.registered_files,
                "registered_files_delta": report.registered_files_delta,
                "languages": report.files_by_language.len(),
                "files_by_language": files_by_language,
                "threads_used": report.threads_used,
                "active_plugins": report.active_plugins,
                "built_at": report.built_at,
                "elapsed_seconds": report.elapsed_seconds,
            }
        });
        match serde_json::to_string_pretty(&payload) {
            Ok(rendered) => println!("{rendered}"),
            Err(err) => eprintln!("failed to render update stats as JSON: {err}"),
        }
        return;
    }

    println!("\nUpdate statistics:");
    println!(
        "  Mode:              {}",
        if report.using_git_mode {
            "git-aware"
        } else {
            "hash-based"
        }
    );
    println!(
        "  Nodes:             {} ({} since last index)",
        report.nodes,
        fmt_delta(report.nodes_delta)
    );
    println!(
        "  Canonical edges:   {} ({} since last index)",
        report.canonical_edges,
        fmt_delta(report.canonical_edges_delta)
    );
    println!("  Raw edges:         {}", report.raw_edges);
    match report.registered_files {
        Some(registered) => println!(
            "  Registered files:  {} ({} since last index) (includes external/dependency files)",
            registered,
            fmt_delta(report.registered_files_delta)
        ),
        None => println!("  Registered files:  n/a (header unavailable)"),
    }
    println!(
        "  Workspace files:   {} (non-external, indexed this build)",
        report.workspace_files_indexed
    );
    println!("  Languages:         {}", report.files_by_language.len());

    // Full per-language workspace file breakdown (the summary line only shows
    // the top few).
    if !report.files_by_language.is_empty() {
        println!("  Files by language (workspace):");
        for (lang, count) in &report.files_by_language {
            println!("    {lang}: {count}");
        }
    }

    println!("  Threads used:      {}", report.threads_used);
    if !report.active_plugins.is_empty() {
        println!("  Active plugins:    {}", report.active_plugins.join(", "));
    }
    println!("  Built at:          {}", report.built_at);
    println!("  Elapsed:           {:.2}s", report.elapsed_seconds);
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
    metrics_format: crate::args::MetricsFormat,
) -> Result<()> {
    use crate::args::MetricsFormat;

    // Prometheus output bypasses the JSON / text branch and emits an
    // OpenMetrics-shaped scrape payload built from the current IndexStatus.
    if matches!(metrics_format, MetricsFormat::Prometheus) {
        let root_path = Path::new(path);
        let storage = GraphStorage::new(root_path);
        let status = build_graph_status(&storage)?;
        let mut streams = crate::output::OutputStreams::with_pager(cli.pager_config());
        let body = format_validation_prometheus(&status);
        streams.write_result(&body)?;
        return streams.finish_checked();
    }

    // JSON / text path: defer to the unified graph status renderer.
    // Programmatic consumers of `run_index_status` only express JSON intent
    // through `cli.json`, so do not synthesize an extra override here.
    run_graph_status_with_format(cli, path, false)
}

/// Run graph status command using unified graph architecture.
///
/// This command reports on the state of the unified graph snapshot stored in
/// the `.sqry/graph/` directory instead of the legacy `.sqry-index`.
///
/// `json_from_format` carries the threaded `--format` decision computed by
/// `resolve_graph_format` at the `Command::Graph` boundary, so that
/// `sqry graph --format json status` honors the alias contract for the
/// global `--json` flag and the per-subcommand `--json` flag (verivus-oss/sqry#79
/// / verivus-oss/sqry#158). The non-`--format` paths continue to flow
/// through `cli.json`.
///
/// # Errors
///
/// Returns an error if manifest cannot be loaded or output formatting fails.
pub fn run_graph_status_with_format(cli: &Cli, path: &str, json_from_format: bool) -> Result<()> {
    let root_path = Path::new(path);
    let storage = GraphStorage::new(root_path);
    let status = build_graph_status(&storage)?;

    // Output result (same format as run_index_status for compatibility)
    let mut streams = crate::output::OutputStreams::with_pager(cli.pager_config());

    let json_out = cli.json || json_from_format;
    if json_out {
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
    use sqry_core::graph::unified::persistence::GraphHeader;
    use std::collections::HashMap;
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Build a synthetic `BuildResult` with the given workspace (non-external)
    /// file total so the delta semantics can be unit-tested in isolation.
    fn build_result_with(
        node_count: usize,
        edge_count: usize,
        workspace_files: usize,
    ) -> BuildResult {
        let mut file_count = HashMap::new();
        file_count.insert("rust".to_string(), workspace_files);
        BuildResult {
            node_count,
            edge_count,
            raw_edge_count: edge_count,
            file_count,
            total_files: workspace_files,
            built_at: "2026-07-06T00:00:00+00:00".to_string(),
            root_path: "/tmp/ws".to_string(),
            thread_count: 8,
            active_plugin_ids: vec!["rust".to_string()],
            analysis_strategies: Vec::new(),
        }
    }

    /// #520 review BLOCKER 1: the file delta must compare like against like.
    /// A pre-update header registers 10 files (3 workspace + 7 external) and the
    /// post-update header registers 13 (6 workspace + 7 external). The build
    /// only parsed 6 workspace files. The reported delta must be the
    /// header-to-header registered delta (+3), NOT the old bug that subtracted
    /// the workspace-only build total from the registered pre-count (6 - 10 =
    /// -4), and the workspace absolute must be reported separately with no
    /// delta contamination.
    #[test]
    fn update_stats_file_delta_is_registered_apples_to_apples() {
        let build = build_result_with(24, 17, 6);
        // strings arg is irrelevant to the delta; file_count is the last arg.
        let pre = GraphHeader::new(20, 15, 100, 10);
        let post = GraphHeader::new(24, 17, 110, 13);

        let report = compute_update_stats(
            &build,
            Some(&pre),
            Some(&post),
            Duration::from_millis(10),
            false,
        );

        assert_eq!(report.registered_files, Some(13));
        assert_eq!(
            report.registered_files_delta,
            Some(3),
            "registered delta must be header-to-header (13 - 10)"
        );
        // The old apples-to-oranges bug would have produced 6 - 10 = -4.
        assert_ne!(report.registered_files_delta, Some(-4));
        assert_eq!(
            report.workspace_files_indexed, 6,
            "workspace absolute is the non-external build total"
        );
        assert_eq!(report.nodes_delta, Some(4));
        assert_eq!(report.canonical_edges_delta, Some(2));
    }

    /// When the post-update header cannot be re-read, registered files and their
    /// delta are honestly omitted (`None`) rather than reporting a wrong number.
    /// The workspace absolute is still available from the build result.
    #[test]
    fn update_stats_omits_registered_delta_when_header_missing() {
        let build = build_result_with(24, 17, 6);
        let pre = GraphHeader::new(20, 15, 100, 10);

        let report =
            compute_update_stats(&build, Some(&pre), None, Duration::from_millis(10), false);

        assert_eq!(report.registered_files, None);
        assert_eq!(report.registered_files_delta, None);
        assert_eq!(report.workspace_files_indexed, 6);
        // Node / edge deltas still resolve against the pre-update header.
        assert_eq!(report.nodes_delta, Some(4));
    }

    /// With no pre-update header (first stats-tracked run), every delta is
    /// omitted but absolutes still report.
    #[test]
    fn update_stats_omits_all_deltas_without_pre_header() {
        let build = build_result_with(24, 17, 6);
        let post = GraphHeader::new(24, 17, 110, 6);

        let report =
            compute_update_stats(&build, None, Some(&post), Duration::from_millis(10), false);

        assert_eq!(report.nodes_delta, None);
        assert_eq!(report.canonical_edges_delta, None);
        assert_eq!(report.registered_files_delta, None);
        assert_eq!(report.registered_files, Some(6));
        assert_eq!(report.workspace_files_indexed, 6);
    }

    #[cfg(feature = "jvm-classpath")]
    #[test]
    fn classpath_auto_detection_miss_skips_pipeline() {
        let tmp_cli_workspace = TempDir::new().unwrap();
        let classpath_opts = ClasspathCliOptions {
            enabled: true,
            depth: crate::args::ClasspathDepthArg::Full,
            classpath_file: None,
            build_system: None,
            force_classpath: true,
        };

        let result = run_classpath_pipeline_only(tmp_cli_workspace.path(), &classpath_opts, false)
            .expect("missing JVM build system should be a non-fatal skip");
        assert!(result.is_none());
    }

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
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
            false, // allow_nested
            &[],   // cfg_flags
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
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
            false, // allow_nested
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
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
            false, // allow_nested
            &[],   // cfg_flags
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
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
            false, // allow_nested
            &[],   // cfg_flags
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
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
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
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
            false, // allow_nested
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
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
            false, // allow_nested
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
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
        );
        assert!(result.is_ok());
    }
    }

    large_stack_test! {
    #[test]
    fn test_no_incremental_triggers_full_rebuild_when_snapshot_exists() {
        // C001a: `sqry index --no-incremental` must rebuild the graph even
        // when `.sqry/graph/snapshot.sqry` already exists (without `--force`).
        use crate::args::Cli;
        use clap::Parser;

        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("rebuild.rs");
        fs::write(&file_path, "fn original() {}").unwrap();

        let cli = Cli::parse_from(["sqry", "index"]);

        // Initial build with one symbol.
        run_index(
            &cli,
            tmp.path().to_str().unwrap(),
            false,
            None,
            false,
            false,
            None,
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
            false, // allow_nested
            &[],   // cfg_flags
            None,  // expand_cache
        )
        .expect("initial build should succeed");

        let storage = GraphStorage::new(tmp.path());
        assert!(storage.exists(), "snapshot must exist after initial build");
        let initial_node_count = storage.load_manifest().unwrap().node_count;

        // Add a second symbol and re-run with `--no-incremental` (force = false).
        // Without C001a, run_index would early-exit because the snapshot
        // already exists; the new symbol would not appear in the manifest.
        fs::write(&file_path, "fn original() {}\nfn added_symbol() {}").unwrap();

        run_index(
            &cli,
            tmp.path().to_str().unwrap(),
            false, // force = false
            None,
            false,
            true,  // no_incremental = true ← drives the full-rebuild path
            None,
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
            false, // allow_nested
            &[],   // cfg_flags
            None,  // expand_cache
        )
        .expect("--no-incremental must rebuild even when snapshot exists");

        let post_rebuild_node_count = storage.load_manifest().unwrap().node_count;
        assert!(
            post_rebuild_node_count > initial_node_count,
            "--no-incremental should rebuild and pick up the new symbol \
             (initial={initial_node_count}, post={post_rebuild_node_count})"
        );
    }
    }

    #[test]
    fn format_validation_prometheus_emits_openmetrics_shape() {
        // C001d-a: the restored Prometheus formatter must emit HELP/TYPE
        // metadata plus the gauge sample lines for every populated field.
        let mut status = IndexStatus::not_found();
        status.exists = true;
        status.path = Some("/tmp/example/.sqry/graph".into());
        status.age_seconds = Some(42);
        status.symbol_count = Some(123);
        status.file_count = Some(11);
        status.supports_relations = true;
        status.cross_language_relation_count = Some(9);
        status.stale = Some(false);

        let body = format_validation_prometheus(&status);

        assert!(body.contains("# HELP sqry_index_exists"));
        assert!(body.contains("# TYPE sqry_index_exists gauge"));
        assert!(body.contains("\nsqry_index_exists 1\n"));
        assert!(body.contains("\nsqry_index_supports_relations 1\n"));
        assert!(body.contains("\nsqry_index_symbol_count 123\n"));
        assert!(body.contains("\nsqry_index_file_count 11\n"));
        assert!(body.contains("\nsqry_index_age_seconds 42\n"));
        assert!(body.contains("\nsqry_index_stale 0\n"));
        assert!(body.contains("\nsqry_index_cross_language_relation_count 9\n"));
    }

    large_stack_test! {
    #[test]
    fn run_index_status_prometheus_format_is_accepted() {
        // C001d-b: invoking `run_index_status` with `MetricsFormat::Prometheus`
        // must succeed (formerly the value was silently dropped via a `_`
        // prefix; now it routes through the restored formatter).
        use crate::args::{Cli, MetricsFormat};
        use clap::Parser;

        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("metrics.rs");
        fs::write(&file_path, "fn metric_target() {}").unwrap();

        let cli = Cli::parse_from(["sqry", "index"]);
        run_index(
            &cli,
            tmp.path().to_str().unwrap(),
            false,
            None,
            false,
            false,
            None,
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
            false, // allow_nested
            &[],   // cfg_flags
            None,  // expand_cache
        )
        .expect("initial build for prometheus test must succeed");

        let cli_json = Cli::parse_from(["sqry", "--json"]);
        let result = run_index_status(
            &cli_json,
            tmp.path().to_str().unwrap(),
            MetricsFormat::Prometheus,
        );
        assert!(
            result.is_ok(),
            "--metrics-format prometheus must succeed: {result:?}"
        );
    }
    }

    // Cluster-E §E.3 — `run_index` refuses to create a nested `.sqry/`
    // when an outer project already has its own graph and the same
    // project boundary contains both. The recovery message names all
    // three paths.
    large_stack_test! {
    #[test]
    fn run_index_rejects_nested_creation_without_allow_nested() {
        use crate::args::Cli;
        use clap::Parser;

        let tmp = TempDir::new().unwrap();
        // Outer project: Cargo.toml + .sqry/graph already in place.
        let proj = tmp.path().join("proj");
        fs::create_dir_all(proj.join(".sqry").join("graph")).unwrap();
        fs::write(proj.join("Cargo.toml"), "[package]\n").unwrap();
        // Inner directory the user mistakenly tries to index.
        let nested = proj.join("sub");
        fs::create_dir_all(&nested).unwrap();

        let cli = Cli::parse_from(["sqry", "index"]);
        let result = run_index(
            &cli,
            nested.to_str().unwrap(),
            false,
            None,
            false,
            false,
            None,
            false,
            false,
            crate::args::ClasspathDepthArg::Full,
            None,
            None,
            false,
            false, // allow_nested = false → guard fires
            &[],   // cfg_flags
            None,  // expand_cache
        );
        let err = result.expect_err("nested creation must error without --allow-nested");
        let msg = err.to_string();
        assert!(
            msg.contains("nested .sqry/ index"),
            "must surface the nested-index recovery text, got: {msg}"
        );
        assert!(
            msg.contains("--allow-nested"),
            "must hint at the --allow-nested escape hatch, got: {msg}"
        );
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

    #[cfg(feature = "jvm-classpath")]
    #[test]
    fn test_resolve_allowed_jars_prefers_nearest_scope() {
        let scopes = vec![
            (
                PathBuf::from("/repo/services/app"),
                std::collections::HashSet::from([PathBuf::from("/jars/app.jar")]),
            ),
            (
                PathBuf::from("/repo"),
                std::collections::HashSet::from([PathBuf::from("/jars/root.jar")]),
            ),
        ];

        let resolved =
            resolve_allowed_jars(Some(Path::new("/repo/services/app/src/Main.java")), &scopes)
                .expect("nearest scope should resolve");
        assert!(
            resolved
                .allowed_jars
                .contains(&PathBuf::from("/jars/app.jar"))
        );
        assert!(
            !resolved
                .allowed_jars
                .contains(&PathBuf::from("/jars/root.jar"))
        );
        assert_eq!(
            resolved.matched_root.as_deref(),
            Some(Path::new("/repo/services/app"))
        );
    }

    #[cfg(feature = "jvm-classpath")]
    #[test]
    fn test_filter_scope_targets_excludes_out_of_scope_jars() {
        let targets = [
            sqry_classpath::graph::emitter::ClasspathNodeRef {
                node_id: sqry_core::graph::unified::node::NodeId::new(1, 0),
                fqn: "com.example.Foo".to_string(),
                jar_path: PathBuf::from("/jars/app.jar"),
                file_id: sqry_core::graph::unified::FileId::new(1),
            },
            sqry_classpath::graph::emitter::ClasspathNodeRef {
                node_id: sqry_core::graph::unified::node::NodeId::new(2, 0),
                fqn: "com.example.Foo".to_string(),
                jar_path: PathBuf::from("/jars/other.jar"),
                file_id: sqry_core::graph::unified::FileId::new(2),
            },
        ];
        let allowed = std::collections::HashSet::from([PathBuf::from("/jars/app.jar")]);

        let filtered = filter_scope_targets(targets.iter().collect(), &allowed);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].jar_path, PathBuf::from("/jars/app.jar"));
    }

    #[cfg(feature = "jvm-classpath")]
    #[test]
    fn test_prefer_direct_targets_exact_import_direct_wins() {
        use sqry_classpath::graph::provenance::{ClasspathProvenance, ClasspathScope};

        let targets = [
            sqry_classpath::graph::emitter::ClasspathNodeRef {
                node_id: sqry_core::graph::unified::node::NodeId::new(1, 0),
                fqn: "com.example.Foo".to_string(),
                jar_path: PathBuf::from("/jars/direct.jar"),
                file_id: sqry_core::graph::unified::FileId::new(1),
            },
            sqry_classpath::graph::emitter::ClasspathNodeRef {
                node_id: sqry_core::graph::unified::node::NodeId::new(2, 0),
                fqn: "com.example.Foo".to_string(),
                jar_path: PathBuf::from("/jars/transitive.jar"),
                file_id: sqry_core::graph::unified::FileId::new(2),
            },
        ];

        let provenance = vec![
            ClasspathProvenance {
                jar_path: PathBuf::from("/jars/direct.jar"),
                coordinates: None,
                is_direct: true,
                scopes: vec![ClasspathScope {
                    module_name: "app".to_owned(),
                    module_root: PathBuf::from("/repo/app"),
                    is_direct: true,
                }],
            },
            ClasspathProvenance {
                jar_path: PathBuf::from("/jars/transitive.jar"),
                coordinates: None,
                is_direct: false,
                scopes: vec![ClasspathScope {
                    module_name: "app".to_owned(),
                    module_root: PathBuf::from("/repo/app"),
                    is_direct: false,
                }],
            },
        ];
        let lookup = build_provenance_lookup(&provenance);

        let result = prefer_direct_targets(
            targets.iter().collect(),
            Some(Path::new("/repo/app")),
            &lookup,
        );
        assert_eq!(result.len(), 1, "direct jar should win over transitive");
        assert_eq!(result[0].jar_path, PathBuf::from("/jars/direct.jar"));
    }

    #[cfg(feature = "jvm-classpath")]
    #[test]
    fn test_prefer_direct_targets_wildcard_same_shape() {
        use sqry_classpath::graph::provenance::{ClasspathProvenance, ClasspathScope};

        // Wildcard imports group by FQN first, then each group goes through
        // prefer_direct_targets. Simulate one FQN group with two candidates.
        let targets = [
            sqry_classpath::graph::emitter::ClasspathNodeRef {
                node_id: sqry_core::graph::unified::node::NodeId::new(10, 0),
                fqn: "com.example.Bar".to_string(),
                jar_path: PathBuf::from("/jars/direct.jar"),
                file_id: sqry_core::graph::unified::FileId::new(10),
            },
            sqry_classpath::graph::emitter::ClasspathNodeRef {
                node_id: sqry_core::graph::unified::node::NodeId::new(11, 0),
                fqn: "com.example.Bar".to_string(),
                jar_path: PathBuf::from("/jars/transitive.jar"),
                file_id: sqry_core::graph::unified::FileId::new(11),
            },
        ];

        let provenance = vec![
            ClasspathProvenance {
                jar_path: PathBuf::from("/jars/direct.jar"),
                coordinates: None,
                is_direct: true,
                scopes: vec![ClasspathScope {
                    module_name: "app".to_owned(),
                    module_root: PathBuf::from("/repo/app"),
                    is_direct: true,
                }],
            },
            ClasspathProvenance {
                jar_path: PathBuf::from("/jars/transitive.jar"),
                coordinates: None,
                is_direct: false,
                scopes: vec![ClasspathScope {
                    module_name: "app".to_owned(),
                    module_root: PathBuf::from("/repo/app"),
                    is_direct: false,
                }],
            },
        ];
        let lookup = build_provenance_lookup(&provenance);

        let result = prefer_direct_targets(
            targets.iter().collect(),
            Some(Path::new("/repo/app")),
            &lookup,
        );
        assert_eq!(
            result.len(),
            1,
            "wildcard: direct jar should win over transitive"
        );
        assert_eq!(result[0].jar_path, PathBuf::from("/jars/direct.jar"));
    }

    #[cfg(feature = "jvm-classpath")]
    #[test]
    fn test_prefer_direct_targets_true_ambiguity_two_direct_jars() {
        use sqry_classpath::graph::provenance::{ClasspathProvenance, ClasspathScope};

        // Two direct jars with the same FQN: true ambiguity, should remain
        // ambiguous (both returned).
        let targets = [
            sqry_classpath::graph::emitter::ClasspathNodeRef {
                node_id: sqry_core::graph::unified::node::NodeId::new(20, 0),
                fqn: "com.example.Baz".to_string(),
                jar_path: PathBuf::from("/jars/direct-a.jar"),
                file_id: sqry_core::graph::unified::FileId::new(20),
            },
            sqry_classpath::graph::emitter::ClasspathNodeRef {
                node_id: sqry_core::graph::unified::node::NodeId::new(21, 0),
                fqn: "com.example.Baz".to_string(),
                jar_path: PathBuf::from("/jars/direct-b.jar"),
                file_id: sqry_core::graph::unified::FileId::new(21),
            },
        ];

        let provenance = vec![
            ClasspathProvenance {
                jar_path: PathBuf::from("/jars/direct-a.jar"),
                coordinates: None,
                is_direct: true,
                scopes: vec![ClasspathScope {
                    module_name: "app".to_owned(),
                    module_root: PathBuf::from("/repo/app"),
                    is_direct: true,
                }],
            },
            ClasspathProvenance {
                jar_path: PathBuf::from("/jars/direct-b.jar"),
                coordinates: None,
                is_direct: true,
                scopes: vec![ClasspathScope {
                    module_name: "app".to_owned(),
                    module_root: PathBuf::from("/repo/app"),
                    is_direct: true,
                }],
            },
        ];
        let lookup = build_provenance_lookup(&provenance);

        let result = prefer_direct_targets(
            targets.iter().collect(),
            Some(Path::new("/repo/app")),
            &lookup,
        );
        assert_eq!(
            result.len(),
            2,
            "two direct jars = true ambiguity, both should remain"
        );
    }
}
