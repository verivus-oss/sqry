// RKG: CODE:SQRY-CLI implements REQ:SQRY-RUBY-QUALIFIED-CALLERS
//! Query command implementation

use crate::args::Cli;
use crate::index_discovery::{augment_query_with_scope, find_nearest_index};
use crate::output::{
    DisplaySymbol, OutputStreams, call_identity_from_qualified_name, create_formatter,
};
use crate::plugin_defaults::{self, PluginSelectionMode};
use anyhow::{Context, Result, bail};
use sqry_core::graph::{
    AcquisitionOperation, AutoBuildHook, FilesystemGraphProvider, GraphAcquirer, GraphAcquisition,
    GraphAcquisitionError, GraphAcquisitionRequest, MissingGraphPolicy, PathPolicy,
    PluginSelectionPolicy, PluginSelectionStatus, StalePolicy,
};
use sqry_core::query::QueryExecutor;
use sqry_core::query::parser_new::Parser as QueryParser;
use sqry_core::query::results::QueryResults;
use sqry_core::query::security::QuerySecurityConfig;
use sqry_core::query::types::{Expr, Value};
use sqry_core::query::validator::ValidationOptions;
use sqry_core::relations::CallIdentityMetadata;
use sqry_core::search::Match as TextMatch;
use sqry_core::search::classifier::{QueryClassifier, QueryType};
use sqry_core::search::fallback::{FallbackConfig, FallbackSearchEngine, SearchResults};
use sqry_core::session::{SessionManager, SessionStats};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

static QUERY_SESSION: std::sync::LazyLock<Mutex<Option<SessionManager>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

const DEFAULT_QUERY_LIMIT: usize = 1000;

/// Simple query statistics for CLI display (replaces `sqry_core::query::QueryStats`).
#[derive(Debug, Clone, Default)]
struct SimpleQueryStats {
    /// Whether a graph/index was used
    used_index: bool,
}

/// Convert `QueryResults` to `Vec<DisplaySymbol>` for display purposes.
///
/// This creates `DisplaySymbol` structs directly from `QueryMatch`,
/// avoiding the deprecated Symbol intermediate type.
fn query_results_to_display_symbols(results: &QueryResults) -> Vec<DisplaySymbol> {
    results
        .iter()
        .map(|m| DisplaySymbol::from_query_match(&m))
        .collect()
}

struct QueryExecution {
    stats: SimpleQueryStats,
    symbols: Vec<DisplaySymbol>,
    executor: Option<QueryExecutor>,
}

enum QueryExecutionOutcome {
    Terminal,
    Continue(Box<QueryExecution>),
}

struct NonSessionQueryParams<'a> {
    cli: &'a Cli,
    query_string: &'a str,
    search_path: &'a str,
    validation_options: ValidationOptions,
    verbose: bool,
    no_parallel: bool,
    relation_context: &'a RelationDisplayContext,
    variables: Option<&'a std::collections::HashMap<String, String>>,
}

struct QueryExecutionParams<'a> {
    cli: &'a Cli,
    query_string: &'a str,
    search_path: &'a Path,
    validation_options: ValidationOptions,
    no_parallel: bool,
    start: Instant,
    query_type: QueryType,
    variables: Option<&'a std::collections::HashMap<String, String>>,
    /// Provider-acquired graph for the canonical workspace. Threaded into
    /// the semantic execution path so the executor uses
    /// `execute_on_preloaded_graph` and avoids a redundant disk load.
    acquisition: &'a GraphAcquisition,
}

struct QueryRenderParams<'a> {
    cli: &'a Cli,
    query_string: &'a str,
    verbose: bool,
    start: Instant,
    relation_context: &'a RelationDisplayContext,
    index_info: IndexDiagnosticInfo,
}

struct HybridQueryParams<'a> {
    cli: &'a Cli,
    query_string: &'a str,
    search_path: &'a Path,
    validation_options: ValidationOptions,
    no_parallel: bool,
    start: Instant,
    query_type: QueryType,
    variables: Option<&'a std::collections::HashMap<String, String>>,
    /// Provider-acquired graph. SGA03 Major #1 (codex iter2): the CLI
    /// hybrid path threads this `Arc<CodeGraph>` directly into
    /// [`FallbackSearchEngine::search_with_preloaded_graph`] (and
    /// siblings) so the semantic attempt runs through
    /// [`QueryExecutor::execute_on_preloaded_graph`] instead of the
    /// executor's `execute_on_graph` cache+disk-load path.
    acquisition: &'a GraphAcquisition,
}

/// Run a query command to search for symbols using AST-aware predicates
///
/// # Arguments
///
/// * `cli` - CLI arguments
/// * `query_string` - Query string with predicates (e.g., "kind:function AND name~=/test/")
/// * `search_path` - Path to search (file or directory)
/// * `explain` - If true, explain the query instead of executing it
/// * `verbose` - If true, show verbose output including cache statistics
/// * `session_mode` - If true, use persistent session for repeated queries
/// * `no_parallel` - If true, disable parallel query execution (for A/B testing)
/// * `timeout_secs` - Query timeout in seconds (max 30s per security policy)
/// * `result_limit` - Maximum number of results to return
///
/// # Errors
/// Returns an error if query validation fails, execution fails, or output cannot be written.
///
/// # STEP_8 precedence
///
/// `search_path` must be resolved by the caller via
/// [`crate::args::Cli::resolve_subcommand_path`] so that an explicit positional
/// `<path>` argument wins over the global `--workspace` /
/// `SQRY_WORKSPACE_FILE` flag.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::fn_params_excessive_bools)] // CLI flags map directly to booleans.
pub fn run_query(
    cli: &Cli,
    query_string: &str,
    search_path: &str,
    explain: bool,
    verbose: bool,
    session_mode: bool,
    no_parallel: bool,
    timeout_secs: Option<u64>,
    result_limit: Option<usize>,
    variables: &[String],
) -> Result<()> {
    // Create output streams with optional pager support
    let mut streams = OutputStreams::with_pager(cli.pager_config());

    ensure_repo_predicate_not_present(query_string)?;

    let validation_options = build_validation_options(cli);

    // Build security config from CLI flags (30s ceiling is enforced by QuerySecurityConfig)
    let security_config = build_security_config(timeout_secs, result_limit);
    maybe_emit_security_diagnostics(&mut streams, &security_config, verbose)?;

    // NOTE: Security enforcement via QueryGuard will be integrated into QueryExecutor
    // in a future enhancement. For now, the config is built and validated.
    let _ = &security_config; // Silence unused warning until full integration

    // Parse --var KEY=VALUE pairs into a variables map for the executor
    let parsed_variables = parse_variable_args(variables)?;
    let variables_opt = if parsed_variables.is_empty() {
        None
    } else {
        Some(&parsed_variables)
    };

    // SGA03 Major #3 fix — strict invalid-path validation must run before
    // pipeline/join dispatch so a malformed path produces an `invalid path`
    // diagnostic (matching the semantic path) instead of "no pipeline matched"
    // or a downstream executor error. Pipeline/join queries today reuse the
    // executor's own graph cache via `execute_join` / `execute_on_graph_with_variables`;
    // running path validation up-front gives them the same path-policy
    // semantics as `acquire_graph_for_cli` without needing a full provider
    // acquisition (the executor's cache load remains the canonical graph
    // source for those code paths).
    //
    // Explain mode is path-independent (it operates on the query string only),
    // so it deliberately skips this check.
    if !explain {
        validate_query_path_strict(Path::new(search_path))?;
    }

    // Check for pipeline queries (base query | stage)
    if let Some(pipeline) = detect_pipeline_query(query_string)? {
        run_pipeline_query(
            cli,
            &mut streams,
            query_string,
            search_path,
            &pipeline,
            no_parallel,
            variables_opt,
        )?;
        return streams.finish_checked();
    }

    // Check for join queries (LHS CALLS RHS)
    if is_join_query(query_string) {
        run_join_query(
            cli,
            &mut streams,
            query_string,
            search_path,
            no_parallel,
            variables_opt,
        )?;
        return streams.finish_checked();
    }

    // If explain mode, use get_query_plan for detailed output (semantic only)
    if explain {
        run_query_explain(query_string, validation_options, no_parallel, &mut streams)?;
        return streams.finish_checked();
    }

    let relation_context = RelationDisplayContext::from_query(query_string);

    // IMPORTANT: Check session mode FIRST, before any index loading
    // This allows session queries to short-circuit directly to the cached executor
    // (fixes CODEX MEDIUM-2: session mode was validating before checking cache)
    // RKG: CODE:SQRY-CLI implements REQ:SQRY-RUBY-QUALIFIED-CALLERS (MEDIUM-2 fix)
    if session_mode {
        let result = run_query_with_session(
            cli,
            &mut streams,
            query_string,
            search_path,
            verbose,
            no_parallel,
            &relation_context,
        );
        // Check result first, then finalize pager
        // If the query failed, return that error; otherwise check pager status
        result?;
        return streams.finish_checked();
    }

    let params = NonSessionQueryParams {
        cli,
        query_string,
        search_path,
        validation_options,
        verbose,
        no_parallel,
        relation_context: &relation_context,
        variables: variables_opt,
    };
    run_query_non_session(&mut streams, &params)?;

    // Finalize pager (flushes buffer, waits for pager if spawned, propagates exit code)
    streams.finish_checked()
}

fn build_validation_options(cli: &Cli) -> ValidationOptions {
    ValidationOptions {
        fuzzy_fields: cli.fuzzy_fields,
        fuzzy_field_distance: cli.fuzzy_field_distance,
    }
}

fn build_security_config(
    timeout_secs: Option<u64>,
    result_limit: Option<usize>,
) -> QuerySecurityConfig {
    let mut config = QuerySecurityConfig::default();
    if let Some(secs) = timeout_secs {
        config = config.with_timeout(Duration::from_secs(secs));
    }
    if let Some(limit) = result_limit {
        config = config.with_result_cap(limit);
    }
    config
}

fn maybe_emit_security_diagnostics(
    streams: &mut OutputStreams,
    security_config: &QuerySecurityConfig,
    verbose: bool,
) -> Result<()> {
    if verbose {
        streams.write_diagnostic(&format!(
            "[Security] timeout={}s, limit={}, memory={}MB",
            security_config.timeout().as_secs(),
            security_config.result_cap(),
            security_config.memory_limit() / (1024 * 1024),
        ))?;
    }
    Ok(())
}

fn run_query_explain(
    query_string: &str,
    validation_options: ValidationOptions,
    no_parallel: bool,
    streams: &mut OutputStreams,
) -> Result<()> {
    let mut executor = create_executor_with_plugins().with_validation_options(validation_options);
    if no_parallel {
        executor = executor.without_parallel();
    }
    let plan = executor.get_query_plan(query_string)?;
    let explain_output = format!(
        "Query Plan:\n  Original: {}\n  Optimized: {}\n\nExecution:\n{}\n\nPerformance:\n  Execution time: {}ms\n  Index-aware: {}\n  Cache: {}",
        plan.original_query,
        plan.optimized_query,
        format_execution_steps(&plan.steps),
        plan.execution_time_ms,
        if plan.used_index { "Yes" } else { "No" },
        format_cache_status(&plan.cache_status),
    );
    streams.write_diagnostic(&explain_output)?;
    Ok(())
}

