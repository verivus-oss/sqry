//! Trace path handler for LSP.
//!
//! Finds K shortest call paths between two symbols using BFS traversal.

use std::path::Path;

use anyhow::Result;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::{
    EdgeFilter, SccPathStrategy, SimplePathStrategy, TraversalConfig, TraversalDirection,
    TraversalLimits, traverse,
};
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::handlers::graph_common::find_nodes_by_name;
use crate::protocol::{
    SqryCallPath, SqryPathStep, SqrySearchItem, SqryTracePathParams, SqryTracePathResult,
};
use crate::session::SessionManager;

/// Default maximum path length
const DEFAULT_MAX_HOPS: usize = 5;

/// Default maximum paths to return
const DEFAULT_MAX_PATHS: usize = 5;

/// Default minimum edge confidence
const DEFAULT_MIN_CONFIDENCE: f64 = 0.5;

/// Execute trace path search.
///
/// Uses BFS to find K shortest call paths between two symbols.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, inputs are invalid,
/// or the graph is unavailable.
pub fn execute(
    session: &SessionManager,
    params: &SqryTracePathParams,
) -> Result<SqryTracePathResult> {
    let root = session.resolve_path(params.path.as_deref())?;
    let from_symbol = params.from_symbol.trim();
    let to_symbol = params.to_symbol.trim();

    if from_symbol.is_empty() {
        anyhow::bail!("from_symbol cannot be empty");
    }
    if to_symbol.is_empty() {
        anyhow::bail!("to_symbol cannot be empty");
    }

    let max_hops = params.max_hops.unwrap_or(DEFAULT_MAX_HOPS);
    let max_paths = params.max_paths.unwrap_or(DEFAULT_MAX_PATHS);
    let min_confidence = params.min_confidence.unwrap_or(DEFAULT_MIN_CONFIDENCE);
    let cross_language = params.cross_language.unwrap_or(true);

    log::debug!(
        "Tracing path: from='{}', to='{}', max_hops={}, root={}",
        from_symbol,
        to_symbol,
        max_hops,
        root.display()
    );

    // Get graph snapshot
    let graph = session
        .graph()?
        .ok_or_else(|| anyhow::anyhow!("No graph available. Run `sqry index` first."))?;

    let snapshot = graph.snapshot();

    // Find source and target nodes
    let from_nodes = find_nodes_by_name(&snapshot, from_symbol);
    let to_nodes = find_nodes_by_name(&snapshot, to_symbol);

    if from_nodes.is_empty() {
        anyhow::bail!("Start symbol '{from_symbol}' not found in graph");
    }
    if to_nodes.is_empty() {
        anyhow::bail!("Target symbol '{to_symbol}' not found in graph");
    }

    // Try Pass 5 optimization for path finding
    let workspace_root = session.root_path();
    let analysis_data = try_load_analysis_data(&graph, workspace_root, "calls");

    // Find all paths between any from/to pair
    let mut all_paths: Vec<Vec<NodeId>> = Vec::new();
    for &from_node in &from_nodes {
        for &to_node in &to_nodes {
            let paths = if let Some((ref scc_data, ref cond_dag)) = analysis_data {
                // Fast path: Use Pass 5 optimizations
                find_k_shortest_paths_optimized(&PathFindParams {
                    snapshot: &snapshot,
                    scc_data,
                    cond_dag,
                    from: from_node,
                    to: to_node,
                    max_hops,
                    max_paths,
                    min_confidence,
                    allow_cross_language: cross_language,
                })
            } else {
                // Slow path: Standard BFS
                find_k_shortest_paths(
                    &snapshot,
                    from_node,
                    to_node,
                    max_hops,
                    max_paths,
                    min_confidence,
                    cross_language,
                )
            };

            if let Some(paths) = paths {
                all_paths.extend(paths);
            }
        }
    }

    // Sort by path length and truncate
    all_paths.sort_by_key(Vec::len);
    all_paths.truncate(max_paths);

    // Convert to result format
    let paths: Vec<SqryCallPath> = all_paths
        .iter()
        .map(|nodes| build_call_path(&snapshot, nodes, &root))
        .collect();

    let total = paths.len();

    Ok(SqryTracePathResult {
        from_symbol: params.from_symbol.clone(),
        to_symbol: params.to_symbol.clone(),
        paths,
        total,
    })
}

