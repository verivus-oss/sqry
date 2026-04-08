//! Graph-based tool execution.
//!
//! This module implements graph-related tools: `get_dependencies`, `export_graph`, and `subgraph`.
//!
//! #
//!
//! ✅ **Fully migrated** - All tools use unified `GraphSnapshot` architecture:
//! - `export_graph`: Uses `GraphSnapshot` for visualization rendering (DOT, D2, Mermaid)
//! - `subgraph`: Uses `GraphSnapshot` for subgraph extraction with BFS traversal
//! - `show_dependencies`: Uses `GraphSnapshot` for dependency tree traversal

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::{EdgeKind, StoreEdgeRef};
use sqry_core::graph::unified::materialize::find_nodes_by_name;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use url::Url;

use crate::engine::{canonicalize_in_workspace, engine_for_workspace, get_graph_identity};
use crate::tools::{ExportGraphArgs, ShowDependenciesArgs, SubgraphArgs};
use sqry_core::visualization::unified::{
    D2Config, DotConfig, MermaidConfig, UnifiedD2Exporter, UnifiedDotExporter,
    UnifiedMermaidExporter,
};

use crate::execution::graph_builders::build_graph_metadata;
use crate::execution::graph_cache::{self, CacheOutcome};
use crate::execution::types::{
    CacheRequestContext, DependencyGraphData, NodeRefData, PositionData, RangeData,
    RelationEdgeData, ToolExecution,
};
use crate::execution::utils::{duration_to_ms, paginate};

