//! Schemars-derived parameter types for MCP tools.
//!
//! These types use serde for deserialization with defaults and
//! schemars for JSON schema generation. They replace the manual
//! validation in `validation.rs` for the rmcp migration.
//!
//! ## Canonical Types
//!
//! This module imports canonical schema types from `sqry_core::schema` and
//! provides JsonSchema-compatible wrappers for MCP tool schema generation.
//! The canonical types are the single source of truth for semantic enums.

use crate::error::RpcError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Import canonical types from sqry_core::schema
// These are re-exported with JsonSchema wrappers for MCP compatibility
use sqry_core::schema::{
    ChangeKind as CoreChangeKind, CycleKind as CoreCycleKind, DuplicateKind as CoreDuplicateKind,
    OutputFormat as CoreOutputFormat, RelationKind as CoreRelationKind,
    UnusedScope as CoreUnusedScope, Visibility as CoreVisibility,
};

// ============================================================================
// Helper Types
// ============================================================================

/// Search filters.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default)]
pub struct SearchFiltersParams {
    /// Limit results to specific languages
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub language: Vec<String>,

    /// Filter by visibility
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<VisibilityParam>,

    /// Filter by symbol kinds (function, method, class, etc.)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbol_kind: Vec<String>,

    /// Minimum semantic relevance score (0.0 - 1.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub score_min: Option<f64>,
}

/// Visibility filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum VisibilityParam {
    Public,
    Private,
}

impl From<VisibilityParam> for CoreVisibility {
    fn from(v: VisibilityParam) -> Self {
        match v {
            VisibilityParam::Public => CoreVisibility::Public,
            VisibilityParam::Private => CoreVisibility::Private,
        }
    }
}

impl From<CoreVisibility> for VisibilityParam {
    fn from(v: CoreVisibility) -> Self {
        match v {
            CoreVisibility::Public => VisibilityParam::Public,
            CoreVisibility::Private => VisibilityParam::Private,
        }
    }
}

/// Pagination parameters.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default)]
pub struct PaginationParams {
    /// Opaque cursor returned from previous page
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,

    /// Number of results per page
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1, max = 500))]
    pub page_size: Option<i64>,
}

/// Relation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RelationTypeParam {
    Callers,
    Callees,
    Imports,
    Exports,
    Returns,
}

impl From<RelationTypeParam> for CoreRelationKind {
    fn from(r: RelationTypeParam) -> Self {
        match r {
            RelationTypeParam::Callers => CoreRelationKind::Callers,
            RelationTypeParam::Callees => CoreRelationKind::Callees,
            RelationTypeParam::Imports => CoreRelationKind::Imports,
            RelationTypeParam::Exports => CoreRelationKind::Exports,
            RelationTypeParam::Returns => CoreRelationKind::Returns,
        }
    }
}

impl From<CoreRelationKind> for RelationTypeParam {
    fn from(r: CoreRelationKind) -> Self {
        match r {
            CoreRelationKind::Callers => RelationTypeParam::Callers,
            CoreRelationKind::Callees => RelationTypeParam::Callees,
            CoreRelationKind::Imports => RelationTypeParam::Imports,
            CoreRelationKind::Exports => RelationTypeParam::Exports,
            CoreRelationKind::Returns => RelationTypeParam::Returns,
        }
    }
}

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum GraphFormatParam {
    Json,
    Dot,
    D2,
    Mermaid,
}

impl Default for GraphFormatParam {
    fn default() -> Self {
        Self::Json
    }
}

impl From<GraphFormatParam> for CoreOutputFormat {
    fn from(f: GraphFormatParam) -> Self {
        match f {
            GraphFormatParam::Json => CoreOutputFormat::Json,
            GraphFormatParam::Dot => CoreOutputFormat::Dot,
            GraphFormatParam::D2 => CoreOutputFormat::D2,
            GraphFormatParam::Mermaid => CoreOutputFormat::Mermaid,
        }
    }
}

impl From<CoreOutputFormat> for GraphFormatParam {
    fn from(f: CoreOutputFormat) -> Self {
        match f {
            CoreOutputFormat::Json => GraphFormatParam::Json,
            CoreOutputFormat::Dot => GraphFormatParam::Dot,
            CoreOutputFormat::D2 => GraphFormatParam::D2,
            CoreOutputFormat::Mermaid => GraphFormatParam::Mermaid,
        }
    }
}

/// Edge kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKindParam {
    Calls,
    Imports,
    Exports,
    Returns,
}

/// Change type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeTypeParam {
    Added,
    Removed,
    Modified,
    Renamed,
    SignatureChanged,
}

impl From<ChangeTypeParam> for CoreChangeKind {
    fn from(c: ChangeTypeParam) -> Self {
        match c {
            ChangeTypeParam::Added => CoreChangeKind::Added,
            ChangeTypeParam::Removed => CoreChangeKind::Removed,
            ChangeTypeParam::Modified => CoreChangeKind::Modified,
            ChangeTypeParam::Renamed => CoreChangeKind::Renamed,
            ChangeTypeParam::SignatureChanged => CoreChangeKind::SignatureChanged,
        }
    }
}

