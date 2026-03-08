//! Complexity metrics handler for LSP.
//!
//! Provides complexity analysis for functions and methods.

use anyhow::Result;

use crate::protocol::{SqryComplexityMetricsParams, SqryComplexityMetricsResult};
use crate::session::SessionManager;

/// Default minimum complexity to report
const DEFAULT_MIN_COMPLEXITY: u32 = 1;

/// Default maximum results
const DEFAULT_MAX_RESULTS: usize = 100;

/// Execute complexity metrics analysis.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved or the graph
/// is unavailable.
pub fn execute(
    session: &SessionManager,
    params: &SqryComplexityMetricsParams,
) -> Result<SqryComplexityMetricsResult> {
    let _root = session.resolve_path(params.path.as_deref())?;

    let min_complexity = params.min_complexity.unwrap_or(DEFAULT_MIN_COMPLEXITY);
    let sort_by_complexity = params.sort_by_complexity.unwrap_or(true);
    let max_results = params.max_results.unwrap_or(DEFAULT_MAX_RESULTS);

    log::debug!(
        "Computing complexity metrics, target={:?}, min_complexity={}",
        params.target,
        min_complexity
    );

    let graph = session
        .graph()?
        .ok_or_else(|| anyhow::anyhow!("No graph available. Run `sqry index` first."))?;

    let snapshot = graph.snapshot();

    // Optionally filter by target file or symbol
    let target_filter: Option<String> = params.target.as_ref().map(|t| t.to_lowercase());

    let mut metrics = collect_complexity_metrics(
        &snapshot,
        target_filter.as_deref(),
        min_complexity,
        max_results,
    );

    // Sort by complexity or name
    if sort_by_complexity {
        metrics.sort_by(|a, b| {
            b.complexity
                .cmp(&a.complexity)
                .then_with(|| a.name.cmp(&b.name))
        });
    } else {
        metrics.sort_by(|a, b| a.name.cmp(&b.name));
    }

    metrics.truncate(max_results);

    let total = metrics.len();
    let max_complexity = metrics.iter().map(|m| m.complexity).max().unwrap_or(0);
    let average_complexity = if metrics.is_empty() {
        0.0
    } else {
        let count = f64::from(u32::try_from(metrics.len()).unwrap_or(u32::MAX));
        metrics.iter().map(|m| f64::from(m.complexity)).sum::<f64>() / count
    };

    Ok(SqryComplexityMetricsResult {
        metrics,
        total,
        average_complexity,
        max_complexity,
    })
}

/// Collect complexity metrics for functions and methods.
///
/// Returns a vector of complexity metrics that meet the minimum complexity threshold.
fn collect_complexity_metrics(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    target_filter: Option<&str>,
    min_complexity: u32,
    max_results: usize,
) -> Vec<crate::protocol::SqryComplexityMetric> {
    use sqry_core::graph::unified::node::NodeKind;

    let strings = snapshot.strings();
    let files = snapshot.files();
    let mut metrics: Vec<crate::protocol::SqryComplexityMetric> = Vec::new();

    for (node_id, entry) in snapshot.iter_nodes() {
        // Only analyze functions and methods
        if !matches!(entry.kind, NodeKind::Function | NodeKind::Method) {
            continue;
        }

        let name = match strings.resolve(entry.name) {
            Some(n) => n.to_string(),
            None => continue,
        };

        let qualified_name = entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .map_or_else(|| name.clone(), |s| s.to_string());

        let file_path = match files.resolve(entry.file) {
            Some(p) => p.display().to_string(),
            None => continue,
        };

        // Apply target filter
        if !matches_target_filter(target_filter, &file_path, &name, &qualified_name) {
            continue;
        }

        let kind = format!("{:?}", entry.kind).to_lowercase();
        let lines = entry
            .end_line
            .saturating_sub(entry.start_line)
            .saturating_add(1);

        // Estimate complexity based on callees and line count
        // This is a simple heuristic - real cyclomatic complexity requires AST analysis
        let callees = snapshot.get_callees(node_id);
        let callee_count = u32::try_from(callees.len()).unwrap_or(u32::MAX);

        // Simple complexity formula: 1 + callee_count / 5 + lines / 20
        let complexity = 1 + callee_count / 5 + lines / 20;

        if complexity < min_complexity {
            continue;
        }

        metrics.push(crate::protocol::SqryComplexityMetric {
            name,
            qualified_name,
            kind,
            file_path,
            complexity,
            lines,
        });

        if metrics.len() >= max_results {
            break;
        }
    }

    metrics
}

/// Check whether a node matches the optional target filter.
fn matches_target_filter(
    filter: Option<&str>,
    file_path: &str,
    name: &str,
    qualified_name: &str,
) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    file_path.to_lowercase().contains(filter)
        || name.to_lowercase().contains(filter)
        || qualified_name.to_lowercase().contains(filter)
}
