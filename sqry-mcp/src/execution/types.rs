//! Data types for MCP tool execution responses.
//!
//! This module contains the response data structures used by the MCP tool handlers.

use serde::Serialize;
use serde_json::Value;

use super::graph_cache;

// ============================================================================
// Classpath Provenance Types
// ============================================================================

/// Provenance information for symbols originating from external classpath JARs.
///
/// Only present in tool results when the symbol comes from a classpath dependency
/// (e.g., a Maven/Gradle library). Workspace symbols do not carry provenance.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceData {
    /// Source type — always `"classpath"` for JAR-sourced symbols.
    pub source: &'static str,
    /// Maven coordinates (e.g., `"com.google.guava:guava:33.0.0"`).
    /// `None` when the JAR has no embedded POM metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinates: Option<String>,
    /// Whether this is a direct (vs. transitive) dependency.
    pub is_direct: bool,
    /// JAR file path this symbol was extracted from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jar_path: Option<String>,
}

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
    /// How the location range was resolved. Populated for stub-aware
    /// resolution (DB15+): one of `"OwnSpan"`, `"CanonicalSibling"`,
    /// `"IncomingEdgeSpan"`, `"ExternSymbol"`, `"Fallback"`. Absent for
    /// node refs that bypass `node_location_for_reporting`.
    #[serde(rename = "resolutionSource", skip_serializing_if = "Option::is_none")]
    pub resolution_source: Option<String>,
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
    /// Macro boundary metadata (only present when the node has macro-related info)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macro_metadata: Option<MacroMetadataResponse>,
    /// Classpath provenance (only present for symbols from external JARs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceData>,
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

/// Response data for the `sqry_query` MCP tool (DB13).
///
/// The tool runs a text query through the sqry-db planner pipeline and
/// returns the matched nodes with minimal location metadata so callers can
/// render links or follow-up queries without fetching the graph snapshot
/// themselves.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqryQueryData {
    /// The query string as submitted by the client (echoed back for
    /// logging / conversation history).
    pub query: String,
    /// Total number of nodes that matched (before `limit` truncation).
    pub total_matches: u64,
    /// Whether `hits` was truncated below `total_matches`.
    pub truncated: bool,
    /// Matched nodes with file + line metadata.
    pub hits: Vec<SqryQueryHit>,
    /// Soft failure for requests that require declaration-fidelity data absent
    /// from the loaded graph snapshot. Present only when the query uses
    /// `items` / `is_definition` against a pre-V16 graph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reindex_required: Option<ReindexRequiredData>,
}

/// Response data for the `context_propagation` MCP tool (T3.7,
/// Cluster G). Wraps the `ContextLeakSet` returned by
/// `ContextPropagationQuery` with the request-scope echo and a
/// `truncated` flag so the client can paginate against
/// `max_results`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPropagationData {
    /// Echo of the request scope (`"global"` or `"file:<path>"`).
    pub scope: String,
    /// Echo of the request mode filter (`"all"` /
    /// `"break_site"` / `"unthreaded_goroutine"` / `"http_handler_leak"`).
    pub mode: String,
    /// Total leaks returned by the query before `max_results` truncation.
    pub total: u64,
    /// Whether `leaks` was truncated below `total`.
    pub truncated: bool,
    /// Flat list of context leaks. The query's classification is
    /// preserved on each entry's `mode` field so a client can
    /// filter further without re-running the query.
    pub leaks: Vec<ContextLeakDto>,
}

/// One context-propagation leak finding, surfaced through the MCP
/// `context_propagation` tool. Mirrors
/// `sqry_db::queries::context_propagation::ContextLeak` (`01_SPEC`
/// §5.2.a + `02_DESIGN` §2.5) while exposing user-facing names and
/// the byte-range span shape.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextLeakDto {
    /// Why this call site is a leak. One of `"break_site"`,
    /// `"unthreaded_goroutine"`, `"http_handler_leak"`.
    pub mode: String,
    /// Caller function's qualified name (or simple name when the
    /// graph has no qualified form).
    pub caller: String,
    /// Callee function's qualified name.
    pub callee: String,
    /// Filesystem path of the caller's source file — included as a
    /// convenience for IDE jump-to. The authoritative location info
    /// is on `call_span`.
    pub caller_file: String,
    /// Source-text range covering the failing call expression. Lines
    /// are 1-based for IDE friendliness; columns are 0-based byte
    /// offsets (matching tree-sitter's `Point` shape).
    pub call_span: ContextLeakSpan,
    /// `NodeId` of the caller's `ctx context.Context` parameter when
    /// the graph plugin emits one. The Go plugin currently leaves
    /// this `None` (parameter `NodeIds` are synthetic and not user-
    /// facing); future plugins may populate it. Serialised as
    /// `{ "index": u32, "generation": u64 }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_ctx_param: Option<ContextLeakNodeRef>,
}

