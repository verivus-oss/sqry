//! Daemon IPC wire-schema → internal `*Args` converters.
//!
//! Phase 8b Task 7. The daemon's JSON-RPC router deserialises
//! incoming tool-method `params` into the `schemars`-generated
//! [`crate::tools::params`] types (same wire contract as the rmcp
//! transport), then calls a `params_to_<tool>_args` function to
//! lower them into the internal `*Args` structs consumed by
//! [`crate::daemon_adapter::execute_*_for_daemon`].
//!
//! # Why duplicate the converter logic?
//!
//! The rmcp `SqryServer` path has its own `convert_*_params`
//! helpers inside `server.rs`. Those helpers are `pub(self)` and
//! intertwined with rmcp-specific code (e.g., `rmcp::ErrorData`
//! mapping). Rather than expand their visibility across the crate
//! boundary — which would force the daemon to depend on rmcp — this
//! module re-implements the same transforms in a daemon-friendly
//! shape that returns the crate-local [`crate::error::RpcError`].
//! The *Args* produced here are bit-for-bit identical to the ones
//! `server.rs` produces: same field values, same bounds validation,
//! same error messages.
//!
//! # Adding a new tool method
//!
//! 1. Ensure the matching `*Params` type exists in
//!    [`crate::tools::params`] and is `Deserialize + JsonSchema`.
//! 2. Mirror the `server.rs` `convert_<tool>_params` function here.
//! 3. Extend [`crate::daemon_adapter`] with a matching
//!    `execute_<tool>_for_daemon` wrapper (Task 4 surface).
//! 4. Extend `sqry-daemon`'s `tool_dispatch` match to route the
//!    JSON-RPC method string to the new helper.

use serde_json::{Value, json};

use crate::error::RpcError;
use crate::pagination::decode_cursor;
use crate::tools::params::{
    ChangeTypeParam, ComplexityMetricsParams, CycleTypeParam, DependencyImpactParams,
    DirectCalleesParams, DirectCallersParams, EdgeKindParam, ExportGraphParams, FindCyclesParams,
    FindUnusedParams, GraphFormatParam, IsNodeInCycleParams, PaginationParams, RelationQueryParams,
    RelationTypeParam, SearchFiltersParams, SemanticDiffParams, SemanticSearchParams,
    ShowDependenciesParams, SubgraphParams, TracePathParams, UnusedScopeParam, VisibilityParam,
};
use crate::tools::{
    ChangeType, ComplexityMetricsArgs, CycleType, DependencyImpactArgs, DirectCalleesArgs,
    DirectCallersArgs, ExportGraphArgs, FindCyclesArgs, FindUnusedArgs, GitVersionRef,
    IsNodeInCycleArgs, PaginationArgs, RelationQueryArgs, RelationType, SearchFilters,
    SemanticDiffArgs, SemanticDiffFilters, SemanticSearchArgs, ShowDependenciesArgs, SubgraphArgs,
    TracePathArgs, UnusedScope, Visibility,
};

// ---------------------------------------------------------------------------
// Shared numeric bounds helpers (mirrors server.rs `validate_*`).
//
// Duplicated on purpose — see module docs.
// ---------------------------------------------------------------------------

fn validate_usize(value: i64, field: &str, min: i64, max: i64) -> Result<usize, RpcError> {
    if value < min || value > max {
        return Err(RpcError::validation(format!(
            "{field} must be in {min}..={max}, got {value}"
        )));
    }
    usize::try_from(value)
        .map_err(|_| RpcError::validation(format!("{field} out of usize range: {value}")))
}

fn validate_max_results(value: i64, max_limit: i64) -> Result<usize, RpcError> {
    validate_usize(value, "max_results", 1, max_limit)
}

fn validate_context_lines(value: i64) -> Result<usize, RpcError> {
    validate_usize(value, "context_lines", 0, 20)
}

