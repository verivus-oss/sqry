//! Hierarchical search command implementation
//!
//! Provides CLI interface for RAG-optimized semantic search with grouping.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;

/// Hierarchical search output
#[derive(Debug, Serialize)]
struct HierarchicalOutput {
    /// Query executed
    query: String,
    /// Files containing matches
    files: Vec<FileGroup>,
    /// Statistics
    stats: HierarchicalStats,
}

#[derive(Debug, Serialize)]
struct FileGroup {
    /// File path
    path: String,
    /// Language
    language: String,
    /// Symbols in this file
    symbols: Vec<HierSymbol>,
    /// Estimated token count
    estimated_tokens: usize,
}

#[derive(Debug, Serialize)]
struct HierSymbol {
    name: String,
    qualified_name: String,
    kind: String,
    line: u32,
    /// Relevance score
    score: f64,
}

#[derive(Debug, Serialize)]
#[allow(clippy::struct_field_names)] // Keep total_* prefixes for readability in output.
struct HierarchicalStats {
    total_files: usize,
    total_symbols: usize,
    total_estimated_tokens: usize,
}

/// Run the hierarchical search command.
///
/// # Errors
/// Returns an error if the graph cannot be loaded or query fails.
// The output shaping is a single pipeline; splitting would obscure flow.
#[allow(clippy::too_many_lines)]
pub fn run_hier_search(
    cli: &Cli,
    query: &str,
    path: Option<&str>,
    max_results: usize,
    max_files: usize,
    context_lines: usize,
    kinds: &[String],
    languages: &[String],
) -> Result<()> {
    let mut streams = OutputStreams::new();

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

    let strings = graph.strings();
    let files = graph.files();
    let query_lower = query.to_lowercase();

    // Filter and score symbols from unified graph
    let mut scored_symbols: Vec<_> = graph
        .nodes()
        .iter()
        .filter(|(_, entry)| {
            // Apply kind filter
            if !kinds.is_empty() {
                let kind_str = format!("{:?}", entry.kind);
                if !kinds.iter().any(|k| k.eq_ignore_ascii_case(&kind_str)) {
                    return false;
                }
            }
            // Apply language filter
            if !languages.is_empty() {
                let lang = files
                    .language_for_file(entry.file)
                    .map(|l| l.to_string())
                    .unwrap_or_default();
                if !languages.iter().any(|l| l.eq_ignore_ascii_case(&lang)) {
                    return false;
                }
            }
            // Match query against name or qualified name
            let name = strings.resolve(entry.name).map(|s| s.to_lowercase());
            let qname = entry
                .qualified_name
                .and_then(|id| strings.resolve(id))
                .map(|s| s.to_lowercase());

            name.as_ref().is_some_and(|n| n.contains(&query_lower))
                || qname.as_ref().is_some_and(|q| q.contains(&query_lower))
        })
        .map(|(_, entry)| {
            let name = strings
                .resolve(entry.name)
                .map(|s| s.to_string())
                .unwrap_or_default();
            let score = compute_relevance(
                &name,
                entry.visibility.and_then(|v| strings.resolve(v)),
                &query_lower,
            );
            (entry, score)
        })
        .collect();

    // Sort by score
    scored_symbols.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored_symbols.truncate(max_results);

    // Group by file
    let mut file_groups: HashMap<String, FileGroup> = HashMap::new();

    for (entry, score) in &scored_symbols {
        let file_path = files
            .resolve(entry.file)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let language = files
            .language_for_file(entry.file)
            .map_or_else(|| "Unknown".to_string(), |l| l.to_string());

        let file_group = file_groups
            .entry(file_path.clone())
            .or_insert_with(|| FileGroup {
                path: file_path,
                language,
                symbols: Vec::new(),
                estimated_tokens: 0,
            });

        let name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let qualified_name = entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .map_or_else(|| name.clone(), |s| s.to_string());

        let hier_sym = HierSymbol {
            name,
            qualified_name,
            kind: format!("{:?}", entry.kind),
            line: entry.start_line,
            score: *score,
        };

        file_group.symbols.push(hier_sym);
        file_group.estimated_tokens += estimate_symbol_tokens(entry, context_lines);
    }

    // Convert to sorted vec
    let mut files_vec: Vec<FileGroup> = file_groups.into_values().collect();
    files_vec.sort_by(|a, b| b.estimated_tokens.cmp(&a.estimated_tokens));
    files_vec.truncate(max_files);

    // Sort symbols within each file
    for file in &mut files_vec {
        file.symbols.sort_by(|a, b| a.line.cmp(&b.line));
    }

    let stats = HierarchicalStats {
        total_files: files_vec.len(),
        total_symbols: scored_symbols.len(),
        total_estimated_tokens: files_vec.iter().map(|f| f.estimated_tokens).sum(),
    };

    let output = HierarchicalOutput {
        query: query.to_string(),
        files: files_vec,
        stats,
    };

    // Output
    if cli.json {
        let json = serde_json::to_string_pretty(&output).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        let text = format_hier_text(&output);
        streams.write_result(&text)?;
    }

    Ok(())
}

fn compute_relevance(name: &str, visibility: Option<std::sync::Arc<str>>, query: &str) -> f64 {
    let mut score: f64 = 0.5; // Base score for matching
    let name_lower = name.to_lowercase();

    // Boost for exact name match
    if name_lower == query {
        score += 0.3;
    } else if name_lower.starts_with(query) {
        score += 0.2;
    }

    // Boost for public visibility
    if visibility.is_some_and(|v| v.as_ref() == "public") {
        score += 0.1;
    }

    score.min(1.0)
}

fn estimate_symbol_tokens(
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    context_lines: usize,
) -> usize {
    // Rough estimate: ~10 tokens per line of code
    let lines = (entry.end_line.saturating_sub(entry.start_line) + 1) as usize;
    let with_context = lines + (context_lines * 2);
    with_context * 10
}

fn format_hier_text(output: &HierarchicalOutput) -> String {
    let mut lines = Vec::new();

    lines.push(format!("Hierarchical search: {}", output.query));
    lines.push(format!(
        "Found {} symbols in {} files (~{} tokens)",
        output.stats.total_symbols, output.stats.total_files, output.stats.total_estimated_tokens
    ));
    lines.push(String::new());

    for file in &output.files {
        lines.push(format!(
            "File: {} [{}] (~{} tokens)",
            file.path, file.language, file.estimated_tokens
        ));

        for sym in &file.symbols {
            lines.push(format!(
                "  {} [{}] line {} (score: {:.2})",
                sym.qualified_name, sym.kind, sym.line, sym.score
            ));
        }

        lines.push(String::new());
    }

    lines.join("\n")
}
