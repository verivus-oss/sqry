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

/// Structured search filter object for narrowing `semantic_search` and
/// `hierarchical_search` results by language, symbol kind, visibility, or
/// minimum relevance score.
///
/// **This is a JSON object, not a query string.** Pass it as the `filters`
/// parameter alongside the `query` parameter.
///
/// For string-style filtering, use query predicates like `lang:rust` in the
/// `query` parameter instead.
///
/// # Example (JSON)
///
/// ```json
/// {
///   "language": ["rust", "typescript"],
///   "symbol_kind": ["function", "method"],
///   "visibility": "public",
///   "cfg_condition": { "semantic_match": "linux" },
///   "score_min": 0.5
/// }
/// ```
///
/// # Query predicates vs structured filters
///
/// | Approach | Parameter | Syntax |
/// |----------|-----------|--------|
/// | Query predicates | `query` | `"lang:rust kind:function vis:public"` |
/// | Structured filters | `filters` | `{"language":["rust"],"symbol_kind":["function"],"visibility":"public"}` |
///
/// Both can be combined: use `query` for complex boolean expressions
/// (AND/OR/NOT/regex) and `filters` for simple pre-filtering.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default)]
pub struct SearchFiltersParams {
    /// Limit results to specific programming languages.
    ///
    /// Example: `["rust", "typescript", "python"]`
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub language: Vec<String>,

    /// Filter by symbol visibility (`"public"` or `"private"`).
    ///
    /// Example: `"public"`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<VisibilityParam>,

    /// Filter by symbol kinds such as `"function"`, `"method"`, `"class"`,
    /// `"struct"`, `"trait"`, `"interface"`, `"enum"`, etc.
    ///
    /// Example: `["function", "method"]`
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub symbol_kind: Vec<String>,

    /// Minimum semantic relevance score (0.0 – 1.0). Results below this
    /// threshold are excluded.
    ///
    /// Example: `0.5`
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub score_min: Option<f64>,

    /// Filter by stored conditional-compilation metadata
    /// (`macro_metadata.cfg_condition`).
    ///
    /// `equals` is byte-exact, `matches` is a Rust regex, and
    /// `semantic_match` applies the same cross-language cfg comparator used by
    /// planner `cfg:<flag>` queries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cfg_condition: Option<CfgConditionFilterParams>,
}

/// Structured `cfg_condition` filter for search tools.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(default)]
pub struct CfgConditionFilterParams {
    /// Byte-exact match against the stored `cfg_condition` string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,

    /// Rust regex matched against the stored `cfg_condition` string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,

