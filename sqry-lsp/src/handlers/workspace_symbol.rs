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
    /// STEP_11_4 — `true` when at least one workspace search root was
    /// dropped from the search because
    /// [`crate::session::SessionManager::evaluate_handler_gate`]
    /// classified it as a member folder. The remaining results come
    /// from the still-included source roots only; consumers should
    /// surface a "partial workspace" hint to users.
    pub partial: bool,
    /// STEP_11_4 — `true` when at least one workspace search root was
    /// dropped because it classified as `Excluded`. Set independently
    /// of [`Self::partial`] so consumers can distinguish "skipped
    /// member folder" from "skipped excluded path".
    pub excluded: bool,
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
    let query_terms = workspace_query_terms(&parsed.query);

    let search_query = search_query_or_match_all(&parsed.query);
    let unfiltered_roots = resolve_search_roots(session);
    // STEP_11_4 — apply the LogicalWorkspace classification to every
    // search root before issuing any per-root query. Member folders
    // are skipped (they do not own a per-root index — their content
    // is reachable through the source roots they belong to) and
    // excluded paths are skipped outright. Each filter sets the
    // matching flag on the response so the consumer can surface a
    // "partial" / "excluded" hint instead of treating the empty
    // result as authoritative.
    let logical_workspace = session.logical_workspace();
    let mut filtered_roots = SearchRoots {
        roots: Vec::with_capacity(unfiltered_roots.roots.len()),
    };
    let mut partial = false;
    let mut excluded = false;
    for root in unfiltered_roots.roots {
        match logical_workspace.classify(&root) {
            sqry_core::workspace::Classification::Excluded => {
                excluded = true;
            }
            sqry_core::workspace::Classification::Member { .. } => {
                partial = true;
            }
            sqry_core::workspace::Classification::Source
            | sqry_core::workspace::Classification::Unknown => {
                filtered_roots.roots.push(root);
            }
        }
    }
    let search_roots = filtered_roots;
    let language_filter: HashSet<String> = parsed.languages.iter().cloned().collect();

    let search_outcome = collect_workspace_symbols(
        session,
        &search_roots,
        search_query.as_ref(),
        &language_filter,
        &query_terms,
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
            partial,
            excluded,
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
        partial,
        excluded,
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
        partial: false,
        excluded: false,
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
    query_terms: &[String],
) -> SearchOutcome {
    let executor = session.executor();
    let mut all_items = Vec::new();

    for root in &search_roots.roots {
        let Some(results) = run_query(session, executor.as_ref(), search_query, root) else {
            continue;
        };

        // Convert QueryResults to WorkspaceSymbolItems
        append_workspace_items_from_results(
            &mut all_items,
            &results,
            root,
            language_filter,
            query_terms,
        );
    }

    SearchOutcome {
        items: all_items,
        used_index: true, // Always uses CodeGraph
    }
}

