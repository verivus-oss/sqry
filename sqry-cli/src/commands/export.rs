//! Export command implementation
//!
//! Provides CLI interface for exporting the code graph in various formats.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use sqry_core::graph::Language;
use sqry_core::visualization::unified::{
    D2Config, Direction, DotConfig, EdgeFilter, JsonConfig, MermaidConfig, UnifiedD2Exporter,
    UnifiedDotExporter, UnifiedJsonExporter, UnifiedMermaidExporter,
};
use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

/// Run the export command.
///
/// # Errors
/// Returns an error if the graph cannot be loaded or exported.
#[allow(clippy::too_many_arguments)]
pub fn run_export(
    cli: &Cli,
    path: Option<&str>,
    format: &str,
    direction: &str,
    filter_lang: Option<&str>,
    filter_edge: Option<&str>,
    highlight_cross: bool,
    show_details: bool,
    show_labels: bool,
    output_file: Option<&str>,
) -> Result<()> {
    let mut streams = OutputStreams::new();

    // Find workspace root
    let root = path.map_or_else(
        || std::env::current_dir().unwrap_or_default(),
        PathBuf::from,
    );

    // Load unified graph
    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&root, &config, cli, no_op_reporter())
        .context("Failed to load unified graph. Run 'sqry index' first.")?;

    let snapshot = graph.snapshot();

    // Parse direction
    let dir = match direction.to_lowercase().as_str() {
        "tb" | "topbottom" | "top-bottom" => Direction::TopToBottom,
        _ => Direction::LeftToRight,
    };

    // Parse language filters
    let filter_languages: HashSet<Language> = filter_lang
        .map(|s| {
            s.split(',')
                .filter_map(|l| parse_language(l.trim()))
                .collect()
        })
        .unwrap_or_default();

    // Parse edge filters
    let filter_edges: HashSet<EdgeFilter> = filter_edge
        .map(|s| {
            s.split(',')
                .filter_map(|e| parse_edge_filter(e.trim()))
                .collect()
        })
        .unwrap_or_default();

    // Export based on format
    let output = match format.to_lowercase().as_str() {
        "dot" | "graphviz" => {
            let config = DotConfig {
                filter_languages,
                filter_edges,
                filter_files: HashSet::new(),
                filter_node_ids: None,
                highlight_cross_language: highlight_cross,
                max_depth: None,
                root_nodes: HashSet::new(),
                direction: dir,
                show_details,
                show_edge_labels: show_labels,
            };
            let exporter = UnifiedDotExporter::with_config(&snapshot, config);
            exporter.export()
        }
        "d2" => {
            let config = D2Config {
                filter_languages,
                filter_edges,
                filter_node_ids: None,
                highlight_cross_language: highlight_cross,
                show_details,
                show_edge_labels: show_labels,
                direction: dir,
            };
            let exporter = UnifiedD2Exporter::with_config(&snapshot, config);
            exporter.export()
        }
        "mermaid" | "md" => {
            let config = MermaidConfig {
                filter_languages,
                filter_edges,
                highlight_cross_language: highlight_cross,
                show_edge_labels: show_labels,
                direction: dir,
                filter_node_ids: None,
            };
            let exporter = UnifiedMermaidExporter::with_config(&snapshot, config);
            exporter.export()
        }
        "json" => {
            let config = JsonConfig {
                include_details: show_details,
                include_edge_metadata: show_labels,
            };
            let exporter = UnifiedJsonExporter::with_config(&snapshot, config);
            serde_json::to_string_pretty(&exporter.export()).context("Failed to serialize JSON")?
        }
        _ => {
            return Err(anyhow::anyhow!(
                "Unknown format: {format}. Use: dot, d2, mermaid, json"
            ));
        }
    };

    // Write output
    if let Some(file_path) = output_file {
        let mut file = File::create(file_path)
            .with_context(|| format!("Failed to create output file: {file_path}"))?;
        file.write_all(output.as_bytes())
            .context("Failed to write output")?;
        streams.write_diagnostic(&format!("Exported to {file_path}"))?;
    } else {
        streams.write_result(&output)?;
    }

    Ok(())
}

/// Parse a language string to Language enum
fn parse_language(s: &str) -> Option<Language> {
    match s.to_lowercase().as_str() {
        "rust" | "rs" => Some(Language::Rust),
        "javascript" | "js" => Some(Language::JavaScript),
        "typescript" | "ts" => Some(Language::TypeScript),
        "python" | "py" => Some(Language::Python),
        "go" => Some(Language::Go),
        "java" => Some(Language::Java),
        "ruby" | "rb" => Some(Language::Ruby),
        "php" => Some(Language::Php),
        "cpp" | "c++" => Some(Language::Cpp),
        "c" => Some(Language::C),
        "swift" => Some(Language::Swift),
        "kotlin" | "kt" => Some(Language::Kotlin),
        "scala" => Some(Language::Scala),
        "sql" => Some(Language::Sql),
        "shell" | "bash" | "sh" => Some(Language::Shell),
        "lua" => Some(Language::Lua),
        "perl" | "pl" => Some(Language::Perl),
        "dart" => Some(Language::Dart),
        "groovy" => Some(Language::Groovy),
        "css" => Some(Language::Css),
        "elixir" | "ex" => Some(Language::Elixir),
        "r" => Some(Language::R),
        "haskell" | "hs" => Some(Language::Haskell),
        "html" => Some(Language::Html),
        "svelte" => Some(Language::Svelte),
        "vue" => Some(Language::Vue),
        "zig" => Some(Language::Zig),
        "terraform" | "tf" => Some(Language::Terraform),
        "puppet" => Some(Language::Puppet),
        "apex" => Some(Language::Apex),
        "abap" => Some(Language::Abap),
        "csharp" | "cs" | "c#" => Some(Language::CSharp),
        "http" => Some(Language::Http),
        "plsql" | "pl/sql" | "oracle" => Some(Language::Plsql),
        "servicenow" | "xanadu" => Some(Language::ServiceNow),
        _ => None,
    }
}

/// Parse an edge filter string
fn parse_edge_filter(s: &str) -> Option<EdgeFilter> {
    match s.to_lowercase().as_str() {
        "calls" | "call" => Some(EdgeFilter::Calls),
        "imports" | "import" => Some(EdgeFilter::Imports),
        "exports" | "export" => Some(EdgeFilter::Exports),
        "references" | "reference" | "refs" => Some(EdgeFilter::References),
        "inherits" | "inherit" | "extends" => Some(EdgeFilter::Inherits),
        "implements" | "implement" => Some(EdgeFilter::Implements),
        "ffi" | "fficall" => Some(EdgeFilter::FfiCall),
        "http" | "httprequest" => Some(EdgeFilter::HttpRequest),
        "db" | "dbquery" | "database" => Some(EdgeFilter::DbQuery),
        _ => None,
    }
}
