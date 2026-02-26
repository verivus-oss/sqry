//! Relation query tool execution.
//!
//! This module implements the `relation_query` tool which finds callers,
//! callees, imports, exports, and return types for a given symbol.
//!
//! # Migration Status (FR-2025-007)
//!
//! ✅ Fully migrated to unified graph architecture. Uses `GraphSnapshot` for:
//! - Symbol lookup via `nodes_by_symbol()` and qualified name search
//! - Relation queries via `edges().edges_from()` and `edges().edges_to()`
//! - Full edge metadata (async, argument count, aliases, etc.)

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeId;

use crate::engine::{canonicalize_in_workspace, engine_for_workspace};
use crate::tools::{CallHierarchyArgs, CallHierarchyDirection, RelationQueryArgs, RelationType};

use crate::execution::graph_builders::build_graph_metadata;
use crate::execution::types::{
    CallHierarchyData, CallHierarchyNode, NodeRefData, PositionData, RangeData, RelationEdgeData,
    RelationQueryData, ToolExecution,
};
use crate::execution::utils::{duration_to_ms, paginate};

/// Execute the `relation_query` tool to find symbol relations.
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
pub fn execute_relation_query(
    args: &RelationQueryArgs,
) -> Result<ToolExecution<RelationQueryData>> {
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require unified graph for relation queries
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();

    let start = Instant::now();

    tracing::debug!(
        symbol = %args.symbol,
        relation = %args.relation.as_str(),
        max_depth = args.max_depth,
        max_results = args.max_results,
        path = %args.path,
        "Executing relation_query tool"
    );

    let edges = collect_relation_edges_unified(&snapshot, &workspace_root, args, args.max_results)?;

    let total = edges.len();
    let (page_slice, next_page_token) = paginate(&edges, &args.pagination);

    let relations = page_slice.to_vec();

    let graph_metadata = build_graph_metadata(Some(&workspace_root), Some(&snapshot), None);

    Ok(ToolExecution {
        data: RelationQueryData {
            relation_type: args.relation.as_str().to_string(),
            relations,
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

/// Collect relation edges for a symbol using unified graph.
fn collect_relation_edges_unified(
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
    args: &RelationQueryArgs,
    max_results: usize,
) -> Result<Vec<RelationEdgeData>> {
    let start_nodes = find_nodes_by_name(snapshot, &args.symbol);
    if start_nodes.is_empty() {
        bail!("Symbol '{}' not found in graph", args.symbol);
    }

    let mut results = Vec::new();
    let mut visited: HashSet<(NodeId, usize)> = HashSet::new();
    let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();

    // Initialize BFS from all matching nodes
    for &node in &start_nodes {
        queue.push_back((node, 0));
    }

    while let Some((current, depth)) = queue.pop_front() {
        if !should_visit(depth, args.max_depth, (current, depth), &mut visited) {
            continue;
        }

        let edges_data =
            collect_edges_for_relation(snapshot, current, depth, workspace_root, args.relation);
        let reached_limit = append_edges_until_limit(&mut results, edges_data, max_results);

        if reached_limit {
            break;
        }

        enqueue_next_nodes(snapshot, current, depth, args.relation, &mut queue);
    }

    Ok(results)
}

/// Check if a node should be visited in BFS traversal.
fn should_visit(
    depth: usize,
    max_depth: usize,
    key: (NodeId, usize),
    visited: &mut HashSet<(NodeId, usize)>,
) -> bool {
    depth < max_depth && visited.insert(key)
}

/// Collect edges for a specific relation type.
fn collect_edges_for_relation(
    snapshot: &GraphSnapshot,
    node: NodeId,
    depth: usize,
    workspace_root: &Path,
    relation: RelationType,
) -> Vec<RelationEdgeData> {
    match relation {
        RelationType::Callers => collect_callers(snapshot, node, depth, workspace_root),
        RelationType::Callees => collect_callees(snapshot, node, depth, workspace_root),
        RelationType::Imports => collect_imports(snapshot, node, depth, workspace_root),
        RelationType::Exports => collect_exports(snapshot, node, depth, workspace_root),
        RelationType::Returns => collect_returns(snapshot, node, depth, workspace_root),
    }
}

/// Append edges to results until `max_results` is reached. Returns true if limit was reached.
fn append_edges_until_limit(
    results: &mut Vec<RelationEdgeData>,
    edges: Vec<RelationEdgeData>,
    max_results: usize,
) -> bool {
    for edge in edges {
        if results.len() >= max_results {
            return true;
        }
        results.push(edge);
    }
    results.len() >= max_results
}

/// Enqueue next nodes for traversal based on relation type.
fn enqueue_next_nodes(
    snapshot: &GraphSnapshot,
    current: NodeId,
    depth: usize,
    relation: RelationType,
    queue: &mut VecDeque<(NodeId, usize)>,
) {
    match relation {
        RelationType::Callers => {
            for caller_id in snapshot.get_callers(current) {
                queue.push_back((caller_id, depth + 1));
            }
        }
        RelationType::Callees => {
            for callee_id in snapshot.get_callees(current) {
                queue.push_back((callee_id, depth + 1));
            }
        }
        // Imports, Exports, Returns don't continue traversal
        _ => {}
    }
}

/// Collect callers (functions that call this node).
fn collect_callers(
    snapshot: &GraphSnapshot,
    node: NodeId,
    depth: usize,
    workspace_root: &Path,
) -> Vec<RelationEdgeData> {
    let mut results = Vec::new();

    for edge in snapshot.edges().edges_to(node) {
        if !matches!(edge.kind, EdgeKind::Calls { .. }) {
            continue;
        }

        let from_ref = build_node_ref(snapshot, edge.source, workspace_root);
        let to_ref = build_node_ref(snapshot, node, workspace_root);

        let metadata = match &edge.kind {
            EdgeKind::Calls {
                argument_count,
                is_async,
            } => Some(json!({
                "argument_count": argument_count,
                "is_async": is_async,
            })),
            _ => None,
        };

        results.push(RelationEdgeData {
            from: Some(from_ref),
            to: Some(to_ref),
            relation_type: "callers".to_string(),
            depth: depth.try_into().unwrap_or(u32::MAX).saturating_add(1),
            metadata,
        });
    }

    results
}

/// Collect callees (functions called by this node).
fn collect_callees(
    snapshot: &GraphSnapshot,
    node: NodeId,
    depth: usize,
    workspace_root: &Path,
) -> Vec<RelationEdgeData> {
    let mut results = Vec::new();

    for edge in snapshot.edges().edges_from(node) {
        if !matches!(edge.kind, EdgeKind::Calls { .. }) {
            continue;
        }

        let from_ref = build_node_ref(snapshot, node, workspace_root);
        let to_ref = build_node_ref(snapshot, edge.target, workspace_root);

        let metadata = match &edge.kind {
            EdgeKind::Calls {
                argument_count,
                is_async,
            } => Some(json!({
                "argument_count": argument_count,
                "is_async": is_async,
            })),
            _ => None,
        };

        results.push(RelationEdgeData {
            from: Some(from_ref),
            to: Some(to_ref),
            relation_type: "callees".to_string(),
            depth: depth.try_into().unwrap_or(u32::MAX).saturating_add(1),
            metadata,
        });
    }

    results
}

/// Collect imports (symbols imported by this node).
fn collect_imports(
    snapshot: &GraphSnapshot,
    node: NodeId,
    depth: usize,
    workspace_root: &Path,
) -> Vec<RelationEdgeData> {
    let mut results = Vec::new();
    let strings = snapshot.strings();

    for edge in snapshot.edges().edges_from(node) {
        let EdgeKind::Imports { alias, is_wildcard } = &edge.kind else {
            continue;
        };

        let from_ref = build_node_ref(snapshot, node, workspace_root);
        let to_ref = build_node_ref(snapshot, edge.target, workspace_root);

        let mut map = Map::new();
        map.insert("is_wildcard".to_string(), Value::Bool(*is_wildcard));
        if let Some(alias_id) = alias
            && let Some(alias_str) = strings.resolve(*alias_id)
        {
            map.insert("alias".to_string(), Value::String(alias_str.to_string()));
        }

        results.push(RelationEdgeData {
            from: Some(from_ref),
            to: Some(to_ref),
            relation_type: "imports".to_string(),
            depth: depth.try_into().unwrap_or(u32::MAX).saturating_add(1),
            metadata: Some(Value::Object(map)),
        });
    }

    results
}

/// Collect exports (symbols exported by this node).
fn collect_exports(
    snapshot: &GraphSnapshot,
    node: NodeId,
    depth: usize,
    workspace_root: &Path,
) -> Vec<RelationEdgeData> {
    let mut results = Vec::new();
    let strings = snapshot.strings();

    for edge in snapshot.edges().edges_from(node) {
        let EdgeKind::Exports { kind, alias } = &edge.kind else {
            continue;
        };

        let from_ref = build_node_ref(snapshot, node, workspace_root);
        let to_ref = build_node_ref(snapshot, edge.target, workspace_root);

        let mut map = Map::new();
        map.insert("kind".to_string(), Value::String(format!("{kind:?}")));
        if let Some(alias_id) = alias
            && let Some(alias_str) = strings.resolve(*alias_id)
        {
            map.insert("alias".to_string(), Value::String(alias_str.to_string()));
        }

        results.push(RelationEdgeData {
            from: Some(from_ref),
            to: Some(to_ref),
            relation_type: "exports".to_string(),
            depth: depth.try_into().unwrap_or(u32::MAX).saturating_add(1),
            metadata: Some(Value::Object(map)),
        });
    }

    results
}

/// Collect return type information (not stored as edges in unified graph).
///
/// Note: The unified graph doesn't have a dedicated Returns edge type.
/// This returns an empty result for now - return type tracking would need
/// to be added to the unified graph edge model.
fn collect_returns(
    snapshot: &GraphSnapshot,
    node: NodeId,
    depth: usize,
    workspace_root: &Path,
) -> Vec<RelationEdgeData> {
    // The unified graph doesn't store return types as edges.
    // We could potentially extract this from node metadata if available.
    let from_ref = build_node_ref(snapshot, node, workspace_root);

    // Check if node has return type in metadata (not currently implemented)
    // For now, return empty as return type edges aren't in the unified model
    let entry = snapshot.get_node(node);
    if entry.is_some() {
        // Return type would be in node metadata if we stored it
        // For now, just indicate the function exists
        vec![RelationEdgeData {
            from: Some(from_ref),
            to: None,
            relation_type: "returns".to_string(),
            depth: depth.try_into().unwrap_or(u32::MAX).saturating_add(1),
            metadata: Some(json!({
                "note": "Return type tracking not yet implemented in unified graph"
            })),
        }]
    } else {
        vec![]
    }
}

/// Build `NodeRefData` from a unified graph node.
fn build_node_ref(snapshot: &GraphSnapshot, node_id: NodeId, workspace_root: &Path) -> NodeRefData {
    use sqry_core::graph::unified::node::NodeKind;

    let Some(entry) = snapshot.get_node(node_id) else {
        return fallback_ref("unknown", workspace_root);
    };

    let strings = snapshot.strings();
    let files = snapshot.files();

    let name = strings
        .resolve(entry.name)
        .map_or_else(|| "unknown".to_string(), |s| s.to_string());

    let qualified_name = entry
        .qualified_name
        .and_then(|sid| strings.resolve(sid))
        .map_or_else(|| name.clone(), |s| s.to_string());

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

    let file_uri = url::Url::from_file_path(&file_path).ok().map_or_else(
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

/// Create a fallback symbol ref for unknown nodes.
fn fallback_ref(name: &str, workspace_root: &Path) -> NodeRefData {
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

/// Execute the `call_hierarchy` tool to find callers or callees of a symbol.
pub fn execute_call_hierarchy(
    args: &CallHierarchyArgs,
) -> Result<ToolExecution<CallHierarchyData>> {
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require unified graph for call hierarchy
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();
    let start = Instant::now();

    tracing::debug!(
        symbol = %args.symbol,
        direction = %args.direction.as_str(),
        max_depth = args.max_depth,
        max_results = args.max_results,
        file_path = ?args.file_path,
        "Executing call_hierarchy tool"
    );

    // Find the root symbol node
    let root_nodes = find_nodes_by_name(&snapshot, &args.symbol);

    // If file_path is specified, filter to nodes in that file
    let root_node = if let Some(ref file_path) = args.file_path {
        let files = snapshot.files();
        root_nodes.into_iter().find(|&node_id| {
            if let Some(entry) = snapshot.get_node(node_id) {
                files
                    .resolve(entry.file)
                    .is_some_and(|p| p.as_ref().ends_with(file_path))
            } else {
                false
            }
        })
    } else {
        root_nodes.into_iter().next()
    };

    let Some(root_node_id) = root_node else {
        bail!(
            "Symbol '{}' not found{}",
            args.symbol,
            args.file_path
                .as_ref()
                .map_or(String::new(), |f| format!(" in file '{f}'"))
        );
    };

    let root_ref = build_node_ref(&snapshot, root_node_id, &workspace_root);

    // Collect hierarchy items
    let direction = args.direction;
    let items = collect_call_hierarchy_items(
        &snapshot,
        root_node_id,
        direction,
        args.max_depth,
        args.max_results,
        &workspace_root,
    );

    let total = items.len();
    let (page_slice, next_page_token) = paginate(&items, &args.pagination);

    let graph_metadata = build_graph_metadata(Some(&workspace_root), Some(&snapshot), None);

    Ok(ToolExecution {
        data: CallHierarchyData {
            root: root_ref,
            direction: direction.as_str().to_string(),
            items: page_slice.to_vec(),
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

/// Collect call hierarchy items for a symbol.
fn collect_call_hierarchy_items(
    snapshot: &GraphSnapshot,
    root_node: NodeId,
    direction: CallHierarchyDirection,
    max_depth: usize,
    max_results: usize,
    workspace_root: &Path,
) -> Vec<CallHierarchyNode> {
    let mut items = Vec::new();
    let mut visited: HashSet<NodeId> = HashSet::new();
    visited.insert(root_node);

    // Get direct callers or callees
    let edges = match direction {
        CallHierarchyDirection::Incoming => snapshot.edges().edges_to(root_node),
        CallHierarchyDirection::Outgoing => snapshot.edges().edges_from(root_node),
    };

    for edge in edges {
        if items.len() >= max_results {
            break;
        }

        // Only process call edges
        if !matches!(edge.kind, EdgeKind::Calls { .. }) {
            continue;
        }

        let related_node = match direction {
            CallHierarchyDirection::Incoming => edge.source,
            CallHierarchyDirection::Outgoing => edge.target,
        };

        if visited.contains(&related_node) {
            continue;
        }
        visited.insert(related_node);

        let node_ref = build_node_ref(snapshot, related_node, workspace_root);

        // Build call ranges from edge spans
        let call_ranges: Vec<RangeData> = edge
            .spans
            .iter()
            .map(|span| RangeData {
                start: PositionData {
                    line: u32::try_from(span.start.line).unwrap_or(u32::MAX),
                    character: u32::try_from(span.start.column).unwrap_or(u32::MAX),
                },
                end: PositionData {
                    line: u32::try_from(span.end.line).unwrap_or(u32::MAX),
                    character: u32::try_from(span.end.column).unwrap_or(u32::MAX),
                },
            })
            .collect();

        // Recursively collect children if depth allows
        let children = if max_depth > 1 {
            collect_call_hierarchy_items(
                snapshot,
                related_node,
                direction,
                max_depth - 1,
                max_results.saturating_sub(items.len()),
                workspace_root,
            )
        } else {
            Vec::new()
        };

        items.push(CallHierarchyNode {
            symbol: node_ref,
            children,
            call_ranges,
        });
    }

    items
}
