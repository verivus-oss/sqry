//! Show dependencies handler for LSP.
//!
//! Shows dependency tree for a file or symbol.

use std::collections::{HashSet, VecDeque};

use anyhow::Result;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::{FileScope, ResolutionMode, SymbolQuery, SymbolResolutionOutcome};

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

/// Collect dependencies via BFS traversal.
///
/// Returns a tuple of (`dependencies`, `truncated_flag`).
fn collect_dependencies_bfs(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    seeds: &[NodeId],
    max_depth: usize,
    max_results: usize,
) -> (Vec<SqryDependency>, bool) {
    let mut dependencies: Vec<SqryDependency> = Vec::new();
    let mut visited = HashSet::new();
    let mut queue: VecDeque<(NodeId, u32)> = seeds.iter().map(|&id| (id, 0u32)).collect();
    let mut truncated = false;

    // Mark seeds as visited
    for seed in seeds {
        visited.insert(*seed);
    }

    while let Some((current_id, depth)) = queue.pop_front() {
        if dependencies.len() >= max_results {
            truncated = true;
            break;
        }

        if depth as usize > max_depth {
            continue;
        }

        // Process outgoing edges (dependencies)
        process_outgoing_deps(
            snapshot,
            current_id,
            depth,
            max_depth,
            &mut visited,
            &mut dependencies,
            &mut queue,
        );
    }

    (dependencies, truncated)
}

/// Process outgoing dependency edges from a single node.
fn process_outgoing_deps(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    current_id: NodeId,
    depth: u32,
    max_depth: usize,
    visited: &mut HashSet<NodeId>,
    dependencies: &mut Vec<SqryDependency>,
    queue: &mut VecDeque<(NodeId, u32)>,
) {
    for edge in snapshot.edges().edges_from(current_id) {
        let Some(dep_type) = classify_dependency_edge(&edge.kind) else {
            continue;
        };

        let target_id = edge.target;
        if !visited.insert(target_id) {
            continue;
        }

        if let Some(dep) = build_dependency_entry(snapshot, target_id, depth, dep_type) {
            dependencies.push(dep);

            if ((depth + 1) as usize) < max_depth {
                queue.push_back((target_id, depth + 1));
            }
        }
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

/// Classify an edge kind as a dependency type, returning `None` if not relevant.
fn classify_dependency_edge(kind: &EdgeKind) -> Option<&'static str> {
    match kind {
        EdgeKind::Calls { .. } => Some("call"),
        EdgeKind::Imports { .. } => Some("import"),
        EdgeKind::References => Some("reference"),
        _ => None,
    }
}

/// Build a `SqryDependency` entry from a target node.
fn build_dependency_entry(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    target_id: NodeId,
    depth: u32,
    dep_type: &str,
) -> Option<SqryDependency> {
    let strings = snapshot.strings();
    let files = snapshot.files();

    let entry = snapshot.get_node(target_id)?;

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let qualified_name =
        crate::conversion::display_entry_qualified_name(entry, strings, files, &name);

    if qualified_name.is_empty() {
        return None;
    }

    let kind = format!("{:?}", entry.kind).to_lowercase();

    let dep_file_path = files
        .resolve(entry.file)
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    Some(SqryDependency {
        name,
        qualified_name,
        kind,
        file_path: dep_file_path,
        depth: depth + 1,
        dependency_type: dep_type.to_string(),
    })
}

/// Find symbol in file by name.
fn find_symbol_in_file(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    target_file: &std::path::Path,
    symbol_name: &str,
) -> Option<NodeId> {
    match snapshot.resolve_symbol(&SymbolQuery {
        symbol: symbol_name,
        file_scope: FileScope::Path(target_file),
        mode: ResolutionMode::Strict,
    }) {
        SymbolResolutionOutcome::Resolved(node_id) => Some(node_id),
        SymbolResolutionOutcome::NotFound
        | SymbolResolutionOutcome::FileNotIndexed
        | SymbolResolutionOutcome::Ambiguous(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::edge::EdgeKind;

    // ── classify_dependency_edge ──────────────────────────────────────────────

    #[test]
    fn calls_edge_classifies_as_call() {
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        assert_eq!(classify_dependency_edge(&kind), Some("call"));
    }

    #[test]
    fn async_calls_edge_classifies_as_call() {
        let kind = EdgeKind::Calls {
            argument_count: 2,
            is_async: true,
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
