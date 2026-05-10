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
use sqry_core::graph::unified::node::{NodeId, NodeKind};
use sqry_core::graph::unified::storage::metadata::MacroNodeMetadata;
use sqry_core::json_response::{Filters, FuzzyFilters, Stats, StreamEvent};
use sqry_core::search::fuzzy::{CandidateGenerator, FuzzyConfig};
use sqry_core::search::matcher::{FuzzyMatcher, MatchAlgorithm, MatchConfig};
use sqry_core::search::trigram::TrigramIndex;
use std::collections::{BTreeMap, HashMap};
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

/// Operator selections for the macro-boundary CLI flags.
///
/// `--cfg-filter` / `--include-generated` / `--macro-boundaries` are bundled
/// into a single struct so the filter pipeline takes one parameter instead
/// of three positional booleans. The struct is `Copy` to keep the call sites
/// cheap.
#[derive(Debug, Clone, Copy)]
struct MacroBoundaryFlags<'a> {
    cfg_filter: Option<&'a str>,
    include_generated: bool,
    macro_boundaries: bool,
}

/// Decide whether a single candidate `NodeId` survives the macro-boundary
/// filter. The decision is sourced **directly** from the live
/// `NodeMetadataStore` — never from environment variables and never from
/// the `DisplaySymbol::metadata` HashMap — so the filter contract is the
/// same regardless of how the candidate set was produced (regex scan,
/// trigram fuzzy, exact lookup, etc.).
///
/// Rules:
/// - When `include_generated` is `false`, drop nodes whose macro metadata
///   reports `macro_generated == Some(true)`.
/// - When `cfg_filter` is `Some(predicate)`, drop nodes whose macro
///   metadata reports a `cfg_condition` that does not equal `predicate`.
///   Nodes with no metadata or no `cfg_condition` are treated as "no cfg"
///   and are dropped — `--cfg-filter` is an inclusive predicate, not a
///   speculative one.
/// - When `cfg_filter` is `None`, no cfg filter is applied; nodes are kept
///   regardless of `cfg_condition`.
fn macro_boundary_keeps_node(
    metadata: Option<&MacroNodeMetadata>,
    flags: MacroBoundaryFlags<'_>,
) -> bool {
    if !flags.include_generated && metadata.is_some_and(|m| m.macro_generated == Some(true)) {
        return false;
    }
    if let Some(filter) = flags.cfg_filter {
        let actual = metadata.and_then(|m| m.cfg_condition.as_deref());
        if actual != Some(filter) {
            return false;
        }
    }
    true
}

/// Apply the macro-boundary filter to a candidate `NodeId` set, consulting
/// the graph's [`NodeMetadataStore`] for each id. Returns the surviving
/// node ids (order preserved).
///
/// This is the production filter pipeline for `--cfg-filter` and
/// `--include-generated`. It is exercised directly by the unit tests in
/// this module via a hand-crafted [`CodeGraph`] so the filter's
/// metadata-store contract is observed end-to-end without indexing real
/// source.
fn filter_nodes_by_macro_boundary(
    graph: &CodeGraph,
    candidates: Vec<NodeId>,
    flags: MacroBoundaryFlags<'_>,
) -> Vec<NodeId> {
    if flags.include_generated && flags.cfg_filter.is_none() {
        return candidates;
    }
    let store = graph.macro_metadata();
    candidates
        .into_iter()
        .filter(|node_id| macro_boundary_keeps_node(store.get(*node_id), flags))
        .collect()
}

/// Populate `DisplaySymbol::metadata` with macro-boundary provenance keys
/// pulled from the graph's [`NodeMetadataStore`]. Keys are only inserted
/// when the underlying metadata is present, keeping non-macro symbols free
/// of empty-string clutter in JSON output.
fn enrich_with_macro_metadata(symbol: &mut DisplaySymbol, metadata: Option<&MacroNodeMetadata>) {
    let Some(meta) = metadata else { return };
    if let Some(true) = meta.macro_generated {
        symbol
            .metadata
            .insert("macro_generated".to_string(), "true".to_string());
    }
    if let Some(cfg) = meta.cfg_condition.as_deref() {
        symbol
            .metadata
            .insert("cfg_condition".to_string(), cfg.to_string());
    }
    if let Some(source) = meta.macro_source.as_deref() {
        symbol
            .metadata
            .insert("macro_source".to_string(), source.to_string());
    }
}

