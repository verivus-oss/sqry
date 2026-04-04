//! Symbol search command implementation

use crate::args::Cli;
use crate::commands::graph::loader::{GraphLoadConfig, load_unified_graph_for_cli};
use crate::index_discovery::find_nearest_index;
use crate::output::{
    DisplaySymbol, FormatterMetadata, JsonSymbol, OutputStreams, create_formatter,
};
use anyhow::{Context, Result};
use regex::RegexBuilder;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::json_response::{Filters, FuzzyFilters, Stats, StreamEvent};
use sqry_core::search::fuzzy::{CandidateGenerator, FuzzyConfig};
use sqry_core::search::matcher::{FuzzyMatcher, MatchAlgorithm, MatchConfig};
use sqry_core::search::trigram::TrigramIndex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// A symbol paired with its fuzzy match score.
type ScoredSymbol = (DisplaySymbol, f64);

/// Apply kind and language filters to symbols.
fn apply_search_filters(cli: &Cli, symbols: &mut Vec<DisplaySymbol>) {
    // Filter by symbol type if specified
    if let Some(kind) = cli.kind {
        let target_type_str = kind.to_string().to_lowercase();
        symbols.retain(|s| s.kind.to_lowercase() == target_type_str);
    }

    // Filter by language if specified
    if let Some(ref lang) = cli.lang {
        symbols.retain(|s| {
            s.file_path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches_language(ext, lang))
        });
    }
}

/// Build search metadata for output formatting.
fn build_search_metadata(
    cli: &Cli,
    pattern: &str,
    scope_info: Option<&FuzzySearchScopeInfo>,
    index_age_seconds: Option<u64>,
    total_matches: usize,
    execution_time: std::time::Duration,
) -> FormatterMetadata {
    let (used_ancestor_index, filtered_to) = if let Some(scope) = scope_info {
        // Include scope info when any filtering was applied
        let used_ancestor = if scope.used_ancestor_index || scope.filtered_to.is_some() {
            Some(scope.used_ancestor_index)
        } else {
            None
        };
        (used_ancestor, scope.filtered_to.clone())
    } else {
        (None, None)
    };

    FormatterMetadata {
        pattern: Some(pattern.to_string()),
        total_matches,
        execution_time,
        filters: build_filters(cli),
        index_age_seconds,
        used_ancestor_index,
        filtered_to,
    }
}

/// Run symbol search command.
/// P2-3 Step 2e: Language filtering uses `file_path()` without index context - allowed
///
/// # Errors
/// Returns an error if search execution fails or output cannot be written.
pub fn run_search(cli: &Cli, pattern: &str, search_path: &str) -> Result<()> {
    // Handle JSON streaming mode separately (fuzzy only, enforced by clap)
    if cli.json_stream {
        return run_json_stream_search(cli, pattern, search_path);
    }

    let start_time = Instant::now();

    // Branch based on search mode, capturing index age and scope info if available
    let (mut all_symbols, index_age_seconds, scope_info) = if cli.fuzzy {
        let (scored_symbols, age, scope) = run_fuzzy_search(cli, pattern, search_path)?;
        let symbols = scored_symbols.into_iter().map(|(s, _)| s).collect();
        (symbols, Some(age), Some(scope))
    } else {
        (run_regular_search(cli, pattern, search_path)?, None, None)
    };

    apply_search_filters(cli, &mut all_symbols);

    // Handle count-only mode
    if cli.count {
        println!("{} matches found", all_symbols.len());
        return Ok(());
    }

    // Apply limit if specified
    let total_matches = all_symbols.len();

    // Optional sorting (opt-in)
    if let Some(sort_field) = cli.sort {
        crate::commands::sort::sort_symbols(&mut all_symbols, sort_field);
    }

    let limit = cli.limit.unwrap_or(if cli.fuzzy { 50 } else { 100 });
    let symbols_to_output = if all_symbols.len() > limit {
        all_symbols.truncate(limit);
        all_symbols
    } else {
        all_symbols
    };

    let execution_time = start_time.elapsed();

    let metadata = build_search_metadata(
        cli,
        pattern,
        scope_info.as_ref(),
        index_age_seconds,
        total_matches,
        execution_time,
    );

    let formatter = create_formatter(cli);

    // Output results using streams with optional pager support
    let mut streams = OutputStreams::with_pager(cli.pager_config());
    formatter.format(&symbols_to_output, Some(&metadata), &mut streams)?;

    // If truncated and not JSON, inform user
    if !cli.json && total_matches > limit {
        eprintln!("\nShowing {limit} of {total_matches} matches (use --limit to adjust)");
    }

    // Finalize pager (flushes buffer, waits for pager if spawned)
    // This propagates non-zero pager exit codes to the CLI exit code
    streams.finish_checked()
}

