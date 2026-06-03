use anyhow::{Context, Result, bail};
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::protocol::{RelationKind, SqryRelationParams, SqryRelationResult, SqrySearchItem};
use crate::session::SessionManager;

const DEFAULT_LIMIT: usize = 200;

/// Execute a relation (callers/callees) query via `CodeGraph`.
///
/// # Errors
///
/// Returns an error when graph loading or query execution fails.
pub fn execute(session: &SessionManager, params: SqryRelationParams) -> Result<SqryRelationResult> {
    let SqryRelationParams {
        relation,
        target,
        path,
        limit,
    } = params;

    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    let root = session.resolve_path(path.as_deref())?;
    let query = build_relation_query(relation, &target)?;

    // SGA06 — acquire the graph through the shared `FilesystemGraphProvider`
    // and run the relation predicate via the preloaded executor entrypoint
    // so this read-only request honours the same pipeline as CLI/MCP.
    let Some(graph) = session.graph_for_path(&root)? else {
        return Ok(SqryRelationResult {
            relation,
            results: Vec::new(),
            total: 0,
            is_truncated: false,
            used_index: true,
        });
    };

    let executor = session.executor();
    let query_results = executor
        .execute_on_preloaded_graph(graph, &query, &root, None)
        .with_context(|| format!("failed to execute relation query '{query}'"))?;

    let total = query_results.len();
    let truncated = total > limit;

    // Convert QueryResults to SqrySearchItems
    let results: Vec<SqrySearchItem> = query_results
        .iter()
        .take(limit)
        .filter_map(|m| {
            let name = m.name().map(|s| s.to_string()).unwrap_or_default();
            let kind = m.kind().as_str().to_string();
            let language = m.language().map_or_else(
                || "unknown".to_string(),
                |l| l.to_string().to_ascii_lowercase(),
            );

            // Build file path
            let file_path = m.relative_path().map(|p| root.join(p))?;
            let uri = Url::from_file_path(&file_path).ok()?;

            // Create location range (0-indexed for LSP)
            let start = Position {
                line: m.start_line().saturating_sub(1),
                character: m.start_column().saturating_sub(1),
            };
            let end = Position {
                line: m.end_line().saturating_sub(1),
                character: m.end_column().saturating_sub(1),
            };
            let location = Location {
                uri,
                range: Range { start, end },
            };

            Some(SqrySearchItem {
                name: name.clone(),
                kind,
                qualified_name: name,
                language,
                location,
                score: None,
            })
        })
        .collect();

    Ok(SqryRelationResult {
        relation,
        results,
        total,
        is_truncated: truncated,
        used_index: true, // Always uses CodeGraph
    })
}

fn build_relation_query(kind: RelationKind, target: &str) -> Result<String> {
    if target.trim().is_empty() {
        bail!("relation target cannot be empty")
    }

    let prefix = match kind {
        RelationKind::Callers => "callers",
        RelationKind::Callees => "callees",
        RelationKind::Imports => "imports",
        RelationKind::Exports => "exports",
        RelationKind::Returns => "returns",
        RelationKind::Wraps => "wraps",
        RelationKind::ChannelPeers => "channel_peers",
        RelationKind::Instantiations => "instantiations",
    };

    Ok(format!("{prefix}:{target}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_relation_query_callers() {
        let q = build_relation_query(RelationKind::Callers, "my_fn").unwrap();
        assert_eq!(q, "callers:my_fn");
    }

    #[test]
    fn build_relation_query_callees() {
        let q = build_relation_query(RelationKind::Callees, "my_fn").unwrap();
        assert_eq!(q, "callees:my_fn");
    }

    #[test]
    fn build_relation_query_imports() {
        let q = build_relation_query(RelationKind::Imports, "std::io").unwrap();
        assert_eq!(q, "imports:std::io");
    }

    #[test]
    fn build_relation_query_exports() {
        let q = build_relation_query(RelationKind::Exports, "MyStruct").unwrap();
        assert_eq!(q, "exports:MyStruct");
    }

    #[test]
    fn build_relation_query_returns() {
        let q = build_relation_query(RelationKind::Returns, "i32").unwrap();
        assert_eq!(q, "returns:i32");
    }

    #[test]
    fn build_relation_query_empty_target_returns_error() {
        assert!(build_relation_query(RelationKind::Callers, "").is_err());
        assert!(build_relation_query(RelationKind::Callers, "   ").is_err());
    }
}
