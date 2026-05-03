//! sqry CLI - Semantic code search tool
//!
//! Search code by what it means, not just what it says.

#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod args;
mod commands;
mod error;
mod index_discovery;
mod output;
mod persistence;
mod plugin_defaults;
mod progress;

/// 16 MB-stack wrapper for tests that call `Cli::parse_from`.
///
/// Clap's recursive subcommand parsing can overflow the default 8 MB debug-mode
/// stack. This macro spawns each test on a 16 MB thread and uses `resume_unwind`
/// so panics (including `#[should_panic]`) propagate correctly.
#[cfg(test)]
macro_rules! large_stack_test {
    ($(#[$attr:meta])* fn $name:ident() $body:block) => {
        $(#[$attr])*
        fn $name() {
            let result = std::thread::Builder::new()
                .stack_size(16 * 1024 * 1024)
                .spawn(move || $body)
                .expect("spawn test thread")
                .join();
            if let Err(panic) = result {
                std::panic::resume_unwind(panic);
            }
        }
    };
}
#[cfg(test)]
pub(crate) use large_stack_test;

use anyhow::{Context, Result};
use args::{Cli, Command, ValidationMode};
use clap::FromArgMatches;
use miette::{GraphicalReportHandler, GraphicalTheme};
use output::OutputStreams;
use sqry_core::query::error::{ExecutionError, QueryError, RichQueryError, ValidationError};

fn main() {
    // Reset SIGPIPE to default (terminate process) so piping to `head`, `less`,
    // etc. doesn't produce "broken pipe" errors. Rust's runtime sets SIG_IGN for
    // SIGPIPE, which causes write errors instead of silent termination.
    reset_sigpipe();

    // Short-circuit version flags before Clap parsing to avoid "missing required
    // argument" errors on subcommands. Without this, `sqry query --version`
    // would require the `<QUERY>` positional before Clap recognizes the flag.
    if version_flag_present() {
        println!("sqry {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Check if JSON output is requested (before parsing to handle errors)
    let json_output = json_output_requested();

    let exit_code = match run() {
        Ok(()) => 0,
        Err(err) => handle_run_error(&err, json_output),
    };

    std::process::exit(exit_code);
}

/// Reset SIGPIPE to default behavior so piping to `head`/`less` exits cleanly.
///
/// Rust's runtime sets `SIG_IGN` for SIGPIPE, which turns pipe closures into
/// `ErrorKind::BrokenPipe` write errors instead of the expected silent process
/// termination. This is the standard fix used by ripgrep, fd, and other CLI tools.
#[cfg(unix)]
fn reset_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn reset_sigpipe() {
    // SIGPIPE is a Unix concept; nothing to do on Windows
}

fn version_flag_present() -> bool {
    std::env::args()
        .skip(1)
        .any(|arg| arg == "--version" || arg == "-V")
}

fn json_output_requested() -> bool {
    std::env::args().any(|arg| arg == "--json" || arg == "-j")
}

/// Compute the effective output format string for `graph *` subcommands
/// from the per-graph `--format` value and the global `--json` flag.
///
/// The global `--json` flag is documented on `Cli` as a top-level
/// output-format switch (`sqry-cli/src/args/mod.rs`), and clap promotes
/// it through the subcommand boundary so users can write
/// `sqry --json graph direct-callers helper`,
/// `sqry graph direct-callers helper --json`, or
/// `sqry graph --format json direct-callers helper` interchangeably.
/// This helper is the single point that reconciles the two surfaces:
///
///   * No `--format`, no `--json`            -> `"text"`         (default).
///   * `--format <fmt>`, no `--json`         -> `"<fmt>"`        (verbatim).
///   * No `--format`, `--json` set           -> `"json"`         (alias).
///   * `--format json`, `--json` set         -> `"json"`         (consistent).
///   * `--format <non-json>`, `--json` set   -> error (loud conflict).
///
/// The conflict diagnostic names both `--format` and `--json` so the
/// caller can see exactly which two flags disagreed; the global
/// `--json` flag would otherwise silently lose to the explicit
/// `--format` value, which is precisely the bug
/// `verivus-oss/sqry#79` / `verivus-oss/sqry#158` reported.
fn resolve_graph_format(format: Option<&str>, json: bool) -> Result<String> {
    match (format, json) {
        (None, false) => Ok("text".to_string()),
        (None, true) => Ok("json".to_string()),
        (Some(fmt), false) => Ok(fmt.to_string()),
        (Some(fmt), true) => {
            if fmt.eq_ignore_ascii_case("json") {
                Ok("json".to_string())
            } else {
                anyhow::bail!(
                    "conflicting output format: --format {fmt} cannot be combined with --json. \
                     The global --json flag is an alias for --format json on graph * subcommands; \
                     drop --json or change --format to json (or text/dot/mermaid/d2 without --json) \
                     to resolve the conflict."
                );
            }
        }
    }
}

fn handle_run_error(err: &anyhow::Error, json_output: bool) -> i32 {
    if let Some(cli_error) = err.downcast_ref::<error::CliError>() {
        return handle_cli_error(cli_error, json_output);
    }

    if let Some(rich_error) = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<RichQueryError>())
    {
        return handle_rich_query_error(rich_error, json_output);
    }

    if let Some(query_error) = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<QueryError>())
    {
        return handle_query_error(query_error, json_output);
    }

    if let Some(validation_error) = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<ValidationError>())
    {
        return handle_validation_error(validation_error, json_output);
    }

    handle_other_error(err, json_output)
}

fn handle_cli_error(cli_error: &error::CliError, json_output: bool) -> i32 {
    // PagerExit doesn't need error output - just propagate the exit code silently
    if let error::CliError::PagerExit(code) = cli_error {
        return *code;
    }

    // NL08: ONNX Runtime missing gets a dedicated multi-line stderr
    // surface so the operator sees the platform-specific install hint
    // on its own line. Exit code 65 (`EX_DATAERR`) is set via
    // `cli_error.exit_code()`.
    if let error::CliError::OnnxRuntimeMissing { hint } = cli_error {
        let mut streams = OutputStreams::new();
        if json_output {
            write_json_error(&mut streams, "sqry::onnx_runtime_missing", hint);
        } else {
            let _ = streams.write_diagnostic("error: ONNX Runtime not found\n");
            let _ = streams.write_diagnostic(&format!("hint: {hint}\n"));
        }
        return cli_error.exit_code();
    }

    let mut streams = OutputStreams::new();
    if json_output {
        let code = match cli_error {
            error::CliError::RuntimeError(_) => "sqry::runtime",
            error::CliError::PagerExit(_) | error::CliError::OnnxRuntimeMissing { .. } => {
                unreachable!() // handled above
            }
        };
        write_json_error(&mut streams, code, &cli_error.to_string());
    } else {
        let _ = streams.write_diagnostic(&format!("Error: {cli_error}"));
    }

    cli_error.exit_code()
}

fn handle_rich_query_error(rich_error: &RichQueryError, json_output: bool) -> i32 {
    let mut streams = OutputStreams::new();

    if json_output {
        // JSON mode: output structured error
        let json_error = rich_error.to_json_value();
        let _ = streams.write_result(&serde_json::to_string_pretty(&json_error).unwrap_or_else(
            |_| {
                // Fallback if JSON serialization fails
                r#"{"error":{"code":"sqry::internal","message":"Failed to serialize error"}}"#
                    .to_string()
            },
        ));
    } else {
        // Terminal mode: format using miette's graphical handler
        let handler = GraphicalReportHandler::new_themed(GraphicalTheme::unicode());
        let mut output = String::new();
        if handler
            .render_report(&mut output, rich_error as &dyn miette::Diagnostic)
            .is_ok()
        {
            let _ = streams.write_diagnostic(&output);
        } else {
            // Fallback to simple formatting
            let _ = streams.write_diagnostic(&format!("Error: {rich_error}"));
        }
    }

    rich_error.exit_code()
}

fn handle_query_error(query_error: &QueryError, json_output: bool) -> i32 {
    let mut streams = OutputStreams::new();

    if json_output {
        // JSON mode: output basic structured error (without source context)
        let code = match query_error {
            QueryError::Lex(_) => "sqry::syntax",
            QueryError::Parse(_) => "sqry::parse",
            QueryError::Validation(_) => "sqry::validation",
            QueryError::Execution(_) => "sqry::execution",
        };
        write_json_error(&mut streams, code, &query_error.to_string());
    } else {
        // Terminal mode
        if let QueryError::Execution(exec_err) = query_error
            && let ExecutionError::LegacyIndexMissingRelations { path, .. } = exec_err
        {
            let warning = format!(
                "Warning: Legacy index detected at {}. Rebuild with `sqry index --force {}` to enable relation queries.",
                path.display(),
                path.display()
            );
            let _ = streams.write_diagnostic(&warning);
        }

        let _ = streams.write_diagnostic(&format!("Error: {query_error}"));
    }

    query_error.exit_code()
}

fn handle_validation_error(validation_error: &ValidationError, json_output: bool) -> i32 {
    let mut streams = OutputStreams::new();
    if json_output {
        write_json_error(
            &mut streams,
            "sqry::validation",
            &validation_error.to_string(),
        );
    } else {
        let _ = streams.write_diagnostic(&format!("Error: {validation_error}"));
    }
    2
}

fn handle_other_error(err: &anyhow::Error, json_output: bool) -> i32 {
    // Other errors exit with code 1
    // Use {e:#} to show full anyhow error chain (alternate format)
    if json_output {
        println!(r#"{{"error":{{"code":"sqry::internal","message":"{err:#}"}}}}"#);
    } else {
        eprintln!("Error: {err:#}");
    }
    1
}

fn write_json_error(streams: &mut OutputStreams, code: &str, message: &str) {
    let json_error = serde_json::json!({
        "error": {
            "code": code,
            "message": message,
        }
    });
    let _ = streams.write_result(
        &serde_json::to_string_pretty(&json_error).unwrap_or_else(|_| {
            format!(r#"{{"error":{{"code":"{code}","message":"{message}"}}}}"#)
        }),
    );
}

#[allow(clippy::too_many_lines)] // CLI dispatch stays centralized for readability.
fn run() -> Result<()> {
    // Expand @alias syntax before clap parsing
    let expanded_args = expand_alias_args()?;

    // Use expanded args for history recording (reflects what actually ran)
    // Skip program name (index 0) for history storage
    let history_argv: Vec<String> = expanded_args[1..].to_vec();

    // Parse command-line arguments with custom taxonomy so help headings apply globally
    let cmd = args::headings::normalize(Cli::command_with_taxonomy());
    let matches = cmd
        .try_get_matches_from(expanded_args)
        .unwrap_or_else(|e| e.exit());
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    // Validate CLI argument constraints (e.g., --headers requires --csv or --tsv)
    if let Some(error) = cli.validate() {
        anyhow::bail!("{error}");
    }

    // Optional: list enabled languages and exit
    if cli.list_languages {
        list_enabled_languages(&cli)?;
        return Ok(());
    }

    // Determine which command to run
    match cli.command.as_deref() {
        Some(Command::Visualize(cmd)) => {
            commands::run_visualize(&cli, cmd).context("Visualize command failed")?;
        }

        // Graph-based queries
        Some(Command::Graph {
            operation,
            path,
            format,
            verbose,
            ..
        }) => {
            let search_path = path.as_deref().unwrap_or(cli.search_path());
            let effective_format = resolve_graph_format(format.as_deref(), cli.json)?;
            commands::run_graph(&cli, operation, search_path, &effective_format, *verbose)
                .context("Graph command failed")?;
        }

        // Explicit search subcommand
        Some(Command::Search {
            pattern,
            path,
            save_as,
            global,
            description,
            validate,
            cfg_filter,
            include_generated,
            macro_boundaries,
        }) => {
            let args = SearchCommandArgs {
                cli: &cli,
                pattern,
                path: path.as_deref(),
                save_as: save_as.as_deref(),
                global: *global,
                description: description.as_deref(),
                validate: *validate,
                history_argv: &history_argv,
                cfg_filter: cfg_filter.as_deref(),
                include_generated: *include_generated,
                macro_boundaries: *macro_boundaries,
            };
            handle_search_command(&args)?;
        }

        // Explicit query subcommand
        Some(Command::Query {
            query,
            path,
            explain,
            verbose,
            session,
            no_parallel,
            save_as,
            global,
            description,
            timeout,
            limit,
            validate,
            var,
            ..
        }) => handle_query_command(
            &cli,
            query,
            path.as_deref(),
            *explain,
            *verbose,
            *session,
            *no_parallel,
            save_as.as_deref(),
            *global,
            description.as_deref(),
            *timeout,
            *limit,
            *validate,
            var,
            &history_argv,
        )?,

        // Structural plan query (DB13 — parser + executor through sqry-db)
        Some(Command::PlanQuery { query, path, limit }) => {
            commands::run_planner_query(&cli, query, path.as_deref(), *limit)
                .context("Plan-query command failed")?;
        }

        // Interactive shell command
        Some(Command::Shell { path }) => {
            commands::run_shell(&cli, path.as_deref().unwrap_or("."))
                .context("Shell command failed")?;
        }

        // Batch command
        Some(Command::Batch(batch_cmd)) => {
            commands::run_batch(
                &cli,
                batch_cmd.path.as_deref().unwrap_or("."),
                batch_cmd.queries.as_path(),
                batch_cmd.output,
                batch_cmd.output_file.as_deref(),
                batch_cmd.continue_on_error,
                batch_cmd.stats,
                batch_cmd.sequential,
            )
            .context("Batch command failed")?;
        }

        // Index command
        Some(Command::Index {
            path,
            force,
            threads,
            status,
            add_to_gitignore,
            no_incremental,
            cache_dir,
            no_compress,
            metrics_format,
            enable_macro_expansion,
            cfg_flags,
            expand_cache,
            classpath,
            no_classpath,
            classpath_depth,
            classpath_file,
            build_system,
            force_classpath,
            ..
        }) => handle_index_command(
            &cli,
            path.as_deref(),
            *force,
            *threads,
            *status,
            *add_to_gitignore,
            *no_incremental,
            cache_dir.as_deref(),
            *no_compress,
            *metrics_format,
            *enable_macro_expansion,
            cfg_flags,
            expand_cache.as_deref(),
            *classpath,
            *no_classpath,
            *classpath_depth,
            classpath_file.as_deref(),
            build_system.as_deref(),
            *force_classpath,
        )?,

        // Analyze command
        Some(Command::Analyze {
            path,
            force,
            threads,
            label_budget,
            density_threshold,
            budget_exceeded_policy,
            no_labels,
        }) => {
            commands::run_analyze(
                &cli,
                path.as_deref(),
                *force,
                *threads,
                *label_budget,
                *density_threshold,
                budget_exceeded_policy.as_deref(),
                *no_labels,
            )
            .context("Analyze command failed")?;
        }

        Some(Command::Lsp { options }) => {
            sqry_lsp::run(options.clone()).context("LSP command failed")?;
        }

        // Update command
        Some(Command::Update {
            path,
            threads,
            stats,
            no_incremental,
            cache_dir,
            classpath,
            no_classpath,
            classpath_depth,
            classpath_file,
            build_system,
            force_classpath,
            ..
        }) => {
            let update_path = path.as_deref().unwrap_or(cli.search_path());
            commands::run_update(
                &cli,
                update_path,
                *threads,
                *stats,
                *no_incremental,
                cache_dir.as_deref(),
                *classpath,
                *no_classpath,
                *classpath_depth,
                classpath_file.as_deref(),
                build_system.as_deref(),
                *force_classpath,
            )
            .context("Update command failed")?;
        }

        // Watch command
        Some(Command::Watch {
            path,
            threads,
            debounce,
            stats,
            build,
            classpath,
            no_classpath,
            classpath_depth,
            classpath_file,
            build_system,
            force_classpath,
            ..
        }) => {
            commands::run_watch(
                &cli,
                path.clone(),
                *threads,
                *debounce,
                *stats,
                *build,
                *classpath,
                *no_classpath,
                *classpath_depth,
                classpath_file.clone(),
                build_system.clone(),
                *force_classpath,
            )
            .context("Watch command failed")?;
        }

        // Repair command
        Some(Command::Repair {
            path,
            fix_orphans,
            fix_dangling,
            recompute_checksum,
            fix_all,
            dry_run,
        }) => {
            let repair_path = path.as_deref().unwrap_or(cli.search_path());
            commands::run_repair(
                &cli,
                repair_path,
                *fix_orphans,
                *fix_dangling,
                *recompute_checksum,
                *fix_all,
                *dry_run,
            )
            .context("Repair command failed")?;
        }

        // Cache command
        Some(Command::Cache { action }) => {
            commands::run_cache(&cli, action).context("Cache command failed")?;
        }

        // Config command
        Some(Command::Config { action }) => {
            handle_config_command(action)?;
        }

        // Completions command
        Some(Command::Completions(completions_cmd)) => {
            commands::run_completions(completions_cmd.shell)
                .context("Completions command failed")?;
        }

        Some(Command::Workspace { action }) => {
            // STEP_8: hard-error on collision between the global `--workspace`
            // flag and the `sqry workspace` subcommand. Each `sqry workspace`
            // subcommand carries its own positional `<workspace>` argument; a
            // silent override would surprise users.
            if cli.workspace.is_some() {
                anyhow::bail!(
                    "the global `--workspace` flag (and `SQRY_WORKSPACE_FILE` env var) \
                     conflicts with the `sqry workspace` subcommand. \
                     The subcommand has its own positional `<workspace>` argument; \
                     drop the global flag (or unset `SQRY_WORKSPACE_FILE`) and pass \
                     the workspace path positionally instead."
                );
            }
            commands::run_workspace(&cli, action).context("Workspace command failed")?;
        }

        // Alias management command
        Some(Command::Alias { action }) => {
            commands::run_alias(&cli, action).context("Alias command failed")?;
        }

        // History management command
        Some(Command::History { action }) => {
            commands::run_history(&cli, action).context("History command failed")?;
        }

        // Insights management command
        Some(Command::Insights { action }) => {
            commands::run_insights(&cli, action).context("Insights command failed")?;
        }

        // Troubleshoot command
        Some(Command::Troubleshoot {
            output,
            dry_run,
            include_trace,
            window,
        }) => {
            commands::run_troubleshoot(&cli, output.as_deref(), *dry_run, *include_trace, window)
                .context("Troubleshoot command failed")?;
        }

        // Natural language query command
        Some(Command::Ask {
            query,
            path,
            auto_execute,
            dry_run,
            threshold,
            model_dir,
            allow_unverified_model,
            allow_model_download,
        }) => {
            let search_path = path.as_deref().unwrap_or(cli.search_path());
            commands::run_ask(
                &cli,
                query,
                search_path,
                *auto_execute,
                *dry_run,
                *threshold,
                model_dir.as_deref(),
                *allow_unverified_model,
                *allow_model_download,
            )
            .context("Ask command failed")?;
        }

        // Duplicate code detection
        Some(Command::Duplicates {
            path,
            r#type,
            threshold,
            max_results,
            exact,
        }) => {
            commands::run_duplicates(
                &cli,
                path.as_deref(),
                r#type,
                *threshold,
                *max_results,
                *exact,
            )
            .context("Duplicates command failed")?;
        }

        // Cycle detection
        Some(Command::Cycles {
            path,
            r#type,
            min_depth,
            max_depth,
            include_self,
            max_results,
        }) => {
            commands::run_cycles(
                &cli,
                path.as_deref(),
                r#type,
                *min_depth,
                *max_depth,
                *include_self,
                *max_results,
            )
            .context("Cycles command failed")?;
        }

        // Unused code detection
        Some(Command::Unused {
            path,
            scope,
            lang,
            kind,
            max_results,
        }) => {
            commands::run_unused(
                &cli,
                path.as_deref(),
                scope,
                lang.as_deref(),
                kind.as_deref(),
                *max_results,
            )
            .context("Unused command failed")?;
        }

        // Graph export
        Some(Command::Export {
            path,
            format,
            direction,
            filter_lang,
            filter_edge,
            highlight_cross,
            show_details,
            show_labels,
            output,
        }) => {
            commands::run_export(
                &cli,
                path.as_deref(),
                format,
                direction,
                filter_lang.as_deref(),
                filter_edge.as_deref(),
                *highlight_cross,
                *show_details,
                *show_labels,
                output.as_deref(),
            )
            .context("Export command failed")?;
        }

        // Explain command
        Some(Command::Explain {
            file,
            symbol,
            path,
            no_context,
            no_relations,
        }) => {
            commands::run_explain(
                &cli,
                file,
                symbol,
                path.as_deref(),
                !no_context,
                !no_relations,
            )
            .context("Explain command failed")?;
        }

        // Similar command
        Some(Command::Similar {
            file,
            symbol,
            path,
            threshold,
            limit,
        }) => {
            commands::run_similar(&cli, file, symbol, path.as_deref(), *threshold, *limit)
                .context("Similar command failed")?;
        }

        // Subgraph command
        Some(Command::Subgraph {
            symbols,
            path,
            depth,
            max_nodes,
            no_callers,
            no_callees,
            include_imports,
        }) => {
            commands::run_subgraph(
                &cli,
                symbols,
                path.as_deref(),
                *depth,
                *max_nodes,
                !no_callers,
                !no_callees,
                *include_imports,
            )
            .context("Subgraph command failed")?;
        }

        // Impact command
        Some(Command::Impact {
            symbol,
            path,
            depth,
            limit,
            direct_only,
            show_files,
        }) => {
            commands::run_impact(
                &cli,
                symbol,
                path.as_deref(),
                *depth,
                *limit,
                !direct_only,
                *show_files,
            )
            .context("Impact command failed")?;
        }

        // Diff command
        Some(Command::Diff {
            base,
            target,
            path,
            limit,
            kind,
            change_type,
            ..
        }) => {
            let kinds: Vec<String> = kind
                .as_ref()
                .map(|s| s.split(',').map(String::from).collect())
                .unwrap_or_default();
            let change_types: Vec<String> = change_type
                .as_ref()
                .map(|s| s.split(',').map(String::from).collect())
                .unwrap_or_default();
            commands::run_diff(
                &cli,
                base,
                target,
                path.as_deref(),
                *limit,
                &kinds,
                &change_types,
            )
            .context("Diff command failed")?;
        }

        // Hier (hierarchical search) command
        Some(Command::Hier {
            query,
            path,
            limit,
            max_files,
            context,
            kind,
            lang,
        }) => {
            let kinds: Vec<String> = kind
                .as_ref()
                .map(|s| s.split(',').map(String::from).collect())
                .unwrap_or_default();
            let languages: Vec<String> = lang
                .as_ref()
                .map(|s| s.split(',').map(String::from).collect())
                .unwrap_or_default();
            commands::run_hier_search(
                &cli,
                query,
                path.as_deref(),
                *limit,
                *max_files,
                *context,
                &kinds,
                &languages,
            )
            .context("Hierarchical search command failed")?;
        }

        // MCP setup/status command
        Some(Command::Mcp { command }) => {
            commands::mcp::run(command).context("MCP command failed")?;
        }

        // Daemon lifecycle management
        Some(Command::Daemon { action }) => {
            commands::daemon::run(&cli, action).context("Daemon command failed")?;
        }

        // No subcommand - use pattern shorthand
        None => handle_no_command(&cli, &history_argv)?,
    }

    Ok(())
}

fn list_enabled_languages(cli: &Cli) -> Result<()> {
    let root = std::path::Path::new(cli.search_path());
    let manager = plugin_defaults::resolve_plugin_selection(
        cli,
        root,
        plugin_defaults::PluginSelectionMode::FreshWrite,
    )?
    .plugin_manager;

    println!("Enabled languages ({}):", manager.plugins().len());
    for p in manager.plugins() {
        let m = p.metadata();
        let exts = p.extensions().join(", ");
        println!("- {} (id: {}, v{}): [{}]", m.name, m.id, m.version, exts);
    }

    Ok(())
}

#[allow(dead_code)] // Macro boundary fields used when search filtering is wired up
struct SearchCommandArgs<'a> {
    cli: &'a Cli,
    pattern: &'a str,
    path: Option<&'a str>,
    save_as: Option<&'a str>,
    global: bool,
    description: Option<&'a str>,
    validate: ValidationMode,
    history_argv: &'a [String],
    cfg_filter: Option<&'a str>,
    include_generated: bool,
    macro_boundaries: bool,
}

fn handle_search_command(args: &SearchCommandArgs<'_>) -> Result<()> {
    let search_path = args.path.unwrap_or(args.cli.search_path());

    // Validate index before execution if requested
    if let Err(code) =
        validate_index_if_requested(args.cli, search_path, args.validate, args.cli.auto_rebuild)
    {
        std::process::exit(code);
    }

    let result = commands::run_search(args.cli, args.pattern, search_path);

    // Record in history (after execution, regardless of result)
    // Pass expanded argv (without program name) to capture what actually ran
    record_history(search_path, "search", args.history_argv, result.is_ok());

    result.context("Search command failed")?;

    // Save as alias if requested
    if let Some(alias_name) = args.save_as {
        commands::save_search_alias(
            args.cli,
            alias_name,
            args.pattern,
            args.global,
            args.description,
        )
        .context("Failed to save alias")?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::fn_params_excessive_bools)] // CLI flags map directly to booleans.
fn handle_query_command(
    cli: &Cli,
    query: &str,
    path: Option<&str>,
    explain: bool,
    verbose: bool,
    session: bool,
    no_parallel: bool,
    save_as: Option<&str>,
    global: bool,
    description: Option<&str>,
    timeout: Option<u64>,
    limit: Option<usize>,
    validate: ValidationMode,
    variables: &[String],
    history_argv: &[String],
) -> Result<()> {
    // Validate mutual exclusivity of --session and --no-parallel
    if session && no_parallel {
        anyhow::bail!(
            "--session and --no-parallel are mutually exclusive. \
            Session mode caches the executor configuration, so subsequent queries \
            cannot toggle parallel execution. Use either --session (for performance) \
            or --no-parallel (for A/B testing), but not both."
        );
    }

    // STEP_8 precedence: positional `<path>` wins; otherwise fall back to the
    // global `--workspace` / `SQRY_WORKSPACE_FILE`; otherwise `cli.search_path()`.
    // Non-UTF-8 workspace paths surface as a hard error rather than silently
    // falling back to `cli.search_path()` (STEP_8 codex iter1 fix).
    let search_path = cli.resolve_subcommand_path(path)?;

    // Validate index before execution if requested
    if let Err(code) = validate_index_if_requested(cli, search_path, validate, cli.auto_rebuild) {
        std::process::exit(code);
    }

    // Don't add .context() here - let QueryError propagate for proper exit code
    let result = commands::run_query(
        cli,
        query,
        search_path,
        explain,
        verbose,
        session,
        no_parallel,
        timeout,
        limit,
        variables,
    );

    // Record in history (after execution, regardless of result)
    // Pass expanded argv (without program name) to capture what actually ran
    record_history(search_path, "query", history_argv, result.is_ok());

    result?;

    // Save as alias if requested
    if let Some(alias_name) = save_as {
        commands::save_query_alias(cli, alias_name, query, global, description)
            .context("Failed to save alias")?;
    }

    Ok(())
}

/// Validate index staleness before query/search execution.
///
/// Returns `Ok(())` if validation passes or is skipped.
/// Returns `Err(exit_code)` if validation fails in strict mode.
fn validate_index_if_requested(
    cli: &Cli,
    search_path: &str,
    validate: ValidationMode,
    auto_rebuild: bool,
) -> Result<(), i32> {
    use commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
    use std::path::Path;

    const ORPHAN_THRESHOLD: f64 = 0.20;

    // Skip validation if not requested
    if matches!(validate, ValidationMode::Off) {
        return Ok(());
    }

    // Try to load the graph to check for orphaned files
    let search_root = Path::new(search_path);
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(search_root);

    if !storage.exists() {
        // No index exists - validation not applicable
        return Ok(());
    }

    // Load the graph from the project root (not the graph directory)
    let config = GraphLoadConfig::default();
    let Ok(graph) = load_unified_graph_for_cli(search_root, &config, cli) else {
        // If we can't load the graph, skip validation
        return Ok(());
    };

    // Count total unique files and orphaned files
    let files = graph.files();
    let mut total_files = 0usize;
    let mut orphaned_files = 0usize;

    // Iterate over unique files in the registry (not nodes)
    for (_file_id, file_path) in files.iter() {
        total_files += 1;
        // FileRegistry stores paths that may be absolute or relative
        // If absolute, join will return the absolute path as-is
        // If relative, join will prepend search_path
        let full_path = if file_path.is_absolute() {
            file_path.to_path_buf()
        } else {
            search_root.join(file_path.as_ref())
        };
        if !full_path.exists() {
            orphaned_files += 1;
        }
    }

    // Calculate orphan ratio (avoid division by zero)
    // Check against threshold (20%)
    let orphan_ratio = if total_files > 0 {
        let orphaned_f = f64::from(u32::try_from(orphaned_files).unwrap_or(u32::MAX));
        let total_f = f64::from(u32::try_from(total_files).unwrap_or(u32::MAX));
        orphaned_f / total_f
    } else {
        0.0
    };
    let is_stale = orphan_ratio > ORPHAN_THRESHOLD;

    match validate {
        ValidationMode::Fail if is_stale => {
            if auto_rebuild {
                eprintln!(
                    "Index is stale ({:.1}% of files missing). Rebuilding because --auto-rebuild is set.",
                    orphan_ratio * 100.0
                );
                if let Err(err) = commands::run_index(
                    cli,
                    search_path,
                    true,
                    None,
                    false,
                    false,
                    None,
                    false,
                    false,
                    &[],
                    None,
                    false,
                    false,
                    crate::args::ClasspathDepthArg::Full,
                    None,
                    None,
                    false,
                ) {
                    eprintln!("Error: auto-rebuild failed: {err}");
                    return Err(2);
                }
                return Ok(());
            }
            eprintln!(
                "Error: Index is stale ({:.1}% of files missing). \
                Run 'sqry index --force' to rebuild.",
                orphan_ratio * 100.0
            );
            Err(2)
        }
        ValidationMode::Warn if is_stale => {
            eprintln!(
                "Warning: Index is stale ({:.1}% of files missing). \
                Consider running 'sqry index --force' to rebuild.",
                orphan_ratio * 100.0
            );
            Ok(())
        }
        _ => Ok(()),
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::fn_params_excessive_bools)] // CLI flags map directly to booleans.
#[allow(unused_variables)] // Classpath params unused without jvm-classpath feature.
#[allow(clippy::used_underscore_binding)] // underscore-prefixed flag mirrors CLI arg naming
fn handle_index_command(
    cli: &Cli,
    path: Option<&str>,
    force: bool,
    threads: Option<usize>,
    status: bool,
    add_to_gitignore: bool,
    no_incremental: bool,
    cache_dir: Option<&str>,
    no_compress: bool,
    metrics_format: crate::args::MetricsFormat,
    enable_macro_expansion: bool,
    cfg_flags: &[String],
    expand_cache: Option<&std::path::Path>,
    classpath: bool,
    _no_classpath: bool,
    classpath_depth: crate::args::ClasspathDepthArg,
    classpath_file: Option<&std::path::Path>,
    build_system: Option<&str>,
    force_classpath: bool,
) -> Result<()> {
    // STEP_8 precedence: positional `<path>` wins; otherwise fall back to the
    // global `--workspace` / `SQRY_WORKSPACE_FILE`; otherwise `cli.search_path()`.
    // Non-UTF-8 workspace paths surface as a hard error rather than silently
    // falling back to `cli.search_path()` (STEP_8 codex iter1 fix).
    let index_path = cli.resolve_subcommand_path(path)?;

    if enable_macro_expansion {
        eprintln!("WARNING: Macro expansion enabled. This executes build scripts and proc macros.");
        eprintln!("         Only use on trusted codebases.");
    }

    if status {
        commands::run_index_status(cli, index_path, metrics_format)
            .context("Index status command failed")?;
    } else {
        commands::run_index(
            cli,
            index_path,
            force,
            threads,
            add_to_gitignore,
            no_incremental,
            cache_dir,
            no_compress,
            enable_macro_expansion,
            cfg_flags,
            expand_cache.map(std::path::Path::new),
            classpath,
            _no_classpath,
            classpath_depth,
            classpath_file,
            build_system,
            force_classpath,
        )
        .context("Index command failed")?;
    }
    Ok(())
}

fn handle_config_command(action: &args::ConfigAction) -> Result<()> {
    use args::ConfigAction;

    match action {
        ConfigAction::Init { path, force } => {
            commands::run_config_init(path.as_deref(), *force).context("Config init failed")?;
        }
        ConfigAction::Show { path, json, key } => {
            commands::run_config_show(path.as_deref(), *json, key.as_deref())
                .context("Config show failed")?;
        }
        ConfigAction::Set {
            path,
            key,
            value,
            yes,
        } => {
            commands::run_config_set(path.as_deref(), key, value, *yes)
                .context("Config set failed")?;
        }
        ConfigAction::Get { path, key } => {
            commands::run_config_get(path.as_deref(), key).context("Config get failed")?;
        }
        ConfigAction::Validate { path } => {
            commands::run_config_validate(path.as_deref()).context("Config validate failed")?;
        }
        ConfigAction::Alias(alias_action) => {
            handle_config_alias_action(alias_action)?;
        }
    }

    Ok(())
}

fn handle_config_alias_action(action: &args::ConfigAliasAction) -> Result<()> {
    use args::ConfigAliasAction;

    match action {
        ConfigAliasAction::Set {
            path,
            name,
            query,
            description,
        } => {
            commands::run_config_alias_set(path.as_deref(), name, query, description.as_deref())
                .context("Config alias set failed")?;
        }
        ConfigAliasAction::List { path, json } => {
            commands::run_config_alias_list(path.as_deref(), *json)
                .context("Config alias list failed")?;
        }
        ConfigAliasAction::Remove { path, name } => {
            commands::run_config_alias_remove(path.as_deref(), name)
                .context("Config alias remove failed")?;
        }
    }

    Ok(())
}

fn handle_no_command(cli: &Cli, history_argv: &[String]) -> Result<()> {
    if let Some(pattern) = &cli.pattern {
        // Run search with pattern shorthand
        let result = commands::run_search(cli, pattern, cli.search_path());

        // Record in history (expanded argv captures what actually ran)
        record_history(cli.search_path(), "search", history_argv, result.is_ok());

        result.context("Search command failed")?;
        return Ok(());
    }

    // No pattern and no subcommand - print usage hint and exit with error
    eprintln!("Error: No pattern or command provided");
    eprintln!();
    eprintln!("Usage: sqry <PATTERN> [PATH]");
    eprintln!("       sqry search <PATTERN> [PATH]");
    eprintln!("       sqry query <QUERY> [PATH]");
    eprintln!("       sqry index [PATH]");
    eprintln!("       sqry update [PATH]");
    eprintln!("       sqry cache <stats|clear>");
    eprintln!("       sqry daemon {{start,stop,status,logs}}");
    eprintln!();
    eprintln!("Try 'sqry --help' for more information.");
    std::process::exit(2)
}

/// Expand @alias syntax in command-line arguments.
///
/// If the first positional argument (after global flags) starts with `@`,
/// it is treated as an alias reference and expanded to the stored command.
///
/// Examples:
///   `sqry @my-funcs` -> `sqry query "kind:function" .`
///   `sqry @my-funcs src/` -> `sqry query "kind:function" src/`
///   `sqry --json @my-funcs` -> `sqry --json query "kind:function" .`
/// Global flags that ALWAYS take a value (the next argument).
/// This MUST include ALL global flags defined in args/mod.rs that require a value,
/// otherwise @alias expansion will fail when those flags precede the alias.
const FLAGS_WITH_VALUES: &[&str] = &[
    // Output control
    "--columns",
    "--limit",
    "--format",
    // Match behaviour / filtering
    "--kind",
    "-k",
    "--lang",
    "-l",
    "--max-depth",
    // Fuzzy search
    "--fuzzy-algorithm",
    "--fuzzy-threshold",
    "--fuzzy-max-candidates",
    // Index validation (P1-14)
    "--validate",
    "--threshold-dangling-refs",
    "--threshold-orphaned-files",
    "--threshold-id-gaps",
    // Hybrid search
    "--context",
    "-C",
    "--max-text-results",
    // Configuration
    "--config-dir",
    "--path",
    "--type",
];

/// Flags with optional values (`num_args` = 0..=1).
/// These flags may or may not consume the next argument depending on whether
/// the next arg looks like a value (numeric) or something else (@alias, -flag).
const FLAGS_WITH_OPTIONAL_VALUES: &[&str] = &["--preview", "-p"];

struct AliasScan {
    alias_index: Option<usize>,
    remaining_path: Option<String>,
}

fn expand_alias_args() -> Result<Vec<String>> {
    use persistence::{AliasManager, PersistenceConfig, open_shared_index};
    use std::path::Path;

    let args: Vec<String> = std::env::args().collect();
    let scan = scan_alias_args(&args);

    let Some(idx) = scan.alias_index else {
        // No alias found, return original args
        return Ok(args);
    };

    let alias_name = alias_name_from_arg(&args[idx])?;
    let lookup_path = scan.remaining_path.as_deref().unwrap_or(".");
    let config = PersistenceConfig::from_env();
    let index = if let Ok(idx) = open_shared_index(Some(Path::new(lookup_path)), config) {
        idx
    } else {
        // If we can't open the index, try with current directory
        let config = PersistenceConfig::from_env();
        open_shared_index(Some(Path::new(".")), config)?
    };

    let manager = AliasManager::new(index);
    let alias_with_scope = load_alias(&manager, alias_name)?;

    Ok(build_expanded_args(
        &args,
        idx,
        scan.remaining_path.as_deref(),
        &alias_with_scope,
    ))
}

fn scan_alias_args(args: &[String]) -> AliasScan {
    let mut skip_next = false;

    for (i, arg) in args.iter().enumerate().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }

        if arg.starts_with('-') {
            if should_skip_next_arg(args, i, arg) {
                skip_next = true;
            }
            continue;
        }

        // Check if this looks like a subcommand (not starting with @)
        if !arg.starts_with('@') {
            // Not an alias, return original args
            return AliasScan {
                alias_index: None,
                remaining_path: None,
            };
        }

        // Found an @alias pattern
        let remaining_path =
            (i + 1 < args.len() && !args[i + 1].starts_with('-')).then(|| args[i + 1].clone());

        return AliasScan {
            alias_index: Some(i),
            remaining_path,
        };
    }

    AliasScan {
        alias_index: None,
        remaining_path: None,
    }
}