/// Group results by macro expansion source when `--macro-boundaries` is
/// active. Symbols that share a `macro_source` are emitted in adjacent
/// runs, with a `macro_boundary_group` metadata key recording the group
/// identifier so JSON consumers can re-segment without reparsing the
/// source string. Symbols without a `macro_source` are placed in a
/// terminal "no-macro" group identified by the empty key, preserving
/// determinism.
fn group_results_by_macro_source(symbols: Vec<DisplaySymbol>) -> Vec<DisplaySymbol> {
    // BTreeMap gives us a deterministic, alphabetic group order which makes
    // the boundary output reproducible across runs and JSON snapshots.
    let mut grouped: BTreeMap<String, Vec<DisplaySymbol>> = BTreeMap::new();
    for mut symbol in symbols {
        let key = symbol
            .metadata
            .get("macro_source")
            .cloned()
            .unwrap_or_default();
        symbol
            .metadata
            .insert("macro_boundary_group".to_string(), key.clone());
        grouped.entry(key).or_default().push(symbol);
    }
    grouped.into_values().flatten().collect()
}

/// Scored variant of [`group_results_by_macro_source`] for the JSON
/// streaming path: groups `(DisplaySymbol, score)` pairs by `macro_source`
/// while preserving each pair's score.
fn group_scored_results_by_macro_source(symbols: Vec<ScoredSymbol>) -> Vec<ScoredSymbol> {
    let mut grouped: BTreeMap<String, Vec<ScoredSymbol>> = BTreeMap::new();
    for (mut symbol, score) in symbols {
        let key = symbol
            .metadata
            .get("macro_source")
            .cloned()
            .unwrap_or_default();
        symbol
            .metadata
            .insert("macro_boundary_group".to_string(), key.clone());
        grouped.entry(key).or_default().push((symbol, score));
    }
    grouped.into_values().flatten().collect()
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
/// `cfg_filter`, `include_generated`, and `macro_boundaries` (C002a) thread
/// the macro-boundary CLI flags through to the search engine so that
/// `--cfg-filter`, `--include-generated`, and `--macro-boundaries` actually
/// influence which macro-generated symbols appear in the result set rather
/// than being silently dropped at the dispatch boundary.
///
/// # Errors
/// Returns an error if search execution fails or output cannot be written.
pub fn run_search(
    cli: &Cli,
    pattern: &str,
    search_path: &str,
    cfg_filter: Option<&str>,
    include_generated: bool,
    macro_boundaries: bool,
) -> Result<()> {
    let macro_flags = MacroBoundaryFlags {
        cfg_filter,
        include_generated,
        macro_boundaries,
    };
    // Handle JSON streaming mode separately (fuzzy only, enforced by clap)
    if cli.json_stream {
        return run_json_stream_search(cli, pattern, search_path, macro_flags);
    }

    let start_time = Instant::now();

    // Branch based on search mode, capturing index age and scope info if available
    let (mut all_symbols, index_age_seconds, scope_info) = if cli.fuzzy {
        let (scored_symbols, age, scope) =
            run_fuzzy_search(cli, pattern, search_path, macro_flags)?;
        let symbols = scored_symbols.into_iter().map(|(s, _)| s).collect();
        (symbols, Some(age), Some(scope))
    } else {
        (
            run_regular_search(cli, pattern, search_path, macro_flags)?,
            None,
            None,
        )
    };

    apply_search_filters(cli, &mut all_symbols);
    if macro_flags.macro_boundaries {
        all_symbols = group_results_by_macro_source(all_symbols);
    }

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
fn run_regular_search(
    cli: &Cli,
    pattern: &str,
    search_path: &str,
    macro_flags: MacroBoundaryFlags<'_>,
) -> Result<Vec<DisplaySymbol>> {
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
        // `--exact`: contract-bound to the planner's `name:<literal>`
        // predicate (see `sqry-db/src/planner/parse.rs` around the
        // `name:` step). Both surfaces route through
        // `GraphSnapshot::find_by_exact_name` for literal patterns, so
        // `sqry --exact NeedTags .` and `sqry query 'name:NeedTags' .`
        // return identical sets. The lookup first checks interned
        // `entry.name` / `entry.qualified_name` byte-for-byte, then falls
        // also checks native dot- and Ruby-`#` qualified display form as
        // graph-canonical `::`. Synthetic placeholders are
        // excluded. `--exact` does not accept glob meta; for glob behaviour
        // use `sqry query 'name:parse_*'` instead.
        //
        // Reachable here only when `cli.exact` is true, because
        // `build_pattern_regex` returns `Ok(Some(_))` for every
        // non-exact path (or propagates the regex error).
        debug_assert!(
            cli.exact,
            "non-exact path is owned by the regex branch above"
        );
        let node_ids = graph.snapshot().find_by_exact_name(pattern);
        matches.extend(node_ids);
    }

    // Deduplicate node IDs
    matches.sort_unstable();
    matches.dedup();

    // Macro-boundary filter: drop candidates whose graph metadata violates
    // `--cfg-filter` / `--include-generated` BEFORE conversion to
    // DisplaySymbol. Filtering at the NodeId layer keeps the production
    // contract identical regardless of the conversion path and lets the
    // unit tests in this module observe the filter on a synthetic graph
    // without exercising the trigram/regex front end.
    let matches = filter_nodes_by_macro_boundary(&graph, matches, macro_flags);

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

    // `B_cost_gate.md` §4 "CLI sqry search (shape-only subset)":
    // run the anchor / prefix / `min_literal_len` shape check
    // BEFORE compiling the regex so a pathologically broad pattern
    // (`.*foo.*`, `.*$`, etc.) is rejected before it can scan the
    // entire arena.
    //
    // The CLI surface has no parsed-query AST so the
    // scope-coupling rule does not apply; we pass `usize::MAX` for
    // the node-count argument so the cap is always engaged. The
    // asymmetry vs the `sqry query` planner-coupled path is
    // documented in `B_cost_gate.md` §Open question 4 + the
    // `docs/cli/scaling-large-codebases.md` recovery doc.
    sqry_core::query::cost_gate::check_regex_pattern_text(
        pattern,
        usize::MAX,
        &sqry_core::query::cost_gate::CostGateConfig::default(),
    )
    .map_err(anyhow::Error::from)?;

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

    let mut symbol = DisplaySymbol {
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
    };

    // Surface macro-boundary provenance (macro_generated, cfg_condition,
    // macro_source) from the graph's NodeMetadataStore so JSON consumers
    // and `--macro-boundaries` grouping have a canonical key set to read.
    enrich_with_macro_metadata(&mut symbol, graph.macro_metadata().get(node_id));

    Some(symbol)
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
    macro_flags: MacroBoundaryFlags<'_>,
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

        // Macro-boundary filter at the NodeId layer (same contract as the
        // regex/exact path in `run_regular_search`).
        let node_ids = filter_nodes_by_macro_boundary(&graph, node_ids, macro_flags);

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

fn run_json_stream_search(
    cli: &Cli,
    pattern: &str,
    search_path: &str,
    macro_flags: MacroBoundaryFlags<'_>,
) -> Result<()> {
    let (mut symbols, age_seconds, scope_info) =
        run_fuzzy_search(cli, pattern, search_path, macro_flags)?;

    // Apply kind/language filters (same semantics as the non-streaming path).
    apply_scored_search_filters(cli, &mut symbols);

    if macro_flags.macro_boundaries {
        symbols = group_scored_results_by_macro_source(symbols);
    }

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

    // ------------------------------------------------------------------
    // Macro-boundary filter tests.
    //
    // These exercise the production filter pipeline directly against a
    // hand-crafted `CodeGraph` + `NodeMetadataStore`. The tests deliberately
    // do NOT go through the indexing pipeline (no parsing, no plugin
    // dispatch) — they isolate the filter contract so a regression in
    // `filter_nodes_by_macro_boundary` / `enrich_with_macro_metadata` /
    // `group_results_by_macro_source` is caught regardless of whether any
    // upstream Rust plugin happens to populate `macro_generated` for a
    // given indexed symbol today.
    // ------------------------------------------------------------------

    use sqry_core::graph::unified::NodeEntry;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::metadata::MacroNodeMetadata;

    /// Test-graph builder. Allocates a single node with the given name in
    /// `test.rs` and returns the resulting `NodeId` so callers can attach
    /// metadata via `graph.macro_metadata_mut().insert(...)`.
    fn add_test_node(graph: &mut CodeGraph, name: &str) -> NodeId {
        let name_id = graph.strings_mut().intern(name).expect("intern name");
        let file_id = graph
            .files_mut()
            .register_with_language(Path::new("/synth/test.rs"), None)
            .expect("register file");
        let entry = NodeEntry::new(NodeKind::Function, name_id, file_id);
        let node_id = graph.nodes_mut().alloc(entry).expect("alloc node");
        graph
            .indices_mut()
            .add(node_id, NodeKind::Function, name_id, None, file_id);
        node_id
    }

    fn macro_metadata(
        generated: bool,
        cfg: Option<&str>,
        source: Option<&str>,
    ) -> MacroNodeMetadata {
        MacroNodeMetadata {
            macro_generated: Some(generated),
            macro_source: source.map(str::to_string),
            cfg_condition: cfg.map(str::to_string),
            cfg_active: None,
            proc_macro_kind: None,
            expansion_cached: None,
            unresolved_attributes: Vec::new(),
        }
    }

    /// `--include-generated` absent ⇒ a node whose graph metadata reports
    /// `macro_generated == Some(true)` is dropped by
    /// `filter_nodes_by_macro_boundary`. This is the structural unit test
    /// the audit calls for: the filter consults the live
    /// `NodeMetadataStore`, not the `DisplaySymbol::metadata` HashMap, and
    /// not an env var.
    #[test]
    fn run_search_drops_macro_generated_when_include_generated_false() {
        let mut graph = CodeGraph::new();
        let user = add_test_node(&mut graph, "user_defined");
        let derived = add_test_node(&mut graph, "derived_by_macro");
        graph
            .macro_metadata_mut()
            .insert(derived, macro_metadata(true, None, Some("derive_Debug")));

        let flags = MacroBoundaryFlags {
            cfg_filter: None,
            include_generated: false,
            macro_boundaries: false,
        };
        let kept = filter_nodes_by_macro_boundary(&graph, vec![user, derived], flags);
        assert_eq!(kept, vec![user], "macro_generated node must be dropped");
    }

    /// `--include-generated` set ⇒ macro-generated nodes survive the
    /// filter, and `convert_node_to_display_symbol` surfaces the
    /// `macro_generated` / `macro_source` provenance into
    /// `DisplaySymbol::metadata` so JSON consumers see it.
    #[test]
    fn run_search_keeps_macro_generated_when_include_generated_true() {
        let mut graph = CodeGraph::new();
        let user = add_test_node(&mut graph, "user_defined");
        let derived = add_test_node(&mut graph, "derived_by_macro");
        graph
            .macro_metadata_mut()
            .insert(derived, macro_metadata(true, None, Some("derive_Debug")));

        let flags = MacroBoundaryFlags {
            cfg_filter: None,
            include_generated: true,
            macro_boundaries: false,
        };
        let kept = filter_nodes_by_macro_boundary(&graph, vec![user, derived], flags);
        assert_eq!(kept, vec![user, derived]);

        // Conversion path enriches the DisplaySymbol metadata HashMap with
        // the underlying provenance so JSON callers can read it.
        let symbol = convert_node_to_display_symbol(&graph, derived).expect("convert derived node");
        assert_eq!(
            symbol.metadata.get("macro_generated").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            symbol.metadata.get("macro_source").map(String::as_str),
            Some("derive_Debug")
        );
    }

    /// `--cfg-filter alpha` ⇒ only nodes whose metadata reports
    /// `cfg_condition == Some("alpha")` survive. Nodes without metadata
    /// (or with a different cfg) are dropped.
    #[test]
    fn run_search_filters_by_cfg_condition() {
        let mut graph = CodeGraph::new();
        let always = add_test_node(&mut graph, "always_present");
        let alpha = add_test_node(&mut graph, "alpha_only");
        let beta = add_test_node(&mut graph, "beta_only");
        graph.macro_metadata_mut().insert(
            alpha,
            macro_metadata(false, Some("feature = \"alpha\""), None),
        );
        graph.macro_metadata_mut().insert(
            beta,
            macro_metadata(false, Some("feature = \"beta\""), None),
        );

        let flags = MacroBoundaryFlags {
            cfg_filter: Some("feature = \"alpha\""),
            include_generated: true,
            macro_boundaries: false,
        };
        let kept = filter_nodes_by_macro_boundary(&graph, vec![always, alpha, beta], flags);
        assert_eq!(
            kept,
            vec![alpha],
            "only nodes whose cfg_condition matches the filter survive"
        );
    }

    /// `--macro-boundaries` ⇒ results are reordered so symbols sharing a
    /// `macro_source` appear in adjacent runs, and each surviving symbol
    /// carries a `macro_boundary_group` metadata key matching its source.
    #[test]
    fn run_search_groups_results_by_macro_source_when_macro_boundaries() {
        let mut graph = CodeGraph::new();
        let plain = add_test_node(&mut graph, "plain_fn");
        let from_serde = add_test_node(&mut graph, "from_serde");
        let from_log = add_test_node(&mut graph, "from_log");
        let from_serde_2 = add_test_node(&mut graph, "from_serde_2");
        graph.macro_metadata_mut().insert(
            from_serde,
            macro_metadata(true, None, Some("serde::Serialize")),
        );
        graph
            .macro_metadata_mut()
            .insert(from_log, macro_metadata(true, None, Some("log::info")));
        graph.macro_metadata_mut().insert(
            from_serde_2,
            macro_metadata(true, None, Some("serde::Serialize")),
        );

        let symbols: Vec<DisplaySymbol> = [plain, from_serde, from_log, from_serde_2]
            .into_iter()
            .map(|nid| convert_node_to_display_symbol(&graph, nid).expect("convert node"))
            .collect();

        let grouped = group_results_by_macro_source(symbols);

        // Each grouped symbol carries the boundary group key.
        for sym in &grouped {
            assert!(
                sym.metadata.contains_key("macro_boundary_group"),
                "missing macro_boundary_group on {}",
                sym.name
            );
        }

        // Symbols sharing a macro_source are now adjacent. Collect the
        // group key sequence and verify each unique key forms a contiguous
        // run.
        let keys: Vec<&str> = grouped
            .iter()
            .map(|s| s.metadata["macro_boundary_group"].as_str())
            .collect();
        let mut seen_starts = std::collections::HashMap::<&str, (usize, usize)>::new();
        for (i, k) in keys.iter().enumerate() {
            seen_starts
                .entry(k)
                .and_modify(|(_, last)| *last = i)
                .or_insert((i, i));
        }
        for (k, (first, last)) in &seen_starts {
            // Every index between first and last must carry the same key.
            for i in *first..=*last {
                assert_eq!(keys[i], *k, "group `{k}` is not contiguous in {keys:?}");
            }
        }

        // The serde group must contain both serde-sourced symbols.
        let serde_count = grouped
            .iter()
            .filter(|s| {
                s.metadata.get("macro_boundary_group").map(String::as_str)
                    == Some("serde::Serialize")
            })
            .count();
        assert_eq!(serde_count, 2, "serde group should contain 2 symbols");
    }
}
