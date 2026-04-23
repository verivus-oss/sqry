//! Node location reporting helper with stub-aware fallback.
//!
//! When a node was created as a cross-file stub (e.g., via `ensure_function`
//! with `None` span), its `start_line` is 0. This helper resolves a real
//! location through a cascade of fallback strategies, so MCP tool output
//! never exposes raw line-0 values to users.
//!
//! # Resolution Order
//!
//! 1. **OwnSpan** — the node itself has `start_line > 0`
//! 2. **CanonicalSibling** — another node in `by_qualified_name` with line > 0
//! 3. **IncomingEdgeSpan** — first incoming `Calls`/`References` edge with a span
//! 4. **ExternSymbol** — canonical sibling in an external file (classpath/header)
//! 5. **Fallback** — raw (possibly zero) node span when nothing else resolves

use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::{BidirectionalEdgeStore, EdgeKind};
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::storage::{AuxiliaryIndices, FileRegistry, NodeArena};

use crate::execution::symbol_utils::relative_path_forward_slash;

/// How the location was resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocationResolutionSource {
    /// Node's own span was valid (start_line > 0).
    OwnSpan,
    /// Resolved via a sibling node sharing the same canonical qualified name.
    CanonicalSibling,
    /// Resolved via an incoming `Calls`/`References` edge span.
    IncomingEdgeSpan,
    /// Resolved via a canonical sibling in an external file (classpath/header).
    ExternSymbol,
    /// No resolution succeeded — raw (possibly zero) node span returned.
    Fallback,
}

/// Resolved location for a node, suitable for MCP tool output.
///
/// `file_path`, `language`, and `resolution_source` are surfaced through
/// `build_node_ref` in [`crate::execution::tools::relations`] (DB15) so the
/// MCP `NodeRefData` payload reflects the resolved location even when the
/// node's own span pointed to a different file (cross-file stub →
/// `CanonicalSibling` / `ExternSymbol` resolution).
#[derive(Debug, Clone)]
pub(crate) struct NodeLocation {
    /// Relative file path (forward-slash separated).
    #[allow(dead_code)] // Preserved for parity fixtures and future tool output expansion.
    pub file_path: String,
    /// 1-indexed line number.
    pub line: u32,
    /// 0-indexed column number.
    pub column: u32,
    /// End line (1-indexed), if available.
    pub end_line: u32,
    /// End column (0-indexed), if available.
    pub end_column: u32,
    /// Language of the file containing the node.
    #[allow(dead_code)] // Preserved for parity fixtures and future tool output expansion.
    pub language: Option<String>,
    /// How the location was resolved.
    #[allow(dead_code)] // Kept for tests/debugging around fallback provenance.
    pub resolution_source: LocationResolutionSource,
}

/// Resolve a node's location for reporting, with stub-aware fallback.
///
/// This is the query-layer DRY replacement for the 27 direct `entry.start_line`
/// reads scattered across MCP tool handlers. It never mutates the graph.
///
/// # Arguments
///
/// * `graph` — the `CodeGraph` to query (caller owns the read lock)
/// * `node_id` — the node to resolve
/// * `workspace_root` — for computing relative paths
pub(crate) fn node_location_for_reporting(
    graph: &CodeGraph,
    node_id: NodeId,
    workspace_root: &std::path::Path,
) -> Option<NodeLocation> {
    resolve_location_inner(
        graph.nodes(),
        graph.files(),
        graph.indices(),
        graph.edges(),
        node_id,
        workspace_root,
    )
}

/// Resolve a node's location for reporting from a `GraphSnapshot`.
///
/// This is the snapshot-flavoured overload used by MCP tool handler helpers
/// that only hold a `&GraphSnapshot` (e.g. `build_node_ref_unified`).
/// Semantically identical to [`node_location_for_reporting`].
pub(crate) fn node_location_for_reporting_snapshot(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    workspace_root: &std::path::Path,
) -> Option<NodeLocation> {
    resolve_location_inner(
        snapshot.nodes(),
        snapshot.files(),
        snapshot.indices(),
        snapshot.edges(),
        node_id,
        workspace_root,
    )
}

