//! Navigation tool execution.
//!
//! This module implements the code navigation tools for finding definitions,
//! references, hover info, document symbols, and workspace symbol search.

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Result, anyhow};
use sqry_core::graph::unified::{FileScope, ResolutionMode, SymbolCandidateOutcome, SymbolQuery};

use crate::engine::engine_for_workspace;
use crate::execution::types::{
    DefinitionData, DocumentSymbolData, GetDefinitionData, GetDocumentSymbolsData,
    GetReferencesData, GetWorkspaceSymbolsData, HoverInfoData, ReferenceLocationData,
    ToolExecution, WorkspaceSymbolData,
};
use crate::execution::utils::duration_to_ms;
use crate::tools::{
    GetDefinitionArgs, GetDocumentSymbolsArgs, GetHoverInfoArgs, GetReferencesArgs,
    GetWorkspaceSymbolsArgs,
};

/// Execute the `get_definition` tool to find where a symbol is defined.
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

fn candidate_nodes_for_symbol(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol: &str,
) -> Vec<sqry_core::graph::unified::NodeId> {
    match snapshot.find_symbol_candidates(&SymbolQuery {
        symbol,
        file_scope: FileScope::Any,
        mode: ResolutionMode::AllowSuffixCandidates,
    }) {
        SymbolCandidateOutcome::Candidates(candidates) => candidates,
        SymbolCandidateOutcome::NotFound | SymbolCandidateOutcome::FileNotIndexed => Vec::new(),
    }
}

fn resolve_hover_node(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol: &str,
) -> Result<sqry_core::graph::unified::NodeId> {
    match candidate_nodes_for_symbol(snapshot, symbol).as_slice() {
        [] => Err(anyhow!("Symbol '{symbol}' not found")),
        [node_id] => Ok(*node_id),
        candidates => Err(anyhow!(
            "Symbol '{symbol}' is ambiguous ({} candidates). Use a canonical qualified name.",
            candidates.len()
        )),
    }
}
pub fn execute_get_definition(
    args: &GetDefinitionArgs,
) -> Result<ToolExecution<GetDefinitionData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(symbol = %args.symbol, "Executing get_definition tool");

    let graph = engine.ensure_graph()?;
    let snapshot = graph.snapshot();

    let files = snapshot.files();
    let strings = snapshot.strings();

    // Find nodes matching the symbol name
    let mut definitions: Vec<DefinitionData> = Vec::new();

    for node_id in candidate_nodes_for_symbol(&snapshot, &args.symbol) {
        let Some(entry) = snapshot.get_node(node_id) else {
            continue;
        };
        let name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
            entry,
            strings,
            files.language_for_file(entry.file),
            &name,
        );

        let file_path = files
            .resolve(entry.file)
            .map(|p| {
                crate::execution::symbol_utils::relative_path_forward_slash(
                    p.as_ref(),
                    &workspace_root,
                )
            })
            .unwrap_or_default();

        let language = files
            .language_for_file(entry.file)
            .map_or_else(|| "unknown".to_string(), |l| l.to_string());

        definitions.push(DefinitionData {
            name,
            qualified_name,
            kind: format!("{:?}", entry.kind),
            file_path,
            line: entry.start_line,
            column: entry.start_column,
            language,
            preview: None, // Would need file content to provide preview
        });
    }

    let total = definitions.len() as u64;
    let data = GetDefinitionData { definitions, total };

    tracing::debug!(total = total, "get_definition completed");

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(total),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Find all nodes that match the given symbol name or qualified name.
fn find_target_nodes(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol: &str,
) -> Vec<sqry_core::graph::unified::NodeId> {
    candidate_nodes_for_symbol(snapshot, symbol)
}

/// Collect declaration references for target nodes.
fn collect_declaration_refs(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    target_nodes: &[sqry_core::graph::unified::NodeId],
    workspace_root: &std::path::Path,
    seen: &mut std::collections::HashSet<(String, u32, u32)>,
) -> Vec<ReferenceLocationData> {
    let files = graph.files();
    let mut references = Vec::new();

    for node_id in target_nodes {
        if let Some(entry) = graph.nodes().get(*node_id) {
            let file_path = files
                .resolve(entry.file)
                .map(|p| {
                    crate::execution::symbol_utils::relative_path_forward_slash(
                        p.as_ref(),
                        workspace_root,
                    )
                })
                .unwrap_or_default();

            let loc_key = (file_path.clone(), entry.start_line, entry.start_column);
            if !seen.contains(&loc_key) {
                seen.insert(loc_key);
                references.push(ReferenceLocationData {
                    file_path,
                    line: entry.start_line,
                    column: entry.start_column,
                    preview: None,
                    is_declaration: true,
                });
            }
        }
    }

    references
}

