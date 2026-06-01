//! `context_propagation` MCP tool — T3.7 (Cluster G).
//!
//! Wraps `sqry-db`'s `ContextPropagationQuery` and exposes the result
//! as a flat list of `ContextLeak` records keyed on caller / callee
//! qualified names + call-site span.
//!
//! Per 02_DESIGN §2.5 + 03_IMPLEMENTATION_PLAN §Cluster G the tool
//! accepts a workspace path, an optional file-scope filter, an
//! optional mode filter, and a `max_results` cap. It dispatches to
//! `ContextPropagationQuery` via the standard `make_query_db_cold`
//! path that every other planner-backed MCP tool uses, so the
//! per-publish cache contract (CLAUDE.md §"Persistence (V10)") is
//! honoured automatically.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};

use sqry_db::queries::context_propagation::{
    ContextLeak, ContextLeakSet, ContextMode, ContextModeFilter, ContextPropagationKey,
    ContextPropagationQuery, ContextScope,
};
use sqry_db::queries::dispatch::make_query_db_cold;

use crate::engine::{canonicalize_in_workspace, engine_for_workspace};
use crate::execution::types::{
    ContextLeakDto, ContextLeakNodeRef, ContextLeakSpan, ContextPropagationData, ToolExecution,
};
use crate::execution::utils::duration_to_ms;
use crate::tools::{ContextPropagationArgs, ContextScopeArg};

/// Executes the `context_propagation` MCP tool.
///
/// The handler is read-only — it never mutates the snapshot — and
/// short-circuits to an empty result set when:
///
/// - `scope` is `File(...)` but the file is not registered in the
///   snapshot's `FileRegistry` (no such file in the index).
/// - `ContextPropagationQuery` returns zero leaks for the given key.
///
/// # Errors
///
/// - The workspace path cannot be resolved or contains a path-traversal
///   attempt.
/// - The unified graph has not been built for this workspace (no
///   `.sqry/graph/`).
pub fn execute_context_propagation(
    args: &ContextPropagationArgs,
) -> Result<ToolExecution<ContextPropagationData>> {
    let start = Instant::now();

    let workspace_path = if args.path == "." {
        None
    } else {
        Some(std::path::PathBuf::from(&args.path))
    };
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _ = canonicalize_in_workspace(&args.path, &workspace_root)?;

    let graph = engine
        .ensure_graph()
        .context("unified graph snapshot is required for context_propagation")?;
    let snapshot = Arc::new(graph.snapshot());

    // Resolve the scope. `Global` is the default. A `File` scope
    // canonicalises the requested path against the workspace root and
    // looks the path up in the snapshot's `FileRegistry`; a non-
    // resolvable file short-circuits to an empty result rather than
    // an error (matches the conservative no-leak path).
    let scope = match &args.scope {
        ContextScopeArg::Global => ContextScope::Global,
        ContextScopeArg::File(path) => {
            let canonical = canonicalize_in_workspace(path, &workspace_root)?;
            let Some(file_id) = snapshot.files().iter().find_map(|(fid, registered)| {
                if registered.as_ref() == canonical.as_path() {
                    Some(fid)
                } else {
                    None
                }
            }) else {
                return Ok(empty_result(&workspace_root, start, &args.path));
            };
            ContextScope::File(file_id)
        }
    };

    let key = ContextPropagationKey {
        scope,
        mode: args.mode,
    };
    let db = make_query_db_cold(Arc::clone(&snapshot), &workspace_root);
    let leak_set: Arc<ContextLeakSet> = db.get::<ContextPropagationQuery>(&key);

    let max = args.max_results;
    let total = leak_set.leaks.len() as u64;
    let truncated = leak_set.leaks.len() > max;
    let mut dtos: Vec<ContextLeakDto> = Vec::with_capacity(leak_set.leaks.len().min(max));
    for leak in leak_set.leaks.iter().take(max) {
        dtos.push(leak_to_dto(&snapshot, leak));
    }

    let data = ContextPropagationData {
        scope: scope_label(&args.scope),
        mode: mode_label(args.mode),
        total,
        truncated,
        leaks: dtos,
    };

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(total),
        truncated: Some(truncated),
        candidates_scanned: None,
        workspace_path: workspace_root.display().to_string(),
    })
}

fn empty_result(
    workspace_root: &std::path::Path,
    start: Instant,
    requested_path: &str,
) -> ToolExecution<ContextPropagationData> {
    ToolExecution {
        data: ContextPropagationData {
            scope: format!("file:{requested_path}"),
            mode: "all".to_string(),
            total: 0,
            truncated: false,
            leaks: Vec::new(),
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(0),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: workspace_root.display().to_string(),
    }
}

fn scope_label(scope: &ContextScopeArg) -> String {
    match scope {
        ContextScopeArg::Global => "global".to_string(),
        ContextScopeArg::File(p) => format!("file:{p}"),
    }
}

fn mode_label(mode: ContextModeFilter) -> String {
    match mode {
        ContextModeFilter::All => "all",
        ContextModeFilter::BreakSite => "break_site",
        ContextModeFilter::UnthreadedGoroutine => "unthreaded_goroutine",
        ContextModeFilter::HttpHandlerLeak => "http_handler_leak",
    }
    .to_string()
}

fn leak_to_dto(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    leak: &ContextLeak,
) -> ContextLeakDto {
    let caller = node_label(snapshot, leak.caller);
    let callee = node_label(snapshot, leak.callee);
    let caller_file = snapshot
        .get_node(leak.caller)
        .and_then(|entry| snapshot.files().resolve(entry.file))
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    // Span coords: lines are tree-sitter 0-indexed; expose 1-based
    // for IDE-friendly jump-to. Columns are byte offsets (matching
    // tree-sitter's `Point::column` shape). Saturating casts guard
    // against the (impossible-in-practice) > u32 source line count.
    let call_span = ContextLeakSpan {
        start_line: leak
            .call_span
            .start
            .line
            .saturating_add(1)
            .min(u32::MAX as usize) as u32,
        start_column: leak.call_span.start.column.min(u32::MAX as usize) as u32,
        end_line: leak
            .call_span
            .end
            .line
            .saturating_add(1)
            .min(u32::MAX as usize) as u32,
        end_column: leak.call_span.end.column.min(u32::MAX as usize) as u32,
    };
    let caller_ctx_param = leak.caller_ctx_param.map(|nid| ContextLeakNodeRef {
        index: nid.index(),
        generation: nid.generation(),
    });
    ContextLeakDto {
        mode: mode_concrete_label(leak.mode),
        caller,
        callee,
        caller_file,
        call_span,
        caller_ctx_param,
    }
}

fn node_label(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node: sqry_core::graph::unified::node::NodeId,
) -> String {
    let Some(entry) = snapshot.get_node(node) else {
        return String::new();
    };
    if let Some(sid) = entry.qualified_name
        && let Some(qualified) = snapshot.strings().resolve(sid)
    {
        return qualified.to_string();
    }
    snapshot
        .strings()
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default()
}

fn mode_concrete_label(mode: ContextMode) -> String {
    match mode {
        ContextMode::BreakSite => "break_site",
        ContextMode::UnthreadedGoroutine => "unthreaded_goroutine",
        ContextMode::HttpHandlerLeak => "http_handler_leak",
    }
    .to_string()
}
