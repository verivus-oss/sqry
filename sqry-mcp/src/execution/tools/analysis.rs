//! Analysis tool execution.
//!
//! This module implements analysis tools: `cross_language_edges`, `dependency_impact`,
//! `semantic_diff`.

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow};
use sqry_core::graph::unified::resolution::{AmbiguousSymbolError, SymbolResolveError};
use sqry_core::graph::unified::{FileScope, ResolutionMode, SymbolCandidateOutcome, SymbolQuery};

/// Stable error code for the `sqry::ambiguous_symbol` MCP envelope.
///
/// Mirrors the CLI [`crate::execution::tools::analysis`] code so a single
/// boundary contract flows through every wire format.
pub const MCP_AMBIGUOUS_SYMBOL_ERROR_CODE: &str = "sqry::ambiguous_symbol";

/// Build the canonical `sqry::ambiguous_symbol` JSON envelope from a
/// shared-resolver [`AmbiguousSymbolError`].
///
/// The MCP transport layer in `sqry-mcp/src/server.rs` converts every
/// `anyhow::Error` returned by an `execute_*` body into
/// `McpError::internal_error(message, None)`. To deliver a structured
/// envelope without a parallel transport-layer change, we encode the
/// envelope as the JSON-RPC error message body itself: the MCP client
/// receives `{"code": -32603, "message": "<envelope JSON>"}` and parses
/// the message as JSON to recover `code`, `message`, `candidates[]`,
/// `truncated`. This keeps the four-field shape stable across CLI and
/// MCP without leaking transport details into the resolver.
#[must_use]
pub fn ambiguous_symbol_envelope_json(err: &AmbiguousSymbolError) -> String {
    // verivus-oss/sqry#214: the previous "specify the qualified name"
    // text is unsatisfiable when N candidates share the same
    // `qualified_name` (e.g., 11 plain-C functions named `do_exit` in 11
    // files). The actual disambiguator is the file the symbol is defined
    // in — surfaced as the per-candidate `file_path` field below and
    // accepted as the `file_path` argument on the MCP tools that hit
    // this resolver.
    let sample_file = err.candidates.first().map(|c| c.file_path.as_str());
    let mut message = format!(
        "Symbol '{}' is ambiguous ({} candidates); pass the `file_path` argument \
         to disambiguate by the file the intended symbol is defined in",
        err.name,
        err.candidates.len()
    );
    if let Some(file) = sample_file {
        message.push_str(&format!(" (e.g., file_path=\"{file}\")"));
    }
    let envelope = serde_json::json!({
        "error": {
            "code": MCP_AMBIGUOUS_SYMBOL_ERROR_CODE,
            "message": message,
            "candidates": err.candidates,
            "truncated": err.truncated,
        }
    });
    serde_json::to_string(&envelope).unwrap_or_else(|_| {
        format!(
            "{{\"error\":{{\"code\":\"{}\",\"message\":\"Symbol '{}' is ambiguous\"}}}}",
            MCP_AMBIGUOUS_SYMBOL_ERROR_CODE, err.name
        )
    })
}

use crate::engine::{Engine, canonicalize_in_workspace, engine_for_workspace};
use crate::tools::{CrossLanguageEdgesArgs, DependencyImpactArgs, SemanticDiffArgs};

use crate::execution::git_worktree;
use crate::execution::graph_builders::build_graph_metadata;
use crate::execution::location::node_location_for_reporting;
use crate::execution::types::{
    CrossLanguageEdgesData, DependencyImpactData, FindUnusedData, ImpactedSymbol, NodeChange,
    NodeRefData, PositionData, RangeData, RelationEdgeData, SemanticDiffData, ToolExecution,
    UnusedSymbolData,
};
use crate::execution::utils::{duration_to_ms, paginate};

fn resolve_workspace_path(path: &str) -> Option<std::path::PathBuf> {
    if path == "." {
        None
    } else {
        Some(std::path::PathBuf::from(path))
    }
}

/// Resolve a symbol to a single `NodeId` using strict resolution.
///
/// # Parameters
///
/// * `snapshot` — graph snapshot to query
/// * `symbol` — raw symbol text (qualified or simple name)
/// * `file_path` — optional pre-canonicalized file path to restrict the search.
///   When provided the resolver uses `FileScope::Path` so only candidates in
///   that file are considered. Path canonicalization and workspace validation
///   are the caller's responsibility (owned by DISAMBIG_2's validation layer).
///
/// # Errors
///
/// * Symbol not found → `"Symbol 'X' not found in graph."`
/// * When `file_path` is given and matches zero candidates →
///   `"No definition of 'X' found in file 'Y'"`
/// * When `file_path` is given and matches multiple candidates in the same
///   file → lists those candidates and suggests a qualified name
/// * Symbol is ambiguous without `file_path` → error includes up to 3 sample
///   candidates with `file:line` and instructs use of `file_path`
fn resolve_global_symbol_strict(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol: &str,
    file_path: Option<&Path>,
) -> Result<sqry_core::graph::unified::node::NodeId> {
    let file_scope = match file_path {
        Some(path) => FileScope::Path(path),
        None => FileScope::Any,
    };

    // Route through the shared ambiguity-aware resolver
    // (`resolve_global_symbol_ambiguity_aware`) so every CLI / MCP / LSP
    // surface that resolves a user-supplied symbol name uses the same
    // implementation. The previous bespoke wrapper (legacy text-only
    // ambiguity messages) drifted from the CLI surface and did not carry
    // the typed candidate metadata MCP clients now consume.
    match snapshot.resolve_global_symbol_ambiguity_aware(symbol, file_scope) {
        Ok(node_id) => Ok(node_id),
        Err(SymbolResolveError::NotFound { .. }) => {
            if let Some(path) = file_path {
                Err(anyhow!(
                    "No definition of '{}' found in file '{}'.",
                    symbol,
                    path.display()
                ))
            } else {
                Err(anyhow!("Symbol '{symbol}' not found in graph."))
            }
        }
        Err(SymbolResolveError::Ambiguous(err)) => {
            // Encode the canonical `sqry::ambiguous_symbol` envelope as
            // the `anyhow::Error` message. The MCP transport layer in
            // `sqry-mcp/src/server.rs` lifts this verbatim into the
            // JSON-RPC `error.message` field; clients parse the message
            // as JSON to recover the structured envelope.
            Err(anyhow!("{}", ambiguous_symbol_envelope_json(&err)))
        }
    }
}

fn candidate_bucket_for_symbol(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol: &str,
) -> Vec<sqry_core::graph::unified::node::NodeId> {
    match snapshot.find_symbol_candidates(&SymbolQuery {
        symbol,
        file_scope: FileScope::Any,
        mode: ResolutionMode::AllowSuffixCandidates,
    }) {
        SymbolCandidateOutcome::Candidates(candidates) => candidates,
        SymbolCandidateOutcome::NotFound | SymbolCandidateOutcome::FileNotIndexed => Vec::new(),
    }
}

fn cycle_type_label(cycle_type: CycleType) -> &'static str {
    match cycle_type {
        CycleType::Calls => "calls",
        CycleType::Imports => "imports",
        CycleType::Modules => "modules",
    }
}

/// Map MCP [`CycleType`] to the [`CircularType`] that sqry-db's
/// [`sqry_db::queries::CyclesQuery`] and [`sqry_db::queries::IsInCycleQuery`]
/// consume. `Modules` collapses to `Imports` so module-level cycles are
/// detected on the `Imports` edge kind — that mirrors the pre-DB17
/// `cycle_edge_kind_name`/`cycle_edge_kind` pair and is asserted by
/// `sqry_db::queries::cycles::edge_probe_for`.
fn mcp_cycle_type_to_core(cycle_type: CycleType) -> CircularType {
    match cycle_type {
        CycleType::Calls => CircularType::Calls,
        CycleType::Imports => CircularType::Imports,
        CycleType::Modules => CircularType::Modules,
    }
}

/// Build the sqry-db [`CycleBounds`] that corresponds to a MCP
/// `FindCyclesArgs` / `IsNodeInCycleArgs` request. `max_results` is the
/// pool cap — sqry-db truncates the [`CyclesQuery`] result to this many
/// cycles; the MCP handler still applies its own `max_results` cap on
/// top via the `CycleBounds::max_results` field so that both handlers
/// stay aligned with the pre-DB17 `CircularConfig::max_results`
/// semantic.
fn cycle_bounds_for(
    min_depth: usize,
    max_depth: Option<usize>,
    max_results: usize,
    include_self_loops: bool,
) -> sqry_db::queries::CycleBounds {
    sqry_db::queries::CycleBounds {
        min_depth,
        max_depth,
        max_results,
        should_include_self_loops: include_self_loops,
    }
}

/// Materialize a cache-hit `CyclesQuery` payload (`Arc<Vec<Vec<NodeId>>>`)
/// into the name-shaped `Vec<Vec<String>>` that [`convert_cycles_to_output`]
/// consumes. Qualified names are preferred; a node without a qualified
/// name falls back to its simple name. Nodes whose entries cannot be
/// resolved (stale `NodeId`s post-tombstone) are skipped silently — they
/// are never in a live cycle because sqry-db's Tarjan walk only visits
/// arena-live nodes per [`sqry_db::queries::SccQuery`].
fn materialize_cycle_node_ids(
    cycles: &[Vec<sqry_core::graph::unified::node::NodeId>],
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> Vec<Vec<String>> {
    let strings = snapshot.strings();
    cycles
        .iter()
        .map(|cycle| {
            cycle
                .iter()
                .filter_map(|&node_id| {
                    snapshot.get_node(node_id).and_then(|entry| {
                        entry
                            .qualified_name
                            .and_then(|sid| strings.resolve(sid))
                            .or_else(|| strings.resolve(entry.name))
                            .map(|s| s.to_string())
                    })
                })
                .collect()
        })
        .filter(|cycle: &Vec<String>| !cycle.is_empty())
        .collect()
}

/// Check whether a node should be included in unused results based on filters.
///
/// Returns `true` if the node passes all scope, language, and kind filters.
fn should_include_in_unused_results(
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    args: &FindUnusedArgs,
    strings: &sqry_core::graph::unified::storage::StringInterner,
    files: &sqry_core::graph::unified::storage::registry::FileRegistry,
) -> bool {
    // Apply scope filter
    let visibility_str = entry
        .visibility
        .and_then(|vid| strings.resolve(vid))
        .map(|s| s.to_string());
    if !matches_scope_filter(entry.kind, visibility_str.as_deref(), args.scope) {
        return false;
    }

    // Apply language filter
    if !args.languages.is_empty() {
        if let Some(lang) = files.language_for_file(entry.file) {
            let lang_str = lang.to_string();
            if !args
                .languages
                .iter()
                .any(|l| l.eq_ignore_ascii_case(&lang_str))
            {
                return false;
            }
        } else {
            return false;
        }
    }

    // Apply kind filter
    if !args.kinds.is_empty() {
        let kind_str = format!("{:?}", entry.kind).to_lowercase();
        if !args.kinds.iter().any(|k| k.eq_ignore_ascii_case(&kind_str)) {
            return false;
        }
    }

    true
}

/// Build an `UnusedSymbolData` from a graph node entry.
fn build_unused_symbol_data(
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    node_id: sqry_core::graph::unified::node::NodeId,
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    strings: &sqry_core::graph::unified::storage::StringInterner,
    files: &sqry_core::graph::unified::storage::registry::FileRegistry,
    workspace_root: &std::path::Path,
) -> UnusedSymbolData {
    let name = strings
        .resolve(entry.name)
        .map_or_else(String::new, |s| s.to_string());
    let language = files.language_for_file(entry.file);
    let qualified_name =
        crate::execution::symbol_utils::display_entry_qualified_name(entry, strings, files, &name);

    let file_path = files.resolve(entry.file);
    let full_path = file_path
        .as_ref()
        .map(|p| workspace_root.join(p.as_ref()))
        .unwrap_or_default();
    let file_uri = url::Url::from_file_path(&full_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&full_path),
        Into::into,
    );

    let language = language.map_or("unknown".to_string(), |l| l.to_string());

    let kind = format!("{:?}", entry.kind).to_lowercase();
    let visibility = format!("{:?}", entry.visibility).to_lowercase();

    let loc = node_location_for_reporting(graph, node_id, workspace_root);

    UnusedSymbolData {
        name,
        qualified_name,
        kind,
        file_uri,
        line: loc.as_ref().map_or(entry.start_line, |l| l.line),
        language,
        visibility,
    }
}

/// Check if a symbol matches the scope filter
fn matches_scope_filter(
    kind: sqry_core::graph::unified::node::NodeKind,
    visibility_str: Option<&str>,
    scope: UnusedScope,
) -> bool {
    use sqry_core::graph::unified::node::NodeKind;

    match scope {
        UnusedScope::Public => visibility_str.is_some_and(|v| v.eq_ignore_ascii_case("public")),
        UnusedScope::Private => visibility_str.is_some_and(|v| v.eq_ignore_ascii_case("private")),
        UnusedScope::Function => matches!(kind, NodeKind::Function | NodeKind::Method),
        UnusedScope::Struct => matches!(
            kind,
            NodeKind::Struct | NodeKind::Class | NodeKind::Interface | NodeKind::Trait
        ),
        UnusedScope::All => true,
    }
}

// Language-specific entry point detection

