//! Explain symbol handler for LSP.
//!
//! Provides detailed information about a symbol including context and relations.

use std::path::Path;

use anyhow::Result;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::NodeId;
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::protocol::{SqryExplainSymbolParams, SqryExplainSymbolResult, SqrySearchItem};
use crate::session::SessionManager;

/// Execute explain symbol.
///
/// Returns detailed information about a symbol including callers and callees.
///
/// # Errors
///
/// Returns an error if the workspace path cannot be resolved, inputs are invalid,
/// or the symbol cannot be found.
pub fn execute(
    session: &SessionManager,
    params: &SqryExplainSymbolParams,
) -> Result<SqryExplainSymbolResult> {
    let root = session.resolve_path(params.path.as_deref())?;
    let symbol_name = params.symbol_name.trim();
    let file_path_str = params.file_path.trim();

    if symbol_name.is_empty() {
        anyhow::bail!("symbol_name cannot be empty");
    }
    if file_path_str.is_empty() {
        anyhow::bail!("file_path cannot be empty");
    }

    let include_context = params.include_context.unwrap_or(true);
    let include_relations = params.include_relations.unwrap_or(true);

    log::debug!(
        "Explaining symbol: symbol='{}', file='{}', root={}",
        symbol_name,
        file_path_str,
        root.display()
    );

    // Get graph snapshot
    let graph = session
        .graph()?
        .ok_or_else(|| anyhow::anyhow!("No graph available. Run `sqry index` first."))?;

    let snapshot = graph.snapshot();
    let strings = snapshot.strings();
    let files = snapshot.files();

    // Resolve the file path
    let target_file = root.join(file_path_str);

    // Find the symbol in the graph, preferring matches in the target file
    let node_id = find_symbol_in_file(&snapshot, &root, &target_file, symbol_name)
        .ok_or_else(|| anyhow::anyhow!("Symbol '{symbol_name}' not found in {file_path_str}"))?;

    let entry = snapshot
        .get_node(node_id)
        .ok_or_else(|| anyhow::anyhow!("Symbol node not found in graph"))?;

    // Extract symbol info
    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let qualified_name = entry
        .qualified_name
        .and_then(|id| strings.resolve(id))
        .map_or_else(|| name.clone(), |s| s.to_string());

    let kind = format!("{:?}", entry.kind).to_lowercase();

    let language = files
        .language_for_file(entry.file)
        .map_or("unknown".to_string(), |l| {
            l.to_string().to_ascii_lowercase()
        });

    let file_path = files
        .resolve(entry.file)
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let signature = entry
        .signature
        .and_then(|id| strings.resolve(id))
        .map(|s| s.to_string());

    let documentation = entry
        .doc
        .and_then(|id| strings.resolve(id))
        .map(|s| s.to_string());

    // Collect relations if requested
    let (incoming_calls, outgoing_calls) = if include_context || include_relations {
        let incoming_calls = collect_callers(&snapshot, node_id, &root);
        let outgoing_calls = collect_callees(&snapshot, node_id, &root);
        (incoming_calls, outgoing_calls)
    } else {
        (Vec::new(), Vec::new())
    };

    Ok(SqryExplainSymbolResult {
        name,
        qualified_name,
        kind,
        file_path,
        start_line: entry.start_line,
        end_line: entry.end_line,
        language,
        signature,
        documentation,
        callers: incoming_calls,
        callees: outgoing_calls,
    })
}

/// Find a symbol in the graph, preferring matches in the target file.
fn find_symbol_in_file(
    snapshot: &GraphSnapshot,
    workspace_root: &Path,
    target_file: &Path,
    symbol_name: &str,
) -> Option<NodeId> {
    let files = snapshot.files();

    // Get target file's relative path
    let target_relative = target_file.strip_prefix(workspace_root).ok()?;

    // First, try to find using nodes_by_symbol which returns all nodes with this name
    let candidates = snapshot.nodes_by_symbol(symbol_name);

    // Look for a match in the target file
    for node_id in &candidates {
        if let Some(entry) = snapshot.get_node(*node_id)
            && let Some(file_path) = files.resolve(entry.file)
            && file_path.as_ref() == target_relative
        {
            return Some(*node_id);
        }
    }

    // If no match in target file, return the first candidate
    if !candidates.is_empty() {
        return Some(candidates[0]);
    }

    // Fall back to global name lookup (tries qualified name first)
    snapshot.find_by_name(symbol_name)
}

/// Collect callers of a symbol.
fn collect_callers(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    workspace_root: &Path,
) -> Vec<SqrySearchItem> {
    let strings = snapshot.strings();
    let files = snapshot.files();

    snapshot
        .get_callers(node_id)
        .into_iter()
        .filter_map(|caller_id| {
            let entry = snapshot.get_node(caller_id)?;
            let name = strings.resolve(entry.name)?.to_string();
            let kind = format!("{:?}", entry.kind).to_lowercase();

            let language = files
                .language_for_file(entry.file)
                .map_or("unknown".to_string(), |l| {
                    l.to_string().to_ascii_lowercase()
                });

            let file_path = files.resolve(entry.file)?;
            let full_path = workspace_root.join(file_path.as_ref());
            let uri = Url::from_file_path(&full_path).ok()?;

            let location = Location {
                uri,
                range: Range {
                    start: Position::new(
                        entry.start_line.saturating_sub(1),
                        entry.start_column.saturating_sub(1),
                    ),
                    end: Position::new(
                        entry.end_line.saturating_sub(1),
                        entry.end_column.saturating_sub(1),
                    ),
                },
            };

            let qualified_name = entry
                .qualified_name
                .and_then(|id| strings.resolve(id))
                .map_or_else(|| name.clone(), |s| s.to_string());

            Some(SqrySearchItem {
                name,
                kind,
                qualified_name,
                language,
                location,
                score: None,
            })
        })
        .collect()
}

/// Collect callees of a symbol.
fn collect_callees(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    workspace_root: &Path,
) -> Vec<SqrySearchItem> {
    let strings = snapshot.strings();
    let files = snapshot.files();

    snapshot
        .get_callees(node_id)
        .into_iter()
        .filter_map(|callee_id| {
            let entry = snapshot.get_node(callee_id)?;
            let name = strings.resolve(entry.name)?.to_string();
            let kind = format!("{:?}", entry.kind).to_lowercase();

            let language = files
                .language_for_file(entry.file)
                .map_or("unknown".to_string(), |l| {
                    l.to_string().to_ascii_lowercase()
                });

            let file_path = files.resolve(entry.file)?;
            let full_path = workspace_root.join(file_path.as_ref());
            let uri = Url::from_file_path(&full_path).ok()?;

            let location = Location {
                uri,
                range: Range {
                    start: Position::new(
                        entry.start_line.saturating_sub(1),
                        entry.start_column.saturating_sub(1),
                    ),
                    end: Position::new(
                        entry.end_line.saturating_sub(1),
                        entry.end_column.saturating_sub(1),
                    ),
                },
            };

            let qualified_name = entry
                .qualified_name
                .and_then(|id| strings.resolve(id))
                .map_or_else(|| name.clone(), |s| s.to_string());

            Some(SqrySearchItem {
                name,
                kind,
                qualified_name,
                language,
                location,
                score: None,
            })
        })
        .collect()
}
