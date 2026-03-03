//! Introspection tool execution.
//!
//! This module implements the introspection tools for listing files,
//! symbols, and graph statistics.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;

use crate::engine::engine_for_workspace;
use crate::execution::types::{
    FileEntryData, GraphStatsData, ListFilesData, ListSymbolsData, SymbolEntryData, ToolExecution,
};
use crate::execution::utils::duration_to_ms;
use crate::tools::{GetGraphStatsArgs, ListFilesArgs, ListSymbolsArgs};

/// Execute the `list_files` tool to list files in the graph.
/// Resolve workspace path from args.path parameter.
///
/// If path is "." (default), returns None to trigger discovery.
/// Otherwise returns Some(path) for explicit workspace resolution.
fn resolve_workspace_path(path: &str) -> Option<PathBuf> {
    if path == "." {
        None
    } else {
        Some(PathBuf::from(path))
    }
}
/// Check if a file's language matches the filter.
fn matches_language_filter(language: Option<&str>, filter: &str) -> bool {
    let filter_lower = filter.to_lowercase();
    if let Some(lang) = language {
        lang.to_lowercase().contains(&filter_lower)
    } else {
        false
    }
}

/// Collect filtered files with pagination.
fn collect_filtered_files(
    files: &sqry_core::graph::unified::storage::registry::FileRegistry,
    args: &ListFilesArgs,
    workspace_root: &std::path::Path,
) -> (Vec<FileEntryData>, u64) {
    let mut file_entries: Vec<FileEntryData> = Vec::new();
    let mut total_count = 0u64;

    for (_file_id, path, lang_opt) in files.iter_with_language() {
        let language = lang_opt.map(|l| l.to_string());

        // Apply language filter if specified
        if let Some(ref filter_lang) = args.language
            && !matches_language_filter(language.as_deref(), filter_lang)
        {
            continue;
        }

        total_count += 1;

        // Apply pagination
        let offset = args.pagination.offset;
        let limit = args.max_results;
        if total_count > offset as u64 && file_entries.len() < limit {
            // Make path relative to workspace (forward slashes for cross-platform JSON)
            let rel_path = crate::execution::symbol_utils::relative_path_forward_slash(
                path.as_ref(),
                workspace_root,
            );

            file_entries.push(FileEntryData {
                path: rel_path,
                language,
            });
        }
    }

    (file_entries, total_count)
}

pub fn execute_list_files(args: &ListFilesArgs) -> Result<ToolExecution<ListFilesData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(path = %args.path, language = ?args.language, "Executing list_files tool");

    let graph = engine.ensure_graph()?;

    let files = graph.files();

    // Collect files with optional language filter
    let (file_entries, total_count) = collect_filtered_files(files, args, &workspace_root);

    let data = ListFilesData {
        files: file_entries,
        total: total_count,
    };

    let truncated = total_count > (args.pagination.offset + args.max_results) as u64;
    let next_token = if truncated {
        Some(format!("{}", args.pagination.offset + args.max_results))
    } else {
        None
    };

    tracing::debug!(total = total_count, "list_files completed");

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: next_token,
        total: Some(total_count),
        truncated: Some(truncated),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Check whether a symbol node matches the kind and language filters.
fn matches_symbol_filters(kind_str: &str, language: &str, args: &ListSymbolsArgs) -> bool {
    // Apply kind filter if specified
    if let Some(ref filter_kind) = args.kind {
        let filter_kind_lower = filter_kind.to_lowercase();
        if !kind_str.to_lowercase().contains(&filter_kind_lower) {
            return false;
        }
    }

    // Apply language filter if specified
    if let Some(ref filter_lang) = args.language {
        let filter_lang_lower = filter_lang.to_lowercase();
        if !language.to_lowercase().contains(&filter_lang_lower) {
            return false;
        }
    }

    true
}