fn should_skip_next_arg(args: &[String], index: usize, arg: &str) -> bool {
    // Handle both --flag=value and --flag value forms
    if arg.contains('=') {
        return false;
    }

    // Check if this flag ALWAYS takes a value
    if FLAGS_WITH_VALUES.contains(&arg) {
        return true;
    }

    // Optional-value flag (like --preview): only skip if next looks numeric
    if FLAGS_WITH_OPTIONAL_VALUES.contains(&arg) {
        return args
            .get(index + 1)
            .is_some_and(|next_arg| is_optional_flag_value(next_arg));
    }

    false
}

fn is_optional_flag_value(arg: &str) -> bool {
    !arg.starts_with('@') && !arg.starts_with('-') && arg.parse::<usize>().is_ok()
}

fn alias_name_from_arg(arg: &str) -> Result<&str> {
    let alias_name = arg.strip_prefix('@').unwrap_or("");
    if alias_name.is_empty() {
        anyhow::bail!("Empty alias name: '@' must be followed by an alias name");
    }
    Ok(alias_name)
}

fn load_alias(
    manager: &persistence::AliasManager,
    alias_name: &str,
) -> Result<persistence::AliasWithScope> {
    use persistence::AliasError;

    match manager.get(alias_name) {
        Ok(a) => Ok(a),
        Err(AliasError::NotFound { name }) => {
            anyhow::bail!(
                "Unknown alias '@{name}'. Use 'sqry alias list' to see available aliases."
            );
        }
        Err(e) => Err(e.into()),
    }
}

