#[cfg(any(test, fuzzing))]
use anyhow::{Result, bail, ensure};
#[cfg(any(test, fuzzing))]
use serde_json::Value;

#[derive(Debug, Clone, Default)]
pub struct SearchFilters {
    pub languages: Vec<String>,
    pub visibility: Option<Visibility>,
    pub kinds: Vec<String>,
    pub min_score: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
}

#[derive(Debug, Clone)]
pub struct PaginationArgs {
    pub offset: usize,
    pub size: usize,
}

#[derive(Debug, Clone)]
pub struct SemanticSearchArgs {
    pub query: String,
    pub path: String,
    pub filters: SearchFilters,
    pub max_results: usize,
    pub context_lines: usize,
    pub pagination: PaginationArgs,
    pub score_min: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationType {
    Callers,
    Callees,
    Imports,
    Exports,
    Returns,
}

impl RelationType {
    pub fn as_str(self) -> &'static str {
        match self {
            RelationType::Callers => "callers",
            RelationType::Callees => "callees",
            RelationType::Imports => "imports",
            RelationType::Exports => "exports",
            RelationType::Returns => "returns",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RelationQueryArgs {
    pub symbol: String,
    pub relation: RelationType,
    pub path: String,
    pub max_depth: usize,
    pub max_results: usize,
    pub pagination: PaginationArgs,
}

#[derive(Debug, Clone)]
pub struct ExplainCodeArgs {
    pub file_path: String,
    pub symbol_name: String,
    pub path: String,
    pub include_context: bool,
    pub include_relations: bool,
}

#[derive(Debug, Clone)]
pub struct ShowDependenciesArgs {
    pub file_path: Option<String>,
    pub symbol_name: Option<String>,
    pub path: String,
    pub max_depth: usize,
    pub max_results: usize,
    pub pagination: PaginationArgs,
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // Mirrors MCP tool flags without extra wrapper types.
pub struct ExportGraphArgs {
    pub file_path: Option<String>,
    pub symbol_name: Option<String>,
    pub path: String,
    pub format: String,
    pub max_depth: usize,
    pub max_results: usize,
    pub pagination: PaginationArgs,
    pub include_calls: bool,
    pub include_imports: bool,
    pub include_exports: bool,
    pub include_returns: bool,
    pub languages: Vec<String>,
    pub verbose: bool,
}

#[derive(Debug, Clone)]
pub struct CrossLanguageEdgesArgs {
    pub path: String,
    pub from_lang: Option<String>,
    pub to_lang: Option<String>,
    pub max_results: usize,
    pub pagination: PaginationArgs,
}

#[derive(Debug, Clone)]
pub struct TracePathArgs {
    pub from_symbol: String,
    pub to_symbol: String,
    pub path: String,
    pub max_hops: usize,
    pub max_paths: usize,
    pub cross_language: bool,
    pub min_confidence: f64,
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)] // Mirrors MCP tool flags without extra wrapper types.
pub struct SubgraphArgs {
    pub symbols: Vec<String>,
    pub path: String,
    pub max_depth: usize,
    pub max_nodes: usize,
    pub include_callers: bool,
    pub include_callees: bool,
    pub include_imports: bool,
    pub cross_language: bool,
    pub pagination: PaginationArgs,
}

#[derive(Debug, Clone)]
pub struct GetIndexStatusArgs {
    pub path: String,
}

/// Arguments for `find_duplicates` tool
#[derive(Debug, Clone)]
pub struct FindDuplicatesArgs {
    pub path: String,
    pub duplicate_type: DuplicateType,
    pub threshold: u32,
    pub exact: bool,
    pub max_results: usize,
    pub pagination: PaginationArgs,
}

/// Duplicate type enum for `find_duplicates` tool
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateType {
    Body,
    Signature,
    Struct,
}

/// Arguments for `find_cycles` tool
#[derive(Debug, Clone)]
pub struct FindCyclesArgs {
    pub path: String,
    pub cycle_type: CycleType,
    pub min_depth: usize,
    pub max_depth: Option<usize>,
    pub include_self_loops: bool,
    pub max_results: usize,
    pub pagination: PaginationArgs,
}

/// Cycle type enum for `find_cycles` tool
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleType {
    Calls,
    Imports,
    Modules,
}

/// Arguments for `find_unused` tool
///
/// Note: Some fields are reserved for future use when reachability analysis
/// is fully implemented for `CodeGraph`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct FindUnusedArgs {
    pub path: String,
    pub scope: UnusedScope,
    pub languages: Vec<String>,
    pub kinds: Vec<String>,
    pub max_results: usize,
    pub pagination: PaginationArgs,
}

/// Scope enum for `find_unused` tool
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnusedScope {
    Public,
    Private,
    Function,
    Struct,
    All,
}

#[derive(Debug, Clone)]
pub struct SearchSimilarArgs {
    pub path: String,
    pub file_path: String,
    pub symbol_name: String,
    pub similarity_threshold: f64,
    pub max_results: usize,
    pub pagination: PaginationArgs,
}

#[derive(Debug, Clone)]
pub struct DependencyImpactArgs {
    pub symbol: String,
    pub path: String,
    pub max_depth: usize,
    pub include_files: bool,
    pub include_indirect: bool,
    pub max_results: usize,
    pub pagination: PaginationArgs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeType {
    Added,
    Removed,
    Modified,
    Renamed,
    SignatureChanged,
}

impl ChangeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeType::Added => "added",
            ChangeType::Removed => "removed",
            ChangeType::Modified => "modified",
            ChangeType::Renamed => "renamed",
            ChangeType::SignatureChanged => "signature_changed",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SemanticDiffFilters {
    pub change_types: Vec<ChangeType>,
    pub symbol_kinds: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GitVersionRef {
    pub git_ref: String,
    #[allow(dead_code)]
    pub file_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SemanticDiffArgs {
    pub base: GitVersionRef,
    pub target: GitVersionRef,
    #[allow(dead_code)]
    pub path: String,
    pub include_unchanged: bool,
    pub filters: SemanticDiffFilters,
    pub max_results: usize,
    pub pagination: PaginationArgs,
}

/// Arguments for `hierarchical_search` tool
/// Semantic search with file → container → symbol grouping for RAG
#[derive(Debug, Clone)]
pub struct HierarchicalSearchArgs {
    /// Semantic query expression
    pub query: String,
    /// Workspace-relative directory to search
    pub path: String,
    /// Search filters (same as `semantic_search`)
    pub filters: SearchFilters,
    /// Maximum symbols before grouping (default: 200)
    pub max_results: usize,
    /// Context lines around matches (default: 3)
    pub context_lines: usize,
    /// Pagination arguments
    pub pagination: PaginationArgs,
    /// Minimum relevance score
    pub score_min: Option<f64>,
    /// Enable auto-merge of small contexts (default: true)
    pub auto_merge: bool,
    /// Token threshold for auto-merge (default: 256)
    pub merge_threshold: usize,
    /// Maximum files per page (default: 20)
    pub max_files: usize,
    /// Maximum containers per file (default: 50)
    pub max_containers_per_file: usize,
    /// Maximum symbols per container (default: 100)
    pub max_symbols_per_container: usize,
    /// Hard limit on total symbols (default: 2000)
    pub max_total_symbols: usize,
    /// Target tokens for file-level grouping (default: 2000)
    pub file_target_tokens: u64,
    /// Target tokens for container-level grouping (default: 1500)
    pub container_target_tokens: u64,
    /// Target tokens for symbol-level detail (default: 500)
    pub symbol_target_tokens: u64,
    /// Target tokens for context clusters (default: 768)
    pub context_cluster_target_tokens: u64,
    /// Include file-level code summary (default: false)
    pub include_file_context: bool,
    /// Include container-level code (default: false)
    pub include_container_context: bool,
    /// File paths to expand (load full details for stubs)
    /// Use this to lazily load specific files from a previous stub response
    pub expand_files: Vec<String>,
}

/// Validate `semantic_search` arguments.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are out of range.
#[cfg(any(test, fuzzing))]
pub fn validate_semantic_search_args(args: &Value) -> Result<SemanticSearchArgs> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required field: query"))?;
    if query.trim().is_empty() {
        bail!("query cannot be empty");
    }

    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(200);
    ensure!(
        (1..=10_000).contains(&max_results),
        "max_results must be between 1 and 10000"
    );

    let context_lines = args
        .get("context_lines")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(3);
    ensure!(
        (0..=20).contains(&context_lines),
        "context_lines must be between 0 and 20"
    );

    let filters = parse_filters(args.get("filters"), args)?;
    let pagination = parse_pagination(args, 50, 500)?;

    let score_min = filters.min_score;

    Ok(SemanticSearchArgs {
        query: query.to_string(),
        path: path.to_string(),
        filters,
        // Validated i64 fits in usize on this platform; reject if out of range
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        context_lines: context_lines
            .try_into()
            .map_err(|_| anyhow::anyhow!("context_lines out of range"))?,
        pagination,
        score_min,
    })
}

/// Validate `relation_query` arguments.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are invalid.
#[cfg(any(test, fuzzing))]
pub fn validate_relation_query_args(args: &Value) -> Result<RelationQueryArgs> {
    let symbol = args
        .get("symbol")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required field: symbol"))?;
    if symbol.trim().is_empty() {
        bail!("symbol cannot be empty");
    }

    let relation = match args
        .get("relation_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required field: relation_type"))?
    {
        "callers" => RelationType::Callers,
        "callees" => RelationType::Callees,
        "imports" => RelationType::Imports,
        "exports" => RelationType::Exports,
        "returns" => RelationType::Returns,
        other => bail!("Unsupported relation_type: {other}"),
    };

    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(1);
    ensure!(
        (1..=5).contains(&max_depth),
        "max_depth must be between 1 and 5"
    );

    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(200);
    ensure!(
        (1..=5_000).contains(&max_results),
        "max_results must be between 1 and 5000"
    );

    let pagination = parse_pagination(args, 50, 500)?;

    Ok(RelationQueryArgs {
        symbol: symbol.to_string(),
        relation,
        path: path.to_string(),
        // Validated i64 fits in usize on this platform; reject if out of range
        max_depth: max_depth
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_depth out of range"))?,
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        pagination,
    })
}

/// Validate `explain_code` arguments.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are invalid.
#[cfg(any(test, fuzzing))]
pub fn validate_explain_code_args(args: &Value) -> Result<ExplainCodeArgs> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required field: file_path"))?;
    let symbol_name = args
        .get("symbol_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required field: symbol_name"))?;