/// Execute the `list_symbols` tool to list symbols in the graph.
pub fn execute_list_symbols(args: &ListSymbolsArgs) -> Result<ToolExecution<ListSymbolsData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(
        path = %args.path,
        kind = ?args.kind,
        language = ?args.language,
        "Executing list_symbols tool"
    );

    let graph = engine.ensure_graph()?;

    let files = graph.files();
    let strings = graph.strings();

    // Collect symbols with optional filters
    let mut symbol_entries: Vec<SymbolEntryData> = Vec::new();
    let mut total_count = 0u64;

    for (_node_id, entry) in graph.nodes().iter() {
        // Get node info
        let name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let qualified_name = entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .map_or_else(|| name.clone(), |s| s.to_string());

        let kind_str = format!("{:?}", entry.kind);

        // Get file info - entry.file is a FileId, use resolve() to get the path
        let file_path = files
            .resolve(entry.file)
            .map(|p| {
                crate::execution::symbol_utils::relative_path_forward_slash(
                    p.as_ref(),
                    &workspace_root,
                )
            })
            .unwrap_or_default();

        let language = files
            .language_for_file(entry.file)
            .map_or_else(|| "unknown".to_string(), |l| l.to_string());

        if !matches_symbol_filters(&kind_str, &language, args) {
            continue;
        }

        total_count += 1;

        // Apply pagination
        let offset = args.pagination.offset;
        let limit = args.max_results;
        if total_count > offset as u64 && symbol_entries.len() < limit {
            let line = entry.start_line;

            symbol_entries.push(SymbolEntryData {
                name,
                qualified_name,
                kind: kind_str,
                file_path,
                line,
                language,
            });
        }
    }

    let data = ListSymbolsData {
        symbols: symbol_entries,
        total: total_count,
    };

    let truncated = total_count > (args.pagination.offset + args.max_results) as u64;
    let next_token = if truncated {
        Some(format!("{}", args.pagination.offset + args.max_results))
    } else {
        None
    };

    tracing::debug!(total = total_count, "list_symbols completed");

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: next_token,
        total: Some(total_count),
        truncated: Some(truncated),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Execute the `get_graph_stats` tool to get graph statistics.
pub fn execute_get_graph_stats(args: &GetGraphStatsArgs) -> Result<ToolExecution<GraphStatsData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(path = %args.path, "Executing get_graph_stats tool");

    let graph = engine.ensure_graph()?;

    // Get basic counts
    let total_nodes = graph.node_count() as u64;
    let total_edges = graph.edge_count() as u64;
    let total_files = graph.files().len() as u64;
    let graph_epoch = graph.epoch();

    // Count nodes by kind
    let mut nodes_by_kind: HashMap<String, u64> = HashMap::new();
    for (kind, count) in graph.indices().iter_kinds() {
        let kind_str = format!("{kind:?}");
        nodes_by_kind.insert(kind_str, count as u64);
    }

    // Count files by language
    let mut files_by_language: HashMap<String, u64> = HashMap::new();
    for (_file_id, _path, language) in graph.files().iter_with_language() {
        let lang_str = language.map_or_else(|| "unknown".to_string(), |l| l.to_string());
        *files_by_language.entry(lang_str).or_insert(0) += 1;
    }

    let data = GraphStatsData {
        total_nodes,
        total_edges,
        total_files,
        nodes_by_kind,
        files_by_language,
        graph_epoch,
    };

    tracing::debug!(
        nodes = total_nodes,
        edges = total_edges,
        files = total_files,
        "get_graph_stats completed"
    );

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(1),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Aggregated symbol and file statistics grouped by language and kind.
struct SymbolStats {
    lang_file_counts: HashMap<String, usize>,
    lang_symbol_counts: HashMap<String, usize>,
    kind_counts: HashMap<String, usize>,
    total_files: usize,
    total_symbols: usize,
}

