use crate::session::SessionManager;
use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sqry_core::query::results::QueryResults;
use std::collections::HashSet;
use std::path::Path;
use tower_lsp::lsp_types::{SymbolInformation, SymbolKind, WorkspaceSymbolParams};

const DEFAULT_PAGE_SIZE: usize = 100;
const MAX_PAGE_SIZE: usize = 200;

#[derive(Debug, Clone)]
pub struct WorkspaceSymbolItem {
    pub info: SymbolInformation,
    pub language: String,
    pub qualified_name: String,
}

#[derive(Debug, Clone)]
pub struct WorkspaceSymbolResult {
    pub items: Vec<WorkspaceSymbolItem>,
    pub total: usize,
    pub next_page_token: Option<String>,
    pub used_index: bool,
    pub page_size: usize,
    pub offset: usize,
    pub languages: Vec<String>,
    pub query: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PageToken {
    query: String,
    languages: Vec<String>,
    offset: usize,
    page_size: usize,
}

struct ParsedQuery {
    query: String,
    languages: Vec<String>,
    page_size: usize,
    offset: usize,
}

/// Handle workspace symbol requests (text search).
///
/// This handler supports multi-project workspaces per `PROJECT_ROOT_SPEC.md`.
/// It iterates over all workspace folders in the `ProjectManager` and aggregates
/// results from each project.
///
/// # Errors
///
/// Returns an error when the search operation fails or when response conversion
/// to LSP types fails.
#[allow(clippy::too_many_lines)] // Aggregates multi-project search, filtering, and paging in one flow.
pub fn handle(
    session: &SessionManager,
    params: &WorkspaceSymbolParams,
) -> Result<Option<WorkspaceSymbolResult>> {
    super::pause_for_test();

    let raw_query = params.query.trim();
    if raw_query.is_empty() {
        return Ok(Some(empty_workspace_result()));
    }

    let parsed = parse_query(raw_query);
    let page_size = clamp_page_size(parsed.page_size, session.config().search_limit);

    let search_query = search_query_or_match_all(&parsed.query);
    let search_roots = resolve_search_roots(session);
    let language_filter: HashSet<String> = parsed.languages.iter().cloned().collect();

    let search_outcome = collect_workspace_symbols(
        session,
        &search_roots,
        search_query.as_ref(),
        &language_filter,
    );

    if search_outcome.items.is_empty() {
        return Ok(Some(WorkspaceSymbolResult {
            items: search_outcome.items,
            total: 0,
            next_page_token: None,
            used_index: search_outcome.used_index,
            page_size,
            offset: 0,
            languages: parsed.languages,
            query: parsed.query,
        }));
    }

    let total = search_outcome.items.len();
    let offset = parsed.offset.min(total);
    let end = (offset + page_size).min(total);

    let page_items = search_outcome.items[offset..end].to_vec();

    let next_page_token = if end < total {
        Some(encode_page_token(&PageToken {
            query: parsed.query.clone(),
            languages: parsed.languages.clone(),
            offset: end,
            page_size,
        })?)
    } else {
        None
    };

    Ok(Some(WorkspaceSymbolResult {
        items: page_items,
        total,
        next_page_token,
        used_index: search_outcome.used_index,
        page_size,
        offset,
        languages: parsed.languages,
        query: parsed.query,
    }))
}

struct SearchRoots {
    roots: Vec<std::path::PathBuf>,
}

struct SearchOutcome {
    items: Vec<WorkspaceSymbolItem>,
    used_index: bool,
}

fn empty_workspace_result() -> WorkspaceSymbolResult {
    WorkspaceSymbolResult {
        items: Vec::new(),
        total: 0,
        next_page_token: None,
        used_index: false,
        page_size: DEFAULT_PAGE_SIZE,
        offset: 0,
        languages: Vec::new(),
        query: String::new(),
    }
}

fn search_query_or_match_all(query: &str) -> std::borrow::Cow<'_, str> {
    if query.is_empty() {
        std::borrow::Cow::Borrowed("name~=/./")
    } else {
        std::borrow::Cow::Borrowed(query)
    }
}

fn resolve_search_roots(session: &SessionManager) -> SearchRoots {
    let workspace_folders = session.project_manager().workspace_folders();
    if workspace_folders.is_empty() {
        SearchRoots {
            roots: vec![session.root_path().to_path_buf()],
        }
    } else {
        SearchRoots {
            roots: workspace_folders,
        }
    }
}

