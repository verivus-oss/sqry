//! Impact command implementation
//!
//! Provides CLI interface for analyzing what would break if a symbol changes.

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli, no_op_reporter};
use crate::index_discovery::find_nearest_index;
use crate::output::OutputStreams;
use anyhow::{Context, Result};
use serde::Serialize;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::resolution::{AmbiguousSymbolError, SymbolResolveError};
use sqry_core::graph::unified::traversal::EdgeClassification;
use sqry_core::graph::unified::{
    EdgeFilter, FileScope, TraversalConfig, TraversalDirection, TraversalLimits, traverse,
};
use std::collections::{HashMap, HashSet};

/// Stable CLI exit code surfaced when a symbol resolution is ambiguous.
///
/// Distinct from `1` (general error) and `2` (not-found / validation) so
/// scripts can branch on the ambiguity case without parsing stderr.
pub const AMBIGUOUS_SYMBOL_EXIT_CODE: i32 = 4;

/// Stable CLI exit code surfaced when a symbol cannot be located in the
/// graph.
pub const SYMBOL_NOT_FOUND_EXIT_CODE: i32 = 2;

/// Stable error code for the `sqry::ambiguous_symbol` envelope.
pub const AMBIGUOUS_SYMBOL_ERROR_CODE: &str = "sqry::ambiguous_symbol";

/// Stable error code for the `sqry::symbol_not_found` envelope.
pub const SYMBOL_NOT_FOUND_ERROR_CODE: &str = "sqry::symbol_not_found";

/// JSON envelope serialized for the `sqry::ambiguous_symbol` error.
///
/// Mirrors the shape used by the MCP boundary so a single response shape
/// flows through every wire format. Kept private because callers should
/// route through [`emit_ambiguous_symbol_error`].
#[derive(Debug, Serialize)]
struct AmbiguousSymbolEnvelope<'a> {
    code: &'static str,
    message: String,
    candidates: &'a [sqry_core::graph::unified::resolution::AmbiguousSymbolCandidate],
    truncated: bool,
}

#[derive(Debug, Serialize)]
struct AmbiguousSymbolWireWrapper<'a> {
    error: AmbiguousSymbolEnvelope<'a>,
}

/// Which disambiguation flags the command that hit an ambiguous symbol
/// actually accepts.
///
/// The ambiguity error message is shared across `sqry explain`, `sqry
/// impact`, and `sqry visualize`, but the three commands do not expose the
/// same flags. Suggesting a flag a command does not implement was the exact
/// user-facing defect in verivus-oss/sqry#512 (the error named `--in`,
/// which `explain` did not accept). This hint keeps the suggested flag
/// honest: the message only ever names a flag the invoking command actually
/// parses.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DisambiguationHint {
    /// The command accepts `--in <file>` (narrow by definition file).
    pub supports_in: bool,
    /// The command accepts `--line <N>` (narrow by definition start line).
    pub supports_line: bool,
}

impl DisambiguationHint {
    /// `--in` and `--line` both available (`explain`, `impact`, `visualize`).
    pub(crate) const fn file_and_line() -> Self {
        Self {
            supports_in: true,
            supports_line: true,
        }
    }
}

/// Emit the `sqry::ambiguous_symbol` error envelope on the active output
/// streams and return the canonical CLI exit code.
///
/// JSON output is written to stdout (the same channel as the success
/// payload) so `--json` consumers can pipe through `jq`. Human output is
/// written to stderr and lists candidates one per line, including a
/// suggested invocation built from the candidate set and the flags the
/// invoking command supports (`hint`).
pub(crate) fn emit_ambiguous_symbol_error(
    streams: &mut OutputStreams,
    err: &AmbiguousSymbolError,
    json_output: bool,
    hint: DisambiguationHint,
) -> i32 {
    let message = build_ambiguity_message(&err.name, &err.candidates, hint);
    if json_output {
        let envelope = AmbiguousSymbolWireWrapper {
            error: AmbiguousSymbolEnvelope {
                code: AMBIGUOUS_SYMBOL_ERROR_CODE,
                message,
                candidates: &err.candidates,
                truncated: err.truncated,
            },
        };
        let json = serde_json::to_string_pretty(&envelope).unwrap_or_else(|_| {
            format!(
                "{{\"error\":{{\"code\":\"{AMBIGUOUS_SYMBOL_ERROR_CODE}\",\"message\":\"{}\"}}}}",
                err.name
            )
        });
        let _ = streams.write_result(&json);
    } else {
        let mut lines = vec![format!("Error: {message}.")];
        if err.truncated {
            lines.push(format!(
                "Showing first {} candidates (more matched):",
                err.candidates.len()
            ));
        } else {
            lines.push("Candidates:".to_string());
        }
        for candidate in &err.candidates {
            lines.push(format!(
                "  - {} [{}] ({}:{}:{})",
                candidate.qualified_name,
                candidate.kind,
                candidate.file_path,
                candidate.start_line,
                candidate.start_column
            ));
        }
        let _ = streams.write_diagnostic(&lines.join("\n"));
    }
    AMBIGUOUS_SYMBOL_EXIT_CODE
}

