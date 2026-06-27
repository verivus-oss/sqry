//! Interactive session shell for repeated semantic queries.
//!
//! Provides a REPL that keeps the `.sqry-index` cache warm via
//! [`SessionManager`] so subsequent queries execute quickly.

use crate::args::Cli;
use crate::output::{DisplaySymbol, FormatterMetadata, OutputStreams, create_formatter};
use anyhow::{Context, Result, anyhow};
use rustyline::history::History;
use rustyline::{DefaultEditor, error::ReadlineError};
use sqry_core::json_response::Filters;
use sqry_core::query::results::QueryResults;
use sqry_core::session::{SessionConfig, SessionManager};
use std::path::{Path, PathBuf};
use std::time::Instant;

const PROMPT: &str = "sqry> ";

/// Execute the interactive shell command.
///
/// # Errors
/// Returns an error if the index cannot be loaded, the session fails to start,
/// or terminal input cannot be read.
pub fn run_shell(cli: &Cli, path: &str) -> Result<()> {
    let workspace = PathBuf::from(path);
    ensure_index_exists(&workspace)?;

    // Create session manager for unified graph caching
    let config = SessionConfig::default();
    let session =
        SessionManager::with_config(config).context("failed to initialise session manager")?;

    // Warm the cache so the first query is instant.
    let start = Instant::now();
    session
        .preload(&workspace)
        .with_context(|| format!("failed to load index from {}", workspace.display()))?;
    let preload_elapsed = start.elapsed();

    println!(
        "Loaded index from {} in {}ms",
        workspace.display(),
        preload_elapsed.as_millis()
    );
    println!("sqry shell - type 'help' for commands, 'exit' to quit");

    let mut rl = DefaultEditor::new()?;
    loop {
        match rl.readline(PROMPT) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if handle_command(cli, trimmed, &session, &workspace, &mut rl) {
                    break;
                }
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C - show helpful message and continue.
                println!("(Press Ctrl+D or type 'exit' to quit)");
            }
            Err(ReadlineError::Eof) => {
                println!("Goodbye!");
                break;
            }
            Err(err) => {
                return Err(anyhow!("failed to read input: {err}"));
            }
        }
    }

    Ok(())
}

fn ensure_index_exists(path: &Path) -> Result<()> {
    use sqry_core::graph::unified::persistence::GraphStorage;

    // Check for new unified graph format first
    let storage = GraphStorage::new(path);
    if storage.exists() {
        return Ok(());
    }

    // Fall back to legacy format check
    let legacy_index_path = path.join(".sqry-index");
    if legacy_index_path.exists() {
        return Ok(());
    }

    Err(anyhow!(
        "no index found at {}. Run `sqry index {}` first.",
        path.display(),
        path.display()
    ))
}

/// Returns `true` if the caller requested to exit the shell.
fn handle_command(
    cli: &Cli,
    input: &str,
    session: &SessionManager,
    workspace: &Path,
    rl: &mut DefaultEditor,
) -> bool {
    match parse_meta_command(input) {
        ShellControl::Help => {
            print_help();
            false
        }
        ShellControl::Stats => {
            print_stats(session);
            false
        }
        ShellControl::Refresh => {
            if let Err(err) = refresh_session(session, workspace) {
                eprintln!("Error: {err}");
            }
            false
        }
        ShellControl::Clear => {
            print!("\x1B[2J\x1B[1;1H");
            false
        }
        ShellControl::History => {
            print_history(rl);
            false
        }
        ShellControl::Exit => true,
        ShellControl::Query(query) => {
            match execute_query(cli, session, workspace, query) {
                Ok(()) => {
                    let _ = rl.add_history_entry(query);
                }
                Err(err) => {
                    eprintln!("Error: {err}");
                }
            }
            false
        }
    }
}

fn print_help() {
    println!("Available commands:");
    println!("  help      - Show this help message");
    println!("  stats     - Show session statistics");
    println!("  refresh   - Reload the index from disk");
    println!("  clear     - Clear the screen");
    println!("  history   - Show previous queries");
    println!("  exit      - Exit the shell");
    println!();
    println!("Enter a query expression (e.g., kind:function AND name:test) to search.");
}

fn print_stats(session: &SessionManager) {
    let stats = session.stats();
    let total_cache_events = stats.cache_hits + stats.cache_misses;
    let hit_rate = if total_cache_events > 0 {
        (u64_to_f64_lossy(stats.cache_hits) / u64_to_f64_lossy(total_cache_events)) * 100.0
    } else {
        0.0
    };

    println!("Session statistics:");
    println!("  Cached graphs  : {}", stats.cached_graphs);
    println!("  Total queries  : {}", stats.total_queries);
    println!(
        "  Cache hits     : {} ({hit_rate:.1}% hit rate)",
        stats.cache_hits
    );
    println!("  Cache misses   : {}", stats.cache_misses);
    println!("  Estimated memory: ~{} MB", stats.total_memory_mb);
}