/// Build filters metadata from CLI flags
fn build_filters(cli: &Cli) -> Filters {
    Filters {
        kind: cli.kind.map(|k| k.to_string()),
        lang: cli.lang.clone(),
        ignore_case: cli.ignore_case,
        exact: cli.exact,
        fuzzy: if cli.fuzzy {
            Some(FuzzyFilters {
                algorithm: cli.fuzzy_algorithm.clone(),
                threshold: cli.fuzzy_threshold,
                max_candidates: Some(cli.fuzzy_max_candidates),
            })
        } else {
            None
        },
    }
}

fn language_from_path(path: &Path) -> &'static str {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map_or("unknown", |ext| match ext.to_lowercase().as_str() {
            "rs" => "rust",
            "js" | "mjs" | "cjs" => "javascript",
            "ts" | "mts" | "cts" => "typescript",
            "jsx" => "javascriptreact",
            "tsx" => "typescriptreact",
            "py" | "pyw" => "python",
            "rb" => "ruby",
            "go" => "go",
            "java" => "java",
            "kt" | "kts" => "kotlin",
            "scala" | "sc" => "scala",
            "c" | "h" => "c",
            "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
            "cs" => "csharp",
            "php" => "php",
            "swift" => "swift",
            "sql" => "sql",
            "dart" => "dart",
            "lua" => "lua",
            "sh" | "bash" | "zsh" => "shell",
            "pl" | "pm" => "perl",
            "groovy" | "gvy" => "groovy",
            "ex" | "exs" => "elixir",
            "r" | "R" => "r",
            "hs" | "lhs" => "haskell",
            "svelte" => "svelte",
            "vue" => "vue",
            "zig" => "zig",
            "css" | "scss" | "sass" | "less" => "css",
            "html" | "htm" => "html",
            "tf" | "tfvars" => "terraform",
            "pp" => "puppet",
            "pls" | "plb" | "pck" => "plsql",
            "cls" | "trigger" => "apex",
            "abap" => "abap",
            _ => "unknown",
        })
}

/// Check if file extension matches language
fn matches_language(ext: &str, lang: &str) -> bool {
    let ext_lower = ext.to_lowercase();
    let lang_lower = lang.to_lowercase();

    match lang_lower.as_str() {
        // Tier 0 languages (original core set)
        "rust" | "rs" => ext_lower == "rs",
        "javascript" | "js" => matches!(ext_lower.as_str(), "js" | "jsx" | "mjs" | "cjs"),
        "typescript" | "ts" => matches!(ext_lower.as_str(), "ts" | "tsx"),
        "python" | "py" => matches!(ext_lower.as_str(), "py" | "pyi" | "pyw"),
        "go" => ext_lower == "go",
        "java" => ext_lower == "java",

        // Tier 1 languages
        "swift" => ext_lower == "swift",
        "c" => matches!(ext_lower.as_str(), "c" | "h"),
        "cpp" | "c++" | "cxx" => {
            matches!(
                ext_lower.as_str(),
                "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "h"
            )
        }
        "csharp" | "c#" | "cs" => matches!(ext_lower.as_str(), "cs" | "csx"),
        "dart" => ext_lower == "dart",
        "kotlin" | "kt" => matches!(ext_lower.as_str(), "kt" | "kts"),
        "ruby" | "rb" => matches!(ext_lower.as_str(), "rb" | "rake" | "gemspec"),
        "scala" => matches!(ext_lower.as_str(), "scala" | "sc"),
        "php" => ext_lower == "php",

        // Tier 2 languages
        "lua" => ext_lower == "lua",
        "elixir" | "ex" => matches!(ext_lower.as_str(), "ex" | "exs"),
        "haskell" | "hs" => matches!(ext_lower.as_str(), "hs" | "lhs"),
        "perl" | "pl" => matches!(ext_lower.as_str(), "pl" | "pm"),
        "r" => ext_lower == "r",
        "shell" | "sh" | "bash" => matches!(ext_lower.as_str(), "sh" | "bash" | "zsh"),
        "zig" => ext_lower == "zig",
        "groovy" => matches!(ext_lower.as_str(), "groovy" | "gvy" | "gy" | "gsh"),

        // Frontend / markup
        "vue" => ext_lower == "vue",
        "svelte" => ext_lower == "svelte",
        "html" => matches!(ext_lower.as_str(), "html" | "htm"),
        "css" => matches!(ext_lower.as_str(), "css" | "scss" | "sass" | "less"),

        // IaC languages
        "terraform" | "tf" | "hcl" => {
            matches!(ext_lower.as_str(), "tf" | "tfvars" | "hcl")
        }
        "puppet" | "pp" => ext_lower == "pp",

        // Data / platform-specific languages
        "sql" => ext_lower == "sql",
        "servicenow" | "servicenow-xanadu" | "servicenow-xanadu-js" | "snjs" => ext_lower == "snjs",
        "apex" | "salesforce" => matches!(ext_lower.as_str(), "cls" | "trigger"),
        "abap" => ext_lower == "abap",
        "plsql" | "oracle-plsql" => matches!(ext_lower.as_str(), "pks" | "pkb" | "pls"),

        // Default: try exact match
        _ => ext_lower == lang_lower,
    }
}

