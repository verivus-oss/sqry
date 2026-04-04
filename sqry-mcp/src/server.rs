//! MCP Server implementation using rmcp SDK.
//!
//! This module implements the `SqryServer` which uses the rmcp SDK for
//! MCP protocol handling while delegating tool execution to the
//! existing execution module.

use crate::error::RpcError;
use crate::execution::{self, ToolExecution};
use crate::feature_flags::FeatureFlags;
use crate::prompts::create_prompt_router;
use crate::resources;
use crate::tools::params::{
    CallHierarchyDirection as CallHierarchyDirectionParam, CallHierarchyParams, ChangeTypeParam,
    ComplexityMetricsParams, CrossLanguageEdgesParams, CycleTypeParam, DependencyImpactParams,
    DirectCalleesParams, DirectCallersParams, DuplicateTypeParam, EdgeKindParam,
    ExpandCacheStatusParams, ExplainCodeParams, ExportGraphParams, FindCyclesParams,
    FindDuplicatesParams, FindUnusedParams, GetDefinitionParams, GetDocumentSymbolsParams,
    GetGraphStatsParams, GetHoverInfoParams, GetIndexStatusParams, GetInsightsParams,
    GetReferencesParams, GetWorkspaceSymbolsParams, GraphFormatParam, HierarchicalSearchParams,
    IsNodeInCycleParams, ListFilesParams, ListSymbolsParams, PaginationParams, PatternSearchParams,
    RebuildIndexParams, RelationQueryParams, RelationTypeParam, SearchFiltersParams,
    SearchSimilarParams, SemanticDiffParams, SemanticSearchParams, ShowDependenciesParams,
    SqryAskParams, SubgraphParams, TracePathParams, UnusedScopeParam, VisibilityParam,
};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::prompt::PromptContext,
    handler::server::router::prompt::PromptRouter,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, Content, GetPromptRequestParams, GetPromptResult, Implementation,
        ListPromptsResult, ListResourcesResult, PaginatedRequestParams, ProtocolVersion,
        ReadResourceRequestParams, ReadResourceResult, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use serde::Serialize;
use serde_json::json;
use sqry_mcp_redaction::{RedactionConfig, Redactor};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::spawn_blocking;
use tokio::time::timeout;
use tracing::{Instrument, info_span};

/// MCP server for sqry semantic code search.
///
/// Uses rmcp SDK for protocol handling while delegating
/// tool execution to existing execution module.
#[derive(Clone)]
pub struct SqryServer {
    /// Feature flags control tool visibility
    feature_flags: FeatureFlags,
    /// Execution timeout for general tools (default: 60s)
    timeout_ms: u64,
    /// Execution timeout for index rebuild operations (default: 600s = 10 min)
    index_timeout_ms: u64,
    /// Retry delay for deadline exceeded errors (default: 500ms)
    retry_delay_ms: u64,
    /// Tool router for rmcp
    tool_router: ToolRouter<Self>,
    /// Prompt router for MCP prompts (appear as `/mcp__sqry__*` in Claude Code)
    prompt_router: PromptRouter<Self>,
    /// Optional response redactor (None = passthrough, no redaction)
    redactor: Option<Arc<Redactor>>,
}

impl SqryServer {
    /// Create a new `SqryServer` with the given feature flags and default timeout.
    pub fn new(feature_flags: FeatureFlags) -> Self {
        let tool_router = Self::filtered_tool_router(&feature_flags);
        Self {
            feature_flags,
            timeout_ms: 60_000,
            index_timeout_ms: 600_000,
            retry_delay_ms: 500,
            tool_router,
            prompt_router: create_prompt_router(),
            redactor: None,
        }
    }

    /// Create a new `SqryServer` with custom timeout, retry delay, and optional redactor.
    pub fn with_config(
        feature_flags: FeatureFlags,
        timeout_ms: u64,
        index_timeout_ms: u64,
        retry_delay_ms: u64,
        redactor: Option<Arc<Redactor>>,
    ) -> Self {
        let tool_router = Self::filtered_tool_router(&feature_flags);
        Self {
            feature_flags,
            timeout_ms,
            index_timeout_ms,
            retry_delay_ms,
            tool_router,
            prompt_router: create_prompt_router(),
            redactor,
        }
    }

    /// Create a `Redactor` from the given preset name, returning `None` for `"none"` (passthrough).
    ///
    /// Uses `RedactionConfig::from_preset_with_env()` to apply the given preset as base,
    /// then layer fine-grained env var overrides (`SQRY_REDACT_PATHS`, `SQRY_REDACT_CODE`, etc.)
    /// on top. This ensures config-file presets are respected even when `SQRY_REDACTION_PRESET`
    /// is not set in the environment.
    pub fn create_redactor(preset: &str) -> Option<Arc<Redactor>> {
        match preset {
            "none" => return None,
            "minimal" | "standard" | "strict" => {}
            other => {
                tracing::warn!("Unknown redaction preset '{other}', disabling redaction");
                return None;
            }
        }

        // Build config from the caller-supplied preset + fine-grained env overrides
        let config = RedactionConfig::from_preset_with_env(preset);
        match Redactor::new(config) {
            Ok(redactor) => Some(Arc::new(redactor)),
            Err(e) => {
                tracing::warn!("Failed to create redactor, disabling redaction: {e}");
                None
            }
        }
    }

    /// Build a tool router filtered by the active feature flags.
    fn filtered_tool_router(feature_flags: &FeatureFlags) -> ToolRouter<Self> {
        let mut router = Self::tool_router();
        let tool_names: Vec<String> = router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.as_ref().to_string())
            .collect();

        for tool_name in tool_names {
            if !feature_flags.is_tool_enabled(&tool_name) {
                router.remove_route(&tool_name);
            }
        }

