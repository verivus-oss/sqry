//! `sqry plan-query` — structural query execution through the sqry-db planner.
//!
//! Bridges the text-syntax parser ([`sqry_db::planner::parse_query`]) and the
//! plan executor ([`sqry_db::planner::execute_plan`]) to a user-facing CLI
//! command. DB13 scope note: the legacy `sqry query` engine remains alongside
//! this subcommand; DB14+ migrates the traversal handlers and eventually
//! replaces the legacy path.
//!
//! # Output
//!
//! Results are printed one per line as
//! `<kind> <qualified_or_short_name> <file>:<line>`. When `cli.json` is set,
//! each row is serialized as a JSON object instead, matching the shape other
//! sqry CLI commands use for JSON output.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

use sqry_db::planner::{ParseError, execute_plan, parse_query};
use sqry_db::queries::dispatch::make_query_db_cold;

/// One row of CLI / JSON output describing a matched node.
#[derive(Debug, Clone, Serialize)]
pub struct PlanQueryHit {
    /// Symbol's short name (as interned in the graph).
    pub name: String,
    /// Fully qualified name if the graph recorded one; else copies `name`.
    pub qualified_name: String,
    /// `NodeKind` in lowercase snake_case form.
    pub kind: String,
    /// Filesystem path of the file containing this symbol.
    pub file: String,
    /// 1-based line number of the symbol's starting location.
    pub line: u32,
    /// Optional visibility string captured on the node entry.
    pub visibility: Option<String>,
}

/// Runs the `sqry plan-query` subcommand.
///
/// # Errors
///
/// Returns an error if:
/// - No `.sqry-index` can be discovered from the working directory.
/// - The indexed graph fails to load.
/// - The text query fails to parse or the resulting plan fails validation.
pub fn run_planner_query(cli: &Cli, query: &str, path: Option<&str>, limit: usize) -> Result<()> {
    let mut streams = OutputStreams::new();

    let search_path = path.map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        PathBuf::from,
    );

    let Some(location) = find_nearest_index(&search_path) else {
        streams.write_diagnostic(
            "No .sqry-index found. Run 'sqry index' first to build the graph index.",
        )?;
        return Ok(());
    };

    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&location.index_root, &config, cli)
        .context("failed to load graph; run 'sqry index' to rebuild")?;

    let plan = parse_query(query).map_err(format_parse_error)?;
    let snapshot = Arc::new(graph.snapshot());
    let db = make_query_db_cold(Arc::clone(&snapshot), &location.index_root);

    let node_ids = execute_plan(&plan, &db);
    let mut hits: Vec<PlanQueryHit> = Vec::with_capacity(node_ids.len().min(limit));

    for node_id in node_ids.into_iter().take(limit) {
        let Some(entry) = snapshot.nodes().get(node_id) else {
            continue;
        };
        let strings = snapshot.strings();
        let files = snapshot.files();
        let name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let qualified_name = entry
            .qualified_name
            .and_then(|sid| strings.resolve(sid))
            .map_or_else(|| name.clone(), |s| s.to_string());
        let file = files
            .resolve(entry.file)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let visibility = entry
            .visibility
            .and_then(|sid| strings.resolve(sid))
            .map(|s| s.to_string());

        hits.push(PlanQueryHit {
            name,
            qualified_name,
            kind: entry.kind.as_str().to_string(),
            file,
            line: entry.start_line,
            visibility,
        });
    }

    if cli.json {
        let payload =
            serde_json::to_string_pretty(&hits).context("serializing plan-query hits as JSON")?;
        streams.write_result(&payload)?;
    } else {
        for hit in &hits {
            streams.write_result(&format!(
                "{} {} {}:{}",
                hit.kind, hit.qualified_name, hit.file, hit.line
            ))?;
        }
    }

    Ok(())
}

/// Wraps a [`ParseError`] with a caret-pointer diagnostic so CLI users can
/// see exactly where the parser choked.
fn format_parse_error(err: ParseError) -> anyhow::Error {
    anyhow::anyhow!("query parse error: {err}")
}