impl From<CoreChangeKind> for ChangeTypeParam {
    fn from(c: CoreChangeKind) -> Self {
        match c {
            CoreChangeKind::Added => ChangeTypeParam::Added,
            CoreChangeKind::Removed => ChangeTypeParam::Removed,
            CoreChangeKind::Modified => ChangeTypeParam::Modified,
            CoreChangeKind::Renamed => ChangeTypeParam::Renamed,
            CoreChangeKind::SignatureChanged => ChangeTypeParam::SignatureChanged,
        }
    }
}

/// Diff filters.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default)]
pub struct SemanticDiffFiltersParams {
    /// Filter by change types
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub change_types: Vec<ChangeTypeParam>,

    /// Filter by symbol kinds (function, class, etc.)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbol_kinds: Vec<String>,
}

/// Git version ref.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GitVersionRefParams {
    /// Git ref
    #[serde(rename = "ref")]
    pub git_ref: String,

    /// Optional file path to limit comparison
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

/// Reference symbol.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ReferenceParams {
    pub file_path: String,

    /// Name of the reference symbol
    pub symbol_name: String,
}

// ============================================================================
// Tool Parameter Types
// ============================================================================

/// `semantic_search` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[schemars(example = "json!({
    \"query\": \"kind:function name~=/auth/ visibility:public\",
    \"path\": \"src/\",
    \"max_results\": 50,
    \"context_lines\": 3
})")]
pub struct SemanticSearchParams {
    /// Semantic query expression
    pub query: String,

    #[serde(default = "default_path")]
    pub path: String,

    /// Search filters
    #[serde(default)]
    pub filters: Option<SearchFiltersParams>,

    #[serde(default = "default_max_results_200")]
    #[schemars(range(min = 1, max = 10000))]
    pub max_results: i64,

    /// Context lines around matches
    #[serde(default = "default_context_lines")]
    #[schemars(range(min = 0, max = 20))]
    pub context_lines: i64,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,
}

/// `hierarchical_search` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[schemars(example = "json!({
    \"query\": \"kind:class name:UserService\",
    \"include_file_context\": true,
    \"include_container_context\": true,
    \"max_files\": 10
})")]
pub struct HierarchicalSearchParams {
    /// Query expression
    pub query: String,

    #[serde(default = "default_path")]
    pub path: String,

    /// Search filters
    #[serde(default)]
    pub filters: Option<SearchFiltersParams>,

    /// Maximum symbols to return (before file grouping)
    #[serde(default = "default_max_results_200")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: i64,

    /// Max total symbols
    #[serde(default = "default_max_total_symbols")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_total_symbols: i64,

    /// Context lines around matches
    #[serde(default = "default_context_lines")]
    #[schemars(range(min = 0, max = 20))]
    pub context_lines: i64,

    /// Auto-expand small symbols
    #[serde(default = "default_true")]
    pub auto_merge: bool,

    /// Merge threshold (tokens)
    #[serde(default = "default_merge_threshold")]
    #[schemars(range(min = 0, max = 1000))]
    pub merge_threshold: i64,

    #[serde(default = "default_max_files")]
    #[schemars(range(min = 1, max = 100))]
    pub max_files: i64,

    #[serde(default = "default_max_containers_per_file")]
    #[schemars(range(min = 1, max = 200))]
    pub max_containers_per_file: i64,

    #[serde(default = "default_max_symbols_per_container")]
    #[schemars(range(min = 1, max = 500))]
    pub max_symbols_per_container: i64,

    /// File-level token target
    #[serde(default = "default_file_target_tokens")]
    #[schemars(range(min = 100, max = 10000))]
    pub file_target_tokens: i64,

    /// Container token target
    #[serde(default = "default_container_target_tokens")]
    #[schemars(range(min = 100, max = 5000))]
    pub container_target_tokens: i64,

    /// Symbol token target
    #[serde(default = "default_symbol_target_tokens")]
    #[schemars(range(min = 50, max = 2000))]
    pub symbol_target_tokens: i64,

    /// Context cluster tokens
    #[serde(default = "default_context_cluster_target_tokens")]
    #[schemars(range(min = 100, max = 2000))]
    pub context_cluster_target_tokens: i64,

    /// Include file overview
    #[serde(default)]
    pub include_file_context: bool,

    /// Include container code
    #[serde(default)]
    pub include_container_context: bool,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,

    /// File paths to expand from stubs
    #[serde(default)]
    pub expand_files: Vec<String>,
}

impl HierarchicalSearchParams {
    /// Validate non-empty query (custom validation).
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.query.trim().is_empty() {
            return Err(RpcError::validation_with_data(
                "query cannot be empty",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "non_empty",
                    "field": "query"
                }),
            ));
        }
        Ok(())
    }
}

/// `relation_query` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct RelationQueryParams {
    /// Symbol name
    pub symbol: String,

    /// Type of relation to query
    pub relation_type: RelationTypeParam,

    #[serde(default = "default_path")]
    pub path: String,

    #[serde(default = "default_max_depth_1")]
    #[schemars(range(min = 1, max = 5))]
    pub max_depth: i64,

    #[serde(default = "default_max_results_200")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_results: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,

    #[serde(default = "default_page_size_50")]
    #[schemars(range(min = 1, max = 500))]
    pub page_size: i64,
}

