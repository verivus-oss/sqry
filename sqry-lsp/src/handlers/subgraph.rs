//! Subgraph extraction handler for LSP.
//!
//! Extracts a focused subgraph around seed symbols.

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Result;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeId;

use crate::protocol::{SqryGraphEdge, SqryGraphNode, SqrySubgraphParams, SqrySubgraphResult};
use crate::session::SessionManager;

/// Default maximum depth for traversal
const DEFAULT_MAX_DEPTH: usize = 2;

/// Default maximum nodes
const DEFAULT_MAX_NODES: usize = 50;

/// Execute subgraph extraction.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, inputs are invalid,
/// or the graph is unavailable.
pub fn execute(
    session: &SessionManager,
    params: &SqrySubgraphParams,
) -> Result<SqrySubgraphResult> {
    let _root = session.resolve_path(params.path.as_deref())?;

    if params.symbols.is_empty() {
        anyhow::bail!("symbols list cannot be empty");
    }

    let max_depth = params.max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
    let max_nodes = params.max_nodes.unwrap_or(DEFAULT_MAX_NODES);
    let include_incoming = params.include_callers.unwrap_or(true);
    let include_outgoing = params.include_callees.unwrap_or(true);
    let include_imports = params.include_imports.unwrap_or(false);

    log::debug!(
        "Extracting subgraph for {:?}, max_depth={}, max_nodes={}",
        params.symbols,
        max_depth,
        max_nodes
    );

    let graph = session
        .graph()?
        .ok_or_else(|| anyhow::anyhow!("No graph available. Run `sqry index` first."))?;

    let snapshot = graph.snapshot();

    // Find seed nodes
    let seeds = find_seed_nodes(&snapshot, &params.symbols);

    if seeds.is_empty() {
        anyhow::bail!("No seed symbols found in graph");
    }

    // BFS to collect subgraph
    let mut nodes: HashMap<NodeId, SqryGraphNode> = HashMap::new();
    let mut edges: Vec<SqryGraphEdge> = Vec::new();
    let mut visited = HashSet::new();
    let mut queue: VecDeque<(NodeId, usize)> = seeds.iter().map(|&id| (id, 0usize)).collect();
    let mut truncated = false;

    while let Some((current_id, depth)) = queue.pop_front() {
        if visited.contains(&current_id) || nodes.len() >= max_nodes {
            if nodes.len() >= max_nodes {
                truncated = true;
            }
            continue;
        }
        visited.insert(current_id);

        let Some(qualified_name) = insert_node_entry(&snapshot, current_id, &mut nodes) else {
            continue;
        };

        if depth >= max_depth {
            continue;
        }

        // Process outgoing edges (callees)
        if include_outgoing {
            process_outgoing_edges(
                &snapshot,
                current_id,
                &qualified_name,
                depth,
                include_imports,
                &visited,
                &mut edges,
                &mut queue,
            );
        }

        // Process incoming edges (callers)
        if include_incoming {
            process_incoming_edges(
                &snapshot,
                current_id,
                &qualified_name,
                depth,
                &visited,
                &mut edges,
                &mut queue,
            );
        }
    }

    let mut node_vec: Vec<SqryGraphNode> = nodes.into_values().collect();
    node_vec.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));

    let total_nodes = node_vec.len();
    let total_edges = edges.len();

    Ok(SqrySubgraphResult {
        nodes: node_vec,
        edges,
        total_nodes,
        total_edges,
        truncated,
    })
}

/// Find seed nodes by name or partial match.
fn find_seed_nodes(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbols: &[String],
) -> Vec<NodeId> {
    let mut seeds: Vec<NodeId> = Vec::new();
    for symbol in symbols {
        if let Some(node_id) = snapshot.find_by_name(symbol) {
            seeds.push(node_id);
        } else {
            let matches = snapshot.nodes_by_symbol(symbol);
            seeds.extend(matches);
        }
    }
    seeds
}

/// Insert a node entry into the nodes map and return its qualified name.
///
/// Returns `None` if the node cannot be found or has an empty qualified name.
fn insert_node_entry(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node_id: NodeId,
    nodes: &mut HashMap<NodeId, SqryGraphNode>,
) -> Option<String> {
    let strings = snapshot.strings();
    let files = snapshot.files();

    let entry = snapshot.get_node(node_id)?;

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let qualified_name = entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| name.clone(), |s| s.to_string());

    if qualified_name.is_empty() {
        return None;
    }

    let kind = format!("{:?}", entry.kind).to_lowercase();

    let language = files
        .language_for_file(entry.file)
        .map_or("unknown".to_string(), |l| {
            l.to_string().to_ascii_lowercase()
        });

    let file_path = files
        .resolve(entry.file)
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    nodes.entry(node_id).or_insert(SqryGraphNode {
        name,
        qualified_name: qualified_name.clone(),
        kind,
        language,
        file_path,
        start_line: entry.start_line,
        end_line: entry.end_line,
    });

    Some(qualified_name)
}

/// Process outgoing edges (callees) from a node, adding edges and enqueuing targets.
fn process_outgoing_edges(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    current_id: NodeId,
    qualified_name: &str,
    depth: usize,
    include_imports: bool,
    visited: &HashSet<NodeId>,
    edges: &mut Vec<SqryGraphEdge>,
    queue: &mut VecDeque<(NodeId, usize)>,
) {
    let strings = snapshot.strings();

    for edge in snapshot.edges().edges_from(current_id) {
        let is_relevant = match &edge.kind {
            EdgeKind::Calls { .. } => true,
            EdgeKind::Imports { .. } if include_imports => true,
            _ => false,
        };

        if !is_relevant {
            continue;
        }

        let target_id = edge.target;
        let Some(target_entry) = snapshot.get_node(target_id) else {
            continue;
        };

        let target_qname = target_entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .map_or_else(String::new, |s| s.to_string());

        if target_qname.is_empty() {
            continue;
        }

        let edge_type = match &edge.kind {
            EdgeKind::Calls { .. } => "call",
            EdgeKind::Imports { .. } => "import",
            _ => "edge",
        };

        edges.push(SqryGraphEdge {
            from: qualified_name.to_string(),
            to: target_qname,
            edge_type: edge_type.to_string(),
            depth: u32::try_from(depth).unwrap_or(u32::MAX),
        });

        if !visited.contains(&target_id) {
            queue.push_back((target_id, depth + 1));
        }
    }
}

/// Process incoming edges (callers) to a node, adding edges and enqueuing callers.
fn process_incoming_edges(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    current_id: NodeId,
    qualified_name: &str,
    depth: usize,
    visited: &HashSet<NodeId>,
    edges: &mut Vec<SqryGraphEdge>,
    queue: &mut VecDeque<(NodeId, usize)>,
) {
    let strings = snapshot.strings();

    let callers = snapshot.get_callers(current_id);
    for caller_id in callers {
        let Some(caller_entry) = snapshot.get_node(caller_id) else {
            continue;
        };

        let caller_qname = caller_entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .map_or_else(String::new, |s| s.to_string());

        if caller_qname.is_empty() {
            continue;
        }

        edges.push(SqryGraphEdge {
            from: caller_qname,
            to: qualified_name.to_string(),
            edge_type: "call".to_string(),
            depth: u32::try_from(depth).unwrap_or(u32::MAX),
        });

        if !visited.contains(&caller_id) {
            queue.push_back((caller_id, depth + 1));
        }
    }
}