fn collect_workspace_symbols(
    session: &SessionManager,
    search_roots: &SearchRoots,
    search_query: &str,
    language_filter: &HashSet<String>,
) -> SearchOutcome {
    let executor = session.executor();
    let mut all_items = Vec::new();

    for root in &search_roots.roots {
        let Some(results) = run_query(executor.as_ref(), search_query, root) else {
            continue;
        };

        // Convert QueryResults to WorkspaceSymbolItems
        append_workspace_items_from_results(&mut all_items, &results, root, language_filter);
    }

    SearchOutcome {
        items: all_items,
        used_index: true, // Always uses CodeGraph
    }
}

fn run_query(
    executor: &sqry_core::query::QueryExecutor,
    search_query: &str,
    root: &Path,
) -> Option<QueryResults> {
    match executor.execute_on_graph(search_query, root) {
        Ok(result) => Some(result),
        Err(e) => {
            log::warn!(
                "workspace/symbol: failed to search project at {root}: {error}",
                root = root.display(),
                error = e
            );
            None
        }
    }
}

/// Convert `QueryResults` to `WorkspaceSymbolItems` using `CodeGraph` data
fn append_workspace_items_from_results(
    items: &mut Vec<WorkspaceSymbolItem>,
    results: &QueryResults,
    root: &Path,
    language_filter: &HashSet<String>,
) {
    use tower_lsp::lsp_types::{Location, Position, Range, Url};

    for m in results.iter() {
        // Check language filter if specified
        let lang = m.language().map_or_else(
            || "unknown".to_string(),
            |l| l.to_string().to_ascii_lowercase(),
        );
        if !language_filter.is_empty() && !matches_language_filter(&lang, language_filter) {
            continue;
        }

        let name = m.name().map(|s| s.to_string()).unwrap_or_default();
        let kind_str = m.kind().as_str();
        let kind = symbol_kind_from_str(kind_str);

        // Build file path
        let file_path = m.relative_path().map(|p| root.join(p)).unwrap_or_default();
        let Ok(uri) = Url::from_file_path(&file_path) else {
            continue;
        };

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

        // Use name as qualified name (no container info available in QueryMatch)
        let qualified_name = name.clone();

        #[allow(deprecated)]
        let info = SymbolInformation {
            name: name.clone(),
            kind,
            tags: None,
            deprecated: None,
            location,
            container_name: None,
        };

        items.push(WorkspaceSymbolItem {
            info,
            language: expand_language_name(&lang),
            qualified_name,
        });
    }
}

/// Expand short language names to their full forms for backward compatibility.
///
/// Language enum Display returns short forms (ts, js, py) but the LSP output
/// should use full forms (typescript, javascript, python) for consistency.
fn expand_language_name(lang: &str) -> String {
    match lang {
        "ts" => "typescript".to_string(),
        "js" => "javascript".to_string(),
        "py" => "python".to_string(),
        "rb" => "ruby".to_string(),
        "rs" => "rust".to_string(),
        "cpp" => "cpp".to_string(),
        _ => lang.to_string(),
    }
}

/// Check if a language matches the filter, handling common aliases.
///
/// Language enum Display returns short forms (ts, js, py) but users often
/// use long forms (typescript, javascript, python) in filters.
fn matches_language_filter(lang: &str, filter: &HashSet<String>) -> bool {
    if filter.contains(lang) {
        return true;
    }

    // Map short form to long forms that users might specify
    let aliases: &[&str] = match lang {
        "ts" => &["typescript"],
        "js" => &["javascript"],
        "py" => &["python"],
        "rb" => &["ruby"],
        "rs" => &["rust"],
        "cpp" => &["c++", "cxx"],
        "csharp" => &["c#"],
        _ => &[],
    };

    aliases.iter().any(|alias| filter.contains(*alias))
}

