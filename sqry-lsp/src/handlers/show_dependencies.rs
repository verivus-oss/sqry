//! Show dependencies handler for LSP.
//!
//! Shows dependency tree for a file or symbol.

use anyhow::Result;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::traversal::EdgeClassification;
use sqry_core::graph::unified::{
    EdgeFilter, FileScope, ResolutionMode, SymbolQuery, SymbolResolutionOutcome, TraversalConfig,
    TraversalDirection, TraversalLimits, traverse,
};

use crate::protocol::{SqryDependency, SqryShowDependenciesParams, SqryShowDependenciesResult};
use crate::session::SessionManager;

/// Default maximum depth
const DEFAULT_MAX_DEPTH: usize = 2;

/// Default maximum results
const DEFAULT_MAX_RESULTS: usize = 500;

/// Execute show dependencies.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, inputs are invalid,
/// or the graph is unavailable.
pub fn execute(
    session: &SessionManager,
    params: &SqryShowDependenciesParams,
) -> Result<SqryShowDependenciesResult> {
    let root = session.resolve_path(params.path.as_deref())?;
    let file_path = params.file_path.trim();

    if file_path.is_empty() {
        anyhow::bail!("file_path cannot be empty");
    }

    let max_depth = params.max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
    let max_results = params.max_results.unwrap_or(DEFAULT_MAX_RESULTS);

    log::debug!(
        "Showing dependencies for '{}', symbol={:?}, max_depth={}",
        file_path,
        params.symbol_name,
        max_depth
    );

    let graph = session
        .graph()?
        .ok_or_else(|| anyhow::anyhow!("No graph available. Run `sqry index` first."))?;

    let snapshot = graph.snapshot();

    // Find seed nodes
    let seeds = find_seed_nodes(&snapshot, &root, file_path, params.symbol_name.as_deref());

    if seeds.is_empty() {
        anyhow::bail!("No symbols found in '{file_path}'");
    }

    let root_name = params
        .symbol_name
        .as_deref()
        .unwrap_or(file_path)
        .to_string();

    // BFS to collect dependencies
    let (mut dependencies, truncated) =
        collect_dependencies_bfs(&snapshot, &seeds, max_depth, max_results);

    // Sort by depth then name
    dependencies.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.name.cmp(&b.name)));

    let total = dependencies.len();

    Ok(SqryShowDependenciesResult {
        root: root_name,
        dependencies,
        total,
        truncated,
    })
}

/// Collect dependencies via BFS traversal using the kernel.
///
/// Returns a tuple of (`dependencies`, `truncated_flag`).
fn collect_dependencies_bfs(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    seeds: &[NodeId],
    max_depth: usize,
    max_results: usize,
) -> (Vec<SqryDependency>, bool) {
    let config = TraversalConfig {
        direction: TraversalDirection::Outgoing,
        edge_filter: EdgeFilter::dependency_edges(),
        limits: TraversalLimits {
            max_depth: u32::try_from(max_depth).unwrap_or(u32::MAX),
            max_nodes: Some(max_results),
            max_edges: None,
            max_paths: None,
        },
    };

    let result = traverse(snapshot, seeds, &config, None);
    let truncated = result.metadata.truncation.is_some();

    // Build dependencies from edges — each edge's target node becomes a dependency
    // Seeds are excluded (they are the roots, not dependencies).
    let seed_set: std::collections::HashSet<NodeId> = seeds.iter().copied().collect();
    let mut seen_targets = std::collections::HashSet::new();

    let dependencies: Vec<SqryDependency> = result
        .edges
        .iter()
        .filter_map(|mat_edge| {
            let target_node = &result.nodes[mat_edge.target_idx];
            // Skip edges pointing back to seeds
            if seed_set.contains(&target_node.node_id) {
                return None;
            }
            // Dedup by target node
            if !seen_targets.insert(target_node.node_id) {
                return None;
            }
            let dep_type = classify_edge_classification(mat_edge.classification);
            Some(SqryDependency {
                name: target_node.name.clone(),
                qualified_name: target_node.qualified_name.clone(),
                kind: target_node.kind.clone(),
                file_path: target_node.file_path.clone(),
                depth: mat_edge.depth,
                dependency_type: dep_type.to_string(),
            })
        })
        .collect();

    (dependencies, truncated)
}