/// Shared implementation for both `CodeGraph` and `GraphSnapshot` callers.
fn resolve_location_inner(
    nodes: &NodeArena,
    files: &FileRegistry,
    indices: &AuxiliaryIndices,
    edges: &BidirectionalEdgeStore,
    node_id: NodeId,
    workspace_root: &std::path::Path,
) -> Option<NodeLocation> {
    let entry = nodes.get(node_id)?;

    let file_path = files
        .resolve(entry.file)
        .map(|p| relative_path_forward_slash(p.as_ref(), workspace_root))
        .unwrap_or_default();

    let language = files.language_for_file(entry.file).map(|l| l.to_string());

    // Strategy 1: OwnSpan — node itself has a real line
    if entry.start_line > 0 {
        return Some(NodeLocation {
            file_path,
            line: entry.start_line,
            column: entry.start_column,
            end_line: entry.end_line,
            end_column: entry.end_column,
            language,
            resolution_source: LocationResolutionSource::OwnSpan,
        });
    }

    // Strategy 2 + 4: CanonicalSibling / ExternSymbol
    // Look up siblings sharing the same qualified name
    if let Some(qn_id) = entry.qualified_name {
        let siblings = indices.by_qualified_name(qn_id);
        for &sibling_id in siblings {
            if sibling_id == node_id {
                continue;
            }
            if let Some(sibling) = nodes.get(sibling_id)
                && sibling.start_line > 0
            {
                let sibling_file_path = files
                    .resolve(sibling.file)
                    .map(|p| relative_path_forward_slash(p.as_ref(), workspace_root))
                    .unwrap_or_default();

                let sibling_language = files.language_for_file(sibling.file).map(|l| l.to_string());

                let source = if files.is_external(sibling.file) {
                    LocationResolutionSource::ExternSymbol
                } else {
                    LocationResolutionSource::CanonicalSibling
                };

                return Some(NodeLocation {
                    file_path: sibling_file_path,
                    line: sibling.start_line,
                    column: sibling.start_column,
                    end_line: sibling.end_line,
                    end_column: sibling.end_column,
                    language: sibling_language,
                    resolution_source: source,
                });
            }
        }
    }

    // Strategy 2b: Try by_name if qualified_name is None (unqualified bare name)
    if entry.qualified_name.is_none() {
        let name_siblings = indices.by_name(entry.name);
        for &sibling_id in name_siblings {
            if sibling_id == node_id {
                continue;
            }
            if let Some(sibling) = nodes.get(sibling_id)
                && sibling.start_line > 0
                && sibling.kind == entry.kind
            {
                let sibling_file_path = files
                    .resolve(sibling.file)
                    .map(|p| relative_path_forward_slash(p.as_ref(), workspace_root))
                    .unwrap_or_default();

                let sibling_language = files.language_for_file(sibling.file).map(|l| l.to_string());

                let source = if files.is_external(sibling.file) {
                    LocationResolutionSource::ExternSymbol
                } else {
                    LocationResolutionSource::CanonicalSibling
                };

                return Some(NodeLocation {
                    file_path: sibling_file_path,
                    line: sibling.start_line,
                    column: sibling.start_column,
                    end_line: sibling.end_line,
                    end_column: sibling.end_column,
                    language: sibling_language,
                    resolution_source: source,
                });
            }
        }
    }

    // Strategy 3: IncomingEdgeSpan — first incoming Calls/References edge with a span
    let incoming = edges.edges_to(node_id);
    for edge_ref in &incoming {
        if !matches!(edge_ref.kind, EdgeKind::Calls { .. } | EdgeKind::References) {
            continue;
        }
        for span in &edge_ref.spans {
            // Edge spans are 0-indexed (tree-sitter). Normalize to 1-indexed.
            let line = u32::try_from(span.start.line.saturating_add(1)).unwrap_or(u32::MAX);
            let column = u32::try_from(span.start.column).unwrap_or(u32::MAX);
            if line > 0 {
                let edge_file_path = files
                    .resolve(edge_ref.file)
                    .map(|p| relative_path_forward_slash(p.as_ref(), workspace_root))
                    .unwrap_or_else(|| file_path.clone());

                return Some(NodeLocation {
                    file_path: edge_file_path,
                    line,
                    column,
                    end_line: u32::try_from(span.end.line.saturating_add(1)).unwrap_or(u32::MAX),
                    end_column: u32::try_from(span.end.column).unwrap_or(u32::MAX),
                    language: language.clone(),
                    resolution_source: LocationResolutionSource::IncomingEdgeSpan,
                });
            }
        }
    }

    // Strategy 5: Fallback — raw (possibly zero) node span
    Some(NodeLocation {
        file_path,
        line: entry.start_line,
        column: entry.start_column,
        end_line: entry.end_line,
        end_column: entry.end_column,
        language,
        resolution_source: LocationResolutionSource::Fallback,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;

    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::NodeEntry;

    fn make_graph_with_nodes(nodes: Vec<(NodeKind, &str, u32, u32)>) -> (CodeGraph, Vec<NodeId>) {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register(std::path::Path::new("test.rs"))
            .unwrap();
        let mut ids = Vec::new();

        for (kind, name, start_line, start_col) in &nodes {
            let name_id = graph.strings_mut().intern(name).unwrap();
            let qn_id = graph.strings_mut().intern(name).unwrap();
            let entry = NodeEntry::new(*kind, name_id, file_id)
                .with_location(*start_line, *start_col, *start_line + 5, 0)
                .with_qualified_name(qn_id);
            let node_id = graph.nodes_mut().alloc(entry).unwrap();
            ids.push(node_id);
        }

        graph.rebuild_indices();
        (graph, ids)
    }

    #[test]
    fn test_own_span_resolution() {
        let (graph, ids) = make_graph_with_nodes(vec![(NodeKind::Function, "main", 10, 0)]);
        let ws = PathBuf::from("/workspace");
        let loc = node_location_for_reporting(&graph, ids[0], &ws).unwrap();
        assert_eq!(loc.line, 10);
        assert_eq!(loc.resolution_source, LocationResolutionSource::OwnSpan);
    }

    #[test]
    fn test_canonical_sibling_resolution() {
        // Node 0 is a stub (line 0), node 1 has a real span — same qualified name
        let (graph, ids) = make_graph_with_nodes(vec![
            (NodeKind::Function, "kfree", 0, 0),
            (NodeKind::Function, "kfree", 42, 0),
        ]);
        let ws = PathBuf::from("/workspace");
        let loc = node_location_for_reporting(&graph, ids[0], &ws).unwrap();
        assert_eq!(loc.line, 42);
        assert_eq!(
            loc.resolution_source,
            LocationResolutionSource::CanonicalSibling
        );
    }

    #[test]
    fn test_fallback_when_all_siblings_are_stubs() {
        let (graph, ids) = make_graph_with_nodes(vec![(NodeKind::Function, "mystery", 0, 0)]);
        let ws = PathBuf::from("/workspace");
        let loc = node_location_for_reporting(&graph, ids[0], &ws).unwrap();
        assert_eq!(loc.line, 0);
        assert_eq!(loc.resolution_source, LocationResolutionSource::Fallback);
    }

    #[test]
    fn test_no_infinite_recursion_two_stubs() {
        // Two stubs with the same name — neither has line > 0.
        // Must not recurse; should fall through to Fallback.
        let (graph, ids) = make_graph_with_nodes(vec![
            (NodeKind::Function, "phantom", 0, 0),
            (NodeKind::Function, "phantom", 0, 0),
        ]);
        let ws = PathBuf::from("/workspace");
        let loc = node_location_for_reporting(&graph, ids[0], &ws).unwrap();
        assert_eq!(loc.line, 0);
        assert_eq!(loc.resolution_source, LocationResolutionSource::Fallback);
    }

    #[test]
    fn test_invalid_node_returns_none() {
        let (graph, _ids) = make_graph_with_nodes(vec![]);
        let ws = PathBuf::from("/workspace");
        let result = node_location_for_reporting(&graph, NodeId::INVALID, &ws);
        assert!(result.is_none());
    }
}
