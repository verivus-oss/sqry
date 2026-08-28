//! Hierarchical search execution module
//!
//! Implements semantic code search with file → container → symbol grouping
//! optimized for RAG token budgets.
//!
//! This module uses native graph types (`NodeId`, `NodeEntry`) directly without
//! intermediate Symbol conversion.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::{NodeId, NodeKind};
use sqry_core::query::results::QueryResults;

use crate::engine::{canonicalize_in_workspace_enforced, engine_for_workspace};
use crate::execution::types::{CodeContext, PositionData, RangeData, ToolExecution};
use crate::execution::utils::duration_to_ms;
use crate::tools::{HierarchicalSearchArgs, Visibility};

mod grouping;
mod merging;
mod token_budget;
mod truncation;

use grouping::{FileContentCache, build_container_tree};
use merging::apply_auto_merge;
use token_budget::apply_token_budgets;
use truncation::{enforce_response_limits, paginate_files};

/// Top-level hierarchical search response
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HierarchicalSearchData {
    /// Query that was executed
    pub query: String,
    /// Files containing matching symbols (paginated)
    pub files: Vec<FileGroup>,
    /// Total symbols matched (after truncation)
    pub total_symbols: u64,
    /// Total files with matches (before pagination)
    pub total_files: u64,
    /// Whether results were truncated due to limits
    pub truncated: bool,
    /// Cursor for next page of files
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

/// File-level grouping (Level 2 in research hierarchy)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileGroup {
    /// Workspace-relative file path
    pub path: String,
    /// Programming language
    pub language: String,
    /// Estimated token count for entire file's results
    pub estimated_tokens: u64,
    /// Total symbols in this file (recursive)
    pub symbol_count: u64,
    /// Containers (classes, structs, impl blocks, modules, etc.)
    pub containers: Vec<ContainerGroup>,
    /// Top-level symbols not in any container
    pub top_level_symbols: Vec<HierarchicalSymbol>,
    /// Maximum relevance score in this file
    pub max_score: f64,
    /// Whether this is a stub (metadata only, no symbols loaded)
    /// Use the `expand_files` parameter to load full details
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_stub: bool,
    /// File-level context (header/summary) when `include_file_context` is true
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_context: Option<String>,
}

impl Default for FileGroup {
    fn default() -> Self {
        Self {
            path: String::new(),
            language: String::new(),
            estimated_tokens: 0,
            symbol_count: 0,
            containers: Vec::new(),
            top_level_symbols: Vec::new(),
            max_score: 0.0,
            is_stub: false,
            file_context: None,
        }
    }
}

/// Container-level grouping (Level 3 in research hierarchy) - RECURSIVE
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerGroup {
    /// Container name (e.g., `MyClass`, `impl Handler`)
    pub name: String,
    /// Qualified name including module path
    pub qualified_name: String,
    /// Container kind (class, struct, impl, module, enum, trait, interface, namespace)
    pub kind: String,
    /// Estimated token count for container + all children (recursive)
    pub estimated_tokens: u64,
    /// Nesting depth (1 = top-level container, 2 = nested in another, etc.)
    pub depth: u32,
    /// Parent container names (for breadcrumb navigation)
    pub parent_path: Vec<String>,
    /// Line range in source file (`start_line`, `end_line`)
    pub byte_range: (usize, usize),
    /// Symbols directly inside this container
    pub symbols: Vec<HierarchicalSymbol>,
    /// Child containers (recursive - classes inside modules, etc.)
    pub nested_containers: Vec<ContainerGroup>,
    /// Total symbol count (recursive - includes nested containers)
    pub symbol_count: u64,
    /// Number of direct children (symbols + nested containers)
    pub children_count: u64,
    /// Names of direct children (for preview/browse)
    pub children_names: Vec<String>,
    /// Container-level context (code) when `include_container_context` is true
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_context: Option<String>,
    /// Token cost of container code (set by merge phase, used to avoid double-counting)
    #[serde(skip)]
    pub merged_container_tokens: u64,
}