/// `explain_code` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ExplainCodeParams {
    /// Source file path
    pub file_path: String,

    /// Symbol name within the file
    pub symbol_name: String,

    #[serde(default = "default_path")]
    pub path: String,

    /// Include context information
    #[serde(default = "default_true")]
    pub include_context: bool,

    /// Include relations information
    #[serde(default = "default_true")]
    pub include_relations: bool,
}

/// `search_similar` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SearchSimilarParams {
    /// Reference symbol
    pub reference: ReferenceParams,

    #[serde(default = "default_path")]
    pub path: String,

    /// Min similarity (0.0-1.0)
    #[serde(default = "default_similarity_threshold")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub similarity_threshold: f64,

    #[serde(default = "default_max_results_20")]
    #[schemars(range(min = 1, max = 200))]
    pub max_results: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,

    #[serde(default = "default_page_size_20")]
    #[schemars(range(min = 1, max = 200))]
    pub page_size: i64,
}

/// `show_dependencies` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ShowDependenciesParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,

    /// Optional symbol name to focus on
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,

    #[serde(default = "default_path")]
    pub path: String,

    #[serde(default = "default_max_depth_2")]
    #[schemars(range(min = 1, max = 5))]
    pub max_depth: i64,

    #[serde(default = "default_max_results_500")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_results: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,

    #[serde(default = "default_page_size_100")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: i64,
}

impl ShowDependenciesParams {
    /// Validate XOR constraint: at least one of `file_path` or `symbol_name` must be provided.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.file_path.is_none() && self.symbol_name.is_none() {
            return Err(RpcError::validation_with_data(
                "At least one of file_path or symbol_name must be provided",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "xor",
                    "fields": ["file_path", "symbol_name"]
                }),
            ));
        }
        Ok(())
    }
}

/// `get_index_status` params.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct GetIndexStatusParams {
    #[serde(default = "default_path")]
    pub path: String,
}

/// `export_graph` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ExportGraphParams {
    #[serde(default = "default_path")]
    pub path: String,

    /// File path with seed symbols
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,

    /// Seed symbol name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,

    #[serde(default)]
    pub format: GraphFormatParam,

    /// Verbose output
    #[serde(default)]
    pub verbose: bool,

    #[serde(default = "default_max_depth_2")]
    #[schemars(range(min = 1, max = 5))]
    pub max_depth: i64,

    #[serde(default = "default_max_results_1000")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_results: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,

    #[serde(default = "default_page_size_200")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: i64,

    /// Edge kinds to include (default: calls only)
    #[serde(default)]
    pub include: Vec<EdgeKindParam>,

    /// Optional language filter for nodes
    #[serde(default)]
    pub languages: Vec<String>,
}

impl ExportGraphParams {
    /// Validate XOR constraint: at least one of `file_path` or `symbol_name` must be provided.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.file_path.is_none() && self.symbol_name.is_none() {
            return Err(RpcError::validation_with_data(
                "At least one of file_path or symbol_name must be provided",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "xor",
                    "fields": ["file_path", "symbol_name"]
                }),
            ));
        }
        Ok(())
    }
}

/// `cross_language_edges` params.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct CrossLanguageEdgesParams {
    #[serde(default = "default_path")]
    pub path: String,

    /// Optional caller language filter (e.g., 'TypeScript')
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_lang: Option<String>,

    /// Optional callee language filter (e.g., 'JavaScript')
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_lang: Option<String>,

    #[serde(default = "default_max_results_500")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_results: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,

    #[serde(default = "default_page_size_200")]
    #[schemars(range(min = 1, max = 1000))]
    pub page_size: i64,
}

/// `trace_path` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct TracePathParams {
    pub from_symbol: String,

    pub to_symbol: String,

    #[serde(default = "default_path")]
    pub path: String,

    #[serde(default = "default_max_hops")]
    #[schemars(range(min = 1, max = 10))]
    pub max_hops: i64,

    #[serde(default = "default_max_paths")]
    #[schemars(range(min = 1, max = 20))]
    pub max_paths: i64,

    /// Allow cross-language paths
    #[serde(default = "default_true")]
    pub cross_language: bool,

    /// Min edge confidence
    #[serde(default = "default_min_confidence")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub min_confidence: f64,
}

/// `subgraph` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[allow(clippy::struct_excessive_bools)] // Mirrors tool flags without extra wrapper types.
pub struct SubgraphParams {
    /// Seed symbols to extract context around
    pub symbols: Vec<String>,

    #[serde(default = "default_path")]
    pub path: String,

    #[serde(default = "default_max_depth_2")]
    #[schemars(range(min = 1, max = 5))]
    pub max_depth: i64,

    /// Max nodes in result
    #[serde(default = "default_max_nodes")]
    #[schemars(range(min = 1, max = 500))]
    pub max_nodes: i64,

    /// Include callers
    #[serde(default = "default_true")]
    pub include_callers: bool,

    /// Include callees
    #[serde(default = "default_true")]
    pub include_callees: bool,

    /// Include import relationships
    #[serde(default)]
    pub include_imports: bool,

    /// Include cross-language edges (HTTP, FFI, etc.)
    #[serde(default = "default_true")]
    pub cross_language: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,

    #[serde(default = "default_page_size_50")]
    #[schemars(range(min = 1, max = 200))]
    pub page_size: i64,
}

