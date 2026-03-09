//! Hierarchical search handler for LSP.
//!
//! Implements RAG-optimized search with file → container → symbol grouping.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::protocol::{
    SqryHierarchicalFileGroup, SqryHierarchicalSearchParams, SqryHierarchicalSearchResult,
    SqryHierarchicalSymbol,
};
use crate::session::SessionManager;

/// Default maximum files to return
const DEFAULT_MAX_FILES: usize = 20;
/// Default maximum symbols per file
const DEFAULT_MAX_SYMBOLS_PER_FILE: usize = 50;
/// Default maximum total symbols
const DEFAULT_MAX_TOTAL_SYMBOLS: usize = 200;

/// Internal struct for holding match data during processing
struct MatchData {
    name: String,
    kind: String,
    file_path: String,
    language: String,
    start_line: u32,
    start_column: u32,
    end_line: u32,
    end_column: u32,
    signature: Option<String>,
    score: f64,
}

/// Execute hierarchical search.
///
/// Groups search results by file → container → symbol for RAG-optimized retrieval.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, the query is empty,
/// or the search execution fails.
#[allow(clippy::too_many_lines)]
pub fn execute(
    session: &SessionManager,
    params: &SqryHierarchicalSearchParams,
) -> Result<SqryHierarchicalSearchResult> {
    let root = session.resolve_path(params.path.as_deref())?;
    let query = params.query.trim();

    if query.is_empty() {
        anyhow::bail!("query cannot be empty");
    }

    log::debug!(
        "Executing hierarchical search: query='{}', root={}",
        query,
        root.display()
    );

    // Get limits
    let max_files = params.max_files.unwrap_or(DEFAULT_MAX_FILES);
    let max_symbols_per_file = params
        .max_symbols_per_file
        .unwrap_or(DEFAULT_MAX_SYMBOLS_PER_FILE);
    let max_total_symbols = params
        .max_total_symbols
        .unwrap_or(DEFAULT_MAX_TOTAL_SYMBOLS);

    // Execute search using CodeGraph
    let executor = session.executor();
    let query_results = executor
        .execute_on_graph(query, &root)
        .with_context(|| format!("failed to execute sqry query '{query}'"))?;

    // Convert QueryResults to MatchData with scores
    let mut matches: Vec<MatchData> = query_results
        .iter()
        .filter_map(|m| {
            let name = m.name().map(|s| s.to_string()).unwrap_or_default();
            let kind = m.kind().as_str().to_string();
            let file_path = m.relative_path()?.display().to_string();
            let language = m.language().map_or_else(
                || "unknown".to_string(),
                |l| l.to_string().to_ascii_lowercase(),
            );

            let score = calculate_relevance_score(&name, &name, query);

            Some(MatchData {
                name,
                kind,
                file_path,
                language,
                start_line: m.start_line(),
                start_column: m.start_column(),
                end_line: m.end_line(),
                end_column: m.end_column(),
                signature: m.signature().map(|s| s.to_string()),
                score,
            })
        })
        .collect();

    // Apply filters
    if let Some(ref languages) = params.languages {
        matches.retain(|m| {
            languages
                .iter()
                .any(|l| l.eq_ignore_ascii_case(&m.language))
        });
    }

    if let Some(ref kinds) = params.symbol_kinds {
        matches.retain(|m| kinds.iter().any(|k| k.eq_ignore_ascii_case(&m.kind)));
    }

    if let Some(min_score) = params.score_min {
        matches.retain(|m| m.score >= min_score);
    }

    // Group by file
    let mut by_file: HashMap<String, Vec<MatchData>> = HashMap::new();
    for m in matches {
        by_file.entry(m.file_path.clone()).or_default().push(m);
    }

    // Build file groups
    let mut files: Vec<SqryHierarchicalFileGroup> = Vec::new();
    let mut total_symbols = 0usize;

    // Sort files by max score descending
    let mut file_entries: Vec<_> = by_file.into_iter().collect();
    file_entries.sort_by(|a, b| {
        let max_a = a.1.iter().map(|m| m.score).fold(0.0_f64, f64::max);
        let max_b = b.1.iter().map(|m| m.score).fold(0.0_f64, f64::max);
        max_b
            .partial_cmp(&max_a)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let truncated = file_entries.len() > max_files;

    for (file_path, mut file_matches) in file_entries.into_iter().take(max_files) {
        if total_symbols >= max_total_symbols {
            break;
        }

        // Sort by score
        file_matches.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Limit per file
        let remaining = max_total_symbols.saturating_sub(total_symbols);
        let limit = max_symbols_per_file.min(remaining);
        file_matches.truncate(limit);

        let language = file_matches
            .first()
            .map(|m| m.language.clone())
            .unwrap_or_default();

        let max_score = file_matches.iter().map(|m| m.score).fold(0.0_f64, f64::max);

        // Convert to symbols (no container grouping since we don't have qualified names)
        let top_level: Vec<SqryHierarchicalSymbol> = file_matches
            .iter()
            .filter_map(|m| match_to_hierarchical(m, &root, &file_path).ok())
            .collect();

        let symbol_count = top_level.len();
        total_symbols += symbol_count;

        files.push(SqryHierarchicalFileGroup {
            path: file_path,
            language,
            symbol_count,
            max_score,
            containers: Vec::new(), // No container grouping without qualified names
            top_level_symbols: top_level,
        });
    }

    let total_files = files.len();

    Ok(SqryHierarchicalSearchResult {
        query: params.query.clone(),
        files,
        total_symbols,
        total_files,
        truncated,
    })
}

/// Calculate simple relevance score based on name matching.
fn calculate_relevance_score(name: &str, qualified_name: &str, query: &str) -> f64 {
    let query_lower = query.to_lowercase();
    let name_lower = name.to_lowercase();
    let qname_lower = qualified_name.to_lowercase();

    if name_lower == query_lower {
        1.0
    } else if name_lower.starts_with(&query_lower) {
        0.9
    } else if name_lower.contains(&query_lower) {
        0.7
    } else if qname_lower.contains(&query_lower) {
        0.5
    } else {
        0.3 // Some match from semantic search
    }
}

/// Convert a `MatchData` to `HierarchicalSymbol`.
fn match_to_hierarchical(
    m: &MatchData,
    workspace_root: &Path,
    file_path: &str,
) -> Result<SqryHierarchicalSymbol> {
    let full_path = workspace_root.join(file_path);
    let uri = Url::from_file_path(&full_path)
        .map_err(|()| anyhow::anyhow!("failed to create URI for {}", full_path.display()))?;

    let start_line = m.start_line.saturating_sub(1);
    let end_line = m.end_line.saturating_sub(1);

    let location = Location {
        uri,
        range: Range {
            start: Position::new(start_line, m.start_column.saturating_sub(1)),
            end: Position::new(end_line, m.end_column.saturating_sub(1)),
        },
    };

    Ok(SqryHierarchicalSymbol {
        name: m.name.clone(),
        qualified_name: m.name.clone(), // No qualified name available
        kind: m.kind.clone(),
        score: m.score,
        location,
        signature: m.signature.clone(),
    })
}
