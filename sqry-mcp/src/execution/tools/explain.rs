//! Explain code tool execution.
//!
//! This module implements the `explain_code` tool which provides detailed
//! information about a symbol including documentation, context, and relations.
//!
//! # Architecture
//!
//! Uses the shared unified-graph resolver for strict file-aware lookup, then
//! pulls relations from graph convenience methods.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Result, anyhow};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::{FileScope, ResolutionMode, SymbolQuery, SymbolResolutionOutcome};

use crate::engine::{canonicalize_in_workspace, engine_for_workspace};
use crate::execution::graph_builders::build_graph_metadata;
use crate::tools::ExplainCodeArgs;

use crate::execution::location::node_location_for_reporting_snapshot;
use crate::execution::symbol_utils::{build_context, get_classpath_provenance_for_node};
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
    // Issue #394: resolve a subdirectory `path` to its owning workspace instead
    // of failing as if it were a workspace root. Subtree scoping for list/scan
    // tools is applied separately via `resolve_workspace_scope`.
    crate::execution::workspace_scope::resolve_workspace_selector(path)
        .ok()
        .flatten()
}
/// Execute the `explain_code` tool.
///
/// # Errors
///
/// Returns an error if workspace resolution, graph acquisition, file
/// canonicalization, or symbol explanation fails.
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
    )?;

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
fn explain_symbol_unified(
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
    target_file: &Path,
    symbol_name: &str,
    include_context: bool,
    include_relations: bool,
) -> Result<ExplainCodeData> {
    let node_id = resolve_explain_symbol_unified(snapshot, target_file, symbol_name)?;

    let Some(entry) = snapshot.get_node(node_id) else {
        return Err(anyhow!("Symbol node not found in graph"));
    };

    let files = snapshot.files();

    // Build symbol reference using shared helper
    let node_ref = build_node_ref_from_entry(entry, node_id, snapshot, workspace_root);

    // Build context (source code around the symbol)
    let file_path = files
        .resolve(entry.file)
        .map(|arc_path| workspace_root.join(arc_path.as_ref()))
        .unwrap_or_default();

    // NOTE: These raw entry spans are used for character-offset math (source code extraction),
    // not for user-visible line/column display. Do NOT migrate to node_location_for_reporting.
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

    let provenance = get_classpath_provenance_for_node(snapshot, node_id);

    Ok(ExplainCodeData {
        symbol: node_ref,
        documentation,
        context,
        relations,
        provenance,
    })
}