impl Default for ContainerGroup {
    fn default() -> Self {
        Self {
            name: String::new(),
            qualified_name: String::new(),
            kind: String::new(),
            estimated_tokens: 0,
            depth: 1,
            parent_path: Vec::new(),
            byte_range: (0, 0),
            symbols: Vec::new(),
            nested_containers: Vec::new(),
            symbol_count: 0,
            children_count: 0,
            children_names: Vec::new(),
            container_context: None,
            merged_container_tokens: 0,
        }
    }
}

/// Symbol-level data (Level 4 in research hierarchy)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HierarchicalSymbol {
    /// Symbol name
    pub name: String,
    /// Qualified name including container/module path
    pub qualified_name: String,
    /// Symbol kind (function, method, field, constant, etc.)
    pub kind: String,
    /// Line/column range
    pub range: RangeData,
    /// Relevance score (0.0 - 1.0)
    pub score: f64,
    /// Estimated token count for this symbol's context
    pub estimated_tokens: u64,
    /// Code context (surrounding lines)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<CodeContext>,
    /// Function/method signature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Whether this symbol was auto-merged with parent container
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub merged: bool,
    /// Original hierarchy level before merge (if merged)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_level: Option<String>,
    /// Number of symbols clustered into this one (for context clustering)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clustered_count: Option<u32>,
    /// Macro boundary metadata (only present when the node has macro-related info)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macro_metadata: Option<super::types::MacroMetadataResponse>,
}

impl Default for HierarchicalSymbol {
    fn default() -> Self {
        Self {
            name: String::new(),
            qualified_name: String::new(),
            kind: String::new(),
            range: RangeData {
                start: PositionData {
                    line: 0,
                    character: 0,
                },
                end: PositionData {
                    line: 0,
                    character: 0,
                },
            },
            score: 0.0,
            estimated_tokens: 0,
            context: None,
            signature: None,
            merged: false,
            original_level: None,
            clustered_count: None,
            macro_metadata: None,
        }
    }
}

/// Extract scored node IDs from `QueryResults`.
///
/// Assigns position-based scores to preserve executor's relevance ordering:
/// - First result gets score 1.0
/// - Last result gets score near 0.5
/// - This ensures results are sorted by relevance when displayed
fn query_results_to_scored_nodes(results: &QueryResults) -> Vec<(NodeId, f64)> {
    let total = f64::from(u32::try_from(results.len().max(1)).unwrap_or(u32::MAX));
    results
        .node_ids()
        .iter()
        .enumerate()
        .map(|(idx, &node_id)| {
            // Score from 1.0 (first) to 0.5 (last) to preserve executor ordering
            let idx = f64::from(u32::try_from(idx).unwrap_or(u32::MAX));
            let score = round_relevance_score(1.0 - (idx / total) * 0.5);
            (node_id, score)
        })
        .collect()
}

pub(crate) fn round_relevance_score(score: f64) -> f64 {
    (score * 1000.0).round() / 1000.0
}

/// Convert `NodeKind` to lowercase string for output.
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
        NodeKind::Import => "import",
        NodeKind::Export => "export",
        NodeKind::Component => "component",
        NodeKind::Service => "service",
        NodeKind::Resource => "resource",
        NodeKind::Endpoint => "endpoint",
        NodeKind::Test => "test",
        NodeKind::CallSite => "call_site",
        NodeKind::StyleRule => "style_rule",
        NodeKind::StyleAtRule => "style_at_rule",
        NodeKind::StyleVariable => "style_variable",
        NodeKind::Lifetime => "lifetime",
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

