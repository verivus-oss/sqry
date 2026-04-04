//! Unused command implementation
//!
//! Provides CLI interface for finding unused/dead code in the codebase.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use serde::Serialize;
use sqry_core::query::{UnusedScope, compute_reachable_set_graph, is_node_unused};
use std::collections::HashMap;

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
    let graph = load_unified_graph_for_cli(&loc.index_root, &config, cli)
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    // Compute reachable set once for performance
    let reachable = compute_reachable_set_graph(&graph);

    let strings = graph.strings();
    let files = graph.files();

    // Find unused symbols
    let mut unused_symbols: Vec<UnusedSymbol> = Vec::new();
    let mut count = 0;

    for (node_id, entry) in graph.nodes().iter() {
        if count >= max_results {
            break;
        }

        // Get language
        let language = files
            .language_for_file(entry.file)
            .map_or_else(|| "Unknown".to_string(), |l| l.to_string());

        // Apply language filter
        if let Some(lang) = lang_filter
            && !language.to_lowercase().contains(&lang.to_lowercase())
        {
            continue;
        }

        // Apply kind filter
        if let Some(kind) = kind_filter {
            let kind_str = format!("{:?}", entry.kind).to_lowercase();
            if !kind_str.contains(&kind.to_lowercase()) {
                continue;
            }
        }

        // Check if unused
        if is_node_unused(node_id, unused_scope, &graph, Some(&reachable)) {
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
            count += 1;
        }
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