/// Execute the `get_dependencies` tool to find symbol dependencies.
///
/// Uses `CodeGraph` exclusively for dependency analysis.
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
pub fn execute_get_dependencies(
    args: &ShowDependenciesArgs,
) -> Result<ToolExecution<DependencyGraphData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _base = canonicalize_in_workspace(&args.path, &workspace_root)?;

    tracing::debug!(
        file_path = args.file_path.as_deref(),
        symbol = args.symbol_name.as_deref(),
        max_depth = args.max_depth,
        max_results = args.max_results,
        "Executing get_dependencies tool"
    );

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();
    let (nodes, edges) = compute_dependencies_unified(args, &snapshot, &workspace_root)?;

    let total = edges.len();
    let truncated = total > args.max_results;
    let mut edges = edges;
    edges.truncate(args.max_results);

    let (edge_slice, next_page_token) = paginate(&edges, &args.pagination);
    let page_edges: Vec<RelationEdgeData> = edge_slice.to_vec();

    let unique_nodes: Vec<NodeRefData> = nodes.into_values().collect();
    let truncated_flag = truncated || next_page_token.is_some();

    let graph_metadata = build_graph_metadata(Some(&workspace_root), Some(&snapshot), None);

    Ok(ToolExecution {
        data: DependencyGraphData {
            nodes: unique_nodes,
            edges: page_edges,
            rendered: None,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: Some(graph_metadata),
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(truncated_flag),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Compute dependencies using the unified graph.
fn compute_dependencies_unified(
    args: &ShowDependenciesArgs,
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
) -> Result<(HashMap<String, NodeRefData>, Vec<RelationEdgeData>)> {
    let seeds = collect_unified_seeds(args, snapshot, workspace_root)?;
    if seeds.is_empty() {
        bail!("No target symbols found for dependency analysis");
    }

    let max_depth = args.max_depth;
    let mut nodes: HashMap<String, NodeRefData> = HashMap::new();
    let mut edges: Vec<RelationEdgeData> = Vec::new();
    let mut visited = HashSet::new();
    let mut queue: VecDeque<(NodeId, usize)> = seeds.iter().map(|&id| (id, 0usize)).collect();

    while let Some((current, depth)) = queue.pop_front() {
        if should_skip_dependency_visit(&mut visited, current, depth, max_depth) {
            continue;
        }

        let mut context = UnifiedDependencyContext {
            snapshot,
            workspace_root,
            max_depth,
            nodes: &mut nodes,
            edges: &mut edges,
            queue: &mut queue,
        };

        process_unified_call_edges(&mut context, current, depth, Direction::Outgoing);
        process_unified_call_edges(&mut context, current, depth, Direction::Incoming);
    }

    Ok((nodes, edges))
}

#[derive(Copy, Clone, Debug)]
enum Direction {
    Outgoing,
    Incoming,
}

fn collect_unified_seeds(
    args: &ShowDependenciesArgs,
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
) -> Result<Vec<NodeId>> {
    let mut seeds = Vec::new();

    if let Some(ref file_path) = args.file_path {
        let canon = canonicalize_in_workspace(file_path, workspace_root)?;
        let rel_path = canon.strip_prefix(workspace_root).unwrap_or(&canon);

        for (node_id, entry) in snapshot.iter_nodes() {
            if let Some(path) = snapshot.files().resolve(entry.file)
                && path.as_ref() == rel_path
            {
                seeds.push(node_id);
            }
        }
    }

    if let Some(ref symbol_name) = args.symbol_name {
        seeds.extend(find_nodes_by_name(snapshot, symbol_name));
    }

    Ok(seeds)
}

fn should_skip_dependency_visit(
    visited: &mut HashSet<(NodeId, usize)>,
    current: NodeId,
    depth: usize,
    max_depth: usize,
) -> bool {
    depth >= max_depth || !visited.insert((current, depth))
}

struct UnifiedDependencyContext<'a> {
    snapshot: &'a GraphSnapshot,
    workspace_root: &'a Path,
    max_depth: usize,
    nodes: &'a mut HashMap<String, NodeRefData>,
    edges: &'a mut Vec<RelationEdgeData>,
    queue: &'a mut VecDeque<(NodeId, usize)>,
}

fn process_unified_call_edges(
    context: &mut UnifiedDependencyContext<'_>,
    current: NodeId,
    depth: usize,
    direction: Direction,
) {
    let (edge_iter, relation_type) = match direction {
        Direction::Outgoing => (context.snapshot.edges().edges_from(current), "callees"),
        Direction::Incoming => (context.snapshot.edges().edges_to(current), "callers"),
    };

    for edge in edge_iter {
        if !matches!(edge.kind, EdgeKind::Calls { .. }) {
            continue;
        }

        let (from_id, to_id, next_id) = match direction {
            Direction::Outgoing => (current, edge.target, edge.target),
            Direction::Incoming => (edge.source, current, edge.source),
        };

        let from_ref = build_node_ref_unified(context.snapshot, from_id, context.workspace_root);
        let to_ref = build_node_ref_unified(context.snapshot, to_id, context.workspace_root);

        insert_relation_edge(
            context.nodes,
            context.edges,
            from_ref,
            to_ref,
            relation_type,
            depth,
            call_edge_metadata(&edge.kind),
        );

        if depth + 1 < context.max_depth {
            context.queue.push_back((next_id, depth + 1));
        }
    }
}

fn insert_relation_edge(
    nodes: &mut HashMap<String, NodeRefData>,
    edges: &mut Vec<RelationEdgeData>,
    from_ref: NodeRefData,
    to_ref: NodeRefData,
    relation_type: &str,
    depth: usize,
    metadata: Option<Value>,
) {
    nodes
        .entry(from_ref.qualified_name.clone())
        .or_insert(from_ref.clone());
    nodes
        .entry(to_ref.qualified_name.clone())
        .or_insert(to_ref.clone());

    edges.push(RelationEdgeData {
        from: Some(from_ref),
        to: Some(to_ref),
        relation_type: relation_type.to_string(),
        depth: depth.try_into().unwrap_or(u32::MAX).saturating_add(1),
        metadata,
    });
}

fn call_edge_metadata(edge_kind: &EdgeKind) -> Option<Value> {
    match edge_kind {
        EdgeKind::Calls {
            argument_count,
            is_async,
        } => Some(json!({
            "argument_count": argument_count,
            "is_async": is_async,
        })),
        _ => None,
    }
}

/// Execute the `export_graph` tool to export a dependency graph.
///
/// Uses `CodeGraph` exclusively for graph export.
pub fn execute_export_graph(args: &ExportGraphArgs) -> Result<ToolExecution<DependencyGraphData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _base = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();
    let seeds = collect_export_graph_seeds_unified(args, &snapshot, &workspace_root)?;
    let (nodes, edges, was_truncated) =
        collect_export_graph_data_unified(args, &snapshot, &workspace_root, &seeds);

    let total = edges.len();
    let (page_edges, nodes_vec, next_page_token) =
        paginate_export_graph(&edges, nodes, &args.pagination);

    // Render from the EXACT post-pagination node set so mermaid/dot/d2 output
    // matches the filtered JSON data rather than the entire snapshot.
    let page_node_ids = resolve_page_node_ids(&snapshot, &nodes_vec);
    let rendered = render_export_graph(args, Some(graph.clone()), &page_node_ids);

    let graph_metadata = build_graph_metadata(Some(&workspace_root), Some(&snapshot), None);

    // Check truncation before moving next_page_token
    let is_truncated = was_truncated || next_page_token.is_some();

    Ok(ToolExecution {
        data: DependencyGraphData {
            nodes: nodes_vec,
            edges: page_edges,
            rendered,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: Some(graph_metadata),
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(is_truncated),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Collect seed nodes from a specific file.
fn collect_seeds_by_file(
    snapshot: &GraphSnapshot,
    file_path: &str,
    workspace_root: &Path,
) -> Result<Vec<NodeId>> {
    let canon = canonicalize_in_workspace(file_path, workspace_root)?;
    let relative_path = canon.strip_prefix(workspace_root).unwrap_or(&canon);
    let files = snapshot.files();

    let mut seeds = Vec::new();
    for (node_id, entry) in snapshot.iter_nodes() {
        if let Some(node_file) = files.resolve(entry.file)
            && node_file.as_ref() == relative_path
        {
            seeds.push(node_id);
        }
    }

    Ok(seeds)
}

/// Collect seed node IDs from unified graph for export.
fn collect_export_graph_seeds_unified(
    args: &ExportGraphArgs,
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
) -> Result<Vec<NodeId>> {
    let mut seeds = Vec::new();

    if let Some(ref file_path) = args.file_path {
        seeds.extend(collect_seeds_by_file(snapshot, file_path, workspace_root)?);
    }

    if let Some(ref symbol_name) = args.symbol_name {
        seeds.extend(find_nodes_by_name(snapshot, symbol_name));
    }

    for symbol in &args.symbols {
        seeds.extend(find_nodes_by_name(snapshot, symbol));
    }

    // Deduplicate seeds while preserving relative order
    seeds.sort_by_key(|id| id.index());
    seeds.dedup();

    if seeds.is_empty() {
        bail!("No seed symbols found for export_graph");
    }

    Ok(seeds)
}

/// Classify an edge for export based on its kind and the include flags in args.
///
/// Returns `Some((relation_type, metadata, should_traverse))` if the edge should be included,
/// or `None` if it should be skipped.
fn classify_edge_for_export(
    edge_kind: &EdgeKind,
    args: &ExportGraphArgs,
) -> Option<(&'static str, Option<Value>, bool)> {
    match edge_kind {
        EdgeKind::Calls {
            argument_count,
            is_async,
        } if args.include_calls => {
            let meta = json!({
                "argument_count": argument_count,
                "is_async": is_async,
            });
            Some(("calls", Some(meta), true))
        }
        EdgeKind::Imports { alias, is_wildcard } if args.include_imports => {
            let mut meta = serde_json::Map::new();
            if let Some(a) = alias {
                meta.insert("alias".to_string(), json!(a));
            }
            meta.insert("is_wildcard".to_string(), json!(is_wildcard));
            Some(("imports", Some(Value::Object(meta)), false))
        }
        EdgeKind::Exports { kind, alias } if args.include_exports => {
            let mut meta = serde_json::Map::new();
            meta.insert(
                "kind".to_string(),
                json!(format!("{kind:?}").to_lowercase()),
            );
            if let Some(a) = alias {
                meta.insert("alias".to_string(), json!(a));
            }
            Some(("exports", Some(Value::Object(meta)), false))
        }
        EdgeKind::TypeOf { .. } if args.include_returns => Some(("returns", None, false)),
        _ => None,
    }
}

/// Process a single BFS node during export graph collection.
///
/// Adds the node to the output map, processes its outgoing edges, and enqueues
/// targets for traversal. Returns `true` if the `max_results` limit was hit.
#[allow(clippy::too_many_arguments)]
fn process_bfs_node_for_export(
    current_id: NodeId,
    depth: usize,
    args: &ExportGraphArgs,
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
    nodes: &mut HashMap<String, NodeRefData>,
    edges: &mut Vec<RelationEdgeData>,
    queue: &mut VecDeque<(NodeId, usize)>,
    visited: &HashSet<NodeId>,
) -> bool {
    let strings = snapshot.strings();

    let Some(entry) = snapshot.get_node(current_id) else {
        return false;
    };

    // Check language filter
    let node_language = snapshot
        .files()
        .language_for_file(entry.file)
        .map_or_else(|| "unknown".to_string(), |l| l.to_string());

    if !args.languages.is_empty()
        && !args
            .languages
            .iter()
            .any(|l| l.eq_ignore_ascii_case(&node_language))
    {
        return false;
    }

    let qualified_name = entry
        .qualified_name
        .and_then(|sid| strings.resolve(sid))
        .map(|s| s.to_string())
        .unwrap_or_default();

    if qualified_name.is_empty() {
        return false;
    }

    let node_ref = build_node_ref_unified(snapshot, current_id, workspace_root);
    nodes.insert(qualified_name, node_ref);

    for edge in snapshot.edges().edges_from(current_id) {
        if edges.len() >= args.max_results {
            break;
        }

        let Some((relation_type, metadata, should_traverse)) =
            classify_edge_for_export(&edge.kind, args)
        else {
            continue;
        };

        let target_id = edge.target;
        let Some(target_entry) = snapshot.get_node(target_id) else {
            continue;
        };

        let target_qname = target_entry
            .qualified_name
            .and_then(|sid| strings.resolve(sid))
            .map(|s| s.to_string())
            .unwrap_or_default();

        if target_qname.is_empty() {
            continue;
        }

        edges.push(RelationEdgeData {
            from: Some(build_node_ref_unified(snapshot, current_id, workspace_root)),
            to: Some(build_node_ref_unified(snapshot, target_id, workspace_root)),
            relation_type: relation_type.to_string(),
            depth: u32::try_from(depth).unwrap_or(u32::MAX),
            metadata,
        });

        if should_traverse && depth < args.max_depth && !visited.contains(&target_id) {
            queue.push_back((target_id, depth + 1));
        }
    }

    edges.len() >= args.max_results
}

/// Collect export graph data from unified graph.
///
/// Returns (nodes, edges, `was_truncated`) where `was_truncated` indicates if
/// `max_results` was reached before processing completed.
fn collect_export_graph_data_unified(
    args: &ExportGraphArgs,
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
    seeds: &[NodeId],
) -> (HashMap<String, NodeRefData>, Vec<RelationEdgeData>, bool) {
    let mut nodes: HashMap<String, NodeRefData> = HashMap::new();
    let mut edges: Vec<RelationEdgeData> = Vec::new();
    let mut visited = HashSet::new();
    let mut queue: VecDeque<(NodeId, usize)> = seeds.iter().map(|&id| (id, 0usize)).collect();
    let mut was_truncated = false;

    while let Some((current_id, depth)) = queue.pop_front() {
        if depth > args.max_depth || visited.contains(&current_id) {
            continue;
        }
        visited.insert(current_id);

        let hit_limit = process_bfs_node_for_export(
            current_id,
            depth,
            args,
            snapshot,
            workspace_root,
            &mut nodes,
            &mut edges,
            &mut queue,
            &visited,
        );

        if hit_limit {
            was_truncated = true;
            break;
        }
    }

    (nodes, edges, was_truncated)
}

fn paginate_export_graph(
    edges: &[RelationEdgeData],
    nodes: HashMap<String, NodeRefData>,
    pagination: &crate::tools::PaginationArgs,
) -> (Vec<RelationEdgeData>, Vec<NodeRefData>, Option<String>) {
    let (page_slice, next_page_token) = paginate(edges, pagination);
    let page_edges: Vec<RelationEdgeData> = page_slice.to_vec();

    let mut page_nodes: HashSet<String> = HashSet::new();
    for edge in &page_edges {
        if let Some(ref from) = edge.from {
            page_nodes.insert(from.qualified_name.clone());
        }
        if let Some(ref to) = edge.to {
            page_nodes.insert(to.qualified_name.clone());
        }
    }

    let mut nodes_vec: Vec<NodeRefData> = nodes
        .into_iter()
        .filter(|(k, _)| page_nodes.contains(k))
        .map(|(_, v)| v)
        .collect();
    nodes_vec.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));

    (page_edges, nodes_vec, next_page_token)
}

/// Resolve post-pagination `NodeRefData` back to `NodeId`s for rendering scope.
///
/// Matches each DTO's qualified name against the snapshot's string interner to recover
/// the corresponding `NodeId`. Only nodes whose qualified names appear in `nodes_vec`
/// are included in the returned set, so the exporters render exactly the post-pagination
/// subgraph rather than the entire snapshot.
fn resolve_page_node_ids(snapshot: &GraphSnapshot, nodes_vec: &[NodeRefData]) -> HashSet<NodeId> {
    let strings = snapshot.strings();
    let page_qnames: HashSet<&str> = nodes_vec
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();

    snapshot
        .iter_nodes()
        .filter_map(|(nid, entry)| {
            let qname = entry.qualified_name.and_then(|sid| strings.resolve(sid))?;
            if page_qnames.contains(qname.as_ref()) {
                Some(nid)
            } else {
                None
            }
        })
        .collect()
}

fn render_export_graph(
    args: &ExportGraphArgs,
    unified_graph: Option<std::sync::Arc<sqry_core::graph::unified::concurrent::CodeGraph>>,
    page_node_ids: &HashSet<NodeId>,
) -> Option<String> {
    if args.format == "json" {
        return None;
    }

    let Some(graph) = unified_graph else {
        tracing::warn!(
            format = args.format,
            "Graph visualization requires unified graph snapshot (.sqry/graph/). \
             Run `sqry graph save` to create it."
        );
        return None;
    };

    let snapshot = graph.snapshot();
    let filter = Some(page_node_ids.clone());

    match args.format.as_str() {
        "dot" => {
            let config = DotConfig::default()
                .with_cross_language_highlight(true)
                .with_details(args.verbose)
                .with_edge_labels(args.verbose)
                .with_filter_node_ids(filter);
            let exporter = UnifiedDotExporter::with_config(&snapshot, config);
            Some(exporter.export())
        }
        "d2" => {
            let config = D2Config::default()
                .with_cross_language_highlight(true)
                .with_details(args.verbose)
                .with_edge_labels(args.verbose)
                .with_filter_node_ids(filter);
            let exporter = UnifiedD2Exporter::with_config(&snapshot, config);
            Some(exporter.export())
        }
        "mermaid" => {
            let config = MermaidConfig::default()
                .with_cross_language_highlight(true)
                .with_edge_labels(args.verbose)
                .with_filter_node_ids(filter);
            let exporter = UnifiedMermaidExporter::with_config(&snapshot, config);
            Some(exporter.export())
        }
        _ => None,
    }
}

/// Execute subgraph extraction around seed symbols.
///
/// Uses `CodeGraph` exclusively for subgraph extraction.
pub fn execute_subgraph(args: &SubgraphArgs) -> Result<ToolExecution<DependencyGraphData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _base = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();

    // Get GraphIdentity for cache isolation
    let identity = get_graph_identity(&workspace_root)?;

    // Create cache key with full GraphIdentity for workspace isolation
    let cache_key = graph_cache::SubgraphCacheKey::new(
        args.symbols.clone(),
        args.max_depth,
        args.max_nodes,
        args.include_callers,
        args.include_callees,
        args.include_imports,
        args.cross_language,
    )
    .with_graph_identity(&identity);

    // Try cache first, fall back to computation
    let CacheOutcome {
        data: graph_data,
        state: cache_state,
        latency_ms: cache_latency_ms,
    } = graph_cache::get_or_compute_subgraph(cache_key, || {
        compute_subgraph_data(args, &snapshot, &workspace_root)
    });

    tracing::debug!(
        cache_state = ?cache_state,
        cache_latency_ms,
        "subgraph cache outcome"
    );

    let (graph_data, next_page_token, total_nodes, _total_edges) =
        apply_subgraph_pagination(graph_data, &args.pagination);

    let execution_ms = duration_to_ms(start.elapsed());

    let graph_metadata = build_graph_metadata(
        Some(&workspace_root),
        Some(&snapshot),
        Some(CacheRequestContext {
            tool: "subgraph",
            state: cache_state,
            latency_ms: cache_latency_ms,
        }),
    );

    Ok(ToolExecution {
        data: graph_data,
        used_index: false,
        used_graph: true,
        graph_metadata: Some(graph_metadata),
        execution_ms,
        next_page_token,
        total: Some(total_nodes as u64),
        truncated: Some(total_nodes > args.max_nodes),
        candidates_scanned: Some(total_nodes as u64),
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

fn compute_subgraph_data(
    args: &SubgraphArgs,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    workspace_root: &Path,
) -> DependencyGraphData {
    compute_subgraph_unified(args, snapshot, workspace_root).unwrap_or_else(|e| {
        tracing::error!(error = %e, "subgraph computation failed, returning empty graph");
        DependencyGraphData::empty()
    })
}

fn apply_subgraph_pagination(
    mut graph_data: DependencyGraphData,
    pagination: &crate::tools::PaginationArgs,
) -> (DependencyGraphData, Option<String>, usize, usize) {
    let total_nodes = graph_data.nodes.len();
    let total_edges = graph_data.edges.len();
    let page_size = pagination.size;
    let offset = pagination.offset;

    let paginated_nodes: Vec<NodeRefData> = graph_data
        .nodes
        .into_iter()
        .skip(offset)
        .take(page_size)
        .collect();

    let paginated_node_names: HashSet<_> = paginated_nodes
        .iter()
        .map(|n| n.qualified_name.as_str())
        .collect();

    let paginated_edges: Vec<RelationEdgeData> = graph_data
        .edges
        .into_iter()
        .filter(|edge| {
            let from_in = edge
                .from
                .as_ref()
                .is_some_and(|f| paginated_node_names.contains(f.qualified_name.as_str()));
            let to_in = edge
                .to
                .as_ref()
                .is_some_and(|t| paginated_node_names.contains(t.qualified_name.as_str()));
            from_in || to_in
        })
        .collect();

    tracing::debug!(
        total_nodes,
        paginated_nodes = paginated_nodes.len(),
        total_edges,
        paginated_edges = paginated_edges.len(),
        "pagination applied to subgraph"
    );

    let next_page_token = if offset + page_size < total_nodes {
        Some((offset + page_size).to_string())
    } else {
        None
    };

    graph_data.nodes = paginated_nodes;
    graph_data.edges = paginated_edges;

    (graph_data, next_page_token, total_nodes, total_edges)
}

// ============================================================================
// Unified Graph Subgraph Computation
// ============================================================================

struct UnifiedSubgraphContext<'a> {
    args: &'a SubgraphArgs,
    snapshot: &'a GraphSnapshot,
    workspace_root: &'a Path,
}

struct UnifiedSubgraphState {
    nodes: HashMap<NodeId, NodeRefData>,
    edges: Vec<RelationEdgeData>,
    visited_forward: HashSet<(NodeId, usize)>,
    visited_backward: HashSet<(NodeId, usize)>,
    queue_forward: VecDeque<(NodeId, usize)>,
    queue_backward: VecDeque<(NodeId, usize)>,
}

impl UnifiedSubgraphState {
    fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            edges: Vec::new(),
            visited_forward: HashSet::new(),
            visited_backward: HashSet::new(),
            queue_forward: VecDeque::new(),
            queue_backward: VecDeque::new(),
        }
    }
}

enum TraversalDecision {
    Process,
    Skip,
    Stop,
}

fn traversal_decision(
    args: &SubgraphArgs,
    nodes_len: usize,
    depth: usize,
    visited: &mut HashSet<(NodeId, usize)>,
    node_id: NodeId,
) -> TraversalDecision {
    if depth >= args.max_depth {
        return TraversalDecision::Skip;
    }
    if nodes_len >= args.max_nodes {
        return TraversalDecision::Stop;
    }
    if !visited.insert((node_id, depth)) {
        return TraversalDecision::Skip;
    }
    TraversalDecision::Process
}

fn collect_seed_nodes_unified(ctx: &UnifiedSubgraphContext<'_>) -> Result<Vec<NodeId>> {
    let mut seeds = Vec::new();
    for symbol_name in &ctx.args.symbols {
        let matches = find_nodes_by_name(ctx.snapshot, symbol_name);
        if matches.is_empty() {
            bail!("Seed symbol '{symbol_name}' not found in unified graph");
        }
        seeds.extend(matches);
    }

    if seeds.is_empty() {
        bail!("No seed symbols found for subgraph extraction");
    }

    Ok(seeds)
}

fn init_subgraph_state(ctx: &UnifiedSubgraphContext<'_>, seeds: &[NodeId]) -> UnifiedSubgraphState {
    let mut state = UnifiedSubgraphState::new();

    for &seed in seeds {
        if ctx.args.include_callees {
            state.queue_forward.push_back((seed, 0));
        }
        if ctx.args.include_callers {
            state.queue_backward.push_back((seed, 0));
        }

        let sym_ref = build_node_ref_unified(ctx.snapshot, seed, ctx.workspace_root);
        state.nodes.entry(seed).or_insert(sym_ref);
    }

    state
}

fn passes_cross_language_filter(
    ctx: &UnifiedSubgraphContext<'_>,
    from: NodeId,
    to: NodeId,
) -> bool {
    if ctx.args.cross_language {
        return true;
    }

    let from_lang = ctx
        .snapshot
        .get_node(from)
        .and_then(|n| ctx.snapshot.files().language_for_file(n.file));
    let to_lang = ctx
        .snapshot
        .get_node(to)
        .and_then(|n| ctx.snapshot.files().language_for_file(n.file));

    from_lang == to_lang
}

fn add_call_edge(
    ctx: &UnifiedSubgraphContext<'_>,
    state: &mut UnifiedSubgraphState,
    from: NodeId,
    to: NodeId,
    edge_kind: &EdgeKind,
    depth: usize,
) -> bool {
    if !passes_cross_language_filter(ctx, from, to) {
        return false;
    }

    let from_ref = build_node_ref_unified(ctx.snapshot, from, ctx.workspace_root);
    let to_ref = build_node_ref_unified(ctx.snapshot, to, ctx.workspace_root);

    state.nodes.entry(from).or_insert(from_ref.clone());
    state.nodes.entry(to).or_insert(to_ref.clone());

    state.edges.push(RelationEdgeData {
        from: Some(from_ref),
        to: Some(to_ref),
        relation_type: "calls".to_string(),
        depth: depth.try_into().unwrap_or(u32::MAX).saturating_add(1),
        metadata: call_edge_metadata(edge_kind),
    });

    true
}

fn process_forward_edge(
    ctx: &UnifiedSubgraphContext<'_>,
    state: &mut UnifiedSubgraphState,
    current: NodeId,
    depth: usize,
    edge: &StoreEdgeRef,
) -> bool {
    if !matches!(edge.kind, EdgeKind::Calls { .. }) {
        return true;
    }
    if state.nodes.len() >= ctx.args.max_nodes {
        return false;
    }

    if add_call_edge(ctx, state, current, edge.target, &edge.kind, depth) {
        state.queue_forward.push_back((edge.target, depth + 1));
    }

    true
}

fn process_backward_edge(
    ctx: &UnifiedSubgraphContext<'_>,
    state: &mut UnifiedSubgraphState,
    current: NodeId,
    depth: usize,
    edge: &StoreEdgeRef,
) -> bool {
    if !matches!(edge.kind, EdgeKind::Calls { .. }) {
        return true;
    }
    if state.nodes.len() >= ctx.args.max_nodes {
        return false;
    }

    if add_call_edge(ctx, state, edge.source, current, &edge.kind, depth) {
        state.queue_backward.push_back((edge.source, depth + 1));
    }

    true
}

fn traverse_forward(ctx: &UnifiedSubgraphContext<'_>, state: &mut UnifiedSubgraphState) {
    while let Some((current, depth)) = state.queue_forward.pop_front() {
        match traversal_decision(
            ctx.args,
            state.nodes.len(),
            depth,
            &mut state.visited_forward,
            current,
        ) {
            TraversalDecision::Process => {}
            TraversalDecision::Skip => continue,
            TraversalDecision::Stop => break,
        }

        for edge in ctx.snapshot.edges().edges_from(current) {
            if !process_forward_edge(ctx, state, current, depth, &edge) {
                break;
            }
        }
    }
}

fn traverse_backward(ctx: &UnifiedSubgraphContext<'_>, state: &mut UnifiedSubgraphState) {
    while let Some((current, depth)) = state.queue_backward.pop_front() {
        match traversal_decision(
            ctx.args,
            state.nodes.len(),
            depth,
            &mut state.visited_backward,
            current,
        ) {
            TraversalDecision::Process => {}
            TraversalDecision::Skip => continue,
            TraversalDecision::Stop => break,
        }

        for edge in ctx.snapshot.edges().edges_to(current) {
            if !process_backward_edge(ctx, state, current, depth, &edge) {
                break;
            }
        }
    }
}

fn add_import_edges(
    ctx: &UnifiedSubgraphContext<'_>,
    state: &mut UnifiedSubgraphState,
    seeds: &[NodeId],
) {
    if !ctx.args.include_imports {
        return;
    }

    for &seed in seeds {
        if state.nodes.len() >= ctx.args.max_nodes {
            break;
        }

        for edge in ctx.snapshot.edges().edges_from(seed) {
            let EdgeKind::Imports { alias, is_wildcard } = &edge.kind else {
                continue;
            };
            if state.nodes.len() >= ctx.args.max_nodes {
                break;
            }

            let from_ref = build_node_ref_unified(ctx.snapshot, seed, ctx.workspace_root);
            let to_ref = build_node_ref_unified(ctx.snapshot, edge.target, ctx.workspace_root);

            state.nodes.entry(seed).or_insert(from_ref.clone());
            state.nodes.entry(edge.target).or_insert(to_ref.clone());

            let mut meta = Map::new();
            meta.insert("is_wildcard".to_string(), Value::Bool(*is_wildcard));
            if let Some(alias_id) = alias
                && let Some(alias_str) = ctx.snapshot.strings().resolve(*alias_id)
            {
                meta.insert("alias".to_string(), Value::String(alias_str.to_string()));
            }

            state.edges.push(RelationEdgeData {
                from: Some(from_ref),
                to: Some(to_ref),
                relation_type: "imports".to_string(),
                depth: 1,
                metadata: Some(Value::Object(meta)),
            });
        }
    }
}

/// Compute subgraph using the unified graph (`GraphSnapshot`).
///
/// This is the migration target for `compute_subgraph_inner`, using the efficient
/// Arena+CSR storage instead of `DashMap` iteration.
#[allow(clippy::too_many_lines)] // Unified subgraph assembly is kept in one routine for traceability.
fn compute_subgraph_unified(
    args: &SubgraphArgs,
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
) -> Result<DependencyGraphData> {
    let ctx = UnifiedSubgraphContext {
        args,
        snapshot,
        workspace_root,
    };
    let seeds = collect_seed_nodes_unified(&ctx)?;
    let mut state = init_subgraph_state(&ctx, &seeds);

    traverse_forward(&ctx, &mut state);
    traverse_backward(&ctx, &mut state);
    add_import_edges(&ctx, &mut state, &seeds);

    // Convert nodes HashMap to Vec for response
    let mut node_vec: Vec<NodeRefData> = state.nodes.into_values().collect();

    // Sort nodes deterministically for reproducible output (GraphRAG requirement)
    node_vec.sort_by(|a, b| {
        a.qualified_name
            .cmp(&b.qualified_name)
            .then(a.name.cmp(&b.name))
            .then(a.file_uri.cmp(&b.file_uri))
    });

    Ok(DependencyGraphData {
        nodes: node_vec,
        edges: state.edges,
        rendered: None,
    })
}

/// Build `NodeRefData` from a unified graph node.
fn build_node_ref_unified(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    workspace_root: &Path,
) -> NodeRefData {
    let Some(entry) = snapshot.get_node(node_id) else {
        return fallback_node_ref_unified("unknown", workspace_root);
    };

    let strings = snapshot.strings();
    let files = snapshot.files();

    let name = strings
        .resolve(entry.name)
        .map_or_else(|| "unknown".to_string(), |s| s.to_string());

    let qualified_name =
        crate::execution::symbol_utils::display_entry_qualified_name(entry, strings, files, &name);

    let kind = match entry.kind {
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

    let language = files
        .language_for_file(entry.file)
        .map_or_else(|| "unknown".to_string(), |l| l.to_string());

    let file_path = files
        .resolve(entry.file)
        .map(|arc_path| workspace_root.join(arc_path.as_ref()))
        .unwrap_or_default();

    let file_uri = Url::from_file_path(&file_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&file_path),
        Into::into,
    );

    NodeRefData {
        name,
        qualified_name,
        kind: kind.to_string(),
        language,
        file_uri,
        range: RangeData {
            start: PositionData {
                line: entry.start_line,
                character: entry.start_column,
            },
            end: PositionData {
                line: entry.end_line,
                character: entry.end_column,
            },
        },
        metadata: None,
    }
}

/// Create a fallback symbol ref for unknown nodes (unified graph version).
fn fallback_node_ref_unified(name: &str, workspace_root: &Path) -> NodeRefData {
    NodeRefData {
        name: name.to_string(),
        qualified_name: name.to_string(),
        kind: "unknown".to_string(),
        language: "unknown".to_string(),
        file_uri: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
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
        metadata: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::tools::PaginationArgs;
    use sqry_core::graph::node::Language;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::edge::BidirectionalEdgeStore;
    use sqry_core::graph::unified::storage::NodeEntry;
    use sqry_core::graph::unified::storage::arena::NodeArena;
    use sqry_core::graph::unified::storage::indices::AuxiliaryIndices;
    use sqry_core::graph::unified::storage::interner::StringInterner;
    use sqry_core::graph::unified::storage::registry::FileRegistry;

    // ============================================================================
    // fallback_node_ref_unified tests
    // ============================================================================

    #[test]
    fn test_fallback_node_ref_unified_basic() {
        let workspace = PathBuf::from("/workspace");
        let result = fallback_node_ref_unified("test_symbol", &workspace);

        assert_eq!(result.name, "test_symbol");
        assert_eq!(result.qualified_name, "test_symbol");
        assert_eq!(result.kind, "unknown");
        assert_eq!(result.language, "unknown");
        assert_eq!(result.file_uri, "/workspace");
        assert_eq!(result.range.start.line, 0);
        assert_eq!(result.range.start.character, 0);
        assert_eq!(result.range.end.line, 0);
        assert_eq!(result.range.end.character, 0);
        assert!(result.metadata.is_none());
    }

    #[test]
    fn test_fallback_node_ref_unified_with_spaces_in_path() {
        let workspace = PathBuf::from("/my workspace/project");
        let result = fallback_node_ref_unified("symbol", &workspace);

        assert_eq!(result.file_uri, "/my workspace/project");
    }

    #[test]
    fn test_fallback_node_ref_unified_with_special_characters() {
        let workspace = PathBuf::from("/workspace");
        let result = fallback_node_ref_unified("test::symbol<T>", &workspace);

        assert_eq!(result.name, "test::symbol<T>");
        assert_eq!(result.qualified_name, "test::symbol<T>");
    }

    #[test]
    fn test_fallback_node_ref_unified_with_empty_name() {
        let workspace = PathBuf::from("/workspace");
        let result = fallback_node_ref_unified("", &workspace);

        assert_eq!(result.name, "");
        assert_eq!(result.qualified_name, "");
    }

    // ============================================================================
    // Helper to build test graph
    // ============================================================================

    /// Create a minimal test graph with a few nodes and edges.
    fn create_test_graph() -> CodeGraph {
        let mut nodes = NodeArena::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let edges = BidirectionalEdgeStore::new();
        let mut indices = AuxiliaryIndices::new();

        // Register file
        let file_id = files
            .register_with_language(Path::new("src/main.rs"), Some(Language::Rust))
            .unwrap();

        // Intern strings
        let name_main = strings.intern("main").unwrap();
        let name_helper = strings.intern("helper").unwrap();
        let qname_main = strings.intern("crate::main").unwrap();
        let qname_helper = strings.intern("crate::helper").unwrap();

        // Add nodes
        let main_entry = NodeEntry {
            kind: NodeKind::Function,
            name: name_main,
            file: file_id,
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 0,
            end_line: 10,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: Some(qname_main),
            visibility: None,
            is_async: false,
            is_static: false,
            body_hash: None,
            is_unsafe: false,
        };
        let main_id = nodes.alloc(main_entry.clone()).unwrap();
        indices.add(
            main_id,
            main_entry.kind,
            main_entry.name,
            main_entry.qualified_name,
            main_entry.file,
        );

        let helper_entry = NodeEntry {
            kind: NodeKind::Function,
            name: name_helper,
            file: file_id,
            start_byte: 100,
            end_byte: 200,
            start_line: 11,
            start_column: 0,
            end_line: 20,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: Some(qname_helper),
            visibility: None,
            is_async: true,
            is_static: false,
            body_hash: None,
            is_unsafe: false,
        };
        let helper_id = nodes.alloc(helper_entry.clone()).unwrap();
        indices.add(
            helper_id,
            helper_entry.kind,
            helper_entry.name,
            helper_entry.qualified_name,
            helper_entry.file,
        );

        CodeGraph::from_components(
            nodes,
            edges,
            strings,
            files,
            indices,
            sqry_core::graph::unified::NodeMetadataStore::new(),
        )
    }

    // ============================================================================
    // find_nodes_by_name_unified tests
    // ============================================================================

    #[test]
    fn test_find_nodes_by_simple_name() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();

        let matches = find_nodes_by_name(&snapshot, "main");
        assert_eq!(matches.len(), 1, "Should find exactly one 'main' node");
    }

    #[test]
    fn test_find_nodes_by_qualified_name() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();

        let matches = find_nodes_by_name(&snapshot, "crate::helper");
        assert_eq!(
            matches.len(),
            1,
            "Should find exactly one 'crate::helper' node"
        );
    }

    #[test]
    fn test_find_nodes_by_qualified_suffix() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();

        // Qualified name search also matches suffix pattern "::name"
        let matches = find_nodes_by_name(&snapshot, "helper");
        assert!(
            !matches.is_empty(),
            "Should find helper by simple name or suffix"
        );
    }

    #[test]
    fn test_find_nodes_nonexistent() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();

        let matches = find_nodes_by_name(&snapshot, "nonexistent_function");
        assert!(matches.is_empty(), "Should not find nonexistent function");
    }

    #[test]
    fn test_find_nodes_empty_name() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();

        let matches = find_nodes_by_name(&snapshot, "");
        assert!(matches.is_empty(), "Empty name should not match any nodes");
    }

    // ============================================================================
    // build_node_ref_unified tests
    // ============================================================================

    #[test]
    fn test_build_node_ref_unified_basic() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();
        let workspace = PathBuf::from("/workspace");

        // Find main node
        let matches = find_nodes_by_name(&snapshot, "main");
        assert!(!matches.is_empty(), "Should find main node");

        let node_id = matches[0];
        let result = build_node_ref_unified(&snapshot, node_id, &workspace);

        assert_eq!(result.name, "main");
        assert_eq!(result.qualified_name, "crate::main");
        assert_eq!(result.kind, "function");
        // Language::to_string() returns lowercase
        assert!(
            result.language.to_lowercase().contains("rust"),
            "Language should contain 'rust': {}",
            result.language
        );
        assert_eq!(result.range.start.line, 1);
        assert_eq!(result.range.end.line, 10);
    }

    #[test]
    fn test_build_node_ref_unified_async_function() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();
        let workspace = PathBuf::from("/workspace");

        // Find helper node (which is async)
        let matches = find_nodes_by_name(&snapshot, "helper");
        assert!(!matches.is_empty(), "Should find helper node");

        let node_id = matches[0];
        let result = build_node_ref_unified(&snapshot, node_id, &workspace);

        assert_eq!(result.name, "helper");
        assert_eq!(result.qualified_name, "crate::helper");
        assert_eq!(result.kind, "function");
    }

    #[test]
    fn test_build_node_ref_unified_invalid_node() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();
        let workspace = PathBuf::from("/workspace");

        // Create an invalid NodeId
        let invalid_id = sqry_core::graph::unified::node::NodeId::new(9999, 0);
        let result = build_node_ref_unified(&snapshot, invalid_id, &workspace);

        // Should return fallback with "unknown" values
        assert_eq!(result.name, "unknown");
        assert_eq!(result.kind, "unknown");
    }

    // ============================================================================
    // DependencyGraphData tests
    // ============================================================================

    #[test]
    fn test_dependency_graph_data_empty() {
        let data = DependencyGraphData::empty();

        assert!(data.nodes.is_empty());
        assert!(data.edges.is_empty());
        assert!(data.rendered.is_none());
    }

    // ============================================================================
    // Integration tests (skip if engine not available)
    // ============================================================================

    #[test]
    fn test_execute_get_dependencies_no_engine() {
        let missing_path = missing_workspace_path();

        let args = ShowDependenciesArgs {
            path: missing_path,
            file_path: Some("src/lib.rs".to_string()),
            symbol_name: None,
            max_depth: 2,
            max_results: 100,
            pagination: PaginationArgs {
                offset: 0,
                size: 50,
            },
        };

        let result = execute_get_dependencies(&args);
        assert!(result.is_err(), "missing workspace should fail gracefully");
    }

    #[test]
    fn test_execute_export_graph_no_engine() {
        let missing_path = missing_workspace_path();

        let args = ExportGraphArgs {
            path: missing_path,
            file_path: Some("src/lib.rs".to_string()),
            symbol_name: None,
            symbols: vec![],
            max_depth: 2,
            max_results: 100,
            format: "json".to_string(),
            include_calls: true,
            include_imports: false,
            include_exports: false,
            include_returns: false,
            languages: vec![],
            verbose: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 50,
            },
        };

        let result = execute_export_graph(&args);
        assert!(result.is_err(), "missing workspace should fail gracefully");
    }

    #[test]
    fn test_execute_subgraph_no_engine() {
        let missing_path = missing_workspace_path();

        let args = SubgraphArgs {
            path: missing_path,
            symbols: vec!["main".to_string()],
            max_depth: 2,
            max_nodes: 50,
            include_callers: true,
            include_callees: true,
            include_imports: false,
            cross_language: true,
            pagination: PaginationArgs {
                offset: 0,
                size: 50,
            },
        };

        let result = execute_subgraph(&args);
        assert!(result.is_err(), "missing workspace should fail gracefully");
    }

    fn missing_workspace_path() -> String {
        let temp_dir = tempfile::TempDir::new().expect("temp dir should be created");
        temp_dir
            .path()
            .join("missing-workspace")
            .display()
            .to_string()
    }

    // ============================================================================
    // NodeKind to kind string mapping tests
    // ============================================================================

    #[test]
    fn test_node_kind_string_mapping() {
        // Test the node kind to string mapping used in build_node_ref_unified
        let graph = create_test_graph();
        let snapshot = graph.snapshot();
        let workspace = PathBuf::from("/workspace");

        let matches = find_nodes_by_name(&snapshot, "main");
        let result = build_node_ref_unified(&snapshot, matches[0], &workspace);

        // NodeKind::Function should map to "function"
        assert_eq!(result.kind, "function");
    }

    // ============================================================================
    // Edge case tests
    // ============================================================================

    #[test]
    fn test_empty_graph_snapshot() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();

        // Empty graph should have no nodes
        assert_eq!(snapshot.nodes().len(), 0);

        // Find should return empty
        let matches = find_nodes_by_name(&snapshot, "anything");
        assert!(matches.is_empty());
    }

    // ============================================================================
    // resolve_page_node_ids tests
    // ============================================================================

    #[test]
    fn test_resolve_page_node_ids_matches_nodes_vec() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();
        let workspace = PathBuf::from("/workspace");

        // Build a NodeRefData for "main" only
        let main_matches = find_nodes_by_name(&snapshot, "main");
        assert!(!main_matches.is_empty(), "test graph must have 'main' node");
        let main_node_ref = build_node_ref_unified(&snapshot, main_matches[0], &workspace);

        let nodes_vec = vec![main_node_ref];
        let resolved = resolve_page_node_ids(&snapshot, &nodes_vec);

        // Should have resolved exactly the "main" NodeId
        assert_eq!(resolved.len(), 1, "should resolve exactly one NodeId");
        assert!(
            resolved.contains(&main_matches[0]),
            "resolved set must contain the main NodeId"
        );
    }

    #[test]
    fn test_resolve_page_node_ids_empty_vec() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();

        let resolved = resolve_page_node_ids(&snapshot, &[]);
        assert!(
            resolved.is_empty(),
            "empty nodes_vec should yield empty NodeId set"
        );
    }

    // ============================================================================
    // render_export_graph filter tests
    // ============================================================================

    #[test]
    fn test_render_export_graph_respects_filter() {
        use std::sync::Arc;

        let graph = create_test_graph();
        let snapshot = graph.snapshot();

        // Collect only the "main" node
        let main_matches = find_nodes_by_name(&snapshot, "main");
        assert!(!main_matches.is_empty(), "test graph must have 'main' node");

        let page_node_ids: HashSet<NodeId> = main_matches.into_iter().collect();

        let args = ExportGraphArgs {
            path: ".".to_string(),
            file_path: None,
            symbol_name: Some("main".to_string()),
            symbols: vec![],
            max_depth: 1,
            max_results: 10,
            format: "mermaid".to_string(),
            include_calls: true,
            include_imports: false,
            include_exports: false,
            include_returns: false,
            languages: vec![],
            verbose: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 50,
            },
        };

        let rendered = render_export_graph(&args, Some(Arc::new(graph)), &page_node_ids);
        assert!(rendered.is_some(), "should produce mermaid output");

        let mermaid = rendered.unwrap();
        // Count node definition lines (lines starting with whitespace then a node identifier
        // followed by '[' for rectangular nodes or '(' for round nodes).
        let node_lines: Vec<&str> = mermaid
            .lines()
            .filter(|l| {
                let trimmed = l.trim_start();
                (trimmed.starts_with('n') || trimmed.contains('['))
                    && trimmed.contains('[')
                    && !trimmed.starts_with("%%")
                    && !trimmed.starts_with("graph")
                    && !trimmed.starts_with("flowchart")
                    && !trimmed.starts_with("subgraph")
                    && !trimmed.starts_with("end")
                    && trimmed.contains("main")
            })
            .collect();
        assert!(
            !node_lines.is_empty(),
            "rendered mermaid should reference the 'main' node; output:\n{mermaid}"
        );

        // Verify "helper" does not appear — it was excluded from page_node_ids
        assert!(
            !mermaid.contains("helper"),
            "rendered mermaid must not contain the excluded 'helper' node; output:\n{mermaid}"
        );
    }

    #[test]
    fn test_render_export_graph_json_format_returns_none() {
        let page_node_ids: HashSet<NodeId> = HashSet::new();

        let args = ExportGraphArgs {
            path: ".".to_string(),
            file_path: None,
            symbol_name: None,
            symbols: vec![],
            max_depth: 1,
            max_results: 10,
            format: "json".to_string(),
            include_calls: true,
            include_imports: false,
            include_exports: false,
            include_returns: false,
            languages: vec![],
            verbose: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 50,
            },
        };

        let rendered = render_export_graph(&args, None, &page_node_ids);
        assert!(rendered.is_none(), "json format should always return None");
    }

    #[test]
    fn test_render_export_graph_no_graph_returns_none() {
        let page_node_ids: HashSet<NodeId> = HashSet::new();

        let args = ExportGraphArgs {
            path: ".".to_string(),
            file_path: None,
            symbol_name: None,
            symbols: vec![],
            max_depth: 1,
            max_results: 10,
            format: "mermaid".to_string(),
            include_calls: true,
            include_imports: false,
            include_exports: false,
            include_returns: false,
            languages: vec![],
            verbose: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 50,
            },
        };

        let rendered = render_export_graph(&args, None, &page_node_ids);
        assert!(
            rendered.is_none(),
            "missing graph should produce no render output"
        );
    }

    // ============================================================================
    // collect_export_graph_seeds_unified symbols field tests
    // ============================================================================

    #[test]
    fn test_export_graph_seeds_from_symbols_vec() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();
        let workspace = PathBuf::from("/workspace");

        let args = ExportGraphArgs {
            path: ".".to_string(),
            file_path: None,
            symbol_name: None,
            symbols: vec!["main".to_string(), "helper".to_string()],
            max_depth: 1,
            max_results: 100,
            format: "json".to_string(),
            include_calls: true,
            include_imports: false,
            include_exports: false,
            include_returns: false,
            languages: vec![],
            verbose: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 50,
            },
        };

        let seeds = collect_export_graph_seeds_unified(&args, &snapshot, &workspace).unwrap();
        assert_eq!(seeds.len(), 2, "should find both main and helper as seeds");
    }

    #[test]
    fn test_export_graph_seeds_deduplication_symbol_name_and_symbols() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();
        let workspace = PathBuf::from("/workspace");

        // Both symbol_name and symbols contain "main" — dedup should yield 1 seed for main
        let args = ExportGraphArgs {
            path: ".".to_string(),
            file_path: None,
            symbol_name: Some("main".to_string()),
            symbols: vec!["main".to_string()],
            max_depth: 1,
            max_results: 100,
            format: "json".to_string(),
            include_calls: true,
            include_imports: false,
            include_exports: false,
            include_returns: false,
            languages: vec![],
            verbose: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 50,
            },
        };

        let seeds_deduped =
            collect_export_graph_seeds_unified(&args, &snapshot, &workspace).unwrap();

        // Without deduplication this would be 2; with dedup it must be 1
        let args_no_dedup = ExportGraphArgs {
            path: ".".to_string(),
            file_path: None,
            symbol_name: Some("main".to_string()),
            symbols: vec![],
            max_depth: 1,
            max_results: 100,
            format: "json".to_string(),
            include_calls: true,
            include_imports: false,
            include_exports: false,
            include_returns: false,
            languages: vec![],
            verbose: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 50,
            },
        };
        let seeds_single =
            collect_export_graph_seeds_unified(&args_no_dedup, &snapshot, &workspace).unwrap();

        assert_eq!(
            seeds_deduped.len(),
            seeds_single.len(),
            "duplicate symbol_name + symbols[\"main\"] should deduplicate to same count as single seed"
        );
    }

    #[test]
    fn test_workspace_path_in_file_uri() {
        let graph = create_test_graph();
        let snapshot = graph.snapshot();
        let workspace = PathBuf::from("/my/workspace");

        let matches = find_nodes_by_name(&snapshot, "main");
        let result = build_node_ref_unified(&snapshot, matches[0], &workspace);

        // File URI should be a valid file URI containing the file path
        // Note: workspace_root.join() canonicalizes the path, so we just check
        // that the URI contains the registered filename
        assert!(
            result.file_uri.contains("main.rs"),
            "file_uri should contain 'main.rs': {}",
            result.file_uri
        );
        // Should be a valid file URI format
        assert!(
            result.file_uri.starts_with("file://") || result.file_uri.starts_with('/'),
            "file_uri should be a valid path or file URI: {}",
            result.file_uri
        );
    }

    // ============================================================================
    // Regression tests for render_export_graph empty filter
    // ============================================================================

    #[test]
    fn test_render_export_graph_empty_filter_yields_empty_output() {
        use std::sync::Arc;

        let graph = create_test_graph();
        let empty_ids: HashSet<NodeId> = HashSet::new();

        let args = ExportGraphArgs {
            path: ".".to_string(),
            file_path: None,
            symbol_name: Some("main".to_string()),
            symbols: vec![],
            max_depth: 1,
            max_results: 10,
            format: "mermaid".to_string(),
            include_calls: true,
            include_imports: false,
            include_exports: false,
            include_returns: false,
            languages: vec![],
            verbose: false,
            pagination: PaginationArgs {
                offset: 0,
                size: 50,
            },
        };

        let rendered = render_export_graph(&args, Some(Arc::new(graph)), &empty_ids);
        assert!(rendered.is_some(), "should still produce mermaid output");

        let mermaid = rendered.unwrap();
        // With empty filter, no nodes should be rendered
        let node_lines: Vec<&str> = mermaid
            .lines()
            .filter(|l| l.trim_start().starts_with('n') && l.contains('['))
            .collect();
        assert_eq!(
            node_lines.len(),
            0,
            "empty filter should yield no nodes in rendered output"
        );
    }

    // ============================================================================
    // Regression test: resolve_page_node_ids with duplicate qualified names
    // ============================================================================

    #[test]
    fn test_resolve_page_node_ids_round_trip() {
        // Verify that resolve_page_node_ids correctly resolves a NodeRefData
        // back to its original NodeId via qualified name matching.
        //
        // Note: If multiple NodeIds share a qualified name (rare in practice),
        // resolve_page_node_ids returns all of them — this is acceptable
        // over-rendering for visualization purposes. The standard test graph
        // has unique qualified names, so this test verifies the normal path.
        let graph = create_test_graph();
        let snapshot = graph.snapshot();

        let main_nodes: Vec<NodeId> = find_nodes_by_name(&snapshot, "main");
        assert_eq!(main_nodes.len(), 1, "test graph has exactly 1 'main' node");

        let node_ref =
            build_node_ref_unified(&snapshot, main_nodes[0], &PathBuf::from("/workspace"));
        let result = resolve_page_node_ids(&snapshot, &[node_ref]);
        assert_eq!(result.len(), 1, "should resolve to exactly 1 NodeId");
        assert!(
            result.contains(&main_nodes[0]),
            "should resolve back to the original NodeId"
        );
    }
}
