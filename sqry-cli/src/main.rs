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
use args::{Cli, Command, RevisionQueryArgs, ValidationMode};
use clap::FromArgMatches;
use miette::{GraphicalReportHandler, GraphicalTheme};
use output::OutputStreams;
use sqry_core::query::error::{ExecutionError, QueryError, RichQueryError, ValidationError};

fn main() {
    // Reset SIGPIPE to default (terminate process) so piping to `head`, `less`,
    // etc. doesn't produce "broken pipe" errors. Rust's runtime sets SIG_IGN for
    // SIGPIPE, which causes write errors instead of silent termination.
    reset_sigpipe();

    // Install a `log` backend so `log::info!` / `debug!` calls from anywhere
    // in the workspace (notably `sqry_core::build` — Pass 5b stats, Pass 5
    // cross-language stats, graph kernel events) reach stderr when the user
    // opts in via `SQRY_LOG` or `RUST_LOG`. Without this init the `log`
    // crate runs with the no-op logger and all events are silently dropped,
    // which is what made Pass 5b's `Pass 5b end: binding=... typematch=...`
    // line unreachable from CLI before this change.
    //
    // Precedence: SQRY_LOG > RUST_LOG > default `off` (silent). Matches the
    // documented sqry CLI convention (see `commands/search.rs::verbose_from_env`).
    init_logging();

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

/// Initialise the `log` backend so `log::info!` / `debug!` events from
/// `sqry_core::build` and other workspace targets reach stderr.
///
/// Precedence:
///   1. `SQRY_LOG` (sqry's documented switch) is read first; its value is
///      copied into the `env_logger` Builder.
///   2. Otherwise `RUST_LOG` is honoured directly.
///   3. Otherwise the default filter is `off` — silent, matches sqry's
///      "default silent" CLI contract.
///
/// The init is idempotent: `try_init` is used so embedded callers (tests,
/// `sqry-cli` used as a library) that already installed a logger are not
/// clobbered.
fn init_logging() {
    let filter = match (
        std::env::var("SQRY_LOG").ok(),
        std::env::var("RUST_LOG").ok(),
    ) {
        (Some(v), _) | (_, Some(v)) if !v.trim().is_empty() => v,
        _ => "off".to_string(),
    };
    let _ = env_logger::Builder::new()
        .parse_filters(&filter)
        .target(env_logger::Target::Stderr)
        .format_timestamp(None)
        .format_module_path(false)
        .try_init();
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

    let mut streams = OutputStreams::new();
    if json_output {
        let code = match cli_error {
            error::CliError::RuntimeError(_) => "sqry::runtime",
            error::CliError::PagerExit(_) => {
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
            // Cooperative cancellation surfaced from the query
            // evaluator (per `A_cancellation.md` §4 + `00_contracts.md`
            // §3.CC-1). The CLI does not currently install a
            // cancellation token, but the variant must be exhaustively
            // matched so adding the foundation does not break the
            // CLI build.
            QueryError::Cancelled => "sqry::cancelled",
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
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());

    // `--exact` historically only bound at the top level (`sqry --exact PAT`),
    // so `sqry search PAT --exact` was rejected even though `sqry search --help`
    // advertises it (#511). The flag now also lives on the `search` subcommand;
    // fold its value into `cli.exact` (the field the search path actually reads)
    // so both spellings drive the identical exact-match path. The fold only ever
    // sets the flag on, never clears it, so the top-level shorthand is untouched.
    if matches!(
        cli.command.as_deref(),
        Some(Command::Search { exact: true, .. })
    ) {
        cli.exact = true;
    }

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
            // `exact` is folded into `cli.exact` right after parsing (see the
            // #511 note above), which is what the search path reads, so the
            // per-arm binding is intentionally ignored here.
            exact: _,
            save_as,
            global,
            description,
            validate,
            cfg_filter,
            include_generated,
            macro_boundaries,
            revision,
            verbose,
        }) => {
            let args = SearchCommandArgs {
                cli: &cli,
                pattern,
                path: path.as_deref(),
                save_as: save_as.as_deref(),
                scope: if *global {
                    SearchAliasScope::Global
                } else {
                    SearchAliasScope::Workspace
                },
                description: description.as_deref(),
                validate: *validate,
                history_argv: &history_argv,
                cfg_filter: cfg_filter.as_deref(),
                include_generated: *include_generated,
                macro_boundaries: *macro_boundaries,
                revision,
                verbose: *verbose,
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
            revision,
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
            revision,
            &history_argv,
        )?,

        // Structural plan query (DB13 — parser + executor through sqry-db)
        Some(Command::PlanQuery { query, path, limit }) => {
            commands::run_planner_query(&cli, query, path.as_deref(), *limit)
                .context("Plan-query command failed")?;
        }

        // Declarative rule-layer execution (P5 L5)
        Some(Command::Rules { action }) => {
            commands::run_rules(&cli, action).context("Rules command failed")?;
        }

        // T3.7 Cluster G-ext: context-propagation analysis CLI surface.
        Some(Command::ContextPropagation {
            path,
            scope,
            mode,
            limit,
        }) => {
            commands::run_context_propagation(&cli, path.as_deref(), scope.as_str(), *mode, *limit)
                .context("Context-propagation command failed")?;
        }

        // Interactive shell command
        Some(Command::Shell { path, .. }) => {
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
            metrics_format,
            cfg_flags,
            expand_cache,
            classpath,
            no_classpath,
            classpath_depth,
            classpath_file,
            build_system,
            force_classpath,
            allow_nested,
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
            *metrics_format,
            cfg_flags,
            expand_cache.as_deref(),
            *classpath,
            *no_classpath,
            *classpath_depth,
            classpath_file.as_deref(),
            build_system.as_deref(),
            *force_classpath,
            *allow_nested,
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
            // C094b: forward the global `--workspace <PATH>` flag (or its
            // `SQRY_WORKSPACE_FILE` env equivalent) into the LSP session so
            // root resolution honours the operator's explicit workspace
            // selection, ahead of the legacy `--index-root` fallback.
            let mut lsp_options = options.clone();
            if lsp_options.workspace.is_none() {
                lsp_options.workspace.clone_from(&cli.workspace);
            }
            sqry_lsp::run(lsp_options).context("LSP command failed")?;
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

        // Repository orientation report
        Some(Command::Overview {
            path,
            format,
            top,
            sections,
            group_depth,
            output,
            no_index,
            redaction,
        }) => {
            commands::run_overview(
                &cli,
                &commands::OverviewOptions {
                    path: path.as_deref(),
                    format,
                    top: *top,
                    sections: sections.as_deref(),
                    group_depth: *group_depth,
                    output: output.as_deref(),
                    no_index: *no_index,
                    redaction,
                },
            )
            .context("Overview command failed")?;
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
            symbol,
            file,
            max_depth,
            max_results,
        }) => {
            commands::run_export(commands::ExportArgs {
                cli: &cli,
                path: path.as_deref(),
                format,
                direction,
                filter_lang: filter_lang.as_deref(),
                filter_edge: filter_edge.as_deref(),
                highlight_cross: *highlight_cross,
                show_details: *show_details,
                show_labels: *show_labels,
                output_file: output.as_deref(),
                symbol: symbol.as_deref(),
                file: file.as_deref(),
                max_depth: *max_depth,
                max_results: *max_results,
            })
            .context("Export command failed")?;
        }

        // Explain command
        Some(Command::Explain {
            file,
            symbol,
            path,
            in_file,
            line,
            no_context,
            no_relations,
        }) => {
            commands::run_explain(
                &cli,
                file,
                symbol,
                path.as_deref(),
                in_file.as_deref(),
                *line,
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

        // Shape-match command (body-shape structural neighbours, U08)
        Some(Command::ShapeMatch {
            symbol,
            file,
            path,
            threshold,
            limit,
        }) => {
            commands::run_shape_match(
                &cli,
                symbol,
                file.as_deref(),
                path.as_deref(),
                *threshold,
                *limit,
            )
            .context("shape-match command failed")?;
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
            in_file,
            line,
            depth,
            limit,
            direct_only,
            show_files,
        }) => {
            commands::run_impact(
                &cli,
                symbol,
                path.as_deref(),
                in_file.as_deref(),
                *line,
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
            structural,
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
                *structural,
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

        // Installation diagnostics (issue #308)
        Some(Command::Doctor { what }) => {
            commands::doctor::run(what).context("Doctor command failed")?;
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

enum SearchAliasScope {
    Global,
    Workspace,
}

struct SearchCommandArgs<'a> {
    cli: &'a Cli,
    pattern: &'a str,
    path: Option<&'a str>,
    save_as: Option<&'a str>,
    scope: SearchAliasScope,
    description: Option<&'a str>,
    validate: ValidationMode,
    history_argv: &'a [String],
    cfg_filter: Option<&'a str>,
    include_generated: bool,
    macro_boundaries: bool,
    revision: &'a RevisionQueryArgs,
    /// Per-subcommand `--verbose` / `-v` flag from `sqry search`. The shorthand
    /// path threads `cli.verbose` instead; either source enables verbose at the
    /// `run_search` call site.
    verbose: bool,
}

fn handle_search_command(args: &SearchCommandArgs<'_>) -> Result<()> {
    let search_path = args.path.unwrap_or(args.cli.search_path());

    // Validate index before execution if requested
    if let Err(code) =
        validate_index_if_requested(args.cli, search_path, args.validate, args.cli.auto_rebuild)
    {
        std::process::exit(code);
    }

    // C002b: forward the macro-boundary flags (formerly extracted then
    // discarded) into the search engine.
    let result = commands::run_search(
        args.cli,
        args.pattern,
        search_path,
        args.cfg_filter,
        args.include_generated,
        args.macro_boundaries,
        args.revision,
        args.verbose,
    );

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
            matches!(args.scope, SearchAliasScope::Global),
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
    revision: &RevisionQueryArgs,
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
        revision,
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
    use commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
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
    let Ok(graph) = load_unified_graph_for_cli(search_root, &config, cli, no_op_reporter()) else {
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
                    crate::args::ClasspathDepthArg::Full,
                    None,
                    None,
                    false,
                    // auto-rebuild path always operates on an existing graph at
                    // `search_path`, so nested-index creation cannot apply.
                    false,
                    // auto-rebuild does not carry `--cfg` / `--expand-cache`
                    // (Phase 1a/1b): stale-index recovery rebuilds with today's
                    // default behaviour.
                    &[],
                    None,
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
    metrics_format: crate::args::MetricsFormat,
    cfg_flags: &[String],
    expand_cache: Option<&std::path::Path>,
    classpath: bool,
    _no_classpath: bool,
    classpath_depth: crate::args::ClasspathDepthArg,
    classpath_file: Option<&std::path::Path>,
    build_system: Option<&str>,
    force_classpath: bool,
    allow_nested: bool,
) -> Result<()> {
    // STEP_8 precedence: positional `<path>` wins; otherwise fall back to the
    // global `--workspace` / `SQRY_WORKSPACE_FILE`; otherwise `cli.search_path()`.
    // Non-UTF-8 workspace paths surface as a hard error rather than silently
    // falling back to `cli.search_path()` (STEP_8 codex iter1 fix).
    let index_path = cli.resolve_subcommand_path(path)?;

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
            classpath,
            _no_classpath,
            classpath_depth,
            classpath_file,
            build_system,
            force_classpath,
            allow_nested,
            cfg_flags,
            expand_cache,
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
        // Run search with pattern shorthand. The top-level shorthand path
        // does not parse the per-subcommand `--cfg-filter` /
        // `--include-generated` / `--macro-boundaries` flags, so pass the
        // documented defaults explicitly (None / false / false). `verbose`
        // is sourced from the top-level `--verbose` flag on `Cli`; env-driven
        // enablement (SQRY_LOG / RUST_LOG) is layered inside `run_search`.
        let revision = RevisionQueryArgs::default();
        let result = commands::run_search(
            cli,
            pattern,
            cli.search_path(),
            None,
            false,
            false,
            &revision,
            cli.verbose,
        );

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
/// Flag tokens that ALWAYS consume the next argument as their value, for every
/// flag that can precede an `@alias` run: the top-level `Cli` flags plus the
/// `query` / `search` subcommand flags (including the flattened
/// `RevisionQueryArgs` / `PluginSelectionArgs`). If any is omitted, `@alias`
/// expansion misreads the flag's value as a positional and bypasses the alias
/// (verivus-oss/sqry#514). The unit test
/// `flags_with_values_covers_query_and_search_scopes` derives the required set
/// from clap's own metadata and fails if this list drifts.
const FLAGS_WITH_VALUES: &[&str] = &[
    // Output control
    "--columns",
    "--limit",
    "--format",
    "--sort",
    "--theme",
    "--pager-cmd",
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
    "--fuzzy-field-distance",
    // Index validation (P1-14)
    "--validate",
    "--threshold-dangling-refs",
    "--threshold-orphaned-files",
    "--threshold-id-gaps",
    // Hybrid search
    "--context",
    "-C",
    "--max-text-results",
    // Query / search input
    "--var",
    "--cfg-filter",
    "--timeout",
    // Alias persistence
    "--save-as",
    "--description",
    // Plugin selection (query/search flatten `PluginSelectionArgs`)
    "--enable-plugin",
    "--enable-language",
    "--disable-plugin",
    "--disable-language",
    // Revision selector (query/search flatten `RevisionQueryArgs`)
    "--revision-id",
    "--revision-ref",
    "--revision-commit",
    "--revision-tree",
    // Workspace / configuration
    "--workspace",
    "--config-dir",
    "--path",
    "--type",
];

/// Flags with optional values (`num_args` = 0..=1).
/// These flags may or may not consume the next argument depending on whether
/// the next arg looks like a value (numeric) or something else (@alias, -flag).
const FLAGS_WITH_OPTIONAL_VALUES: &[&str] = &["--preview", "-p"];

#[derive(Debug)]
struct AliasScan {
    alias_index: Option<usize>,
    /// The single optional positional (search path) drawn from the tokens that
    /// follow the `@alias`, wherever it sat relative to the trailing flags. See
    /// [`partition_alias_tail`] for the order-independent partition that
    /// produces this and [`post_alias_flags`](AliasScan::post_alias_flags).
    remaining_path: Option<String>,
    /// Every flag token that follows the `@alias` (each already carrying any
    /// value it consumes), with the one positional path lifted out into
    /// [`remaining_path`](AliasScan::remaining_path). Kept as a partitioned set
    /// rather than a raw tail slice so the path and the trailing flags can
    /// never diverge from one peek-based index again (verivus-oss/sqry#528
    /// rounds 1-5: every prior fix patched one placement and left the model
    /// intact, so a new flag/path arrangement kept slipping through).
    post_alias_flags: Vec<String>,
    /// Index of a leading `query` / `search` subcommand word that precedes an
    /// `@alias` (i.e. `sqry query @name`, verivus-oss/sqry#514). The word
    /// is dropped during expansion so it is not mistaken for a global flag;
    /// the alias's own stored command drives the run.
    subcommand_prefix: Option<usize>,
}

impl AliasScan {
    /// The "not an alias run" result: the caller falls back to the original,
    /// unexpanded argv.
    fn none() -> Self {
        AliasScan {
            alias_index: None,
            remaining_path: None,
            post_alias_flags: Vec::new(),
            subcommand_prefix: None,
        }
    }
}

/// The flag / path partition of the argv tokens that follow an `@alias`.
struct AliasTail {
    /// Flag tokens (each with any value they consume) in original order.
    flags: Vec<String>,
    /// The single optional positional (search path), wherever it appeared.
    path: Option<String>,
}

/// Subcommand tokens after which a bare `@alias` is still recognized as an
/// alias run rather than a subcommand argument.
///
/// `sqry query @name` / `sqry search @name` used to slip through as a literal
/// query/pattern (the planner then choked on `@`, verivus-oss/sqry#514).
/// Only the two alias-carrying commands are eligible; every other subcommand
/// treats a following `@token` as its own positional.
const ALIAS_PREFIX_SUBCOMMANDS: &[&str] = &["query", "search"];

fn expand_alias_args() -> Result<Vec<String>> {
    use persistence::{AliasManager, PersistenceConfig, open_shared_index};
    use std::path::Path;

    let args: Vec<String> = std::env::args().collect();
    let scan = scan_alias_args(&args)?;

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
        &scan.post_alias_flags,
        scan.subcommand_prefix,
        &alias_with_scope,
    ))
}

/// Whether `token` collides with a top-level subcommand name or alias.
///
/// A `search` alias expands to the top-level shorthand (no `search` word) so
/// trailing flags route to the shorthand parse. If the stored pattern happens
/// to equal a subcommand token, clap would misparse it as that subcommand, so
/// the caller keeps the explicit `search` form instead. Computed from clap's
/// own command metadata to stay in sync with the real subcommand set.
fn collides_with_subcommand(token: &str) -> bool {
    use clap::CommandFactory as _;

    args::Cli::command()
        .get_subcommands()
        .any(|sub| sub.get_name() == token || sub.get_all_aliases().any(|alias| alias == token))
}

/// Whether `flag` (a bare flag token such as `--cfg-filter`, `--save-as=x`, or
/// `-k`) is defined directly on the top-level `Cli` parser, i.e. the shorthand
/// search surface a bare `sqry <pattern>` invocation uses.
///
/// Computed from clap's own metadata (long name, visible aliases, short name)
/// so newly added flags never silently drift out of sync, mirroring
/// `collides_with_subcommand` above.
fn flag_defined_on_top_level_cli(flag: &str) -> bool {
    use clap::CommandFactory as _;

    let name = flag.split('=').next().unwrap_or(flag);
    let bare = name.trim_start_matches('-');

    args::Cli::command().get_arguments().any(|arg| {
        arg.get_long().is_some_and(|long| long == bare)
            || arg
                .get_visible_aliases()
                .is_some_and(|aliases| aliases.contains(&bare))
            || arg
                .get_short()
                .is_some_and(|short| short.to_string() == bare)
    })
}

/// Whether the next argv token is consumed as the value of the last short in a
/// bare short-flag cluster.
enum ClusterTail {
    /// No value-taking short, or the value is glued inside the same token
    /// (`-kfoo` == `-k foo`): the following token is a fresh flag or the path.
    None,
    /// The last short takes a mandatory value from the next token (`-jk foo`).
    ConsumesNext,
    /// The last short takes an optional value from the next token (`-p`): the
    /// caller decides via a numeric peek.
    OptionalNext,
}

/// Split a bare short-flag token into the individual short flags it bundles.
///
/// clap lets short flags cluster (`-jc` == `-j -c`) and lets a value attach to
/// the first value-taking short either glued (`-kfoo` == `-k foo`) or as the
/// next token (`-k foo`). Returns `None` for anything that is not a bare short
/// cluster (a `--long` flag, a `-x=value` form, or a non-flag token), so the
/// caller treats those tokens whole. On a match, the returned `Vec` holds each
/// clustered short as its own `-x` token for classification, and `ClusterTail`
/// says whether the following argv token is this cluster's value.
///
/// We stop expanding at the first value-taking short: everything after it in
/// the same token is that flag's glued value, not another flag. This keeps a
/// bundled cluster from being misjudged (verivus-oss/sqry#528 round 6:
/// `flag_defined_on_top_level_cli` only ever matched single-char shorts, so a
/// whole `-jc` cluster looked like an unknown, subcommand-scoped flag and
/// wrongly forced the explicit `search` word onto an otherwise-shorthand run).
fn split_short_cluster(token: &str) -> Option<(Vec<String>, ClusterTail)> {
    if !token.starts_with('-') || token.starts_with("--") || token.contains('=') || token.len() < 2
    {
        return None;
    }

    let body: Vec<char> = token[1..].chars().collect();
    if body.is_empty() {
        return None;
    }

    let mut shorts = Vec::new();
    let mut tail = ClusterTail::None;
    for (idx, ch) in body.iter().enumerate() {
        let short = format!("-{ch}");
        let mandatory_value = FLAGS_WITH_VALUES.contains(&short.as_str());
        let optional_value = FLAGS_WITH_OPTIONAL_VALUES.contains(&short.as_str());
        shorts.push(short);
        if mandatory_value || optional_value {
            let has_glued_value = idx + 1 < body.len();
            tail = if has_glued_value {
                ClusterTail::None
            } else if mandatory_value {
                ClusterTail::ConsumesNext
            } else {
                ClusterTail::OptionalNext
            };
            break;
        }
    }

    Some((shorts, tail))
}

/// Expand a flag token into the individual flags to classify: a `--long` flag
/// is itself, a bare short cluster splits into its members (`-jc` -> `-j`,
/// `-c`), so each is judged on its own scope.
fn flags_to_classify(arg: &str) -> Vec<String> {
    match split_short_cluster(arg) {
        Some((shorts, _)) => shorts,
        None => vec![arg.to_string()],
    }
}

/// Whether any flag token in `tokens` is scoped to the `search` subcommand
/// only, i.e. absent from the top-level `Cli` shorthand surface (e.g.
/// `--cfg-filter`, `--save-as`, `--global`, `--description`,
/// `--include-generated`, `--macro-boundaries`, `--revision-*`).
///
/// `tokens` may be flags that precede the alias (`sqry search --cfg-filter
/// test @qfuncs`) or flags that trail it, after the alias/path
/// (`sqry search @qfuncs --cfg-filter test`, verivus-oss/sqry#528 round 5):
/// the same "is this flag even legal on the shorthand surface" question
/// applies at either position, so both call sites route through this one
/// predicate rather than each growing its own copy that can drift out of
/// sync with the other.
///
/// A bundled short cluster is subcommand-scoped iff ANY of its members is
/// (verivus-oss/sqry#528 round 6): the cluster is split into its individual
/// shorts and each is judged separately, so `-ic` (both `-i` / `-c` top-level)
/// stays on the shorthand surface while a cluster carrying any `search`-only
/// short would not.
///
/// When true, an alias must keep its explicit subcommand word
/// (verivus-oss/sqry#528 round 4: `sqry search --cfg-filter test @qfuncs`
/// used to drop the `search` word and expand to the top-level shorthand form,
/// which has no `--cfg-filter` flag, so clap rejected it; round 5: the same
/// failure reappeared for `sqry search @qfuncs --cfg-filter test`, a trailing
/// flag the round-4 fix did not inspect) so every flag parses under the
/// scope that actually defines it, regardless of which side of the alias it
/// sits on.
fn any_flag_requires_subcommand_scope(tokens: &[String]) -> bool {
    tokens
        .iter()
        .filter(|arg| arg.starts_with('-'))
        .flat_map(|arg| flags_to_classify(arg))
        .any(|flag| !flag_defined_on_top_level_cli(&flag))
}

/// Scan forward from `start` past any global flags (and the values they
/// consume) after a `query` / `search` prefix word to find an `@alias` token.
///
/// Returns the index of the first `@alias` reached, or `None` when the first
/// non-flag token is a real positional (a genuine query / pattern, not an
/// alias). A flag value is never mistaken for the alias: value-taking flags
/// consume their next argument via [`should_skip_next_arg`], so
/// `sqry query --lang @weird` treats `@weird` as the value of `--lang`, not an
/// alias (verivus-oss/sqry#514).
fn find_alias_after_prefix(args: &[String], start: usize) -> Option<usize> {
    let mut j = start;
    while let Some(token) = args.get(j) {
        // clap end-of-options boundary: a bare `--` after the prefix word ends
        // alias detection. `sqry search -- @gpicks` / `sqry query -- @gpicks`
        // must treat the following `@token` as a literal query/pattern, never an
        // alias (verivus-oss/sqry#528 round 8: this used to skip the `--` and
        // resolve `@gpicks` as an alias, which then failed "Unknown alias" for a
        // literal string that the direct form searches for verbatim).
        if token == "--" {
            return None;
        }
        if token.starts_with('@') {
            return Some(j);
        }
        if token.starts_with('-') {
            j += if should_skip_next_arg(args, j, token) {
                2
            } else {
                1
            };
            continue;
        }
        // A non-flag, non-`@` token is a real positional, not an alias run.
        return None;
    }
    None
}

/// Partition the argv tokens that follow an `@alias` into flag tokens (each
/// with any value it consumes) and at most one positional, the search path.
///
/// This replaces the pre-round-6 "the path is the single token right after the
/// alias" peek that only ever inspected `args[alias + 1]`. That peek could not
/// see a path that sat AFTER a trailing flag (`@qfuncs --cfg-filter test .`),
/// so it resolved `path = None`, synthesized a default `"."`, AND swept the
/// real `.` into the trailing flags, handing clap two positionals
/// (verivus-oss/sqry#528 rounds 1-5). The partition is order-independent:
/// the path may appear before, after, or among the trailing flags, and a
/// value-taking flag consumes its value (space- or `=`-joined, glued short, or
/// bundled short) so that value is never mistaken for the path.
///
/// The alias contract admits a single optional path (the stored pattern
/// already supplies the query), so a second positional is a clear user error
/// rather than a silently dropped argument.
///
/// A bare `--` token ends flag classification, mirroring clap's own
/// end-of-options escape: every token after it is positional, even one that
/// looks like a flag (`-5`, `--foo`). Before this, `sqry @picks -- -5`
/// swept the escaped `-5` into `flags` and clap rejected it at the top-level
/// parse ("unexpected argument '-5' found"), while the direct
/// `sqry search "<pattern>" -- -5` form parsed fine (clap's own `--` handling
/// took over) and only failed later at the path-existence check. The `--`
/// marker itself is consumed here, not carried through as a flag token; see
/// `build_expanded_args` for the matching re-emit half that puts a fresh `--`
/// back in front of a path that needs it (verivus-oss/sqry#528 round 7).
///
/// A value-taking flag immediately followed by `--` does NOT swallow the `--`
/// as its value: `should_skip_next_arg` refuses to consume a bare `--`, so the
/// flag is emitted value-less and the `--` is then handled as the
/// end-of-options marker. `sqry @picks --cfg-filter -- -5` thus partitions to
/// `flags = ["--cfg-filter"]`, `path = "-5"`, so the expanded argv matches the
/// direct `sqry search "<pattern>" --cfg-filter -- -5` form (both then error
/// "a value is required for '--cfg-filter'", verivus-oss/sqry#528 round 8).
fn partition_alias_tail(tokens: &[String]) -> Result<AliasTail> {
    let mut flags = Vec::new();
    let mut path: Option<String> = None;

    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];

        if tok == "--" {
            for rest in &tokens[i + 1..] {
                if let Some(existing) = &path {
                    anyhow::bail!(
                        "Ambiguous alias invocation: two paths given ('{existing}' and \
                         '{rest}'). An @alias run takes at most one search path."
                    );
                }
                path = Some(rest.clone());
            }
            break;
        }

        if tok.starts_with('-') {
            flags.push(tok.clone());
            if should_skip_next_arg(tokens, i, tok)
                && let Some(value) = tokens.get(i + 1)
            {
                flags.push(value.clone());
                i += 2;
                continue;
            }
            i += 1;
        } else {
            if let Some(existing) = &path {
                anyhow::bail!(
                    "Ambiguous alias invocation: two paths given ('{existing}' and '{tok}'). \
                     An @alias run takes at most one search path."
                );
            }
            path = Some(tok.clone());
            i += 1;
        }
    }

    Ok(AliasTail { flags, path })
}

fn scan_alias_args(args: &[String]) -> Result<AliasScan> {
    let mut skip_next = false;

    for (i, arg) in args.iter().enumerate().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }

        // clap end-of-options boundary: the first bare `--` ends alias
        // detection. Any `@token` at or after it is a literal positional (a
        // search pattern / path), not an alias, exactly as the direct clap
        // parse would treat it. `sqry -- @gpicks` therefore searches for the
        // literal `@gpicks` instead of expanding the alias
        // (verivus-oss/sqry#528 round 8). A flag can never carry us past a
        // `--` because `should_skip_next_arg` refuses to consume one as a
        // value, so this token is always seen before it could be mistaken for a
        // flag value.
        if arg == "--" {
            return Ok(AliasScan::none());
        }

        if arg.starts_with('-') {
            if should_skip_next_arg(args, i, arg) {
                skip_next = true;
            }
            continue;
        }

        // Check if this looks like a subcommand (not starting with @)
        if !arg.starts_with('@') {
            // `sqry query @name` / `sqry search @name` (optionally with global
            // flags in between, e.g. `sqry query --json @name`): the `@alias`
            // after an alias-carrying subcommand is still an alias run
            // (verivus-oss/sqry#514), not a literal query/pattern. Scan past
            // any intervening flags (and the values they consume) to find it,
            // and mark the subcommand word for removal during expansion.
            if ALIAS_PREFIX_SUBCOMMANDS.contains(&arg.as_str())
                && let Some(alias_at) = find_alias_after_prefix(args, i + 1)
            {
                let tail = partition_alias_tail(&args[alias_at + 1..])?;
                return Ok(AliasScan {
                    alias_index: Some(alias_at),
                    remaining_path: tail.path,
                    post_alias_flags: tail.flags,
                    subcommand_prefix: Some(i),
                });
            }

            // Not an alias, return original args
            return Ok(AliasScan::none());
        }

        // Found an @alias pattern: partition everything after it into the
        // trailing flags and the single optional path.
        let tail = partition_alias_tail(&args[i + 1..])?;
        return Ok(AliasScan {
            alias_index: Some(i),
            remaining_path: tail.path,
            post_alias_flags: tail.flags,
            subcommand_prefix: None,
        });
    }

    Ok(AliasScan::none())
}

fn should_skip_next_arg(args: &[String], index: usize, arg: &str) -> bool {
    // A bare `--` is clap's end-of-options marker, never a flag's value. When
    // the next token is `--`, the flag consumes nothing: under the direct parse
    // the flag then errors "a value is required for '<flag>'" (the `--` walls
    // off the value slot), so the alias form must not swallow the `--` either.
    // This is the single value-consumption choke point shared by
    // `scan_alias_args`, `find_alias_after_prefix`, and `partition_alias_tail`,
    // so honoring the boundary here keeps every argv-scanning site in lockstep
    // (verivus-oss/sqry#528 round 8: `@picks --cfg-filter -- -5` used to eat
    // the `--` as `--cfg-filter`'s value, diverging from the direct
    // `search pick_ --cfg-filter -- -5` "a value is required" error).
    if args.get(index + 1).is_some_and(|next| next == "--") {
        return false;
    }

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

    // Bundled short cluster whose final short takes a value from the next
    // token (`-jk foo` == `-j -k foo`, verivus-oss/sqry#528 round 6). A
    // glued value (`-kfoo`) reports `ClusterTail::None`, so the following token
    // stays available as a flag or the path.
    match split_short_cluster(arg) {
        Some((_, ClusterTail::ConsumesNext)) => return true,
        Some((_, ClusterTail::OptionalNext)) => {
            return args
                .get(index + 1)
                .is_some_and(|next_arg| is_optional_flag_value(next_arg));
        }
        _ => {}
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
    post_alias_flags: &[String],
    subcommand_prefix: Option<usize>,
    alias_with_scope: &persistence::AliasWithScope,
) -> Vec<String> {
    let mut expanded = vec![args[0].clone()]; // Program name

    // Flags that appeared before the alias, dropping any leading `query` /
    // `search` subcommand word from a `sqry query @name` invocation
    // (verivus-oss/sqry#514): the alias's own stored command drives the run,
    // so the typed word must not be copied through as a positional/flag.
    let pre_alias_flags: Vec<String> = args
        .iter()
        .enumerate()
        .take(alias_index)
        .skip(1)
        .filter(|(i, _)| Some(*i) != subcommand_prefix)
        .map(|(_, arg)| arg.clone())
        .collect();

    // Flags that trail the alias, i.e. `sqry search @qfuncs --cfg-filter test`,
    // `sqry search @qfuncs . --cfg-filter test`, or `sqry search @qfuncs
    // --cfg-filter test .`. These come pre-partitioned from
    // [`partition_alias_tail`] with the single path already lifted out into
    // `remaining_path`, so a real path can never leak into this set and force a
    // second positional (verivus-oss/sqry#528 rounds 1-5). Both `pre`- and
    // `trailing_flags` feed the same `use_shorthand` decision below so a
    // subcommand-scoped flag on either side keeps the explicit command word.
    let trailing_flags = post_alias_flags;

    // A `search` alias expands to the top-level shorthand form (no `search`
    // token) so trailing user flags such as `-c` route to the top-level `Cli`
    // parse, which owns the shorthand match/output flags. The explicit `search`
    // subcommand does not accept those flags, which is exactly why
    // `sqry @name . -c` used to fail with "Usage: sqry search ..."
    // (verivus-oss/sqry#514). A `query` alias keeps its subcommand because
    // the structured planner has no shorthand form. The shorthand is skipped
    // when the stored pattern would collide with a real subcommand name, so
    // clap cannot misparse the pattern as a subcommand, AND when any flag on
    // either side of the alias is scoped to `search` only (e.g.
    // `--cfg-filter`, `--save-as`, `--global`, `--description`,
    // `--include-generated`, `--macro-boundaries`, `--revision-*`): the
    // top-level shorthand has no such flag, so clap would reject it.
    // `sqry search --cfg-filter test @qfuncs` used to drop the `search` word
    // for exactly this reason (verivus-oss/sqry#528 round 4); `sqry search
    // @qfuncs --cfg-filter test` failed the same way for a trailing flag
    // (round 5), independent of the round-3 fix that made `query` always keep
    // its word.
    let command = alias_with_scope.alias.command.as_str();
    let stored_args = &alias_with_scope.alias.args;
    let use_shorthand = command == "search"
        && stored_args
            .first()
            .is_some_and(|pattern| !collides_with_subcommand(pattern))
        && !any_flag_requires_subcommand_scope(&pre_alias_flags)
        && !any_flag_requires_subcommand_scope(trailing_flags);

    // The command word (when kept) must precede the pre-alias flags: those
    // flags can be subcommand-scoped (e.g. `--var` / `--enable-plugin` on
    // `query`), and clap rejects a subcommand-only flag that appears before
    // its subcommand token (verivus-oss/sqry#514). For a shorthand `search`
    // alias there is no command word, so the flags stay at the front where the
    // top-level `Cli` parse owns them. Global flags (`--json`) parse correctly
    // in either position.
    if !use_shorthand {
        expanded.push(command.to_string());
    }
    expanded.extend(pre_alias_flags);

    // Add the stored arguments
    expanded.extend(stored_args.iter().cloned());

    // Add the path if provided, otherwise use "."
    let path_token = remaining_path.map_or_else(|| ".".to_string(), ToString::to_string);

    // A path that starts with `-` (e.g. a negative-number directory name
    // reached through `sqry @picks -- -5`, verivus-oss/sqry#528 round 7)
    // parses fine coming out of `partition_alias_tail` (the `--` there already
    // walled it off from flag classification), but the expanded argv handed
    // to clap has no such wall unless we re-emit one: without a fresh `--`
    // immediately before the path, clap sees a bare `-5` and rejects it as an
    // unrecognized flag. Re-emitting `--` here puts the alias run through the
    // same parse state as the direct `sqry search "<pattern>" -- -5` form. The
    // trailing flags move ahead of the `--` so they still parse as flags
    // instead of being swept into positionals by the same end-of-options
    // marker; a path that needs no escaping keeps the original
    // path-then-flags order byte-for-byte.
    if path_token.starts_with('-') {
        expanded.extend(trailing_flags.iter().cloned());
        expanded.push("--".to_string());
        expanded.push(path_token);
    } else {
        expanded.push(path_token);
        expanded.extend(trailing_flags.iter().cloned());
    }

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
mod alias_scan_tests {
    use super::*;

    fn sv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    large_stack_test! {
    /// Drift guard: every value-taking flag reachable before an `@alias` (the
    /// top-level `Cli` flags plus the `query` / `search` subcommand flags,
    /// including their flattened `RevisionQueryArgs` / `PluginSelectionArgs`)
    /// must be listed in `FLAGS_WITH_VALUES` (or `FLAGS_WITH_OPTIONAL_VALUES`
    /// for `num_args = 0..=1`). Derived from clap's own metadata so a newly
    /// added value flag that is not registered here fails CI rather than
    /// silently regressing `@alias` expansion (verivus-oss/sqry#514).
    /// Building the clap command tree is stack-heavy, hence `large_stack_test!`.
    #[test]
    fn flags_with_values_covers_query_and_search_scopes() {
        use clap::CommandFactory as _;
        use clap::builder::ArgAction;
        use std::collections::BTreeSet;

        let required: BTreeSet<&str> = FLAGS_WITH_VALUES.iter().copied().collect();
        let optional: BTreeSet<&str> = FLAGS_WITH_OPTIONAL_VALUES.iter().copied().collect();

        let cmd = args::Cli::command();
        let mut missing: Vec<String> = Vec::new();

        let mut audit = |command: &clap::Command| {
            for arg in command.get_arguments() {
                if !matches!(arg.get_action(), ArgAction::Set | ArgAction::Append) {
                    continue;
                }
                let is_optional = arg
                    .get_num_args()
                    .is_some_and(|range| range.min_values() == 0);

                let mut tokens: Vec<String> = Vec::new();
                if let Some(long) = arg.get_long() {
                    tokens.push(format!("--{long}"));
                    if let Some(aliases) = arg.get_visible_aliases() {
                        for alias in aliases {
                            tokens.push(format!("--{alias}"));
                        }
                    }
                }
                if let Some(short) = arg.get_short() {
                    tokens.push(format!("-{short}"));
                }

                for token in tokens {
                    let covered = if is_optional {
                        optional.contains(token.as_str()) || required.contains(token.as_str())
                    } else {
                        required.contains(token.as_str())
                    };
                    if !covered {
                        missing.push(format!("{token} (optional={is_optional})"));
                    }
                }
            }
        };

        audit(&cmd);
        for name in ["query", "search"] {
            if let Some(sub) = cmd.find_subcommand(name) {
                audit(sub);
            }
        }

        assert!(
            missing.is_empty(),
            "value-taking flags reachable before an @alias on top-level / query / \
             search are missing from FLAGS_WITH_VALUES / FLAGS_WITH_OPTIONAL_VALUES: {missing:?}"
        );
    }
    }

    #[test]
    fn plain_alias_with_trailing_flag_is_recognized() {
        // `sqry @picks . -c`: the alias is at index 1, the path is ".", and the
        // trailing `-c` is partitioned into the post-alias flags for the
        // expanded parse (verivus-oss/sqry#514).
        let scan = scan_alias_args(&sv(&["sqry", "@picks", ".", "-c"])).unwrap();
        assert_eq!(scan.alias_index, Some(1));
        assert_eq!(scan.remaining_path.as_deref(), Some("."));
        assert_eq!(scan.post_alias_flags, sv(&["-c"]));
        assert_eq!(scan.subcommand_prefix, None);
    }

    #[test]
    fn plain_alias_flag_before_path_is_partitioned() {
        // `sqry @picks --cfg-filter test .`: the path `.` sits AFTER a
        // value-taking flag. The pre-round-6 peek looked only at the token
        // right after the alias (`--cfg-filter`), resolved `path = None`, and
        // swept the real `.` into the trailing flags, so the expansion carried
        // two positionals. The order-independent partition lifts the single
        // path out no matter where it sits (verivus-oss/sqry#528 round 6).
        let scan = scan_alias_args(&sv(&["sqry", "@picks", "--cfg-filter", "test", "."])).unwrap();
        assert_eq!(scan.alias_index, Some(1));
        assert_eq!(scan.remaining_path.as_deref(), Some("."));
        assert_eq!(scan.post_alias_flags, sv(&["--cfg-filter", "test"]));
    }

    #[test]
    fn plain_alias_equals_joined_flag_value_is_partitioned() {
        // `=`-joined flag values consume nothing from the next token, so the
        // path `.` is still cleanly the single positional
        // (verivus-oss/sqry#528 round 6).
        let scan = scan_alias_args(&sv(&["sqry", "@picks", "--cfg-filter=test", "."])).unwrap();
        assert_eq!(scan.remaining_path.as_deref(), Some("."));
        assert_eq!(scan.post_alias_flags, sv(&["--cfg-filter=test"]));
    }

    #[test]
    fn plain_alias_two_paths_is_ambiguous_error() {
        // The alias contract admits a single optional path (the stored pattern
        // supplies the query), so a second positional is a clear error rather
        // than a silently dropped argument (verivus-oss/sqry#528 round 6).
        let err = scan_alias_args(&sv(&["sqry", "@picks", "src", "lib"])).unwrap_err();
        assert!(
            err.to_string().contains("Ambiguous alias invocation"),
            "two paths after an alias must error, got {err}"
        );
    }

    #[test]
    fn query_prefixed_alias_is_recognized_and_marks_prefix() {
        // `sqry query @picks`: the leading `query` word is marked for removal so
        // it is not copied through as a positional (verivus-oss/sqry#514).
        let scan = scan_alias_args(&sv(&["sqry", "query", "@picks"])).unwrap();
        assert_eq!(scan.alias_index, Some(2));
        assert_eq!(scan.subcommand_prefix, Some(1));
    }

    #[test]
    fn search_prefixed_alias_is_recognized() {
        let scan = scan_alias_args(&sv(&["sqry", "search", "@picks", "src"])).unwrap();
        assert_eq!(scan.alias_index, Some(2));
        assert_eq!(scan.remaining_path.as_deref(), Some("src"));
        assert_eq!(scan.subcommand_prefix, Some(1));
    }

    #[test]
    fn query_prefixed_alias_with_intervening_flag_is_recognized() {
        // `sqry query --json @picks`: the scan skips the global flag to reach
        // the `@alias` (verivus-oss/sqry#514).
        let scan = scan_alias_args(&sv(&["sqry", "query", "--json", "@picks"])).unwrap();
        assert_eq!(scan.alias_index, Some(3));
        assert_eq!(scan.subcommand_prefix, Some(1));
    }

    #[test]
    fn query_prefixed_value_flag_does_not_capture_alias_lookalike() {
        // `--lang` consumes its next argument, so `@weird` is its value, not an
        // alias: this is a genuine query, not an alias run.
        let scan = scan_alias_args(&sv(&["sqry", "query", "--lang", "@weird"])).unwrap();
        assert_eq!(scan.alias_index, None);
        assert_eq!(scan.subcommand_prefix, None);
    }

    #[test]
    fn non_alias_subcommand_is_not_an_alias() {
        // `sqry query kind:function` is a genuine query, not an alias run.
        let scan = scan_alias_args(&sv(&["sqry", "query", "kind:function"])).unwrap();
        assert_eq!(scan.alias_index, None);
        assert_eq!(scan.subcommand_prefix, None);
    }

    #[test]
    fn graph_prefixed_alias_is_not_recognized() {
        // Only `query` / `search` carry aliases; other subcommands treat a
        // following `@token` as their own positional.
        let scan = scan_alias_args(&sv(&["sqry", "graph", "@picks"])).unwrap();
        assert_eq!(scan.alias_index, None);
    }

    large_stack_test! {
    #[test]
    fn subcommand_names_collide() {
        // clap-derived subcommand set must be recognized so a stored pattern
        // that equals one keeps the explicit `search` form. Building the full
        // clap command tree is stack-heavy, hence `large_stack_test!`.
        assert!(collides_with_subcommand("index"));
        assert!(collides_with_subcommand("query"));
        assert!(!collides_with_subcommand("pick_"));
        assert!(!collides_with_subcommand("TODO"));
    }
    }

    fn saved_alias(command: &str, args: &[&str]) -> persistence::AliasWithScope {
        persistence::AliasWithScope {
            name: "picks".to_string(),
            alias: persistence::SavedAlias {
                command: command.to_string(),
                args: args.iter().map(|s| (*s).to_string()).collect(),
                created: chrono::Utc::now(),
                description: None,
            },
            scope: persistence::StorageScope::Local,
        }
    }

    /// Run the real scan + expansion pipeline end to end: partition `argv` via
    /// [`scan_alias_args`] then feed the exact fields into
    /// [`build_expanded_args`], so the partition and the expansion can never be
    /// tested against divergent inputs (verivus-oss/sqry#528 round 6).
    fn expand_via_scan(argv: &[String], alias: &persistence::AliasWithScope) -> Vec<String> {
        let scan = scan_alias_args(argv).unwrap();
        build_expanded_args(
            argv,
            scan.alias_index.unwrap(),
            scan.remaining_path.as_deref(),
            &scan.post_alias_flags,
            scan.subcommand_prefix,
            alias,
        )
    }

    large_stack_test! {
    #[test]
    fn search_alias_expands_to_shorthand() {
        // A search alias drops the `search` word so trailing flags route to the
        // top-level shorthand parse (verivus-oss/sqry#514).
        let alias = saved_alias("search", &["pick_"]);
        let expanded = expand_via_scan(&sv(&["sqry", "@picks", ".", "-c"]), &alias);
        assert_eq!(expanded, sv(&["sqry", "pick_", ".", "-c"]));
    }
    }

    large_stack_test! {
    #[test]
    fn search_alias_pattern_colliding_with_subcommand_keeps_explicit_form() {
        // A stored pattern equal to a subcommand name keeps the explicit
        // `search` word so clap cannot misparse it as that subcommand.
        let alias = saved_alias("search", &["index"]);
        let expanded = expand_via_scan(&sv(&["sqry", "@picks"]), &alias);
        assert_eq!(expanded, sv(&["sqry", "search", "index", "."]));
    }
    }

    #[test]
    fn query_alias_keeps_subcommand_and_drops_prefix_word() {
        // A query alias keeps its `query` subcommand, and the typed `query`
        // prefix word (index 1) is dropped from the expansion.
        let alias = saved_alias("query", &["kind:function"]);
        let expanded = expand_via_scan(&sv(&["sqry", "query", "@funcs"]), &alias);
        assert_eq!(expanded, sv(&["sqry", "query", "kind:function", "."]));
    }

    #[test]
    fn query_alias_value_flag_expands_after_command_word() {
        // `sqry query --var k=v @qfuncs`: the scan skips `--var`'s value to
        // reach the alias, and the expansion emits the `query` word BEFORE the
        // subcommand-scoped `--var k=v` so clap accepts it (a query flag before
        // its subcommand token would be rejected, verivus-oss/sqry#514).
        let argv = sv(&["sqry", "query", "--var", "k=v", "@qfuncs"]);

        let scan = scan_alias_args(&argv).unwrap();
        assert_eq!(scan.alias_index, Some(4));
        assert_eq!(scan.subcommand_prefix, Some(1));

        let alias = saved_alias("query", &["kind:$k"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&["sqry", "query", "--var", "k=v", "kind:$k", "."])
        );
    }

    #[test]
    fn query_alias_value_flag_before_path_expands_after_command_word() {
        // `sqry query --var k=v @qfuncs .`: mirrors the flag-before-path search
        // class member for `query`. The path `.` must be the single positional,
        // never merged with a synthesized default (verivus-oss/sqry#528
        // round 6).
        let argv = sv(&["sqry", "query", "--var", "k=v", "@qfuncs", "."]);
        let alias = saved_alias("query", &["kind:$k"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&["sqry", "query", "--var", "k=v", "kind:$k", "."])
        );
    }

    #[test]
    fn query_alias_trailing_flag_before_path_expands_after_command_word() {
        // `sqry query @qfuncs --var k=v .`: the subcommand-scoped `--var k=v`
        // trails the alias and the path `.` sits after the flag value. The
        // partition consumes `k=v` as `--var`'s value and keeps `.` as the sole
        // positional (verivus-oss/sqry#528 round 6).
        let argv = sv(&["sqry", "query", "@qfuncs", "--var", "k=v", "."]);
        let scan = scan_alias_args(&argv).unwrap();
        assert_eq!(scan.remaining_path.as_deref(), Some("."));
        assert_eq!(scan.post_alias_flags, sv(&["--var", "k=v"]));

        let alias = saved_alias("query", &["kind:$k"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&["sqry", "query", "kind:$k", ".", "--var", "k=v"])
        );
    }

    #[test]
    fn query_alias_equals_joined_value_flag_before_path() {
        // `sqry query @qfuncs --var=k=v .`: an `=`-joined value must be handled
        // identically to the space-separated form, leaving `.` as the single
        // path (verivus-oss/sqry#528 round 6).
        let argv = sv(&["sqry", "query", "@qfuncs", "--var=k=v", "."]);
        let alias = saved_alias("query", &["kind:$k"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&["sqry", "query", "kind:$k", ".", "--var=k=v"])
        );
    }

    large_stack_test! {
    #[test]
    fn search_alias_flag_before_path_keeps_command_word() {
        // `sqry search @qfuncs --cfg-filter test .`: the round-5 tip resolved
        // `path = None` (the token after the alias was `--cfg-filter`),
        // synthesized a default `"."`, AND swept the real `.` into the trailing
        // flags, so clap saw `search <pattern> . --cfg-filter test .` with two
        // path positionals and exited 2. The partition lifts the single `.`
        // out and keeps the `search` word for the subcommand-scoped
        // `--cfg-filter` (verivus-oss/sqry#528 round 6).
        let argv = sv(&["sqry", "search", "@qfuncs", "--cfg-filter", "test", "."]);
        let scan = scan_alias_args(&argv).unwrap();
        assert_eq!(scan.remaining_path.as_deref(), Some("."));
        assert_eq!(scan.post_alias_flags, sv(&["--cfg-filter", "test"]));

        let alias = saved_alias("search", &["kind:function"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&["sqry", "search", "kind:function", ".", "--cfg-filter", "test"])
        );
    }
    }

    large_stack_test! {
    #[test]
    fn search_shorthand_flag_before_path_keeps_command_word() {
        // Same flag-before-path bug via the top-level shorthand form
        // (`sqry @qfuncs --cfg-filter test .`, no `search` word typed): the
        // subcommand-scoped `--cfg-filter` still forces the `search` word back
        // on so clap parses it under the scope that owns it
        // (verivus-oss/sqry#528 round 6).
        let argv = sv(&["sqry", "@qfuncs", "--cfg-filter", "test", "."]);
        let alias = saved_alias("search", &["kind:function"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&["sqry", "search", "kind:function", ".", "--cfg-filter", "test"])
        );
    }
    }

    large_stack_test! {
    #[test]
    fn search_shorthand_equals_joined_flag_before_path_keeps_command_word() {
        // `=`-joined subcommand-scoped value before the path
        // (`sqry @qfuncs --cfg-filter=test .`): same expansion as the
        // space-separated form (verivus-oss/sqry#528 round 6).
        let argv = sv(&["sqry", "@qfuncs", "--cfg-filter=test", "."]);
        let alias = saved_alias("search", &["kind:function"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&["sqry", "search", "kind:function", ".", "--cfg-filter=test"])
        );
    }
    }

    large_stack_test! {
    #[test]
    fn search_shorthand_top_level_flag_before_path_uses_shorthand() {
        // `sqry @qfuncs -c .`: a genuinely top-level flag before the path keeps
        // the shorthand form (no `search` word) with `.` as the path
        // (verivus-oss/sqry#528 round 6).
        let argv = sv(&["sqry", "@qfuncs", "-c", "."]);
        let alias = saved_alias("search", &["kind:function"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(expanded, sv(&["sqry", "kind:function", ".", "-c"]));
    }
    }

    large_stack_test! {
    #[test]
    fn search_shorthand_bundled_short_cluster_uses_shorthand() {
        // `sqry @picks -ic .`: `-ic` bundles two top-level shorts (`-i`
        // ignore_case, `-c` count). The round-5 tip classified the whole `-ic`
        // token as an unknown, subcommand-scoped flag (its
        // `flag_defined_on_top_level_cli` matched only single-char shorts), so
        // it forced the `search` word back on and expanded to `search pick_ .
        // -ic`, which the `search` subcommand (no `-i`/`-c`) rejected with exit
        // 2. Splitting the cluster into `-i` + `-c` classifies both as
        // top-level, keeping the shorthand form (verivus-oss/sqry#528
        // round 6).
        let argv = sv(&["sqry", "@picks", "-ic", "."]);
        let alias = saved_alias("search", &["pick_"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(expanded, sv(&["sqry", "pick_", ".", "-ic"]));
    }
    }

    large_stack_test! {
    #[test]
    fn search_alias_value_flag_expands_with_command_word() {
        // `sqry search --cfg-filter test @qfuncs`: `--cfg-filter` is scoped to
        // the `Search` subcommand only (absent from the top-level `Cli`
        // shorthand), so the shorthand-drop optimization used for a plain
        // `search` alias (see `search_alias_expands_to_shorthand`) must NOT
        // apply here. The expansion must emit the `search` word BEFORE
        // `--cfg-filter test` so clap parses the flag under the scope that
        // defines it (verivus-oss/sqry#528 round 4: this used to drop the
        // `search` word and expand to the top-level shorthand form, which has
        // no `--cfg-filter` flag, so clap rejected the run with "unexpected
        // argument '--cfg-filter' found").
        let argv = sv(&["sqry", "search", "--cfg-filter", "test", "@qfuncs"]);

        let scan = scan_alias_args(&argv).unwrap();
        assert_eq!(scan.alias_index, Some(4));
        assert_eq!(scan.subcommand_prefix, Some(1));

        let alias = saved_alias("search", &["kind:function"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&[
                "sqry",
                "search",
                "--cfg-filter",
                "test",
                "kind:function",
                "."
            ])
        );
    }
    }

    large_stack_test! {
    #[test]
    fn search_alias_trailing_value_flag_expands_with_command_word() {
        // `sqry search @qfuncs --cfg-filter test`: the subcommand-scoped
        // `--cfg-filter test` now trails the alias instead of leading it. The
        // round-4 fix only inspected `pre_alias_flags`, so this trailing
        // placement still dropped the `search` word and expanded to the
        // top-level shorthand form, which clap rejected with "unexpected
        // argument '--cfg-filter' found" (verivus-oss/sqry#528 round 5).
        let argv = sv(&["sqry", "search", "@qfuncs", "--cfg-filter", "test"]);

        let scan = scan_alias_args(&argv).unwrap();
        assert_eq!(scan.alias_index, Some(2));
        assert_eq!(scan.subcommand_prefix, Some(1));
        assert_eq!(scan.remaining_path, None);

        let alias = saved_alias("search", &["kind:function"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&[
                "sqry",
                "search",
                "kind:function",
                ".",
                "--cfg-filter",
                "test"
            ])
        );
    }
    }

    large_stack_test! {
    #[test]
    fn search_alias_trailing_value_flag_expands_with_command_word_and_path() {
        // Same trailing-flag bug as above, but with an explicit path between
        // the alias and the flag: `sqry search @qfuncs . --cfg-filter test`
        // (verivus-oss/sqry#528 round 5).
        let argv = sv(&["sqry", "search", "@qfuncs", ".", "--cfg-filter", "test"]);

        let scan = scan_alias_args(&argv).unwrap();
        assert_eq!(scan.alias_index, Some(2));
        assert_eq!(scan.subcommand_prefix, Some(1));
        assert_eq!(scan.remaining_path.as_deref(), Some("."));

        let alias = saved_alias("search", &["kind:function"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&[
                "sqry",
                "search",
                "kind:function",
                ".",
                "--cfg-filter",
                "test"
            ])
        );
    }
    }

    large_stack_test! {
    #[test]
    fn search_alias_trailing_top_level_flag_still_uses_shorthand() {
        // Boundary check: a genuinely top-level flag (`-c`, defined on `Cli`
        // itself) trailing the alias must still take the shorthand path (no
        // `search` word), the same as `search_alias_expands_to_shorthand`.
        // The round-5 fix must not regress this by treating every trailing
        // flag as subcommand-scoped.
        let argv = sv(&["sqry", "@qfuncs", ".", "-c"]);
        let alias = saved_alias("search", &["kind:function"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&["sqry", "kind:function", ".", "-c"])
        );
    }
    }

    #[test]
    fn query_alias_trailing_value_flag_expands_after_command_word() {
        // `sqry query @qfuncs --var k=v`: mirrors
        // `query_alias_value_flag_expands_after_command_word` but with the
        // subcommand-scoped `--var k=v` trailing the alias instead of
        // leading it. A `query` alias always keeps its `query` word
        // (`use_shorthand` requires `command == "search"`), so this
        // placement never had the round-5 shorthand-drop bug; this test
        // locks that in so a future change to the shorthand condition cannot
        // silently reintroduce it for `query` (verivus-oss/sqry#528
        // round 5 completeness audit).
        let argv = sv(&["sqry", "query", "@qfuncs", "--var", "k=v"]);

        let scan = scan_alias_args(&argv).unwrap();
        assert_eq!(scan.alias_index, Some(2));
        assert_eq!(scan.subcommand_prefix, Some(1));
        assert_eq!(scan.remaining_path, None);

        let alias = saved_alias("query", &["kind:$k"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&["sqry", "query", "kind:$k", ".", "--var", "k=v"])
        );
    }

    // -----------------------------------------------------------------------
    // verivus-oss/sqry#528 round 7: `--` end-of-options in the alias tail.
    //
    // `partition_alias_tail` had no `--` handling at all: a bare `--` was
    // itself classified as an unknown flag token, so a hyphen-leading path
    // escaped past it (`sqry @picks -- -5`) got swept into the trailing flags
    // instead of becoming the single path. The direct `sqry search "<pattern>"
    // -- -5` form parses fine (clap's own `--` escape takes over), so the
    // alias form diverged from it at the parse level, not just at runtime.
    // Every case below exits 2 with "unexpected argument" on the round-6 tip
    // (67fcc3e28) and reaches the runtime path check on this branch.
    // -----------------------------------------------------------------------

    #[test]
    fn alias_double_dash_then_path_partitions() {
        // `sqry @picks -- -5`: the `--` marker ends flag classification, so
        // `-5` becomes the sole path instead of a trailing flag token, and the
        // `--` itself is consumed rather than carried through as a flag. On
        // the round-6 tip, `--` was pushed to `post_alias_flags` as an unknown
        // flag and `-5` followed it as a second unknown flag, leaving
        // `remaining_path` unset (verivus-oss/sqry#528 round 7).
        let scan = scan_alias_args(&sv(&["sqry", "@picks", "--", "-5"])).unwrap();
        assert_eq!(scan.alias_index, Some(1));
        assert_eq!(scan.remaining_path.as_deref(), Some("-5"));
        assert!(
            scan.post_alias_flags.is_empty(),
            "the `--` marker must not be carried through as a flag token, got {:?}",
            scan.post_alias_flags
        );
    }

    large_stack_test! {
    #[test]
    fn alias_double_dash_hyphen_path_reemits_escape() {
        // The expanded argv must put a fresh `--` directly in front of the
        // escaped path so the downstream clap parse also treats `-5` as the
        // path, not a flag. On the round-6 tip this expanded to
        // `sqry search pick_ . -- -5` (the default `.` path plus `--`/`-5`
        // both misread as subcommand-scoped-looking flags, which also forced
        // the explicit `search` word back on); here it must be the clean
        // shorthand form with `-5` itself as the path
        // (verivus-oss/sqry#528 round 7).
        let argv = sv(&["sqry", "@picks", "--", "-5"]);
        let alias = saved_alias("search", &["pick_"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(expanded, sv(&["sqry", "pick_", "--", "-5"]));
    }
    }

    large_stack_test! {
    #[test]
    fn alias_double_dash_flag_before_escape_reorders_before_dash_dash() {
        // `sqry query @qfuncs --var k=v -- -5`: a subcommand-scoped flag
        // precedes the `--` escape. The re-emit must place `--var k=v` BEFORE
        // the re-emitted `--` / `-5` pair, not after: clap's `--` covers every
        // token that follows it, so a flag placed after it would be swept
        // into positionals instead of parsing as a flag
        // (verivus-oss/sqry#528 round 7).
        let argv = sv(&["sqry", "query", "@qfuncs", "--var", "k=v", "--", "-5"]);

        let scan = scan_alias_args(&argv).unwrap();
        assert_eq!(scan.remaining_path.as_deref(), Some("-5"));
        assert_eq!(scan.post_alias_flags, sv(&["--var", "k=v"]));

        let alias = saved_alias("query", &["kind:$k"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&["sqry", "query", "kind:$k", "--var", "k=v", "--", "-5"])
        );
    }
    }

    #[test]
    fn alias_double_dash_two_paths_is_ambiguous_error() {
        // `sqry @picks -- a b`: two positionals after the `--` escape is the
        // same ambiguous-alias-invocation error as two positionals without
        // one (`plain_alias_two_paths_is_ambiguous_error`); the alias contract
        // still admits at most one search path.
        let err = scan_alias_args(&sv(&["sqry", "@picks", "--", "a", "b"])).unwrap_err();
        assert!(
            err.to_string().contains("Ambiguous alias invocation"),
            "two paths after -- must error, got {err}"
        );
    }

    // -----------------------------------------------------------------------
    // verivus-oss/sqry#528 round 8: `--` end-of-options honored at the
    // alias-DETECTION and flag-VALUE layers, not just the tail partition.
    //
    // Round 7 taught `partition_alias_tail` about `--`, but two layers still
    // ignored the boundary: `scan_alias_args` / `find_alias_after_prefix` kept
    // scanning past a bare `--` to resolve an `@token` that clap would treat as
    // a literal positional, and `should_skip_next_arg` swallowed a `--` sitting
    // in a value-flag's value slot. Both diverge from the direct clap parse.
    // Every detection-stop case below resolves an alias on the round-7 tip
    // (34bd71927) and resolves NONE (literal positional) here; the flag-value
    // case swallows the `--` there and walls it off here.
    // -----------------------------------------------------------------------

    #[test]
    fn double_dash_before_bare_alias_is_not_detected() {
        // `sqry -- @gpicks`: the first bare `--` ends alias detection, so
        // `@gpicks` is a literal search pattern, not an alias. On the round-7
        // tip the scan skipped the `--` and resolved the alias at index 2.
        let scan = scan_alias_args(&sv(&["sqry", "--", "@gpicks"])).unwrap();
        assert_eq!(scan.alias_index, None);
        assert_eq!(scan.subcommand_prefix, None);
    }

    #[test]
    fn double_dash_after_search_prefix_is_not_detected() {
        // `sqry search -- @gpicks`: the `--` after the `search` prefix word ends
        // alias detection, so `@gpicks` is the literal pattern the `search`
        // subcommand receives, not an alias lookup.
        let scan = scan_alias_args(&sv(&["sqry", "search", "--", "@gpicks"])).unwrap();
        assert_eq!(scan.alias_index, None);
        assert_eq!(scan.subcommand_prefix, None);
    }

    #[test]
    fn double_dash_after_query_prefix_is_not_detected() {
        // `sqry query -- @gpicks`: same as the search-prefix case but for the
        // structured `query` command.
        let scan = scan_alias_args(&sv(&["sqry", "query", "--", "@gpicks"])).unwrap();
        assert_eq!(scan.alias_index, None);
        assert_eq!(scan.subcommand_prefix, None);
    }

    #[test]
    fn double_dash_then_literal_at_string_is_not_unknown_alias() {
        // `sqry search -- @somestring`: on the round-7 tip this resolved an
        // alias named `somestring`, which then failed "Unknown alias". After
        // the boundary fix it is a literal positional (no alias resolution), so
        // the direct clap parse searches for `@somestring` verbatim.
        let scan = scan_alias_args(&sv(&["sqry", "search", "--", "@somestring"])).unwrap();
        assert_eq!(scan.alias_index, None);
    }

    #[test]
    fn double_dash_before_bare_alias_after_global_flag_is_not_detected() {
        // `sqry --json -- @picks`: a global flag then the `--` boundary. The
        // flag does not carry the scan past the `--`, and the `--` stops
        // detection before `@picks`.
        let scan = scan_alias_args(&sv(&["sqry", "--json", "--", "@picks"])).unwrap();
        assert_eq!(scan.alias_index, None);
    }

    #[test]
    fn double_dash_before_prefix_subcommand_is_not_detected() {
        // `sqry -- search @picks`: the leading `--` makes `search` and `@picks`
        // literal positionals (the shorthand pattern/path), never a prefix
        // subcommand + alias.
        let scan = scan_alias_args(&sv(&["sqry", "--", "search", "@picks"])).unwrap();
        assert_eq!(scan.alias_index, None);
        assert_eq!(scan.subcommand_prefix, None);
    }

    #[test]
    fn value_flag_does_not_swallow_double_dash_as_value() {
        // `sqry @picks --cfg-filter -- -5`: `--cfg-filter` takes a value, but a
        // bare `--` is never that value. The flag is emitted value-less and the
        // `--` walls `-5` off as the sole path. On the round-7 tip
        // `should_skip_next_arg` consumed the `--`, so `-5` was swept into the
        // trailing flags and no path was resolved.
        let scan = scan_alias_args(&sv(&["sqry", "@picks", "--cfg-filter", "--", "-5"])).unwrap();
        assert_eq!(scan.alias_index, Some(1));
        assert_eq!(scan.remaining_path.as_deref(), Some("-5"));
        assert_eq!(scan.post_alias_flags, sv(&["--cfg-filter"]));
    }

    #[test]
    fn should_skip_next_arg_refuses_bare_double_dash() {
        // Unit-level guard for the value-consumption choke point: a value flag
        // consumes a normal next token but never a bare `--`.
        assert!(should_skip_next_arg(
            &sv(&["--cfg-filter", "x"]),
            0,
            "--cfg-filter"
        ));
        assert!(!should_skip_next_arg(
            &sv(&["--cfg-filter", "--"]),
            0,
            "--cfg-filter"
        ));
    }

    large_stack_test! {
    #[test]
    fn value_flag_double_dash_expands_to_direct_form() {
        // The expanded argv must be byte-for-byte the direct
        // `sqry search pick_ --cfg-filter -- -5` shape so clap produces the
        // identical "a value is required for '--cfg-filter'" error. The
        // subcommand-scoped `--cfg-filter` keeps the explicit `search` word, and
        // the re-emitted `--` precedes the hyphen path `-5`.
        let argv = sv(&["sqry", "@picks", "--cfg-filter", "--", "-5"]);
        let alias = saved_alias("search", &["pick_"]);
        let expanded = expand_via_scan(&argv, &alias);
        assert_eq!(
            expanded,
            sv(&["sqry", "search", "pick_", "--cfg-filter", "--", "-5"])
        );
    }
    }

    #[test]
    fn trailing_double_dash_with_nothing_after_leaves_no_path() {
        // `sqry @picks --`: the `--` marks end-of-options with no positional
        // after it, so no path is resolved (the default `.` is synthesized
        // later) and the marker is not carried through as a flag.
        let scan = scan_alias_args(&sv(&["sqry", "@picks", "--"])).unwrap();
        assert_eq!(scan.alias_index, Some(1));
        assert_eq!(scan.remaining_path, None);
        assert!(scan.post_alias_flags.is_empty());
    }

    #[test]
    fn double_dash_then_literal_double_dash_is_path() {
        // `sqry @picks -- --`: the first `--` is the boundary, the second is a
        // literal positional (a path named `--`), matching clap's own handling
        // of `sqry search "<pattern>" -- --`.
        let scan = scan_alias_args(&sv(&["sqry", "@picks", "--", "--"])).unwrap();
        assert_eq!(scan.remaining_path.as_deref(), Some("--"));
        assert!(scan.post_alias_flags.is_empty());
    }

    #[test]
    fn double_dash_repeated_two_paths_is_ambiguous_error() {
        // `sqry @picks -- -5 --`: the boundary yields two positionals (`-5` and
        // `--`), the same at-most-one-path ambiguity error, matching clap
        // rejecting a second positional in the direct form.
        let err = scan_alias_args(&sv(&["sqry", "@picks", "--", "-5", "--"])).unwrap_err();
        assert!(
            err.to_string().contains("Ambiguous alias invocation"),
            "two paths after -- must error, got {err}"
        );
    }

    #[test]
    fn equals_joined_flag_then_trailing_double_dash_partitions() {
        // `sqry @picks --cfg-filter=test --`: the glued-value flag consumes
        // nothing from the next token, and the trailing `--` (nothing after)
        // leaves the flag intact with no path.
        let scan = scan_alias_args(&sv(&["sqry", "@picks", "--cfg-filter=test", "--"])).unwrap();
        assert_eq!(scan.remaining_path, None);
        assert_eq!(scan.post_alias_flags, sv(&["--cfg-filter=test"]));
    }

    #[test]
    fn bundled_short_then_trailing_double_dash_partitions() {
        // `sqry @picks -ic --`: a bundled short cluster with no value, then the
        // boundary. The cluster stays whole in the flags and no path resolves.
        let scan = scan_alias_args(&sv(&["sqry", "@picks", "-ic", "--"])).unwrap();
        assert_eq!(scan.remaining_path, None);
        assert_eq!(scan.post_alias_flags, sv(&["-ic"]));
    }
}

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

    large_stack_test! {
    /// #511: `sqry search <PAT> --exact` must parse (the flag is advertised in
    /// `sqry search --help`). Before the fix `--exact` only bound at the top
    /// level, so this invocation was rejected as an unknown argument.
    #[test]
    fn search_exact_flag_parses_on_subcommand() {
        let cli = args::Cli::parse_from(["sqry", "search", "GrokClient", "--exact"]);
        assert!(
            matches!(
                cli.command.as_deref(),
                Some(args::Command::Search { exact: true, .. })
            ),
            "`search --exact` must set the Search variant's exact flag"
        );
        // The short form `-x` must parse identically.
        let cli_short = args::Cli::parse_from(["sqry", "search", "GrokClient", "-x"]);
        assert!(matches!(
            cli_short.command.as_deref(),
            Some(args::Command::Search { exact: true, .. })
        ));
    }
    }

    large_stack_test! {
    /// #511: the top-level shorthand `sqry --exact <PAT>` still parses onto the
    /// `Cli::exact` field with no subcommand. Regression guard so folding the
    /// subcommand flag never breaks the shorthand.
    #[test]
    fn top_level_exact_shorthand_still_parses() {
        let cli = args::Cli::parse_from(["sqry", "--exact", "GrokClient"]);
        assert!(cli.command.is_none(), "shorthand must not select a subcommand");
        assert!(cli.exact, "top-level --exact must set Cli::exact");
    }
    }

    large_stack_test! {
    /// #517: `sqry graph stats --format json` (format AFTER the operation) must
    /// parse now that `Graph::format` is `global = true`. Before the fix the
    /// arg only bound before the operation and this was rejected.
    #[test]
    fn graph_format_after_operation_parses() {
        let cli = args::Cli::parse_from(["sqry", "graph", "stats", "--format", "json"]);
        match cli.command.as_deref() {
            Some(args::Command::Graph { format, operation, .. }) => {
                assert_eq!(format.as_deref(), Some("json"));
                assert!(matches!(operation, args::GraphOperation::Stats { .. }));
            }
            other => panic!("expected Command::Graph, got {other:?}"),
        }
    }
    }

    large_stack_test! {
    /// #517: `sqry graph --format json stats` (format BEFORE the operation)
    /// must keep parsing. Regression guard for the pre-existing placement.
    #[test]
    fn graph_format_before_operation_still_parses() {
        let cli = args::Cli::parse_from(["sqry", "graph", "--format", "json", "stats"]);
        match cli.command.as_deref() {
            Some(args::Command::Graph { format, operation, .. }) => {
                assert_eq!(format.as_deref(), Some("json"));
                assert!(matches!(operation, args::GraphOperation::Stats { .. }));
            }
            other => panic!("expected Command::Graph, got {other:?}"),
        }
    }
    }
}