/// Build the human-readable ambiguity error message.
///
/// The previous text ("specify the qualified name") was the user-visible
/// half of the bug in verivus-oss/sqry#214: when N candidates share the
/// same `qualified_name` (for example 11 plain-C functions named `do_exit`
/// in 11 files), no qualified name uniquely identifies any of them. The
/// actual disambiguators are the file the symbol is defined in (`--in`) and
/// the line it starts on (`--line`).
///
/// The suggestion is tailored to the candidate set and to the flags the
/// invoking command actually accepts (`hint`, see verivus-oss/sqry#512):
///
/// * `--in <file>` is only suggested when the candidates span more than one
///   file. When every candidate lives in the same file (two definitions in
///   one file, verivus-oss/sqry#512), `--in` cannot disambiguate them, so
///   it is omitted in favour of `--line`.
/// * `--line <N>` is suggested whenever the command supports it, since it
///   always distinguishes definitions at different lines.
fn build_ambiguity_message(
    name: &str,
    candidates: &[sqry_core::graph::unified::resolution::AmbiguousSymbolCandidate],
    hint: DisambiguationHint,
) -> String {
    use std::collections::BTreeSet;

    let candidate_count = candidates.len();
    let distinct_files: BTreeSet<&str> = candidates.iter().map(|c| c.file_path.as_str()).collect();
    let single_file = distinct_files.len() <= 1;
    let sample = candidates.first();

    let mut suggestions: Vec<String> = Vec::new();
    if hint.supports_in && !single_file {
        match sample {
            Some(c) => suggestions.push(format!("`--in <file>` (e.g., `--in {}`)", c.file_path)),
            None => suggestions.push("`--in <file>`".to_string()),
        }
    }
    if hint.supports_line {
        match sample {
            Some(c) => suggestions.push(format!("`--line <N>` (e.g., `--line {}`)", c.start_line)),
            None => suggestions.push("`--line <N>`".to_string()),
        }
    }

    let tail = if suggestions.is_empty() {
        "; pass a more specific qualified name to disambiguate".to_string()
    } else {
        format!("; disambiguate with {}", suggestions.join(" or "))
    };

    format!("Symbol '{name}' is ambiguous ({candidate_count} candidates){tail}")
}

/// Emit the `sqry::symbol_not_found` envelope on the active output streams
/// and return the canonical CLI exit code.
pub(crate) fn emit_symbol_not_found(
    streams: &mut OutputStreams,
    name: &str,
    json_output: bool,
) -> i32 {
    let message = format!("Symbol '{name}' not found in graph");
    if json_output {
        let envelope = serde_json::json!({
            "error": {
                "code": SYMBOL_NOT_FOUND_ERROR_CODE,
                "message": message,
            }
        });
        let json = serde_json::to_string_pretty(&envelope)
            .unwrap_or_else(|_| format!("{{\"error\":{{\"code\":\"{SYMBOL_NOT_FOUND_ERROR_CODE}\",\"message\":\"{name}\"}}}}"));
        let _ = streams.write_result(&json);
    } else {
        let _ = streams.write_diagnostic(&format!("Error: {message}."));
    }
    SYMBOL_NOT_FOUND_EXIT_CODE
}

/// Impact analysis output
#[derive(Debug, Serialize)]
struct ImpactOutput {
    /// Symbol being analyzed
    symbol: String,
    /// Direct dependents (depth 1)
    direct: Vec<ImpactSymbol>,
    /// Indirect dependents (depth > 1)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    indirect: Vec<ImpactSymbol>,
    /// Affected files
    #[serde(skip_serializing_if = "Vec::is_empty")]
    affected_files: Vec<String>,
    /// Statistics
    stats: ImpactStats,
}