    if symbol_name.trim().is_empty() {
        bail!("symbol_name cannot be empty");
    }

    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let include_context = args
        .get("include_context")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let include_relations = args
        .get("include_relations")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    Ok(ExplainCodeArgs {
        file_path: file_path.to_string(),
        symbol_name: symbol_name.to_string(),
        path: path.to_string(),
        include_context,
        include_relations,
    })
}

/// Validate `show_dependencies` arguments.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are invalid.
#[cfg(any(test, fuzzing))]
pub fn validate_show_dependencies_args(args: &Value) -> Result<ShowDependenciesArgs> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let symbol_name = args
        .get("symbol_name")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
    if file_path.is_none() && symbol_name.is_none() {
        bail!("At least one of file_path or symbol_name must be provided");
    }

    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(2);
    ensure!(
        (1..=5).contains(&max_depth),
        "max_depth must be between 1 and 5"
    );

    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(500);
    ensure!(
        (1..=5_000).contains(&max_results),
        "max_results must be between 1 and 5000"
    );

    let pagination = parse_pagination(args, 100, 1_000)?;

    Ok(ShowDependenciesArgs {
        file_path,
        symbol_name,
        path: path.to_string(),
        // Validated i64 fits in usize on this platform; reject if out of range
        max_depth: max_depth
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_depth out of range"))?,
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        pagination,
    })
}