/// Count symbol and file statistics grouped by language and kind.
fn count_symbol_stats(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> SymbolStats {
    let files = snapshot.files();
    let mut lang_file_counts: HashMap<String, usize> = HashMap::new();
    let mut lang_symbol_counts: HashMap<String, usize> = HashMap::new();
    let mut kind_counts: HashMap<String, usize> = HashMap::new();
    let mut total_files = 0usize;
    let mut total_symbols = 0usize;
    let mut seen_files = std::collections::HashSet::new();

    for (_node_id, entry) in snapshot.iter_nodes() {
        total_symbols += 1;

        let kind = format!("{:?}", entry.kind).to_lowercase();
        *kind_counts.entry(kind).or_insert(0) += 1;

        let language = files
            .language_for_file(entry.file)
            .map_or("unknown".to_string(), |l| {
                l.to_string().to_ascii_lowercase()
            });

        *lang_symbol_counts.entry(language.clone()).or_insert(0) += 1;

        if seen_files.insert(entry.file) {
            total_files += 1;
            *lang_file_counts.entry(language).or_insert(0) += 1;
        }
    }

    SymbolStats {
        lang_file_counts,
        lang_symbol_counts,
        kind_counts,
        total_files,
        total_symbols,
    }
}

/// Count total edges and cross-language edge count.
fn count_edge_stats(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> (usize, usize) {
    let files = snapshot.files();
    let mut total_edges = 0usize;
    let mut cross_language_edges = 0usize;

    for (source_id, target_id, _edge_kind) in snapshot.iter_edges() {
        total_edges += 1;

        if let (Some(from_entry), Some(to_entry)) =
            (snapshot.get_node(source_id), snapshot.get_node(target_id))
        {
            let from_lang = files.language_for_file(from_entry.file);
            let to_lang = files.language_for_file(to_entry.file);

            if let (Some(fl), Some(tl)) = (from_lang, to_lang)
                && fl != tl
            {
                cross_language_edges += 1;
            }
        }
    }

    (total_edges, cross_language_edges)
}

/// Estimate the number of 2-hop cycles (A calls B, B calls A).
fn estimate_cycle_count(snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot) -> usize {
    let mut cycles = 0usize;
    for (node_id, _entry) in snapshot.iter_nodes() {
        let callees = snapshot.get_callees(node_id);
        for callee in callees {
            let callee_callees = snapshot.get_callees(callee);
            if callee_callees.contains(&node_id) {
                cycles += 1;
            }
        }
    }
    cycles / 2 // Each cycle is counted twice
}

/// Estimate the number of potentially unused symbols (no callers but has callees).
fn estimate_unused_count(snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot) -> usize {
    let mut unused_symbols = 0usize;
    for (node_id, _entry) in snapshot.iter_nodes() {
        let callers = snapshot.get_callers(node_id);
        if callers.is_empty() {
            let has_outgoing = !snapshot.get_callees(node_id).is_empty();
            if has_outgoing {
                unused_symbols += 1;
            }
        }
    }
    unused_symbols
}

/// Execute the `get_insights` tool to provide codebase health metrics.
pub fn execute_get_insights(
    args: &crate::tools::GetInsightsArgs,
) -> Result<ToolExecution<super::super::types::GetInsightsData>> {
    use super::super::types::{
        GetInsightsData, HealthIndicatorsData, KindStatsData, LanguageStatsData,
    };

    let start = std::time::Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(path = %args.path, "Executing get_insights tool");

    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();

    let SymbolStats {
        lang_file_counts,
        lang_symbol_counts,
        kind_counts,
        total_files,
        total_symbols,
    } = count_symbol_stats(&snapshot);

    let (total_edges, cross_language_edges) = count_edge_stats(&snapshot);

    let cycles = estimate_cycle_count(&snapshot);
    let unused_symbols = estimate_unused_count(&snapshot);

    // Count duplicate groups (symbols with same name)
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    let strings = snapshot.strings();
    for (_node_id, entry) in snapshot.iter_nodes() {
        if let Some(name) = strings.resolve(entry.name) {
            *name_counts.entry(name.to_string()).or_insert(0) += 1;
        }
    }
    let duplicate_groups = name_counts.values().filter(|&&c| c > 1).count();

    // Build language stats
    let mut languages: Vec<LanguageStatsData> = lang_file_counts
        .iter()
        .map(|(lang, &files_count)| LanguageStatsData {
            language: lang.clone(),
            files: files_count,
            symbols: *lang_symbol_counts.get(lang).unwrap_or(&0),
        })
        .collect();
    languages.sort_by(|a, b| b.files.cmp(&a.files));

    // Build kind stats
    let mut symbol_kinds: Vec<KindStatsData> = kind_counts
        .into_iter()
        .map(|(kind, count)| KindStatsData { kind, count })
        .collect();
    symbol_kinds.sort_by(|a, b| b.count.cmp(&a.count));

    let data = GetInsightsData {
        total_files,
        total_symbols,
        total_edges,
        languages,
        symbol_kinds,
        health: HealthIndicatorsData {
            cycles,
            unused_symbols,
            duplicate_groups,
            cross_language_edges,
        },
    };

    tracing::debug!(
        total_files = total_files,
        total_symbols = total_symbols,
        "get_insights completed"
    );

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(1),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Check whether a node matches the complexity target filter.
///
/// Returns `true` if the node's file path, name, or qualified name contains the filter string.
fn matches_complexity_filter(
    file_path: &str,
    name: &str,
    qualified_name: &str,
    filter: &str,
) -> bool {
    file_path.to_lowercase().contains(filter)
        || name.to_lowercase().contains(filter)
        || qualified_name.to_lowercase().contains(filter)
}

/// Compute an estimated complexity score for a function/method node.
///
/// Uses a simple heuristic: `1 + callee_count / 5 + lines / 20`.
/// Real cyclomatic complexity would require AST analysis.
fn compute_estimated_complexity(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node_id: sqry_core::graph::unified::node::NodeId,
    start_line: u32,
    end_line: u32,
) -> u32 {
    let lines = end_line.saturating_sub(start_line).saturating_add(1);
    let callees = snapshot.get_callees(node_id);
    let callee_count = u32::try_from(callees.len()).unwrap_or(u32::MAX);
    1 + callee_count / 5 + lines / 20
}

/// Execute the `complexity_metrics` tool to analyze code complexity.
pub fn execute_complexity_metrics(
    args: &crate::tools::ComplexityMetricsArgs,
) -> Result<ToolExecution<super::super::types::ComplexityMetricsData>> {
    use super::super::types::{ComplexityMetricData, ComplexityMetricsData};
    use sqry_core::graph::unified::node::NodeKind;

    let start = std::time::Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(
        path = %args.path,
        target = ?args.target,
        min_complexity = args.min_complexity,
        "Executing complexity_metrics tool"
    );

    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files = snapshot.files();

    let target_filter: Option<String> = args.target.as_ref().map(|t| t.to_lowercase());

    let mut metrics: Vec<ComplexityMetricData> = Vec::new();

    for (node_id, entry) in snapshot.iter_nodes() {
        if !matches!(entry.kind, NodeKind::Function | NodeKind::Method) {
            continue;
        }

        let name = match strings.resolve(entry.name) {
            Some(n) => n.to_string(),
            None => continue,
        };

        let qualified_name = entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .map_or_else(|| name.clone(), |s| s.to_string());

        let file_path = match files.resolve(entry.file) {
            Some(p) => crate::execution::symbol_utils::path_to_forward_slash(&p),
            None => continue,
        };

        if let Some(ref filter) = target_filter
            && !matches_complexity_filter(&file_path, &name, &qualified_name, filter)
        {
            continue;
        }

        let kind = format!("{:?}", entry.kind).to_lowercase();
        let lines = entry
            .end_line
            .saturating_sub(entry.start_line)
            .saturating_add(1);

        let complexity =
            compute_estimated_complexity(&snapshot, node_id, entry.start_line, entry.end_line);

        if complexity < args.min_complexity {
            continue;
        }

        metrics.push(ComplexityMetricData {
            name,
            qualified_name,
            kind,
            file_path,
            complexity,
            lines,
        });
    }

    // Sort by complexity or name
    if args.sort_by_complexity {
        metrics.sort_by(|a, b| {
            b.complexity
                .cmp(&a.complexity)
                .then_with(|| a.name.cmp(&b.name))
        });
    } else {
        metrics.sort_by(|a, b| a.name.cmp(&b.name));
    }

    metrics.truncate(args.max_results);

    let total = metrics.len();
    let max_complexity = metrics.iter().map(|m| m.complexity).max().unwrap_or(0);
    let average_complexity = if metrics.is_empty() {
        0.0
    } else {
        let count = f64::from(u32::try_from(metrics.len()).unwrap_or(u32::MAX));
        metrics.iter().map(|m| f64::from(m.complexity)).sum::<f64>() / count
    };

    let data = ComplexityMetricsData {
        metrics,
        total,
        average_complexity,
        max_complexity,
    };

    tracing::debug!(
        total = total,
        max_complexity = max_complexity,
        "complexity_metrics completed"
    );

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(total as u64),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}
