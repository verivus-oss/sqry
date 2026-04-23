use crate::cancel;
use crate::config::ConfigDiff;
use crate::handlers::LspHandlerError;
use crate::handlers::{
    ask, batch_counts, call_hierarchy, code_action, complexity_metrics, definition,
    dependency_impact, direct_relations, document_symbol, execute_command, explain_symbol,
    get_insights, graph_export, graph_stats, hierarchical_search, hover, index, is_node_in_cycle,
    pattern_search, references, relations, search, semantic_diff, show_dependencies,
    similar_symbols, subgraph, trace_path, workspace_symbol,
};
use crate::protocol::{
    SqryAskParams, SqryAskResult, SqryBatchCallerCalleeCountParams,
    SqryBatchCallerCalleeCountResult, SqryComplexityMetricsParams, SqryComplexityMetricsResult,
    SqryDependencyImpactParams, SqryDependencyImpactResult, SqryDirectCalleesParams,
    SqryDirectCalleesResult, SqryDirectCallersParams, SqryDirectCallersResult,
    SqryExplainSymbolParams, SqryExplainSymbolResult, SqryGetInsightsParams, SqryGetInsightsResult,
    SqryGraphExportParams, SqryGraphExportResult, SqryGraphStatsParams, SqryGraphStatsResult,
    SqryHierarchicalSearchParams, SqryHierarchicalSearchResult, SqryIndexStatusParams,
    SqryIndexStatusResult, SqryIsNodeInCycleParams, SqryIsNodeInCycleResult,
    SqryListCircularDependenciesParams, SqryListCircularDependenciesResult,
    SqryListCrossLanguageRelationsParams, SqryListCrossLanguageRelationsResult,
    SqryListDuplicateGroupsParams, SqryListDuplicateGroupsResult, SqryListFilesByLanguageParams,
    SqryListFilesByLanguageResult, SqryListFilesParams, SqryListFilesResult, SqryListSymbolsParams,
    SqryListSymbolsResult, SqryListUnusedSymbolsParams, SqryListUnusedSymbolsResult,
    SqryPatternSearchParams, SqryPatternSearchResult, SqryRelationParams, SqryRelationResult,
    SqrySearchParams, SqrySearchResult, SqrySemanticDiffParams, SqrySemanticDiffResult,
    SqryShowDependenciesParams, SqryShowDependenciesResult, SqrySimilarSymbolsParams,
    SqrySimilarSymbolsResult, SqrySubgraphParams, SqrySubgraphResult, SqryTracePathParams,
    SqryTracePathResult,
};
use crate::session::SessionManager;
use log::{info, warn};
use serde_json::{Value, json};
use sqry_core::progress::IndexProgress;
use sqry_core::query::error::QueryError;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio::task::JoinError;
use tokio::time;
use tower_lsp::async_trait;
use tower_lsp::jsonrpc::{Error as RpcError, ErrorCode, Result as RpcResult};
use tower_lsp::lsp_types::notification::Progress as ProgressNotification;
use tower_lsp::lsp_types::request::WorkDoneProgressCreate;
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    CallHierarchyServerCapability, CodeActionKind, CodeActionOptions, CodeActionParams,
    CodeActionProviderCapability, CodeActionResponse, DidChangeConfigurationParams,
    DidChangeTextDocumentParams, DidChangeWorkspaceFoldersParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentSymbolParams, DocumentSymbolResponse, ExecuteCommandParams,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, HoverProviderCapability,
    InitializeParams, InitializeResult, InitializedParams, Location, MessageType, NumberOrString,
    OneOf, ProgressParams, ProgressParamsValue, ReferenceParams, ServerCapabilities, ServerInfo,
    SymbolInformation, TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    WorkDoneProgress, WorkDoneProgressBegin, WorkDoneProgressCreateParams, WorkDoneProgressEnd,
    WorkDoneProgressOptions, WorkDoneProgressReport, WorkspaceFoldersServerCapabilities,
    WorkspaceServerCapabilities, WorkspaceSymbolOptions, WorkspaceSymbolParams,
};
use tower_lsp::{Client, LanguageServer};

pub struct SqryLanguageServer {
    client: Client,
    sessions: SessionManager,
}

impl SqryLanguageServer {
    #[must_use]
    pub fn new(client: Client, sessions: SessionManager) -> Self {
        Self { client, sessions }
    }

    /// Execute the custom `sqry/search` request.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the search worker fails, times out, or is
    /// cancelled.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped before completion (e.g., on
    /// LSP `$/cancelRequest`). This is safe because the search operation is read-only
    /// and does not mutate shared state.
    pub async fn handle_sqry_search(
        &self,
        params: SqrySearchParams,
    ) -> RpcResult<SqrySearchResult> {
        let session = self.sessions.clone();
        let timeout = session.config().search_timeout;
        let worker_session = session.clone();
        let handle = cancel::spawn_blocking(move || search::execute(&worker_session, &params));
        let start = Instant::now();

        let result = time::timeout(timeout, handle).await;
        self.finish_search_request(result, start, timeout).await
    }

    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It only sends telemetry notifications to
    /// the client and does not mutate server state.
    async fn emit_search_telemetry(
        &self,
        outcome: &str,
        duration: std::time::Duration,
        result: Option<&SqrySearchResult>,
        reason: Option<&str>,
    ) {
        let payload = match result {
            Some(result) => json!({
                "event": "sqry/search",
                "outcome": outcome,
                "results": result.results.len(),
                "total": result.total,
                "truncated": result.is_truncated,
                "usedIndex": result.used_index,
                "durationMs": duration.as_millis(),
            }),
            None => json!({
                "event": "sqry/search",
                "outcome": outcome,
                "reason": reason,
                "durationMs": duration.as_millis(),
            }),
        };
        let () = self.client.telemetry_event(payload).await;
    }

    async fn finish_search_request(
        &self,
        result: Result<
            Result<Result<SqrySearchResult, anyhow::Error>, JoinError>,
            time::error::Elapsed,
        >,
        start: Instant,
        timeout: std::time::Duration,
    ) -> RpcResult<SqrySearchResult> {
        if let Ok(join_result) = result {
            match join_result {
                Ok(Ok(result)) => {
                    self.emit_search_telemetry("success", start.elapsed(), Some(&result), None)
                        .await;
                    Ok(result)
                }
                Ok(Err(err)) => {
                    let message = err.to_string();
                    self.emit_search_telemetry("error", start.elapsed(), None, Some(&message))
                        .await;
                    Err(map_error(err))
                }
                Err(join_err) => {
                    let outcome = join_error_outcome(&join_err);
                    self.emit_search_telemetry(outcome, start.elapsed(), None, None)
                        .await;
                    Err(map_join_error(&join_err))
                }
            }
        } else {
            self.emit_search_telemetry("timeout", start.elapsed(), None, None)
                .await;
            Err(RpcError {
                code: ErrorCode::RequestCancelled,
                message: format!("search timed out after {}ms", timeout.as_millis()).into(),
                data: None,
            })
        }
    }

    /// Execute the custom `sqry/references` relation query.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the relation query fails or times out.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped before completion (e.g., on
    /// LSP `$/cancelRequest`). This is safe because the relation query is read-only
    /// and does not mutate shared state.
    pub async fn handle_sqry_relation(
        &self,
        params: SqryRelationParams,
    ) -> RpcResult<SqryRelationResult> {
        let session = self.sessions.clone();
        let timeout = session.config().search_timeout;
        let worker_session = session.clone();
        let handle = cancel::spawn_blocking(move || relations::execute(&worker_session, params));
        let start = Instant::now();

        let result = time::timeout(timeout, handle).await;
        self.finish_relation_request(result, start, timeout).await
    }

    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It only sends telemetry notifications to
    /// the client and does not mutate server state.
    async fn emit_relation_telemetry(
        &self,
        outcome: &str,
        duration: std::time::Duration,
        result: Option<&SqryRelationResult>,
        reason: Option<&str>,
    ) {
        let payload = match result {
            Some(result) => json!({
                "event": "sqry/relation",
                "outcome": outcome,
                "relation": format!("{:?}", result.relation),
                "results": result.results.len(),
                "total": result.total,
                "truncated": result.is_truncated,
                "usedIndex": result.used_index,
                "durationMs": duration.as_millis(),
            }),
            None => json!({
                "event": "sqry/relation",
                "outcome": outcome,
                "reason": reason,
                "durationMs": duration.as_millis(),
            }),
        };
        let () = self.client.telemetry_event(payload).await;
    }

