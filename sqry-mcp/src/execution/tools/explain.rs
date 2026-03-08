//! Explain code tool execution.
//!
//! This module implements the `explain_code` tool which provides detailed
//! information about a symbol including documentation, context, and relations.
//!
//! # Architecture
//!
//! Uses `CodeGraph` exclusively for:
//! - Symbol lookup via `nodes_by_symbol()` and file-based search
//! - Relation queries via `get_callers()` and `get_callees()` convenience methods

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, anyhow};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;

use crate::engine::{canonicalize_in_workspace, engine_for_workspace};
use crate::execution::graph_builders::build_graph_metadata;
use crate::tools::ExplainCodeArgs;

use crate::execution::symbol_utils::build_context;
use crate::execution::types::{ExplainCodeData, ExplainRelations, NodeRefData, ToolExecution};
use crate::execution::utils::duration_to_ms;

/// Execute the `explain_code` tool to provide detailed symbol information.
///
/// Uses `CodeGraph` exclusively for symbol lookup and relation queries.
/// Resolve workspace path from args.path parameter.
///
/// If path is "." (default), returns None to trigger discovery.
/// Otherwise returns Some(path) for explicit workspace resolution.
fn resolve_workspace_path(path: &str) -> Option<PathBuf> {
    if path == "." {
        None
    } else {
        Some(PathBuf::from(path))
    }
}
pub fn execute_explain_code(args: &ExplainCodeArgs) -> Result<ToolExecution<ExplainCodeData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _base = canonicalize_in_workspace(&args.path, &workspace_root)?;
    let file_path = canonicalize_in_workspace(&args.file_path, &workspace_root)?;

    tracing::debug!(
        file_path = %args.file_path,
        symbol = %args.symbol_name,
        include_context = args.include_context,
        include_relations = args.include_relations,
        "Executing explain_code tool"
    );

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();

    // Find and explain the symbol using unified graph
    let result = explain_symbol_unified(
        &snapshot,
        &workspace_root,
        &file_path,
        &args.symbol_name,
        args.include_context,
        args.include_relations,
    )?
    .ok_or_else(|| {
        anyhow!(
            "Symbol '{}' not found in {}",
            args.symbol_name,
            args.file_path
        )
    })?;

    let graph_metadata = build_graph_metadata(Some(&workspace_root), Some(&snapshot), None);

    Ok(ToolExecution {
        data: result,
        used_index: false,
        used_graph: true,
        graph_metadata: Some(graph_metadata),
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(1),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

fn build_context_if_requested(
    include_context: bool,
    file_path: &Path,
    start_line: usize,
    end_line: usize,
) -> Result<Option<crate::execution::types::CodeContext>> {
    if include_context {
        build_context(file_path, start_line, end_line, 3)
    } else {
        Ok(None)
    }
}

fn build_explain_relations(
    incoming_calls: Vec<NodeRefData>,
    outgoing_calls: Vec<NodeRefData>,
) -> Option<ExplainRelations> {
    if incoming_calls.is_empty() && outgoing_calls.is_empty() {
        return None;
    }

    Some(ExplainRelations {
        callers: if incoming_calls.is_empty() {
            None
        } else {
            Some(incoming_calls)
        },
        callees: if outgoing_calls.is_empty() {
            None
        } else {
            Some(outgoing_calls)
        },
    })
}

/// Explain a symbol using the unified graph.
///
/// Returns `Ok(Some(...))` if symbol found, `Ok(None)` if not found in graph.
fn explain_symbol_unified(
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
    target_file: &Path,
    symbol_name: &str,
    include_context: bool,
    include_relations: bool,
) -> Result<Option<ExplainCodeData>> {
    // Find the symbol by name, preferring matches in the target file
    let node_id = find_symbol_in_file_unified(snapshot, workspace_root, target_file, symbol_name);

    let Some(node_id) = node_id else {
        return Ok(None);
    };

    let Some(entry) = snapshot.get_node(node_id) else {
        return Ok(None);
    };

    let files = snapshot.files();

    // Build symbol reference using shared helper
    let node_ref = build_node_ref_from_entry(entry, snapshot, workspace_root);

    // Build context (source code around the symbol)
    let file_path = files
        .resolve(entry.file)
        .map(|arc_path| workspace_root.join(arc_path.as_ref()))
        .unwrap_or_default();

    let context = build_context_if_requested(
        include_context,
        &file_path,
        entry.start_line as usize,
        entry.end_line as usize,
    )?;

    // Collect relations from unified graph
    let relations = if include_relations {
        let incoming_calls = collect_callers_unified(snapshot, node_id, workspace_root);
        let outgoing_calls = collect_callees_unified(snapshot, node_id, workspace_root);

        build_explain_relations(incoming_calls, outgoing_calls)
    } else {
        None
    };

    // Note: Documentation is not stored in the unified graph yet
    // This would require extending the node metadata
    let documentation = None;

    Ok(Some(ExplainCodeData {
        symbol: node_ref,
        documentation,
        context,
        relations,
    }))
}

/// Find a symbol in the unified graph, preferring matches in the target file.
fn find_symbol_in_file_unified(
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
    target_file: &Path,
    symbol_name: &str,
) -> Option<NodeId> {
    let files = snapshot.files();

    // Get all nodes matching the symbol name
    let matches = snapshot.nodes_by_symbol(symbol_name);

    if matches.is_empty() {
        return None;
    }

    // Prefer a match in the target file
    let relative_target = target_file
        .strip_prefix(workspace_root)
        .unwrap_or(target_file);

    for node_id in &matches {
        if let Some(entry) = snapshot.get_node(*node_id)
            && let Some(node_file) = files.resolve(entry.file)
            && node_file.as_ref() == relative_target
        {
            return Some(*node_id);
        }
    }

    // If no match in target file, return first match
    Some(matches[0])
}

/// Convert a `NodeKind` to its string representation.
fn node_kind_to_str(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Class => "class",
        NodeKind::Module => "module",
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Interface => "interface",
        NodeKind::Trait => "trait",
        NodeKind::Method => "method",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::Type => "type",
        _ => "function",
    }
}

/// Build a `NodeRefData` from a unified graph node entry.
fn build_node_ref_from_entry(
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
) -> NodeRefData {
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

    let kind = node_kind_to_str(entry.kind);

    let language = files
        .language_for_file(entry.file)
        .map_or_else(|| "unknown".to_string(), |l| l.to_string());

    let file_path = files
        .resolve(entry.file)
        .map(|arc_path| workspace_root.join(arc_path.as_ref()))
        .unwrap_or_default();

    let file_uri = url::Url::from_file_path(&file_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&file_path),
        Into::into,
    );

    NodeRefData {
        name,
        qualified_name,
        kind: kind.to_string(),
        language,
        file_uri,
        range: crate::execution::types::RangeData {
            start: crate::execution::types::PositionData {
                line: entry.start_line,
                character: entry.start_column,
            },
            end: crate::execution::types::PositionData {
                line: entry.end_line,
                character: entry.end_column,
            },
        },
        metadata: None,
    }
}

/// Collect callers from the unified graph.
fn collect_callers_unified(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    workspace_root: &Path,
) -> Vec<NodeRefData> {
    snapshot
        .get_callers(node_id)
        .into_iter()
        .filter_map(|caller_id| snapshot.get_node(caller_id))
        .map(|entry| build_node_ref_from_entry(entry, snapshot, workspace_root))
        .collect()
}

/// Collect callees from the unified graph.
fn collect_callees_unified(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    workspace_root: &Path,
) -> Vec<NodeRefData> {
    snapshot
        .get_callees(node_id)
        .into_iter()
        .filter_map(|callee_id| snapshot.get_node(callee_id))
        .map(|entry| build_node_ref_from_entry(entry, snapshot, workspace_root))
        .collect()
}