/// 1-based-line, 0-based-byte-column source range, matching the
/// `Span` shape that the underlying `sqry_db` `ContextLeak` carries.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextLeakSpan {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

/// Opaque caller-ctx-param `NodeId` reference. The Go plugin never
/// populates this today; the struct exists so future plugins can
/// emit a stable IDE jump-to handle without breaking the wire shape.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextLeakNodeRef {
    pub index: u32,
    pub generation: u64,
}

/// One matched node's metadata for the `sqry_query` tool response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqryQueryHit {
    /// Symbol's short name as interned in the graph.
    pub name: String,
    /// Fully qualified name if the graph recorded one; else equals `name`.
    pub qualified_name: String,
    /// `NodeKind` in lowercase `snake_case` form.
    pub kind: String,
    /// Filesystem path of the file containing this symbol.
    pub file: String,
    /// 1-based line number of the symbol's starting location.
    pub line: u32,
    /// Visibility modifier if recorded on the node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
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
    /// Classpath provenance (only present for symbols from external JARs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceData>,
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

/// One structural-neighbour match (body-shape descriptor, U07). Carries the two
/// distinct AC-4 numbers: exact structural identity and approximate similarity.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralNeighborData {
    pub symbol: NodeRefData,
    /// True when the neighbour's `shape_hash` is byte-identical to the probe's
    /// (a rename/relocate-invariant exact structural match).
    pub shape_hash_exact: bool,
    /// Approximate MinHash Jaccard similarity (0.0–1.0).
    pub jaccard: f64,
}

/// `structural_similar` result: a probe plus its identifier-blind structural
/// neighbours, ranked exact-first then by MinHash similarity.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuralSimilarData {
    pub reference: NodeRefData,
    pub results: Vec<StructuralNeighborData>,
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

/// A single channel / generic edge difference between two snapshots (T2
/// `02_DESIGN.md` §7.6). Only `Added` / `Removed` apply to edges.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeChange {
    /// Qualified name of the edge source node (a `CallSite`).
    pub source: String,
    /// Qualified name of the edge target node (a `Channel` or generic callee).
    pub target: String,
    /// `"send"` / `"receive"` / `"close"` for `ChannelPeer`; the inference kind
    /// (`"explicit"` / `"inferred"` / `"partial"` / `"unknown"`) for
    /// Instantiates.
    pub discriminator: String,
    /// Resolved generic type arguments in declaration order (Instantiates
    /// only; empty for `ChannelPeer`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub type_args: Vec<EdgeTypeArg>,
}

