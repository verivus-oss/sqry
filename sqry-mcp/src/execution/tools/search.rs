//! Search tool execution.
//!
//! This module implements the semantic search and `find_similar` tools
//! for querying and finding related symbols in the codebase.
//!
//! Uses native graph types (`NodeId`, `NodeEntry`) directly without intermediate
//! Symbol conversion.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use sqry_core::graph::unified::node::NodeId;
use sqry_core::graph::unified::{FileScope, ResolutionMode, SymbolQuery, SymbolResolutionOutcome};
use sqry_core::search::matcher::{FuzzyMatcher, MatchConfig};

use crate::engine::{canonicalize_in_workspace, engine_for_workspace};
use crate::tools::{SearchSimilarArgs, SemanticSearchArgs};

use crate::execution::symbol_utils::{build_search_hits_from_nodes, filter_node};
use crate::execution::types::{
    FindSimilarData, SemanticSearchData, SimilarSymbolData, ToolExecution,
};
use crate::execution::utils::{duration_to_ms, paginate};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SemanticSortKey {
    display_name: String,
    relative_path: String,
    start_line: u32,
    start_column: u32,
    end_line: u32,
    end_column: u32,
}

/// Execute the `semantic_search` tool to find symbols matching a query.
///
/// Uses native graph types (`NodeId`) directly without intermediate Symbol conversion.
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

/// Resolve workspace root for security checking only (no validation required).
///
/// This function determines the workspace root path without requiring a valid
/// sqry workspace to exist. It's used for security checks to ensure paths don't
/// escape the workspace boundary, even if the workspace hasn't been initialized yet.
///
/// Priority order:
/// 1. `SQRY_MCP_WORKSPACE_ROOT` env var (primary security boundary)
/// 2. `SQRY_WORKSPACE_ROOT` env var (backward compatibility)
/// 3. Current working directory
fn resolve_workspace_root_for_security() -> Result<PathBuf> {
    use std::env;

    // Check SQRY_MCP_WORKSPACE_ROOT first (primary)
    if let Ok(root) = env::var("SQRY_MCP_WORKSPACE_ROOT") {
        let path = PathBuf::from(root);
        return std::fs::canonicalize(&path)
            .with_context(|| format!("Failed to canonicalize workspace root: {}", path.display()));
    }

    // Check SQRY_WORKSPACE_ROOT (backward compatibility)
    if let Ok(root) = env::var("SQRY_WORKSPACE_ROOT") {
        let path = PathBuf::from(root);
        return std::fs::canonicalize(&path)
            .with_context(|| format!("Failed to canonicalize workspace root: {}", path.display()));
    }

    // Fallback to current directory
    let cwd = env::current_dir().context("Failed to get current directory")?;
    std::fs::canonicalize(&cwd).with_context(|| {
        format!(
            "Failed to canonicalize current directory: {}",
            cwd.display()
        )
    })
}

fn semantic_sort_key(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    node_id: NodeId,
    workspace_root: &Path,
) -> SemanticSortKey {
    let Some(entry) = snapshot.get_node(node_id) else {
        return SemanticSortKey {
            display_name: String::new(),
            relative_path: String::new(),
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
        };
    };

    let strings = snapshot.strings();
    let files = snapshot.files();
    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let display_name = crate::execution::symbol_utils::display_entry_qualified_name(
        entry,
        strings,
        files.language_for_file(entry.file),
        &name,
    );
    let relative_path = files.resolve(entry.file).map_or_else(String::new, |path| {
        crate::execution::symbol_utils::relative_path_forward_slash(
            workspace_root.join(path.as_ref()),
            workspace_root,
        )
    });

    SemanticSortKey {
        display_name,
        relative_path,
        start_line: entry.start_line,
        start_column: entry.start_column,
        end_line: entry.end_line,
        end_column: entry.end_column,
    }
}

