//! Get insights handler for LSP.
//!
//! Provides codebase health metrics and statistics.

use std::collections::HashMap;

use anyhow::Result;

use crate::protocol::{
    SqryGetInsightsParams, SqryGetInsightsResult, SqryHealthIndicators, SqryKindStats,
    SqryLanguageStats,
};
use crate::session::SessionManager;

/// Execute get insights.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved or the graph
/// is unavailable.
pub fn execute(
    session: &SessionManager,
    params: &SqryGetInsightsParams,
) -> Result<SqryGetInsightsResult> {
    let _root = session.resolve_path(params.path.as_deref())?;

    log::debug!("Computing codebase insights");

    let graph = session
        .graph()?
        .ok_or_else(|| anyhow::anyhow!("No graph available. Run `sqry index` first."))?;

    let snapshot = graph.snapshot();

    // Count files, symbols, languages, and kinds
    let stats = count_language_and_kind_stats(&snapshot);
    let total_files = stats.total_files;
    let total_symbols = stats.total_symbols;
    let lang_file_counts = stats.lang_file_counts;
    let lang_symbol_counts = stats.lang_symbol_counts;
    let kind_counts = stats.kind_counts;

    // Count edges and cross-language edges
    let (total_edges, cross_language_edges) = count_edge_stats(&snapshot);

    // Count cycles (simplified - just check for mutual call patterns)
    let cycles = estimate_cycles(&snapshot);

    // Count unused symbols and duplicate groups
    let unused_symbols = estimate_unused(&snapshot);
    let duplicate_groups = count_duplicate_groups(&snapshot);

    // Build language stats
    let mut languages: Vec<SqryLanguageStats> = lang_file_counts
        .iter()
        .map(|(lang, &files)| SqryLanguageStats {
            language: lang.clone(),
            files,
            symbols: *lang_symbol_counts.get(lang).unwrap_or(&0),
        })
        .collect();
    languages.sort_by(|a, b| b.files.cmp(&a.files));

    // Build kind stats
    let mut symbol_kinds: Vec<SqryKindStats> = kind_counts
        .into_iter()
        .map(|(kind, count)| SqryKindStats { kind, count })
        .collect();
    symbol_kinds.sort_by(|a, b| b.count.cmp(&a.count));

    Ok(SqryGetInsightsResult {
        total_files,
        total_symbols,
        total_edges,
        languages,
        symbol_kinds,
        health: SqryHealthIndicators {
            cycles,
            unused_symbols,
            duplicate_groups,
            cross_language_edges,
        },
    })
}

/// Aggregated statistics about languages and symbol kinds.
struct LanguageKindStats {
    total_files: usize,
    total_symbols: usize,
    lang_file_counts: HashMap<String, usize>,
    lang_symbol_counts: HashMap<String, usize>,
    kind_counts: HashMap<String, usize>,
}