/// Cluster-C iter-2: reject `budget_rows: 0` at the daemon-hosted MCP
/// boundary too. Mirror of `sqry-mcp/src/server.rs::validate_budget_rows`.
fn validate_budget_rows(value: Option<u64>) -> Result<Option<u64>, RpcError> {
    match value {
        Some(0) => Err(RpcError::validation_with_data(
            "budget_rows must be > 0".to_string(),
            serde_json::json!({
                "kind": "validation",
                "constraint": "range",
                "field": "budget_rows",
                "min": 1,
                "actual": 0,
            }),
        )),
        other => Ok(other),
    }
}

fn validate_max_depth(value: i64, max_limit: i64) -> Result<usize, RpcError> {
    validate_usize(value, "max_depth", 1, max_limit)
}

fn validate_page_size(value: i64) -> Result<usize, RpcError> {
    validate_usize(value, "page_size", 1, 500)
}

fn validate_max_nodes(value: i64) -> Result<usize, RpcError> {
    validate_usize(value, "max_nodes", 1, 500)
}

fn validate_max_hops(value: i64) -> Result<usize, RpcError> {
    validate_usize(value, "max_hops", 1, 10)
}

fn validate_max_paths(value: i64) -> Result<usize, RpcError> {
    validate_usize(value, "max_paths", 1, 20)
}

fn convert_pagination(
    page_token: Option<String>,
    page_size: i64,
    pagination: Option<&PaginationParams>,
) -> Result<PaginationArgs, RpcError> {
    let cursor = pagination.and_then(|p| p.cursor.clone()).or(page_token);
    let size = pagination.and_then(|p| p.page_size).unwrap_or(page_size);
    let validated_size = validate_page_size(size)?;

    let offset = if let Some(token) = cursor {
        decode_cursor(&token).map_err(|e| RpcError::validation(e.to_string()))?
    } else {
        0
    };

    Ok(PaginationArgs {
        offset,
        size: validated_size,
    })
}

fn convert_filters(filters: Option<SearchFiltersParams>) -> SearchFilters {
    let Some(f) = filters else {
        return SearchFilters::default();
    };

    let visibility = f.visibility.map(|v| match v {
        VisibilityParam::Public => Visibility::Public,
        VisibilityParam::Private => Visibility::Private,
    });

    SearchFilters {
        languages: f.language,
        visibility,
        kinds: f.symbol_kind,
        min_score: f.score_min,
    }
}

// ---------------------------------------------------------------------------
// Generic deserialise entrypoint.
// ---------------------------------------------------------------------------

/// Deserialise a JSON-RPC `params` [`Value`] into the schemars-typed
/// `*Params` struct for a given tool. Returns an [`RpcError`] with code
/// `-32602` on structural mismatch, matching the JSON-RPC 2.0 spec.
fn deserialise_params<T>(params: Value) -> Result<T, RpcError>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value::<T>(params).map_err(|e| RpcError::validation(e.to_string()))
}

// ---------------------------------------------------------------------------
// 14 params → Args converters (mirror of server.rs bodies).
// ---------------------------------------------------------------------------

/// `semantic_search`: JSON → [`SemanticSearchArgs`].
pub fn params_to_semantic_search_args(params: Value) -> Result<SemanticSearchArgs, RpcError> {
    let params: SemanticSearchParams = deserialise_params(params)?;
    // Mirror the rmcp-side non-empty `query` guard in `server.rs` so the
    // daemon path returns the same structured `-32602` RpcError on a
    // blank / whitespace-only query instead of bubbling up through
    // `inner::execute_semantic_search` as `-32603`.
    if params.query.trim().is_empty() {
        return Err(RpcError::validation_with_data(
            "query cannot be empty",
            json!({"kind": "validation", "constraint": "non_empty", "field": "query"}),
        ));
    }
    let filters = convert_filters(params.filters);
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;
    let score_min = filters.min_score;
    let max_results = validate_max_results(params.max_results, 10_000)?;
    let context_lines = validate_context_lines(params.context_lines)?;

    Ok(SemanticSearchArgs {
        query: params.query,
        path: params.path,
        filters,
        max_results,
        context_lines,
        pagination,
        score_min,
        include_classpath: params.include_classpath,
        budget_rows: validate_budget_rows(params.budget_rows)?,
    })
}