    /// Cross-language semantic cfg match. A bare flag such as `linux`
    /// matches Go `linux`, Rust `target_os = "linux"`, and compound
    /// expressions containing a positive `linux` term.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_match: Option<String>,
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

// ============================================================================
// Phase β joint-stubs: framework + resolved_via filter param wrappers
// ============================================================================
//
// These wrappers expose `sqry_core::schema::FrameworkId` and
// `sqry_core::schema::ResolvedVia` through the JsonSchema-generating
// path used by every MCP tool input schema. The wrappers are paper-thin
// (paper-thin pass-throughs with `From` impls in both directions), matching
// the established pattern used for `VisibilityParam` / `DuplicateTypeParam`.
//
// They are accepted as optional fields on the five tool param structs
// (`relation_query`, `direct_callers`, `direct_callees`, `semantic_search`,
// `sqry_query`) in this PR but the fields are **not consumed** by tool
// execution — Plan A and Plan B's downstream PRs wire each filter into
// the planner / executor. See the field documentation on each Params struct.

/// MCP-facing framework identifier (Plan A joint-stub).
///
/// Wraps [`sqry_core::schema::FrameworkId`] with a `JsonSchema` derive so the
/// tool's input schema declares `framework` as a typed enum. Snake-case
/// serde so the wire form is `"flask"`, `"fast_api"`, ...
///
/// Discriminants `0..=18` match `FrameworkId` exactly (the wrapper is a
/// 1:1 mapping). Variant ordering is appended in lockstep with the core
/// enum — a new framework MUST be added to both.
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FrameworkIdParam {
    AspNetCore,
    Actix,
    Axum,
    Chi,
    Django,
    Express,
    FastApi,
    Fastify,
    Flask,
    Gin,
    Koa,
    Laravel,
    NestJs,
    Rails,
    Rocket,
    Sinatra,
    Spring,
    Starlette,
    Symfony,
}

impl From<FrameworkIdParam> for sqry_core::schema::FrameworkId {
    fn from(f: FrameworkIdParam) -> Self {
        use sqry_core::schema::FrameworkId;
        match f {
            FrameworkIdParam::AspNetCore => FrameworkId::AspNetCore,
            FrameworkIdParam::Actix => FrameworkId::Actix,
            FrameworkIdParam::Axum => FrameworkId::Axum,
            FrameworkIdParam::Chi => FrameworkId::Chi,
            FrameworkIdParam::Django => FrameworkId::Django,
            FrameworkIdParam::Express => FrameworkId::Express,
            FrameworkIdParam::FastApi => FrameworkId::FastApi,
            FrameworkIdParam::Fastify => FrameworkId::Fastify,
            FrameworkIdParam::Flask => FrameworkId::Flask,
            FrameworkIdParam::Gin => FrameworkId::Gin,
            FrameworkIdParam::Koa => FrameworkId::Koa,
            FrameworkIdParam::Laravel => FrameworkId::Laravel,
            FrameworkIdParam::NestJs => FrameworkId::NestJs,
            FrameworkIdParam::Rails => FrameworkId::Rails,
            FrameworkIdParam::Rocket => FrameworkId::Rocket,
            FrameworkIdParam::Sinatra => FrameworkId::Sinatra,
            FrameworkIdParam::Spring => FrameworkId::Spring,
            FrameworkIdParam::Starlette => FrameworkId::Starlette,
            FrameworkIdParam::Symfony => FrameworkId::Symfony,
        }
    }
}

impl From<sqry_core::schema::FrameworkId> for FrameworkIdParam {
    fn from(f: sqry_core::schema::FrameworkId) -> Self {
        use sqry_core::schema::FrameworkId;
        match f {
            FrameworkId::AspNetCore => FrameworkIdParam::AspNetCore,
            FrameworkId::Actix => FrameworkIdParam::Actix,
            FrameworkId::Axum => FrameworkIdParam::Axum,
            FrameworkId::Chi => FrameworkIdParam::Chi,
            FrameworkId::Django => FrameworkIdParam::Django,
            FrameworkId::Express => FrameworkIdParam::Express,
            FrameworkId::FastApi => FrameworkIdParam::FastApi,
            FrameworkId::Fastify => FrameworkIdParam::Fastify,
            FrameworkId::Flask => FrameworkIdParam::Flask,
            FrameworkId::Gin => FrameworkIdParam::Gin,
            FrameworkId::Koa => FrameworkIdParam::Koa,
            FrameworkId::Laravel => FrameworkIdParam::Laravel,
            FrameworkId::NestJs => FrameworkIdParam::NestJs,
            FrameworkId::Rails => FrameworkIdParam::Rails,
            FrameworkId::Rocket => FrameworkIdParam::Rocket,
            FrameworkId::Sinatra => FrameworkIdParam::Sinatra,
            FrameworkId::Spring => FrameworkIdParam::Spring,
            FrameworkId::Starlette => FrameworkIdParam::Starlette,
            FrameworkId::Symfony => FrameworkIdParam::Symfony,
        }
    }
}

/// MCP-facing dispatch-resolution provenance (Plan B V12 8-variant form).
///
/// Wraps [`sqry_core::schema::ResolvedVia`] with a `JsonSchema` derive so the
/// tool's input schema declares `resolved_via` as a typed enum array.
/// Snake-case serde — wire forms are `"direct"`, `"type_match"`,
/// `"binding_plane"`, `"virtual_dispatch"`, `"interface_dispatch"`,
/// `"duck_typed"`, `"structural"`, `"promiscuous_elided"`. Stays in
/// lockstep with the core enum (Plan B DESIGN §3.2 — pinned discriminants
/// 0..=7 on the core side).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedViaParam {
    Direct,
    TypeMatch,
    BindingPlane,
    VirtualDispatch,
    InterfaceDispatch,
    DuckTyped,
    Structural,
    PromiscuousElided,
}

impl From<ResolvedViaParam> for sqry_core::schema::ResolvedVia {
    fn from(r: ResolvedViaParam) -> Self {
        use sqry_core::schema::ResolvedVia;
        match r {
            ResolvedViaParam::Direct => ResolvedVia::Direct,
            ResolvedViaParam::TypeMatch => ResolvedVia::TypeMatch,
            ResolvedViaParam::BindingPlane => ResolvedVia::BindingPlane,
            ResolvedViaParam::VirtualDispatch => ResolvedVia::VirtualDispatch,
            ResolvedViaParam::InterfaceDispatch => ResolvedVia::InterfaceDispatch,
            ResolvedViaParam::DuckTyped => ResolvedVia::DuckTyped,
            ResolvedViaParam::Structural => ResolvedVia::Structural,
            ResolvedViaParam::PromiscuousElided => ResolvedVia::PromiscuousElided,
        }
    }
}