    async fn finish_relation_request(
        &self,
        result: Result<
            Result<Result<SqryRelationResult, anyhow::Error>, JoinError>,
            time::error::Elapsed,
        >,
        start: Instant,
        timeout: std::time::Duration,
    ) -> RpcResult<SqryRelationResult> {
        if let Ok(join_result) = result {
            match join_result {
                Ok(Ok(result)) => {
                    self.emit_relation_telemetry("success", start.elapsed(), Some(&result), None)
                        .await;
                    Ok(result)
                }
                Ok(Err(err)) => {
                    let message = err.to_string();
                    self.emit_relation_telemetry("error", start.elapsed(), None, Some(&message))
                        .await;
                    Err(map_error(err))
                }
                Err(join_err) => {
                    let outcome = join_error_outcome(&join_err);
                    self.emit_relation_telemetry(outcome, start.elapsed(), None, None)
                        .await;
                    Err(map_join_error(&join_err))
                }
            }
        } else {
            self.emit_relation_telemetry("timeout", start.elapsed(), None, None)
                .await;
            Err(RpcError {
                code: ErrorCode::RequestCancelled,
                message: format!("relation query timed out after {}ms", timeout.as_millis()).into(),
                data: None,
            })
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It only sends telemetry notifications to
    /// the client and does not mutate server state.
    async fn emit_call_hierarchy_telemetry(
        &self,
        handler: &str,
        outcome: &str,
        duration: std::time::Duration,
        total: Option<usize>,
        returned: Option<usize>,
        truncated: Option<bool>,
        error: Option<&str>,
    ) {
        let mut payload = serde_json::Map::new();
        payload.insert("event".into(), Value::String("sqry/callHierarchy".into()));
        payload.insert("handler".into(), Value::String(handler.to_string()));
        payload.insert("outcome".into(), Value::String(outcome.to_string()));
        // Duration beyond u64::MAX ms (~584 million years) is impossible; clamp to max
        let duration_ms = duration.as_millis().try_into().unwrap_or(u64::MAX);
        payload.insert(
            "durationMs".into(),
            Value::Number(serde_json::Number::from(duration_ms)),
        );
        if let Some(total) = total {
            payload.insert(
                "total".into(),
                Value::Number(serde_json::Number::from(total as u64)),
            );
        }
        if let Some(returned) = returned {
            payload.insert(
                "returned".into(),
                Value::Number(serde_json::Number::from(returned as u64)),
            );
        }
        if let Some(truncated) = truncated {
            payload.insert("truncated".into(), Value::Bool(truncated));
        }
        if let Some(error) = error {
            payload.insert("error".into(), Value::String(error.to_string()));
        }
        let () = self.client.telemetry_event(Value::Object(payload)).await;
    }

    async fn run_call_hierarchy_with_timeout<T, R, F>(
        &self,
        handler_name: &'static str,
        telemetry_handler: &'static str,
        timeout_label: &'static str,
        timeout: std::time::Duration,
        handle: cancel::CancelableJoinHandle<Result<T, call_hierarchy::CallHierarchyError>>,
        on_success: F,
    ) -> RpcResult<R>
    where
        T: Send + 'static,
        F: FnOnce(T) -> (R, CallHierarchyMetrics),
    {
        let mut guard = HandlerGuard::new(handler_name);
        if let Ok(join_result) = time::timeout(timeout, handle).await {
            match join_result {
                Ok(Ok(result)) => {
                    let duration = guard.elapsed();
                    let (output, metrics) = on_success(result);
                    self.emit_call_hierarchy_telemetry(
                        telemetry_handler,
                        "success",
                        duration,
                        Some(metrics.total),
                        Some(metrics.returned),
                        Some(metrics.truncated),
                        None,
                    )
                    .await;
                    log_handler_success(handler_name, duration);
                    guard.mark_complete();
                    Ok(output)
                }
                Ok(Err(err)) => {
                    let duration = guard.elapsed();
                    let message = err.to_string();
                    self.emit_call_hierarchy_telemetry(
                        telemetry_handler,
                        "error",
                        duration,
                        None,
                        None,
                        None,
                        Some(&message),
                    )
                    .await;
                    log_handler_error(handler_name, duration, &err);
                    guard.mark_complete();
                    Err(map_call_hierarchy_error(err))
                }
                Err(join_err) => {
                    let duration = guard.elapsed();
                    let outcome = join_error_outcome(&join_err);
                    let message = join_err.to_string();
                    self.emit_call_hierarchy_telemetry(
                        telemetry_handler,
                        outcome,
                        duration,
                        None,
                        None,
                        None,
                        Some(&message),
                    )
                    .await;
                    log_handler_join_error(handler_name, duration, &join_err);
                    guard.mark_complete();
                    Err(map_join_error(&join_err))
                }
            }
        } else {
            let duration = guard.elapsed();
            let message = format!("{timeout_label} timed out after {}ms", timeout.as_millis());
            self.emit_call_hierarchy_telemetry(
                telemetry_handler,
                "timeout",
                duration,
                None,
                None,
                None,
                Some(&message),
            )
            .await;
            warn!("{message}");
            log_handler_timeout(handler_name, duration);
            guard.mark_complete();
            Err(RpcError {
                code: ErrorCode::RequestCancelled,
                message: message.into(),
                data: None,
            })
        }
    }

    /// Execute the custom `sqry/indexStatus` request.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when index status retrieval fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs a read-only query to retrieve
    /// index status without spawning tasks or mutating shared state.
    #[allow(clippy::unused_async)] // Required by tower_lsp custom_method signature; kept async for API consistency.
    pub async fn handle_index_status(
        &self,
        params: SqryIndexStatusParams,
    ) -> RpcResult<SqryIndexStatusResult> {
        let status =
            index::index_status(&self.sessions, params.path.as_deref()).map_err(map_error)?;
        Ok(SqryIndexStatusResult { status })
    }

    /// Execute the custom `sqry/listFiles` request.
    ///
    /// Returns a paginated list of indexed file paths.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the file listing fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs a read-only query to retrieve
    /// file paths without spawning tasks or mutating shared state.
    #[allow(clippy::unused_async)] // Required by tower_lsp custom_method signature; kept async for API consistency.
    pub async fn handle_list_files(
        &self,
        params: SqryListFilesParams,
    ) -> RpcResult<SqryListFilesResult> {
        index::list_files(&self.sessions, &params).map_err(map_error)
    }

    /// Execute the custom `sqry/listSymbols` request.
    ///
    /// Returns a paginated list of indexed symbols.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the symbol listing fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs a read-only query to retrieve
    /// symbols without spawning tasks or mutating shared state.
    #[allow(clippy::unused_async)] // Required by tower_lsp custom_method signature; kept async for API consistency.
    pub async fn handle_list_symbols(
        &self,
        params: SqryListSymbolsParams,
    ) -> RpcResult<SqryListSymbolsResult> {
        index::list_symbols(&self.sessions, &params).map_err(map_error)
    }

    /// Execute the custom `sqry/listFilesByLanguage` request.
    ///
    /// Returns a paginated list of file paths for a specific language.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the file listing fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs a read-only query to retrieve
    /// file paths without spawning tasks or mutating shared state.
    #[allow(clippy::unused_async)] // Required by tower_lsp custom_method signature; kept async for API consistency.
    pub async fn handle_list_files_by_language(
        &self,
        params: SqryListFilesByLanguageParams,
    ) -> RpcResult<SqryListFilesByLanguageResult> {
        index::list_files_by_language(&self.sessions, params).map_err(map_error)
    }

    /// Execute the custom `sqry/listCrossLanguageRelations` request.
    ///
    /// Returns a paginated list of cross-language relations (imports, calls).
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the relation listing fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs a read-only query to retrieve
    /// relations without spawning tasks or mutating shared state.
    #[allow(clippy::unused_async)] // Required by tower_lsp custom_method signature; kept async for API consistency.
    pub async fn handle_list_cross_language_relations(
        &self,
        params: SqryListCrossLanguageRelationsParams,
    ) -> RpcResult<SqryListCrossLanguageRelationsResult> {
        index::list_cross_language_relations(&self.sessions, &params).map_err(map_error)
    }

    /// Execute the custom `sqry/listDuplicateGroups` request.
    ///
    /// Returns groups of duplicate symbols (by body, signature, or struct fields).
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the duplicate detection fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs a read-only query to detect
    /// duplicates without spawning tasks or mutating shared state.
    #[allow(clippy::unused_async)] // Required by tower_lsp custom_method signature; kept async for API consistency.
    pub async fn handle_list_duplicate_groups(
        &self,
        params: SqryListDuplicateGroupsParams,
    ) -> RpcResult<SqryListDuplicateGroupsResult> {
        index::list_duplicate_groups(&self.sessions, &params).map_err(map_error)
    }

    /// Execute the custom `sqry/listCircularDependencies` request.
    ///
    /// Returns cycles detected in call graphs, import graphs, or module dependencies.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the cycle detection fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs a read-only query to detect
    /// cycles without spawning tasks or mutating shared state.
    #[allow(clippy::unused_async)] // Required by tower_lsp custom_method signature; kept async for API consistency.
    pub async fn handle_list_circular_dependencies(
        &self,
        params: SqryListCircularDependenciesParams,
    ) -> RpcResult<SqryListCircularDependenciesResult> {
        index::list_circular_dependencies(&self.sessions, &params).map_err(map_error)
    }

    /// Execute the custom `sqry/listUnusedSymbols` request.
    ///
    /// Returns symbols that appear to be unused based on reachability analysis.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the unused detection fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs a read-only query to detect
    /// unused symbols without spawning tasks or mutating shared state.
    #[allow(clippy::unused_async)] // Required by tower_lsp custom_method signature; kept async for API consistency.
    pub async fn handle_list_unused_symbols(
        &self,
        params: SqryListUnusedSymbolsParams,
    ) -> RpcResult<SqryListUnusedSymbolsResult> {
        index::list_unused_symbols(&self.sessions, params).map_err(map_error)
    }

    /// Execute the custom `sqry/hierarchicalSearch` request.
    ///
    /// Returns search results grouped by file → container → symbol.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the hierarchical search fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_hierarchical_search(
        &self,
        params: SqryHierarchicalSearchParams,
    ) -> RpcResult<SqryHierarchicalSearchResult> {
        let session = self.sessions.clone();
        let handle =
            cancel::spawn_blocking(move || hierarchical_search::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/ask` request.
    ///
    /// Translates natural language queries to sqry commands.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the translation fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_ask(&self, params: SqryAskParams) -> RpcResult<SqryAskResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || ask::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/directCallers` request.
    ///
    /// Returns symbols that directly call the given symbol.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the query fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_direct_callers(
        &self,
        params: SqryDirectCallersParams,
    ) -> RpcResult<SqryDirectCallersResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || {
            direct_relations::execute_direct_callers(&session, &params)
        });
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/directCallees` request.
    ///
    /// Returns symbols that the given symbol directly calls.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the query fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_direct_callees(
        &self,
        params: SqryDirectCalleesParams,
    ) -> RpcResult<SqryDirectCalleesResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || {
            direct_relations::execute_direct_callees(&session, &params)
        });
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/batchCallerCalleeCount` request.
    ///
    /// Returns caller and callee counts for multiple symbols in one request.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the query fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_batch_caller_callee_count(
        &self,
        params: SqryBatchCallerCalleeCountParams,
    ) -> RpcResult<SqryBatchCallerCalleeCountResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || {
            batch_counts::batch_caller_callee_count(&session, &params)
        });
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/graphStats` request.
    ///
    /// Returns statistics about the unified code graph.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the query fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_graph_stats(
        &self,
        params: SqryGraphStatsParams,
    ) -> RpcResult<SqryGraphStatsResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || graph_stats::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/patternSearch` request.
    ///
    /// Searches for symbols matching a pattern with wildcard support (* and ?).
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the search fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_pattern_search(
        &self,
        params: SqryPatternSearchParams,
    ) -> RpcResult<SqryPatternSearchResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || pattern_search::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/dependencyImpact` request.
    ///
    /// Analyzes what symbols would be affected if a given symbol changes.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the analysis fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_dependency_impact(
        &self,
        params: SqryDependencyImpactParams,
    ) -> RpcResult<SqryDependencyImpactResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || dependency_impact::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/explainSymbol` request.
    ///
    /// Returns detailed information about a symbol including callers and callees.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when the symbol lookup fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_explain_symbol(
        &self,
        params: SqryExplainSymbolParams,
    ) -> RpcResult<SqryExplainSymbolResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || explain_symbol::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/tracePath` request.
    ///
    /// Finds K shortest call paths between two symbols.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when path tracing fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_trace_path(
        &self,
        params: SqryTracePathParams,
    ) -> RpcResult<SqryTracePathResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || trace_path::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/graphExport` request.
    ///
    /// Exports dependency graphs in various formats.
    ///
    /// # Errors
    ///
    /// Returns an RPC error when graph export fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_graph_export(
        &self,
        params: SqryGraphExportParams,
    ) -> RpcResult<SqryGraphExportResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || graph_export::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/subgraph` request.
    ///
    /// # Errors
    ///
    /// Returns an RPC error if subgraph extraction fails.
    pub async fn handle_subgraph(
        &self,
        params: SqrySubgraphParams,
    ) -> RpcResult<SqrySubgraphResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || subgraph::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/isNodeInCycle` request.
    ///
    /// # Errors
    ///
    /// Returns an RPC error if cycle detection fails.
    pub async fn handle_is_node_in_cycle(
        &self,
        params: SqryIsNodeInCycleParams,
    ) -> RpcResult<SqryIsNodeInCycleResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || is_node_in_cycle::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/similarSymbols` request.
    ///
    /// # Errors
    ///
    /// Returns an RPC error if similar-symbols lookup fails.
    pub async fn handle_similar_symbols(
        &self,
        params: SqrySimilarSymbolsParams,
    ) -> RpcResult<SqrySimilarSymbolsResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || similar_symbols::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/showDependencies` request.
    ///
    /// # Errors
    ///
    /// Returns an RPC error if dependency analysis fails.
    pub async fn handle_show_dependencies(
        &self,
        params: SqryShowDependenciesParams,
    ) -> RpcResult<SqryShowDependenciesResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || show_dependencies::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/complexityMetrics` request.
    ///
    /// # Errors
    ///
    /// Returns an RPC error if complexity metrics collection fails.
    pub async fn handle_complexity_metrics(
        &self,
        params: SqryComplexityMetricsParams,
    ) -> RpcResult<SqryComplexityMetricsResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || complexity_metrics::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/getInsights` request.
    ///
    /// # Errors
    ///
    /// Returns an RPC error if insights collection fails.
    pub async fn handle_get_insights(
        &self,
        params: SqryGetInsightsParams,
    ) -> RpcResult<SqryGetInsightsResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || get_insights::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }

    /// Execute the custom `sqry/semanticDiff` request.
    ///
    /// Compares symbols between two git refs (commits, branches, or tags) and returns
    /// a detailed breakdown of added, removed, modified, renamed, and signature-changed
    /// symbols.
    ///
    /// # Errors
    ///
    /// Returns an RPC error if semantic diff computation fails.
    ///
    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped.
    pub async fn handle_semantic_diff(
        &self,
        params: SqrySemanticDiffParams,
    ) -> RpcResult<SqrySemanticDiffResult> {
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || semantic_diff::execute(&session, &params));
        match handle.await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(err)) => Err(map_error(err)),
            Err(join_err) => Err(map_join_error(&join_err)),
        }
    }
}

