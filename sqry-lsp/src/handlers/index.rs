use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// Write performance log entry to file for debugging LSP performance issues.
///
/// Gated behind the `SQRY_LSP_PERF_LOG` environment variable. Only enabled
/// when set to "1", "true", or "yes" (case-insensitive). Setting to "0" or
/// any other value disables logging. When enabled, writes to
/// `$XDG_DATA_HOME/sqry/lsp-perf.log` (or `~/.local/share/sqry/lsp-perf.log`).
fn perf_log(msg: &str) {
    // Only log when explicitly enabled with truthy value
    let enabled = std::env::var("SQRY_LSP_PERF_LOG")
        .is_ok_and(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"));
    if !enabled {
        return;
    }

    // Use XDG_DATA_HOME or fallback to ~/.local/share
    let data_dir = std::env::var("XDG_DATA_HOME").map_or_else(
        |_| {
            std::env::var("HOME").map_or_else(
                |_| std::path::PathBuf::from("/tmp"),
                |h| std::path::PathBuf::from(h).join(".local/share"),
            )
        },
        std::path::PathBuf::from,
    );
    let log_dir = data_dir.join("sqry");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("lsp-perf.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(file, "[{now}] {msg}");
    }
}
use sqry_core::graph::node::Language;
use sqry_core::graph::unified::NodeKind;
use sqry_core::graph::unified::build::BuildConfig;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_core::json_response::IndexStatus;
use sqry_core::progress::{IndexProgress, ProgressReporter, SharedReporter};
use sqry_plugin_registry::create_plugin_manager;

use crate::protocol::{
    CrossLanguageRelation, SortOrder, SqryCycle, SqryDuplicateGroup,
    SqryListCircularDependenciesParams, SqryListCircularDependenciesResult,
    SqryListCrossLanguageRelationsParams, SqryListCrossLanguageRelationsResult,
    SqryListDuplicateGroupsParams, SqryListDuplicateGroupsResult, SqryListFilesByLanguageParams,
    SqryListFilesByLanguageResult, SqryListFilesParams, SqryListFilesResult, SqryListSymbolsParams,
    SqryListSymbolsResult, SqryListUnusedSymbolsParams, SqryListUnusedSymbolsResult,
};
use crate::session::SessionManager;
use sqry_core::query::{CircularConfig, CircularType, find_all_cycles_graph};
use sqry_core::query::{UnusedScope, find_unused_nodes};

// ===== Graph-based statistics functions =====

/// Convert `NodeKind` to display string for statistics.
fn node_kind_to_string(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Class => "class",
        NodeKind::Interface => "interface",
        NodeKind::Trait => "trait",
        NodeKind::Module => "module",
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Type => "type",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::EnumVariant => "enum_variant",
        NodeKind::Macro => "macro",
        NodeKind::Parameter => "parameter",
        NodeKind::Property => "property",
        NodeKind::CallSite => "call_site",
        NodeKind::Import => "import",
        NodeKind::Export => "export",
        NodeKind::StyleRule => "style_rule",
        NodeKind::StyleAtRule => "style_at_rule",
        NodeKind::StyleVariable => "style_variable",
        NodeKind::Lifetime => "lifetime",
        NodeKind::Component => "component",
        NodeKind::Service => "service",
        NodeKind::Resource => "resource",
        NodeKind::Endpoint => "endpoint",
        NodeKind::Test => "test",
        NodeKind::Other => "other",
    }
}

/// Compute symbol counts grouped by `NodeKind` from the unified graph.
fn compute_symbol_counts_from_graph(graph: &CodeGraph) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (kind, count) in graph.indices().iter_kinds() {
        let kind_str = node_kind_to_string(kind).to_string();
        counts.insert(kind_str, count);
    }
    counts
}

/// Compute file counts grouped by language from the unified graph.
fn compute_file_counts_from_graph(graph: &CodeGraph) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (_file_id, _path, language) in graph.files().iter_with_language() {
        let lang_str = language.map_or_else(|| "unknown".to_string(), |l| l.to_string());
        *counts.entry(lang_str).or_insert(0) += 1;
    }
    counts
}

/// Compute cross-language relation counts from the unified graph.
///
/// Returns a map of "`source_lang→target_lang`" to count.
/// Note: This is a simplified implementation that returns an empty map.
/// Full cross-language edge iteration requires snapshot access.
fn compute_relation_counts_from_graph(_graph: &CodeGraph) -> HashMap<String, usize> {
    // Cross-language edge statistics require edge iteration which is expensive.
    // Return empty map for now - this can be computed from graph_stats if needed.
    HashMap::new()
}

/// Count total cross-language edges in the graph.
///
/// Note: This is a simplified implementation that returns 0.
/// Full cross-language edge counting requires snapshot access.
fn count_cross_language_edges(_graph: &CodeGraph) -> usize {
    // Cross-language edge counting requires edge iteration which is expensive.
    // Return 0 for now - the exact count can be computed from manifest if needed.
    0
}