impl From<sqry_core::schema::ResolvedVia> for ResolvedViaParam {
    fn from(r: sqry_core::schema::ResolvedVia) -> Self {
        use sqry_core::schema::ResolvedVia;
        match r {
            ResolvedVia::Direct => ResolvedViaParam::Direct,
            ResolvedVia::TypeMatch => ResolvedViaParam::TypeMatch,
            ResolvedVia::BindingPlane => ResolvedViaParam::BindingPlane,
            ResolvedVia::VirtualDispatch => ResolvedViaParam::VirtualDispatch,
            ResolvedVia::InterfaceDispatch => ResolvedViaParam::InterfaceDispatch,
            ResolvedVia::DuckTyped => ResolvedViaParam::DuckTyped,
            ResolvedVia::Structural => ResolvedViaParam::Structural,
            ResolvedVia::PromiscuousElided => ResolvedViaParam::PromiscuousElided,
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
    /// T3.6 (Cluster G): outbound `Wraps` edges. Surfaces every
    /// error-chain wrap relationship (`fmt.Errorf %w`, `Unwrap`,
    /// `errors.{Is,As,Join}`) from the resolved symbol, irrespective
    /// of `WrapKind`. For kind-filtered queries use
    /// `sqry_query "wraps:<kind>"`.
    Wraps,
    /// T2.4 (Go channels): `ChannelPeer` edges anchored on a channel /
    /// containing function. The container-level `rename_all = "lowercase"`
    /// would serialize this as `"channelpeers"`, so the wire string is pinned
    /// explicitly.
    #[serde(rename = "channel_peers")]
    ChannelPeers,
    /// T2.5 (Go generics): `Instantiates` edges of a generic function /
    /// method. The explicit rename is redundant (already lowercases to
    /// `"instantiations"`) but kept for symmetry with `ChannelPeers`.
    #[serde(rename = "instantiations")]
    Instantiations,
}

impl From<RelationTypeParam> for CoreRelationKind {
    fn from(r: RelationTypeParam) -> Self {
        match r {
            RelationTypeParam::Callers => CoreRelationKind::Callers,
            RelationTypeParam::Callees => CoreRelationKind::Callees,
            RelationTypeParam::Imports => CoreRelationKind::Imports,
            RelationTypeParam::Exports => CoreRelationKind::Exports,
            RelationTypeParam::Returns => CoreRelationKind::Returns,
            RelationTypeParam::Wraps => CoreRelationKind::Wraps,
            RelationTypeParam::ChannelPeers => CoreRelationKind::ChannelPeers,
            RelationTypeParam::Instantiations => CoreRelationKind::Instantiations,
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
            CoreRelationKind::Wraps => RelationTypeParam::Wraps,
            CoreRelationKind::ChannelPeers => RelationTypeParam::ChannelPeers,
            CoreRelationKind::Instantiations => RelationTypeParam::Instantiations,
        }
    }
}

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum GraphFormatParam {
    #[default]
    Json,
    Dot,
    D2,
    Mermaid,
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
    /// Semantic query expression in the **core query parser** grammar:
    /// predicates `lang:`, `kind:`, `path:`, `name:`, `visibility:`,
    /// `parent:`, etc., plus combinators `AND` / `OR` / `NOT`, plus
    /// the inline regex predicate `name~=/regex/`. The regex inside
    /// `name~=/.../` uses Rust regex syntax.
    ///
    /// This is NOT the same as the CLI `sqry search "<pattern>"`
    /// regex — that is a top-level pattern, not a `name~=` predicate.
    /// And it is NOT the planner grammar used by `sqry_query` /
    /// `sqry plan-query`, which does not accept `name~=`.
    ///
    /// On workspaces with >50k symbols, every `name~=/regex/` must be
    /// paired with at least one of `lang:`, `path:`, or `kind:`. The
    /// cost gate (cluster-B IMP-B) returns `query_too_broad` for
    /// unpaired regex predicates.
    pub query: String,

    #[serde(default = "default_path")]
    pub path: String,

    /// Structured filter object for pre-filtering by language, kind,
    /// visibility, or minimum score. Pass a JSON object — for string-style
    /// predicates, use the `query` parameter instead.
    #[serde(default)]
    pub filters: Option<SearchFiltersParams>,

    #[serde(default = "default_max_results_200")]
    #[schemars(range(min = 1, max = 10000))]
    pub max_results: i64,

    /// Context lines around matches
    #[serde(default = "default_context_lines")]
    #[schemars(range(min = 0, max = 20))]
    pub context_lines: i64,

    /// Include classpath (external dependency) results.
    /// Defaults to false — only workspace results are returned.
    #[serde(default)]
    pub include_classpath: bool,

    #[serde(default)]
    pub pagination: Option<PaginationParams>,

    /// Per-call runtime row budget cap (per `C_budget.md` §C5 +
    /// `00_contracts.md` §3.CC-2). When set, the executor's
    /// `evaluate_all` hot loop trips after examining `budget_rows`
    /// nodes and surfaces the canonical `query_too_broad` envelope
    /// with `details.source = "runtime_budget"`. `None` (the
    /// default) defers to the daemon-wide
    /// `SQRY_TOOL_BUDGET_ROWS` env var or the documented default
    /// (`5_000_000` rows).
    #[serde(default)]
    pub budget_rows: Option<u64>,

    /// Phase β joint-stub (Plan A) — filter results to nodes carrying
    /// framework-route metadata for the named framework. Declared in
    /// **first** position relative to `resolved_via` per the Phase β
    /// joint-stubs ordering contract.
    ///
    /// **Stub:** no extractor populates framework metadata in this PR,
    /// so a non-`None` value matches zero nodes. Plan A's downstream
    /// PR (`feat/framework-route-extractors`) wires the planner
    /// predicate that consumes this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<FrameworkIdParam>,

    /// Phase β joint-stub (Plan B) — filter results to nodes whose
    /// outgoing `Calls` edges include at least one resolution
    /// provenance in the given set. Declared in **second** position
    /// relative to `framework` per the Phase β joint-stubs ordering
    /// contract.
    ///
    /// **Stub:** today the planner accepts the filter and threads it
    /// through to the executor, but no resolver emits the new
    /// dispatch-resolution variants Plan B will add (the existing
    /// 3-variant enum is still in use). Plan B's downstream PR
    /// (`U_WS2_8_MCP_FILTERS`) wires the predicate end-to-end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_via: Option<Vec<ResolvedViaParam>>,

    /// Optional loaded revision id. When omitted, semantic_search targets the
    /// live workspace exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_id: Option<String>,

    /// Optional Git ref selector for a loaded revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_ref: Option<String>,

    /// Optional commit object id selector for a loaded revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_commit: Option<String>,

    /// Optional tree object id selector for a loaded revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_tree: Option<String>,

    /// Select a loaded dirty snapshot revision.
    #[serde(default)]
    pub revision_dirty: bool,

    /// Include untracked files for `revision_dirty`.
    #[serde(default)]
    pub revision_include_untracked: bool,

    /// Include ignored files for `revision_dirty`.
    #[serde(default)]
    pub revision_include_ignored: bool,
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
    /// Query expression in the **core query parser** grammar (same as
    /// `semantic_search.query`): predicates `lang:`, `kind:`, `path:`,
    /// `name:`, `visibility:`, etc., combinators `AND` / `OR` / `NOT`,
    /// and the inline regex predicate `name~=/regex/` using Rust regex
    /// syntax.
    ///
    /// NOT the planner grammar used by `sqry_query` / `sqry plan-query`,
    /// which does not accept `name~=`.
    ///
    /// On workspaces with >50k symbols, pair `name~=/regex/` with
    /// `lang:`, `path:`, or `kind:` — see
    /// `docs/cli/scaling-large-codebases.md`.
    pub query: String,

    #[serde(default = "default_path")]
    pub path: String,

    /// Structured filter object for pre-filtering by language, kind,
    /// visibility, or minimum score. Pass a JSON object — for string-style
    /// predicates, use the `query` parameter instead.
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

    /// Per-call runtime row budget cap (per `C_budget.md` §C5).
    /// See [`SemanticSearchParams::budget_rows`] for the contract.
    #[serde(default)]
    pub budget_rows: Option<u64>,
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

    /// Per-call runtime row budget cap (per `C_budget.md` §C5).
    /// See [`SemanticSearchParams::budget_rows`] for the contract.
    /// Currently advisory on this surface — `relation_query`'s
    /// inner body uses sqry-db planner queries rather than the
    /// executor's `evaluate_all` hot loop, so the budget does not
    /// gate this path. Tracked for follow-up under cluster-C's
    /// deferred-row marker.
    #[serde(default)]
    pub budget_rows: Option<u64>,

    /// Phase β joint-stub (Plan A) — framework filter. First in the
    /// Phase β joint-stubs ordering. See [`SemanticSearchParams::framework`]
    /// for the contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<FrameworkIdParam>,

    /// Phase β joint-stub (Plan B) — resolved-via set filter. Second in the
    /// Phase β joint-stubs ordering. See [`SemanticSearchParams::resolved_via`]
    /// for the contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_via: Option<Vec<ResolvedViaParam>>,
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

/// `structural_similar` params (body-shape descriptor, U07).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct StructuralSimilarParams {
    /// Reference function/method name (simple or qualified).
    pub symbol_name: String,

    /// Optional file to disambiguate the probe symbol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,

    #[serde(default = "default_path")]
    pub path: String,

    /// Minimum MinHash similarity floor (0.0-1.0).
    #[serde(default = "default_similarity_threshold")]
    #[schemars(range(min = 0.0, max = 1.0))]
    pub similarity_threshold: f64,

    #[serde(default = "default_max_results_20")]
    #[schemars(range(min = 1, max = 200))]
    pub max_results: i64,
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

/// `workspace_status` params (`STEP_7`).
///
/// Returns the aggregate `WorkspaceIndexStatus` for the
/// currently-resolved workspace plus identity surfaces. The optional
/// `workspace_id` is **not** used for routing today (the session
/// resolver still anchors on `path`); it is echoed back in the
/// response so clients can detect mismatches against the identity they
/// expected.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[schemars(example = "json!({
    \"path\": \".\"
})")]
pub struct WorkspaceStatusParams {
    /// Workspace path. Defaults to the current directory if omitted.
    #[serde(default = "default_path")]
    pub path: String,
    /// Optional client-supplied workspace identity (full 64-char hex
    /// digest of the BLAKE3 `WorkspaceId`). Echoed back in the
    /// response so clients can validate that the server resolved the
    /// expected workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
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

