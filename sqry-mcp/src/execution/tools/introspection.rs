//! Introspection tool execution.
//!
//! This module implements the introspection tools for listing files,
//! symbols, and graph statistics.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Result;

use crate::engine::engine_for_workspace;
use crate::execution::location::node_location_for_reporting;
use crate::execution::types::{
    CrateCacheEntry, ExpandCacheStatusData, FileEntryData, GraphStatsData, ListFilesData,
    ListSymbolsData, MacroBoundariesStatsData, SymbolEntryData, ToolExecution,
};
use crate::execution::utils::duration_to_ms;
use crate::tools::{ExpandCacheStatusArgs, GetGraphStatsArgs, ListFilesArgs, ListSymbolsArgs};

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

    for (node_id, entry) in graph.nodes().iter() {
        // Gate 0d iter-2 Major fix: skip Phase 4c-prime unified-away
        // losers. They remain in the arena as inert duplicates but
        // must not appear in `list_symbols` output — the prior bug
        // was that `StringId::INVALID` resolved via
        // `unwrap_or_default()` to an empty string and losers leaked
        // as blank/unnamed symbol rows. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            continue;
        }
        // Get node info
        let name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
            entry, strings, files, &name,
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
            let loc = node_location_for_reporting(&graph, node_id, &workspace_root);
            let line = loc.as_ref().map_or(entry.start_line, |l| l.line);

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

    // Count files by language, and track external file count
    let mut files_by_language: HashMap<String, u64> = HashMap::new();
    let mut classpath_file_count: u64 = 0;
    for (file_id, _path, language) in graph.files().iter_with_language() {
        let lang_str = language.map_or_else(|| "unknown".to_string(), |l| l.to_string());
        *files_by_language.entry(lang_str).or_insert(0) += 1;
        if graph.files().is_external(file_id) {
            classpath_file_count += 1;
        }
    }

    // Count workspace vs classpath nodes using a snapshot for iteration.
    // Gate 0d iter-2 fix: skip unified losers — they are inert duplicates
    // and must not be counted on any publish-visible surface.
    // See `NodeEntry::is_unified_loser`.
    let mut classpath_node_count: u64 = 0;
    {
        let snapshot = graph.snapshot();
        let files_ref = snapshot.files();
        for (_node_id, entry) in snapshot.iter_nodes() {
            if entry.is_unified_loser() {
                continue;
            }
            if files_ref.is_external(entry.file) {
                classpath_node_count += 1;
            }
        }
    }

    // Only include workspace/classpath breakdown if there are external files
    let has_classpath = classpath_file_count > 0 || classpath_node_count > 0;

    let data = GraphStatsData {
        total_nodes,
        total_edges,
        total_files,
        workspace_nodes: if has_classpath {
            Some(total_nodes.saturating_sub(classpath_node_count))
        } else {
            None
        },
        classpath_nodes: if has_classpath {
            Some(classpath_node_count)
        } else {
            None
        },
        workspace_files: if has_classpath {
            Some(total_files.saturating_sub(classpath_file_count))
        } else {
            None
        },
        classpath_files: if has_classpath {
            Some(classpath_file_count)
        } else {
            None
        },
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
        // Gate 0d iter-2 fix: skip unified losers from `get_insights`
        // symbol counts. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            continue;
        }
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
        if let (Some(from_entry), Some(to_entry)) =
            (snapshot.get_node(source_id), snapshot.get_node(target_id))
        {
            // Gate 0d iter-2 fix: skip edges whose endpoints are
            // unified losers. Phase 4c-prime `NodeRemapTable`
            // rewrites edges through `merge_node_into` so losers
            // should not appear as edge endpoints post-finalize, but
            // defensive filtering here keeps counts honest if any
            // path misses the remap.
            if from_entry.is_unified_loser() || to_entry.is_unified_loser() {
                continue;
            }

            total_edges += 1;

            let from_lang = files.language_for_file(from_entry.file);
            let to_lang = files.language_for_file(to_entry.file);

            if let (Some(fl), Some(tl)) = (from_lang, to_lang)
                && fl != tl
            {
                cross_language_edges += 1;
            }
        } else {
            // Endpoint resolution failed but the edge exists — still
            // count it as an edge (pre-iter-2 behavior).
            total_edges += 1;
        }
    }

    (total_edges, cross_language_edges)
}