/// Run regular (non-fuzzy) symbol search
fn run_regular_search(cli: &Cli, pattern: &str, search_path: &str) -> Result<Vec<DisplaySymbol>> {
    // Load unified graph
    let search_path_path = Path::new(search_path);
    let index_location = find_nearest_index(search_path_path);
    let index_root = index_location
        .as_ref()
        .map_or(search_path_path, |loc| loc.index_root.as_path());

    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(index_root, &config, cli)
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    // Build regex for pattern matching if regex mode
    let pattern_regex = build_pattern_regex(cli, pattern)?;

    // Find matching nodes
    let mut matches = Vec::new();
    let strings = graph.strings();
    let indices = graph.indices();

    if let Some(regex) = pattern_regex {
        // Regex search: scan all interned strings
        for (str_id, s) in strings.iter() {
            if regex.is_match(s) {
                // If matches, get all nodes with this name
                matches.extend_from_slice(indices.by_qualified_name(str_id));
                matches.extend_from_slice(indices.by_name(str_id));
            }
        }
    } else {
        // Substring search using optimized graph method
        let node_ids = graph.snapshot().find_by_pattern(pattern);
        matches.extend(node_ids);
    }

    // Deduplicate node IDs
    matches.sort_unstable();
    matches.dedup();

    // Convert to DisplaySymbols
    let mut all_symbols = Vec::with_capacity(matches.len());

    for node_id in matches {
        if let Some(symbol) = convert_node_to_display_symbol(&graph, node_id) {
            all_symbols.push(symbol);
        }
    }

    Ok(all_symbols)
}

fn build_pattern_regex(cli: &Cli, pattern: &str) -> Result<Option<regex::Regex>> {
    if cli.exact {
        return Ok(None);
    }

    let regex = RegexBuilder::new(pattern)
        .case_insensitive(cli.ignore_case)
        .build()
        .context("Invalid regex pattern")?;
    Ok(Some(regex))
}

// Helper to convert CodeGraph node to DisplaySymbol
fn convert_node_to_display_symbol(
    graph: &CodeGraph,
    node_id: sqry_core::graph::unified::node::NodeId,
) -> Option<DisplaySymbol> {
    let entry = graph.nodes().get(node_id)?;
    let strings = graph.strings();
    let files = graph.files();

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let file_path = files
        .resolve(entry.file)
        .map(|s| PathBuf::from(s.as_ref()))
        .unwrap_or_default();

    let language = language_from_path(&file_path).to_string();

    let mut metadata = HashMap::new();
    metadata.insert(
        "__raw_file_path".to_string(),
        file_path.to_string_lossy().to_string(),
    );
    metadata.insert("__raw_language".to_string(), language.clone());

    let qualified_name = entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| name.clone(), |s| s.to_string());

    Some(DisplaySymbol {
        name,
        qualified_name,
        kind: node_kind_to_string(entry.kind).to_string(),
        file_path,
        start_line: entry.start_line as usize,
        start_column: entry.start_column as usize,
        end_line: entry.end_line as usize,
        end_column: entry.end_column as usize,
        metadata,
        caller_identity: None,
        callee_identity: None,
    })
}

