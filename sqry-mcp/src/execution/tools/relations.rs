//! Relation query tool execution.
//!
//! This module implements the `relation_query` tool which finds callers,
//! callees, imports, exports, and return types for a given symbol.
//!
//! # DB15 migration (post-followup)
//!
//! Two different primitives serve the `relation_query` surface:
//!
//! 1. **Name-keyed predicate dispatch** (Phase N "unified surface
//!    contract", planner-canonical) is used by `direct_callers` /
//!    `direct_callees` via [`crate::execution::relation_dispatch`].
//!    Those tools take a user-supplied symbol name as the predicate
//!    value and want callers / callees of "any node with that name".
//! 2. **NodeId-anchored graph traversal** is used by `relation_query`.
//!    `find_nodes_by_name` resolves the user's `args.symbol` to a set
//!    of `start_nodes`, and from that point the operation is to walk
//!    Calls edges touching those specific nodes. Going through sqry-db
//!    for the first hop would re-do the resolution step less
//!    precisely (the segment matcher is stricter than
//!    `find_symbol_candidates`'s suffix matcher) and can broaden the
//!    BFS frontier with unrelated same-named symbols. The DB15
//!    followup switched this path to direct edge enumeration;
//!    `collect_call_relation_via_db` explains the rationale in its
//!    rustdoc.
//!
//! Structural relations (`Imports`, `Exports`, `Returns`) are always
//! `edges_from(start)` walks because they enumerate "what does this node
//! import / export / return", not "which nodes import / export / return
//! X". Routing them through `sqry-db` would silently change the
//! user-visible semantic.
//!
//! Edge metadata (`argument_count`, `is_async`, `alias`, `is_wildcard`,
//! `kind`) is reconstructed from the enumerated edges so the MCP payload
//! contract is preserved.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::edge::kind::TypeOfContext;
use sqry_core::graph::unified::materialize::find_nodes_by_name;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::node::NodeKind;

use crate::engine::{canonicalize_in_workspace, engine_for_workspace};
use crate::tools::{CallHierarchyArgs, CallHierarchyDirection, RelationQueryArgs, RelationType};

use crate::execution::graph_builders::build_graph_metadata;
use crate::execution::location::node_location_for_reporting_snapshot;
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

/// Execute the `relation_query` tool to find symbol relations.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, the unified
/// graph cannot be loaded or auto-built, or the requested symbol does not
/// exist anywhere in the graph.
pub fn execute_relation_query(
    args: &RelationQueryArgs,
) -> Result<ToolExecution<RelationQueryData>> {
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _search_root = canonicalize_in_workspace(&args.path, &workspace_root)?;

    // Require unified graph for relation queries
    let graph = engine.ensure_graph()?;

    let ctx = crate::daemon_adapter::WorkspaceContext {
        workspace_root,
        graph,
        executor: engine.executor_arc(),
    };
    inner::execute_relation_query(&ctx, args)
}

pub(crate) mod inner {
    use super::{
        RelationQueryArgs, RelationQueryData, Result, ToolExecution, build_graph_metadata,
        collect_relation_edges_unified, duration_to_ms, paginate,
    };
    use crate::daemon_adapter::WorkspaceContext;
    use std::sync::Arc;
    use std::time::Instant;