/// Collect unique languages from the graph.
fn collect_languages_from_graph(graph: &CodeGraph) -> Vec<String> {
    let mut languages: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (_file_id, _path, language) in graph.files().iter_with_language() {
        if let Some(lang) = language {
            languages.insert(lang.to_string());
        }
    }
    let mut langs: Vec<String> = languages.into_iter().collect();
    langs.sort();
    langs
}

/// Check if a Language enum matches a user-provided language string.
///
/// Handles both short forms (js, ts, py) and long forms (javascript, typescript, python).
fn language_matches(lang: Language, query: &str) -> bool {
    let query = query.to_lowercase();
    let short = lang.to_string(); // e.g., "js", "ts", "py", "rust"

    // Direct match with short form
    if short == query {
        return true;
    }

    // Map long forms to Language variants

    match lang {
        Language::JavaScript => query == "javascript",
        Language::TypeScript => query == "typescript",
        Language::Python => query == "python",
        Language::Cpp => query == "c++" || query == "cplusplus",
        Language::CSharp => query == "c#" || query == "cs",
        Language::Html => query == "html5",
        Language::Shell => query == "bash" || query == "sh",
        // For others, the short form is the canonical name
        _ => false,
    }
}

/// Fetch the current index status for the requested path.
///
/// Uses the unified graph (`.sqry/graph/`) for status information.
///
/// # Errors
///
/// Returns an error when the session cannot resolve the path or when loading
/// the graph fails.
pub fn index_status(session: &SessionManager, path: Option<&str>) -> Result<IndexStatus> {
    let handler_start = Instant::now();
    perf_log(&format!("index_status START path={path:?}"));

    let target = session.resolve_path(path)?;
    let graph_storage = GraphStorage::new(&target);

    if !graph_storage.exists() {
        perf_log("index_status: graph does not exist");
        return Ok(IndexStatus::not_found());
    }

    // Use session's cached graph
    let load_start = Instant::now();
    let graph = session
        .graph_for_path(&target)?
        .ok_or_else(|| anyhow::anyhow!("graph exists but could not be loaded"))?;

    perf_log(&format!(
        "index_status graph load took {elapsed:?}",
        elapsed = load_start.elapsed()
    ));

    // Get stats from the graph
    let symbol_count = graph.node_count();
    let file_count = graph.files().len();
    let languages = collect_languages_from_graph(&graph);

    // Use manifest built_at timestamp instead of file metadata.
    // On Linux, metadata.created() returns the inode birth time which does not
    // update when the snapshot file is overwritten, causing stale age reports.
    let manifest = graph_storage
        .load_manifest()
        .context("failed to load graph manifest")?;
    let built_at = chrono::DateTime::parse_from_rfc3339(&manifest.built_at)
        .context("failed to parse manifest built_at timestamp")?;
    let now = chrono::Utc::now();
    let age_seconds = now
        .signed_duration_since(built_at.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0) as u64;
    let datetime = built_at.with_timezone(&chrono::Utc);

    // Compute grouped counts for tree view grouping
    let symbol_counts_by_kind = compute_symbol_counts_from_graph(&graph);
    let file_counts_by_language = compute_file_counts_from_graph(&graph);
    let relation_counts_by_pair = compute_relation_counts_from_graph(&graph);
    let cross_language_count = count_cross_language_edges(&graph);

    let builder = IndexStatus::from_index(
        graph_storage.snapshot_path().display().to_string(),
        datetime.to_rfc3339(),
        age_seconds,
    )
    .symbol_count(symbol_count)
    .file_count(file_count)
    .languages(languages)
    .has_relations(graph.edge_count() > 0)
    .has_trigram(false) // Unified graph doesn't use trigram index
    .cross_language_relation_count(cross_language_count)
    .symbol_counts_by_kind(symbol_counts_by_kind)
    .file_counts_by_language(file_counts_by_language)
    .relation_counts_by_pair(relation_counts_by_pair);

    perf_log(&format!(
        "index_status TOTAL took {elapsed:?}, symbols={symbol_count}, files={file_count}",
        elapsed = handler_start.elapsed()
    ));

    Ok(builder.build())
}

#[derive(Debug, Clone)]
pub struct RebuildSummary {
    pub total_symbols: usize,
    pub duration: Duration,
}

/// Rebuild the sqry unified graph on disk.
///
/// This function builds a unified `CodeGraph` from source files and persists it
/// to `.sqry/graph/snapshot.sqry`. It uses the same build pipeline as the CLI
/// `sqry index` command.
///
/// # Errors
///
/// Returns an error when graph building or persistence fails.
pub fn rebuild_index(
    session: &SessionManager,
    target: &Path,
    reporter: &SharedReporter,
    _force: bool,
) -> Result<RebuildSummary> {
    use sqry_core::graph::unified::build::build_and_persist_graph_with_progress;

    let start = Instant::now();

    let plugins = create_plugin_manager();
    let build_config = BuildConfig {
        label_budget: sqry_core::graph::unified::analysis::resolve_label_budget_config(
            target, None, None, None, false,
        )?,
        ..BuildConfig::default()
    };

    let (graph, _build_result) = build_and_persist_graph_with_progress(
        target,
        &plugins,
        &build_config,
        "lsp:rebuild_index",
        reporter.clone(),
    )
    .context("Failed to build and persist unified graph")?;

    // Clear the graph cache so it reloads the newly built graph
    session.clear_graph_cache();

    let node_count = graph.node_count();

    Ok(RebuildSummary {
        total_symbols: node_count,
        duration: start.elapsed(),
    })
}

