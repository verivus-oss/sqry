//! Tool definitions and parameter handling for sqry-mcp.
//!
//! This module contains two key submodules with distinct roles:
//!
//! - **`params`**: Public API contract for rmcp SDK integration. Contains schemars-derived
//!   parameter structs that define the external JSON schema exposed to MCP clients.
//!   These types are deserialized from incoming requests and converted to internal types.
//!
//! - **`validation`**: Internal validation types and logic. Contains the `*Args` structs
//!   used by the execution layer, along with `validate_*` functions that enforce
//!   business rules (bounds checking, required fields, etc.). The rmcp server converts
//!   from `params::*` types to `validation::*Args` types.
//!
//!
//! ## Data Flow
//!
//! ```text
//! MCP Client Request
//!       │
//!       ▼
//! params::*Params (schemars/serde)
//!       │
//!       ▼ (server.rs convert_*_params functions)
//!       │
//! validation::*Args (internal)
//!       │
//!       ▼
//! execution::execute_*() functions
//! ```

pub mod params;
#[cfg_attr(test, allow(dead_code))]
mod validation;
pub mod workspace_status;

pub use workspace_status::{WorkspaceStatusArgs, execute_workspace_status};

pub use params::{ContextPropagationParams, SqryAskParams, SqryQueryParams};
pub use validation::{
    CallHierarchyArgs,
    CallHierarchyDirection,
    CfgConditionFilter,
    ChangeType,
    // Insights tool args
    ComplexityMetricsArgs,
    CrossLanguageEdgesArgs,
    CycleType,
    DependencyImpactArgs,
    // New graph-based tool args
    DirectCalleesArgs,
    DirectCallersArgs,
    DuplicateType,
    ExpandCacheStatusArgs,
    ExplainCodeArgs,
    ExportGraphArgs,
    FindCyclesArgs,
    FindDuplicatesArgs,
    FindUnusedArgs,
    // Introspection tool args
    GetDefinitionArgs,
    GetDocumentSymbolsArgs,
    GetGraphStatsArgs,
    GetHoverInfoArgs,
    GetIndexStatusArgs,
    GetInsightsArgs,
    GetReferencesArgs,
    GetWorkspaceSymbolsArgs,
    GitVersionRef,
    HierarchicalSearchArgs,
    IsNodeInCycleArgs,
    ListFilesArgs,
    ListSymbolsArgs,
    PaginationArgs,
    PatternSearchArgs,
    // Index tool args
    RebuildIndexArgs,
    RelationQueryArgs,
    RelationType,
    SearchFilters,
    SearchSimilarArgs,
    SemanticDiffArgs,
    SemanticDiffFilters,
    SemanticSearchArgs,
    ShowDependenciesArgs,
    SubgraphArgs,
    TracePathArgs,
    UnusedScope,
    Visibility,
};
#[allow(unused_imports)]
pub use validation::{ContextPropagationArgs, ContextScopeArg};

#[cfg(any(test, fuzzing))]
#[allow(unused_imports)]
pub use validation::{
    validate_cross_language_edges_args, validate_dependency_impact_args,
    validate_explain_code_args, validate_export_graph_args, validate_get_index_status_args,
    validate_relation_query_args, validate_search_similar_args, validate_semantic_diff_args,
    validate_semantic_search_args, validate_show_dependencies_args, validate_subgraph_args,
    validate_trace_path_args,
};