    /// Daemon/SqryServer-shared body for `relation_query`.
    pub(crate) fn execute_relation_query(
        ctx: &WorkspaceContext,
        args: &RelationQueryArgs,
    ) -> Result<ToolExecution<RelationQueryData>> {
        let snapshot = Arc::new(ctx.graph.snapshot());

        let start = Instant::now();

        tracing::debug!(
            symbol = %args.symbol,
            relation = %args.relation.as_str(),
            max_depth = args.max_depth,
            max_results = args.max_results,
            path = %args.path,
            framework = ?args.framework,
            resolved_via_filter = ?args.resolved_via,
            "Executing relation_query tool"
        );
        // Phase β joint-stubs: filter params threaded end-to-end. The
        // canonical evaluation path for `framework` / `resolved_via`
        // lives in the planner pipeline reached via `sqry_query` /
        // `overlay_phase_beta_filters` — see
        // `sqry-db/tests/phase_beta_predicate_evaluation.rs` for
        // predicate-evaluation coverage. This relation-walk executor
        // does not re-evaluate the predicates inline; the args propagate
        // so daemon-side logging / future planner integration can
        // observe them without breaking ABI.
        let _ = (&args.framework, &args.resolved_via);

        let edges =
            collect_relation_edges_unified(&snapshot, &ctx.workspace_root, args, args.max_results)?;

        let total = edges.len();
        let (page_slice, next_page_token) = paginate(&edges, &args.pagination);

        let relations = page_slice.to_vec();

        let graph_metadata = build_graph_metadata(Some(&ctx.workspace_root), Some(&snapshot), None);

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
            workspace_path: crate::execution::symbol_utils::path_to_forward_slash(
                &ctx.workspace_root,
            ),
        })
    }
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

/// Collect relation edges for a symbol using the unified graph.
///
/// Predicate-style relations (`Callers`, `Callees`) route through sqry-db.
/// Structural relations (`Imports`, `Exports`, `Returns`) iterate the start
/// nodes' outgoing edges directly because they enumerate "what does this
/// node import/export/return", not "which nodes import X".
fn collect_relation_edges_unified(
    snapshot: &Arc<GraphSnapshot>,
    workspace_root: &Path,
    args: &RelationQueryArgs,
    max_results: usize,
) -> Result<Vec<RelationEdgeData>> {
    let start_nodes = find_nodes_by_name(snapshot, &args.symbol);
    if start_nodes.is_empty() {
        bail!("Symbol '{}' not found in graph", args.symbol);
    }

    match args.relation {
        RelationType::Callers | RelationType::Callees => Ok(collect_call_relation_via_db(
            snapshot,
            workspace_root,
            &start_nodes,
            &args.symbol,
            args.relation,
            args.max_depth,
            max_results,
        )),
        RelationType::Imports | RelationType::Exports | RelationType::Returns => {
            Ok(collect_structural_relation(
                snapshot,
                workspace_root,
                &start_nodes,
                args.relation,
                max_results,
            ))
        }
    }
}