#[derive(Debug, Serialize)]
struct ImpactSymbol {
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
    line: u32,
    /// How this symbol depends on the target
    relation: String,
    /// Depth from target symbol
    depth: usize,
}

#[derive(Debug, Serialize)]
struct ImpactStats {
    direct_count: usize,
    indirect_count: usize,
    total_affected: usize,
    affected_files_count: usize,
    max_depth: usize,
}

/// Result of BFS traversal collecting dependents.
struct BfsResult {
    visited: HashSet<NodeId>,
    node_depths: HashMap<NodeId, usize>,
    node_relations: HashMap<NodeId, String>,
    max_depth_reached: usize,
}

/// Perform BFS to collect all reverse dependents of a target node.
///
/// Uses the traversal kernel with incoming direction and dependency edges
/// (calls, imports, references, inheritance). Converts the kernel's
/// `TraversalResult` into the `BfsResult` expected by downstream code.
///
/// # Dispatch path (DB18)
///
/// `impact` is a **NodeId-anchored multi-hop BFS** under the Phase 3C
/// dispatch taxonomy; it does not route through sqry-db's name-keyed
/// queries. The target is resolved to a single `NodeId` in
/// [`run_impact`] via substring / qualified-name matching *before* this
/// traversal starts.
///
/// # Frontier invariant
///
/// Traversal broadens strictly through edges physically adjacent to
/// already-visited `NodeId`s (kernel `traverse` with `edges_from` in
/// the `Incoming` direction). It never re-resolves a name at depth ≥ 1,
/// preserving the same-name frontier invariant: a user who seeds on
/// `AlphaMarker::helper` cannot pull in unrelated `BetaMarker::helper`
/// dependents. The single-seed `target_node_id` lookup in
/// [`run_impact`] guarantees only one canonical anchor per invocation.
fn collect_dependents_bfs(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    target_node_id: NodeId,
    effective_max_depth: usize,
) -> BfsResult {
    let snapshot = graph.snapshot();

    let config = TraversalConfig {
        direction: TraversalDirection::Incoming,
        edge_filter: EdgeFilter::dependency_edges(),
        limits: TraversalLimits {
            max_depth: u32::try_from(effective_max_depth).unwrap_or(u32::MAX),
            max_nodes: None,
            max_edges: None,
            max_paths: None,
        },
    };

    let result = traverse(&snapshot, &[target_node_id], &config, None);

    let mut visited: HashSet<NodeId> = HashSet::new();
    let mut node_depths: HashMap<NodeId, usize> = HashMap::new();
    let mut node_relations: HashMap<NodeId, String> = HashMap::new();
    let mut actual_max_depth: usize = 0;

    for (idx, mat_node) in result.nodes.iter().enumerate() {
        // Skip the target node itself — we only want dependents
        if mat_node.node_id == target_node_id {
            continue;
        }

        visited.insert(mat_node.node_id);

        // Find the minimum depth edge leading to this node to determine its depth
        let depth = result
            .edges
            .iter()
            .filter(|e| e.source_idx == idx || e.target_idx == idx)
            .map(|e| e.depth as usize)
            .min()
            .unwrap_or(1);

        node_depths.insert(mat_node.node_id, depth);
        actual_max_depth = actual_max_depth.max(depth);

        // Determine relation type from the first edge classification reaching this node
        let relation = result
            .edges
            .iter()
            .find(|e| e.source_idx == idx || e.target_idx == idx)
            .map(|e| classify_relation(&e.classification))
            .unwrap_or_default();

        node_relations.insert(mat_node.node_id, relation);
    }

    BfsResult {
        visited,
        node_depths,
        node_relations,
        max_depth_reached: actual_max_depth,
    }
}

