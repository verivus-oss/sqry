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
        let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
            entry,
            strings,
            files.language_for_file(entry.file),
            &name,
        );

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

fn collect_complexity_metric(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node_id: sqry_core::graph::unified::node::NodeId,
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    target_filter: Option<&str>,
) -> Option<super::super::types::ComplexityMetricData> {
    use super::super::types::ComplexityMetricData;
    use sqry_core::graph::unified::node::NodeKind;

    if !matches!(entry.kind, NodeKind::Function | NodeKind::Method) {
        return None;
    }

    let strings = snapshot.strings();
    let files = snapshot.files();
    let name = strings.resolve(entry.name)?.to_string();
    let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
        entry,
        strings,
        files.language_for_file(entry.file),
        &name,
    );
    let file_path =
        crate::execution::symbol_utils::path_to_forward_slash(&files.resolve(entry.file)?);

    if let Some(filter) = target_filter
        && !matches_complexity_filter(&file_path, &name, &qualified_name, filter)
    {
        return None;
    }

    let lines = entry
        .end_line
        .saturating_sub(entry.start_line)
        .saturating_add(1);
    let complexity =
        compute_estimated_complexity(snapshot, node_id, entry.start_line, entry.end_line);

    Some(ComplexityMetricData {
        name,
        qualified_name,
        kind: format!("{:?}", entry.kind).to_lowercase(),
        file_path,
        complexity,
        lines,
    })
}

fn summarize_complexity_metrics(
    metrics: Vec<super::super::types::ComplexityMetricData>,
) -> super::super::types::ComplexityMetricsData {
    use super::super::types::ComplexityMetricsData;

    let total = metrics.len();
    let max_complexity = metrics
        .iter()
        .map(|metric| metric.complexity)
        .max()
        .unwrap_or(0);
    let average_complexity = if metrics.is_empty() {
        0.0
    } else {
        let count = f64::from(u32::try_from(metrics.len()).unwrap_or(u32::MAX));
        metrics
            .iter()
            .map(|metric| f64::from(metric.complexity))
            .sum::<f64>()
            / count
    };

    ComplexityMetricsData {
        metrics,
        total,
        average_complexity,
        max_complexity,
    }
}

