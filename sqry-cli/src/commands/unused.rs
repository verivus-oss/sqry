//! Unused command implementation
//!
//! Provides CLI interface for finding unused/dead code in the codebase.
//!
//! # Dispatch path (DB18)
//!
//! `unused` is a **name-keyed predicate** under the Phase 3C dispatch
//! taxonomy: the question is "which nodes match this scope AND are not
//! reachable from any entry point", which is exactly the planner-canonical
//! contract that sqry-db's [`sqry_db::queries::UnusedQuery`] caches, keyed
//! on [`sqry_db::queries::UnusedKey`]. See also
//! [`sqry_mcp::execution::tools::analysis::execute_find_unused`] — this
//! CLI handler mirrors the same MCP-style superset-key + post-filter
//! pattern so CLI and MCP share one cache behavior.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use serde::Serialize;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::query::UnusedScope;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Unused symbol for output
#[derive(Debug, Serialize)]
struct UnusedSymbol {
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
    line: u32,
    language: String,
    visibility: String,
}

/// Unused symbols grouped by file
#[derive(Debug, Serialize)]
struct UnusedByFile {
    file: String,
    count: usize,
    symbols: Vec<UnusedSymbol>,
}

/// Run the unused command.
///
/// # Dispatch path (DB18)
///
/// The handler acquires a per-call [`sqry_db::QueryDb`] via
/// [`sqry_db::queries::dispatch::make_query_db`], dispatches
/// [`sqry_db::queries::UnusedQuery`] keyed on
/// [`sqry_db::queries::UnusedKey`], and applies the CLI's free-form
/// `--lang` / `--kind` substring filters as a post-filter (they are
/// user-supplied free-form substrings, not the structured filter lists
/// sqry-db consumes — sqry-db must see the full candidate set so
/// filtered-out prefixes cannot push valid later matches out of the
/// window).
///
/// The handler asks sqry-db for `node_count` rows (an upper bound; `UnusedQuery`
/// early-breaks once the underlying graph is exhausted) so the CLI substring
/// filters and the binding-plane post-filter are the single authoritative
/// gates on what reaches the user. This mirrors the MCP pattern in
/// [`sqry_mcp::execution::tools::analysis::execute_find_unused`].
///
/// The `--scope` argument maps one-to-one onto
/// [`sqry_core::query::UnusedScope`] (the same enum sqry-db consumes),
/// so no scope-superset widening is needed here — unlike MCP's
/// `UnusedScope::Struct` which is strictly broader than sqry-db's
/// (MCP's `Struct` includes `Interface | Trait`).
///
/// # Errors
/// Returns an error if the graph cannot be loaded.
pub fn run_unused(
    cli: &Cli,
    path: Option<&str>,
    scope: &str,
    lang_filter: Option<&str>,
    kind_filter: Option<&str>,
    max_results: usize,
) -> Result<()> {
    let mut streams = OutputStreams::new();

    // Parse scope
    let unused_scope = UnusedScope::try_parse(scope).with_context(|| {
        format!("Invalid scope: {scope}. Use: public, private, function, struct, all")
    })?;

    // Find index
    let search_path = path.map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        std::path::PathBuf::from,
    );

    let index_location = find_nearest_index(&search_path);
    let Some(ref loc) = index_location else {
        streams
            .write_diagnostic("No .sqry-index found. Run 'sqry index' first to build the index.")?;
        return Ok(());
    };

    // Load unified graph
    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&loc.index_root, &config, cli, no_op_reporter())
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    // Route through sqry-db: `UnusedQuery` is a name-keyed predicate in
    // the planner taxonomy, cached per-snapshot. Boundary filters can
    // suppress raw rows even when the user supplied no `--lang` / `--kind`
    // filter, so request the full candidate pool and truncate only after all
    // user-facing filters run.
    let snapshot = std::sync::Arc::new(graph.snapshot());
    let unused_ids =
        boundary_filtered_unused_ids(&snapshot, &loc.index_root, unused_scope, max_results);

    let strings = snapshot.strings();
    let files = snapshot.files();

    // Post-filter: apply --lang and --kind substring filters + truncate
    // at max_results.
    let mut unused_symbols: Vec<UnusedSymbol> = Vec::new();
    for &node_id in &unused_ids {
        if unused_symbols.len() >= max_results {
            break;
        }

        let Some(entry) = snapshot.nodes().get(node_id) else {
            continue;
        };

        let language = files
            .language_for_file(entry.file)
            .map_or_else(|| "Unknown".to_string(), |l| l.to_string());

        if let Some(lang) = lang_filter
            && !language.to_lowercase().contains(&lang.to_lowercase())
        {
            continue;
        }

        if let Some(kind) = kind_filter {
            let kind_str = format!("{:?}", entry.kind).to_lowercase();
            if !kind_str.contains(&kind.to_lowercase()) {
                continue;
            }
        }

        let name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();

        let qualified_name = entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .map_or_else(|| name.clone(), |s| s.to_string());

        let file_path = files
            .resolve(entry.file)
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        let visibility = entry
            .visibility
            .and_then(|id| strings.resolve(id))
            .map_or_else(|| "unknown".to_string(), |s| s.to_string());

        unused_symbols.push(UnusedSymbol {
            name,
            qualified_name,
            kind: format!("{:?}", entry.kind),
            file: file_path,
            line: entry.start_line,
            language,
            visibility,
        });
    }

    // Group by file for text output
    let mut by_file: HashMap<String, Vec<UnusedSymbol>> = HashMap::new();
    for sym in unused_symbols {
        by_file.entry(sym.file.clone()).or_default().push(sym);
    }

    let mut grouped: Vec<UnusedByFile> = by_file
        .into_iter()
        .map(|(file, symbols)| UnusedByFile {
            file,
            count: symbols.len(),
            symbols,
        })
        .collect();

    // Sort by file path
    grouped.sort_by(|a, b| a.file.cmp(&b.file));

    // Output
    if cli.json {
        let json = serde_json::to_string_pretty(&grouped).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        let output = format_unused_text(&grouped, unused_scope);
        streams.write_result(&output)?;
    }

    Ok(())
}

