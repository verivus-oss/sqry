//! Relation query tool execution.
//!
//! This module implements the `relation_query` tool which finds callers,
//! callees, imports, exports, and return types for a given symbol.
//!
//! #
//!
//! ✅ Fully migrated to unified graph architecture. Uses `GraphSnapshot` for:
//! - Symbol lookup via shared resolver buckets
//! - Relation queries via `edges().edges_from()` and `edges().edges_to()`
//! - Full edge metadata (async, argument count, aliases, etc.)

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::materialize::find_nodes_by_name;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::node::NodeKind;

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

/// Returns true if the node kind is a "definition" kind suitable as a call hierarchy root.
fn is_definition_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::Module
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::EnumVariant
            | NodeKind::Macro
            | NodeKind::Variable
            | NodeKind::Constant
            | NodeKind::Component
            | NodeKind::Service
            | NodeKind::Resource
            | NodeKind::Endpoint
            | NodeKind::Test
    )
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

    // Find all matching root symbol nodes
    let root_nodes = find_nodes_by_name(&snapshot, &args.symbol);

    // If file_path is specified, filter to nodes in that file
    let file_filtered: Vec<NodeId> = if let Some(ref file_path) = args.file_path {
        let files = snapshot.files();
        root_nodes
            .into_iter()
            .filter(|&node_id| {
                if let Some(entry) = snapshot.get_node(node_id) {
                    files
                        .resolve(entry.file)
                        .is_some_and(|p| p.as_ref().ends_with(file_path))
                } else {
                    false
                }
            })
            .collect()
    } else {
        root_nodes
    };

    if file_filtered.is_empty() {
        bail!(
            "Symbol '{}' not found{}",
            args.symbol,
            args.file_path
                .as_ref()
                .map_or(String::new(), |f| format!(" in file '{f}'"))
        );
    }

    // Rank candidates: prefer definition-like node kinds (Function, Method, Class, etc.)
    // over reference kinds (CallSite, Import, Export, etc.)
    let definition_nodes: Vec<NodeId> = file_filtered
        .iter()
        .copied()
        .filter(|&node_id| {
            snapshot
                .get_node(node_id)
                .is_some_and(|entry| is_definition_kind(entry.kind))
        })
        .collect();

    // Use definition nodes if available, otherwise fall back to all matches
    let candidates = if definition_nodes.is_empty() {
        &file_filtered
    } else {
        &definition_nodes
    };

    // Use first candidate as the reported root (API contract: single root)
    let root_node_id = candidates[0];
    let root_ref = build_node_ref(&snapshot, root_node_id, &workspace_root);

    // Collect hierarchy items from the single root node
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

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::edge::EdgeKind;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;
    use std::path::{Path, PathBuf};

    fn workspace_root() -> PathBuf {
        PathBuf::from("/tmp/test_workspace")
    }

    fn make_graph_with_call_edge() -> (CodeGraph, NodeId, NodeId) {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();

        let name_a = graph.strings_mut().intern("caller_fn").unwrap();
        let name_b = graph.strings_mut().intern("callee_fn").unwrap();

        let entry_a = NodeEntry::new(NodeKind::Function, name_a, file_id);
        let entry_b = NodeEntry::new(NodeKind::Function, name_b, file_id);

        let node_a = graph.nodes_mut().alloc(entry_a).unwrap();
        let node_b = graph.nodes_mut().alloc(entry_b).unwrap();

        graph
            .indices_mut()
            .add(node_a, NodeKind::Function, name_a, None, file_id);
        graph
            .indices_mut()
            .add(node_b, NodeKind::Function, name_b, None, file_id);

        let call_kind = EdgeKind::Calls {
            argument_count: 2,
            is_async: false,
        };
        graph
            .edges_mut()
            .add_edge(node_a, node_b, call_kind, file_id);

        (graph, node_a, node_b)
    }

    // ===== resolve_workspace_path tests =====

    #[test]
    fn resolve_workspace_path_dot_returns_none() {
        assert!(resolve_workspace_path(".").is_none());
    }

    #[test]
    fn resolve_workspace_path_explicit_returns_some() {
        let result = resolve_workspace_path("/workspace/project");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), PathBuf::from("/workspace/project"));
    }

    #[test]
    fn resolve_workspace_path_empty_string_returns_some() {
        // Empty string is not "." so it's treated as explicit
        let result = resolve_workspace_path("");
        assert!(result.is_some());
    }

    // ===== is_definition_kind tests =====
    //
    // Consolidated from 16 individual tests into two parameterized tests
    // covering all definition vs reference NodeKind variants.

    /// All node kinds that count as definition sites for call-hierarchy roots.
    #[test]
    fn is_definition_kind_returns_true_for_definition_kinds() {
        let definition_kinds = [
            NodeKind::Function,
            NodeKind::Method,
            NodeKind::Class,
            NodeKind::Interface,
            NodeKind::Trait,
            NodeKind::Module,
            NodeKind::Struct,
            NodeKind::Enum,
            NodeKind::EnumVariant,
            NodeKind::Macro,
            NodeKind::Variable,
            NodeKind::Constant,
            NodeKind::Component,
            NodeKind::Service,
            NodeKind::Resource,
            NodeKind::Endpoint,
            NodeKind::Test,
        ];
        for kind in definition_kinds {
            assert!(
                is_definition_kind(kind),
                "Expected {kind:?} to be a definition kind"
            );
        }
    }

    /// Node kinds that are reference-only and must not serve as call-hierarchy roots.
    #[test]
    fn is_definition_kind_returns_false_for_reference_kinds() {
        let reference_kinds = [
            NodeKind::CallSite,
            NodeKind::Import,
            NodeKind::Parameter,
            NodeKind::Property,
        ];
        for kind in reference_kinds {
            assert!(
                !is_definition_kind(kind),
                "Expected {kind:?} to NOT be a definition kind"
            );
        }
    }

    // ===== should_visit tests =====

    #[test]
    fn should_visit_depth_zero_max_one_is_true() {
        // Create a minimal node to get a real NodeId
        let mut g = CodeGraph::new();
        let file_id = g.files_mut().register(Path::new("x.rs")).unwrap();
        let nm = g.strings_mut().intern("f").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let nid = g.nodes_mut().alloc(entry).unwrap();

        let mut vis: std::collections::HashSet<(NodeId, usize)> = std::collections::HashSet::new();
        // depth=0, max_depth=1, first visit
        assert!(should_visit(0, 1, (nid, 0), &mut vis));
        // second visit (already visited)
        assert!(!should_visit(0, 1, (nid, 0), &mut vis));
    }

    #[test]
    fn should_visit_depth_at_or_above_max_returns_false() {
        let mut g = CodeGraph::new();
        let file_id = g.files_mut().register(Path::new("x.rs")).unwrap();
        let nm = g.strings_mut().intern("g").unwrap();
        let entry = NodeEntry::new(NodeKind::Function, nm, file_id);
        let nid = g.nodes_mut().alloc(entry).unwrap();
        let mut vis = std::collections::HashSet::new();
        // depth == max_depth
        assert!(!should_visit(5, 5, (nid, 5), &mut vis));
        // depth > max_depth
        assert!(!should_visit(6, 5, (nid, 6), &mut vis));
    }

    // ===== append_edges_until_limit tests =====

    fn make_edge_data(rel: &str) -> RelationEdgeData {
        RelationEdgeData {
            from: None,
            to: None,
            relation_type: rel.to_string(),
            depth: 1,
            metadata: None,
        }
    }

    #[test]
    fn append_edges_until_limit_below_limit_returns_false() {
        let mut results = Vec::new();
        let edges = vec![make_edge_data("callers"), make_edge_data("callers")];
        let reached = append_edges_until_limit(&mut results, edges, 10);
        assert!(!reached);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn append_edges_until_limit_reaches_limit_returns_true() {
        let mut results = Vec::new();
        let edges: Vec<_> = (0..5).map(|_| make_edge_data("callers")).collect();
        let reached = append_edges_until_limit(&mut results, edges, 3);
        assert!(reached);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn append_edges_until_limit_already_at_limit_returns_true() {
        let mut results: Vec<RelationEdgeData> = (0..3).map(|_| make_edge_data("x")).collect();
        let new_edges = vec![make_edge_data("y")];
        let reached = append_edges_until_limit(&mut results, new_edges, 3);
        assert!(reached);
        assert_eq!(results.len(), 3); // no new edges appended
    }

    // ===== fallback_ref tests =====

    #[test]
    fn fallback_ref_creates_unknown_node() {
        let ws = PathBuf::from("/workspace");
        let r = fallback_ref("mystery", &ws);
        assert_eq!(r.name, "mystery");
        assert_eq!(r.qualified_name, "mystery");
        assert_eq!(r.kind, "unknown");
        assert_eq!(r.language, "unknown");
        assert_eq!(r.range.start.line, 0);
        assert_eq!(r.range.end.line, 0);
        assert!(r.metadata.is_none());
    }

    // ===== build_node_ref tests =====

    #[test]
    fn build_node_ref_for_existing_node() {
        let (graph, node_a, _node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let node_ref = build_node_ref(&snapshot, node_a, &ws);
        assert_eq!(node_ref.name, "caller_fn");
        assert_eq!(node_ref.kind, "function");
    }

    #[test]
    fn build_node_ref_for_invalid_node_returns_fallback() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        // Create a NodeId that doesn't exist
        let fake_node = NodeId::new(9999, 0);
        let node_ref = build_node_ref(&snapshot, fake_node, &ws);
        assert_eq!(node_ref.name, "unknown");
        assert_eq!(node_ref.kind, "unknown");
    }

    #[test]
    fn build_node_ref_maps_class_kind() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm = graph.strings_mut().intern("MyClass").unwrap();
        let entry = NodeEntry::new(NodeKind::Class, nm, file_id);
        let nid = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();
        let r = build_node_ref(&snapshot, nid, &workspace_root());
        assert_eq!(r.kind, "class");
    }

    #[test]
    fn build_node_ref_maps_struct_kind() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm = graph.strings_mut().intern("MyStruct").unwrap();
        let entry = NodeEntry::new(NodeKind::Struct, nm, file_id);
        let nid = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();
        let r = build_node_ref(&snapshot, nid, &workspace_root());
        assert_eq!(r.kind, "struct");
    }

    #[test]
    fn build_node_ref_maps_module_kind() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm = graph.strings_mut().intern("mymod").unwrap();
        let entry = NodeEntry::new(NodeKind::Module, nm, file_id);
        let nid = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();
        let r = build_node_ref(&snapshot, nid, &workspace_root());
        assert_eq!(r.kind, "module");
    }

    #[test]
    fn build_node_ref_maps_method_kind() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm = graph.strings_mut().intern("do_thing").unwrap();
        let entry = NodeEntry::new(NodeKind::Method, nm, file_id);
        let nid = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();
        let r = build_node_ref(&snapshot, nid, &workspace_root());
        assert_eq!(r.kind, "method");
    }

    #[test]
    fn build_node_ref_maps_enum_kind() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm = graph.strings_mut().intern("Color").unwrap();
        let entry = NodeEntry::new(NodeKind::Enum, nm, file_id);
        let nid = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();
        let r = build_node_ref(&snapshot, nid, &workspace_root());
        assert_eq!(r.kind, "enum");
    }

    #[test]
    fn build_node_ref_maps_variable_kind() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm = graph.strings_mut().intern("MY_VAR").unwrap();
        let entry = NodeEntry::new(NodeKind::Variable, nm, file_id);
        let nid = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();
        let r = build_node_ref(&snapshot, nid, &workspace_root());
        assert_eq!(r.kind, "variable");
    }

    #[test]
    fn build_node_ref_maps_constant_kind() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm = graph.strings_mut().intern("MAX").unwrap();
        let entry = NodeEntry::new(NodeKind::Constant, nm, file_id);
        let nid = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();
        let r = build_node_ref(&snapshot, nid, &workspace_root());
        assert_eq!(r.kind, "constant");
    }

    #[test]
    fn build_node_ref_maps_interface_kind() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm = graph.strings_mut().intern("Readable").unwrap();
        let entry = NodeEntry::new(NodeKind::Interface, nm, file_id);
        let nid = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();
        let r = build_node_ref(&snapshot, nid, &workspace_root());
        assert_eq!(r.kind, "interface");
    }

    #[test]
    fn build_node_ref_maps_trait_kind() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm = graph.strings_mut().intern("Display").unwrap();
        let entry = NodeEntry::new(NodeKind::Trait, nm, file_id);
        let nid = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();
        let r = build_node_ref(&snapshot, nid, &workspace_root());
        assert_eq!(r.kind, "trait");
    }

    #[test]
    fn build_node_ref_maps_type_kind() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm = graph.strings_mut().intern("Alias").unwrap();
        let entry = NodeEntry::new(NodeKind::Type, nm, file_id);
        let nid = graph.nodes_mut().alloc(entry).unwrap();
        let snapshot = graph.snapshot();
        let r = build_node_ref(&snapshot, nid, &workspace_root());
        assert_eq!(r.kind, "type");
    }

    /// Verify that the `_` fallback arm of `build_node_ref` maps node kinds not
    /// explicitly listed (e.g. `CallSite`, `Other`) to the string `"function"`.
    #[test]
    fn build_node_ref_fallback_arm_maps_unrecognized_kinds_to_function() {
        let fallback_kinds = [NodeKind::CallSite, NodeKind::Other];
        for kind in fallback_kinds {
            let mut graph = CodeGraph::new();
            let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
            let nm = graph.strings_mut().intern("some_sym").unwrap();
            let entry = NodeEntry::new(kind, nm, file_id);
            let nid = graph.nodes_mut().alloc(entry).unwrap();
            let snapshot = graph.snapshot();
            let r = build_node_ref(&snapshot, nid, &workspace_root());
            assert_eq!(
                r.kind, "function",
                "Expected fallback kind 'function' for NodeKind::{kind:?}"
            );
        }
    }

    // ===== collect_callers / collect_callees tests =====

    #[test]
    fn collect_callees_returns_callee_edge() {
        let (graph, node_a, _node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let callees = collect_callees(&snapshot, node_a, 0, &ws);
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].relation_type, "callees");
        assert_eq!(callees[0].depth, 1);
        // metadata should contain argument_count
        let meta = callees[0].metadata.as_ref().unwrap();
        assert!(meta.get("argument_count").is_some());
        // from is caller_fn, to is callee_fn
        assert_eq!(callees[0].from.as_ref().unwrap().name, "caller_fn");
        assert_eq!(callees[0].to.as_ref().unwrap().name, "callee_fn");
    }

    #[test]
    fn collect_callers_returns_caller_edge() {
        let (graph, _node_a, node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let callers = collect_callers(&snapshot, node_b, 0, &ws);
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].relation_type, "callers");
        // from is caller_fn, to is callee_fn
        assert_eq!(callers[0].from.as_ref().unwrap().name, "caller_fn");
        assert_eq!(callers[0].to.as_ref().unwrap().name, "callee_fn");
    }

    #[test]
    fn collect_callers_empty_when_no_callers() {
        let (graph, node_a, _node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        // node_a is the caller, it should have no callers itself
        let callers = collect_callers(&snapshot, node_a, 0, &ws);
        assert!(callers.is_empty());
    }

    #[test]
    fn collect_callees_empty_when_no_callees() {
        let (graph, _node_a, node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        // node_b is the callee, it should have no callees itself
        let callees = collect_callees(&snapshot, node_b, 0, &ws);
        assert!(callees.is_empty());
    }

    // ===== collect_imports tests =====

    #[test]
    fn collect_imports_returns_import_edges() {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(Path::new("src/main.rs"))
            .unwrap();
        let nm_a = graph.strings_mut().intern("module_a").unwrap();
        let nm_b = graph.strings_mut().intern("module_b").unwrap();

        let entry_a = NodeEntry::new(NodeKind::Module, nm_a, file_id);
        let entry_b = NodeEntry::new(NodeKind::Module, nm_b, file_id);
        let node_a = graph.nodes_mut().alloc(entry_a).unwrap();
        let node_b = graph.nodes_mut().alloc(entry_b).unwrap();
        graph
            .indices_mut()
            .add(node_a, NodeKind::Module, nm_a, None, file_id);
        graph
            .indices_mut()
            .add(node_b, NodeKind::Module, nm_b, None, file_id);

        let import_kind = EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        };
        graph
            .edges_mut()
            .add_edge(node_a, node_b, import_kind, file_id);

        let snapshot = graph.snapshot();
        let ws = workspace_root();
        let imports = collect_imports(&snapshot, node_a, 0, &ws);
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].relation_type, "imports");
        let meta = imports[0].metadata.as_ref().unwrap();
        assert_eq!(meta["is_wildcard"], serde_json::Value::Bool(false));
    }

    #[test]
    fn collect_imports_with_wildcard() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/a.rs")).unwrap();
        let nm_a = graph.strings_mut().intern("a").unwrap();
        let nm_b = graph.strings_mut().intern("b").unwrap();
        let node_a = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Module, nm_a, file_id))
            .unwrap();
        let node_b = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Module, nm_b, file_id))
            .unwrap();
        graph
            .indices_mut()
            .add(node_a, NodeKind::Module, nm_a, None, file_id);
        graph
            .indices_mut()
            .add(node_b, NodeKind::Module, nm_b, None, file_id);

        let import_kind = EdgeKind::Imports {
            alias: None,
            is_wildcard: true,
        };
        graph
            .edges_mut()
            .add_edge(node_a, node_b, import_kind, file_id);

        let snapshot = graph.snapshot();
        let imports = collect_imports(&snapshot, node_a, 0, &workspace_root());
        assert_eq!(imports.len(), 1);
        let meta = imports[0].metadata.as_ref().unwrap();
        assert_eq!(meta["is_wildcard"], serde_json::Value::Bool(true));
    }

    // ===== collect_returns tests =====

    #[test]
    #[ignore = "validates placeholder behavior: collect_returns always returns a stub entry \
                until return-type tracking is implemented in the unified graph edge model"]
    fn collect_returns_returns_entry_when_node_exists() {
        let (graph, node_a, _node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let returns = collect_returns(&snapshot, node_a, 0, &ws);
        assert_eq!(returns.len(), 1);
        assert_eq!(returns[0].relation_type, "returns");
        assert!(returns[0].from.is_some());
        assert!(returns[0].to.is_none());
    }

    #[test]
    fn collect_returns_empty_when_node_not_found() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let fake_node = NodeId::new(9999, 0);
        let returns = collect_returns(&snapshot, fake_node, 0, &ws);
        assert!(returns.is_empty());
    }

    // ===== find_nodes_by_name tests =====

    #[test]
    fn find_nodes_by_name_finds_registered_symbol() {
        let (graph, node_a, _node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();

        let nodes = find_nodes_by_name(&snapshot, "caller_fn");
        assert!(!nodes.is_empty());
        assert!(nodes.contains(&node_a));
    }

    #[test]
    fn find_nodes_by_name_returns_empty_for_nonexistent() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();

        let nodes = find_nodes_by_name(&snapshot, "does_not_exist_xyz");
        assert!(nodes.is_empty());
    }

    // ===== collect_edges_for_relation dispatch tests =====

    #[test]
    fn collect_edges_for_relation_callers_dispatches_correctly() {
        let (graph, _node_a, node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let edges = collect_edges_for_relation(&snapshot, node_b, 0, &ws, RelationType::Callers);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation_type, "callers");
    }

    #[test]
    fn collect_edges_for_relation_callees_dispatches_correctly() {
        let (graph, node_a, _node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let edges = collect_edges_for_relation(&snapshot, node_a, 0, &ws, RelationType::Callees);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation_type, "callees");
    }

    #[test]
    fn collect_edges_for_relation_returns_dispatches_correctly() {
        let (graph, node_a, _) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let edges = collect_edges_for_relation(&snapshot, node_a, 0, &ws, RelationType::Returns);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation_type, "returns");
    }

    // ===== enqueue_next_nodes tests =====

    #[test]
    fn enqueue_next_nodes_callers_adds_callers_to_queue() {
        let (graph, _node_a, node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();

        let mut queue: std::collections::VecDeque<(NodeId, usize)> =
            std::collections::VecDeque::new();
        enqueue_next_nodes(&snapshot, node_b, 0, RelationType::Callers, &mut queue);

        // node_b's caller is node_a
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].1, 1); // depth should be depth+1
    }

    #[test]
    fn enqueue_next_nodes_callees_adds_callees_to_queue() {
        let (graph, node_a, _node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();

        let mut queue: std::collections::VecDeque<(NodeId, usize)> =
            std::collections::VecDeque::new();
        enqueue_next_nodes(&snapshot, node_a, 0, RelationType::Callees, &mut queue);

        // node_a calls node_b
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].1, 1);
    }

    #[test]
    fn enqueue_next_nodes_imports_does_not_enqueue() {
        let (graph, node_a, _) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();

        let mut queue: std::collections::VecDeque<(NodeId, usize)> =
            std::collections::VecDeque::new();
        enqueue_next_nodes(&snapshot, node_a, 0, RelationType::Imports, &mut queue);
        assert!(queue.is_empty());
    }

    #[test]
    fn enqueue_next_nodes_exports_does_not_enqueue() {
        let (graph, node_a, _) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();

        let mut queue: std::collections::VecDeque<(NodeId, usize)> =
            std::collections::VecDeque::new();
        enqueue_next_nodes(&snapshot, node_a, 0, RelationType::Exports, &mut queue);
        assert!(queue.is_empty());
    }

    #[test]
    fn enqueue_next_nodes_returns_does_not_enqueue() {
        let (graph, node_a, _) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();

        let mut queue: std::collections::VecDeque<(NodeId, usize)> =
            std::collections::VecDeque::new();
        enqueue_next_nodes(&snapshot, node_a, 0, RelationType::Returns, &mut queue);
        assert!(queue.is_empty());
    }

    // ===== collect_exports tests =====

    #[test]
    fn collect_exports_returns_export_edges() {
        use sqry_core::graph::unified::ExportKind;

        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm_a = graph.strings_mut().intern("exported_fn").unwrap();
        let nm_b = graph.strings_mut().intern("target_fn").unwrap();
        let node_a = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, nm_a, file_id))
            .unwrap();
        let node_b = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, nm_b, file_id))
            .unwrap();
        graph
            .indices_mut()
            .add(node_a, NodeKind::Function, nm_a, None, file_id);
        graph
            .indices_mut()
            .add(node_b, NodeKind::Function, nm_b, None, file_id);

        let export_kind = EdgeKind::Exports {
            kind: ExportKind::Direct,
            alias: None,
        };
        graph
            .edges_mut()
            .add_edge(node_a, node_b, export_kind, file_id);

        let snapshot = graph.snapshot();
        let ws = workspace_root();
        let exports = collect_exports(&snapshot, node_a, 0, &ws);
        assert_eq!(exports.len(), 1);
        assert_eq!(exports[0].relation_type, "exports");
        let meta = exports[0].metadata.as_ref().unwrap();
        assert!(meta.get("kind").is_some());
    }

    // ===== collect_call_hierarchy_items tests =====

    #[test]
    fn collect_call_hierarchy_items_outgoing_finds_callees() {
        let (graph, node_a, _node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let items = collect_call_hierarchy_items(
            &snapshot,
            node_a,
            CallHierarchyDirection::Outgoing,
            2,
            10,
            &ws,
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].symbol.name, "callee_fn");
    }

    #[test]
    fn collect_call_hierarchy_items_incoming_finds_callers() {
        let (graph, _node_a, node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let items = collect_call_hierarchy_items(
            &snapshot,
            node_b,
            CallHierarchyDirection::Incoming,
            2,
            10,
            &ws,
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].symbol.name, "caller_fn");
    }

    #[test]
    fn collect_call_hierarchy_items_respects_max_results() {
        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();
        let nm_root = graph.strings_mut().intern("root").unwrap();
        let node_root = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, nm_root, file_id))
            .unwrap();
        graph
            .indices_mut()
            .add(node_root, NodeKind::Function, nm_root, None, file_id);

        // Add 5 callees
        let call_kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        for i in 0..5u32 {
            let nm = graph.strings_mut().intern(&format!("callee_{i}")).unwrap();
            let node = graph
                .nodes_mut()
                .alloc(NodeEntry::new(NodeKind::Function, nm, file_id))
                .unwrap();
            graph
                .indices_mut()
                .add(node, NodeKind::Function, nm, None, file_id);
            graph
                .edges_mut()
                .add_edge(node_root, node, call_kind.clone(), file_id);
        }

        let snapshot = graph.snapshot();
        let ws = workspace_root();

        // max_results = 3
        let items = collect_call_hierarchy_items(
            &snapshot,
            node_root,
            CallHierarchyDirection::Outgoing,
            1,
            3,
            &ws,
        );
        assert!(items.len() <= 3);
    }

    #[test]
    fn collect_call_hierarchy_items_empty_when_no_edges() {
        let (graph, _node_a, node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        // node_b has no callees
        let items = collect_call_hierarchy_items(
            &snapshot,
            node_b,
            CallHierarchyDirection::Outgoing,
            2,
            10,
            &ws,
        );
        assert!(items.is_empty());
    }

    #[test]
    fn collect_call_hierarchy_items_depth_one_no_recursion() {
        let (graph, node_a, _node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        // max_depth = 1 means no recursive children
        let items = collect_call_hierarchy_items(
            &snapshot,
            node_a,
            CallHierarchyDirection::Outgoing,
            1,
            10,
            &ws,
        );
        assert_eq!(items.len(), 1);
        assert!(items[0].children.is_empty()); // no recursion when max_depth == 1
    }
}