/// Execute the `complexity_metrics` tool to analyze code complexity.
pub fn execute_complexity_metrics(
    args: &crate::tools::ComplexityMetricsArgs,
) -> Result<ToolExecution<super::super::types::ComplexityMetricsData>> {
    use super::super::types::ComplexityMetricData;

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
    let target_filter = args.target.as_ref().map(|target| target.to_lowercase());

    let mut metrics: Vec<ComplexityMetricData> = Vec::new();

    for (node_id, entry) in snapshot.iter_nodes() {
        let Some(metric) =
            collect_complexity_metric(&snapshot, node_id, entry, target_filter.as_deref())
        else {
            continue;
        };

        if metric.complexity >= args.min_complexity {
            metrics.push(metric);
        }
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
    let data = summarize_complexity_metrics(metrics);
    let total = data.total;
    let max_complexity = data.max_complexity;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::types::ComplexityMetricData;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;
    use std::path::PathBuf;

    fn test_workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn make_metric(name: &str, complexity: u32, lines: u32) -> ComplexityMetricData {
        ComplexityMetricData {
            name: name.to_string(),
            qualified_name: name.to_string(),
            kind: "function".to_string(),
            file_path: "src/main.rs".to_string(),
            complexity,
            lines,
        }
    }

    // ===== summarize_complexity_metrics tests =====

    #[test]
    fn summarize_empty_list_returns_zero_totals() {
        let result = summarize_complexity_metrics(vec![]);
        assert_eq!(result.total, 0);
        assert_eq!(result.max_complexity, 0);
        assert!((result.average_complexity - 0.0).abs() < f64::EPSILON);
        assert!(result.metrics.is_empty());
    }

    #[test]
    fn summarize_single_metric() {
        let metrics = vec![make_metric("foo", 5, 20)];
        let result = summarize_complexity_metrics(metrics);
        assert_eq!(result.total, 1);
        assert_eq!(result.max_complexity, 5);
        assert!((result.average_complexity - 5.0).abs() < f64::EPSILON);
        assert_eq!(result.metrics.len(), 1);
        assert_eq!(result.metrics[0].name, "foo");
    }

    #[test]
    fn summarize_multiple_metrics_computes_max_and_average() {
        let metrics = vec![
            make_metric("low", 2, 10),
            make_metric("mid", 6, 30),
            make_metric("high", 10, 50),
        ];
        let result = summarize_complexity_metrics(metrics);
        assert_eq!(result.total, 3);
        assert_eq!(result.max_complexity, 10);
        // Average = (2 + 6 + 10) / 3 = 6.0
        assert!((result.average_complexity - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn summarize_preserves_metric_order() {
        let metrics = vec![make_metric("z_last", 1, 5), make_metric("a_first", 3, 15)];
        let result = summarize_complexity_metrics(metrics);
        assert_eq!(result.metrics[0].name, "z_last");
        assert_eq!(result.metrics[1].name, "a_first");
    }

    // ===== matches_complexity_filter tests =====

    #[test]
    fn matches_complexity_filter_by_file_path() {
        assert!(matches_complexity_filter(
            "src/main.rs",
            "foo",
            "mod::foo",
            "main"
        ));
    }

    #[test]
    fn matches_complexity_filter_by_name() {
        assert!(matches_complexity_filter(
            "src/lib.rs",
            "process_data",
            "mod::process_data",
            "process"
        ));
    }

    #[test]
    fn matches_complexity_filter_by_qualified_name() {
        assert!(matches_complexity_filter(
            "src/lib.rs",
            "run",
            "engine::run",
            "engine"
        ));
    }

    #[test]
    fn matches_complexity_filter_case_insensitive() {
        // Fields are lowercased internally, but the filter is expected pre-lowered
        // (callers lowercase the target before passing it in)
        assert!(matches_complexity_filter(
            "src/Main.rs",
            "Foo",
            "Mod::Foo",
            "foo"
        ));
        assert!(matches_complexity_filter(
            "src/Main.rs",
            "FooBar",
            "Mod::FooBar",
            "foobar"
        ));
    }

    #[test]
    fn matches_complexity_filter_no_match() {
        assert!(!matches_complexity_filter(
            "src/lib.rs",
            "run",
            "engine::run",
            "xyz"
        ));
    }

    // ===== collect_complexity_metric tests =====

    #[test]
    fn collect_complexity_metric_skips_non_function_kinds() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let name = graph.strings_mut().intern("MyStruct").unwrap();
        let file = graph
            .files_mut()
            .register(&workspace_root.join("src/lib.rs"))
            .unwrap();
        let entry = NodeEntry::new(NodeKind::Struct, name, file);
        let node_id = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let result = collect_complexity_metric(&snapshot, node_id, &entry, None);
        assert!(result.is_none());
    }

    #[test]
    fn collect_complexity_metric_returns_function() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let name = graph.strings_mut().intern("process").unwrap();
        let file = graph
            .files_mut()
            .register(&workspace_root.join("src/lib.rs"))
            .unwrap();
        let entry = NodeEntry::new(NodeKind::Function, name, file).with_location(10, 0, 30, 0);
        let node_id = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let result = collect_complexity_metric(&snapshot, node_id, &entry, None);
        assert!(result.is_some());
        let metric = result.unwrap();
        assert_eq!(metric.name, "process");
        assert_eq!(metric.kind, "function");
        assert_eq!(metric.lines, 21); // 30 - 10 + 1
        assert!(metric.complexity >= 1);
    }

    #[test]
    fn collect_complexity_metric_returns_method() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let name = graph.strings_mut().intern("run").unwrap();
        let file = graph
            .files_mut()
            .register(&workspace_root.join("src/engine.rs"))
            .unwrap();
        let entry = NodeEntry::new(NodeKind::Method, name, file).with_location(5, 0, 15, 0);
        let node_id = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let result = collect_complexity_metric(&snapshot, node_id, &entry, None);
        assert!(result.is_some());
        let metric = result.unwrap();
        assert_eq!(metric.name, "run");
        assert_eq!(metric.kind, "method");
    }

    #[test]
    fn collect_complexity_metric_filters_by_target() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let name = graph.strings_mut().intern("handle").unwrap();
        let file = graph
            .files_mut()
            .register(&workspace_root.join("src/server.rs"))
            .unwrap();
        let entry = NodeEntry::new(NodeKind::Function, name, file).with_location(1, 0, 10, 0);
        let node_id = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();

        // Filter matches name
        let result = collect_complexity_metric(&snapshot, node_id, &entry, Some("handle"));
        assert!(result.is_some());

        // Filter matches file path
        let result = collect_complexity_metric(&snapshot, node_id, &entry, Some("server"));
        assert!(result.is_some());

        // Filter does not match
        let result = collect_complexity_metric(&snapshot, node_id, &entry, Some("nonexistent"));
        assert!(result.is_none());
    }

    #[test]
    fn collect_complexity_metric_filter_case_insensitive() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let name = graph.strings_mut().intern("ProcessData").unwrap();
        let file = graph
            .files_mut()
            .register(&workspace_root.join("src/handler.rs"))
            .unwrap();
        let entry = NodeEntry::new(NodeKind::Function, name, file).with_location(1, 0, 5, 0);
        let node_id = graph.nodes_mut().alloc(entry.clone()).unwrap();

        let snapshot = graph.snapshot();
        let result = collect_complexity_metric(&snapshot, node_id, &entry, Some("processdata"));
        assert!(result.is_some());
    }

    // ===== compute_estimated_complexity tests =====

    #[test]
    fn compute_estimated_complexity_baseline() {
        let mut graph = CodeGraph::new();
        let name = graph.strings_mut().intern("f").unwrap();
        let file = graph
            .files_mut()
            .register(&test_workspace_root().join("src/f.rs"))
            .unwrap();
        let entry = NodeEntry::new(NodeKind::Function, name, file);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();

        // 0 lines span (start==end), 0 callees: 1 + 0/5 + 1/20 = 1
        let c = compute_estimated_complexity(&snapshot, node_id, 10, 10);
        assert_eq!(c, 1);
    }

    #[test]
    fn compute_estimated_complexity_with_lines() {
        let mut graph = CodeGraph::new();
        let name = graph.strings_mut().intern("f").unwrap();
        let file = graph
            .files_mut()
            .register(&test_workspace_root().join("src/f.rs"))
            .unwrap();
        let entry = NodeEntry::new(NodeKind::Function, name, file);
        let node_id = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();

        // 100 lines: 1 + 0/5 + 101/20 = 1 + 5 = 6
        let c = compute_estimated_complexity(&snapshot, node_id, 0, 100);
        assert_eq!(c, 6);
    }

    // ===== resolve_workspace_path tests =====

    #[test]
    fn resolve_workspace_path_dot_returns_none() {
        assert!(resolve_workspace_path(".").is_none());
    }

    #[test]
    fn resolve_workspace_path_explicit_returns_some() {
        let result = resolve_workspace_path("/some/path");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), PathBuf::from("/some/path"));
    }

    // ===== matches_language_filter tests =====

    #[test]
    fn matches_language_filter_exact_match() {
        assert!(matches_language_filter(Some("Rust"), "rust"));
    }

    #[test]
    fn matches_language_filter_partial_match() {
        assert!(matches_language_filter(Some("TypeScript"), "script"));
    }

    #[test]
    fn matches_language_filter_no_match() {
        assert!(!matches_language_filter(Some("Python"), "rust"));
    }

    #[test]
    fn matches_language_filter_none_language() {
        assert!(!matches_language_filter(None, "rust"));
    }

    // ===== matches_symbol_filters tests =====

    #[test]
    fn matches_symbol_filters_no_filters() {
        let args = ListSymbolsArgs {
            path: ".".to_string(),
            kind: None,
            language: None,
            max_results: 100,
            pagination: crate::tools::PaginationArgs {
                offset: 0,
                size: 100,
            },
        };
        assert!(matches_symbol_filters("Function", "rust", &args));
    }

    #[test]
    fn matches_symbol_filters_kind_filter() {
        let args = ListSymbolsArgs {
            path: ".".to_string(),
            kind: Some("function".to_string()),
            language: None,
            max_results: 100,
            pagination: crate::tools::PaginationArgs {
                offset: 0,
                size: 100,
            },
        };
        assert!(matches_symbol_filters("Function", "rust", &args));
        assert!(!matches_symbol_filters("Struct", "rust", &args));
    }

    #[test]
    fn matches_symbol_filters_language_filter() {
        let args = ListSymbolsArgs {
            path: ".".to_string(),
            kind: None,
            language: Some("python".to_string()),
            max_results: 100,
            pagination: crate::tools::PaginationArgs {
                offset: 0,
                size: 100,
            },
        };
        assert!(matches_symbol_filters("Function", "python", &args));
        assert!(!matches_symbol_filters("Function", "rust", &args));
    }
}