pub struct ChannelProgressReporter {
    sender: tokio::sync::mpsc::UnboundedSender<IndexProgress>,
}

impl ChannelProgressReporter {
    #[must_use]
    pub fn new(sender: tokio::sync::mpsc::UnboundedSender<IndexProgress>) -> Self {
        Self { sender }
    }
}

impl ProgressReporter for ChannelProgressReporter {
    fn report(&self, event: IndexProgress) {
        let _ = self.sender.send(event);
    }
}

/// Default limit for list operations.
const DEFAULT_LIST_LIMIT: usize = 100;

/// Maximum limit for list operations.
const MAX_LIST_LIMIT: usize = 1000;

/// List indexed files with pagination support.
///
/// Returns a paginated list of file paths from the unified graph. Files are
/// sorted alphabetically for consistent ordering.
///
/// # Errors
///
/// Returns an error when the session cannot resolve the path or when loading
/// the graph fails.
pub fn list_files(
    session: &SessionManager,
    params: &SqryListFilesParams,
) -> Result<SqryListFilesResult> {
    let handler_start = Instant::now();
    perf_log("list_files START");

    let target = session.resolve_path(params.path.as_deref())?;

    // Use session's cached graph to avoid disk I/O on every request
    let graph_start = Instant::now();
    let Some(graph) = session.graph_for_path(&target)? else {
        perf_log("list_files no graph found, returning empty");
        return Ok(SqryListFilesResult {
            files: vec![],
            total: 0,
            offset: 0,
            limit: params
                .limit
                .unwrap_or(DEFAULT_LIST_LIMIT)
                .min(MAX_LIST_LIMIT),
            has_more: false,
        });
    };
    perf_log(&format!(
        "list_files graph_for_path took {elapsed:?}",
        elapsed = graph_start.elapsed()
    ));

    // Collect and sort file paths from the graph
    let mut files: Vec<String> = graph
        .files()
        .iter()
        .map(|(_file_id, path)| path.display().to_string())
        .collect();
    files.sort();

    let total = files.len();
    let offset = params.offset.unwrap_or(0);
    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .min(MAX_LIST_LIMIT);

    // Apply pagination
    let paginated: Vec<String> = files.into_iter().skip(offset).take(limit).collect();

    let has_more = offset + paginated.len() < total;

    perf_log(&format!(
        "list_files TOTAL took {elapsed:?}, returning {returned} of {total} files",
        elapsed = handler_start.elapsed(),
        returned = paginated.len()
    ));

    Ok(SqryListFilesResult {
        files: paginated,
        total,
        offset,
        limit,
        has_more,
    })
}

/// Parse a kind string to `NodeKind`.
fn parse_node_kind(kind_str: &str) -> Option<NodeKind> {
    match kind_str.to_lowercase().as_str() {
        "function" => Some(NodeKind::Function),
        "method" => Some(NodeKind::Method),
        "class" => Some(NodeKind::Class),
        "interface" => Some(NodeKind::Interface),
        "trait" => Some(NodeKind::Trait),
        "module" => Some(NodeKind::Module),
        "variable" => Some(NodeKind::Variable),
        "constant" => Some(NodeKind::Constant),
        "type" => Some(NodeKind::Type),
        "struct" => Some(NodeKind::Struct),
        "enum" => Some(NodeKind::Enum),
        "enum_variant" | "enumvariant" => Some(NodeKind::EnumVariant),
        "macro" => Some(NodeKind::Macro),
        "parameter" => Some(NodeKind::Parameter),
        "property" => Some(NodeKind::Property),
        "call_site" | "callsite" => Some(NodeKind::CallSite),
        "import" => Some(NodeKind::Import),
        "export" => Some(NodeKind::Export),
        "style_rule" | "stylerule" => Some(NodeKind::StyleRule),
        "style_at_rule" | "styleatrule" => Some(NodeKind::StyleAtRule),
        "style_variable" | "stylevariable" => Some(NodeKind::StyleVariable),
        "lifetime" => Some(NodeKind::Lifetime),
        "component" => Some(NodeKind::Component),
        "service" => Some(NodeKind::Service),
        "resource" => Some(NodeKind::Resource),
        "endpoint" => Some(NodeKind::Endpoint),
        "test" => Some(NodeKind::Test),
        "other" => Some(NodeKind::Other),
        _ => None,
    }
}