fn refresh_session(session: &SessionManager, workspace: &Path) -> Result<()> {
    let start = Instant::now();
    session
        .invalidate(workspace)
        .with_context(|| format!("failed to invalidate session for {}", workspace.display()))?;
    session
        .preload(workspace)
        .with_context(|| format!("failed to reload index for {}", workspace.display()))?;
    let elapsed = start.elapsed();

    println!(
        "Index reloaded in {}ms for {}",
        elapsed.as_millis(),
        workspace.display()
    );

    Ok(())
}

fn print_history(rl: &DefaultEditor) {
    let history = rl.history();
    if history.len() == 0 {
        println!("History is empty.");
        return;
    }

    for (idx, entry) in history.iter().enumerate() {
        println!("{:>3}: {}", idx + 1, entry);
    }
}

fn u64_to_f64_lossy(value: u64) -> f64 {
    let narrowed = u32::try_from(value).unwrap_or(u32::MAX);
    f64::from(narrowed)
}

/// Convert `QueryResults` to `Vec<DisplaySymbol>` for display purposes.
fn query_results_to_display_symbols(results: &QueryResults) -> Vec<DisplaySymbol> {
    results
        .iter()
        .map(|m| DisplaySymbol::from_query_match(&m))
        .collect()
}

fn execute_query(cli: &Cli, session: &SessionManager, workspace: &Path, query: &str) -> Result<()> {
    let limit = cli.limit.unwrap_or(100);
    let count_only = cli.count;

    let before_stats = session.stats();
    let start = Instant::now();
    let query_results = session
        .query(workspace, query)
        .with_context(|| format!("failed to execute query \"{query}\""))?;
    let elapsed = start.elapsed();
    let after_stats = session.stats();

    let total_matches = query_results.len();

    if count_only {
        println!("{total_matches}");
        return Ok(());
    }

    // Convert to Vec<DisplaySymbol> for display
    let mut results = query_results_to_display_symbols(&query_results);

    if limit > 0 && results.len() > limit {
        results.truncate(limit);
    }

    let mut streams = OutputStreams::with_pager(cli.pager_config());
    let formatter = create_formatter(cli);
    let metadata = FormatterMetadata {
        pattern: Some(query.to_string()),
        total_matches,
        execution_time: elapsed,
        filters: Filters {
            kind: None,
            lang: None,
            ignore_case: cli.ignore_case,
            exact: cli.exact,
            fuzzy: None,
        },
        index_age_seconds: None,
        used_ancestor_index: None,
        filtered_to: None,
        revision: None,
    };

    formatter.format(&results, Some(&metadata), &mut streams)?;

    if !cli.json && total_matches > limit && limit > 0 {
        streams.write_diagnostic(&format!(
            "\nShowing {limit} of {total_matches} matches (use --limit to adjust)"
        ))?;
    }

    if !cli.json {
        let served_from_cache = after_stats.cache_hits > before_stats.cache_hits;
        let cache_status = if served_from_cache {
            "cache hit"
        } else {
            "cache miss"
        };

        streams.write_diagnostic(&format!(
            "{} results in {}ms — {}",
            total_matches,
            elapsed.as_millis(),
            cache_status
        ))?;
    }

    streams.finish_checked()
}

fn parse_meta_command(input: &str) -> ShellControl<'_> {
    match input {
        "help" | ".help" => ShellControl::Help,
        "stats" | ".stats" => ShellControl::Stats,
        "refresh" | ".refresh" => ShellControl::Refresh,
        "clear" | ".clear" => ShellControl::Clear,
        "history" | ".history" => ShellControl::History,
        "exit" | ".exit" | "quit" | ".quit" => ShellControl::Exit,
        other => ShellControl::Query(other),
    }
}

enum ShellControl<'a> {
    Help,
    Stats,
    Refresh,
    Clear,
    History,
    Exit,
    Query(&'a str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn parse_meta_command_recognises_aliases() {
        assert!(matches!(parse_meta_command("help"), ShellControl::Help));
        assert!(matches!(parse_meta_command(".stats"), ShellControl::Stats));
        assert!(matches!(parse_meta_command("exit"), ShellControl::Exit));

        if let ShellControl::Query(query) = parse_meta_command("kind:function") {
            assert_eq!(query, "kind:function");
        } else {
            panic!("expected query variant");
        }
    }

    #[test]
    fn ensure_index_exists_validates_presence() {
        let temp = tempdir().unwrap();
        assert!(ensure_index_exists(temp.path()).is_err());

        let index_path = temp.path().join(".sqry-index");
        fs::File::create(&index_path).unwrap();

        ensure_index_exists(temp.path()).unwrap();
    }
}