    /// Multiple seed symbol names (alternative to `symbol_name` for multi-seed exports)
    #[serde(default)]
    pub symbols: Vec<String>,

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
    /// Validate XOR constraint: at least one of `file_path`, `symbol_name`, or `symbols` must be provided.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.file_path.is_none() && self.symbol_name.is_none() && self.symbols.is_empty() {
            return Err(RpcError::validation_with_data(
                "At least one of file_path, symbol_name, or symbols must be provided",
                serde_json::json!({
                    "kind": "validation",
                    "constraint": "xor",
                    "fields": ["file_path", "symbol_name", "symbols"]
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

    /// Optional file path to disambiguate the symbol when multiple
    /// definitions share the same name. Relative to workspace root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,

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
// Structural Query Tool (DB13 — planner)
// ============================================================================

/// `sqry_query` params.
///
/// Executes a structural query through the `sqry-db` planner (parse →
/// compile → fuse → execute). The text syntax is documented in
/// `docs/superpowers/specs/2026-04-12-derived-analysis-db-query-planner-design.md`
/// (§3 — Text Syntax Frontend).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[schemars(example = "json!({
    \"query\": \"kind:function has:caller\",
    \"path\": \".\",
    \"limit\": 100
})")]
pub struct SqryQueryParams {
    /// Text query in the planner syntax (e.g.
    /// `"kind:function callers:main in:src/api/**"`).
    pub query: String,

    /// Workspace path. Defaults to the current directory if omitted.
    #[serde(default = "default_path")]
    pub path: String,

    /// Maximum number of matching nodes to include in the response.
    /// Defaults to 1000; larger values widen the response payload but do
    /// not change execution cost on the server side.
    #[serde(default)]
    pub limit: Option<u64>,

    /// Per-call runtime row budget cap (per `C_budget.md` §C5).
    /// See [`SemanticSearchParams::budget_rows`] for the contract.
    /// Currently advisory on this surface — `sqry_query`'s body
    /// runs through the sqry-db planner rather than the
    /// executor's `evaluate_all` hot loop, so the budget does not
    /// gate this path. Tracked for follow-up under cluster-C's
    /// deferred-row marker.
    #[serde(default)]
    pub budget_rows: Option<u64>,

    /// Phase β joint-stub (Plan A) — framework filter. First in the
    /// Phase β joint-stubs ordering. See [`SemanticSearchParams::framework`]
    /// for the contract.
    ///
    /// On `sqry_query` (the planner-direct surface) this param is
    /// orthogonal to the `query` text — the planner predicate that will
    /// consume it (`Predicate::FrameworkEq`) can also be expressed via
    /// the planner grammar's `framework:<id>` token in Plan A. Both
    /// surfaces converge on the same IR variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<FrameworkIdParam>,

    /// Phase β joint-stub (Plan B) — resolved-via set filter. Second in the
    /// Phase β joint-stubs ordering. See [`SemanticSearchParams::resolved_via`]
    /// for the contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_via: Option<Vec<ResolvedViaParam>>,
}

// ============================================================================
// Analysis Tool Parameter Types
// ============================================================================

/// Duplicate type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum DuplicateTypeParam {
    #[default]
    Body,
    Signature,
    Struct,
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
#[derive(Default)]
pub enum CycleTypeParam {
    #[default]
    Calls,
    Imports,
    Modules,
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
#[derive(Default)]
pub enum UnusedScopeParam {
    Public,
    Private,
    Function,
    Struct,
    #[default]
    All,
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

    /// Maximum number of member symbols to return per duplicate group (default 10).
    ///
    /// Groups with more members than this cap will have their `members_truncated`
    /// field set to `true` and `total_members` will reflect the full pre-truncation
    /// count. Set to `0` to return all members with no cap (pre-v9.1 behavior).
    /// Valid range: `[0, 10000]`.
    #[serde(default = "default_max_members_per_group")]
    #[schemars(range(min = 0, max = 10000))]
    pub max_members_per_group: i64,

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

/// `context_propagation` mode filter (T3.7, Cluster G).
///
/// Maps directly onto
/// [`sqry_db::queries::context_propagation::ContextModeFilter`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextModeParam {
    /// Return every classified leak.
    #[default]
    All,
    /// Only `BreakSite` leaks (sync caller with ctx + ctx-accepting callee).
    BreakSite,
    /// Only `UnthreadedGoroutine` leaks (`go callee(...)` paths).
    UnthreadedGoroutine,
    /// Only `HttpHandlerLeak` leaks
    /// (`func(http.ResponseWriter, *http.Request)` callers).
    HttpHandlerLeak,
}

impl From<ContextModeParam> for sqry_db::queries::context_propagation::ContextModeFilter {
    fn from(m: ContextModeParam) -> Self {
        use sqry_db::queries::context_propagation::ContextModeFilter as Cmf;
        match m {
            ContextModeParam::All => Cmf::All,
            ContextModeParam::BreakSite => Cmf::BreakSite,
            ContextModeParam::UnthreadedGoroutine => Cmf::UnthreadedGoroutine,
            ContextModeParam::HttpHandlerLeak => Cmf::HttpHandlerLeak,
        }
    }
}

/// `context_propagation` scope selector (T3.7, Cluster G). Mirrors
/// the contract in `02_DESIGN` §2.5 row 3 (`scope: "global" | "file"`)
/// while keeping the file path attached on the same value the way
/// the underlying `sqry_db::queries::context_propagation::ContextScope`
/// does. Tagged externally for JSON-Schema clarity:
///
/// ```json
/// { "kind": "global" }
/// { "kind": "file", "path": "src/foo.go" }
/// ```
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextScopeParam {
    /// Whole-workspace scope (default).
    #[default]
    Global,
    /// Restrict to leaks whose caller function lives in this file.
    File {
        /// File path, resolved relative to the workspace root.
        path: String,
    },
}

/// `context_propagation` params (T3.7, Cluster G).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ContextPropagationParams {
    /// Workspace path. Defaults to the current directory if omitted.
    #[serde(default = "default_path")]
    pub path: String,

    /// Scope selector. Defaults to `global`. See [`ContextScopeParam`]
    /// for the wire shape.
    #[serde(default)]
    pub scope: ContextScopeParam,

    /// Mode filter. Defaults to `all`.
    #[serde(default)]
    pub mode: ContextModeParam,

    /// Maximum number of leak records to return. Defaults to 200.
    #[serde(default = "default_max_results_200")]
    #[schemars(range(min = 1, max = 5000))]
    pub max_results: i64,
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

    /// T3.8 (Cluster G): suppress symbols whose `cfg_condition` slot
    /// is populated. A platform-specific implementation (`//go:build
    /// linux`, `#[cfg(unix)]`) often looks "unused" on the analyst's
    /// host but is live on other platforms — set this to `true` to
    /// exclude that class of finding.
    #[serde(default)]
    pub exclude_cfg_gated: bool,
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

fn default_max_members_per_group() -> i64 {
    10
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

    /// Optional file path to disambiguate the symbol when multiple
    /// definitions share the same name. Relative to workspace root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,

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

    /// Include classpath (external dependency) results.
    /// Defaults to false — only workspace results are returned.
    #[serde(default)]
    pub include_classpath: bool,

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

    /// Phase β joint-stub (Plan A) — framework filter. First in the
    /// Phase β joint-stubs ordering. See [`SemanticSearchParams::framework`]
    /// for the contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<FrameworkIdParam>,

    /// Phase β joint-stub (Plan B) — resolved-via set filter. Second in the
    /// Phase β joint-stubs ordering. See [`SemanticSearchParams::resolved_via`]
    /// for the contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_via: Option<Vec<ResolvedViaParam>>,
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

    /// Phase β joint-stub (Plan A) — framework filter. First in the
    /// Phase β joint-stubs ordering. See [`SemanticSearchParams::framework`]
    /// for the contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framework: Option<FrameworkIdParam>,

    /// Phase β joint-stub (Plan B) — resolved-via set filter. Second in the
    /// Phase β joint-stubs ordering. See [`SemanticSearchParams::resolved_via`]
    /// for the contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_via: Option<Vec<ResolvedViaParam>>,
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

    /// Budget-safe summary mode (issue #394). When true, return only aggregate
    /// counts (total + per-kind + per-language histograms) for the scoped set,
    /// with no per-symbol rows. Use this to overview a large subtree (e.g.
    /// `path="rust/"`) where a full `semantic_search` would exhaust the row
    /// budget. Default false (normal per-symbol listing).
    #[serde(default)]
    pub summary: bool,

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

/// `expand_cache_status` params.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
pub struct ExpandCacheStatusParams {
    /// Workspace path
    #[serde(default = "default_path")]
    pub path: String,
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
            file_path: None,
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn test_is_node_in_cycle_empty_symbol_invalid() {
        let params = IsNodeInCycleParams {
            symbol: String::new(),
            path: ".".to_string(),
            cycle_type: CycleTypeParam::Calls,
            min_depth: 2,
            max_depth: None,
            include_self_loops: false,
            file_path: None,
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
            file_path: None,
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
            file_path: None,
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
            file_path: None,
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
            file_path: None,
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
            file_path: None,
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
            file_path: None,
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
            file_path: None,
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
            include_classpath: false,
            pagination: None,
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn test_pattern_search_empty_pattern_invalid() {
        let params = PatternSearchParams {
            pattern: String::new(),
            path: ".".to_string(),
            max_results: 100,
            include_classpath: false,
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
            include_classpath: false,
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
            framework: None,
            resolved_via: None,
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn test_direct_callers_empty_symbol_invalid() {
        let params = DirectCallersParams {
            symbol: String::new(),
            path: ".".to_string(),
            max_results: 100,
            pagination: None,
            framework: None,
            resolved_via: None,
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
            framework: None,
            resolved_via: None,
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
            framework: None,
            resolved_via: None,
        };
        assert!(params.validate().is_ok());
    }

    #[test]
    fn test_direct_callees_empty_symbol_invalid() {
        let params = DirectCalleesParams {
            symbol: String::new(),
            path: ".".to_string(),
            max_results: 100,
            pagination: None,
            framework: None,
            resolved_via: None,
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
            framework: None,
            resolved_via: None,
        };
        assert!(params.validate().is_err());
    }

    // ========================================================================
    // Phase β joint-stubs — framework + resolved_via filter parse tests
    // ========================================================================
    //
    // One test per tool, confirming the new optional params parse from
    // JSON into the matching Params struct and that the field values
    // round-trip. None of these tests assert behavioural impact —
    // execution today is a no-op (Plan A / Plan B's downstream PRs wire
    // the predicates). The contract these tests pin is *shape only*:
    // - both fields accept `None` (default) and are skipped on serialize
    // - `framework` accepts the typed `FrameworkIdParam` enum
    // - `resolved_via` accepts a `Vec<ResolvedViaParam>` set
    // - declaration order matches the joint-stubs contract: `framework`
    //   first, `resolved_via` second (verified by serde reading both
    //   fields independently of source order)

    fn assert_framework_and_resolved_via<'de, P>(json: &'de str, tool: &str)
    where
        P: serde::Deserialize<'de>,
    {
        let _: P = serde_json::from_str(json)
            .unwrap_or_else(|err| panic!("Phase β joint-stub params for {tool} must parse: {err}"));
    }

    #[test]
    fn phase_beta_semantic_search_accepts_framework_and_resolved_via() {
        let json = r#"{
            "query": "kind:function",
            "framework": "flask",
            "resolved_via": ["direct", "type_match"]
        }"#;
        assert_framework_and_resolved_via::<SemanticSearchParams>(json, "semantic_search");
        // Round-trip on a struct literal — confirms field-name ordering
        // declaration matches the joint-stubs contract (framework first,
        // resolved_via second).
        let params: SemanticSearchParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.framework, Some(FrameworkIdParam::Flask));
        assert_eq!(
            params.resolved_via,
            Some(vec![ResolvedViaParam::Direct, ResolvedViaParam::TypeMatch]),
        );
    }

    #[test]
    fn phase_beta_relation_query_accepts_framework_and_resolved_via() {
        let json = r#"{
            "symbol": "main",
            "relation_type": "callers",
            "framework": "spring",
            "resolved_via": ["binding_plane"]
        }"#;
        assert_framework_and_resolved_via::<RelationQueryParams>(json, "relation_query");
        let params: RelationQueryParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.framework, Some(FrameworkIdParam::Spring));
        assert_eq!(
            params.resolved_via,
            Some(vec![ResolvedViaParam::BindingPlane]),
        );
    }

    #[test]
    fn phase_beta_direct_callers_accepts_framework_and_resolved_via() {
        let json = r#"{
            "symbol": "main",
            "framework": "axum",
            "resolved_via": ["direct"]
        }"#;
        assert_framework_and_resolved_via::<DirectCallersParams>(json, "direct_callers");
        let params: DirectCallersParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.framework, Some(FrameworkIdParam::Axum));
        assert_eq!(params.resolved_via, Some(vec![ResolvedViaParam::Direct]),);
    }

    #[test]
    fn phase_beta_direct_callees_accepts_framework_and_resolved_via() {
        let json = r#"{
            "symbol": "main",
            "framework": "gin",
            "resolved_via": ["type_match"]
        }"#;
        assert_framework_and_resolved_via::<DirectCalleesParams>(json, "direct_callees");
        let params: DirectCalleesParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.framework, Some(FrameworkIdParam::Gin));
        assert_eq!(params.resolved_via, Some(vec![ResolvedViaParam::TypeMatch]),);
    }

