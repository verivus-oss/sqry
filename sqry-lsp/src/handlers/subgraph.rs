//! Subgraph extraction handler for LSP.
//!
//! Extracts a focused subgraph around seed symbols.

use anyhow::Result;
use sqry_core::graph::unified::traversal::EdgeClassification;
use sqry_core::graph::unified::{
    EdgeFilter, TraversalConfig, TraversalDirection, TraversalLimits, traverse,
};

use crate::handlers::graph_common::collect_symbol_seeds;
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
    let seeds = collect_symbol_seeds(&snapshot, &params.symbols);

    if seeds.is_empty() {
        anyhow::bail!("No seed symbols found in graph");
    }

    // Select traversal direction based on flags
    #[allow(
        clippy::match_same_arms,
        reason = "(true,false) and (false,false) both yield Outgoing but for distinct semantic reasons"
    )]
    let direction = match (include_outgoing, include_incoming) {
        (true, true) => TraversalDirection::Both,
        (true, false) => TraversalDirection::Outgoing,
        (false, true) => TraversalDirection::Incoming,
        // Both false: default to outgoing to still discover the seed
        (false, false) => TraversalDirection::Outgoing,
    };

    let config = TraversalConfig {
        direction,
        edge_filter: EdgeFilter {
            include_calls: true,
            include_imports,
            include_references: false,
            include_inheritance: false,
            include_structural: false,
            include_type_edges: false,
            include_database: false,
            include_service: false,
            cross_boundary: None,
        },
        limits: TraversalLimits {
            max_depth: u32::try_from(max_depth).unwrap_or(u32::MAX),
            max_nodes: Some(max_nodes),
            max_edges: None,
            max_paths: None,
        },
    };

    let result = traverse(&snapshot, &seeds, &config, None);
    let truncated = result.metadata.truncation.is_some();

    // Convert MaterializedNode → SqryGraphNode
    let mut node_vec: Vec<SqryGraphNode> = result
        .nodes
        .iter()
        .map(|mat| SqryGraphNode {
            name: mat.name.clone(),
            qualified_name: mat.qualified_name.clone(),
            kind: mat.kind.clone(),
            language: mat.language.clone(),
            file_path: mat.file_path.clone(),
            start_line: mat.start_line,
            end_line: mat.end_line,
        })
        .collect();
    node_vec.sort_by(|a, b| a.qualified_name.cmp(&b.qualified_name));

    // Convert MaterializedEdge → SqryGraphEdge
    let edges: Vec<SqryGraphEdge> = result
        .edges
        .iter()
        .map(|mat_edge| {
            let from = result.nodes[mat_edge.source_idx].qualified_name.clone();
            let to = result.nodes[mat_edge.target_idx].qualified_name.clone();
            let edge_type = classify_edge_type(mat_edge.classification);
            SqryGraphEdge {
                from,
                to,
                edge_type: edge_type.to_string(),
                depth: mat_edge.depth,
            }
        })
        .collect();

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

/// Map an `EdgeClassification` to a human-readable edge type string.
fn classify_edge_type(classification: EdgeClassification) -> &'static str {
    match classification {
        EdgeClassification::Call { .. } => "call",
        EdgeClassification::Import { .. } => "import",
        EdgeClassification::Export { .. } => "export",
        EdgeClassification::Reference => "reference",
        EdgeClassification::Inherits => "inherits",
        EdgeClassification::Implements => "implements",
        EdgeClassification::Contains => "contains",
        EdgeClassification::Defines => "defines",
        EdgeClassification::TypeOf => "type_of",
        EdgeClassification::DatabaseAccess => "database",
        EdgeClassification::ServiceInteraction => "service",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── DEFAULT_* constants ───────────────────────────────────────────────────

    #[test]
    fn default_max_depth_is_two() {
        assert_eq!(DEFAULT_MAX_DEPTH, 2);
    }

    #[test]
    fn default_max_nodes_is_50() {
        assert_eq!(DEFAULT_MAX_NODES, 50);
    }
}