/// Estimate the number of 2-hop cycles (A calls B, B calls A).
fn estimate_cycle_count(snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot) -> usize {
    let mut cycles = 0usize;
    for (node_id, entry) in snapshot.iter_nodes() {
        // Gate 0d iter-2 fix: skip unified losers. They have no
        // outgoing/incoming publish-visible edges (merged into
        // the winner), but defensive filter keeps counts honest.
        if entry.is_unified_loser() {
            continue;
        }
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
    for (node_id, entry) in snapshot.iter_nodes() {
        // Gate 0d iter-2 fix: skip unified losers from "unused"
        // counts. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            continue;
        }
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
#[allow(
    clippy::too_many_lines,
    reason = "aggregates multiple health metrics in a single pass; extraction into helpers would obscure the data-flow logic"
)]
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

    // Count duplicate groups (symbols with same name).
    // Gate 0d iter-2 fix: skip unified losers — their `name` is
    // `StringId::INVALID` which would resolve to empty / none, but
    // the explicit guard is clearer than relying on string-resolve
    // side-effects. See `NodeEntry::is_unified_loser`.
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    let strings = snapshot.strings();
    for (_node_id, entry) in snapshot.iter_nodes() {
        if entry.is_unified_loser() {
            continue;
        }
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

    // Compute macro boundary statistics from metadata store
    let macro_boundaries = {
        let meta_store = snapshot.macro_metadata();
        if meta_store.is_empty() {
            None
        } else {
            let mut attribute_macros_detected = 0usize;
            let mut cfg_gated_symbols = 0usize;
            let mut macro_generated_symbols = 0usize;
            let mut unresolved_attributes = 0usize;

            for (_, meta) in meta_store.iter() {
                if meta.proc_macro_kind
                    == Some(sqry_core::graph::unified::ProcMacroFunctionKind::Attribute)
                {
                    attribute_macros_detected += 1;
                }
                if meta.cfg_condition.is_some() {
                    cfg_gated_symbols += 1;
                }
                if meta.macro_generated == Some(true) {
                    macro_generated_symbols += 1;
                }
                unresolved_attributes += meta.unresolved_attributes.len();
            }

            // Check expand cache status
            let expand_cache_status = detect_expand_cache_status(&workspace_root);

            Some(MacroBoundariesStatsData {
                attribute_macros_detected,
                cfg_gated_symbols,
                macro_generated_symbols,
                unresolved_attributes,
                expand_cache_status,
            })
        }
    };

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
        macro_boundaries,
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

/// Detect the expand cache status for a workspace root.
///
/// Checks for `.sqry/expand-cache/` directory and returns `"fresh"`, `"stale"`, or `"absent"`.
fn detect_expand_cache_status(workspace_root: &std::path::Path) -> String {
    let cache_dir = workspace_root.join(".sqry/expand-cache");
    if !cache_dir.exists() {
        return "absent".to_string();
    }

    // Check if any .json files exist and whether they are recent (within 24 hours)
    let Ok(entries) = std::fs::read_dir(&cache_dir) else {
        return "absent".to_string();
    };

    let mut has_files = false;
    let mut all_fresh = true;
    let now = std::time::SystemTime::now();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            has_files = true;
            if let Ok(metadata) = entry.metadata()
                && let Ok(modified) = metadata.modified()
                && let Ok(age) = now.duration_since(modified)
                && age > std::time::Duration::from_secs(86400)
            {
                all_fresh = false;
            }
        }
    }

    if !has_files {
        "absent".to_string()
    } else if all_fresh {
        "fresh".to_string()
    } else {
        "stale".to_string()
    }
}

