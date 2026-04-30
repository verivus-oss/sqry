//! Explain command implementation
//!
//! Provides CLI interface for explaining a symbol with context and relations.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
use crate::commands::impact::{emit_ambiguous_symbol_error, emit_symbol_not_found};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use sqry_core::graph::unified::FileScope;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::resolution::SymbolResolveError;
use sqry_core::graph::unified::storage::{FileRegistry, NodeEntry};

/// Symbol explanation output
#[derive(Debug, Serialize)]
struct ExplainOutput {
    /// Symbol name
    name: String,
    /// Qualified name
    qualified_name: String,
    /// Symbol kind
    kind: String,
    /// File path
    file: String,
    /// Line number
    line: u32,
    /// Language
    language: String,
    /// Visibility
    visibility: Option<String>,
    /// Documentation/comments
    documentation: Option<String>,
    /// Context (surrounding code)
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<SymbolContext>,
}

#[derive(Debug, Serialize)]
struct SymbolContext {
    /// Code snippet
    code: String,
    /// Start line of snippet
    start_line: u32,
    /// End line of snippet
    end_line: u32,
}

/// Find a symbol in the graph by file path and symbol name using the
/// shared ambiguity-aware resolver.
///
/// The resolver scope is restricted to `file_path` so the candidate set
/// reflects "what `symbol_name` could mean inside that one file." When
/// two or more candidates collide (e.g. a struct field shadowed by a
/// local variable in another function in the same file) the typed
/// [`SymbolResolveError::Ambiguous`] payload propagates up to
/// [`run_explain`], which renders the standard `sqry::ambiguous_symbol`
/// envelope.
fn resolve_symbol_by_file_and_name(
    snapshot: &GraphSnapshot,
    file_path: &std::path::Path,
    symbol_name: &str,
) -> Result<sqry_core::graph::unified::NodeId, SymbolResolveError> {
    snapshot.resolve_global_symbol_ambiguity_aware(symbol_name, FileScope::Path(file_path))
}

/// Build a `SymbolContext` by reading the source file and extracting the relevant lines.
fn build_symbol_context(
    index_root: &std::path::Path,
    files_registry: &FileRegistry,
    entry: &NodeEntry,
) -> Option<SymbolContext> {
    let file_to_read = files_registry
        .resolve(entry.file)
        .map(|p| index_root.join(p.as_ref()));

    let file_to_read = file_to_read?;
    let content = std::fs::read_to_string(&file_to_read).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let start = entry.start_line.saturating_sub(1) as usize;
    let end = (entry.end_line as usize).min(lines.len());

    if start < lines.len() {
        let code_lines: Vec<&str> = lines[start..end].to_vec();
        Some(SymbolContext {
            code: code_lines.join("\n"),
            start_line: entry.start_line,
            end_line: entry.end_line,
        })
    } else {
        None
    }
}

/// Run the explain command.
///
/// # Errors
/// Returns an error if the graph cannot be loaded or symbol cannot be found.
pub fn run_explain(
    cli: &Cli,
    file_path: &str,
    symbol_name: &str,
    path: Option<&str>,
    include_context: bool,
    _include_relations: bool,
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

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files_registry = snapshot.files();
    let requested_file_path = if std::path::Path::new(file_path).is_absolute() {
        std::path::PathBuf::from(file_path)
    } else {
        loc.index_root.join(file_path)
    };

    // Route resolution through the shared ambiguity-aware resolver. On
    // ambiguity (e.g. `NeedTags` matching a struct field + a local
    // variable inside the same file) we surface the typed
    // `sqry::ambiguous_symbol` envelope and exit with the canonical
    // ambiguous-symbol exit code (4); on absence we surface the
    // `sqry::symbol_not_found` envelope and exit with 2.
    let node_id =
        match resolve_symbol_by_file_and_name(&snapshot, &requested_file_path, symbol_name) {
            Ok(id) => id,
            Err(SymbolResolveError::Ambiguous(err)) => {
                let exit_code = emit_ambiguous_symbol_error(&mut streams, &err, cli.json);
                std::process::exit(exit_code);
            }
            Err(SymbolResolveError::NotFound { name }) => {
                let exit_code = emit_symbol_not_found(&mut streams, &name, cli.json);
                std::process::exit(exit_code);
            }
        };
    let symbol_entry = snapshot
        .get_node(node_id)
        .ok_or_else(|| anyhow!("Symbol node not found in graph"))?;

    let file_path_resolved = files_registry
        .resolve(symbol_entry.file)
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let language = files_registry
        .language_for_file(symbol_entry.file)
        .map_or_else(|| "Unknown".to_string(), |l| l.to_string());

    let name = strings
        .resolve(symbol_entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let qualified_name = symbol_entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| name.clone(), |s| s.to_string());

    let visibility = symbol_entry
        .visibility
        .and_then(|id| strings.resolve(id))
        .map(|s| s.to_string());

    let documentation = symbol_entry
        .doc
        .and_then(|id| strings.resolve(id))
        .map(|s| s.to_string());

    // Build context if requested
    let context = if include_context {
        build_symbol_context(&loc.index_root, files_registry, symbol_entry)
    } else {
        None
    };

    let output = ExplainOutput {
        name,
        qualified_name,
        kind: format!("{:?}", symbol_entry.kind),
        file: file_path_resolved,
        line: symbol_entry.start_line,
        language,
        visibility,
        documentation,
        context,
    };

    // Output
    if cli.json {
        let json = serde_json::to_string_pretty(&output).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        let text = format_explain_text(&output);
        streams.write_result(&text)?;
    }

    Ok(())
}

fn format_explain_text(output: &ExplainOutput) -> String {
    let mut lines = Vec::new();

    lines.push(format!("Symbol: {}", output.qualified_name));
    lines.push(format!("  Kind: {}", output.kind));
    lines.push(format!("  File: {}:{}", output.file, output.line));
    lines.push(format!("  Language: {}", output.language));

    if let Some(ref vis) = output.visibility {
        lines.push(format!("  Visibility: {vis}"));
    }

    if let Some(ref doc) = output.documentation {
        lines.push(format!("  Documentation: {doc}"));
    }

    if let Some(ref ctx) = output.context {
        lines.push(String::new());
        lines.push(format!("Code (lines {}-{}):", ctx.start_line, ctx.end_line));
        for (i, line) in ctx.code.lines().enumerate() {
            lines.push(format!("{:4} | {}", ctx.start_line as usize + i, line));
        }
    }

    lines.join("\n")
}