/// Collect call-style relations (`Callers`/`Callees`) by enumerating Calls
/// edges anchored on each `start_node`, then BFS-expanding via
/// `snapshot.get_callers` / `get_callees` for additional depth.
///
/// # Why not route through sqry-db here?
///
/// `relation_query` resolves a name to `start_nodes` through
/// `find_nodes_by_name` (suffix-aware), and from that point on the
/// operation is fundamentally NodeId-anchored: enumerate the Calls edges
/// touching each `start_node` and walk them. sqry-db's predicate queries
/// are name-keyed and would re-do the resolution step less precisely
/// (the segment matcher is stricter than `find_symbol_candidates`'s
/// suffix matcher). The post-DB15 Codex review caught a multi-hop bug
/// where a stripped-name dispatch leaked unrelated same-named chains
/// into the BFS frontier; the structural fix is to not go through the
/// name-keyed dispatch at all for `relation_query`.
///
/// `direct_callers` / `direct_callees` still route through sqry-db (see
/// [`crate::execution::relation_dispatch`]) because they take a
/// user-supplied name as the predicate value rather than a resolved
/// NodeId set, and sqry-db's segment-aware matching with caching is the
/// right primitive there.
///
/// The Phase N architectural mandate that "transport must not bypass
/// sqry-db" targets bespoke name-keyed predicate dispatch (which DB15
/// already retired), not NodeId-anchored graph traversal.
/// `snapshot.edges().edges_to(node)` is a graph primitive, not
/// transport-owned traversal code.
fn collect_call_relation_via_db(
    snapshot: &Arc<GraphSnapshot>,
    workspace_root: &Path,
    start_nodes: &[NodeId],
    _symbol: &str,
    relation: RelationType,
    max_depth: usize,
    max_results: usize,
) -> Vec<RelationEdgeData> {
    debug_assert!(matches!(
        relation,
        RelationType::Callers | RelationType::Callees
    ));

    let mut results = Vec::new();
    let start_set: HashSet<NodeId> = start_nodes.iter().copied().collect();

    // Depth-1: enumerate Calls edges touching each start node directly.
    // Track which neighbours actually produced an edge so depth-2+ BFS
    // only walks from real depth-1 endpoints.
    let mut depth_one_anchors: Vec<NodeId> = Vec::new();
    let mut depth_one_anchor_set: HashSet<NodeId> = HashSet::new();

    for &start_node in start_nodes {
        if results.len() >= max_results {
            return results;
        }
        let edges = match relation {
            RelationType::Callers => snapshot.edges().edges_to(start_node),
            RelationType::Callees => snapshot.edges().edges_from(start_node),
            _ => unreachable!("guarded by debug_assert above"),
        };
        for edge in edges {
            if results.len() >= max_results {
                return results;
            }
            if !matches!(edge.kind, EdgeKind::Calls { .. }) {
                continue;
            }
            let counterpart = match relation {
                RelationType::Callers => edge.source,
                RelationType::Callees => edge.target,
                _ => unreachable!(),
            };
            let (from_id, to_id) = match relation {
                RelationType::Callers => (counterpart, start_node),
                RelationType::Callees => (start_node, counterpart),
                _ => unreachable!(),
            };
            let from_ref = build_node_ref(snapshot, from_id, workspace_root);
            let to_ref = build_node_ref(snapshot, to_id, workspace_root);
            let metadata = match &edge.kind {
                EdgeKind::Calls {
                    argument_count,
                    is_async,
                    resolved_via,
                } => Some(json!({
                    "argument_count": argument_count,
                    "is_async": is_async,
                    "resolved_via": serde_json::to_value(resolved_via)
                        .unwrap_or(Value::Null),
                })),
                _ => None,
            };
            results.push(RelationEdgeData {
                from: Some(from_ref),
                to: Some(to_ref),
                relation_type: relation.as_str().to_string(),
                depth: 1,
                metadata,
            });
            if depth_one_anchor_set.insert(counterpart) {
                depth_one_anchors.push(counterpart);
            }
        }
    }

    if max_depth <= 1 {
        return results;
    }

    // Depth >1: NodeId-anchored BFS seeded ONLY from depth-1 anchors
    // (counterparts of the start nodes' direct Calls edges). Each anchor
    // becomes the new "start" for the next level.
    let mut visited: HashSet<NodeId> = start_set;
    visited.extend(&depth_one_anchor_set);

    let mut current_frontier: Vec<NodeId> = depth_one_anchors;
    for depth in 1..max_depth {
        if results.len() >= max_results {
            return results;
        }
        let mut next_frontier: Vec<NodeId> = Vec::new();
        for &node in &current_frontier {
            let neighbours = match relation {
                RelationType::Callers => snapshot.get_callers(node),
                RelationType::Callees => snapshot.get_callees(node),
                _ => unreachable!("relation enum guarded by debug_assert above"),
            };
            for next in neighbours {
                if !visited.insert(next) {
                    continue;
                }
                let neighbour_set: HashSet<NodeId> = std::iter::once(node).collect();
                let edges = collect_call_edges_between(
                    snapshot,
                    next,
                    &neighbour_set,
                    relation,
                    depth,
                    workspace_root,
                );
                for edge in edges {
                    if results.len() >= max_results {
                        return results;
                    }
                    results.push(edge);
                }
                next_frontier.push(next);
            }
        }
        if next_frontier.is_empty() {
            break;
        }
        current_frontier = next_frontier;
    }

    results
}