#[async_trait]
impl LanguageServer for SqryLanguageServer {
    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It returns static server capabilities and
    /// performs only atomic workspace folder registration.
    async fn initialize(&self, params: InitializeParams) -> RpcResult<InitializeResult> {
        info!("sqry-lsp initialize request received");

        // Extract workspace folders (per PROJECT_ROOT_SPEC.md Section 9.1)
        let workspace_folders: Vec<PathBuf> = params
            .workspace_folders
            .as_ref()
            .map(|folders| {
                folders
                    .iter()
                    .filter_map(|f| f.uri.to_file_path().ok())
                    .collect()
            })
            .unwrap_or_default();

        if !workspace_folders.is_empty() {
            info!(
                "registering {} workspace folder(s) with ProjectManager",
                workspace_folders.len()
            );
            self.sessions.set_workspace_folders(workspace_folders);
        }

        if let Some(root) = &self.sessions.options().index_root {
            info!("using explicit index root: {}", root.display());
        }

        let code_action_options = CodeActionOptions {
            code_action_kinds: Some(vec![CodeActionKind::REFACTOR, CodeActionKind::EMPTY]),
            work_done_progress_options: WorkDoneProgressOptions {
                work_done_progress: Some(false),
            },
            resolve_provider: Some(false),
        };

        let workspace_symbol_options = WorkspaceSymbolOptions {
            work_done_progress_options: WorkDoneProgressOptions {
                work_done_progress: Some(false),
            },
            resolve_provider: Some(false),
        };

        let capabilities = ServerCapabilities {
            text_document_sync: Some(TextDocumentSyncCapability::Options(
                TextDocumentSyncOptions {
                    open_close: Some(true),
                    change: Some(TextDocumentSyncKind::INCREMENTAL),
                    ..Default::default()
                },
            )),
            hover_provider: Some(HoverProviderCapability::Simple(true)),
            definition_provider: Some(OneOf::Left(true)),
            references_provider: Some(OneOf::Left(true)),
            document_symbol_provider: Some(OneOf::Left(true)),
            workspace_symbol_provider: Some(OneOf::Right(workspace_symbol_options)),
            code_action_provider: Some(CodeActionProviderCapability::Options(code_action_options)),
            call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
            // Per PROJECT_ROOT_SPEC.md Section 9.1: support workspace folder change notifications
            workspace: Some(WorkspaceServerCapabilities {
                workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                    supported: Some(true),
                    change_notifications: Some(OneOf::Left(true)),
                }),
                file_operations: None,
            }),
            ..ServerCapabilities::default()
        };
        let server_info = Some(ServerInfo {
            name: "sqry-lsp".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        });

        Ok(InitializeResult {
            server_info,
            capabilities,
        })
    }

    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It sends a log notification and may spawn
    /// a background auto-index task that does not block the handler return. The
    /// background task uses its own progress token and reporter, so aborting it
    /// leaves the server in a consistent state.
    async fn initialized(&self, _params: InitializedParams) {
        let () = self
            .client
            .log_message(tower_lsp::lsp_types::MessageType::INFO, "sqry-lsp ready")
            .await;

        // Auto-index workspace folders that don't have a graph snapshot.
        // Opt-out via SQRY_AUTO_INDEX=false or SQRY_AUTO_INDEX=0.
        let auto_var = std::env::var("SQRY_AUTO_INDEX").unwrap_or_default();
        if auto_var == "false" || auto_var == "0" {
            return;
        }

        // Collect workspace folders; fall back to legacy root path.
        let pm = self.sessions.project_manager();
        let folders = pm.workspace_folders();
        let targets: Vec<PathBuf> = if folders.is_empty() {
            vec![self.sessions.root_path().to_path_buf()]
        } else {
            folders
        };

        // Filter to folders that are missing a graph snapshot.
        let needs_index: Vec<PathBuf> = targets
            .into_iter()
            .filter(|dir| {
                let storage = sqry_core::graph::unified::persistence::GraphStorage::new(dir);
                // Needs index if: no manifest, OR manifest but no snapshot (corruption)
                !storage.exists() || !storage.snapshot_exists()
            })
            .collect();

        if needs_index.is_empty() {
            return;
        }

        info!(
            "Auto-indexing {} workspace folder(s) without a graph snapshot",
            needs_index.len()
        );

        // Spawn a background rebuild for each folder that needs indexing.
        for target in needs_index {
            let client = self.client.clone();
            let session = self.sessions.clone();

            tokio::spawn(async move {
                let token =
                    NumberOrString::String(format!("sqry-auto-index-{}", uuid::Uuid::new_v4()));

                // Create work-done progress token (best-effort; skip on error).
                if client
                    .send_request::<WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                        token: token.clone(),
                    })
                    .await
                    .is_err()
                {
                    warn!("Failed to create progress token for auto-index");
                }

                let begin = WorkDoneProgressBegin {
                    title: "Auto-indexing workspace".to_string(),
                    cancellable: Some(false),
                    message: Some(format!("Root: {}", target.display())),
                    percentage: None,
                };
                client
                    .send_notification::<ProgressNotification>(ProgressParams {
                        token: token.clone(),
                        value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(begin)),
                    })
                    .await;

                let (tx, rx) = mpsc::unbounded_channel();
                let reporter: sqry_core::progress::SharedReporter =
                    Arc::new(index::ChannelProgressReporter::new(tx)) as _;

                let target_clone = target.clone();
                let rebuild = cancel::spawn_blocking(move || {
                    index::rebuild_index(&session, &target_clone, &reporter, false)
                });

                // Forward progress events while the build is running.
                let client_fwd = client.clone();
                let token_fwd = token.clone();
                let progress_task = tokio::spawn(async move {
                    forward_progress(rx, client_fwd, token_fwd).await;
                });

                let end_msg = match rebuild.await {
                    Ok(Ok(summary)) => format!(
                        "Indexed {} symbols in {:.2}s",
                        summary.total_symbols,
                        summary.duration.as_secs_f64()
                    ),
                    Ok(Err(err)) => {
                        warn!("Auto-index failed for {}: {err}", target.display());
                        format!("Auto-index failed: {err}")
                    }
                    Err(join_err) => {
                        warn!("Auto-index task error for {}: {join_err}", target.display());
                        "Auto-index failed (task error)".to_string()
                    }
                };

                progress_task.abort();

                client
                    .send_notification::<ProgressNotification>(ProgressParams {
                        token,
                        value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                            WorkDoneProgressEnd {
                                message: Some(end_msg.clone()),
                            },
                        )),
                    })
                    .await;

                client
                    .log_message(MessageType::INFO, format!("Auto-index: {end_msg}"))
                    .await;
            });
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs atomic updates to the document
    /// store via `DashMap`, which remains consistent even if the operation is cancelled.
    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let doc = params.text_document;
        match uri_to_path(&doc.uri) {
            Some(path) => {
                let limits = &self.sessions.config().document_limits;
                let _ = self.sessions.documents().open(
                    path.as_path(),
                    Some(doc.language_id),
                    doc.version,
                    doc.text.as_str(),
                    limits,
                );
            }
            None => log::warn!("didOpen received non-file URI '{}', ignoring", doc.uri),
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs atomic updates to the document
    /// store via `DashMap`, which remains consistent even if the operation is cancelled.
    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        match uri_to_path(&params.text_document.uri) {
            Some(path) => {
                let limits = &self.sessions.config().document_limits;
                self.sessions.documents().change(
                    &path,
                    Some(params.text_document.version),
                    &params.content_changes,
                    limits,
                );
            }
            None => log::warn!(
                "didChange received non-file URI '{}', ignoring",
                params.text_document.uri
            ),
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs atomic updates to the document
    /// store via `DashMap`, which remains consistent even if the operation is cancelled.
    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        if let Some(path) = uri_to_path(&params.text_document.uri) {
            self.sessions.documents().close(&path);
        }
    }

    /// Handle workspace folder changes (per `PROJECT_ROOT_SPEC.md` Section 9.1).
    ///
    /// This handler processes `workspace/didChangeWorkspaceFolders` notifications
    /// to add or remove workspace folders dynamically. Added folders are registered
    /// with the `ProjectManager`, and removed folders have their associated Projects
    /// torn down.
    ///
    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It performs atomic updates via the
    /// `SessionManager` which uses `RwLock`-protected state.
    async fn did_change_workspace_folders(&self, params: DidChangeWorkspaceFoldersParams) {
        let event = &params.event;

        // Process added folders
        let added: Vec<PathBuf> = event
            .added
            .iter()
            .filter_map(|folder| folder.uri.to_file_path().ok())
            .collect();

        // Process removed folders
        let removed: Vec<PathBuf> = event
            .removed
            .iter()
            .filter_map(|folder| folder.uri.to_file_path().ok())
            .collect();

        if !added.is_empty() || !removed.is_empty() {
            info!(
                "workspace folder change: {} added, {} removed",
                added.len(),
                removed.len()
            );
            self.sessions
                .update_workspace_folders(added.clone(), removed.clone());

            // Log details
            for path in &added {
                info!("workspace folder added: {}", path.display());
            }
            for path in &removed {
                info!("workspace folder removed: {}", path.display());
            }
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. Shutdown is idempotent and properly
    /// tears down the `SessionManager` and `ProjectManager`.
    async fn shutdown(&self) -> RpcResult<()> {
        info!("sqry-lsp shutdown requested");
        // Per PROJECT_ROOT_SPEC.md Section 6.3: shutdown lifecycle
        self.sessions.shutdown();
        Ok(())
    }

    /// # Cancellation Safety
    ///
    /// This handler is cancellation-safe. It updates configuration via `RwLock`, which
    /// remains consistent even if the operation is cancelled mid-execution.
    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        match self.sessions.apply_client_settings(&params.settings) {
            Ok(diff) => {
                if let Some(message) = build_config_update_message(&diff) {
                    let () = self.client.log_message(MessageType::INFO, message).await;
                }
            }
            Err(err) => {
                let () = self
                    .client
                    .log_message(
                        MessageType::ERROR,
                        format!("failed to apply configuration: {err}"),
                    )
                    .await;
            }
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped before completion (e.g., on
    /// LSP `$/cancelRequest`). This is safe because hover queries are read-only and
    /// do not mutate shared state.
    async fn hover(&self, params: HoverParams) -> RpcResult<Option<Hover>> {
        let mut guard = HandlerGuard::new("hover");
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || hover::handle(&session, &params));
        match handle.await {
            Ok(Ok(result)) => {
                log_handler_success("hover", guard.elapsed());
                guard.mark_complete();
                Ok(result)
            }
            Ok(Err(err)) => {
                log_handler_error("hover", guard.elapsed(), &err);
                guard.mark_complete();
                Err(map_error(err))
            }
            Err(join_err) => {
                log_handler_join_error("hover", guard.elapsed(), &join_err);
                guard.mark_complete();
                Err(map_join_error(&join_err))
            }
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()` with a timeout.
    /// The task is automatically aborted if the future is dropped before completion (e.g.,
    /// on LSP `$/cancelRequest`) or if it exceeds the configured timeout. This is safe
    /// because call hierarchy preparation is read-only and does not mutate shared state.
    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> RpcResult<Option<Vec<CallHierarchyItem>>> {
        let session = self.sessions.clone();
        let timeout = session.config().call_hierarchy.timeout;
        let handle = cancel::spawn_blocking(move || call_hierarchy::prepare(&session, &params));
        self.run_call_hierarchy_with_timeout(
            "call_hierarchy_prepare",
            "prepare",
            "callHierarchy/prepare",
            timeout,
            handle,
            |result: Option<Vec<CallHierarchyItem>>| {
                let total = result.as_ref().map_or(0, std::vec::Vec::len);
                let metrics = CallHierarchyMetrics {
                    total,
                    returned: total,
                    truncated: false,
                };
                (result, metrics)
            },
        )
        .await
    }

    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()` with a timeout.
    /// The task is automatically aborted if the future is dropped before completion (e.g.,
    /// on LSP `$/cancelRequest`) or if it exceeds the configured timeout. This is safe
    /// because incoming call queries are read-only and do not mutate shared state.
    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> RpcResult<Option<Vec<CallHierarchyIncomingCall>>> {
        let session = self.sessions.clone();
        let timeout = session.config().call_hierarchy.timeout;
        let handle = cancel::spawn_blocking(move || call_hierarchy::incoming(&session, &params));
        self.run_call_hierarchy_with_timeout(
            "call_hierarchy_incoming",
            "incoming",
            "callHierarchy/incomingCalls",
            timeout,
            handle,
            |response: call_hierarchy::CallHierarchyResponse<CallHierarchyIncomingCall>| {
                let total = response.total;
                let returned = response.items.len();
                let truncated = response.is_truncated;
                let items = response.items;
                let metrics = CallHierarchyMetrics {
                    total,
                    returned,
                    truncated,
                };
                (Some(items), metrics)
            },
        )
        .await
    }

    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()` with a timeout.
    /// The task is automatically aborted if the future is dropped before completion (e.g.,
    /// on LSP `$/cancelRequest`) or if it exceeds the configured timeout. This is safe
    /// because outgoing call queries are read-only and do not mutate shared state.
    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> RpcResult<Option<Vec<CallHierarchyOutgoingCall>>> {
        let session = self.sessions.clone();
        let timeout = session.config().call_hierarchy.timeout;
        let handle = cancel::spawn_blocking(move || call_hierarchy::outgoing(&session, &params));
        self.run_call_hierarchy_with_timeout(
            "call_hierarchy_outgoing",
            "outgoing",
            "callHierarchy/outgoingCalls",
            timeout,
            handle,
            |response: call_hierarchy::CallHierarchyResponse<CallHierarchyOutgoingCall>| {
                let total = response.total;
                let returned = response.items.len();
                let truncated = response.is_truncated;
                let items = response.items;
                let metrics = CallHierarchyMetrics {
                    total,
                    returned,
                    truncated,
                };
                (Some(items), metrics)
            },
        )
        .await
    }

    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped before completion (e.g., on
    /// LSP `$/cancelRequest`). This is safe because definition queries are read-only
    /// and do not mutate shared state.
    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> RpcResult<Option<GotoDefinitionResponse>> {
        let mut guard = HandlerGuard::new("definition");
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || definition::handle(&session, &params));
        match handle.await {
            Ok(Ok(result)) => {
                log_handler_success("definition", guard.elapsed());
                guard.mark_complete();
                Ok(result)
            }
            Ok(Err(err)) => {
                log_handler_error("definition", guard.elapsed(), &err);
                guard.mark_complete();
                Err(map_error(err))
            }
            Err(join_err) => {
                log_handler_join_error("definition", guard.elapsed(), &join_err);
                guard.mark_complete();
                Err(map_join_error(&join_err))
            }
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped before completion (e.g., on
    /// LSP `$/cancelRequest`). This is safe because reference queries are read-only
    /// and do not mutate shared state.
    async fn references(&self, params: ReferenceParams) -> RpcResult<Option<Vec<Location>>> {
        let mut guard = HandlerGuard::new("references");
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || references::handle(&session, &params));
        match handle.await {
            Ok(Ok(result)) => {
                log_handler_success("references", guard.elapsed());
                guard.mark_complete();
                Ok(result)
            }
            Ok(Err(err)) => {
                log_handler_error("references", guard.elapsed(), &err);
                guard.mark_complete();
                Err(map_error(err))
            }
            Err(join_err) => {
                log_handler_join_error("references", guard.elapsed(), &join_err);
                guard.mark_complete();
                Err(map_join_error(&join_err))
            }
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped before completion (e.g., on
    /// LSP `$/cancelRequest`). This is safe because document symbol queries are
    /// read-only and do not mutate shared state.
    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> RpcResult<Option<DocumentSymbolResponse>> {
        let mut guard = HandlerGuard::new("document_symbol");
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || document_symbol::handle(&session, &params));
        match handle.await {
            Ok(Ok(result)) => {
                log_handler_success("document_symbol", guard.elapsed());
                guard.mark_complete();
                Ok(result)
            }
            Ok(Err(err)) => {
                log_handler_error("document_symbol", guard.elapsed(), &err);
                guard.mark_complete();
                Err(map_error(err))
            }
            Err(join_err) => {
                log_handler_join_error("document_symbol", guard.elapsed(), &join_err);
                guard.mark_complete();
                Err(map_join_error(&join_err))
            }
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped before completion (e.g., on
    /// LSP `$/cancelRequest`). This is safe because workspace symbol queries are
    /// read-only and do not mutate shared state.
    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> RpcResult<Option<Vec<SymbolInformation>>> {
        let mut guard = HandlerGuard::new("workspace_symbol");
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || workspace_symbol::handle(&session, &params));
        match handle.await {
            Ok(Ok(result)) => {
                log_handler_success("workspace_symbol", guard.elapsed());
                guard.mark_complete();
                Ok(result.map(|page| {
                    page.items
                        .into_iter()
                        .map(|item| item.info)
                        .collect::<Vec<_>>()
                }))
            }
            Ok(Err(err)) => {
                log_handler_error("workspace_symbol", guard.elapsed(), &err);
                guard.mark_complete();
                Err(map_error(err))
            }
            Err(join_err) => {
                log_handler_join_error("workspace_symbol", guard.elapsed(), &join_err);
                guard.mark_complete();
                Err(map_join_error(&join_err))
            }
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()`. The task
    /// is automatically aborted if the future is dropped before completion (e.g., on
    /// LSP `$/cancelRequest`). This is safe because code action queries are read-only
    /// and do not mutate shared state.
    async fn code_action(&self, params: CodeActionParams) -> RpcResult<Option<CodeActionResponse>> {
        let mut guard = HandlerGuard::new("code_action");
        let session = self.sessions.clone();
        let handle = cancel::spawn_blocking(move || code_action::handle(&session, &params));
        match handle.await {
            Ok(Ok(result)) => {
                log_handler_success("code_action", guard.elapsed());
                guard.mark_complete();
                Ok(result)
            }
            Ok(Err(err)) => {
                log_handler_error("code_action", guard.elapsed(), &err);
                guard.mark_complete();
                Err(map_error(err))
            }
            Err(join_err) => {
                log_handler_join_error("code_action", guard.elapsed(), &join_err);
                guard.mark_complete();
                Err(map_join_error(&join_err))
            }
        }
    }

    /// # Cancellation Safety
    ///
    /// This handler spawns a blocking task via `cancel::spawn_blocking()` for most
    /// commands. The task is automatically aborted if the future is dropped before
    /// completion (e.g., on LSP `$/cancelRequest`). For the special `sqry.index`
    /// command, cancellation aborts both the index rebuild task and the progress
    /// reporting task. This is safe because command execution does not leave the
    /// system in an inconsistent state.
    async fn execute_command(&self, params: ExecuteCommandParams) -> RpcResult<Option<Value>> {
        match params.command.as_str() {
            "sqry.index" => {
                self.run_index_command(params.arguments).await?;
                Ok(None)
            }
            command => {
                let mut guard = HandlerGuard::new("execute_command");
                let session = self.sessions.clone();
                let command = command.to_string();
                let args = params.arguments;
                let handle = cancel::spawn_blocking(move || {
                    execute_command::execute(&session, &command, args)
                });
                match handle.await {
                    Ok(Ok(result)) => {
                        log_handler_success("execute_command", guard.elapsed());
                        guard.mark_complete();
                        Ok(result)
                    }
                    Ok(Err(err)) => {
                        log_handler_error("execute_command", guard.elapsed(), &err);
                        guard.mark_complete();
                        Err(map_error(err))
                    }
                    Err(join_err) => {
                        log_handler_join_error("execute_command", guard.elapsed(), &join_err);
                        guard.mark_complete();
                        Err(map_join_error(&join_err))
                    }
                }
            }
        }
    }
}

fn build_config_update_message(diff: &ConfigDiff) -> Option<String> {
    let mut summary = Vec::new();
    if let Some(level) = diff.log_level {
        summary.push(format!("log level={level}"));
    }
    if let Some(limit) = diff.search_limit {
        summary.push(format!("search.limit={limit}"));
    }
    if let Some(timeout) = diff.search_timeout {
        summary.push(format!("search.timeout={}ms", timeout.as_millis()));
    }
    if let Some(root) = diff.index_root.as_ref() {
        summary.push(format!("indexRoot={}", root.display()));
    }
    if let Some(path) = diff.sqry_path.as_ref() {
        summary.push(format!("sqry.path={}", path.display()));
    }
    if let Some(max_bytes) = diff.document_source_max_bytes {
        summary.push(format!("document.sourceMaxBytes={max_bytes}"));
    }
    if let Some(max_bytes) = diff.document_data_max_bytes {
        summary.push(format!("document.dataMaxBytes={max_bytes}"));
    }
    if let Some(max_results) = diff.call_hierarchy_max_results {
        summary.push(format!("callHierarchy.maxResults={max_results}"));
    }
    if let Some(timeout) = diff.call_hierarchy_timeout {
        summary.push(format!("callHierarchy.timeout={}ms", timeout.as_millis()));
    }
    if let Some(include_detail) = diff.call_hierarchy_include_detail {
        summary.push(format!("callHierarchy.includeDetail={include_detail}"));
    }

    if summary.is_empty() {
        None
    } else {
        Some(format!(
            "sqry configuration updated: {}",
            summary.join(", ")
        ))
    }
}

fn log_handler_success(handler: &'static str, duration: std::time::Duration) {
    // Duration beyond u64::MAX ms (~584 million years) is impossible; clamp to max
    let duration_ms = duration.as_millis().try_into().unwrap_or(u64::MAX);
    tracing::info!(
        target: "sqry_lsp::handler",
        handler,
        duration_ms,
        status = "success"
    );
}

fn log_handler_error<E>(handler: &'static str, duration: std::time::Duration, error: &E)
where
    E: std::fmt::Display,
{
    // Duration beyond u64::MAX ms (~584 million years) is impossible; clamp to max
    let duration_ms = duration.as_millis().try_into().unwrap_or(u64::MAX);
    tracing::error!(
        target: "sqry_lsp::handler",
        handler,
        duration_ms,
        status = "error",
        %error
    );
}

fn log_handler_join_error(handler: &'static str, duration: std::time::Duration, err: &JoinError) {
    // Duration beyond u64::MAX ms (~584 million years) is impossible; clamp to max
    let duration_ms = duration.as_millis().try_into().unwrap_or(u64::MAX);
    if err.is_cancelled() {
        tracing::warn!(
            target: "sqry_lsp::handler",
            handler,
            event = "cancelled",
            acknowledgement_ms = duration_ms
        );
    } else if err.is_panic() {
        tracing::error!(
            target: "sqry_lsp::handler",
            handler,
            duration_ms,
            status = "panic"
        );
    } else {
        tracing::error!(
            target: "sqry_lsp::handler",
            handler,
            duration_ms,
            status = "join_error",
            error = %err
        );
    }
}

fn log_handler_timeout(handler: &'static str, duration: std::time::Duration) {
    // Duration beyond u64::MAX ms (~584 million years) is impossible; clamp to max
    let duration_ms = duration.as_millis().try_into().unwrap_or(u64::MAX);
    tracing::warn!(
        target: "sqry_lsp::handler",
        handler,
        event = "timeout",
        duration_ms
    );
}

fn join_error_outcome(err: &JoinError) -> &'static str {
    if err.is_cancelled() {
        "cancelled"
    } else if err.is_panic() {
        "panic"
    } else {
        "join-error"
    }
}

struct CallHierarchyMetrics {
    total: usize,
    returned: usize,
    truncated: bool,
}

struct HandlerGuard {
    name: &'static str,
    start: Instant,
    completed: bool,
}

impl HandlerGuard {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            start: Instant::now(),
            completed: false,
        }
    }

    fn elapsed(&self) -> std::time::Duration {
        self.start.elapsed()
    }

    fn mark_complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for HandlerGuard {
    fn drop(&mut self) {
        if !self.completed {
            // Duration beyond u64::MAX ms (~584 million years) is impossible; clamp to max
            let ack_ms = self
                .start
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            tracing::warn!(
                target: "sqry_lsp::handler",
                handler = self.name,
                event = "cancelled",
                acknowledgement_ms = ack_ms
            );
        }
    }
}

fn map_error(err: anyhow::Error) -> RpcError {
    match err.downcast::<QueryError>() {
        Ok(query_err) => RpcError::invalid_params(query_err.to_string()),
        Err(other) => match other.downcast::<LspHandlerError>() {
            Ok(handler_err @ LspHandlerError::InvalidParams(_)) => RpcError {
                code: ErrorCode::InvalidParams,
                message: handler_err.to_string().into(),
                data: None,
            },
            Err(other) => RpcError {
                code: ErrorCode::InternalError,
                message: other.to_string().into(),
                data: None,
            },
        },
    }
}

fn map_join_error(err: &JoinError) -> RpcError {
    if err.is_cancelled() {
        RpcError {
            code: ErrorCode::RequestCancelled,
            message: "request cancelled".into(),
            data: None,
        }
    } else if err.is_panic() {
        RpcError {
            code: ErrorCode::InternalError,
            message: "task panicked during execution".into(),
            data: None,
        }
    } else {
        RpcError {
            code: ErrorCode::InternalError,
            message: err.to_string().into(),
            data: None,
        }
    }
}

fn map_call_hierarchy_error(err: call_hierarchy::CallHierarchyError) -> RpcError {
    match err {
        call_hierarchy::CallHierarchyError::IndexMissing => RpcError {
            code: ErrorCode::InvalidParams,
            message: "sqry index not found. Run `sqry index` before using call hierarchy.".into(),
            data: None,
        },
        call_hierarchy::CallHierarchyError::InvalidData(reason) => RpcError {
            code: ErrorCode::InvalidParams,
            message: format!("invalid call hierarchy item: {reason}").into(),
            data: None,
        },
        call_hierarchy::CallHierarchyError::UnsavedBuffer { uri } => RpcError {
            code: ErrorCode::InvalidParams,
            message: format!("Save '{uri}' before expanding call hierarchy.").into(),
            data: None,
        },
        call_hierarchy::CallHierarchyError::RelationQueryFailed(reason)
        | call_hierarchy::CallHierarchyError::SerializationError(reason) => RpcError {
            code: ErrorCode::InternalError,
            message: reason.into(),
            data: None,
        },
    }
}

fn uri_to_path(uri: &tower_lsp::lsp_types::Url) -> Option<PathBuf> {
    uri.to_file_path().ok()
}

impl SqryLanguageServer {
    /// # Cancellation Safety
    ///
    /// This handler spawns two tasks: an index rebuild task via `cancel::spawn_blocking()`
    /// and a progress reporting task via `tokio::spawn()`. If the future is dropped before
    /// completion (e.g., on LSP `$/cancelRequest`), both tasks are aborted. The index
    /// rebuild is aborted mid-operation, which is safe because the index can be rebuilt
    /// from scratch without leaving the system in an inconsistent state. The `ProgressGuard`
    /// ensures that `WorkDoneProgress` notifications are properly terminated.
    async fn run_index_command(&self, arguments: Vec<Value>) -> RpcResult<()> {
        let path_override = arguments
            .first()
            .and_then(|value| value.as_str())
            .map(str::to_string);

        // Extract force flag (second argument, defaults to false)
        let force = arguments
            .get(1)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let target = self
            .sessions
            .resolve_path(path_override.as_deref())
            .map_err(map_error)?;

        let token = NumberOrString::String(format!("sqry-index-{}", uuid::Uuid::new_v4()));

        self.client
            .send_request::<WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                token: token.clone(),
            })
            .await?;

        let begin = WorkDoneProgressBegin {
            title: "Rebuilding sqry index".to_string(),
            cancellable: Some(false),
            message: Some(format!("Root: {}", target.display())),
            percentage: None,
        };
        self.client
            .send_notification::<ProgressNotification>(ProgressParams {
                token: token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(begin)),
            })
            .await;

        let (tx, rx) = mpsc::unbounded_channel();
        let reporter: sqry_core::progress::SharedReporter =
            Arc::new(index::ChannelProgressReporter::new(tx)) as _;

        let client = self.client.clone();
        let token_clone = token.clone();
        let progress_task = tokio::spawn(async move {
            forward_progress(rx, client, token_clone).await;
        });

        let session = self.sessions.clone();
        let target_clone = target.clone();
        let rebuild = cancel::spawn_blocking(move || {
            index::rebuild_index(&session, &target_clone, &reporter, force)
        });

        // Use ProgressGuard to ensure WorkDoneProgress is always ended
        let guard = ProgressGuard::new(self.client.clone(), token.clone());

        let summary = match rebuild.await {
            Ok(Ok(summary)) => summary,
            Ok(Err(err)) => {
                progress_task.abort();
                // ProgressGuard will handle cleanup via Drop
                guard.end(format!("✗ Index build failed: {err}")).await;
                return Err(map_error(err));
            }
            Err(join_err) => {
                progress_task.abort();
                // ProgressGuard will handle cleanup via Drop
                guard
                    .end("✗ Index build failed (task error)".to_string())
                    .await;
                return Err(map_join_error(&join_err));
            }
        };

        let _ = progress_task.await; // Wait for progress task to complete

        // End progress with success message
        guard
            .end(format!(
                "✓ Indexed {} symbols in {:.2}s",
                summary.total_symbols,
                summary.duration.as_secs_f64()
            ))
            .await;

        // Show completion message that will auto-dismiss after a few seconds
        let () = self
            .client
            .show_message(
                MessageType::INFO,
                format!(
                    "Index built for {} ({} symbols)",
                    target
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("workspace"),
                    summary.total_symbols
                ),
            )
            .await;

        let () = self
            .client
            .telemetry_event(json!({
                "event": "sqry/index",
                "outcome": "success",
                "symbols": summary.total_symbols,
                "durationMs": summary.duration.as_millis(),
            }))
            .await;

        Ok(())
    }
}

/// RAII guard to ensure `WorkDoneProgress` is always ended
///
/// This struct ensures that progress notifications are properly terminated
/// even if the operation fails or panics. When the guard is dropped without
/// explicitly calling `end()`, it logs a warning to help catch missing cleanup.
struct ProgressGuard {
    client: Client,
    token: NumberOrString,
    ended: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ProgressGuard {
    fn new(client: Client, token: NumberOrString) -> Self {
        Self {
            client,
            token,
            ended: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// # Cancellation Safety
    ///
    /// This method is cancellation-safe. It sends a `WorkDoneProgress` End notification
    /// and sets a flag. If cancelled, the Drop implementation will detect the incomplete
    /// state and log a warning.
    async fn end(self, message: String) {
        use std::sync::atomic::Ordering;

        self.ended.store(true, Ordering::SeqCst);

        let end = WorkDoneProgressEnd {
            message: Some(message),
        };
        let () = self
            .client
            .send_notification::<ProgressNotification>(ProgressParams {
                token: self.token.clone(),
                value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(end)),
            })
            .await;
    }
}

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;

        if !self.ended.load(Ordering::SeqCst) {
            // Progress was not properly ended - send End notification to clear spinner
            log::warn!(
                "Progress token {:?} was not properly ended, sending End notification",
                self.token
            );

            // Clone what we need for the spawned task
            let client = self.client.clone();
            let token = self.token.clone();

            // Spawn a task to send the End notification
            // We can't await in Drop, so we fire-and-forget
            // This ensures the client's progress indicator is cleared even on cancellation
            tokio::spawn(async move {
                let end = WorkDoneProgressEnd {
                    message: Some("Operation cancelled".to_string()),
                };
                let () = client
                    .send_notification::<ProgressNotification>(ProgressParams {
                        token,
                        value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(end)),
                    })
                    .await;
            });
        }
    }
}

