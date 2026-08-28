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
pub(crate) fn perf_log(msg: &str) {
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
use sqry_core::graph::unified::build::BuildConfig;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::persistence::GraphStorage;
use sqry_core::graph::unified::{EdgeKind, NodeKind};
use sqry_core::json_response::IndexStatus;
use sqry_core::progress::{IndexProgress, ProgressReporter, SharedReporter};
use sqry_core::workspace::Classification;
use sqry_plugin_registry::create_plugin_manager;

use crate::handlers::LspHandlerError;
use crate::protocol::{
    CrossLanguageRelation, SortOrder, SqryCycle, SqryDuplicateGroup,
    SqryListCircularDependenciesParams, SqryListCircularDependenciesResult,
    SqryListCrossLanguageRelationsParams, SqryListCrossLanguageRelationsResult,
    SqryListDuplicateGroupsParams, SqryListDuplicateGroupsResult, SqryListFilesByLanguageParams,
    SqryListFilesByLanguageResult, SqryListFilesParams, SqryListFilesResult, SqryListSymbolsParams,
    SqryListSymbolsResult, SqryListUnusedSymbolsParams, SqryListUnusedSymbolsResult,
};
use crate::session::SessionManager;
use sqry_core::query::{CircularType, UnusedScope};

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
        NodeKind::TypeParameter => "type_parameter",
        NodeKind::Annotation => "annotation",
        NodeKind::AnnotationValue => "annotation_value",
        NodeKind::LambdaTarget => "lambda_target",
        NodeKind::JavaModule => "java_module",
        NodeKind::EnumConstant => "enum_constant",
        NodeKind::Channel => "channel",
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

fn resolve_cycle_member_column(
    graph: &CodeGraph,
    target: &Path,
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    member_name: &str,
) -> Option<u32> {
    let Some(file_path) = graph.files().resolve(entry.file) else {
        log::warn!(
            "failed to resolve cycle member source for {} (file id {:?})",
            member_name,
            entry.file
        );
        return None;
    };
    let resolved_path = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        target.join(file_path)
    };
    let Some(line_index) = entry.start_line.checked_sub(1).map(|line| line as usize) else {
        log::warn!(
            "cycle member line missing for {} at {}",
            member_name,
            resolved_path.display()
        );
        return None;
    };
    let Ok(content) = std::fs::read_to_string(&resolved_path) else {
        log::warn!(
            "failed to read cycle member source for {} at {}",
            member_name,
            resolved_path.display()
        );
        return None;
    };
    let Some(line_text) = content.split('\n').nth(line_index) else {
        log::warn!(
            "cycle member line {} out of bounds for {} at {}",
            entry.start_line,
            member_name,
            resolved_path.display()
        );
        return None;
    };
    let byte_col = entry.start_column as usize;
    if byte_col > line_text.len() {
        log::warn!(
            "cycle member byte column {} out of bounds for {} at {}:{}",
            entry.start_column,
            member_name,
            resolved_path.display(),
            entry.start_line
        );
        return None;
    }
    crate::utils::position::line_byte_to_utf16_col(line_text, byte_col)
        .try_into()
        .ok()
}

/// Returns `true` if the given `EdgeKind` is a cross-language relation type.
///
/// Used by both [`compute_cross_language_stats`] and
/// [`list_cross_language_relations`] so that counting and listing stay in sync.
fn is_cross_language_edge_kind(edge_kind: &EdgeKind) -> bool {
    matches!(
        edge_kind,
        EdgeKind::Imports { .. }
            | EdgeKind::Calls { .. }
            | EdgeKind::FfiCall { .. }
            | EdgeKind::HttpRequest { .. }
            | EdgeKind::GrpcCall { .. }
            | EdgeKind::WebAssemblyCall
            | EdgeKind::MessageQueue { .. }
            | EdgeKind::ProtocolCall { .. }
    )
}

