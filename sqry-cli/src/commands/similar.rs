//! Similar command implementation
//!
//! Provides CLI interface for finding similar symbols using fuzzy matching.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result, anyhow};
use serde::Serialize;

/// Similar symbols output
#[derive(Debug, Serialize)]
struct SimilarOutput {
    /// Reference symbol
    reference: NodeRef,
    /// Similar symbols found
    similar: Vec<SimilarSymbol>,
    /// Statistics
    stats: SimilarStats,
}

#[derive(Debug, Serialize)]
struct NodeRef {
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
    line: u32,
}

#[derive(Debug, Serialize)]
struct SimilarSymbol {
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
    line: u32,
    /// Similarity score (0.0 - 1.0)
    similarity: f64,
}

#[derive(Debug, Serialize)]
struct SimilarStats {
    total_found: usize,
    threshold: f64,
}

/// Run the similar command.
///
/// # Errors
/// Returns an error if the graph cannot be loaded or symbol cannot be found.
// The CLI flow is linear and readability outweighs splitting into helpers.
#[allow(clippy::too_many_lines)]
pub fn run_similar(
    cli: &Cli,
    file_path: &str,
    symbol_name: &str,
    path: Option<&str>,
    threshold: f64,
    max_results: usize,
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
    let graph = load_unified_graph_for_cli(&loc.index_root, &config, cli)
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    let strings = graph.strings();
    let files_registry = graph.files();
    let target_file = std::path::Path::new(file_path);

    // Find the reference symbol in the unified graph
    let (ref_node_id, ref_entry) = graph
        .nodes()
        .iter()
        .find(|(_, entry)| {
            // Check if file matches
            let sym_file = files_registry.resolve(entry.file);
            let file_matches = sym_file
                .as_ref()
                .is_some_and(|p| p.as_ref() == target_file || p.ends_with(file_path));

            if !file_matches {
                return false;
            }

            // Check if name matches
            let name = strings.resolve(entry.name);
            let qname = entry.qualified_name.and_then(|id| strings.resolve(id));

            name.is_some_and(|n| n.as_ref() == symbol_name)
                || qname.is_some_and(|q| q.as_ref() == symbol_name)
        })
        .ok_or_else(|| anyhow!("Symbol '{symbol_name}' not found in '{file_path}'"))?;

    let ref_name = strings
        .resolve(ref_entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let ref_qualified_name = ref_entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| ref_name.clone(), |s| s.to_string());

    let ref_file_path = files_registry
        .resolve(ref_entry.file)
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let reference = NodeRef {
        name: ref_name.clone(),
        qualified_name: ref_qualified_name.clone(),
        kind: format!("{:?}", ref_entry.kind),
        file: ref_file_path,
        line: ref_entry.start_line,
    };

    // Find similar symbols (same kind, similar name)
    let mut similar_symbols: Vec<_> = graph
        .nodes()
        .iter()
        .filter(|(node_id, entry)| {
            // Skip the reference symbol itself
            if *node_id == ref_node_id {
                return false;
            }
            // Must be same kind
            if entry.kind != ref_entry.kind {
                return false;
            }
            true
        })
        .filter_map(|(_, entry)| {
            let name = strings.resolve(entry.name)?;
            let similarity = compute_similarity(&ref_name, &name);

            if similarity >= threshold {
                let file_path = files_registry
                    .resolve(entry.file)
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();

                let qualified_name = entry
                    .qualified_name
                    .and_then(|id| strings.resolve(id))
                    .map_or_else(|| name.to_string(), |s| s.to_string());

                Some(SimilarSymbol {
                    name: name.to_string(),
                    qualified_name,
                    kind: format!("{:?}", entry.kind),
                    file: file_path,
                    line: entry.start_line,
                    similarity,
                })
            } else {
                None
            }
        })
        .collect();

    // Sort by similarity (descending)
    similar_symbols.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    similar_symbols.truncate(max_results);

    let stats = SimilarStats {
        total_found: similar_symbols.len(),
        threshold,
    };

    let output = SimilarOutput {
        reference,
        similar: similar_symbols,
        stats,
    };

    // Output
    if cli.json {
        let json = serde_json::to_string_pretty(&output).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        let text = format_similar_text(&output);
        streams.write_result(&text)?;
    }

    Ok(())
}

/// Compute similarity between two strings using Levenshtein distance.
fn compute_similarity(a: &str, b: &str) -> f64 {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();

    if a_lower == b_lower {
        return 1.0;
    }

    let distance = levenshtein_distance(&a_lower, &b_lower);
    let max_len = a_lower.len().max(b_lower.len());

    if max_len == 0 {
        return 1.0;
    }

    let distance_f = f64::from(u32::try_from(distance).unwrap_or(u32::MAX));
    let max_len_f = f64::from(u32::try_from(max_len).unwrap_or(u32::MAX));
    1.0 - (distance_f / max_len_f)
}

/// Calculate Levenshtein distance between two strings.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let a_len = a_chars.len();
    let b_len = b_chars.len();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut matrix = vec![vec![0usize; b_len + 1]; a_len + 1];

    for (i, row) in matrix.iter_mut().enumerate().take(a_len + 1) {
        row[0] = i;
    }
    for (j, val) in matrix[0].iter_mut().enumerate().take(b_len + 1) {
        *val = j;
    }

    for i in 1..=a_len {
        for j in 1..=b_len {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            matrix[i][j] = (matrix[i - 1][j] + 1)
                .min(matrix[i][j - 1] + 1)
                .min(matrix[i - 1][j - 1] + cost);
        }
    }

    matrix[a_len][b_len]
}

fn format_similar_text(output: &SimilarOutput) -> String {
    let mut lines = Vec::new();

    lines.push(format!(
        "Finding symbols similar to: {} [{}]",
        output.reference.qualified_name, output.reference.kind
    ));
    lines.push(format!(
        "Threshold: {:.0}%, Found: {}",
        output.stats.threshold * 100.0,
        output.stats.total_found
    ));
    lines.push(String::new());

    if output.similar.is_empty() {
        lines.push("No similar symbols found.".to_string());
    } else {
        lines.push("Similar symbols:".to_string());
        for sym in &output.similar {
            lines.push(format!(
                "  {} ({:.0}% similar)",
                sym.qualified_name,
                sym.similarity * 100.0
            ));
            lines.push(format!("    {}:{}", sym.file, sym.line));
        }
    }

    lines.join("\n")
}