/// List indexed symbols with pagination support.
///
/// Returns a paginated list of symbols from the unified graph. Symbols are
/// sorted by name for consistent ordering.
///
/// # Errors
///
/// Returns an error when the session cannot resolve the path or when loading
/// the graph fails.
pub fn list_symbols(
    session: &SessionManager,
    params: &SqryListSymbolsParams,
) -> Result<SqryListSymbolsResult> {
    let handler_start = Instant::now();
    perf_log(&format!(
        "list_symbols START kind={kind:?}",
        kind = &params.kind
    ));

    let resolve_start = Instant::now();
    let target = session.resolve_path(params.path.as_deref())?;
    perf_log(&format!(
        "list_symbols resolve_path took {elapsed:?}",
        elapsed = resolve_start.elapsed()
    ));

    // Use session's cached graph to avoid disk I/O on every request
    let graph_start = Instant::now();
    let Some(graph) = session.graph_for_path(&target)? else {
        perf_log("list_symbols no graph found, returning empty");
        return Ok(SqryListSymbolsResult {
            symbols: vec![],
            total: 0,
            offset: 0,
            limit: params
                .limit
                .unwrap_or(DEFAULT_LIST_LIMIT)
                .min(MAX_LIST_LIMIT),
            has_more: false,
        });
    };
    perf_log(&format!(
        "list_symbols graph_for_path took {elapsed:?}",
        elapsed = graph_start.elapsed()
    ));

    // Parse kind filter if specified
    let kind_filter: Option<NodeKind> = if let Some(kind_str) = &params.kind {
        let kind = parse_node_kind(kind_str)
            .ok_or_else(|| anyhow::anyhow!("Invalid symbol kind: {kind_str}"))?;
        Some(kind)
    } else {
        None
    };

    // Collect and filter nodes from the graph
    let query_start = Instant::now();
    let nodes: Vec<_> = graph
        .nodes()
        .iter()
        .filter(|(_id, entry)| {
            // Filter by kind if specified
            kind_filter.is_none_or(|k| entry.kind == k)
        })
        .collect();
    let raw_count = nodes.len();
    perf_log(&format!(
        "list_symbols node iteration took {elapsed:?}, found {raw_count} nodes",
        elapsed = query_start.elapsed()
    ));

    // Convert nodes to search items with names for sorting
    let convert_start = Instant::now();
    let mut symbols: Vec<_> = nodes
        .into_iter()
        .filter_map(|(node_id, entry)| {
            let name = graph.strings().resolve(entry.name)?.to_string();
            crate::conversion::node_to_search_item(node_id, entry, &graph, &target)
                .map(|item| (name, item))
        })
        .collect();
    perf_log(&format!(
        "list_symbols symbol conversion took {elapsed:?} for {count} symbols",
        elapsed = convert_start.elapsed(),
        count = symbols.len()
    ));

    // Sort by name for consistent ordering
    let sort_start = Instant::now();
    symbols.sort_by(|a, b| a.0.cmp(&b.0));
    perf_log(&format!(
        "list_symbols sort took {elapsed:?}",
        elapsed = sort_start.elapsed()
    ));

    let total = symbols.len();
    let offset = params.offset.unwrap_or(0);
    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .min(MAX_LIST_LIMIT);

    // Apply pagination
    let paginated = symbols
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(_, item)| item)
        .collect::<Vec<_>>();

    let has_more = offset + paginated.len() < total;

    perf_log(&format!(
        "list_symbols TOTAL took {elapsed:?}, returning {returned} of {total} symbols",
        elapsed = handler_start.elapsed(),
        returned = paginated.len()
    ));

    Ok(SqryListSymbolsResult {
        symbols: paginated,
        total,
        offset,
        limit,
        has_more,
    })
}

/// List indexed files filtered by language with pagination support.
///
/// Returns a paginated list of file paths that contain symbols of the specified language.
/// Files are sorted alphabetically for consistent ordering.
///
/// # Errors
///
/// Returns an error when the session cannot resolve the path or when loading
/// the index fails.
pub fn list_files_by_language(
    session: &SessionManager,
    params: SqryListFilesByLanguageParams,
) -> Result<SqryListFilesByLanguageResult> {
    let target = session.resolve_path(params.path.as_deref())?;
    let storage = GraphStorage::new(&target);
    let query_lang = params.language.to_lowercase();
    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .min(MAX_LIST_LIMIT);
    let offset = params.offset.unwrap_or(0);

    if !storage.exists() {
        return Ok(SqryListFilesByLanguageResult {
            language: params.language,
            files: vec![],
            total: 0,
            offset,
            limit,
            has_more: false,
        });
    }

    let graph = session
        .graph_for_path(&target)?
        .ok_or_else(|| anyhow::anyhow!("graph exists but could not be loaded"))?;

    // Collect files matching the requested language
    let mut files: Vec<String> = graph
        .files()
        .iter_with_language()
        .filter_map(|(_file_id, path, lang)| {
            lang.filter(|l| language_matches(*l, &query_lang))
                .map(|_| path.display().to_string())
        })
        .collect();
    files.sort();

    let total = files.len();

    // Apply pagination
    let paginated: Vec<String> = files.into_iter().skip(offset).take(limit).collect();
    let has_more = offset + paginated.len() < total;

    Ok(SqryListFilesByLanguageResult {
        language: params.language,
        files: paginated,
        total,
        offset,
        limit,
        has_more,
    })
}