fn build_expanded_args(
    args: &[String],
    alias_index: usize,
    remaining_path: Option<&str>,
    alias_with_scope: &persistence::AliasWithScope,
) -> Vec<String> {
    let mut expanded = vec![args[0].clone()]; // Program name

    // Add global flags that appeared before the alias
    expanded.extend(args.iter().take(alias_index).skip(1).cloned());

    // Add the command (search or query)
    expanded.push(alias_with_scope.alias.command.clone());

    // Add the stored arguments
    expanded.extend(alias_with_scope.alias.args.iter().cloned());

    // Add the path if provided, otherwise use "."
    let has_path = remaining_path.is_some();
    if let Some(path) = remaining_path {
        expanded.push(path.to_string());
    } else {
        expanded.push(".".to_string());
    }

    // Add any remaining arguments after the path
    let path_offset = if has_path { 2 } else { 1 };
    expanded.extend(args.iter().skip(alias_index + path_offset).cloned());

    expanded
}

/// Record a command execution in history.
///
/// This is a best-effort operation - failures are logged but don't affect
/// command execution. History recording can be disabled via `SQRY_NO_HISTORY=1`.
///
/// # Arguments
///
/// * `search_path` - The actual path being searched/queried (for per-project history)
/// * `command` - The command name (search/query)
/// * `argv` - The complete command-line arguments (excluding program name)
/// * `success` - Whether the command succeeded
fn record_history(search_path: &str, command: &str, argv: &[String], success: bool) {
    use persistence::{HistoryManager, PersistenceConfig, open_shared_index};
    use std::path::{Path, PathBuf};

    // Create config - checks SQRY_NO_HISTORY internally
    let config = PersistenceConfig::from_env();

    // Skip if history is disabled
    if !config.history_enabled {
        return;
    }

    // Try to open index and record - silently ignore failures
    let result = (|| -> anyhow::Result<()> {
        let index = open_shared_index(Some(Path::new(search_path)), config)?;
        let manager = HistoryManager::new(index);

        // Use the actual search path as the working directory context
        // Resolve to absolute path for consistency
        let working_dir =
            std::fs::canonicalize(search_path).unwrap_or_else(|_| PathBuf::from(search_path));

        // Duration is None since we don't track execution time in the simple case
        manager.record(command, argv, &working_dir, success, None)?;
        Ok(())
    })();

    // Log failures in debug mode but don't affect command execution
    if let Err(e) = result {
        log::debug!("Failed to record history: {e}");
    }
}