/// Validate `export_graph` arguments.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are invalid.
#[cfg(any(test, fuzzing))]
pub fn validate_export_graph_args(args: &Value) -> Result<ExportGraphArgs> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let symbol_name = args
        .get("symbol_name")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
    if file_path.is_none() && symbol_name.is_none() {
        bail!("At least one of file_path or symbol_name must be provided");
    }

    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(2);
    ensure!(
        (1..=5).contains(&max_depth),
        "max_depth must be between 1 and 5"
    );

    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(1000);
    ensure!(
        (1..=5_000).contains(&max_results),
        "max_results must be between 1 and 5000"
    );

    // Edge kind inclusion
    let mut include_calls = true; // default to calls
    let mut include_imports = false;
    let mut include_exports = false;
    let mut include_returns = false;
    if let Some(include) = args.get("include").and_then(|v| v.as_array()) {
        include_calls = false;
        for item in include {
            let s = item
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("include entries must be strings"))?;
            match s {
                "calls" => include_calls = true,
                "imports" => include_imports = true,
                "exports" => include_exports = true,
                "returns" => include_returns = true,
                other => bail!("Unsupported include kind: {other}"),
            }
        }
    }

    let languages = args
        .get("languages")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let pagination = parse_pagination(args, 200, 1_000)?;

    let format = args
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");
    ensure!(
        matches!(format, "json" | "dot" | "d2" | "mermaid"),
        "format must be one of: json | dot | d2 | mermaid"
    );

    Ok(ExportGraphArgs {
        file_path,
        symbol_name,
        path: path.to_string(),
        format: format.to_string(),
        // Validated i64 fits in usize on this platform; reject if out of range
        max_depth: max_depth
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_depth out of range"))?,
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        pagination,
        include_calls,
        include_imports,
        include_exports,
        include_returns,
        languages,
        verbose: args
            .get("verbose")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// Validate `cross_language_edges` arguments.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are invalid.
#[cfg(any(test, fuzzing))]
pub fn validate_cross_language_edges_args(args: &Value) -> Result<CrossLanguageEdgesArgs> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let from_lang = args
        .get("from_lang")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);
    let to_lang = args
        .get("to_lang")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);

    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(500);
    ensure!(
        (1..=5_000).contains(&max_results),
        "max_results must be between 1 and 5000"
    );

    let pagination = parse_pagination(args, 200, 1_000)?;

    Ok(CrossLanguageEdgesArgs {
        path: path.to_string(),
        from_lang,
        to_lang,
        // Validated i64 fits in usize on this platform; reject if out of range
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        pagination,
    })
}

/// Validate `trace_path` arguments.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are invalid.
#[cfg(any(test, fuzzing))]
pub fn validate_trace_path_args(args: &Value) -> Result<TracePathArgs> {
    let from_symbol = args
        .get("from_symbol")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required field 'from_symbol'"))?;

    let to_symbol = args
        .get("to_symbol")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required field 'to_symbol'"))?;

    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let max_hops = args
        .get("max_hops")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(5);
    ensure!(
        (1..=10).contains(&max_hops),
        "max_hops must be between 1 and 10"
    );

    let max_paths = args
        .get("max_paths")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(5);
    ensure!(
        (1..=20).contains(&max_paths),
        "max_paths must be between 1 and 20"
    );

    let cross_language = args
        .get("cross_language")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let min_confidence = args
        .get("min_confidence")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.5);
    ensure!(
        (0.0..=1.0).contains(&min_confidence),
        "min_confidence must be between 0.0 and 1.0"
    );

    Ok(TracePathArgs {
        from_symbol: from_symbol.to_string(),
        to_symbol: to_symbol.to_string(),
        path: path.to_string(),
        // Validated i64 fits in usize on this platform; reject if out of range
        max_hops: max_hops
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_hops out of range"))?,
        max_paths: max_paths
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_paths out of range"))?,
        cross_language,
        min_confidence,
    })
}

/// Validate `subgraph` arguments.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are invalid.
#[cfg(any(test, fuzzing))]
pub fn validate_subgraph_args(args: &Value) -> Result<SubgraphArgs> {
    let symbols_array = args
        .get("symbols")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("Missing required field 'symbols' (must be an array)"))?;

    let symbols: Vec<String> = symbols_array
        .iter()
        .filter_map(|v| v.as_str())
        .map(std::string::ToString::to_string)
        .collect();

    if symbols.is_empty() {
        bail!("symbols array must contain at least one symbol");
    }

    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(2);
    ensure!(
        (1..=5).contains(&max_depth),
        "max_depth must be between 1 and 5"
    );

    let max_nodes = args
        .get("max_nodes")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(50);
    ensure!(
        (1..=500).contains(&max_nodes),
        "max_nodes must be between 1 and 500"
    );

    let include_incoming_calls = args
        .get("include_callers")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let include_outgoing_calls = args
        .get("include_callees")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let include_imports = args
        .get("include_imports")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let cross_language = args
        .get("cross_language")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let pagination = parse_pagination(args, 50, 200)?;

    Ok(SubgraphArgs {
        symbols,
        path: path.to_string(),
        // Validated i64 fits in usize on this platform; reject if out of range
        max_depth: max_depth
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_depth out of range"))?,
        max_nodes: max_nodes
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_nodes out of range"))?,
        include_callers: include_incoming_calls,
        include_callees: include_outgoing_calls,
        include_imports,
        cross_language,
        pagination,
    })
}

#[must_use]
#[cfg(any(test, fuzzing))]
pub fn validate_get_index_status_args(args: &Value) -> GetIndexStatusArgs {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    GetIndexStatusArgs {
        path: path.to_string(),
    }
}

/// Validate `search_similar` arguments.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are invalid.
#[cfg(any(test, fuzzing))]
pub fn validate_search_similar_args(args: &Value) -> Result<SearchSimilarArgs> {
    let reference = args
        .get("reference")
        .ok_or_else(|| anyhow::anyhow!("Missing required field: reference"))?;
    let reference_obj = reference
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("reference must be an object"))?;

    let file_path = reference_obj
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("reference.file_path is required"))?;
    let symbol_name = reference_obj
        .get("symbol_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("reference.symbol_name is required"))?;
    if symbol_name.trim().is_empty() {
        bail!("reference.symbol_name cannot be empty");
    }

    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let similarity_threshold = args
        .get("similarity_threshold")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.7);
    ensure!(
        (0.0..=1.0).contains(&similarity_threshold),
        "similarity_threshold must be between 0.0 and 1.0"
    );

    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(20);
    ensure!(
        (1..=200).contains(&max_results),
        "max_results must be between 1 and 200"
    );

    let pagination = parse_pagination(args, 20, 200)?;

    Ok(SearchSimilarArgs {
        path: path.to_string(),
        file_path: file_path.to_string(),
        symbol_name: symbol_name.to_string(),
        similarity_threshold,
        // Validated i64 fits in usize on this platform; reject if out of range
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        pagination,
    })
}

