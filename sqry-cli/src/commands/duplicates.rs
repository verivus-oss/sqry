//! Duplicates command implementation
//!
//! Provides CLI interface for finding duplicate code in the codebase.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use serde::Serialize;
use sqry_core::query::{DuplicateConfig, DuplicateType, build_duplicate_groups_graph};

/// Duplicate group for output
#[derive(Debug, Serialize)]
struct DuplicateGroupOutput {
    /// Group identifier (hash as 32-char hex string for body duplicates, 16-char for others)
    ///
    /// For body duplicates with 128-bit `body_hash`, this is formatted as a 32-character
    /// lowercase hexadecimal string (e.g., "000000000000000012345678abcdef01").
    /// For signature/struct duplicates, this is a 16-character hex string from the u64 hash.
    group_id: String,
    /// Number of duplicates in this group
    count: usize,
    /// Symbols in this group
    symbols: Vec<DuplicateSymbol>,
}

/// Symbol info for duplicate output
#[derive(Debug, Serialize)]
struct DuplicateSymbol {
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
    line: u32,
    language: String,
}

/// Run the duplicates command.
///
/// # Errors
/// Returns an error if the graph cannot be loaded or duplicates cannot be found.
pub fn run_duplicates(
    cli: &Cli,
    path: Option<&str>,
    dup_type: &str,
    threshold: u32,
    max_results: usize,
    exact: bool,
) -> Result<()> {
    let mut streams = OutputStreams::new();

    // Parse duplicate type
    let duplicate_type: DuplicateType = dup_type
        .parse()
        .with_context(|| format!("Invalid duplicate type: {dup_type}"))?;

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
    let graph_config = GraphLoadConfig::default();
    let graph = load_unified_graph(&loc.index_root, &graph_config)
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    // Build config
    let config = DuplicateConfig {
        threshold: if exact {
            1.0
        } else {
            f64::from(threshold) / 100.0
        },
        max_results,
        is_exact_only: exact || threshold >= 100,
    };

    // Find duplicates using graph-based detection
    let groups = build_duplicate_groups_graph(duplicate_type, &graph, &config);

    let strings = graph.strings();
    let files = graph.files();

    // Convert to output format
    let mut output_groups: Vec<DuplicateGroupOutput> = groups
        .into_iter()
        .filter(|g| g.node_ids.len() > 1)
        .map(|group| {
            let symbols: Vec<DuplicateSymbol> = group
                .node_ids
                .iter()
                .filter_map(|&node_id| {
                    let entry = graph.nodes().get(node_id)?;

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

                    let language = files
                        .language_for_file(entry.file)
                        .map_or_else(|| "Unknown".to_string(), |l| l.to_string());

                    Some(DuplicateSymbol {
                        name,
                        qualified_name,
                        kind: format!("{:?}", entry.kind),
                        file: file_path,
                        line: entry.start_line,
                        language,
                    })
                })
                .collect();

            // Format group_id as hex string
            // - For body duplicates with 128-bit hash: 32-char hex
            // - For others: 16-char hex from u64
            let group_id = if let Some(body_hash) = group.body_hash_128 {
                format!("{body_hash}") // BodyHash128::Display is 32-char hex
            } else {
                format!("{:016x}", group.hash)
            };

            DuplicateGroupOutput {
                group_id,
                count: symbols.len(),
                symbols,
            }
        })
        .filter(|g| g.count > 1)
        .collect();

    // Sort by group size (largest first) for deterministic output
    // Secondary sort by group_id string for stable ordering
    output_groups.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.group_id.cmp(&b.group_id))
    });
    output_groups.truncate(max_results);

    // Output
    if cli.json {
        let json =
            serde_json::to_string_pretty(&output_groups).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        let output = format_duplicates_text(&output_groups, duplicate_type);
        streams.write_result(&output)?;
    }

    Ok(())
}

/// Format duplicates as human-readable text
fn format_duplicates_text(groups: &[DuplicateGroupOutput], dup_type: DuplicateType) -> String {
    let mut lines = Vec::new();

    let type_name = match dup_type {
        DuplicateType::Body => "body",
        DuplicateType::Signature => "signature",
        DuplicateType::Struct => "struct",
    };

    lines.push(format!(
        "Found {} duplicate groups (type: {})",
        groups.len(),
        type_name
    ));
    lines.push(String::new());

    for (i, group) in groups.iter().enumerate() {
        lines.push(format!("Group {} ({} duplicates):", i + 1, group.count));
        for sym in &group.symbols {
            lines.push(format!(
                "  {} [{}] {}:{}",
                sym.qualified_name, sym.kind, sym.file, sym.line
            ));
        }
        lines.push(String::new());
    }

    if groups.is_empty() {
        lines.push("No duplicates found.".to_string());
    }

    lines.join("\n")
}