/// `relation_query`: JSON → [`RelationQueryArgs`].
pub fn params_to_relation_query_args(params: Value) -> Result<RelationQueryArgs, RpcError> {
    let params: RelationQueryParams = deserialise_params(params)?;
    let relation = match params.relation_type {
        RelationTypeParam::Callers => RelationType::Callers,
        RelationTypeParam::Callees => RelationType::Callees,
        RelationTypeParam::Imports => RelationType::Imports,
        RelationTypeParam::Exports => RelationType::Exports,
        RelationTypeParam::Returns => RelationType::Returns,
    };

    let pagination = convert_pagination(params.page_token, params.page_size, None)?;
    let max_depth = validate_max_depth(params.max_depth, 5)?;
    let max_results = validate_max_results(params.max_results, 5_000)?;

    Ok(RelationQueryArgs {
        symbol: params.symbol,
        relation,
        path: params.path,
        max_depth,
        max_results,
        pagination,
    })
}

/// `direct_callers`: JSON → [`DirectCallersArgs`].
pub fn params_to_direct_callers_args(params: Value) -> Result<DirectCallersArgs, RpcError> {
    let params: DirectCallersParams = deserialise_params(params)?;
    // Mirror the rmcp-side `params.validate()` guard (non-empty `symbol`).
    params.validate()?;
    let max_results = validate_max_results(params.max_results, 500)?;
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;

    Ok(DirectCallersArgs {
        symbol: params.symbol,
        path: params.path,
        max_results,
        pagination,
    })
}

/// `direct_callees`: JSON → [`DirectCalleesArgs`].
pub fn params_to_direct_callees_args(params: Value) -> Result<DirectCalleesArgs, RpcError> {
    let params: DirectCalleesParams = deserialise_params(params)?;
    // Mirror the rmcp-side `params.validate()` guard (non-empty `symbol`).
    params.validate()?;
    let max_results = validate_max_results(params.max_results, 500)?;
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;

    Ok(DirectCalleesArgs {
        symbol: params.symbol,
        path: params.path,
        max_results,
        pagination,
    })
}

/// `find_unused`: JSON → [`FindUnusedArgs`].
pub fn params_to_find_unused_args(params: Value) -> Result<FindUnusedArgs, RpcError> {
    let params: FindUnusedParams = deserialise_params(params)?;
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;
    let max_results = validate_max_results(params.max_results, 1_000)?;

    let scope = match params.scope {
        UnusedScopeParam::Public => UnusedScope::Public,
        UnusedScopeParam::Private => UnusedScope::Private,
        UnusedScopeParam::Function => UnusedScope::Function,
        UnusedScopeParam::Struct => UnusedScope::Struct,
        UnusedScopeParam::All => UnusedScope::All,
    };

    Ok(FindUnusedArgs {
        path: params.path,
        scope,
        languages: params.language,
        kinds: params.symbol_kind,
        max_results,
        pagination,
    })
}

/// `find_cycles`: JSON → [`FindCyclesArgs`].
pub fn params_to_find_cycles_args(params: Value) -> Result<FindCyclesArgs, RpcError> {
    let params: FindCyclesParams = deserialise_params(params)?;
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;
    let min_depth = validate_usize(params.min_depth, "min_depth", 2, 100)?;
    let max_depth = params
        .max_depth
        .map(|v| validate_usize(v, "max_depth", 2, 100))
        .transpose()?;
    let max_results = validate_max_results(params.max_results, 500)?;

    if let Some(max) = max_depth
        && max < min_depth
    {
        return Err(RpcError::validation("max_depth must be >= min_depth"));
    }

    let cycle_type = match params.cycle_type {
        CycleTypeParam::Calls => CycleType::Calls,
        CycleTypeParam::Imports => CycleType::Imports,
        CycleTypeParam::Modules => CycleType::Modules,
    };

    Ok(FindCyclesArgs {
        path: params.path,
        cycle_type,
        min_depth,
        max_depth,
        include_self_loops: params.include_self_loops,
        max_results,
        pagination,
    })
}

