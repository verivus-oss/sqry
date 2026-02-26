//! Graph-based query commands using the unified graph architecture
//!
//! This module implements CLI commands for advanced graph operations:
//! - trace-path: Find shortest path between symbols
//! - call-chain-depth: Calculate maximum call chain depth
//! - dependency-tree: Show transitive dependencies
//! - nodes: List nodes from the unified graph
//! - edges: List edges from the unified graph
//! - cross-language: List cross-language relationships
//! - stats: Show graph statistics
//!
//! # Migration Status (FR-2025-007)
//!
//! All graph operations use the unified `CodeGraph` architecture.
//! Legacy graph code has been removed.

pub mod loader;

use crate::args::{Cli, GraphOperation};
use anyhow::{Context, Result, bail};
use loader::GraphLoadConfig;
pub use loader::load_unified_graph;
use sqry_core::graph::Language;
// Unified graph types
use sqry_core::graph::CodeGraph as UnifiedCodeGraph;
use sqry_core::graph::unified::edge::EdgeKind as UnifiedEdgeKind;
use sqry_core::graph::unified::{MqProtocol, NodeEntry, NodeKind as UnifiedNodeKind, StringId};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

type UnifiedGraphSnapshot = sqry_core::graph::unified::concurrent::GraphSnapshot;

/// Run a graph-based query command.
///
/// # Errors
/// Returns an error if the unified graph cannot be loaded or the query fails.
#[allow(clippy::too_many_lines)] // Single dispatch for graph ops; splitting obscures flow.
pub fn run_graph(
    cli: &Cli,
    operation: &GraphOperation,
    search_path: &str,
    format: &str,
    verbose: bool,
) -> Result<()> {
    let root = PathBuf::from(search_path);

    // Handle Status command early - it checks the unified graph, not the legacy graph
    if matches!(operation, GraphOperation::Status) {
        return super::run_graph_status(cli, search_path);
    }

    let config = build_graph_load_config(cli);
    let unified_graph =
        load_unified_graph(&root, &config).context("Failed to load unified graph")?;

    match operation {
        GraphOperation::Stats {
            by_file,
            by_language,
        } => run_stats_unified(&unified_graph, *by_file, *by_language, format),
        GraphOperation::TracePath {
            from,
            to,
            languages,
            full_paths,
        } => run_trace_path_unified(
            &unified_graph,
            from,
            to,
            languages.as_deref(),
            *full_paths,
            format,
            verbose,
        ),
        GraphOperation::Cycles {
            min_length,
            max_length,
            imports_only,
            languages,
        } => run_cycles_unified(
            &unified_graph,
            *min_length,
            *max_length,
            *imports_only,
            languages.as_deref(),
            format,
            verbose,
        ),
        GraphOperation::CallChainDepth {
            symbol,
            languages,
            show_chain,
        } => run_call_chain_depth_unified(
            &unified_graph,
            symbol,
            languages.as_deref(),
            *show_chain,
            format,
            verbose,
        ),
        GraphOperation::DependencyTree {
            module,
            max_depth,
            cycles_only,
        } => run_dependency_tree_unified(
            &unified_graph,
            module,
            *max_depth,
            *cycles_only,
            format,
            verbose,
        ),
        GraphOperation::CrossLanguage {
            from_lang,
            to_lang,
            edge_type,
            min_confidence,
        } => run_cross_language_unified(
            &unified_graph,
            from_lang.as_deref(),
            to_lang.as_deref(),
            edge_type.as_deref(),
            *min_confidence,
            format,
            verbose,
        ),
        GraphOperation::Nodes {
            kind,
            languages,
            file,
            name,
            qualified_name,
            limit,
            offset,
            full_paths,
        } => run_nodes_unified(
            &unified_graph,
            root.as_path(),
            &NodeFilterOptions {
                kind: kind.as_deref(),
                languages: languages.as_deref(),
                file: file.as_deref(),
                name: name.as_deref(),
                qualified_name: qualified_name.as_deref(),
            },
            &PaginationOptions {
                limit: *limit,
                offset: *offset,
            },
            &OutputOptions {
                full_paths: *full_paths,
                format,
                verbose,
            },
        ),
        GraphOperation::Edges {
            kind,
            from,
            to,
            from_lang,
            to_lang,
            file,
            limit,
            offset,
            full_paths,
        } => run_edges_unified(
            &unified_graph,
            root.as_path(),
            &EdgeFilterOptions {
                kind: kind.as_deref(),
                from: from.as_deref(),
                to: to.as_deref(),
                from_lang: from_lang.as_deref(),
                to_lang: to_lang.as_deref(),
                file: file.as_deref(),
            },
            &PaginationOptions {
                limit: *limit,
                offset: *offset,
            },
            &OutputOptions {
                full_paths: *full_paths,
                format,
                verbose,
            },
        ),
        GraphOperation::Complexity {
            target,
            sort_complexity,
            min_complexity,
            languages,
        } => run_complexity_unified(
            &unified_graph,
            target.as_deref(),
            *sort_complexity,
            *min_complexity,
            languages.as_deref(),
            format,
            verbose,
        ),
        GraphOperation::DirectCallers {
            symbol,
            limit,
            languages,
            full_paths,
        } => run_direct_callers_unified(
            &unified_graph,
            root.as_path(),
            &DirectCallOptions {
                symbol,
                limit: *limit,
                languages: languages.as_deref(),
                full_paths: *full_paths,
                format,
                verbose,
            },
        ),
        GraphOperation::DirectCallees {
            symbol,
            limit,
            languages,
            full_paths,
        } => run_direct_callees_unified(
            &unified_graph,
            root.as_path(),
            &DirectCallOptions {
                symbol,
                limit: *limit,
                languages: languages.as_deref(),
                full_paths: *full_paths,
                format,
                verbose,
            },
        ),
        GraphOperation::CallHierarchy {
            symbol,
            depth,
            direction,
            languages,
            full_paths,
        } => run_call_hierarchy_unified(
            &unified_graph,
            root.as_path(),
            &CallHierarchyOptions {
                symbol,
                max_depth: *depth,
                direction,
                languages: languages.as_deref(),
                full_paths: *full_paths,
                format,
                verbose,
            },
        ),
        GraphOperation::IsInCycle {
            symbol,
            cycle_type,
            show_cycle,
        } => run_is_in_cycle_unified(
            &unified_graph,
            symbol,
            cycle_type,
            *show_cycle,
            format,
            verbose,
        ),
        GraphOperation::Status => {
            unreachable!("Status is handled before loading the unified graph in run_graph")
        }
    }
}

fn build_graph_load_config(cli: &Cli) -> GraphLoadConfig {
    GraphLoadConfig {
        include_hidden: cli.hidden,
        follow_symlinks: cli.follow,
        max_depth: if cli.max_depth == 0 {
            None
        } else {
            Some(cli.max_depth)
        },
        force_build: false, // Load from snapshot if available
    }
}

fn resolve_node_name(snapshot: &UnifiedGraphSnapshot, entry: &NodeEntry) -> String {
    snapshot
        .strings()
        .resolve(entry.name)
        .map_or_else(|| "?".to_string(), |s| s.to_string())
}

fn resolve_node_label(snapshot: &UnifiedGraphSnapshot, entry: &NodeEntry) -> String {
    entry
        .qualified_name
        .and_then(|id| snapshot.strings().resolve(id))
        .or_else(|| snapshot.strings().resolve(entry.name))
        .map_or_else(|| "?".to_string(), |s| s.to_string())
}

fn resolve_node_language(snapshot: &UnifiedGraphSnapshot, entry: &NodeEntry) -> String {
    snapshot
        .files()
        .language_for_file(entry.file)
        .map_or_else(|| "Unknown".to_string(), |l| format!("{l:?}"))
}

fn resolve_node_file_path(
    snapshot: &UnifiedGraphSnapshot,
    entry: &NodeEntry,
    full_paths: bool,
) -> String {
    snapshot.files().resolve(entry.file).map_or_else(
        || "unknown".to_string(),
        |p| {
            if full_paths {
                p.to_string_lossy().to_string()
            } else {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            }
        },
    )
}

fn resolve_node_label_by_id(
    snapshot: &UnifiedGraphSnapshot,
    node_id: UnifiedNodeId,
) -> Option<String> {
    snapshot
        .get_node(node_id)
        .map(|entry| resolve_node_label(snapshot, entry))
}
/// Print unified graph statistics.
///
/// - Node count
/// - Edge count
/// - Cross-language edge count
/// - Edge kind totals
/// - Optional file/language breakdowns
fn run_stats_unified(
    graph: &UnifiedCodeGraph,
    by_file: bool,
    by_language: bool,
    format: &str,
) -> Result<()> {
    let snapshot = graph.snapshot();

    // Fast path: Skip expensive edge iteration for basic stats only
    let compute_detailed = by_file || by_language;
    let (node_count, edge_count, cross_language_count, kind_counts) =
        collect_edge_stats_unified(&snapshot, compute_detailed);

    let lang_counts = if by_language {
        collect_language_counts_unified(&snapshot)
    } else {
        HashMap::new()
    };
    let file_counts = if by_file {
        collect_file_counts_unified(&snapshot)
    } else {
        HashMap::new()
    };
    let file_count = snapshot.files().len();

    // Build stats and options structs
    let stats = GraphStats {
        node_count,
        edge_count,
        cross_language_count,
        kind_counts: &kind_counts,
        lang_counts: &lang_counts,
        file_counts: &file_counts,
        file_count,
    };
    let display_options = StatsDisplayOptions {
        by_language,
        by_file,
    };

    // Output in requested format
    match format {
        "json" => {
            print_stats_unified_json(&stats, &display_options)?;
        }
        _ => {
            print_stats_unified_text(&stats, &display_options);
        }
    }

    Ok(())
}

fn collect_edge_stats_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    compute_detailed: bool,
) -> (usize, usize, usize, HashMap<String, usize>) {
    let node_count = snapshot.nodes().len();

    // Get O(1) edge count from store stats (no iteration needed!)
    let edge_stats = snapshot.edges().stats();
    let edge_count = edge_stats.forward.csr_edge_count + edge_stats.forward.delta_edge_count
        - edge_stats.forward.tombstone_count;

    let mut kind_counts: HashMap<String, usize> = HashMap::new();
    let mut cross_language_count = 0usize;

    // Only iterate for detailed stats when requested (expensive operation)
    if compute_detailed {
        for (src_id, tgt_id, kind) in snapshot.iter_edges() {
            let kind_str = format!("{kind:?}");
            *kind_counts.entry(kind_str).or_insert(0) += 1;

            // Check for cross-language edges
            if let (Some(src_entry), Some(tgt_entry)) =
                (snapshot.get_node(src_id), snapshot.get_node(tgt_id))
            {
                let src_lang = snapshot.files().language_for_file(src_entry.file);
                let tgt_lang = snapshot.files().language_for_file(tgt_entry.file);
                if src_lang != tgt_lang && src_lang.is_some() && tgt_lang.is_some() {
                    cross_language_count += 1;
                }
            }
        }
    }

    (node_count, edge_count, cross_language_count, kind_counts)
}

fn collect_language_counts_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> HashMap<String, usize> {
    let mut lang_counts = HashMap::new();
    for (_node_id, entry) in snapshot.iter_nodes() {
        if let Some(lang) = snapshot.files().language_for_file(entry.file) {
            let lang_str = format!("{lang:?}");
            *lang_counts.entry(lang_str).or_insert(0) += 1;
        } else {
            *lang_counts.entry("Unknown".to_string()).or_insert(0) += 1;
        }
    }
    lang_counts
}

fn collect_file_counts_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> HashMap<String, usize> {
    let mut file_counts = HashMap::new();
    for (_node_id, entry) in snapshot.iter_nodes() {
        if let Some(path) = snapshot.files().resolve(entry.file) {
            let file_str = path.to_string_lossy().to_string();
            *file_counts.entry(file_str).or_insert(0) += 1;
        }
    }
    file_counts
}

/// Statistics data from graph analysis.
struct GraphStats<'a> {
    node_count: usize,
    edge_count: usize,
    cross_language_count: usize,
    kind_counts: &'a HashMap<String, usize>,
    lang_counts: &'a HashMap<String, usize>,
    file_counts: &'a HashMap<String, usize>,
    file_count: usize,
}

/// Display options for statistics output.
struct StatsDisplayOptions {
    by_language: bool,
    by_file: bool,
}

/// Print unified graph stats in text format.
fn print_stats_unified_text(stats: &GraphStats<'_>, options: &StatsDisplayOptions) {
    println!("Graph Statistics (Unified Graph)");
    println!("=================================");
    println!();
    println!("Total Nodes: {node_count}", node_count = stats.node_count);
    println!("Total Edges: {edge_count}", edge_count = stats.edge_count);
    println!("Files: {file_count}", file_count = stats.file_count);

    // Only show detailed stats if computed
    if !stats.kind_counts.is_empty() {
        println!();
        println!(
            "Cross-Language Edges: {cross_language_count}",
            cross_language_count = stats.cross_language_count
        );
        println!();

        println!("Edges by Kind:");
        let mut sorted_kinds: Vec<_> = stats.kind_counts.iter().collect();
        sorted_kinds.sort_by_key(|(kind, _)| kind.as_str());
        for (kind, count) in sorted_kinds {
            println!("  {kind}: {count}");
        }
    }
    println!();

    if options.by_language && !stats.lang_counts.is_empty() {
        println!("Nodes by Language:");
        let mut sorted_langs: Vec<_> = stats.lang_counts.iter().collect();
        sorted_langs.sort_by_key(|(lang, _)| lang.as_str());
        for (lang, count) in sorted_langs {
            println!("  {lang}: {count}");
        }
        println!();
    }

    println!("Files: {file_count}", file_count = stats.file_count);
    if options.by_file && !stats.file_counts.is_empty() {
        println!();
        println!("Nodes by File (top 10):");
        let mut sorted_files: Vec<_> = stats.file_counts.iter().collect();
        sorted_files.sort_by(|a, b| b.1.cmp(a.1));
        for (file, count) in sorted_files.into_iter().take(10) {
            println!("  {file}: {count}");
        }
    }
}

/// Print unified graph stats in JSON format.
fn print_stats_unified_json(stats: &GraphStats<'_>, options: &StatsDisplayOptions) -> Result<()> {
    use serde_json::{Map, Value, json};

    let mut output = Map::new();
    output.insert("node_count".into(), json!(stats.node_count));
    output.insert("edge_count".into(), json!(stats.edge_count));
    output.insert(
        "cross_language_edge_count".into(),
        json!(stats.cross_language_count),
    );
    output.insert("edges_by_kind".into(), json!(stats.kind_counts));
    output.insert("file_count".into(), json!(stats.file_count));

    if options.by_language {
        output.insert("nodes_by_language".into(), json!(stats.lang_counts));
        output.insert("language_count".into(), json!(stats.lang_counts.len()));
    }

    if options.by_file {
        output.insert("nodes_by_file".into(), json!(stats.file_counts));
    }

    let value = Value::Object(output);
    println!("{}", serde_json::to_string_pretty(&value)?);

    Ok(())
}

// ===== Unified Graph Trace Path =====

use sqry_core::graph::unified::node::NodeId as UnifiedNodeId;