/// List cross-language relations with pagination support.
///
/// Finds relations (imports, calls) where the source and target are in different languages.
/// Uses pre-built `HashMap`s for O(1) symbol lookups instead of O(N) scans.
///
/// # Errors
///
/// Returns an error when the session cannot resolve the path or when loading
/// the index fails.
#[allow(clippy::too_many_lines)] // Exhaustive relation filtering and scoring kept in one handler.
pub fn list_cross_language_relations(
    session: &SessionManager,
    params: &SqryListCrossLanguageRelationsParams,
) -> Result<SqryListCrossLanguageRelationsResult> {
    use sqry_core::graph::unified::EdgeKind;

    let target = session.resolve_path(params.path.as_deref())?;
    let limit = clamp_list_limit(params.limit);

    // Use unified graph instead of legacy index
    let Some(graph) = session.graph_for_path(&target)? else {
        return Ok(empty_cross_language_relations(limit));
    };

    // Collect cross-language relations from graph edges
    let mut relations = Vec::new();

    // Create snapshot for edge iteration
    let snapshot = graph.snapshot();

    // Iterate over all edges and find cross-language ones
    // iter_edges returns (source_id, target_id, edge_kind) tuples
    for (source_id, target_id, edge_kind) in snapshot.iter_edges() {
        // Get source and target nodes
        let Some(source_entry) = snapshot.nodes().get(source_id) else {
            continue;
        };
        let Some(target_entry) = snapshot.nodes().get(target_id) else {
            continue;
        };

        // Get languages for source and target files
        let source_lang = snapshot
            .files()
            .language_for_file(source_entry.file)
            .map(|l| l.to_string());
        let target_lang = snapshot
            .files()
            .language_for_file(target_entry.file)
            .map(|l| l.to_string());

        // Only include if languages differ
        let (Some(from_language), Some(to_language)) = (source_lang, target_lang) else {
            continue;
        };
        if !languages_differ(&from_language, &to_language) {
            continue;
        }

        // Determine relation type from edge kind
        let relation_type = match edge_kind {
            EdgeKind::Imports { .. } => "import",
            EdgeKind::Calls { .. } => "call",
            EdgeKind::FfiCall { .. } => "ffi",
            EdgeKind::HttpRequest { .. } => "http",
            EdgeKind::GrpcCall { .. } => "grpc",
            EdgeKind::WebAssemblyCall => "wasm",
            EdgeKind::MessageQueue { .. } => "mq",
            EdgeKind::ProtocolCall { .. } => "protocol",
            _ => continue, // Skip non-cross-language edge types
        };

        // Resolve symbol names
        let from_symbol = snapshot
            .strings()
            .resolve(source_entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let to_symbol = snapshot
            .strings()
            .resolve(target_entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();

        // Resolve file paths
        let from_file = snapshot
            .files()
            .resolve(source_entry.file)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let to_file = snapshot
            .files()
            .resolve(target_entry.file)
            .map(|p| p.display().to_string());

        relations.push(CrossLanguageRelation {
            relation_type: relation_type.to_string(),
            from_symbol,
            from_language,
            from_file,
            to_symbol,
            to_language,
            to_file,
        });
    }

    let mut relations = apply_language_filter(relations, params);
    sort_relations(&mut relations, params.sort_order);

    let total = relations.len();
    let offset = params.offset.unwrap_or(0);
    let paginated: Vec<CrossLanguageRelation> =
        relations.into_iter().skip(offset).take(limit).collect();
    let has_more = offset + paginated.len() < total;

    Ok(SqryListCrossLanguageRelationsResult {
        relations: paginated,
        total,
        offset,
        limit,
        has_more,
        overflow: None, // Overflow tracking not needed for graph-based implementation
    })
}

fn clamp_list_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_LIST_LIMIT).min(MAX_LIST_LIMIT)
}

fn empty_cross_language_relations(limit: usize) -> SqryListCrossLanguageRelationsResult {
    SqryListCrossLanguageRelationsResult {
        relations: vec![],
        total: 0,
        offset: 0,
        limit,
        has_more: false,
        overflow: None,
    }
}

fn languages_differ(left: &str, right: &str) -> bool {
    left.to_lowercase() != right.to_lowercase()
}

fn apply_language_filter(
    relations: Vec<CrossLanguageRelation>,
    params: &SqryListCrossLanguageRelationsParams,
) -> Vec<CrossLanguageRelation> {
    if params.source_language.is_none() && params.target_language.is_none() {
        return relations;
    }

    relations
        .into_iter()
        .filter(|rel| {
            let source_match = params
                .source_language
                .as_ref()
                .is_none_or(|src| rel.from_language.eq_ignore_ascii_case(src));
            let target_match = params
                .target_language
                .as_ref()
                .is_none_or(|tgt| rel.to_language.eq_ignore_ascii_case(tgt));
            source_match && target_match
        })
        .collect()
}

