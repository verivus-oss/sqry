//! `daemon/query` JSON-RPC handler for revision-aware structural queries.
//!
//! The handler deliberately reuses `sqry-core`'s query executor against an
//! already acquired graph. Omitted revision targets use the existing live
//! workspace acquisition path; explicit targets query only resident revision
//! handles and attach provenance to the response.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::Value;
use sqry_core::graph::CodeGraph;
use sqry_core::query::QueryExecutor;
use sqry_daemon_protocol::{QueryRequest, QueryResult, RevisionQueryTarget, SearchItem};

use super::super::protocol::{ResponseEnvelope, ResponseMeta};
use super::super::tool_core;
use super::{HandlerContext, MethodError, daemon_load_revision};

const DEFAULT_QUERY_LIMIT: usize = 1_000;

/// Handle one `daemon/query` request.
pub(crate) async fn handle(ctx: &HandlerContext, params: Value) -> Result<Value, MethodError> {
    let req: QueryRequest = match params {
        Value::Null => {
            return Err(MethodError::InvalidParams(serde::de::Error::custom(
                "daemon/query requires params",
            )));
        }
        other => serde_json::from_value(other).map_err(MethodError::InvalidParams)?,
    };
    validate_request(&req)?;

    if let Some(target) = req.revision.clone() {
        return handle_revision_query(ctx, req, target).await;
    }

    let path = req.search_path.clone();
    let tool_timeout = Duration::from_secs(ctx.config.tool_timeout_secs);
    let verdict = tool_core::acquire_and_execute(
        Arc::clone(&ctx.manager),
        Arc::clone(&ctx.workspace_builder),
        Arc::clone(&ctx.tool_executor),
        &ctx.cpu_executor,
        tool_timeout,
        &path,
        Some("daemon/query"),
        move |wctx, cancel| -> anyhow::Result<Value> {
            let result =
                run_query_on_graph(&wctx.graph, &req, Path::new(&req.search_path), cancel)?;
            serde_json::to_value(&result).context("serialise QueryResult")
        },
    )
    .await
    .map_err(MethodError::Daemon)?;

    match verdict {
        tool_core::ExecuteVerdict::Fresh { inner, state } => {
            let envelope = ResponseEnvelope {
                result: inner,
                meta: ResponseMeta::fresh_from(state, ctx.daemon_version),
            };
            serde_json::to_value(&envelope)
                .map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
        }
        tool_core::ExecuteVerdict::Stale {
            inner,
            last_good_at,
            last_error,
            ..
        } => {
            let envelope = ResponseEnvelope {
                result: inner,
                meta: ResponseMeta::stale_from(last_good_at, last_error, ctx.daemon_version),
            };
            serde_json::to_value(&envelope)
                .map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
        }
    }
}