/// Map an `EdgeClassification` to a human-readable relation label.
#[allow(clippy::trivially_copy_pass_by_ref)] // API consistency with other command handlers
fn classify_relation(classification: &EdgeClassification) -> String {
    match classification {
        EdgeClassification::Call { .. } => "calls".to_string(),
        EdgeClassification::Import { .. } => "imports".to_string(),
        EdgeClassification::Reference => "references".to_string(),
        EdgeClassification::Inherits => "inherits".to_string(),
        EdgeClassification::Implements => "implements".to_string(),
        EdgeClassification::Export { .. } => "exports".to_string(),
        EdgeClassification::Contains => "contains".to_string(),
        EdgeClassification::Defines => "defines".to_string(),
        EdgeClassification::TypeOf => "type_of".to_string(),
        EdgeClassification::DatabaseAccess => "database_access".to_string(),
        EdgeClassification::ServiceInteraction => "service_interaction".to_string(),
    }
}

/// Categorized impact symbols after BFS traversal.
struct CategorizedImpact {
    direct: Vec<ImpactSymbol>,
    indirect: Vec<ImpactSymbol>,
    affected_files: HashSet<String>,
}

/// Build categorized impact symbols from BFS results.
fn build_impact_symbols(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    bfs: &BfsResult,
    include_indirect: bool,
    include_files: bool,
) -> CategorizedImpact {
    let strings = graph.strings();
    let files = graph.files();
    let mut direct: Vec<ImpactSymbol> = Vec::new();
    let mut indirect: Vec<ImpactSymbol> = Vec::new();
    let mut affected_files: HashSet<String> = HashSet::new();

    for &node_id in &bfs.visited {
        if let Some(entry) = graph.nodes().get(node_id) {
            let depth = *bfs.node_depths.get(&node_id).unwrap_or(&0);
            let relation = bfs
                .node_relations
                .get(&node_id)
                .cloned()
                .unwrap_or_default();

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

            let impact_sym = ImpactSymbol {
                name,
                qualified_name,
                kind: format!("{:?}", entry.kind),
                file: file_path.clone(),
                line: entry.start_line,
                relation,
                depth,
            };

            if include_files {
                affected_files.insert(file_path);
            }

            if depth == 1 {
                direct.push(impact_sym);
            } else if include_indirect {
                indirect.push(impact_sym);
            }
        }
    }

    CategorizedImpact {
        direct,
        indirect,
        affected_files,
    }
}