// ---------------------------------------------------------------------------
// U4 wiring smoke tests — verify `Command::Daemon` is dispatched from main.rs
// and `commands::daemon::run` is reachable from the dispatch table.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod wiring_tests {
    use super::*;
    use clap::Parser;

    large_stack_test! {
    /// Smoke test: `sqry daemon start` reaches the Daemon variant with defaults.
    ///
    /// These tests exercise parse wiring (spec: "compilation smoke tests via
    /// `Cli::parse_from`"). The `run()` match arm at main.rs:837 is a compile-time
    /// wiring guarantee: if `commands::daemon::run` signature changes, this module
    /// would fail to compile.
    #[test]
    fn daemon_start_parses_wiring() {
        let cli = args::Cli::parse_from(["sqry", "daemon", "start"]);
        if let Some(args::Command::Daemon { action }) = cli.command.as_deref() {
            assert!(
                matches!(
                    action.as_ref(),
                    args::DaemonAction::Start { sqryd_path: None, timeout: 10 }
                ),
                "DaemonAction::Start must have default sqryd_path=None, timeout=10"
            );
        } else {
            panic!("Expected Command::Daemon");
        }
    }
    }

    large_stack_test! {
    /// Smoke test: `sqry daemon stop --timeout 30` reaches the Daemon::Stop variant
    /// with timeout=30.
    #[test]
    fn daemon_stop_parses_wiring() {
        let cli = args::Cli::parse_from(["sqry", "daemon", "stop", "--timeout", "30"]);
        if let Some(args::Command::Daemon { action }) = cli.command.as_deref() {
            assert!(
                matches!(action.as_ref(), args::DaemonAction::Stop { timeout: 30 }),
                "DaemonAction::Stop must have timeout=30 when --timeout 30 is passed"
            );
        } else {
            panic!("Expected Command::Daemon");
        }
    }
    }

    large_stack_test! {
    /// Smoke test: `sqry daemon status --json` reaches the Daemon variant.
    #[test]
    fn daemon_status_json_parses_wiring() {
        let cli = args::Cli::parse_from(["sqry", "daemon", "status", "--json"]);
        if let Some(args::Command::Daemon { action }) = cli.command.as_deref() {
            assert!(
                matches!(action.as_ref(), args::DaemonAction::Status { json: true }),
                "DaemonAction::Status must have json=true when --json is passed"
            );
        } else {
            panic!("Expected Command::Daemon");
        }
    }
    }

    large_stack_test! {
    /// Smoke test: `sqry daemon logs -f` reaches the Daemon::Logs variant with follow=true.
    #[test]
    fn daemon_logs_follow_parses_wiring() {
        let cli = args::Cli::parse_from(["sqry", "daemon", "logs", "-f"]);
        if let Some(args::Command::Daemon { action }) = cli.command.as_deref() {
            assert!(
                matches!(action.as_ref(), args::DaemonAction::Logs { follow: true, .. }),
                "DaemonAction::Logs must have follow=true when -f is passed"
            );
        } else {
            panic!("Expected Command::Daemon");
        }
    }
    }
}