/// Execute the `cross_language_edges` tool to find cross-language dependencies.
///
/// Uses the unified graph (`GraphSnapshot`) to find edges where source and target
/// nodes belong to different programming languages.
pub fn execute_cross_language_edges(
    args: &CrossLanguageEdgesArgs,
) -> Result<ToolExecution<CrossLanguageEdgesData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _base = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph for cross-language edge detection
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();
    let files = snapshot.files();
    let strings = snapshot.strings();

    let mut edges: Vec<RelationEdgeData> = Vec::new();

    // Iterate over all edges and find cross-language ones
    for (source_id, target_id, edge_kind) in snapshot.iter_edges() {
        let Some(source_node) = snapshot.get_node(source_id) else {
            continue;
        };
        let Some(target_node) = snapshot.get_node(target_id) else {
            continue;
        };

        // Get languages from file registry
        let source_lang = files.language_for_file(source_node.file);
        let target_lang = files.language_for_file(target_node.file);

        // Skip if same language or unknown languages
        let (Some(from_lang), Some(to_lang)) = (source_lang, target_lang) else {
            continue;
        };
        if from_lang == to_lang {
            continue;
        }

        // Apply language filters
        if let Some(ref fl) = args.from_lang
            && !from_lang.to_string().eq_ignore_ascii_case(fl)
        {
            continue;
        }
        if let Some(ref tl) = args.to_lang
            && !to_lang.to_string().eq_ignore_ascii_case(tl)
        {
            continue;
        }

        // Build NodeRefData for source node
        let from_ref = build_node_ref_from_node(
            source_node,
            source_id,
            &graph,
            from_lang,
            files,
            strings,
            &workspace_root,
        );

        // Build NodeRefData for target node
        let to_ref = build_node_ref_from_node(
            target_node,
            target_id,
            &graph,
            to_lang,
            files,
            strings,
            &workspace_root,
        );

        edges.push(RelationEdgeData {
            from: Some(from_ref),
            to: Some(to_ref),
            relation_type: format!("{edge_kind:?}").to_lowercase(),
            depth: 1,
            metadata: None,
        });

        if edges.len() >= args.max_results {
            break;
        }
    }

    let total = edges.len();
    let (page_slice, next_page_token) = paginate(&edges, &args.pagination);
    let page_edges = page_slice.to_vec();

    let graph_metadata = build_graph_metadata(Some(&workspace_root), Some(&snapshot), None);

    Ok(ToolExecution {
        data: CrossLanguageEdgesData {
            edges: page_edges,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: Some(graph_metadata),
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(total > args.max_results),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Builds a `NodeRefData` from a unified graph `NodeEntry`.
fn build_node_ref_from_node(
    node: &sqry_core::graph::unified::storage::arena::NodeEntry,
    node_id: sqry_core::graph::unified::node::NodeId,
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    language: sqry_core::graph::Language,
    files: &sqry_core::graph::unified::storage::FileRegistry,
    strings: &sqry_core::graph::unified::storage::StringInterner,
    workspace_root: &std::path::Path,
) -> NodeRefData {
    use sqry_core::graph::unified::node::NodeKind;

    // Get symbol name from string interner
    let name = strings
        .resolve(node.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    // Get qualified name if present
    let qualified_name =
        crate::execution::symbol_utils::display_entry_qualified_name(node, strings, files, &name);

    // Map NodeKind to kind string
    let kind = match node.kind {
        NodeKind::Class => "class",
        NodeKind::Module => "module",
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Interface => "interface",
        NodeKind::Trait => "trait",
        NodeKind::Method => "method",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::Type => "type",
        _ => "function",
    };

    // Get file path using resolve() which returns Option<Arc<Path>>
    let file_path = files
        .resolve(node.file)
        .map(|arc_path| workspace_root.join(arc_path.as_ref()))
        .unwrap_or_default();

    let file_uri = url::Url::from_file_path(&file_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&file_path),
        Into::into,
    );

    let loc = node_location_for_reporting(graph, node_id, workspace_root);
    let resolution_source = loc.as_ref().map(|l| format!("{:?}", l.resolution_source));

    NodeRefData {
        name,
        qualified_name,
        kind: kind.to_string(),
        language: language.to_string(),
        file_uri,
        range: RangeData {
            start: PositionData {
                line: loc.as_ref().map_or(node.start_line, |l| l.line),
                character: loc.as_ref().map_or(node.start_column, |l| l.column),
            },
            end: PositionData {
                line: loc.as_ref().map_or(node.end_line, |l| l.end_line),
                character: loc.as_ref().map_or(node.end_column, |l| l.end_column),
            },
        },
        metadata: None,
        resolution_source,
    }
}

/// Build a `NodeRefData` for an impact analysis result from a graph node entry.
fn build_impact_node_ref(
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    node_id: sqry_core::graph::unified::node::NodeId,
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    strings: &sqry_core::graph::unified::storage::StringInterner,
    files: &sqry_core::graph::unified::storage::registry::FileRegistry,
    workspace_root: &std::path::Path,
) -> NodeRefData {
    let name = strings
        .resolve(entry.name)
        .map_or_else(String::new, |s| s.to_string());
    let qualified_name =
        crate::execution::symbol_utils::display_entry_qualified_name(entry, strings, files, &name);

    let file_path = files.resolve(entry.file);
    let full_path = file_path
        .as_ref()
        .map(|p| workspace_root.join(p.as_ref()))
        .unwrap_or_default();
    let file_uri = url::Url::from_file_path(&full_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&full_path),
        Into::into,
    );

    let language = files
        .language_for_file(entry.file)
        .map_or("unknown".to_string(), |l| l.to_string());

    let kind = format!("{:?}", entry.kind).to_lowercase();

    let loc = node_location_for_reporting(graph, node_id, workspace_root);
    let resolution_source = loc.as_ref().map(|l| format!("{:?}", l.resolution_source));

    NodeRefData {
        name,
        qualified_name,
        kind,
        language,
        file_uri,
        range: RangeData {
            start: PositionData {
                line: loc.as_ref().map_or(entry.start_line, |l| l.line),
                character: loc.as_ref().map_or(entry.start_column, |l| l.column),
            },
            end: PositionData {
                line: loc.as_ref().map_or(entry.end_line, |l| l.end_line),
                character: loc.as_ref().map_or(entry.end_column, |l| l.end_column),
            },
        },
        metadata: None,
        resolution_source,
    }
}

/// Process a caller node and return impacted symbol data and file URI.
fn process_caller_node(
    entry: &sqry_core::graph::unified::NodeEntry,
    node_id: sqry_core::graph::unified::node::NodeId,
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    strings: &sqry_core::graph::unified::storage::StringInterner,
    files: &sqry_core::graph::unified::storage::registry::FileRegistry,
    workspace_root: &std::path::Path,
    depth: usize,
    _args: &DependencyImpactArgs,
) -> (ImpactedSymbol, String) {
    let node_ref = build_impact_node_ref(entry, node_id, graph, strings, files, workspace_root);
    let file_uri = node_ref.file_uri.clone();

    let symbol = ImpactedSymbol {
        symbol: node_ref,
        depth: u32::try_from(depth + 1).unwrap_or(u32::MAX),
        impact_type: "caller".to_string(),
    };

    (symbol, file_uri)
}

/// BFS traversal to collect all impacted callers of a target symbol.
///
/// Returns the list of impacted symbols and set of affected file URIs.
///
/// # Frontier invariant (DB16)
///
/// The BFS is strictly NodeId-anchored. The seed is a single
/// `target_node_id` (the caller resolves via
/// [`resolve_global_symbol_strict`] which errors on ambiguity, so the seed
/// set is always exactly one node). The frontier is expanded only via
/// `snapshot.get_callers(current_id)` — a direct CSR lookup for incoming
/// `Calls` edges to `current_id`. There is no name-keyed dispatch anywhere
/// in the loop, so the multi-hop frontier-broadening bug DB15's followup
/// fixed for `relation_query` cannot manifest here: an ambiguous simple
/// name never reaches the frontier because the resolver rejects it before
/// BFS starts.
fn collect_impacted_callers_bfs(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    target_node_id: sqry_core::graph::unified::node::NodeId,
    args: &DependencyImpactArgs,
    workspace_root: &std::path::Path,
) -> (Vec<ImpactedSymbol>, HashSet<String>) {
    let strings = snapshot.strings();
    let files = snapshot.files();

    let mut impacted: Vec<ImpactedSymbol> = Vec::new();
    let mut affected_files: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(sqry_core::graph::unified::node::NodeId, usize)> = VecDeque::new();
    let mut visited: HashSet<sqry_core::graph::unified::node::NodeId> = HashSet::new();

    queue.push_back((target_node_id, 0));
    visited.insert(target_node_id);

    while let Some((current_id, depth)) = queue.pop_front() {
        if depth >= args.max_depth {
            continue;
        }

        let callers = snapshot.get_callers(current_id);
        for caller_id in callers {
            if visited.contains(&caller_id) {
                continue;
            }

            let Some(entry) = snapshot.get_node(caller_id) else {
                continue;
            };
            let (symbol, file_uri) = process_caller_node(
                entry,
                caller_id,
                graph,
                strings,
                files,
                workspace_root,
                depth,
                args,
            );

            if args.include_files {
                affected_files.insert(file_uri);
            }

            impacted.push(symbol);

            if args.include_indirect {
                visited.insert(caller_id);
                queue.push_back((caller_id, depth + 1));
            }
        }
    }

    (impacted, affected_files)
}

/// Execute the `dependency_impact` tool to analyze impact of changes.
///
/// # Dispatch path (DB16)
///
/// `dependency_impact` is **NodeId-anchored**: the user supplies a symbol
/// name, it's resolved via [`resolve_global_symbol_strict`] (ambiguity is
/// rejected with a canonical-name hint), and from that point on the BFS
/// walks only via `snapshot.get_callers(current_id)`. This handler does
/// NOT route through sqry-db at depth-1; the name-to-NodeId resolution is
/// the only name-keyed operation, and `resolve_global_symbol_strict`
/// already owns it with the strict segment matcher.
///
/// This mirrors the DB15 followup decision for `relation_query`: once the
/// seed is a concrete NodeId, BFS via direct CSR edge lookups is the right
/// primitive, and sqry-db's name-keyed predicate queries (which could
/// reintroduce a segment-matcher mismatch at depth 1) are not. See
/// [`crate::execution::relation_dispatch`] module docs for the taxonomy.
///
/// # Behavior
///
/// Ambiguous simple names return a
/// "Use a canonical qualified name" error via
/// [`resolve_global_symbol_strict`]. This is stricter than the DB15
/// behavior shift for `direct_callers` / `direct_callees` (which now
/// return the union across ambiguous matches) — `dependency_impact` must
/// stay single-node anchored because the returned impact depths are
/// meaningless across a union of unrelated chains.
pub fn execute_dependency_impact(
    args: &DependencyImpactArgs,
) -> Result<ToolExecution<DependencyImpactData>> {
    // Pre-refactor timing: `start` fires before engine resolution.
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    tracing::debug!(
        symbol = %args.symbol,
        max_depth = args.max_depth,
        include_files = args.include_files,
        include_indirect = args.include_indirect,
        "Executing dependency_impact tool"
    );

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let ctx = crate::daemon_adapter::WorkspaceContext {
        workspace_root,
        graph,
        executor: engine.executor_arc(),
    };
    inner::execute_dependency_impact(&ctx, args, start)
}

/// Execute the `semantic_diff` tool to compare code between git refs.
///
/// # DB20 migration
///
/// Phase 1 (worktree creation) and Phase 2 (graph build) still happen here —
/// `semantic_diff` is the only MCP tool that needs two independent graphs
/// built from git worktrees, so that orchestration stays in the handler.
///
/// Phase 3 (the actual diff computation) routes through
/// [`sqry_db::ComparativeQueryDb::diff`] (DB20, Option A). The comparative
/// DB is the only handler that does NOT go through `make_query_db` — it
/// deliberately bypasses the `ShardedCache` because cross-snapshot results
/// have no meaningful invalidation criterion. See
/// `docs/superpowers/specs/2026-04-12-derived-analysis-db-query-planner-design.md`
/// (M6) for the rationale.
///
/// Phases 4–7 (filter / summary / pagination) remain in the MCP handler so
/// the wire DTO (`SemanticDiffData` + `NodeChange` camelCase shape + `fileUri`
/// URLs + `resolution_source`) stays stable at the network boundary.
pub fn execute_semantic_diff(args: &SemanticDiffArgs) -> Result<ToolExecution<SemanticDiffData>> {
    // Pre-refactor timing: `start` fires before engine resolution.
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    // NOTE: `semantic_diff` builds its own per-worktree graphs in the inner
    // body and does not consume `ctx.graph` or `ctx.executor`. We still call
    // `ensure_graph` here so every public `execute_*` emits the same
    // `WorkspaceContext` shape required by the daemon path (Task 4).
    // `ctx.graph` therefore reflects the workspace's CURRENT index, not either
    // of the git refs being diffed.
    let graph = engine.ensure_graph()?;
    let ctx = crate::daemon_adapter::WorkspaceContext {
        workspace_root,
        graph,
        executor: engine.executor_arc(),
    };
    inner::execute_semantic_diff(&ctx, args, start)
}

/// Convert a sqry-db [`sqry_db::NodeChange`] into the MCP wire-format
/// [`crate::execution::types::NodeChange`], translating worktree paths back
/// to workspace paths and building `file://` URIs.
///
/// This preserves the exact pre-DB20 wire shape: `change_type` is a
/// lowercase string ("added" / "removed" / "modified" / "renamed" /
/// "signature_changed"), `baseLocation` / `targetLocation` are
/// [`NodeRefData`] structs with `fileUri` URLs, and empty-optional fields
/// are omitted via `serde(skip_serializing_if = "Option::is_none")`.
fn convert_db_change_to_wire(
    db_change: sqry_db::NodeChange,
    workspace_root: &std::path::Path,
    base_worktree: &std::path::Path,
    target_worktree: &std::path::Path,
) -> Result<NodeChange> {
    let sqry_db::NodeChange {
        symbol_name,
        qualified_name,
        kind,
        change_type,
        base_location,
        target_location,
        signature_before,
        signature_after,
    } = db_change;

    let base_wire = base_location
        .map(|loc| {
            db_location_to_ref(
                &loc,
                workspace_root,
                base_worktree,
                &symbol_name,
                &qualified_name,
                &kind,
            )
        })
        .transpose()?;
    let target_wire = target_location
        .map(|loc| {
            db_location_to_ref(
                &loc,
                workspace_root,
                target_worktree,
                &symbol_name,
                &qualified_name,
                &kind,
            )
        })
        .transpose()?;

    Ok(NodeChange {
        symbol_name,
        qualified_name,
        kind,
        change_type: change_type.as_str().to_string(),
        base_location: base_wire,
        target_location: target_wire,
        signature_before,
        signature_after,
    })
}

/// Convert a sqry-db [`sqry_db::NodeLocation`] into the MCP wire-format
/// [`NodeRefData`]. Worktree paths are translated back to the real
/// workspace root; the resulting path is rendered as a `file://` URI.
fn db_location_to_ref(
    loc: &sqry_db::NodeLocation,
    workspace_root: &std::path::Path,
    worktree_root: &std::path::Path,
    symbol_name: &str,
    qualified_name: &str,
    kind: &str,
) -> Result<NodeRefData> {
    let real_path = if let Ok(relative) = loc.file_path.strip_prefix(worktree_root) {
        workspace_root.join(relative)
    } else {
        tracing::trace!(
            path = %loc.file_path.display(),
            "Worktree path did not match expected root; using as-is"
        );
        loc.file_path.clone()
    };

    let file_uri = url::Url::from_file_path(&real_path)
        .map_err(|()| anyhow!("Invalid file path: {}", real_path.display()))?
        .to_string();

    Ok(NodeRefData {
        name: symbol_name.to_string(),
        qualified_name: qualified_name.to_string(),
        kind: kind.to_string(),
        language: loc.language.clone(),
        file_uri,
        range: RangeData {
            start: PositionData {
                line: loc.start_line.saturating_sub(1),
                character: 0,
            },
            end: PositionData {
                line: loc.end_line.saturating_sub(1),
                character: 0,
            },
        },
        metadata: None,
        resolution_source: None,
    })
}

/// Summarise wire-format changes by their string `change_type`. Matches the
/// pre-DB20 `diff_comparator::compute_summary` semantics exactly.
fn summarise_wire_changes(changes: &[NodeChange]) -> crate::execution::types::DiffSummary {
    let mut summary = crate::execution::types::DiffSummary {
        added: 0,
        removed: 0,
        modified: 0,
        renamed: 0,
        signature_changed: 0,
        unchanged: 0,
    };
    for change in changes {
        match change.change_type.as_str() {
            "added" => summary.added += 1,
            "removed" => summary.removed += 1,
            "modified" => summary.modified += 1,
            "renamed" => summary.renamed += 1,
            "signature_changed" => summary.signature_changed += 1,
            _ => summary.unchanged += 1,
        }
    }
    summary
}

// ============================================================================
// Find Duplicates Tool
// ============================================================================

use crate::execution::types::{
    CycleData, CycleNodeData, DuplicateGroupData, DuplicateSymbolData, FindCyclesData,
    FindDuplicatesData,
};
use crate::tools::{
    CycleType, DuplicateType, FindCyclesArgs, FindDuplicatesArgs, FindUnusedArgs, UnusedScope,
};
use sqry_core::query::DuplicateType as CoreDuplicateType;
use sqry_core::query::{CircularType, DuplicateConfig, build_duplicate_groups_graph};

/// Convert raw duplicate groups from the core library into output-format `DuplicateGroupData`.
///
/// Performs node lookup, name resolution, file path construction, and URL building
/// for each node in each group.
fn convert_duplicate_groups(
    groups: Vec<sqry_core::query::DuplicateGroup>,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    workspace_root: &std::path::Path,
) -> Vec<DuplicateGroupData> {
    let strings = snapshot.strings();
    let files = snapshot.files();

    groups
        .into_iter()
        .filter(|g| g.total_members > 1)
        .map(|group| {
            let symbols: Vec<DuplicateSymbolData> = group
                .node_ids
                .iter()
                .filter_map(|&node_id| {
                    let entry = snapshot.get_node(node_id)?;

                    let name = strings
                        .resolve(entry.name)
                        .map(|s| s.to_string())
                        .unwrap_or_default();

                    let language = files.language_for_file(entry.file);
                    let qualified_name =
                        crate::execution::symbol_utils::display_entry_qualified_name(
                            entry, strings, files, &name,
                        );

                    let file_path = files.resolve(entry.file)?;
                    let full_path = workspace_root.join(file_path.as_ref());
                    let file_uri = url::Url::from_file_path(&full_path).ok().map_or_else(
                        || crate::execution::symbol_utils::path_to_forward_slash(&full_path),
                        Into::into,
                    );

                    let language = language.map_or("unknown".to_string(), |l| l.to_string());

                    let loc = node_location_for_reporting(graph, node_id, workspace_root);

                    Some(DuplicateSymbolData {
                        name,
                        qualified_name,
                        kind: format!("{:?}", entry.kind).to_lowercase(),
                        file_uri,
                        line: loc.as_ref().map_or(entry.start_line, |l| l.line),
                        language,
                    })
                })
                .collect();

            // Format group_id as hex string:
            // - For body duplicates with 128-bit hash: 32-char hex
            // - For others: 16-char hex from u64
            let group_id = if let Some(body_hash) = group.body_hash_128 {
                format!("{body_hash}") // BodyHash128::Display is 32-char hex
            } else {
                format!("{:016x}", group.hash)
            };

            DuplicateGroupData {
                group_id,
                count: symbols.len(),
                total_members: group.total_members,
                members_truncated: group.members_truncated,
                symbols,
            }
        })
        .filter(|g| g.total_members > 1)
        .collect()
}

/// Execute the `find_duplicates` tool to find duplicate code patterns.
///
/// Uses `CodeGraph` with `body_hash` for body duplicate detection and signature/struct
/// hashing for other duplicate types.
pub fn execute_find_duplicates(
    args: &FindDuplicatesArgs,
) -> Result<ToolExecution<FindDuplicatesData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    // Convert MCP DuplicateType to core DuplicateType
    let (core_dup_type, dup_type_str) = match args.duplicate_type {
        DuplicateType::Body => (CoreDuplicateType::Body, "body"),
        DuplicateType::Signature => (CoreDuplicateType::Signature, "signature"),
        DuplicateType::Struct => (CoreDuplicateType::Struct, "struct"),
    };

    // Build duplicate detection config
    let config = DuplicateConfig {
        threshold: if args.exact {
            1.0
        } else {
            f64::from(args.threshold) / 100.0
        },
        max_results: args.max_results,
        is_exact_only: args.exact || args.threshold >= 100,
        max_members_per_group: args.max_members_per_group,
    };

    // Find duplicates using CodeGraph
    let groups = build_duplicate_groups_graph(core_dup_type, &graph, &config);

    let snapshot = graph.snapshot();

    // Convert to output format
    let mut output_groups = convert_duplicate_groups(groups, &snapshot, &graph, &workspace_root);

    // Sort by total_members (largest first, using pre-truncation count so that
    // large groups with the same displayed count are ranked correctly), secondary
    // by group_id for stability.
    output_groups.sort_by(|a, b| {
        b.total_members
            .cmp(&a.total_members)
            .then_with(|| a.group_id.cmp(&b.group_id))
    });

    let total = output_groups.len();
    let truncated = total > args.max_results;
    output_groups.truncate(args.max_results);

    let (page_slice, next_page_token) = paginate(&output_groups, &args.pagination);
    let page_groups = page_slice.to_vec();

    Ok(ToolExecution {
        data: FindDuplicatesData {
            duplicate_type: dup_type_str.to_string(),
            threshold: args.threshold,
            groups: page_groups,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(truncated),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

// ============================================================================
// Find Cycles Tool
// ============================================================================

/// Resolve a single cycle symbol name to a `CycleNodeData`, looking up node info from the graph.
fn resolve_cycle_node(
    name: &str,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    workspace_root: &std::path::Path,
) -> CycleNodeData {
    let strings = snapshot.strings();
    let files = snapshot.files();

    let node_ids = candidate_bucket_for_symbol(snapshot, name);
    if let Some(&node_id) = node_ids.first()
        && let Some(entry) = snapshot.get_node(node_id)
    {
        let node_name = strings
            .resolve(entry.name)
            .map_or_else(|| name.to_string(), |s| s.to_string());
        let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
            entry, strings, files, &node_name,
        );
        let file_path = files
            .resolve(entry.file)
            .map(|p| workspace_root.join(p.as_ref()))
            .unwrap_or_default();
        let file_uri = url::Url::from_file_path(&file_path).ok().map_or_else(
            || crate::execution::symbol_utils::path_to_forward_slash(&file_path),
            Into::into,
        );

        let loc = node_location_for_reporting(graph, node_id, workspace_root);

        return CycleNodeData {
            name: node_name,
            qualified_name,
            file_uri,
            line: loc.as_ref().map_or(entry.start_line, |l| l.line),
        };
    }

    // Node not found, use name as-is
    CycleNodeData {
        name: name.to_string(),
        qualified_name: name.to_string(),
        file_uri: String::new(),
        line: 0,
    }
}

/// Convert raw cycle data (lists of symbol name strings) into output-format `CycleData` structs.
fn convert_cycles_to_output(
    cycles: Vec<Vec<String>>,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    workspace_root: &std::path::Path,
) -> Vec<CycleData> {
    cycles
        .into_iter()
        .map(|cycle| {
            let nodes: Vec<CycleNodeData> = cycle
                .iter()
                .map(|name| resolve_cycle_node(name, snapshot, graph, workspace_root))
                .collect();

            // Build chain string: A -> B -> C -> A
            let chain = if cycle.is_empty() {
                String::new()
            } else {
                let mut parts = cycle.clone();
                parts.push(cycle[0].clone()); // Close the cycle
                parts.join(" \u{2192} ")
            };

            CycleData {
                depth: nodes.len(),
                nodes,
                chain,
            }
        })
        .collect()
}

/// Execute the `find_cycles` tool to find circular dependencies.
///
/// # Dispatch path (DB17)
///
/// `find_cycles` is a **name-keyed predicate** under the Phase 3C dispatch
/// taxonomy (`ART:mcp-dispatch-taxonomy`): "enumerate every non-trivial SCC
/// in the graph's `Calls`/`Imports` edge-kind projection whose size falls
/// inside `[min_depth, max_depth]`". That is the planner contract sqry-db's
/// [`sqry_db::queries::CyclesQuery`] caches, keyed on
/// [`sqry_db::queries::CyclesKey`] (`circular_type` + `bounds`).
///
/// The handler:
/// 1. Acquires a per-call [`sqry_db::QueryDb`] via
///    [`crate::execution::relation_dispatch::make_query_db`] (see that
///    module's docs for the Phase 3 dispatch taxonomy).
/// 2. Dispatches [`sqry_db::queries::CyclesQuery`] with the user's
///    `circular_type` and a [`sqry_db::queries::CycleBounds`] copy of the
///    MCP filter knobs. sqry-db runs one cached Tarjan SCC pass per edge
///    kind (via [`sqry_db::queries::SccQuery`]) and applies the bounds on
///    top. Second calls on the same snapshot are O(1) cache hits.
/// 3. Materializes the returned
///    [`sqry_core::graph::unified::node::NodeId`] vectors into the
///    qualified-name-shaped [`Vec<Vec<String>>`] that
///    [`convert_cycles_to_output`] consumes via [`materialize_cycle_node_ids`],
///    then builds per-cycle [`CycleData`] rows for the MCP payload.
///
/// # Behavior shift
///
/// The pre-DB17 implementation tried to load a pre-persisted `SccData`
/// blob from disk (`try_load_scc`) and only fell back to on-demand Tarjan
/// when the blob was missing. That fast-path is retired: sqry-db computes
/// and caches on demand in-memory, so no disk round-trip is needed and the
/// "CSR build"/"SCC computation" failure-warning branch
/// (`handle_cycle_analysis_failure`) that printed "Run `sqry index --force`
/// or `sqry analyze`" is gone. The result set is still bounded by
/// `args.max_results` plus `min_depth`/`max_depth`, matching the old
/// `extract_cycles_from_scc` + `should_include_scc` filtering exactly.
pub fn execute_find_cycles(args: &FindCyclesArgs) -> Result<ToolExecution<FindCyclesData>> {
    // Pre-refactor timing: `start` fires before engine resolution.
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let ctx = crate::daemon_adapter::WorkspaceContext {
        workspace_root,
        graph,
        executor: engine.executor_arc(),
    };
    inner::execute_find_cycles(&ctx, args, start)
}

// ============================================================================
// Find Unused Tool
// ============================================================================

/// Map the MCP [`UnusedScope`] to a sqry-core [`sqry_core::query::UnusedScope`]
/// that is a *provable superset* of the MCP semantic, so the MCP post-filter
/// in [`should_include_in_unused_results`] / [`matches_scope_filter`] is the
/// single authoritative gate on what reaches the user.
///
/// # Why this is not an isomorphic mapping
///
/// The sqry-db scope must **never be narrower** than MCP's. Post-filtering
/// cannot recover rows sqry-db never returned. Audit:
///
/// | MCP scope | sqry-db semantic          | Relation to MCP   | Dispatch              |
/// |-----------|---------------------------|-------------------|-----------------------|
/// | `All`     | any node                  | exact             | sqry-db `All`         |
/// | `Public`  | visibility in `public`/`pub` | superset (MCP is strict `"public"`) | sqry-db `Public` |
/// | `Private` | visibility not in `public`/`pub` | superset (MCP is strict `"private"`) | sqry-db `Private` |
/// | `Function`| `Function` \| `Method`    | exact             | sqry-db `Function`    |
/// | `Struct`  | `Struct` \| `Class`       | **narrower** (MCP adds `Interface` \| `Trait`) | sqry-db `All` |
///
/// The DB16 landing passed `Struct` through verbatim, which silently dropped
/// unused interfaces and traits from results. That regression is the Codex
/// review blocker fixed here.
fn mcp_scope_to_core_superset(scope: UnusedScope) -> sqry_core::query::UnusedScope {
    match scope {
        UnusedScope::All => sqry_core::query::UnusedScope::All,
        UnusedScope::Public => sqry_core::query::UnusedScope::Public,
        UnusedScope::Private => sqry_core::query::UnusedScope::Private,
        UnusedScope::Function => sqry_core::query::UnusedScope::Function,
        // MCP `Struct` is `Struct | Class | Interface | Trait`; sqry-db's
        // `Struct` is only `Struct | Class`. Use `All` as the provable
        // superset and let the MCP post-filter narrow.
        UnusedScope::Struct => sqry_core::query::UnusedScope::All,
    }
}

/// Execute the `find_unused` tool to find unused/dead code.
///
/// # Dispatch path (DB16 + Codex post-review followup)
///
/// `find_unused` is a **name-keyed predicate** under the Phase 3C dispatch
/// taxonomy: the question is "which nodes match this scope AND are not
/// reachable from any entry point", which is exactly the planner-canonical
/// contract that sqry-db's [`sqry_db::queries::UnusedQuery`] computes.
///
/// The handler acquires a per-call [`sqry_db::QueryDb`] via
/// [`crate::execution::relation_dispatch::make_query_db`] (see that module's
/// docs for the Phase 3 dispatch taxonomy), dispatches
/// [`sqry_db::queries::UnusedQuery`] keyed on
/// [`sqry_db::queries::UnusedKey`] with:
///
/// * a **superset** sqry-db scope chosen by [`mcp_scope_to_core_superset`]
///   so the stage never returns fewer candidates than MCP's contract
///   accepts (see that function's docs for the full audit table —
///   `UnusedScope::Struct` in particular maps to sqry-db `All` because
///   sqry-db's `Struct` drops `Interface` / `Trait`), and
/// * `max_results = node_count` — i.e. no sqry-db truncation — whenever
///   MCP has a post-filter that may reject raw candidates (stricter
///   scope, `languages`, or `kinds`). This replaces the earlier fixed
///   `16x` cap, which could silently under-return when the first
///   16 × `max_results` raw candidates were mostly filtered out MCP-side
///   (Codex finding #2). When no MCP-side filter can narrow (MCP scope
///   is `All` / `Function` and both filter lists are empty), sqry-db
///   scope matches MCP exactly and the handler asks for only
///   `max_results` rows — strictly cheaper.
///
/// The post-filter ([`should_include_in_unused_results`]) applies MCP's
/// `languages` and `kinds` filters AND re-applies the scope filter using
/// MCP's stricter semantic for `Private` (visibility string is literally
/// `"private"`, rather than sqry-db's broader "anything that is not
/// `public`/`pub`") and broader semantic for `Struct` (`Struct | Class |
/// Interface | Trait`, vs sqry-db's `Struct | Class`). Because sqry-db is
/// invoked with the superset scope, the post-filter is the single
/// authoritative gate on what reaches the user, and the MCP API surface
/// is preserved exactly.
///
/// # Behavior shift
///
/// Results are now planner-canonical: the cache lives in sqry-db, so a
/// warm cache across MCP calls on the same snapshot is O(1) after the first
/// query. The legacy SCC-condensation fast-path (`find_unused_with_reachability`,
/// `mark_reachable_from_entries`, `identify_entry_points`) has been deleted —
/// those were DB14-era precomputed-SCC consumers that the sqry-db
/// [`sqry_db::queries::ReachableFromEntryPointsQuery`] supersedes. The
/// previous "Analysis files not found" empty-result warning branch is gone;
/// sqry-db computes on demand and caches.
pub fn execute_find_unused(args: &FindUnusedArgs) -> Result<ToolExecution<FindUnusedData>> {
    // Pre-refactor timing: `start` fires before engine resolution.
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let ctx = crate::daemon_adapter::WorkspaceContext {
        workspace_root,
        graph,
        executor: engine.executor_arc(),
    };
    inner::execute_find_unused(&ctx, args, start)
}

// ============================================================================
// New Graph-Based Analysis Tools
// ============================================================================

use crate::execution::types::{
    CallerCalleeData, DirectCalleesData, DirectCallersData, NodeInCycleData, PatternMatchData,
    PatternSearchData,
};
use crate::tools::{DirectCalleesArgs, DirectCallersArgs, IsNodeInCycleArgs, PatternSearchArgs};

/// Execute the `is_node_in_cycle` tool to check if a symbol is in a cycle.
///
/// # Dispatch path (DB17)
///
/// `is_node_in_cycle` is a **hybrid** under the Phase 3C dispatch taxonomy:
/// the surface takes a user-supplied symbol name (resolved via
/// [`resolve_global_symbol_strict`] — ambiguous simple names are rejected
/// with a canonical-qualified-name hint, mirroring the DB16 policy for
/// NodeId-anchored handlers), but the predicate it then answers is
/// strictly per-node ("does this `NodeId` participate in a cycle whose
/// size falls inside `[min_depth, max_depth]`"), which is exactly the
/// planner contract sqry-db's [`sqry_db::queries::IsInCycleQuery`]
/// caches on [`sqry_db::queries::IsInCycleKey`].
///
/// The handler:
/// 1. Resolves the symbol strictly to a single `NodeId` (ambiguous names
///    surface an MCP-error with the canonical-name workaround — a stricter
///    policy than sqry-db's name-keyed union because answering "in a
///    cycle?" for several unrelated `NodeId`s under one request has no
///    well-defined single boolean).
/// 2. Dispatches [`sqry_db::queries::IsInCycleQuery`] via
///    [`crate::execution::relation_dispatch::make_query_db`] to get the
///    boolean answer.
/// 3. Only if the answer is `true` dispatches [`sqry_db::queries::CyclesQuery`]
///    with the same `circular_type` + `bounds` to fetch the containing
///    cycle. sqry-db's `IsInCycleQuery` returns only `bool` today, so
///    this is a two-query flow; the second call is a cache hit on the
///    already-warmed [`sqry_db::queries::SccQuery`] and O(1) after the
///    first. Swapping to a richer `IsInCycleQuery` that returns the
///    component directly would save one Arc clone but requires sqry-db
///    changes; that is deliberately out of scope for DB17 (migration,
///    not query-surface invention — see the DB17 DAG entry).
///
/// # Behavior shift
///
/// The pre-DB17 implementation tried to load pre-persisted SCC data from
/// disk via `try_load_scc` and fell back to on-demand Tarjan otherwise.
/// That disk-first fast-path is retired — sqry-db computes and caches
/// on demand in memory, so first-call cost shifts into sqry-db and
/// warm-cache cost is the same O(1). The `CycleType::Modules` edge
/// probe now flows through
/// [`sqry_db::queries::cycles::edge_probe_for`], which collapses
/// `Modules` onto the `Imports` edge kind exactly like the pre-DB17
/// `cycle_edge_kind_name` helper did.
///
/// A second behavior-shift nuance (relative to the pre-DB17 CLI path
/// `find_cycle_containing_node`): the pre-DB17 implementation treated
/// `CycleType::Modules` inconsistently between the predicate path and
/// the materialization path. The DB17 migration routes both through
/// `edge_probe_for(Modules) = Imports`, so the two calls now agree on
/// the same edge-probe. If a user previously saw `in_cycle=true` for
/// `Modules` but got an empty or mismatched `cycle` list (or vice
/// versa), that asymmetry is silently fixed in DB17.
///
/// # `in_cycle=true, cycle=null` is unreachable
///
/// Codex peer-review Low 1 (2026-04-15): when the predicate reports
/// `in_cycle = true`, the containing-cycle lookup must always
/// succeed. The DB17 followup removes the `max_results = 100` cap on
/// the warm-cache `CyclesQuery` (it had no semantic justification
/// once `IsInCycleQuery` had already confirmed the node *is* in a
/// cycle — a 100-cycle cap could only hide the answer, never improve
/// correctness) and uses `usize::MAX` for the containing-cycle lookup.
/// The predicate-path `IsInCycleQuery` still uses the
/// handler-supplied `max_results = 100` default on the outer
/// `find_cycles` surface; this function's two calls are aligned on
/// bounds + circular_type so the `CyclesQuery` is a cache hit on the
/// already-warmed `SccQuery`.
pub fn execute_is_node_in_cycle(
    args: &IsNodeInCycleArgs,
) -> Result<ToolExecution<NodeInCycleData>> {
    // Pre-refactor timing: `start` fires before engine resolution.
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let ctx = crate::daemon_adapter::WorkspaceContext {
        workspace_root,
        graph,
        executor: engine.executor_arc(),
    };
    inner::execute_is_node_in_cycle(&ctx, args, start)
}

/// Execute the `pattern_search` tool to find symbols by pattern.
pub fn execute_pattern_search(
    args: &PatternSearchArgs,
) -> Result<ToolExecution<PatternSearchData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files = snapshot.files();

    // Find nodes matching the pattern
    let matching_ids = snapshot.find_by_pattern(&args.pattern);

    // Filter out classpath (external) nodes unless explicitly requested
    let filtered_ids: Vec<_> = if args.include_classpath {
        matching_ids
    } else {
        matching_ids
            .into_iter()
            .filter(|&node_id| {
                !crate::execution::symbol_utils::is_node_external(&snapshot, node_id)
            })
            .collect()
    };

    // Convert to output format
    let mut matches: Vec<PatternMatchData> = filtered_ids
        .into_iter()
        .filter_map(|node_id| {
            let entry = snapshot.get_node(node_id)?;
            let name = strings.resolve(entry.name)?.to_string();
            let language = files.language_for_file(entry.file);
            let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
                entry, strings, files, &name,
            );

            let file_path = files.resolve(entry.file)?;
            let full_path = workspace_root.join(file_path.as_ref());
            let file_uri = url::Url::from_file_path(&full_path).ok().map_or_else(
                || crate::execution::symbol_utils::path_to_forward_slash(&full_path),
                Into::into,
            );

            let language = language.map_or("unknown".to_string(), |l| l.to_string());

            let kind = format!("{:?}", entry.kind).to_lowercase();

            let provenance = crate::execution::symbol_utils::get_classpath_provenance_for_node(
                &snapshot, node_id,
            );

            let loc = node_location_for_reporting(&graph, node_id, &workspace_root);

            Some(PatternMatchData {
                name,
                qualified_name,
                kind,
                file_uri,
                line: loc.as_ref().map_or(entry.start_line, |l| l.line),
                language,
                provenance,
            })
        })
        .collect();

    let total = matches.len();
    matches.truncate(args.max_results);

    let (page_slice, next_page_token) = paginate(&matches, &args.pagination);
    let page_matches = page_slice.to_vec();

    Ok(ToolExecution {
        data: PatternSearchData {
            pattern: args.pattern.clone(),
            matches: page_matches,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(total > args.max_results),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Find similar symbol names for error message suggestions.
///
/// Uses simple substring and prefix matching to find candidates.
fn find_similar_symbols(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    query: &str,
    max_suggestions: usize,
) -> Vec<String> {
    let query_lower = query.to_lowercase();
    let strings = snapshot.strings();
    let mut suggestions = Vec::new();

    // Extract the simple name part (after last :: or .)
    let simple_query = query_lower
        .rsplit("::")
        .next()
        .unwrap_or(&query_lower)
        .split('.')
        .next_back()
        .unwrap_or(&query_lower);

    for (_node_id, entry) in snapshot.nodes().iter() {
        // Gate 0d iter-2 fix: skip unified losers from symbol
        // suggestion lists. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            continue;
        }
        if suggestions.len() >= max_suggestions * 2 {
            break;
        }

        let name = match entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .or_else(|| strings.resolve(entry.name))
        {
            Some(n) => n.to_string(),
            None => continue,
        };

        let name_lower = name.to_lowercase();

        // Check for substring match (either direction) with a minimum length threshold
        // to avoid matching single-character names like "b" for "build_all_with_budget"
        let is_substring_match = name_lower.contains(simple_query)
            || (simple_query.contains(&name_lower) && name_lower.len() >= 3);
        if is_substring_match && name != query {
            suggestions.push(name);
        }
    }

    // Deduplicate and limit
    suggestions.sort();
    suggestions.dedup();
    suggestions.truncate(max_suggestions);
    suggestions
}

fn decorate_single_symbol_lookup_error(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    if !error.to_string().contains("not found in graph") {
        return error;
    }

    let suggestions = find_similar_symbols(snapshot, symbol, 3);
    let suggestions_str = if suggestions.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nDid you mean one of these?\n  • {}",
            suggestions.join("\n  • ")
        )
    };

    anyhow!(
        "Symbol '{symbol}' not found in graph.\n\n\
         Hints:\n\
         • Use the full qualified name (e.g., 'module::function_name')\n\
         • Check for typos in the symbol name\n\
         • Run 'semantic_search' to find the correct symbol name{suggestions_str}"
    )
}

/// Execute the `direct_callers` tool to find direct callers of a symbol.
///
/// DB15 migration: routes the depth-1 caller predicate through sqry-db's
/// `mcp_callers_query` (which dispatches to `CalleesQuery` per the planner
/// inversion documented in [`crate::execution::relation_dispatch`]).
/// Behavior shift relative to the pre-DB15 implementation: instead of
/// "callers of THE canonically-resolved node X", returns "callers of any
/// node named X" (planner-aligned, segment-aware). The two collapse for
/// uniquely-named symbols; for ambiguous names the new behavior is broader
/// and matches the planner-canonical semantic (Phase N "Unified Surface
/// Contract": planner IR is the canonical semantics surface, CLI/MCP/LSP
/// mirror).
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, the unified
/// graph cannot be loaded or auto-built, or the requested symbol does not
/// exist anywhere in the graph (suggestion-decorated diagnostic).
pub fn execute_direct_callers(
    args: &DirectCallersArgs,
) -> Result<ToolExecution<DirectCallersData>> {
    // Pre-refactor timing: `start` fires before engine resolution.
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let ctx = crate::daemon_adapter::WorkspaceContext {
        workspace_root,
        graph,
        executor: engine.executor_arc(),
    };
    inner::execute_direct_callers(&ctx, args, start)
}

/// Execute the `direct_callees` tool to find direct callees of a symbol.
///
/// DB15 migration: routes the depth-1 callee predicate through sqry-db's
/// `mcp_callees_query` (which dispatches to `CallersQuery` per the planner
/// inversion documented in [`crate::execution::relation_dispatch`]). See
/// [`execute_direct_callers`] for the behavior-shift note that applies
/// equally here.
///
/// # Errors
///
/// Same error surface as [`execute_direct_callers`].
pub fn execute_direct_callees(
    args: &DirectCalleesArgs,
) -> Result<ToolExecution<DirectCalleesData>> {
    // Pre-refactor timing: `start` fires before engine resolution.
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let ctx = crate::daemon_adapter::WorkspaceContext {
        workspace_root,
        graph,
        executor: engine.executor_arc(),
    };
    inner::execute_direct_callees(&ctx, args, start)
}

/// Build `CallerCalleeData` rows for a sqry-db result set, looking up
/// per-node display info from the snapshot.
fn build_caller_callee_data(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node_ids: &[sqry_core::graph::unified::node::NodeId],
    workspace_root: &std::path::Path,
) -> Vec<CallerCalleeData> {
    let strings = snapshot.strings();
    let files = snapshot.files();

    node_ids
        .iter()
        .filter_map(|&node_id| {
            let entry = snapshot.get_node(node_id)?;
            let name = strings.resolve(entry.name)?.to_string();
            let language = files.language_for_file(entry.file);
            let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
                entry, strings, files, &name,
            );

            let loc = node_location_for_reporting(graph, node_id, workspace_root);

            // Resolved location may live in a different file than
            // entry.file (cross-file stub via CanonicalSibling /
            // ExternSymbol). Use the resolved location's file/language so
            // the file_uri / line / language fields stay consistent.
            let file_path = loc
                .as_ref()
                .filter(|l| !l.file_path.is_empty())
                .map(|l| workspace_root.join(&l.file_path))
                .or_else(|| {
                    files
                        .resolve(entry.file)
                        .map(|p| workspace_root.join(p.as_ref()))
                })
                .unwrap_or_default();

            let file_uri = url::Url::from_file_path(&file_path).ok().map_or_else(
                || crate::execution::symbol_utils::path_to_forward_slash(&file_path),
                Into::into,
            );

            let language = loc
                .as_ref()
                .and_then(|l| l.language.clone())
                .or_else(|| language.map(|l| l.to_string()))
                .unwrap_or_else(|| "unknown".to_string());

            let kind = format!("{:?}", entry.kind).to_lowercase();

            Some(CallerCalleeData {
                name,
                qualified_name,
                kind,
                file_uri,
                line: loc.as_ref().map_or(entry.start_line, |l| l.line),
                language,
            })
        })
        .collect()
}

pub(crate) mod inner {
    use super::*;

    /// Daemon/SqryServer-shared body for `dependency_impact`.
    pub(crate) fn execute_dependency_impact(
        ctx: &crate::daemon_adapter::WorkspaceContext,
        args: &DependencyImpactArgs,
        start: Instant,
    ) -> Result<ToolExecution<DependencyImpactData>> {
        let workspace_root: &std::path::Path = &ctx.workspace_root;
        let graph = &ctx.graph;

        let snapshot = graph.snapshot();

        let target_node_id =
            resolve_global_symbol_strict(&snapshot, &args.symbol, args.file_path.as_deref())?;

        let (mut impacted, affected_files) =
            collect_impacted_callers_bfs(&snapshot, graph, target_node_id, args, workspace_root);

        let total = impacted.len();
        let truncated = total > args.max_results;
        impacted.truncate(args.max_results);

        let (page_slice, next_page_token) = paginate(&impacted, &args.pagination);
        let page_impacted: Vec<ImpactedSymbol> = page_slice.to_vec();

        let affected_files_vec = if args.include_files {
            let mut files_list: Vec<String> = affected_files.into_iter().collect();
            files_list.sort();
            Some(files_list)
        } else {
            None
        };

        let truncated_flag = truncated || next_page_token.is_some();

        Ok(ToolExecution {
            data: DependencyImpactData {
                target_symbol: args.symbol.clone(),
                impacted_symbols: page_impacted,
                affected_files: affected_files_vec,
                total: total as u64,
            },
            used_index: false,
            used_graph: true,
            graph_metadata: None,
            execution_ms: duration_to_ms(start.elapsed()),
            next_page_token,
            total: Some(total as u64),
            truncated: Some(truncated_flag),
            candidates_scanned: None,
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
        })
    }

    /// Daemon/SqryServer-shared body for `semantic_diff`.
    ///
    /// `ctx.graph` is unused — `semantic_diff` builds per-git-ref graphs from
    /// worktrees (see rustdoc on the public wrapper). The context is still
    /// supplied for daemon-path symmetry.
    pub(crate) fn execute_semantic_diff(
        ctx: &crate::daemon_adapter::WorkspaceContext,
        args: &SemanticDiffArgs,
        start: Instant,
    ) -> Result<ToolExecution<SemanticDiffData>> {
        use sqry_core::git::resolve_ref_to_commit;
        use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
        use sqry_db::{ComparativeQueryDb, DiffOptions};

        let workspace_root: &std::path::Path = &ctx.workspace_root;

        tracing::debug!(
            base_ref = %args.base.git_ref,
            target_ref = %args.target.git_ref,
            include_unchanged = args.include_unchanged,
            max_results = args.max_results,
            "Executing semantic_diff tool"
        );

        // Identical-OID fast-path: if both refs resolve to the same commit
        // (e.g., self-diff against `HEAD`), the diff is provably empty. Skip
        // worktree creation + per-ref graph builds, which on kernel-scale
        // repos blow the 120s tool deadline. (verivus-oss/sqry#213)
        let base_sha = resolve_ref_to_commit(workspace_root, &args.base.git_ref)
            .map_err(|e| anyhow!("Failed to resolve base ref '{}': {e}", args.base.git_ref))?;
        let target_sha =
            resolve_ref_to_commit(workspace_root, &args.target.git_ref).map_err(|e| {
                anyhow!(
                    "Failed to resolve target ref '{}': {e}",
                    args.target.git_ref
                )
            })?;
        if base_sha == target_sha {
            tracing::debug!(
                base_ref = %args.base.git_ref,
                target_ref = %args.target.git_ref,
                sha = %base_sha,
                "semantic_diff identical-OID fast-path: emitting empty diff"
            );
            return Ok(ToolExecution {
                data: SemanticDiffData {
                    base_ref: args.base.git_ref.clone(),
                    target_ref: args.target.git_ref.clone(),
                    changes: Vec::new(),
                    summary: summarise_wire_changes(&[]),
                    total: 0,
                },
                used_index: false,
                used_graph: false,
                graph_metadata: None,
                execution_ms: duration_to_ms(start.elapsed()),
                next_page_token: None,
                total: Some(0),
                truncated: Some(false),
                candidates_scanned: None,
                workspace_path: crate::execution::symbol_utils::path_to_forward_slash(
                    workspace_root,
                ),
            });
        }

        // Phase 1: Create git worktrees (one per ref). The manager is RAII: the
        // worktrees are cleaned up when it drops at the end of this function.
        let worktree_mgr = git_worktree::WorktreeManager::create(
            workspace_root,
            &args.base.git_ref,
            &args.target.git_ref,
        )?;

        // Phase 2: Build CodeGraphs for both worktrees. These do NOT populate
        // the engine's shared `QueryDb` — the handler owns them directly.
        let plugins = Engine::plugin_manager();
        let config = BuildConfig::default();
        let base_graph = build_unified_graph(worktree_mgr.base_path(), &plugins, &config)
            .map_err(|e| anyhow!("Failed to build base graph: {e}"))?;
        let target_graph = build_unified_graph(worktree_mgr.target_path(), &plugins, &config)
            .map_err(|e| anyhow!("Failed to build target graph: {e}"))?;

        // Phase 3: Compare snapshots through ComparativeQueryDb (uncached).
        let base_snapshot = Arc::new(base_graph.snapshot());
        let target_snapshot = Arc::new(target_graph.snapshot());
        let cmp_db =
            ComparativeQueryDb::new(Arc::clone(&base_snapshot), Arc::clone(&target_snapshot));
        let diff_opts = DiffOptions {
            old_worktree_path: worktree_mgr.base_path().to_path_buf(),
            new_worktree_path: worktree_mgr.target_path().to_path_buf(),
        };
        let diff_output = cmp_db.diff(&diff_opts);

        let worktree_base = worktree_mgr.base_path().to_path_buf();
        let worktree_target = worktree_mgr.target_path().to_path_buf();
        let mut changes: Vec<NodeChange> = diff_output
            .changes
            .into_iter()
            .map(|c| convert_db_change_to_wire(c, workspace_root, &worktree_base, &worktree_target))
            .collect::<Result<Vec<_>>>()?;

        // Phase 4: Apply filters (wire-level strings — predicates unchanged).
        if !args.filters.change_types.is_empty() {
            changes.retain(|change| {
                args.filters
                    .change_types
                    .iter()
                    .any(|ct| ct.as_str() == change.change_type)
            });
        }
        if !args.filters.symbol_kinds.is_empty() {
            changes.retain(|change| {
                args.filters
                    .symbol_kinds
                    .iter()
                    .any(|kind| kind.eq_ignore_ascii_case(&change.kind))
            });
        }
        if !args.include_unchanged {
            changes.retain(|change| change.change_type != "unchanged");
        }

        // Phase 5: Compute summary post-filter, pre-pagination.
        let summary = summarise_wire_changes(&changes);

        // Phase 6: Apply pagination.
        let total = changes.len();
        let truncated = total > args.max_results;
        changes.truncate(args.max_results);

        let (page_slice, next_page_token) = paginate(&changes, &args.pagination);
        let page_changes = page_slice.to_vec();

        let execution_ms = duration_to_ms(start.elapsed());

        tracing::debug!(
            total_changes = total,
            page_size = page_changes.len(),
            execution_ms = execution_ms,
            "semantic_diff tool completed"
        );

        Ok(ToolExecution {
            data: SemanticDiffData {
                base_ref: args.base.git_ref.clone(),
                target_ref: args.target.git_ref.clone(),
                changes: page_changes,
                summary,
                total: total as u64,
            },
            used_index: false,
            used_graph: true,
            graph_metadata: None,
            execution_ms,
            next_page_token,
            total: Some(total as u64),
            truncated: Some(truncated),
            candidates_scanned: None,
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
        })
        // worktree_mgr drops here, automatic cleanup.
    }

    /// Daemon/SqryServer-shared body for `find_cycles`.
    pub(crate) fn execute_find_cycles(
        ctx: &crate::daemon_adapter::WorkspaceContext,
        args: &FindCyclesArgs,
        start: Instant,
    ) -> Result<ToolExecution<FindCyclesData>> {
        let workspace_root: &std::path::Path = &ctx.workspace_root;
        let graph = &ctx.graph;

        let snapshot = Arc::new(graph.snapshot());

        // Route through sqry-db: `CyclesQuery` is the name-keyed cycle
        // predicate in the planner taxonomy, cached per-snapshot.
        // PN3 CLIENT_LOAD: cold-load derived cache from workspace companion file.
        let db =
            sqry_db::queries::dispatch::make_query_db_cold(Arc::clone(&snapshot), workspace_root);
        let key = sqry_db::queries::CyclesKey {
            circular_type: mcp_cycle_type_to_core(args.cycle_type),
            bounds: cycle_bounds_for(
                args.min_depth,
                args.max_depth,
                args.max_results,
                args.include_self_loops,
            ),
        };
        let cycle_node_ids = db.get::<sqry_db::queries::CyclesQuery>(&key);
        let cycles = materialize_cycle_node_ids(&cycle_node_ids, snapshot.as_ref());

        // Convert to output format
        let output_cycles =
            convert_cycles_to_output(cycles, snapshot.as_ref(), graph, workspace_root);

        let total = output_cycles.len();
        let (page_slice, next_page_token) = paginate(&output_cycles, &args.pagination);
        let page_cycles = page_slice.to_vec();

        Ok(ToolExecution {
            data: FindCyclesData {
                cycle_type: cycle_type_label(args.cycle_type).to_string(),
                cycles: page_cycles,
                total: total as u64,
            },
            used_index: false,
            used_graph: true,
            graph_metadata: None,
            execution_ms: duration_to_ms(start.elapsed()),
            next_page_token,
            total: Some(total as u64),
            truncated: Some(total > args.max_results),
            candidates_scanned: None,
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
        })
    }

    /// Daemon/SqryServer-shared body for `find_unused`.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "Shared tool-handler contract returns Result for uniform daemon and MCP dispatch."
    )]
    pub(crate) fn execute_find_unused(
        ctx: &crate::daemon_adapter::WorkspaceContext,
        args: &FindUnusedArgs,
        start: Instant,
    ) -> Result<ToolExecution<FindUnusedData>> {
        let workspace_root: &std::path::Path = &ctx.workspace_root;
        let graph = &ctx.graph;

        let snapshot = Arc::new(graph.snapshot());

        let scope_str = match args.scope {
            UnusedScope::Public => "public",
            UnusedScope::Private => "private",
            UnusedScope::Function => "function",
            UnusedScope::Struct => "struct",
            UnusedScope::All => "all",
        };

        // PN3 CLIENT_LOAD: cold-load derived cache from workspace companion file.
        let db =
            sqry_db::queries::dispatch::make_query_db_cold(Arc::clone(&snapshot), workspace_root);
        let node_count = snapshot.nodes().len();
        // Boundary filters include MCP language/kind/scope narrowing and the
        // binding-plane post-filter. Any of them can suppress early raw rows,
        // so request the full pool and truncate only after all filters run.
        let candidate_cap = node_count.max(args.max_results);
        let key = sqry_db::queries::UnusedKey {
            scope: mcp_scope_to_core_superset(args.scope),
            max_results: candidate_cap,
        };
        let raw_unused_ids = db.get::<sqry_db::queries::UnusedQuery>(&key);
        let unused_ids = sqry_db::queries::unused_post_filter::apply_binding_plane_post_filter(
            &raw_unused_ids,
            &snapshot,
            &db,
        );

        let strings = snapshot.strings();
        let files = snapshot.files();

        let mut unused_symbols: Vec<UnusedSymbolData> = Vec::new();
        for &node_id in &unused_ids {
            if unused_symbols.len() >= args.max_results {
                break;
            }
            let Some(entry) = snapshot.get_node(node_id) else {
                continue;
            };
            // MCP post-filter: applies language + kind filters and re-asserts
            // the scope using MCP's stricter `Private` semantic.
            if !should_include_in_unused_results(entry, args, strings, files) {
                continue;
            }
            unused_symbols.push(build_unused_symbol_data(
                entry,
                node_id,
                graph,
                strings,
                files,
                workspace_root,
            ));
        }

        let total = unused_symbols.len();
        let (page_slice, next_page_token) = paginate(&unused_symbols, &args.pagination);

        Ok(ToolExecution {
            data: FindUnusedData {
                scope: scope_str.to_string(),
                symbols: page_slice.to_vec(),
                total: total as u64,
            },
            used_index: false,
            used_graph: true,
            graph_metadata: None,
            execution_ms: duration_to_ms(start.elapsed()),
            next_page_token,
            total: Some(total as u64),
            truncated: Some(total > args.max_results),
            candidates_scanned: None,
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
        })
    }

    /// Daemon/SqryServer-shared body for `is_node_in_cycle`.
    pub(crate) fn execute_is_node_in_cycle(
        ctx: &crate::daemon_adapter::WorkspaceContext,
        args: &IsNodeInCycleArgs,
        start: Instant,
    ) -> Result<ToolExecution<NodeInCycleData>> {
        let workspace_root: &std::path::Path = &ctx.workspace_root;
        let graph = &ctx.graph;

        let snapshot = Arc::new(graph.snapshot());

        let node_id = resolve_global_symbol_strict(
            snapshot.as_ref(),
            &args.symbol,
            args.file_path.as_deref(),
        )?;

        let circular_type = mcp_cycle_type_to_core(args.cycle_type);
        let predicate_bounds =
            cycle_bounds_for(args.min_depth, args.max_depth, 100, args.include_self_loops);

        // PN3 CLIENT_LOAD: cold-load derived cache from workspace companion file.
        let db =
            sqry_db::queries::dispatch::make_query_db_cold(Arc::clone(&snapshot), workspace_root);
        let in_cycle =
            db.get::<sqry_db::queries::IsInCycleQuery>(&sqry_db::queries::IsInCycleKey {
                node_id,
                circular_type,
                bounds: predicate_bounds,
            });

        let cycle = if in_cycle {
            let cycle_lookup_bounds = cycle_bounds_for(
                args.min_depth,
                args.max_depth,
                usize::MAX,
                args.include_self_loops,
            );
            let cycles_key = sqry_db::queries::CyclesKey {
                circular_type,
                bounds: cycle_lookup_bounds,
            };
            let all_cycles = db.get::<sqry_db::queries::CyclesQuery>(&cycles_key);
            all_cycles
                .iter()
                .find(|component| component.contains(&node_id))
                .map(|component| {
                    materialize_cycle_node_ids(std::slice::from_ref(component), snapshot.as_ref())
                        .into_iter()
                        .next()
                        .unwrap_or_default()
                })
        } else {
            None
        };

        let cycle_type_str = match args.cycle_type {
            CycleType::Calls => "calls",
            CycleType::Imports => "imports",
            CycleType::Modules => "modules",
        };

        Ok(ToolExecution {
            data: NodeInCycleData {
                symbol: args.symbol.clone(),
                in_cycle,
                cycle_type: cycle_type_str.to_string(),
                cycle,
            },
            used_index: false,
            used_graph: true,
            graph_metadata: None,
            execution_ms: duration_to_ms(start.elapsed()),
            next_page_token: None,
            total: None,
            truncated: None,
            candidates_scanned: None,
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
        })
    }

    /// Daemon/SqryServer-shared body for `direct_callers`.
    pub(crate) fn execute_direct_callers(
        ctx: &crate::daemon_adapter::WorkspaceContext,
        args: &DirectCallersArgs,
        start: Instant,
    ) -> Result<ToolExecution<DirectCallersData>> {
        let workspace_root: &std::path::Path = &ctx.workspace_root;
        let graph = &ctx.graph;

        let snapshot = std::sync::Arc::new(graph.snapshot());

        // Existence check — surfaces the same suggestion-decorated error when
        // the symbol is unresolvable.
        if sqry_core::graph::unified::materialize::find_nodes_by_name(&snapshot, &args.symbol)
            .is_empty()
        {
            return Err(decorate_single_symbol_lookup_error(
                &snapshot,
                &args.symbol,
                anyhow!("Symbol '{}' not found in graph.", args.symbol),
            ));
        }

        // PN3 CLIENT_LOAD: cold-load derived cache from workspace companion file.
        let db = sqry_db::queries::dispatch::make_query_db_cold(
            std::sync::Arc::clone(&snapshot),
            workspace_root,
        );
        let key = sqry_db::queries::RelationKey::exact(&args.symbol);
        let caller_ids = crate::execution::relation_dispatch::mcp_callers_query(&db, &key);

        let mut callers = build_caller_callee_data(graph, &snapshot, &caller_ids, workspace_root);

        let total = callers.len();
        callers.truncate(args.max_results);

        let (page_slice, next_page_token) = paginate(&callers, &args.pagination);
        let page_callers = page_slice.to_vec();

        Ok(ToolExecution {
            data: DirectCallersData {
                target: args.symbol.clone(),
                callers: page_callers,
                total: total as u64,
            },
            used_index: false,
            used_graph: true,
            graph_metadata: None,
            execution_ms: duration_to_ms(start.elapsed()),
            next_page_token,
            total: Some(total as u64),
            truncated: Some(total > args.max_results),
            candidates_scanned: None,
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
        })
    }

    /// Daemon/SqryServer-shared body for `direct_callees`.
    pub(crate) fn execute_direct_callees(
        ctx: &crate::daemon_adapter::WorkspaceContext,
        args: &DirectCalleesArgs,
        start: Instant,
    ) -> Result<ToolExecution<DirectCalleesData>> {
        let workspace_root: &std::path::Path = &ctx.workspace_root;
        let graph = &ctx.graph;

        let snapshot = std::sync::Arc::new(graph.snapshot());

        if sqry_core::graph::unified::materialize::find_nodes_by_name(&snapshot, &args.symbol)
            .is_empty()
        {
            return Err(decorate_single_symbol_lookup_error(
                &snapshot,
                &args.symbol,
                anyhow!("Symbol '{}' not found in graph.", args.symbol),
            ));
        }

        // PN3 CLIENT_LOAD: cold-load derived cache from workspace companion file.
        let db = sqry_db::queries::dispatch::make_query_db_cold(
            std::sync::Arc::clone(&snapshot),
            workspace_root,
        );
        let key = sqry_db::queries::RelationKey::exact(&args.symbol);
        let callee_ids = crate::execution::relation_dispatch::mcp_callees_query(&db, &key);

        let mut callees = build_caller_callee_data(graph, &snapshot, &callee_ids, workspace_root);

        let total = callees.len();
        callees.truncate(args.max_results);

        let (page_slice, next_page_token) = paginate(&callees, &args.pagination);
        let page_callees = page_slice.to_vec();

        Ok(ToolExecution {
            data: DirectCalleesData {
                source: args.symbol.clone(),
                callees: page_callees,
                total: total as u64,
            },
            used_index: false,
            used_graph: true,
            graph_metadata: None,
            execution_ms: duration_to_ms(start.elapsed()),
            next_page_token,
            total: Some(total as u64),
            truncated: Some(total > args.max_results),
            candidates_scanned: None,
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::edge::{EdgeKind, ResolvedVia};
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::NodeEntry;
    use std::path::Path;

    // ========================================================================
    // Find Unused Tests
    // ========================================================================

    /// Helper to create a test graph with entry points and reachable/unreachable nodes
    fn create_test_graph_for_unused() -> CodeGraph {
        use sqry_core::graph::unified::ExportKind;
        let mut graph = CodeGraph::new();

        // Register test files
        let main_rs = graph
            .files_mut()
            .register(Path::new("src/main.rs"))
            .unwrap();
        let lib_rs = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let helper_rs = graph
            .files_mut()
            .register(Path::new("src/helper.rs"))
            .unwrap();

        // Create nodes
        let main_fn = graph.strings_mut().intern("main").unwrap();
        let public_fn = graph.strings_mut().intern("public_function").unwrap();
        let used_fn = graph.strings_mut().intern("used_helper").unwrap();
        let unused_fn = graph.strings_mut().intern("unused_helper").unwrap();

        // Create visibility strings
        let vis_public = graph.strings_mut().intern("public").unwrap();
        let vis_private = graph.strings_mut().intern("private").unwrap();

        // Allocate nodes
        let mut main_entry = NodeEntry::new(NodeKind::Function, main_fn, main_rs);
        main_entry.visibility = Some(vis_public);
        let node_main = graph.nodes_mut().alloc(main_entry).unwrap();
        graph
            .indices_mut()
            .add(node_main, NodeKind::Function, main_fn, None, main_rs);

        let mut public_entry = NodeEntry::new(NodeKind::Function, public_fn, lib_rs);
        public_entry.visibility = Some(vis_public);
        let node_public = graph.nodes_mut().alloc(public_entry).unwrap();
        graph
            .indices_mut()
            .add(node_public, NodeKind::Function, public_fn, None, lib_rs);

        let mut used_entry = NodeEntry::new(NodeKind::Function, used_fn, helper_rs);
        used_entry.visibility = Some(vis_private);
        let node_used = graph.nodes_mut().alloc(used_entry).unwrap();
        graph
            .indices_mut()
            .add(node_used, NodeKind::Function, used_fn, None, helper_rs);

        let mut unused_entry = NodeEntry::new(NodeKind::Function, unused_fn, helper_rs);
        unused_entry.visibility = Some(vis_private);
        let node_unused = graph.nodes_mut().alloc(unused_entry).unwrap();
        graph
            .indices_mut()
            .add(node_unused, NodeKind::Function, unused_fn, None, helper_rs);

        // Create call edges: main -> public_function -> used_helper
        // unused_helper is not called by anyone
        let call_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        graph
            .edges_mut()
            .add_edge(node_main, node_public, call_kind.clone(), main_rs);
        graph
            .edges_mut()
            .add_edge(node_public, node_used, call_kind, lib_rs);

        // Add export edge from lib.rs for public_function
        let export_kind = EdgeKind::Exports {
            kind: ExportKind::Direct,
            alias: None,
        };
        graph
            .edges_mut()
            .add_edge(node_public, node_public, export_kind, lib_rs);

        graph
    }

    #[test]
    fn test_matches_scope_filter_public() {
        let graph = create_test_graph_for_unused();
        let snapshot = graph.snapshot();
        let strings = snapshot.strings();

        let public_node = candidate_bucket_for_symbol(&snapshot, "public_function")
            .into_iter()
            .next()
            .unwrap();
        let public_entry = snapshot.get_node(public_node).unwrap();

        let visibility_string = public_entry
            .visibility
            .and_then(|vid| strings.resolve(vid))
            .map(|s| s.to_string());
        let visibility_str = visibility_string.as_deref();

        // Public function should match Public scope
        assert!(matches_scope_filter(
            public_entry.kind,
            visibility_str,
            UnusedScope::Public
        ));

        // Public function should match All scope
        assert!(matches_scope_filter(
            public_entry.kind,
            visibility_str,
            UnusedScope::All
        ));

        // Public function should NOT match Private scope
        assert!(!matches_scope_filter(
            public_entry.kind,
            visibility_str,
            UnusedScope::Private
        ));
    }

    #[test]
    fn test_matches_scope_filter_private() {
        let graph = create_test_graph_for_unused();
        let snapshot = graph.snapshot();
        let strings = snapshot.strings();

        let unused_node = candidate_bucket_for_symbol(&snapshot, "unused_helper")
            .into_iter()
            .next()
            .unwrap();
        let unused_entry = snapshot.get_node(unused_node).unwrap();

        let visibility_string = unused_entry
            .visibility
            .and_then(|vid| strings.resolve(vid))
            .map(|s| s.to_string());
        let visibility_str = visibility_string.as_deref();

        // Private function should match Private scope
        assert!(matches_scope_filter(
            unused_entry.kind,
            visibility_str,
            UnusedScope::Private
        ));

        // Private function should match All scope
        assert!(matches_scope_filter(
            unused_entry.kind,
            visibility_str,
            UnusedScope::All
        ));

        // Private function should NOT match Public scope
        assert!(!matches_scope_filter(
            unused_entry.kind,
            visibility_str,
            UnusedScope::Public
        ));
    }

    #[test]
    fn test_matches_scope_filter_function() {
        let graph = create_test_graph_for_unused();
        let snapshot = graph.snapshot();

        let main_node = candidate_bucket_for_symbol(&snapshot, "main")
            .into_iter()
            .next()
            .unwrap();
        let main_entry = snapshot.get_node(main_node).unwrap();

        // Function should match Function scope
        assert!(matches_scope_filter(
            main_entry.kind,
            None,
            UnusedScope::Function
        ));

        // Function should NOT match Struct scope
        assert!(!matches_scope_filter(
            main_entry.kind,
            None,
            UnusedScope::Struct
        ));
    }

    // ========================================================================
    // Cycle helper mapping function tests
    // ========================================================================

    #[test]
    fn test_cycle_type_label_calls() {
        assert_eq!(cycle_type_label(CycleType::Calls), "calls");
    }

    #[test]
    fn test_cycle_type_label_imports() {
        assert_eq!(cycle_type_label(CycleType::Imports), "imports");
    }

    #[test]
    fn test_cycle_type_label_modules() {
        assert_eq!(cycle_type_label(CycleType::Modules), "modules");
    }

    #[test]
    fn test_mcp_cycle_type_to_core_parity() {
        // Locks the MCP→sqry-db cycle-type bridge used by the DB17
        // migration of `find_cycles` / `is_node_in_cycle`. Any shift in
        // the mapping (e.g. collapsing `Modules` onto `Calls`) would be
        // a silent behavior change for the MCP API.
        assert_eq!(
            mcp_cycle_type_to_core(CycleType::Calls),
            CircularType::Calls
        );
        assert_eq!(
            mcp_cycle_type_to_core(CycleType::Imports),
            CircularType::Imports
        );
        assert_eq!(
            mcp_cycle_type_to_core(CycleType::Modules),
            CircularType::Modules
        );
    }

    // ========================================================================
    // resolve_workspace_path tests
    // ========================================================================

    #[test]
    fn test_resolve_workspace_path_dot_returns_none() {
        assert!(resolve_workspace_path(".").is_none());
    }

    #[test]
    fn test_resolve_workspace_path_explicit_path_returns_some() {
        let result = resolve_workspace_path("/some/path");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), std::path::PathBuf::from("/some/path"));
    }

    // ========================================================================
    // matches_scope_filter for Struct scope
    // ========================================================================

    #[test]
    fn test_matches_scope_filter_struct_for_class() {
        assert!(matches_scope_filter(
            sqry_core::graph::unified::node::NodeKind::Class,
            None,
            UnusedScope::Struct
        ));
    }

    #[test]
    fn test_matches_scope_filter_struct_for_interface() {
        assert!(matches_scope_filter(
            sqry_core::graph::unified::node::NodeKind::Interface,
            None,
            UnusedScope::Struct
        ));
    }

    #[test]
    fn test_matches_scope_filter_struct_for_trait() {
        assert!(matches_scope_filter(
            sqry_core::graph::unified::node::NodeKind::Trait,
            None,
            UnusedScope::Struct
        ));
    }

    #[test]
    fn test_matches_scope_filter_function_for_method() {
        assert!(matches_scope_filter(
            sqry_core::graph::unified::node::NodeKind::Method,
            None,
            UnusedScope::Function
        ));
    }

    // ========================================================================
    // convert_cycles_to_output tests
    // ========================================================================

    #[test]
    fn test_convert_cycles_to_output_empty_cycles() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");
        let result = convert_cycles_to_output(vec![], &snapshot, &graph, ws);
        assert!(result.is_empty());
    }

    #[test]
    fn test_convert_cycles_to_output_nonempty_cycle() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");

        // A cycle with two symbol names
        let cycles = vec![vec!["alpha".to_string(), "beta".to_string()]];
        let result = convert_cycles_to_output(cycles, &snapshot, &graph, ws);
        assert_eq!(result.len(), 1);
        let cycle = &result[0];
        assert_eq!(cycle.depth, 2);
        // Chain should contain both names and close the cycle
        assert!(cycle.chain.contains("alpha"));
        assert!(cycle.chain.contains("beta"));
        // Chain closes: last entry repeated at end
        assert!(cycle.chain.ends_with("alpha"));
    }

    #[test]
    fn test_convert_cycles_to_output_single_node_cycle() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");

        let cycles = vec![vec!["selfloop".to_string()]];
        let result = convert_cycles_to_output(cycles, &snapshot, &graph, ws);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].depth, 1);
        // Chain: "selfloop → selfloop"
        assert!(result[0].chain.contains("selfloop"));
    }

    // ========================================================================
    // candidate_bucket_for_symbol tests
    // ========================================================================

    #[test]
    fn test_candidate_bucket_finds_registered_symbol() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/x.rs")).unwrap();
        let nm = graph.strings_mut().intern("my_func").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, nm, None, file_id);

        let snapshot = graph.snapshot();
        let candidates = candidate_bucket_for_symbol(&snapshot, "my_func");
        assert!(!candidates.is_empty());
        assert!(candidates.contains(&node_id));
    }

    #[test]
    fn test_candidate_bucket_returns_empty_for_unknown() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let candidates = candidate_bucket_for_symbol(&snapshot, "nonexistent_xyz_123");
        assert!(candidates.is_empty());
    }

    // ========================================================================
    // find_similar_symbols tests
    // ========================================================================

    #[test]
    fn test_find_similar_symbols_finds_substring_match() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();

        for name in &["process_request", "process_response", "other_func"] {
            let nm = graph.strings_mut().intern(name).unwrap();
            let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
            let nid = graph.nodes_mut().alloc(entry).unwrap();
            graph
                .indices_mut()
                .add(nid, NodeKind::Function, nm, None, file_id);
        }

        let snapshot = graph.snapshot();
        let suggestions = find_similar_symbols(&snapshot, "process", 5);
        // Should find process_request and process_response
        assert!(!suggestions.is_empty());
        assert!(suggestions.iter().any(|s| s.contains("process")));
    }

    #[test]
    fn test_find_similar_symbols_empty_graph_returns_empty() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let suggestions = find_similar_symbols(&snapshot, "anything", 5);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn test_find_similar_symbols_truncated_to_max() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        for i in 0..20 {
            let name = format!("foo_func_{i}");
            let nm = graph.strings_mut().intern(&name).unwrap();
            let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
            let nid = graph.nodes_mut().alloc(entry).unwrap();
            graph
                .indices_mut()
                .add(nid, NodeKind::Function, nm, None, file_id);
        }

        let snapshot = graph.snapshot();
        let suggestions = find_similar_symbols(&snapshot, "foo_func", 3);
        assert!(suggestions.len() <= 3);
    }

    // ========================================================================
    // decorate_single_symbol_lookup_error tests
    // ========================================================================

    #[test]
    fn test_decorate_error_for_not_found_adds_hints() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let err = anyhow::anyhow!("Symbol 'xyz' not found in graph.");
        let decorated = decorate_single_symbol_lookup_error(&snapshot, "xyz", err);
        let msg = decorated.to_string();
        assert!(msg.contains("not found in graph"));
        assert!(msg.contains("Hints:") || msg.contains("Use the full qualified name"));
    }

    #[test]
    fn test_decorate_error_for_other_error_unchanged() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let err = anyhow::anyhow!("Some other error.");
        let decorated = decorate_single_symbol_lookup_error(&snapshot, "xyz", err);
        assert_eq!(decorated.to_string(), "Some other error.");
    }

    // ========================================================================
    // build_unused_symbol_data tests
    // ========================================================================

    #[test]
    fn test_build_unused_symbol_data_basic() {
        let mut graph = CodeGraph::new();
        let ws = Path::new("/workspace");
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("orphan_fn").unwrap();
        let vis_pub = graph.strings_mut().intern("public").unwrap();
        let mut entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        entry.visibility = Some(vis_pub);
        let node_id = graph.nodes_mut().alloc(entry.clone()).unwrap();
        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        let files = snapshot.files();

        let data = build_unused_symbol_data(&entry, node_id, &graph, strings, files, ws);
        assert_eq!(data.name, "orphan_fn");
        assert_eq!(data.kind, "function");
        assert!(!data.file_uri.is_empty());
    }

    // ========================================================================
    // Entry-point classification tests removed in DB16: classification now
    // lives in sqry-db (`sqry_db::queries::EntryPointsQuery`) and is tested
    // there.
    // ========================================================================

    // ========================================================================
    // resolve_global_symbol_strict error paths
    // ========================================================================

    #[test]
    fn test_resolve_global_symbol_strict_not_found() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let result = resolve_global_symbol_strict(&snapshot, "nonexistent_symbol_xyz", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_resolve_global_symbol_strict_found() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("unique_function_xyz").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, nm, None, file_id);

        let snapshot = graph.snapshot();
        let result = resolve_global_symbol_strict(&snapshot, "unique_function_xyz", None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), node_id);
    }

    // ========================================================================
    // resolve_global_symbol_strict: file_path disambiguation (DISAMBIG_1)
    // ========================================================================

    /// Build a graph with two files each containing a function with the same
    /// simple name, so that simple-name lookup is ambiguous.
    fn make_ambiguous_two_file_graph() -> (
        CodeGraph,
        sqry_core::graph::unified::file::id::FileId,
        sqry_core::graph::unified::file::id::FileId,
    ) {
        let mut graph = CodeGraph::new();
        let file_a = graph
            .files_mut()
            .register(Path::new("src/module_a.rs"))
            .unwrap();
        let file_b = graph
            .files_mut()
            .register(Path::new("src/module_b.rs"))
            .unwrap();

        let nm = graph.strings_mut().intern("shared_fn").unwrap();

        // Node in file_a at line 10
        let mut entry_a = NodeEntry::new(NodeKind::Function, nm, file_a);
        entry_a.start_line = 10;
        let node_a = graph.nodes_mut().alloc(entry_a).unwrap();
        graph
            .indices_mut()
            .add(node_a, NodeKind::Function, nm, None, file_a);

        // Node in file_b at line 20
        let mut entry_b = NodeEntry::new(NodeKind::Function, nm, file_b);
        entry_b.start_line = 20;
        let node_b = graph.nodes_mut().alloc(entry_b).unwrap();
        graph
            .indices_mut()
            .add(node_b, NodeKind::Function, nm, None, file_b);

        (graph, file_a, file_b)
    }

    /// Build a graph with four files each containing `shared_fn` so the global
    /// ambiguity error can be verified to cap sample output at 3 candidates.
    fn make_ambiguous_four_file_graph() -> CodeGraph {
        let mut graph = CodeGraph::new();
        let paths = [
            "src/module_a.rs",
            "src/module_b.rs",
            "src/module_c.rs",
            "src/module_d.rs",
        ];
        let nm = graph.strings_mut().intern("shared_fn").unwrap();
        for (i, path) in paths.iter().enumerate() {
            let file_id = graph.files_mut().register(Path::new(path)).unwrap();
            let mut entry = NodeEntry::new(NodeKind::Function, nm, file_id);
            entry.start_line = (i as u32 + 1) * 10;
            let node_id = graph.nodes_mut().alloc(entry).unwrap();
            graph
                .indices_mut()
                .add(node_id, NodeKind::Function, nm, None, file_id);
        }
        graph
    }

    /// Build a graph with one file containing two functions with the same simple
    /// name (different qualified names) so that a file-scoped lookup is still
    /// ambiguous within that file.
    fn make_same_file_two_candidate_graph()
    -> (CodeGraph, sqry_core::graph::unified::file::id::FileId) {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("src/module_x.rs"))
            .unwrap();

        let nm = graph.strings_mut().intern("shared_fn").unwrap();
        let qn_a = graph
            .strings_mut()
            .intern("module_x::ImplA::shared_fn")
            .unwrap();
        let qn_b = graph
            .strings_mut()
            .intern("module_x::ImplB::shared_fn")
            .unwrap();

        // First candidate at line 5
        let mut entry_a = NodeEntry::new(NodeKind::Function, nm, file_id);
        entry_a.start_line = 5;
        entry_a.qualified_name = Some(qn_a);
        let node_a = graph.nodes_mut().alloc(entry_a).unwrap();
        graph
            .indices_mut()
            .add(node_a, NodeKind::Function, nm, Some(qn_a), file_id);

        // Second candidate at line 25
        let mut entry_b = NodeEntry::new(NodeKind::Function, nm, file_id);
        entry_b.start_line = 25;
        entry_b.qualified_name = Some(qn_b);
        let node_b = graph.nodes_mut().alloc(entry_b).unwrap();
        graph
            .indices_mut()
            .add(node_b, NodeKind::Function, nm, Some(qn_b), file_id);

        (graph, file_id)
    }

    #[test]
    fn test_resolve_global_symbol_strict_file_path_narrows_to_one() {
        let (graph, _file_a, _file_b) = make_ambiguous_two_file_graph();
        let snapshot = graph.snapshot();

        // Resolving without file_path should fail due to ambiguity
        let result_ambiguous = resolve_global_symbol_strict(&snapshot, "shared_fn", None);
        assert!(result_ambiguous.is_err());
        let err_msg = result_ambiguous.unwrap_err().to_string();
        assert!(
            err_msg.contains("ambiguous"),
            "Expected ambiguous error, got: {err_msg}"
        );

        // Resolving with a file_path pointing to src/module_a.rs should succeed
        let path_a = Path::new("src/module_a.rs");
        let result = resolve_global_symbol_strict(&snapshot, "shared_fn", Some(path_a));
        assert!(
            result.is_ok(),
            "Expected Ok with file_path, got: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_resolve_global_symbol_strict_file_path_zero_candidates() {
        let (graph, _file_a, _file_b) = make_ambiguous_two_file_graph();
        let snapshot = graph.snapshot();

        // Resolving with a file_path to a file not indexed in the graph
        let nonexistent_path = Path::new("src/nonexistent.rs");
        let result = resolve_global_symbol_strict(&snapshot, "shared_fn", Some(nonexistent_path));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        // FileNotIndexed or NotFound -> "No definition of 'X' found in file 'Y'"
        assert!(
            err_msg.contains("No definition of") || err_msg.contains("not found"),
            "Expected no-definition error, got: {err_msg}"
        );
    }

    #[test]
    fn test_resolve_global_symbol_strict_ambiguity_error_includes_samples() {
        // C_AMBIGUOUS migration: the message body is now the canonical
        // `sqry::ambiguous_symbol` JSON envelope (`code`, `message`,
        // `candidates[]`, `truncated`). Legacy "Try one of:" / "  - "
        // bullet text was retired alongside `build_global_ambiguity_error`.
        let (graph, _file_a, _file_b) = make_ambiguous_two_file_graph();
        let snapshot = graph.snapshot();

        let result = resolve_global_symbol_strict(&snapshot, "shared_fn", None);
        let err = result.expect_err("ambiguous symbol must surface as Err");
        let err_msg = err.to_string();

        let envelope: serde_json::Value =
            serde_json::from_str(&err_msg).expect("envelope must be valid JSON");
        let error_obj = envelope.get("error").expect("envelope wraps `error`");
        assert_eq!(error_obj["code"], "sqry::ambiguous_symbol");
        let msg = error_obj["message"].as_str().expect("message is string");
        assert!(msg.contains("shared_fn"));
        assert!(msg.contains("ambiguous"));
        assert_eq!(error_obj["truncated"], serde_json::Value::Bool(false));

        let candidates = error_obj["candidates"]
            .as_array()
            .expect("candidates[] required");
        assert_eq!(candidates.len(), 2);

        // Both candidate file paths and lines are present in the typed
        // payload (no longer in the freeform message text).
        let combined: Vec<(String, u64)> = candidates
            .iter()
            .map(|c| {
                (
                    c["file_path"].as_str().unwrap().to_string(),
                    c["start_line"].as_u64().unwrap(),
                )
            })
            .collect();
        assert!(
            combined
                .iter()
                .any(|(p, l)| p.contains("module_a.rs") && *l == 10),
            "expected module_a.rs:10 candidate, got {combined:?}"
        );
        assert!(
            combined
                .iter()
                .any(|(p, l)| p.contains("module_b.rs") && *l == 20),
            "expected module_b.rs:20 candidate, got {combined:?}"
        );
    }

    #[test]
    fn test_resolve_global_symbol_strict_ambiguity_error_caps_samples_at_three() {
        // C_AMBIGUOUS migration: candidate cap is now
        // `AMBIGUOUS_SYMBOL_CANDIDATE_CAP` (20), not 3 — the legacy 3-sample
        // truncation was bound to the freeform message text. With 4
        // candidates the typed envelope now lists all 4 candidates and
        // does NOT mark `truncated`.
        let graph = make_ambiguous_four_file_graph();
        let snapshot = graph.snapshot();

        let result = resolve_global_symbol_strict(&snapshot, "shared_fn", None);
        let err = result.expect_err("ambiguous symbol must surface as Err");
        let err_msg = err.to_string();

        let envelope: serde_json::Value =
            serde_json::from_str(&err_msg).expect("envelope must be valid JSON");
        let error_obj = envelope.get("error").expect("envelope wraps `error`");
        let candidates = error_obj["candidates"]
            .as_array()
            .expect("candidates[] required");
        assert_eq!(candidates.len(), 4, "all 4 candidates must surface");
        assert_eq!(
            error_obj["truncated"],
            serde_json::Value::Bool(false),
            "4 < cap of 20, truncated stays false"
        );
    }

    #[test]
    fn test_resolve_global_symbol_strict_file_path_not_found_error_mentions_file() {
        let (graph, _file_a, _file_b) = make_ambiguous_two_file_graph();
        let snapshot = graph.snapshot();

        let missing_path = Path::new("kernel/rcu/tree.c");
        let result = resolve_global_symbol_strict(&snapshot, "shared_fn", Some(missing_path));
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        // The error message must mention the file name for usability
        assert!(
            err_msg.contains("tree.c") || err_msg.contains("kernel"),
            "Expected file name in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_resolve_global_symbol_strict_same_file_ambiguity_lists_candidates() {
        // C_AMBIGUOUS migration: same-file ambiguity now surfaces the
        // canonical envelope (no longer a bespoke "Try a more qualified
        // name" string).
        let (graph, _file_id) = make_same_file_two_candidate_graph();
        let snapshot = graph.snapshot();

        let file_path = Path::new("src/module_x.rs");
        let result = resolve_global_symbol_strict(&snapshot, "shared_fn", Some(file_path));
        let err = result.expect_err("ambiguous symbol must surface as Err");
        let err_msg = err.to_string();

        let envelope: serde_json::Value =
            serde_json::from_str(&err_msg).expect("envelope must be valid JSON");
        let error_obj = envelope.get("error").expect("envelope wraps `error`");
        assert_eq!(error_obj["code"], "sqry::ambiguous_symbol");

        let candidates = error_obj["candidates"]
            .as_array()
            .expect("candidates[] required");
        let qnames: Vec<&str> = candidates
            .iter()
            .map(|c| c["qualified_name"].as_str().unwrap())
            .collect();
        assert!(
            qnames.iter().any(|q| q.ends_with("ImplA::shared_fn")),
            "expected qualified name ending in ImplA::shared_fn in {qnames:?}"
        );
        assert!(
            qnames.iter().any(|q| q.ends_with("ImplB::shared_fn")),
            "expected qualified name ending in ImplB::shared_fn in {qnames:?}"
        );
        // The file_path is reflected in every candidate's `file_path`.
        for cand in candidates {
            assert!(
                cand["file_path"].as_str().unwrap().contains("module_x.rs"),
                "every candidate must include the matched file path"
            );
        }
    }

    // ========================================================================
    // collect_impacted_callers_bfs tests
    // ========================================================================

    #[test]
    fn test_collect_impacted_callers_bfs_direct_caller() {
        use crate::tools::{DependencyImpactArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm_a = graph.strings_mut().intern("caller_of_target").unwrap();
        let nm_b = graph.strings_mut().intern("target_fn").unwrap();
        let node_a = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, nm_a, file_id))
            .unwrap();
        let node_b = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, nm_b, file_id))
            .unwrap();
        graph
            .indices_mut()
            .add(node_a, NodeKind::Function, nm_a, None, file_id);
        graph
            .indices_mut()
            .add(node_b, NodeKind::Function, nm_b, None, file_id);

        let call_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        graph
            .edges_mut()
            .add_edge(node_a, node_b, call_kind, file_id);

        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");
        let args = DependencyImpactArgs {
            symbol: "target_fn".to_string(),
            path: ".".to_string(),
            max_depth: 2,
            max_results: 100,
            include_indirect: false,
            include_files: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
            file_path: None,
        };

        let (impacted, _) = collect_impacted_callers_bfs(&snapshot, &graph, node_b, &args, ws);
        assert_eq!(impacted.len(), 1);
        assert_eq!(impacted[0].symbol.name, "caller_of_target");
        assert_eq!(impacted[0].impact_type, "caller");
    }

    #[test]
    fn test_collect_impacted_callers_bfs_include_files() {
        use crate::tools::{DependencyImpactArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm_a = graph.strings_mut().intern("caller_with_files").unwrap();
        let nm_b = graph.strings_mut().intern("target_with_files").unwrap();
        let node_a = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, nm_a, file_id))
            .unwrap();
        let node_b = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, nm_b, file_id))
            .unwrap();
        graph
            .indices_mut()
            .add(node_a, NodeKind::Function, nm_a, None, file_id);
        graph
            .indices_mut()
            .add(node_b, NodeKind::Function, nm_b, None, file_id);

        let call_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        graph
            .edges_mut()
            .add_edge(node_a, node_b, call_kind, file_id);

        let snapshot = graph.snapshot();
        let ws = Path::new("/workspace");
        let args = DependencyImpactArgs {
            symbol: "target_with_files".to_string(),
            path: ".".to_string(),
            max_depth: 1,
            max_results: 100,
            include_indirect: false,
            include_files: true,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
            file_path: None,
        };

        let (impacted, affected_files) =
            collect_impacted_callers_bfs(&snapshot, &graph, node_b, &args, ws);
        assert_eq!(impacted.len(), 1);
        assert!(!affected_files.is_empty());
    }

    // ========================================================================
    // should_include_in_unused_results tests
    // ========================================================================

    #[test]
    fn test_should_include_in_unused_results_passes_all_scope() {
        use crate::tools::{FindUnusedArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("some_fn").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let _ = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        let files = snapshot.files();

        let args = FindUnusedArgs {
            path: ".".to_string(),
            scope: UnusedScope::All,
            languages: vec![],
            kinds: vec![],
            max_results: 100,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
        };

        assert!(should_include_in_unused_results(
            &entry, &args, strings, files
        ));
    }

    #[test]
    fn test_should_include_in_unused_results_fails_language_filter() {
        use crate::tools::{FindUnusedArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("some_fn").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let _ = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        let files = snapshot.files();

        // Filter for Python, but file has no language (registered without language)
        let args = FindUnusedArgs {
            path: ".".to_string(),
            scope: UnusedScope::All,
            languages: vec!["python".to_string()],
            kinds: vec![],
            max_results: 100,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
        };

        assert!(!should_include_in_unused_results(
            &entry, &args, strings, files
        ));
    }

    #[test]
    fn test_should_include_in_unused_results_fails_kind_filter() {
        use crate::tools::{FindUnusedArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("my_struct").unwrap();
        let entry = NodeEntry::new(NodeKind::Struct, nm, file_id);
        let _ = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        let files = snapshot.files();

        let args = FindUnusedArgs {
            path: ".".to_string(),
            scope: UnusedScope::All,
            languages: vec![],
            kinds: vec!["function".to_string()], // Only functions, but this is a struct
            max_results: 100,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
        };

        assert!(!should_include_in_unused_results(
            &entry, &args, strings, files
        ));
    }

    // ========================================================================
    // process_caller_node tests
    // ========================================================================

    #[test]
    fn test_process_caller_node_builds_impacted_symbol() {
        use crate::tools::{DependencyImpactArgs, PaginationArgs};
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm = graph.strings_mut().intern("caller_fn").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let strings = snapshot.strings();
        let files = snapshot.files();
        let ws = Path::new("/workspace");

        let args = DependencyImpactArgs {
            symbol: "caller_fn".to_string(),
            path: ".".to_string(),
            max_depth: 2,
            max_results: 100,
            include_indirect: false,
            include_files: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 100,
            },
            file_path: None,
        };

        let (symbol, _file_uri) =
            process_caller_node(&entry, node_id, &graph, strings, files, ws, 0, &args);
        assert_eq!(symbol.symbol.name, "caller_fn");
        assert_eq!(symbol.impact_type, "caller");
        assert_eq!(symbol.depth, 1); // depth+1
    }

    // ========================================================================
    // PARSE_2: hierarchical_search implicit AND integration test
    //
    // hierarchical_search calls executor.execute_on_graph(query, &search_root),
    // which goes through the same parse_query_ast → execute_evaluate_with
    // pipeline as execute_on_preloaded_graph.  This test proves that the
    // implicit AND parser fix (PARSE_1) is exercised on the shared parser path
    // that hierarchical_search uses — satisfying US-1's requirement that
    // "hierarchical_search benefits automatically (shares parser via
    // executor.execute_on_graph)".
    //
    // Before PARSE_1, `kind:function smb2_open` failed with an UnexpectedToken
    // error because parse_and() stopped after the first predicate when no
    // explicit AND token followed.  After PARSE_1, the bare word `smb2_open`
    // is treated as implicit AND with name~=/smb2_open/, producing
    // And([Condition(kind=function), Condition(name~=/smb2_open/)]).
    // ========================================================================

    #[test]
    fn hierarchical_search_implicit_and_kind_plus_bare_word_returns_results() {
        use sqry_core::graph::Language;
        use sqry_core::query::QueryExecutor;
        use std::sync::Arc;

        // Build a minimal in-memory graph containing a Function node named
        // `smb2_open`.  This mirrors the Linux kernel fixture referenced in
        // US-1 without requiring on-disk graph loading.
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register_with_language(Path::new("/workspace/fs/smb2.c"), Some(Language::C))
            .expect("register smb2.c");

        let nm = graph
            .strings_mut()
            .intern("smb2_open")
            .expect("intern smb2_open");
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let node_id = graph
            .nodes_mut()
            .alloc(entry)
            .expect("alloc smb2_open node");
        // Register in the auxiliary indices so kind-based index lookups
        // (used by some predicate optimizers) also find the node.
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, nm, None, file_id);

        let graph = Arc::new(graph);
        let executor = QueryExecutor::new();
        let workspace_root = Path::new("/workspace");

        // The implicit-AND query: bare word `smb2_open` is promoted to
        // name~=/smb2_open/ and combined with kind:function via implicit AND.
        // This is the same query string that hierarchical_search passes to
        // executor.execute_on_graph via normalized_query() → execute_evaluate_with().
        let results = executor
            .execute_on_preloaded_graph(
                Arc::clone(&graph),
                "kind:function smb2_open",
                workspace_root,
                None,
            )
            .expect("execute_on_preloaded_graph must succeed for implicit AND query");

        // The query must find exactly the smb2_open function node.
        assert_eq!(
            results.len(),
            1,
            "kind:function smb2_open via shared executor parser must return the smb2_open node"
        );

        let matched_name = results
            .iter()
            .next()
            .and_then(|m| m.name())
            .map(|n| n.to_string())
            .unwrap_or_default();
        assert_eq!(
            matched_name, "smb2_open",
            "matched node name must be smb2_open"
        );
    }
}