/// Resolved effective index root, augmented query, and diagnostic info.
struct EffectiveIndexResolution {
    index_root: PathBuf,
    query: String,
    info: IndexDiagnosticInfo,
}

/// Walk up the directory tree to find the nearest index, determine the effective
/// index root, augment the query with scope filters if needed, and build diagnostic info.
fn resolve_effective_index_root(
    search_path: &Path,
    query_string: &str,
) -> EffectiveIndexResolution {
    let index_location = find_nearest_index(search_path);

    if let Some(ref loc) = index_location {
        let root = loc.index_root.clone();
        let (query, filtered_to) = if loc.requires_scope_filter {
            if let Some(relative_scope) = loc.relative_scope() {
                let scope_str = if loc.is_file_query {
                    relative_scope.to_string_lossy().into_owned()
                } else {
                    format!("{}/**", relative_scope.display())
                };
                let augmented =
                    augment_query_with_scope(query_string, &relative_scope, loc.is_file_query);
                (augmented, Some(scope_str))
            } else {
                (query_string.to_string(), None)
            }
        } else {
            (query_string.to_string(), None)
        };
        let info = IndexDiagnosticInfo {
            index_root: Some(root.clone()),
            filtered_to,
            used_ancestor_index: loc.is_ancestor,
        };
        EffectiveIndexResolution {
            index_root: root,
            query,
            info,
        }
    } else {
        EffectiveIndexResolution {
            index_root: search_path.to_path_buf(),
            query: query_string.to_string(),
            info: IndexDiagnosticInfo::default(),
        }
    }
}

fn run_query_non_session(
    streams: &mut OutputStreams,
    params: &NonSessionQueryParams<'_>,
) -> Result<()> {
    let NonSessionQueryParams {
        cli,
        query_string,
        search_path,
        validation_options,
        verbose,
        no_parallel,
        relation_context,
        variables,
    } = *params;
    let search_path_path = Path::new(search_path);

    // SGA03 Major #4 — `--text` mode is pure text scanning and must
    // continue to work on unindexed paths. Skip full graph acquisition
    // here; the strict path-validation step already ran in `run_query`
    // before pipeline/join detection, so invalid paths are rejected
    // up-front in this mode too.
    if cli.text {
        return run_query_text_only(streams, params);
    }

    // SGA03 regression fix — validate query syntax BEFORE full graph
    // acquisition so that an invalid query string (parse error or
    // unknown field) is reported as a parse / validation error
    // (exit 2) rather than being masked by `acquire_graph_for_cli`'s
    // "no graph found" path-acquisition error (exit 1) when the
    // search path is a valid but unindexed directory.
    //
    // CLI_INTEGRATION.md §4 Exit behavior: invalid query syntax remains
    // a query-parse failure, not an acquisition failure. Path validation
    // (in `run_query`) still fires before this probe, so an invalid path
    // wins over an invalid query — matching the existing precedence
    // tested in `cli_invalid_path_rejected_before_graph_load`.
    //
    // The probe is gated on `QueryClassifier::classify(query) == Semantic`
    // so it only fires for queries that pre-SGA03 would have produced an
    // exit-2 parse error anyway. Hybrid- and Text-classified queries keep
    // their forgiving fallback semantics (e.g. `unknown_field:value`
    // falling back to a text search at exit 0) — the parse probe must
    // not regress that behavior.
    if QueryClassifier::classify(query_string) == QueryType::Semantic {
        probe_validate_query_syntax(cli, search_path_path, query_string, validation_options)?;
    }

    // SGA03: route the read-only graph acquisition through
    // `FilesystemGraphProvider`. The provider canonicalizes the path,
    // enforces strict path policy (existence + workspace boundary +
    // symlink-escape rejection), and verifies snapshot integrity *before*
    // any query work runs. Path errors here precede the index-ancestor
    // discovery diagnostic, satisfying the SGA03 acceptance criterion that
    // invalid paths fail before graph load.
    let acquisition = acquire_graph_for_cli(cli, search_path_path)?;

    // Index ancestor discovery: find nearest .sqry-index in directory tree.
    // The provider already validated the path; this call only computes the
    // CLI-specific scope filter (`(query) AND path:...`).
    let resolution = resolve_effective_index_root(search_path_path, query_string);
    let EffectiveIndexResolution {
        index_root: effective_index_root,
        query: effective_query,
        info: index_info,
    } = resolution;

    let query_type = QueryClassifier::classify(&effective_query);

    let start = Instant::now();
    let execution_params = QueryExecutionParams {
        cli,
        query_string: &effective_query,
        search_path: &effective_index_root,
        validation_options,
        no_parallel,
        start,
        query_type,
        variables,
        acquisition: &acquisition,
    };
    let outcome = execute_query_mode(streams, &execution_params)?;
    let render_params = QueryRenderParams {
        cli,
        query_string: &effective_query,
        verbose,
        start,
        relation_context,
        index_info,
    };
    render_query_outcome(streams, outcome, render_params)
}

fn execute_query_mode(
    streams: &mut OutputStreams,
    params: &QueryExecutionParams<'_>,
) -> Result<QueryExecutionOutcome> {
    let cli = params.cli;
    let query_string = params.query_string;
    let search_path = params.search_path;
    let validation_options = params.validation_options;
    let no_parallel = params.no_parallel;
    let start = params.start;
    let query_type = params.query_type;
    let variables = params.variables;
    let acquisition = params.acquisition;

    if should_use_hybrid_search(cli) {
        let params = HybridQueryParams {
            cli,
            query_string,
            search_path,
            validation_options,
            no_parallel,
            start,
            query_type,
            variables,
            acquisition,
        };
        execute_hybrid_query(streams, &params)
    } else {
        execute_semantic_query(
            cli,
            query_string,
            search_path,
            validation_options,
            no_parallel,
            variables,
            acquisition,
        )
    }
}

fn render_query_outcome(
    streams: &mut OutputStreams,
    outcome: QueryExecutionOutcome,
    params: QueryRenderParams<'_>,
) -> Result<()> {
    let QueryRenderParams {
        cli,
        query_string,
        verbose,
        start,
        relation_context,
        index_info,
    } = params;
    if let QueryExecutionOutcome::Continue(mut execution) = outcome {
        let elapsed = start.elapsed();
        let execution = &mut *execution;
        let diagnostics = QueryDiagnostics::Standard { index_info };
        render_semantic_results(
            cli,
            streams,
            query_string,
            &mut execution.symbols,
            &execution.stats,
            elapsed,
            verbose,
            execution.executor.as_ref(),
            &diagnostics,
            relation_context,
        )?;
    }

    Ok(())
}

/// SGA03 Major #4 — execute the `--text` (text-only) mode without
/// acquiring a graph through `FilesystemGraphProvider`.
///
/// Text-only search is intentionally unindexed: it ripgrep-scans files
/// under `search_path` and never consults the graph. Routing it through
/// `acquire_graph_for_cli` would have made `sqry query --text` fail on
/// any directory that has not been indexed yet, which is a regression
/// versus pre-migration behavior.
///
/// Strict path validation still ran in `run_query` before we got here,
/// so non-existent / non-canonicalizable paths have already been
/// rejected; this function can assume the path is valid.
fn run_query_text_only(
    streams: &mut OutputStreams,
    params: &NonSessionQueryParams<'_>,
) -> Result<()> {
    let NonSessionQueryParams {
        cli,
        query_string,
        search_path,
        ..
    } = *params;
    let search_path_path = Path::new(search_path);

    // SGA03 Major #4 (codex iter3): pure `--text` search must not touch the
    // persisted graph manifest or the plugin selection it implies. Building
    // an executor through `create_executor_with_plugins_for_cli` would call
    // `resolve_plugin_selection(.., PluginSelectionMode::ReadOnly)`, which
    // for any existing `.sqry/graph` resolves the manifest's
    // `active_plugin_ids` and fails when the persisted ids no longer match
    // the running binary's registry — even though text mode is a ripgrep
    // scan that never consults the graph or any plugin field.
    //
    // `FallbackSearchEngine::with_config` constructs a default
    // `QueryExecutor` (no plugin manager, no manifest read), and
    // `search_text_only` only exercises the text searcher, so neither the
    // discarded `validation_options` nor `no_parallel` hooks are observable
    // here. Strict path validation already ran in `run_query` before
    // dispatching, satisfying the SGA03 invalid-path tightening.
    let config = build_hybrid_config(cli);
    let mut engine = FallbackSearchEngine::with_config(config)?;

    let start = Instant::now();
    let results = engine.search_text_only(query_string, search_path_path)?;
    let elapsed = start.elapsed();

    match results {
        SearchResults::Text { matches, .. } => {
            render_text_results(cli, streams, &matches, elapsed)?;
        }
        SearchResults::Semantic { results, .. } => {
            // `search_text_only` is supposed to return `Text`, but be
            // defensive — render any semantic results through the standard
            // formatter so we never silently drop matches.
            let mut symbols = query_results_to_display_symbols(&results);
            let stats = SimpleQueryStats { used_index: false };
            let diagnostics = QueryDiagnostics::Standard {
                index_info: IndexDiagnosticInfo::default(),
            };
            render_semantic_results(
                cli,
                streams,
                query_string,
                &mut symbols,
                &stats,
                elapsed,
                params.verbose,
                None,
                &diagnostics,
                params.relation_context,
            )?;
        }
    }
    Ok(())
}

fn execute_hybrid_query(
    streams: &mut OutputStreams,
    params: &HybridQueryParams<'_>,
) -> Result<QueryExecutionOutcome> {
    let cli = params.cli;
    let query_string = params.query_string;
    let search_path = params.search_path;
    let validation_options = params.validation_options;
    let no_parallel = params.no_parallel;
    let start = params.start;
    let query_type = params.query_type;
    let variables = params.variables;
    let acquisition = params.acquisition;

    // Resolve variables in the query string for hybrid search.
    // FallbackSearchEngine doesn't support variable threading, so we resolve
    // at the AST level and serialize back to a query string before passing it.
    let effective_query = if let Some(vars) = variables {
        let ast = QueryParser::parse_query(query_string)
            .map_err(|e| anyhow::anyhow!("Failed to parse query for variable resolution: {e}"))?;
        let resolved = sqry_core::query::types::resolve_variables(&ast.root, vars)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let resolved_ast = sqry_core::query::types::Query {
            root: resolved,
            span: ast.span,
        };
        std::borrow::Cow::Owned(sqry_core::query::parsed_query::serialize_query(
            &resolved_ast,
        ))
    } else {
        std::borrow::Cow::Borrowed(query_string)
    };

    // Use hybrid search engine with plugin-enabled executor
    // This allows metadata queries like async:true and visibility:public to work
    let config = build_hybrid_config(cli);
    let mut executor = create_executor_with_plugins_for_cli(cli, search_path)?
        .with_validation_options(validation_options);
    if no_parallel {
        executor = executor.without_parallel();
    }
    let mut engine = FallbackSearchEngine::with_config_and_executor(config.clone(), executor)?;

    emit_search_mode_diagnostic(cli, streams, query_type, &config)?;

    let results = run_hybrid_search(cli, &mut engine, &effective_query, search_path, acquisition)?;
    let elapsed = start.elapsed();

    match results {
        SearchResults::Semantic { results, .. } => {
            let symbols = query_results_to_display_symbols(&results);
            Ok(QueryExecutionOutcome::Continue(Box::new(QueryExecution {
                stats: build_query_stats(true, symbols.len()),
                symbols,
                executor: None,
            })))
        }
        SearchResults::Text { matches, .. } => {
            render_text_results(cli, streams, &matches, elapsed)?;
            Ok(QueryExecutionOutcome::Terminal)
        }
    }
}