fn sort_relations(relations: &mut [CrossLanguageRelation], sort_order: SortOrder) {
    match sort_order {
        SortOrder::Alphabetical => sort_relations_alphabetical(relations),
        SortOrder::ByFrequency => sort_relations_by_frequency(relations),
        SortOrder::ByRelevance => sort_relations_by_relevance(relations),
    }
}

fn sort_relations_alphabetical(relations: &mut [CrossLanguageRelation]) {
    relations.sort_unstable_by(|a, b| {
        a.relation_type
            .cmp(&b.relation_type)
            .then_with(|| a.from_symbol.cmp(&b.from_symbol))
    });
}

fn sort_relations_by_frequency(relations: &mut [CrossLanguageRelation]) {
    let mut pair_counts: HashMap<(String, String), usize> =
        HashMap::with_capacity(relations.len() / 10 + 1);
    for rel in relations.iter() {
        *pair_counts
            .entry((rel.from_language.clone(), rel.to_language.clone()))
            .or_insert(0) += 1;
    }

    relations.sort_unstable_by(|a, b| {
        let a_freq = pair_counts
            .get(&(a.from_language.clone(), a.to_language.clone()))
            .copied()
            .unwrap_or(0);
        let b_freq = pair_counts
            .get(&(b.from_language.clone(), b.to_language.clone()))
            .copied()
            .unwrap_or(0);

        b_freq
            .cmp(&a_freq)
            .then_with(|| a.relation_type.cmp(&b.relation_type))
            .then_with(|| a.from_symbol.cmp(&b.from_symbol))
    });
}

fn sort_relations_by_relevance(relations: &mut [CrossLanguageRelation]) {
    let capacity = relations.len() / 10 + 1;
    let mut pair_counts: HashMap<(String, String), usize> = HashMap::with_capacity(capacity);
    let mut symbol_pair_counts: HashMap<(String, String), usize> =
        HashMap::with_capacity(relations.len());

    for rel in relations.iter() {
        *pair_counts
            .entry((rel.from_language.clone(), rel.to_language.clone()))
            .or_insert(0) += 1;
        *symbol_pair_counts
            .entry((rel.from_symbol.clone(), rel.to_symbol.clone()))
            .or_insert(0) += 1;
    }

    relations.sort_unstable_by(|a, b| {
        let (a_type_score, b_type_score) = relation_type_scores(&a.relation_type, &b.relation_type);
        let a_unique_score = relation_unique_score(&symbol_pair_counts, a);
        let b_unique_score = relation_unique_score(&symbol_pair_counts, b);
        let a_rarity_score = relation_rarity_score(&pair_counts, a);
        let b_rarity_score = relation_rarity_score(&pair_counts, b);

        let a_relevance = a_type_score + a_unique_score + a_rarity_score;
        let b_relevance = b_type_score + b_unique_score + b_rarity_score;

        b_relevance
            .cmp(&a_relevance)
            .then_with(|| a.relation_type.cmp(&b.relation_type))
            .then_with(|| a.from_symbol.cmp(&b.from_symbol))
    });
}

fn relation_type_scores(left: &str, right: &str) -> (usize, usize) {
    let left_score = if left == "call" { 100usize } else { 50 };
    let right_score = if right == "call" { 100usize } else { 50 };
    (left_score, right_score)
}

fn relation_unique_score(
    symbol_pair_counts: &HashMap<(String, String), usize>,
    relation: &CrossLanguageRelation,
) -> usize {
    let symbol_count = symbol_pair_counts
        .get(&(relation.from_symbol.clone(), relation.to_symbol.clone()))
        .copied()
        .unwrap_or(1);
    100usize / symbol_count.max(1)
}

fn relation_rarity_score(
    pair_counts: &HashMap<(String, String), usize>,
    relation: &CrossLanguageRelation,
) -> usize {
    let pair_count = pair_counts
        .get(&(relation.from_language.clone(), relation.to_language.clone()))
        .copied()
        .unwrap_or(1);
    50usize / pair_count.max(1)
}

// ===== CD Predicate Handlers =====

