//! Explain symbol handler for LSP.
//!
//! Provides detailed information about a symbol including context and relations.

use std::path::Path;

use anyhow::Result;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::{FileScope, ResolutionMode, SymbolQuery, SymbolResolutionOutcome};
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

    let node_id = resolve_explain_symbol(&snapshot, &target_file, symbol_name)?;

    let entry = snapshot
        .get_node(node_id)
        .ok_or_else(|| anyhow::anyhow!("Symbol node not found in graph"))?;

    // Extract symbol info
    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();

    let qualified_name =
        crate::conversion::display_entry_qualified_name(entry, strings, files, &name);

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

fn resolve_explain_symbol(
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
        SymbolResolutionOutcome::NotFound => {
            anyhow::bail!(
                "Symbol '{symbol_name}' not found in {}",
                target_file.display()
            )
        }
        SymbolResolutionOutcome::FileNotIndexed => {
            anyhow::bail!("File '{}' is not indexed", target_file.display())
        }
        SymbolResolutionOutcome::Ambiguous(_) => {
            anyhow::bail!(
                "Symbol '{symbol_name}' is ambiguous in {}",
                target_file.display()
            )
        }
    }
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

            let qualified_name =
                crate::conversion::display_entry_qualified_name(entry, strings, files, &name);

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

            let qualified_name =
                crate::conversion::display_entry_qualified_name(entry, strings, files, &name);

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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::node::NodeKind;
    use sqry_core::graph::unified::storage::arena::NodeEntry;

    use super::resolve_explain_symbol;

    fn test_workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn resolve_explain_symbol_prefers_requested_file() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let symbol_name = graph.strings_mut().intern("main").unwrap();
        let requested_file = graph
            .files_mut()
            .register(&workspace_root.join("sqry-lsp/src/main.rs"))
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
        let resolved = resolve_explain_symbol(
            &snapshot,
            &workspace_root.join("sqry-lsp/src/main.rs"),
            "main",
        )
        .unwrap();

        assert_eq!(resolved, requested_node);
    }

    #[test]
    fn resolve_explain_symbol_returns_not_found_for_wrong_file() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let symbol_name = graph.strings_mut().intern("main").unwrap();
        let requested_file = graph
            .files_mut()
            .register(&workspace_root.join("sqry-lsp/src/main.rs"))
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
        let err = resolve_explain_symbol(
            &snapshot,
            &workspace_root.join("sqry-lsp/src/main.rs"),
            "main",
        )
        .unwrap_err();

        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn resolve_explain_symbol_returns_file_not_indexed() {
        let mut graph = CodeGraph::new();
        let workspace_root = test_workspace_root();
        let indexed_file = graph
            .files_mut()
            .register(&workspace_root.join("sqry-lsp/src/main.rs"))
            .unwrap();
        let unindexed_path = workspace_root.join("sqry-lsp/src/not_indexed.rs");
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
        let err = resolve_explain_symbol(&snapshot, &unindexed_path, "main").unwrap_err();

        assert!(err.to_string().contains("not indexed"));
    }
}