/// Collect caller references (incoming edges) for target nodes.
fn collect_caller_refs(
    graph: &sqry_core::graph::unified::concurrent::CodeGraph,
    target_nodes: &[sqry_core::graph::unified::NodeId],
    max_results: usize,
    workspace_root: &std::path::Path,
    seen: &mut std::collections::HashSet<(String, u32, u32)>,
    existing_count: usize,
) -> Vec<ReferenceLocationData> {
    let files = graph.files();
    let mut references = Vec::new();

    for target_id in target_nodes {
        let reverse_store = graph.edges().reverse();
        let incoming = reverse_store.edges_from(*target_id);
        for edge_ref in &incoming {
            if existing_count + references.len() >= max_results {
                break;
            }

            let source_id = edge_ref.target;
            if let Some(entry) = graph.nodes().get(source_id) {
                let file_path = files
                    .resolve(entry.file)
                    .map(|p| {
                        crate::execution::symbol_utils::relative_path_forward_slash(
                            p.as_ref(),
                            workspace_root,
                        )
                    })
                    .unwrap_or_default();

                let loc_key = (file_path.clone(), entry.start_line, entry.start_column);
                if !seen.contains(&loc_key) {
                    seen.insert(loc_key);
                    references.push(ReferenceLocationData {
                        file_path,
                        line: entry.start_line,
                        column: entry.start_column,
                        preview: None,
                        is_declaration: false,
                    });
                }
            }
        }
    }

    references
}