/// Find K shortest paths using the kernel with `SimplePathStrategy`.
fn find_k_shortest_paths(
    snapshot: &GraphSnapshot,
    from: NodeId,
    to: NodeId,
    max_hops: usize,
    max_paths: usize,
    min_confidence: f64,
    allow_cross_language: bool,
) -> Option<Vec<Vec<NodeId>>> {
    let mut strategy = SimplePathStrategy::new(to, min_confidence, allow_cross_language);

    let config = TraversalConfig {
        direction: TraversalDirection::Outgoing,
        edge_filter: EdgeFilter::calls_only(),
        limits: TraversalLimits {
            max_depth: u32::try_from(max_hops).unwrap_or(u32::MAX),
            max_nodes: None,
            max_edges: None,
            max_paths: Some(max_paths),
        },
    };

    let result = traverse(snapshot, &[from], &config, Some(&mut strategy));

    // Convert index-based paths to NodeId-based paths
    let paths: Vec<Vec<NodeId>> = result
        .paths
        .unwrap_or_default()
        .into_iter()
        .map(|index_path| {
            index_path
                .iter()
                .map(|&idx| result.nodes[idx].node_id)
                .collect()
        })
        .collect();

    if paths.is_empty() { None } else { Some(paths) }
}

/// Parameters for path finding.
struct PathFindParams<'a> {
    snapshot: &'a GraphSnapshot,
    scc_data: &'a sqry_core::graph::unified::analysis::SccData,
    cond_dag: &'a sqry_core::graph::unified::analysis::CondensationDag,
    from: NodeId,
    to: NodeId,
    max_hops: usize,
    max_paths: usize,
    min_confidence: f64,
    allow_cross_language: bool,
}

/// Find `K` shortest paths with SCC-pruned kernel strategy.
///
/// Uses condensation DAG for pruning: skips branches that can't reach target.
fn find_k_shortest_paths_optimized(params: &PathFindParams<'_>) -> Option<Vec<Vec<NodeId>>> {
    let PathFindParams {
        snapshot,
        scc_data,
        cond_dag,
        from,
        to,
        max_hops,
        max_paths,
        min_confidence,
        allow_cross_language,
    } = *params;

    let mut strategy =
        SccPathStrategy::new(scc_data, cond_dag, to, min_confidence, allow_cross_language);

    let config = TraversalConfig {
        direction: TraversalDirection::Outgoing,
        edge_filter: EdgeFilter::calls_only(),
        limits: TraversalLimits {
            max_depth: u32::try_from(max_hops).unwrap_or(u32::MAX),
            max_nodes: None,
            max_edges: None,
            max_paths: Some(max_paths),
        },
    };

    let result = traverse(snapshot, &[from], &config, Some(&mut strategy));

    // Convert index-based paths to NodeId-based paths
    let paths: Vec<Vec<NodeId>> = result
        .paths
        .unwrap_or_default()
        .into_iter()
        .map(|index_path| {
            index_path
                .iter()
                .map(|&idx| result.nodes[idx].node_id)
                .collect()
        })
        .collect();

    if paths.is_empty() { None } else { Some(paths) }
}

/// Compute confidence score for an edge.
fn edge_confidence(kind: &EdgeKind) -> f64 {
    match kind {
        EdgeKind::Calls { is_async, .. } => {
            if *is_async {
                0.9
            } else {
                1.0
            }
        }
        EdgeKind::Imports { is_wildcard, .. } => {
            if *is_wildcard {
                0.7
            } else {
                0.95
            }
        }
        EdgeKind::References => 0.8,
        EdgeKind::Inherits | EdgeKind::Implements => 0.95,
        _ => 1.0,
    }
}

/// Build a location from a node entry and workspace root.
fn build_node_location(
    snapshot: &GraphSnapshot,
    entry: &sqry_core::graph::unified::NodeEntry,
    workspace_root: &Path,
) -> Location {
    let files = snapshot.files();
    let file_path = files.resolve(entry.file);
    if let Some(fp) = file_path {
        let full_path = workspace_root.join(fp.as_ref());
        let uri = Url::from_file_path(&full_path)
            .unwrap_or_else(|()| Url::parse(&format!("file://{}", full_path.display())).unwrap());
        Location {
            uri,
            range: Range {
                start: Position::new(
                    entry.start_line.saturating_sub(1),
                    entry.start_column.saturating_sub(1),
                ),
                end: Position::new(
                    entry.end_line.saturating_sub(1),
                    entry.end_column.saturating_sub(1),
                ),
            },
        }
    } else {
        Location {
            uri: Url::parse("file:///unknown").unwrap(),
            range: Range::default(),
        }
    }
}

