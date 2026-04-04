//! Execute multiple semantic queries from a batch file using the unified graph.
//!
//! The `sqry batch` command loads queries from a file and executes them using
//! the unified graph format (`.sqry/graph/snapshot.sqry`). Supports parallel
//! execution and multiple output formats.

use crate::args::{BatchFormat, Cli};
use crate::commands::query::create_executor_with_plugins_for_cli;
use crate::output::{DisplaySymbol, JsonSymbol};
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::Serialize;
use sqry_core::query::results::QueryResults;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEFAULT_BATCH_LIMIT: usize = 1_000;

/// Run the batch command using the provided CLI configuration.
///
/// # Errors
/// Returns an error if queries cannot be loaded, the index is missing,
/// or any query execution fails.
pub fn run_batch(
    cli: &Cli,
    path: &str,
    queries_path: &Path,
    output: BatchFormat,
    output_file: Option<&Path>,
    continue_on_error: bool,
    stats: bool,
    sequential: bool,
) -> Result<()> {
    let workspace = PathBuf::from(path);
    ensure_index_exists(&workspace)?;

    let queries = load_queries(queries_path)
        .with_context(|| format!("failed to read queries from {}", queries_path.display()))?;
    if queries.is_empty() {
        bail!(
            "no queries found in {} (ensure file contains non-empty, non-comment lines)",
            queries_path.display()
        );
    }

    let load_start = Instant::now();
    let executor = create_executor_with_plugins_for_cli(cli, &workspace)?;
    let preload_elapsed = load_start.elapsed();

    let should_capture_results =
        !cli.count && matches!(output, BatchFormat::Json | BatchFormat::Jsonl);
    let limit = cli.limit.unwrap_or(DEFAULT_BATCH_LIMIT);

    let outcomes = execute_queries(
        &executor,
        &workspace,
        &queries,
        limit,
        should_capture_results,
        continue_on_error,
        sequential,
    )?;

    let summary = BatchSummary::from_outcomes(&outcomes);

    let render_options = RenderOutputOptions {
        output,
        workspace: &workspace,
        preload_elapsed,
        summary: &summary,
        should_capture_results,
        limit,
        stats,
    };
    let rendered = render_output(&outcomes, &render_options)?;

    write_output(output_file, &rendered)?;

    Ok(())
}

fn ensure_index_exists(path: &Path) -> Result<()> {
    use sqry_core::graph::unified::persistence::GraphStorage;

    let storage = GraphStorage::new(path);
    if storage.exists() {
        return Ok(());
    }

    bail!(
        "no index found at {}. Run `sqry index {}` first.",
        path.display(),
        path.display()
    );
}

fn load_queries(path: &Path) -> Result<Vec<String>> {
    let file = File::open(path)
        .with_context(|| format!("failed to open queries file {}", path.display()))?;
    let reader = BufReader::new(file);

    let mut queries = Vec::new();
    for line in reader.lines() {
        let line = line.context("failed to read query line")?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        queries.push(trimmed.to_string());
    }

    Ok(queries)
}

/// Convert `QueryResults` to `Vec<DisplaySymbol>` for display purposes.
fn query_results_to_display_symbols(results: &QueryResults) -> Vec<DisplaySymbol> {
    results
        .iter()
        .map(|m| DisplaySymbol::from_query_match(&m))
        .collect()
}

struct QueryExecution {
    position: usize,
    query: String,
    result: Result<Vec<DisplaySymbol>>,
    elapsed: Duration,
}

fn execute_single_query(
    executor: &sqry_core::query::QueryExecutor,
    workspace: &Path,
    idx: usize,
    query: &str,
) -> QueryExecution {
    let position = idx + 1;
    let start = Instant::now();
    let query_results = executor.execute_on_graph(query, workspace);
    let elapsed = start.elapsed();

    // Convert QueryResults to Vec<DisplaySymbol> for batch processing
    let result = query_results.map(|qr| query_results_to_display_symbols(&qr));

    QueryExecution {
        position,
        query: query.to_string(),
        result,
        elapsed,
    }
}

fn process_query_result(
    execution: QueryExecution,
    total_queries: usize,
    limit: usize,
    should_capture_results: bool,
    continue_on_error: bool,
) -> Result<QueryOutcome> {
    let QueryExecution {
        position,
        query,
        result,
        elapsed,
    } = execution;

    eprintln!("[{position}/{total_queries}] {query}");

    match result {
        Ok(mut symbols) => {
            let total_matches = symbols.len();

            if limit > 0 && limit < symbols.len() {
                symbols.truncate(limit);
            }

            eprintln!(
                "[{position}/{total_queries}] ok: {} results in {}ms",
                total_matches,
                elapsed.as_millis()
            );

            let displayed_matches = symbols.len();
            let captured_results = if should_capture_results {
                Some(symbols)
            } else {
                None
            };

            Ok(QueryOutcome::Success(BatchEntry {
                position,
                query,
                elapsed,
                total_matches,
                displayed_matches,
                results: captured_results,
            }))
        }
        Err(err) => {
            let message = err.to_string();
            eprintln!("[{position}/{total_queries}] error: {message}");

            if continue_on_error {
                Ok(QueryOutcome::Failure(BatchFailedEntry {
                    position,
                    query,
                    error: message,
                }))
            } else {
                Err(err).context(format!("failed to execute query \"{query}\""))
            }
        }
    }
}