/// Execute the `get_references` tool to find all references to a symbol.
pub fn execute_get_references(
    args: &GetReferencesArgs,
) -> Result<ToolExecution<GetReferencesData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(symbol = %args.symbol, "Executing get_references tool");

    let graph = engine.ensure_graph()?;
    let snapshot = graph.snapshot();

    // Find definition nodes for the symbol
    let target_node_ids = find_target_nodes(&snapshot, &args.symbol);

    // Collect references
    let mut references: Vec<ReferenceLocationData> = Vec::new();
    let mut seen_locations = std::collections::HashSet::new();

    // Include declarations if requested
    if args.include_declaration {
        references.extend(collect_declaration_refs(
            &graph,
            &target_node_ids,
            &workspace_root,
            &mut seen_locations,
        ));
    }

    // Find callers (incoming edges)
    references.extend(collect_caller_refs(
        &graph,
        &target_node_ids,
        args.max_results,
        &workspace_root,
        &mut seen_locations,
        references.len(),
    ));

    let total = references.len() as u64;
    let data = GetReferencesData {
        symbol: args.symbol.clone(),
        references,
        total,
    };

    tracing::debug!(total = total, "get_references completed");

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(total),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Execute the `get_hover_info` tool to get symbol information.
pub fn execute_get_hover_info(args: &GetHoverInfoArgs) -> Result<ToolExecution<HoverInfoData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(symbol = %args.symbol, "Executing get_hover_info tool");

    let graph = engine.ensure_graph()?;
    let snapshot = graph.snapshot();
    let node_id = resolve_hover_node(&snapshot, &args.symbol)?;

    let files = snapshot.files();
    let strings = snapshot.strings();
    let entry = snapshot
        .get_node(node_id)
        .ok_or_else(|| anyhow!("Resolved symbol '{}' missing from graph", args.symbol))?;

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let qualified_name = crate::execution::symbol_utils::display_entry_qualified_name(
        entry,
        strings,
        files.language_for_file(entry.file),
        &name,
    );
    let file_path = files
        .resolve(entry.file)
        .map(|p| {
            crate::execution::symbol_utils::relative_path_forward_slash(p.as_ref(), &workspace_root)
        })
        .unwrap_or_default();
    let language = files
        .language_for_file(entry.file)
        .map_or_else(|| "unknown".to_string(), |l| l.to_string());
    let signature = entry
        .signature
        .and_then(|id| strings.resolve(id))
        .map(|s| s.to_string());
    let documentation = entry
        .doc
        .and_then(|id| strings.resolve(id))
        .map(|s| s.to_string());

    let data = HoverInfoData {
        name,
        qualified_name,
        kind: format!("{:?}", entry.kind),
        file_path,
        line: entry.start_line,
        language,
        signature,
        documentation,
    };

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(1),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Execute the `get_document_symbols` tool to list symbols in a file.
pub fn execute_get_document_symbols(
    args: &GetDocumentSymbolsArgs,
) -> Result<ToolExecution<GetDocumentSymbolsData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(file_path = %args.file_path, "Executing get_document_symbols tool");

    let graph = engine.ensure_graph()?;

    let files = graph.files();
    let strings = graph.strings();

    // Find the file ID for the given path
    let target_path = std::path::Path::new(&args.file_path);
    let file_id = files.get(target_path).or_else(|| {
        // Try with workspace prefix
        let full_path = workspace_root.join(&args.file_path);
        files.get(&full_path)
    });

    let file_id =
        file_id.ok_or_else(|| anyhow::anyhow!("File '{}' not found in graph", args.file_path))?;

    // Collect symbols in this file
    let mut symbols: Vec<DocumentSymbolData> = Vec::new();

    for (_node_id, entry) in graph.nodes().iter() {
        if entry.file == file_id {
            let fallback_name = strings
                .resolve(entry.name)
                .map(|s| s.to_string())
                .unwrap_or_default();
            let name = crate::execution::symbol_utils::display_entry_qualified_name(
                entry,
                strings,
                files.language_for_file(entry.file),
                &fallback_name,
            );

            symbols.push(DocumentSymbolData {
                name,
                kind: format!("{:?}", entry.kind),
                line: entry.start_line,
                end_line: Some(entry.end_line),
                children: vec![], // Flat list for now, could build hierarchy later
            });
        }
    }

    // Sort by line number
    symbols.sort_by_key(|s| s.line);

    let total = symbols.len() as u64;
    let data = GetDocumentSymbolsData {
        file_path: args.file_path.replace('\\', "/"),
        symbols,
        total,
    };

    tracing::debug!(total = total, "get_document_symbols completed");

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(total),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Execute the `get_workspace_symbols` tool to search for symbols.
pub fn execute_get_workspace_symbols(
    args: &GetWorkspaceSymbolsArgs,
) -> Result<ToolExecution<GetWorkspaceSymbolsData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();

    tracing::debug!(query = %args.query, "Executing get_workspace_symbols tool");

    let graph = engine.ensure_graph()?;

    let files = graph.files();
    let strings = graph.strings();

    let query_lower = args.query.to_lowercase();

    // Search for matching symbols
    let mut symbols: Vec<WorkspaceSymbolData> = Vec::new();

    for (_node_id, entry) in graph.nodes().iter() {
        if symbols.len() >= args.max_results {
            break;
        }

        let fallback_name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let display_name = crate::execution::symbol_utils::display_entry_qualified_name(
            entry,
            strings,
            files.language_for_file(entry.file),
            &fallback_name,
        );
        let qualified_name = display_name.clone();

        // Fuzzy match on name or qualified name
        let name_lower = display_name.to_lowercase();
        let qname_lower = qualified_name.to_lowercase();

        if name_lower.contains(&query_lower) || qname_lower.contains(&query_lower) {
            let file_path = files
                .resolve(entry.file)
                .map(|p| {
                    crate::execution::symbol_utils::relative_path_forward_slash(
                        p.as_ref(),
                        &workspace_root,
                    )
                })
                .unwrap_or_default();

            let language = files
                .language_for_file(entry.file)
                .map_or_else(|| "unknown".to_string(), |l| l.to_string());

            // Calculate a simple relevance score
            let score = if name_lower == query_lower {
                1.0
            } else if name_lower.starts_with(&query_lower) {
                0.9
            } else if name_lower.contains(&query_lower) {
                0.7
            } else {
                0.5
            };

            symbols.push(WorkspaceSymbolData {
                name: display_name,
                qualified_name,
                kind: format!("{:?}", entry.kind),
                file_path,
                line: entry.start_line,
                language,
                score,
            });
        }
    }

    // Sort by score (highest first)
    symbols.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total = symbols.len() as u64;
    let data = GetWorkspaceSymbolsData {
        query: args.query.clone(),
        symbols,
        total,
    };

    tracing::debug!(total = total, "get_workspace_symbols completed");

    Ok(ToolExecution {
        data,
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token: None,
        total: Some(total),
        truncated: Some(false),
        candidates_scanned: None,
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}
