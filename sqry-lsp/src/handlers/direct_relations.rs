//! Direct callers/callees handlers for LSP.
//!
//! Provides optimized direct relation queries using `CodeGraph`. SGA06 routes
//! graph acquisition through [`SessionManager::graph_for_path`] (shared
//! `FilesystemGraphProvider` pipeline) and runs queries via
//! `QueryExecutor::execute_on_preloaded_graph`.

use anyhow::{Context, Result};
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::protocol::{
    SqryDirectCalleesParams, SqryDirectCalleesResult, SqryDirectCallersParams,
    SqryDirectCallersResult, SqrySearchItem,
};
use crate::session::SessionManager;

/// Default limit for caller/callee results
const DEFAULT_LIMIT: usize = 100;

/// Execute direct callers query.
///
/// Finds all symbols that directly call the given symbol using `CodeGraph`.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved or the query fails.
pub fn execute_direct_callers(
    session: &SessionManager,
    params: &SqryDirectCallersParams,
) -> Result<SqryDirectCallersResult> {
    let root = session.resolve_path(params.path.as_deref())?;
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);

    log::debug!(
        "Executing direct callers: symbol='{}', root={}",
        params.symbol,
        root.display()
    );

    // SGA06 — acquire the graph through the shared provider before running
    // the callers: predicate via the preloaded executor entrypoint.
    let Some(graph) = session.graph_for_path(&root)? else {
        return Ok(SqryDirectCallersResult {
            symbol: params.symbol.clone(),
            callers: Vec::new(),
            total: 0,
            truncated: false,
        });
    };

    let query = format!("callers:{}", params.symbol);
    let executor = session.executor();
    let query_results = executor
        .execute_on_preloaded_graph(graph, &query, &root, None)
        .with_context(|| format!("failed to execute callers query for '{}'", params.symbol))?;

    let total_found = query_results.len();
    let truncated = total_found > limit;

    // Convert QueryResults to SqrySearchItems
    let callers: Vec<SqrySearchItem> = query_results
        .iter()
        .take(limit)
        .filter_map(|m| {
            let name = m.name().map(|s| s.to_string()).unwrap_or_default();
            let kind = m.kind().as_str().to_string();
            let language = m.language().map_or_else(
                || "unknown".to_string(),
                |l| l.to_string().to_ascii_lowercase(),
            );

            let file_path = m.relative_path().map(|p| root.join(p))?;
            let uri = Url::from_file_path(&file_path).ok()?;

            let start = Position {
                line: m.start_line().saturating_sub(1),
                character: m.start_column().saturating_sub(1),
            };
            let end = Position {
                line: m.end_line().saturating_sub(1),
                character: m.end_column().saturating_sub(1),
            };
            let location = Location {
                uri,
                range: Range { start, end },
            };

            Some(SqrySearchItem {
                name: name.clone(),
                kind,
                qualified_name: name,
                language,
                location,
                score: None,
            })
        })
        .collect();

    Ok(SqryDirectCallersResult {
        symbol: params.symbol.clone(),
        callers,
        total: total_found,
        truncated,
    })
}

/// Execute direct callees query.
///
/// Finds all symbols that the given symbol directly calls using `CodeGraph`.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved or the query fails.
pub fn execute_direct_callees(
    session: &SessionManager,
    params: &SqryDirectCalleesParams,
) -> Result<SqryDirectCalleesResult> {
    let root = session.resolve_path(params.path.as_deref())?;
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);

    log::debug!(
        "Executing direct callees: symbol='{}', root={}",
        params.symbol,
        root.display()
    );

    // SGA06 — acquire the graph through the shared provider before running
    // the callees: predicate via the preloaded executor entrypoint.
    let Some(graph) = session.graph_for_path(&root)? else {
        return Ok(SqryDirectCalleesResult {
            symbol: params.symbol.clone(),
            callees: Vec::new(),
            total: 0,
            truncated: false,
        });
    };

    let query = format!("callees:{}", params.symbol);
    let executor = session.executor();
    let query_results = executor
        .execute_on_preloaded_graph(graph, &query, &root, None)
        .with_context(|| format!("failed to execute callees query for '{}'", params.symbol))?;

    let total_found = query_results.len();
    let truncated = total_found > limit;

    // Convert QueryResults to SqrySearchItems
    let callees: Vec<SqrySearchItem> = query_results
        .iter()
        .take(limit)
        .filter_map(|m| {
            let name = m.name().map(|s| s.to_string()).unwrap_or_default();
            let kind = m.kind().as_str().to_string();
            let language = m.language().map_or_else(
                || "unknown".to_string(),
                |l| l.to_string().to_ascii_lowercase(),
            );

            let file_path = m.relative_path().map(|p| root.join(p))?;
            let uri = Url::from_file_path(&file_path).ok()?;

            let start = Position {
                line: m.start_line().saturating_sub(1),
                character: m.start_column().saturating_sub(1),
            };
            let end = Position {
                line: m.end_line().saturating_sub(1),
                character: m.end_column().saturating_sub(1),
            };
            let location = Location {
                uri,
                range: Range { start, end },
            };

            Some(SqrySearchItem {
                name: name.clone(),
                kind,
                qualified_name: name,
                language,
                location,
                score: None,
            })
        })
        .collect();

    Ok(SqryDirectCalleesResult {
        symbol: params.symbol.clone(),
        callees,
        total: total_found,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── DEFAULT_LIMIT constant ────────────────────────────────────────────────

    #[test]
    fn default_limit_is_100() {
        assert_eq!(DEFAULT_LIMIT, 100);
    }
}