/// Find shortest path between two symbols using the unified graph architecture.
///
/// This function implements BFS path-finding on the unified `CodeGraph`,
/// supporting language filtering and multiple output formats.
fn run_trace_path_unified(
    graph: &UnifiedCodeGraph,
    from: &str,
    to: &str,
    languages: Option<&str>,
    full_paths: bool,
    format: &str,
    verbose: bool,
) -> Result<()> {
    let snapshot = graph.snapshot();

    // Find start candidates
    let start_candidates = snapshot.nodes_by_symbol(from);
    if start_candidates.is_empty() {
        bail!(
            "Symbol '{from}' not found in graph. Use `sqry --lang` to inspect available languages."
        );
    }

    // Find target candidates
    let target_candidates = snapshot.nodes_by_symbol(to);
    if target_candidates.is_empty() {
        bail!("Symbol '{to}' not found in graph.");
    }

    // Parse language filter
    let language_list = parse_language_filter(languages)?;
    let language_filter: HashSet<_> = language_list.into_iter().collect();

    let filtered_starts =
        filter_nodes_by_language_unified(&snapshot, start_candidates, &language_filter);

    if filtered_starts.is_empty() {
        bail!(
            "Symbol '{}' not found in requested languages: {}",
            from,
            display_languages(&language_filter)
        );
    }

    let filtered_targets: HashSet<_> =
        filter_nodes_by_language_unified(&snapshot, target_candidates, &language_filter)
            .into_iter()
            .collect();

    if filtered_targets.is_empty() {
        bail!(
            "Symbol '{}' not found in requested languages: {}",
            to,
            display_languages(&language_filter)
        );
    }

    // BFS to find shortest path
    let path = find_path_unified_bfs(
        &snapshot,
        &filtered_starts,
        &filtered_targets,
        &language_filter,
    )
    .ok_or_else(|| anyhow::anyhow!("No path found from '{from}' to '{to}'"))?;

    if path.is_empty() {
        bail!("Path resolution returned no nodes");
    }

    write_trace_path_output_unified(&snapshot, &path, full_paths, verbose, format)?;

    Ok(())
}

fn filter_nodes_by_language_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    candidates: Vec<UnifiedNodeId>,
    language_filter: &HashSet<Language>,
) -> Vec<UnifiedNodeId> {
    if language_filter.is_empty() {
        return candidates;
    }

    candidates
        .into_iter()
        .filter(|&node_id| {
            if let Some(entry) = snapshot.get_node(node_id) {
                snapshot
                    .files()
                    .language_for_file(entry.file)
                    .is_some_and(|lang| language_filter.contains(&lang))
            } else {
                false
            }
        })
        .collect()
}

fn write_trace_path_output_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    path: &[UnifiedNodeId],
    full_paths: bool,
    verbose: bool,
    format: &str,
) -> Result<()> {
    match format {
        "json" => print_trace_path_unified_json(snapshot, path, full_paths, verbose),
        "dot" | "mermaid" | "d2" => {
            // Visualization formats require additional migration work
            // Fall back to text output with a note
            eprintln!(
                "Note: Visualization format '{format}' not yet migrated to unified graph. Using text output."
            );
            print_trace_path_unified_text(snapshot, path, full_paths, verbose);
            Ok(())
        }
        _ => {
            print_trace_path_unified_text(snapshot, path, full_paths, verbose);
            Ok(())
        }
    }
}

/// BFS path finding on unified graph.
fn find_path_unified_bfs(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    starts: &[UnifiedNodeId],
    targets: &HashSet<UnifiedNodeId>,
    language_filter: &HashSet<Language>,
) -> Option<Vec<UnifiedNodeId>> {
    let mut queue = VecDeque::new();
    let mut parents: HashMap<UnifiedNodeId, Option<UnifiedNodeId>> = HashMap::new();

    // Initialize BFS from all start nodes
    for &start in starts {
        parents.insert(start, None);
        queue.push_back(start);
    }

    while let Some(current) = queue.pop_front() {
        // Check if we reached a target
        if targets.contains(&current) {
            return Some(reconstruct_path_unified(current, &parents));
        }

        // Get callees (neighbors)
        for callee in snapshot.get_callees(current) {
            // Skip if already visited
            if parents.contains_key(&callee) {
                continue;
            }

            if !callee_matches_language(snapshot, callee, language_filter) {
                continue;
            }

            parents.insert(callee, Some(current));
            queue.push_back(callee);
        }
    }

    None
}

fn callee_matches_language(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    callee: UnifiedNodeId,
    language_filter: &HashSet<Language>,
) -> bool {
    if language_filter.is_empty() {
        return true;
    }

    let Some(entry) = snapshot.get_node(callee) else {
        return false;
    };
    let lang = snapshot.files().language_for_file(entry.file);
    lang.is_some_and(|l| language_filter.contains(&l))
}

/// Reconstruct path from BFS parent map.
fn reconstruct_path_unified(
    target: UnifiedNodeId,
    parents: &HashMap<UnifiedNodeId, Option<UnifiedNodeId>>,
) -> Vec<UnifiedNodeId> {
    let mut path = Vec::new();
    let mut current = target;

    loop {
        path.push(current);
        match parents.get(&current).and_then(|&p| p) {
            Some(parent) => current = parent,
            None => break,
        }
    }

    path.reverse();
    path
}

/// Print trace path in text format using unified graph.
fn print_trace_path_unified_text(
    snapshot: &UnifiedGraphSnapshot,
    path: &[UnifiedNodeId],
    full_paths: bool,
    verbose: bool,
) {
    // Get symbol names for display
    let start_name = path
        .first()
        .and_then(|&id| snapshot.get_node(id))
        .map_or_else(
            || "?".to_string(),
            |entry| resolve_node_name(snapshot, entry),
        );

    let end_name = path
        .last()
        .and_then(|&id| snapshot.get_node(id))
        .map_or_else(
            || "?".to_string(),
            |entry| resolve_node_name(snapshot, entry),
        );

    println!(
        "Path from '{start_name}' to '{end_name}' ({} steps):",
        path.len().saturating_sub(1)
    );
    println!();

    for (i, &node_id) in path.iter().enumerate() {
        if let Some(entry) = snapshot.get_node(node_id) {
            let qualified_name = resolve_node_label(snapshot, entry);
            let file_path = resolve_node_file_path(snapshot, entry, full_paths);
            let language = resolve_node_language(snapshot, entry);

            let step = i + 1;
            println!("  {step}. {qualified_name} ({language} in {file_path})");

            if verbose {
                println!(
                    "     └─ {file_path}:{}:{}",
                    entry.start_line, entry.start_column
                );
            }

            if i < path.len() - 1 {
                println!("     │");
                println!("     ↓");
            }
        }
    }
}