fn boundary_filtered_unused_ids(
    snapshot: &Arc<GraphSnapshot>,
    index_root: &Path,
    unused_scope: UnusedScope,
    max_results: usize,
) -> Vec<NodeId> {
    // PN3 CLIENT_LOAD: opportunistic cold-load from workspace companion file.
    let db = sqry_db::queries::dispatch::make_query_db_cold(Arc::clone(snapshot), index_root);
    let candidate_cap = snapshot.nodes().len().max(max_results);
    let key = sqry_db::queries::UnusedKey {
        scope: unused_scope,
        max_results: candidate_cap,
    };
    let raw_unused_ids = db.get::<sqry_db::queries::UnusedQuery>(&key);
    sqry_db::queries::unused_post_filter::apply_binding_plane_post_filter(
        &raw_unused_ids,
        snapshot,
        &db,
    )
}

/// Format unused symbols as human-readable text
fn format_unused_text(groups: &[UnusedByFile], scope: UnusedScope) -> String {
    let mut lines = Vec::new();

    let total: usize = groups.iter().map(|g| g.count).sum();
    let scope_name = match scope {
        UnusedScope::Public => "public",
        UnusedScope::Private => "private",
        UnusedScope::Function => "function",
        UnusedScope::Struct => "struct",
        UnusedScope::All => "all",
    };

    lines.push(format!(
        "Found {total} unused symbols (scope: {scope_name})"
    ));
    lines.push(String::new());

    for group in groups {
        lines.push(format!("{} ({} unused):", group.file, group.count));
        for sym in &group.symbols {
            lines.push(format!("  {} [{}] line {}", sym.name, sym.kind, sym.line));
        }
        lines.push(String::new());
    }

    if groups.is_empty() {
        lines.push("No unused symbols found.".to_string());
    }

    lines.join("\n")
}