/// Validate `dependency_impact` arguments.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are invalid.
#[cfg(any(test, fuzzing))]
pub fn validate_dependency_impact_args(args: &Value) -> Result<DependencyImpactArgs> {
    let symbol = args
        .get("symbol")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required field: symbol"))?;
    if symbol.trim().is_empty() {
        bail!("symbol cannot be empty");
    }

    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(3);
    ensure!(
        (1..=10).contains(&max_depth),
        "max_depth must be between 1 and 10"
    );

    let include_files = args
        .get("include_files")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let include_indirect = args
        .get("include_indirect")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(500);
    ensure!(
        (1..=5_000).contains(&max_results),
        "max_results must be between 1 and 5000"
    );

    let pagination = parse_pagination(args, 100, 500)?;

    Ok(DependencyImpactArgs {
        symbol: symbol.to_string(),
        path: path.to_string(),
        // Validated i64 fits in usize on this platform; reject if out of range
        max_depth: max_depth
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_depth out of range"))?,
        include_files,
        include_indirect,
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        pagination,
    })
}

/// Validate `semantic_diff` arguments.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are invalid.
#[cfg(any(test, fuzzing))]
pub fn validate_semantic_diff_args(args: &Value) -> Result<SemanticDiffArgs> {
    let base = args
        .get("base")
        .ok_or_else(|| anyhow::anyhow!("Missing required field: base"))?;
    let base_obj = base
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("base must be an object"))?;
    let base_ref = base_obj
        .get("ref")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("base.ref is required"))?;
    if base_ref.trim().is_empty() {
        bail!("base.ref cannot be empty");
    }
    let base_file_path = base_obj
        .get("file_path")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);

    let target = args
        .get("target")
        .ok_or_else(|| anyhow::anyhow!("Missing required field: target"))?;
    let target_obj = target
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("target must be an object"))?;
    let target_ref = target_obj
        .get("ref")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("target.ref is required"))?;
    if target_ref.trim().is_empty() {
        bail!("target.ref cannot be empty");
    }
    let target_file_path = target_obj
        .get("file_path")
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string);

    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let include_unchanged = args
        .get("include_unchanged")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let filters = parse_semantic_diff_filters(args.get("filters"))?;

    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(500);
    ensure!(
        (1..=5_000).contains(&max_results),
        "max_results must be between 1 and 5000"
    );

    let pagination = parse_pagination(args, 100, 500)?;

    Ok(SemanticDiffArgs {
        base: GitVersionRef {
            git_ref: base_ref.to_string(),
            file_path: base_file_path,
        },
        target: GitVersionRef {
            git_ref: target_ref.to_string(),
            file_path: target_file_path,
        },
        path: path.to_string(),
        include_unchanged,
        filters,
        // Validated i64 fits in usize on this platform; reject if out of range
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        pagination,
    })
}

#[cfg(any(test, fuzzing))]
fn parse_semantic_diff_filters(value: Option<&Value>) -> Result<SemanticDiffFilters> {
    let Some(val) = value else {
        return Ok(SemanticDiffFilters::default());
    };
    ensure!(val.is_object(), "filters must be an object");
    let mut filters = SemanticDiffFilters::default();

    if let Some(change_types_val) = val.get("change_types") {
        let change_types = change_types_val
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("filters.change_types must be an array"))?
            .iter()
            .map(|entry| {
                let s = entry.as_str().ok_or_else(|| {
                    anyhow::anyhow!("filters.change_types entries must be strings")
                })?;
                match s {
                    "added" => Ok(ChangeType::Added),
                    "removed" => Ok(ChangeType::Removed),
                    "modified" => Ok(ChangeType::Modified),
                    "renamed" => Ok(ChangeType::Renamed),
                    "signature_changed" => Ok(ChangeType::SignatureChanged),
                    other => bail!("Invalid change type: {other}"),
                }
            })
            .collect::<Result<Vec<_>>>()?;
        filters.change_types = change_types;
    }

    if let Some(symbol_kinds_val) = val.get("symbol_kinds") {
        let kinds = symbol_kinds_val
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("filters.symbol_kinds must be an array"))?
            .iter()
            .map(|entry| {
                let s = entry.as_str().ok_or_else(|| {
                    anyhow::anyhow!("filters.symbol_kinds entries must be strings")
                })?;
                Ok(s.trim().to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        filters.symbol_kinds = kinds.into_iter().filter(|s| !s.is_empty()).collect();
    }

    Ok(filters)
}

#[cfg(any(test, fuzzing))]
fn parse_filters(value: Option<&Value>, root: &Value) -> Result<SearchFilters> {
    let Some(val) = value else {
        return Ok(SearchFilters::default());
    };
    ensure!(val.is_object(), "filters must be an object");
    let mut filters = SearchFilters::default();

    if let Some(lang_val) = val.get("language") {
        let langs = lang_val
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("filters.language must be an array"))?
            .iter()
            .map(|entry| {
                let s = entry
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("filters.language entries must be strings"))?;
                Ok(s.trim().to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        filters.languages = langs.into_iter().filter(|s| !s.is_empty()).collect();
    }

    if let Some(vis_val) = val.get("visibility") {
        let vis_str = vis_val
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("filters.visibility must be a string"))?;
        filters.visibility = match vis_str {
            "public" => Some(Visibility::Public),
            "private" => Some(Visibility::Private),
            other => bail!("Invalid visibility: {other}"),
        };
    }

    if let Some(kind_val) = val.get("kind") {
        let kind = kind_val
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("filters.kind must be a string"))?;
        if kind.trim().is_empty() {
            bail!("filters.kind cannot be empty");
        }
        filters.kinds.push(kind.trim().to_string());
    }

    if let Some(kind_array) = val.get("symbol_kind") {
        let kinds = kind_array
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("filters.symbol_kind must be an array"))?
            .iter()
            .map(|entry| {
                let s = entry.as_str().ok_or_else(|| {
                    anyhow::anyhow!("filters.symbol_kind entries must be strings")
                })?;
                Ok(s.trim().to_string())
            })
            .collect::<Result<Vec<_>>>()?;
        filters
            .kinds
            .extend(kinds.into_iter().filter(|s| !s.is_empty()));
    }

    if let Some(score_min) = val
        .get("score_min")
        .or_else(|| root.get("score_min"))
        .or_else(|| root.get("min_score"))
    {
        let score = score_min
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("score_min must be a number"))?;
        ensure!(
            (0.0..=1.0).contains(&score),
            "score_min must be between 0.0 and 1.0"
        );
        filters.min_score = Some(score);
    }

    filters.kinds.retain(|value| !value.trim().is_empty());

    Ok(filters)
}