fn execute_semantic_query(
    cli: &Cli,
    query_string: &str,
    search_path: &Path,
    validation_options: ValidationOptions,
    no_parallel: bool,
    variables: Option<&std::collections::HashMap<String, String>>,
    acquisition: &GraphAcquisition,
) -> Result<QueryExecutionOutcome> {
    let mut executor = create_executor_with_plugins_for_cli(cli, search_path)?
        .with_validation_options(validation_options);
    if no_parallel {
        executor = executor.without_parallel();
    }
    // SGA03: execute on the provider-acquired graph rather than re-loading
    // through `execute_on_graph_with_variables`. The provider already
    // canonicalized the workspace and verified snapshot integrity; using
    // `execute_on_preloaded_graph` avoids a redundant disk load and keeps
    // the executor's process-wide graph_cache untouched (the same contract
    // daemon callers rely on).
    let query_results = executor.execute_on_preloaded_graph(
        Arc::clone(&acquisition.graph),
        query_string,
        &acquisition.workspace_root,
        variables,
    )?;
    let symbols = query_results_to_display_symbols(&query_results);
    let stats = SimpleQueryStats { used_index: true };
    Ok(QueryExecutionOutcome::Continue(Box::new(QueryExecution {
        stats,
        symbols,
        executor: Some(executor),
    })))
}

fn emit_search_mode_diagnostic(
    cli: &Cli,
    streams: &mut OutputStreams,
    query_type: QueryType,
    config: &FallbackConfig,
) -> Result<()> {
    if !config.show_search_mode || cli.json {
        return Ok(());
    }

    let message = match query_type {
        QueryType::Semantic => "[Semantic search mode]",
        QueryType::Text => "[Text search mode]",
        QueryType::Hybrid => "[Hybrid mode: trying semantic first...]",
    };
    streams.write_diagnostic(message)?;
    Ok(())
}

fn run_hybrid_search(
    cli: &Cli,
    engine: &mut FallbackSearchEngine,
    query_string: &str,
    search_path: &Path,
    acquisition: &GraphAcquisition,
) -> Result<SearchResults> {
    if cli.text {
        // Force text-only search — graph is unused on this branch.
        engine.search_text_only(query_string, search_path)
    } else if cli.semantic {
        // Force semantic-only search against the provider-acquired graph.
        // SGA03 Major #1 (codex iter2): `search_semantic_only` re-enters
        // the executor's cache+disk-load path; the preloaded variant
        // forwards the acquired `Arc<CodeGraph>` straight to
        // `execute_on_preloaded_graph`.
        engine.search_semantic_only_with_preloaded_graph(
            query_string,
            Arc::clone(&acquisition.graph),
            search_path,
        )
    } else {
        // Automatic hybrid search with fallback — same provider-acquired
        // graph is reused for both the semantic attempt and any text
        // fallback that follows.
        engine.search_with_preloaded_graph(
            query_string,
            Arc::clone(&acquisition.graph),
            search_path,
        )
    }
}

fn build_query_stats(used_index: bool, _symbol_count: usize) -> SimpleQueryStats {
    SimpleQueryStats { used_index }
}

fn render_text_results(
    cli: &Cli,
    streams: &mut OutputStreams,
    matches: &[TextMatch],
    elapsed: Duration,
) -> Result<()> {
    if cli.json {
        // JSON mode: serialize text matches directly
        let json_output = serde_json::json!({
            "text_matches": matches,
            "match_count": matches.len(),
            "execution_time_ms": elapsed.as_millis(),
        });
        streams.write_result(&serde_json::to_string_pretty(&json_output)?)?;
    } else if cli.count {
        // Count mode: just show the count
        streams.write_result(&matches.len().to_string())?;
    } else {
        // Normal mode: print matches in grep format
        for m in matches {
            streams.write_result(&format!(
                "{}:{}:{}",
                m.path.display(),
                m.line,
                m.line_text.trim()
            ))?;
        }

        // Show performance info to stderr (not in JSON or count mode)
        streams.write_diagnostic(&format!(
            "\nQuery executed ({}ms) - {} text matches found",
            elapsed.as_millis(),
            matches.len()
        ))?;
    }

    Ok(())
}

// RKG: CODE:SQRY-CLI implements REQ:SQRY-RUBY-QUALIFIED-CALLERS (MEDIUM-2 fix)
fn run_query_with_session(
    cli: &Cli,
    streams: &mut OutputStreams,
    query_string: &str,
    search_path: &str,
    verbose: bool,
    _no_parallel: bool,
    relation_ctx: &RelationDisplayContext,
) -> Result<()> {
    if cli.text {
        bail!("--session is only available for semantic queries (remove --text)");
    }

    let search_path_path = Path::new(search_path);

    // SGA03 regression fix — validate query syntax BEFORE full graph
    // acquisition so that invalid syntax / unknown fields surface as
    // parse / validation errors (exit 2) rather than being masked by
    // a "no graph found" acquisition error (exit 1) when the path is
    // valid but unindexed. Mirrors the probe in `run_query_non_session`,
    // including the `Semantic`-only classification gate that preserves
    // the forgiving Hybrid/Text fallback behavior pinned by
    // `tests/exit_codes.rs`.
    if QueryClassifier::classify(query_string) == QueryType::Semantic {
        probe_validate_query_syntax(
            cli,
            search_path_path,
            query_string,
            build_validation_options(cli),
        )?;
    }

    // SGA03: enforce strict path policy via the shared provider before any
    // session work runs. Session mode keeps its own warm graph cache, so the
    // acquired graph is dropped immediately after path validation — the
    // provider's role here is purely to fail invalid paths before the
    // session manager loads anything.
    //
    // Session mode pins `MissingGraphPolicy::Error` (no auto-build hook) so
    // the provider runs path-policy validation but does not invoke the CLI
    // auto-build path used by non-session queries. A typed `NoGraph`
    // outcome here is intentionally swallowed so the session-specific
    // "no index found" diagnostic produced by `resolve_session_index` below
    // stays the canonical user-facing message — that contract is pinned by
    // `tests/integration_tests.rs::test_query_session_requires_index`.
    // Any other acquisition error (invalid path, incompatible graph, load
    // failure) propagates so SGA03's strict path-policy semantics remain
    // in force.
    match acquire_graph_for_cli_typed(cli, search_path_path, MissingGraphPolicy::Error)? {
        Ok(_acquisition) => {}
        Err(GraphAcquisitionError::NoGraph { .. }) => {
            // Fall through: `resolve_session_index` below produces the
            // canonical "no index found" diagnostic.
        }
        Err(other) => return Err(map_acquisition_error(other)),
    }

    // Index ancestor discovery for session mode
    let (workspace, relative_scope, is_file_query, is_ancestor) =
        resolve_session_index(search_path_path)?;

    // Build index diagnostic info (for ancestor index or file queries)
    let index_info = if is_ancestor || relative_scope.is_some() {
        // Build filtered_to with proper format (file vs directory)
        let filtered_to = relative_scope.as_ref().map(|p| {
            if is_file_query {
                p.to_string_lossy().into_owned()
            } else {
                format!("{}/**", p.display())
            }
        });
        IndexDiagnosticInfo {
            index_root: Some(workspace.clone()),
            filtered_to,
            used_ancestor_index: is_ancestor,
        }
    } else {
        IndexDiagnosticInfo::default()
    };

    // Augment query with scope filter if using ancestor index
    let effective_query: std::borrow::Cow<'_, str> = if let Some(ref scope) = relative_scope {
        std::borrow::Cow::Owned(augment_query_with_scope(query_string, scope, is_file_query))
    } else {
        std::borrow::Cow::Borrowed(query_string)
    };

    // Check session cache first before expensive validation
    // (fixes CODEX MEDIUM-2: avoid validation on warm queries)
    let mut guard = QUERY_SESSION
        .lock()
        .expect("global session cache mutex poisoned");

    if guard.is_none() {
        // Cold start: create session (graph will be loaded on first query)
        let config = sqry_core::session::SessionConfig::default();
        *guard = Some(
            SessionManager::with_config(config).context("failed to initialise session manager")?,
        );
    }

    let session = guard.as_ref().expect("session manager must be initialised");
    let before = session.stats();
    let start = Instant::now();
    let query_results = session
        .query(&workspace, &effective_query)
        .with_context(|| format!("failed to execute query \"{}\"", &effective_query))?;
    let elapsed = start.elapsed();
    let after = session.stats();
    let cache_hit = after.cache_hits > before.cache_hits;

    let mut symbols = query_results_to_display_symbols(&query_results);

    let stats = SimpleQueryStats { used_index: true };

    let diagnostics = QueryDiagnostics::Session {
        cache_hit,
        stats: after,
        index_info,
    };
    render_semantic_results(
        cli,
        streams,
        &effective_query,
        &mut symbols,
        &stats,
        elapsed,
        verbose,
        None,
        &diagnostics,
        relation_ctx,
    )
}

/// Resolve index location for session mode, walking up directory tree if needed.
///
/// Returns `(index_root, relative_scope, is_file_query, is_ancestor)` for query augmentation.
/// For session mode, file paths are not supported (must be directory).
fn resolve_session_index(path: &Path) -> Result<(PathBuf, Option<PathBuf>, bool, bool)> {
    if !path.exists() {
        bail!(
            "session mode requires a directory ({} does not exist)",
            path.display()
        );
    }

    // Session mode requires a directory, not a file
    if path.is_file() {
        bail!(
            "session mode requires a directory path ({} is a file). \
             For file-specific queries, omit --session.",
            path.display()
        );
    }

    // Use index discovery to find nearest .sqry-index
    if let Some(loc) = find_nearest_index(path) {
        let relative_scope = if loc.requires_scope_filter {
            loc.relative_scope()
        } else {
            None
        };
        Ok((
            loc.index_root,
            relative_scope,
            loc.is_file_query,
            loc.is_ancestor,
        ))
    } else {
        bail!(
            "no index found at {} or any parent directory. \
             Run `sqry index <root>` first.",
            path.display()
        );
    }
}

fn ensure_repo_predicate_not_present(query_string: &str) -> Result<()> {
    if let Ok(query) = QueryParser::parse_query(query_string) {
        if expr_has_repo_predicate(&query.root) {
            bail!(
                "repo: filters are only supported via `sqry workspace query` (multi-repo command)"
            );
        }
        return Ok(());
    }

    if query_string.contains("repo:") {
        bail!("repo: filters are only supported via `sqry workspace query` (multi-repo command)");
    }

    Ok(())
}