/// A resolved generic type argument on an [`EdgeChange`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeTypeArg {
    pub name: String,
    pub default_typed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticDiffData {
    pub base_ref: String,
    pub target_ref: String,
    pub changes: Vec<NodeChange>,
    pub summary: DiffSummary,
    pub total: u64,
    /// `ChannelPeer` edges added in the target snapshot (T2 §7.6).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub channel_peer_edges_added: Vec<EdgeChange>,
    /// `ChannelPeer` edges removed in the target snapshot.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub channel_peer_edges_removed: Vec<EdgeChange>,
    /// `Instantiates` edges added in the target snapshot.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub instantiates_edges_added: Vec<EdgeChange>,
    /// `Instantiates` edges removed in the target snapshot.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub instantiates_edges_removed: Vec<EdgeChange>,
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
    /// Number of symbols displayed in this group (`symbols.len()`).
    ///
    /// This is the displayed (possibly truncated) count, not the total pre-truncation
    /// count. Preserved as the displayed count for backward compatibility.
    /// See `total_members` for the full pre-truncation count.
    pub count: usize,
    /// Total number of members in this group before any per-group cap was applied.
    ///
    /// When `members_truncated` is `false` this equals `count`.  When
    /// `members_truncated` is `true` this is larger than `count` and reflects
    /// the actual number of duplicates found before the
    /// `max_members_per_group` limit was applied.
    pub total_members: usize,
    /// `true` when the `symbols` list was capped by `max_members_per_group`.
    ///
    /// When `true`, `total_members > count` and the caller may request more
    /// members by increasing `max_members_per_group` or setting it to `0`
    /// for unlimited.
    pub members_truncated: bool,
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
    /// Classpath provenance (only present for symbols from external JARs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceData>,
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
    /// Symbols in the workspace (empty in summary mode).
    pub symbols: Vec<SymbolEntryData>,
    /// Total number of symbols (matching the scope filters).
    pub total: u64,
    /// Budget-safe aggregate summary (issue #394). Present only when the request
    /// asked for `summary` mode; omitted otherwise so non-summary responses stay
    /// byte-identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ListSymbolsSummary>,
    /// Soft failure for requests that require declaration-fidelity data absent
    /// from the loaded graph snapshot. Present only when `items_only` was
    /// requested against a pre-V16 graph.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reindex_required: Option<ReindexRequiredData>,
}

/// Soft reindex result for tool calls that need graph data absent from the
/// loaded snapshot.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReindexRequiredData {
    /// Human-readable reason for the soft result.
    pub reason: String,
}

/// Aggregate counts for `list_symbols` summary mode (issue #394). Computed over
/// the scoped set without materializing per-symbol rows and without the planner
/// row budget, so it succeeds on large subtrees where `semantic_search` would
/// trip `query_too_broad`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSymbolsSummary {
    /// Count of matching symbols per `NodeKind` (lowercased Debug name).
    pub by_kind: std::collections::BTreeMap<String, u64>,
    /// Count of matching symbols per language.
    pub by_language: std::collections::BTreeMap<String, u64>,
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
    /// Number of workspace (non-external) nodes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_nodes: Option<u64>,
    /// Number of classpath (external) nodes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classpath_nodes: Option<u64>,
    /// Number of workspace (non-external) files
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_files: Option<u64>,
    /// Number of classpath (external) files
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classpath_files: Option<u64>,
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
    /// Macro boundary metadata (only present when the node has macro-related info)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macro_metadata: Option<MacroMetadataResponse>,
    /// Classpath provenance (only present for symbols from external JARs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceData>,
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
    /// Classpath provenance (only present for references in external JARs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceData>,
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
    /// Macro boundary metadata for the searched symbol (only present when available)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macro_metadata: Option<MacroMetadataResponse>,
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
    /// Classpath provenance (only present for symbols from external JARs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceData>,
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
    /// Macro boundary metadata (only present when the node has macro-related info)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macro_metadata: Option<MacroMetadataResponse>,
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
    /// Macro boundary statistics (only present when metadata store is non-empty)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macro_boundaries: Option<MacroBoundariesStatsData>,
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

// ============================================================================
// Macro Boundary Response Types
// ============================================================================

/// Macro boundary metadata attached to search/navigation responses.
///
/// Only included when a node has macro-relevant metadata. Uses
/// `skip_serializing_if` so absent fields don't appear in JSON.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroMetadataResponse {
    /// Whether this symbol was generated by macro expansion
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macro_generated: Option<bool>,
    /// Qualified name of the macro that generated this symbol
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macro_source: Option<String>,
    /// The cfg predicate string (e.g., `"test"`, `"feature = \"serde\""`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cfg_condition: Option<String>,
    /// Whether this cfg is active (`None` = unknown)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cfg_active: Option<bool>,
    /// Proc-macro kind for proc-macro function nodes
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proc_macro_kind: Option<String>,
    /// Whether expansion data came from cache vs live `cargo expand`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expansion_cached: Option<bool>,
    /// Unresolved attribute paths
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved_attributes: Vec<String>,
}