#[cfg(any(test, fuzzing))]
fn parse_pagination(args: &Value, default_size: usize, max_size: usize) -> Result<PaginationArgs> {
    use crate::pagination::decode_cursor;

    let pagination_obj = args.get("pagination");

    let page_size = pagination_obj
        .and_then(|obj| obj.get("page_size").or_else(|| obj.get("pageSize")))
        .and_then(serde_json::Value::as_i64)
        .or_else(|| args.get("page_size").and_then(serde_json::Value::as_i64))
        // Validated i64 fits in usize on this platform; clamp to max
        .unwrap_or(default_size.try_into().unwrap_or(i64::MAX));
    // Validated i64 fits in usize on this platform; reject if out of range
    let page_size_usize: usize = page_size
        .try_into()
        .map_err(|_| anyhow::anyhow!("page_size out of range"))?;
    ensure!(
        page_size >= 1 && page_size_usize <= max_size,
        "page_size must be between 1 and {max_size}"
    );

    let cursor_value = pagination_obj
        .and_then(|obj| obj.get("cursor"))
        .or_else(|| args.get("page_token"));

    let offset = if let Some(token) = cursor_value {
        let token_str = token
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("cursor must be a string"))?;
        decode_cursor(token_str)?
    } else {
        0
    };

    Ok(PaginationArgs {
        offset,
        size: page_size_usize,
    })
}