fn expr_has_repo_predicate(expr: &Expr) -> bool {
    match expr {
        Expr::And(operands) | Expr::Or(operands) => operands.iter().any(expr_has_repo_predicate),
        Expr::Not(operand) => expr_has_repo_predicate(operand),
        Expr::Condition(condition) => condition.field.as_str() == "repo",
        Expr::Join(join) => {
            expr_has_repo_predicate(&join.left) || expr_has_repo_predicate(&join.right)
        }
    }
}

/// Info about which index was used and any scope filtering applied.
#[derive(Default)]
struct IndexDiagnosticInfo {
    /// Path to the index root directory (where .sqry-index lives)
    index_root: Option<PathBuf>,
    /// Scope filter applied (e.g., "src/**" or "main.rs")
    filtered_to: Option<String>,
    /// True if index was found in an ancestor directory
    used_ancestor_index: bool,
}

enum QueryDiagnostics {
    Standard {
        index_info: IndexDiagnosticInfo,
    },
    Session {
        cache_hit: bool,
        stats: SessionStats,
        index_info: IndexDiagnosticInfo,
    },
}

struct QueryLimitInfo {
    total_matches: usize,
    limit: usize,
    truncated: bool,
}

#[allow(clippy::too_many_arguments)]
fn render_semantic_results(
    cli: &Cli,
    streams: &mut OutputStreams,
    query_string: &str,
    symbols: &mut Vec<DisplaySymbol>,
    stats: &SimpleQueryStats,
    elapsed: Duration,
    verbose: bool,
    executor_opt: Option<&QueryExecutor>,
    diagnostics: &QueryDiagnostics,
    relation_ctx: &RelationDisplayContext,
) -> Result<()> {
    // Optional sorting (opt-in)
    apply_sorting(cli, symbols);

    // Apply limit if specified (default: 1000 for query command)
    let limit_info = apply_symbol_limit(symbols, cli.limit.unwrap_or(DEFAULT_QUERY_LIMIT));

    // Extract index info from diagnostics for JSON output
    let index_info = match diagnostics {
        QueryDiagnostics::Standard { index_info }
        | QueryDiagnostics::Session { index_info, .. } => index_info,
    };

    // Build metadata for structured JSON output
    let metadata =
        build_formatter_metadata(query_string, limit_info.total_matches, elapsed, index_info);

    let identity_overrides = build_identity_overrides(cli, symbols, relation_ctx);

    let display_symbols =
        build_display_symbols_with_identities(symbols, identity_overrides.as_ref());

    // Create formatter based on CLI flags
    format_semantic_output(cli, streams, &display_symbols, &metadata)?;

    maybe_emit_truncation_notice(cli, &limit_info);

    if cli.json || cli.count {
        return Ok(());
    }

    write_query_summary(streams, stats, elapsed, symbols.len(), diagnostics)?;

    if verbose {
        emit_verbose_cache_stats(streams, stats, executor_opt, diagnostics)?;
    }

    maybe_emit_debug_cache(cli, streams, executor_opt, stats)?;

    Ok(())
}

fn apply_sorting(cli: &Cli, symbols: &mut [DisplaySymbol]) {
    if let Some(sort_field) = cli.sort {
        crate::commands::sort::sort_symbols(symbols, sort_field);
    }
}

fn apply_symbol_limit(symbols: &mut Vec<DisplaySymbol>, limit: usize) -> QueryLimitInfo {
    let total_matches = symbols.len();
    let truncated = total_matches > limit;
    if truncated {
        symbols.truncate(limit);
    }
    QueryLimitInfo {
        total_matches,
        limit,
        truncated,
    }
}

fn build_formatter_metadata(
    query_string: &str,
    total_matches: usize,
    elapsed: Duration,
    index_info: &IndexDiagnosticInfo,
) -> crate::output::FormatterMetadata {
    crate::output::FormatterMetadata {
        pattern: Some(query_string.to_string()),
        total_matches,
        execution_time: elapsed,
        filters: sqry_core::json_response::Filters {
            kind: None,
            lang: None,
            ignore_case: false,
            exact: false,
            fuzzy: None,
        },
        index_age_seconds: None,
        // Include scope info when any filtering is applied (ancestor or file query)
        used_ancestor_index: if index_info.used_ancestor_index || index_info.filtered_to.is_some() {
            Some(index_info.used_ancestor_index)
        } else {
            None
        },
        filtered_to: index_info.filtered_to.clone(),
    }
}

fn build_identity_overrides(
    cli: &Cli,
    symbols: &[DisplaySymbol],
    relation_ctx: &RelationDisplayContext,
) -> Option<DisplayIdentities> {
    if cli.qualified_names || cli.json {
        Some(compute_display_identities(symbols, relation_ctx))
    } else {
        None
    }
}

fn format_semantic_output(
    cli: &Cli,
    streams: &mut OutputStreams,
    display_symbols: &[DisplaySymbol],
    metadata: &crate::output::FormatterMetadata,
) -> Result<()> {
    let formatter = create_formatter(cli);
    formatter.format(display_symbols, Some(metadata), streams)?;
    Ok(())
}

fn maybe_emit_truncation_notice(cli: &Cli, limit_info: &QueryLimitInfo) {
    if !cli.json && limit_info.truncated {
        eprintln!(
            "\nShowing {} of {} matches (use --limit to adjust)",
            limit_info.limit, limit_info.total_matches
        );
    }
}

fn build_display_symbols_with_identities(
    symbols: &[DisplaySymbol],
    identity_overrides: Option<&DisplayIdentities>,
) -> Vec<DisplaySymbol> {
    match identity_overrides {
        Some(identities) => symbols
            .iter()
            .enumerate()
            .map(|(idx, symbol)| {
                let invoker_identity = identities
                    .invoker_identities
                    .get(idx)
                    .and_then(Clone::clone);
                let target_identity = identities.target_identities.get(idx).and_then(Clone::clone);

                // Use the appropriate constructor based on which identity is present
                if invoker_identity.is_some() {
                    symbol.clone().with_caller_identity(invoker_identity)
                } else if target_identity.is_some() {
                    symbol.clone().with_callee_identity(target_identity)
                } else {
                    symbol.clone()
                }
            })
            .collect(),
        None => symbols.to_vec(),
    }
}

fn write_query_summary(
    streams: &mut OutputStreams,
    stats: &SimpleQueryStats,
    elapsed: Duration,
    symbol_count: usize,
    diagnostics: &QueryDiagnostics,
) -> Result<()> {
    use std::fmt::Write as _;

    streams.write_diagnostic("")?;

    // Extract index_info from diagnostics
    let index_info = match diagnostics {
        QueryDiagnostics::Standard { index_info }
        | QueryDiagnostics::Session { index_info, .. } => index_info,
    };

    // Build index status message with ancestor info if applicable
    let index_status = if stats.used_index {
        if index_info.used_ancestor_index {
            if let Some(ref root) = index_info.index_root {
                format!("✓ Using index from {}", root.display())
            } else {
                "✓ Used index".to_string()
            }
        } else {
            "✓ Used index".to_string()
        }
    } else {
        "ℹ No index found".to_string()
    };

    let mut msg = format!(
        "{} - Query executed ({}ms) - {} symbols found",
        index_status,
        elapsed.as_millis(),
        symbol_count
    );

    // Add scope filter info if applicable (ancestor index or file query)
    if let Some(ref filtered_to) = index_info.filtered_to {
        let _ = write!(msg, " (filtered to {filtered_to})");
    }

    if let QueryDiagnostics::Session { cache_hit, .. } = diagnostics {
        let cache_state = if *cache_hit {
            "session cache hit"
        } else {
            "session cache miss"
        };
        let _ = write!(msg, " [{cache_state}]");
    }

    streams.write_diagnostic(&msg)?;

    Ok(())
}

fn emit_verbose_cache_stats(
    streams: &mut OutputStreams,
    _stats: &SimpleQueryStats,
    executor_opt: Option<&QueryExecutor>,
    diagnostics: &QueryDiagnostics,
) -> Result<()> {
    match (executor_opt, diagnostics) {
        (Some(executor), _) => emit_executor_cache_stats(streams, executor),
        (None, QueryDiagnostics::Session { stats, .. }) => emit_session_cache_stats(streams, stats),
        _ => emit_hybrid_cache_notice(streams),
    }
}

fn emit_executor_cache_stats(streams: &mut OutputStreams, executor: &QueryExecutor) -> Result<()> {
    let (parse_stats, result_stats) = executor.cache_stats();

    streams.write_diagnostic("")?;
    streams.write_diagnostic("Cache Statistics:")?;

    let parse_msg = format!(
        "  Parse cache:  {:.1}% hit rate ({} hits, {} misses, {} evictions)",
        parse_stats.hit_rate() * 100.0,
        parse_stats.hits,
        parse_stats.misses,
        parse_stats.evictions,
    );
    streams.write_diagnostic(&parse_msg)?;

    let result_msg = format!(
        "  Result cache: {:.1}% hit rate ({} hits, {} misses, {} evictions)",
        result_stats.hit_rate() * 100.0,
        result_stats.hits,
        result_stats.misses,
        result_stats.evictions,
    );
    streams.write_diagnostic(&result_msg)?;

    Ok(())
}

fn emit_session_cache_stats(streams: &mut OutputStreams, stats: &SessionStats) -> Result<()> {
    let total_cache_events = stats.cache_hits + stats.cache_misses;
    let hit_rate = if total_cache_events > 0 {
        (u64_to_f64_lossy(stats.cache_hits) / u64_to_f64_lossy(total_cache_events)) * 100.0
    } else {
        0.0
    };

    streams.write_diagnostic("")?;
    streams.write_diagnostic("Session statistics:")?;
    let _ = streams.write_diagnostic(&format!("  Cached indexes : {}", stats.cached_graphs));
    let _ = streams.write_diagnostic(&format!("  Total queries  : {}", stats.total_queries));
    let _ = streams.write_diagnostic(&format!(
        "  Cache hits     : {} ({hit_rate:.1}% hit rate)",
        stats.cache_hits
    ));
    let _ = streams.write_diagnostic(&format!("  Cache misses   : {}", stats.cache_misses));
    let _ = streams.write_diagnostic(&format!(
        "  Estimated memory: ~{} MB",
        stats.total_memory_mb
    ));

    Ok(())
}

fn emit_hybrid_cache_notice(streams: &mut OutputStreams) -> Result<()> {
    streams.write_diagnostic("")?;
    streams.write_diagnostic("Cache statistics not available in hybrid search mode")?;
    Ok(())
}

struct DisplayIdentities {
    invoker_identities: Vec<Option<CallIdentityMetadata>>,
    target_identities: Vec<Option<CallIdentityMetadata>>,
}

fn compute_display_identities(
    symbols: &[DisplaySymbol],
    relation_ctx: &RelationDisplayContext,
) -> DisplayIdentities {
    // Build identity metadata from symbol qualified names for relation queries.
    // For callers: queries, each result is a caller and gets caller_identity.
    // For callees: queries, each result is a callee and gets callee_identity.
    let has_incoming_targets = !relation_ctx.caller_targets.is_empty();
    let has_outgoing_targets = !relation_ctx.callee_targets.is_empty();

    let identities: Vec<Option<CallIdentityMetadata>> = symbols
        .iter()
        .map(build_identity_from_display_symbol)
        .collect();

    if has_incoming_targets {
        DisplayIdentities {
            invoker_identities: identities,
            target_identities: vec![None; symbols.len()],
        }
    } else if has_outgoing_targets {
        DisplayIdentities {
            invoker_identities: vec![None; symbols.len()],
            target_identities: identities,
        }
    } else {
        DisplayIdentities {
            invoker_identities: vec![None; symbols.len()],
            target_identities: vec![None; symbols.len()],
        }
    }
}

