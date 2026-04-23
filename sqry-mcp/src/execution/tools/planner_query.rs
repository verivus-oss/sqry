//! `sqry_query` MCP tool — structural query execution through the sqry-db
//! planner pipeline (parse → compile → fuse → execute).
//!
//! DB13 scope: this tool is parallel to the legacy `run_query` CLI path. DB14+
//! migrates the traversal handlers onto the planner; once migration completes
//! the legacy path is deleted.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};

use sqry_db::planner::{execute_plan, parse_query};
use sqry_db::queries::dispatch::make_query_db_cold;

use crate::engine::{canonicalize_in_workspace, engine_for_workspace};
use crate::execution::types::{SqryQueryData, SqryQueryHit, ToolExecution};
use crate::execution::utils::duration_to_ms;
use crate::tools::SqryQueryParams;

/// Default upper bound when the client omits `limit`. Mirrors the CLI default
/// so both frontends behave identically.
const DEFAULT_LIMIT: usize = 1_000;

/// Upper cap on the limit parameter — prevents a single tool call from
/// serialising tens of thousands of results into the MCP channel.
const MAX_LIMIT: usize = 10_000;

/// Executes the `sqry_query` tool against the current workspace graph.
///
/// # Errors
///
/// - If the workspace has no unified graph (`.sqry/graph/`).
/// - If the text query fails to parse or the resulting plan fails validation.
pub fn execute_sqry_query(params: &SqryQueryParams) -> Result<ToolExecution<SqryQueryData>> {
    let start = Instant::now();

    let workspace_path = if params.path == "." {
        None
    } else {
        Some(std::path::PathBuf::from(&params.path))
    };
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    // Guard against path traversal — same pattern other tools use.
    let _ = canonicalize_in_workspace(&params.path, &workspace_root)?;

    let graph = engine
        .ensure_graph()
        .context("unified graph snapshot is required for sqry_query")?;

    let plan =
        parse_query(&params.query).map_err(|err| anyhow::anyhow!("query parse error: {err}"))?;

    let snapshot = Arc::new(graph.snapshot());
    let db = make_query_db_cold(Arc::clone(&snapshot), &workspace_root);

    let node_ids = execute_plan(&plan, &db);
    let total_matches = node_ids.len() as u64;

    let limit = params
        .limit
        .map(|n| n as usize)
        .unwrap_or(DEFAULT_LIMIT)
        .min(MAX_LIMIT);

    let truncated = node_ids.len() > limit;
    let mut hits: Vec<SqryQueryHit> = Vec::with_capacity(node_ids.len().min(limit));
    for node_id in node_ids.into_iter().take(limit) {
        let Some(entry) = snapshot.nodes().get(node_id) else {
            continue;
        };
        let strings = snapshot.strings();
        let files = snapshot.files();
        let name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let qualified_name = entry
            .qualified_name
            .and_then(|sid| strings.resolve(sid))
            .map_or_else(|| name.clone(), |s| s.to_string());
        let file = files
            .resolve(entry.file)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let visibility = entry
            .visibility
            .and_then(|sid| strings.resolve(sid))
            .map(|s| s.to_string());

        hits.push(SqryQueryHit {
            name,
            qualified_name,
            kind: entry.kind.as_str().to_string(),
            file,
            line: entry.start_line,
            visibility,
        });
    }

    let data = SqryQueryData {
        query: params.query.clone(),
        total_matches,
        truncated,
        hits,
    };

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(total_matches),
        truncated: Some(truncated),
        candidates_scanned: None,
        workspace_path: workspace_root.display().to_string(),
    })
}