fn run_query(
    session: &SessionManager,
    executor: &sqry_core::query::QueryExecutor,
    search_query: &str,
    root: &Path,
) -> Option<QueryResults> {
    // SGA06 — acquire the graph through the shared `FilesystemGraphProvider`
    // pipeline before running the workspace-symbol predicate. Each workspace
    // root produces its own `Arc<CodeGraph>` (or `None` when the index is
    // missing), so unindexed roots silently skip rather than tripping the
    // executor's own `get_or_load_graph` fallback.
    let graph = match session.graph_for_path(root) {
        Ok(Some(graph)) => graph,
        Ok(None) => {
            log::debug!(
                "workspace/symbol: no graph at {root}, skipping",
                root = root.display()
            );
            return None;
        }
        Err(e) => {
            log::warn!(
                "workspace/symbol: failed to acquire graph at {root}: {error}",
                root = root.display(),
                error = e
            );
            return None;
        }
    };

    match executor.execute_on_preloaded_graph(graph, search_query, root, None) {
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
    query_terms: &[String],
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
        let qualified_name = name.clone();
        if !matches_workspace_query(&name, &qualified_name, query_terms) {
            continue;
        }
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

fn workspace_query_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn matches_workspace_query(name: &str, qualified_name: &str, query_terms: &[String]) -> bool {
    if query_terms.is_empty() {
        return true;
    }

    let name_lower = name.to_ascii_lowercase();
    let qualified_name_lower = qualified_name.to_ascii_lowercase();

    query_terms
        .iter()
        .all(|term| name_lower.contains(term) || qualified_name_lower.contains(term))
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

    // ── parse_query ──────────────────────────────────────────────────────────

    #[test]
    fn parse_query_keeps_search_terms() {
        let parsed = parse_query("lang:rust page_size:5 helper");
        assert_eq!(parsed.query, "helper");
        assert_eq!(parsed.languages, vec!["rust".to_string()]);
        assert_eq!(parsed.page_size, 5);
    }

    #[test]
    fn parse_query_no_directives_returns_whole_string() {
        let parsed = parse_query("process data");
        assert_eq!(parsed.query, "process data");
        assert!(parsed.languages.is_empty());
        assert_eq!(parsed.page_size, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn parse_query_language_equals_variant() {
        let parsed = parse_query("language=python foo");
        assert_eq!(parsed.languages, vec!["python".to_string()]);
        assert_eq!(parsed.query, "foo");
    }

    #[test]
    fn parse_query_lang_equals_variant() {
        let parsed = parse_query("lang=go bar");
        assert_eq!(parsed.languages, vec!["go".to_string()]);
        assert_eq!(parsed.query, "bar");
    }

    #[test]
    fn parse_query_language_colon_variant() {
        let parsed = parse_query("language:java baz");
        assert_eq!(parsed.languages, vec!["java".to_string()]);
        assert_eq!(parsed.query, "baz");
    }

    #[test]
    fn parse_query_multiple_languages_comma() {
        let parsed = parse_query("lang:rust,go,python fn");
        assert_eq!(
            parsed.languages,
            vec!["go".to_string(), "python".to_string(), "rust".to_string()]
        );
        assert_eq!(parsed.query, "fn");
    }

    #[test]
    fn parse_query_multiple_lang_directives_dedup() {
        let parsed = parse_query("lang:rust lang:rust helper");
        assert_eq!(parsed.languages, vec!["rust".to_string()]);
    }

    #[test]
    fn parse_query_page_size_limit_alias() {
        let parsed = parse_query("limit:10 hello");
        assert_eq!(parsed.page_size, 10);
        assert_eq!(parsed.query, "hello");
    }

    #[test]
    fn parse_query_page_size_camel_alias() {
        let parsed = parse_query("pageSize:20 hello");
        assert_eq!(parsed.page_size, 20);
        assert_eq!(parsed.query, "hello");
    }

    #[test]
    fn parse_query_page_size_zero_ignored() {
        let parsed = parse_query("page_size:0 hello");
        // Zero is ignored so default is kept
        assert_eq!(parsed.page_size, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn parse_query_page_size_invalid_ignored() {
        let parsed = parse_query("page_size:abc hello");
        assert_eq!(parsed.page_size, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn parse_query_page_size_capped_at_max() {
        let parsed = parse_query("page_size:9999 hello");
        assert_eq!(parsed.page_size, MAX_PAGE_SIZE);
    }

    #[test]
    fn parse_query_empty_lang_comma_skipped() {
        // "lang:,rust" — the empty segment before comma must be skipped
        let parsed = parse_query("lang:,rust foo");
        assert_eq!(parsed.languages, vec!["rust".to_string()]);
    }

    // ── parse_language_part ─────────────────────────────────────────────────

    #[test]
    fn parse_language_part_returns_false_for_plain_word() {
        let mut langs: Vec<String> = Vec::new();
        assert!(!parse_language_part("hello", &mut langs));
        assert!(langs.is_empty());
    }

    #[test]
    fn parse_language_part_lang_colon() {
        let mut langs: Vec<String> = Vec::new();
        assert!(parse_language_part("lang:rust", &mut langs));
        assert_eq!(langs, vec!["rust".to_string()]);
    }

    #[test]
    fn parse_language_part_language_colon() {
        let mut langs: Vec<String> = Vec::new();
        assert!(parse_language_part("language:go", &mut langs));
        assert_eq!(langs, vec!["go".to_string()]);
    }

    #[test]
    fn parse_language_part_lang_equals() {
        let mut langs: Vec<String> = Vec::new();
        assert!(parse_language_part("lang=ts", &mut langs));
        assert_eq!(langs, vec!["ts".to_string()]);
    }

    #[test]
    fn parse_language_part_language_equals() {
        let mut langs: Vec<String> = Vec::new();
        assert!(parse_language_part("language=java", &mut langs));
        assert_eq!(langs, vec!["java".to_string()]);
    }

    // ── parse_page_token_part ────────────────────────────────────────────────

    #[test]
    fn parse_page_token_part_plain_word_returns_false() {
        let mut token: Option<String> = None;
        assert!(!parse_page_token_part("hello", &mut token));
        assert!(token.is_none());
    }

    #[test]
    fn parse_page_token_part_page_token_colon() {
        let mut token: Option<String> = None;
        assert!(parse_page_token_part("page_token:abc123", &mut token));
        assert_eq!(token, Some("abc123".to_string()));
    }

    #[test]
    fn parse_page_token_part_page_colon() {
        let mut token: Option<String> = None;
        assert!(parse_page_token_part("page:xyz", &mut token));
        assert_eq!(token, Some("xyz".to_string()));
    }

    #[test]
    fn parse_page_token_part_token_colon() {
        let mut token: Option<String> = None;
        assert!(parse_page_token_part("token:tok", &mut token));
        assert_eq!(token, Some("tok".to_string()));
    }

    #[test]
    fn parse_page_token_part_empty_rest_does_not_set() {
        let mut token: Option<String> = None;
        // prefix matches but rest is empty
        assert!(parse_page_token_part("page_token:", &mut token));
        assert!(token.is_none());
    }

    // ── parse_page_size_part ─────────────────────────────────────────────────

    #[test]
    fn parse_page_size_part_plain_word_returns_false() {
        let mut size = 100usize;
        assert!(!parse_page_size_part("hello", &mut size));
    }

    #[test]
    fn parse_page_size_part_page_size_colon() {
        let mut size = DEFAULT_PAGE_SIZE;
        assert!(parse_page_size_part("page_size:15", &mut size));
        assert_eq!(size, 15);
    }

    #[test]
    fn parse_page_size_part_limit_colon() {
        let mut size = DEFAULT_PAGE_SIZE;
        assert!(parse_page_size_part("limit:25", &mut size));
        assert_eq!(size, 25);
    }

    #[test]
    fn parse_page_size_part_page_size_camel_colon() {
        let mut size = DEFAULT_PAGE_SIZE;
        assert!(parse_page_size_part("pageSize:30", &mut size));
        assert_eq!(size, 30);
    }

    #[test]
    fn parse_page_size_part_returns_false_for_zero() {
        let mut size = DEFAULT_PAGE_SIZE;
        assert!(!parse_page_size_part("page_size:0", &mut size));
    }

    #[test]
    fn parse_page_size_part_returns_false_for_non_numeric() {
        let mut size = DEFAULT_PAGE_SIZE;
        assert!(!parse_page_size_part("page_size:nope", &mut size));
    }

    #[test]
    fn parse_page_size_part_caps_at_max() {
        let mut size = DEFAULT_PAGE_SIZE;
        assert!(parse_page_size_part("page_size:99999", &mut size));
        assert_eq!(size, MAX_PAGE_SIZE);
    }

    // ── clamp_page_size ──────────────────────────────────────────────────────

    #[test]
    fn clamp_page_size_zero_request_uses_default() {
        assert_eq!(clamp_page_size(0, 500), DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn clamp_page_size_respects_max_page_size() {
        assert_eq!(clamp_page_size(999, 1000), MAX_PAGE_SIZE);
    }

    #[test]
    fn clamp_page_size_respects_config_limit() {
        assert_eq!(clamp_page_size(50, 10), 10);
    }

    #[test]
    fn clamp_page_size_config_zero_becomes_one() {
        // config_limit.max(1) => even 0 config limit gives at least 1
        let result = clamp_page_size(5, 0);
        assert_eq!(result, 1);
    }

    #[test]
    fn clamp_page_size_normal_path() {
        assert_eq!(clamp_page_size(50, 500), 50);
    }

    // ── encode/decode_page_token ─────────────────────────────────────────────

    #[test]
    fn encode_decode_page_token_roundtrip() {
        let token = PageToken {
            query: "hello".to_string(),
            languages: vec!["rust".to_string()],
            offset: 100,
            page_size: 20,
        };
        let encoded = encode_page_token(&token).expect("encode");
        let decoded = decode_page_token(&encoded).expect("decode");
        assert_eq!(decoded.query, token.query);
        assert_eq!(decoded.languages, token.languages);
        assert_eq!(decoded.offset, token.offset);
        assert_eq!(decoded.page_size, token.page_size);
    }

    #[test]
    fn decode_page_token_invalid_base64_returns_err() {
        assert!(decode_page_token("not-valid-base64!!!").is_err());
    }

    #[test]
    fn decode_page_token_valid_base64_invalid_json_returns_err() {
        // base64 of "not-json"
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let encoded = URL_SAFE_NO_PAD.encode(b"not-json");
        assert!(decode_page_token(&encoded).is_err());
    }

    // ── apply_page_token ─────────────────────────────────────────────────────

    #[test]
    fn apply_page_token_none_does_nothing() {
        let mut query = "hello".to_string();
        let mut langs: Vec<String> = vec![];
        let mut offset = 0usize;
        let mut size = DEFAULT_PAGE_SIZE;
        apply_page_token(None, &mut query, &mut langs, &mut offset, &mut size);
        assert_eq!(query, "hello");
        assert_eq!(offset, 0);
    }

    #[test]
    fn apply_page_token_sets_offset_when_query_and_langs_match() {
        let token = PageToken {
            query: "fn".to_string(),
            languages: vec!["rust".to_string()],
            offset: 50,
            page_size: 10,
        };
        let encoded = encode_page_token(&token).expect("encode");

        let mut query = "fn".to_string();
        let mut langs = vec!["rust".to_string()];
        let mut offset = 0usize;
        let mut size = DEFAULT_PAGE_SIZE;
        apply_page_token(
            Some(&encoded),
            &mut query,
            &mut langs,
            &mut offset,
            &mut size,
        );
        assert_eq!(offset, 50);
        assert_eq!(size, 10);
    }

    #[test]
    fn apply_page_token_mismatched_query_is_ignored() {
        let token = PageToken {
            query: "other".to_string(),
            languages: vec![],
            offset: 50,
            page_size: 10,
        };
        let encoded = encode_page_token(&token).expect("encode");

        let mut query = "hello".to_string(); // different from token.query
        let mut langs: Vec<String> = vec![];
        let mut offset = 0usize;
        let mut size = DEFAULT_PAGE_SIZE;
        apply_page_token(
            Some(&encoded),
            &mut query,
            &mut langs,
            &mut offset,
            &mut size,
        );
        // Should not apply offset because query != token.query and query is not empty
        assert_eq!(offset, 0);
    }

    #[test]
    fn apply_page_token_empty_query_takes_from_token() {
        let token = PageToken {
            query: "from_token".to_string(),
            languages: vec!["go".to_string()],
            offset: 25,
            page_size: 5,
        };
        let encoded = encode_page_token(&token).expect("encode");

        let mut query = String::new(); // empty
        let mut langs: Vec<String> = vec![];
        let mut offset = 0usize;
        let mut size = DEFAULT_PAGE_SIZE;
        apply_page_token(
            Some(&encoded),
            &mut query,
            &mut langs,
            &mut offset,
            &mut size,
        );
        assert_eq!(query, "from_token");
        assert_eq!(langs, vec!["go".to_string()]);
        assert_eq!(offset, 25);
    }

    #[test]
    fn apply_page_token_invalid_token_string_is_ignored() {
        let mut query = "hello".to_string();
        let mut langs: Vec<String> = vec![];
        let mut offset = 0usize;
        let mut size = DEFAULT_PAGE_SIZE;
        let bad = "!!!invalid!!!".to_string();
        apply_page_token(Some(&bad), &mut query, &mut langs, &mut offset, &mut size);
        assert_eq!(query, "hello");
        assert_eq!(offset, 0);
    }

    // ── workspace_query_terms ────────────────────────────────────────────────

    #[test]
    fn workspace_query_terms_split_and_lowercase() {
        let terms = workspace_query_terms("Process Data");
        assert_eq!(terms, vec!["process".to_string(), "data".to_string()]);
    }

    #[test]
    fn workspace_query_terms_empty_string() {
        let terms = workspace_query_terms("");
        assert!(terms.is_empty());
    }

    #[test]
    fn workspace_query_terms_extra_whitespace() {
        let terms = workspace_query_terms("  foo   bar  ");
        assert_eq!(terms, vec!["foo".to_string(), "bar".to_string()]);
    }

    // ── matches_workspace_query ───────────────────────────────────────────────

    #[test]
    fn matches_workspace_query_requires_all_terms() {
        let terms = vec!["process".to_string(), "data".to_string()];
        assert!(matches_workspace_query(
            "process_data",
            "process_data",
            &terms
        ));
        assert!(!matches_workspace_query(
            "process_only",
            "process_only",
            &terms
        ));
    }

    #[test]
    fn matches_workspace_query_empty_terms_always_true() {
        assert!(matches_workspace_query(
            "anything",
            "qualified::anything",
            &[]
        ));
    }

    #[test]
    fn matches_workspace_query_term_in_qualified_name() {
        let terms = vec!["module".to_string()];
        // Not in name but in qualified_name
        assert!(matches_workspace_query("fn", "module::fn", &terms));
    }

    #[test]
    fn matches_workspace_query_case_insensitive() {
        let terms = vec!["hello".to_string()];
        assert!(matches_workspace_query(
            "HELLO_world",
            "HELLO_world",
            &terms
        ));
    }

    // ── expand_language_name ─────────────────────────────────────────────────

    #[test]
    fn expand_language_name_short_to_long() {
        assert_eq!(expand_language_name("ts"), "typescript");
        assert_eq!(expand_language_name("js"), "javascript");
        assert_eq!(expand_language_name("py"), "python");
        assert_eq!(expand_language_name("rb"), "ruby");
        assert_eq!(expand_language_name("rs"), "rust");
        assert_eq!(expand_language_name("cpp"), "cpp");
    }

    #[test]
    fn expand_language_name_unknown_passthrough() {
        assert_eq!(expand_language_name("go"), "go");
        assert_eq!(expand_language_name("java"), "java");
        assert_eq!(expand_language_name("haskell"), "haskell");
    }

    // ── matches_language_filter ──────────────────────────────────────────────

    #[test]
    fn matches_language_filter_direct_match() {
        let filter: HashSet<String> = ["rust".to_string()].into_iter().collect();
        assert!(matches_language_filter("rust", &filter));
    }

    #[test]
    fn matches_language_filter_alias_ts_typescript() {
        let filter: HashSet<String> = ["typescript".to_string()].into_iter().collect();
        assert!(matches_language_filter("ts", &filter));
    }

    #[test]
    fn matches_language_filter_alias_js_javascript() {
        let filter: HashSet<String> = ["javascript".to_string()].into_iter().collect();
        assert!(matches_language_filter("js", &filter));
    }

    #[test]
    fn matches_language_filter_alias_py_python() {
        let filter: HashSet<String> = ["python".to_string()].into_iter().collect();
        assert!(matches_language_filter("py", &filter));
    }

    #[test]
    fn matches_language_filter_alias_rb_ruby() {
        let filter: HashSet<String> = ["ruby".to_string()].into_iter().collect();
        assert!(matches_language_filter("rb", &filter));
    }

    #[test]
    fn matches_language_filter_alias_rs_rust() {
        let filter: HashSet<String> = ["rust".to_string()].into_iter().collect();
        assert!(matches_language_filter("rs", &filter));
    }

    #[test]
    fn matches_language_filter_alias_cpp_cxx() {
        let filter: HashSet<String> = ["cxx".to_string()].into_iter().collect();
        assert!(matches_language_filter("cpp", &filter));
    }

    #[test]
    fn matches_language_filter_alias_cpp_plus() {
        let filter: HashSet<String> = ["c++".to_string()].into_iter().collect();
        assert!(matches_language_filter("cpp", &filter));
    }

    #[test]
    fn matches_language_filter_alias_csharp_hash() {
        let filter: HashSet<String> = ["c#".to_string()].into_iter().collect();
        assert!(matches_language_filter("csharp", &filter));
    }

    #[test]
    fn matches_language_filter_no_match_returns_false() {
        let filter: HashSet<String> = ["go".to_string()].into_iter().collect();
        assert!(!matches_language_filter("rust", &filter));
    }

    #[test]
    fn matches_language_filter_unknown_lang_no_aliases() {
        let filter: HashSet<String> = ["haskell".to_string()].into_iter().collect();
        // "hs" has no aliases mapped — direct "hs" != "haskell"
        assert!(!matches_language_filter("hs", &filter));
    }

    // ── symbol_kind_from_str ─────────────────────────────────────────────────

    #[test]
    fn symbol_kind_from_str_all_known_kinds() {
        assert_eq!(symbol_kind_from_str("function"), SymbolKind::FUNCTION);
        assert_eq!(symbol_kind_from_str("method"), SymbolKind::METHOD);
        assert_eq!(symbol_kind_from_str("class"), SymbolKind::CLASS);
        assert_eq!(symbol_kind_from_str("struct"), SymbolKind::STRUCT);
        assert_eq!(symbol_kind_from_str("interface"), SymbolKind::INTERFACE);
        assert_eq!(symbol_kind_from_str("enum"), SymbolKind::ENUM);
        assert_eq!(symbol_kind_from_str("variable"), SymbolKind::VARIABLE);
        assert_eq!(symbol_kind_from_str("parameter"), SymbolKind::VARIABLE);
        assert_eq!(symbol_kind_from_str("constant"), SymbolKind::CONSTANT);
        assert_eq!(symbol_kind_from_str("type"), SymbolKind::TYPE_PARAMETER);
        assert_eq!(
            symbol_kind_from_str("typealias"),
            SymbolKind::TYPE_PARAMETER
        );
        assert_eq!(symbol_kind_from_str("module"), SymbolKind::NAMESPACE);
        assert_eq!(symbol_kind_from_str("namespace"), SymbolKind::NAMESPACE);
        assert_eq!(symbol_kind_from_str("property"), SymbolKind::PROPERTY);
        assert_eq!(symbol_kind_from_str("import"), SymbolKind::PACKAGE);
        assert_eq!(symbol_kind_from_str("unknown_kind"), SymbolKind::OBJECT);
    }

    #[test]
    fn symbol_kind_from_str_case_insensitive() {
        assert_eq!(symbol_kind_from_str("FUNCTION"), SymbolKind::FUNCTION);
        assert_eq!(symbol_kind_from_str("Method"), SymbolKind::METHOD);
        assert_eq!(symbol_kind_from_str("CLASS"), SymbolKind::CLASS);
    }

    // ── search_query_or_match_all ────────────────────────────────────────────

    #[test]
    fn search_query_or_match_all_empty_returns_match_all() {
        let result = search_query_or_match_all("");
        assert_eq!(result, "name~=/./");
    }

    #[test]
    fn search_query_or_match_all_non_empty_returns_same() {
        let result = search_query_or_match_all("process");
        assert_eq!(result, "process");
    }

    // ── empty_workspace_result ───────────────────────────────────────────────

    #[test]
    fn empty_workspace_result_has_expected_defaults() {
        let result = empty_workspace_result();
        assert!(result.items.is_empty());
        assert_eq!(result.total, 0);
        assert!(result.next_page_token.is_none());
        assert!(!result.used_index);
        assert_eq!(result.page_size, DEFAULT_PAGE_SIZE);
        assert_eq!(result.offset, 0);
        assert!(result.languages.is_empty());
        assert!(result.query.is_empty());
        assert!(!result.partial);
        assert!(!result.excluded);
    }
}