        router
    }

    /// Get filtered list of tools based on feature flags.
    pub fn get_filtered_tools(&self) -> Vec<Tool> {
        self.tool_router.list_all()
    }

    /// Check if a tool is enabled by feature flags.
    fn is_tool_enabled(&self, tool_name: &str) -> bool {
        self.feature_flags.is_tool_enabled(tool_name)
    }

    /// Ensure a tool is enabled or return a user-friendly error.
    fn ensure_tool_enabled(&self, tool_name: &str) -> Result<(), McpError> {
        if self.is_tool_enabled(tool_name) {
            return Ok(());
        }

        let reason = self
            .feature_flags
            .disabled_reason(tool_name)
            .unwrap_or_else(|| "Tool is disabled".to_string());
        Err(McpError::invalid_request(reason, None))
    }

    /// Execute a tool function with the default timeout and tracing.
    /// Returns the full `ToolExecution` response including metadata (`execution_ms`, pagination, etc.)
    async fn execute_tool<F, T>(&self, tool_name: &str, f: F) -> Result<serde_json::Value, McpError>
    where
        F: FnOnce() -> anyhow::Result<ToolExecution<T>> + Send + 'static,
        T: Serialize + Send + 'static,
    {
        self.execute_tool_with_timeout(tool_name, self.timeout_ms, f)
            .await
    }

    /// Execute a tool function with a custom timeout.
    ///
    /// Used for long-running operations (e.g., `rebuild_index`) that need a
    /// longer timeout than the general tool default.
    async fn execute_tool_with_timeout<F, T>(
        &self,
        tool_name: &str,
        timeout_ms: u64,
        f: F,
    ) -> Result<serde_json::Value, McpError>
    where
        F: FnOnce() -> anyhow::Result<ToolExecution<T>> + Send + 'static,
        T: Serialize + Send + 'static,
    {
        let span = info_span!("tool_execution", tool = tool_name);
        let timeout_duration = Duration::from_millis(timeout_ms);
        let tool_name_owned = tool_name.to_string();
        let retry_delay_ms = self.retry_delay_ms;
        let redactor_clone = self.redactor.clone();

        async move {
            let result = timeout(timeout_duration, spawn_blocking(f)).await;

            // Helper: redact an error message string if redactor is active
            let redact_error = |msg: String| -> String {
                if let Some(ref redactor) = redactor_clone {
                    let mut val = serde_json::Value::String(msg);
                    redactor.redact(&mut val);
                    match val {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    }
                } else {
                    msg
                }
            };

            match result {
                Ok(Ok(Ok(execution))) => {
                    // Build response matching native implementation's build_success()
                    // Preserves all metadata: execution_ms, pagination, used_index, etc.
                    let mut response = Self::build_response(execution)?;

                    // Apply redaction if configured (none preset = no redactor = skip)
                    if let Some(ref redactor) = redactor_clone {
                        redactor.redact(&mut response);
                    }

                    Ok(response)
                }
                Ok(Ok(Err(anyhow_err))) => Err(McpError::internal_error(
                    redact_error(anyhow_err.to_string()),
                    None,
                )),
                Ok(Err(join_err)) => Err(McpError::internal_error(
                    redact_error(format!("Task panicked: {join_err}")),
                    None,
                )),
                Err(_) => Err(rpc_error_to_mcp(RpcError::deadline_exceeded(
                    &tool_name_owned,
                    timeout_ms,
                    retry_delay_ms,
                ))),
            }
        }
        .instrument(span)
        .await
    }

    /// Build a response JSON object from `ToolExecution`, preserving all metadata.
    fn build_response<T: Serialize>(
        execution: ToolExecution<T>,
    ) -> Result<serde_json::Value, McpError> {
        let mut response = serde_json::Map::new();

        // Protocol version
        response.insert("version".to_string(), json!("2024-11-05"));

        // Serialize the main data
        let data = serde_json::to_value(&execution.data).map_err(|e| {
            McpError::internal_error(format!("Failed to serialize result: {e}"), None)
        })?;
        response.insert("data".to_string(), data);

        // Execution metadata
        response.insert("execution_ms".to_string(), json!(execution.execution_ms));

        if execution.used_index {
            response.insert("used_index".to_string(), json!(true));
        }

        if execution.used_graph {
            response.insert("used_graph".to_string(), json!(true));
        }

        if let Some(metadata) = execution.graph_metadata {
            let metadata_value = serde_json::to_value(&metadata).map_err(|e| {
                McpError::internal_error(format!("Failed to serialize graph_metadata: {e}"), None)
            })?;
            response.insert("graph_metadata".to_string(), metadata_value);
        }

        // Pagination metadata
        if let Some(token) = execution.next_page_token {
            response.insert("next_page_token".to_string(), json!(token));
        }

        if let Some(total) = execution.total {
            response.insert("total".to_string(), json!(total));
        }

        if let Some(truncated) = execution.truncated {
            response.insert("truncated".to_string(), json!(truncated));
        }

        if let Some(scanned) = execution.candidates_scanned {
            response.insert("candidates_scanned".to_string(), json!(scanned));
        }

        // Workspace path
        if !execution.workspace_path.is_empty() {
            response.insert(
                "workspace_path".to_string(),
                json!(execution.workspace_path),
            );
        }

        Ok(serde_json::Value::Object(response))
    }

    /// Build a successful `CallToolResult` from JSON value.
    fn success_result(value: &serde_json::Value) -> CallToolResult {
        CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
        )])
    }
}

