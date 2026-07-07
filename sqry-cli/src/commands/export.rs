//! Export command implementation
//!
//! Provides CLI interface for exporting the code graph in various formats.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
use crate::output::OutputStreams;
use anyhow::{Context, Result, bail};
use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::materialize::find_nodes_by_name;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::visualization::archify::{
    ArchifyConfig, DEFAULT_MAX_COMPONENTS, export_archify_json,
};
use sqry_core::visualization::subgraph::SeededSubgraphConfig;
use sqry_core::visualization::unified::{
    D2Config, Direction, DotConfig, EdgeFilter, JsonConfig, MermaidConfig, UnifiedD2Exporter,
    UnifiedDotExporter, UnifiedJsonExporter, UnifiedMermaidExporter,
};
use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

/// Parameters for the `sqry export` command.
///
/// Grouped into a struct so the archify-scoped seed arguments (`symbol`,
/// `file`, `max_depth`, `max_results`) can be threaded through without a
/// double-digit positional signature.
pub struct ExportArgs<'a> {
    /// Shared CLI context (config, plugin selection).
    pub cli: &'a Cli,
    /// Search path (defaults to the current directory).
    pub path: Option<&'a str>,
    /// Output format (`dot`, `d2`, `mermaid`, `json`, `archify`).
    pub format: &'a str,
    /// Graph layout direction (`lr` / `tb`).
    pub direction: &'a str,
    /// Comma-separated language filter.
    pub filter_lang: Option<&'a str>,
    /// Comma-separated edge-kind filter.
    pub filter_edge: Option<&'a str>,
    /// Highlight cross-language edges.
    pub highlight_cross: bool,
    /// Include node details (signatures, docs).
    pub show_details: bool,
    /// Show edge labels.
    pub show_labels: bool,
    /// Output file (default: stdout).
    pub output_file: Option<&'a str>,
    /// Archify seed symbol name.
    pub symbol: Option<&'a str>,
    /// Archify seed file path.
    pub file: Option<&'a str>,
    /// Archify BFS depth (default 2, cap 5).
    pub max_depth: Option<usize>,
    /// Archify node-visit cap (default 1000).
    pub max_results: Option<usize>,
}

/// Run the export command.
///
/// # Errors
/// Returns an error if the graph cannot be loaded or exported, or if the
/// archify format is requested without a seed (`--symbol` / `--file`).
pub fn run_export(args: ExportArgs<'_>) -> Result<()> {
    let ExportArgs {
        cli,
        path,
        format,
        direction,
        filter_lang,
        filter_edge,
        highlight_cross,
        show_details,
        show_labels,
        output_file,
        symbol,
        file,
        max_depth,
        max_results,
    } = args;

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

    // Archify runs a dedicated seeded path with its own edge retention.
    if format.eq_ignore_ascii_case("archify") {
        let output = render_archify(&snapshot, &root, symbol, file, max_depth, max_results)?;
        return write_export_output(&mut streams, &output, output_file);
    }

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
                "Unknown format: {format}. Use: dot, d2, mermaid, json, archify"
            ));
        }
    };

    write_export_output(&mut streams, &output, output_file)
}

/// Write export output to a file or stdout.
fn write_export_output(
    streams: &mut OutputStreams,
    output: &str,
    output_file: Option<&str>,
) -> Result<()> {
    if let Some(file_path) = output_file {
        let mut file = File::create(file_path)
            .with_context(|| format!("Failed to create output file: {file_path}"))?;
        file.write_all(output.as_bytes())
            .context("Failed to write output")?;
        streams.write_diagnostic(&format!("Exported to {file_path}"))?;
    } else {
        streams.write_result(output)?;
    }
    Ok(())
}

/// Render the Archify architecture JSON for a seeded subgraph.
///
/// v1 requires an explicit seed (`--symbol` or `--file`): there is no implicit
/// whole-repository auto-seed, which keeps the export deterministic and
/// reviewable. Seed resolution happens here at the CLI layer (it hands concrete
/// `NodeId`s to the entry-point-agnostic core builder), matching the crate
/// layering: entry-point detection is a `sqry-db` concern and stays out of the
/// core traversal.
fn render_archify(
    snapshot: &GraphSnapshot,
    root: &std::path::Path,
    symbol: Option<&str>,
    file: Option<&str>,
    max_depth: Option<usize>,
    max_results: Option<usize>,
) -> Result<String> {
    let mut seeds: Vec<NodeId> = Vec::new();
    if let Some(name) = symbol {
        seeds.extend(find_nodes_by_name(snapshot, name));
        if seeds.is_empty() {
            bail!(
                "Archify seed symbol '{name}' not found. Run 'sqry index' first, or check the name."
            );
        }
    }
    if let Some(file_path) = file {
        let file_seeds = resolve_file_seeds(snapshot, root, file_path);
        if file_seeds.is_empty() {
            bail!(
                "Archify seed file '{file_path}' has no indexed symbols (unknown or empty file)."
            );
        }
        seeds.extend(file_seeds);
    }

    if seeds.is_empty() {
        bail!(
            "Archify export requires an explicit seed. Pass --symbol <name> or --file <path>. \
             v1 does not auto-seed the whole repository."
        );
    }

    let subgraph_config = SeededSubgraphConfig {
        max_depth: max_depth.unwrap_or(sqry_core::visualization::subgraph::DEFAULT_MAX_DEPTH),
        max_results: max_results.unwrap_or(sqry_core::visualization::subgraph::DEFAULT_MAX_RESULTS),
        languages: Vec::new(),
    }
    .normalized();

    let seed_label = match (symbol, file) {
        (Some(s), _) => s.to_string(),
        (None, Some(f)) => f.to_string(),
        _ => String::new(),
    };
    let archify_config = ArchifyConfig {
        seed_label,
        title: String::new(),
        max_depth: subgraph_config.max_depth,
        max_components: DEFAULT_MAX_COMPONENTS,
    };

    export_archify_json(snapshot, &seeds, &subgraph_config, &archify_config)
        .context("Failed to build Archify architecture JSON")
}

/// Resolve every indexed symbol defined in `file_path` to a seed `NodeId`.
///
/// Matches on the graph's workspace-relative file path (forward-slashed):
/// exact match, or the node path ending in the requested suffix, so both
/// `src/api.rs` and `api.rs` resolve intuitively.
fn resolve_file_seeds(
    snapshot: &GraphSnapshot,
    root: &std::path::Path,
    file_path: &str,
) -> Vec<NodeId> {
    let normalized = normalize_seed_path(root, file_path);
    let files = snapshot.files();
    let mut seeds = Vec::new();
    for (node_id, entry) in snapshot.iter_nodes() {
        if entry.is_unified_loser() {
            continue;
        }
        let Some(node_file) = files.resolve(entry.file) else {
            continue;
        };
        let node_path = node_file.to_string_lossy().replace('\\', "/");
        if node_path == normalized
            || node_path.ends_with(&format!("/{normalized}"))
            || node_path.ends_with(&format!("/{file_path}"))
            || node_path == file_path
        {
            seeds.push(node_id);
        }
    }
    seeds
}

/// Normalize a seed file path to the graph's workspace-relative convention.
fn normalize_seed_path(root: &std::path::Path, file_path: &str) -> String {
    let raw = std::path::Path::new(file_path);
    let relative = raw.strip_prefix(root).unwrap_or(raw);
    relative.to_string_lossy().replace('\\', "/")
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
