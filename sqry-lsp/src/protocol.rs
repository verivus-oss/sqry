use serde::{Deserialize, Serialize};
use sqry_core::json_response::IndexStatus;
use tower_lsp::lsp_types::Location;

// Re-export canonical schema types for protocol compatibility
pub use sqry_core::schema::RelationKind;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SqrySearchParams {
    pub query: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SqrySearchItem {
    pub name: String,
    pub kind: String,
    pub qualified_name: String,
    pub language: String,
    pub location: Location,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SqrySearchResult {
    pub results: Vec<SqrySearchItem>,
    pub total: usize,
    #[serde(rename = "truncated")]
    pub is_truncated: bool,
    pub used_index: bool,
}

/// Sort order for cross-language results
#[derive(Debug, Serialize, Deserialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SortOrder {
    /// Sort alphabetically by symbol/file name
    #[default]
    Alphabetical,
    /// Sort by frequency (most common language pairs first)
    ByFrequency,
    /// Sort by relevance score (if available)
    ByRelevance,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SqryRelationParams {
    pub relation: RelationKind,
    pub target: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SqryRelationResult {
    pub relation: RelationKind,
    pub results: Vec<SqrySearchItem>,
    pub total: usize,
    #[serde(rename = "truncated")]
    pub is_truncated: bool,
    pub used_index: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SqryIndexStatusParams {
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct SqryIndexStatusResult {
    pub status: IndexStatus,
}

// ===== List Files Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SqryListFilesParams {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Clone)]
pub struct SqryListFilesResult {
    pub files: Vec<String>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub has_more: bool,
}

// ===== List Symbols Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SqryListSymbolsParams {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Filter by symbol kind (e.g., "function", "class", "method")
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct SqryListSymbolsResult {
    pub symbols: Vec<SqrySearchItem>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub has_more: bool,
}

// ===== List Files by Language Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SqryListFilesByLanguageParams {
    /// The language to filter files by (e.g., "rust", "typescript")
    pub language: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Clone)]
pub struct SqryListFilesByLanguageResult {
    pub language: String,
    pub files: Vec<String>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub has_more: bool,
}

// ===== Cross-Language Relations Endpoint =====

/// A cross-language relation item showing a connection between different languages
#[derive(Debug, Serialize, Clone)]
pub struct CrossLanguageRelation {
    /// Type of relation (e.g., "import", "call", "export")
    pub relation_type: String,
    /// Source symbol name
    pub from_symbol: String,
    /// Source language
    pub from_language: String,
    /// Source file path
    pub from_file: String,
    /// Target symbol name
    pub to_symbol: String,
    /// Target language (inferred from file extension)
    pub to_language: String,
    /// Target file path (if known)
    pub to_file: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SqryListCrossLanguageRelationsParams {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Sort order for results
    #[serde(default)]
    pub sort_order: SortOrder,
    /// Filter by source language (e.g., "rust", "go")
    #[serde(default)]
    pub source_language: Option<String>,
    /// Filter by target language (e.g., "javascript", "python")
    #[serde(default)]
    pub target_language: Option<String>,
}

/// Overflow information when results are truncated
///
/// # Overflow Conditions
///
/// This struct is populated when the per-language-pair limit is exceeded:
///
/// **Per-Language-Pair Limit**: A language pair (e.g., Python→JavaScript) has
/// exceeded the maximum stored relations (10,000 per pair). Additional relations
/// exist in the codebase but were not indexed.
///
/// # Client Handling
///
/// IDE clients should check for overflow and display appropriate UI:
/// - Show a warning indicator when `total_dropped > 0`
/// - Display message like "Results truncated for \[pairs\]"
/// - Suggest filtering by specific language pair to see more results
#[derive(Debug, Serialize, Clone, Default)]
pub struct OverflowInfo {
    /// Total count of relations that were dropped/truncated.
    /// These relations exist in the codebase but were not returned.
    pub total_dropped: usize,
    /// Language pairs that hit their storage limit during indexing.
    /// Each tuple is `(from_language, to_language)` in normalized form.
    pub truncated_pairs: Vec<(String, String)>,
}

#[derive(Debug, Serialize, Clone)]
pub struct SqryListCrossLanguageRelationsResult {
    pub relations: Vec<CrossLanguageRelation>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub has_more: bool,
    /// Overflow information if any results were truncated
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overflow: Option<OverflowInfo>,
}

// ===== Duplicate Groups Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SqryListDuplicateGroupsParams {
    #[serde(default)]
    pub path: Option<String>,
    /// Type of duplicate to detect. Currently only "body" is supported.
    #[serde(default = "default_duplicate_type")]
    pub duplicate_type: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn default_duplicate_type() -> String {
    "body".to_string()
}

/// A group of duplicate symbols sharing the same hash
#[derive(Debug, Serialize, Clone)]
pub struct SqryDuplicateGroup {
    /// Unique identifier for this duplicate group (hash)
    pub group_id: String,
    /// Number of symbols in this group
    pub count: usize,
    /// Representative name for the group (first symbol's name)
    pub representative_name: String,
    /// All symbols in the duplicate group
    pub symbols: Vec<SqrySearchItem>,
}

#[derive(Debug, Serialize, Clone)]
pub struct SqryListDuplicateGroupsResult {
    pub groups: Vec<SqryDuplicateGroup>,
    pub total_groups: usize,
    pub total_symbols: usize,
    /// Whether results were truncated due to limit
    pub truncated: bool,
}

// ===== Circular Dependencies Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SqryListCircularDependenciesParams {
    #[serde(default)]
    pub path: Option<String>,
    /// Type of circular dependency: "calls", "imports", or "modules"
    #[serde(default = "default_circular_type")]
    pub circular_type: String,
    #[serde(default)]
    pub limit: Option<usize>,
    /// Include self-loops (A -> A) in results
    #[serde(default)]
    pub should_include_self_loops: bool,
}

fn default_circular_type() -> String {
    "calls".to_string()
}

/// Location data for a cycle member
#[derive(Debug, Serialize, Clone)]
pub struct CycleMemberLocation {
    /// Symbol name (same as in members array)
    pub name: String,
    /// File path (URI)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// 0-based line offset within the source file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// 0-based UTF-16 column offset within the source line, per LSP spec.
    /// `None` if the source text cannot be loaded (file deleted, unreadable,
    /// or offset invalid).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// A cycle in the dependency graph
#[derive(Debug, Serialize, Clone)]
pub struct SqryCycle {
    /// Unique identifier for this cycle (hash of members)
    pub cycle_id: String,
    /// Number of nodes in the cycle
    pub depth: usize,
    /// Nodes in the cycle (symbol names or file paths)
    pub members: Vec<String>,
    /// Type of cycle ("calls", "imports", "modules")
    pub cycle_type: String,
    /// Location data for each member (when available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_locations: Option<Vec<CycleMemberLocation>>,
}

#[derive(Debug, Serialize, Clone)]
pub struct SqryListCircularDependenciesResult {
    pub cycles: Vec<SqryCycle>,
    /// Total number of cycles available before truncation.
    /// When `truncated` is true, this value equals `limit + 1` as a lower-bound
    /// sentinel because the handler did not enumerate beyond `limit + 1` for
    /// performance. When `truncated` is false, this is the exact cycle count.
    pub total_cycles: usize,
    /// Whether results were truncated due to limit
    pub truncated: bool,
}

// ===== Unused Symbols Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SqryListUnusedSymbolsParams {
    #[serde(default)]
    pub path: Option<String>,
    /// Scope of unused analysis: "public", "private", "function", "struct", or "all"
    #[serde(default = "default_unused_scope")]
    pub scope: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

fn default_unused_scope() -> String {
    "all".to_string()
}

#[derive(Debug, Serialize, Clone)]
pub struct SqryListUnusedSymbolsResult {
    pub symbols: Vec<SqrySearchItem>,
    /// Total matching unused symbols before limit applied.
    /// When `truncated` is true, this value equals `limit + 1` as a lower-bound
    /// sentinel because the handler did not enumerate further for performance.
    /// When `truncated` is false, this is the exact count of unused symbols.
    pub total: usize,
    /// Whether results were truncated due to limit
    pub truncated: bool,
    /// Scope that was applied
    pub scope: String,
}

// ===== Hierarchical Search Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqryHierarchicalSearchParams {
    /// The semantic query to search for
    pub query: String,
    #[serde(default)]
    pub path: Option<String>,
    /// Maximum number of files to return
    #[serde(default)]
    pub max_files: Option<usize>,
    /// Maximum symbols per file
    #[serde(default)]
    pub max_symbols_per_file: Option<usize>,
    /// Maximum total symbols across all files
    #[serde(default)]
    pub max_total_symbols: Option<usize>,
    /// Include container context (class/struct code)
    #[serde(default)]
    pub include_container_context: Option<bool>,
    /// Filter by programming languages
    #[serde(default)]
    pub languages: Option<Vec<String>>,
    /// Filter by symbol kinds (function, class, method, etc.)
    #[serde(default)]
    pub symbol_kinds: Option<Vec<String>>,
    /// Minimum relevance score (0.0-1.0)
    #[serde(default)]
    pub score_min: Option<f64>,
}

/// A file containing matching symbols
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryHierarchicalFileGroup {
    /// Workspace-relative file path
    pub path: String,
    /// Programming language
    pub language: String,
    /// Total symbols in this file
    pub symbol_count: usize,
    /// Maximum relevance score in this file
    pub max_score: f64,
    /// Containers (classes, structs, modules)
    pub containers: Vec<SqryHierarchicalContainer>,
    /// Top-level symbols not in any container
    pub top_level_symbols: Vec<SqryHierarchicalSymbol>,
}

/// A container (class, struct, module) containing symbols
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryHierarchicalContainer {
    /// Container name
    pub name: String,
    /// Qualified name
    pub qualified_name: String,
    /// Container kind (class, struct, impl, module)
    pub kind: String,
    /// Symbols in this container
    pub symbols: Vec<SqryHierarchicalSymbol>,
    /// Line range
    pub start_line: u32,
    pub end_line: u32,
}

/// A symbol in the hierarchical search results
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryHierarchicalSymbol {
    /// Symbol name
    pub name: String,
    /// Qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// Relevance score
    pub score: f64,
    /// Location in the file
    pub location: Location,
    /// Signature if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryHierarchicalSearchResult {
    /// Query that was executed
    pub query: String,
    /// Files containing matches (grouped hierarchically)
    pub files: Vec<SqryHierarchicalFileGroup>,
    /// Total symbols returned
    pub total_symbols: usize,
    /// Total files with matches
    pub total_files: usize,
    /// Whether results were truncated
    pub truncated: bool,
}

// ===== Ask (Natural Language Translation) Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqryAskParams {
    /// Natural language query to translate
    pub query: String,
    #[serde(default)]
    pub path: Option<String>,
}

/// Disambiguation option when query is ambiguous
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryAskDisambiguationOption {
    /// Translated command
    pub command: String,
    /// Detected intent
    pub intent: String,
    /// Human-readable description
    pub description: String,
    /// Confidence score
    pub confidence: f32,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryAskResult {
    /// Response type: "execute", "confirm", "disambiguate", or "reject"
    pub response_type: String,
    /// Translated command (for execute/confirm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Confidence score (for execute/confirm)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Detected intent (for execute)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// Confirmation/disambiguation prompt
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Rejection reason
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Suggested alternatives (for reject)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
    /// Disambiguation options (for disambiguate)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<SqryAskDisambiguationOption>,
}

// ===== Direct Callers/Callees Endpoints =====

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryDirectCallersParams {
    /// Symbol name to find callers for
    pub symbol: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryDirectCallersResult {
    /// Symbol that was queried
    pub symbol: String,
    /// Callers of this symbol
    pub callers: Vec<SqrySearchItem>,
    /// Total callers found
    pub total: usize,
    /// Whether results were truncated
    pub truncated: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryDirectCalleesParams {
    /// Symbol name to find callees for
    pub symbol: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryDirectCalleesResult {
    /// Symbol that was queried
    pub symbol: String,
    /// Callees of this symbol
    pub callees: Vec<SqrySearchItem>,
    /// Total callees found
    pub total: usize,
    /// Whether results were truncated
    pub truncated: bool,
}

// ===== Batch Caller/Callee Count Endpoint =====

/// Reference to a symbol for batch counting.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SymbolRef {
    /// Symbol name (simple or qualified)
    pub name: String,
    /// Optional file path to scope the lookup
    #[serde(default)]
    pub file: Option<String>,
    /// Optional line number to disambiguate
    #[serde(default)]
    pub line: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct SqryBatchCallerCalleeCountParams {
    /// Symbols to count callers/callees for
    pub symbols: Vec<SymbolRef>,
    /// Optional workspace path
    #[serde(default)]
    pub path: Option<String>,
}

/// Caller/callee counts for a single symbol.
#[derive(Debug, Serialize, Clone)]
pub struct SymbolCount {
    /// Symbol name that was queried
    pub name: String,
    /// Number of direct callers
    pub callers: usize,
    /// Number of direct callees
    pub callees: usize,
}

#[derive(Debug, Serialize, Clone)]
pub struct SqryBatchCallerCalleeCountResult {
    /// Counts for each requested symbol
    pub counts: Vec<SymbolCount>,
}

// ===== Graph Stats Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqryGraphStatsParams {
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryGraphStatsResult {
    /// Total nodes in the graph
    pub total_nodes: u64,
    /// Total edges in the graph
    pub total_edges: u64,
    /// Total files indexed
    pub total_files: u64,
    /// Nodes by kind
    pub nodes_by_kind: std::collections::HashMap<String, u64>,
    /// Files by language
    pub files_by_language: std::collections::HashMap<String, u64>,
    /// Graph epoch (version)
    pub graph_epoch: u64,
}

// ===== Dependency Impact Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryDependencyImpactParams {
    /// Symbol to analyze
    pub symbol: String,
    #[serde(default)]
    pub path: Option<String>,
    /// Maximum depth of transitive dependencies
    #[serde(default)]
    pub max_depth: Option<usize>,
    /// Include indirect dependencies
    #[serde(default)]
    pub include_indirect: Option<bool>,
}

/// A symbol affected by changes to the queried symbol
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryAffectedSymbol {
    /// Symbol name
    pub name: String,
    /// Qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// File path
    pub file_path: String,
    /// Line number
    pub line: u32,
    /// Whether this is a direct or indirect dependency
    pub is_direct: bool,
    /// Depth from the queried symbol
    pub depth: u32,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryDependencyImpactResult {
    /// Symbol that was analyzed
    pub symbol: String,
    /// Symbols that would be affected
    pub affected: Vec<SqryAffectedSymbol>,
    /// Total affected count
    pub total: usize,
    /// Files affected
    pub affected_files: Vec<String>,
    /// Whether results were truncated
    pub truncated: bool,
}

// ===== Explain Symbol Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryExplainSymbolParams {
    /// File path containing the symbol
    pub file_path: String,
    /// Symbol name
    pub symbol_name: String,
    #[serde(default)]
    pub path: Option<String>,
    /// Include context (callers, callees)
    #[serde(default)]
    pub include_context: Option<bool>,
    /// Include relations
    #[serde(default)]
    pub include_relations: Option<bool>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryExplainSymbolResult {
    /// Symbol name
    pub name: String,
    /// Qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// File path
    pub file_path: String,
    /// Line range
    pub start_line: u32,
    pub end_line: u32,
    /// Language
    pub language: String,
    /// Signature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Documentation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    /// Callers if `include_context` is true
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callers: Vec<SqrySearchItem>,
    /// Callees if `include_context` is true
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callees: Vec<SqrySearchItem>,
}

// ===== Graph Export Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqryGraphExportParams {
    #[serde(default)]
    pub path: Option<String>,
    /// File path to scope the export
    #[serde(default)]
    pub file_path: Option<String>,
    /// Symbol name to center the export on
    #[serde(default)]
    pub symbol_name: Option<String>,
    /// Output format: "json", "dot", "d2", "mermaid"
    #[serde(default = "default_graph_format")]
    pub format: String,
    /// Maximum traversal depth (default: 2)
    #[serde(default)]
    pub max_depth: Option<usize>,
    /// Maximum results (default: 1000)
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Include call edges (default: true)
    #[serde(default)]
    pub include_calls: Option<bool>,
    /// Include import edges (default: false)
    #[serde(default)]
    pub include_imports: Option<bool>,
    /// Include detailed labels (default: false)
    #[serde(default)]
    pub verbose: Option<bool>,
}

fn default_graph_format() -> String {
    "json".to_string()
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryGraphExportResult {
    /// Nodes in the exported graph
    pub nodes: Vec<SqryGraphNode>,
    /// Edges in the exported graph
    pub edges: Vec<SqryGraphEdge>,
    /// Total nodes
    pub total_nodes: usize,
    /// Total edges
    pub total_edges: usize,
    /// Rendered graph (if format is not json)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendered: Option<String>,
    /// Whether results were truncated
    pub truncated: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryGraphNode {
    /// Symbol name
    pub name: String,
    /// Qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// Language
    pub language: String,
    /// File path
    pub file_path: String,
    /// Start line
    pub start_line: u32,
    /// End line
    pub end_line: u32,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryGraphEdge {
    /// Source symbol qualified name
    pub from: String,
    /// Target symbol qualified name
    pub to: String,
    /// Edge type (call, import, etc.)
    pub edge_type: String,
    /// Traversal depth
    pub depth: u32,
}

// ===== Trace Path Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqryTracePathParams {
    /// Start symbol (name or qualified name)
    pub from_symbol: String,
    /// Target symbol (name or qualified name)
    pub to_symbol: String,
    #[serde(default)]
    pub path: Option<String>,
    /// Maximum path length (default: 5)
    #[serde(default)]
    pub max_hops: Option<usize>,
    /// Maximum paths to return (default: 5)
    #[serde(default)]
    pub max_paths: Option<usize>,
    /// Minimum edge confidence (default: 0.5)
    #[serde(default)]
    pub min_confidence: Option<f64>,
    /// Allow cross-language paths (default: true)
    #[serde(default)]
    pub cross_language: Option<bool>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryTracePathResult {
    /// Symbol that was searched from
    pub from_symbol: String,
    /// Symbol that was searched to
    pub to_symbol: String,
    /// Found paths
    pub paths: Vec<SqryCallPath>,
    /// Total paths found
    pub total: usize,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryCallPath {
    /// Steps in the path
    pub steps: Vec<SqryPathStep>,
    /// Number of edges (`steps.len()` - 1)
    pub length: u32,
    /// Relevance score (higher is better)
    pub score: f64,
    /// Whether path crosses language boundaries
    pub cross_language: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryPathStep {
    /// Symbol at this step
    pub symbol: SqrySearchItem,
    /// Edge type connecting to next step
    pub edge_type: String,
    /// Edge confidence (0.0-1.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

// ===== Pattern Search Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqryPatternSearchParams {
    /// Pattern to search for (supports wildcards: * for any, ? for single char)
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryPatternSearchResult {
    /// Pattern that was searched
    pub pattern: String,
    /// Matching symbols
    pub matches: Vec<SqrySearchItem>,
    /// Total matches found
    pub total: usize,
    /// Whether results were truncated
    pub truncated: bool,
}

// ===== Subgraph Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqrySubgraphParams {
    /// Seed symbols to extract context around
    pub symbols: Vec<String>,
    #[serde(default)]
    pub path: Option<String>,
    /// Maximum traversal depth (default: 2)
    #[serde(default)]
    pub max_depth: Option<usize>,
    /// Maximum nodes to include (default: 50)
    #[serde(default)]
    pub max_nodes: Option<usize>,
    /// Include callers (default: true)
    #[serde(default)]
    pub include_callers: Option<bool>,
    /// Include callees (default: true)
    #[serde(default)]
    pub include_callees: Option<bool>,
    /// Include imports (default: false)
    #[serde(default)]
    pub include_imports: Option<bool>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqrySubgraphResult {
    /// Nodes in the subgraph
    pub nodes: Vec<SqryGraphNode>,
    /// Edges in the subgraph
    pub edges: Vec<SqryGraphEdge>,
    /// Total nodes
    pub total_nodes: usize,
    /// Total edges
    pub total_edges: usize,
    /// Whether results were truncated
    pub truncated: bool,
}

// ===== Is Node In Cycle Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqryIsNodeInCycleParams {
    /// Symbol name to check
    pub symbol: String,
    #[serde(default)]
    pub path: Option<String>,
    /// Cycle type: "calls", "imports", or "all" (default: "calls")
    #[serde(default)]
    pub cycle_type: Option<String>,
    /// Show the cycle path if found (default: false)
    #[serde(default)]
    pub show_cycle: Option<bool>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryIsNodeInCycleResult {
    /// Symbol that was checked
    pub symbol: String,
    /// Whether the symbol is in a cycle
    pub in_cycle: bool,
    /// Cycle path if `show_cycle` was true and symbol is in cycle
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_path: Option<Vec<String>>,
    /// Cycle type that was checked
    pub cycle_type: String,
}

// ===== Similar Symbols Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqrySimilarSymbolsParams {
    /// Reference symbol file path
    pub file_path: String,
    /// Reference symbol name
    pub symbol_name: String,
    #[serde(default)]
    pub path: Option<String>,
    /// Maximum results (default: 20)
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Minimum similarity threshold (default: 0.7)
    #[serde(default)]
    pub similarity_threshold: Option<f64>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqrySimilarSymbolsResult {
    /// Reference symbol
    pub reference: SqrySearchItem,
    /// Similar symbols with scores
    pub similar: Vec<SqrySimilarSymbol>,
    /// Total similar symbols found
    pub total: usize,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqrySimilarSymbol {
    /// The similar symbol
    pub symbol: SqrySearchItem,
    /// Similarity score (0.0-1.0)
    pub similarity: f64,
}

// ===== Show Dependencies Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqryShowDependenciesParams {
    /// File path to analyze
    pub file_path: String,
    #[serde(default)]
    pub path: Option<String>,
    /// Symbol name (optional - if not provided, shows file dependencies)
    #[serde(default)]
    pub symbol_name: Option<String>,
    /// Maximum depth (default: 2)
    #[serde(default)]
    pub max_depth: Option<usize>,
    /// Maximum results (default: 500)
    #[serde(default)]
    pub max_results: Option<usize>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryShowDependenciesResult {
    /// Root of the dependency tree
    pub root: String,
    /// Dependencies
    pub dependencies: Vec<SqryDependency>,
    /// Total dependencies
    pub total: usize,
    /// Whether results were truncated
    pub truncated: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryDependency {
    /// Symbol name
    pub name: String,
    /// Qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// File path
    pub file_path: String,
    /// Depth in dependency tree
    pub depth: u32,
    /// Dependency type (import, call, etc.)
    pub dependency_type: String,
}

// ===== Complexity Metrics Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqryComplexityMetricsParams {
    #[serde(default)]
    pub path: Option<String>,
    /// Target file or symbol (optional - if not provided, analyzes all)
    #[serde(default)]
    pub target: Option<String>,
    /// Minimum complexity to report (default: 1)
    #[serde(default)]
    pub min_complexity: Option<u32>,
    /// Sort by complexity (default: true)
    #[serde(default)]
    pub sort_by_complexity: Option<bool>,
    /// Maximum results (default: 100)
    #[serde(default)]
    pub max_results: Option<usize>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryComplexityMetricsResult {
    /// Complexity metrics for symbols
    pub metrics: Vec<SqryComplexityMetric>,
    /// Total symbols analyzed
    pub total: usize,
    /// Average complexity
    pub average_complexity: f64,
    /// Maximum complexity found
    pub max_complexity: u32,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryComplexityMetric {
    /// Symbol name
    pub name: String,
    /// Qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// File path
    pub file_path: String,
    /// Cyclomatic complexity
    pub complexity: u32,
    /// Line count
    pub lines: u32,
}

// ===== Get Insights Endpoint =====

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqryGetInsightsParams {
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryGetInsightsResult {
    /// Total files
    pub total_files: usize,
    /// Total symbols
    pub total_symbols: usize,
    /// Total edges
    pub total_edges: usize,
    /// Languages with file counts
    pub languages: Vec<SqryLanguageStats>,
    /// Symbol kinds with counts
    pub symbol_kinds: Vec<SqryKindStats>,
    /// Health indicators
    pub health: SqryHealthIndicators,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryLanguageStats {
    pub language: String,
    pub files: usize,
    pub symbols: usize,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryKindStats {
    pub kind: String,
    pub count: usize,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryHealthIndicators {
    /// Number of cycles detected
    pub cycles: usize,
    /// Number of unused symbols
    pub unused_symbols: usize,
    /// Number of duplicate groups
    pub duplicate_groups: usize,
    /// Cross-language edges count
    pub cross_language_edges: usize,
}

// ===== Semantic Diff Endpoint =====

/// Git version reference for semantic diff
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryGitVersionRef {
    /// Git ref (commit, branch, tag)
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Optional file path to limit comparison
    #[serde(default)]
    pub file_path: Option<String>,
}

/// Filters for semantic diff results
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SqrySemanticDiffFilters {
    /// Filter by change types (added, removed, modified, renamed, `signature_changed`)
    #[serde(default)]
    pub change_types: Vec<String>,
    /// Filter by symbol kinds (function, class, method, etc.)
    #[serde(default)]
    pub symbol_kinds: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqrySemanticDiffParams {
    /// Base version to compare from
    pub base: SqryGitVersionRef,
    /// Target version to compare to
    pub target: SqryGitVersionRef,
    #[serde(default)]
    pub path: Option<String>,
    /// Include unchanged symbols (default: false)
    #[serde(default)]
    pub include_unchanged: Option<bool>,
    /// Maximum results (default: 500)
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Filters for results
    #[serde(default)]
    pub filters: Option<SqrySemanticDiffFilters>,
}

/// Location of a symbol in a specific version
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqrySymbolLocationRef {
    /// File path (relative to workspace)
    pub file_path: String,
    /// Start line (1-based)
    pub start_line: u32,
    /// End line (1-based)
    pub end_line: u32,
    /// Start column (0-based)
    pub start_column: u32,
    /// End column (0-based)
    pub end_column: u32,
}

/// A symbol change between two versions
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqrySymbolChange {
    /// Symbol name
    pub symbol_name: String,
    /// Qualified name (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qualified_name: Option<String>,
    /// Symbol kind (function, class, method, etc.)
    pub kind: String,
    /// Change type: added, removed, modified, renamed, `signature_changed`, unchanged
    pub change_type: String,
    /// Location in base version (if exists)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_location: Option<SqrySymbolLocationRef>,
    /// Location in target version (if exists)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_location: Option<SqrySymbolLocationRef>,
    /// Signature in base version (for `signature_changed`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_before: Option<String>,
    /// Signature in target version (for `signature_changed`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_after: Option<String>,
}

/// Summary of diff results
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqryDiffSummary {
    /// Symbols added in target
    pub added: usize,
    /// Symbols removed from base
    pub removed: usize,
    /// Symbols modified (body changed)
    pub modified: usize,
    /// Symbols renamed
    pub renamed: usize,
    /// Symbols with signature changes
    pub signature_changed: usize,
    /// Unchanged symbols (only if `include_unchanged`)
    pub unchanged: usize,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SqrySemanticDiffResult {
    /// Base ref that was compared
    pub base_ref: String,
    /// Target ref that was compared
    pub target_ref: String,
    /// Symbol changes
    pub changes: Vec<SqrySymbolChange>,
    /// Summary of changes
    pub summary: SqryDiffSummary,
    /// Total changes found
    pub total: u64,
    /// Whether results were truncated
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================================
    // SortOrder tests
    // ==========================================================================

    #[test]
    fn test_sort_order_default_is_alphabetical() {
        assert_eq!(SortOrder::default(), SortOrder::Alphabetical);
    }

    #[test]
    fn test_sort_order_clone() {
        let order = SortOrder::ByFrequency;
        let cloned = order;
        assert_eq!(cloned, SortOrder::ByFrequency);
    }

    #[test]
    fn test_sort_order_equality() {
        assert_eq!(SortOrder::Alphabetical, SortOrder::Alphabetical);
        assert_ne!(SortOrder::Alphabetical, SortOrder::ByFrequency);
        assert_ne!(SortOrder::ByFrequency, SortOrder::ByRelevance);
    }

    #[test]
    fn test_sort_order_serialize() {
        let json = serde_json::to_string(&SortOrder::Alphabetical).unwrap();
        assert_eq!(json, "\"alphabetical\"");

        let json = serde_json::to_string(&SortOrder::ByFrequency).unwrap();
        assert_eq!(json, "\"byFrequency\"");

        let json = serde_json::to_string(&SortOrder::ByRelevance).unwrap();
        assert_eq!(json, "\"byRelevance\"");
    }

    #[test]
    fn test_sort_order_deserialize() {
        let order: SortOrder = serde_json::from_str("\"alphabetical\"").unwrap();
        assert_eq!(order, SortOrder::Alphabetical);

        let order: SortOrder = serde_json::from_str("\"byFrequency\"").unwrap();
        assert_eq!(order, SortOrder::ByFrequency);

        let order: SortOrder = serde_json::from_str("\"byRelevance\"").unwrap();
        assert_eq!(order, SortOrder::ByRelevance);
    }

    // ==========================================================================
    // RelationKind tests
    // ==========================================================================

    #[test]
    fn test_relation_kind_serialize() {
        assert_eq!(
            serde_json::to_string(&RelationKind::Callers).unwrap(),
            "\"callers\""
        );
        assert_eq!(
            serde_json::to_string(&RelationKind::Callees).unwrap(),
            "\"callees\""
        );
        assert_eq!(
            serde_json::to_string(&RelationKind::Imports).unwrap(),
            "\"imports\""
        );
        assert_eq!(
            serde_json::to_string(&RelationKind::Exports).unwrap(),
            "\"exports\""
        );
        assert_eq!(
            serde_json::to_string(&RelationKind::Returns).unwrap(),
            "\"returns\""
        );
    }

    #[test]
    fn test_relation_kind_deserialize() {
        assert!(matches!(
            serde_json::from_str::<RelationKind>("\"callers\"").unwrap(),
            RelationKind::Callers
        ));
        assert!(matches!(
            serde_json::from_str::<RelationKind>("\"callees\"").unwrap(),
            RelationKind::Callees
        ));
        assert!(matches!(
            serde_json::from_str::<RelationKind>("\"imports\"").unwrap(),
            RelationKind::Imports
        ));
        assert!(matches!(
            serde_json::from_str::<RelationKind>("\"exports\"").unwrap(),
            RelationKind::Exports
        ));
        assert!(matches!(
            serde_json::from_str::<RelationKind>("\"returns\"").unwrap(),
            RelationKind::Returns
        ));
    }

    // ==========================================================================
    // Default function tests
    // ==========================================================================

    #[test]
    fn test_default_duplicate_type() {
        assert_eq!(default_duplicate_type(), "body");
    }

    #[test]
    fn test_default_circular_type() {
        assert_eq!(default_circular_type(), "calls");
    }

    #[test]
    fn test_default_unused_scope() {
        assert_eq!(default_unused_scope(), "all");
    }

    // ==========================================================================
    // OverflowInfo tests
    // ==========================================================================

    #[test]
    fn test_overflow_info_default() {
        let info = OverflowInfo::default();
        assert_eq!(info.total_dropped, 0);
        assert!(info.truncated_pairs.is_empty());
    }

    #[test]
    fn test_overflow_info_with_data() {
        let info = OverflowInfo {
            total_dropped: 100,
            truncated_pairs: vec![
                ("python".to_string(), "javascript".to_string()),
                ("rust".to_string(), "go".to_string()),
            ],
        };
        assert_eq!(info.total_dropped, 100);
        assert_eq!(info.truncated_pairs.len(), 2);
    }

    // ==========================================================================
    // Params default tests
    // ==========================================================================

    #[test]
    fn test_sqry_search_params_deserialize() {
        let json = r#"{"query": "test"}"#;
        let params: SqrySearchParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.query, "test");
        assert!(params.path.is_none());
        assert!(params.limit.is_none());
    }

    #[test]
    fn test_sqry_search_params_with_optionals() {
        let json = r#"{"query": "test", "path": "/src", "limit": 50}"#;
        let params: SqrySearchParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.query, "test");
        assert_eq!(params.path, Some("/src".to_string()));
        assert_eq!(params.limit, Some(50));
    }

    #[test]
    fn test_sqry_list_files_params_default() {
        let params = SqryListFilesParams::default();
        assert!(params.path.is_none());
        assert!(params.offset.is_none());
        assert!(params.limit.is_none());
    }

    #[test]
    fn test_sqry_list_symbols_params_default() {
        let params = SqryListSymbolsParams::default();
        assert!(params.path.is_none());
        assert!(params.offset.is_none());
        assert!(params.limit.is_none());
        assert!(params.kind.is_none());
    }

    #[test]
    fn test_sqry_list_cross_language_params_default() {
        let params = SqryListCrossLanguageRelationsParams::default();
        assert!(params.path.is_none());
        assert!(params.offset.is_none());
        assert!(params.limit.is_none());
        assert_eq!(params.sort_order, SortOrder::Alphabetical);
        assert!(params.source_language.is_none());
        assert!(params.target_language.is_none());
    }

    #[test]
    fn test_sqry_list_duplicate_groups_params_deserialize_default() {
        let json = r"{}";
        let params: SqryListDuplicateGroupsParams = serde_json::from_str(json).unwrap();
        assert!(params.path.is_none());
        assert_eq!(params.duplicate_type, "body");
        assert!(params.limit.is_none());
    }

    #[test]
    fn test_sqry_list_circular_deps_params_deserialize_default() {
        let json = r"{}";
        let params: SqryListCircularDependenciesParams = serde_json::from_str(json).unwrap();
        assert!(params.path.is_none());
        assert_eq!(params.circular_type, "calls");
        assert!(params.limit.is_none());
        assert!(!params.should_include_self_loops);
    }

    #[test]
    fn test_sqry_list_unused_symbols_params_deserialize_default() {
        let json = r"{}";
        let params: SqryListUnusedSymbolsParams = serde_json::from_str(json).unwrap();
        assert!(params.path.is_none());
        assert_eq!(params.scope, "all");
        assert!(params.limit.is_none());
    }
}