/// # Cancellation Safety
///
/// This function is cancellation-safe. It forwards progress notifications from a channel
/// to the LSP client. If the task is cancelled (via abort), it stops processing events,
/// which is safe because progress notifications are informational only.
async fn forward_progress(
    mut rx: mpsc::UnboundedReceiver<IndexProgress>,
    client: Client,
    token: NumberOrString,
) {
    let mut state = ProgressState::new();
    while let Some(event) = rx.recv().await {
        if !handle_progress_event(event, &client, &token, &mut state).await {
            break;
        }
    }
}

struct ProgressState {
    last_report: std::time::Instant,
    total_files: Option<usize>,
    current: usize,
}

impl ProgressState {
    fn new() -> Self {
        Self {
            last_report: std::time::Instant::now(),
            total_files: None,
            current: 0,
        }
    }
}

async fn handle_progress_event(
    event: IndexProgress,
    client: &Client,
    token: &NumberOrString,
    state: &mut ProgressState,
) -> bool {
    match event {
        IndexProgress::Started { total_files: total } => {
            handle_started_event(client, token, state, total).await;
        }
        IndexProgress::FileProcessing {
            current: idx,
            total,
            path,
        } => {
            handle_file_processing_event(client, token, state, idx, total, &path).await;
        }
        IndexProgress::FileCompleted { path, .. } => {
            handle_file_completed_event(client, token, state, &path).await;
        }
        IndexProgress::IngestProgress {
            files_processed,
            total_files: total,
            total_symbols,
            eta,
            ..
        } => {
            handle_ingest_progress_event(
                client,
                token,
                state,
                files_processed,
                total,
                total_symbols,
                eta,
            )
            .await;
        }
        IndexProgress::Completed {
            total_symbols,
            duration,
        } => {
            handle_completed_event(client, token, total_symbols, duration).await;
            return false;
        }
        // Graph build and saving events are not handled by LSP progress
        // (LSP only tracks file-level indexing progress)
        _ => {}
    }

    true
}