pub fn execute_semantic_search(
    args: &SemanticSearchArgs,
) -> Result<ToolExecution<SemanticSearchData>> {
    // SECURITY: Validate path BEFORE loading workspace/engine
    // This ensures we reject malicious paths even if workspace is invalid
    // We resolve workspace root for security checking (no validation required)
    let workspace_root_for_security = resolve_workspace_root_for_security()?;

    // Check path security before any other operations
    let search_root = canonicalize_in_workspace(&args.path, &workspace_root_for_security)?;

    // Now load the engine (requires valid workspace)
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root();
    let query = args.query.trim();
    if query.is_empty() {
        bail!("query cannot be empty");
    }

    tracing::debug!(
        query = %query,
        path = %search_root.display(),
        max_results = args.max_results,
        context_lines = args.context_lines,
        "Executing semantic_search tool"
    );

    let start = Instant::now();

    // Get the graph for filtering
    let graph = engine.ensure_graph()?;
    let snapshot = graph.snapshot();

    let query_results = engine
        .executor()
        .execute_on_graph(query, &search_root)
        .with_context(|| format!("Failed to execute query '{query}'"))?;

    let nodes_searched = query_results.len();
    let elapsed = duration_to_ms(start.elapsed());

    // Filter using NodeId + graph lookups
    let mut filtered: Vec<NodeId> = query_results
        .node_ids()
        .iter()
        .filter(|&&node_id| filter_node(&snapshot, node_id, &args.filters))
        .copied()
        .collect();

    // Filter out classpath (external) nodes unless explicitly requested
    if !args.include_classpath {
        filtered.retain(|&node_id| {
            !crate::execution::symbol_utils::is_node_external(&snapshot, node_id)
        });
    }

    // Sort by user-facing display identity for deterministic MCP output.
    filtered.sort_by_key(|&node_id| semantic_sort_key(&snapshot, node_id, workspace_root));

    // Apply score (all results get 1.0 for now since executor doesn't provide scores)
    let mut scored: Vec<(NodeId, f64)> =
        filtered.into_iter().map(|node_id| (node_id, 1.0)).collect();

    if let Some(min_score) = args.score_min {
        scored.retain(|(_, score)| (*score) >= min_score);
    }

    let total = scored.len();
    let limited_len = total.min(args.max_results);
    let truncated = total > args.max_results;
    scored.truncate(limited_len);

    let (page_slice, next_token) = paginate(&scored, &args.pagination);

    // Build search hits using NodeId + graph lookups
    let hits =
        build_search_hits_from_nodes(&snapshot, page_slice, args.context_lines, workspace_root)?;
    let truncated_flag = truncated || next_token.is_some();

    Ok(ToolExecution {
        data: SemanticSearchData {
            results: hits,
            total: total as u64,
            truncated: truncated_flag,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: elapsed,
        next_page_token: next_token,
        total: Some(total as u64),
        truncated: Some(truncated_flag),
        candidates_scanned: Some(nodes_searched as u64),
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Find the reference node for similarity search.
///
/// Returns (`node_id`, `node_entry`) or an error if not found.
fn find_reference_node(
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    symbol_name: &str,
    file_path: &std::path::Path,
    workspace_root: &std::path::Path,
) -> Result<(
    sqry_core::graph::unified::NodeId,
    sqry_core::graph::unified::NodeEntry,
)> {
    let relative_file = file_path.strip_prefix(workspace_root).unwrap_or(file_path);

    let reference_node_id = match snapshot.resolve_symbol(&SymbolQuery {
        symbol: symbol_name,
        file_scope: FileScope::Path(relative_file),
        mode: ResolutionMode::Strict,
    }) {
        SymbolResolutionOutcome::Resolved(node_id) => node_id,
        SymbolResolutionOutcome::NotFound | SymbolResolutionOutcome::FileNotIndexed => {
            return Err(anyhow!(
                "Symbol '{}' not found in {}",
                symbol_name,
                file_path.display()
            ));
        }
        SymbolResolutionOutcome::Ambiguous(candidates) => {
            return Err(anyhow!(
                "Symbol '{}' is ambiguous in {} ({} candidates)",
                symbol_name,
                file_path.display(),
                candidates.len()
            ));
        }
    };

    let reference_entry = snapshot
        .get_node(reference_node_id)
        .ok_or_else(|| anyhow!("Reference symbol entry not found"))?;

    Ok((reference_node_id, reference_entry.clone()))
}

/// Execute the `find_similar` tool to find symbols similar to a reference.
///
/// Uses `CodeGraph` to find symbols with similar names using fuzzy matching.
#[allow(clippy::too_many_lines)]
pub fn execute_find_similar(args: &SearchSimilarArgs) -> Result<ToolExecution<FindSimilarData>> {
    let start = Instant::now();
    let workspace_path = resolve_workspace_path(&args.path);
    let engine = engine_for_workspace(workspace_path.as_ref())?;
    let workspace_root = engine.workspace_root().to_path_buf();
    let _scope_root = canonicalize_in_workspace(&args.path, &workspace_root)?;
    let file_path = canonicalize_in_workspace(&args.file_path, &workspace_root)?;

    tracing::debug!(
        file_path = %args.file_path,
        symbol = %args.symbol_name,
        similarity_threshold = args.similarity_threshold,
        max_results = args.max_results,
        "Executing find_similar tool"
    );

    // Require the unified graph
    let graph = engine.ensure_graph()?;

    let snapshot = graph.snapshot();

    // Find the reference symbol using name lookup
    let (reference_node_id, reference_entry) =
        find_reference_node(&snapshot, &args.symbol_name, &file_path, &workspace_root)?;

    // Build reference symbol data
    let strings = snapshot.strings();
    let files = snapshot.files();

    let ref_name = strings
        .resolve(reference_entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let ref_qualified_name = reference_entry
        .qualified_name
        .and_then(|sid| strings.resolve(sid))
        .map_or_else(|| ref_name.clone(), |s| s.to_string());
    let ref_kind = reference_entry.kind;
    let ref_language = files
        .language_for_file(reference_entry.file)
        .map_or_else(|| "unknown".to_string(), |l| l.to_string());

    let reference_ref = build_node_ref_from_node(&reference_entry, &snapshot, &workspace_root);

    // Use FuzzyMatcher for similarity
    let matcher = FuzzyMatcher::with_config(MatchConfig {
        min_score: args.similarity_threshold,
        ..MatchConfig::default()
    });

    let mut candidates: Vec<(SimilarSymbolData, f64)> = Vec::new();
    let mut candidates_scanned: u64 = 0;
    let mut seen_names: HashSet<String> = HashSet::new();
    seen_names.insert(ref_qualified_name.clone());

    // Iterate through all nodes and find similar ones
    for (node_id, entry) in snapshot.iter_nodes() {
        candidates_scanned += 1;

        // Skip the reference symbol itself
        if node_id == reference_node_id {
            continue;
        }

        // Only compare same kind
        if entry.kind != ref_kind {
            continue;
        }

        // Get candidate name
        let candidate_name = strings
            .resolve(entry.name)
            .map(|s| s.to_string())
            .unwrap_or_default();
        let candidate_canonical_name = entry
            .qualified_name
            .and_then(|sid| strings.resolve(sid))
            .map_or_else(|| candidate_name.clone(), |s| s.to_string());

        // Skip already seen
        if seen_names.contains(&candidate_canonical_name) {
            continue;
        }
        seen_names.insert(candidate_canonical_name);

        // Calculate similarity using FuzzyMatcher on names
        let score = matcher.score(&ref_name, &candidate_name);
        if score < args.similarity_threshold {
            continue;
        }

        // Get candidate language
        let candidate_language_enum = files.language_for_file(entry.file);
        let candidate_language =
            candidate_language_enum.map_or_else(|| "unknown".to_string(), |l| l.to_string());

        // Only same language (unless cross-language is desired)
        if candidate_language != ref_language {
            continue;
        }

        // Build candidate file path and URI
        let candidate_file = files
            .resolve(entry.file)
            .map(|p| workspace_root.join(p.as_ref()))
            .unwrap_or_default();
        let file_uri = url::Url::from_file_path(&candidate_file).ok().map_or_else(
            || crate::execution::symbol_utils::path_to_forward_slash(&candidate_file),
            std::convert::Into::into,
        );
        let candidate_display_name = crate::execution::symbol_utils::display_entry_qualified_name(
            entry,
            strings,
            candidate_language_enum,
            &candidate_name,
        );

        // Build NodeRefData for the candidate
        let candidate_ref = crate::execution::types::NodeRefData {
            name: candidate_name,
            qualified_name: candidate_display_name,
            kind: format!("{:?}", entry.kind),
            language: candidate_language,
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
        };

        let similar_data = SimilarSymbolData {
            symbol: candidate_ref,
            similarity: score,
        };

        candidates.push((similar_data, score));

        // Early exit if we have enough
        if candidates.len() >= args.max_results * 2 {
            break;
        }
    }

    // Sort by similarity descending
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let total = candidates.len();
    let truncated = total > args.max_results;
    candidates.truncate(args.max_results);

    let (page_slice, next_page_token) = paginate(&candidates, &args.pagination);
    let results: Vec<SimilarSymbolData> = page_slice.iter().map(|(data, _)| data.clone()).collect();
    let truncated_flag = truncated || next_page_token.is_some();

    tracing::debug!(
        candidates_scanned = candidates_scanned,
        total_candidates = total,
        returned = results.len(),
        truncated = truncated_flag,
        "find_similar candidate summary"
    );

    Ok(ToolExecution {
        data: FindSimilarData {
            reference: reference_ref,
            results,
            total: total as u64,
        },
        used_index: false,
        used_graph: true,
        graph_metadata: None,
        execution_ms: duration_to_ms(start.elapsed()),
        next_page_token,
        total: Some(total as u64),
        truncated: Some(truncated_flag),
        candidates_scanned: Some(candidates_scanned),
        workspace_path: crate::execution::symbol_utils::path_to_forward_slash(workspace_root),
    })
}

/// Build `NodeRefData` from a graph node entry.
fn build_node_ref_from_node(
    entry: &sqry_core::graph::unified::storage::arena::NodeEntry,
    snapshot: &sqry_core::graph::unified::concurrent::GraphSnapshot,
    workspace_root: &Path,
) -> crate::execution::types::NodeRefData {
    use crate::execution::types::{NodeRefData, PositionData, RangeData};

    let strings = snapshot.strings();
    let files = snapshot.files();

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
        .map(|p| workspace_root.join(p.as_ref()))
        .unwrap_or_default();
    let file_uri = url::Url::from_file_path(&file_path).ok().map_or_else(
        || crate::execution::symbol_utils::path_to_forward_slash(&file_path),
        std::convert::Into::into,
    );

    let language = files
        .language_for_file(entry.file)
        .map_or_else(|| "unknown".to_string(), |l| l.to_string());

    NodeRefData {
        name,
        qualified_name,
        kind: format!("{:?}", entry.kind),
        language,
        file_uri,
        range: RangeData {
            start: PositionData {
                line: entry.start_line,
                character: entry.start_column,
            },
            end: PositionData {
                line: entry.end_line,
                character: entry.end_column,
            },
        },
        metadata: None,
    }
}

#[cfg(test)]
mod tests {
    use super::SemanticSortKey;

    #[test]
    fn semantic_sort_key_orders_by_display_name_then_location() {
        let mut keys = [
            SemanticSortKey {
                display_name: "beta.run".to_string(),
                relative_path: "src/lib.rs".to_string(),
                start_line: 10,
                start_column: 0,
                end_line: 12,
                end_column: 1,
            },
            SemanticSortKey {
                display_name: "alpha.run".to_string(),
                relative_path: "src/z.rs".to_string(),
                start_line: 8,
                start_column: 0,
                end_line: 9,
                end_column: 1,
            },
            SemanticSortKey {
                display_name: "alpha.run".to_string(),
                relative_path: "src/a.rs".to_string(),
                start_line: 4,
                start_column: 0,
                end_line: 6,
                end_column: 1,
            },
        ];

        keys.sort();

        assert_eq!(keys[0].relative_path, "src/a.rs");
        assert_eq!(keys[1].relative_path, "src/z.rs");
        assert_eq!(keys[2].display_name, "beta.run");
    }
}
