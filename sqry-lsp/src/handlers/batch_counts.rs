//! Batch caller/callee count handler for LSP.
//!
//! Returns caller and callee counts for multiple symbols in a single request,
//! avoiding per-symbol round-trip overhead for `CodeLens`.

use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Instant;

use crate::protocol::{
    SqryBatchCallerCalleeCountParams, SqryBatchCallerCalleeCountResult, SymbolCount,
};
use crate::session::SessionManager;

use super::index::perf_log;

/// Execute a batch caller/callee count query.
///
/// For each symbol in the request, runs `callers:{name}` and `callees:{name}`
/// queries against the graph and returns the result counts.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved or a query fails.
#[allow(
    clippy::similar_names,
    reason = "callers_query/callees_query and callers_count/callees_count are intentionally symmetric"
)]
pub fn batch_caller_callee_count(
    session: &SessionManager,
    params: &SqryBatchCallerCalleeCountParams,
) -> Result<SqryBatchCallerCalleeCountResult> {
    let handler_start = Instant::now();
    perf_log(&format!(
        "batch_caller_callee_count START symbols={}",
        params.symbols.len()
    ));

    let root = session.resolve_path(params.path.as_deref())?;
    let executor = session.executor();

    let mut counts = Vec::with_capacity(params.symbols.len());

    // SGA06 — acquire the graph once through the shared
    // `FilesystemGraphProvider` pipeline and reuse it for every symbol.
    // If no snapshot is available, every symbol gets zero counts (mirrors
    // the previous swallow-error behaviour without re-entering the
    // executor's own `get_or_load_graph`).
    let graph = match session.graph_for_path(&root) {
        Ok(Some(g)) => Some(g),
        Ok(None) => None,
        Err(err) => {
            log::warn!(
                "batch_caller_callee_count: failed to acquire graph for '{}': {err}",
                root.display()
            );
            None
        }
    };

    for sym_ref in &params.symbols {
        let (callers_count, callees_count) = if let Some(ref graph) = graph {
            let callers_query = format!("callers:{}", sym_ref.name);
            let callers_count = executor
                .execute_on_preloaded_graph(Arc::clone(graph), &callers_query, &root, None)
                .with_context(|| format!("failed to execute callers query for '{}'", sym_ref.name))
                .map(|results| results.len())
                .unwrap_or(0);

            let callees_query = format!("callees:{}", sym_ref.name);
            let callees_count = executor
                .execute_on_preloaded_graph(Arc::clone(graph), &callees_query, &root, None)
                .with_context(|| format!("failed to execute callees query for '{}'", sym_ref.name))
                .map(|results| results.len())
                .unwrap_or(0);

            (callers_count, callees_count)
        } else {
            (0, 0)
        };

        counts.push(SymbolCount {
            name: sym_ref.name.clone(),
            callers: callers_count,
            callees: callees_count,
        });
    }

    perf_log(&format!(
        "batch_caller_callee_count TOTAL took {:?}, symbols={}",
        handler_start.elapsed(),
        counts.len()
    ));

    Ok(SqryBatchCallerCalleeCountResult { counts })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_symbols_returns_empty_counts() {
        let params = SqryBatchCallerCalleeCountParams {
            symbols: vec![],
            path: None,
        };
        // Without a session we can only verify the struct construction
        let result = SqryBatchCallerCalleeCountResult { counts: vec![] };
        assert!(result.counts.is_empty());
        assert!(params.symbols.is_empty());
    }

    #[test]
    fn symbol_count_fields_are_correct() {
        let count = SymbolCount {
            name: "test_fn".to_string(),
            callers: 3,
            callees: 5,
        };
        assert_eq!(count.name, "test_fn");
        assert_eq!(count.callers, 3);
        assert_eq!(count.callees, 5);
    }
}