async fn send_progress_report(
    client: &Client,
    token: &NumberOrString,
    message: String,
    percentage: Option<u32>,
) {
    client
        .send_notification::<ProgressNotification>(ProgressParams {
            token: token.clone(),
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                WorkDoneProgressReport {
                    cancellable: Some(false),
                    message: Some(message),
                    percentage,
                },
            )),
        })
        .await;
}

async fn handle_started_event(
    client: &Client,
    token: &NumberOrString,
    state: &mut ProgressState,
    total_files: usize,
) {
    state.total_files = Some(total_files);
    state.current = 0;
    send_progress_report(
        client,
        token,
        "Scanning project files...".to_string(),
        calc_percentage(state.current, state.total_files),
    )
    .await;
    state.last_report = std::time::Instant::now();
}

async fn handle_file_processing_event(
    client: &Client,
    token: &NumberOrString,
    state: &mut ProgressState,
    current: usize,
    total: usize,
    path: &std::path::Path,
) {
    use std::time::Duration;

    state.total_files = Some(total);
    state.current = current.saturating_sub(1);
    if state.last_report.elapsed() >= Duration::from_millis(200) {
        send_progress_report(
            client,
            token,
            format!("Processing {}", path.display()),
            calc_percentage(state.current, state.total_files),
        )
        .await;
        state.last_report = std::time::Instant::now();
    }
}