    #[test]
    fn phase_beta_sqry_query_accepts_framework_and_resolved_via() {
        let json = r#"{
            "query": "kind:function",
            "framework": "fast_api",
            "resolved_via": ["direct", "binding_plane"]
        }"#;
        assert_framework_and_resolved_via::<SqryQueryParams>(json, "sqry_query");
        let params: SqryQueryParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.framework, Some(FrameworkIdParam::FastApi));
        assert_eq!(
            params.resolved_via,
            Some(vec![
                ResolvedViaParam::Direct,
                ResolvedViaParam::BindingPlane
            ]),
        );
    }

    /// Phase β joint-stubs: both filter params are optional. Omitting
    /// them must continue to deserialize cleanly (back-compat for every
    /// existing MCP client). Tested on each of the 5 tools.
    #[test]
    fn phase_beta_filter_params_are_optional_back_compat() {
        let s: SemanticSearchParams = serde_json::from_str(r#"{"query": "kind:function"}"#)
            .expect("semantic_search must parse with no framework / resolved_via");
        assert!(s.framework.is_none() && s.resolved_via.is_none());

        let r: RelationQueryParams =
            serde_json::from_str(r#"{"symbol": "m", "relation_type": "callers"}"#)
                .expect("relation_query must parse without filters");
        assert!(r.framework.is_none() && r.resolved_via.is_none());

        let dc: DirectCallersParams = serde_json::from_str(r#"{"symbol": "m"}"#)
            .expect("direct_callers must parse without filters");
        assert!(dc.framework.is_none() && dc.resolved_via.is_none());

        let de: DirectCalleesParams = serde_json::from_str(r#"{"symbol": "m"}"#)
            .expect("direct_callees must parse without filters");
        assert!(de.framework.is_none() && de.resolved_via.is_none());

        let q: SqryQueryParams = serde_json::from_str(r#"{"query": "kind:function"}"#)
            .expect("sqry_query must parse without filters");
        assert!(q.framework.is_none() && q.resolved_via.is_none());
    }

    /// Phase β joint-stubs: the wrapper enum round-trips cleanly to the
    /// canonical core type and back. Locks the From / Into impls.
    #[test]
    fn phase_beta_framework_id_param_round_trips_through_core() {
        for param in [
            FrameworkIdParam::Flask,
            FrameworkIdParam::Spring,
            FrameworkIdParam::AspNetCore,
            FrameworkIdParam::Symfony,
        ] {
            let core: sqry_core::schema::FrameworkId = param.into();
            let back: FrameworkIdParam = core.into();
            assert_eq!(param, back);
        }
    }

    #[test]
    fn phase_beta_resolved_via_param_round_trips_through_core() {
        for param in [
            ResolvedViaParam::Direct,
            ResolvedViaParam::TypeMatch,
            ResolvedViaParam::BindingPlane,
        ] {
            let core: sqry_core::schema::ResolvedVia = param.into();
            let back: ResolvedViaParam = core.into();
            assert_eq!(param, back);
        }
    }
}