/// Convert `NodeKind` to lowercase string for display.
fn node_kind_to_string(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Class => "class",
        NodeKind::Interface => "interface",
        NodeKind::Trait => "trait",
        NodeKind::Module => "module",
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Type => "type",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::EnumVariant => "enum_variant",
        NodeKind::Macro => "macro",
        NodeKind::Parameter => "parameter",
        NodeKind::Property => "property",
        NodeKind::Import => "import",
        NodeKind::Export => "export",
        NodeKind::Component => "component",
        NodeKind::Service => "service",
        NodeKind::Resource => "resource",
        NodeKind::Endpoint => "endpoint",
        NodeKind::Test => "test",
        NodeKind::CallSite => "call_site",
        NodeKind::StyleRule => "style_rule",
        NodeKind::StyleAtRule => "style_at_rule",
        NodeKind::StyleVariable => "style_variable",
        NodeKind::Lifetime => "lifetime",
        NodeKind::TypeParameter => "type_parameter",
        NodeKind::Annotation => "annotation",
        NodeKind::AnnotationValue => "annotation_value",
        NodeKind::LambdaTarget => "lambda_target",
        NodeKind::JavaModule => "java_module",
        NodeKind::EnumConstant => "enum_constant",
        NodeKind::Other => "other",
    }
}

/// Scope info returned from fuzzy search for JSON output
struct FuzzySearchScopeInfo {
    used_ancestor_index: bool,
    filtered_to: Option<String>,
}

/// Resolved index location for fuzzy search.
struct FuzzyIndexResolution {
    index_root: PathBuf,
    scope_filter: Option<PathBuf>,
    is_file_query: bool,
    scope_info: FuzzySearchScopeInfo,
}

/// Resolve index location and scope filter for fuzzy search.
fn resolve_fuzzy_index(search_path: &Path) -> FuzzyIndexResolution {
    let index_location = find_nearest_index(search_path);

    if let Some(ref loc) = index_location {
        let scope = if loc.requires_scope_filter {
            loc.relative_scope()
        } else {
            None
        };
        let info = FuzzySearchScopeInfo {
            used_ancestor_index: loc.is_ancestor,
            filtered_to: scope.as_ref().map(|p| {
                if loc.is_file_query {
                    p.to_string_lossy().into_owned()
                } else {
                    format!("{}/**", p.display())
                }
            }),
        };
        FuzzyIndexResolution {
            index_root: loc.index_root.clone(),
            scope_filter: scope,
            is_file_query: loc.is_file_query,
            scope_info: info,
        }
    } else {
        FuzzyIndexResolution {
            index_root: search_path.to_path_buf(),
            scope_filter: None,
            is_file_query: false,
            scope_info: FuzzySearchScopeInfo {
                used_ancestor_index: false,
                filtered_to: None,
            },
        }
    }
}

/// Build a `TrigramIndex` from all interned strings in the graph.
fn build_trigram_index_from_graph(graph: &CodeGraph) -> Arc<TrigramIndex> {
    let mut trigram_index = TrigramIndex::new();
    for (str_id, s) in graph.strings().iter() {
        trigram_index.add_symbol(str_id.index() as usize, s);
    }
    Arc::new(trigram_index)
}