fn resolve_explain_symbol_unified(
    snapshot: &GraphSnapshot,
    target_file: &Path,
    symbol_name: &str,
) -> Result<NodeId> {
    let query = SymbolQuery {
        symbol: symbol_name,
        file_scope: FileScope::Path(target_file),
        mode: ResolutionMode::Strict,
    };

    match snapshot.resolve_symbol(&query) {
        SymbolResolutionOutcome::Resolved(node_id) => Ok(node_id),
        SymbolResolutionOutcome::NotFound => Err(anyhow!(
            "Symbol '{symbol_name}' not found in {}",
            target_file.display()
        )),
        SymbolResolutionOutcome::FileNotIndexed => {
            Err(anyhow!("File '{}' is not indexed", target_file.display()))
        }
        SymbolResolutionOutcome::Ambiguous(_) => Err(anyhow!(
            "Symbol '{symbol_name}' is ambiguous in {}",
            target_file.display()
        )),
    }
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
    node_id: NodeId,
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
) -> NodeRefData {
    let strings = snapshot.strings();
    let files = snapshot.files();

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let qualified_name =
        crate::execution::symbol_utils::display_entry_qualified_name(entry, strings, files, &name);

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

    let loc = node_location_for_reporting_snapshot(snapshot, node_id, workspace_root);
    let resolution_source = loc.as_ref().map(|l| format!("{:?}", l.resolution_source));

    NodeRefData {
        name,
        qualified_name,
        kind: kind.to_string(),
        language,
        file_uri,
        range: crate::execution::types::RangeData {
            start: crate::execution::types::PositionData {
                line: loc.as_ref().map_or(entry.start_line, |l| l.line),
                character: loc.as_ref().map_or(entry.start_column, |l| l.column),
            },
            end: crate::execution::types::PositionData {
                line: loc.as_ref().map_or(entry.end_line, |l| l.end_line),
                character: loc.as_ref().map_or(entry.end_column, |l| l.end_column),
            },
        },
        metadata: None,
        resolution_source,
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
        .filter_map(|caller_id| {
            let entry = snapshot.get_node(caller_id)?;
            Some(build_node_ref_from_entry(
                entry,
                caller_id,
                snapshot,
                workspace_root,
            ))
        })
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
        .filter_map(|callee_id| {
            let entry = snapshot.get_node(callee_id)?;
            Some(build_node_ref_from_entry(
                entry,
                callee_id,
                snapshot,
                workspace_root,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;

    use super::resolve_explain_symbol_unified;

    fn test_workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn resolve_explain_symbol_unified_prefers_requested_file() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let symbol_name = graph.strings_mut().intern("main").unwrap();
        let requested_file = graph
            .files_mut()
            .register(&workspace_root.join("sqry-mcp/src/main.rs"))
            .unwrap();
        let other_file = graph
            .files_mut()
            .register(&workspace_root.join("archive/main.rs"))
            .unwrap();

        let requested_node = graph
            .nodes_mut()
            .alloc(NodeEntry::new(
                NodeKind::Function,
                symbol_name,
                requested_file,
            ))
            .unwrap();
        let other_node = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, symbol_name, other_file))
            .unwrap();

        graph.indices_mut().add(
            requested_node,
            NodeKind::Function,
            symbol_name,
            None,
            requested_file,
        );
        graph.indices_mut().add(
            other_node,
            NodeKind::Function,
            symbol_name,
            None,
            other_file,
        );

        let snapshot = graph.snapshot();
        let resolved = resolve_explain_symbol_unified(
            &snapshot,
            &workspace_root.join("sqry-mcp/src/main.rs"),
            "main",
        )
        .unwrap();

        assert_eq!(resolved, requested_node);
    }

    #[test]
    fn resolve_explain_symbol_unified_supports_qualified_name_lookup() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let symbol_name = graph.strings_mut().intern("main").unwrap();
        let qualified_name = graph.strings_mut().intern("sqry_mcp::main").unwrap();
        let file_id = graph
            .files_mut()
            .register(&workspace_root.join("sqry-mcp/src/main.rs"))
            .unwrap();

        let node = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, symbol_name, file_id)
                    .with_qualified_name(qualified_name),
            )
            .unwrap();

        graph.indices_mut().add(
            node,
            NodeKind::Function,
            symbol_name,
            Some(qualified_name),
            file_id,
        );

        let snapshot = graph.snapshot();
        let resolved = resolve_explain_symbol_unified(
            &snapshot,
            &workspace_root.join("sqry-mcp/src/main.rs"),
            "sqry_mcp::main",
        )
        .unwrap();

        assert_eq!(resolved, node);
    }

    #[test]
    fn resolve_explain_symbol_unified_returns_not_found_for_wrong_file() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let symbol_name = graph.strings_mut().intern("main").unwrap();
        let requested_file = graph
            .files_mut()
            .register(&workspace_root.join("sqry-mcp/src/main.rs"))
            .unwrap();
        let other_file = graph
            .files_mut()
            .register(&workspace_root.join("archive/main.rs"))
            .unwrap();
        let anchor_name = graph.strings_mut().intern("anchor").unwrap();

        let requested_anchor = graph
            .nodes_mut()
            .alloc(NodeEntry::new(
                NodeKind::Function,
                anchor_name,
                requested_file,
            ))
            .unwrap();
        let other_node = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, symbol_name, other_file))
            .unwrap();

        graph.indices_mut().add(
            requested_anchor,
            NodeKind::Function,
            anchor_name,
            None,
            requested_file,
        );
        graph.indices_mut().add(
            other_node,
            NodeKind::Function,
            symbol_name,
            None,
            other_file,
        );

        let snapshot = graph.snapshot();
        let err = resolve_explain_symbol_unified(
            &snapshot,
            &workspace_root.join("sqry-mcp/src/main.rs"),
            "main",
        )
        .unwrap_err();

        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn resolve_explain_symbol_unified_returns_file_not_indexed() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let indexed_file = graph
            .files_mut()
            .register(&workspace_root.join("sqry-mcp/src/main.rs"))
            .unwrap();
        let unindexed_path = workspace_root.join("sqry-mcp/src/not_indexed.rs");
        graph.files_mut().register(&unindexed_path).unwrap();
        let symbol_name = graph.strings_mut().intern("main").unwrap();

        let node = graph
            .nodes_mut()
            .alloc(NodeEntry::new(
                NodeKind::Function,
                symbol_name,
                indexed_file,
            ))
            .unwrap();
        graph
            .indices_mut()
            .add(node, NodeKind::Function, symbol_name, None, indexed_file);

        let snapshot = graph.snapshot();
        let err = resolve_explain_symbol_unified(&snapshot, &unindexed_path, "main").unwrap_err();

        assert!(err.to_string().contains("not indexed"));
    }
}