async fn handle_file_completed_event(
    client: &Client,
    token: &NumberOrString,
    state: &mut ProgressState,
    path: &std::path::Path,
) {
    use std::time::Duration;

    state.current += 1;
    if state.last_report.elapsed() >= Duration::from_millis(200) {
        send_progress_report(
            client,
            token,
            format!("Indexed {}", path.display()),
            calc_percentage(state.current, state.total_files),
        )
        .await;
        state.last_report = std::time::Instant::now();
    }
}

async fn handle_ingest_progress_event(
    client: &Client,
    token: &NumberOrString,
    state: &mut ProgressState,
    files_processed: usize,
    total_files: usize,
    total_symbols: usize,
    eta: Option<std::time::Duration>,
) {
    use std::fmt::Write;
    use std::time::Duration;

    state.total_files = Some(total_files);
    state.current = files_processed;
    if state.last_report.elapsed() >= Duration::from_millis(200) {
        let mut message = format!(
            "Ingesting symbols: {files_processed}/{total_files} files, {total_symbols} symbols"
        );
        if let Some(eta) = eta {
            let _ = write!(message, " (ETA {}s)", eta.as_secs());
        }
        send_progress_report(
            client,
            token,
            message,
            calc_percentage(state.current, state.total_files),
        )
        .await;
        state.last_report = std::time::Instant::now();
    }
}

async fn handle_completed_event(
    client: &Client,
    token: &NumberOrString,
    total_symbols: usize,
    duration: std::time::Duration,
) {
    send_progress_report(
        client,
        token,
        format!(
            "Indexed {} symbols in {:.2}s",
            total_symbols,
            duration.as_secs_f64()
        ),
        Some(100),
    )
    .await;
}

fn calc_percentage(done: usize, total: Option<usize>) -> Option<u32> {
    total.and_then(|total_files| {
        if total_files == 0 {
            None
        } else {
            let capped = u64::try_from(done.min(total_files)).unwrap_or(u64::MAX);
            let total = u64::try_from(total_files).unwrap_or(u64::MAX);
            // Rounded percentage: (capped / total) * 100
            let numerator = capped.saturating_mul(100).saturating_add(total / 2);
            let percentage =
                u32::try_from((numerator / total).min(u64::from(u32::MAX))).unwrap_or(u32::MAX);
            Some(percentage)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn map_join_error_reports_cancelled_new() {
        let handle = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        handle.abort();
        let err = handle.await.unwrap_err();
        let rpc_error = map_join_error(&err);
        assert_eq!(rpc_error.code, ErrorCode::RequestCancelled);
    }
}
