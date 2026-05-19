//! Dependency impact handler for LSP.
//!
//! Analyzes what symbols would be affected if a given symbol changes.

use std::collections::HashSet;

use anyhow::Result;
use sqry_core::graph::unified::{
    EdgeFilter, FileScope, ResolutionMode, SymbolQuery, SymbolResolutionOutcome, TraversalConfig,
    TraversalDirection, TraversalLimits, traverse,
};

use crate::protocol::{SqryAffectedSymbol, SqryDependencyImpactParams, SqryDependencyImpactResult};
use crate::session::SessionManager;

/// Default maximum depth for dependency traversal
const DEFAULT_MAX_DEPTH: usize = 3;

/// Default maximum results
const DEFAULT_MAX_RESULTS: usize = 500;

/// Execute dependency impact analysis.
///
/// Uses BFS traversal on call/import edges to find symbols that depend on the target.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, the graph is
/// unavailable, or the target symbol cannot be found.
pub fn execute(
    session: &SessionManager,
    params: &SqryDependencyImpactParams,
) -> Result<SqryDependencyImpactResult> {
    let root = session.resolve_path(params.path.as_deref())?;
    let symbol = params.symbol.trim();

    if symbol.is_empty() {
        anyhow::bail!("symbol cannot be empty");
    }

    let max_depth = params.max_depth.unwrap_or(DEFAULT_MAX_DEPTH);
    let include_indirect = params.include_indirect.unwrap_or(true);

    log::debug!(
        "Executing dependency impact: symbol='{}', max_depth={}, root={}",
        symbol,
        max_depth,
        root.display()
    );

    // Get graph snapshot
    let graph = session
        .graph()?
        .ok_or_else(|| anyhow::anyhow!("No graph available. Run `sqry index` first."))?;

    let snapshot = graph.snapshot();

    // Find the target symbol
    let witness = snapshot.resolve_symbol_with_witness(&SymbolQuery {
        symbol,
        file_scope: FileScope::Any,
        mode: ResolutionMode::Strict,
    });
    let target_node_id = match witness.outcome {
        SymbolResolutionOutcome::Resolved(node_id) => node_id,
        SymbolResolutionOutcome::NotFound | SymbolResolutionOutcome::FileNotIndexed => {
            anyhow::bail!("Symbol '{symbol}' not found in graph.")
        }
        SymbolResolutionOutcome::Ambiguous(candidates) => {
            anyhow::bail!(
                "Symbol '{symbol}' is ambiguous in graph ({} candidates). Use a canonical qualified name.",
                candidates.len()
            )
        }
    };

    // BFS traversal to find all impacted symbols
    let (mut affected, affected_files) = collect_callers_bfs(
        &snapshot,
        target_node_id,
        max_depth,
        include_indirect,
        DEFAULT_MAX_RESULTS,
    );

    let total = affected.len();
    let truncated = total >= DEFAULT_MAX_RESULTS;

    // Sort by depth (direct dependencies first)
    affected.sort_by(|a, b| a.depth.cmp(&b.depth));

    let mut affected_files_vec: Vec<String> = affected_files.into_iter().collect();
    affected_files_vec.sort();

    Ok(SqryDependencyImpactResult {
        symbol: params.symbol.clone(),
        affected,
        total,
        affected_files: affected_files_vec,
        truncated,
    })
}

/// Collect callers via BFS traversal using the kernel.
///
/// Returns a tuple of (`affected_symbols`, `affected_file_paths`).
fn collect_callers_bfs(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    target: sqry_core::graph::unified::node::NodeId,
    max_depth: usize,
    include_indirect: bool,
    max_results: usize,
) -> (Vec<SqryAffectedSymbol>, HashSet<String>) {
    let effective_max_depth = if include_indirect {
        max_depth
    } else {
        max_depth.min(1)
    };
    let config = TraversalConfig {
        direction: TraversalDirection::Incoming,
        edge_filter: EdgeFilter::calls_only(),
        limits: TraversalLimits {
            max_depth: u32::try_from(effective_max_depth).unwrap_or(u32::MAX),
            max_nodes: Some(max_results),
            max_edges: None,
            max_paths: None,
        },
    };

    let result = traverse(snapshot, &[target], &config, None);

    let mut affected: Vec<SqryAffectedSymbol> = Vec::new();
    let mut affected_files: HashSet<String> = HashSet::new();

    // Skip the seed node (index 0 = target). All other nodes are callers.
    for (idx, mat_node) in result.nodes.iter().enumerate() {
        if mat_node.node_id == target {
            continue;
        }

        // Determine depth: find the minimum depth edge leading to this node
        let depth = result
            .edges
            .iter()
            .filter(|e| e.target_idx == idx || e.source_idx == idx)
            .map(|e| e.depth)
            .min()
            .unwrap_or(1);

        let is_direct = depth <= 1;

        let symbol = SqryAffectedSymbol {
            name: mat_node.name.clone(),
            qualified_name: mat_node.qualified_name.clone(),
            kind: mat_node.kind.clone(),
            file_path: mat_node.file_path.clone(),
            line: mat_node.start_line,
            is_direct,
            depth,
        };

        collect_affected_file(&symbol, &mut affected_files);
        affected.push(symbol);
    }

    (affected, affected_files)
}