/// `is_node_in_cycle`: JSON → [`IsNodeInCycleArgs`].
pub fn params_to_is_node_in_cycle_args(params: Value) -> Result<IsNodeInCycleArgs, RpcError> {
    let params: IsNodeInCycleParams = deserialise_params(params)?;
    // Mirror the custom validation on `IsNodeInCycleParams::validate`.
    params.validate()?;

    let cycle_type = match params.cycle_type {
        CycleTypeParam::Calls => CycleType::Calls,
        CycleTypeParam::Imports => CycleType::Imports,
        CycleTypeParam::Modules => CycleType::Modules,
    };

    let file_path = params
        .file_path
        .map(|s| std::path::PathBuf::from(s.replace('\\', "/")));

    Ok(IsNodeInCycleArgs {
        symbol: params.symbol,
        path: params.path,
        cycle_type,
        min_depth: params.min_depth,
        max_depth: params.max_depth,
        include_self_loops: params.include_self_loops,
        file_path,
    })
}

/// `trace_path`: JSON → [`TracePathArgs`].
pub fn params_to_trace_path_args(params: Value) -> Result<TracePathArgs, RpcError> {
    let params: TracePathParams = deserialise_params(params)?;
    let max_hops = validate_max_hops(params.max_hops)?;
    let max_paths = validate_max_paths(params.max_paths)?;

    Ok(TracePathArgs {
        from_symbol: params.from_symbol,
        to_symbol: params.to_symbol,
        path: params.path,
        max_hops,
        max_paths,
        cross_language: params.cross_language,
        min_confidence: params.min_confidence,
    })
}

/// `subgraph`: JSON → [`SubgraphArgs`].
pub fn params_to_subgraph_args(params: Value) -> Result<SubgraphArgs, RpcError> {
    let params: SubgraphParams = deserialise_params(params)?;
    params.validate()?;
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;
    let max_depth = validate_max_depth(params.max_depth, 5)?;
    let max_nodes = validate_max_nodes(params.max_nodes)?;

    Ok(SubgraphArgs {
        symbols: params.symbols,
        path: params.path,
        max_depth,
        max_nodes,
        include_callers: params.include_callers,
        include_callees: params.include_callees,
        include_imports: params.include_imports,
        cross_language: params.cross_language,
        pagination,
    })
}

/// `export_graph`: JSON → [`ExportGraphArgs`].
pub fn params_to_export_graph_args(params: Value) -> Result<ExportGraphArgs, RpcError> {
    let params: ExportGraphParams = deserialise_params(params)?;
    params.validate()?;
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;
    let max_depth = validate_max_depth(params.max_depth, 5)?;
    let max_results = validate_max_results(params.max_results, 5_000)?;

    let mut include_calls = params.include.is_empty();
    let mut include_imports = false;
    let mut include_exports = false;
    let mut include_returns = false;

    for kind in &params.include {
        match kind {
            EdgeKindParam::Calls => include_calls = true,
            EdgeKindParam::Imports => include_imports = true,
            EdgeKindParam::Exports => include_exports = true,
            EdgeKindParam::Returns => include_returns = true,
        }
    }

    let format = match params.format {
        GraphFormatParam::Json => "json",
        GraphFormatParam::Dot => "dot",
        GraphFormatParam::D2 => "d2",
        GraphFormatParam::Mermaid => "mermaid",
    };

    let mut symbols = params.symbols;
    if let Some(ref name) = params.symbol_name
        && !symbols.contains(name)
    {
        symbols.push(name.clone());
    }

    Ok(ExportGraphArgs {
        file_path: params.file_path,
        symbol_name: params.symbol_name,
        symbols,
        path: params.path,
        format: format.to_string(),
        max_depth,
        max_results,
        pagination,
        include_calls,
        include_imports,
        include_exports,
        include_returns,
        languages: params.languages,
        verbose: params.verbose,
    })
}

/// `complexity_metrics`: JSON → [`ComplexityMetricsArgs`].
pub fn params_to_complexity_metrics_args(params: Value) -> Result<ComplexityMetricsArgs, RpcError> {
    let params: ComplexityMetricsParams = deserialise_params(params)?;
    let max_results = validate_max_results(params.max_results, 1000)?;

    Ok(ComplexityMetricsArgs {
        path: params.path,
        target: params.target,
        min_complexity: params.min_complexity,
        sort_by_complexity: params.sort_by_complexity,
        max_results,
    })
}