fn execute_queries(
    executor: &sqry_core::query::QueryExecutor,
    workspace: &Path,
    queries: &[String],
    limit: usize,
    should_capture_results: bool,
    continue_on_error: bool,
    sequential: bool,
) -> Result<Vec<QueryOutcome>> {
    // Execute queries in parallel (or sequential if flag is set)
    // P2-7 Phase 2: Parallel batch execution using Rayon
    let intermediate_results: Vec<_> = if sequential {
        // Sequential fallback for debugging
        queries
            .iter()
            .enumerate()
            .map(|(idx, query)| execute_single_query(executor, workspace, idx, query))
            .collect()
    } else {
        // Parallel execution (default)
        queries
            .par_iter()
            .enumerate()
            .map(|(idx, query)| execute_single_query(executor, workspace, idx, query))
            .collect()
    };

    // Process results sequentially to print progress messages in order.
    let mut outcomes = Vec::with_capacity(queries.len());
    for execution in intermediate_results {
        let outcome = process_query_result(
            execution,
            queries.len(),
            limit,
            should_capture_results,
            continue_on_error,
        )?;
        outcomes.push(outcome);
    }

    Ok(outcomes)
}

struct RenderOutputOptions<'a> {
    output: BatchFormat,
    workspace: &'a Path,
    preload_elapsed: Duration,
    summary: &'a BatchSummary,
    should_capture_results: bool,
    limit: usize,
    stats: bool,
}

fn render_output(outcomes: &[QueryOutcome], options: &RenderOutputOptions<'_>) -> Result<String> {
    let mut rendered = match options.output {
        BatchFormat::Text => render_text(outcomes, options.limit),
        BatchFormat::Csv => render_csv(outcomes),
        BatchFormat::Json => render_json(
            outcomes,
            options.workspace,
            options.preload_elapsed,
            options.summary,
            options.should_capture_results,
        )?,
        BatchFormat::Jsonl => render_jsonl(outcomes, options.should_capture_results)?,
    };

    if options.stats && matches!(options.output, BatchFormat::Text | BatchFormat::Csv) {
        let stats_block = render_stats_block(options.preload_elapsed, options.summary);
        if !stats_block.is_empty() {
            if !rendered.is_empty() && !rendered.ends_with('\n') {
                rendered.push('\n');
            }
            rendered.push_str(&stats_block);
        }
    }

    Ok(rendered)
}

fn render_text(outcomes: &[QueryOutcome], limit: usize) -> String {
    use std::fmt::Write as _;

    let mut lines = Vec::new();
    for outcome in outcomes {
        match outcome {
            QueryOutcome::Success(entry) => {
                let mut line = format!(
                    "Query {}: {} ({}ms) - {} results",
                    entry.position,
                    entry.query,
                    entry.elapsed.as_millis(),
                    entry.total_matches
                );
                if entry.total_matches != entry.displayed_matches && limit > 0 {
                    let _ = write!(
                        line,
                        " (showing {} results; use --limit to adjust)",
                        entry.displayed_matches
                    );
                }
                lines.push(line);
            }
            QueryOutcome::Failure(entry) => {
                lines.push(format!("Query {} failed: {}", entry.position, entry.error));
            }
        }
    }

    lines.join("\n")
}

fn render_csv(outcomes: &[QueryOutcome]) -> String {
    let mut rows = Vec::with_capacity(outcomes.len() + 1);
    rows.push("position,query,elapsed_ms,result_count,displayed_count,error".to_string());

    for outcome in outcomes {
        match outcome {
            QueryOutcome::Success(entry) => {
                rows.push(format!(
                    "{},{},{},{},{},",
                    entry.position,
                    csv_escape(&entry.query),
                    entry.elapsed.as_millis(),
                    entry.total_matches,
                    entry.displayed_matches
                ));
            }
            QueryOutcome::Failure(entry) => {
                rows.push(format!(
                    "{},{},,,,{}",
                    entry.position,
                    csv_escape(&entry.query),
                    csv_escape(&entry.error)
                ));
            }
        }
    }

    rows.join("\n")
}