/// Execute hierarchical search.
///
/// # Errors
///
/// Returns an error if workspace resolution, graph acquisition, query
/// classification, or hierarchical search execution fails.
#[allow(clippy::too_many_lines)] // Pipeline staging is kept in one function for end-to-end clarity.
pub fn execute_hierarchical_search(
    args: &HierarchicalSearchArgs,
    cancel: &sqry_core::query::cancellation::CancellationToken,
) -> Result<ToolExecution<HierarchicalSearchData>> {
    let start = Instant::now();

    // Use per-workspace engine cache for multi-repository support
    let explicit_path = if args.path.is_empty() || args.path == "." {
        None
    } else {
        Some(PathBuf::from(&args.path))
    };

    let engine = engine_for_workspace(explicit_path.as_ref())?;
    let workspace_root = engine.workspace_root();
    let search_root = canonicalize_in_workspace_enforced(&args.path, workspace_root)?;

    tracing::debug!(
        query = %args.query,
        path = %search_root.display(),
        max_results = args.max_results,
        max_total_symbols = args.max_total_symbols,
        "Executing hierarchical_search"
    );

    // Get CodeGraph for file symbol lookup
    let graph = engine.ensure_graph()?;
    let snapshot = graph.snapshot();

    // Step 1: Execute semantic search to get matched symbols
    let executor = engine.executor();
    let query = normalized_query(&args.query)?;

    // `A_cancellation.md` §3 + §A4 + `C_budget.md` §C5: route
    // through the budgeted+cancellable executor so per-batch
    // cancellation poll AND per-node row budget both fire inside
    // `evaluate_all`. The maintainer's reported
    // `kind:function AND name~=/.*_set$/` query reaches this path
    // through `hierarchical_search` and was one of the two primary
    // P0-1 failure modes.
    let budget = sqry_core::query::budget::QueryBudget::from_per_call_or_env(
        args.budget_rows,
        cancel.clone(),
    );
    let query_results = executor
        .execute_on_graph_with_variables_with_budget(query, &search_root, None, &budget)
        .context("Failed to execute semantic query")?;

    // Extract scored node IDs (preserves executor relevance ordering)
    let scored_nodes = query_results_to_scored_nodes(&query_results);

    if scored_nodes.is_empty() {
        return Ok(empty_hierarchical_execution(
            &args.query,
            true, // always uses graph
            workspace_root,
            duration_to_ms(start.elapsed()),
        ));
    }

    // Step 2: Apply filters using graph lookups
    let filtered_nodes = apply_filters(&snapshot, &scored_nodes, args);

    // Limit to max_results
    let filtered_nodes: Vec<(NodeId, f64)> =
        filtered_nodes.into_iter().take(args.max_results).collect();

    let candidates_scanned = filtered_nodes.len();

    // Step 4: Group nodes by file using graph lookups
    let nodes_by_file = group_nodes_by_file(&snapshot, filtered_nodes, workspace_root);

    // Step 5: Build hierarchical structure for each file
    let file_content_cache = FileContentCache::new();
    let mut files = build_file_groups(
        nodes_by_file,
        &snapshot,
        workspace_root,
        args,
        &file_content_cache,
    )?;

    // Step 5b: Apply auto-merge (PROPAGATE ERRORS)
    apply_auto_merge_if_enabled(&mut files, args, &file_content_cache, workspace_root)?;

    // Step 5c: Apply token budgets
    apply_token_budgets_for_files(&mut files, args, &file_content_cache, workspace_root)?;

    // Step 6: Sort files deterministically (by max score desc, then path asc)
    sort_files_deterministic(&mut files);

    let total_files = files.len() as u64;

    // Step 6b: Handle expand_files mode (lazy loading of specific files)
    // If expand_files is non-empty, only return those files with full details
    let (limit_truncated, files) = apply_expand_or_limits(files, args);

    // Step 8: Recompute total_symbols AFTER truncation (excludes stub symbols)
    let total_symbols = total_symbols_after_truncation(&files);

    // Step 9: Apply pagination
    let (paginated_files, next_token, page_truncated) = paginate_files(files, args);

    // Final truncated flag is true if ANY truncation occurred
    let truncated = limit_truncated || page_truncated;

    let elapsed = duration_to_ms(start.elapsed());

    Ok(ToolExecution {
        data: HierarchicalSearchData {
            query: args.query.clone(),
            files: paginated_files,
            total_symbols,
            total_files,
            truncated,
            next_page_token: next_token.clone(),
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: elapsed,
        next_page_token: next_token,
        total: Some(total_symbols),
        truncated: Some(truncated),
        candidates_scanned: Some(candidates_scanned as u64),
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

fn normalized_query(query: &str) -> Result<&str> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        anyhow::bail!("query cannot be empty");
    }
    Ok(trimmed)
}

fn empty_hierarchical_execution(
    query: &str,
    used_index: bool,
    workspace_root: &Path,
    execution_ms: u64,
) -> ToolExecution<HierarchicalSearchData> {
    ToolExecution {
        data: HierarchicalSearchData {
            query: query.to_string(),
            files: Vec::new(),
            total_symbols: 0,
            total_files: 0,
            truncated: false,
            next_page_token: None,
        },
        used_index,
        used_graph: false,
        graph_metadata: None,
        execution_ms,
        next_page_token: None,
        total: Some(0),
        truncated: Some(false),
        candidates_scanned: Some(0),
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    }
}

/// Apply search filters to nodes using graph lookups
fn apply_filters(
    snapshot: &GraphSnapshot,
    nodes: &[(NodeId, f64)],
    args: &HierarchicalSearchArgs,
) -> Vec<(NodeId, f64)> {
    nodes
        .iter()
        .filter(|(node_id, score)| {
            matches_language_node(snapshot, *node_id, args)
                && matches_kind_node(snapshot, *node_id, args)
                && matches_visibility_node(snapshot, *node_id, args)
                && matches_score(*score, args.score_min)
        })
        .copied()
        .collect()
}

fn matches_language_node(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    args: &HierarchicalSearchArgs,
) -> bool {
    if args.filters.languages.is_empty() {
        return true;
    }

    let Some(entry) = snapshot.get_node(node_id) else {
        return false;
    };

    let actual = snapshot.files().language_for_file(entry.file);

    args.filters
        .languages
        .iter()
        .any(|candidate| crate::execution::symbol_utils::language_filter_selects(actual, candidate))
}

fn matches_kind_node(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    args: &HierarchicalSearchArgs,
) -> bool {
    if args.filters.kinds.is_empty() {
        return true;
    }

    let Some(entry) = snapshot.get_node(node_id) else {
        return false;
    };

    let kind = node_kind_to_string(entry.kind);
    args.filters
        .kinds
        .iter()
        .any(|k| k.eq_ignore_ascii_case(kind))
}

fn matches_visibility_node(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    args: &HierarchicalSearchArgs,
) -> bool {
    let Some(vis) = &args.filters.visibility else {
        return true;
    };

    let Some(entry) = snapshot.get_node(node_id) else {
        return false;
    };

    let visibility = entry
        .visibility
        .and_then(|id| snapshot.strings().resolve(id))
        .map(|s| s.to_ascii_lowercase());

    match vis {
        Visibility::Public => visibility.as_deref() == Some("public"),
        Visibility::Private => visibility.as_deref() == Some("private"),
    }
}

fn matches_score(score: f64, min_score: Option<f64>) -> bool {
    if let Some(min) = min_score {
        score >= min
    } else {
        true
    }
}

/// Group nodes by their file path using graph lookups
fn group_nodes_by_file(
    snapshot: &GraphSnapshot,
    nodes: Vec<(NodeId, f64)>,
    workspace_root: &Path,
) -> HashMap<String, Vec<(NodeId, f64)>> {
    let files = snapshot.files();
    let mut by_file: HashMap<String, Vec<(NodeId, f64)>> = HashMap::new();

    for (node_id, score) in nodes {
        let Some(entry) = snapshot.get_node(node_id) else {
            continue;
        };

        let file_path = files
            .resolve(entry.file)
            .map(|p| {
                crate::execution::symbol_utils::relative_path_forward_slash(&p, workspace_root)
            })
            .unwrap_or_default();

        by_file.entry(file_path).or_default().push((node_id, score));
    }

    by_file
}

/// Pre-index all nodes from the graph by file path.
///
/// This iterates all nodes once (O(N)) instead of once per file (O(F×N)),
/// significantly improving performance for large graphs with many files.
fn preindex_nodes_by_file(snapshot: &GraphSnapshot) -> HashMap<String, Vec<NodeId>> {
    let files = snapshot.files();

    // Build a FileId -> relative path lookup
    let file_paths: HashMap<_, _> = files.iter().collect();

    let mut by_file: HashMap<String, Vec<NodeId>> = HashMap::new();

    // Single pass over all nodes.
    // Gate 0d iter-2 fix: skip unified losers from hierarchical
    // search preindex. See `NodeEntry::is_unified_loser`.
    for (node_id, entry) in snapshot.iter_nodes() {
        if entry.is_unified_loser() {
            continue;
        }
        let Some(relative_path) = file_paths.get(&entry.file) else {
            continue;
        };
        let file_path_str = crate::execution::symbol_utils::path_to_forward_slash(relative_path);

        by_file.entry(file_path_str).or_default().push(node_id);
    }

    by_file
}

/// Build file groups for all files in the search results.
fn build_file_groups(
    nodes_by_file: HashMap<String, Vec<(NodeId, f64)>>,
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
    args: &HierarchicalSearchArgs,
    file_cache: &FileContentCache,
) -> Result<Vec<FileGroup>> {
    // Pre-index all nodes by file path once (O(N) instead of O(F×N))
    let all_nodes_by_file = preindex_nodes_by_file(snapshot);

    let mut files = Vec::new();

    for (file_path, file_nodes) in nodes_by_file {
        let file_group = build_file_group(
            &file_path,
            &file_nodes,
            &all_nodes_by_file,
            snapshot,
            workspace_root,
            args,
            file_cache,
        )?;
        files.push(file_group);
    }

    Ok(files)
}

/// Build a `FileGroup` from nodes in a single file
fn build_file_group(
    file_path: &str,
    nodes: &[(NodeId, f64)],
    all_nodes_by_file: &HashMap<String, Vec<NodeId>>,
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
    args: &HierarchicalSearchArgs,
    file_cache: &FileContentCache,
) -> Result<FileGroup> {
    let files = snapshot.files();

    // Get language from first node
    let language = nodes
        .first()
        .and_then(|(node_id, _)| snapshot.get_node(*node_id))
        .and_then(|entry| files.language_for_file(entry.file))
        .map_or_else(
            || "unknown".to_string(),
            |l| l.to_string().to_ascii_lowercase(),
        );

    // Get max score
    let max_score = nodes
        .iter()
        .map(|(_, score)| *score)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map_or(0.0, round_relevance_score);

    // Get all nodes in this file from the pre-indexed map for container building
    let full_path = workspace_root.join(file_path);
    let empty_vec = Vec::new();
    let all_file_nodes = all_nodes_by_file.get(file_path).unwrap_or(&empty_vec);

    // Build container tree and assign symbols
    let (containers, top_level_symbols) = build_container_tree(
        nodes,
        all_file_nodes,
        snapshot,
        workspace_root,
        args,
        file_cache,
        &full_path,
    )?;

    // Calculate totals
    let container_symbols: u64 = containers.iter().map(|c| c.symbol_count).sum();
    let top_level_count = top_level_symbols.len() as u64;
    let symbol_count = container_symbols + top_level_count;

    let container_tokens: u64 = containers.iter().map(|c| c.estimated_tokens).sum();
    let top_level_tokens: u64 = top_level_symbols.iter().map(|s| s.estimated_tokens).sum();
    let estimated_tokens = container_tokens + top_level_tokens;

    Ok(FileGroup {
        path: file_path.to_string(),
        language,
        estimated_tokens,
        symbol_count,
        containers,
        top_level_symbols,
        max_score,
        is_stub: false,
        file_context: None,
    })
}

fn apply_auto_merge_if_enabled(
    files: &mut [FileGroup],
    args: &HierarchicalSearchArgs,
    file_cache: &FileContentCache,
    workspace_root: &Path,
) -> Result<()> {
    if args.auto_merge && !args.expand_files.is_empty() {
        // Skip auto-merge for expand_files requests (per Codex recommendation)
        tracing::debug!("Skipping auto-merge for expand_files request");
        return Ok(());
    }

    if !args.auto_merge {
        return Ok(());
    }

    for file in files {
        let full_path = workspace_root.join(&file.path);

        // Propagate error instead of silent skip
        let content = file_cache.get(&full_path).with_context(|| {
            format!(
                "Failed to read file for auto-merge: {path}",
                path = file.path.as_str()
            )
        })?;

        apply_auto_merge(file, &content, args).with_context(|| {
            format!(
                "Auto-merge failed for file: {path}",
                path = file.path.as_str()
            )
        })?;
    }

    Ok(())
}

fn apply_token_budgets_for_files(
    files: &mut [FileGroup],
    args: &HierarchicalSearchArgs,
    file_cache: &FileContentCache,
    workspace_root: &Path,
) -> Result<()> {
    for file in files {
        let full_path = workspace_root.join(&file.path);

        let content = file_cache.get(&full_path).with_context(|| {
            format!(
                "Failed to read file for token budget: {path}",
                path = file.path.as_str()
            )
        })?;

        apply_token_budgets(file, &content, args).with_context(|| {
            format!(
                "Token budget enforcement failed for file: {path}",
                path = file.path.as_str()
            )
        })?;
    }

    Ok(())
}

fn apply_expand_or_limits(
    files: Vec<FileGroup>,
    args: &HierarchicalSearchArgs,
) -> (bool, Vec<FileGroup>) {
    if args.expand_files.is_empty() {
        // Step 7: Enforce response size limits (creates stubs for files beyond limit)
        let mut files = files;
        let limit_truncated = enforce_response_limits(files.as_mut_slice(), args);
        (limit_truncated, files)
    } else {
        // Filter to only requested files, skip limit enforcement
        let expanded: Vec<FileGroup> = files
            .into_iter()
            .filter(|f| args.expand_files.contains(&f.path))
            .collect();
        (false, expanded)
    }
}

fn total_symbols_after_truncation(files: &[FileGroup]) -> u64 {
    files
        .iter()
        .filter(|f| !f.is_stub)
        .map(|f| f.symbol_count)
        .sum()
}

/// Sort files deterministically by max score (desc) then path (asc)
fn sort_files_deterministic(files: &mut [FileGroup]) {
    files.sort_by(|a, b| match b.max_score.partial_cmp(&a.max_score) {
        Some(std::cmp::Ordering::Equal) | None => a.path.cmp(&b.path),
        Some(ord) => ord,
    });
}

/// Estimate tokens for a code snippet
/// Uses ~4 chars per token approximation with 1.2x code adjustment
#[allow(clippy::float_cmp)] // Approximate threshold comparison
#[must_use]
pub fn estimate_tokens(content: &str) -> u64 {
    if content.is_empty() {
        return 0;
    }
    let char_count = u64::try_from(content.len()).unwrap_or(u64::MAX);
    let base_estimate = char_count.div_ceil(4); // ~4 chars per token
    let adjusted = base_estimate.saturating_mul(6).div_ceil(5);
    adjusted.max(1)
}

#[cfg(test)]
mod tests {
    use super::round_relevance_score;

    #[test]
    #[allow(clippy::float_cmp)] // Rounding produces exact f64 values for these inputs
    fn round_relevance_score_stabilizes_serialized_output() {
        assert_eq!(round_relevance_score(0.923_076_923_076_923_2), 0.923);
        assert_eq!(round_relevance_score(0.980_769_230_769_230_8), 0.981);
        assert_eq!(round_relevance_score(1.0), 1.0);
    }
}