/// Run fuzzy symbol search using index.
/// Returns (scored symbols, `index_age_seconds`, `scope_info`).
fn run_fuzzy_search(
    cli: &Cli,
    pattern: &str,
    search_path: &str,
) -> Result<(Vec<ScoredSymbol>, u64, FuzzySearchScopeInfo)> {
    let search_path_path = Path::new(search_path);

    // Index ancestor discovery
    let resolution = resolve_fuzzy_index(search_path_path);
    let FuzzyIndexResolution {
        index_root,
        scope_filter,
        is_file_query,
        scope_info,
    } = resolution;

    let config = GraphLoadConfig::default();
    let graph = load_unified_graph_for_cli(&index_root, &config, cli)
        .context("Failed to load graph. Run 'sqry index' to build the graph.")?;

    // Age of graph (approximate, since we don't have file metadata here easily, return 0 for now)
    let age_seconds = 0;

    // Build TrigramIndex from graph strings on the fly
    let trigram_index_arc = build_trigram_index_from_graph(&graph);

    let algorithm = parse_fuzzy_algorithm(&cli.fuzzy_algorithm)?;
    let fuzzy_config = build_fuzzy_config(cli, 0.1);
    let match_config = build_match_config(cli, algorithm);

    // Create candidate generator
    let generator = CandidateGenerator::with_config(trigram_index_arc, fuzzy_config);

    maybe_log_fuzzy_config(cli, algorithm);

    // Generate candidates (StringIds as usize)
    let candidate_ids = generator.generate(pattern);

    if candidate_ids.is_empty() {
        return Ok((Vec::new(), age_seconds, scope_info));
    }

    // Match and score
    let matcher = FuzzyMatcher::with_config(match_config.clone());

    // Pre-resolve strings to manage lifetimes
    let resolved_candidates: Vec<(usize, Arc<str>)> = candidate_ids
        .iter()
        .filter_map(|&id| {
            let str_id = u32::try_from(id).ok()?;
            let str_id = sqry_core::graph::unified::string::StringId::new(str_id);
            graph.strings().resolve(str_id).map(|s| (id, s))
        })
        .collect();

    let candidate_targets = resolved_candidates.iter().map(|(id, s)| (*id, s.as_ref()));

    // Score candidates
    let match_results = matcher.match_many(pattern, candidate_targets);

    // Convert to DisplaySymbols
    let mut symbols = Vec::new();
    let indices = graph.indices();

    for result in match_results {
        let Ok(str_id) = u32::try_from(result.entry_id) else {
            continue;
        };
        let str_id = sqry_core::graph::unified::string::StringId::new(str_id);

        // Find nodes with this name
        // We check both qualified and simple names because TrigramIndex was built from all strings.
        // A string might be a qualified name or a simple name.
        // If it's a qualified name, `by_qualified_name` will find it.
        // If it's a simple name, `by_name` will find it.

        let mut node_ids = Vec::new();
        node_ids.extend_from_slice(indices.by_qualified_name(str_id));
        node_ids.extend_from_slice(indices.by_name(str_id));
        node_ids.sort_unstable();
        node_ids.dedup();

        for node_id in node_ids {
            if let Some(symbol) = convert_node_to_display_symbol(&graph, node_id) {
                // We need to keep the score to sort.
                // We return (DisplaySymbol, score) internally then sort.
                symbols.push((symbol, result.score));
            }
        }
    }

    // Sort by score descending
    symbols.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    maybe_log_fuzzy_results(symbols.len());

    let mut final_symbols = symbols;

    // Post-filter results to query scope if using ancestor index
    if let Some(ref scope) = scope_filter {
        filter_fuzzy_results_by_scope(&mut final_symbols, scope, is_file_query);
    }

    Ok((final_symbols, age_seconds, scope_info))
}

/// Filter fuzzy search results to only include symbols within the given scope.
fn filter_fuzzy_results_by_scope(
    symbols: &mut Vec<ScoredSymbol>,
    scope: &Path,
    is_file_query: bool,
) {
    symbols.retain(|(symbol, _)| {
        if is_file_query {
            symbol.file_path == scope
        } else {
            symbol.file_path.starts_with(scope)
        }
    });
}

fn run_json_stream_search(cli: &Cli, pattern: &str, search_path: &str) -> Result<()> {
    let (mut symbols, age_seconds, scope_info) = run_fuzzy_search(cli, pattern, search_path)?;

    // Apply kind/language filters (same semantics as the non-streaming path).
    apply_scored_search_filters(cli, &mut symbols);

    let limit = cli.limit.unwrap_or(50);
    let mut count = 0;

    for (symbol, score) in symbols.iter().take(limit) {
        let json_symbol = JsonSymbol::from(symbol);
        let event = StreamEvent::PartialResult {
            result: json_symbol,
            score: *score,
        };
        let json = serde_json::to_string(&event)?;
        println!("{json}");
        count += 1;
    }

    emit_stream_summary(symbols.len(), count, age_seconds, Some(&scope_info))?;

    Ok(())
}

/// Apply kind and language filters to scored symbols.
fn apply_scored_search_filters(cli: &Cli, symbols: &mut Vec<ScoredSymbol>) {
    if let Some(kind) = cli.kind {
        let target_type_str = kind.to_string().to_lowercase();
        symbols.retain(|(s, _)| s.kind.to_lowercase() == target_type_str);
    }

    if let Some(ref lang) = cli.lang {
        symbols.retain(|(s, _)| {
            s.file_path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches_language(ext, lang))
        });
    }
}

fn parse_fuzzy_algorithm(algorithm: &str) -> Result<MatchAlgorithm> {
    match algorithm.to_lowercase().as_str() {
        "levenshtein" => Ok(MatchAlgorithm::Levenshtein),
        "jaro-winkler" | "jaro_winkler" => Ok(MatchAlgorithm::JaroWinkler),
        _ => anyhow::bail!(
            "Unknown fuzzy algorithm '{algorithm}'. Use 'levenshtein' or 'jaro-winkler'."
        ),
    }
}