/// List duplicate symbol groups.
///
/// Groups symbols by body hash (for duplicates:body). Signature and struct types
/// are planned for future releases.
///
/// Uses the unified `CodeGraph` with `body_hash` for duplicate detection.
///
/// # Errors
///
/// Returns an error when the session cannot resolve the path or when loading
/// the graph fails.
pub fn list_duplicate_groups(
    session: &SessionManager,
    params: &SqryListDuplicateGroupsParams,
) -> Result<SqryListDuplicateGroupsResult> {
    use crate::conversion::node_to_search_item;
    use sqry_core::graph::unified::node::NodeId;
    use std::collections::HashMap;

    let handler_start = Instant::now();
    perf_log(&format!(
        "list_duplicate_groups START type={duplicate_type}",
        duplicate_type = params.duplicate_type.as_str()
    ));

    let target = session.resolve_path(params.path.as_deref())?;

    // Use unified graph instead of legacy index
    let Some(graph) = session.graph_for_path(&target)? else {
        perf_log("list_duplicate_groups no graph found, returning empty");
        return Ok(SqryListDuplicateGroupsResult {
            groups: vec![],
            total_groups: 0,
            total_symbols: 0,
            truncated: false,
        });
    };

    // Validate duplicate_type - currently only "body" is supported
    if params.duplicate_type.as_str() != "body" {
        return Err(anyhow::anyhow!(
            "Unsupported duplicate_type '{duplicate_type}'. Currently only 'body' is supported.",
            duplicate_type = params.duplicate_type.as_str()
        ));
    }

    // Group nodes by body hash (for body duplicates)
    // BodyHash128 stores high/low u64 values - use as_u128() for grouping
    let mut hash_groups: HashMap<u128, Vec<NodeId>> = HashMap::new();

    for (node_id, entry) in graph.nodes().iter() {
        if let Some(body_hash) = entry.body_hash {
            let hash_key = body_hash.as_u128();
            hash_groups.entry(hash_key).or_default().push(node_id);
        }
    }

    // Filter to groups with 2+ nodes (actual duplicates)
    let mut duplicate_groups: Vec<_> = hash_groups
        .into_iter()
        .filter(|(_, nodes)| nodes.len() >= 2)
        .collect();

    // Sort for deterministic ordering: descending by count, then by hash for tie-breaking
    duplicate_groups.sort_by(|(hash_a, nodes_a), (hash_b, nodes_b)| {
        nodes_b
            .len()
            .cmp(&nodes_a.len())
            .then_with(|| hash_a.cmp(hash_b))
    });

    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .min(MAX_LIST_LIMIT);
    let actual_total_groups = duplicate_groups.len();
    let actual_total_symbols: usize = duplicate_groups.iter().map(|(_, nodes)| nodes.len()).sum();
    let truncated = actual_total_groups > limit;

    // Convert to protocol types
    let mut result_groups: Vec<SqryDuplicateGroup> = Vec::new();

    for (hash, node_ids) in duplicate_groups.into_iter().take(limit) {
        let mut items: Vec<_> = node_ids
            .iter()
            .filter_map(|&node_id| {
                let entry = graph.nodes().get(node_id)?;
                node_to_search_item(node_id, entry, &graph, &target)
            })
            .collect();

        // Sort symbols within group by file URI and location for stable ordering
        items.sort_by(|a, b| {
            a.location
                .uri
                .as_str()
                .cmp(b.location.uri.as_str())
                .then_with(|| {
                    a.location
                        .range
                        .start
                        .line
                        .cmp(&b.location.range.start.line)
                })
                .then_with(|| {
                    a.location
                        .range
                        .start
                        .character
                        .cmp(&b.location.range.start.character)
                })
        });

        if items.is_empty() {
            continue;
        }

        let representative_name = items.first().map(|s| s.name.clone()).unwrap_or_default();

        // Generate stable group ID from full 128-bit hash
        let group_id = format!("{hash:032x}");

        result_groups.push(SqryDuplicateGroup {
            group_id,
            count: items.len(),
            representative_name,
            symbols: items,
        });
    }

    perf_log(&format!(
        "list_duplicate_groups TOTAL took {elapsed:?}, groups={groups}/{actual_total_groups}, symbols={actual_total_symbols}",
        elapsed = handler_start.elapsed(),
        groups = result_groups.len()
    ));

    Ok(SqryListDuplicateGroupsResult {
        total_groups: actual_total_groups,
        total_symbols: actual_total_symbols,
        truncated,
        groups: result_groups,
    })
}