fn render_json(
    outcomes: &[QueryOutcome],
    workspace: &Path,
    preload_elapsed: Duration,
    summary: &BatchSummary,
    capture_results: bool,
) -> Result<String> {
    let mut queries = Vec::new();
    let mut errors = Vec::new();

    for outcome in outcomes {
        match outcome {
            QueryOutcome::Success(entry) => {
                let results = if capture_results {
                    entry
                        .results
                        .as_ref()
                        .map(|symbols| symbols.iter().map(JsonSymbol::from).collect())
                } else {
                    None
                };

                queries.push(BatchJsonQuery {
                    position: entry.position,
                    query: entry.query.clone(),
                    elapsed_ms: entry.elapsed.as_millis(),
                    result_count: entry.total_matches,
                    displayed_count: entry.displayed_matches,
                    truncated_count: if entry.total_matches == entry.displayed_matches {
                        None
                    } else {
                        Some(entry.displayed_matches)
                    },
                    results,
                });
            }
            QueryOutcome::Failure(entry) => errors.push(BatchJsonError {
                position: entry.position,
                query: entry.query.clone(),
                error: entry.error.clone(),
            }),
        }
    }

    let payload = BatchJsonPayload {
        session: BatchJsonSession {
            path: workspace.display().to_string(),
            executor_setup_ms: preload_elapsed.as_millis(),
            total_queries: summary.total_queries,
            success_count: summary.success_count,
            failure_count: summary.failure_count,
            total_query_time_ms: summary.total_query_time.as_millis(),
            average_query_time_ms: summary.average_query_time_ms(),
        },
        queries,
        errors,
    };

    Ok(serde_json::to_string_pretty(&payload)?)
}

fn render_jsonl(outcomes: &[QueryOutcome], capture_results: bool) -> Result<String> {
    let mut lines = Vec::new();

    for outcome in outcomes {
        match outcome {
            QueryOutcome::Success(entry) => {
                let results = if capture_results {
                    entry
                        .results
                        .as_ref()
                        .map(|symbols| symbols.iter().map(JsonSymbol::from).collect())
                } else {
                    None
                };

                let row = serde_json::to_string(&BatchJsonQuery {
                    position: entry.position,
                    query: entry.query.clone(),
                    elapsed_ms: entry.elapsed.as_millis(),
                    result_count: entry.total_matches,
                    displayed_count: entry.displayed_matches,
                    truncated_count: if entry.total_matches == entry.displayed_matches {
                        None
                    } else {
                        Some(entry.displayed_matches)
                    },
                    results,
                })?;
                lines.push(row);
            }
            QueryOutcome::Failure(entry) => {
                let row = serde_json::to_string(&BatchJsonError {
                    position: entry.position,
                    query: entry.query.clone(),
                    error: entry.error.clone(),
                })?;
                lines.push(row);
            }
        }
    }

    Ok(lines.join("\n"))
}

fn render_stats_block(preload_elapsed: Duration, summary: &BatchSummary) -> String {
    let mut lines = Vec::new();

    lines.push("Batch Statistics:".to_string());
    lines.push(format!(
        "  Executor setup time: {} ms",
        preload_elapsed.as_millis()
    ));
    lines.push(format!("  Total queries: {}", summary.total_queries));
    lines.push(format!("  Successful queries: {}", summary.success_count));
    lines.push(format!("  Failed queries: {}", summary.failure_count));
    lines.push(format!(
        "  Total query time: {} ms",
        summary.total_query_time.as_millis()
    ));
    if let Some(avg) = summary.average_query_time_ms() {
        lines.push(format!("  Average query time: {avg} ms"));
    }

    lines.join("\n")
}