/// Print trace path in JSON format using unified graph.
fn print_trace_path_unified_json(
    snapshot: &UnifiedGraphSnapshot,
    path: &[UnifiedNodeId],
    full_paths: bool,
    verbose: bool,
) -> Result<()> {
    use serde_json::json;

    let nodes: Vec<_> = path
        .iter()
        .filter_map(|&node_id| {
            let entry = snapshot.get_node(node_id)?;
            let qualified_name = resolve_node_label(snapshot, entry);
            let file_path = resolve_node_file_path(snapshot, entry, full_paths);
            let language = resolve_node_language(snapshot, entry);

            if verbose {
                Some(json!({
                    "id": format!("{node_id:?}"),
                    "name": qualified_name,
                    "language": language,
                    "file": file_path,
                    "span": {
                        "start": { "line": entry.start_line, "column": entry.start_column },
                        "end": { "line": entry.end_line, "column": entry.end_column }
                    }
                }))
            } else {
                Some(json!({
                    "id": format!("{node_id:?}"),
                    "name": qualified_name,
                    "language": language,
                    "file": file_path
                }))
            }
        })
        .collect();

    let output = json!({
        "path": nodes,
        "length": path.len(),
        "steps": path.len().saturating_sub(1)
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ===== Unified Graph Cycles =====

/// Detect cycles in the graph using the unified graph architecture.
///
/// This function detects all cycles in the graph using DFS,
/// with support for filtering by language and edge type.
fn run_cycles_unified(
    graph: &UnifiedCodeGraph,
    min_length: usize,
    max_length: Option<usize>,
    imports_only: bool,
    languages: Option<&str>,
    format: &str,
    verbose: bool,
) -> Result<()> {
    let snapshot = graph.snapshot();

    let language_list = parse_language_filter(languages)?;
    let language_filter: HashSet<_> = language_list.into_iter().collect();

    // Detect all cycles in the graph
    let cycles = detect_cycles_unified(&snapshot, imports_only, &language_filter);

    // Filter by length
    let filtered_cycles: Vec<_> = cycles
        .into_iter()
        .filter(|cycle| {
            let len = cycle.len();
            len >= min_length && max_length.is_none_or(|max| len <= max)
        })
        .collect();

    if verbose {
        eprintln!(
            "Found {} cycles (min_length={}, max_length={:?})",
            filtered_cycles.len(),
            min_length,
            max_length
        );
    }

    match format {
        "json" => print_cycles_unified_json(&filtered_cycles, &snapshot)?,
        _ => print_cycles_unified_text(&filtered_cycles, &snapshot),
    }

    Ok(())
}

/// Detect all cycles in the unified graph using depth-first search.
fn detect_cycles_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    imports_only: bool,
    language_filter: &HashSet<Language>,
) -> Vec<Vec<UnifiedNodeId>> {
    // Build adjacency list first for efficient traversal
    let mut adjacency: HashMap<UnifiedNodeId, Vec<UnifiedNodeId>> = HashMap::new();

    for (src_id, tgt_id, kind) in snapshot.iter_edges() {
        // Filter edge types
        if imports_only && !matches!(kind, UnifiedEdgeKind::Imports { .. }) {
            continue;
        }

        adjacency.entry(src_id).or_default().push(tgt_id);
    }

    let mut cycles = Vec::new();
    let mut visited = HashSet::new();
    let mut rec_stack = HashSet::new();
    let mut path = Vec::new();

    for (node_id, entry) in snapshot.iter_nodes() {
        // Apply language filter
        if !language_filter.is_empty() {
            let node_lang = snapshot.files().language_for_file(entry.file);
            if !node_lang.is_some_and(|l| language_filter.contains(&l)) {
                continue;
            }
        }

        if !visited.contains(&node_id) {
            detect_cycles_unified_dfs(
                snapshot,
                node_id,
                &adjacency,
                &mut visited,
                &mut rec_stack,
                &mut path,
                &mut cycles,
            );
        }
    }

    cycles
}

/// DFS helper for cycle detection on unified graph.
#[allow(clippy::only_used_in_recursion)]
fn detect_cycles_unified_dfs(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node: UnifiedNodeId,
    adjacency: &HashMap<UnifiedNodeId, Vec<UnifiedNodeId>>,
    visited: &mut HashSet<UnifiedNodeId>,
    rec_stack: &mut HashSet<UnifiedNodeId>,
    path: &mut Vec<UnifiedNodeId>,
    cycles: &mut Vec<Vec<UnifiedNodeId>>,
) {
    visited.insert(node);
    rec_stack.insert(node);
    path.push(node);

    // Get neighbors from adjacency list
    if let Some(neighbors) = adjacency.get(&node) {
        for &neighbor in neighbors {
            if rec_stack.contains(&neighbor) {
                record_cycle_if_new(path, neighbor, cycles);
                continue;
            }

            if !visited.contains(&neighbor) {
                detect_cycles_unified_dfs(
                    snapshot, neighbor, adjacency, visited, rec_stack, path, cycles,
                );
            }
        }
    }

    path.pop();
    rec_stack.remove(&node);
}

fn record_cycle_if_new(
    path: &[UnifiedNodeId],
    neighbor: UnifiedNodeId,
    cycles: &mut Vec<Vec<UnifiedNodeId>>,
) {
    // Found a cycle - extract the cycle from path
    if let Some(cycle_start) = path.iter().position(|&n| n == neighbor) {
        let cycle: Vec<_> = path[cycle_start..].to_vec();
        if !cycles.contains(&cycle) {
            cycles.push(cycle);
        }
    }
}

/// Print cycles in text format using unified graph.
fn print_cycles_unified_text(cycles: &[Vec<UnifiedNodeId>], snapshot: &UnifiedGraphSnapshot) {
    if cycles.is_empty() {
        println!("No cycles found.");
        return;
    }

    let cycle_count = cycles.len();
    println!("Found {cycle_count} cycle(s):");
    println!();

    for (i, cycle) in cycles.iter().enumerate() {
        let cycle_index = i + 1;
        let cycle_length = cycle.len();
        println!("Cycle {cycle_index} (length {cycle_length}):");

        for &node_id in cycle {
            if let Some(entry) = snapshot.get_node(node_id) {
                let name = resolve_node_label(snapshot, entry);
                let language = resolve_node_language(snapshot, entry);

                println!("  → {name} ({language})");
            }
        }

        // Show the cycle back to start
        if let Some(&first) = cycle.first()
            && let Some(entry) = snapshot.get_node(first)
        {
            let name = resolve_node_label(snapshot, entry);

            println!("  → {name} (cycle)");
        }

        println!();
    }
}

/// Print cycles in JSON format using unified graph.
fn print_cycles_unified_json(
    cycles: &[Vec<UnifiedNodeId>],
    snapshot: &UnifiedGraphSnapshot,
) -> Result<()> {
    use serde_json::json;

    let cycle_data: Vec<_> = cycles
        .iter()
        .map(|cycle| {
            let nodes: Vec<_> = cycle
                .iter()
                .filter_map(|&node_id| {
                    let entry = snapshot.get_node(node_id)?;
                    let name = resolve_node_label(snapshot, entry);
                    let language = resolve_node_language(snapshot, entry);
                    let file = resolve_node_file_path(snapshot, entry, true);

                    Some(json!({
                        "id": format!("{node_id:?}"),
                        "name": name,
                        "language": language,
                        "file": file
                    }))
                })
                .collect();

            json!({
                "length": cycle.len(),
                "nodes": nodes
            })
        })
        .collect();

    let output = json!({
        "count": cycles.len(),
        "cycles": cycle_data
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ============================================================================
// Call Chain Depth - Unified Graph Implementation
// ============================================================================

/// Type alias for unified graph depth results.
type UnifiedDepthResult = (UnifiedNodeId, usize, Option<Vec<Vec<UnifiedNodeId>>>);

/// Calculate maximum call chain depth for a symbol using the unified graph.
///
/// This function finds all nodes matching the symbol name, calculates the
/// maximum depth of the call chain from each node, and optionally builds
/// the actual call chains.
fn run_call_chain_depth_unified(
    graph: &UnifiedCodeGraph,
    symbol: &str,
    languages: Option<&str>,
    show_chain: bool,
    format: &str,
    verbose: bool,
) -> Result<()> {
    let snapshot = graph.snapshot();
    let lang_filter = parse_language_filter_unified(languages);

    // Find all nodes matching the symbol
    let matching_nodes = filter_matching_nodes_by_language(&snapshot, symbol, &lang_filter);

    if matching_nodes.is_empty() {
        bail!("Symbol '{symbol}' not found in graph (after language filtering)");
    }

    let mut results = build_depth_results(&snapshot, &matching_nodes, show_chain);

    // Sort by depth (descending)
    results.sort_by_key(|(_, depth, _)| std::cmp::Reverse(*depth));

    if verbose {
        eprintln!(
            "Call chain depth analysis: {} symbol(s) matching '{}'",
            results.len(),
            symbol
        );
    }

    // Format output
    write_call_chain_depth_output(&results, &snapshot, show_chain, verbose, format)
}

fn filter_matching_nodes_by_language(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol: &str,
    lang_filter: &[String],
) -> Vec<UnifiedNodeId> {
    let mut matching_nodes = snapshot.nodes_by_symbol(symbol);
    if lang_filter.is_empty() {
        return matching_nodes;
    }

    matching_nodes.retain(|&node_id| {
        let Some(entry) = snapshot.get_node(node_id) else {
            return false;
        };
        let Some(lang) = snapshot.files().language_for_file(entry.file) else {
            return false;
        };
        lang_filter
            .iter()
            .any(|filter| filter.eq_ignore_ascii_case(&format!("{lang:?}")))
    });

    matching_nodes
}

fn build_depth_results(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    matching_nodes: &[UnifiedNodeId],
    show_chain: bool,
) -> Vec<UnifiedDepthResult> {
    let mut results = Vec::new();
    for &node_id in matching_nodes {
        let depth = calculate_call_chain_depth_unified(snapshot, node_id);
        let chains = show_chain.then(|| build_call_chain_unified(snapshot, node_id));
        results.push((node_id, depth, chains));
    }
    results
}

fn write_call_chain_depth_output(
    results: &[UnifiedDepthResult],
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    show_chain: bool,
    verbose: bool,
    format: &str,
) -> Result<()> {
    if format == "json" {
        print_call_chain_depth_unified_json(results, snapshot, show_chain, verbose)
    } else {
        print_call_chain_depth_unified_text(results, snapshot, show_chain, verbose);
        Ok(())
    }
}

/// Calculate the maximum call chain depth from a starting node.
///
/// Uses BFS to find the longest path through the call graph without cycles.
fn calculate_call_chain_depth_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    start: UnifiedNodeId,
) -> usize {
    let mut max_depth = 0;
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    queue.push_back((start, 0));

    while let Some((current, depth)) = queue.pop_front() {
        if !visited.insert(current) {
            continue;
        }

        max_depth = max_depth.max(depth);

        // Get callees
        let callees = snapshot.get_callees(current);
        for callee in callees {
            if !visited.contains(&callee) {
                queue.push_back((callee, depth + 1));
            }
        }
    }

    max_depth
}

/// Build all call chains from a starting node using BFS.
///
/// Returns a vector of paths, where each path is a vector of node IDs
/// representing a complete call chain from start to a leaf node.
fn build_call_chain_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    start: UnifiedNodeId,
) -> Vec<Vec<UnifiedNodeId>> {
    let mut chains = Vec::new();
    let mut queue = VecDeque::new();

    queue.push_back(vec![start]);

    while let Some(path) = queue.pop_front() {
        let current = *path.last().unwrap();
        let callees = snapshot.get_callees(current);

        if callees.is_empty() {
            // Leaf node - this is a complete chain
            chains.push(path);
        } else {
            for callee in callees {
                // Avoid cycles
                if !path.contains(&callee) {
                    let mut new_path = path.clone();
                    new_path.push(callee);
                    queue.push_back(new_path);
                }
            }
        }

        // Limit number of chains to prevent explosion
        if chains.len() >= 100 {
            break;
        }
    }

    chains
}

/// Print call chain depth results in text format using unified graph.
fn print_call_chain_depth_unified_text(
    results: &[UnifiedDepthResult],
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    show_chain: bool,
    verbose: bool,
) {
    if results.is_empty() {
        println!("No results found.");
        return;
    }

    println!("Call Chain Depth Analysis");
    println!("========================");
    println!();

    for (node_id, depth, chains) in results {
        if let Some(entry) = snapshot.get_node(*node_id) {
            print_call_chain_entry(
                snapshot,
                entry,
                *depth,
                chains.as_ref(),
                show_chain,
                verbose,
            );
        }
    }
}

fn print_call_chain_entry(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    entry: &NodeEntry,
    depth: usize,
    chains: Option<&Vec<Vec<UnifiedNodeId>>>,
    show_chain: bool,
    verbose: bool,
) {
    let name = entry
        .qualified_name
        .and_then(|id| snapshot.strings().resolve(id))
        .or_else(|| snapshot.strings().resolve(entry.name))
        .map_or_else(|| "?".to_string(), |s| s.to_string());

    let language = snapshot
        .files()
        .language_for_file(entry.file)
        .map_or_else(|| "Unknown".to_string(), |l| format!("{l:?}"));

    println!("Symbol: {name} ({language})");
    println!("Depth:  {depth}");

    if verbose {
        let file = snapshot.files().resolve(entry.file).map_or_else(
            || "unknown".to_string(),
            |p| p.to_string_lossy().to_string(),
        );
        println!("File:   {file}");
        let line = entry.start_line;
        let column = entry.start_column;
        println!("Line:   {line}:{column}");
    }

    if let Some(chain_list) = chains.filter(|_| show_chain) {
        print_call_chain_list(snapshot, chain_list);
    }

    println!();
}

fn print_call_chain_list(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    chain_list: &[Vec<UnifiedNodeId>],
) {
    let chain_count = chain_list.len();
    println!("Chains: {chain_count} path(s)");
    for (i, chain) in chain_list.iter().take(5).enumerate() {
        let chain_index = i + 1;
        println!("  Chain {chain_index}:");
        for (j, &chain_node_id) in chain.iter().enumerate() {
            if let Some(chain_entry) = snapshot.get_node(chain_node_id) {
                let chain_name = chain_entry
                    .qualified_name
                    .and_then(|id| snapshot.strings().resolve(id))
                    .or_else(|| snapshot.strings().resolve(chain_entry.name))
                    .map_or_else(|| "?".to_string(), |s| s.to_string());
                let step = j + 1;
                println!("    {step}. {chain_name}");
            }
        }
    }
    if chain_list.len() > 5 {
        let remaining = chain_list.len() - 5;
        println!("  ... and {remaining} more chains");
    }
}

/// Print call chain depth results in JSON format using unified graph.
fn print_call_chain_depth_unified_json(
    results: &[UnifiedDepthResult],
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    _show_chain: bool,
    verbose: bool,
) -> Result<()> {
    use serde_json::json;

    let items: Vec<_> = results
        .iter()
        .filter_map(|(node_id, depth, chains)| {
            let entry = snapshot.get_node(*node_id)?;

            let name = entry
                .qualified_name
                .and_then(|id| snapshot.strings().resolve(id))
                .or_else(|| snapshot.strings().resolve(entry.name))
                .map_or_else(|| "?".to_string(), |s| s.to_string());

            let language = snapshot
                .files()
                .language_for_file(entry.file)
                .map_or_else(|| "Unknown".to_string(), |l| format!("{l:?}"));

            let mut obj = json!({
                "symbol": name,
                "language": language,
                "depth": depth,
            });

            if verbose {
                let file = snapshot.files().resolve(entry.file).map_or_else(
                    || "unknown".to_string(),
                    |p| p.to_string_lossy().to_string(),
                );
                obj["file"] = json!(file);
            }

            if let Some(chain_list) = chains {
                let chain_json: Vec<Vec<String>> = chain_list
                    .iter()
                    .map(|chain| {
                        chain
                            .iter()
                            .filter_map(|&nid| {
                                snapshot.get_node(nid).map(|e| {
                                    e.qualified_name
                                        .and_then(|id| snapshot.strings().resolve(id))
                                        .or_else(|| snapshot.strings().resolve(e.name))
                                        .map_or_else(|| "?".to_string(), |s| s.to_string())
                                })
                            })
                            .collect()
                    })
                    .collect();
                obj["chains"] = json!(chain_json);
            }

            Some(obj)
        })
        .collect();

    let output = json!({
        "results": items,
        "count": results.len()
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Parse language filter for unified graph operations.
///
/// Returns a vector of language name strings to match against.
fn parse_language_filter_unified(languages: Option<&str>) -> Vec<String> {
    if let Some(langs) = languages {
        langs.split(',').map(|s| s.trim().to_string()).collect()
    } else {
        Vec::new()
    }
}

// ============================================================================
// Dependency Tree - Unified Graph Implementation
// ============================================================================

/// A subgraph representation for unified graph operations.
///
/// This is the unified graph equivalent of `sqry_core::graph::SubGraph`.
struct UnifiedSubGraph {
    /// Node IDs in this subgraph
    nodes: Vec<UnifiedNodeId>,
    /// Edges as (from, to, kind) tuples
    edges: Vec<(UnifiedNodeId, UnifiedNodeId, UnifiedEdgeKind)>,
}

/// Show transitive dependencies for a module using the unified graph.
///
/// This function finds all nodes matching the module name, builds a
/// dependency tree, and outputs it in the requested format.
fn run_dependency_tree_unified(
    graph: &UnifiedCodeGraph,
    module: &str,
    max_depth: Option<usize>,
    cycles_only: bool,
    format: &str,
    verbose: bool,
) -> Result<()> {
    let snapshot = graph.snapshot();

    // Find root nodes for this module
    let root_nodes = snapshot.nodes_by_symbol(module);
    if root_nodes.is_empty() {
        bail!("Module '{module}' not found in graph");
    }

    // Build dependency tree via BFS from root nodes
    let mut subgraph = build_dependency_tree_unified(&snapshot, &root_nodes);

    if subgraph.nodes.is_empty() {
        bail!("Module '{module}' has no dependencies");
    }

    // Apply max_depth filter if specified
    if let Some(depth_limit) = max_depth {
        subgraph = filter_by_depth_unified(&snapshot, &subgraph, &root_nodes, depth_limit);
    }

    // If cycles_only is requested, filter to only nodes involved in cycles
    if cycles_only {
        subgraph = filter_cycles_only_unified(&subgraph);
        if subgraph.nodes.is_empty() {
            println!("No circular dependencies found for module '{module}'");
            return Ok(());
        }
    }

    if verbose {
        eprintln!(
            "Dependency tree: {} nodes, {} edges",
            subgraph.nodes.len(),
            subgraph.edges.len()
        );
    }

    // Format output
    match format {
        "json" => print_dependency_tree_unified_json(&subgraph, &snapshot, verbose),
        "dot" | "mermaid" | "d2" => {
            // Visualization formats - fall back to text with note
            println!("Note: Visualization format '{format}' uses text output for unified graph.");
            println!();
            print_dependency_tree_unified_text(&subgraph, &snapshot, cycles_only, verbose);
            Ok(())
        }
        _ => {
            print_dependency_tree_unified_text(&subgraph, &snapshot, cycles_only, verbose);
            Ok(())
        }
    }
}

/// Build a dependency tree by traversing from root nodes using BFS.
///
/// Collects all transitively reachable nodes and their connecting edges.
fn build_dependency_tree_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    root_nodes: &[UnifiedNodeId],
) -> UnifiedSubGraph {
    let (visited_nodes, mut edges) = collect_dependency_edges_unified(snapshot, root_nodes);
    let node_set: HashSet<_> = visited_nodes.iter().copied().collect();
    add_internal_edges_unified(snapshot, &node_set, &mut edges);

    UnifiedSubGraph {
        nodes: visited_nodes.into_iter().collect(),
        edges,
    }
}

fn collect_dependency_edges_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    root_nodes: &[UnifiedNodeId],
) -> (
    HashSet<UnifiedNodeId>,
    Vec<(UnifiedNodeId, UnifiedNodeId, UnifiedEdgeKind)>,
) {
    let mut visited_nodes = HashSet::new();
    let mut edges = Vec::new();
    let mut queue = VecDeque::new();

    // Initialize with root nodes
    for &root in root_nodes {
        visited_nodes.insert(root);
        queue.push_back(root);
    }

    // BFS to collect all dependencies
    while let Some(current) = queue.pop_front() {
        // Get all outgoing edges from this node (calls, imports, etc.)
        for (from, to, kind) in snapshot.iter_edges() {
            if from == current && visited_nodes.insert(to) {
                edges.push((from, to, kind));
                queue.push_back(to);
            }
        }
    }

    (visited_nodes, edges)
}

fn add_internal_edges_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node_set: &HashSet<UnifiedNodeId>,
    edges: &mut Vec<(UnifiedNodeId, UnifiedNodeId, UnifiedEdgeKind)>,
) {
    for (from, to, kind) in snapshot.iter_edges() {
        if node_set.contains(&from)
            && node_set.contains(&to)
            && !edge_exists_unified(edges, from, to)
        {
            edges.push((from, to, kind));
        }
    }
}

fn edge_exists_unified(
    edges: &[(UnifiedNodeId, UnifiedNodeId, UnifiedEdgeKind)],
    from: UnifiedNodeId,
    to: UnifiedNodeId,
) -> bool {
    edges.iter().any(|&(f, t, _)| f == from && t == to)
}

/// Filter subgraph by maximum depth from root nodes.
fn filter_by_depth_unified(
    _snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    subgraph: &UnifiedSubGraph,
    root_nodes: &[UnifiedNodeId],
    max_depth: usize,
) -> UnifiedSubGraph {
    // BFS to assign depths
    let mut depths: HashMap<UnifiedNodeId, usize> = HashMap::new();
    let mut queue = VecDeque::new();

    // Build adjacency list from edges
    let mut adj: HashMap<UnifiedNodeId, Vec<UnifiedNodeId>> = HashMap::new();
    for &(from, to, _) in &subgraph.edges {
        adj.entry(from).or_default().push(to);
    }

    // Initialize from root nodes
    let node_set: HashSet<_> = subgraph.nodes.iter().copied().collect();
    for &root in root_nodes {
        if node_set.contains(&root) {
            depths.insert(root, 0);
            queue.push_back((root, 0));
        }
    }

    // BFS to compute depths
    let mut visited = HashSet::new();
    while let Some((current, depth)) = queue.pop_front() {
        if !visited.insert(current) {
            continue;
        }

        if depth >= max_depth {
            continue;
        }

        if let Some(neighbors) = adj.get(&current) {
            for &neighbor in neighbors {
                depths.entry(neighbor).or_insert(depth + 1);
                queue.push_back((neighbor, depth + 1));
            }
        }
    }

    // Keep only nodes within depth limit
    let filtered_nodes: Vec<_> = subgraph
        .nodes
        .iter()
        .filter(|n| depths.get(n).is_some_and(|&d| d <= max_depth))
        .copied()
        .collect();

    let filtered_node_set: HashSet<_> = filtered_nodes.iter().copied().collect();

    // Keep only edges between filtered nodes
    let filtered_edges: Vec<_> = subgraph
        .edges
        .iter()
        .filter(|(from, to, _)| filtered_node_set.contains(from) && filtered_node_set.contains(to))
        .map(|&(from, to, ref kind)| (from, to, kind.clone()))
        .collect();

    UnifiedSubGraph {
        nodes: filtered_nodes,
        edges: filtered_edges,
    }
}

/// Filter subgraph to only include nodes involved in cycles.
fn filter_cycles_only_unified(subgraph: &UnifiedSubGraph) -> UnifiedSubGraph {
    let adj = build_adjacency_unified(&subgraph.edges);
    let in_cycle = collect_cycle_nodes_unified(&subgraph.nodes, &adj);
    let filtered_nodes: Vec<_> = subgraph
        .nodes
        .iter()
        .filter(|n| in_cycle.contains(n))
        .copied()
        .collect();

    let node_set: HashSet<_> = filtered_nodes.iter().copied().collect();
    let filtered_edges = filter_edges_by_nodes_unified(&subgraph.edges, &node_set);

    UnifiedSubGraph {
        nodes: filtered_nodes,
        edges: filtered_edges,
    }
}

fn build_adjacency_unified(
    edges: &[(UnifiedNodeId, UnifiedNodeId, UnifiedEdgeKind)],
) -> HashMap<UnifiedNodeId, Vec<UnifiedNodeId>> {
    let mut adj: HashMap<UnifiedNodeId, Vec<UnifiedNodeId>> = HashMap::new();
    for &(from, to, _) in edges {
        adj.entry(from).or_default().push(to);
    }
    adj
}

fn collect_cycle_nodes_unified(
    nodes: &[UnifiedNodeId],
    adj: &HashMap<UnifiedNodeId, Vec<UnifiedNodeId>>,
) -> HashSet<UnifiedNodeId> {
    let mut in_cycle = HashSet::new();
    let mut visited = HashSet::new();
    let mut rec_stack = HashSet::new();

    for &node in nodes {
        if !visited.contains(&node) {
            let mut path = Vec::new();
            dfs_cycles_unified(
                node,
                adj,
                &mut visited,
                &mut rec_stack,
                &mut in_cycle,
                &mut path,
            );
        }
    }

    in_cycle
}

fn dfs_cycles_unified(
    node: UnifiedNodeId,
    adj: &HashMap<UnifiedNodeId, Vec<UnifiedNodeId>>,
    visited: &mut HashSet<UnifiedNodeId>,
    rec_stack: &mut HashSet<UnifiedNodeId>,
    in_cycle: &mut HashSet<UnifiedNodeId>,
    path: &mut Vec<UnifiedNodeId>,
) {
    visited.insert(node);
    rec_stack.insert(node);
    path.push(node);

    if let Some(neighbors) = adj.get(&node) {
        for &neighbor in neighbors {
            if !visited.contains(&neighbor) {
                dfs_cycles_unified(neighbor, adj, visited, rec_stack, in_cycle, path);
            } else if rec_stack.contains(&neighbor) {
                let cycle_start = path.iter().position(|&n| n == neighbor).unwrap_or(0);
                for &cycle_node in &path[cycle_start..] {
                    in_cycle.insert(cycle_node);
                }
                in_cycle.insert(neighbor);
            }
        }
    }

    path.pop();
    rec_stack.remove(&node);
}

fn filter_edges_by_nodes_unified(
    edges: &[(UnifiedNodeId, UnifiedNodeId, UnifiedEdgeKind)],
    node_set: &HashSet<UnifiedNodeId>,
) -> Vec<(UnifiedNodeId, UnifiedNodeId, UnifiedEdgeKind)> {
    edges
        .iter()
        .filter(|(from, to, _)| node_set.contains(from) && node_set.contains(to))
        .map(|&(from, to, ref kind)| (from, to, kind.clone()))
        .collect()
}

/// Print dependency tree in text format using unified graph.
fn print_dependency_tree_unified_text(
    subgraph: &UnifiedSubGraph,
    snapshot: &UnifiedGraphSnapshot,
    cycles_only: bool,
    verbose: bool,
) {
    let title = if cycles_only {
        "Dependency Tree (Cycles Only)"
    } else {
        "Dependency Tree"
    };

    println!("{title}");
    println!("{}", "=".repeat(title.len()));
    println!();

    // Print nodes
    let node_count = subgraph.nodes.len();
    println!("Nodes ({node_count}):");
    for &node_id in &subgraph.nodes {
        if let Some(entry) = snapshot.get_node(node_id) {
            let name = resolve_node_label(snapshot, entry);
            let language = resolve_node_language(snapshot, entry);

            if verbose {
                let file = resolve_node_file_path(snapshot, entry, true);
                let line = entry.start_line;
                println!("  {name} ({language}) - {file}:{line}");
            } else {
                println!("  {name} ({language})");
            }
        }
    }

    println!();
    let edge_count = subgraph.edges.len();
    println!("Edges ({edge_count}):");
    for (from_id, to_id, kind) in &subgraph.edges {
        let from_name =
            resolve_node_label_by_id(snapshot, *from_id).unwrap_or_else(|| "?".to_string());
        let to_name = resolve_node_label_by_id(snapshot, *to_id).unwrap_or_else(|| "?".to_string());

        println!("  {from_name} --[{kind:?}]--> {to_name}");
    }
}

/// Print dependency tree in JSON format using unified graph.
fn print_dependency_tree_unified_json(
    subgraph: &UnifiedSubGraph,
    snapshot: &UnifiedGraphSnapshot,
    verbose: bool,
) -> Result<()> {
    use serde_json::json;

    let nodes: Vec<_> = subgraph
        .nodes
        .iter()
        .filter_map(|&node_id| {
            let entry = snapshot.get_node(node_id)?;
            let name = resolve_node_label(snapshot, entry);
            let language = resolve_node_language(snapshot, entry);

            let mut obj = json!({
                "id": format!("{node_id:?}"),
                "name": name,
                "language": language,
            });

            if verbose {
                let file = resolve_node_file_path(snapshot, entry, true);
                obj["file"] = json!(file);
                obj["line"] = json!(entry.start_line);
            }

            Some(obj)
        })
        .collect();

    let edges: Vec<_> = subgraph
        .edges
        .iter()
        .filter_map(|(from_id, to_id, kind)| {
            let from_name = resolve_node_label_by_id(snapshot, *from_id)?;
            let to_name = resolve_node_label_by_id(snapshot, *to_id)?;

            Some(json!({
                "from": from_name,
                "to": to_name,
                "kind": format!("{kind:?}"),
            }))
        })
        .collect();

    let output = json!({
        "nodes": nodes,
        "edges": edges,
        "node_count": subgraph.nodes.len(),
        "edge_count": subgraph.edges.len(),
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ===== Cross-Language Unified Implementation =====

/// Result type for cross-language edges in unified graph
type UnifiedCrossLangEdge = (
    UnifiedNodeId,
    UnifiedNodeId,
    UnifiedEdgeKind,
    sqry_core::graph::Language, // from_lang
    sqry_core::graph::Language, // to_lang
);

/// List cross-language relationships using the unified graph architecture.
fn run_cross_language_unified(
    graph: &UnifiedCodeGraph,
    from_lang: Option<&str>,
    to_lang: Option<&str>,
    edge_type: Option<&str>,
    _min_confidence: f64,
    format: &str,
    verbose: bool,
) -> Result<()> {
    let snapshot = graph.snapshot();

    // Parse language filters
    let from_language = from_lang.map(parse_language).transpose()?;
    let to_language = to_lang.map(parse_language).transpose()?;

    // Collect cross-language edges
    let mut cross_lang_edges: Vec<UnifiedCrossLangEdge> = Vec::new();

    for (src_id, tgt_id, kind) in snapshot.iter_edges() {
        // Get source and target language
        let (src_lang, tgt_lang) = match (snapshot.get_node(src_id), snapshot.get_node(tgt_id)) {
            (Some(src_entry), Some(tgt_entry)) => {
                let src_l = snapshot.files().language_for_file(src_entry.file);
                let tgt_l = snapshot.files().language_for_file(tgt_entry.file);
                match (src_l, tgt_l) {
                    (Some(s), Some(t)) => (s, t),
                    _ => continue,
                }
            }
            _ => continue,
        };

        // Only include cross-language edges
        if src_lang == tgt_lang {
            continue;
        }

        // Apply from_lang filter
        if let Some(filter_lang) = from_language
            && src_lang != filter_lang
        {
            continue;
        }

        // Apply to_lang filter
        if let Some(filter_lang) = to_language
            && tgt_lang != filter_lang
        {
            continue;
        }

        // Apply edge type filter
        if let Some(kind_str) = edge_type
            && !edge_kind_matches_unified(&kind, kind_str)
        {
            continue;
        }

        cross_lang_edges.push((src_id, tgt_id, kind.clone(), src_lang, tgt_lang));
    }

    // Note: min_confidence filtering is not yet implemented for unified graph
    // as EdgeKind doesn't carry confidence metadata in the same way

    // Output in requested format
    match format {
        "json" => print_cross_language_unified_json(&cross_lang_edges, &snapshot, verbose)?,
        _ => print_cross_language_unified_text(&cross_lang_edges, &snapshot, verbose),
    }

    Ok(())
}

/// Check if a unified edge kind matches a filter string.
fn edge_kind_matches_unified(kind: &UnifiedEdgeKind, filter: &str) -> bool {
    let kind_str = format!("{kind:?}").to_lowercase();
    let filter_lower = filter.to_lowercase();
    kind_str.contains(&filter_lower)
}

/// Print cross-language edges in text format (unified graph).
fn print_cross_language_unified_text(
    edges: &[UnifiedCrossLangEdge],
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    verbose: bool,
) {
    println!("Cross-Language Relationships (Unified Graph)");
    println!("=============================================");
    println!();
    let edge_count = edges.len();
    println!("Found {edge_count} cross-language edges");
    println!();

    for (src_id, tgt_id, kind, src_lang, tgt_lang) in edges {
        let src_name = snapshot
            .get_node(*src_id)
            .and_then(|e| {
                e.qualified_name
                    .and_then(|id| snapshot.strings().resolve(id))
                    .or_else(|| snapshot.strings().resolve(e.name))
            })
            .map_or_else(|| "?".to_string(), |s| s.to_string());

        let tgt_name = snapshot
            .get_node(*tgt_id)
            .and_then(|e| {
                e.qualified_name
                    .and_then(|id| snapshot.strings().resolve(id))
                    .or_else(|| snapshot.strings().resolve(e.name))
            })
            .map_or_else(|| "?".to_string(), |s| s.to_string());

        println!("  {src_lang:?} → {tgt_lang:?}");
        println!("  {src_name} → {tgt_name}");
        println!("  Kind: {kind:?}");

        if verbose
            && let (Some(src_entry), Some(tgt_entry)) =
                (snapshot.get_node(*src_id), snapshot.get_node(*tgt_id))
        {
            let src_file = snapshot.files().resolve(src_entry.file).map_or_else(
                || "unknown".to_string(),
                |p| p.to_string_lossy().to_string(),
            );
            let tgt_file = snapshot.files().resolve(tgt_entry.file).map_or_else(
                || "unknown".to_string(),
                |p| p.to_string_lossy().to_string(),
            );
            let src_line = src_entry.start_line;
            let tgt_line = tgt_entry.start_line;
            println!("  From: {src_file}:{src_line}");
            println!("  To:   {tgt_file}:{tgt_line}");
        }

        println!();
    }
}

/// Print cross-language edges in JSON format (unified graph).
fn print_cross_language_unified_json(
    edges: &[UnifiedCrossLangEdge],
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    verbose: bool,
) -> Result<()> {
    use serde_json::{Value, json};

    let items: Vec<_> = edges
        .iter()
        .filter_map(|(src_id, tgt_id, kind, src_lang, tgt_lang)| {
            let src_entry = snapshot.get_node(*src_id)?;
            let tgt_entry = snapshot.get_node(*tgt_id)?;

            let src_name = src_entry
                .qualified_name
                .and_then(|id| snapshot.strings().resolve(id))
                .or_else(|| snapshot.strings().resolve(src_entry.name))
                .map_or_else(|| "?".to_string(), |s| s.to_string());

            let tgt_name = tgt_entry
                .qualified_name
                .and_then(|id| snapshot.strings().resolve(id))
                .or_else(|| snapshot.strings().resolve(tgt_entry.name))
                .map_or_else(|| "?".to_string(), |s| s.to_string());

            let mut obj = json!({
                "from": {
                    "symbol": src_name,
                    "language": format!("{src_lang:?}")
                },
                "to": {
                    "symbol": tgt_name,
                    "language": format!("{tgt_lang:?}")
                },
                "kind": format!("{kind:?}"),
            });

            if verbose {
                let src_file = snapshot.files().resolve(src_entry.file).map_or_else(
                    || "unknown".to_string(),
                    |p| p.to_string_lossy().to_string(),
                );
                let tgt_file = snapshot.files().resolve(tgt_entry.file).map_or_else(
                    || "unknown".to_string(),
                    |p| p.to_string_lossy().to_string(),
                );

                obj["from"]["file"] = Value::from(src_file);
                obj["from"]["line"] = Value::from(src_entry.start_line);
                obj["to"]["file"] = Value::from(tgt_file);
                obj["to"]["line"] = Value::from(tgt_entry.start_line);
            }

            Some(obj)
        })
        .collect();

    let output = json!({
        "edges": items,
        "count": edges.len()
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ===== Graph Nodes/Edges Unified Implementation =====

const DEFAULT_GRAPH_LIST_LIMIT: usize = 1000;
const MAX_GRAPH_LIST_LIMIT: usize = 10_000;

/// Pagination options for list queries.
struct PaginationOptions {
    limit: usize,
    offset: usize,
}

/// Output formatting options.
struct OutputOptions<'a> {
    full_paths: bool,
    format: &'a str,
    verbose: bool,
}

/// Filter options for node queries.
struct NodeFilterOptions<'a> {
    kind: Option<&'a str>,
    languages: Option<&'a str>,
    file: Option<&'a str>,
    name: Option<&'a str>,
    qualified_name: Option<&'a str>,
}

/// Filter options for edge queries.
struct EdgeFilterOptions<'a> {
    kind: Option<&'a str>,
    from: Option<&'a str>,
    to: Option<&'a str>,
    from_lang: Option<&'a str>,
    to_lang: Option<&'a str>,
    file: Option<&'a str>,
}

/// List unified graph nodes with filtering.
fn run_nodes_unified(
    graph: &UnifiedCodeGraph,
    root: &Path,
    filters: &NodeFilterOptions<'_>,
    pagination: &PaginationOptions,
    output: &OutputOptions<'_>,
) -> Result<()> {
    let snapshot = graph.snapshot();
    let kind_filter = parse_node_kind_filter(filters.kind)?;
    let language_filter = parse_language_filter(filters.languages)?
        .into_iter()
        .collect::<HashSet<_>>();
    let file_filter = filters.file.map(normalize_filter_input);
    let effective_limit = normalize_graph_limit(pagination.limit);
    let show_full_paths = output.full_paths || output.verbose;

    let mut matches = Vec::new();
    for (node_id, entry) in snapshot.iter_nodes() {
        if !kind_filter.is_empty() && !kind_filter.contains(&entry.kind) {
            continue;
        }

        if !language_filter.is_empty() {
            let Some(lang) = snapshot.files().language_for_file(entry.file) else {
                continue;
            };
            if !language_filter.contains(&lang) {
                continue;
            }
        }

        if let Some(filter) = file_filter.as_deref()
            && !file_filter_matches(&snapshot, entry.file, root, filter)
        {
            continue;
        }

        if let Some(filter) = filters.name
            && !resolve_node_name(&snapshot, entry).contains(filter)
        {
            continue;
        }

        if let Some(filter) = filters.qualified_name {
            let Some(qualified) = resolve_optional_string(&snapshot, entry.qualified_name) else {
                continue;
            };
            if !qualified.contains(filter) {
                continue;
            }
        }

        matches.push(node_id);
    }

    let total = matches.len();
    let start = pagination.offset.min(total);
    let end = (start + effective_limit).min(total);
    let truncated = total > start + effective_limit;
    let page = &matches[start..end];
    let page_info = ListPage::new(total, effective_limit, pagination.offset, truncated);
    let render_paths = RenderPaths::new(root, show_full_paths);

    if output.format == "json" {
        print_nodes_unified_json(&snapshot, page, &page_info, &render_paths)
    } else {
        print_nodes_unified_text(&snapshot, page, &page_info, &render_paths, output.verbose);
        Ok(())
    }
}

/// List unified graph edges with filtering.
fn run_edges_unified(
    graph: &UnifiedCodeGraph,
    root: &Path,
    filters: &EdgeFilterOptions<'_>,
    pagination: &PaginationOptions,
    output: &OutputOptions<'_>,
) -> Result<()> {
    let snapshot = graph.snapshot();
    let kind_filter = parse_edge_kind_filter(filters.kind)?;
    let from_language = filters.from_lang.map(parse_language).transpose()?;
    let to_language = filters.to_lang.map(parse_language).transpose()?;
    let file_filter = filters.file.map(normalize_filter_input);
    let effective_limit = normalize_graph_limit(pagination.limit);
    let show_full_paths = output.full_paths || output.verbose;

    let mut matches = Vec::new();
    for (src_id, tgt_id, kind) in snapshot.iter_edges() {
        if !kind_filter.is_empty() && !kind_filter.contains(kind.tag()) {
            continue;
        }

        let (Some(src_entry), Some(tgt_entry)) =
            (snapshot.get_node(src_id), snapshot.get_node(tgt_id))
        else {
            continue;
        };

        if let Some(filter_lang) = from_language {
            let Some(lang) = snapshot.files().language_for_file(src_entry.file) else {
                continue;
            };
            if lang != filter_lang {
                continue;
            }
        }

        if let Some(filter_lang) = to_language {
            let Some(lang) = snapshot.files().language_for_file(tgt_entry.file) else {
                continue;
            };
            if lang != filter_lang {
                continue;
            }
        }

        if let Some(filter) = filters.from
            && !node_label_matches(&snapshot, src_entry, filter)
        {
            continue;
        }

        if let Some(filter) = filters.to
            && !node_label_matches(&snapshot, tgt_entry, filter)
        {
            continue;
        }

        if let Some(filter) = file_filter.as_deref()
            && !file_filter_matches(&snapshot, src_entry.file, root, filter)
        {
            continue;
        }

        matches.push((src_id, tgt_id, kind));
    }

    let total = matches.len();
    let start = pagination.offset.min(total);
    let end = (start + effective_limit).min(total);
    let truncated = total > start + effective_limit;
    let page = &matches[start..end];
    let page_info = ListPage::new(total, effective_limit, pagination.offset, truncated);
    let render_paths = RenderPaths::new(root, show_full_paths);

    if output.format == "json" {
        print_edges_unified_json(&snapshot, page, &page_info, &render_paths)
    } else {
        print_edges_unified_text(&snapshot, page, &page_info, &render_paths, output.verbose);
        Ok(())
    }
}

fn print_nodes_unified_text(
    snapshot: &UnifiedGraphSnapshot,
    nodes: &[UnifiedNodeId],
    page: &ListPage,
    paths: &RenderPaths<'_>,
    verbose: bool,
) {
    println!("Graph Nodes (Unified Graph)");
    println!("===========================");
    println!();
    let shown = nodes.len();
    println!(
        "Found {total} node(s). Showing {shown} (offset {offset}, limit {limit}).",
        total = page.total,
        offset = page.offset,
        limit = page.limit
    );
    if page.truncated {
        println!("Results truncated. Use --limit/--offset to page.");
    }
    println!();

    for (index, node_id) in nodes.iter().enumerate() {
        let Some(entry) = snapshot.get_node(*node_id) else {
            continue;
        };
        let display_index = page.offset + index + 1;
        let name = resolve_node_name(snapshot, entry);
        let qualified = resolve_optional_string(snapshot, entry.qualified_name);
        let language = resolve_node_language_text(snapshot, entry);
        let kind = entry.kind.as_str();
        let file = render_file_path(snapshot, entry.file, paths.root, paths.full_paths);

        println!("{display_index}. {name} ({kind}, {language})");
        println!(
            "   File: {file}:{}:{}",
            entry.start_line, entry.start_column
        );
        if let Some(qualified) = qualified.as_ref()
            && qualified != &name
        {
            println!("   Qualified: {qualified}");
        }

        if verbose {
            println!("   Id: {}", format_node_id(*node_id));
            if let Some(signature) = resolve_optional_string(snapshot, entry.signature) {
                println!("   Signature: {signature}");
            }
            if let Some(visibility) = resolve_optional_string(snapshot, entry.visibility) {
                println!("   Visibility: {visibility}");
            }
            println!(
                "   Location: {}:{}-{}:{}",
                entry.start_line, entry.start_column, entry.end_line, entry.end_column
            );
            println!("   Byte range: {}-{}", entry.start_byte, entry.end_byte);
            println!(
                "   Flags: async={}, static={}",
                entry.is_async, entry.is_static
            );
            if let Some(doc) = resolve_optional_string(snapshot, entry.doc) {
                let condensed = condense_whitespace(&doc);
                println!("   Doc: {condensed}");
            }
        }

        println!();
    }
}

fn print_nodes_unified_json(
    snapshot: &UnifiedGraphSnapshot,
    nodes: &[UnifiedNodeId],
    page: &ListPage,
    paths: &RenderPaths<'_>,
) -> Result<()> {
    use serde_json::json;

    let items: Vec<_> = nodes
        .iter()
        .filter_map(|node_id| {
            let entry = snapshot.get_node(*node_id)?;
            let name = resolve_node_name(snapshot, entry);
            let qualified = resolve_optional_string(snapshot, entry.qualified_name);
            let language = resolve_node_language_json(snapshot, entry);
            let file = render_file_path(snapshot, entry.file, paths.root, paths.full_paths);
            let signature = resolve_optional_string(snapshot, entry.signature);
            let doc = resolve_optional_string(snapshot, entry.doc);
            let visibility = resolve_optional_string(snapshot, entry.visibility);

            Some(json!({
                "id": node_id_json(*node_id),
                "name": name,
                "qualified_name": qualified,
                "kind": entry.kind.as_str(),
                "language": language,
                "file": file,
                "location": {
                    "start_line": entry.start_line,
                    "start_column": entry.start_column,
                    "end_line": entry.end_line,
                    "end_column": entry.end_column,
                },
                "byte_range": {
                    "start": entry.start_byte,
                    "end": entry.end_byte,
                },
                "signature": signature,
                "doc": doc,
                "visibility": visibility,
                "is_async": entry.is_async,
                "is_static": entry.is_static,
            }))
        })
        .collect();

    let output = json!({
        "count": page.total,
        "limit": page.limit,
        "offset": page.offset,
        "truncated": page.truncated,
        "nodes": items,
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn print_edges_unified_text(
    snapshot: &UnifiedGraphSnapshot,
    edges: &[(UnifiedNodeId, UnifiedNodeId, UnifiedEdgeKind)],
    page: &ListPage,
    paths: &RenderPaths<'_>,
    verbose: bool,
) {
    println!("Graph Edges (Unified Graph)");
    println!("===========================");
    println!();
    let shown = edges.len();
    println!(
        "Found {total} edge(s). Showing {shown} (offset {offset}, limit {limit}).",
        total = page.total,
        offset = page.offset,
        limit = page.limit
    );
    if page.truncated {
        println!("Results truncated. Use --limit/--offset to page.");
    }
    println!();

    for (index, (src_id, tgt_id, kind)) in edges.iter().enumerate() {
        let (Some(src_entry), Some(tgt_entry)) =
            (snapshot.get_node(*src_id), snapshot.get_node(*tgt_id))
        else {
            continue;
        };
        let display_index = page.offset + index + 1;
        let src_name = resolve_node_label(snapshot, src_entry);
        let tgt_name = resolve_node_label(snapshot, tgt_entry);
        let src_lang = resolve_node_language_text(snapshot, src_entry);
        let tgt_lang = resolve_node_language_text(snapshot, tgt_entry);
        let file = render_file_path(snapshot, src_entry.file, paths.root, paths.full_paths);

        println!("{display_index}. {src_name} ({src_lang}) → {tgt_name} ({tgt_lang})");
        println!("   Kind: {}", kind.tag());
        println!("   File: {file}");

        if verbose {
            println!(
                "   Source: {}:{}:{}",
                file, src_entry.start_line, src_entry.start_column
            );
            let target_file =
                render_file_path(snapshot, tgt_entry.file, paths.root, paths.full_paths);
            println!(
                "   Target: {}:{}:{}",
                target_file, tgt_entry.start_line, tgt_entry.start_column
            );
            println!("   Source Id: {}", format_node_id(*src_id));
            println!("   Target Id: {}", format_node_id(*tgt_id));
            print_edge_metadata_text(snapshot, kind);
        }

        println!();
    }
}

fn print_edges_unified_json(
    snapshot: &UnifiedGraphSnapshot,
    edges: &[(UnifiedNodeId, UnifiedNodeId, UnifiedEdgeKind)],
    page: &ListPage,
    paths: &RenderPaths<'_>,
) -> Result<()> {
    use serde_json::json;

    let items: Vec<_> = edges
        .iter()
        .filter_map(|(src_id, tgt_id, kind)| {
            let src_entry = snapshot.get_node(*src_id)?;
            let tgt_entry = snapshot.get_node(*tgt_id)?;
            let file = render_file_path(snapshot, src_entry.file, paths.root, paths.full_paths);

            Some(json!({
                "source": node_ref_json(snapshot, *src_id, src_entry, paths.root, paths.full_paths),
                "target": node_ref_json(snapshot, *tgt_id, tgt_entry, paths.root, paths.full_paths),
                "kind": kind.tag(),
                "file": file,
                "metadata": edge_metadata_json(snapshot, kind),
            }))
        })
        .collect();

    let output = json!({
        "count": page.total,
        "limit": page.limit,
        "offset": page.offset,
        "truncated": page.truncated,
        "edges": items,
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ===== Complexity Unified Implementation =====

/// Result type for complexity metrics in unified graph
type UnifiedComplexityResult = (UnifiedNodeId, usize);

/// Calculate and display complexity metrics using the unified graph architecture.
fn run_complexity_unified(
    graph: &UnifiedCodeGraph,
    target: Option<&str>,
    sort: bool,
    min_complexity: usize,
    languages: Option<&str>,
    format: &str,
    verbose: bool,
) -> Result<()> {
    let snapshot = graph.snapshot();

    // Parse language filter
    let language_list = parse_language_filter_for_complexity(languages)?;
    let language_filter: HashSet<_> = language_list.into_iter().collect();

    // Calculate complexity for all functions
    let mut complexities =
        calculate_complexity_metrics_unified(&snapshot, target, &language_filter);

    // Filter by minimum complexity
    complexities.retain(|(_, score)| *score >= min_complexity);

    // Sort if requested
    if sort {
        complexities.sort_by(|a, b| b.1.cmp(&a.1));
    }

    if verbose {
        eprintln!(
            "Analyzed {} functions (min_complexity={})",
            complexities.len(),
            min_complexity
        );
    }

    match format {
        "json" => print_complexity_unified_json(&complexities, &snapshot)?,
        _ => print_complexity_unified_text(&complexities, &snapshot),
    }

    Ok(())
}

/// Parse language filter for complexity command.
fn parse_language_filter_for_complexity(languages: Option<&str>) -> Result<Vec<Language>> {
    if let Some(langs) = languages {
        langs.split(',').map(|s| parse_language(s.trim())).collect()
    } else {
        Ok(Vec::new())
    }
}

/// Calculate complexity metrics for all functions in the unified graph.
fn calculate_complexity_metrics_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    target: Option<&str>,
    language_filter: &HashSet<Language>,
) -> Vec<UnifiedComplexityResult> {
    use sqry_core::graph::unified::node::NodeKind as UnifiedNodeKind;

    let mut complexities = Vec::new();

    for (node_id, entry) in snapshot.iter_nodes() {
        if !node_matches_language_filter(snapshot, entry, language_filter) {
            continue;
        }

        if !matches!(
            entry.kind,
            UnifiedNodeKind::Function | UnifiedNodeKind::Method
        ) {
            continue;
        }

        if !node_matches_target(snapshot, entry, target) {
            continue;
        }

        // Calculate complexity score
        let score = calculate_complexity_score_unified(snapshot, node_id);
        complexities.push((node_id, score));
    }

    complexities
}

fn node_matches_language_filter(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    entry: &NodeEntry,
    language_filter: &HashSet<Language>,
) -> bool {
    if language_filter.is_empty() {
        return true;
    }

    let Some(lang) = snapshot.files().language_for_file(entry.file) else {
        return false;
    };
    language_filter.contains(&lang)
}

fn node_matches_target(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    entry: &NodeEntry,
    target: Option<&str>,
) -> bool {
    let Some(target_name) = target else {
        return true;
    };

    let name = entry
        .qualified_name
        .and_then(|id| snapshot.strings().resolve(id))
        .or_else(|| snapshot.strings().resolve(entry.name))
        .map_or_else(String::new, |s| s.to_string());

    name.contains(target_name)
}

/// Calculate complexity score for a single function in the unified graph.
fn calculate_complexity_score_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node_id: UnifiedNodeId,
) -> usize {
    use sqry_core::graph::unified::edge::EdgeKind as UnifiedEdgeKindEnum;

    // Simple complexity metric: count of outgoing call edges + call chain depth
    let mut call_count = 0;
    let mut max_depth = 0;

    // Count direct calls by iterating over all outgoing edges
    for edge_ref in snapshot.edges().edges_from(node_id) {
        if matches!(edge_ref.kind, UnifiedEdgeKindEnum::Calls { .. }) {
            call_count += 1;

            // Calculate depth to this callee
            let depth = calculate_call_depth_unified(snapshot, edge_ref.target, 1);
            max_depth = max_depth.max(depth);
        }
    }

    // Complexity = direct calls + max chain depth
    call_count + max_depth
}

/// Calculate call depth from a node in the unified graph.
fn calculate_call_depth_unified(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node_id: UnifiedNodeId,
    current_depth: usize,
) -> usize {
    use sqry_core::graph::unified::edge::EdgeKind as UnifiedEdgeKindEnum;

    const MAX_DEPTH: usize = 20; // Prevent infinite recursion

    if current_depth >= MAX_DEPTH {
        return current_depth;
    }

    let mut max_child_depth = current_depth;

    for edge_ref in snapshot.edges().edges_from(node_id) {
        if matches!(edge_ref.kind, UnifiedEdgeKindEnum::Calls { .. }) {
            let child_depth =
                calculate_call_depth_unified(snapshot, edge_ref.target, current_depth + 1);
            max_child_depth = max_child_depth.max(child_depth);
        }
    }

    max_child_depth
}

/// Print complexity metrics in text format (unified graph).
fn print_complexity_unified_text(
    complexities: &[UnifiedComplexityResult],
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) {
    println!("Code Complexity Metrics (Unified Graph)");
    println!("=======================================");
    println!();
    let complexity_count = complexities.len();
    println!("Analyzed {complexity_count} functions");
    println!();

    if complexities.is_empty() {
        println!("No functions found matching the criteria.");
        return;
    }

    // Calculate statistics
    let scores: Vec<_> = complexities.iter().map(|(_, score)| *score).collect();
    let total: usize = scores.iter().sum();
    #[allow(clippy::cast_precision_loss)] // Display-only metric; precision is non-critical.
    let avg = total as f64 / scores.len() as f64;
    let max = *scores.iter().max().unwrap_or(&0);

    println!("Statistics:");
    println!("  Average complexity: {avg:.1}");
    println!("  Maximum complexity: {max}");
    println!();

    println!("Functions by complexity:");
    for (node_id, score) in complexities {
        let bars = "█".repeat((*score).min(50));

        let (name, file, lang_str) = if let Some(entry) = snapshot.get_node(*node_id) {
            let n = entry
                .qualified_name
                .and_then(|id| snapshot.strings().resolve(id))
                .or_else(|| snapshot.strings().resolve(entry.name))
                .map_or_else(|| "?".to_string(), |s| s.to_string());

            let f = snapshot.files().resolve(entry.file).map_or_else(
                || "unknown".to_string(),
                |p| p.to_string_lossy().to_string(),
            );

            let l = snapshot
                .files()
                .language_for_file(entry.file)
                .map_or_else(|| "Unknown".to_string(), |lang| format!("{lang:?}"));

            (n, f, l)
        } else {
            (
                "?".to_string(),
                "unknown".to_string(),
                "Unknown".to_string(),
            )
        };

        println!("  {bars} {score:3} {lang_str}:{file}:{name}");
    }
}

/// Print complexity metrics in JSON format (unified graph).
fn print_complexity_unified_json(
    complexities: &[UnifiedComplexityResult],
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> Result<()> {
    use serde_json::json;

    let items: Vec<_> = complexities
        .iter()
        .filter_map(|(node_id, score)| {
            let entry = snapshot.get_node(*node_id)?;

            let name = entry
                .qualified_name
                .and_then(|id| snapshot.strings().resolve(id))
                .or_else(|| snapshot.strings().resolve(entry.name))
                .map_or_else(|| "?".to_string(), |s| s.to_string());

            let file = snapshot.files().resolve(entry.file).map_or_else(
                || "unknown".to_string(),
                |p| p.to_string_lossy().to_string(),
            );

            let language = snapshot
                .files()
                .language_for_file(entry.file)
                .map_or_else(|| "Unknown".to_string(), |l| format!("{l:?}"));

            Some(json!({
                "symbol": name,
                "file": file,
                "language": language,
                "complexity": score,
            }))
        })
        .collect();

    let output = json!({
        "function_count": complexities.len(),
        "functions": items,
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

// ===== Helper Functions =====

const VALID_NODE_KIND_NAMES: &[&str] = &[
    "function",
    "method",
    "class",
    "interface",
    "trait",
    "module",
    "variable",
    "constant",
    "type",
    "struct",
    "enum",
    "enum_variant",
    "macro",
    "call_site",
    "import",
    "export",
    "lifetime",
    "component",
    "service",
    "resource",
    "endpoint",
    "test",
    "other",
];

const VALID_EDGE_KIND_TAGS: &[&str] = &[
    "defines",
    "contains",
    "calls",
    "references",
    "imports",
    "exports",
    "type_of",
    "inherits",
    "implements",
    "lifetime_constraint",
    "trait_method_binding",
    "macro_expansion",
    "ffi_call",
    "http_request",
    "grpc_call",
    "web_assembly_call",
    "db_query",
    "table_read",
    "table_write",
    "triggered_by",
    "message_queue",
    "web_socket",
    "graphql_operation",
    "process_exec",
    "file_ipc",
    "protocol_call",
];

struct ListPage {
    total: usize,
    limit: usize,
    offset: usize,
    truncated: bool,
}

impl ListPage {
    fn new(total: usize, limit: usize, offset: usize, truncated: bool) -> Self {
        Self {
            total,
            limit,
            offset,
            truncated,
        }
    }
}

struct RenderPaths<'a> {
    root: &'a Path,
    full_paths: bool,
}

impl<'a> RenderPaths<'a> {
    fn new(root: &'a Path, full_paths: bool) -> Self {
        Self { root, full_paths }
    }
}

fn normalize_graph_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_GRAPH_LIST_LIMIT
    } else {
        limit.min(MAX_GRAPH_LIST_LIMIT)
    }
}

fn normalize_filter_input(input: &str) -> String {
    input.trim().replace('\\', "/").to_ascii_lowercase()
}

fn normalize_path_for_match(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn file_filter_matches(
    snapshot: &UnifiedGraphSnapshot,
    file_id: sqry_core::graph::unified::FileId,
    root: &Path,
    filter: &str,
) -> bool {
    let Some(path) = snapshot.files().resolve(file_id) else {
        return false;
    };
    let normalized = normalize_path_for_match(&path);
    if normalized.contains(filter) {
        return true;
    }

    if let Ok(relative) = path.strip_prefix(root) {
        let normalized_relative = normalize_path_for_match(relative);
        if normalized_relative.contains(filter) {
            return true;
        }
    }

    false
}

fn render_file_path(
    snapshot: &UnifiedGraphSnapshot,
    file_id: sqry_core::graph::unified::FileId,
    root: &Path,
    full_paths: bool,
) -> String {
    snapshot.files().resolve(file_id).map_or_else(
        || "unknown".to_string(),
        |path| {
            if full_paths {
                path.to_string_lossy().to_string()
            } else if let Ok(relative) = path.strip_prefix(root) {
                relative.to_string_lossy().to_string()
            } else {
                path.to_string_lossy().to_string()
            }
        },
    )
}

fn resolve_optional_string(
    snapshot: &UnifiedGraphSnapshot,
    value: Option<StringId>,
) -> Option<String> {
    value
        .and_then(|id| snapshot.strings().resolve(id))
        .map(|s| s.to_string())
}

fn resolve_node_language_text(snapshot: &UnifiedGraphSnapshot, entry: &NodeEntry) -> String {
    snapshot
        .files()
        .language_for_file(entry.file)
        .map_or_else(|| "Unknown".to_string(), |lang| format!("{lang:?}"))
}

fn resolve_node_language_json(snapshot: &UnifiedGraphSnapshot, entry: &NodeEntry) -> String {
    snapshot
        .files()
        .language_for_file(entry.file)
        .map_or_else(|| "unknown".to_string(), |lang| lang.to_string())
}

fn node_label_matches(snapshot: &UnifiedGraphSnapshot, entry: &NodeEntry, filter: &str) -> bool {
    let name = resolve_node_name(snapshot, entry);
    if name.contains(filter) {
        return true;
    }

    if let Some(qualified) = resolve_optional_string(snapshot, entry.qualified_name)
        && qualified.contains(filter)
    {
        return true;
    }

    false
}

fn condense_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_node_id(node_id: UnifiedNodeId) -> String {
    format!(
        "index={}, generation={}",
        node_id.index(),
        node_id.generation()
    )
}

fn node_id_json(node_id: UnifiedNodeId) -> serde_json::Value {
    use serde_json::json;

    json!({
        "index": node_id.index(),
        "generation": node_id.generation(),
    })
}

fn node_ref_json(
    snapshot: &UnifiedGraphSnapshot,
    node_id: UnifiedNodeId,
    entry: &NodeEntry,
    root: &Path,
    full_paths: bool,
) -> serde_json::Value {
    use serde_json::json;

    let name = resolve_node_name(snapshot, entry);
    let qualified = resolve_optional_string(snapshot, entry.qualified_name);
    let language = resolve_node_language_json(snapshot, entry);
    let file = render_file_path(snapshot, entry.file, root, full_paths);

    json!({
        "id": node_id_json(node_id),
        "name": name,
        "qualified_name": qualified,
        "language": language,
        "file": file,
        "location": {
            "start_line": entry.start_line,
            "start_column": entry.start_column,
            "end_line": entry.end_line,
            "end_column": entry.end_column,
        },
    })
}

fn resolve_string_id(snapshot: &UnifiedGraphSnapshot, id: StringId) -> Option<String> {
    snapshot.strings().resolve(id).map(|s| s.to_string())
}

#[allow(clippy::too_many_lines)] // Exhaustive edge metadata mapping; keep variants together.
fn edge_metadata_json(
    snapshot: &UnifiedGraphSnapshot,
    kind: &UnifiedEdgeKind,
) -> serde_json::Value {
    use serde_json::json;

    match kind {
        UnifiedEdgeKind::Defines
        | UnifiedEdgeKind::Contains
        | UnifiedEdgeKind::References
        | UnifiedEdgeKind::TypeOf { .. }
        | UnifiedEdgeKind::Inherits
        | UnifiedEdgeKind::Implements
        | UnifiedEdgeKind::WebAssemblyCall => json!({}),
        UnifiedEdgeKind::Calls {
            argument_count,
            is_async,
        } => json!({
            "argument_count": argument_count,
            "is_async": is_async,
        }),
        UnifiedEdgeKind::Imports { alias, is_wildcard } => json!({
            "alias": alias.and_then(|id| resolve_string_id(snapshot, id)),
            "is_wildcard": is_wildcard,
        }),
        UnifiedEdgeKind::Exports { kind, alias } => json!({
            "kind": kind,
            "alias": alias.and_then(|id| resolve_string_id(snapshot, id)),
        }),
        UnifiedEdgeKind::LifetimeConstraint { constraint_kind } => json!({
            "constraint_kind": constraint_kind,
        }),
        UnifiedEdgeKind::TraitMethodBinding {
            trait_name,
            impl_type,
            is_ambiguous,
        } => json!({
            "trait_name": resolve_string_id(snapshot, *trait_name),
            "impl_type": resolve_string_id(snapshot, *impl_type),
            "is_ambiguous": is_ambiguous,
        }),
        UnifiedEdgeKind::MacroExpansion {
            expansion_kind,
            is_verified,
        } => json!({
            "expansion_kind": expansion_kind,
            "is_verified": is_verified,
        }),
        UnifiedEdgeKind::FfiCall { convention } => json!({
            "convention": convention,
        }),
        UnifiedEdgeKind::HttpRequest { method, url } => json!({
            "method": method,
            "url": url.and_then(|id| resolve_string_id(snapshot, id)),
        }),
        UnifiedEdgeKind::GrpcCall { service, method } => json!({
            "service": resolve_string_id(snapshot, *service),
            "method": resolve_string_id(snapshot, *method),
        }),
        UnifiedEdgeKind::DbQuery { query_type, table } => json!({
            "query_type": query_type,
            "table": table.and_then(|id| resolve_string_id(snapshot, id)),
        }),
        UnifiedEdgeKind::TableRead { table_name, schema } => json!({
            "table_name": resolve_string_id(snapshot, *table_name),
            "schema": schema.and_then(|id| resolve_string_id(snapshot, id)),
        }),
        UnifiedEdgeKind::TableWrite {
            table_name,
            schema,
            operation,
        } => json!({
            "table_name": resolve_string_id(snapshot, *table_name),
            "schema": schema.and_then(|id| resolve_string_id(snapshot, id)),
            "operation": operation,
        }),
        UnifiedEdgeKind::TriggeredBy {
            trigger_name,
            schema,
        } => json!({
            "trigger_name": resolve_string_id(snapshot, *trigger_name),
            "schema": schema.and_then(|id| resolve_string_id(snapshot, id)),
        }),
        UnifiedEdgeKind::MessageQueue { protocol, topic } => {
            let protocol_value = match protocol {
                MqProtocol::Kafka => Some("kafka".to_string()),
                MqProtocol::Sqs => Some("sqs".to_string()),
                MqProtocol::RabbitMq => Some("rabbit_mq".to_string()),
                MqProtocol::Nats => Some("nats".to_string()),
                MqProtocol::Redis => Some("redis".to_string()),
                MqProtocol::Other(id) => resolve_string_id(snapshot, *id),
            };
            json!({
                "protocol": protocol_value,
                "topic": topic.and_then(|id| resolve_string_id(snapshot, id)),
            })
        }
        UnifiedEdgeKind::WebSocket { event } => json!({
            "event": event.and_then(|id| resolve_string_id(snapshot, id)),
        }),
        UnifiedEdgeKind::GraphQLOperation { operation } => json!({
            "operation": resolve_string_id(snapshot, *operation),
        }),
        UnifiedEdgeKind::ProcessExec { command } => json!({
            "command": resolve_string_id(snapshot, *command),
        }),
        UnifiedEdgeKind::FileIpc { path_pattern } => json!({
            "path_pattern": path_pattern.and_then(|id| resolve_string_id(snapshot, id)),
        }),
        UnifiedEdgeKind::ProtocolCall { protocol, metadata } => json!({
            "protocol": resolve_string_id(snapshot, *protocol),
            "metadata": metadata.and_then(|id| resolve_string_id(snapshot, id)),
        }),
    }
}

fn print_edge_metadata_text(snapshot: &UnifiedGraphSnapshot, kind: &UnifiedEdgeKind) {
    let metadata = edge_metadata_json(snapshot, kind);
    let Some(map) = metadata.as_object() else {
        return;
    };
    if map.is_empty() {
        return;
    }
    if let Ok(serialized) = serde_json::to_string(map) {
        println!("   Metadata: {serialized}");
    }
}

fn parse_node_kind_filter(kinds: Option<&str>) -> Result<HashSet<UnifiedNodeKind>> {
    let mut filter = HashSet::new();
    let Some(kinds) = kinds else {
        return Ok(filter);
    };
    for raw in kinds.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.to_ascii_lowercase();
        let Some(kind) = UnifiedNodeKind::parse(&normalized) else {
            return Err(anyhow::anyhow!(
                "Unknown node kind: {trimmed}. Valid kinds: {}",
                VALID_NODE_KIND_NAMES.join(", ")
            ));
        };
        filter.insert(kind);
    }
    Ok(filter)
}

fn parse_edge_kind_filter(kinds: Option<&str>) -> Result<HashSet<String>> {
    let mut filter = HashSet::new();
    let Some(kinds) = kinds else {
        return Ok(filter);
    };
    for raw in kinds.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = trimmed.to_ascii_lowercase();
        if !VALID_EDGE_KIND_TAGS.contains(&normalized.as_str()) {
            return Err(anyhow::anyhow!(
                "Unknown edge kind: {trimmed}. Valid kinds: {}",
                VALID_EDGE_KIND_TAGS.join(", ")
            ));
        }
        filter.insert(normalized);
    }
    Ok(filter)
}

fn display_languages(languages: &HashSet<Language>) -> String {
    let mut items: Vec<Language> = languages.iter().copied().collect();
    items.sort();
    items
        .into_iter()
        .map(|lang| lang.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_language_filter(languages: Option<&str>) -> Result<Vec<Language>> {
    if let Some(langs) = languages {
        langs.split(',').map(|s| parse_language(s.trim())).collect()
    } else {
        Ok(Vec::new())
    }
}

fn parse_language(s: &str) -> Result<Language> {
    match s.to_lowercase().as_str() {
        // Phase 0 languages
        "javascript" | "js" => Ok(Language::JavaScript),
        "typescript" | "ts" => Ok(Language::TypeScript),
        "python" | "py" => Ok(Language::Python),
        "cpp" | "c++" | "cxx" => Ok(Language::Cpp),
        // Phase 1 languages
        "rust" | "rs" => Ok(Language::Rust),
        "go" => Ok(Language::Go),
        "java" => Ok(Language::Java),
        "c" => Ok(Language::C),
        "csharp" | "cs" => Ok(Language::CSharp),
        // Phase 2 languages
        "ruby" => Ok(Language::Ruby),
        "php" => Ok(Language::Php),
        "swift" => Ok(Language::Swift),
        // Phase 3 languages
        "kotlin" => Ok(Language::Kotlin),
        "scala" => Ok(Language::Scala),
        "sql" => Ok(Language::Sql),
        "dart" => Ok(Language::Dart),
        // Phase 5A languages
        "lua" => Ok(Language::Lua),
        "perl" => Ok(Language::Perl),
        "shell" | "bash" => Ok(Language::Shell),
        "groovy" => Ok(Language::Groovy),
        // Phase 5B languages
        "elixir" | "ex" => Ok(Language::Elixir),
        "r" => Ok(Language::R),
        // Phase 5C languages
        "haskell" | "hs" => Ok(Language::Haskell),
        "svelte" => Ok(Language::Svelte),
        "vue" => Ok(Language::Vue),
        "zig" => Ok(Language::Zig),
        // Other
        "http" => Ok(Language::Http),
        _ => bail!("Unknown language: {s}"),
    }
}

// ===== Direct Callers/Callees =====

/// Options for direct callers/callees lookup.
struct DirectCallOptions<'a> {
    /// Symbol name to search for.
    symbol: &'a str,
    /// Maximum number of results to return.
    limit: usize,
    /// Optional language filter (comma-separated).
    languages: Option<&'a str>,
    /// Show full file paths instead of relative.
    full_paths: bool,
    /// Output format: "text" or "json".
    format: &'a str,
    /// Enable verbose output.
    verbose: bool,
}

/// Find all direct callers of a symbol using the unified graph.
fn run_direct_callers_unified(
    graph: &UnifiedCodeGraph,
    root: &Path,
    options: &DirectCallOptions<'_>,
) -> Result<()> {
    use serde_json::json;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files = snapshot.files();

    let language_filter = parse_language_filter(options.languages)?
        .into_iter()
        .collect::<HashSet<_>>();

    // Find the target node(s) matching the symbol
    let target_nodes = find_nodes_by_name(&snapshot, options.symbol);

    if target_nodes.is_empty() {
        bail!(
            "Symbol '{symbol}' not found in the graph",
            symbol = options.symbol
        );
    }

    if options.verbose {
        eprintln!(
            "Found {count} node(s) matching symbol '{symbol}'",
            count = target_nodes.len(),
            symbol = options.symbol
        );
    }

    // Collect all callers
    let mut callers = Vec::new();
    let reverse_store = snapshot.edges().reverse();

    for target_id in &target_nodes {
        for edge_ref in reverse_store.edges_from(*target_id) {
            if callers.len() >= options.limit {
                break;
            }

            // In reverse store, edge_ref.target is the actual caller
            let caller_id = edge_ref.target;

            // Filter by edge type - only Calls edges
            if !matches!(edge_ref.kind, UnifiedEdgeKind::Calls { .. }) {
                continue;
            }

            if let Some(entry) = snapshot.nodes().get(caller_id) {
                // Apply language filter
                if !language_filter.is_empty()
                    && let Some(lang) = files.language_for_file(entry.file)
                    && !language_filter.contains(&lang)
                {
                    continue;
                }

                let name = strings.resolve(entry.name).unwrap_or_default().to_string();
                let qualified_name = entry
                    .qualified_name
                    .and_then(|id| strings.resolve(id))
                    .map_or_else(|| name.clone(), |s| s.to_string());
                let language = files
                    .language_for_file(entry.file)
                    .map_or_else(|| "unknown".to_string(), |l| l.to_string());
                let file_path = files
                    .resolve(entry.file)
                    .map(|p| {
                        if options.full_paths {
                            p.display().to_string()
                        } else {
                            p.strip_prefix(root)
                                .unwrap_or(p.as_ref())
                                .display()
                                .to_string()
                        }
                    })
                    .unwrap_or_default();

                callers.push(json!({
                    "name": name,
                    "qualified_name": qualified_name,
                    "kind": format!("{:?}", entry.kind),
                    "file": file_path,
                    "line": entry.start_line,
                    "language": language
                }));
            }
        }
    }

    if options.format == "json" {
        let output = json!({
            "symbol": options.symbol,
            "callers": callers,
            "total": callers.len(),
            "truncated": callers.len() >= options.limit
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Callers of '{symbol}':", symbol = options.symbol);
        println!();
        if callers.is_empty() {
            println!("  (no callers found)");
        } else {
            for caller in &callers {
                let name = caller["qualified_name"].as_str().unwrap_or("");
                let file = caller["file"].as_str().unwrap_or("");
                let line = caller["line"].as_u64().unwrap_or(0);
                println!("  {name} ({file}:{line})");
            }
            println!();
            println!("Total: {total} caller(s)", total = callers.len());
        }
    }

    Ok(())
}

/// Find all direct callees of a symbol using the unified graph.
fn run_direct_callees_unified(
    graph: &UnifiedCodeGraph,
    root: &Path,
    options: &DirectCallOptions<'_>,
) -> Result<()> {
    use serde_json::json;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files = snapshot.files();

    let language_filter = parse_language_filter(options.languages)?
        .into_iter()
        .collect::<HashSet<_>>();

    // Find the source node(s) matching the symbol
    let source_nodes = find_nodes_by_name(&snapshot, options.symbol);

    if source_nodes.is_empty() {
        bail!(
            "Symbol '{symbol}' not found in the graph",
            symbol = options.symbol
        );
    }

    if options.verbose {
        eprintln!(
            "Found {count} node(s) matching symbol '{symbol}'",
            count = source_nodes.len(),
            symbol = options.symbol
        );
    }

    // Collect all callees
    let mut callees = Vec::new();
    let edge_store = snapshot.edges();

    for source_id in &source_nodes {
        for edge_ref in edge_store.edges_from(*source_id) {
            if callees.len() >= options.limit {
                break;
            }

            // Filter by edge type - only Calls edges
            if !matches!(edge_ref.kind, UnifiedEdgeKind::Calls { .. }) {
                continue;
            }

            let callee_id = edge_ref.target;

            if let Some(entry) = snapshot.nodes().get(callee_id) {
                // Apply language filter
                if !language_filter.is_empty()
                    && let Some(lang) = files.language_for_file(entry.file)
                    && !language_filter.contains(&lang)
                {
                    continue;
                }

                let name = strings.resolve(entry.name).unwrap_or_default().to_string();
                let qualified_name = entry
                    .qualified_name
                    .and_then(|id| strings.resolve(id))
                    .map_or_else(|| name.clone(), |s| s.to_string());
                let language = files
                    .language_for_file(entry.file)
                    .map_or_else(|| "unknown".to_string(), |l| l.to_string());
                let file_path = files
                    .resolve(entry.file)
                    .map(|p| {
                        if options.full_paths {
                            p.display().to_string()
                        } else {
                            p.strip_prefix(root)
                                .unwrap_or(p.as_ref())
                                .display()
                                .to_string()
                        }
                    })
                    .unwrap_or_default();

                callees.push(json!({
                    "name": name,
                    "qualified_name": qualified_name,
                    "kind": format!("{:?}", entry.kind),
                    "file": file_path,
                    "line": entry.start_line,
                    "language": language
                }));
            }
        }
    }

    if options.format == "json" {
        let output = json!({
            "symbol": options.symbol,
            "callees": callees,
            "total": callees.len(),
            "truncated": callees.len() >= options.limit
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Callees of '{symbol}':", symbol = options.symbol);
        println!();
        if callees.is_empty() {
            println!("  (no callees found)");
        } else {
            for callee in &callees {
                let name = callee["qualified_name"].as_str().unwrap_or("");
                let file = callee["file"].as_str().unwrap_or("");
                let line = callee["line"].as_u64().unwrap_or(0);
                println!("  {name} ({file}:{line})");
            }
            println!();
            println!("Total: {total} callee(s)", total = callees.len());
        }
    }

    Ok(())
}

// ===== Call Hierarchy =====

/// Options for call hierarchy display.
struct CallHierarchyOptions<'a> {
    /// Symbol name to search for.
    symbol: &'a str,
    /// Maximum traversal depth.
    max_depth: usize,
    /// Direction: "incoming", "outgoing", or "both".
    direction: &'a str,
    /// Optional language filter (comma-separated).
    languages: Option<&'a str>,
    /// Show full file paths instead of relative.
    full_paths: bool,
    /// Output format: "text" or "json".
    format: &'a str,
    /// Enable verbose output.
    verbose: bool,
}

/// Show call hierarchy for a symbol using the unified graph.
fn run_call_hierarchy_unified(
    graph: &UnifiedCodeGraph,
    root: &Path,
    options: &CallHierarchyOptions<'_>,
) -> Result<()> {
    use serde_json::json;

    let snapshot = graph.snapshot();

    let language_filter = parse_language_filter(options.languages)?
        .into_iter()
        .collect::<HashSet<_>>();

    // Find the node(s) matching the symbol
    let start_nodes = find_nodes_by_name(&snapshot, options.symbol);

    if start_nodes.is_empty() {
        bail!("Symbol '{}' not found in the graph", options.symbol);
    }

    if options.verbose {
        eprintln!(
            "Found {} node(s) matching symbol '{}' (direction={})",
            start_nodes.len(),
            options.symbol,
            options.direction
        );
    }

    let include_incoming = options.direction == "incoming" || options.direction == "both";
    let include_outgoing = options.direction == "outgoing" || options.direction == "both";

    let mut result = json!({
        "symbol": options.symbol,
        "direction": options.direction,
        "max_depth": options.max_depth
    });

    // Build incoming hierarchy (callers)
    if include_incoming {
        let incoming = build_call_hierarchy_tree(
            &snapshot,
            &start_nodes,
            options.max_depth,
            true, // incoming
            &language_filter,
            root,
            options.full_paths,
        );
        result["incoming"] = incoming;
    }

    // Build outgoing hierarchy (callees)
    if include_outgoing {
        let outgoing = build_call_hierarchy_tree(
            &snapshot,
            &start_nodes,
            options.max_depth,
            false, // outgoing
            &language_filter,
            root,
            options.full_paths,
        );
        result["outgoing"] = outgoing;
    }

    if options.format == "json" {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Call hierarchy for '{symbol}':", symbol = options.symbol);
        println!();

        if include_incoming {
            println!("Incoming calls (callers):");
            if let Some(incoming) = result["incoming"].as_array() {
                print_hierarchy_text(incoming, 1);
            }
            println!();
        }

        if include_outgoing {
            println!("Outgoing calls (callees):");
            if let Some(outgoing) = result["outgoing"].as_array() {
                print_hierarchy_text(outgoing, 1);
            }
        }
    }

    Ok(())
}

/// Build a call hierarchy tree.
#[allow(clippy::items_after_statements, clippy::too_many_lines)]
fn build_call_hierarchy_tree(
    snapshot: &UnifiedGraphSnapshot,
    start_nodes: &[sqry_core::graph::unified::node::NodeId],
    max_depth: usize,
    incoming: bool,
    language_filter: &HashSet<Language>,
    root: &Path,
    full_paths: bool,
) -> serde_json::Value {
    use serde_json::json;
    use sqry_core::graph::unified::node::NodeId as UnifiedNodeId;

    let _strings = snapshot.strings();
    let _files = snapshot.files();

    let mut result = Vec::new();
    let mut visited = HashSet::new();

    /// Configuration for call hierarchy traversal.
    struct TraversalConfig<'a> {
        max_depth: usize,
        incoming: bool,
        language_filter: &'a HashSet<Language>,
        root: &'a Path,
        full_paths: bool,
    }

    fn traverse(
        snapshot: &UnifiedGraphSnapshot,
        node_id: UnifiedNodeId,
        depth: usize,
        config: &TraversalConfig<'_>,
        visited: &mut HashSet<UnifiedNodeId>,
    ) -> serde_json::Value {
        let strings = snapshot.strings();
        let files = snapshot.files();

        let Some(entry) = snapshot.nodes().get(node_id) else {
            return json!(null);
        };

        let name = strings.resolve(entry.name).unwrap_or_default().to_string();
        let qualified_name = entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .map_or_else(|| name.clone(), |s| s.to_string());
        let language = files
            .language_for_file(entry.file)
            .map_or_else(|| "unknown".to_string(), |l| l.to_string());
        let file_path = files
            .resolve(entry.file)
            .map(|p| {
                if config.full_paths {
                    p.display().to_string()
                } else {
                    p.strip_prefix(config.root)
                        .unwrap_or(p.as_ref())
                        .display()
                        .to_string()
                }
            })
            .unwrap_or_default();

        let mut node_json = json!({
            "name": name,
            "qualified_name": qualified_name,
            "kind": format!("{:?}", entry.kind),
            "file": file_path,
            "line": entry.start_line,
            "language": language
        });

        // Recurse if not at max depth and not visited
        if depth < config.max_depth && !visited.contains(&node_id) {
            visited.insert(node_id);

            let mut children = Vec::new();
            let edges = if config.incoming {
                snapshot.edges().reverse().edges_from(node_id)
            } else {
                snapshot.edges().edges_from(node_id)
            };

            for edge_ref in edges {
                if !matches!(edge_ref.kind, UnifiedEdgeKind::Calls { .. }) {
                    continue;
                }

                let related_id = edge_ref.target;

                // Apply language filter
                if !config.language_filter.is_empty()
                    && let Some(related_entry) = snapshot.nodes().get(related_id)
                    && let Some(lang) = files.language_for_file(related_entry.file)
                    && !config.language_filter.contains(&lang)
                {
                    continue;
                }

                let child = traverse(snapshot, related_id, depth + 1, config, visited);

                if !child.is_null() {
                    children.push(child);
                }
            }

            if !children.is_empty() {
                node_json["children"] = json!(children);
            }
        }

        node_json
    }

    let config = TraversalConfig {
        max_depth,
        incoming,
        language_filter,
        root,
        full_paths,
    };

    for &node_id in start_nodes {
        let tree = traverse(snapshot, node_id, 0, &config, &mut visited);
        if !tree.is_null() {
            result.push(tree);
        }
    }

    json!(result)
}

/// Print hierarchy in text format.
fn print_hierarchy_text(nodes: &[serde_json::Value], indent: usize) {
    let prefix = "  ".repeat(indent);
    for node in nodes {
        let name = node["qualified_name"].as_str().unwrap_or("?");
        let file = node["file"].as_str().unwrap_or("?");
        let line = node["line"].as_u64().unwrap_or(0);
        println!("{prefix}{name} ({file}:{line})");

        if let Some(children) = node["children"].as_array() {
            print_hierarchy_text(children, indent + 1);
        }
    }
}

// ===== Is In Cycle =====

/// Check if a symbol is in a cycle using the unified graph.
fn run_is_in_cycle_unified(
    graph: &UnifiedCodeGraph,
    symbol: &str,
    cycle_type: &str,
    show_cycle: bool,
    format: &str,
    verbose: bool,
) -> Result<()> {
    use serde_json::json;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();

    // Find the node(s) matching the symbol
    let target_nodes = find_nodes_by_name(&snapshot, symbol);

    if target_nodes.is_empty() {
        bail!("Symbol '{symbol}' not found in the graph");
    }

    if verbose {
        eprintln!(
            "Checking if symbol '{}' is in a {} cycle ({} node(s) found)",
            symbol,
            cycle_type,
            target_nodes.len()
        );
    }

    let imports_only = cycle_type == "imports";
    let calls_only = cycle_type == "calls";

    // Check each matching node for cycles
    let mut found_cycles = Vec::new();

    for &target_id in &target_nodes {
        if let Some(cycle) =
            find_cycle_containing_node(&snapshot, target_id, imports_only, calls_only)
        {
            // Convert cycle to qualified names
            let cycle_names: Vec<String> = cycle
                .iter()
                .filter_map(|&node_id| {
                    snapshot.nodes().get(node_id).and_then(|entry| {
                        entry
                            .qualified_name
                            .and_then(|id| strings.resolve(id))
                            .map(|s| s.to_string())
                            .or_else(|| strings.resolve(entry.name).map(|s| s.to_string()))
                    })
                })
                .collect();

            found_cycles.push(json!({
                "node": format!("{target_id:?}"),
                "cycle": cycle_names
            }));
        }
    }

    let in_cycle = !found_cycles.is_empty();

    if format == "json" {
        let output = if show_cycle {
            json!({
                "symbol": symbol,
                "in_cycle": in_cycle,
                "cycle_type": cycle_type,
                "cycles": found_cycles
            })
        } else {
            json!({
                "symbol": symbol,
                "in_cycle": in_cycle,
                "cycle_type": cycle_type
            })
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if in_cycle {
        println!("Symbol '{symbol}' IS in a {cycle_type} cycle.");
        if show_cycle {
            for (i, cycle) in found_cycles.iter().enumerate() {
                println!();
                println!("Cycle {}:", i + 1);
                if let Some(names) = cycle["cycle"].as_array() {
                    for (j, name) in names.iter().enumerate() {
                        let prefix = if j == 0 { "  " } else { "  → " };
                        println!("{prefix}{name}", name = name.as_str().unwrap_or("?"));
                    }
                    // Show the loop back
                    if let Some(first) = names.first() {
                        println!("  → {} (cycle)", first.as_str().unwrap_or("?"));
                    }
                }
            }
        }
    } else {
        println!("Symbol '{symbol}' is NOT in any {cycle_type} cycle.");
    }

    Ok(())
}

/// Find a cycle containing a specific node.
fn find_cycle_containing_node(
    snapshot: &UnifiedGraphSnapshot,
    target: sqry_core::graph::unified::node::NodeId,
    imports_only: bool,
    calls_only: bool,
) -> Option<Vec<sqry_core::graph::unified::node::NodeId>> {
    // DFS to find a path from target back to itself
    let mut stack = vec![(target, vec![target])];
    let mut visited = HashSet::new();

    while let Some((current, path)) = stack.pop() {
        if visited.contains(&current) && path.len() > 1 {
            continue;
        }
        visited.insert(current);

        for edge_ref in snapshot.edges().edges_from(current) {
            // Filter by edge type
            let is_import = matches!(edge_ref.kind, UnifiedEdgeKind::Imports { .. });
            let is_call = matches!(edge_ref.kind, UnifiedEdgeKind::Calls { .. });

            if imports_only && !is_import {
                continue;
            }
            if calls_only && !is_call {
                continue;
            }
            if !imports_only && !calls_only && !is_import && !is_call {
                continue;
            }

            let next = edge_ref.target;

            // Found a cycle back to the target!
            if next == target && path.len() > 1 {
                return Some(path);
            }

            // Don't revisit in this path
            if !path.contains(&next) {
                let mut new_path = path.clone();
                new_path.push(next);
                stack.push((next, new_path));
            }
        }
    }

    None
}

/// Find nodes by name (simple or qualified).
fn find_nodes_by_name(
    snapshot: &UnifiedGraphSnapshot,
    name: &str,
) -> Vec<sqry_core::graph::unified::node::NodeId> {
    let strings = snapshot.strings();
    let mut matches = Vec::new();

    for (node_id, entry) in snapshot.nodes().iter() {
        let simple_name = strings
            .resolve(entry.name)
            .map_or_else(String::new, |s| s.to_string());
        let qualified_name = entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .map_or_else(String::new, |s| s.to_string());

        if simple_name == name
            || qualified_name == name
            || qualified_name.ends_with(&format!("::{name}"))
            || qualified_name.ends_with(&format!(".{name}"))
        {
            matches.push(node_id);
        }
    }

    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================================
    // parse_language tests
    // ==========================================================================

    #[test]
    fn test_parse_language_javascript_variants() {
        assert_eq!(parse_language("javascript").unwrap(), Language::JavaScript);
        assert_eq!(parse_language("js").unwrap(), Language::JavaScript);
        assert_eq!(parse_language("JavaScript").unwrap(), Language::JavaScript);
        assert_eq!(parse_language("JS").unwrap(), Language::JavaScript);
    }

    #[test]
    fn test_parse_language_typescript_variants() {
        assert_eq!(parse_language("typescript").unwrap(), Language::TypeScript);
        assert_eq!(parse_language("ts").unwrap(), Language::TypeScript);
        assert_eq!(parse_language("TypeScript").unwrap(), Language::TypeScript);
    }

    #[test]
    fn test_parse_language_python_variants() {
        assert_eq!(parse_language("python").unwrap(), Language::Python);
        assert_eq!(parse_language("py").unwrap(), Language::Python);
        assert_eq!(parse_language("PYTHON").unwrap(), Language::Python);
    }

    #[test]
    fn test_parse_language_cpp_variants() {
        assert_eq!(parse_language("cpp").unwrap(), Language::Cpp);
        assert_eq!(parse_language("c++").unwrap(), Language::Cpp);
        assert_eq!(parse_language("cxx").unwrap(), Language::Cpp);
        assert_eq!(parse_language("CPP").unwrap(), Language::Cpp);
    }

    #[test]
    fn test_parse_language_rust_variants() {
        assert_eq!(parse_language("rust").unwrap(), Language::Rust);
        assert_eq!(parse_language("rs").unwrap(), Language::Rust);
    }

    #[test]
    fn test_parse_language_go() {
        assert_eq!(parse_language("go").unwrap(), Language::Go);
        assert_eq!(parse_language("Go").unwrap(), Language::Go);
    }

    #[test]
    fn test_parse_language_java() {
        assert_eq!(parse_language("java").unwrap(), Language::Java);
    }

    #[test]
    fn test_parse_language_c() {
        assert_eq!(parse_language("c").unwrap(), Language::C);
        assert_eq!(parse_language("C").unwrap(), Language::C);
    }

    #[test]
    fn test_parse_language_csharp_variants() {
        assert_eq!(parse_language("csharp").unwrap(), Language::CSharp);
        assert_eq!(parse_language("cs").unwrap(), Language::CSharp);
        assert_eq!(parse_language("CSharp").unwrap(), Language::CSharp);
    }

    #[test]
    fn test_parse_language_ruby() {
        assert_eq!(parse_language("ruby").unwrap(), Language::Ruby);
    }

    #[test]
    fn test_parse_language_php() {
        assert_eq!(parse_language("php").unwrap(), Language::Php);
    }

    #[test]
    fn test_parse_language_swift() {
        assert_eq!(parse_language("swift").unwrap(), Language::Swift);
    }

    #[test]
    fn test_parse_language_kotlin() {
        assert_eq!(parse_language("kotlin").unwrap(), Language::Kotlin);
    }

    #[test]
    fn test_parse_language_scala() {
        assert_eq!(parse_language("scala").unwrap(), Language::Scala);
    }

    #[test]
    fn test_parse_language_sql() {
        assert_eq!(parse_language("sql").unwrap(), Language::Sql);
    }

    #[test]
    fn test_parse_language_dart() {
        assert_eq!(parse_language("dart").unwrap(), Language::Dart);
    }

    #[test]
    fn test_parse_language_lua() {
        assert_eq!(parse_language("lua").unwrap(), Language::Lua);
    }

    #[test]
    fn test_parse_language_perl() {
        assert_eq!(parse_language("perl").unwrap(), Language::Perl);
    }

    #[test]
    fn test_parse_language_shell_variants() {
        assert_eq!(parse_language("shell").unwrap(), Language::Shell);
        assert_eq!(parse_language("bash").unwrap(), Language::Shell);
    }

    #[test]
    fn test_parse_language_groovy() {
        assert_eq!(parse_language("groovy").unwrap(), Language::Groovy);
    }

    #[test]
    fn test_parse_language_elixir_variants() {
        assert_eq!(parse_language("elixir").unwrap(), Language::Elixir);
        assert_eq!(parse_language("ex").unwrap(), Language::Elixir);
    }

    #[test]
    fn test_parse_language_r() {
        assert_eq!(parse_language("r").unwrap(), Language::R);
        assert_eq!(parse_language("R").unwrap(), Language::R);
    }

    #[test]
    fn test_parse_language_haskell_variants() {
        assert_eq!(parse_language("haskell").unwrap(), Language::Haskell);
        assert_eq!(parse_language("hs").unwrap(), Language::Haskell);
    }

    #[test]
    fn test_parse_language_svelte() {
        assert_eq!(parse_language("svelte").unwrap(), Language::Svelte);
    }

    #[test]
    fn test_parse_language_vue() {
        assert_eq!(parse_language("vue").unwrap(), Language::Vue);
    }

    #[test]
    fn test_parse_language_zig() {
        assert_eq!(parse_language("zig").unwrap(), Language::Zig);
    }

    #[test]
    fn test_parse_language_http() {
        assert_eq!(parse_language("http").unwrap(), Language::Http);
    }

    #[test]
    fn test_parse_language_unknown() {
        let result = parse_language("unknown_language");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unknown language"));
    }

    // ==========================================================================
    // parse_language_filter tests
    // ==========================================================================

    #[test]
    fn test_parse_language_filter_none() {
        let result = parse_language_filter(None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_language_filter_single() {
        let result = parse_language_filter(Some("rust")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], Language::Rust);
    }

    #[test]
    fn test_parse_language_filter_multiple() {
        let result = parse_language_filter(Some("rust,python,go")).unwrap();
        assert_eq!(result.len(), 3);
        assert!(result.contains(&Language::Rust));
        assert!(result.contains(&Language::Python));
        assert!(result.contains(&Language::Go));
    }

    #[test]
    fn test_parse_language_filter_with_spaces() {
        let result = parse_language_filter(Some("rust , python , go")).unwrap();
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_parse_language_filter_with_aliases() {
        let result = parse_language_filter(Some("js,ts,py")).unwrap();
        assert_eq!(result.len(), 3);
        assert!(result.contains(&Language::JavaScript));
        assert!(result.contains(&Language::TypeScript));
        assert!(result.contains(&Language::Python));
    }

    #[test]
    fn test_parse_language_filter_invalid() {
        let result = parse_language_filter(Some("rust,invalid,python"));
        assert!(result.is_err());
    }

    // ==========================================================================
    // parse_language_filter_unified tests
    // ==========================================================================

    #[test]
    fn test_parse_language_filter_unified_none() {
        let result = parse_language_filter_unified(None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_language_filter_unified_single() {
        let result = parse_language_filter_unified(Some("rust"));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "rust");
    }

    #[test]
    fn test_parse_language_filter_unified_multiple() {
        let result = parse_language_filter_unified(Some("rust,python,go"));
        assert_eq!(result.len(), 3);
        assert!(result.contains(&"rust".to_string()));
        assert!(result.contains(&"python".to_string()));
        assert!(result.contains(&"go".to_string()));
    }

    #[test]
    fn test_parse_language_filter_unified_with_spaces() {
        let result = parse_language_filter_unified(Some(" rust , python "));
        assert_eq!(result.len(), 2);
        assert!(result.contains(&"rust".to_string()));
        assert!(result.contains(&"python".to_string()));
    }

    // ==========================================================================
    // parse_language_filter_for_complexity tests
    // ==========================================================================

    #[test]
    fn test_parse_language_filter_for_complexity_none() {
        let result = parse_language_filter_for_complexity(None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_language_filter_for_complexity_single() {
        let result = parse_language_filter_for_complexity(Some("rust")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], Language::Rust);
    }

    #[test]
    fn test_parse_language_filter_for_complexity_multiple() {
        let result = parse_language_filter_for_complexity(Some("rust,python")).unwrap();
        assert_eq!(result.len(), 2);
    }

    // ==========================================================================
    // display_languages tests
    // ==========================================================================

    #[test]
    fn test_display_languages_empty() {
        let languages: HashSet<Language> = HashSet::new();
        assert_eq!(display_languages(&languages), "");
    }

    #[test]
    fn test_display_languages_single() {
        let mut languages = HashSet::new();
        languages.insert(Language::Rust);
        let result = display_languages(&languages);
        assert_eq!(result, "rust");
    }

    #[test]
    fn test_display_languages_multiple() {
        let mut languages = HashSet::new();
        languages.insert(Language::Rust);
        languages.insert(Language::Python);
        let result = display_languages(&languages);
        // Result should be sorted (py comes before rust)
        assert!(result.contains("py"));
        assert!(result.contains("rust"));
        assert!(result.contains(", "));
    }

    // ==========================================================================
    // edge_kind_matches_unified tests
    // ==========================================================================

    #[test]
    fn test_edge_kind_matches_unified_calls() {
        let kind = UnifiedEdgeKind::Calls {
            argument_count: 2,
            is_async: false,
        };
        assert!(edge_kind_matches_unified(&kind, "calls"));
        assert!(edge_kind_matches_unified(&kind, "Calls"));
        assert!(edge_kind_matches_unified(&kind, "CALLS"));
    }

    #[test]
    fn test_edge_kind_matches_unified_imports() {
        let kind = UnifiedEdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        };
        assert!(edge_kind_matches_unified(&kind, "imports"));
        assert!(edge_kind_matches_unified(&kind, "import"));
    }

    #[test]
    fn test_edge_kind_matches_unified_no_match() {
        let kind = UnifiedEdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        assert!(!edge_kind_matches_unified(&kind, "imports"));
        assert!(!edge_kind_matches_unified(&kind, "exports"));
    }

    #[test]
    fn test_edge_kind_matches_unified_partial() {
        let kind = UnifiedEdgeKind::Calls {
            argument_count: 1,
            is_async: true,
        };
        // "async" should match since the debug output contains "is_async: true"
        assert!(edge_kind_matches_unified(&kind, "async"));
    }

    // ==========================================================================
    // parse_node_kind_filter tests
    // ==========================================================================

    #[test]
    fn test_parse_node_kind_filter_none() {
        let result = parse_node_kind_filter(None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_node_kind_filter_valid() {
        let result = parse_node_kind_filter(Some("Function,macro,call_site")).unwrap();
        assert_eq!(result.len(), 3);
        assert!(result.contains(&UnifiedNodeKind::Function));
        assert!(result.contains(&UnifiedNodeKind::Macro));
        assert!(result.contains(&UnifiedNodeKind::CallSite));
    }

    #[test]
    fn test_parse_node_kind_filter_invalid() {
        let result = parse_node_kind_filter(Some("function,unknown"));
        assert!(result.is_err());
    }

    // ==========================================================================
    // parse_edge_kind_filter tests
    // ==========================================================================

    #[test]
    fn test_parse_edge_kind_filter_none() {
        let result = parse_edge_kind_filter(None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_edge_kind_filter_valid() {
        let result = parse_edge_kind_filter(Some("calls,table_read,HTTP_REQUEST")).unwrap();
        assert!(result.contains("calls"));
        assert!(result.contains("table_read"));
        assert!(result.contains("http_request"));
    }

    #[test]
    fn test_parse_edge_kind_filter_invalid() {
        let result = parse_edge_kind_filter(Some("calls,unknown_edge"));
        assert!(result.is_err());
    }

    // ==========================================================================
    // normalize_graph_limit tests
    // ==========================================================================

    #[test]
    fn test_normalize_graph_limit_default_on_zero() {
        assert_eq!(normalize_graph_limit(0), DEFAULT_GRAPH_LIST_LIMIT);
    }

    #[test]
    fn test_normalize_graph_limit_clamps_max() {
        assert_eq!(
            normalize_graph_limit(MAX_GRAPH_LIST_LIMIT + 1),
            MAX_GRAPH_LIST_LIMIT
        );
    }

    // ==========================================================================
    // reconstruct_path_unified tests
    // ==========================================================================

    #[test]
    fn test_reconstruct_path_unified_single_node() {
        use sqry_core::graph::unified::node::NodeId;

        let target = NodeId::new(0, 0);
        let mut parents = HashMap::new();
        parents.insert(target, None);

        let path = reconstruct_path_unified(target, &parents);
        assert_eq!(path.len(), 1);
        assert_eq!(path[0], target);
    }

    #[test]
    fn test_reconstruct_path_unified_two_nodes() {
        use sqry_core::graph::unified::node::NodeId;

        let start = NodeId::new(0, 0);
        let end = NodeId::new(1, 0);
        let mut parents = HashMap::new();
        parents.insert(start, None);
        parents.insert(end, Some(start));

        let path = reconstruct_path_unified(end, &parents);
        assert_eq!(path.len(), 2);
        assert_eq!(path[0], start);
        assert_eq!(path[1], end);
    }

    #[test]
    fn test_reconstruct_path_unified_three_nodes() {
        use sqry_core::graph::unified::node::NodeId;

        let a = NodeId::new(0, 0);
        let b = NodeId::new(1, 0);
        let c = NodeId::new(2, 0);
        let mut parents = HashMap::new();
        parents.insert(a, None);
        parents.insert(b, Some(a));
        parents.insert(c, Some(b));

        let path = reconstruct_path_unified(c, &parents);
        assert_eq!(path.len(), 3);
        assert_eq!(path[0], a);
        assert_eq!(path[1], b);
        assert_eq!(path[2], c);
    }
}
