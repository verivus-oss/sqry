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
    SqryQueryParams, StructuralSimilarParams, SubgraphParams, TracePathParams, UnusedScopeParam,
    VisibilityParam, WorkspaceStatusParams,
};
use crate::workspace_session::{self, WorkspaceSessionRegistry};
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
use sqry_core::workspace::LogicalWorkspace;
use sqry_mcp_redaction::{LogicalWorkspaceView, RedactionConfig, Redactor, compute_source_root_id};
use std::path::{Path, PathBuf};
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
    /// Session-scoped workspace selection state.
    workspace_sessions: Arc<WorkspaceSessionRegistry>,
}

impl SqryServer {
    /// Create a new `SqryServer` with the given feature flags and default timeout.
    pub fn new(feature_flags: FeatureFlags) -> Self {
        let tool_router = Self::filtered_tool_router(&feature_flags);
        // Set manifest tool count and names from the filtered tool registry
        let tool_list = tool_router.list_all();
        #[allow(clippy::cast_possible_truncation)] // Server config values fit in target type
        resources::set_tool_count(tool_list.len() as u32);
        resources::set_tool_names(
            tool_list
                .iter()
                .map(|t| t.name.as_ref().to_string())
                .collect(),
        );
        Self {
            feature_flags,
            timeout_ms: 60_000,
            index_timeout_ms: 600_000,
            retry_delay_ms: 500,
            tool_router,
            prompt_router: create_prompt_router(),
            redactor: None,
            workspace_sessions: Arc::new(WorkspaceSessionRegistry::default()),
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
        // Set manifest tool count and names from the filtered tool registry
        let tool_list = tool_router.list_all();
        #[allow(clippy::cast_possible_truncation)] // Server config values fit in target type
        resources::set_tool_count(tool_list.len() as u32);
        resources::set_tool_names(
            tool_list
                .iter()
                .map(|t| t.name.as_ref().to_string())
                .collect(),
        );
        Self {
            feature_flags,
            timeout_ms,
            index_timeout_ms,
            retry_delay_ms,
            tool_router,
            prompt_router: create_prompt_router(),
            redactor,
            workspace_sessions: Arc::new(WorkspaceSessionRegistry::default()),
        }
    }

    /// Create a `Redactor` from the given preset name.
    ///
    /// Uses `RedactionConfig::from_preset_with_env()` to apply the given preset as base,
    /// then layer fine-grained env var overrides (`SQRY_REDACT_PATHS`, `SQRY_REDACT_CODE`, etc.)
    /// on top. This ensures config-file presets are respected even when `SQRY_REDACTION_PRESET`
    /// is not set in the environment.
    ///
    /// `STEP_7` codex iter4 BLOCK fix — `"none"` no longer short-circuits
    /// to [`None`]. Acceptance criterion 6 requires excluded paths to render
    /// as the opaque-hash form **regardless of preset**, including
    /// `preset=none`. The redaction walker enforces criterion 6 in
    /// passthrough mode only when a [`Redactor`] exists **and** a
    /// [`LogicalWorkspaceView`] is bound at request time (via
    /// [`Self::redactor_for_workspace`]). Returning `None` for `"none"`
    /// kept the criterion-6 path off end-to-end. We now construct a
    /// passthrough redactor (`RedactionConfig::none()`); the
    /// `redact_excluded_in_passthrough` branch in
    /// `sqry_mcp_redaction::walker` only rewrites excluded paths and
    /// leaves every other field verbatim, preserving criterion 3
    /// (`preset=none + path inside source_root → absolute emitted`).
    /// Unknown preset names still return `None` so misconfiguration
    /// degrades to no-redaction rather than panicking.
    pub fn create_redactor(preset: &str) -> Option<Arc<Redactor>> {
        match preset {
            "none" | "minimal" | "relative" | "standard" | "strict" => {}
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

    /// Build a per-request redactor scoped to the resolved workspace.
    ///
    /// `STEP_7` codex iter2 BLOCK fix — the redactor MUST be bound to
    /// the resolved [`LogicalWorkspace`] (translated to a
    /// [`LogicalWorkspaceView`]) when the per-request session resolved
    /// one, so JSON path fields render with the workspace-aware forms
    /// (`<source_root_id>/<rel>`, `<workspace_id_short>/<rel>`,
    /// `<excluded>/[hash]`) specified by acceptance criteria 3-9.
    ///
    /// Pre-fix this helper only set `config.workspace_root` and the
    /// walker fell back to the legacy `redact_path()` pipeline (no
    /// workspace-aware rendering, no exclusion-precedence handling).
    /// Now we route through [`Redactor::with_logical_workspace`] when a
    /// view is available; the legacy single-root path is preserved when
    /// no logical workspace is bound (single-root / pre-resolution
    /// paths) so call sites that don't run inside a request context
    /// still get the same redactor as before.
    fn redactor_for_workspace(
        redactor: &Arc<Redactor>,
        workspace_root: Option<&Path>,
        logical: Option<&LogicalWorkspace>,
    ) -> Option<Redactor> {
        let mut config = redactor.config().clone();
        if let Some(workspace_root) = workspace_root {
            config.workspace_root = Some(workspace_root.to_path_buf());
        }
        if let Some(logical) = logical {
            // Workspace-aware path: bind the `LogicalWorkspaceView` so
            // path fields render with the source-root-id /
            // workspace-id-short / opaque-hash forms required by
            // acceptance criteria 4 / 5 / 6. We keep `workspace_root`
            // set so `canonicalize_for_hash` can resolve relative
            // inputs to absolute (and the workspace-aware reconstruct
            // step in `redact_path_with_workspace` re-promotes the
            // canonicalize-stripped path back to absolute before
            // source-root containment).
            let view = logical_workspace_to_view(logical);
            return Redactor::with_logical_workspace(config, view).ok();
        }
        Redactor::new(config).ok()
    }

    fn redact_error_message(redactor: Option<&Redactor>, msg: String) -> String {
        if let Some(redactor) = redactor {
            let mut value = serde_json::Value::String(msg);
            redactor.redact(&mut value);
            match value {
                serde_json::Value::String(redacted) => redacted,
                other => other.to_string(),
            }
        } else {
            msg
        }
    }

    fn build_redacted_response<T: Serialize>(
        execution: ToolExecution<T>,
        redactor: Option<&Redactor>,
    ) -> Result<serde_json::Value, McpError> {
        let mut response = Self::build_response(execution)?;
        if let Some(redactor) = redactor {
            redactor.redact(&mut response);
        }
        Ok(response)
    }

    fn cancel_if_timeout_elapsed<T>(
        result: &Result<T, tokio::time::error::Elapsed>,
        cancel: &sqry_core::query::cancellation::CancellationToken,
    ) {
        if result.is_err() {
            cancel.cancel();
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
    /// Resolve request workspace before entering `spawn_blocking`.
    async fn execute_tool_for_request<P, F, T>(
        &self,
        tool_name: &str,
        params: &P,
        context: &RequestContext<RoleServer>,
        f: F,
    ) -> Result<serde_json::Value, McpError>
    where
        P: Serialize,
        // `A_cancellation.md` §2 Caller-site changes: the user
        // closure now receives a borrowed CancellationToken so it
        // can poll for deadline-driven cancellation and propagate
        // the token into `inner::execute_*`.
        F: FnOnce(
                &sqry_core::query::cancellation::CancellationToken,
            ) -> anyhow::Result<ToolExecution<T>>
            + Send
            + 'static,
        T: Serialize + Send + 'static,
    {
        let resolved_workspace = self
            .workspace_sessions
            .resolve_for_request(params, context)
            .await
            .map_err(|error| McpError::invalid_request(error.to_string(), None))?;

        tracing::debug!(
            tool = tool_name,
            workspace = %resolved_workspace.workspace_root().display(),
            source = resolved_workspace.resolution_source().as_str(),
            "Resolved session-scoped workspace"
        );

        let workspace_root = resolved_workspace.workspace_root().to_path_buf();
        let logical = resolved_workspace.logical_workspace();
        // Clone so the same per-request LogicalWorkspace is consumed
        // both by the redactor binding (`execute_tool_with_timeout`)
        // and by the blocking-thread thread-local override
        // (`with_workspace_override`). Arc<LogicalWorkspace> clones
        // are O(1) refcount bumps, not deep copies.
        let logical_for_redaction = logical.clone();
        self.execute_tool_with_timeout(
            tool_name,
            self.timeout_ms,
            Some(workspace_root),
            logical_for_redaction,
            move |cancel| {
                // Inner-closure trampoline: the wrapper hands us a
                // borrowed token; we clone (cheap Arc bump) so the
                // owned clone can be moved into the
                // `with_workspace_override` inner closure, which
                // hands the borrowed reference to the user-supplied
                // `f`.
                let cancel_inner = cancel.clone();
                workspace_session::with_workspace_override(
                    Some(resolved_workspace.workspace_root()),
                    logical,
                    move || f(&cancel_inner),
                )
            },
        )
        .await
    }

    /// Resolve request workspace before entering `spawn_blocking` for long-running tools.
    async fn execute_tool_with_timeout_for_request<P, F, T>(
        &self,
        tool_name: &str,
        timeout_ms: u64,
        params: &P,
        context: &RequestContext<RoleServer>,
        f: F,
    ) -> Result<serde_json::Value, McpError>
    where
        P: Serialize,
        // See `execute_tool_for_request` for the cancellation
        // closure-signature rationale (`A_cancellation.md` §2).
        F: FnOnce(
                &sqry_core::query::cancellation::CancellationToken,
            ) -> anyhow::Result<ToolExecution<T>>
            + Send
            + 'static,
        T: Serialize + Send + 'static,
    {
        let resolved_workspace = self
            .workspace_sessions
            .resolve_for_request(params, context)
            .await
            .map_err(|error| McpError::invalid_request(error.to_string(), None))?;

        tracing::debug!(
            tool = tool_name,
            workspace = %resolved_workspace.workspace_root().display(),
            source = resolved_workspace.resolution_source().as_str(),
            "Resolved session-scoped workspace"
        );

        let workspace_root = resolved_workspace.workspace_root().to_path_buf();
        let logical = resolved_workspace.logical_workspace();
        // See `execute_tool_for_request` for the rationale on cloning.
        let logical_for_redaction = logical.clone();
        self.execute_tool_with_timeout(
            tool_name,
            timeout_ms,
            Some(workspace_root),
            logical_for_redaction,
            move |cancel| {
                let cancel_inner = cancel.clone();
                workspace_session::with_workspace_override(
                    Some(resolved_workspace.workspace_root()),
                    logical,
                    move || f(&cancel_inner),
                )
            },
        )
        .await
    }

    /// Execute a tool function with a custom timeout.
    ///
    /// Used for long-running operations (e.g., `rebuild_index`) that need a
    /// longer timeout than the general tool default.
    ///
    /// `redaction_logical_workspace` carries the per-request resolved
    /// [`LogicalWorkspace`] (when one was bound by
    /// `WorkspaceSessionRegistry::resolve_for_request`). It is plumbed
    /// down into [`Self::redactor_for_workspace`] so the live response
    /// redactor binds the matching [`LogicalWorkspaceView`] for every
    /// tool call (`STEP_7` codex iter2 BLOCK fix). Threading it as an
    /// explicit parameter — rather than relying on the
    /// `LOGICAL_WORKSPACE_OVERRIDE` thread-local set by
    /// `with_workspace_override` — is required because the redactor
    /// binding runs **after** `spawn_blocking` returns, back on the
    /// tokio reactor thread where the blocking-thread thread-local is
    /// no longer in scope.
    async fn execute_tool_with_timeout<F, T>(
        &self,
        tool_name: &str,
        timeout_ms: u64,
        redaction_workspace_root: Option<PathBuf>,
        redaction_logical_workspace: Option<Arc<LogicalWorkspace>>,
        f: F,
    ) -> Result<serde_json::Value, McpError>
    where
        // Closure receives a borrowed CancellationToken so it can both
        // observe (poll) and propagate (clone into nested calls). The
        // wrapper retains ownership and signals via `cancel.cancel()`
        // when the per-tool deadline elapses, so the in-flight
        // `spawn_blocking` thread observes the signal on its next
        // pass-boundary poll inside `evaluate_all` (per
        // `A_cancellation.md` §2 + `00_contracts.md` §3.CC-1).
        F: FnOnce(
                &sqry_core::query::cancellation::CancellationToken,
            ) -> anyhow::Result<ToolExecution<T>>
            + Send
            + 'static,
        T: Serialize + Send + 'static,
    {
        let span = info_span!("tool_execution", tool = tool_name);
        let timeout_duration = Duration::from_millis(timeout_ms);
        let tool_name_owned = tool_name.to_string();
        let retry_delay_ms = self.retry_delay_ms;
        let redactor_clone = self.redactor.clone();
        let redaction_workspace_root = redaction_workspace_root.clone();
        let redaction_logical_workspace = redaction_logical_workspace.clone();

        // Per-request cancellation token: wrapper owns the canonical
        // clone, closure owns a Send/Clone copy moved into
        // `spawn_blocking`. Every `cancel.cancel()` on either clone
        // flips the same `Arc<AtomicBool>` flag so both surfaces
        // observe the cancellation immediately.
        let cancel = sqry_core::query::cancellation::CancellationToken::new();
        let cancel_for_closure = cancel.clone();

        async move {
            let join_handle = spawn_blocking(move || f(&cancel_for_closure));
            let result = timeout(timeout_duration, join_handle).await;

            // Deadline elapsed → flip the token *before* falling
            // through so the detached blocking thread observes
            // cancellation on its next per-batch poll inside
            // `evaluate_all`. We must NOT await the JoinHandle — per
            // GT-6 a running `spawn_blocking` task cannot be aborted,
            // and the contract on the deadline arm is fire-and-forget;
            // the cooperative-cancellation token is what frees the
            // blocking-pool slot once the closure body returns.
            Self::cancel_if_timeout_elapsed(&result, &cancel);

            let workspace_scoped_redactor = redactor_clone.as_ref().and_then(|redactor| {
                Self::redactor_for_workspace(
                    redactor,
                    redaction_workspace_root.as_deref(),
                    redaction_logical_workspace.as_deref(),
                )
            });

            let scoped_redactor = workspace_scoped_redactor.as_ref();

            match result {
                Ok(Ok(Ok(execution))) => Self::build_redacted_response(execution, scoped_redactor),
                Ok(Ok(Err(anyhow_err))) => {
                    // `A_cancellation.md` §4: if the closure observed
                    // the cancellation we just signalled (deadline
                    // elapsed → token flipped → `evaluate_all`
                    // short-circuited with
                    // `QueryError::Cancelled`), surface the canonical
                    // `RpcError::deadline_exceeded` envelope so the
                    // wire shape is identical to the wrapper-only
                    // timeout path. This downcast must run BEFORE
                    // the existing `RpcError` downcast so the
                    // cancellation arm is not classified as a
                    // generic internal error.
                    if let Some(sqry_core::query::QueryError::Cancelled) =
                        anyhow_err.downcast_ref::<sqry_core::query::QueryError>()
                    {
                        return Err(rpc_error_to_mcp(RpcError::deadline_exceeded(
                            &tool_name_owned,
                            timeout_ms,
                            retry_delay_ms,
                        )));
                    }
                    // `B_cost_gate.md` §3 + `00_contracts.md` §3.CC-2:
                    // pre-flight cost-gate rejection emerges from
                    // `execute_evaluate_with` as a `CostGateError`
                    // wrapped in `anyhow::Error`. Reshape into the
                    // canonical `RpcError::query_too_broad` envelope
                    // (4-key wire shape, 7-key `details` payload) so
                    // the standalone path produces byte-identical
                    // output to the daemon's `DaemonError::QueryTooBroad`
                    // arm.
                    if let Some(gate_err) =
                        anyhow_err.downcast_ref::<sqry_core::query::cost_gate::CostGateError>()
                    {
                        let details = gate_err.to_query_too_broad_details();
                        let message = gate_err.to_string();
                        return Err(rpc_error_to_mcp(RpcError::query_too_broad(
                            message, details,
                        )));
                    }
                    // Planner-side cost gate (`sqry_query`,
                    // `plan-query`). Distinct error type, identical
                    // wire envelope.
                    if let Some(gate_err) = anyhow_err
                        .downcast_ref::<sqry_db::planner::cost_gate::PlannerCostGateError>(
                    ) {
                        let details = gate_err.to_query_too_broad_details();
                        let message = gate_err.to_string();
                        return Err(rpc_error_to_mcp(RpcError::query_too_broad(
                            message, details,
                        )));
                    }
                    // `C_budget.md` §3 + `00_contracts.md` §3.CC-2:
                    // runtime row-budget exceedance surfaces through
                    // the canonical `query_too_broad` envelope with
                    // `details.source = "runtime_budget"` (vs the
                    // static-gate `details.source = "static_estimate"`).
                    // Same envelope shape as the static-gate path so
                    // MCP clients use a single parser regardless of
                    // which side observed first.
                    if let Some(budget_err) =
                        anyhow_err.downcast_ref::<sqry_core::query::budget::BudgetExceeded>()
                    {
                        // Cluster-C iter-2: include the sanitised
                        // `predicate_shape` so the runtime_budget
                        // envelope is wire-comparable to the
                        // cluster-B static_estimate envelope.
                        let details = serde_json::json!({
                            "source": "runtime_budget",
                            "kind": sqry_core::query::cost_gate::KIND_QUERY_TOO_BROAD,
                            "examined": budget_err.examined,
                            "limit": budget_err.limit,
                            "predicate_shape": budget_err.predicate_shape.clone(),
                            "suggested_predicates":
                                sqry_core::query::cost_gate::SCOPE_FILTER_FIELDS,
                            "doc_url":
                                sqry_core::query::cost_gate::QUERY_TOO_BROAD_DOC_URL,
                        });
                        return Err(rpc_error_to_mcp(RpcError::query_too_broad(
                            budget_err.to_string(),
                            details,
                        )));
                    }
                    // Structured tool errors are surfaced through the
                    // canonical envelope rather than the opaque
                    // internal-error fallback, so MCP clients can
                    // pattern-match on `details.code`.
                    if let Some(rpc_err) = anyhow_err.downcast_ref::<RpcError>() {
                        Err(rpc_error_to_mcp(rpc_err.clone()))
                    } else {
                        Err(McpError::internal_error(
                            Self::redact_error_message(scoped_redactor, anyhow_err.to_string()),
                            None,
                        ))
                    }
                }
                Ok(Err(join_err)) => Err(McpError::internal_error(
                    Self::redact_error_message(
                        scoped_redactor,
                        format!("Task panicked: {join_err}"),
                    ),
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
    ///
    /// Delegates to [`crate::response::build_tool_response`] with
    /// `include_version = true` (the rmcp transport carries the MCP
    /// protocol version as a field in the response object).
    fn build_response<T: Serialize>(
        execution: ToolExecution<T>,
    ) -> Result<serde_json::Value, McpError> {
        crate::response::build_tool_response(execution, true)
    }

    /// Build a successful `CallToolResult` from JSON value.
    ///
    /// Serialises `value` with pretty-printed JSON, then enforces the
    /// `SQRY_MCP_MAX_OUTPUT_BYTES` byte-cap (default 50 000) via
    /// [`crate::output_caps::truncate_response`]. Truncated payloads
    /// have a fixed marker appended so consumers detect the cut
    /// deterministically. Single-site cap enforcement guarantees every
    /// `#[tool]`-annotated handler is covered uniformly.
    fn success_result(value: &serde_json::Value) -> CallToolResult {
        let serialised = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
        let capped = crate::output_caps::truncate_response(
            &serialised,
            crate::output_caps::max_output_bytes(),
        );
        CallToolResult::success(vec![Content::text(capped.into_owned())])
    }

    /// Iter2-B C029c — test-helper: drive the canonical `success_result`
    /// dispatch boundary directly with a pre-built `serde_json::Value`.
    ///
    /// Gated behind `feature = "test-helpers"` so the production binary
    /// surface is unchanged. Integration tests in `tests/` can opt in via
    /// the `sqry-mcp = { path = ".", features = ["test-helpers"] }`
    /// dev-dep self-reference. This exists so the
    /// `output_caps_truncation_via_dispatch.rs` integration test can
    /// observe the actual `CallToolResult` shape that every
    /// `#[tool]`-decorated handler emits — i.e. the same code path the
    /// `SQRY_MCP_MAX_OUTPUT_BYTES` cap is enforced through, not just the
    /// helper-level `truncate_response` covered by
    /// `output_caps_truncation_smoke.rs`.
    #[cfg(feature = "test-helpers")]
    #[doc(hidden)]
    #[allow(dead_code)] // Used by integration tests via `sqry_mcp::server_test_helpers`
    pub fn build_success_result_for_tests(value: &serde_json::Value) -> CallToolResult {
        // Calls the private `success_result` to guarantee the test
        // exercises the production code path verbatim — no parallel
        // implementation, no helper-only test surface.
        Self::success_result(value)
    }

    /// Iter2-B C029c — test-helper: minimal `SqryServer` constructor
    /// equivalent to `Self::new(FeatureFlags::default())`. Preserved as
    /// a separate symbol so test code documents its intent explicitly
    /// and the helper signature stays stable across `FeatureFlags`
    /// refactors.
    ///
    /// Gated behind `feature = "test-helpers"`.
    #[cfg(feature = "test-helpers")]
    #[doc(hidden)]
    #[allow(dead_code)] // Used by integration tests via `sqry_mcp::server_test_helpers`
    pub fn new_for_tests() -> Self {
        Self::new(FeatureFlags::default())
    }
}

/// Tool implementations using rmcp macros.
#[tool_router]
impl SqryServer {
    /// Search symbols by name, kind, visibility, and language.
    #[tool(
        description = "Search symbols by name, kind, visibility, and language. Phase A C indirect-call precision (U18.1) adds three C-scoped predicates: `address_taken:true|false`, `resolved_via:direct|type_match|binding_plane`, and `callsite_promiscuous:true|false`. These are populated by the C plugin only; on non-C nodes they evaluate to false. For incremental cache behaviour and structural-IR query authoring, prefer `sqry_query`.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn semantic_search(
        &self,
        Parameters(params): Parameters<SemanticSearchParams>,
        context: RequestContext<RoleServer>,
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

        let args = convert_semantic_search_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("semantic_search", &params, &context, move |cancel| {
                execution::execute_semantic_search(&args, cancel)
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("hierarchical_search")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_hierarchical_search_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("hierarchical_search", &params, &context, move |cancel| {
                execution::execute_hierarchical_search(&args, cancel)
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("relation_query")?;

        let args = convert_relation_query_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("relation_query", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("call_hierarchy")?;

        let args = convert_call_hierarchy_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("call_hierarchy", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("explain_code")?;

        let args = convert_explain_code_params(params.clone());
        let result = self
            .execute_tool_for_request("explain_code", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("search_similar")?;

        let args = convert_search_similar_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("search_similar", &params, &context, move |_cancel| {
                execution::execute_find_similar(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Find functions structurally similar to a reference function via the
    /// identifier-blind body-shape descriptor (control-flow shape + MinHash).
    #[tool(
        description = "Find functions structurally similar to a reference function via the identifier-blind body-shape descriptor (control-flow shape + MinHash); reports exact shape_hash identity plus approximate Jaccard. Distinct from name-based search_similar.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn structural_similar(
        &self,
        Parameters(params): Parameters<StructuralSimilarParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("structural_similar")?;

        let args = convert_structural_similar_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("structural_similar", &params, &context, move |_cancel| {
                execution::execute_structural_similar(&args)
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("show_dependencies")?;

        // Custom XOR validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_show_dependencies_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("show_dependencies", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_index_status")?;

        let args = convert_get_index_status_params(params.clone());
        let result = self
            .execute_tool_for_request("get_index_status", &params, &context, move |_cancel| {
                execution::execute_index_status(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Return aggregate `WorkspaceIndexStatus` for the resolved
    /// workspace, plus `workspace_id_short`/`workspace_id_full` and the
    /// projection of `source_roots` / `member_folders` / `exclusions`
    /// (`STEP_7` acceptance criteria 1 + 2).
    #[tool(
        description = "Return the aggregate WorkspaceIndexStatus for the active logical workspace, with workspace identity and structure projection",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn workspace_status(
        &self,
        Parameters(params): Parameters<WorkspaceStatusParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("workspace_status")?;

        let args = convert_workspace_status_params(params.clone());
        let result = self
            .execute_tool_for_request("workspace_status", &params, &context, move |_cancel| {
                crate::tools::execute_workspace_status(&args)
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("rebuild_index")?;

        let args = convert_rebuild_index_params(params.clone());
        let result = self
            .execute_tool_with_timeout_for_request(
                "rebuild_index",
                self.index_timeout_ms,
                &params,
                &context,
                move |_cancel| execution::execute_rebuild_index(&args),
            )
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("export_graph")?;

        // Custom XOR validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_export_graph_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("export_graph", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("cross_language_edges")?;

        let args = convert_cross_language_edges_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("cross_language_edges", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("trace_path")?;

        let args = convert_trace_path_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("trace_path", &params, &context, move |_cancel| {
                execution::execute_trace_path(&args)
            })
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("subgraph")?;

        // Custom non-empty validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_subgraph_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("subgraph", &params, &context, move |_cancel| {
                execution::execute_subgraph(&args)
            })
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("dependency_impact")?;

        let args = convert_dependency_impact_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("dependency_impact", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("semantic_diff")?;

        let args = convert_semantic_diff_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("semantic_diff", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("find_duplicates")?;

        let args = convert_find_duplicates_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("find_duplicates", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("find_cycles")?;

        let args = convert_find_cycles_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("find_cycles", &params, &context, move |_cancel| {
                execution::execute_find_cycles(&args)
            })
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("find_unused")?;

        let args = convert_find_unused_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("find_unused", &params, &context, move |_cancel| {
                execution::execute_find_unused(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Detect Go call sites that leak `context.Context` propagation
    /// (T3.7, Cluster G).
    #[tool(
        description = "Detect Go call-sites that leak context.Context propagation: callers with ctx + ctx-accepting callees where ctx is not threaded. Modes: break_site (sync), unthreaded_goroutine (go f), http_handler_leak (http.HandlerFunc shape).",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn context_propagation(
        &self,
        Parameters(params): Parameters<crate::tools::ContextPropagationParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("context_propagation")?;

        let args = convert_context_propagation_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("context_propagation", &params, &context, move |_cancel| {
                execution::execute_context_propagation(&args)
            })
            .await?;

        Ok(Self::success_result(&result))
    }

    /// Execute a structural query through the sqry-db planner (DB13).
    #[tool(
        description = "Execute a structural query via the sqry-db planner: parses a predicate-chain text syntax (kind:function has:caller ...), runs it against the unified graph, and returns matching nodes with file+line metadata",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn sqry_query(
        &self,
        Parameters(params): Parameters<SqryQueryParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("sqry_query")?;

        let args = params.clone();
        let result = self
            .execute_tool_for_request("sqry_query", &params, &context, move |_cancel| {
                execution::execute_sqry_query(&args)
            })
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("is_node_in_cycle")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_is_node_in_cycle_params(params.clone());
        let result = self
            .execute_tool_for_request("is_node_in_cycle", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("pattern_search")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_pattern_search_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("pattern_search", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("direct_callers")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_direct_callers_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("direct_callers", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("direct_callees")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_direct_callees_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("direct_callees", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("list_files")?;

        let args = convert_list_files_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("list_files", &params, &context, move |_cancel| {
                execution::execute_list_files(&args)
            })
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("list_symbols")?;

        let args = convert_list_symbols_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("list_symbols", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_graph_stats")?;

        let args = convert_get_graph_stats_params(params.clone());
        let result = self
            .execute_tool_for_request("get_graph_stats", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_insights")?;

        let args = convert_get_insights_params(params.clone());
        let result = self
            .execute_tool_for_request("get_insights", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("complexity_metrics")?;

        let args = convert_complexity_metrics_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("complexity_metrics", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("expand_cache_status")?;

        let args = convert_expand_cache_status_params(params.clone());
        let result = self
            .execute_tool_for_request("expand_cache_status", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_definition")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_get_definition_params(params.clone());
        let result = self
            .execute_tool_for_request("get_definition", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_references")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_get_references_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("get_references", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_hover_info")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_get_hover_info_params(params.clone());
        let result = self
            .execute_tool_for_request("get_hover_info", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_document_symbols")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args = convert_get_document_symbols_params(params.clone());
        let result = self
            .execute_tool_for_request("get_document_symbols", &params, &context, move |_cancel| {
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
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_tool_enabled("get_workspace_symbols")?;

        // Custom validation
        params.validate().map_err(rpc_error_to_mcp)?;

        let args =
            convert_get_workspace_symbols_params(params.clone()).map_err(rpc_error_to_mcp)?;
        let result = self
            .execute_tool_for_request("get_workspace_symbols", &params, &context, move |_cancel| {
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
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_server_info(Implementation::new("sqry-mcp", env!("CARGO_PKG_VERSION")))
        .with_instructions(
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
                 - Find similar: search_similar\n\
                 - Explain symbol context: explain_code\n\n\
                 The `filters` parameter on semantic_search/hierarchical_search is a JSON object \
                 (e.g., {\"language\":[\"rust\"]}), not a string. \
                 For string-style predicates like `lang:rust`, use the `query` parameter.\n\n\
                 Detailed docs available as resources: \
                 sqry://docs/tool-guide, sqry://docs/query-syntax, \
                 sqry://docs/patterns, sqry://docs/architecture"
        )
    }

    /// List available prompts - these appear as `/mcp__sqry__*` commands in Claude Code.
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        let prompts = self.prompt_router.list_all();
        Ok(ListPromptsResult::with_all_items(prompts))
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
        Ok(ListResourcesResult::with_all_items(
            resources::list_resources(),
        ))
    }

    /// Read a documentation resource by URI.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        match resources::read_resource(&request.uri) {
            Some(contents) => Ok(ReadResourceResult::new(vec![contents])),
            None => Err(McpError::resource_not_found(
                format!("unknown resource: {}", request.uri),
                None,
            )),
        }
    }

    async fn on_initialized(&self, context: rmcp::service::NotificationContext<RoleServer>) {
        self.workspace_sessions
            .record_client_info(context.peer.peer_info());
    }

    async fn on_roots_list_changed(
        &self,
        _context: rmcp::service::NotificationContext<RoleServer>,
    ) {
        self.workspace_sessions.invalidate_roots();
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

/// Cluster-C iter-2: validate per-call `budget_rows`. The MCP wire
/// contract (`C_budget.md` §C5) forbids `0` because a budget of zero
/// would trip on the first row, which is never the operator intent.
/// Return `RpcError::validation_with_data` (-32602 `InvalidParams`) so
/// callers see a typed validation failure instead of falling through
/// to the env / default path silently.
fn validate_budget_rows(value: Option<u64>) -> Result<Option<u64>, RpcError> {
    match value {
        Some(0) => Err(RpcError::validation_with_data(
            "budget_rows must be > 0".to_string(),
            json!({
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
        cfg_condition: f.cfg_condition.map(|cfg| crate::tools::CfgConditionFilter {
            equals: cfg.equals,
            matches: cfg.matches,
            semantic_match: cfg.semantic_match,
        }),
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
        budget_rows: validate_budget_rows(params.budget_rows)?,
        // Phase β joint-stubs: thread the framework + resolved_via filter
        // params from the MCP boundary into the validated args struct.
        // Downstream Plan A / Plan B PRs read these on the executor side.
        framework: params.framework.map(Into::into),
        resolved_via: params
            .resolved_via
            .map(|v| v.into_iter().map(Into::into).collect()),
        revision_id: params.revision_id,
        revision_ref: params.revision_ref,
        revision_commit: params.revision_commit,
        revision_tree: params.revision_tree,
        revision_dirty: params.revision_dirty,
        revision_include_untracked: params.revision_include_untracked,
        revision_include_ignored: params.revision_include_ignored,
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
        budget_rows: validate_budget_rows(params.budget_rows)?,
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
        RelationTypeParam::Wraps => RelationType::Wraps,
        RelationTypeParam::ChannelPeers => RelationType::ChannelPeers,
        RelationTypeParam::Instantiations => RelationType::Instantiations,
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
        // Phase β joint-stubs: thread MCP filter params through to the
        // validated args struct.
        framework: params.framework.map(Into::into),
        resolved_via: params
            .resolved_via
            .map(|v| v.into_iter().map(Into::into).collect()),
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

fn convert_structural_similar_params(
    params: StructuralSimilarParams,
) -> Result<crate::tools::StructuralSimilarArgs, RpcError> {
    let max_results = validate_max_results(params.max_results, 200)?;
    Ok(crate::tools::StructuralSimilarArgs {
        path: params.path,
        file_path: params.file_path,
        symbol_name: params.symbol_name,
        similarity_threshold: params.similarity_threshold,
        max_results,
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

fn convert_workspace_status_params(
    params: WorkspaceStatusParams,
) -> crate::tools::WorkspaceStatusArgs {
    crate::tools::WorkspaceStatusArgs {
        workspace_id: params.workspace_id,
        path: params.path,
    }
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
    let max_members_per_group = validate_usize(
        params.max_members_per_group,
        "max_members_per_group",
        0,
        10_000,
    )?;

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
        max_members_per_group,
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

fn convert_context_propagation_params(
    params: crate::tools::ContextPropagationParams,
) -> Result<crate::tools::ContextPropagationArgs, RpcError> {
    use crate::tools::params::ContextScopeParam;
    use crate::tools::{ContextPropagationArgs, ContextScopeArg};
    let max_results = validate_max_results(params.max_results, 5_000)?;
    let scope = match params.scope {
        ContextScopeParam::Global => ContextScopeArg::Global,
        ContextScopeParam::File { path } => ContextScopeArg::File(path),
    };
    Ok(ContextPropagationArgs {
        path: params.path,
        scope,
        mode: params.mode.into(),
        max_results,
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
        exclude_cfg_gated: params.exclude_cfg_gated,
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

    let file_path = params
        .file_path
        .map(|s| std::path::PathBuf::from(s.replace('\\', "/")));

    IsNodeInCycleArgs {
        symbol: params.symbol,
        path: params.path,
        cycle_type,
        min_depth: params.min_depth,
        max_depth: params.max_depth,
        include_self_loops: params.include_self_loops,
        file_path,
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
        // Phase β joint-stubs: thread MCP filter params through.
        framework: params.framework.map(Into::into),
        resolved_via: params
            .resolved_via
            .map(|v| v.into_iter().map(Into::into).collect()),
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
        // Phase β joint-stubs: thread MCP filter params through.
        framework: params.framework.map(Into::into),
        resolved_via: params
            .resolved_via
            .map(|v| v.into_iter().map(Into::into).collect()),
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
        summary: params.summary,
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

/// Translate a [`sqry_core::workspace::LogicalWorkspace`] into the
/// leaf-crate [`LogicalWorkspaceView`] consumed by the redaction
/// pipeline.
///
/// `STEP_7` codex iter2 BLOCK fix support — the leaf
/// `sqry-mcp-redaction` crate deliberately does not depend on
/// `sqry-core`, so the translation lives here in `sqry-mcp` (the only
/// crate that already pulls in both). We compute each
/// `source_root_id` via [`compute_source_root_id`] so the redactor's
/// per-source-root prefix exactly matches what
/// `sqry_mcp_redaction::rules::path::redact_path_with_workspace` will
/// emit at runtime (and what the unit tests in
/// `sqry-mcp-redaction/tests/workspace_aware_paths.rs` already lock
/// down).
fn logical_workspace_to_view(workspace: &LogicalWorkspace) -> LogicalWorkspaceView {
    let workspace_id_short = workspace.workspace_id().as_short_hex();
    let source_roots = workspace
        .source_roots()
        .iter()
        .map(|root| {
            let id = compute_source_root_id(&workspace_id_short, &root.path);
            (id, root.path.clone())
        })
        .collect();
    let member_folders = workspace
        .member_folders()
        .iter()
        .map(|m| m.path.clone())
        .collect();
    let exclusions = workspace.exclusions().to_vec();
    LogicalWorkspaceView {
        workspace_id_short,
        source_roots,
        member_folders,
        exclusions,
    }
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
    fn create_redactor_accepts_relative_preset_and_keeps_redaction_enabled() {
        // Issue #394 item 4 regression: `relative` MUST be an accepted preset so
        // the server builds an enabled redactor. If it fell into the unknown-preset
        // branch, `create_redactor` would return None and the server would run
        // with redaction DISABLED, leaking absolute host paths and code/docs.
        assert!(
            SqryServer::create_redactor("relative").is_some(),
            "relative preset must produce an enabled redactor, not disable redaction"
        );
        // The known presets all stay enabled.
        for preset in ["none", "minimal", "standard", "strict"] {
            assert!(
                SqryServer::create_redactor(preset).is_some(),
                "preset `{preset}` must produce a redactor"
            );
        }
        // A genuinely-unknown preset still degrades to no redactor (documented).
        assert!(SqryServer::create_redactor("bogus-preset").is_none());
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

    // ----------------------------------------------------------------------
    // Phase β joint-stubs — MCP-converter integration coverage for the new
    // `framework` / `resolved_via` filter params. Pins the param→args
    // translation so the end-to-end wire path (JSON params → converter →
    // validated args → executor → planner predicate) stays type-checked
    // and behaviourally pinned alongside the predicate-evaluation tests in
    // `sqry-db/tests/phase_beta_predicate_evaluation.rs`.
    // ----------------------------------------------------------------------

    #[test]
    fn convert_relation_query_threads_phase_beta_filter_params() {
        use crate::tools::params::{FrameworkIdParam, RelationTypeParam, ResolvedViaParam};
        use sqry_core::graph::unified::edge::kind::ResolvedVia;
        use sqry_core::schema::FrameworkId;

        let params = RelationQueryParams {
            symbol: "handler".to_string(),
            relation_type: RelationTypeParam::Callers,
            path: ".".to_string(),
            max_depth: 1,
            max_results: 50,
            page_token: None,
            page_size: 50,
            budget_rows: None,
            framework: Some(FrameworkIdParam::Flask),
            resolved_via: Some(vec![
                ResolvedViaParam::Direct,
                ResolvedViaParam::VirtualDispatch,
            ]),
        };

        let args = convert_relation_query_params(params).expect("convert");
        assert_eq!(args.framework, Some(FrameworkId::Flask));
        assert_eq!(
            args.resolved_via,
            Some(vec![ResolvedVia::Direct, ResolvedVia::VirtualDispatch]),
        );
    }

    #[test]
    fn convert_relation_query_omits_phase_beta_filter_params_when_absent() {
        use crate::tools::params::RelationTypeParam;

        let params = RelationQueryParams {
            symbol: "handler".to_string(),
            relation_type: RelationTypeParam::Callers,
            path: ".".to_string(),
            max_depth: 1,
            max_results: 50,
            page_token: None,
            page_size: 50,
            budget_rows: None,
            framework: None,
            resolved_via: None,
        };

        let args = convert_relation_query_params(params).expect("convert");
        assert!(args.framework.is_none());
        assert!(args.resolved_via.is_none());
    }

    #[test]
    fn convert_direct_callers_threads_phase_beta_filter_params() {
        use crate::tools::params::{FrameworkIdParam, ResolvedViaParam};
        use sqry_core::graph::unified::edge::kind::ResolvedVia;
        use sqry_core::schema::FrameworkId;

        let params = DirectCallersParams {
            symbol: "handler".to_string(),
            path: ".".to_string(),
            max_results: 50,
            pagination: None,
            framework: Some(FrameworkIdParam::Django),
            resolved_via: Some(vec![ResolvedViaParam::InterfaceDispatch]),
        };

        let args = convert_direct_callers_params(params).expect("convert");
        assert_eq!(args.framework, Some(FrameworkId::Django));
        assert_eq!(
            args.resolved_via,
            Some(vec![ResolvedVia::InterfaceDispatch]),
        );
    }

    #[test]
    fn convert_direct_callees_threads_phase_beta_filter_params() {
        use crate::tools::params::{FrameworkIdParam, ResolvedViaParam};
        use sqry_core::graph::unified::edge::kind::ResolvedVia;
        use sqry_core::schema::FrameworkId;

        let params = DirectCalleesParams {
            symbol: "handler".to_string(),
            path: ".".to_string(),
            max_results: 50,
            pagination: None,
            framework: Some(FrameworkIdParam::Spring),
            resolved_via: Some(vec![
                ResolvedViaParam::PromiscuousElided,
                ResolvedViaParam::DuckTyped,
            ]),
        };

        let args = convert_direct_callees_params(params).expect("convert");
        assert_eq!(args.framework, Some(FrameworkId::Spring));
        assert_eq!(
            args.resolved_via,
            Some(vec![ResolvedVia::PromiscuousElided, ResolvedVia::DuckTyped]),
        );
    }
}