/// Build a path step from a node entry at a given index in the path.
fn build_path_step(
    snapshot: &GraphSnapshot,
    entry: &sqry_core::graph::unified::NodeEntry,
    node_id: NodeId,
    idx: usize,
    path_nodes: &[NodeId],
    workspace_root: &Path,
) -> SqryPathStep {
    let strings = snapshot.strings();
    let files = snapshot.files();

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let qualified_name =
        crate::conversion::display_entry_qualified_name(entry, strings, files, &name);

    let kind = format!("{:?}", entry.kind).to_lowercase();

    let lang = files.language_for_file(entry.file);
    let language = lang.map_or("unknown".to_string(), |l| {
        l.to_string().to_ascii_lowercase()
    });

    let location = build_node_location(snapshot, entry, workspace_root);

    let symbol = SqrySearchItem {
        name,
        kind,
        qualified_name,
        language,
        location,
        score: None,
    };

    let (edge_type, confidence) = if idx == 0 {
        ("start".to_string(), None)
    } else if let Some(&prev_node) = path_nodes.get(idx - 1) {
        get_edge_info(snapshot, prev_node, node_id)
    } else {
        ("call".to_string(), Some(1.0))
    };

    SqryPathStep {
        symbol,
        edge_type,
        confidence,
    }
}

/// Build a `CallPath` from a list of node IDs.
fn build_call_path(
    snapshot: &GraphSnapshot,
    path_nodes: &[NodeId],
    workspace_root: &Path,
) -> SqryCallPath {
    let files = snapshot.files();
    let mut steps = Vec::new();
    let mut cross_language = false;
    let mut prev_lang: Option<sqry_core::graph::Language> = None;

    for (idx, &node_id) in path_nodes.iter().enumerate() {
        let Some(entry) = snapshot.get_node(node_id) else {
            continue;
        };

        let lang = files.language_for_file(entry.file);

        // Check for cross-language transition
        if let (Some(prev), Some(current)) = (prev_lang, lang)
            && prev != current
        {
            cross_language = true;
        }
        prev_lang = lang;

        steps.push(build_path_step(
            snapshot,
            entry,
            node_id,
            idx,
            path_nodes,
            workspace_root,
        ));
    }

    let length = steps.len().saturating_sub(1).try_into().unwrap_or(u32::MAX);
    let score = calculate_path_score(&steps, cross_language);

    SqryCallPath {
        steps,
        length,
        score,
        cross_language,
    }
}

/// Get edge type and confidence between two nodes.
fn get_edge_info(snapshot: &GraphSnapshot, from: NodeId, to: NodeId) -> (String, Option<f64>) {
    for edge in snapshot.edges().edges_from(from) {
        if edge.target == to {
            let edge_type = match &edge.kind {
                EdgeKind::Calls { is_async, .. } => {
                    if *is_async {
                        "async_call"
                    } else {
                        "call"
                    }
                }
                EdgeKind::Imports { .. } => "import",
                EdgeKind::Exports { .. } => "export",
                EdgeKind::References => "reference",
                EdgeKind::Inherits => "inherits",
                EdgeKind::Implements => "implements",
                _ => "edge",
            };
            let confidence = edge_confidence(&edge.kind);
            return (edge_type.to_string(), Some(confidence));
        }
    }
    ("unknown".to_string(), None)
}

/// Calculate path score (higher is better).
fn calculate_path_score(steps: &[SqryPathStep], cross_language: bool) -> f64 {
    let step_count = u32::try_from(steps.len()).unwrap_or(u32::MAX).max(1);
    let length_penalty = 1.0 / f64::from(step_count);
    let cross_lang_bonus = if cross_language { 0.1 } else { 0.0 };
    length_penalty + cross_lang_bonus
}