impl SubgraphParams {
    /// Validate non-empty symbols array (custom validation).
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.symbols.is_empty() {
            return Err(RpcError::validation_with_data(
                "symbols array must contain at least one symbol",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "non_empty",
                    "field": "symbols"
                }),
            ));
        }
        Ok(())
    }
}

/// `dependency_impact` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DependencyImpactParams {
    /// Symbol name
    pub symbol: String,

    #[serde(default = "default_path")]
    pub path: String,

    /// Maximum depth of transitive dependencies
    #[serde(default = "default_max_depth_3")]
    #[schemars(range(min = 1, max = 10))]
    pub max_depth: i64,

    /// Include affected file paths
    #[serde(default = "default_true")]
    pub include_files: bool,

    /// Include indirect dependencies
    #[serde(default = "default_true")]
    pub include_indirect: bool,

    #[serde(default = "default_max_results_500")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_results: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,

    #[serde(default = "default_page_size_100")]
    #[schemars(range(min = 1, max = 500))]
    pub page_size: i64,
}

/// `semantic_diff` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct SemanticDiffParams {
    /// Base version for comparison
    pub base: GitVersionRefParams,

    /// Target version for comparison
    pub target: GitVersionRefParams,

    #[serde(default = "default_path")]
    pub path: String,

    /// Include unchanged symbols
    #[serde(default)]
    pub include_unchanged: bool,

    /// Filters for change types and symbol kinds
    #[serde(default)]
    pub filters: Option<SemanticDiffFiltersParams>,

    #[serde(default = "default_max_results_500")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_results: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,

    #[serde(default = "default_page_size_100")]
    #[schemars(range(min = 1, max = 500))]
    pub page_size: i64,
}

// ============================================================================
// Natural Language Tool (P2-18)
// ============================================================================

/// `sqry_ask` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[schemars(example = "json!({
    \"query\": \"who calls the authenticate function?\",
    \"path\": \".\",
    \"execute\": true
})")]
pub struct SqryAskParams {
    /// Natural language query
    pub query: String,

    #[serde(default = "default_path")]
    pub path: String,

    /// Whether to execute the translated command and return results
    #[serde(default)]
    pub execute: bool,
}

// NOTE: Validation is performed in validation.rs via validate_sqry_ask_args().
// The SqryAskParams struct is kept for schema generation via schemars and
// potential future rmcp SDK migration.

// ============================================================================
// Analysis Tool Parameter Types
// ============================================================================

/// Duplicate type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DuplicateTypeParam {
    Body,
    Signature,
    Struct,
}

impl Default for DuplicateTypeParam {
    fn default() -> Self {
        Self::Body
    }
}

impl From<DuplicateTypeParam> for CoreDuplicateKind {
    fn from(d: DuplicateTypeParam) -> Self {
        match d {
            DuplicateTypeParam::Body => CoreDuplicateKind::Body,
            DuplicateTypeParam::Signature => CoreDuplicateKind::Signature,
            DuplicateTypeParam::Struct => CoreDuplicateKind::Struct,
        }
    }
}

impl From<CoreDuplicateKind> for DuplicateTypeParam {
    fn from(d: CoreDuplicateKind) -> Self {
        match d {
            CoreDuplicateKind::Body => DuplicateTypeParam::Body,
            CoreDuplicateKind::Signature => DuplicateTypeParam::Signature,
            CoreDuplicateKind::Struct => DuplicateTypeParam::Struct,
        }
    }
}

/// Cycle type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CycleTypeParam {
    Calls,
    Imports,
    Modules,
}

impl Default for CycleTypeParam {
    fn default() -> Self {
        Self::Calls
    }
}

impl From<CycleTypeParam> for CoreCycleKind {
    fn from(c: CycleTypeParam) -> Self {
        match c {
            CycleTypeParam::Calls => CoreCycleKind::Calls,
            CycleTypeParam::Imports => CoreCycleKind::Imports,
            CycleTypeParam::Modules => CoreCycleKind::Modules,
        }
    }
}

impl From<CoreCycleKind> for CycleTypeParam {
    fn from(c: CoreCycleKind) -> Self {
        match c {
            CoreCycleKind::Calls => CycleTypeParam::Calls,
            CoreCycleKind::Imports => CycleTypeParam::Imports,
            CoreCycleKind::Modules => CycleTypeParam::Modules,
        }
    }
}

/// Unused scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum UnusedScopeParam {
    Public,
    Private,
    Function,
    Struct,
    All,
}

impl Default for UnusedScopeParam {
    fn default() -> Self {
        Self::All
    }
}

impl From<UnusedScopeParam> for CoreUnusedScope {
    fn from(s: UnusedScopeParam) -> Self {
        match s {
            UnusedScopeParam::Public => CoreUnusedScope::Public,
            UnusedScopeParam::Private => CoreUnusedScope::Private,
            UnusedScopeParam::Function => CoreUnusedScope::Function,
            UnusedScopeParam::Struct => CoreUnusedScope::Struct,
            UnusedScopeParam::All => CoreUnusedScope::All,
        }
    }
}

impl From<CoreUnusedScope> for UnusedScopeParam {
    fn from(s: CoreUnusedScope) -> Self {
        match s {
            CoreUnusedScope::Public => UnusedScopeParam::Public,
            CoreUnusedScope::Private => UnusedScopeParam::Private,
            CoreUnusedScope::Function => UnusedScopeParam::Function,
            CoreUnusedScope::Struct => UnusedScopeParam::Struct,
            CoreUnusedScope::All => UnusedScopeParam::All,
        }
    }
}