/// Track affected file from an affected symbol.
fn collect_affected_file(symbol: &SqryAffectedSymbol, affected_files: &mut HashSet<String>) {
    if !symbol.file_path.is_empty() {
        affected_files.insert(symbol.file_path.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::Language;
    use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
    use sqry_core::graph::unified::{EdgeKind, FileId, NodeEntry, NodeId, NodeKind, ResolvedVia};

    struct TestGraph {
        graph: CodeGraph,
        file_id: Option<FileId>,
    }

    impl TestGraph {
        fn new() -> Self {
            Self {
                graph: CodeGraph::new(),
                file_id: None,
            }
        }

        fn ensure_file_id(&mut self) -> FileId {
            if let Some(file_id) = self.file_id {
                return file_id;
            }

            let file_path = std::path::PathBuf::from("/dependency-impact-tests/test.rs");
            let file_id = self
                .graph
                .files_mut()
                .register_with_language(&file_path, Some(Language::Rust))
                .expect("register test file");
            self.file_id = Some(file_id);
            file_id
        }

        fn add_node(&mut self, name: &str) -> NodeId {
            let file_id = self.ensure_file_id();
            let name_id = self.graph.strings_mut().intern(name).expect("intern name");
            let qualified_name_id = self
                .graph
                .strings_mut()
                .intern(&format!("test::{name}"))
                .expect("intern qualified name");
            let entry = NodeEntry::new(NodeKind::Function, name_id, file_id)
                .with_qualified_name(qualified_name_id)
                .with_location(1, 0, 10, 0);
            let node_id = self.graph.nodes_mut().alloc(entry).expect("alloc node");
            self.graph.indices_mut().add(
                node_id,
                NodeKind::Function,
                name_id,
                Some(qualified_name_id),
                file_id,
            );
            node_id
        }

        fn add_call_edge(&mut self, source: NodeId, target: NodeId) {
            let file_id = self.ensure_file_id();
            self.graph.edges_mut().add_edge(
                source,
                target,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                file_id,
            );
        }

        fn snapshot(&self) -> GraphSnapshot {
            self.graph.snapshot()
        }
    }

    fn make_affected(file_path: &str) -> SqryAffectedSymbol {
        SqryAffectedSymbol {
            name: "foo".to_string(),
            qualified_name: "mod::foo".to_string(),
            kind: "function".to_string(),
            file_path: file_path.to_string(),
            line: 1,
            is_direct: true,
            depth: 1,
        }
    }

    // ── collect_affected_file ─────────────────────────────────────────────────

    #[test]
    fn non_empty_file_path_is_inserted() {
        let mut set = std::collections::HashSet::new();
        let sym = make_affected("src/lib.rs");
        collect_affected_file(&sym, &mut set);
        assert!(set.contains("src/lib.rs"));
    }

    #[test]
    fn empty_file_path_is_not_inserted() {
        let mut set = std::collections::HashSet::new();
        let sym = make_affected("");
        collect_affected_file(&sym, &mut set);
        assert!(set.is_empty());
    }

    #[test]
    fn duplicate_file_paths_deduplicated() {
        let mut set = std::collections::HashSet::new();
        collect_affected_file(&make_affected("src/lib.rs"), &mut set);
        collect_affected_file(&make_affected("src/lib.rs"), &mut set);
        assert_eq!(set.len(), 1);
    }

    // ── DEFAULT_* constants ───────────────────────────────────────────────────

    #[test]
    fn default_max_depth_is_three() {
        assert_eq!(DEFAULT_MAX_DEPTH, 3);
    }

    #[test]
    fn default_max_results_is_500() {
        assert_eq!(DEFAULT_MAX_RESULTS, 500);
    }

    #[test]
    fn direct_only_dependency_impact_reports_immediate_callers() {
        let mut graph = TestGraph::new();
        let target = graph.add_node("target");
        let direct = graph.add_node("direct");
        let indirect = graph.add_node("indirect");
        graph.add_call_edge(direct, target);
        graph.add_call_edge(indirect, direct);

        let snapshot = graph.snapshot();
        let (affected, affected_files) = collect_callers_bfs(&snapshot, target, 3, false, 10);

        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0].qualified_name, "test::direct");
        assert!(affected[0].is_direct);
        assert_eq!(affected[0].depth, 1);
        assert_eq!(affected_files.len(), 1);
    }

    #[test]
    fn indirect_dependency_impact_reports_transitive_callers() {
        let mut graph = TestGraph::new();
        let target = graph.add_node("target");
        let direct = graph.add_node("direct");
        let indirect = graph.add_node("indirect");
        graph.add_call_edge(direct, target);
        graph.add_call_edge(indirect, direct);

        let snapshot = graph.snapshot();
        let (mut affected, _) = collect_callers_bfs(&snapshot, target, 3, true, 10);
        affected.sort_by(|left, right| {
            left.depth
                .cmp(&right.depth)
                .then(left.name.cmp(&right.name))
        });

        assert_eq!(affected.len(), 2);
        assert_eq!(affected[0].qualified_name, "test::direct");
        assert_eq!(affected[0].depth, 1);
        assert!(affected[0].is_direct);
        assert_eq!(affected[1].qualified_name, "test::indirect");
        assert_eq!(affected[1].depth, 2);
        assert!(!affected[1].is_direct);
    }
}