/// Count files, symbols, language distributions, and kind distributions.
fn count_language_and_kind_stats(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> LanguageKindStats {
    let files = snapshot.files();
    let mut lang_file_counts: HashMap<String, usize> = HashMap::new();
    let mut lang_symbol_counts: HashMap<String, usize> = HashMap::new();
    let mut kind_counts: HashMap<String, usize> = HashMap::new();

    let mut total_files = 0usize;
    let mut total_symbols = 0usize;
    let mut seen_files = std::collections::HashSet::new();

    for (_node_id, entry) in snapshot.iter_nodes() {
        total_symbols += 1;

        let kind = format!("{:?}", entry.kind).to_lowercase();
        *kind_counts.entry(kind).or_insert(0) += 1;

        let language = files
            .language_for_file(entry.file)
            .map_or("unknown".to_string(), |l| {
                l.to_string().to_ascii_lowercase()
            });

        *lang_symbol_counts.entry(language.clone()).or_insert(0) += 1;

        if seen_files.insert(entry.file) {
            total_files += 1;
            *lang_file_counts.entry(language).or_insert(0) += 1;
        }
    }

    LanguageKindStats {
        total_files,
        total_symbols,
        lang_file_counts,
        lang_symbol_counts,
        kind_counts,
    }
}

/// Count total edges and cross-language edges.
fn count_edge_stats(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> (usize, usize) {
    let files = snapshot.files();
    let mut total_edges = 0usize;
    let mut cross_language_edges = 0usize;

    for (source_id, target_id, _edge_kind) in snapshot.iter_edges() {
        total_edges += 1;

        if let (Some(from_entry), Some(to_entry)) =
            (snapshot.get_node(source_id), snapshot.get_node(target_id))
        {
            let from_lang = files.language_for_file(from_entry.file);
            let to_lang = files.language_for_file(to_entry.file);

            if let (Some(fl), Some(tl)) = (from_lang, to_lang)
                && fl != tl
            {
                cross_language_edges += 1;
            }
        }
    }

    (total_edges, cross_language_edges)
}

/// Estimate the number of mutual-call cycles (simplified heuristic).
fn estimate_cycles(snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot) -> usize {
    let mut cycles = 0usize;
    for (node_id, _entry) in snapshot.iter_nodes() {
        let callees = snapshot.get_callees(node_id);
        for callee in callees {
            let callee_callees = snapshot.get_callees(callee);
            if callee_callees.contains(&node_id) {
                cycles += 1;
            }
        }
    }
    cycles / 2 // Each cycle is counted twice
}

/// Estimate the number of potentially unused symbols (no callers but has callees).
fn estimate_unused(snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot) -> usize {
    let mut unused_symbols = 0usize;
    for (node_id, _entry) in snapshot.iter_nodes() {
        let callers = snapshot.get_callers(node_id);
        if callers.is_empty() {
            let has_outgoing = !snapshot.get_callees(node_id).is_empty();
            if has_outgoing {
                unused_symbols += 1;
            }
        }
    }
    unused_symbols
}

/// Count groups of symbols that share the same name.
fn count_duplicate_groups(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> usize {
    let strings = snapshot.strings();
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    for (_node_id, entry) in snapshot.iter_nodes() {
        if let Some(name) = strings.resolve(entry.name) {
            *name_counts.entry(name.to_string()).or_insert(0) += 1;
        }
    }
    name_counts.values().filter(|&&c| c > 1).count()
}

#[cfg(test)]
mod tests {
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;

    use super::*;

    fn empty_graph() -> CodeGraph {
        CodeGraph::new()
    }

    fn graph_with_two_nodes_same_name() -> CodeGraph {
        let mut graph = CodeGraph::new();
        let workspace_root = std::path::Path::new("/workspace");
        let name = graph.strings_mut().intern("foo").unwrap();
        let file = graph
            .files_mut()
            .register(&workspace_root.join("a.rs"))
            .unwrap();
        let file2 = graph
            .files_mut()
            .register(&workspace_root.join("b.rs"))
            .unwrap();
        let n1 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name, file))
            .unwrap();
        let n2 = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, name, file2))
            .unwrap();
        graph
            .indices_mut()
            .add(n1, NodeKind::Function, name, None, file);
        graph
            .indices_mut()
            .add(n2, NodeKind::Function, name, None, file2);
        graph
    }

    // ── count_duplicate_groups ────────────────────────────────────────────────

    #[test]
    fn count_duplicate_groups_empty_graph_returns_zero() {
        let graph = empty_graph();
        let snapshot = graph.snapshot();
        assert_eq!(count_duplicate_groups(&snapshot), 0);
    }

    #[test]
    fn count_duplicate_groups_with_duplicates_returns_one() {
        let graph = graph_with_two_nodes_same_name();
        let snapshot = graph.snapshot();
        // Both nodes share name "foo" → 1 duplicate group
        assert_eq!(count_duplicate_groups(&snapshot), 1);
    }

    // ── estimate_cycles ───────────────────────────────────────────────────────

    #[test]
    fn estimate_cycles_empty_graph_returns_zero() {
        let graph = empty_graph();
        let snapshot = graph.snapshot();
        assert_eq!(estimate_cycles(&snapshot), 0);
    }

    // ── estimate_unused ───────────────────────────────────────────────────────

    #[test]
    fn estimate_unused_empty_graph_returns_zero() {
        let graph = empty_graph();
        let snapshot = graph.snapshot();
        assert_eq!(estimate_unused(&snapshot), 0);
    }

    // ── count_language_and_kind_stats ─────────────────────────────────────────

    #[test]
    fn count_language_and_kind_stats_empty_graph() {
        let graph = empty_graph();
        let snapshot = graph.snapshot();
        let stats = count_language_and_kind_stats(&snapshot);
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.total_symbols, 0);
        assert!(stats.lang_file_counts.is_empty());
    }

    #[test]
    fn count_language_and_kind_stats_with_nodes() {
        let graph = graph_with_two_nodes_same_name();
        let snapshot = graph.snapshot();
        let stats = count_language_and_kind_stats(&snapshot);
        assert_eq!(stats.total_symbols, 2);
        // 2 distinct files → 2 total files
        assert_eq!(stats.total_files, 2);
    }

    // ── count_edge_stats ──────────────────────────────────────────────────────

    #[test]
    fn count_edge_stats_empty_graph_returns_zeros() {
        let graph = empty_graph();
        let snapshot = graph.snapshot();
        let (total, cross) = count_edge_stats(&snapshot);
        assert_eq!(total, 0);
        assert_eq!(cross, 0);
    }
}