/// `find_duplicates` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FindDuplicatesParams {
    #[serde(default = "default_path")]
    pub path: String,

    /// Type of duplicate to find
    #[serde(default)]
    pub duplicate_type: DuplicateTypeParam,

    /// Similarity threshold percentage (0-100)
    #[serde(default = "default_threshold")]
    #[schemars(range(min = 0, max = 100))]
    pub threshold: i64,

    /// Exact matches only
    #[serde(default)]
    pub exact: bool,

    #[serde(default = "default_max_results_100")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: i64,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,
}

/// `find_cycles` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FindCyclesParams {
    #[serde(default = "default_path")]
    pub path: String,

    /// Type of cycles to detect
    #[serde(default)]
    pub cycle_type: CycleTypeParam,

    /// Minimum cycle depth to report
    #[serde(default = "default_min_depth")]
    #[schemars(range(min = 2))]
    pub min_depth: i64,

    /// Maximum cycle depth to report (unbounded if not set)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<i64>,

    /// Include self-loops
    #[serde(default)]
    pub include_self_loops: bool,

    #[serde(default = "default_max_results_100")]
    #[schemars(range(min = 1, max = 500))]
    pub max_results: i64,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,
}

/// `find_unused` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FindUnusedParams {
    #[serde(default = "default_path")]
    pub path: String,

    /// Unused scope filter
    #[serde(default)]
    pub scope: UnusedScopeParam,

    /// Filter by languages
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub language: Vec<String>,

    /// Filter by symbol kinds
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symbol_kind: Vec<String>,

    #[serde(default = "default_max_results_100")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: i64,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,
}

// ============================================================================
// Default Value Functions
// ============================================================================

fn default_path() -> String {
    ".".to_string()
}

fn default_true() -> bool {
    true
}

fn default_max_results_20() -> i64 {
    20
}

fn default_max_results_200() -> i64 {
    200
}

fn default_max_results_500() -> i64 {
    500
}

fn default_max_results_1000() -> i64 {
    1000
}

fn default_max_total_symbols() -> i64 {
    2000
}

fn default_context_lines() -> i64 {
    3
}

fn default_merge_threshold() -> i64 {
    256
}

fn default_max_files() -> i64 {
    20
}

fn default_max_containers_per_file() -> i64 {
    50
}

fn default_max_symbols_per_container() -> i64 {
    100
}

fn default_file_target_tokens() -> i64 {
    2000
}

fn default_container_target_tokens() -> i64 {
    1500
}

fn default_symbol_target_tokens() -> i64 {
    500
}

fn default_context_cluster_target_tokens() -> i64 {
    768
}

fn default_page_size_20() -> i64 {
    20
}

fn default_page_size_50() -> i64 {
    50
}

fn default_page_size_100() -> i64 {
    100
}

fn default_page_size_200() -> i64 {
    200
}

fn default_max_depth_1() -> i64 {
    1
}

fn default_max_depth_2() -> i64 {
    2
}

fn default_max_depth_3() -> i64 {
    3
}

fn default_max_hops() -> i64 {
    5
}

fn default_max_paths() -> i64 {
    5
}

fn default_max_nodes() -> i64 {
    50
}

fn default_similarity_threshold() -> f64 {
    0.7
}

fn default_min_confidence() -> f64 {
    0.5
}

fn default_threshold() -> i64 {
    80
}

fn default_max_results_100() -> i64 {
    100
}

fn default_min_depth() -> i64 {
    2
}

// ============================================================================
// New Graph-Based Tool Parameter Types
// ============================================================================

/// `is_node_in_cycle` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct IsNodeInCycleParams {
    /// Symbol name
    pub symbol: String,

    #[serde(default = "default_path")]
    pub path: String,

    /// Type of cycles to check for
    #[serde(default)]
    pub cycle_type: CycleTypeParam,

    /// Minimum cycle depth to consider (default: 2)
    #[serde(default = "default_cycle_min_depth")]
    #[schemars(range(min = 1, max = 100))]
    pub min_depth: usize,

    /// Maximum cycle depth to consider (optional, unbounded by default)
    #[schemars(range(min = 1, max = 100))]
    pub max_depth: Option<usize>,

    /// Include self-loops
    #[serde(default)]
    pub include_self_loops: bool,
}

fn default_cycle_min_depth() -> usize {
    2
}

impl IsNodeInCycleParams {
    /// Validate parameters: non-empty symbol and valid depth ranges.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.symbol.trim().is_empty() {
            return Err(RpcError::validation_with_data(
                "symbol cannot be empty",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "non_empty",
                    "field": "symbol"
                }),
            ));
        }

        // min_depth must be at least 1 (0 doesn't make sense for cycle detection)
        if self.min_depth < 1 {
            return Err(RpcError::validation_with_data(
                "min_depth must be at least 1",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "range",
                    "field": "min_depth",
                    "min": 1,
                    "actual": self.min_depth
                }),
            ));
        }

        // If max_depth is provided, it must be >= min_depth
        if let Some(max) = self.max_depth {
            if max < 1 {
                return Err(RpcError::validation_with_data(
                    "max_depth must be at least 1",
                    serde_json::json!({
                        "kind": "validation",
                        "constraint": "range",
                        "field": "max_depth",
                        "min": 1,
                        "actual": max
                    }),
                ));
            }
            if max < self.min_depth {
                return Err(RpcError::validation_with_data(
                    "max_depth must be >= min_depth",
                    serde_json::json!({
                        "kind": "validation",
                        "constraint": "ordering",
                        "field": "max_depth",
                        "min_depth": self.min_depth,
                        "max_depth": max
                    }),
                ));
            }
        }

        Ok(())
    }
}