/// Macro boundaries statistics for `get_insights` responses.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MacroBoundariesStatsData {
    /// Number of attribute macros detected on items
    pub attribute_macros_detected: usize,
    /// Number of symbols gated by `cfg/cfg_attr`
    pub cfg_gated_symbols: usize,
    /// Number of symbols generated by macro expansion
    pub macro_generated_symbols: usize,
    /// Number of unresolved attribute paths across all nodes
    pub unresolved_attributes: usize,
    /// Expand cache status: "fresh", "stale", or "absent"
    pub expand_cache_status: String,
}

/// Response data for `expand_cache_status` tool.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpandCacheStatusData {
    /// Whether the expand cache directory exists
    pub cache_exists: bool,
    /// Path to the expand cache directory
    pub cache_path: String,
    /// Number of cache files found
    pub cache_files: usize,
    /// Total size of cache files in bytes
    pub total_size_bytes: u64,
    /// Per-crate cache info (crate name to status)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub crates: Vec<CrateCacheEntry>,
    /// Overall freshness: "fresh", "stale", or "absent"
    pub status: String,
}

/// Per-crate expand cache entry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrateCacheEntry {
    /// Crate name
    pub crate_name: String,
    /// Cache file path (relative)
    pub file_name: String,
    /// File size in bytes
    pub size_bytes: u64,
    /// Number of generated symbols in this cache entry
    pub generated_symbols: usize,
    /// Confidence level
    pub confidence: String,
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

    // ========== CallPath tests ==========

    #[test]
    #[allow(clippy::float_cmp)] // Approximate threshold comparison
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
                resolution_source: None,
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
            #[allow(clippy::unreadable_literal)] // Threshold constant
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

    // ===== MacroMetadataResponse tests =====

    #[test]
    fn test_macro_metadata_response_serialization_skips_empty_fields() {
        let response = MacroMetadataResponse {
            macro_generated: Some(true),
            macro_source: None,
            cfg_condition: None,
            cfg_active: None,
            proc_macro_kind: None,
            expansion_cached: None,
            unresolved_attributes: vec![],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json.get("macroGenerated").unwrap(), true);
        assert!(json.get("macroSource").is_none());
        assert!(json.get("cfgCondition").is_none());
        assert!(json.get("unresolvedAttributes").is_none());
    }

    #[test]
    fn test_macro_metadata_response_full_serialization() {
        let response = MacroMetadataResponse {
            macro_generated: Some(true),
            macro_source: Some("derive_Debug".to_string()),
            cfg_condition: Some("test".to_string()),
            cfg_active: Some(true),
            proc_macro_kind: Some("derive".to_string()),
            expansion_cached: Some(false),
            unresolved_attributes: vec!["custom_attr".to_string()],
        };
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json.get("macroGenerated").unwrap(), true);
        assert_eq!(json.get("macroSource").unwrap(), "derive_Debug");
        assert_eq!(json.get("cfgCondition").unwrap(), "test");
        assert_eq!(json.get("cfgActive").unwrap(), true);
        assert_eq!(json.get("procMacroKind").unwrap(), "derive");
        assert_eq!(json.get("expansionCached").unwrap(), false);
        let attrs = json
            .get("unresolvedAttributes")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0], "custom_attr");
    }

    #[test]
    fn test_macro_boundaries_stats_data_serialization() {
        let stats = MacroBoundariesStatsData {
            attribute_macros_detected: 5,
            cfg_gated_symbols: 3,
            macro_generated_symbols: 10,
            unresolved_attributes: 2,
            expand_cache_status: "fresh".to_string(),
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json.get("attributeMacrosDetected").unwrap(), 5);
        assert_eq!(json.get("cfgGatedSymbols").unwrap(), 3);
        assert_eq!(json.get("macroGeneratedSymbols").unwrap(), 10);
        assert_eq!(json.get("unresolvedAttributes").unwrap(), 2);
        assert_eq!(json.get("expandCacheStatus").unwrap(), "fresh");
    }

    #[test]
    fn test_expand_cache_status_data_serialization() {
        let data = ExpandCacheStatusData {
            cache_exists: true,
            cache_path: ".sqry/expand-cache".to_string(),
            cache_files: 2,
            total_size_bytes: 4096,
            crates: vec![CrateCacheEntry {
                crate_name: "my_crate".to_string(),
                file_name: "my_crate.json".to_string(),
                size_bytes: 2048,
                generated_symbols: 5,
                confidence: "heuristic".to_string(),
            }],
            status: "fresh".to_string(),
        };
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json.get("cacheExists").unwrap(), true);
        assert_eq!(json.get("cacheFiles").unwrap(), 2);
        assert_eq!(json.get("status").unwrap(), "fresh");
        let crates = json.get("crates").unwrap().as_array().unwrap();
        assert_eq!(crates.len(), 1);
        assert_eq!(crates[0].get("crateName").unwrap(), "my_crate");
        assert_eq!(crates[0].get("generatedSymbols").unwrap(), 5);
    }

    #[test]
    fn test_expand_cache_status_data_empty_crates_skipped() {
        let data = ExpandCacheStatusData {
            cache_exists: false,
            cache_path: ".sqry/expand-cache".to_string(),
            cache_files: 0,
            total_size_bytes: 0,
            crates: vec![],
            status: "absent".to_string(),
        };
        let json = serde_json::to_value(&data).unwrap();
        assert!(json.get("crates").is_none());
        assert_eq!(json.get("status").unwrap(), "absent");
    }

    #[test]
    fn test_definition_data_with_macro_metadata() {
        let def = DefinitionData {
            name: "my_fn".to_string(),
            qualified_name: "crate::my_fn".to_string(),
            kind: "Function".to_string(),
            file_path: "src/lib.rs".to_string(),
            line: 10,
            column: 1,
            language: "rust".to_string(),
            preview: None,
            macro_metadata: Some(MacroMetadataResponse {
                macro_generated: Some(true),
                macro_source: Some("derive_Debug".to_string()),
                cfg_condition: None,
                cfg_active: None,
                proc_macro_kind: None,
                expansion_cached: None,
                unresolved_attributes: vec![],
            }),
            provenance: None,
        };
        let json = serde_json::to_value(&def).unwrap();
        let mm = json.get("macroMetadata").unwrap();
        assert_eq!(mm.get("macroGenerated").unwrap(), true);
        assert_eq!(mm.get("macroSource").unwrap(), "derive_Debug");
    }

    #[test]
    fn test_definition_data_without_macro_metadata() {
        let def = DefinitionData {
            name: "my_fn".to_string(),
            qualified_name: "crate::my_fn".to_string(),
            kind: "Function".to_string(),
            file_path: "src/lib.rs".to_string(),
            line: 10,
            column: 1,
            language: "rust".to_string(),
            preview: None,
            macro_metadata: None,
            provenance: None,
        };
        let json = serde_json::to_value(&def).unwrap();
        assert!(json.get("macroMetadata").is_none());
    }

    #[test]
    fn test_get_insights_data_with_macro_boundaries() {
        let data = GetInsightsData {
            total_files: 10,
            total_symbols: 100,
            total_edges: 50,
            languages: vec![],
            symbol_kinds: vec![],
            health: HealthIndicatorsData {
                cycles: 0,
                unused_symbols: 5,
                duplicate_groups: 2,
                cross_language_edges: 0,
            },
            macro_boundaries: Some(MacroBoundariesStatsData {
                attribute_macros_detected: 3,
                cfg_gated_symbols: 7,
                macro_generated_symbols: 12,
                unresolved_attributes: 1,
                expand_cache_status: "stale".to_string(),
            }),
        };
        let json = serde_json::to_value(&data).unwrap();
        let mb = json.get("macroBoundaries").unwrap();
        assert_eq!(mb.get("attributeMacrosDetected").unwrap(), 3);
        assert_eq!(mb.get("expandCacheStatus").unwrap(), "stale");
    }

    #[test]
    fn test_get_insights_data_without_macro_boundaries() {
        let data = GetInsightsData {
            total_files: 10,
            total_symbols: 100,
            total_edges: 50,
            languages: vec![],
            symbol_kinds: vec![],
            health: HealthIndicatorsData {
                cycles: 0,
                unused_symbols: 0,
                duplicate_groups: 0,
                cross_language_edges: 0,
            },
            macro_boundaries: None,
        };
        let json = serde_json::to_value(&data).unwrap();
        assert!(json.get("macroBoundaries").is_none());
    }
}
