//! Pattern search handler for LSP.
//!
//! Implements wildcard pattern matching for symbol names.

use anyhow::Result;
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::protocol::{SqryPatternSearchParams, SqryPatternSearchResult, SqrySearchItem};
use crate::session::SessionManager;

/// Default limit for pattern search results
const DEFAULT_LIMIT: usize = 100;

/// Execute pattern search.
///
/// Searches for symbols matching a pattern with wildcard support (* and ?).
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, the pattern is empty,
/// or the graph is unavailable.
pub fn execute(
    session: &SessionManager,
    params: &SqryPatternSearchParams,
) -> Result<SqryPatternSearchResult> {
    let config = session.config();
    let configured_limit = config.search_limit;
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(configured_limit);

    let root = session.resolve_path(params.path.as_deref())?;
    let pattern = params.pattern.trim();

    if pattern.is_empty() {
        anyhow::bail!("pattern cannot be empty");
    }

    log::debug!(
        "Executing pattern search: pattern='{}', root={}",
        pattern,
        root.display()
    );

    // Get graph snapshot
    let graph = session
        .graph()?
        .ok_or_else(|| anyhow::anyhow!("No graph available. Run `sqry index` first."))?;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files = snapshot.files();

    // Find nodes matching the pattern
    let matching_ids = snapshot.find_by_pattern(pattern);

    let total = matching_ids.len();
    let truncated = total > limit;

    // Convert to SqrySearchItems
    let matches: Vec<SqrySearchItem> = matching_ids
        .into_iter()
        .take(limit)
        .filter_map(|node_id| {
            let entry = snapshot.get_node(node_id)?;
            let name = strings.resolve(entry.name)?.to_string();
            let kind = format!("{:?}", entry.kind).to_lowercase();

            let language = files
                .language_for_file(entry.file)
                .map_or("unknown".to_string(), |l| {
                    l.to_string().to_ascii_lowercase()
                });

            // Build file path and URI
            let file_path = files.resolve(entry.file)?;
            let full_path = root.join(file_path.as_ref());
            let uri = Url::from_file_path(&full_path).ok()?;

            // Create location range (0-indexed for LSP)
            let start = Position {
                line: entry.start_line.saturating_sub(1),
                character: entry.start_column.saturating_sub(1),
            };
            let end = Position {
                line: entry.end_line.saturating_sub(1),
                character: entry.end_column.saturating_sub(1),
            };
            let location = Location {
                uri,
                range: Range { start, end },
            };

            let qualified_name = entry
                .qualified_name
                .and_then(|id| strings.resolve(id))
                .map_or_else(|| name.clone(), |s| s.to_string());

            Some(SqrySearchItem {
                name,
                kind,
                qualified_name,
                language,
                location,
                score: None,
            })
        })
        .collect();

    Ok(SqryPatternSearchResult {
        pattern: params.pattern.clone(),
        matches,
        total,
        truncated,
    })
}