/// Try to load Pass 5 analysis data (SCC + condensation DAG).
///
/// Returns None if analyses are unavailable or validation fails.
fn try_load_analysis_data(
    graph: &sqry_core::graph::unified::CodeGraph,
    workspace_root: &Path,
    edge_kind: &str,
) -> Option<(
    sqry_core::graph::unified::analysis::SccData,
    sqry_core::graph::unified::analysis::CondensationDag,
)> {
    let storage = sqry_core::graph::unified::persistence::GraphStorage::new(workspace_root);
    sqry_core::graph::unified::analysis::try_load_scc_and_condensation(
        &storage,
        &graph.snapshot(),
        edge_kind,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::edge::EdgeKind;

    // ── edge_confidence ──────────────────────────────────────────────────────

    #[test]
    fn edge_confidence_sync_call_is_one() {
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        let c = edge_confidence(&kind);
        assert!((c - 1.0).abs() < f64::EPSILON, "expected 1.0, got {c}");
    }

    #[test]
    fn edge_confidence_async_call_is_point_nine() {
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: true,
        };
        let c = edge_confidence(&kind);
        assert!((c - 0.9).abs() < f64::EPSILON, "expected 0.9, got {c}");
    }

    #[test]
    fn edge_confidence_non_wildcard_import_is_point_95() {
        let kind = EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        };
        let c = edge_confidence(&kind);
        assert!((c - 0.95).abs() < f64::EPSILON, "expected 0.95, got {c}");
    }

    #[test]
    fn edge_confidence_wildcard_import_is_point_seven() {
        let kind = EdgeKind::Imports {
            alias: None,
            is_wildcard: true,
        };
        let c = edge_confidence(&kind);
        assert!((c - 0.7).abs() < f64::EPSILON, "expected 0.7, got {c}");
    }

    #[test]
    fn edge_confidence_references_is_point_eight() {
        let c = edge_confidence(&EdgeKind::References);
        assert!((c - 0.8).abs() < f64::EPSILON, "expected 0.8, got {c}");
    }

    #[test]
    fn edge_confidence_inherits_is_point_95() {
        let c = edge_confidence(&EdgeKind::Inherits);
        assert!((c - 0.95).abs() < f64::EPSILON, "expected 0.95, got {c}");
    }

    #[test]
    fn edge_confidence_implements_is_point_95() {
        let c = edge_confidence(&EdgeKind::Implements);
        assert!((c - 0.95).abs() < f64::EPSILON, "expected 0.95, got {c}");
    }

    #[test]
    fn edge_confidence_other_kind_is_one() {
        // e.g., Defines, Contains, etc. fall through to _ => 1.0
        let c = edge_confidence(&EdgeKind::Defines);
        assert!((c - 1.0).abs() < f64::EPSILON, "expected 1.0, got {c}");
    }

    // ── kernel is_followable_edge (re-exported) ────────────────────────────

    #[test]
    fn kernel_followable_sync_call_above_threshold() {
        use sqry_core::graph::unified::is_followable_edge;
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        };
        assert!(is_followable_edge(&kind, 0.5));
        assert!(is_followable_edge(&kind, 1.0));
    }

    #[test]
    fn kernel_followable_async_call_below_threshold() {
        use sqry_core::graph::unified::is_followable_edge;
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: true,
        };
        // Kernel assigns confidence 1.0 to all Calls (sync and async)
        // so threshold 0.95 is still followable in the kernel model
        assert!(is_followable_edge(&kind, 0.95));
        assert!(is_followable_edge(&kind, 1.0));
    }

    #[test]
    fn kernel_followable_references_at_low_confidence() {
        use sqry_core::graph::unified::is_followable_edge;
        // References have confidence 0.7 in the kernel
        assert!(is_followable_edge(&EdgeKind::References, 0.5));
        assert!(!is_followable_edge(&EdgeKind::References, 0.8));
    }

    #[test]
    fn kernel_followable_defines_at_low_confidence() {
        use sqry_core::graph::unified::is_followable_edge;
        // Defines falls into the "everything else: 0.3" bucket
        assert!(is_followable_edge(&EdgeKind::Defines, 0.3));
        assert!(!is_followable_edge(&EdgeKind::Defines, 0.5));
    }

    // ── calculate_path_score ─────────────────────────────────────────────────

    fn make_path_step() -> SqryPathStep {
        use crate::protocol::SqrySearchItem;
        SqryPathStep {
            symbol: SqrySearchItem {
                name: "fn".to_string(),
                kind: "function".to_string(),
                qualified_name: "fn".to_string(),
                language: "rust".to_string(),
                location: tower_lsp::lsp_types::Location {
                    uri: tower_lsp::lsp_types::Url::parse("file:///f.rs").unwrap(),
                    range: tower_lsp::lsp_types::Range::default(),
                },
                score: None,
            },
            edge_type: String::new(),
            confidence: None,
        }
    }

    #[test]
    fn calculate_path_score_single_step_no_cross_language() {
        let steps = vec![make_path_step()];
        let score = calculate_path_score(&steps, false);
        // 1 step: length_penalty = 1/1 = 1.0, no cross-lang bonus
        assert!(
            (score - 1.0).abs() < f64::EPSILON,
            "expected 1.0, got {score}"
        );
    }

    #[test]
    fn calculate_path_score_cross_language_adds_bonus() {
        let steps = vec![make_path_step()];
        let score = calculate_path_score(&steps, true);
        // 1 step: 1.0 + 0.1 bonus = 1.1
        assert!(
            (score - 1.1).abs() < f64::EPSILON,
            "expected 1.1, got {score}"
        );
    }

    #[test]
    fn calculate_path_score_more_steps_lower_score() {
        let steps_1 = vec![make_path_step()];
        let steps_5 = vec![
            make_path_step(),
            make_path_step(),
            make_path_step(),
            make_path_step(),
            make_path_step(),
        ];
        let score_1 = calculate_path_score(&steps_1, false);
        let score_5 = calculate_path_score(&steps_5, false);
        assert!(
            score_1 > score_5,
            "1-step path should score higher than 5-step"
        );
    }
}
