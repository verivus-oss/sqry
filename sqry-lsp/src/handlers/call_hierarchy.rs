//! Call hierarchy handlers using `CodeGraph` via `execute_on_graph()`.
//!
//! Provides call hierarchy support for the LSP, finding callers and callees
//! of functions/methods using the unified `CodeGraph`.

use crate::handlers::pause_for_test;
use crate::session::{NodeMatch, SessionManager};
use crate::utils::symbol_kind::node_kind_to_symbol_kind;
use serde::{Deserialize, Serialize};
use sqry_core::graph::unified::NodeKind;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use tower_lsp::lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyIncomingCallsParams, CallHierarchyItem,
    CallHierarchyOutgoingCall, CallHierarchyOutgoingCallsParams, CallHierarchyPrepareParams,
    Position, Range, Url,
};

pub(crate) const CALL_HIERARCHY_VERSION: u32 = 2;
pub(crate) const UNSAVED_MESSAGE: &str = "Save file to enable call hierarchy";

#[derive(Debug)]
pub enum CallHierarchyError {
    IndexMissing,
    InvalidData(String),
    UnsavedBuffer { uri: Url },
    RelationQueryFailed(String),
    SerializationError(String),
}

impl fmt::Display for CallHierarchyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallHierarchyError::IndexMissing => {
                write!(
                    f,
                    "sqry index not found. Build the index to enable call hierarchy."
                )
            }
            CallHierarchyError::InvalidData(reason) => {
                write!(f, "Invalid call hierarchy request payload: {reason}")
            }
            CallHierarchyError::UnsavedBuffer { uri } => {
                write!(f, "Call hierarchy unavailable for unsaved buffer: {uri}")
            }
            CallHierarchyError::RelationQueryFailed(reason) => {
                write!(f, "Relation query failed: {reason}")
            }
            CallHierarchyError::SerializationError(reason) => {
                write!(f, "Serialization error: {reason}")
            }
        }
    }
}

impl std::error::Error for CallHierarchyError {}

pub type Result<T> = std::result::Result<T, CallHierarchyError>;