/// Map an `EdgeClassification` to a dependency type string.
#[allow(
    clippy::match_same_arms,
    reason = "Reference is explicitly mapped for documentation clarity; wildcard is a fallback"
)]
fn classify_edge_classification(classification: EdgeClassification) -> &'static str {
    match classification {
        EdgeClassification::Call { .. } => "call",
        EdgeClassification::Import { .. } => "import",
        #[allow(clippy::match_same_arms)] // Dependency direction arms intentionally separate
        EdgeClassification::Export { .. } => "export",
        EdgeClassification::Reference => "reference",
        EdgeClassification::Inherits => "inherits",
        EdgeClassification::Implements => "implements",
        _ => "reference",
    }
}

/// Find seed nodes from a file path and optional symbol name.
fn find_seed_nodes(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    root: &std::path::Path,
    file_path: &str,
    symbol_name: Option<&str>,
) -> Vec<NodeId> {
    let files = snapshot.files();
    let target_file = root.join(file_path);
    let target_relative = target_file.strip_prefix(root).unwrap_or(&target_file);

    if let Some(symbol_name) = symbol_name {
        find_symbol_in_file(snapshot, target_relative, symbol_name)
            .map(|id| vec![id])
            .unwrap_or_default()
    } else {
        snapshot
            .iter_nodes()
            .filter_map(|(node_id, entry)| {
                // Gate 0d iter-2 fix: skip unified losers. See
                // `NodeEntry::is_unified_loser`.
                if entry.is_unified_loser() {
                    return None;
                }
                let node_file = files.resolve(entry.file)?;
                if node_file.as_ref() == target_relative {
                    Some(node_id)
                } else {
                    None
                }
            })
            .collect()
    }
}

/// Find symbol in file by name.
fn find_symbol_in_file(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    target_file: &std::path::Path,
    symbol_name: &str,
) -> Option<NodeId> {
    let witness = snapshot.resolve_symbol_with_witness(&SymbolQuery {
        symbol: symbol_name,
        file_scope: FileScope::Path(target_file),
        mode: ResolutionMode::Strict,
    });
    match witness.outcome {
        SymbolResolutionOutcome::Resolved(node_id) => Some(node_id),
        SymbolResolutionOutcome::NotFound
        | SymbolResolutionOutcome::FileNotIndexed
        | SymbolResolutionOutcome::Ambiguous(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::edge::{EdgeKind, ResolvedVia};

    /// Classify an edge kind as a dependency type, returning `None` if not relevant.
    fn classify_dependency_edge(kind: &EdgeKind) -> Option<&'static str> {
        match kind {
            EdgeKind::Calls { .. } => Some("call"),
            EdgeKind::Imports { .. } => Some("import"),
            EdgeKind::References => Some("reference"),
            _ => None,
        }
    }

    // ── classify_dependency_edge ──────────────────────────────────────────────

    #[test]
    fn calls_edge_classifies_as_call() {
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        assert_eq!(classify_dependency_edge(&kind), Some("call"));
    }

    #[test]
    fn async_calls_edge_classifies_as_call() {
        let kind = EdgeKind::Calls {
            argument_count: 2,
            is_async: true,
            resolved_via: ResolvedVia::Direct,
        };
        assert_eq!(classify_dependency_edge(&kind), Some("call"));
    }

    #[test]
    fn imports_edge_classifies_as_import() {
        let kind = EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        };
        assert_eq!(classify_dependency_edge(&kind), Some("import"));
    }

    #[test]
    fn wildcard_imports_edge_classifies_as_import() {
        let kind = EdgeKind::Imports {
            alias: None,
            is_wildcard: true,
        };
        assert_eq!(classify_dependency_edge(&kind), Some("import"));
    }

    #[test]
    fn references_edge_classifies_as_reference() {
        assert_eq!(
            classify_dependency_edge(&EdgeKind::References),
            Some("reference")
        );
    }

    #[test]
    fn defines_edge_returns_none() {
        assert_eq!(classify_dependency_edge(&EdgeKind::Defines), None);
    }

    #[test]
    fn contains_edge_returns_none() {
        assert_eq!(classify_dependency_edge(&EdgeKind::Contains), None);
    }

    #[test]
    fn inherits_edge_returns_none() {
        assert_eq!(classify_dependency_edge(&EdgeKind::Inherits), None);
    }

    // ── DEFAULT_* constants ───────────────────────────────────────────────────

    #[test]
    fn default_max_depth_is_two() {
        assert_eq!(DEFAULT_MAX_DEPTH, 2);
    }

    #[test]
    fn default_max_results_is_500() {
        assert_eq!(DEFAULT_MAX_RESULTS, 500);
    }
}