/// Compute cross-language edge statistics from the unified graph in a single pass.
///
/// Returns `(total_count, pair_counts)` where `pair_counts` maps
/// "`source_lang→target_lang`" to the number of cross-language edges
/// between those two languages.
fn compute_cross_language_stats(graph: &CodeGraph) -> (usize, HashMap<String, usize>) {
    let snapshot = graph.snapshot();
    let mut total: usize = 0;
    let mut pair_counts: HashMap<String, usize> = HashMap::new();

    for (source_id, target_id, edge_kind) in snapshot.iter_edges() {
        if !is_cross_language_edge_kind(&edge_kind) {
            continue;
        }

        let Some(source_entry) = snapshot.nodes().get(source_id) else {
            continue;
        };
        let Some(target_entry) = snapshot.nodes().get(target_id) else {
            continue;
        };

        let Some(source_lang) = snapshot.files().language_for_file(source_entry.file) else {
            continue;
        };
        let Some(target_lang) = snapshot.files().language_for_file(target_entry.file) else {
            continue;
        };

        if source_lang != target_lang {
            total += 1;
            let key = format!("{source_lang}\u{2192}{target_lang}");
            *pair_counts.entry(key).or_insert(0) += 1;
        }
    }

    (total, pair_counts)
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
    // Parses through the one central table rather than a local alias list.
    // The previous hand-rolled match covered only seven languages, so
    // `golang`, `rs`, `kt`, and `hcl` were rejected here while `from_id`
    // accepted them elsewhere (issue #714).
    Language::from_id(query) == Some(lang)
}

/// Build the `IndexStatus` for a member-folder request per §1.4
/// (acceptance criterion 3).
///
/// Member folders never own a snapshot; they route through the
/// workspace's source roots. The response carries the **full**
/// `WorkspaceIndexStatus` aggregate — `source_root_statuses`,
/// `missing_count`, `building_count`, `ok_count`, `error_count`, and
/// `generated_at` — inside `IndexStatus.aggregate`, plus a dedicated
/// `partial` flag when any source root is `Missing` or `Error`. The
/// summary scalars (`exists`, `path`, `building`, `age_seconds`) on
/// the outer `IndexStatus` are convenience projections so simple
/// consumers can render a one-line summary without re-walking the
/// vector. See [`sqry_core::json_response::IndexStatus::aggregate`]
/// for the precise contract.
fn member_folder_aggregate_status(session: &SessionManager, target: &Path) -> IndexStatus {
    let workspace = session.logical_workspace();
    let aggregate = crate::session::aggregate_workspace_index_status(workspace.as_ref());
    IndexStatus::aggregate(target, aggregate)
}

fn load_status_graph(
    session: &SessionManager,
    target: &Path,
    _graph_storage: &GraphStorage,
) -> Result<std::sync::Arc<CodeGraph>> {
    // SGA06 — route the index-status graph load through the shared
    // FilesystemGraphProvider via `acquire_session_graph`. The provider
    // applies the canonical path-policy / plugin-selection / SHA-256
    // integrity checks, and the caller-side self-heal preserves the
    // historic LSP behaviour: a corrupt snapshot is auto-rebuilt and
    // the session caches are cleared.
    match crate::session::acquire_session_graph(target, "lsp:index_status") {
        Ok(graph) => Ok(graph),
        Err(err) => {
            // The acquisition path returns `BuildFailed` when the auto-build
            // hook (or the corrupt-load self-heal branch) itself failed.
            // Otherwise we treat the error as a hard failure surfaced to
            // the LSP client.
            if matches!(
                err,
                sqry_core::graph::acquisition::GraphAcquisitionError::BuildFailed { .. }
            ) {
                // Make sure stale caches are cleared so the next request
                // re-runs through the provider rather than returning a stale
                // `Arc<CodeGraph>`.
                session.clear_graph_cache();
                session.clear_project_graph_cache_for_path(target);
            }
            Err(crate::session::map_acquisition_error_for_lsp(err, target)).with_context(|| {
                format!(
                    "index status graph acquisition failed for {}",
                    target.display()
                )
            })
        }
    }
}

