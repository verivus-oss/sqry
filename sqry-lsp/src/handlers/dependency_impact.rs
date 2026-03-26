//! Dependency impact handler for LSP.
//!
//! Analyzes what symbols would be affected if a given symbol changes.

use std::collections::{HashSet, VecDeque};

use anyhow::Result;
use sqry_core::graph::unified::{FileScope, ResolutionMode, SymbolQuery, SymbolResolutionOutcome};

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
    let target_node_id = match snapshot.resolve_symbol(&SymbolQuery {
        symbol,
        file_scope: FileScope::Any,
        mode: ResolutionMode::Strict,
    }) {
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

/// Collect callers via BFS traversal.
///
/// Returns a tuple of (`affected_symbols`, `affected_file_paths`).
fn collect_callers_bfs(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    target: sqry_core::graph::unified::node::NodeId,
    max_depth: usize,
    include_indirect: bool,
    max_results: usize,
) -> (Vec<SqryAffectedSymbol>, HashSet<String>) {
    let mut affected: Vec<SqryAffectedSymbol> = Vec::new();
    let mut affected_files: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(sqry_core::graph::unified::node::NodeId, usize)> = VecDeque::new();
    let mut visited: HashSet<sqry_core::graph::unified::node::NodeId> = HashSet::new();

    queue.push_back((target, 0));
    visited.insert(target);

    while let Some((current_id, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }

        // Get callers (incoming call edges) - these are the symbols that depend on this one
        let callers = snapshot.get_callers(current_id);
        for caller_id in callers {
            if visited.contains(&caller_id) {
                continue;
            }
            visited.insert(caller_id);

            let Some(symbol) = build_affected_symbol(snapshot, caller_id, depth) else {
                continue;
            };

            collect_affected_file(&symbol, &mut affected_files);
            affected.push(symbol);

            // Continue traversal if including indirect dependencies
            if include_indirect && affected.len() < max_results {
                queue.push_back((caller_id, depth + 1));
            }
        }

        // Early exit if we have enough results
        if affected.len() >= max_results {
            break;
        }
    }

    (affected, affected_files)
}

/// Track affected file from an affected symbol.
fn collect_affected_file(symbol: &SqryAffectedSymbol, affected_files: &mut HashSet<String>) {
    if !symbol.file_path.is_empty() {
        affected_files.insert(symbol.file_path.clone());
    }
}

/// Build an `SqryAffectedSymbol` from a node ID at a given traversal depth.
fn build_affected_symbol(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node_id: sqry_core::graph::unified::node::NodeId,
    depth: usize,
) -> Option<SqryAffectedSymbol> {
    let strings = snapshot.strings();
    let files = snapshot.files();

    let node = snapshot.get_node(node_id)?;

    let name = strings
        .resolve(node.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let qualified_name =
        crate::conversion::display_entry_qualified_name(node, strings, files, &name);

    let kind = format!("{:?}", node.kind).to_lowercase();

    let file_path = files
        .resolve(node.file)
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let is_direct = depth == 0;
    let current_depth = u32::try_from(depth + 1).unwrap_or(u32::MAX);

    Some(SqryAffectedSymbol {
        name,
        qualified_name,
        kind,
        file_path,
        line: node.start_line,
        is_direct,
        depth: current_depth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
