//! Trace path execution for finding call paths between symbols.
//!
//! This module implements the `trace_path` tool which finds K shortest call paths
//! between two symbols using BFS with path enumeration.
//!
//! # Migration Status (FR-2025-007)
//!
//! ✅ Fully migrated to unified graph architecture. Uses `GraphSnapshot` for:
//! - Symbol lookup via `nodes_by_symbol()` and qualified name search
//! - Path traversal via `edges().edges_from()` with full edge metadata
//! - Cross-language detection via `FileRegistry.language_for_file()`

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Result, bail};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeId;

use crate::engine::{canonicalize_in_workspace, engine_for_workspace, get_graph_identity};
use crate::tools::TracePathArgs;

use crate::execution::graph_builders::build_graph_metadata;
use crate::execution::graph_cache::{self, CacheOutcome};
use crate::execution::types::{
    CacheRequestContext, CallPath, NodeRefData, PathStep, PositionData, RangeData, ToolExecution,
    TracePathData,
};
use crate::execution::utils::duration_to_ms;

/// Execute the `trace_path` tool to find call paths between symbols.
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
pub fn execute_trace_path(args: &TracePathArgs) -> Result<ToolExecution<TracePathData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _base = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require unified graph for path tracing
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();

    // Get GraphIdentity for cache isolation
    let identity = get_graph_identity(&workspace_root)?;

    // Create cache key with full GraphIdentity for workspace isolation
    let cache_key = graph_cache::TracePathCacheKey::new(
        args.from_symbol.clone(),
        args.to_symbol.clone(),
        args.max_hops,
        args.max_paths,
        args.cross_language,
        args.min_confidence,
    )
    .with_graph_identity(&identity);

    // Try cache first, fall back to computation
    let CacheOutcome {
        data: trace_data,
        state: cache_state,
        latency_ms: cache_latency_ms,
    } = graph_cache::get_or_compute_trace_path(cache_key, || {
        compute_trace_path_unified(args, &snapshot, &workspace_root).unwrap_or_else(|e| {
            tracing::error!(error = %e, "trace_path computation failed, returning empty paths");
            TracePathData {
                paths: vec![],
                from_symbol: args.from_symbol.clone(),
                to_symbol: args.to_symbol.clone(),
            }
        })
    });

    tracing::debug!(
        cache_state = ?cache_state,
        cache_latency_ms,
        "trace_path cache outcome"
    );

    let graph_metadata = build_graph_metadata(
        Some(&workspace_root),
        Some(&snapshot),
        Some(CacheRequestContext {
            tool: "trace_path",
            state: cache_state,
            latency_ms: cache_latency_ms,
        }),
    );

    let execution_ms = duration_to_ms(start.elapsed());
    let total_paths = trace_data.paths.len() as u64;

    Ok(ToolExecution {
        data: trace_data,
        used_index: false,
        used_graph: true,
        graph_metadata: Some(graph_metadata),
        execution_ms,
        next_page_token: None,
        total: Some(total_paths),
        truncated: None,
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Inner computation logic for `trace_path` using unified graph.
fn compute_trace_path_unified(
    args: &TracePathArgs,
    snapshot: &GraphSnapshot,
    workspace_root: &std::path::Path,
) -> Result<TracePathData> {
    let (from_nodes, to_nodes) = resolve_trace_nodes(snapshot, args)?;
    let all_paths = collect_paths(snapshot, &from_nodes, &to_nodes, args, workspace_root);
    let result_paths = build_call_paths(snapshot, &all_paths, workspace_root);

    Ok(TracePathData {
        paths: result_paths,
        from_symbol: args.from_symbol.clone(),
        to_symbol: args.to_symbol.clone(),
    })
}

fn resolve_trace_nodes(
    snapshot: &GraphSnapshot,
    args: &TracePathArgs,
) -> Result<(Vec<NodeId>, Vec<NodeId>)> {
    let from_nodes = find_nodes_by_name(snapshot, &args.from_symbol);
    let to_nodes = find_nodes_by_name(snapshot, &args.to_symbol);

    if from_nodes.is_empty() {
        bail!("Start symbol '{}' not found in graph", args.from_symbol);
    }
    if to_nodes.is_empty() {
        bail!("Target symbol '{}' not found in graph", args.to_symbol);
    }

    Ok((from_nodes, to_nodes))
}

fn collect_paths(
    snapshot: &GraphSnapshot,
    from_nodes: &[NodeId],
    to_nodes: &[NodeId],
    args: &TracePathArgs,
    workspace_root: &std::path::Path,
) -> Vec<Vec<NodeId>> {
    // Try Pass 5 optimization: use precomputed condensation DAG if available
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(workspace_root);
    let analysis_data = if args.cross_language {
        // For cross-language paths, use calls edge kind
        sqry_core::graph::unified::analysis::try_load_scc_and_condensation(
            &storage, snapshot, "calls",
        )
    } else {
        None
    };

    let mut all_paths: Vec<Vec<NodeId>> = Vec::new();

    for &from_node in from_nodes {
        for &to_node in to_nodes {
            let paths = if let Some((ref scc_data, ref cond_dag)) = analysis_data {
                // Fast path: use condensation DAG
                find_paths_with_condensation(
                    snapshot,
                    from_node,
                    to_node,
                    scc_data,
                    cond_dag,
                    args.max_hops,
                    args.max_paths,
                    args.min_confidence,
                )
                .or_else(|| {
                    // Fall back to BFS if condensation path fails
                    find_k_shortest_paths_unified(
                        snapshot,
                        from_node,
                        to_node,
                        args.max_hops,
                        args.max_paths,
                        args.min_confidence,
                        args.cross_language,
                    )
                })
            } else {
                // Slow path: BFS traversal
                find_k_shortest_paths_unified(
                    snapshot,
                    from_node,
                    to_node,
                    args.max_hops,
                    args.max_paths,
                    args.min_confidence,
                    args.cross_language,
                )
            };

            if let Some(paths) = paths {
                all_paths.extend(paths);
            }
        }
    }

    all_paths.sort_by_key(Vec::len);
    all_paths.truncate(args.max_paths);
    all_paths
}

fn build_call_paths(
    snapshot: &GraphSnapshot,
    paths: &[Vec<NodeId>],
    workspace_root: &std::path::Path,
) -> Vec<CallPath> {
    paths
        .iter()
        .map(|nodes| build_call_path(snapshot, nodes, workspace_root))
        .collect()
}

fn build_call_path(
    snapshot: &GraphSnapshot,
    path_nodes: &[NodeId],
    workspace_root: &std::path::Path,
) -> CallPath {
    let (steps, cross_language) = build_path_steps(snapshot, path_nodes, workspace_root);
    let length = steps.len().saturating_sub(1).try_into().unwrap_or(u32::MAX);
    let score = calculate_path_score(&steps, cross_language);

    CallPath {
        steps,
        length,
        score,
        cross_language,
    }
}

fn build_path_steps(
    snapshot: &GraphSnapshot,
    path_nodes: &[NodeId],
    workspace_root: &std::path::Path,
) -> (Vec<PathStep>, bool) {
    let files = snapshot.files();
    let mut steps = Vec::new();
    let mut cross_language = false;
    let mut prev_lang: Option<sqry_core::graph::Language> = None;

    for (idx, &node_id) in path_nodes.iter().enumerate() {
        let Some(entry) = snapshot.get_node(node_id) else {
            continue;
        };

        let lang = files.language_for_file(entry.file);
        let sym_ref = build_node_ref_from_node(entry, lang, snapshot, workspace_root);

        if let (Some(prev), Some(current)) = (prev_lang, lang)
            && prev != current
        {
            cross_language = true;
        }
        prev_lang = lang;

        let (edge_type, confidence) = edge_info_for_step(snapshot, path_nodes, idx, node_id);

        steps.push(PathStep {
            symbol: sym_ref,
            edge_type,
            confidence,
        });
    }

    (steps, cross_language)
}

fn edge_info_for_step(
    snapshot: &GraphSnapshot,
    path_nodes: &[NodeId],
    idx: usize,
    node_id: NodeId,
) -> (String, Option<f64>) {
    if idx == 0 {
        return ("start".to_string(), None);
    }

    if let Some(&prev_node) = path_nodes.get(idx - 1) {
        return get_edge_info_between(snapshot, prev_node, node_id);
    }

    ("call".to_string(), Some(1.0))
}

/// Find nodes by name (simple or qualified) in the unified graph.
fn find_nodes_by_name(snapshot: &GraphSnapshot, name: &str) -> Vec<NodeId> {
    let strings = snapshot.strings();

    // First try exact match by simple name
    let mut matches = snapshot.nodes_by_symbol(name);

    // Also search by qualified name if no simple name matches
    if matches.is_empty() {
        for (node_id, entry) in snapshot.iter_nodes() {
            if let Some(qname_id) = entry.qualified_name
                && let Some(qname) = strings.resolve(qname_id)
                && (qname.as_ref() == name || qname.ends_with(&format!("::{name}")))
            {
                matches.push(node_id);
            }
        }
    }

    matches
}

/// Compute confidence score for an edge based on `EdgeKind` metadata.
fn edge_confidence_from_kind(kind: &EdgeKind) -> f64 {
    match kind {
        EdgeKind::Calls { is_async, .. } => {
            // Async calls slightly lower confidence (may be indirect)
            if *is_async { 0.9 } else { 1.0 }
        }
        EdgeKind::Imports { is_wildcard, .. } => {
            // Wildcard imports are less specific
            if *is_wildcard { 0.7 } else { 0.95 }
        }
        EdgeKind::References => 0.8,
        EdgeKind::Inherits | EdgeKind::Implements => 0.95,
        _ => 1.0,
    }
}

/// Configuration for path finding.
struct PathFindConfig {
    max_hops: usize,
    max_paths: usize,
    min_confidence: f64,
    allow_cross_language: bool,
}

/// Find K shortest paths using BFS with path enumeration on unified graph.
fn find_k_shortest_paths_unified(
    snapshot: &GraphSnapshot,
    from: NodeId,
    to: NodeId,
    max_hops: usize,
    max_paths: usize,
    min_confidence: f64,
    allow_cross_language: bool,
) -> Option<Vec<Vec<NodeId>>> {
    let config = PathFindConfig {
        max_hops,
        max_paths,
        min_confidence,
        allow_cross_language,
    };

    let mut paths = Vec::new();
    let mut queue: VecDeque<(NodeId, Vec<NodeId>, usize)> = VecDeque::new();
    queue.push_back((from, vec![from], 0));

    while let Some((current, path, depth)) = queue.pop_front() {
        if depth > config.max_hops {
            continue;
        }

        if current == to {
            paths.push(path.clone());
            if paths.len() >= config.max_paths {
                break;
            }
            continue;
        }

        explore_edges(snapshot, current, &path, depth, &config, &mut queue);
    }

    if paths.is_empty() { None } else { Some(paths) }
}

/// Explore outgoing edges from a node and enqueue valid next steps.
fn explore_edges(
    snapshot: &GraphSnapshot,
    current: NodeId,
    path: &[NodeId],
    depth: usize,
    config: &PathFindConfig,
    queue: &mut VecDeque<(NodeId, Vec<NodeId>, usize)>,
) {
    for edge in snapshot.edges().edges_from(current) {
        if !is_followable_edge(&edge.kind, config.min_confidence) {
            continue;
        }

        let next = edge.target;

        if !is_valid_transition(snapshot, current, next, config.allow_cross_language) {
            continue;
        }

        if path.contains(&next) {
            continue;
        }

        let mut new_path = path.to_vec();
        new_path.push(next);
        queue.push_back((next, new_path, depth + 1));
    }
}

/// Check if an edge is followable (is a call edge with sufficient confidence).
fn is_followable_edge(kind: &EdgeKind, min_confidence: f64) -> bool {
    if !matches!(kind, EdgeKind::Calls { .. }) {
        return false;
    }
    edge_confidence_from_kind(kind) >= min_confidence
}

/// Check if a transition between nodes is valid (respects cross-language filter).
fn is_valid_transition(
    snapshot: &GraphSnapshot,
    current: NodeId,
    next: NodeId,
    allow_cross_language: bool,
) -> bool {
    if allow_cross_language {
        return true;
    }

    let files = snapshot.files();
    let current_entry = snapshot.get_node(current);
    let next_entry = snapshot.get_node(next);

    match (current_entry, next_entry) {
        (Some(ce), Some(ne)) => {
            let current_lang = files.language_for_file(ce.file);
            let next_lang = files.language_for_file(ne.file);
            match (current_lang, next_lang) {
                (Some(cl), Some(nl)) => cl == nl,
                _ => true, // Allow if language unknown
            }
        }
        _ => true, // Allow if nodes not found
    }
}

/// Get edge type and confidence between two nodes.
fn get_edge_info_between(
    snapshot: &GraphSnapshot,
    from: NodeId,
    to: NodeId,
) -> (String, Option<f64>) {
    for edge in snapshot.edges().edges_from(from) {
        if edge.target == to {
            let edge_type = match &edge.kind {
                EdgeKind::Calls { is_async, .. } => {
                    if *is_async {
                        "async_call"
                    } else {
                        "call"
                    }
                }
                EdgeKind::Imports { .. } => "import",
                EdgeKind::Exports { .. } => "export",
                EdgeKind::References => "reference",
                EdgeKind::Inherits => "inherits",
                EdgeKind::Implements => "implements",
                _ => "edge",
            };
            let confidence = edge_confidence_from_kind(&edge.kind);
            return (edge_type.to_string(), Some(confidence));
        }
    }
    ("unknown".to_string(), None)
}

/// Build `NodeRefData` from a unified graph `NodeEntry`.
fn build_node_ref_from_node(
    node: &sqry_core::graph::unified::storage::arena::NodeEntry,
    language: Option<sqry_core::graph::Language>,
    snapshot: &GraphSnapshot,
    workspace_root: &std::path::Path,
) -> NodeRefData {
    use sqry_core::graph::unified::node::NodeKind;

    let strings = snapshot.strings();
    let files = snapshot.files();

    // Get symbol name from string interner
    let name = strings
        .resolve(node.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    // Get qualified name if present
    let qualified_name = node
        .qualified_name
        .and_then(|sid| strings.resolve(sid))
        .map_or_else(|| name.clone(), |s| s.to_string());

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

    // Get file path
    let file_path = files
        .resolve(node.file)
        .map(|arc_path| workspace_root.join(arc_path.as_ref()))
        .unwrap_or_default();

    let file_uri = url::Url::from_file_path(&file_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&file_path),
        Into::into,
    );

    NodeRefData {
        name,
        qualified_name,
        kind: kind.to_string(),
        language: language.map_or_else(|| "unknown".to_string(), |l| l.to_string()),
        file_uri,
        range: RangeData {
            start: PositionData {
                line: node.start_line,
                character: node.start_column,
            },
            end: PositionData {
                line: node.end_line,
                character: node.end_column,
            },
        },
        metadata: None,
    }
}

/// Calculate path score (higher is better).
fn calculate_path_score(steps: &[PathStep], cross_language: bool) -> f64 {
    let step_count = u32::try_from(steps.len()).unwrap_or(u32::MAX).max(1);
    let length_penalty = 1.0 / f64::from(step_count);
    let cross_lang_bonus = if cross_language { 0.1 } else { 0.0 };
    length_penalty + cross_lang_bonus
}

/// Find an edge connecting nodes in current_scc to nodes in next_scc.
///
/// Returns (source, target) node IDs if found, None otherwise.
fn find_connecting_edge(
    snapshot: &GraphSnapshot,
    current_members: &[u32],
    next_scc: u32,
    scc_data: &sqry_core::graph::unified::analysis::SccData,
    min_confidence: f64,
) -> Option<(NodeId, NodeId)> {
    for &current_node_idx in current_members {
        let current_node = NodeId::new(current_node_idx, 0);

        for edge in snapshot.edges().edges_from(current_node) {
            if !matches!(edge.kind, EdgeKind::Calls { .. }) {
                continue;
            }

            if edge_confidence_from_kind(&edge.kind) < min_confidence {
                continue;
            }

            let target_scc = scc_data.scc_of(edge.target)?;
            if target_scc == next_scc {
                return Some((current_node, edge.target));
            }
        }
    }
    None
}

/// Find paths using Pass 5 condensation DAG (fast path)
fn find_paths_with_condensation(
    snapshot: &GraphSnapshot,
    from: NodeId,
    to: NodeId,
    scc_data: &sqry_core::graph::unified::analysis::SccData,
    cond_dag: &sqry_core::graph::unified::analysis::CondensationDag,
    max_hops: usize,
    max_paths: usize,
    min_confidence: f64,
) -> Option<Vec<Vec<NodeId>>> {
    // Map nodes to their SCCs
    let from_scc = scc_data.scc_of(from)?;
    let to_scc = scc_data.scc_of(to)?;

    // If same SCC, use scoped BFS within the SCC (avoids full-graph BFS fallback)
    if from_scc == to_scc {
        return find_intra_scc_paths(
            snapshot,
            from,
            to,
            scc_data,
            from_scc,
            max_hops,
            max_paths,
            min_confidence,
        );
    }

    // Use condensation DAG to find SCC-level path
    let scc_path = cond_dag.find_scc_path(from_scc, to_scc)?;

    // Reconstruct node-level path from SCC path
    // For each pair of adjacent SCCs, find an edge between them
    let mut node_path = vec![from];

    for window in scc_path.windows(2) {
        let current_scc = window[0];
        let next_scc = window[1];

        // Get all nodes in the current SCC
        let current_members = scc_data.scc_members(current_scc);

        // Find an edge from current SCC to next SCC
        let (source, target) = find_connecting_edge(
            snapshot,
            current_members,
            next_scc,
            scc_data,
            min_confidence,
        )?;

        // Add source to path if not already last
        if let Some(&last) = node_path.last()
            && last != source
        {
            node_path.push(source);
        }
        node_path.push(target);
    }

    // Ensure path ends at target
    if let Some(&last) = node_path.last()
        && last != to
    {
        // Try to find a path from last node to target within same SCC
        if scc_data.scc_of(last)? == to_scc {
            node_path.push(to);
        }
    }

    Some(vec![node_path])
}

/// Find paths between two nodes within the same SCC using scoped BFS.
///
/// Only explores edges to nodes that are members of the given SCC, avoiding
/// full-graph traversal. The effective hop limit is `min(max_hops, scc_size)`
/// to respect both the user's configured limit and the SCC's natural bound.
fn find_intra_scc_paths(
    snapshot: &GraphSnapshot,
    from: NodeId,
    to: NodeId,
    scc_data: &sqry_core::graph::unified::analysis::SccData,
    scc_id: u32,
    max_hops: usize,
    max_paths: usize,
    min_confidence: f64,
) -> Option<Vec<Vec<NodeId>>> {
    let members: HashSet<u32> = scc_data.scc_members(scc_id).iter().copied().collect();
    let effective_max_hops = max_hops.min(members.len());

    let mut paths = Vec::new();
    let mut queue: VecDeque<(NodeId, Vec<NodeId>, usize)> = VecDeque::new();
    queue.push_back((from, vec![from], 0));

    while let Some((current, path, depth)) = queue.pop_front() {
        if depth > effective_max_hops {
            continue;
        }

        if current == to {
            paths.push(path);
            if paths.len() >= max_paths {
                break;
            }
            continue;
        }

        for edge in snapshot.edges().edges_from(current) {
            if !is_followable_edge(&edge.kind, min_confidence) {
                continue;
            }

            let next = edge.target;

            // Only follow edges to nodes within the same SCC
            if !members.contains(&next.index()) {
                continue;
            }

            if path.contains(&next) {
                continue;
            }

            let mut new_path = path.clone();
            new_path.push(next);
            queue.push_back((next, new_path, depth + 1));
        }
    }

    if paths.is_empty() { None } else { Some(paths) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::analysis::CondensationDag;
    use sqry_core::graph::unified::analysis::csr::CsrAdjacency;
    use sqry_core::graph::unified::analysis::scc::SccData;
    use sqry_core::graph::unified::compaction::snapshot_edges;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::NodeEntry;
    use std::path::Path;

    /// Build a graph with a 3-node cycle: A→B→C→A, plus a disconnected node D.
    fn build_cycle_graph() -> (CodeGraph, NodeId, NodeId, NodeId, NodeId) {
        let mut graph = CodeGraph::new();
        let file = graph.files_mut().register(Path::new("test.rs")).unwrap();

        let name_a = graph.strings_mut().intern("A").unwrap();
        let name_b = graph.strings_mut().intern("B").unwrap();
        let name_c = graph.strings_mut().intern("C").unwrap();
        let name_d = graph.strings_mut().intern("D").unwrap();

        let node_a = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name_a, file))
            .unwrap();
        let node_b = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name_b, file))
            .unwrap();
        let node_c = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name_c, file))
            .unwrap();
        let node_d = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name_d, file))
            .unwrap();

        let call = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };

        // A→B→C→A cycle
        graph
            .edges_mut()
            .add_edge(node_a, node_b, call.clone(), file);
        graph
            .edges_mut()
            .add_edge(node_b, node_c, call.clone(), file);
        graph.edges_mut().add_edge(node_c, node_a, call, file);

        (graph, node_a, node_b, node_c, node_d)
    }

    /// Build SCC + condensation data from a graph.
    fn build_scc_and_condensation(graph: &CodeGraph) -> (SccData, CondensationDag) {
        let snapshot = graph.snapshot();
        let edges = snapshot.edges();
        let forward = edges.forward();
        let compaction = snapshot_edges(&forward, snapshot.nodes().len());
        let adjacency = CsrAdjacency::build_from_snapshot(&compaction).unwrap();
        let call_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let scc = SccData::compute_tarjan(&adjacency, &call_kind).unwrap();
        let dag = CondensationDag::build(&scc, &adjacency).unwrap();
        (scc, dag)
    }

    #[test]
    fn test_find_intra_scc_paths_finds_path() {
        let (graph, node_a, _node_b, node_c, _node_d) = build_cycle_graph();
        let snapshot = graph.snapshot();
        let (scc, _dag) = build_scc_and_condensation(&graph);

        let scc_id = scc.scc_of(node_a).unwrap();
        // A, B, C should all be in the same SCC
        assert_eq!(scc.scc_of(node_a), scc.scc_of(node_c));

        let result = find_intra_scc_paths(&snapshot, node_a, node_c, &scc, scc_id, 10, 5, 0.0);

        assert!(
            result.is_some(),
            "Should find a path from A to C within SCC"
        );
        let paths = result.unwrap();
        assert!(!paths.is_empty());

        // The shortest path A→B→C should be found
        let shortest = &paths[0];
        assert_eq!(*shortest.first().unwrap(), node_a);
        assert_eq!(*shortest.last().unwrap(), node_c);
    }

    #[test]
    fn test_find_intra_scc_paths_respects_max_paths() {
        let (graph, node_a, _node_b, node_c, _node_d) = build_cycle_graph();
        let snapshot = graph.snapshot();
        let (scc, _dag) = build_scc_and_condensation(&graph);

        let scc_id = scc.scc_of(node_a).unwrap();

        let result = find_intra_scc_paths(&snapshot, node_a, node_c, &scc, scc_id, 10, 1, 0.0);

        assert!(result.is_some());
        let paths = result.unwrap();
        assert_eq!(paths.len(), 1, "Should return at most 1 path");
    }

    #[test]
    fn test_find_intra_scc_paths_no_path_when_not_in_scc() {
        let (graph, node_a, _node_b, _node_c, node_d) = build_cycle_graph();
        let snapshot = graph.snapshot();
        let (scc, _dag) = build_scc_and_condensation(&graph);

        // D is not in the same SCC as A (D is disconnected)
        let scc_id_a = scc.scc_of(node_a).unwrap();
        let scc_id_d = scc.scc_of(node_d).unwrap();
        assert_ne!(scc_id_a, scc_id_d, "D should be in a different SCC");

        // Searching for D within A's SCC should yield no path
        let result = find_intra_scc_paths(&snapshot, node_a, node_d, &scc, scc_id_a, 10, 5, 0.0);

        assert!(result.is_none(), "No path from A to D within A's SCC");
    }

    #[test]
    fn test_find_paths_with_condensation_same_scc() {
        let (graph, node_a, _node_b, node_c, _node_d) = build_cycle_graph();
        let snapshot = graph.snapshot();
        let (scc, dag) = build_scc_and_condensation(&graph);

        // Same-SCC case should no longer return None
        let result =
            find_paths_with_condensation(&snapshot, node_a, node_c, &scc, &dag, 10, 5, 0.0);

        assert!(
            result.is_some(),
            "Same-SCC case should return paths, not None"
        );
        let paths = result.unwrap();
        assert!(!paths.is_empty());

        // Verify the path starts at A and ends at C
        let path = &paths[0];
        assert_eq!(*path.first().unwrap(), node_a);
        assert_eq!(*path.last().unwrap(), node_c);
    }

    #[test]
    fn test_find_intra_scc_paths_respects_max_hops() {
        // In a 3-node cycle A→B→C→A, path from A to C is A→B→C (2 hops).
        // With max_hops=1, the path should not be found.
        let (graph, node_a, _node_b, node_c, _node_d) = build_cycle_graph();
        let snapshot = graph.snapshot();
        let (scc, _dag) = build_scc_and_condensation(&graph);

        let scc_id = scc.scc_of(node_a).unwrap();

        // max_hops=1: A→B→C requires 2 hops, should not be reachable
        let result = find_intra_scc_paths(&snapshot, node_a, node_c, &scc, scc_id, 1, 5, 0.0);
        assert!(
            result.is_none(),
            "max_hops=1 should not reach C from A (needs 2 hops)"
        );

        // max_hops=2: A→B→C requires 2 hops, should be found
        let result = find_intra_scc_paths(&snapshot, node_a, node_c, &scc, scc_id, 2, 5, 0.0);
        assert!(result.is_some(), "max_hops=2 should find path A→B→C");
    }

    #[test]
    fn test_find_intra_scc_paths_from_equals_to() {
        // When from == to, depth 0 matches immediately (zero-hop path)
        let (graph, node_a, _node_b, _node_c, _node_d) = build_cycle_graph();
        let snapshot = graph.snapshot();
        let (scc, _dag) = build_scc_and_condensation(&graph);

        let scc_id = scc.scc_of(node_a).unwrap();

        let result = find_intra_scc_paths(&snapshot, node_a, node_a, &scc, scc_id, 10, 5, 0.0);

        assert!(result.is_some(), "from==to should return a zero-hop path");
        let paths = result.unwrap();
        assert_eq!(
            paths[0].len(),
            1,
            "Zero-hop path should contain only one node"
        );
        assert_eq!(paths[0][0], node_a);
    }

    #[test]
    fn test_find_intra_scc_paths_single_node_scc_with_self_loop() {
        // Build a graph with a single self-looping node
        let mut graph = CodeGraph::new();
        let file = graph.files_mut().register(Path::new("test.rs")).unwrap();
        let name = graph.strings_mut().intern("self_loop").unwrap();
        let node = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name, file))
            .unwrap();

        let call = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        graph.edges_mut().add_edge(node, node, call, file);

        let (scc, _dag) = build_scc_and_condensation(&graph);
        let snapshot = graph.snapshot();
        let scc_id = scc.scc_of(node).unwrap();

        // from == to with self-loop: should still find zero-hop path
        let result = find_intra_scc_paths(&snapshot, node, node, &scc, scc_id, 10, 5, 0.0);
        assert!(result.is_some());
        assert_eq!(result.unwrap()[0], vec![node]);
    }
}