/// Validate `hierarchical_search` arguments.
///
/// Hierarchical search returns semantic search results organized into a
/// file → container → symbol hierarchy optimized for RAG token budgets.
///
/// # Errors
///
/// Returns an error if required fields are missing or values are out of range.
#[allow(clippy::too_many_lines)] // Parsing logic mirrors MCP argument schema.
#[cfg(any(test, fuzzing))]
pub fn validate_hierarchical_search_args(args: &Value) -> Result<HierarchicalSearchArgs> {
    // Required: query
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required field: query"))?;
    if query.trim().is_empty() {
        bail!("query cannot be empty");
    }

    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    // max_results: 1-1000, default 200
    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(200);
    ensure!(
        (1..=1000).contains(&max_results),
        "max_results must be between 1 and 1000"
    );

    // context_lines: 0-20, default 3
    let context_lines = args
        .get("context_lines")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(3);
    ensure!(
        (0..=20).contains(&context_lines),
        "context_lines must be between 0 and 20"
    );

    // auto_merge: default true
    let auto_merge = args
        .get("auto_merge")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);

    // merge_threshold: 0-1000, default 256
    let merge_threshold = args
        .get("merge_threshold")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(256);
    ensure!(
        (0..=1000).contains(&merge_threshold),
        "merge_threshold must be between 0 and 1000"
    );

    // max_files: 1-100, default 20
    let max_files = args
        .get("max_files")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(20);
    ensure!(
        (1..=100).contains(&max_files),
        "max_files must be between 1 and 100"
    );

    // max_containers_per_file: 1-200, default 50
    let max_containers_per_file = args
        .get("max_containers_per_file")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(50);
    ensure!(
        (1..=200).contains(&max_containers_per_file),
        "max_containers_per_file must be between 1 and 200"
    );

    // max_symbols_per_container: 1-500, default 100
    let max_symbols_per_container = args
        .get("max_symbols_per_container")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(100);
    ensure!(
        (1..=500).contains(&max_symbols_per_container),
        "max_symbols_per_container must be between 1 and 500"
    );

    // max_total_symbols: 1-5000, default 2000
    let max_total_symbols = args
        .get("max_total_symbols")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(2000);
    ensure!(
        (1..=5000).contains(&max_total_symbols),
        "max_total_symbols must be between 1 and 5000"
    );

    // file_target_tokens: 100-10000, default 2000
    let file_target_tokens = args
        .get("file_target_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(2000);
    ensure!(
        (100..=10000).contains(&file_target_tokens),
        "file_target_tokens must be between 100 and 10000"
    );

    // container_target_tokens: 100-5000, default 1500
    let container_target_tokens = args
        .get("container_target_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(1500);
    ensure!(
        (100..=5000).contains(&container_target_tokens),
        "container_target_tokens must be between 100 and 5000"
    );

    // symbol_target_tokens: 50-2000, default 500
    let symbol_target_tokens = args
        .get("symbol_target_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(500);
    ensure!(
        (50..=2000).contains(&symbol_target_tokens),
        "symbol_target_tokens must be between 50 and 2000"
    );

    // context_cluster_target_tokens: 100-2000, default 768
    let context_cluster_target_tokens = args
        .get("context_cluster_target_tokens")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(768);
    ensure!(
        (100..=2000).contains(&context_cluster_target_tokens),
        "context_cluster_target_tokens must be between 100 and 2000"
    );

    // include_file_context: default false
    let include_file_context = args
        .get("include_file_context")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // include_container_context: default false
    let include_container_context = args
        .get("include_container_context")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    // expand_files: array of file paths to expand from stubs (default: empty)
    let expand_files: Vec<String> = args
        .get("expand_files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let filters = parse_filters(args.get("filters"), args)?;
    let pagination = parse_pagination(args, 20, 100)?;

    let score_min = filters.min_score;

    Ok(HierarchicalSearchArgs {
        query: query.to_string(),
        path: path.to_string(),
        filters,
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        context_lines: context_lines
            .try_into()
            .map_err(|_| anyhow::anyhow!("context_lines out of range"))?,
        pagination,
        score_min,
        auto_merge,
        merge_threshold: merge_threshold
            .try_into()
            .map_err(|_| anyhow::anyhow!("merge_threshold out of range"))?,
        max_files: max_files
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_files out of range"))?,
        max_containers_per_file: max_containers_per_file
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_containers_per_file out of range"))?,
        max_symbols_per_container: max_symbols_per_container
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_symbols_per_container out of range"))?,
        max_total_symbols: max_total_symbols
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_total_symbols out of range"))?,
        file_target_tokens: file_target_tokens
            .try_into()
            .map_err(|_| anyhow::anyhow!("file_target_tokens out of range"))?,
        container_target_tokens: container_target_tokens
            .try_into()
            .map_err(|_| anyhow::anyhow!("container_target_tokens out of range"))?,
        symbol_target_tokens: symbol_target_tokens
            .try_into()
            .map_err(|_| anyhow::anyhow!("symbol_target_tokens out of range"))?,
        context_cluster_target_tokens: context_cluster_target_tokens
            .try_into()
            .map_err(|_| anyhow::anyhow!("context_cluster_target_tokens out of range"))?,
        include_file_context,
        include_container_context,
        expand_files,
    })
}

/// Validate `sqry_ask` tool arguments (P2-18)
///
/// Deserializes and validates the natural language query parameters.
/// Returns a validated `SqryAskParams` ready for execution.
#[cfg(any(test, fuzzing))]
pub fn validate_sqry_ask_args(args: &Value) -> Result<super::params::SqryAskParams> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing required field: query"))?;
    if query.trim().is_empty() {
        bail!("query cannot be empty");
    }
    ensure!(
        query.len() <= 4096,
        "query exceeds maximum length of 4096 characters"
    );

    // Validate path type - must be string if provided, fail on type mismatch
    let path = match args.get("path") {
        Some(v) if v.is_string() => v.as_str().unwrap(),
        Some(v) if v.is_null() => ".", // null is acceptable, defaults to "."
        Some(_) => bail!("path must be a string"),
        None => ".",
    };

    let execute = match args.get("execute") {
        Some(v) if v.is_boolean() => v.as_bool().unwrap_or(false),
        Some(v) if v.is_null() => false,
        Some(_) => bail!("execute must be a boolean"),
        None => false,
    };

    Ok(super::params::SqryAskParams {
        query: query.to_string(),
        path: path.to_string(),
        execute,
    })
}

/// Validate `find_duplicates` arguments.
///
/// # Errors
///
/// Returns an error if values are out of range.
#[cfg(any(test, fuzzing))]
pub fn validate_find_duplicates_args(args: &Value) -> Result<FindDuplicatesArgs> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let duplicate_type = match args
        .get("duplicate_type")
        .and_then(|v| v.as_str())
        .unwrap_or("body")
    {
        "body" => DuplicateType::Body,
        "signature" => DuplicateType::Signature,
        "struct" => DuplicateType::Struct,
        other => bail!("Invalid duplicate_type: {other}. Use: body, signature, struct"),
    };

    let threshold = args
        .get("threshold")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(80);
    ensure!(
        (0..=100).contains(&threshold),
        "threshold must be between 0 and 100"
    );

    let exact = args
        .get("exact")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(100);
    ensure!(
        (1..=1000).contains(&max_results),
        "max_results must be between 1 and 1000"
    );

    let pagination = parse_pagination(args, 50, 500)?;

    Ok(FindDuplicatesArgs {
        path: path.to_string(),
        duplicate_type,
        threshold: threshold
            .try_into()
            .map_err(|_| anyhow::anyhow!("threshold out of range"))?,
        exact,
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        pagination,
    })
}

/// Validate `find_cycles` arguments.
///
/// # Errors
///
/// Returns an error if values are out of range.
#[cfg(any(test, fuzzing))]
pub fn validate_find_cycles_args(args: &Value) -> Result<FindCyclesArgs> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let cycle_type = match args
        .get("cycle_type")
        .and_then(|v| v.as_str())
        .unwrap_or("calls")
    {
        "calls" => CycleType::Calls,
        "imports" => CycleType::Imports,
        "modules" => CycleType::Modules,
        other => bail!("Invalid cycle_type: {other}. Use: calls, imports, modules"),
    };

    let min_depth = args
        .get("min_depth")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(2);
    ensure!(min_depth >= 2, "min_depth must be at least 2");

    let max_depth = args.get("max_depth").and_then(serde_json::Value::as_i64);
    if let Some(max) = max_depth {
        ensure!(max >= 2, "max_depth must be at least 2");
        ensure!(max >= min_depth, "max_depth must be >= min_depth");
    }

    let include_self_loops = args
        .get("include_self_loops")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(100);
    ensure!(
        (1..=500).contains(&max_results),
        "max_results must be between 1 and 500"
    );

    let pagination = parse_pagination(args, 50, 200)?;

    Ok(FindCyclesArgs {
        path: path.to_string(),
        cycle_type,
        min_depth: min_depth
            .try_into()
            .map_err(|_| anyhow::anyhow!("min_depth out of range"))?,
        max_depth: max_depth
            .map(|v| {
                v.try_into()
                    .map_err(|_| anyhow::anyhow!("max_depth out of range"))
            })
            .transpose()?,
        include_self_loops,
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        pagination,
    })
}

/// Validate `find_unused` arguments.
///
/// # Errors
///
/// Returns an error if values are out of range.
#[cfg(any(test, fuzzing))]
pub fn validate_find_unused_args(args: &Value) -> Result<FindUnusedArgs> {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    let scope = match args.get("scope").and_then(|v| v.as_str()).unwrap_or("all") {
        "public" => UnusedScope::Public,
        "private" => UnusedScope::Private,
        "function" => UnusedScope::Function,
        "struct" => UnusedScope::Struct,
        "all" => UnusedScope::All,
        other => bail!("Invalid scope: {other}. Use: public, private, function, struct, all"),
    };

    let languages = args
        .get("language")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let kinds = args
        .get("symbol_kind")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let max_results = args
        .get("max_results")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(100);
    ensure!(
        (1..=1000).contains(&max_results),
        "max_results must be between 1 and 1000"
    );

    let pagination = parse_pagination(args, 50, 500)?;

    Ok(FindUnusedArgs {
        path: path.to_string(),
        scope,
        languages,
        kinds,
        max_results: max_results
            .try_into()
            .map_err(|_| anyhow::anyhow!("max_results out of range"))?,
        pagination,
    })
}

// ============================================================================
// New Graph-Based Tool Args Types
// ============================================================================

