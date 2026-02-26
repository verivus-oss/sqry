//! Is node in cycle handler for LSP.
//!
//! Checks if a specific symbol participates in any circular dependency chains.

use std::collections::HashSet;

use anyhow::Result;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeId;

use crate::protocol::{SqryIsNodeInCycleParams, SqryIsNodeInCycleResult};
use crate::session::SessionManager;

/// Execute is-node-in-cycle check.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, inputs are invalid,
/// or the graph is unavailable.
pub fn execute(
    session: &SessionManager,
    params: &SqryIsNodeInCycleParams,
) -> Result<SqryIsNodeInCycleResult> {
    let _root = session.resolve_path(params.path.as_deref())?;
    let symbol = params.symbol.trim();

    if symbol.is_empty() {
        anyhow::bail!("symbol cannot be empty");
    }

    let cycle_type = params.cycle_type.as_deref().unwrap_or("calls");
    let show_cycle = params.show_cycle.unwrap_or(false);

    log::debug!("Checking if '{symbol}' is in cycle (type: {cycle_type})");

    let graph = session
        .graph()?
        .ok_or_else(|| anyhow::anyhow!("No graph available. Run `sqry index` first."))?;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();

    // Find the target node
    let node_id = snapshot
        .find_by_name(symbol)
        .ok_or_else(|| anyhow::anyhow!("Symbol '{symbol}' not found in graph"))?;

    // Try Pass 5 optimization: use precomputed SCC data if available
    let workspace_root = session.root_path();
    let edge_kind_name = match cycle_type {
        "imports" => "imports",
        _ => "calls", // Modules and calls both use call edges
    };

    let (in_cycle, cycle_path) =
        if let Some(scc_data) = try_load_scc_data(&graph, workspace_root, edge_kind_name) {
            // Fast path: O(1) SCC membership check
            detect_cycle_via_scc(&scc_data, node_id, show_cycle)
        } else {
            // Slow path: DFS cycle detection
            find_cycle(&snapshot, node_id, cycle_type, show_cycle)
        };

    // Convert cycle path to qualified names
    let cycle_path = cycle_path.map(|path| {
        path.iter()
            .filter_map(|&id| {
                let entry = snapshot.get_node(id)?;
                entry
                    .qualified_name
                    .and_then(|qid| strings.resolve(qid))
                    .or_else(|| strings.resolve(entry.name))
                    .map(|s| s.to_string())
            })
            .collect()
    });

    Ok(SqryIsNodeInCycleResult {
        symbol: params.symbol.clone(),
        in_cycle,
        cycle_path,
        cycle_type: cycle_type.to_string(),
    })
}

/// Detect whether a node is in a cycle using precomputed SCC data.
fn detect_cycle_via_scc(
    scc_data: &sqry_core::graph::unified::analysis::SccData,
    node_id: NodeId,
    show_cycle: bool,
) -> (bool, Option<Vec<NodeId>>) {
    let in_cycle = if let Some(scc_id) = scc_data.scc_of(node_id) {
        let members = scc_data.scc_members(scc_id);
        let size = members.len();
        let is_self_loop = size == 1 && scc_data.has_self_loop[scc_id as usize];

        // Node is in a cycle if SCC size > 1 or has self-loop
        size > 1 || is_self_loop
    } else {
        false
    };

    // If in cycle and user wants the path, extract it from SCC
    let cycle_path = if in_cycle && show_cycle {
        if let Some(scc_id) = scc_data.scc_of(node_id) {
            let members = scc_data.scc_members(scc_id);
            Some(members.iter().map(|&idx| NodeId::new(idx, 0)).collect())
        } else {
            None
        }
    } else {
        None
    };

    (in_cycle, cycle_path)
}

/// Check if an edge is relevant for the given cycle type.
fn is_relevant_edge(edge_kind: &EdgeKind, cycle_type: &str) -> bool {
    match cycle_type {
        "imports" => matches!(edge_kind, EdgeKind::Imports { .. }),
        "all" => matches!(edge_kind, EdgeKind::Calls { .. } | EdgeKind::Imports { .. }),
        _ => matches!(edge_kind, EdgeKind::Calls { .. }),
    }
}

/// Find if node is in a cycle and optionally return the cycle path.
fn find_cycle(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    start: NodeId,
    cycle_type: &str,
    return_path: bool,
) -> (bool, Option<Vec<NodeId>>) {
    let mut visited = HashSet::new();
    let mut rec_stack = HashSet::new();
    let mut path = Vec::new();

    if dfs_cycle(
        snapshot,
        start,
        start,
        cycle_type,
        &mut visited,
        &mut rec_stack,
        &mut path,
    ) {
        if return_path {
            // Find where the cycle starts
            if let Some(pos) = path.iter().position(|&n| n == start) {
                let cycle: Vec<NodeId> = path[pos..].to_vec();
                return (true, Some(cycle));
            }
        }
        return (true, None);
    }

    (false, None)
}

/// DFS to detect cycle.
fn dfs_cycle(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    current: NodeId,
    target: NodeId,
    cycle_type: &str,
    visited: &mut HashSet<NodeId>,
    rec_stack: &mut HashSet<NodeId>,
    path: &mut Vec<NodeId>,
) -> bool {
    visited.insert(current);
    rec_stack.insert(current);
    path.push(current);

    for edge in snapshot.edges().edges_from(current) {
        if !is_relevant_edge(&edge.kind, cycle_type) {
            continue;
        }

        let neighbor = edge.target;

        // Found cycle back to target
        if neighbor == target && path.len() > 1 {
            path.push(neighbor);
            return true;
        }

        if !visited.contains(&neighbor)
            && dfs_cycle(
                snapshot, neighbor, target, cycle_type, visited, rec_stack, path,
            )
        {
            return true;
        }
    }

    path.pop();
    rec_stack.remove(&current);
    false
}

/// Try to load SCC data for cycle detection.
///
/// Returns None if Pass 5 analyses are not available or validation fails.
fn try_load_scc_data(
    graph: &sqry_core::graph::unified::CodeGraph,
    workspace_root: &std::path::Path,
    edge_kind: &str,
) -> Option<sqry_core::graph::unified::analysis::SccData> {
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(workspace_root);
    sqry_core::graph::unified::analysis::try_load_scc(&storage, &graph.snapshot(), edge_kind)
}