/// List circular dependencies (cycles) in the codebase.
///
/// Finds cycles in call graphs, import graphs, or module-level dependencies
/// using Tarjan's strongly connected components algorithm.
///
/// # Errors
///
/// Returns an error when the session cannot resolve the path or when loading
/// the index fails.
pub fn list_circular_dependencies(
    session: &SessionManager,
    params: &SqryListCircularDependenciesParams,
) -> Result<SqryListCircularDependenciesResult> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let handler_start = Instant::now();
    perf_log(&format!(
        "list_circular_dependencies START type={circular_type}",
        circular_type = params.circular_type.as_str()
    ));

    let target = session.resolve_path(params.path.as_deref())?;

    let Some(graph) = session.graph_for_path(&target)? else {
        perf_log("list_circular_dependencies no graph found, returning empty");
        return Ok(SqryListCircularDependenciesResult {
            cycles: vec![],
            total_cycles: 0,
            truncated: false,
        });
    };

    // Parse circular type - strict validation
    let Some(circular_type) = CircularType::try_parse(&params.circular_type) else {
        return Err(anyhow::anyhow!(
            "Unsupported circular_type '{circular_type}'. Valid values: 'calls', 'imports', 'modules'.",
            circular_type = params.circular_type.as_str()
        ));
    };

    let config = CircularConfig {
        should_include_self_loops: params.should_include_self_loops,
        max_results: params
            .limit
            .unwrap_or(DEFAULT_LIST_LIMIT)
            .min(MAX_LIST_LIMIT),
        ..Default::default()
    };

    // Find cycles using graph-based algorithm
    let cycles = find_all_cycles_graph(circular_type, &graph, &config);

    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .min(MAX_LIST_LIMIT);
    let actual_total_cycles = cycles.len();
    let truncated = actual_total_cycles > limit;

    // Convert to protocol types
    let result_cycles: Vec<SqryCycle> = cycles
        .into_iter()
        .take(limit)
        .map(|members| {
            // Generate stable cycle ID from sorted members
            let mut sorted_members = members.clone();
            sorted_members.sort();
            let mut hasher = DefaultHasher::new();
            sorted_members.hash(&mut hasher);
            let cycle_hash = hasher.finish();
            let cycle_id = format!("{cycle_hash:016x}");

            SqryCycle {
                cycle_id,
                depth: members.len(),
                members,
                cycle_type: params.circular_type.clone(),
            }
        })
        .collect();

    perf_log(&format!(
        "list_circular_dependencies TOTAL took {elapsed:?}, cycles={cycles}/{actual_total_cycles}",
        elapsed = handler_start.elapsed(),
        cycles = result_cycles.len()
    ));

    Ok(SqryListCircularDependenciesResult {
        total_cycles: actual_total_cycles,
        truncated,
        cycles: result_cycles,
    })
}

/// List unused symbols.
///
/// Identifies symbols with no references from reachable code paths.
/// Uses reachability analysis from entry points (main, lib exports, tests).
///
/// Uses the unified `CodeGraph` for unused symbol detection.
///
/// # Errors
///
/// Returns an error when the session cannot resolve the path or when loading
/// the graph fails.
pub fn list_unused_symbols(
    session: &SessionManager,
    params: SqryListUnusedSymbolsParams,
) -> Result<SqryListUnusedSymbolsResult> {
    use crate::conversion::node_to_search_item;

    let handler_start = Instant::now();
    perf_log(&format!(
        "list_unused_symbols START scope={scope}",
        scope = params.scope
    ));

    let target = session.resolve_path(params.path.as_deref())?;

    // Use unified graph instead of legacy index
    let Some(graph) = session.graph_for_path(&target)? else {
        perf_log("list_unused_symbols no graph found, returning empty");
        return Ok(SqryListUnusedSymbolsResult {
            symbols: vec![],
            total: 0,
            truncated: false,
            scope: params.scope,
        });
    };

    // Parse scope - strict validation
    let Some(scope) = UnusedScope::try_parse(&params.scope) else {
        return Err(anyhow::anyhow!(
            "Unsupported scope '{scope}'. Valid values: 'public', 'private', 'function', 'struct', 'all'.",
            scope = params.scope
        ));
    };

    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .min(MAX_LIST_LIMIT);

    // Find unused nodes using graph-based analysis
    // find_unused_nodes computes reachable set internally
    let check_start = Instant::now();
    let unused_node_ids = find_unused_nodes(scope, &graph, limit + 1);
    perf_log(&format!(
        "list_unused_symbols find_unused_nodes took {elapsed:?}, found {found}",
        elapsed = check_start.elapsed(),
        found = unused_node_ids.len()
    ));

    // Convert to protocol types
    let mut unused_symbols: Vec<_> = unused_node_ids
        .iter()
        .filter_map(|&node_id| {
            let entry = graph.nodes().get(node_id)?;
            node_to_search_item(node_id, entry, &graph, &target)
        })
        .collect();

    let truncated = unused_symbols.len() > limit;
    unused_symbols.truncate(limit);

    perf_log(&format!(
        "list_unused_symbols TOTAL took {elapsed:?}, unused={count}",
        elapsed = handler_start.elapsed(),
        count = unused_symbols.len()
    ));

    Ok(SqryListUnusedSymbolsResult {
        total: unused_symbols.len(),
        truncated,
        scope: params.scope,
        symbols: unused_symbols,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================================
    // SortOrder tests
    // ==========================================================================

    #[test]
    fn test_sort_order_default() {
        let order: SortOrder = Default::default();
        assert_eq!(order, SortOrder::Alphabetical);
    }

    #[test]
    fn test_sort_order_debug() {
        assert_eq!(
            format!("{order:?}", order = SortOrder::Alphabetical),
            "Alphabetical"
        );
        assert_eq!(
            format!("{order:?}", order = SortOrder::ByFrequency),
            "ByFrequency"
        );
        assert_eq!(
            format!("{order:?}", order = SortOrder::ByRelevance),
            "ByRelevance"
        );
    }
}