/// Arguments for `is_node_in_cycle` tool.
#[derive(Debug, Clone)]
pub struct IsNodeInCycleArgs {
    /// Symbol to check
    pub symbol: String,
    /// Workspace path
    pub path: String,
    /// Type of cycle to check
    pub cycle_type: CycleType,
    /// Minimum cycle depth to consider
    pub min_depth: usize,
    /// Maximum cycle depth to consider (None = unbounded)
    pub max_depth: Option<usize>,
    /// Whether to include self-loops as cycles
    pub include_self_loops: bool,
}

/// Arguments for `pattern_search` tool.
#[derive(Debug, Clone)]
pub struct PatternSearchArgs {
    /// Search pattern (substring match)
    pub pattern: String,
    /// Workspace path
    pub path: String,
    /// Maximum results
    pub max_results: usize,
    /// Pagination
    pub pagination: PaginationArgs,
}

/// Arguments for `direct_callers` tool.
#[derive(Debug, Clone)]
pub struct DirectCallersArgs {
    /// Symbol to find callers for
    pub symbol: String,
    /// Workspace path
    pub path: String,
    /// Maximum results
    pub max_results: usize,
    /// Pagination
    pub pagination: PaginationArgs,
}

/// Arguments for `direct_callees` tool.
#[derive(Debug, Clone)]
pub struct DirectCalleesArgs {
    /// Symbol to find callees for
    pub symbol: String,
    /// Workspace path
    pub path: String,
    /// Maximum results
    pub max_results: usize,
    /// Pagination
    pub pagination: PaginationArgs,
}

// ============================================================================
// Introspection Tool Args
// ============================================================================

/// Arguments for `list_files` tool.
#[derive(Debug, Clone)]
pub struct ListFilesArgs {
    /// Workspace path
    pub path: String,
    /// Optional language filter
    pub language: Option<String>,
    /// Maximum results
    pub max_results: usize,
    /// Pagination
    pub pagination: PaginationArgs,
}

/// Arguments for `list_symbols` tool.
#[derive(Debug, Clone)]
pub struct ListSymbolsArgs {
    /// Workspace path
    pub path: String,
    /// Optional kind filter (function, class, method, etc.)
    pub kind: Option<String>,
    /// Optional language filter
    pub language: Option<String>,
    /// Maximum results
    pub max_results: usize,
    /// Pagination
    pub pagination: PaginationArgs,
}

/// Arguments for `get_graph_stats` tool.
#[derive(Debug, Clone)]
pub struct GetGraphStatsArgs {
    /// Workspace path
    pub path: String,
}

// ============================================================================
// Navigation Tool Args
// ============================================================================

/// Arguments for `get_definition` tool.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GetDefinitionArgs {
    /// Symbol name to look up
    pub symbol: String,
    /// Workspace path (reserved for multi-workspace support)
    pub path: String,
}

/// Arguments for `get_references` tool.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GetReferencesArgs {
    /// Symbol name to find references for
    pub symbol: String,
    /// Workspace path (reserved for multi-workspace support)
    pub path: String,
    /// Whether to include the declaration
    pub include_declaration: bool,
    /// Maximum results
    pub max_results: usize,
    /// Pagination (reserved for paginated results)
    pub pagination: PaginationArgs,
}

/// Arguments for `get_hover_info` tool.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GetHoverInfoArgs {
    /// Symbol name to get info for
    pub symbol: String,
    /// Workspace path (reserved for multi-workspace support)
    pub path: String,
}

/// Arguments for `get_document_symbols` tool.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GetDocumentSymbolsArgs {
    /// File path to get symbols from
    pub file_path: String,
    /// Workspace path (reserved for multi-workspace support)
    pub path: String,
}

/// Arguments for `get_workspace_symbols` tool.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct GetWorkspaceSymbolsArgs {
    /// Query to search for
    pub query: String,
    /// Workspace path (reserved for multi-workspace support)
    pub path: String,
    /// Maximum results
    pub max_results: usize,
    /// Pagination (reserved for paginated results)
    pub pagination: PaginationArgs,
}

// ============================================================================
// Insights Tool Arguments
// ============================================================================

/// Arguments for `get_insights` tool.
#[derive(Debug, Clone)]
pub struct GetInsightsArgs {
    /// Workspace path
    pub path: String,
}

/// Arguments for `complexity_metrics` tool.
#[derive(Debug, Clone)]
pub struct ComplexityMetricsArgs {
    /// Workspace path
    pub path: String,
    /// Optional target file or symbol to filter
    pub target: Option<String>,
    /// Minimum complexity to report
    pub min_complexity: u32,
    /// Sort by complexity descending
    pub sort_by_complexity: bool,
    /// Maximum results
    pub max_results: usize,
}

/// Arguments for `rebuild_index` tool.
#[derive(Debug, Clone)]
pub struct RebuildIndexArgs {
    /// Workspace path
    pub path: String,
    /// Force rebuild even if index exists
    pub force: bool,
}

/// Direction for call hierarchy traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallHierarchyDirection {
    /// Find callers (incoming calls)
    Incoming,
    /// Find callees (outgoing calls)
    Outgoing,
}

impl CallHierarchyDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            CallHierarchyDirection::Incoming => "incoming",
            CallHierarchyDirection::Outgoing => "outgoing",
        }
    }
}

/// Arguments for `call_hierarchy` tool.
#[derive(Debug, Clone)]
pub struct CallHierarchyArgs {
    /// Symbol name or qualified identifier
    pub symbol: String,
    /// Optional file path to disambiguate
    pub file_path: Option<String>,
    /// Direction of the call hierarchy
    pub direction: CallHierarchyDirection,
    /// Workspace path
    pub path: String,
    /// Maximum depth of the call tree
    pub max_depth: usize,
    /// Maximum results
    pub max_results: usize,
    /// Pagination args
    pub pagination: PaginationArgs,
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;
    use serde_json::json;

    #[test]
    fn semantic_search_defaults_are_applied() {
        let args = json!({ "query": "kind:function" });
        let parsed = validate_semantic_search_args(&args).unwrap();
        assert_eq!(parsed.path, ".");
        assert_eq!(parsed.max_results, 200);
        assert_eq!(parsed.context_lines, 3);
        assert_eq!(parsed.pagination.offset, 0);
        assert_eq!(parsed.pagination.size, 50);
    }