/// For a `(frontier_node, start_set)` pair, look up the actual Calls
/// edges between them and emit one `RelationEdgeData` per edge with full
/// metadata (`argument_count`, `is_async`).
///
/// Direction depends on the relation: `Callers` means the edge goes
/// `frontier -> start`; `Callees` means `start -> frontier`.
fn collect_call_edges_between(
    snapshot: &GraphSnapshot,
    frontier_node: NodeId,
    start_set: &HashSet<NodeId>,
    relation: RelationType,
    depth: usize,
    workspace_root: &Path,
) -> Vec<RelationEdgeData> {
    let mut emitted = Vec::new();
    let edges = match relation {
        RelationType::Callers => snapshot.edges().edges_from(frontier_node),
        RelationType::Callees => snapshot.edges().edges_to(frontier_node),
        _ => unreachable!("only Callers/Callees route here"),
    };
    for edge in edges {
        if !matches!(edge.kind, EdgeKind::Calls { .. }) {
            continue;
        }
        let counterpart = match relation {
            RelationType::Callers => edge.target,
            RelationType::Callees => edge.source,
            _ => unreachable!(),
        };
        if !start_set.contains(&counterpart) {
            continue;
        }
        let (from_id, to_id) = match relation {
            RelationType::Callers => (frontier_node, counterpart),
            RelationType::Callees => (counterpart, frontier_node),
            _ => unreachable!(),
        };
        let from_ref = build_node_ref(snapshot, from_id, workspace_root);
        let to_ref = build_node_ref(snapshot, to_id, workspace_root);
        let metadata = match &edge.kind {
            EdgeKind::Calls {
                argument_count,
                is_async,
                resolved_via,
            } => Some(json!({
                "argument_count": argument_count,
                "is_async": is_async,
                "resolved_via": serde_json::to_value(resolved_via)
                    .unwrap_or(Value::Null),
            })),
            _ => None,
        };
        emitted.push(RelationEdgeData {
            from: Some(from_ref),
            to: Some(to_ref),
            relation_type: relation.as_str().to_string(),
            depth: u32::try_from(depth).unwrap_or(u32::MAX).saturating_add(1),
            metadata,
        });
    }
    emitted
}