fn symbol_kind_from_str(kind: &str) -> SymbolKind {
    match kind.to_lowercase().as_str() {
        "function" => SymbolKind::FUNCTION,
        "method" => SymbolKind::METHOD,
        "class" => SymbolKind::CLASS,
        "struct" => SymbolKind::STRUCT,
        "interface" => SymbolKind::INTERFACE,
        "enum" => SymbolKind::ENUM,
        "variable" | "parameter" => SymbolKind::VARIABLE,
        "constant" => SymbolKind::CONSTANT,
        "type" | "typealias" => SymbolKind::TYPE_PARAMETER,
        "module" | "namespace" => SymbolKind::NAMESPACE,
        "property" => SymbolKind::PROPERTY,
        "import" => SymbolKind::PACKAGE,
        _ => SymbolKind::OBJECT,
    }
}

fn parse_query(input: &str) -> ParsedQuery {
    let mut languages = Vec::new();
    let mut page_size = DEFAULT_PAGE_SIZE;
    let mut token_raw = None;
    let mut query_parts = Vec::new();

    for part in input.split_whitespace() {
        if parse_language_part(part, &mut languages)
            || parse_page_token_part(part, &mut token_raw)
            || parse_page_size_part(part, &mut page_size)
        {
            continue;
        }

        query_parts.push(part);
    }

    let mut query = query_parts.join(" ");
    let mut offset = 0usize;

    apply_page_token(
        token_raw.as_ref(),
        &mut query,
        &mut languages,
        &mut offset,
        &mut page_size,
    );
    languages.sort();
    languages.dedup();

    ParsedQuery {
        query,
        languages,
        page_size,
        offset,
    }
}

fn parse_language_part(part: &str, languages: &mut Vec<String>) -> bool {
    let Some(rest) = part
        .strip_prefix("lang:")
        .or_else(|| part.strip_prefix("language:"))
        .or_else(|| part.strip_prefix("lang="))
        .or_else(|| part.strip_prefix("language="))
    else {
        return false;
    };

    languages.extend(
        rest.split(',')
            .filter_map(|lang| {
                let trimmed = lang.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_ascii_lowercase())
                }
            })
            .collect::<Vec<_>>(),
    );
    true
}

fn parse_page_token_part(part: &str, token_raw: &mut Option<String>) -> bool {
    let Some(rest) = part
        .strip_prefix("page_token:")
        .or_else(|| part.strip_prefix("page:"))
        .or_else(|| part.strip_prefix("token:"))
    else {
        return false;
    };

    if !rest.is_empty() {
        *token_raw = Some(rest.to_string());
    }
    true
}

fn parse_page_size_part(part: &str, page_size: &mut usize) -> bool {
    let Some(rest) = part
        .strip_prefix("page_size:")
        .or_else(|| part.strip_prefix("limit:"))
        .or_else(|| part.strip_prefix("pageSize:"))
    else {
        return false;
    };

    let Ok(value) = rest.parse::<usize>() else {
        return false;
    };
    if value == 0 {
        return false;
    }

    *page_size = value.min(MAX_PAGE_SIZE);
    true
}

fn apply_page_token(
    token_raw: Option<&String>,
    query: &mut String,
    languages: &mut Vec<String>,
    offset: &mut usize,
    page_size: &mut usize,
) {
    let Some(token_str) = token_raw else {
        return;
    };
    let Ok(token) = decode_page_token(token_str) else {
        return;
    };
    if !query.is_empty() && token.query != *query {
        return;
    }

    if query.is_empty() {
        query.clone_from(&token.query);
    }
    if languages.is_empty() {
        languages.clone_from(&token.languages);
    }

    if *languages == token.languages {
        *offset = token.offset;
    }

    *page_size = token.page_size.min(MAX_PAGE_SIZE);
}

fn clamp_page_size(requested: usize, config_limit: usize) -> usize {
    let capped = if requested == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        requested
    };
    capped.min(MAX_PAGE_SIZE).min(config_limit.max(1)).max(1)
}

fn encode_page_token(token: &PageToken) -> Result<String> {
    let bytes = serde_json::to_vec(token)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_page_token(raw: &str) -> Result<PageToken> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .with_context(|| "failed to decode workspace symbol page token")?;
    let token: PageToken = serde_json::from_slice(&bytes)
        .with_context(|| "invalid workspace symbol page token payload")?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_keeps_search_terms() {
        let parsed = parse_query("lang:rust page_size:5 helper");
        assert_eq!(parsed.query, "helper");
        assert_eq!(parsed.languages, vec!["rust".to_string()]);
        assert_eq!(parsed.page_size, 5);
    }
}
