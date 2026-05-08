use anyhow::{Context, Result};
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::protocol::{SqrySearchItem, SqrySearchParams, SqrySearchResult};
use crate::session::SessionManager;

/// Execute a text/semantic search via `CodeGraph`.
///
/// # Errors
///
/// Returns an error when graph loading or query execution fails.
pub fn execute(session: &SessionManager, params: &SqrySearchParams) -> Result<SqrySearchResult> {
    let config = session.config();
    let configured_limit = config.search_limit;
    let mut requested_limit = params.limit.unwrap_or(configured_limit);
    if requested_limit == 0 {
        requested_limit = configured_limit;
    }
    let limit = requested_limit.min(configured_limit);

    let root = session.resolve_path(params.path.as_deref())?;
    let query = params.query.trim();

    log::debug!(
        "Executing search: query='{query}', root={root}",
        root = root.display()
    );

    // SGA06 — route graph acquisition through `SessionManager::graph_for_path`
    // so the read-only LSP search path uses the shared
    // `FilesystemGraphProvider` pipeline instead of re-entering the
    // executor's own `get_or_load_graph` (which would bypass the SGA-migrated
    // path-policy / SHA-256 / plugin-compat checks).
    let Some(graph) = session.graph_for_path(&root)? else {
        // No snapshot on disk yet — surface an empty, non-truncated result
        // (the LSP startup filter / auto-build is responsible for indexing).
        return Ok(SqrySearchResult {
            results: Vec::new(),
            total: 0,
            is_truncated: false,
            used_index: true,
        });
    };

    let executor = session.executor();
    let query_results = executor
        .execute_on_preloaded_graph(graph, query, &root, None)
        .with_context(|| format!("failed to execute sqry query '{query}'"))?;

    let total = query_results.len();
    let truncated = total > limit;

    // Convert QueryResults to SqrySearchItems
    let results: Vec<SqrySearchItem> = query_results
        .iter()
        .take(limit)
        .filter_map(|m| {
            let name = m.name().map(|s| s.to_string()).unwrap_or_default();
            let kind = m.kind().as_str().to_string();
            let language = m.language().map_or_else(
                || "unknown".to_string(),
                |l| l.to_string().to_ascii_lowercase(),
            );

            // Build file path
            let file_path = m.relative_path().map(|p| root.join(p))?;
            let uri = Url::from_file_path(&file_path).ok()?;

            // Create location range (0-indexed for LSP)
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
                qualified_name: name, // No container info available
                language,
                location,
                score: None,
            })
        })
        .collect();

    Ok(SqrySearchResult {
        results,
        total,
        is_truncated: truncated,
        used_index: true, // Always uses CodeGraph
    })
}