/// Collect structural (NodeId-anchored) relations: `Imports`, `Exports`,
/// `Returns`. These enumerate the start nodes' own outgoing edges and are
/// always single-level.
fn collect_structural_relation(
    snapshot: &Arc<GraphSnapshot>,
    workspace_root: &Path,
    start_nodes: &[NodeId],
    relation: RelationType,
    max_results: usize,
) -> Vec<RelationEdgeData> {
    debug_assert!(matches!(
        relation,
        RelationType::Imports | RelationType::Exports | RelationType::Returns
    ));

    let mut results = Vec::new();
    for &start in start_nodes {
        if results.len() >= max_results {
            break;
        }
        let edges = match relation {
            RelationType::Imports => collect_imports(snapshot, start, 0, workspace_root),
            RelationType::Exports => collect_exports(snapshot, start, 0, workspace_root),
            RelationType::Returns => collect_returns(snapshot, start, 0, workspace_root),
            _ => unreachable!("guarded by debug_assert"),
        };
        for edge in edges {
            if results.len() >= max_results {
                break;
            }
            results.push(edge);
        }
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

/// Walks outgoing `TypeOf{Return}` edges from `node` and emits one entry
/// per resolved target type node. Mirrors the planner's `Predicate::Returns`
/// evaluator in `sqry_db::planner::execute::node_returns_type` for
/// cross-engine consistency. See B2 cluster of the BadLiveware Go-batch DAG.
///
/// Returns an empty `Vec` when the node has no Return edges (e.g. void
/// function, constructor, or a language whose plugin does not yet emit
/// `TypeOf{Return}` edges) — never a placeholder stub.
fn collect_returns(
    snapshot: &GraphSnapshot,
    node: NodeId,
    depth: usize,
    workspace_root: &Path,
) -> Vec<RelationEdgeData> {
    let mut results = Vec::new();

    // Bail when the node id is unknown to the snapshot. Mirrors the guard
    // in `collect_imports` / `collect_exports` (those rely on `edges_from`
    // returning empty for unknown nodes; here we additionally surface no
    // entries so the caller sees the same "no relation" semantic).
    if snapshot.get_node(node).is_none() {
        return results;
    }

    for edge in snapshot.edges().edges_from(node) {
        if !matches!(
            edge.kind,
            EdgeKind::TypeOf {
                context: Some(TypeOfContext::Return),
                ..
            }
        ) {
            continue;
        }
        // Skip edges whose target is no longer resolvable (tombstoned by
        // a remap pass or pointing into an unloaded segment).
        if snapshot.get_node(edge.target).is_none() {
            continue;
        }

        let from_ref = build_node_ref(snapshot, node, workspace_root);
        let to_ref = build_node_ref(snapshot, edge.target, workspace_root);

        results.push(RelationEdgeData {
            from: Some(from_ref),
            to: Some(to_ref),
            relation_type: "returns".to_string(),
            depth: depth.try_into().unwrap_or(u32::MAX).saturating_add(1),
            metadata: None,
        });
    }

    results
}

/// Build `NodeRefData` from a unified graph node.
///
/// Uses [`node_location_for_reporting_snapshot`] as the source of truth for
/// `range`, `file_uri`, `language`, and `resolution_source`. When the node
/// is a cross-file stub, the resolved location may live in a different file
/// than `entry.file` (e.g. an `ExternSymbol` resolution into a header or
/// classpath JAR), so file-path / language / range must all come from the
/// resolved location to stay consistent. Falls back to the raw entry when
/// resolution returns `None` (corrupt graph).
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

    let loc = node_location_for_reporting_snapshot(snapshot, node_id, workspace_root);

    // Use the resolved location's file/language when available — the
    // resolved file may differ from `entry.file` when the node was a stub
    // resolved through a sibling or extern symbol.
    let language = loc
        .as_ref()
        .and_then(|l| l.language.clone())
        .or_else(|| files.language_for_file(entry.file).map(|l| l.to_string()))
        .unwrap_or_else(|| "unknown".to_string());

    let file_path = loc
        .as_ref()
        .filter(|l| !l.file_path.is_empty())
        .map(|l| workspace_root.join(&l.file_path))
        .or_else(|| {
            files
                .resolve(entry.file)
                .map(|arc_path| workspace_root.join(arc_path.as_ref()))
        })
        .unwrap_or_default();

    let file_uri = url::Url::from_file_path(&file_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&file_path),
        Into::into,
    );

    let resolution_source = loc.as_ref().map(|l| format!("{:?}", l.resolution_source));

    NodeRefData {
        name,
        qualified_name,
        kind: kind.to_string(),
        language,
        file_uri,
        range: RangeData {
            start: PositionData {
                line: loc.as_ref().map_or(entry.start_line, |l| l.line),
                character: loc.as_ref().map_or(entry.start_column, |l| l.column),
            },
            end: PositionData {
                line: loc.as_ref().map_or(entry.end_line, |l| l.end_line),
                character: loc.as_ref().map_or(entry.end_column, |l| l.end_column),
            },
        },
        metadata: None,
        resolution_source,
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
        resolution_source: None,
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
    #[cfg(test)]
    use sqry_core::graph::unified::edge::ResolvedVia;
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
            resolved_via: ResolvedVia::Direct,
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

    // ===== collect_call_edges_between tests (DB15 migration) =====
    //
    // collect_callers / collect_callees were retired in DB15. The new
    // primitive `collect_call_edges_between` enumerates per-edge metadata
    // for one (frontier_node, start_set) pair after sqry-db identifies the
    // matching frontier set.

    #[test]
    fn collect_call_edges_between_emits_callee_edge_under_callees_relation() {
        let (graph, node_a, node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();
        // Callees relation: the user asked "what does X call?" with X =
        // node_a. sqry-db returns frontier = {node_b} (callees of X). The
        // dispatcher's start_set holds X = {node_a} (the caller). For each
        // frontier node we iterate edges_to(frontier) looking for source
        // in start_set, then emit the (caller -> callee) edge.
        let start_set: HashSet<NodeId> = std::iter::once(node_a).collect();
        let edges = collect_call_edges_between(
            &snapshot,
            node_b,
            &start_set,
            RelationType::Callees,
            0,
            &ws,
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation_type, "callees");
        assert_eq!(edges[0].depth, 1);
        // Output keeps the physical edge direction: from=caller,
        // to=callee.
        assert_eq!(edges[0].from.as_ref().unwrap().name, "caller_fn");
        assert_eq!(edges[0].to.as_ref().unwrap().name, "callee_fn");
        let meta = edges[0].metadata.as_ref().unwrap();
        assert!(meta.get("argument_count").is_some());
    }

    #[test]
    fn collect_call_edges_between_emits_caller_edge_under_callers_relation() {
        let (graph, node_a, node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();
        // Callers relation: the user asked "who calls X?" with X = node_b.
        // sqry-db returns frontier = {node_a} (callers of X). The
        // dispatcher's start_set holds X = {node_b}. For each frontier
        // node we iterate edges_from(frontier) looking for target in
        // start_set, then emit the (caller -> callee) edge.
        let start_set: HashSet<NodeId> = std::iter::once(node_b).collect();
        let edges = collect_call_edges_between(
            &snapshot,
            node_a,
            &start_set,
            RelationType::Callers,
            0,
            &ws,
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].relation_type, "callers");
        assert_eq!(edges[0].from.as_ref().unwrap().name, "caller_fn");
        assert_eq!(edges[0].to.as_ref().unwrap().name, "callee_fn");
        let meta = edges[0].metadata.as_ref().unwrap();
        assert!(meta.get("is_async").is_some());
    }

    #[test]
    fn collect_call_edges_between_empty_when_frontier_has_no_call_edges() {
        let (graph, _node_a, node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();
        // Use node_b (the callee) as frontier under Callers — node_b has
        // no outgoing Calls edges so the result must be empty.
        let start_set: HashSet<NodeId> = HashSet::new();
        let edges = collect_call_edges_between(
            &snapshot,
            node_b,
            &start_set,
            RelationType::Callers,
            0,
            &ws,
        );
        assert!(edges.is_empty());
    }

    #[test]
    fn collect_call_edges_between_skips_edges_outside_start_set() {
        let (graph, node_a, _node_b) = make_graph_with_call_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();
        // Frontier node_a has an outgoing Calls edge to node_b, but our
        // start_set is empty — so the edge does not match.
        let start_set: HashSet<NodeId> = HashSet::new();
        let edges = collect_call_edges_between(
            &snapshot,
            node_a,
            &start_set,
            RelationType::Callers,
            0,
            &ws,
        );
        assert!(edges.is_empty());
    }

    // ===== collect_call_relation_via_db multi-hop regression =====

    /// Codex's post-DB15 review found that depth-2+ `relation_query`
    /// could leak chains belonging to unrelated same-named symbols when
    /// the start set was narrower than the BFS frontier. The structural
    /// fix is to enumerate Calls edges directly from the resolved
    /// `start_nodes` (NodeId-anchored) at depth 1, then BFS only from
    /// the counterparts of those edges. This unit test constructs a
    /// graph that distinguishes `alpha::helper` from `beta::helper`
    /// (the Rust language plugin doesn't surface this distinction so a
    /// Rust integration fixture cannot exercise the bug; an in-memory
    /// graph can).
    ///
    /// Layout:
    ///
    /// ```text
    /// alpha::helper, beta::helper
    /// alpha::caller_a -> alpha::helper
    /// beta::caller_b  -> beta::helper
    /// alpha::root_a   -> alpha::caller_a
    /// beta::root_b    -> beta::caller_b
    /// ```
    ///
    /// Querying with `start_nodes = [alpha::helper]` and
    /// `max_depth = 2` must emit ONLY the alpha chain:
    ///   alpha::caller_a -> alpha::helper (depth 1)
    ///   alpha::root_a   -> alpha::caller_a (depth 2)
    ///
    /// The pre-fix dispatch would seed depth-2 BFS with both caller_a
    /// AND caller_b (broad sqry-db result), then emit
    /// `beta::root_b -> beta::caller_b` at depth 2 — leaking the
    /// unrelated chain.
    #[test]
    fn collect_call_relation_via_db_does_not_leak_unrelated_same_named_chains() {
        use std::sync::Arc;

        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("lib.rs")).unwrap();

        // Distinct qualified names, identical simple name (`helper`).
        let mk_node = |g: &mut CodeGraph, qname: &str, simple: &str| -> NodeId {
            let qn = g.strings_mut().intern(qname).unwrap();
            let nm = g.strings_mut().intern(simple).unwrap();
            g.nodes_mut()
                .alloc(NodeEntry::new(NodeKind::Function, nm, file_id).with_qualified_name(qn))
                .unwrap()
        };
        let alpha_helper = mk_node(&mut graph, "alpha::helper", "helper");
        let beta_helper = mk_node(&mut graph, "beta::helper", "helper");
        let alpha_caller = mk_node(&mut graph, "alpha::caller_a", "caller_a");
        let beta_caller = mk_node(&mut graph, "beta::caller_b", "caller_b");
        let alpha_root = mk_node(&mut graph, "alpha::root_a", "root_a");
        let beta_root = mk_node(&mut graph, "beta::root_b", "root_b");

        let calls = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        graph
            .edges_mut()
            .add_edge(alpha_caller, alpha_helper, calls.clone(), file_id);
        graph
            .edges_mut()
            .add_edge(beta_caller, beta_helper, calls.clone(), file_id);
        graph
            .edges_mut()
            .add_edge(alpha_root, alpha_caller, calls.clone(), file_id);
        graph
            .edges_mut()
            .add_edge(beta_root, beta_caller, calls, file_id);

        let snapshot = Arc::new(graph.snapshot());
        let start_nodes = vec![alpha_helper];

        let edges = collect_call_relation_via_db(
            &snapshot,
            &workspace_root(),
            &start_nodes,
            "alpha::helper",
            RelationType::Callers,
            2,
            100,
        );

        // Every emitted edge must touch only the alpha chain.
        for edge in &edges {
            let from_qn = edge
                .from
                .as_ref()
                .map(|f| f.qualified_name.as_str())
                .unwrap_or("");
            let to_qn = edge
                .to
                .as_ref()
                .map(|f| f.qualified_name.as_str())
                .unwrap_or("");
            assert!(
                !from_qn.contains("beta") && !to_qn.contains("beta"),
                "depth-2 BFS leaked a beta chain: from={from_qn:?} \
                 to={to_qn:?} (full edges: {edges:#?})"
            );
        }

        // Positive: alpha::caller_a -> alpha::helper at depth 1.
        let depth1 = edges
            .iter()
            .any(|e| e.depth == 1 && e.from.as_ref().is_some_and(|f| f.name == "caller_a"));
        assert!(
            depth1,
            "expected alpha::caller_a -> alpha::helper at depth 1, got {edges:#?}"
        );

        // Positive: alpha::root_a -> alpha::caller_a at depth 2.
        let depth2 = edges
            .iter()
            .any(|e| e.depth >= 2 && e.from.as_ref().is_some_and(|f| f.name == "root_a"));
        assert!(
            depth2,
            "expected alpha::root_a -> alpha::caller_a at depth >= 2, got {edges:#?}"
        );

        // Sanity: beta nodes were defined and have edges in the graph
        // (so their absence from the result is the BFS filter, not a
        // missing fixture).
        assert!(graph_has_edge(&snapshot, beta_root, beta_caller));
        assert!(graph_has_edge(&snapshot, beta_caller, beta_helper));
    }

    fn graph_has_edge(
        snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
        src: NodeId,
        tgt: NodeId,
    ) -> bool {
        snapshot
            .edges()
            .edges_from(src)
            .iter()
            .any(|edge| edge.target == tgt)
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

    /// Build a small graph with two functions and one type. `caller_fn`
    /// has a `TypeOf{Return}` edge into `ret_type`; `other_fn` has no
    /// outgoing TypeOf{Return} edges and exists only to prove the helper
    /// returns an empty `Vec` (not a placeholder stub) for nodes that
    /// genuinely lack a return-type edge.
    fn make_graph_with_return_type_edge() -> (CodeGraph, NodeId, NodeId, NodeId) {
        use sqry_core::graph::unified::edge::kind::TypeOfContext;

        let mut graph = CodeGraph::new();
        let file_id = graph.files_mut().register(Path::new("src/lib.rs")).unwrap();

        let caller_name = graph.strings_mut().intern("caller_fn").unwrap();
        let other_name = graph.strings_mut().intern("other_fn").unwrap();
        let ret_name = graph.strings_mut().intern("ret_type").unwrap();

        let caller = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, caller_name, file_id))
            .unwrap();
        let other = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, other_name, file_id))
            .unwrap();
        let ret_type_node = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Type, ret_name, file_id))
            .unwrap();

        graph
            .indices_mut()
            .add(caller, NodeKind::Function, caller_name, None, file_id);
        graph
            .indices_mut()
            .add(other, NodeKind::Function, other_name, None, file_id);
        graph
            .indices_mut()
            .add(ret_type_node, NodeKind::Type, ret_name, None, file_id);

        graph.edges_mut().add_edge(
            caller,
            ret_type_node,
            EdgeKind::TypeOf {
                context: Some(TypeOfContext::Return),
                index: None,
                name: None,
            },
            file_id,
        );

        (graph, caller, other, ret_type_node)
    }

    #[test]
    fn collect_returns_emits_edge_when_typeof_return_present() {
        let (graph, caller, _other, ret_type_node) = make_graph_with_return_type_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let returns = collect_returns(&snapshot, caller, 0, &ws);
        assert_eq!(
            returns.len(),
            1,
            "expected exactly one TypeOf{{Return}} edge entry"
        );
        let edge = &returns[0];
        assert_eq!(edge.relation_type, "returns");
        let from = edge
            .from
            .as_ref()
            .expect("from must be populated by collect_returns");
        let to = edge
            .to
            .as_ref()
            .expect("to must be populated post-fix (no more placeholder stub)");
        assert_eq!(from.name, "caller_fn");
        assert_eq!(to.name, "ret_type");
        // The post-fix shape carries no `note` metadata — the placeholder
        // is gone and the only structured payload would be edge-level
        // metadata that TypeOf{Return} does not carry today.
        assert!(
            edge.metadata.is_none(),
            "metadata must be None after migrating to real edge walk; got {:?}",
            edge.metadata
        );
        // Sanity: snapshot resolution returned a real node for the target.
        let resolved_target_name = snapshot
            .get_node(ret_type_node)
            .and_then(|e| snapshot.strings().resolve(e.name).map(|s| s.to_string()));
        assert_eq!(resolved_target_name.as_deref(), Some("ret_type"));
    }

    #[test]
    fn collect_returns_empty_for_node_without_return_edge() {
        let (graph, _caller, other, _ret_type_node) = make_graph_with_return_type_edge();
        let snapshot = graph.snapshot();
        let ws = workspace_root();

        let returns = collect_returns(&snapshot, other, 0, &ws);
        assert!(
            returns.is_empty(),
            "node with no TypeOf{{Return}} edges must yield an empty Vec, not a stub; got {returns:?}"
        );
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

    // ===== collect_structural_relation dispatch tests =====
    //
    // The DB15 migration retired the BFS-style `collect_edges_for_relation`
    // and `enqueue_next_nodes` in favour of two narrower helpers:
    // `collect_call_relation_via_db` (Callers/Callees, sqry-db-routed) and
    // `collect_structural_relation` (Imports/Exports/Returns, NodeId-anchored
    // edge enumeration). The Callers/Callees path is exercised by
    // `collect_call_edges_between_*` above; the structural path is
    // exercised by the per-relation `collect_*` tests below
    // (`collect_imports_*`, `collect_exports_*`, `collect_returns_*`).

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
            resolved_via: ResolvedVia::Direct,
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