/// Run the impact command.
///
/// `in_file` is the optional file-path disambiguator surfaced as `--in
/// <FILE>` on the CLI; equivalent to the MCP `dependency_impact.file_path`
/// argument. When set, the resolver is restricted to candidates defined in
/// that file. `line` is the optional `--line <N>` disambiguator that narrows
/// an ambiguous match set to the definition starting on that one-based line,
/// covering the same-file / same-qualified-name case that a file alone
/// cannot (verivus-oss/sqry#512).
///
/// # Errors
/// Returns an error if the graph cannot be loaded or symbol cannot be found.
#[allow(clippy::too_many_arguments)] // CLI arg fan-out mirrors the `Impact` clap variant one-to-one.
pub fn run_impact(
    cli: &Cli,
    symbol: &str,
    path: Option<&str>,
    in_file: Option<&str>,
    line: Option<u32>,
    max_depth: usize,
    max_results: usize,
    include_indirect: bool,
    include_files: bool,
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

    // Load graph
    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&loc.index_root, &config, cli, no_op_reporter())
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    // Resolve the target symbol via the shared ambiguity-aware resolver.
    // The legacy `nodes().iter().find()` substring scan was the bug
    // surfaced in `verivus-oss/sqry#77` / `#156`: it silently picked the
    // first match (or returned "not found in graph" when nothing matched
    // by name) and gave the user no way to disambiguate. The shared
    // resolver returns a typed [`SymbolResolveError`] with the full
    // candidate list which we render through the
    // `sqry::ambiguous_symbol` envelope.
    //
    // `--in <file>` (verivus-oss/sqry#214) plumbs through as
    // `FileScope::Path`, restricting the resolver to candidates defined
    // in the named file. This is the CLI counterpart to the MCP
    // `dependency_impact.file_path` argument. `--line <N>` narrows an
    // ambiguous set to the definition on that line (verivus-oss/sqry#512).
    let snapshot = graph.snapshot();
    let in_file_path = in_file.map(std::path::PathBuf::from);
    let file_scope = in_file_path
        .as_deref()
        .map_or(FileScope::Any, FileScope::Path);
    let target_node_id =
        match snapshot.resolve_global_symbol_ambiguity_aware_with_line(symbol, file_scope, line) {
            Ok(node_id) => node_id,
            Err(SymbolResolveError::Ambiguous(err)) => {
                let exit_code = emit_ambiguous_symbol_error(
                    &mut streams,
                    &err,
                    cli.json,
                    DisambiguationHint::file_and_line(),
                );
                std::process::exit(exit_code);
            }
            Err(SymbolResolveError::NotFound { name }) => {
                if let Some(path) = in_file {
                    let _ = streams.write_diagnostic(&format!(
                        "Error: No definition of '{name}' found in file '{path}'."
                    ));
                    std::process::exit(SYMBOL_NOT_FOUND_EXIT_CODE);
                }
                let exit_code = emit_symbol_not_found(&mut streams, &name, cli.json);
                std::process::exit(exit_code);
            }
        };

    // BFS to find all dependents (reverse dependency traversal)
    let effective_max_depth = if include_indirect { max_depth } else { 1 };
    let bfs = collect_dependents_bfs(&graph, target_node_id, effective_max_depth);

    // Build categorized output
    let mut impact = build_impact_symbols(&graph, &bfs, include_indirect, include_files);

    // Sort for determinism
    impact
        .direct
        .sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));
    impact.indirect.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then(a.qualified_name.cmp(&b.qualified_name))
    });

    // Apply limit
    impact.direct.truncate(max_results);
    impact
        .indirect
        .truncate(max_results.saturating_sub(impact.direct.len()));

    let mut files_vec: Vec<String> = impact.affected_files.into_iter().collect();
    files_vec.sort();

    let stats = ImpactStats {
        direct_count: impact.direct.len(),
        indirect_count: impact.indirect.len(),
        total_affected: impact.direct.len() + impact.indirect.len(),
        affected_files_count: files_vec.len(),
        max_depth: bfs.max_depth_reached,
    };

    let output = ImpactOutput {
        symbol: symbol.to_string(),
        direct: impact.direct,
        indirect: impact.indirect,
        affected_files: if include_files { files_vec } else { Vec::new() },
        stats,
    };

    // Output
    if cli.json {
        let json = serde_json::to_string_pretty(&output).context("Failed to serialize to JSON")?;
        streams.write_result(&json)?;
    } else {
        let text = format_impact_text(&output);
        streams.write_result(&text)?;
    }

    Ok(())
}

/// Format direct dependents section for text output.
fn format_direct_dependents(lines: &mut Vec<String>, direct: &[ImpactSymbol]) {
    if !direct.is_empty() {
        lines.push("Direct dependents:".to_string());
        for sym in direct {
            lines.push(format!(
                "  {} [{}] ({} this)",
                sym.qualified_name, sym.kind, sym.relation
            ));
            lines.push(format!("    {}:{}", sym.file, sym.line));
        }
    }
}

/// Format indirect dependents section for text output.
fn format_indirect_dependents(lines: &mut Vec<String>, indirect: &[ImpactSymbol]) {
    if !indirect.is_empty() {
        lines.push(String::new());
        lines.push("Indirect dependents:".to_string());
        for sym in indirect {
            lines.push(format!(
                "  {} [{}] depth={} ({} chain)",
                sym.qualified_name, sym.kind, sym.depth, sym.relation
            ));
            lines.push(format!("    {}:{}", sym.file, sym.line));
        }
    }
}

fn format_impact_text(output: &ImpactOutput) -> String {
    let mut lines = Vec::new();

    lines.push(format!("Impact analysis for: {}", output.symbol));
    lines.push(format!(
        "Total affected: {} ({} direct, {} indirect)",
        output.stats.total_affected, output.stats.direct_count, output.stats.indirect_count
    ));
    if output.stats.affected_files_count > 0 {
        lines.push(format!(
            "Affected files: {}",
            output.stats.affected_files_count
        ));
    }
    lines.push(String::new());

    if output.direct.is_empty() && output.indirect.is_empty() {
        lines.push("No dependents found. This symbol appears to be unused.".to_string());
    } else {
        format_direct_dependents(&mut lines, &output.direct);
        format_indirect_dependents(&mut lines, &output.indirect);
    }

    if !output.affected_files.is_empty() {
        lines.push(String::new());
        lines.push("Affected files:".to_string());
        for file in &output.affected_files {
            lines.push(format!("  {file}"));
        }
    }

    lines.join("\n")
}