fn build_identity_from_display_symbol(symbol: &DisplaySymbol) -> Option<CallIdentityMetadata> {
    let language = symbol.metadata.get("__raw_language").map(String::as_str);
    let is_static = symbol
        .metadata
        .get("static")
        .is_some_and(|value| value == "true");

    build_identity_from_qualified_name(&symbol.qualified_name, &symbol.kind, language, is_static)
}
fn build_identity_from_qualified_name(
    qualified: &str,
    kind: &str,
    language: Option<&str>,
    is_static: bool,
) -> Option<CallIdentityMetadata> {
    call_identity_from_qualified_name(qualified, kind, language, is_static)
}

/// Format execution steps for display
fn format_execution_steps(steps: &[sqry_core::query::ExecutionStep]) -> String {
    steps
        .iter()
        .map(|step| {
            format!(
                "  {}. {} ({}ms)",
                step.step_num, step.operation, step.time_ms
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format cache status for display
fn format_cache_status(status: &sqry_core::query::CacheStatus) -> String {
    match (status.parse_cache_hit, status.result_cache_hit) {
        (true, true) => "HIT (100% cached)".to_string(),
        (true, false) => "PARTIAL HIT (query cached, results computed)".to_string(),
        (false, true) => "PARTIAL HIT (query parsed, results cached)".to_string(),
        (false, false) => "MISS (first run)".to_string(),
    }
}

fn env_debug_cache_enabled() -> bool {
    matches!(
        env::var("SQRY_CACHE_DEBUG"),
        Ok(value) if value == "1" || value.eq_ignore_ascii_case("true")
    )
}

#[derive(Default)]
struct RelationDisplayContext {
    caller_targets: Vec<String>,
    callee_targets: Vec<String>,
}

impl RelationDisplayContext {
    fn from_query(query_str: &str) -> Self {
        match QueryParser::parse_query(query_str) {
            Ok(ast) => {
                let mut ctx = Self::default();
                collect_relation_targets(&ast.root, &mut ctx);
                ctx
            }
            Err(_) => Self::default(),
        }
    }
}

fn collect_relation_targets(expr: &Expr, ctx: &mut RelationDisplayContext) {
    match expr {
        Expr::And(operands) | Expr::Or(operands) => {
            for operand in operands {
                collect_relation_targets(operand, ctx);
            }
        }
        Expr::Not(inner) => collect_relation_targets(inner, ctx),
        Expr::Join(join) => {
            collect_relation_targets(&join.left, ctx);
            collect_relation_targets(&join.right, ctx);
        }
        Expr::Condition(condition) => match condition.field.as_str() {
            "callers" => {
                if let Value::String(value) = &condition.value
                    && !value.is_empty()
                {
                    ctx.caller_targets.push(value.clone());
                }
            }
            "callees" => {
                if let Value::String(value) = &condition.value
                    && !value.is_empty()
                {
                    ctx.callee_targets.push(value.clone());
                }
            }
            _ => {}
        },
    }
}

fn should_debug_cache(cli: &Cli) -> bool {
    cli.debug_cache || env_debug_cache_enabled()
}

// RKG: CODE:SQRY-CLI implements REQ:SQRY-P2-6-CACHE-EVICTION-POLICY
fn maybe_emit_debug_cache(
    cli: &Cli,
    streams: &mut OutputStreams,
    executor_opt: Option<&QueryExecutor>,
    _stats: &SimpleQueryStats,
) -> Result<()> {
    if !should_debug_cache(cli) {
        return Ok(());
    }

    let Some(executor) = executor_opt else {
        streams.write_diagnostic("CacheStats unavailable in this mode")?;
        return Ok(());
    };

    let (parse_stats, result_stats) = executor.cache_stats();

    let debug_line = format!(
        "CacheStats{{parse_hits={}, parse_misses={}, result_hits={}, result_misses={}}}",
        parse_stats.hits, parse_stats.misses, result_stats.hits, result_stats.misses,
    );
    streams.write_diagnostic(&debug_line)?;
    Ok(())
}

/// Build hybrid search configuration from CLI flags
fn build_hybrid_config(cli: &Cli) -> FallbackConfig {
    let mut config = FallbackConfig::from_env();

    // Override with CLI flags
    if cli.no_fallback {
        config.fallback_enabled = false;
    }

    config.text_context_lines = cli.context;
    config.max_text_results = cli.max_text_results;

    // Disable search mode output in JSON mode
    if cli.json {
        config.show_search_mode = false;
    }

    config
}

/// Determine if hybrid search should be used based on CLI flags
fn should_use_hybrid_search(cli: &Cli) -> bool {
    // Cache debugging requires direct access to QueryExecutor stats.
    if should_debug_cache(cli) {
        return false;
    }

    // Always use hybrid search (it handles --text, --semantic, and hybrid modes)
    // The only reason NOT to use it would be if hybrid search is explicitly disabled
    // via environment variable or if we need old behavior for compatibility
    true
}

/// Create a `QueryExecutor` with all built-in plugins registered
pub(crate) fn create_executor_with_plugins() -> QueryExecutor {
    let plugin_manager = crate::plugin_defaults::create_plugin_manager();
    QueryExecutor::with_plugin_manager(plugin_manager)
}

pub(crate) fn create_executor_with_plugins_for_cli(
    cli: &Cli,
    search_path: &Path,
) -> Result<QueryExecutor> {
    let effective_root = find_nearest_index(search_path)
        .map_or_else(|| search_path.to_path_buf(), |location| location.index_root);
    let resolved_plugins = plugin_defaults::resolve_plugin_selection(
        cli,
        &effective_root,
        PluginSelectionMode::ReadOnly,
    )?;
    Ok(QueryExecutor::with_plugin_manager(
        resolved_plugins.plugin_manager,
    ))
}

/// Strict path-only validation shared by every CLI `sqry query` mode.
///
/// SGA03 Major #3 / Major #4 fix — the migration tightened CLI path
/// handling so non-existent, non-canonicalizable, symlink-escaping, and
/// outside-workspace paths fail *before* any graph load, pipeline / join
/// dispatch, or text-only search runs. The provider already enforces this
/// for the semantic/hybrid graph-load path; this helper duplicates the
/// path-only portion so:
///
/// - Pipeline (`base | aggregation`) and join (`LHS CALLS RHS`) queries
///   that today route through `run_pipeline_query` / `run_join_query`
///   share the same strict invalid-path semantics as the regular
///   semantic path.
/// - `--text` mode (which deliberately skips full graph acquisition so
///   it keeps working on unindexed paths) still rejects invalid paths
///   up-front rather than failing later inside the text scanner with a
///   less informative diagnostic.
///
/// The check is intentionally tighter than `Path::exists`: it requires
/// canonicalization to succeed, which rejects dangling symlinks and
/// permission-denied paths at the boundary. A workspace-boundary check
/// can only be applied once we have a workspace root; for graph-backed
/// modes that's [`acquire_graph_for_cli`]'s job, while text-only mode
/// against an unindexed path simply has no workspace to bound.
/// Parse-only validation probe for the query string.
///
/// SGA03 regression fix — strict CLI path validation now runs before query
/// parsing in `acquire_graph_for_cli`, which means an invalid query string
/// against an *unindexed-but-valid* path was being masked by the provider's
/// `NoGraph` acquisition error (exit 1, "no graph found...") instead of
/// surfacing as a parse / validation error (exit 2).
///
/// The CLI integration spec (`docs/.../CLI_INTEGRATION.md` §4 Exit behavior)
/// requires invalid query syntax to remain a query-parse failure, not an
/// acquisition failure. This helper runs the executor's full parse and
/// validate step *without* loading any graph (purely an AST plus
/// field-registry check), so it can fire on unindexed directories the
/// same way a regular semantic query would have under the pre-SGA03
/// behavior.
///
/// Path validation already runs in `run_query` before this helper is
/// invoked, so an invalid *path* still wins over an invalid *query*. If the
/// path is valid but the directory has no graph, the parse probe reports
/// the parse error (exit 2); if the parse succeeds, control falls through
/// to `acquire_graph_for_cli` which reports the missing graph (exit 1).
///
/// Returns `Ok(())` when the query is well-formed; otherwise returns the
/// underlying [`QueryError`] / [`RichQueryError`] so the CLI's existing
/// error-mapping in `main::handle_run_error` produces exit code 2.
fn probe_validate_query_syntax(
    cli: &Cli,
    search_path: &Path,
    query_string: &str,
    validation_options: ValidationOptions,
) -> Result<()> {
    // Use the same plugin manager the executor would build for this path
    // so plugin-contributed query fields validate correctly. Falls back to
    // the default plugin manager if plugin resolution itself fails — that
    // failure mode will resurface in `acquire_graph_for_cli` with its
    // canonical diagnostic and the parse probe should not double-report.
    let executor = match create_executor_with_plugins_for_cli(cli, search_path) {
        Ok(executor) => executor.with_validation_options(validation_options),
        Err(_) => create_executor_with_plugins().with_validation_options(validation_options),
    };
    executor.parse_query_ast(query_string).map(|_| ())
}

fn validate_query_path_strict(search_path: &Path) -> Result<PathBuf> {
    if !search_path.exists() {
        bail!(
            "invalid path {}: path does not exist",
            search_path.display()
        );
    }
    match search_path.canonicalize() {
        Ok(canonical) => Ok(canonical),
        Err(err) => bail!(
            "invalid path {}: path cannot be canonicalized: {err}",
            search_path.display()
        ),
    }
}

/// Acquire the read-only graph for `search_path` through the shared
/// [`FilesystemGraphProvider`].
///
/// SGA03 routes CLI `sqry query` graph acquisition through the same provider
/// the standalone MCP engine uses. The provider owns:
///
/// 1. Strict path-policy validation (existence, workspace boundary,
///    symlink-escape rejection) **before** any disk graph load.
/// 2. Nearest `.sqry/graph` ancestor discovery (matching the CLI's existing
///    `find_nearest_index` semantics).
/// 3. Manifest SHA-256 verification, snapshot deserialization, and plugin
///    selection compatibility.
///
/// CLI `sqry query` is read-only at the snapshot-load layer but preserves the
/// pre-SGA03 auto-index-on-missing-graph behavior. When no `.sqry/graph`
/// exists for the resolved workspace, the provider invokes the
/// [`AutoBuildHook`] installed below, which honors `SQRY_AUTO_INDEX`
/// (default `true`) and otherwise returns a typed
/// [`GraphAcquisitionError::NoGraph`] so the CLI surfaces the existing
/// "No graph found" diagnostic at exit code 1.
///
/// `SQRY_AUTO_INDEX=false` (or `=0`) preserves the disabled-mode contract
/// exercised by `tests/exit_codes.rs::test_exit_code_1_no_graph_with_auto_index_disabled`.
/// The auto-build hook does **not** broaden any other error class into an
/// auto-build (see `CLI_INTEGRATION.md §2 Inputs And Flags`). It triggers
/// only on the no-artifact branch the provider takes when no ancestor
/// `.sqry/graph` is found.
pub(crate) fn acquire_graph_for_cli(cli: &Cli, search_path: &Path) -> Result<GraphAcquisition> {
    acquire_graph_for_cli_with_policy(cli, search_path, MissingGraphPolicy::AutoBuildIfEnabled)
}

/// Variant of [`acquire_graph_for_cli`] that lets the caller pin the
/// missing-graph policy explicitly. The user-facing error is rendered by
/// [`map_acquisition_error`].
pub(crate) fn acquire_graph_for_cli_with_policy(
    cli: &Cli,
    search_path: &Path,
    missing_graph_policy: MissingGraphPolicy,
) -> Result<GraphAcquisition> {
    let (provider, request) =
        build_cli_provider_and_request(cli, search_path, missing_graph_policy)?;
    provider.acquire(request).map_err(map_acquisition_error)
}

/// Typed-error variant for callers that need to discriminate
/// [`GraphAcquisitionError`] variants before they are rendered to the user
/// message.
///
/// Session mode (`--session`) uses this to swallow `NoGraph` so the
/// session-specific "no index found" diagnostic from `resolve_session_index`
/// remains the canonical user message (pinned by
/// `tests/integration_tests.rs::test_query_session_requires_index`), while
/// still propagating every other acquisition error (invalid path,
/// incompatible graph, load failure).
///
/// Plugin-selection / configuration failures still surface through the
/// shared `anyhow::Error` path (they are CLI-config problems, not graph-
/// acquisition outcomes), so the outer return type is still `Result`. The
/// inner `Result` is the typed [`GraphAcquisitionError`] for the callers
/// that need to inspect it.
pub(crate) fn acquire_graph_for_cli_typed(
    cli: &Cli,
    search_path: &Path,
    missing_graph_policy: MissingGraphPolicy,
) -> Result<std::result::Result<GraphAcquisition, GraphAcquisitionError>> {
    let (provider, request) =
        build_cli_provider_and_request(cli, search_path, missing_graph_policy)?;
    Ok(provider.acquire(request))
}

/// Shared provider/request construction for the CLI's filesystem-backed
/// graph acquisition. Resolves CLI plugin selection (which may surface
/// `anyhow::Error`-shaped configuration failures), wires the optional
/// `AutoBuildHook` for [`MissingGraphPolicy::AutoBuildIfEnabled`], and
/// returns the configured provider plus the read-only `GraphAcquisitionRequest`.
fn build_cli_provider_and_request(
    cli: &Cli,
    search_path: &Path,
    missing_graph_policy: MissingGraphPolicy,
) -> Result<(FilesystemGraphProvider, GraphAcquisitionRequest)> {
    // Resolve the same plugin selection the executor would use; the provider
    // needs it both for snapshot deserialization and for unknown-plugin-id
    // detection on the manifest.
    //
    // The plugin resolution uses an "effective root" that may not be the same
    // as the canonical workspace the provider discovers. To preserve current
    // CLI behavior we resolve plugins against the user-supplied search path
    // first; if that fails because the workspace cannot be located, the
    // provider call below will surface the canonical typed error.
    let plugin_root = find_nearest_index(search_path)
        .map_or_else(|| search_path.to_path_buf(), |location| location.index_root);
    let resolved_plugins = plugin_defaults::resolve_plugin_selection(
        cli,
        &plugin_root,
        PluginSelectionMode::ReadOnly,
    )?;
    let mut provider = FilesystemGraphProvider::new(Arc::new(resolved_plugins.plugin_manager));

    // Only attach the auto-build hook for `AutoBuildIfEnabled` callers.
    // Session-mode callers pass `Error` so missing graphs surface as a typed
    // `NoGraph` error and the session-specific "no index found" diagnostic
    // can run.
    if matches!(missing_graph_policy, MissingGraphPolicy::AutoBuildIfEnabled) {
        // Resolve a second plugin manager with the same selection so the
        // auto-build hook can move an `Arc<PluginManager>` into a `'static`
        // closure without giving up the provider's manager. `PluginManager`
        // is not `Clone`; both managers carry identical selection because
        // `resolve_plugin_selection(ReadOnly)` is deterministic for the
        // same (cli, plugin_root) inputs and there is no `.sqry/graph` to
        // invalidate here (we're on the missing-artifact branch).
        let hook_plugins = plugin_defaults::resolve_plugin_selection(
            cli,
            &plugin_root,
            PluginSelectionMode::ReadOnly,
        )?;
        let hook_plugin_manager = Arc::new(hook_plugins.plugin_manager);

        let auto_build_hook: AutoBuildHook = Arc::new(move |canonical_request: &Path| {
            // Mirror `Engine::ensure_graph` (Gate A iter 1): `SQRY_AUTO_INDEX`
            // gate first; if disabled, surface `NoGraph` so the CLI's
            // existing `map_acquisition_error` produces "No graph found ..."
            // (preserving the pre-SGA03 exit-1 contract). Do NOT broaden
            // auto-index semantics — only the no-artifact branch reaches
            // this hook.
            if !is_auto_index_enabled() {
                return Err(GraphAcquisitionError::NoGraph {
                    workspace_root: canonical_request.to_path_buf(),
                });
            }

            log::info!(
                "No graph found at {}, auto-building index",
                canonical_request.display()
            );

            let config = sqry_core::graph::unified::build::BuildConfig::default();
            let (graph, _build_result) = sqry_core::graph::unified::build::build_and_persist_graph(
                canonical_request,
                &hook_plugin_manager,
                &config,
                "cli:auto_index",
            )
            .map_err(|e| GraphAcquisitionError::BuildFailed {
                workspace_root: canonical_request.to_path_buf(),
                reason: format!("{e}"),
            })?;
            Ok(Arc::new(graph))
        });

        provider = provider.with_auto_build_hook(auto_build_hook);
    }

    let request = GraphAcquisitionRequest {
        requested_path: search_path.to_path_buf(),
        operation: AcquisitionOperation::ReadOnlyQuery,
        path_policy: PathPolicy::default(),
        missing_graph_policy,
        stale_policy: StalePolicy::default(),
        plugin_selection_policy: PluginSelectionPolicy::default(),
        tool_name: Some("sqry_query"),
    };
    Ok((provider, request))
}

/// Returns `true` when CLI auto-indexing is enabled (the default).
///
/// Mirrors `sqry-mcp::engine::is_auto_index_enabled`: `SQRY_AUTO_INDEX=false`
/// or `SQRY_AUTO_INDEX=0` disables auto-indexing; any other value (including
/// unset) enables it. Kept local to the CLI to avoid pulling sqry-mcp into
/// the CLI's dependency graph.
fn is_auto_index_enabled() -> bool {
    match std::env::var("SQRY_AUTO_INDEX") {
        Ok(val) => val != "false" && val != "0",
        Err(_) => true,
    }
}

/// Map a typed [`GraphAcquisitionError`] into an `anyhow::Error` while
/// preserving the variant identity so CLI diagnostics can distinguish path
/// errors from incompatible-graph errors.
fn map_acquisition_error(err: GraphAcquisitionError) -> anyhow::Error {
    match err {
        GraphAcquisitionError::InvalidPath { path, reason } => {
            anyhow::anyhow!("invalid path {}: {}", path.display(), reason)
        }
        GraphAcquisitionError::NoGraph { workspace_root } => {
            anyhow::anyhow!(
                "No graph found for {}. Run `sqry index {}` first.",
                workspace_root.display(),
                workspace_root.display()
            )
        }
        GraphAcquisitionError::IncompatibleGraph {
            source_root,
            status,
        } => match status {
            PluginSelectionStatus::IncompatibleUnknownPluginIds {
                unknown_plugin_ids,
                manifest_path,
            } => {
                let suggested = sqry_plugin_registry::missing_features_for(&unknown_plugin_ids);
                let all_have_features =
                    sqry_plugin_registry::all_unknown_ids_have_features(&unknown_plugin_ids);
                let manifest_str = manifest_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<unknown>".to_string());
                let suggestion = if !suggested.is_empty() {
                    format!(
                        "Rebuild this binary with `cargo install --path sqry-cli --features {}` \
                         or rebuild the index: `sqry index {} --force`.",
                        suggested.join(","),
                        source_root.display(),
                    )
                } else if all_have_features {
                    format!(
                        "Rebuild the index with the binary that produced it: \
                         `sqry index {} --force`.",
                        source_root.display(),
                    )
                } else {
                    format!(
                        "The unknown ids do not match any known feature flag — \
                         the manifest may be from a newer sqry version. \
                         Rebuild the index: `sqry index {} --force`.",
                        source_root.display(),
                    )
                };
                anyhow::anyhow!(
                    "Incompatible graph at {} — manifest references plugins this binary \
                     cannot load: {}. Manifest: {}. {}",
                    source_root.display(),
                    unknown_plugin_ids.join(", "),
                    manifest_str,
                    suggestion,
                )
            }
            PluginSelectionStatus::IncompatibleSnapshotFormat { reason } => anyhow::anyhow!(
                "Incompatible graph at {}: {}. Run `sqry index {} --force` to rebuild.",
                source_root.display(),
                reason,
                source_root.display()
            ),
            PluginSelectionStatus::Exact => {
                anyhow::anyhow!(
                    "Incompatible graph at {} (no detail); rerun `sqry index --force`",
                    source_root.display()
                )
            }
            other => anyhow::anyhow!(
                "Incompatible graph at {}: {other:?}. Rebuild with `sqry index {} --force`.",
                source_root.display(),
                source_root.display(),
            ),
        },
        GraphAcquisitionError::LoadFailed {
            source_root,
            reason,
        } => anyhow::anyhow!(
            "Failed to load graph at {}: {}",
            source_root.display(),
            reason
        ),
        other => anyhow::anyhow!("graph acquisition failed: {other}"),
    }
}

fn u64_to_f64_lossy(value: u64) -> f64 {
    let narrowed = u32::try_from(value).unwrap_or(u32::MAX);
    f64::from(narrowed)
}

// ============================================================================
// Variable, Join, and Pipeline support
// ============================================================================

/// Parse `--var KEY=VALUE` arguments into a `HashMap`.
fn parse_variable_args(args: &[String]) -> Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for arg in args {
        let (key, value) = arg
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("Invalid --var format: '{arg}'. Expected KEY=VALUE"))?;
        if key.is_empty() {
            bail!("Variable name cannot be empty in --var '{arg}'");
        }
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

/// Check if a query string contains a join expression at the root level.
///
/// Returns `false` on parse errors (the normal flow will handle the error).
fn is_join_query(query_str: &str) -> bool {
    match QueryParser::parse_query(query_str) {
        Ok(ast) => matches!(ast.root, Expr::Join(_)),
        Err(_) => false,
    }
}

/// Detect a pipeline query (base query | aggregation stages).
///
/// If the query string contains a `|` character, pipeline parse errors are
/// treated as hard errors (the user intended a pipeline query). If no `|`
/// is present, returns `None` (not a pipeline query).
fn detect_pipeline_query(
    query_str: &str,
) -> Result<Option<sqry_core::query::types::PipelineQuery>> {
    match QueryParser::parse_pipeline_query(query_str) {
        Ok(result) => Ok(result),
        Err(e) => {
            // If the query contains a pipe, the user intended a pipeline query
            // and the parse error should be surfaced (not silently ignored).
            if query_str.contains('|') {
                Err(anyhow::anyhow!("Pipeline parse error: {e}"))
            } else {
                Ok(None)
            }
        }
    }
}

/// Run a join query and render results.
fn run_join_query(
    cli: &Cli,
    streams: &mut OutputStreams,
    query_string: &str,
    search_path: &str,
    no_parallel: bool,
    variables: Option<&std::collections::HashMap<String, String>>,
) -> Result<()> {
    let validation_options = build_validation_options(cli);
    let mut executor = create_executor_with_plugins_for_cli(cli, Path::new(search_path))?
        .with_validation_options(validation_options);
    if no_parallel {
        executor = executor.without_parallel();
    }

    let resolved_path = Path::new(search_path);
    let join_results = executor.execute_join(query_string, resolved_path, variables)?;

    if join_results.truncated() {
        streams.write_diagnostic(&format!(
            "Join query: {} pairs matched via {} (results truncated — cap reached)",
            join_results.len(),
            join_results.edge_kind()
        ))?;
    } else {
        streams.write_diagnostic(&format!(
            "Join query: {} pairs matched via {}",
            join_results.len(),
            join_results.edge_kind()
        ))?;
    }

    for pair in join_results.iter() {
        let left_name = pair.left.name().unwrap_or_default();
        let left_path = pair
            .left
            .relative_path()
            .map_or_else(|| "<unknown>".to_string(), |p| p.display().to_string());
        let right_name = pair.right.name().unwrap_or_default();
        let right_path = pair
            .right
            .relative_path()
            .map_or_else(|| "<unknown>".to_string(), |p| p.display().to_string());

        if cli.json {
            // JSON mode: each pair as a JSON object
            let json = serde_json::json!({
                "left": {
                    "name": left_name.as_ref(),
                    "kind": pair.left.kind().as_str(),
                    "path": left_path,
                    "line": pair.left.start_line(),
                },
                "edge": pair.edge_kind.to_string(),
                "right": {
                    "name": right_name.as_ref(),
                    "kind": pair.right.kind().as_str(),
                    "path": right_path,
                    "line": pair.right.start_line(),
                },
            });
            streams.write_result(&json.to_string())?;
        } else {
            streams.write_result(&format!(
                "{} ({}:{}) {} {} ({}:{})",
                left_name,
                left_path,
                pair.left.start_line(),
                pair.edge_kind,
                right_name,
                right_path,
                pair.right.start_line(),
            ))?;
        }
    }

    Ok(())
}

/// Run a pipeline query (base query + aggregation stages) and render results.
fn run_pipeline_query(
    cli: &Cli,
    streams: &mut OutputStreams,
    _query_string: &str,
    search_path: &str,
    pipeline: &sqry_core::query::types::PipelineQuery,
    no_parallel: bool,
    variables: Option<&std::collections::HashMap<String, String>>,
) -> Result<()> {
    let validation_options = build_validation_options(cli);
    let mut executor = create_executor_with_plugins_for_cli(cli, Path::new(search_path))?
        .with_validation_options(validation_options);
    if no_parallel {
        executor = executor.without_parallel();
    }

    let resolved_path = Path::new(search_path);

    // Execute the base query portion (before the pipe)
    // Serialize the base query from the parsed AST for reliable reconstruction
    let base_query = sqry_core::query::parsed_query::serialize_query(&pipeline.query);

    let results =
        executor.execute_on_graph_with_variables(&base_query, resolved_path, variables)?;

    // Execute each pipeline stage
    for stage in &pipeline.stages {
        let aggregation = sqry_core::query::execute_pipeline_stage(&results, stage);

        if cli.json {
            render_aggregation_json(streams, &aggregation)?;
        } else {
            streams.write_result(&format!("{aggregation}"))?;
        }
    }

    Ok(())
}

/// Render aggregation results as JSON.
fn render_aggregation_json(
    streams: &mut OutputStreams,
    aggregation: &sqry_core::query::pipeline::AggregationResult,
) -> Result<()> {
    use sqry_core::query::pipeline::AggregationResult;
    let json = match aggregation {
        AggregationResult::Count(r) => serde_json::json!({
            "type": "count",
            "total": r.total,
        }),
        AggregationResult::GroupBy(r) => serde_json::json!({
            "type": "group_by",
            "field": r.field,
            "groups": r.groups.iter().map(|(k, v)| serde_json::json!({"value": k, "count": v})).collect::<Vec<_>>(),
        }),
        AggregationResult::Top(r) => serde_json::json!({
            "type": "top",
            "field": r.field,
            "n": r.n,
            "entries": r.entries.iter().map(|(k, v)| serde_json::json!({"value": k, "count": v})).collect::<Vec<_>>(),
        }),
        AggregationResult::Stats(r) => serde_json::json!({
            "type": "stats",
            "total": r.total,
            "by_kind": r.by_kind.iter().map(|(k, v)| serde_json::json!({"value": k, "count": v})).collect::<Vec<_>>(),
            "by_lang": r.by_lang.iter().map(|(k, v)| serde_json::json!({"value": k, "count": v})).collect::<Vec<_>>(),
            "by_visibility": r.by_visibility.iter().map(|(k, v)| serde_json::json!({"value": k, "count": v})).collect::<Vec<_>>(),
        }),
    };
    streams.write_result(&json.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::relations::CallIdentityKind;

    // ==========================================================================
    // u64_to_f64_lossy tests
    // ==========================================================================

    #[test]
    fn test_u64_to_f64_lossy_zero() {
        assert!((u64_to_f64_lossy(0) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_u64_to_f64_lossy_small_values() {
        assert!((u64_to_f64_lossy(1) - 1.0).abs() < f64::EPSILON);
        assert!((u64_to_f64_lossy(100) - 100.0).abs() < f64::EPSILON);
        assert!((u64_to_f64_lossy(1000) - 1000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_u64_to_f64_lossy_u32_max() {
        let u32_max = u64::from(u32::MAX);
        assert!((u64_to_f64_lossy(u32_max) - f64::from(u32::MAX)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_u64_to_f64_lossy_overflow_clamps_to_u32_max() {
        // Values larger than u32::MAX should clamp
        let large_value = u64::from(u32::MAX) + 1;
        assert!((u64_to_f64_lossy(large_value) - f64::from(u32::MAX)).abs() < f64::EPSILON);
    }

    // ==========================================================================
    // format_cache_status tests
    // ==========================================================================

    #[test]
    fn test_format_cache_status_full_hit() {
        let status = sqry_core::query::CacheStatus {
            parse_cache_hit: true,
            result_cache_hit: true,
        };
        assert_eq!(format_cache_status(&status), "HIT (100% cached)");
    }

    #[test]
    fn test_format_cache_status_parse_hit_only() {
        let status = sqry_core::query::CacheStatus {
            parse_cache_hit: true,
            result_cache_hit: false,
        };
        assert_eq!(
            format_cache_status(&status),
            "PARTIAL HIT (query cached, results computed)"
        );
    }

    #[test]
    fn test_format_cache_status_result_hit_only() {
        let status = sqry_core::query::CacheStatus {
            parse_cache_hit: false,
            result_cache_hit: true,
        };
        assert_eq!(
            format_cache_status(&status),
            "PARTIAL HIT (query parsed, results cached)"
        );
    }

    #[test]
    fn test_format_cache_status_full_miss() {
        let status = sqry_core::query::CacheStatus {
            parse_cache_hit: false,
            result_cache_hit: false,
        };
        assert_eq!(format_cache_status(&status), "MISS (first run)");
    }

    // ==========================================================================
    // format_execution_steps tests
    // ==========================================================================

    #[test]
    fn test_format_execution_steps_empty() {
        let steps: Vec<sqry_core::query::ExecutionStep> = vec![];
        assert_eq!(format_execution_steps(&steps), "");
    }

    #[test]
    fn test_format_execution_steps_single() {
        let steps = vec![sqry_core::query::ExecutionStep {
            step_num: 1,
            operation: "Parse query".to_string(),
            result_count: 0,
            time_ms: 5,
        }];
        assert_eq!(format_execution_steps(&steps), "  1. Parse query (5ms)");
    }

    #[test]
    fn test_format_execution_steps_multiple() {
        let steps = vec![
            sqry_core::query::ExecutionStep {
                step_num: 1,
                operation: "Parse".to_string(),
                result_count: 100,
                time_ms: 2,
            },
            sqry_core::query::ExecutionStep {
                step_num: 2,
                operation: "Optimize".to_string(),
                result_count: 50,
                time_ms: 3,
            },
            sqry_core::query::ExecutionStep {
                step_num: 3,
                operation: "Execute".to_string(),
                result_count: 25,
                time_ms: 10,
            },
        ];
        let expected = "  1. Parse (2ms)\n  2. Optimize (3ms)\n  3. Execute (10ms)";
        assert_eq!(format_execution_steps(&steps), expected);
    }

    // ==========================================================================
    // expr_has_repo_predicate tests
    // ==========================================================================

    #[test]
    fn test_expr_has_repo_predicate_simple_repo() {
        let query = QueryParser::parse_query("repo:myrepo").unwrap();
        assert!(expr_has_repo_predicate(&query.root));
    }

    #[test]
    fn test_expr_has_repo_predicate_no_repo() {
        let query = QueryParser::parse_query("kind:function").unwrap();
        assert!(!expr_has_repo_predicate(&query.root));
    }

    #[test]
    fn test_expr_has_repo_predicate_nested_and() {
        let query = QueryParser::parse_query("kind:function AND repo:myrepo").unwrap();
        assert!(expr_has_repo_predicate(&query.root));
    }

    #[test]
    fn test_expr_has_repo_predicate_nested_or() {
        let query = QueryParser::parse_query("kind:function OR repo:myrepo").unwrap();
        assert!(expr_has_repo_predicate(&query.root));
    }

    #[test]
    fn test_expr_has_repo_predicate_nested_not() {
        let query = QueryParser::parse_query("NOT repo:myrepo").unwrap();
        assert!(expr_has_repo_predicate(&query.root));
    }

    #[test]
    fn test_expr_has_repo_predicate_complex_no_repo() {
        let query = QueryParser::parse_query("kind:function AND name:foo OR lang:rust").unwrap();
        assert!(!expr_has_repo_predicate(&query.root));
    }

    // ==========================================================================
    // RelationDisplayContext tests
    // ==========================================================================

    #[test]
    fn test_relation_context_no_relations() {
        let ctx = RelationDisplayContext::from_query("kind:function");
        assert!(ctx.caller_targets.is_empty());
        assert!(ctx.callee_targets.is_empty());
    }

    #[test]
    fn test_relation_context_with_callers() {
        let ctx = RelationDisplayContext::from_query("callers:foo");
        assert_eq!(ctx.caller_targets, vec!["foo"]);
        assert!(ctx.callee_targets.is_empty());
    }

    #[test]
    fn test_relation_context_with_callees() {
        let ctx = RelationDisplayContext::from_query("callees:bar");
        assert!(ctx.caller_targets.is_empty());
        assert_eq!(ctx.callee_targets, vec!["bar"]);
    }

    #[test]
    fn test_relation_context_with_both() {
        let ctx = RelationDisplayContext::from_query("callers:foo AND callees:bar");
        assert_eq!(ctx.caller_targets, vec!["foo"]);
        assert_eq!(ctx.callee_targets, vec!["bar"]);
    }

    #[test]
    fn test_relation_context_invalid_query() {
        // Invalid queries should return default context
        let ctx = RelationDisplayContext::from_query("invalid query syntax ???");
        assert!(ctx.caller_targets.is_empty());
        assert!(ctx.callee_targets.is_empty());
    }

    #[test]
    fn test_build_identity_from_qualified_name_preserves_ruby_instance_display() {
        let identity = build_identity_from_qualified_name(
            "Admin::Users::Controller::show",
            "method",
            Some("ruby"),
            false,
        )
        .expect("ruby instance identity");

        assert_eq!(identity.qualified, "Admin::Users::Controller#show");
        assert_eq!(identity.method_kind, CallIdentityKind::Instance);
    }

    #[test]
    fn test_build_identity_from_qualified_name_preserves_ruby_singleton_display() {
        let identity = build_identity_from_qualified_name(
            "Admin::Users::Controller::show",
            "method",
            Some("ruby"),
            true,
        )
        .expect("ruby singleton identity");

        assert_eq!(identity.qualified, "Admin::Users::Controller.show");
        assert_eq!(identity.method_kind, CallIdentityKind::Singleton);
    }

    // ==========================================================================
    // ensure_repo_predicate_not_present tests
    // ==========================================================================

    #[test]
    fn test_ensure_repo_not_present_ok() {
        let result = ensure_repo_predicate_not_present("kind:function");
        assert!(result.is_ok());
    }

    #[test]
    fn test_ensure_repo_not_present_fails_with_repo() {
        let result = ensure_repo_predicate_not_present("repo:myrepo");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("repo: filters are only supported")
        );
    }

    #[test]
    fn test_ensure_repo_not_present_fails_with_nested_repo() {
        let result = ensure_repo_predicate_not_present("kind:function AND repo:myrepo");
        assert!(result.is_err());
    }

    #[test]
    fn test_ensure_repo_not_present_fallback_text_check() {
        // Even if query doesn't parse, text-based check should work
        let result = ensure_repo_predicate_not_present("invalid??? repo:something");
        assert!(result.is_err());
    }

    // ==========================================================================
    // parse_variable_args tests
    // ==========================================================================

    #[test]
    fn test_parse_variable_args_empty() {
        let result = parse_variable_args(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_variable_args_single_key_value() {
        let args = vec!["FOO=bar".to_string()];
        let result = parse_variable_args(&args).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("FOO"), Some(&"bar".to_string()));
    }

    #[test]
    fn test_parse_variable_args_multiple() {
        let args = vec!["A=1".to_string(), "B=hello world".to_string()];
        let result = parse_variable_args(&args).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result.get("A"), Some(&"1".to_string()));
        assert_eq!(result.get("B"), Some(&"hello world".to_string()));
    }

    #[test]
    fn test_parse_variable_args_value_with_equals() {
        // Only the first '=' is the separator; rest is the value
        let args = vec!["KEY=val=ue".to_string()];
        let result = parse_variable_args(&args).unwrap();
        assert_eq!(result.get("KEY"), Some(&"val=ue".to_string()));
    }

    #[test]
    fn test_parse_variable_args_no_equals_errors() {
        let args = vec!["NOEQUALS".to_string()];
        let err = parse_variable_args(&args).unwrap_err();
        assert!(
            err.to_string().contains("Invalid --var format"),
            "Unexpected error: {err}"
        );
    }

    #[test]
    fn test_parse_variable_args_empty_key_errors() {
        let args = vec!["=value".to_string()];
        let err = parse_variable_args(&args).unwrap_err();
        assert!(
            err.to_string().contains("Variable name cannot be empty"),
            "Unexpected error: {err}"
        );
    }

    #[test]
    fn test_parse_variable_args_empty_value_allowed() {
        let args = vec!["KEY=".to_string()];
        let result = parse_variable_args(&args).unwrap();
        assert_eq!(result.get("KEY"), Some(&String::new()));
    }

    // ==========================================================================
    // is_join_query tests
    // ==========================================================================

    #[test]
    fn test_is_join_query_non_join() {
        assert!(!is_join_query("kind:function"));
        assert!(!is_join_query("name:foo AND kind:method"));
    }

    #[test]
    fn test_is_join_query_invalid_query_returns_false() {
        // parse errors → false, not panic
        assert!(!is_join_query("invalid ??? syntax {{{"));
    }

    #[test]
    fn test_is_join_query_positive() {
        // A valid join expression uses the CALLS operator between two sub-queries.
        // The parser recognises `(lhs) CALLS (rhs)` as a Join expression.
        assert!(
            is_join_query("(kind:function) CALLS (kind:function)"),
            "CALLS join expression must be detected as a join query"
        );
    }

    // ==========================================================================
    // detect_pipeline_query tests
    // ==========================================================================

    #[test]
    fn test_detect_pipeline_query_no_pipe_returns_none() {
        let result = detect_pipeline_query("kind:function").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_pipeline_query_invalid_without_pipe_returns_none() {
        // No pipe → even parse errors silently return None
        let result = detect_pipeline_query("invalid query !!!").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_pipeline_query_invalid_with_pipe_errors() {
        // A well-formed pipeline query (base `|` valid stage) must return Ok.
        // The presence of `|` only turns parse *errors* into hard errors; a
        // successful parse must always return Ok(Some(_)).
        let result = detect_pipeline_query("kind:function | count");
        assert!(
            result.is_ok(),
            "A valid pipeline query must return Ok, got: {result:?}"
        );
        assert!(
            result.unwrap().is_some(),
            "A valid pipeline query must return Ok(Some(_))"
        );
    }

    // ==========================================================================
    // apply_symbol_limit tests
    // ==========================================================================

    #[test]
    fn test_apply_symbol_limit_no_truncation() {
        let mut symbols: Vec<DisplaySymbol> = (0..5)
            .map(|i| DisplaySymbol {
                name: format!("sym{i}"),
                qualified_name: format!("sym{i}"),
                kind: "function".to_string(),
                file_path: std::path::PathBuf::from("a.rs"),
                start_line: i,
                start_column: 0,
                end_line: i,
                end_column: 0,
                metadata: std::collections::HashMap::new(),
                caller_identity: None,
                callee_identity: None,
            })
            .collect();

        let info = apply_symbol_limit(&mut symbols, 10);
        assert_eq!(symbols.len(), 5);
        assert!(!info.truncated);
        assert_eq!(info.total_matches, 5);
        assert_eq!(info.limit, 10);
    }

    #[test]
    fn test_apply_symbol_limit_truncates() {
        let mut symbols: Vec<DisplaySymbol> = (0..20)
            .map(|i| DisplaySymbol {
                name: format!("sym{i}"),
                qualified_name: format!("sym{i}"),
                kind: "function".to_string(),
                file_path: std::path::PathBuf::from("a.rs"),
                start_line: i,
                start_column: 0,
                end_line: i,
                end_column: 0,
                metadata: std::collections::HashMap::new(),
                caller_identity: None,
                callee_identity: None,
            })
            .collect();

        let info = apply_symbol_limit(&mut symbols, 5);
        assert_eq!(symbols.len(), 5);
        assert!(info.truncated);
        assert_eq!(info.total_matches, 20);
        assert_eq!(info.limit, 5);
    }

    #[test]
    fn test_apply_symbol_limit_exact_boundary() {
        let mut symbols: Vec<DisplaySymbol> = (0..5)
            .map(|i| DisplaySymbol {
                name: format!("sym{i}"),
                qualified_name: format!("sym{i}"),
                kind: "function".to_string(),
                file_path: std::path::PathBuf::from("a.rs"),
                start_line: i,
                start_column: 0,
                end_line: i,
                end_column: 0,
                metadata: std::collections::HashMap::new(),
                caller_identity: None,
                callee_identity: None,
            })
            .collect();

        let info = apply_symbol_limit(&mut symbols, 5);
        assert_eq!(symbols.len(), 5);
        assert!(!info.truncated, "Exact boundary should not truncate");
    }

    // ==========================================================================
    // u64_to_f64_lossy additional edge cases
    // ==========================================================================

    #[test]
    fn test_u64_to_f64_lossy_large_values_clamp_to_u32_max() {
        let very_large = u64::MAX;
        let result = u64_to_f64_lossy(very_large);
        // Should clamp to u32::MAX
        assert!((result - f64::from(u32::MAX)).abs() < f64::EPSILON);
    }

    // ==========================================================================
    // env_debug_cache_enabled tests
    // ==========================================================================

    #[serial_test::serial]
    #[test]
    fn test_env_debug_cache_disabled_by_default() {
        // In a clean test environment, SQRY_CACHE_DEBUG should not be set
        // (if it is set externally, we skip this test)
        unsafe {
            std::env::remove_var("SQRY_CACHE_DEBUG");
        }
        assert!(!env_debug_cache_enabled());
    }

    #[serial_test::serial]
    #[test]
    fn test_env_debug_cache_enabled_with_1() {
        unsafe {
            std::env::set_var("SQRY_CACHE_DEBUG", "1");
        }
        let result = env_debug_cache_enabled();
        unsafe {
            std::env::remove_var("SQRY_CACHE_DEBUG");
        }
        assert!(result);
    }

    #[serial_test::serial]
    #[test]
    fn test_env_debug_cache_enabled_with_true() {
        unsafe {
            std::env::set_var("SQRY_CACHE_DEBUG", "true");
        }
        let result = env_debug_cache_enabled();
        unsafe {
            std::env::remove_var("SQRY_CACHE_DEBUG");
        }
        assert!(result);
    }

    #[serial_test::serial]
    #[test]
    fn test_env_debug_cache_enabled_with_true_uppercase() {
        unsafe {
            std::env::set_var("SQRY_CACHE_DEBUG", "TRUE");
        }
        let result = env_debug_cache_enabled();
        unsafe {
            std::env::remove_var("SQRY_CACHE_DEBUG");
        }
        assert!(result);
    }

    #[serial_test::serial]
    #[test]
    fn test_env_debug_cache_disabled_with_zero() {
        unsafe {
            std::env::set_var("SQRY_CACHE_DEBUG", "0");
        }
        let result = env_debug_cache_enabled();
        unsafe {
            std::env::remove_var("SQRY_CACHE_DEBUG");
        }
        assert!(!result);
    }

    // ==========================================================================
    // build_query_stats tests
    // ==========================================================================

    #[test]
    fn test_build_query_stats_with_index() {
        let stats = build_query_stats(true, 10);
        assert!(stats.used_index);
    }

    #[test]
    fn test_build_query_stats_without_index() {
        let stats = build_query_stats(false, 10);
        assert!(!stats.used_index);
    }
}