fn build_fuzzy_config(cli: &Cli, min_similarity: f64) -> FuzzyConfig {
    FuzzyConfig {
        max_candidates: cli.fuzzy_max_candidates,
        min_similarity,
    }
}

fn build_match_config(cli: &Cli, algorithm: MatchAlgorithm) -> MatchConfig {
    MatchConfig {
        algorithm,
        min_score: cli.fuzzy_threshold,
        case_sensitive: !cli.ignore_case,
    }
}

fn maybe_log_fuzzy_config(cli: &Cli, algorithm: MatchAlgorithm) {
    if std::env::var("RUST_LOG").is_ok() {
        eprintln!("[DEBUG] Using fuzzy algorithm: {algorithm:?}");
        eprintln!("[DEBUG] Min score threshold: {}", cli.fuzzy_threshold);
    }
}

fn maybe_log_fuzzy_results(count: usize) {
    if std::env::var("RUST_LOG").is_ok() {
        eprintln!("[DEBUG] Found {count} fuzzy matches");
    }
}

fn emit_stream_summary(
    final_count: usize,
    total_streamed: usize,
    age_seconds: u64,
    scope_info: Option<&FuzzySearchScopeInfo>,
) -> Result<()> {
    let mut stats = Stats::new(final_count, total_streamed).with_index_age(age_seconds);
    // Add scope info if filtering was applied
    if let Some(scope) = scope_info
        && (scope.used_ancestor_index || scope.filtered_to.is_some())
    {
        stats = stats.with_scope_info(scope.used_ancestor_index, scope.filtered_to.clone());
    }
    let summary = StreamEvent::<JsonSymbol>::FinalSummary { stats };
    let json = serde_json::to_string(&summary)?;
    println!("{json}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_language_rust() {
        assert!(matches_language("rs", "rust"));
        assert!(matches_language("rs", "Rust"));
        assert!(matches_language("rs", "rs"));
        assert!(!matches_language("js", "rust"));
    }

    #[test]
    fn test_matches_language_javascript() {
        assert!(matches_language("js", "javascript"));
        assert!(matches_language("jsx", "javascript"));
        assert!(matches_language("js", "js"));
        assert!(!matches_language("ts", "javascript"));
    }

    #[test]
    fn test_matches_language_typescript() {
        assert!(matches_language("ts", "typescript"));
        assert!(matches_language("tsx", "typescript"));
        assert!(matches_language("ts", "ts"));
        assert!(!matches_language("js", "typescript"));
    }

    #[test]
    fn test_matches_language_swift() {
        assert!(matches_language("swift", "swift"));
        assert!(matches_language("swift", "Swift"));
        assert!(!matches_language("c", "swift"));
    }

    #[test]
    fn test_matches_language_c() {
        assert!(matches_language("c", "c"));
        assert!(matches_language("h", "c"));
        assert!(matches_language("C", "c"));
        assert!(!matches_language("cpp", "c"));
    }

    #[test]
    fn test_matches_language_cpp() {
        assert!(matches_language("cpp", "cpp"));
        assert!(matches_language("cc", "cpp"));
        assert!(matches_language("cxx", "cpp"));
        assert!(matches_language("hpp", "cpp"));
        assert!(matches_language("hh", "cpp"));
        assert!(matches_language("hxx", "cpp"));
        assert!(matches_language("h", "cpp")); // Headers can be C++
        assert!(matches_language("cpp", "c++")); // Alternative name
        assert!(!matches_language("c", "cpp"));
    }

    #[test]
    fn test_matches_language_csharp() {
        assert!(matches_language("cs", "csharp"));
        assert!(matches_language("cs", "c#"));
        assert!(matches_language("csx", "csharp"));
        assert!(matches_language("cs", "CSharp"));
        assert!(!matches_language("cpp", "csharp"));
    }

    #[test]
    fn test_matches_language_dart() {
        assert!(matches_language("dart", "dart"));
        assert!(matches_language("dart", "Dart"));
        assert!(!matches_language("d", "dart"));
    }

    #[test]
    fn test_matches_language_sql() {
        assert!(matches_language("sql", "sql"));
        assert!(matches_language("sql", "SQL"));
        assert!(!matches_language("rs", "sql"));
    }

    #[test]
    fn test_matches_language_servicenow() {
        assert!(matches_language("snjs", "servicenow"));
        assert!(matches_language("snjs", "ServiceNow-Xanadu"));
        assert!(matches_language("snjs", "servicenow-xanadu-js"));
        assert!(!matches_language("js", "servicenow"));
    }
}