/// `semantic_diff`: JSON → [`SemanticDiffArgs`].
pub fn params_to_semantic_diff_args(params: Value) -> Result<SemanticDiffArgs, RpcError> {
    let params: SemanticDiffParams = deserialise_params(params)?;
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;
    let max_results = validate_max_results(params.max_results, 5_000)?;

    let filters = params
        .filters
        .map(|f| {
            let change_types = f
                .change_types
                .into_iter()
                .map(|ct| match ct {
                    ChangeTypeParam::Added => ChangeType::Added,
                    ChangeTypeParam::Removed => ChangeType::Removed,
                    ChangeTypeParam::Modified => ChangeType::Modified,
                    ChangeTypeParam::Renamed => ChangeType::Renamed,
                    ChangeTypeParam::SignatureChanged => ChangeType::SignatureChanged,
                })
                .collect();

            SemanticDiffFilters {
                change_types,
                symbol_kinds: f.symbol_kinds,
            }
        })
        .unwrap_or_default();

    Ok(SemanticDiffArgs {
        base: GitVersionRef {
            git_ref: params.base.git_ref,
            file_path: params.base.file_path,
        },
        target: GitVersionRef {
            git_ref: params.target.git_ref,
            file_path: params.target.file_path,
        },
        path: params.path,
        include_unchanged: params.include_unchanged,
        filters,
        max_results,
        pagination,
    })
}

/// `dependency_impact`: JSON → [`DependencyImpactArgs`].
pub fn params_to_dependency_impact_args(params: Value) -> Result<DependencyImpactArgs, RpcError> {
    let params: DependencyImpactParams = deserialise_params(params)?;
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;
    let max_depth = validate_max_depth(params.max_depth, 10)?;
    let max_results = validate_max_results(params.max_results, 5_000)?;

    let file_path = params
        .file_path
        .map(|s| std::path::PathBuf::from(s.replace('\\', "/")));

    Ok(DependencyImpactArgs {
        symbol: params.symbol,
        path: params.path,
        max_depth,
        include_files: params.include_files,
        include_indirect: params.include_indirect,
        max_results,
        pagination,
        file_path,
    })
}

/// `sqry_ask` (NL07): JSON → [`crate::tools::params::SqryAskParams`].
///
/// Unlike the other 14 daemon-routed tools, `sqry_ask` does not have a
/// post-validation `*Args` lowering — the executor consumes
/// [`crate::tools::params::SqryAskParams`] directly. This converter
/// performs the same struct-shape deserialise + bounds checks the
/// rmcp `SqryServer::sqry_ask` path runs before dispatch, returning
/// the canonical `RpcError` on failure.
pub fn params_to_sqry_ask_args(
    params: Value,
) -> Result<crate::tools::params::SqryAskParams, RpcError> {
    let parsed: crate::tools::params::SqryAskParams = deserialise_params(params)?;
    if parsed.query.trim().is_empty() {
        return Err(RpcError::validation(
            "sqry_ask: `query` must not be empty".to_string(),
        ));
    }
    if parsed.query.len() > 4096 {
        return Err(RpcError::validation(format!(
            "sqry_ask: `query` exceeds 4096-character cap (got {} chars)",
            parsed.query.len()
        )));
    }
    Ok(parsed)
}

/// `show_dependencies`: JSON → [`ShowDependenciesArgs`].
pub fn params_to_show_dependencies_args(params: Value) -> Result<ShowDependenciesArgs, RpcError> {
    let params: ShowDependenciesParams = deserialise_params(params)?;
    params.validate()?;
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;
    let max_depth = validate_max_depth(params.max_depth, 5)?;
    let max_results = validate_max_results(params.max_results, 5_000)?;

    Ok(ShowDependenciesArgs {
        file_path: params.file_path,
        symbol_name: params.symbol_name,
        path: params.path,
        max_depth,
        max_results,
        pagination,
    })
}