fn write_output(path: Option<&Path>, content: &str) -> Result<()> {
    if content.is_empty() {
        return Ok(());
    }

    if let Some(file_path) = path {
        let mut writer = BufWriter::new(
            File::create(file_path)
                .with_context(|| format!("failed to create {}", file_path.display()))?,
        );
        writer.write_all(content.as_bytes())?;
        if !content.ends_with('\n') {
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
    } else {
        print!("{content}");
        if !content.ends_with('\n') {
            println!();
        }
    }

    Ok(())
}

fn csv_escape(value: &str) -> String {
    let needs_quotes = value.contains(',') || value.contains('"') || value.contains('\n');
    if needs_quotes {
        let escaped = value.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

/// Per-query outcome recorded during execution.
enum QueryOutcome {
    Success(BatchEntry),
    Failure(BatchFailedEntry),
}

/// Successful query execution details.
struct BatchEntry {
    position: usize,
    query: String,
    elapsed: Duration,
    total_matches: usize,
    displayed_matches: usize,
    results: Option<Vec<DisplaySymbol>>,
}

/// Failed query details.
struct BatchFailedEntry {
    position: usize,
    query: String,
    error: String,
}

/// Aggregate execution summary used for stats/output.
struct BatchSummary {
    total_queries: usize,
    success_count: usize,
    failure_count: usize,
    total_query_time: Duration,
}

impl BatchSummary {
    fn from_outcomes(outcomes: &[QueryOutcome]) -> Self {
        let mut success_count = 0usize;
        let mut total_time = Duration::ZERO;

        for outcome in outcomes {
            if let QueryOutcome::Success(entry) = outcome {
                success_count += 1;
                total_time += entry.elapsed;
            }
        }

        let total_queries = outcomes.len();
        let failure_count = total_queries.saturating_sub(success_count);
        Self {
            total_queries,
            success_count,
            failure_count,
            total_query_time: total_time,
        }
    }

    fn average_query_time_ms(&self) -> Option<u128> {
        if self.success_count == 0 {
            None
        } else {
            Some(self.total_query_time.as_millis() / self.success_count as u128)
        }
    }
}

#[derive(Serialize)]
struct BatchJsonPayload {
    session: BatchJsonSession,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    queries: Vec<BatchJsonQuery>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    errors: Vec<BatchJsonError>,
}

#[derive(Serialize)]
struct BatchJsonSession {
    path: String,
    executor_setup_ms: u128,
    total_queries: usize,
    success_count: usize,
    failure_count: usize,
    total_query_time_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    average_query_time_ms: Option<u128>,
}

#[derive(Serialize)]
struct BatchJsonQuery {
    position: usize,
    query: String,
    elapsed_ms: u128,
    result_count: usize,
    displayed_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "truncated")]
    truncated_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    results: Option<Vec<JsonSymbol>>,
}

#[derive(Serialize)]
struct BatchJsonError {
    position: usize,
    query: String,
    error: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn create_test_display_symbol(name: &str, file: &str, line: usize) -> DisplaySymbol {
        let mut metadata = HashMap::new();
        metadata.insert("__raw_language".to_string(), "rust".to_string());
        metadata.insert("__raw_file_path".to_string(), file.to_string());
        DisplaySymbol {
            name: name.to_string(),
            qualified_name: format!("test::{name}"),
            kind: "function".to_string(),
            file_path: PathBuf::from(file),
            start_line: line,
            start_column: 1,
            end_line: line,
            end_column: 10,
            metadata,
            caller_identity: None,
            callee_identity: None,
        }
    }

    #[test]
    fn load_queries_ignores_comments_and_blank_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("queries.txt");
        fs::write(
            &path,
            "# comment\nkind:function\n\n   callers(main)\n# another\n",
        )
        .unwrap();

        let queries = load_queries(&path).unwrap();
        assert_eq!(queries, vec!["kind:function", "callers(main)"]);
    }

    #[test]
    fn csv_escape_quotes_fields() {
        assert_eq!(csv_escape("kind:function"), "kind:function");
        assert_eq!(csv_escape("name,kind"), "\"name,kind\"");
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn render_text_includes_limit_note() {
        let outcomes = vec![QueryOutcome::Success(BatchEntry {
            position: 1,
            query: "kind:function".to_string(),
            elapsed: Duration::from_millis(42),
            total_matches: 120,
            displayed_matches: 100,
            results: None,
        })];

        let rendered = render_text(&outcomes, 100);
        assert!(rendered.contains("Query 1: kind:function (42ms) - 120 results"));
        assert!(rendered.contains("showing 100 results"));
    }

    #[test]
    fn render_json_includes_session_metadata() {
        let symbol = create_test_display_symbol("test_func", "src/lib.rs", 5);

        let outcomes = vec![QueryOutcome::Success(BatchEntry {
            position: 1,
            query: "kind:function".to_string(),
            elapsed: Duration::from_millis(40),
            total_matches: 1,
            displayed_matches: 1,
            results: Some(vec![symbol]),
        })];

        let summary = BatchSummary::from_outcomes(&outcomes);
        let rendered = render_json(
            &outcomes,
            Path::new("/tmp/workspace"),
            Duration::from_millis(885),
            &summary,
            true,
        )
        .unwrap();

        assert!(rendered.contains("\"path\": \"/tmp/workspace\""));
        assert!(rendered.contains("\"executor_setup_ms\": 885"));
        assert!(rendered.contains("\"queries\""));
    }

    #[test]
    fn render_jsonl_outputs_lines() {
        let outcomes = vec![
            QueryOutcome::Success(BatchEntry {
                position: 1,
                query: "kind:function".to_string(),
                elapsed: Duration::from_millis(40),
                total_matches: 2,
                displayed_matches: 2,
                results: Some(vec![]),
            }),
            QueryOutcome::Failure(BatchFailedEntry {
                position: 2,
                query: "broken".to_string(),
                error: "syntax error".to_string(),
            }),
        ];

        let rendered = render_jsonl(&outcomes, false).unwrap();
        let lines: Vec<_> = rendered.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"position\":1"));
        assert!(lines[1].contains("\"error\""));
    }
}