fn position_from_span(line: usize, column: usize) -> Position {
    Position::new(
        u32::try_from(line).unwrap_or(u32::MAX),
        u32::try_from(column).unwrap_or(u32::MAX),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum CallHierarchyData {
    Saved {
        file_path: PathBuf,
        qualified_name: String,
        language: String,
        start_line: usize,
        start_column: usize,
        #[serde(default = "default_version")]
        version: u32,
    },
    Unsaved {
        file_path: PathBuf,
        qualified_name: String,
        message: String,
        #[serde(default = "default_version")]
        version: u32,
    },
}

fn default_version() -> u32 {
    CALL_HIERARCHY_VERSION
}

#[derive(Debug, Clone)]
pub struct CallHierarchyResponse<T> {
    pub items: Vec<T>,
    pub total: usize,
    pub is_truncated: bool,
}

/// Prepare call hierarchy items for the given location.
///
/// Uses graph builders to find the node at the cursor position. The node
/// is marked as "Saved" if the file exists on disk without unsaved editor
/// changes, or "Unsaved" if the editor has modifications.
///
/// # Errors
///
/// Returns an error if the call hierarchy item cannot be resolved from the
/// current graph state or the request payload is invalid.
pub fn prepare(
    session: &SessionManager,
    params: &CallHierarchyPrepareParams,
) -> Result<Option<Vec<CallHierarchyItem>>> {
    pause_for_test();

    let uri = &params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;

    // Use graph-native node lookup at position
    let node = session
        .node_at(uri, position)
        .map_err(|err| CallHierarchyError::RelationQueryFailed(err.to_string()))?;

    let Some(node) = node else {
        return Ok(None);
    };

    // Check if the document has unsaved changes
    // A document is "unsaved" if it's in the DocumentStore (open in editor)
    // and has modifications that differ from disk
    let file_path = &node.file_path;
    let has_unsaved_changes = session.has_unsaved_changes(file_path);

    if has_unsaved_changes {
        let item = build_unsaved_item(session, &node)?;
        Ok(Some(vec![item]))
    } else {
        let item = build_saved_item(session, &node)?;
        Ok(Some(vec![item]))
    }
}

/// Resolve incoming calls (callers) for a prepared call-hierarchy item.
///
/// # Errors
///
/// Returns an error if the workspace root cannot be resolved, the input item
/// is invalid, or the call hierarchy query fails.
pub fn incoming(
    session: &SessionManager,
    params: &CallHierarchyIncomingCallsParams,
) -> Result<CallHierarchyResponse<CallHierarchyIncomingCall>> {
    pause_for_test();

    let saved_data = parse_saved_request(&params.item)?;
    let workspace_root = session
        .resolve_path(None)
        .map_err(|e| CallHierarchyError::RelationQueryFailed(e.to_string()))?;

    let config = session.config();
    let max_results = config.call_hierarchy.max_results;
    let include_detail = config.call_hierarchy.include_detail;

    // Find ALL target nodes by qualified name to get their NodeIds.
    // This is important because calls may go to stub nodes (in the caller's file)
    // rather than the real definition. We collect incoming edges from all nodes
    // with the same qualified name.
    let executor = session.executor();
    let name_query = format!("name:{}", saved_data.qualified_name);
    let target_results = executor
        .execute_on_graph(&name_query, &workspace_root)
        .map_err(|e| CallHierarchyError::RelationQueryFailed(e.to_string()))?;

    let graph = target_results.graph();
    let all_target_node_ids: Vec<_> = target_results.node_ids().to_vec();

    if all_target_node_ids.is_empty() {
        return Ok(CallHierarchyResponse {
            items: Vec::new(),
            total: 0,
            is_truncated: false,
        });
    }

    // Collect unique incoming edges from ALL matching nodes (real definition + stubs)
    let incoming_edges = collect_unique_incoming_edges(graph, &all_target_node_ids);

    // Filter for Calls edges and group callers
    let (caller_groups, total) =
        build_caller_groups(graph, incoming_edges.into_iter(), max_results);

    let truncated = total > max_results;

    // Build incoming calls
    let mut calls = Vec::new();
    for group in caller_groups.values() {
        if let Some(call) = build_incoming_call(group, include_detail)? {
            calls.push(call);
        }
    }

    Ok(CallHierarchyResponse {
        items: calls,
        total,
        is_truncated: truncated,
    })
}

/// Resolve outgoing calls (callees) for a prepared call-hierarchy item.
///
/// # Errors
///
/// Returns an error if the workspace root cannot be resolved, the input item
/// is invalid, or the call hierarchy query fails.
pub fn outgoing(
    session: &SessionManager,
    params: &CallHierarchyOutgoingCallsParams,
) -> Result<CallHierarchyResponse<CallHierarchyOutgoingCall>> {
    pause_for_test();

    let saved_data = parse_saved_request(&params.item)?;
    let workspace_root = session
        .resolve_path(None)
        .map_err(|e| CallHierarchyError::RelationQueryFailed(e.to_string()))?;

    let config = session.config();
    let max_results = config.call_hierarchy.max_results;
    let include_detail = config.call_hierarchy.include_detail;

    // Find source node by name to get its NodeId
    let executor = session.executor();
    let name_query = format!("name:{}", saved_data.qualified_name);
    let source_results = executor
        .execute_on_graph(&name_query, &workspace_root)
        .map_err(|e| CallHierarchyError::RelationQueryFailed(e.to_string()))?;

    // Find the matching node by file path
    let graph = source_results.graph();
    let source_node_id = find_node_by_file(graph, source_results.node_ids(), &saved_data.file_path);

    let Some(source_node_id) = source_node_id else {
        return Ok(CallHierarchyResponse {
            items: Vec::new(),
            total: 0,
            is_truncated: false,
        });
    };

    // Use the graph we already have
    let outgoing_edges = graph.edges().edges_from(source_node_id);

    // Filter for Calls edges and group callees
    let (callee_groups, total) =
        build_callee_groups(graph, outgoing_edges.into_iter(), max_results);

    let truncated = total > max_results;

    // Build outgoing calls
    let mut calls = Vec::new();
    for group in callee_groups.values() {
        if let Some(call) = build_outgoing_call(group, include_detail)? {
            calls.push(call);
        }
    }

    Ok(CallHierarchyResponse {
        items: calls,
        total,
        is_truncated: truncated,
    })
}

/// Collect unique incoming edges from all target nodes, deduplicating by source node ID.
fn collect_unique_incoming_edges(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    target_node_ids: &[sqry_core::graph::unified::NodeId],
) -> Vec<sqry_core::graph::unified::StoreEdgeRef> {
    let mut seen_callers: std::collections::HashSet<sqry_core::graph::unified::NodeId> =
        std::collections::HashSet::new();
    let mut edges = Vec::new();
    for &target_node_id in target_node_ids {
        for edge in graph.edges().edges_to(target_node_id) {
            if seen_callers.insert(edge.source) {
                edges.push(edge);
            }
        }
    }
    edges
}

/// Find a node from `candidates` whose file matches the given path.
fn find_node_by_file(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    candidates: &[sqry_core::graph::unified::NodeId],
    file_path: &std::path::Path,
) -> Option<sqry_core::graph::unified::NodeId> {
    candidates
        .iter()
        .find(|&&node_id| {
            if let Some(entry) = graph.nodes().get(node_id) {
                let node_file = graph.files().resolve(entry.file);
                node_file.as_ref().is_some_and(|p| p.as_ref() == file_path)
            } else {
                false
            }
        })
        .copied()
}

/// Append call-site ranges from edge spans to a group's call ranges.
fn append_call_ranges(
    group: &mut Vec<Range>,
    edge_spans: &[sqry_core::graph::node::Span],
    fallback_start_line: u32,
    fallback_start_column: u32,
    fallback_end_line: u32,
    fallback_end_column: u32,
) {
    if edge_spans.is_empty() {
        // Fallback: use the node's definition position
        let range = Range::new(
            Position::new(fallback_start_line.saturating_sub(1), fallback_start_column),
            Position::new(fallback_end_line.saturating_sub(1), fallback_end_column),
        );
        if !group.contains(&range) {
            group.push(range);
        }
    } else {
        // Use precise call site spans from edge
        for span in edge_spans {
            let range = Range::new(
                position_from_span(span.start.line, span.start.column),
                position_from_span(span.end.line, span.end.column),
            );
            if !group.contains(&range) {
                group.push(range);
            }
        }
    }
}

/// Filter incoming edges for Calls, building caller groups keyed by name.
fn build_caller_groups(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    edges: impl Iterator<Item = sqry_core::graph::unified::StoreEdgeRef>,
    max_results: usize,
) -> (HashMap<String, CallerGroup>, usize) {
    let mut caller_groups: HashMap<String, CallerGroup> = HashMap::new();
    let mut total = 0;

    for edge in edges {
        if !matches!(
            edge.kind,
            sqry_core::graph::unified::edge::EdgeKind::Calls { .. }
        ) {
            continue;
        }

        total += 1;
        if total > max_results {
            continue; // Still count for total but don't process
        }

        let Some(caller_entry) = graph.nodes().get(edge.source) else {
            continue;
        };

        let name = graph
            .strings()
            .resolve(caller_entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let key = name.clone();

        let file_path = graph
            .files()
            .resolve(caller_entry.file)
            .map(|p| p.to_path_buf());

        let group = caller_groups.entry(key).or_insert_with(|| CallerGroup {
            name: name.clone(),
            kind: caller_entry.kind,
            file_path,
            start_line: caller_entry.start_line,
            start_column: caller_entry.start_column,
            end_line: caller_entry.end_line,
            end_column: caller_entry.end_column,
            language: graph
                .files()
                .language_for_file(caller_entry.file)
                .map(|l| l.to_string()),
            call_ranges: Vec::new(),
        });

        append_call_ranges(
            &mut group.call_ranges,
            &edge.spans,
            caller_entry.start_line,
            caller_entry.start_column,
            caller_entry.end_line,
            caller_entry.end_column,
        );
    }

    (caller_groups, total)
}

/// Filter outgoing edges for Calls, building callee groups keyed by name.
fn build_callee_groups(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    edges: impl Iterator<Item = sqry_core::graph::unified::StoreEdgeRef>,
    max_results: usize,
) -> (HashMap<String, CalleeGroup>, usize) {
    let mut callee_groups: HashMap<String, CalleeGroup> = HashMap::new();
    let mut total = 0;

    for edge in edges {
        if !matches!(
            edge.kind,
            sqry_core::graph::unified::edge::EdgeKind::Calls { .. }
        ) {
            continue;
        }

        total += 1;
        if total > max_results {
            continue; // Still count for total but don't process
        }

        let Some(callee_entry) = graph.nodes().get(edge.target) else {
            continue;
        };

        let name = graph
            .strings()
            .resolve(callee_entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let key = name.clone();

        let file_path = graph
            .files()
            .resolve(callee_entry.file)
            .map(|p| p.to_path_buf());

        let group = callee_groups.entry(key).or_insert_with(|| CalleeGroup {
            name: name.clone(),
            kind: callee_entry.kind,
            file_path,
            start_line: callee_entry.start_line,
            start_column: callee_entry.start_column,
            end_line: callee_entry.end_line,
            end_column: callee_entry.end_column,
            language: graph
                .files()
                .language_for_file(callee_entry.file)
                .map(|l| l.to_string()),
            call_ranges: Vec::new(),
        });

        append_call_ranges(
            &mut group.call_ranges,
            &edge.spans,
            callee_entry.start_line,
            callee_entry.start_column,
            callee_entry.end_line,
            callee_entry.end_column,
        );
    }

    (callee_groups, total)
}

/// Parsed data from a saved `CallHierarchyItem`
struct SavedItemData {
    file_path: PathBuf,
    qualified_name: String,
}

fn parse_saved_request(item: &CallHierarchyItem) -> Result<SavedItemData> {
    let data = parse_data(item)?;
    match data {
        CallHierarchyData::Saved {
            file_path,
            qualified_name,
            ..
        } => Ok(SavedItemData {
            file_path,
            qualified_name,
        }),
        CallHierarchyData::Unsaved { .. } => Err(CallHierarchyError::UnsavedBuffer {
            uri: item.uri.clone(),
        }),
    }
}

fn parse_data(item: &CallHierarchyItem) -> Result<CallHierarchyData> {
    let value = item
        .data
        .clone()
        .ok_or_else(|| CallHierarchyError::InvalidData("missing data field".into()))?;
    let data = serde_json::from_value::<CallHierarchyData>(value)
        .map_err(|err| CallHierarchyError::InvalidData(err.to_string()))?;
    // Allow both v1 and v2 for backward compatibility during migration
    Ok(data)
}

fn build_saved_item(session: &SessionManager, node: &NodeMatch) -> Result<CallHierarchyItem> {
    let file_path = &node.file_path;
    let uri = Url::from_file_path(file_path)
        .map_err(|()| CallHierarchyError::SerializationError("invalid file path".into()))?;

    let range = super::node_range_lsp(session, node)
        .map_err(|err| CallHierarchyError::RelationQueryFailed(err.to_string()))?;

    let language = node
        .language
        .clone()
        .or_else(|| crate::utils::language::infer_language_from_path(file_path))
        .unwrap_or_else(|| "unknown".to_string());

    let data = CallHierarchyData::Saved {
        file_path: file_path.clone(),
        qualified_name: node.qualified_name_or_name().to_string(),
        language,
        start_line: node.start_line as usize,
        start_column: node.start_column as usize,
        version: CALL_HIERARCHY_VERSION,
    };

    let data_value = serde_json::to_value(&data)
        .map_err(|err| CallHierarchyError::SerializationError(err.to_string()))?;

    Ok(CallHierarchyItem {
        name: node.name.clone(),
        kind: node_kind_to_symbol_kind(node.kind),
        tags: None,
        detail: None,
        uri,
        range,
        selection_range: range,
        data: Some(data_value),
    })
}

fn build_unsaved_item(session: &SessionManager, node: &NodeMatch) -> Result<CallHierarchyItem> {
    let file_path = &node.file_path;
    let uri = Url::from_file_path(file_path)
        .map_err(|()| CallHierarchyError::SerializationError("invalid file path".into()))?;

    let range = super::node_range_lsp(session, node)
        .map_err(|err| CallHierarchyError::RelationQueryFailed(err.to_string()))?;

    let data = CallHierarchyData::Unsaved {
        file_path: file_path.clone(),
        qualified_name: node.qualified_name_or_name().to_string(),
        message: UNSAVED_MESSAGE.into(),
        version: CALL_HIERARCHY_VERSION,
    };

    let data_value = serde_json::to_value(&data)
        .map_err(|err| CallHierarchyError::SerializationError(err.to_string()))?;

    Ok(CallHierarchyItem {
        name: node.name.clone(),
        kind: node_kind_to_symbol_kind(node.kind),
        tags: None,
        detail: Some(format!("{UNSAVED_MESSAGE}: {uri}")),
        uri,
        range,
        selection_range: range,
        data: Some(data_value),
    })
}

struct CallerGroup {
    name: String,
    kind: NodeKind,
    file_path: Option<PathBuf>,
    start_line: u32,
    start_column: u32,
    end_line: u32,
    end_column: u32,
    language: Option<String>,
    call_ranges: Vec<Range>,
}

struct CalleeGroup {
    name: String,
    kind: NodeKind,
    file_path: Option<PathBuf>,
    start_line: u32,
    start_column: u32,
    end_line: u32,
    end_column: u32,
    language: Option<String>,
    call_ranges: Vec<Range>,
}

fn build_incoming_call(
    group: &CallerGroup,
    _include_detail: bool,
) -> Result<Option<CallHierarchyIncomingCall>> {
    let Some(ref file_path) = group.file_path else {
        return Ok(None);
    };

    let uri = Url::from_file_path(file_path)
        .map_err(|()| CallHierarchyError::SerializationError("invalid file path".into()))?;

    let range = Range::new(
        Position::new(
            group.start_line.saturating_sub(1),
            group.start_column.saturating_sub(1),
        ),
        Position::new(
            group.end_line.saturating_sub(1),
            group.end_column.saturating_sub(1),
        ),
    );

    let language = group
        .language
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    let data = CallHierarchyData::Saved {
        file_path: file_path.clone(),
        qualified_name: group.name.clone(),
        language,
        start_line: group.start_line as usize,
        start_column: group.start_column as usize,
        version: CALL_HIERARCHY_VERSION,
    };

    let data_value = serde_json::to_value(&data)
        .map_err(|err| CallHierarchyError::SerializationError(err.to_string()))?;

    let item = CallHierarchyItem {
        name: group.name.clone(),
        kind: node_kind_to_symbol_kind(group.kind),
        tags: None,
        detail: None,
        uri,
        range,
        selection_range: range,
        data: Some(data_value),
    };

    let from_ranges = if group.call_ranges.is_empty() {
        vec![range]
    } else {
        group.call_ranges.clone()
    };

    Ok(Some(CallHierarchyIncomingCall {
        from: item,
        from_ranges,
    }))
}

fn build_outgoing_call(
    group: &CalleeGroup,
    _include_detail: bool,
) -> Result<Option<CallHierarchyOutgoingCall>> {
    let Some(ref file_path) = group.file_path else {
        return Ok(None);
    };

    let uri = Url::from_file_path(file_path)
        .map_err(|()| CallHierarchyError::SerializationError("invalid file path".into()))?;

    let range = Range::new(
        Position::new(
            group.start_line.saturating_sub(1),
            group.start_column.saturating_sub(1),
        ),
        Position::new(
            group.end_line.saturating_sub(1),
            group.end_column.saturating_sub(1),
        ),
    );

    let language = group
        .language
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    let data = CallHierarchyData::Saved {
        file_path: file_path.clone(),
        qualified_name: group.name.clone(),
        language,
        start_line: group.start_line as usize,
        start_column: group.start_column as usize,
        version: CALL_HIERARCHY_VERSION,
    };

    let data_value = serde_json::to_value(&data)
        .map_err(|err| CallHierarchyError::SerializationError(err.to_string()))?;

    let item = CallHierarchyItem {
        name: group.name.clone(),
        kind: node_kind_to_symbol_kind(group.kind),
        tags: None,
        detail: None,
        uri,
        range,
        selection_range: range,
        data: Some(data_value),
    };

    let from_ranges = if group.call_ranges.is_empty() {
        vec![range]
    } else {
        group.call_ranges.clone()
    };

    Ok(Some(CallHierarchyOutgoingCall {
        to: item,
        from_ranges,
    }))
}