/// `pattern_search` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct PatternSearchParams {
    /// Substring pattern
    pub pattern: String,

    #[serde(default = "default_path")]
    pub path: String,

    #[serde(default = "default_max_results_100")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: i64,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,
}

impl PatternSearchParams {
    /// Validate non-empty pattern.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.pattern.trim().is_empty() {
            return Err(RpcError::validation_with_data(
                "pattern cannot be empty",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "non_empty",
                    "field": "pattern"
                }),
            ));
        }
        Ok(())
    }
}

/// `direct_callers` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DirectCallersParams {
    /// Symbol name
    pub symbol: String,

    #[serde(default = "default_path")]
    pub path: String,

    #[serde(default = "default_max_results_100")]
    #[schemars(range(min = 1, max = 500))]
    pub max_results: i64,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,
}

impl DirectCallersParams {
    /// Validate non-empty symbol.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.symbol.trim().is_empty() {
            return Err(RpcError::validation_with_data(
                "symbol cannot be empty",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "non_empty",
                    "field": "symbol"
                }),
            ));
        }
        Ok(())
    }
}

/// `direct_callees` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct DirectCalleesParams {
    /// Symbol name
    pub symbol: String,

    #[serde(default = "default_path")]
    pub path: String,

    #[serde(default = "default_max_results_100")]
    #[schemars(range(min = 1, max = 500))]
    pub max_results: i64,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,
}

impl DirectCalleesParams {
    /// Validate non-empty symbol.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.symbol.trim().is_empty() {
            return Err(RpcError::validation_with_data(
                "symbol cannot be empty",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "non_empty",
                    "field": "symbol"
                }),
            ));
        }
        Ok(())
    }
}

// ============================================================================
// Introspection Tool Parameters
// ============================================================================

/// `list_files` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListFilesParams {
    #[serde(default = "default_path")]
    pub path: String,

    /// Optional language filter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    #[serde(default = "default_max_results_500")]
    #[schemars(range(min = 1, max = 10000))]
    pub max_results: i64,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,
}

/// `list_symbols` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ListSymbolsParams {
    #[serde(default = "default_path")]
    pub path: String,

    /// Optional kind filter (function, method, class, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    /// Optional language filter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    #[serde(default = "default_max_results_500")]
    #[schemars(range(min = 1, max = 10000))]
    pub max_results: i64,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,
}

/// `get_graph_stats` params.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct GetGraphStatsParams {
    #[serde(default = "default_path")]
    pub path: String,
}

// ============================================================================
// Navigation Tool Parameters
// ============================================================================

/// `get_definition` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetDefinitionParams {
    /// Symbol name to look up
    pub symbol: String,

    #[serde(default = "default_path")]
    pub path: String,
}

impl GetDefinitionParams {
    /// Validate non-empty symbol.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.symbol.trim().is_empty() {
            return Err(RpcError::validation_with_data(
                "symbol cannot be empty",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "non_empty",
                    "field": "symbol"
                }),
            ));
        }
        Ok(())
    }
}

/// `get_references` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetReferencesParams {
    /// Symbol name to find references for
    pub symbol: String,

    #[serde(default = "default_path")]
    pub path: String,

    /// Whether to include the declaration
    #[serde(default = "default_true")]
    pub include_declaration: bool,

    #[serde(default = "default_max_results_100")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: i64,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,
}

impl GetReferencesParams {
    /// Validate non-empty symbol.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.symbol.trim().is_empty() {
            return Err(RpcError::validation_with_data(
                "symbol cannot be empty",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "non_empty",
                    "field": "symbol"
                }),
            ));
        }
        Ok(())
    }
}

/// `get_hover_info` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetHoverInfoParams {
    /// Symbol name to get info for
    pub symbol: String,

    #[serde(default = "default_path")]
    pub path: String,
}

impl GetHoverInfoParams {
    /// Validate non-empty symbol.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.symbol.trim().is_empty() {
            return Err(RpcError::validation_with_data(
                "symbol cannot be empty",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "non_empty",
                    "field": "symbol"
                }),
            ));
        }
        Ok(())
    }
}

/// `get_document_symbols` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetDocumentSymbolsParams {
    /// File path to get symbols from
    pub file_path: String,

    #[serde(default = "default_path")]
    pub path: String,
}

impl GetDocumentSymbolsParams {
    /// Validate non-empty `file_path`.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.file_path.trim().is_empty() {
            return Err(RpcError::validation_with_data(
                "file_path cannot be empty",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "non_empty",
                    "field": "file_path"
                }),
            ));
        }
        Ok(())
    }
}

/// `get_workspace_symbols` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct GetWorkspaceSymbolsParams {
    /// Query to search for
    pub query: String,

    #[serde(default = "default_path")]
    pub path: String,

    #[serde(default = "default_max_results_100")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: i64,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,
}