/// Execute the `expand_cache_status` tool to report macro expansion cache state.
pub fn execute_expand_cache_status(
    args: &ExpandCacheStatusArgs,
) -> Result<ToolExecution<ExpandCacheStatusData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(path = %args.path, "Executing expand_cache_status tool");

    let cache_dir = workspace_root.join(".sqry/expand-cache");
    let cache_exists = cache_dir.exists();
    let cache_path = crate::execution::symbol_utils::path_to_forward_slash(&cache_dir);

    let mut cache_files = 0usize;
    let mut total_size_bytes = 0u64;
    let mut crates: Vec<CrateCacheEntry> = Vec::new();

    if cache_exists && let Ok(entries) = std::fs::read_dir(&cache_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                cache_files += 1;
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                total_size_bytes += size;

                // Try to parse the cache file to extract crate info
                let file_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                let (crate_name, generated_symbols, confidence) = parse_cache_entry_summary(&path);

                crates.push(CrateCacheEntry {
                    crate_name,
                    file_name,
                    size_bytes: size,
                    generated_symbols,
                    confidence,
                });
            }
        }
    }

    // Sort crates by name for deterministic output
    crates.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));

    let status = detect_expand_cache_status(&workspace_root);

    let data = ExpandCacheStatusData {
        cache_exists,
        cache_path,
        cache_files,
        total_size_bytes,
        crates,
        status,
    };

    tracing::debug!(
        cache_exists = cache_exists,
        cache_files = cache_files,
        "expand_cache_status completed"
    );

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: false,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(cache_files as u64),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Parse a cache JSON file to extract summary info (crate name, symbol count, confidence).
fn parse_cache_entry_summary(path: &std::path::Path) -> (String, usize, String) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return (String::new(), 0, "unknown".to_string());
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) else {
        return (String::new(), 0, "unknown".to_string());
    };

    let crate_name = json
        .get("crate_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut generated_count = 0usize;
    if let Some(files) = json.get("files").and_then(|v| v.as_object()) {
        for (_file_path, file_data) in files {
            if let Some(syms) = file_data
                .get("generated_symbols")
                .and_then(|v| v.as_array())
            {
                generated_count += syms.len();
            }
        }
    }

    let confidence = json
        .get("files")
        .and_then(|v| v.as_object())
        .and_then(|files| {
            files
                .values()
                .next()
                .and_then(|f| f.get("confidence"))
                .and_then(|c| c.as_str())
        })
        .unwrap_or("unknown")
        .to_string();

    (crate_name, generated_count, confidence)
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
    let qualified_name =
        crate::execution::symbol_utils::display_entry_qualified_name(entry, strings, files, &name);
    let file_path =
        crate::execution::symbol_utils::path_to_forward_slash(&files.resolve(entry.file)?);

    if let Some(filter) = target_filter
        && !matches_complexity_filter(&file_path, &name, &qualified_name, filter)
    {
        return None;
    }

    // NOTE: These raw entry spans are used for line-count computation and complexity
    // estimation, not for user-visible display. Do NOT migrate to node_location_for_reporting.
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
///
/// # Dispatch path (DB17)
///
/// `complexity_metrics` remains a **direct linear scan** over the
/// snapshot — it is NOT migrated to sqry-db in DB17.
///
/// ## Why no sqry-db route?
///
/// The DB17 audit confirmed that sqry-db currently exposes no
/// `ComplexityQuery` / `ComplexityMetricsQuery` / `CyclomaticComplexityQuery`
/// derived query (see `sqry-db/src/queries/mod.rs` — the published query
/// set is `SccQuery`, `CondensationQuery`, `ReachabilityQuery`, the
/// relation family, `CyclesQuery`/`IsInCycleQuery`, and the unused/
/// entry-point family only). DB17 scope is migration, not new query
/// invention; introducing a `ComplexityQuery` is out of scope and
/// belongs to a dedicated future unit (see the DB17 DAG entry at
/// `docs/superpowers/plans/2026-04-12-phase3-4-combined-implementation-dag.toml`).
///
/// Semantically, `complexity_metrics` is a mixed question: a global
/// linear scan over [`sqry_core::graph::unified::concurrent::GraphSnapshot::iter_nodes`]
/// with per-node cyclomatic complexity computation. Each per-node
/// answer depends on the node's own AST shape (fan-out + branch
/// metadata), not on any cross-node relation. That is structurally
/// different from the edge-revision-indexed predicates sqry-db caches
/// (which are valuable because recomputing them from scratch every
/// call would require re-walking the whole graph). For complexity,
/// each per-node answer is already local to one node's metadata —
/// there is no shared substructure to cache, so the planner-canonical
/// gain of a derived query is marginal.
///
/// If this ever shifts (e.g. the metric becomes cross-node, or the
/// per-node cost becomes non-trivial), this handler should migrate via
/// the DB15 pattern: [`crate::execution::relation_dispatch::make_query_db`]
/// plus an `execute`-dispatching predicate. Until then, the linear
/// scan here is both correct and the architecturally correct layer.
///
/// ## Shape
///
/// Iterates every node via `snapshot.iter_nodes()`, skips
/// macro-generated symbols (those are compiler output, not human code),
/// computes the per-node complexity via
/// [`collect_complexity_metric`], and collects the rows that meet
/// `args.min_complexity`. Post-filters by `args.target` (substring
/// match on symbol name / file path) inside
/// [`collect_complexity_metric`].
pub fn execute_complexity_metrics(
    args: &crate::tools::ComplexityMetricsArgs,
) -> Result<ToolExecution<super::super::types::ComplexityMetricsData>> {
    // Pre-refactor timing: `start` fires before engine resolution.
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

    let ctx = crate::daemon_adapter::WorkspaceContext {
        workspace_root,
        graph,
        executor: engine.executor_arc(),
    };
    inner::execute_complexity_metrics(&ctx, args, start)
}

pub(crate) mod inner {
    use super::{
        Result, ToolExecution, collect_complexity_metric, duration_to_ms,
        summarize_complexity_metrics,
    };
    use crate::daemon_adapter::WorkspaceContext;
    use std::time::Instant;

    /// Daemon/SqryServer-shared body for `complexity_metrics`.
    pub(crate) fn execute_complexity_metrics(
        ctx: &WorkspaceContext,
        args: &crate::tools::ComplexityMetricsArgs,
        start: Instant,
    ) -> Result<ToolExecution<super::super::super::types::ComplexityMetricsData>> {
        use super::super::super::types::ComplexityMetricData;

        let snapshot = ctx.graph.snapshot();
        let target_filter = args.target.as_ref().map(|target| target.to_lowercase());

        let mut metrics: Vec<ComplexityMetricData> = Vec::new();

        let macro_meta_store = snapshot.macro_metadata();

        for (node_id, entry) in snapshot.iter_nodes() {
            // Gate 0d iter-2 fix: skip unified losers from
            // `complexity_metrics`. See `NodeEntry::is_unified_loser`.
            if entry.is_unified_loser() {
                continue;
            }
            // Skip macro-generated symbols — they are compiler output, not human code
            if let Some(meta) = macro_meta_store.get_macro(node_id)
                && meta.macro_generated == Some(true)
            {
                continue;
            }

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
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(
                &ctx.workspace_root,
            ),
        })
    }
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

    // ===== macro boundary tests =====

    #[test]
    fn test_insights_macro_boundaries_stats() {
        use sqry_core::graph::unified::MacroNodeMetadata;

        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let name1 = graph.strings_mut().intern("my_fn").unwrap();
        let name2 = graph.strings_mut().intern("gen_fn").unwrap();
        let name3 = graph.strings_mut().intern("cfg_fn").unwrap();
        let file = graph
            .files_mut()
            .register(&workspace_root.join("src/test.rs"))
            .unwrap();

        let entry1 = NodeEntry::new(NodeKind::Function, name1, file);
        let node1 = graph.nodes_mut().alloc(entry1).unwrap();

        let entry2 = NodeEntry::new(NodeKind::Function, name2, file);
        let node2 = graph.nodes_mut().alloc(entry2).unwrap();

        let entry3 = NodeEntry::new(NodeKind::Function, name3, file);
        let node3 = graph.nodes_mut().alloc(entry3).unwrap();

        // Node 1: has proc_macro_kind (attribute macro detected)
        graph.macro_metadata_mut().insert(
            node1,
            MacroNodeMetadata {
                proc_macro_kind: Some(sqry_core::graph::unified::ProcMacroFunctionKind::Attribute),
                unresolved_attributes: vec!["unknown_attr".to_string()],
                ..Default::default()
            },
        );

        // Node 2: macro generated
        graph.macro_metadata_mut().insert(
            node2,
            MacroNodeMetadata {
                macro_generated: Some(true),
                macro_source: Some("derive_Debug".to_string()),
                ..Default::default()
            },
        );

        // Node 3: cfg gated
        graph.macro_metadata_mut().insert(
            node3,
            MacroNodeMetadata {
                cfg_condition: Some("test".to_string()),
                cfg_active: Some(true),
                ..Default::default()
            },
        );

        let snapshot = graph.snapshot();
        let meta_store = snapshot.macro_metadata();

        assert!(!meta_store.is_empty());

        let mut attribute_macros = 0;
        let mut cfg_gated = 0;
        let mut macro_generated = 0;
        let mut unresolved = 0;

        for (_, meta) in meta_store.iter() {
            if meta.proc_macro_kind.is_some() {
                attribute_macros += 1;
            }
            if meta.cfg_condition.is_some() {
                cfg_gated += 1;
            }
            if meta.macro_generated == Some(true) {
                macro_generated += 1;
            }
            unresolved += meta.unresolved_attributes.len();
        }

        assert_eq!(attribute_macros, 1);
        assert_eq!(cfg_gated, 1);
        assert_eq!(macro_generated, 1);
        assert_eq!(unresolved, 1);
    }

    /// Gate 0d iter-2 regression: MCP `list_symbols` MUST skip Phase
    /// 4c-prime unified losers. Before the fix,
    /// `introspection.rs:171` walked `graph.nodes().iter()` with no
    /// `is_unified_loser()` guard and `StringId::INVALID` resolved to
    /// an empty string via `unwrap_or_default()`, leaking losers as
    /// blank/unnamed symbol rows.
    ///
    /// We reproduce the iteration pattern directly (rather than
    /// going through `execute_list_symbols`, which needs an engine +
    /// workspace). The loser is constructed by directly mutating the
    /// arena entry to the `NodeEntry::is_unified_loser` sentinel —
    /// this is the same end-state `merge_node_into` leaves in the
    /// arena, just without invoking that `pub(crate)` helper from
    /// outside sqry-core.
    #[test]
    fn list_symbols_iteration_pattern_excludes_unified_losers() {
        use sqry_core::graph::unified::node::NodeId;
        use sqry_core::graph::unified::string::StringId;

        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let name = graph.strings_mut().intern("shared_fn").unwrap();
        let qn = graph.strings_mut().intern("mod::shared_fn").unwrap();
        let file_a = graph
            .files_mut()
            .register(&workspace_root.join("src/a.rs"))
            .unwrap();
        let file_b = graph
            .files_mut()
            .register(&workspace_root.join("src/b.rs"))
            .unwrap();

        let (winner_id, loser_id): (NodeId, NodeId) = {
            let arena = graph.nodes_mut();
            let mut w =
                NodeEntry::new(NodeKind::Function, name, file_a).with_location(10, 0, 20, 0);
            w.qualified_name = Some(qn);
            let w_id = arena.alloc(w).unwrap();
            let mut l = NodeEntry::new(NodeKind::Function, name, file_b).with_location(5, 0, 15, 0);
            l.qualified_name = Some(qn);
            let l_id = arena.alloc(l).unwrap();
            (w_id, l_id)
        };
        graph.files_mut().record_node(file_a, winner_id);
        graph.files_mut().record_node(file_b, loser_id);

        // Simulate the end-state `merge_node_into` leaves in the
        // arena for a loser (name == INVALID, qualified_name = None,
        // content-addressable fields cleared). We cannot call the
        // `pub(crate)` merge helper from here.
        {
            let arena = graph.nodes_mut();
            let l_mut = arena.get_mut(loser_id).expect("loser present");
            l_mut.name = StringId::INVALID;
            l_mut.qualified_name = None;
            l_mut.signature = None;
            l_mut.body_hash = None;
            l_mut.doc = None;
            l_mut.visibility = None;
        }
        graph.rebuild_indices();

        // Sanity: the loser is marked.
        assert!(
            graph
                .nodes()
                .get(loser_id)
                .expect("loser")
                .is_unified_loser()
        );

        // Reproduce the exact filter pattern used by `execute_list_symbols`.
        let mut visible: Vec<NodeId> = Vec::new();
        for (node_id, entry) in graph.nodes().iter() {
            if entry.is_unified_loser() {
                continue;
            }
            visible.push(node_id);
        }

        assert!(
            visible.contains(&winner_id),
            "list_symbols iteration must yield the winner"
        );
        assert!(
            !visible.contains(&loser_id),
            "list_symbols iteration must NOT yield the unified loser \
             (iter-2 blocker)"
        );
        assert_eq!(
            visible.iter().filter(|&&id| id == winner_id).count(),
            1,
            "winner must appear exactly once"
        );
    }

    #[test]
    fn test_complexity_excludes_generated() {
        use sqry_core::graph::unified::MacroNodeMetadata;

        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let name_human = graph.strings_mut().intern("human_fn").unwrap();
        let name_generated = graph.strings_mut().intern("generated_fn").unwrap();
        let file = graph
            .files_mut()
            .register(&workspace_root.join("src/lib.rs"))
            .unwrap();

        let entry_human =
            NodeEntry::new(NodeKind::Function, name_human, file).with_location(1, 0, 50, 0);
        let _node_human = graph.nodes_mut().alloc(entry_human.clone()).unwrap();

        let entry_generated =
            NodeEntry::new(NodeKind::Function, name_generated, file).with_location(51, 0, 100, 0);
        let node_generated = graph.nodes_mut().alloc(entry_generated.clone()).unwrap();

        // Mark the second node as macro-generated
        graph.macro_metadata_mut().insert(
            node_generated,
            MacroNodeMetadata {
                macro_generated: Some(true),
                ..Default::default()
            },
        );

        let snapshot = graph.snapshot();
        let meta_store = snapshot.macro_metadata();

        // Simulate the complexity collection loop with generated-exclusion logic
        let mut metrics = Vec::new();
        for (node_id, entry) in snapshot.iter_nodes() {
            if let Some(meta) = meta_store.get_macro(node_id)
                && meta.macro_generated == Some(true)
            {
                continue;
            }
            if let Some(metric) = collect_complexity_metric(&snapshot, node_id, entry, None) {
                metrics.push(metric);
            }
        }

        assert_eq!(
            metrics.len(),
            1,
            "Should only have 1 metric (generated excluded)"
        );
        assert_eq!(metrics[0].name, "human_fn");
    }

    #[test]
    fn test_detect_expand_cache_status_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let status = detect_expand_cache_status(tmp.path());
        assert_eq!(status, "absent");
    }

    #[test]
    fn test_detect_expand_cache_status_with_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join(".sqry/expand-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        // Create a cache file
        let cache_file = cache_dir.join("my_crate.json");
        std::fs::write(&cache_file, r#"{"crate_name": "my_crate", "files": {}}"#).unwrap();

        let status = detect_expand_cache_status(tmp.path());
        assert_eq!(status, "fresh");
    }

    #[test]
    fn test_parse_cache_entry_summary_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_file = tmp.path().join("test_crate.json");
        std::fs::write(
            &cache_file,
            r#"{
                "crate_name": "my_crate",
                "files": {
                    "src/lib.rs": {
                        "generated_symbols": ["my_crate::<Debug>::fmt", "my_crate::clone"],
                        "confidence": "heuristic"
                    }
                }
            }"#,
        )
        .unwrap();

        let (name, count, confidence) = parse_cache_entry_summary(&cache_file);
        assert_eq!(name, "my_crate");
        assert_eq!(count, 2);
        assert_eq!(confidence, "heuristic");
    }

    #[test]
    fn test_parse_cache_entry_summary_invalid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_file = tmp.path().join("bad.json");
        std::fs::write(&cache_file, "not json").unwrap();

        let (name, count, confidence) = parse_cache_entry_summary(&cache_file);
        assert!(name.is_empty());
        assert_eq!(count, 0);
        assert_eq!(confidence, "unknown");
    }

    #[test]
    fn test_parse_cache_entry_summary_nonexistent() {
        let (name, count, confidence) =
            parse_cache_entry_summary(std::path::Path::new("/nonexistent/file.json"));
        assert!(name.is_empty());
        assert_eq!(count, 0);
        assert_eq!(confidence, "unknown");
    }
}