async fn handle_revision_query(
    ctx: &HandlerContext,
    req: QueryRequest,
    target: RevisionQueryTarget,
) -> Result<Value, MethodError> {
    // #566: guard the direct-canonicalize revision branch (bypasses the
    // `resolve_path` funnel) so a relative path is not resolved against the
    // daemon's own CWD.
    crate::ipc::path_policy::ensure_absolute_workspace_path(Path::new(&req.search_path)).map_err(
        |reason| MethodError::Daemon(crate::error::DaemonError::InvalidArgument { reason }),
    )?;
    let root = PathBuf::from(&req.search_path)
        .canonicalize()
        .map_err(|err| {
            MethodError::Daemon(crate::error::DaemonError::InvalidArgument {
                reason: format!(
                    "daemon/query revision target path {} could not be canonicalized: {err}",
                    req.search_path
                ),
            })
        })?;
    let (revision_id, metadata) = daemon_load_revision::resolve_query_target(ctx, &root, &target)?;
    let guard = ctx.manager.acquire_resident_query(&revision_id)?;
    let graph = guard.graph().ok_or_else(|| {
        MethodError::Daemon(crate::error::DaemonError::RevisionSourceUnavailable {
            reason: format!("resident revision {} had no loaded graph", revision_id.0),
            path: Some(root.clone()),
        })
    })?;
    let tool_timeout = Duration::from_secs(ctx.config.tool_timeout_secs);
    let query_root = root.clone();

    // issue #503 Phase 2: submit the query on the shared dedicated CPU pool.
    // `CpuExecutor::run` owns the token, flips it on deadline (fire-and-forget),
    // and maps the result through the same `classify_closure_error` ladder the
    // central path uses, so the ToolTimeout / envelope shape is unchanged. The
    // async caller holds no Tokio blocking thread while the query runs.
    let mut result = ctx
        .cpu_executor
        .run(tool_timeout, &root, move |cancel| {
            run_query_on_graph(&graph, &req, &query_root, cancel)
        })
        .await
        .map_err(MethodError::Daemon)?;
    result.revision = Some(metadata);
    drop(guard);

    let envelope = ResponseEnvelope {
        result,
        meta: ResponseMeta::fresh_from(
            crate::workspace::WorkspaceState::Loaded,
            ctx.daemon_version,
        ),
    };
    serde_json::to_value(&envelope).map_err(|err| MethodError::Internal(anyhow::Error::new(err)))
}

fn validate_request(req: &QueryRequest) -> Result<(), MethodError> {
    if req.query.trim().is_empty() {
        return Err(MethodError::InvalidParams(serde::de::Error::custom(
            "daemon/query: query must not be empty",
        )));
    }
    if let Some(0) = req.limit {
        return Err(MethodError::InvalidParams(serde::de::Error::custom(
            "daemon/query: limit must be greater than zero",
        )));
    }
    Ok(())
}

fn run_query_on_graph(
    graph: &Arc<CodeGraph>,
    req: &QueryRequest,
    workspace_root: &Path,
    cancel: &sqry_core::query::cancellation::CancellationToken,
) -> anyhow::Result<QueryResult> {
    let executor =
        QueryExecutor::with_plugin_manager(sqry_plugin_registry::create_plugin_manager());
    // Cancellable variant so a deadline-flipped token reaches the
    // evaluator's per-batch poll. `None` is the `variables` argument, not a
    // token (issue #503 Phase 1). This helper takes the token in place
    // rather than via a never-cancelled wrapper because daemon_query.rs has
    // no in-crate test callers, so a wrapper would be dead code.
    let query_results = executor.execute_on_preloaded_graph_cancellable(
        Arc::clone(graph),
        &req.query,
        workspace_root,
        None,
        cancel,
    )?;
    let total = query_results.len();
    let limit = req.limit.map_or(DEFAULT_QUERY_LIMIT, |limit| {
        usize::try_from(limit).unwrap_or(usize::MAX)
    });
    let items = query_results
        .iter()
        .take(limit)
        .map(|query_match| query_match_to_search_item(&query_match))
        .collect();

    Ok(QueryResult {
        items,
        total: u64::try_from(total).unwrap_or(u64::MAX),
        truncated: total > limit,
        revision: None,
    })
}

fn query_match_to_search_item(
    query_match: &sqry_core::query::results::QueryMatch<'_>,
) -> SearchItem {
    let name = query_match
        .name()
        .map(|name| name.to_string())
        .unwrap_or_default();
    let qualified_name = query_match
        .qualified_name()
        .map_or_else(|| name.clone(), |qualified| qualified.to_string());
    let file_path = query_match
        .file_path()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let language = query_match.language().map_or_else(
        || "unknown".to_owned(),
        |language| language.to_string().to_ascii_lowercase(),
    );

    SearchItem {
        name,
        qualified_name,
        kind: query_match.kind().as_str().to_owned(),
        language,
        file_path,
        start_line: query_match.start_line(),
        start_column: query_match.start_column(),
        end_line: query_match.end_line(),
        end_column: query_match.end_column(),
        score: None,
    }
}
