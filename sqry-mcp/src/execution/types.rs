//! Data types for MCP tool execution responses.
//!
//! This module contains the response data structures used by the MCP tool handlers.

use serde::Serialize;
use serde_json::Value;

use super::graph_cache;

/// Result wrapper for tool executions
pub struct ToolExecution<T>
where
    T: Serialize,
{
    pub data: T,
    pub used_index: bool,
    pub used_graph: bool,
    pub graph_metadata: Option<GraphMetadata>,
    pub execution_ms: u64,
    pub next_page_token: Option<String>,
    pub total: Option<u64>,
    pub truncated: Option<bool>,
    pub candidates_scanned: Option<u64>,
    pub workspace_path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphMetadata {
    pub total_nodes: u64,
    pub total_edges: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    pub cross_language_edges: u64,
    pub graph_version: String,
    pub rebuild_epoch_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<GraphCacheMetadata>,
    /// Per-language confidence metadata from the graph manifest.
    /// Maps language name (e.g., "rust") to confidence level and limitations.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub confidence: std::collections::HashMap<String, sqry_core::confidence::ConfidenceMetadata>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphCacheMetadata {
    pub strategy: GraphCacheStrategyMetadata,
    pub trace_path: CacheMetricsSummary,
    pub subgraph: CacheMetricsSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_request: Option<CacheRequestEvent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphCacheStrategyMetadata {
    pub policy: &'static str,
    pub ttl_seconds: u64,
    pub trace_path_capacity: usize,
    pub subgraph_capacity: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheMetricsSummary {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub expired: u64,
    pub hit_rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warm_latency: Option<graph_cache::LatencyStatsSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cold_latency: Option<graph_cache::LatencyStatsSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event: Option<graph_cache::CacheEvent>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheRequestEvent {
    pub tool: &'static str,
    pub state: graph_cache::CacheState,
    pub latency_ms: u64,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CacheRequestContext {
    pub tool: &'static str,
    pub state: graph_cache::CacheState,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeData {
    pub start: PositionData,
    pub end: PositionData,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionData {
    pub line: u32,
    pub character: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeContext {
    pub code: String,
    pub lines_before: usize,
    pub lines_after: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRefData {
    pub name: String,
    #[serde(rename = "qualifiedName")]
    pub qualified_name: String,
    pub kind: String,
    pub language: String,
    #[serde(rename = "fileUri")]
    pub file_uri: String,
    pub range: RangeData,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub name: String,
    #[serde(rename = "qualifiedName")]
    pub qualified_name: String,
    pub kind: String,
    pub language: String,
    #[serde(rename = "fileUri")]
    pub file_uri: String,
    pub range: RangeData,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<CodeContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relations: Option<SearchHitRelations>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchHitRelations {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callers: Vec<NodeRefData>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callees: Vec<NodeRefData>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticSearchData {
    pub results: Vec<SearchHit>,
    pub total: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatusData {
    pub has_index: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_symbols: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_indexed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_relations: Option<bool>,
}

/// Response data for `rebuild_index` tool.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RebuildIndexData {
    /// Whether the rebuild was successful
    pub success: bool,
    /// Root path of the index
    pub root_path: String,
    /// Number of nodes (symbols) in the rebuilt index
    pub node_count: u64,
    /// Number of edges (relations) in the rebuilt index
    pub edge_count: u64,
    /// Number of files indexed
    pub files_indexed: u64,
    /// Timestamp when the index was built (RFC3339)
    pub built_at: String,
    /// Message describing the rebuild result
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationEdgeData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<NodeRefData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<NodeRefData>,
    #[serde(rename = "type")]
    pub relation_type: String,
    pub depth: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationQueryData {
    pub relation_type: String,
    pub relations: Vec<RelationEdgeData>,
    pub total: u64,
}

/// A node in the call hierarchy tree.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyNode {
    /// The symbol at this node
    pub symbol: NodeRefData,
    /// Child nodes (callers or callees depending on direction)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<CallHierarchyNode>,
    /// Call site ranges (where the call occurs)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub call_ranges: Vec<RangeData>,
}

/// Response data for call hierarchy tool.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallHierarchyData {
    /// The root symbol being analyzed
    pub root: NodeRefData,
    /// Direction of the hierarchy
    pub direction: String,
    /// The hierarchy nodes (callers or callees)
    pub items: Vec<CallHierarchyNode>,
    /// Total number of items found
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainRelations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callers: Option<Vec<NodeRefData>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callees: Option<Vec<NodeRefData>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainCodeData {
    pub symbol: NodeRefData,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<CodeContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relations: Option<ExplainRelations>,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DependencyGraphData {
    pub nodes: Vec<NodeRefData>,
    pub edges: Vec<RelationEdgeData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rendered: Option<String>,
}

impl DependencyGraphData {
    /// Create an empty graph (used as fallback when computation fails).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathStep {
    pub symbol: NodeRefData,
    pub edge_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallPath {
    pub steps: Vec<PathStep>,
    pub length: u32,
    pub score: f64,
    pub cross_language: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TracePathData {
    pub paths: Vec<CallPath>,
    pub from_symbol: String,
    pub to_symbol: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrossLanguageEdgesData {
    pub edges: Vec<RelationEdgeData>,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarSymbolData {
    pub symbol: NodeRefData,
    pub similarity: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindSimilarData {
    pub reference: NodeRefData,
    pub results: Vec<SimilarSymbolData>,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImpactedSymbol {
    pub symbol: NodeRefData,
    pub depth: u32,
    pub impact_type: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyImpactData {
    pub target_symbol: String,
    pub impacted_symbols: Vec<ImpactedSymbol>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_files: Option<Vec<String>>,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeChange {
    pub symbol_name: String,
    pub qualified_name: String,
    pub kind: String,
    pub change_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_location: Option<NodeRefData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_location: Option<NodeRefData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature_after: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticDiffData {
    pub base_ref: String,
    pub target_ref: String,
    pub changes: Vec<NodeChange>,
    pub summary: DiffSummary,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub added: u64,
    pub removed: u64,
    pub modified: u64,
    pub renamed: u64,
    pub signature_changed: u64,
    pub unchanged: u64,
}

/// Disambiguation option for natural language translation (P2-18)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NlDisambiguationOption {
    /// The command for this option
    pub command: String,
    /// The detected intent
    pub intent: String,
    /// Human-readable description
    pub description: String,
    /// Confidence score for this option
    pub confidence: f32,
}

/// Response data for natural language translation (P2-18)
///
/// The MCP server performs translation only - command execution is
/// the responsibility of the MCP client.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NlTranslationData {
    /// The type of response: execute, confirm, disambiguate, reject
    pub response_type: String,
    /// The translated command (if available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Translation confidence score (0.0-1.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Detected intent type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// User-facing prompt (for confirm/disambiguate)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Rejection reason (for reject)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Suggestions for improvement
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
    /// Disambiguation options (for disambiguate)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<NlDisambiguationOption>,
    /// Captured stdout from command execution (if execute=true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_output: Option<String>,
}

// ============================================================================
// Duplicate Detection Types
// ============================================================================

/// Symbol info in a duplicate group
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateSymbolData {
    /// Symbol name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Symbol kind (function, method, struct, etc.)
    pub kind: String,
    /// File URI
    pub file_uri: String,
    /// Start line (1-indexed)
    pub line: u32,
    /// Language
    pub language: String,
}

/// A group of duplicate symbols
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateGroupData {
    /// Group identifier as hex string.
    ///
    /// For body duplicates with 128-bit `body_hash`, this is a 32-character
    /// lowercase hexadecimal string (e.g., "000000000000000012345678abcdef01").
    /// For signature/struct duplicates, this is a 16-character hex string.
    pub group_id: String,
    /// Number of duplicates in this group
    pub count: usize,
    /// Symbols in this group
    pub symbols: Vec<DuplicateSymbolData>,
}

/// Response data for `find_duplicates` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindDuplicatesData {
    /// Type of duplicates found
    pub duplicate_type: String,
    /// Threshold used
    pub threshold: u32,
    /// Groups of duplicate symbols
    pub groups: Vec<DuplicateGroupData>,
    /// Total number of groups found
    pub total: u64,
}

// ============================================================================
// Cycle Detection Types
// ============================================================================

/// A single symbol in a cycle
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleNodeData {
    /// Symbol name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// File URI
    pub file_uri: String,
    /// Start line (1-indexed)
    pub line: u32,
}

/// A detected cycle
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleData {
    /// Cycle depth (number of nodes)
    pub depth: usize,
    /// Nodes in the cycle (A → B → C → A becomes [A, B, C])
    pub nodes: Vec<CycleNodeData>,
    /// Human-readable cycle chain (e.g., "A → B → C → A")
    pub chain: String,
}

/// Response data for `find_cycles` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindCyclesData {
    /// Type of cycles found
    pub cycle_type: String,
    /// Detected cycles
    pub cycles: Vec<CycleData>,
    /// Total number of cycles found
    pub total: u64,
}

// ============================================================================
// Unused Code Detection Types
// ============================================================================

/// An unused symbol
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnusedSymbolData {
    /// Symbol name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Symbol kind (function, method, struct, etc.)
    pub kind: String,
    /// File URI
    pub file_uri: String,
    /// Start line (1-indexed)
    pub line: u32,
    /// Language
    pub language: String,
    /// Visibility (public, private, etc.)
    pub visibility: String,
}

/// Response data for `find_unused` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindUnusedData {
    /// Scope used for analysis
    pub scope: String,
    /// Unused symbols
    pub symbols: Vec<UnusedSymbolData>,
    /// Total number of unused symbols found
    pub total: u64,
}

// ============================================================================
// New Graph-Based Tool Response Types
// ============================================================================

/// Response data for `is_node_in_cycle` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInCycleData {
    /// Symbol that was checked
    pub symbol: String,
    /// Whether the symbol is in a cycle
    pub in_cycle: bool,
    /// Type of cycle checked (calls, imports, modules)
    pub cycle_type: String,
    /// If in a cycle, the cycle containing this symbol
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle: Option<Vec<String>>,
}

/// Response data for `pattern_search` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternSearchData {
    /// Search pattern used
    pub pattern: String,
    /// Matching symbols
    pub matches: Vec<PatternMatchData>,
    /// Total matches found
    pub total: u64,
}

/// A single pattern match result
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternMatchData {
    /// Symbol name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// File URI
    pub file_uri: String,
    /// Line number
    pub line: u32,
    /// Language
    pub language: String,
}

/// Response data for `direct_callers` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectCallersData {
    /// Target symbol that was queried
    pub target: String,
    /// Symbols that call the target
    pub callers: Vec<CallerCalleeData>,
    /// Total number of callers
    pub total: u64,
}

/// Response data for `direct_callees` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectCalleesData {
    /// Source symbol that was queried
    pub source: String,
    /// Symbols called by the source
    pub callees: Vec<CallerCalleeData>,
    /// Total number of callees
    pub total: u64,
}

/// A caller or callee symbol
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallerCalleeData {
    /// Symbol name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// File URI
    pub file_uri: String,
    /// Line number
    pub line: u32,
    /// Language
    pub language: String,
}

// ============================================================================
// Introspection Tool Response Types
// ============================================================================

/// A file entry for `list_files` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntryData {
    /// File path relative to workspace
    pub path: String,
    /// Language of the file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// Response data for `list_files` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListFilesData {
    /// Files in the workspace
    pub files: Vec<FileEntryData>,
    /// Total number of files
    pub total: u64,
}

/// A symbol entry for `list_symbols` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolEntryData {
    /// Symbol name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// File path relative to workspace
    pub file_path: String,
    /// Start line (1-indexed)
    pub line: u32,
    /// Language
    pub language: String,
}

/// Response data for `list_symbols` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSymbolsData {
    /// Symbols in the workspace
    pub symbols: Vec<SymbolEntryData>,
    /// Total number of symbols
    pub total: u64,
}

/// Response data for `get_graph_stats` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphStatsData {
    /// Total number of nodes
    pub total_nodes: u64,
    /// Total number of edges
    pub total_edges: u64,
    /// Total number of files
    pub total_files: u64,
    /// Symbol counts by kind
    pub nodes_by_kind: std::collections::HashMap<String, u64>,
    /// File counts by language
    pub files_by_language: std::collections::HashMap<String, u64>,
    /// Graph version/epoch
    pub graph_epoch: u64,
}

// ============================================================================
// Navigation Tool Response Types
// ============================================================================

/// Response data for `get_definition` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionData {
    /// Symbol name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// File path
    pub file_path: String,
    /// Line number (1-indexed)
    pub line: u32,
    /// Column number (1-indexed)
    pub column: u32,
    /// Language
    pub language: String,
    /// Preview of the definition
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// Response data for `get_definition` tool (may have multiple results)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDefinitionData {
    /// Found definitions
    pub definitions: Vec<DefinitionData>,
    /// Total count
    pub total: u64,
}

/// Reference location data
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceLocationData {
    /// File path
    pub file_path: String,
    /// Line number (1-indexed)
    pub line: u32,
    /// Column number (1-indexed)
    pub column: u32,
    /// Preview of the reference context
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// Is this the declaration?
    pub is_declaration: bool,
}

/// Response data for `get_references` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetReferencesData {
    /// Symbol that was searched
    pub symbol: String,
    /// References found
    pub references: Vec<ReferenceLocationData>,
    /// Total count
    pub total: u64,
}

/// Response data for `get_hover_info` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HoverInfoData {
    /// Symbol name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// File path
    pub file_path: String,
    /// Line number
    pub line: u32,
    /// Language
    pub language: String,
    /// Type signature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Documentation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
}

/// Document symbol with children
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSymbolData {
    /// Symbol name
    pub name: String,
    /// Symbol kind
    pub kind: String,
    /// Start line (1-indexed)
    pub line: u32,
    /// End line (1-indexed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    /// Child symbols
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<DocumentSymbolData>,
}

/// Response data for `get_document_symbols` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDocumentSymbolsData {
    /// File path
    pub file_path: String,
    /// Symbols in the document
    pub symbols: Vec<DocumentSymbolData>,
    /// Total count
    pub total: u64,
}

/// Workspace symbol search result
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSymbolData {
    /// Symbol name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// File path
    pub file_path: String,
    /// Line number
    pub line: u32,
    /// Language
    pub language: String,
    /// Match score (0.0 - 1.0)
    pub score: f64,
}

/// Response data for `get_workspace_symbols` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWorkspaceSymbolsData {
    /// Query that was searched
    pub query: String,
    /// Matching symbols
    pub symbols: Vec<WorkspaceSymbolData>,
    /// Total count
    pub total: u64,
}

// ============================================================================
// Insights Tool Data Types
// ============================================================================

/// Language statistics for insights
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageStatsData {
    /// Language name
    pub language: String,
    /// Number of files
    pub files: usize,
    /// Number of symbols
    pub symbols: usize,
}

/// Symbol kind statistics for insights
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KindStatsData {
    /// Kind name
    pub kind: String,
    /// Count
    pub count: usize,
}

/// Health indicators for insights
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthIndicatorsData {
    /// Number of cycles detected
    pub cycles: usize,
    /// Number of unused symbols
    pub unused_symbols: usize,
    /// Number of duplicate groups
    pub duplicate_groups: usize,
    /// Number of cross-language edges
    pub cross_language_edges: usize,
}

/// Response data for `get_insights` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetInsightsData {
    /// Total number of files
    pub total_files: usize,
    /// Total number of symbols
    pub total_symbols: usize,
    /// Total number of edges
    pub total_edges: usize,
    /// Statistics by language
    pub languages: Vec<LanguageStatsData>,
    /// Statistics by symbol kind
    pub symbol_kinds: Vec<KindStatsData>,
    /// Health indicators
    pub health: HealthIndicatorsData,
}

/// Complexity metric for a single function/method
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplexityMetricData {
    /// Symbol name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Symbol kind
    pub kind: String,
    /// File path
    pub file_path: String,
    /// Estimated complexity
    pub complexity: u32,
    /// Line count
    pub lines: u32,
}

/// Response data for `complexity_metrics` tool
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplexityMetricsData {
    /// Complexity metrics for each function/method
    pub metrics: Vec<ComplexityMetricData>,
    /// Total count
    pub total: usize,
    /// Average complexity
    pub average_complexity: f64,
    /// Maximum complexity
    pub max_complexity: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== PositionData tests ==========

    #[test]
    fn test_position_data_creation() {
        let pos = PositionData {
            line: 10,
            character: 5,
        };
        assert_eq!(pos.line, 10);
        assert_eq!(pos.character, 5);
    }

    #[test]
    fn test_position_data_serialization() {
        let pos = PositionData {
            line: 42,
            character: 15,
        };
        let json = serde_json::to_string(&pos).unwrap();
        assert!(json.contains("\"line\":42"));
        assert!(json.contains("\"character\":15"));
    }

    // ========== RangeData tests ==========

    #[test]
    fn test_range_data_creation() {
        let range = RangeData {
            start: PositionData {
                line: 1,
                character: 0,
            },
            end: PositionData {
                line: 10,
                character: 20,
            },
        };
        assert_eq!(range.start.line, 1);
        assert_eq!(range.end.line, 10);
    }

    #[test]
    fn test_range_data_serialization() {
        let range = RangeData {
            start: PositionData {
                line: 5,
                character: 10,
            },
            end: PositionData {
                line: 5,
                character: 25,
            },
        };
        let json = serde_json::to_string(&range).unwrap();
        assert!(json.contains("\"start\""));
        assert!(json.contains("\"end\""));
    }

    // ========== CodeContext tests ==========

    #[test]
    fn test_code_context_creation() {
        let ctx = CodeContext {
            code: "fn main() {}".to_string(),
            lines_before: 3,
            lines_after: 3,
        };
        assert_eq!(ctx.code, "fn main() {}");
        assert_eq!(ctx.lines_before, 3);
        assert_eq!(ctx.lines_after, 3);
    }

    #[test]
    fn test_code_context_serialization() {
        let ctx = CodeContext {
            code: "let x = 1;".to_string(),
            lines_before: 2,
            lines_after: 2,
        };
        let json = serde_json::to_string(&ctx).unwrap();
        assert!(json.contains("\"code\""));
        assert!(json.contains("\"linesBefore\""));
        assert!(json.contains("\"linesAfter\""));
    }

    // ========== DependencyGraphData tests ==========

    #[test]
    fn test_dependency_graph_data_empty() {
        let graph = DependencyGraphData::empty();
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
        assert!(graph.rendered.is_none());
    }

    #[test]
    fn test_dependency_graph_data_default() {
        let graph = DependencyGraphData::default();
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
        assert!(graph.rendered.is_none());
    }

    #[test]
    fn test_dependency_graph_data_serialization_empty() {
        let graph = DependencyGraphData::empty();
        let json = serde_json::to_string(&graph).unwrap();
        assert!(json.contains("\"nodes\":[]"));
        assert!(json.contains("\"edges\":[]"));
        // rendered should be skipped when None
        assert!(!json.contains("\"rendered\""));
    }

    // ========== SearchHitRelations tests ==========

    #[test]
    fn test_search_hit_relations_default() {
        let relations = SearchHitRelations::default();
        assert!(relations.callers.is_empty());
        assert!(relations.callees.is_empty());
    }

    #[test]
    fn test_search_hit_relations_serialization_empty() {
        let relations = SearchHitRelations::default();
        let json = serde_json::to_string(&relations).unwrap();
        // Empty vectors should be skipped
        assert_eq!(json, "{}");
    }

    // ========== DiffSummary tests ==========

    #[test]
    fn test_diff_summary_creation() {
        let summary = DiffSummary {
            added: 10,
            removed: 5,
            modified: 3,
            renamed: 1,
            signature_changed: 2,
            unchanged: 100,
        };
        assert_eq!(summary.added, 10);
        assert_eq!(summary.removed, 5);
        assert_eq!(summary.modified, 3);
        assert_eq!(summary.renamed, 1);
        assert_eq!(summary.signature_changed, 2);
        assert_eq!(summary.unchanged, 100);
    }

    #[test]
    fn test_diff_summary_serialization() {
        let summary = DiffSummary {
            added: 1,
            removed: 2,
            modified: 3,
            renamed: 0,
            signature_changed: 1,
            unchanged: 50,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"added\":1"));
        assert!(json.contains("\"removed\":2"));
        assert!(json.contains("\"signatureChanged\":1"));
    }

    // ========== NlDisambiguationOption tests ==========

    #[test]
    fn test_nl_disambiguation_option_creation() {
        let option = NlDisambiguationOption {
            command: "query kind:function".to_string(),
            intent: "search".to_string(),
            description: "Search for functions".to_string(),
            confidence: 0.85,
        };
        assert_eq!(option.command, "query kind:function");
        assert_eq!(option.intent, "search");
        assert_eq!(option.confidence, 0.85);
    }

    #[test]
    fn test_nl_disambiguation_option_serialization() {
        let option = NlDisambiguationOption {
            command: "test".to_string(),
            intent: "execute".to_string(),
            description: "Run tests".to_string(),
            confidence: 0.9,
        };
        let json = serde_json::to_string(&option).unwrap();
        assert!(json.contains("\"command\":\"test\""));
        assert!(json.contains("\"confidence\":0.9"));
    }

    // ========== NlTranslationData tests ==========

    #[test]
    fn test_nl_translation_data_execute() {
        let data = NlTranslationData {
            response_type: "execute".to_string(),
            command: Some("query kind:function".to_string()),
            confidence: Some(0.95),
            intent: Some("search".to_string()),
            prompt: None,
            reason: None,
            suggestions: vec![],
            options: vec![],
            execution_output: None,
        };
        assert_eq!(data.response_type, "execute");
        assert!(data.command.is_some());
    }

    #[test]
    fn test_nl_translation_data_reject() {
        let data = NlTranslationData {
            response_type: "reject".to_string(),
            command: None,
            confidence: None,
            intent: None,
            prompt: None,
            reason: Some("Query too vague".to_string()),
            suggestions: vec!["Be more specific".to_string()],
            options: vec![],
            execution_output: None,
        };
        assert_eq!(data.response_type, "reject");
        assert_eq!(data.reason, Some("Query too vague".to_string()));
    }

    #[test]
    fn test_nl_translation_data_serialization_skips_none() {
        let data = NlTranslationData {
            response_type: "execute".to_string(),
            command: Some("test".to_string()),
            confidence: None,
            intent: None,
            prompt: None,
            reason: None,
            suggestions: vec![],
            options: vec![],
            execution_output: None,
        };
        let json = serde_json::to_string(&data).unwrap();
        // Optional fields should be skipped when None
        assert!(!json.contains("\"confidence\""));
        assert!(!json.contains("\"suggestions\""));
    }

    // ========== CallPath tests ==========

    #[test]
    fn test_call_path_creation() {
        let path = CallPath {
            steps: vec![],
            length: 3,
            score: 0.85,
            cross_language: false,
        };
        assert_eq!(path.length, 3);
        assert_eq!(path.score, 0.85);
        assert!(!path.cross_language);
    }

    #[test]
    fn test_call_path_cross_language() {
        let path = CallPath {
            steps: vec![],
            length: 5,
            score: 0.7,
            cross_language: true,
        };
        assert!(path.cross_language);
    }

    // ========== ImpactedSymbol tests ==========

    #[test]
    fn test_impacted_symbol_creation() {
        let symbol = ImpactedSymbol {
            symbol: NodeRefData {
                name: "test_fn".to_string(),
                qualified_name: "module::test_fn".to_string(),
                kind: "function".to_string(),
                language: "rust".to_string(),
                file_uri: "file:///test.rs".to_string(),
                range: RangeData {
                    start: PositionData {
                        line: 1,
                        character: 0,
                    },
                    end: PositionData {
                        line: 5,
                        character: 1,
                    },
                },
                metadata: None,
            },
            depth: 2,
            impact_type: "caller".to_string(),
        };
        assert_eq!(symbol.depth, 2);
        assert_eq!(symbol.impact_type, "caller");
    }

    // ========== GraphMetadata tests ==========

    #[test]
    fn test_graph_metadata_creation() {
        let metadata = GraphMetadata {
            total_nodes: 1000,
            total_edges: 5000,
            languages: vec!["rust".to_string(), "python".to_string()],
            cross_language_edges: 50,
            graph_version: "2.0.0".to_string(),
            rebuild_epoch_ms: 1704067200000,
            cache: None,
            confidence: std::collections::HashMap::new(),
        };
        assert_eq!(metadata.total_nodes, 1000);
        assert_eq!(metadata.total_edges, 5000);
        assert_eq!(metadata.languages.len(), 2);
    }

    #[test]
    fn test_graph_metadata_serialization() {
        let metadata = GraphMetadata {
            total_nodes: 100,
            total_edges: 200,
            languages: vec![],
            cross_language_edges: 0,
            graph_version: "1.0.0".to_string(),
            rebuild_epoch_ms: 0,
            cache: None,
            confidence: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&metadata).unwrap();
        assert!(json.contains("\"totalNodes\":100"));
        assert!(json.contains("\"graphVersion\":\"1.0.0\""));
        // Empty languages should be skipped
        assert!(!json.contains("\"languages\""));
        // Empty confidence should be skipped
        assert!(!json.contains("\"confidence\""));
    }

    #[test]
    fn test_graph_metadata_with_confidence() {
        use sqry_core::confidence::{ConfidenceLevel, ConfidenceMetadata};

        let mut confidence = std::collections::HashMap::new();
        confidence.insert(
            "rust".to_string(),
            ConfidenceMetadata {
                level: ConfidenceLevel::AstOnly,
                limitations: vec!["No type inference".to_string()],
                unavailable_features: vec!["rust-analyzer".to_string()],
            },
        );

        let metadata = GraphMetadata {
            total_nodes: 100,
            total_edges: 200,
            languages: vec!["rust".to_string()],
            cross_language_edges: 0,
            graph_version: "2.8.0".to_string(),
            rebuild_epoch_ms: 0,
            cache: None,
            confidence,
        };

        let json = serde_json::to_string(&metadata).unwrap();
        // Confidence should be included when non-empty
        assert!(json.contains("\"confidence\""), "confidence field missing");
        assert!(json.contains("\"rust\""), "rust language key missing");
        assert!(json.contains("\"level\""), "level field missing");
        assert!(
            json.contains("\"ast_only\""),
            "ast_only value missing in: {json}"
        );
        assert!(json.contains("\"No type inference\""), "limitation missing");
    }

    // ========== IndexStatusData tests ==========

    #[test]
    fn test_index_status_data_no_index() {
        let status = IndexStatusData {
            has_index: false,
            root_path: None,
            indexed_symbols: None,
            files_indexed: None,
            index_version: None,
            created_at: None,
            updated_at: None,
            has_relations: None,
        };
        assert!(!status.has_index);
    }

    #[test]
    fn test_index_status_data_with_index() {
        let status = IndexStatusData {
            has_index: true,
            root_path: Some("/project".to_string()),
            indexed_symbols: Some(5000),
            files_indexed: Some(100),
            index_version: Some("2.0.0".to_string()),
            created_at: Some("2024-01-01T00:00:00Z".to_string()),
            updated_at: Some("2024-01-02T00:00:00Z".to_string()),
            has_relations: Some(true),
        };
        assert!(status.has_index);
        assert_eq!(status.indexed_symbols, Some(5000));
    }
}