impl GetWorkspaceSymbolsParams {
    /// Validate non-empty query.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.query.trim().is_empty() {
            return Err(RpcError::validation_with_data(
                "query cannot be empty",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "non_empty",
                    "field": "query"
                }),
            ));
        }
        Ok(())
    }
}

// ============================================================================
// Insights Tool Parameters
// ============================================================================

/// `get_insights` params.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct GetInsightsParams {
    #[serde(default = "default_path")]
    pub path: String,
}

/// `complexity_metrics` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ComplexityMetricsParams {
    #[serde(default = "default_path")]
    pub path: String,

    /// Optional target file or symbol to filter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,

    /// Minimum complexity to report (default: 1)
    #[serde(default = "default_min_complexity")]
    #[schemars(range(min = 1, max = 100))]
    pub min_complexity: u32,

    /// Sort by complexity descending (default: true)
    #[serde(default = "default_true")]
    pub sort_by_complexity: bool,

    #[serde(default = "default_max_results_100")]
    #[schemars(range(min = 1, max = 1000))]
    pub max_results: i64,
}

/// `rebuild_index` params.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct RebuildIndexParams {
    #[serde(default = "default_path")]
    pub path: String,

    /// Force rebuild
    #[serde(default = "default_true")]
    pub force: bool,
}

fn default_min_complexity() -> u32 {
    1
}

/// Call direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CallHierarchyDirection {
    /// Find callers (incoming calls)
    Incoming,
    /// Find callees (outgoing calls)
    Outgoing,
}