/// Tool implementations using rmcp macros.
#[tool_router]
impl SqryServer {
    /// Search symbols by name, kind, visibility, and language.
    #[tool(
        description = "Search symbols by name, kind, visibility, and language",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn semantic_search(
        &self,
        Parameters(params): Parameters<SemanticSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        // Feature flag guard (explicit, defense in depth)
        self.ensure_tool_enabled("semantic_search")?;

        // Non-empty query validation
        if params.query.trim().is_empty() {
            return Err(rpc_error_to_mcp(RpcError::validation_with_data(
                "query cannot be empty",
                json!({"kind": "validation", "constraint": "non_empty", "field": "query"}),
            )));
        }

        let args = convert_semantic_search_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("semantic_search", move || {
                execution::execute_semantic_search(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Search symbols with results grouped by file and container for RAG.
    #[tool(
        description = "Search symbols with results grouped by file and container for RAG",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn hierarchical_search(
        &self,
        Parameters(params): Parameters<HierarchicalSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("hierarchical_search")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_hierarchical_search_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("hierarchical_search", move || {
                execution::execute_hierarchical_search(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Query callers, callees, imports, exports, or returns for a symbol.
    #[tool(
        description = "Query callers, callees, imports, exports, or returns for a symbol",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn relation_query(
        &self,
        Parameters(params): Parameters<RelationQueryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("relation_query")?;

        let args = convert_relation_query_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("relation_query", move || {
                execution::execute_relation_query(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Get call hierarchy as a tree (incoming or outgoing).
    #[tool(
        description = "Get call hierarchy as a tree (incoming or outgoing)",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn call_hierarchy(
        &self,
        Parameters(params): Parameters<CallHierarchyParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("call_hierarchy")?;

        let args = convert_call_hierarchy_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("call_hierarchy", move || {
                execution::execute_call_hierarchy(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Explain a symbol with optional context and relations.
    #[tool(
        description = "Explain a symbol with optional context and relations",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn explain_code(
        &self,
        Parameters(params): Parameters<ExplainCodeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("explain_code")?;

        let args = convert_explain_code_params(params);
        let result = self
            .execute_tool("explain_code", move || {
                execution::execute_explain_code(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Find symbols similar to a reference symbol using fuzzy matching.
    #[tool(
        description = "Find symbols similar to a reference symbol using fuzzy matching",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn search_similar(
        &self,
        Parameters(params): Parameters<SearchSimilarParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("search_similar")?;

        let args = convert_search_similar_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("search_similar", move || {
                execution::execute_find_similar(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Show dependency tree for a file or symbol.
    #[tool(
        description = "Show dependency tree for a file or symbol",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn show_dependencies(
        &self,
        Parameters(params): Parameters<ShowDependenciesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("show_dependencies")?;

        // Custom XOR validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_show_dependencies_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("show_dependencies", move || {
                execution::execute_get_dependencies(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Get the current status and metadata of the symbol index.
    #[tool(
        description = "Get the current status and metadata of the symbol index",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_index_status(
        &self,
        Parameters(params): Parameters<GetIndexStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_index_status")?;

        let args = convert_get_index_status_params(params);
        let result = self
            .execute_tool("get_index_status", move || {
                execution::execute_index_status(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Rebuild the code graph index from source files.
    #[tool(
        description = "Rebuild the code graph index from source files",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn rebuild_index(
        &self,
        Parameters(params): Parameters<RebuildIndexParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("rebuild_index")?;

        let args = convert_rebuild_index_params(params);
        let result = self
            .execute_tool_with_timeout("rebuild_index", self.index_timeout_ms, move || {
                execution::execute_rebuild_index(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Export a dependency subgraph as JSON, DOT, D2, or Mermaid.
    #[tool(
        description = "Export a dependency subgraph as JSON, DOT, D2, or Mermaid",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn export_graph(
        &self,
        Parameters(params): Parameters<ExportGraphParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("export_graph")?;

        // Custom XOR validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_export_graph_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("export_graph", move || {
                execution::execute_export_graph(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// List cross-language call edges where caller/callee languages differ.
    #[tool(
        description = "List cross-language call edges where caller/callee languages differ",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn cross_language_edges(
        &self,
        Parameters(params): Parameters<CrossLanguageEdgesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("cross_language_edges")?;

        let args = convert_cross_language_edges_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("cross_language_edges", move || {
                execution::execute_cross_language_edges(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Find ranked call paths between two symbols with cross-language support.
    #[tool(
        description = "Find ranked call paths between two symbols with cross-language support",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn trace_path(
        &self,
        Parameters(params): Parameters<TracePathParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("trace_path")?;

        let args = convert_trace_path_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("trace_path", move || execution::execute_trace_path(&args))
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Extract a focused subgraph around seed symbols for RAG retrieval.
    #[tool(
        description = "Extract a focused subgraph around seed symbols for RAG retrieval",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn subgraph(
        &self,
        Parameters(params): Parameters<SubgraphParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("subgraph")?;

        // Custom non-empty validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_subgraph_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("subgraph", move || execution::execute_subgraph(&args))
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Analyze what would break if a symbol is changed or removed.
    #[tool(
        description = "Analyze what would break if a symbol is changed or removed (reverse dependency analysis)",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn dependency_impact(
        &self,
        Parameters(params): Parameters<DependencyImpactParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("dependency_impact")?;

        let args = convert_dependency_impact_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("dependency_impact", move || {
                execution::execute_dependency_impact(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Compare symbol-level changes between git refs.
    #[tool(
        description = "Compare symbol-level changes between git refs",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn semantic_diff(
        &self,
        Parameters(params): Parameters<SemanticDiffParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("semantic_diff")?;

        let args = convert_semantic_diff_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("semantic_diff", move || {
                execution::execute_semantic_diff(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Find duplicate functions, signatures, or structs.
    #[tool(
        description = "Find duplicate functions, signatures, or structs",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn find_duplicates(
        &self,
        Parameters(params): Parameters<FindDuplicatesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("find_duplicates")?;

        let args = convert_find_duplicates_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("find_duplicates", move || {
                execution::execute_find_duplicates(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Find circular dependencies in calls, imports, or modules.
    #[tool(
        description = "Find circular dependencies in calls, imports, or modules",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn find_cycles(
        &self,
        Parameters(params): Parameters<FindCyclesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("find_cycles")?;

        let args = convert_find_cycles_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("find_cycles", move || execution::execute_find_cycles(&args))
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Find unreachable or unused symbols.
    #[tool(
        description = "Find unreachable or unused symbols",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn find_unused(
        &self,
        Parameters(params): Parameters<FindUnusedParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("find_unused")?;

        let args = convert_find_unused_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("find_unused", move || execution::execute_find_unused(&args))
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Translate natural language into sqry query commands.
    #[tool(
        description = "Translate natural language into sqry query commands",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn sqry_ask(
        &self,
        Parameters(params): Parameters<SqryAskParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("sqry_ask")?;

        // SqryAskParams maps directly to the execution args
        let result = self
            .execute_tool("sqry_ask", move || execution::execute_sqry_ask(&params))
            .await?;

        Ok(Self::success_result(&result))
    }

    // ========================================================================
    // New Graph-Based Tools
    // ========================================================================

    /// Check if a specific symbol participates in a cycle.
    #[tool(
        description = "Check if a specific symbol participates in a cycle",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn is_node_in_cycle(
        &self,
        Parameters(params): Parameters<IsNodeInCycleParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("is_node_in_cycle")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_is_node_in_cycle_params(params);
        let result = self
            .execute_tool("is_node_in_cycle", move || {
                execution::execute_is_node_in_cycle(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Find symbols by substring match on name.
    #[tool(
        description = "Find symbols by substring match on name",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn pattern_search(
        &self,
        Parameters(params): Parameters<PatternSearchParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("pattern_search")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_pattern_search_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("pattern_search", move || {
                execution::execute_pattern_search(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Find immediate callers of a symbol (depth=1).
    #[tool(
        description = "Find immediate callers of a symbol (depth=1)",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn direct_callers(
        &self,
        Parameters(params): Parameters<DirectCallersParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("direct_callers")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_direct_callers_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("direct_callers", move || {
                execution::execute_direct_callers(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Find immediate callees of a symbol (depth=1).
    #[tool(
        description = "Find immediate callees of a symbol (depth=1)",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn direct_callees(
        &self,
        Parameters(params): Parameters<DirectCalleesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("direct_callees")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_direct_callees_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("direct_callees", move || {
                execution::execute_direct_callees(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    // ========================================================================
    // Introspection Tools
    // ========================================================================

    /// List indexed files, optionally filtered by language.
    #[tool(
        description = "List indexed files, optionally filtered by language",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_files(
        &self,
        Parameters(params): Parameters<ListFilesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("list_files")?;

        let args = convert_list_files_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("list_files", move || execution::execute_list_files(&args))
            .await?;

        Ok(Self::success_result(&result))
    }

    /// List indexed symbols, filterable by kind and language.
    #[tool(
        description = "List indexed symbols, filterable by kind and language",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_symbols(
        &self,
        Parameters(params): Parameters<ListSymbolsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("list_symbols")?;

        let args = convert_list_symbols_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("list_symbols", move || {
                execution::execute_list_symbols(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Get node, edge, file counts and language breakdown.
    #[tool(
        description = "Get node, edge, file counts and language breakdown",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_graph_stats(
        &self,
        Parameters(params): Parameters<GetGraphStatsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_graph_stats")?;

        let args = convert_get_graph_stats_params(params);
        let result = self
            .execute_tool("get_graph_stats", move || {
                execution::execute_get_graph_stats(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Get codebase health metrics including cycle and quality indicators.
    #[tool(
        description = "Get codebase health metrics including cycle and quality indicators",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_insights(
        &self,
        Parameters(params): Parameters<GetInsightsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_insights")?;

        let args = convert_get_insights_params(params);
        let result = self
            .execute_tool("get_insights", move || {
                execution::execute_get_insights(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Estimate function complexity from call graph and line count.
    #[tool(
        description = "Estimate function complexity from call graph and line count",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn complexity_metrics(
        &self,
        Parameters(params): Parameters<ComplexityMetricsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("complexity_metrics")?;

        let args = convert_complexity_metrics_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("complexity_metrics", move || {
                execution::execute_complexity_metrics(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Get the status of the macro expansion cache.
    #[tool(
        description = "Get the status of the macro expansion cache (.sqry/expand-cache/)",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn expand_cache_status(
        &self,
        Parameters(params): Parameters<ExpandCacheStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("expand_cache_status")?;

        let args = convert_expand_cache_status_params(params);
        let result = self
            .execute_tool("expand_cache_status", move || {
                execution::execute_expand_cache_status(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    // ========================================================================
    // Navigation Tools
    // ========================================================================

    /// Find where a symbol is defined.
    #[tool(
        description = "Find where a symbol is defined",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_definition(
        &self,
        Parameters(params): Parameters<GetDefinitionParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_definition")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_get_definition_params(params);
        let result = self
            .execute_tool("get_definition", move || {
                execution::execute_get_definition(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Find all references to a symbol.
    #[tool(
        description = "Find all references to a symbol",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_references(
        &self,
        Parameters(params): Parameters<GetReferencesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_references")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_get_references_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("get_references", move || {
                execution::execute_get_references(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Get symbol signature, documentation, and type info.
    #[tool(
        description = "Get symbol signature, documentation, and type info",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_hover_info(
        &self,
        Parameters(params): Parameters<GetHoverInfoParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_hover_info")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_get_hover_info_params(params);
        let result = self
            .execute_tool("get_hover_info", move || {
                execution::execute_get_hover_info(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Get all symbols in a document.
    #[tool(
        description = "Get all symbols (functions, classes, etc.) defined in a specific file.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_document_symbols(
        &self,
        Parameters(params): Parameters<GetDocumentSymbolsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_document_symbols")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_get_document_symbols_params(params);
        let result = self
            .execute_tool("get_document_symbols", move || {
                execution::execute_get_document_symbols(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Search symbols by name across the workspace.
    #[tool(
        description = "Search symbols by name across the workspace",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_workspace_symbols(
        &self,
        Parameters(params): Parameters<GetWorkspaceSymbolsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_workspace_symbols")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_get_workspace_symbols_params(params).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool("get_workspace_symbols", move || {
                execution::execute_get_workspace_symbols(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }
}

/// Implement `ServerHandler` for rmcp protocol handling.
#[tool_handler]
impl ServerHandler for SqryServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
            server_info: Implementation {
                name: "sqry-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                title: None,
                description: None,
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "MCP server for sqry AST-based semantic code search. \
                 Unlike embedding-based search that treats code as text, \
                 sqry parses code like a compiler to understand structure \
                 (functions, classes, types) and relationships (calls, imports, inheritance).\n\n\
                 Tool selection guide:\n\
                 - Search by name/kind/visibility: semantic_search, pattern_search, get_workspace_symbols\n\
                 - Search with RAG grouping: hierarchical_search\n\
                 - Navigate to definition/references: get_definition, get_references, get_hover_info\n\
                 - Trace relationships: relation_query, direct_callers, direct_callees, call_hierarchy\n\
                 - Trace call paths: trace_path\n\
                 - Analyze impact: dependency_impact, show_dependencies, subgraph\n\
                 - Code quality: find_cycles, find_unused, find_duplicates, is_node_in_cycle, complexity_metrics\n\
                 - Compare versions: semantic_diff\n\
                 - Inspect index: get_index_status, get_graph_stats, get_insights, list_files, list_symbols\n\
                 - Macro expansion: expand_cache_status\n\
                 - File symbols: get_document_symbols\n\
                 - Export/visualize: export_graph\n\
                 - Cross-language: cross_language_edges\n\
                 - Natural language: sqry_ask\n\
                 - Find similar: search_similar\n\
                 - Explain symbol context: explain_code\n\n\
                 Detailed docs available as resources: \
                 sqry://docs/tool-guide, sqry://docs/query-syntax, \
                 sqry://docs/patterns, sqry://docs/architecture"
                    .into(),
            ),
        }
    }

    /// List available prompts - these appear as `/mcp__sqry__*` commands in Claude Code.
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let prompts = self.prompt_router.list_all();
        Ok(ListPromptsResult {
            prompts,
            ..Default::default()
        })
    }

    /// Get a specific prompt by name.
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        let prompt_context = PromptContext::new(self, request.name, request.arguments, context);
        self.prompt_router.get_prompt(prompt_context).await
    }

    /// List available documentation resources.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: resources::list_resources(),
            ..Default::default()
        })
    }

    /// Read a documentation resource by URI.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        match resources::read_resource(&request.uri) {
            Some(contents) => Ok(ReadResourceResult {
                contents: vec![contents],
            }),
            None => Err(McpError::resource_not_found(
                format!("unknown resource: {}", request.uri),
                None,
            )),
        }
    }
}

// ============================================================================
// Error Bridge: RpcError -> McpError
// ============================================================================

/// Convert `RpcError` to `McpError`, preserving error details.
fn rpc_error_to_mcp(err: RpcError) -> McpError {
    let data = serde_json::json!({
        "kind": err.kind,
        "retryable": err.retryable,
        "retry_after_ms": err.retry_after_ms,
        "details": err.details,
    });

    // Map error codes: -32602 = invalid params, everything else = internal error
    match err.code {
        -32602 => McpError::invalid_params(err.message, Some(data)),
        _ => McpError::internal_error(err.message, Some(data)),
    }
}

// ============================================================================
// Parameter Conversion Functions
// ============================================================================

use crate::pagination::decode_cursor;
use crate::tools::{
    ChangeType, ComplexityMetricsArgs, CrossLanguageEdgesArgs, CycleType, DependencyImpactArgs,
    DuplicateType, ExplainCodeArgs, ExportGraphArgs, FindCyclesArgs, FindDuplicatesArgs,
    FindUnusedArgs, GetDefinitionArgs, GetDocumentSymbolsArgs, GetGraphStatsArgs, GetHoverInfoArgs,
    GetIndexStatusArgs, GetInsightsArgs, GetReferencesArgs, GetWorkspaceSymbolsArgs, GitVersionRef,
    HierarchicalSearchArgs, ListFilesArgs, ListSymbolsArgs, PaginationArgs, RelationQueryArgs,
    RelationType, SearchFilters, SearchSimilarArgs, SemanticDiffArgs, SemanticDiffFilters,
    SemanticSearchArgs, ShowDependenciesArgs, SubgraphArgs, TracePathArgs, UnusedScope, Visibility,
};

// ============================================================================
// Validation Helpers - Parity with tools::validation
// ============================================================================

/// Validate and convert i64 to usize with bounds check.
/// Matches validation bounds from tools/validation.rs.
fn validate_usize(value: i64, field: &str, min: i64, max: i64) -> Result<usize, RpcError> {
    if !(min..=max).contains(&value) {
        return Err(RpcError::validation_with_data(
            format!("{field} must be between {min} and {max}"),
            json!({
                "kind": "validation",
                "constraint": "range",
                "field": field,
                "min": min,
                "max": max,
                "actual": value
            }),
        ));
    }
    value.try_into().map_err(|_| {
        RpcError::validation_with_data(
            format!("{field} out of range for platform"),
            json!({"kind": "validation", "field": field, "actual": value}),
        )
    })
}

/// Validate `max_results` (1..=10,000 for search, 1..=5,000 for relations).
fn validate_max_results(value: i64, max_limit: i64) -> Result<usize, RpcError> {
    validate_usize(value, "max_results", 1, max_limit)
}

/// Validate `context_lines` (0..=20).
fn validate_context_lines(value: i64) -> Result<usize, RpcError> {
    validate_usize(value, "context_lines", 0, 20)
}

/// Validate `max_depth` (1..=5 for relations, 1..=10 for dependencies).
fn validate_max_depth(value: i64, max_limit: i64) -> Result<usize, RpcError> {
    validate_usize(value, "max_depth", 1, max_limit)
}

/// Validate `page_size` (1..=500).
fn validate_page_size(value: i64) -> Result<usize, RpcError> {
    validate_usize(value, "page_size", 1, 500)
}

/// Validate `max_nodes` (1..=500).
fn validate_max_nodes(value: i64) -> Result<usize, RpcError> {
    validate_usize(value, "max_nodes", 1, 500)
}

/// Validate `max_hops` (1..=10).
fn validate_max_hops(value: i64) -> Result<usize, RpcError> {
    validate_usize(value, "max_hops", 1, 10)
}

/// Validate `max_paths` (1..=20).
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

    // Validate page_size bounds (1..=500)
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

    SearchFilters {
        languages: f.language,
        visibility: f.visibility.map(|v| match v {
            VisibilityParam::Public => Visibility::Public,
            VisibilityParam::Private => Visibility::Private,
        }),
        kinds: f.symbol_kind,
        min_score: f.score_min,
    }
}

fn convert_semantic_search_params(
    params: SemanticSearchParams,
) -> Result<SemanticSearchArgs, RpcError> {
    let filters = convert_filters(params.filters);
    // Use default page_size of 50 (from validation.rs parse_pagination)
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;
    let score_min = filters.min_score;

    // Validate with bounds from validation.rs
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
    })
}

fn convert_hierarchical_search_params(
    params: HierarchicalSearchParams,
) -> Result<HierarchicalSearchArgs, RpcError> {
    let filters = convert_filters(params.filters);
    let pagination = convert_pagination(None, 20, params.pagination.as_ref())?;
    let score_min = filters.min_score;

    // Validate all numeric bounds
    let max_results = validate_max_results(params.max_results, 10_000)?;
    let context_lines = validate_context_lines(params.context_lines)?;
    let merge_threshold = validate_usize(params.merge_threshold, "merge_threshold", 0, 1000)?;
    let max_files = validate_usize(params.max_files, "max_files", 1, 100)?;
    let max_containers_per_file = validate_usize(
        params.max_containers_per_file,
        "max_containers_per_file",
        1,
        500,
    )?;
    let max_symbols_per_container = validate_usize(
        params.max_symbols_per_container,
        "max_symbols_per_container",
        1,
        500,
    )?;
    let max_total_symbols =
        validate_usize(params.max_total_symbols, "max_total_symbols", 1, 10_000)?;

    // Token targets are u64, validate they're non-negative and convert
    let file_target_tokens = u64::try_from(params.file_target_tokens)
        .map_err(|_| RpcError::validation("file_target_tokens must be non-negative"))?;
    let container_target_tokens = u64::try_from(params.container_target_tokens)
        .map_err(|_| RpcError::validation("container_target_tokens must be non-negative"))?;
    let symbol_target_tokens = u64::try_from(params.symbol_target_tokens)
        .map_err(|_| RpcError::validation("symbol_target_tokens must be non-negative"))?;
    let context_cluster_target_tokens = u64::try_from(params.context_cluster_target_tokens)
        .map_err(|_| RpcError::validation("context_cluster_target_tokens must be non-negative"))?;

    Ok(HierarchicalSearchArgs {
        query: params.query,
        path: params.path,
        filters,
        max_results,
        context_lines,
        pagination,
        score_min,
        auto_merge: params.auto_merge,
        merge_threshold,
        max_files,
        max_containers_per_file,
        max_symbols_per_container,
        max_total_symbols,
        file_target_tokens,
        container_target_tokens,
        symbol_target_tokens,
        context_cluster_target_tokens,
        include_file_context: params.include_file_context,
        include_container_context: params.include_container_context,
        expand_files: params.expand_files,
    })
}

fn convert_relation_query_params(
    params: RelationQueryParams,
) -> Result<RelationQueryArgs, RpcError> {
    let relation = match params.relation_type {
        RelationTypeParam::Callers => RelationType::Callers,
        RelationTypeParam::Callees => RelationType::Callees,
        RelationTypeParam::Imports => RelationType::Imports,
        RelationTypeParam::Exports => RelationType::Exports,
        RelationTypeParam::Returns => RelationType::Returns,
    };

    let pagination = convert_pagination(params.page_token, params.page_size, None)?;

    // Validate with bounds from validation.rs
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

fn convert_call_hierarchy_params(
    params: CallHierarchyParams,
) -> Result<crate::tools::CallHierarchyArgs, RpcError> {
    use crate::tools::{CallHierarchyArgs, CallHierarchyDirection};

    let direction = match params.direction {
        CallHierarchyDirectionParam::Incoming => CallHierarchyDirection::Incoming,
        CallHierarchyDirectionParam::Outgoing => CallHierarchyDirection::Outgoing,
    };

    let pagination = convert_pagination(params.page_token, params.page_size, None)?;
    let max_depth = validate_max_depth(params.max_depth, 5)?;
    let max_results = validate_max_results(params.max_results, 5_000)?;

    Ok(CallHierarchyArgs {
        symbol: params.symbol,
        file_path: params.file_path,
        direction,
        path: params.path,
        max_depth,
        max_results,
        pagination,
    })
}

fn convert_explain_code_params(params: ExplainCodeParams) -> ExplainCodeArgs {
    ExplainCodeArgs {
        file_path: params.file_path,
        symbol_name: params.symbol_name,
        path: params.path,
        include_context: params.include_context,
        include_relations: params.include_relations,
    }
}

fn convert_search_similar_params(
    params: SearchSimilarParams,
) -> Result<SearchSimilarArgs, RpcError> {
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;

    // Validate bounds
    let max_results = validate_max_results(params.max_results, 200)?;

    Ok(SearchSimilarArgs {
        path: params.path,
        file_path: params.reference.file_path,
        symbol_name: params.reference.symbol_name,
        similarity_threshold: params.similarity_threshold,
        max_results,
        pagination,
    })
}

fn convert_show_dependencies_params(
    params: ShowDependenciesParams,
) -> Result<ShowDependenciesArgs, RpcError> {
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;

    // Validate bounds (dependencies uses max_depth 1..=5)
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

fn convert_get_index_status_params(params: GetIndexStatusParams) -> GetIndexStatusArgs {
    GetIndexStatusArgs { path: params.path }
}

fn convert_rebuild_index_params(params: RebuildIndexParams) -> crate::tools::RebuildIndexArgs {
    crate::tools::RebuildIndexArgs {
        path: params.path,
        force: params.force,
    }
}

fn convert_export_graph_params(params: ExportGraphParams) -> Result<ExportGraphArgs, RpcError> {
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;

    // Validate bounds
    let max_depth = validate_max_depth(params.max_depth, 5)?;
    let max_results = validate_max_results(params.max_results, 5_000)?;

    // Parse include edge kinds
    let mut include_calls = params.include.is_empty(); // Default to calls if nothing specified
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

fn convert_cross_language_edges_params(
    params: CrossLanguageEdgesParams,
) -> Result<CrossLanguageEdgesArgs, RpcError> {
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;

    // Validate bounds
    let max_results = validate_max_results(params.max_results, 5_000)?;

    Ok(CrossLanguageEdgesArgs {
        path: params.path,
        from_lang: params.from_lang,
        to_lang: params.to_lang,
        max_results,
        pagination,
    })
}

fn convert_trace_path_params(params: TracePathParams) -> Result<TracePathArgs, RpcError> {
    // Validate bounds
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

fn convert_subgraph_params(params: SubgraphParams) -> Result<SubgraphArgs, RpcError> {
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;

    // Validate bounds
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

fn convert_dependency_impact_params(
    params: DependencyImpactParams,
) -> Result<DependencyImpactArgs, RpcError> {
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;

    // Validate bounds (dependency_impact uses max_depth 1..=10)
    let max_depth = validate_max_depth(params.max_depth, 10)?;
    let max_results = validate_max_results(params.max_results, 5_000)?;

    Ok(DependencyImpactArgs {
        symbol: params.symbol,
        path: params.path,
        max_depth,
        include_files: params.include_files,
        include_indirect: params.include_indirect,
        max_results,
        pagination,
    })
}

fn convert_semantic_diff_params(params: SemanticDiffParams) -> Result<SemanticDiffArgs, RpcError> {
    let pagination = convert_pagination(params.page_token, params.page_size, None)?;

    // Validate bounds
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

fn convert_find_duplicates_params(
    params: FindDuplicatesParams,
) -> Result<FindDuplicatesArgs, RpcError> {
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;

    // Validate bounds
    let threshold = u32::try_from(validate_usize(params.threshold, "threshold", 0, 100)?)
        .map_err(|_| RpcError::validation("threshold must fit in u32"))?;
    let max_results = validate_max_results(params.max_results, 1_000)?;

    let duplicate_type = match params.duplicate_type {
        DuplicateTypeParam::Body => DuplicateType::Body,
        DuplicateTypeParam::Signature => DuplicateType::Signature,
        DuplicateTypeParam::Struct => DuplicateType::Struct,
    };

    Ok(FindDuplicatesArgs {
        path: params.path,
        duplicate_type,
        threshold,
        exact: params.exact,
        max_results,
        pagination,
    })
}

fn convert_find_cycles_params(params: FindCyclesParams) -> Result<FindCyclesArgs, RpcError> {
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;

    // Validate bounds
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

fn convert_find_unused_params(params: FindUnusedParams) -> Result<FindUnusedArgs, RpcError> {
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;

    // Validate bounds
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

// ============================================================================
// New Graph-Based Tool Parameter Conversion
// ============================================================================

use crate::tools::{DirectCalleesArgs, DirectCallersArgs, IsNodeInCycleArgs, PatternSearchArgs};

fn convert_is_node_in_cycle_params(params: IsNodeInCycleParams) -> IsNodeInCycleArgs {
    let cycle_type = match params.cycle_type {
        CycleTypeParam::Calls => CycleType::Calls,
        CycleTypeParam::Imports => CycleType::Imports,
        CycleTypeParam::Modules => CycleType::Modules,
    };

    IsNodeInCycleArgs {
        symbol: params.symbol,
        path: params.path,
        cycle_type,
        min_depth: params.min_depth,
        max_depth: params.max_depth,
        include_self_loops: params.include_self_loops,
    }
}

fn convert_pattern_search_params(
    params: PatternSearchParams,
) -> Result<PatternSearchArgs, RpcError> {
    let max_results = validate_max_results(params.max_results, 1000)?;
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;

    Ok(PatternSearchArgs {
        pattern: params.pattern,
        path: params.path,
        max_results,
        pagination,
        include_classpath: params.include_classpath,
    })
}

fn convert_direct_callers_params(
    params: DirectCallersParams,
) -> Result<DirectCallersArgs, RpcError> {
    let max_results = validate_max_results(params.max_results, 500)?;
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;

    Ok(DirectCallersArgs {
        symbol: params.symbol,
        path: params.path,
        max_results,
        pagination,
    })
}

fn convert_direct_callees_params(
    params: DirectCalleesParams,
) -> Result<DirectCalleesArgs, RpcError> {
    let max_results = validate_max_results(params.max_results, 500)?;
    let pagination = convert_pagination(None, 50, params.pagination.as_ref())?;

    Ok(DirectCalleesArgs {
        symbol: params.symbol,
        path: params.path,
        max_results,
        pagination,
    })
}

// ============================================================================
// Introspection Tool Conversion Functions
// ============================================================================

fn convert_list_files_params(params: ListFilesParams) -> Result<ListFilesArgs, RpcError> {
    let max_results = validate_max_results(params.max_results, 10000)?;
    let pagination = convert_pagination(None, 500, params.pagination.as_ref())?;

    Ok(ListFilesArgs {
        path: params.path,
        language: params.language,
        max_results,
        pagination,
    })
}

fn convert_list_symbols_params(params: ListSymbolsParams) -> Result<ListSymbolsArgs, RpcError> {
    let max_results = validate_max_results(params.max_results, 10000)?;
    let pagination = convert_pagination(None, 500, params.pagination.as_ref())?;

    Ok(ListSymbolsArgs {
        path: params.path,
        kind: params.kind,
        language: params.language,
        max_results,
        pagination,
    })
}

fn convert_get_graph_stats_params(params: GetGraphStatsParams) -> GetGraphStatsArgs {
    GetGraphStatsArgs { path: params.path }
}

fn convert_get_insights_params(params: GetInsightsParams) -> GetInsightsArgs {
    GetInsightsArgs { path: params.path }
}

fn convert_expand_cache_status_params(
    params: ExpandCacheStatusParams,
) -> crate::tools::ExpandCacheStatusArgs {
    crate::tools::ExpandCacheStatusArgs { path: params.path }
}

fn convert_complexity_metrics_params(
    params: ComplexityMetricsParams,
) -> Result<ComplexityMetricsArgs, RpcError> {
    let max_results = validate_max_results(params.max_results, 1000)?;

    Ok(ComplexityMetricsArgs {
        path: params.path,
        target: params.target,
        min_complexity: params.min_complexity,
        sort_by_complexity: params.sort_by_complexity,
        max_results,
    })
}

// ============================================================================
// Navigation Tool Conversion Functions
// ============================================================================

fn convert_get_definition_params(params: GetDefinitionParams) -> GetDefinitionArgs {
    GetDefinitionArgs {
        symbol: params.symbol,
        path: params.path,
    }
}

fn convert_get_references_params(
    params: GetReferencesParams,
) -> Result<GetReferencesArgs, RpcError> {
    let max_results = validate_max_results(params.max_results, 1000)?;
    let pagination = convert_pagination(None, 100, params.pagination.as_ref())?;

    Ok(GetReferencesArgs {
        symbol: params.symbol,
        path: params.path,
        include_declaration: params.include_declaration,
        max_results,
        pagination,
    })
}

fn convert_get_hover_info_params(params: GetHoverInfoParams) -> GetHoverInfoArgs {
    GetHoverInfoArgs {
        symbol: params.symbol,
        path: params.path,
    }
}

fn convert_get_document_symbols_params(params: GetDocumentSymbolsParams) -> GetDocumentSymbolsArgs {
    GetDocumentSymbolsArgs {
        file_path: params.file_path,
        path: params.path,
    }
}

fn convert_get_workspace_symbols_params(
    params: GetWorkspaceSymbolsParams,
) -> Result<GetWorkspaceSymbolsArgs, RpcError> {
    let max_results = validate_max_results(params.max_results, 1000)?;
    let pagination = convert_pagination(None, 100, params.pagination.as_ref())?;

    Ok(GetWorkspaceSymbolsArgs {
        query: params.query,
        path: params.path,
        max_results,
        pagination,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sqry_server_creation() {
        let server = SqryServer::new(FeatureFlags::default());
        assert_eq!(server.timeout_ms, 60_000);
    }

    #[test]
    fn test_feature_flag_filtering() {
        let flags = FeatureFlags::default();
        // All tools should be enabled by default
        assert!(flags.is_tool_enabled("semantic_search"));
        assert!(flags.is_tool_enabled("hierarchical_search"));
    }

    #[test]
    fn test_error_conversion() {
        let rpc_err = RpcError::validation("test error");
        let mcp_err = rpc_error_to_mcp(rpc_err);
        // Verify it's an invalid_params error
        assert!(mcp_err.to_string().contains("test error"));
    }

    #[test]
    fn test_expand_cache_status_params_conversion() {
        let params = ExpandCacheStatusParams {
            path: "/my/workspace".to_string(),
        };
        let args = convert_expand_cache_status_params(params);
        assert_eq!(args.path, "/my/workspace");
    }

    #[test]
    fn test_expand_cache_status_params_deserialized_default_path() {
        // When deserialized from JSON without a "path" field, serde uses default_path()
        let params: ExpandCacheStatusParams = serde_json::from_str("{}").unwrap();
        assert_eq!(params.path, ".");
    }
}