/// Fetch the current index status for the requested path.
///
/// Uses the unified graph (`.sqry/graph/`) for status information.
///
/// Routes per §1.4: source-root paths return per-source-root data; member
/// folders return an aggregate `WorkspaceIndexStatus`; excluded paths
/// return `IndexStatus::excluded()`; out-of-workspace paths return
/// `IndexStatus::not_found()`.
///
/// # Errors
///
/// Returns an error when the session cannot resolve the path or when loading
/// the graph fails.
pub fn index_status(session: &SessionManager, path: Option<&str>) -> Result<IndexStatus> {
    let handler_start = Instant::now();
    perf_log(&format!("index_status START path={path:?}"));

    // §1.4 contract: classify the requested path against the logical
    // workspace **before** consulting graph storage so member /
    // excluded / unknown paths short-circuit to the contract-correct
    // wire shape (rather than returning per-source-root data for a
    // path that does not own a snapshot).
    //
    // - `Source` (path is or descends from a source root) → continue
    //   into the per-source-root graph load below (today's behaviour).
    // - `Member` → aggregate WorkspaceIndexStatus over every source
    //   root, surfaced through the IndexStatus extra fields. Member
    //   folders never own a snapshot themselves; they route through
    //   their workspace's source roots.
    // - `Excluded` → IndexStatus::excluded(): no graph data, no path,
    //   no per-source-root detail.
    // - `Unknown` → IndexStatus::not_found(); `--index-root` boundary
    //   enforcement happens in `session.resolve_path`.
    let resolve_attempt = session.resolve_path(path);
    let target = match resolve_attempt {
        Ok(p) => p,
        Err(_err) => {
            // resolve_path rejected the input — either non-canonicalizable
            // or outside the --index-root boundary. The contract
            // (acceptance criterion 7) keeps that boundary intact: surface
            // a "not found" payload so the client cannot use sqry/indexStatus
            // as a probe for paths outside the security envelope.
            perf_log("index_status: resolve_path rejected the input");
            return Ok(IndexStatus::not_found());
        }
    };

    match session.classify_path(&target) {
        sqry_core::workspace::Classification::Excluded => {
            perf_log("index_status: path classified as Excluded");
            let mut status = IndexStatus::excluded();
            status.path = Some(target.display().to_string());
            return Ok(status);
        }
        sqry_core::workspace::Classification::Unknown => {
            perf_log("index_status: path classified as Unknown");
            return Ok(IndexStatus::not_found());
        }
        sqry_core::workspace::Classification::Member { reason: _ } => {
            perf_log("index_status: path classified as Member -> aggregate");
            return Ok(member_folder_aggregate_status(session, &target));
        }
        sqry_core::workspace::Classification::Source => {
            // Continue into the per-source-root branch below.
        }
    }

    let graph_storage = GraphStorage::new(&target);

    if !graph_storage.exists() {
        perf_log("index_status: graph does not exist");
        return Ok(IndexStatus::not_found());
    }

    // Load the requested source-root graph directly so index status remains
    // bounded by `resolve_path()` even when ambient ancestor git metadata exists.
    let load_start = Instant::now();
    let graph_arc = load_status_graph(session, &target, &graph_storage)?;
    let graph: &CodeGraph = &graph_arc;

    perf_log(&format!(
        "index_status graph load took {elapsed:?}",
        elapsed = load_start.elapsed()
    ));

    // Get stats from the graph
    let symbol_count = graph.node_count();
    let file_count = graph.files().len();
    let languages = collect_languages_from_graph(graph);

    // Get file metadata for timestamp
    let metadata = std::fs::metadata(graph_storage.snapshot_path())
        .context("failed to read graph metadata")?;
    let created = metadata
        .modified()
        .or_else(|_| metadata.created())
        .context("failed to read graph timestamp")?;

    let created_timestamp = created
        .duration_since(std::time::UNIX_EPOCH)
        .context("invalid creation timestamp")?
        .as_secs();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("failed to get current time")?
        .as_secs();

    let age_seconds = now.saturating_sub(created_timestamp);
    let datetime = chrono::DateTime::<chrono::Utc>::from(created);

    // Compute grouped counts for tree view grouping
    let symbol_counts_by_kind = compute_symbol_counts_from_graph(graph);
    let file_counts_by_language = compute_file_counts_from_graph(graph);
    let (cross_language_count, relation_counts_by_pair) = compute_cross_language_stats(graph);

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
    let plugin_selection = (!plugins.plugins().is_empty()).then(|| {
        sqry_core::graph::unified::persistence::PluginSelectionManifest {
            active_plugin_ids: plugins
                .plugins()
                .iter()
                .map(|plugin| plugin.metadata().id.to_string())
                .collect(),
            high_cost_mode: None,
        }
    });

    let (graph, _build_result) = build_and_persist_graph_with_progress(
        target,
        &plugins,
        &build_config,
        "lsp:rebuild_index",
        plugin_selection,
        reporter.clone(),
    )
    .context("Failed to build and persist unified graph")?;

    // Clear the graph cache so it reloads the newly built graph
    session.clear_graph_cache();
    session.clear_project_graph_cache_for_path(target);

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
    let language = params.language;
    let query_lang = language.to_lowercase();
    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .min(MAX_LIST_LIMIT);
    let offset = params.offset.unwrap_or(0);

    match session.classify_path(&target) {
        Classification::Source => {}
        Classification::Member { .. } | Classification::Excluded | Classification::Unknown => {
            return Ok(empty_files_by_language_result(language, offset, limit));
        }
    }

    let Some(graph) = session.graph_for_path(&target)? else {
        return Ok(empty_files_by_language_result(language, offset, limit));
    };

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
        language,
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

        // Get languages for source and target files (as enums for comparison)
        let source_lang_enum = snapshot.files().language_for_file(source_entry.file);
        let target_lang_enum = snapshot.files().language_for_file(target_entry.file);

        // Only include if both have known languages and they differ
        let (Some(sl), Some(tl)) = (source_lang_enum, target_lang_enum) else {
            continue;
        };
        if sl == tl {
            continue;
        }

        // Convert to strings for output
        let from_language = sl.to_string();
        let to_language = tl.to_string();

        // Filter to cross-language edge kinds using the shared predicate
        if !is_cross_language_edge_kind(&edge_kind) {
            continue;
        }
        let relation_type = match edge_kind {
            EdgeKind::Imports { .. } => "import",
            EdgeKind::Calls { .. } => "call",
            EdgeKind::FfiCall { .. } => "ffi",
            EdgeKind::HttpRequest { .. } => "http",
            EdgeKind::GrpcCall { .. } => "grpc",
            EdgeKind::WebAssemblyCall => "wasm",
            EdgeKind::MessageQueue { .. } => "mq",
            EdgeKind::ProtocolCall { .. } => "protocol",
            _ => continue,
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

fn empty_files_by_language_result(
    language: String,
    offset: usize,
    limit: usize,
) -> SqryListFilesByLanguageResult {
    SqryListFilesByLanguageResult {
        language,
        files: vec![],
        total: 0,
        offset,
        limit,
        has_more: false,
    }
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

    let duplicate_groups = collect_duplicate_body_groups(&graph);

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

fn collect_duplicate_body_groups(graph: &CodeGraph) -> Vec<(u128, Vec<NodeId>)> {
    let mut hash_groups: HashMap<u128, Vec<NodeId>> = HashMap::new();
    for (node_id, entry) in graph.nodes().iter() {
        if entry.is_unified_loser() {
            continue;
        }
        if let Some(body_hash) = entry.body_hash {
            hash_groups
                .entry(body_hash.as_u128())
                .or_default()
                .push(node_id);
        }
    }

    let mut duplicate_groups: Vec<_> = hash_groups
        .into_iter()
        .filter(|(_, nodes)| nodes.len() >= 2)
        .collect();
    duplicate_groups.sort_by(|(hash_a, nodes_a), (hash_b, nodes_b)| {
        nodes_b
            .len()
            .cmp(&nodes_a.len())
            .then_with(|| hash_a.cmp(hash_b))
    });
    duplicate_groups
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
#[allow(
    clippy::too_many_lines,
    reason = "cycle detection logic with SCC formatting is inherently verbose"
)]
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
        return Err(LspHandlerError::InvalidParams(format!(
            "Unsupported circular_type '{circular_type}'. Valid values: 'calls', 'imports', 'modules'.",
            circular_type = params.circular_type.as_str()
        ))
        .into());
    };

    let cap = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .min(MAX_LIST_LIMIT);

    // Route through sqry-db: `CyclesQuery` is the name-keyed cycle
    // predicate in the Phase 3C dispatch taxonomy (DB19), cached
    // per-snapshot. Behavior matches the pre-DB19
    // `CircularConfig { min_depth: 2, max_depth: None,
    // max_results: cap, should_include_self_loops }` default exactly.
    // The `Arc<Vec<Vec<NodeId>>>` result is materialized to qualified
    // names below (mirroring the pre-DB19 return-shape contract).
    let snapshot = std::sync::Arc::new(graph.snapshot());
    // PN3 CLIENT_LOAD: opportunistic cold-load from workspace companion file.
    let workspace_root = session.index_root_for_cold_load();
    let db = sqry_db::queries::dispatch::make_query_db_cold(
        std::sync::Arc::clone(&snapshot),
        &workspace_root,
    );
    let cycle_node_ids = db.get::<sqry_db::queries::CyclesQuery>(&sqry_db::queries::CyclesKey {
        circular_type,
        bounds: sqry_db::queries::CycleBounds {
            min_depth: 2,
            max_depth: None,
            max_results: cap.saturating_add(1),
            should_include_self_loops: params.should_include_self_loops,
        },
    });
    let cycles: Vec<Vec<String>> = {
        let strings = snapshot.strings();
        cycle_node_ids
            .iter()
            .map(|component| {
                component
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
            .filter(|names: &Vec<String>| !names.is_empty())
            .collect()
    };

    let probed_total_cycles = cycles.len();
    let truncated = probed_total_cycles > cap;

    // Build a lookup from qualified/simple name to node entry for location resolution
    let name_to_node: HashMap<
        String,
        (
            &sqry_core::graph::unified::storage::arena::NodeEntry,
            sqry_core::graph::unified::NodeId,
        ),
    > = {
        let strings = graph.strings();
        let mut map = HashMap::new();
        for (node_id, entry) in graph.nodes().iter() {
            // Gate 0d iter-2 fix: skip unified losers from LSP
            // cycle-name lookup map. See `NodeEntry::is_unified_loser`.
            if entry.is_unified_loser() {
                continue;
            }
            if let Some(name) = entry
                .qualified_name
                .and_then(|id| strings.resolve(id))
                .or_else(|| strings.resolve(entry.name))
            {
                map.insert(name.to_string(), (entry, node_id));
            }
        }
        map
    };

    // Convert to protocol types
    let result_cycles: Vec<SqryCycle> = cycles
        .into_iter()
        .take(cap)
        .map(|members| {
            // Generate stable cycle ID from sorted members
            let mut sorted_members = members.clone();
            sorted_members.sort();
            let mut hasher = DefaultHasher::new();
            sorted_members.hash(&mut hasher);
            let cycle_hash = hasher.finish();
            let cycle_id = format!("{cycle_hash:016x}");

            // Resolve locations for each member
            let locations: Vec<crate::protocol::CycleMemberLocation> = members
                .iter()
                .map(|member_name| {
                    if let Some((entry, _node_id)) = name_to_node.get(member_name) {
                        let file_uri = graph.files().resolve(entry.file).and_then(|file_path| {
                            let path = if file_path.is_absolute() {
                                file_path.to_path_buf()
                            } else {
                                target.join(&*file_path)
                            };
                            tower_lsp::lsp_types::Url::from_file_path(&path)
                                .ok()
                                .map(|u| u.to_string())
                        });

                        let line = if entry.start_line > 0 {
                            Some(entry.start_line.saturating_sub(1))
                        } else {
                            None
                        };
                        let column = if entry.start_line > 0 {
                            resolve_cycle_member_column(&graph, &target, entry, member_name)
                        } else {
                            None
                        };

                        crate::protocol::CycleMemberLocation {
                            name: member_name.clone(),
                            file: file_uri,
                            line,
                            column,
                        }
                    } else {
                        crate::protocol::CycleMemberLocation {
                            name: member_name.clone(),
                            file: None,
                            line: None,
                            column: None,
                        }
                    }
                })
                .collect();

            let member_locations = if locations.iter().any(|l| l.file.is_some()) {
                Some(locations)
            } else {
                None
            };

            SqryCycle {
                cycle_id,
                depth: members.len(),
                members,
                cycle_type: params.circular_type.clone(),
                member_locations,
            }
        })
        .collect();

    perf_log(&format!(
        "list_circular_dependencies TOTAL took {elapsed:?}, cycles={cycles}/{actual_total_cycles}",
        elapsed = handler_start.elapsed(),
        cycles = result_cycles.len(),
        actual_total_cycles = probed_total_cycles
    ));

    Ok(SqryListCircularDependenciesResult {
        total_cycles: if truncated {
            cap.saturating_add(1)
        } else {
            probed_total_cycles
        },
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
        return Err(LspHandlerError::InvalidParams(format!(
            "Unsupported scope '{scope}'. Valid values: 'public', 'private', 'function', 'struct', 'all'.",
            scope = params.scope
        ))
        .into());
    };

    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .min(MAX_LIST_LIMIT);

    // Route through sqry-db: `UnusedQuery` is a name-keyed predicate in
    // the Phase 3C dispatch taxonomy (DB19), cached per-snapshot. The LSP
    // surface only exposes `scope` (no free-form `lang` / `kind`
    // post-filter) so we can use the same scope in the key (no superset
    // widening needed — unlike MCP's `Struct` variant, the LSP contract
    // passes `UnusedScope` verbatim to both the user and sqry-db).
    //
    // The binding-plane post-filter can suppress raw rows before the LSP
    // `limit` is applied, so fetch a full raw pool and truncate only after the
    // post-filter has produced the user-facing candidate list.
    let check_start = Instant::now();
    let snapshot = std::sync::Arc::new(graph.snapshot());
    // PN3 CLIENT_LOAD: opportunistic cold-load from workspace companion file.
    let workspace_root = session.index_root_for_cold_load();
    let db = sqry_db::queries::dispatch::make_query_db_cold(
        std::sync::Arc::clone(&snapshot),
        &workspace_root,
    );
    let raw_unused_node_ids =
        db.get::<sqry_db::queries::UnusedQuery>(&sqry_db::queries::UnusedKey {
            scope,
            max_results: snapshot.nodes().len().max(limit.saturating_add(1)),
        });
    let unused_node_ids = sqry_db::queries::unused_post_filter::apply_binding_plane_post_filter(
        &raw_unused_node_ids,
        &snapshot,
        &db,
    );
    perf_log(&format!(
        "list_unused_symbols UnusedQuery took {elapsed:?}, found {found}",
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

    let total = unused_symbols.len();
    let truncated = total > limit;
    unused_symbols.truncate(limit);

    perf_log(&format!(
        "list_unused_symbols TOTAL took {elapsed:?}, unused={count}",
        elapsed = handler_start.elapsed(),
        count = unused_symbols.len()
    ));

    Ok(SqryListUnusedSymbolsResult {
        total: if truncated {
            limit.saturating_add(1)
        } else {
            total
        },
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
        let order = SortOrder::default();
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