    #[test]
    fn semantic_search_filters_and_pagination() {
        let args = json!({
            "query": "kind:function",
            "filters": {
                "language": ["rust", "typescript"],
                "visibility": "public",
                "symbol_kind": ["method", "function"],
                "score_min": 0.4
            },
            "page_token": "10",
            "page_size": 20
        });
        let parsed = validate_semantic_search_args(&args).unwrap();
        assert_eq!(parsed.filters.languages, vec!["rust", "typescript"]);
        assert_eq!(parsed.filters.visibility, Some(Visibility::Public));
        assert_eq!(parsed.filters.kinds, vec!["method", "function"]);
        assert_eq!(parsed.filters.min_score, Some(0.4));
        assert_eq!(parsed.pagination.offset, 10);
        assert_eq!(parsed.pagination.size, 20);
    }

    #[test]
    fn semantic_search_supports_nested_pagination() {
        let args = json!({
            "query": "kind:function",
            "pagination": {
                "cursor": "offset:5",
                "page_size": 15
            }
        });
        let parsed = validate_semantic_search_args(&args).unwrap();
        assert_eq!(parsed.pagination.offset, 5);
        assert_eq!(parsed.pagination.size, 15);
    }

    #[test]
    fn relation_query_defaults() {
        let args = json!({
            "symbol": "helper",
            "relation_type": "callers"
        });
        let parsed = validate_relation_query_args(&args).unwrap();
        assert_eq!(parsed.relation, RelationType::Callers);
        assert_eq!(parsed.max_depth, 1);
        assert_eq!(parsed.max_results, 200);
    }

    #[test]
    fn relation_query_rejects_invalid_relation() {
        let args = json!({
            "symbol": "helper",
            "relation_type": "unknown"
        });
        assert!(validate_relation_query_args(&args).is_err());
    }

    #[test]
    fn explain_code_defaults_flags() {
        let args = json!({
            "file_path": "src/main.rs",
            "symbol_name": "main"
        });
        let parsed = validate_explain_code_args(&args).unwrap();
        assert!(parsed.include_context);
        assert!(parsed.include_relations);
    }

    #[test]
    fn get_dependencies_requires_target() {
        let args = json!({ "path": "." });
        assert!(validate_show_dependencies_args(&args).is_err());
    }

    #[test]
    fn index_status_defaults_to_workspace() {
        let args = json!({});
        let parsed = validate_get_index_status_args(&args);
        assert_eq!(parsed.path, ".");
    }

    #[test]
    fn find_similar_requires_reference() {
        let args = json!({});
        assert!(validate_search_similar_args(&args).is_err());
    }

    #[test]
    fn find_similar_defaults() {
        let args = json!({
            "reference": {
                "file_path": "sqry-mcp/src/server.rs",
                "symbol_name": "SqryServer"
            }
        });
        let parsed = validate_search_similar_args(&args).unwrap();
        assert_eq!(parsed.path, ".");
        assert_abs_diff_eq!(parsed.similarity_threshold, 0.7, epsilon = 1e-10);
        assert_eq!(parsed.max_results, 20);
        assert_eq!(parsed.pagination.size, 20);
    }

    // ===== sqry_ask validation tests (P2-18) =====

    #[test]
    fn sqry_ask_valid_query_and_path() {
        let args = json!({
            "query": "find all public functions",
            "path": "src/"
        });
        let parsed = validate_sqry_ask_args(&args).unwrap();
        assert_eq!(parsed.query, "find all public functions");
        assert_eq!(parsed.path, "src/");
    }

    #[test]
    fn sqry_ask_defaults_path_to_dot() {
        let args = json!({
            "query": "find functions"
        });
        let parsed = validate_sqry_ask_args(&args).unwrap();
        assert_eq!(parsed.query, "find functions");
        assert_eq!(parsed.path, ".");
    }

    #[test]
    fn sqry_ask_null_path_defaults_to_dot() {
        let args = json!({
            "query": "find functions",
            "path": null
        });
        let parsed = validate_sqry_ask_args(&args).unwrap();
        assert_eq!(parsed.path, ".");
    }

    #[test]
    fn sqry_ask_rejects_numeric_path() {
        let args = json!({
            "query": "find functions",
            "path": 123
        });
        let result = validate_sqry_ask_args(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("path must be a string"), "Error was: {err}");
    }

    #[test]
    fn sqry_ask_rejects_object_path() {
        let args = json!({
            "query": "find functions",
            "path": { "dir": "src" }
        });
        let result = validate_sqry_ask_args(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("path must be a string"), "Error was: {err}");
    }

    #[test]
    fn sqry_ask_requires_query() {
        let args = json!({
            "path": "."
        });
        let result = validate_sqry_ask_args(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("query"), "Error was: {err}");
    }

    #[test]
    fn sqry_ask_rejects_empty_query() {
        let args = json!({
            "query": "   ",
            "path": "."
        });
        let result = validate_sqry_ask_args(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("empty"), "Error was: {err}");
    }

    #[test]
    fn sqry_ask_rejects_oversized_query() {
        let long_query = "a".repeat(5000);
        let args = json!({
            "query": long_query,
            "path": "."
        });
        let result = validate_sqry_ask_args(&args);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("maximum length") || err.contains("4096"),
            "Error was: {err}"
        );
    }

    #[test]
    fn sqry_ask_execute_true() {
        let args = json!({
            "query": "find functions",
            "execute": true
        });
        let parsed = validate_sqry_ask_args(&args).unwrap();
        assert!(parsed.execute);
    }

    #[test]
    fn sqry_ask_execute_null_defaults_false() {
        let args = json!({
            "query": "find functions",
            "execute": null
        });
        let parsed = validate_sqry_ask_args(&args).unwrap();
        assert!(!parsed.execute);
    }

    #[test]
    fn sqry_ask_execute_missing_defaults_false() {
        let args = json!({
            "query": "find functions"
        });
        let parsed = validate_sqry_ask_args(&args).unwrap();
        assert!(!parsed.execute);
    }

    #[test]
    fn sqry_ask_rejects_non_bool_execute() {
        let args = json!({
            "query": "find functions",
            "execute": "yes"
        });
        let result = validate_sqry_ask_args(&args);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("execute must be a boolean"),
        );
    }
}