/// `call_hierarchy` params.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CallHierarchyParams {
    /// Symbol name
    pub symbol: String,

    /// File path to disambiguate
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,

    /// Incoming or outgoing
    pub direction: CallHierarchyDirection,

    #[serde(default = "default_path")]
    pub path: String,

    #[serde(default = "default_max_depth_1")]
    #[schemars(range(min = 1, max = 5))]
    pub max_depth: i64,

    #[serde(default = "default_max_results_200")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_results: i64,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,

    #[serde(default = "default_page_size_50")]
    #[schemars(range(min = 1, max = 500))]
    pub page_size: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::schema_for;

    #[test]
    fn test_semantic_search_params_deser() {
        let json = r#"{"query": "test"}"#;
        let params: SemanticSearchParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.query, "test");
        assert_eq!(params.path, ".");
        assert_eq!(params.max_results, 200);
    }

    #[test]
    fn test_show_dependencies_xor_valid() {
        let params = ShowDependenciesParams {
            file_path: Some("test.rs".to_string()),
            symbol_name: None,
            path: ".".to_string(),
            max_depth: 2,
            max_results: 500,
            page_token: None,
            page_size: 100,
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn test_show_dependencies_xor_invalid() {
        let params = ShowDependenciesParams {
            file_path: None,
            symbol_name: None,
            path: ".".to_string(),
            max_depth: 2,
            max_results: 500,
            page_token: None,
            page_size: 100,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn test_subgraph_non_empty_valid() {
        let params = SubgraphParams {
            symbols: vec!["foo".to_string()],
            path: ".".to_string(),
            max_depth: 2,
            max_nodes: 50,
            include_callers: true,
            include_callees: true,
            include_imports: false,
            cross_language: true,
            page_token: None,
            page_size: 50,
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn test_subgraph_non_empty_invalid() {
        let params = SubgraphParams {
            symbols: vec![],
            path: ".".to_string(),
            max_depth: 2,
            max_nodes: 50,
            include_callers: true,
            include_callees: true,
            include_imports: false,
            cross_language: true,
            page_token: None,
            page_size: 50,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn test_schema_generation() {
        // Verify schemars can generate schemas for all types
        let _ = schema_for!(SemanticSearchParams);
        let _ = schema_for!(HierarchicalSearchParams);
        let _ = schema_for!(RelationQueryParams);
        let _ = schema_for!(ExplainCodeParams);
        let _ = schema_for!(SearchSimilarParams);
        let _ = schema_for!(ShowDependenciesParams);
        let _ = schema_for!(GetIndexStatusParams);
        let _ = schema_for!(ExportGraphParams);
        let _ = schema_for!(CrossLanguageEdgesParams);
        let _ = schema_for!(TracePathParams);
        let _ = schema_for!(SubgraphParams);
        let _ = schema_for!(DependencyImpactParams);
        let _ = schema_for!(SemanticDiffParams);
        let _ = schema_for!(SqryAskParams);
        // New graph-based tool params
        let _ = schema_for!(FindCyclesParams);
        let _ = schema_for!(FindDuplicatesParams);
        let _ = schema_for!(FindUnusedParams);
        let _ = schema_for!(IsNodeInCycleParams);
        let _ = schema_for!(PatternSearchParams);
        let _ = schema_for!(DirectCallersParams);
        let _ = schema_for!(DirectCalleesParams);
    }

    // ========================================================================
    // IsNodeInCycleParams validation tests
    // ========================================================================

    #[test]
    fn test_is_node_in_cycle_valid() {
        let params = IsNodeInCycleParams {
            symbol: "my_function".to_string(),
            path: ".".to_string(),
            cycle_type: CycleTypeParam::Calls,
            min_depth: 2,
            max_depth: None,
            include_self_loops: false,
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn test_is_node_in_cycle_empty_symbol_invalid() {
        let params = IsNodeInCycleParams {
            symbol: "".to_string(),
            path: ".".to_string(),
            cycle_type: CycleTypeParam::Calls,
            min_depth: 2,
            max_depth: None,
            include_self_loops: false,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn test_is_node_in_cycle_whitespace_symbol_invalid() {
        let params = IsNodeInCycleParams {
            symbol: "   ".to_string(),
            path: ".".to_string(),
            cycle_type: CycleTypeParam::Imports,
            min_depth: 2,
            max_depth: None,
            include_self_loops: false,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn test_is_node_in_cycle_min_depth_zero_invalid() {
        let params = IsNodeInCycleParams {
            symbol: "my_function".to_string(),
            path: ".".to_string(),
            cycle_type: CycleTypeParam::Calls,
            min_depth: 0,
            max_depth: None,
            include_self_loops: false,
        };
        let err = params.validate().unwrap_err();
        assert!(err.message.contains("min_depth must be at least 1"));
    }

    #[test]
    fn test_is_node_in_cycle_max_depth_zero_invalid() {
        let params = IsNodeInCycleParams {
            symbol: "my_function".to_string(),
            path: ".".to_string(),
            cycle_type: CycleTypeParam::Calls,
            min_depth: 1,
            max_depth: Some(0),
            include_self_loops: false,
        };
        let err = params.validate().unwrap_err();
        assert!(err.message.contains("max_depth must be at least 1"));
    }

    #[test]
    fn test_is_node_in_cycle_max_depth_less_than_min_depth_invalid() {
        let params = IsNodeInCycleParams {
            symbol: "my_function".to_string(),
            path: ".".to_string(),
            cycle_type: CycleTypeParam::Calls,
            min_depth: 5,
            max_depth: Some(3),
            include_self_loops: false,
        };
        let err = params.validate().unwrap_err();
        assert!(err.message.contains("max_depth must be >= min_depth"));
    }

    #[test]
    fn test_is_node_in_cycle_valid_depth_ranges() {
        // min_depth = 1 is valid
        let params = IsNodeInCycleParams {
            symbol: "my_function".to_string(),
            path: ".".to_string(),
            cycle_type: CycleTypeParam::Calls,
            min_depth: 1,
            max_depth: None,
            include_self_loops: true,
        };
        assert!(params.validate().is_ok());

        // max_depth = min_depth is valid
        let params = IsNodeInCycleParams {
            symbol: "my_function".to_string(),
            path: ".".to_string(),
            cycle_type: CycleTypeParam::Calls,
            min_depth: 3,
            max_depth: Some(3),
            include_self_loops: false,
        };
        assert!(params.validate().is_ok());

        // max_depth > min_depth is valid
        let params = IsNodeInCycleParams {
            symbol: "my_function".to_string(),
            path: ".".to_string(),
            cycle_type: CycleTypeParam::Calls,
            min_depth: 2,
            max_depth: Some(10),
            include_self_loops: false,
        };
        assert!(params.validate().is_ok());
    }

    // ========================================================================
    // PatternSearchParams validation tests
    // ========================================================================

    #[test]
    fn test_pattern_search_valid() {
        let params = PatternSearchParams {
            pattern: "test".to_string(),
            path: ".".to_string(),
            max_results: 100,
            pagination: None,
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn test_pattern_search_empty_pattern_invalid() {
        let params = PatternSearchParams {
            pattern: "".to_string(),
            path: ".".to_string(),
            max_results: 100,
            pagination: None,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn test_pattern_search_whitespace_pattern_invalid() {
        let params = PatternSearchParams {
            pattern: "   ".to_string(),
            path: ".".to_string(),
            max_results: 100,
            pagination: None,
        };
        assert!(params.validate().is_err());
    }

    // ========================================================================
    // DirectCallersParams validation tests
    // ========================================================================

    #[test]
    fn test_direct_callers_valid() {
        let params = DirectCallersParams {
            symbol: "my_function".to_string(),
            path: ".".to_string(),
            max_results: 100,
            pagination: None,
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn test_direct_callers_empty_symbol_invalid() {
        let params = DirectCallersParams {
            symbol: "".to_string(),
            path: ".".to_string(),
            max_results: 100,
            pagination: None,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn test_direct_callers_whitespace_symbol_invalid() {
        let params = DirectCallersParams {
            symbol: "   ".to_string(),
            path: ".".to_string(),
            max_results: 100,
            pagination: None,
        };
        assert!(params.validate().is_err());
    }

    // ========================================================================
    // DirectCalleesParams validation tests
    // ========================================================================

    #[test]
    fn test_direct_callees_valid() {
        let params = DirectCalleesParams {
            symbol: "my_function".to_string(),
            path: ".".to_string(),
            max_results: 100,
            pagination: None,
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn test_direct_callees_empty_symbol_invalid() {
        let params = DirectCalleesParams {
            symbol: "".to_string(),
            path: ".".to_string(),
            max_results: 100,
            pagination: None,
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn test_direct_callees_whitespace_symbol_invalid() {
        let params = DirectCalleesParams {
            symbol: "   ".to_string(),
            path: ".".to_string(),
            max_results: 100,
            pagination: None,
        };
        assert!(params.validate().is_err());
    }
}
