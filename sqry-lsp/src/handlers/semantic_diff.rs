//! Semantic diff handler for LSP.
//!
//! Compares two git refs and returns semantic symbol changes using the
//! shared diff implementation from sqry-core.

use std::sync::Arc;

use anyhow::{Result, bail};
use sqry_core::git::WorktreeManager;
use sqry_core::graph::diff::{DiffSummary, GraphComparator, NodeLocation};
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_plugin_registry::create_plugin_manager;

use crate::protocol::{
    SqryDiffSummary, SqrySemanticDiffParams, SqrySemanticDiffResult, SqrySymbolChange,
    SqrySymbolLocationRef,
};
use crate::session::SessionManager;

/// Default maximum results.
const DEFAULT_MAX_RESULTS: usize = 500;

/// Execute semantic diff analysis.
///
/// Compares symbols between two git refs by:
/// 1. Creating temporary git worktrees for both versions
/// 2. Building `CodeGraphs` for each worktree
/// 3. Comparing the graphs to detect added/removed/modified/renamed symbols
///
/// # Errors
///
/// Returns error if:
/// - Not a git repository
/// - Git refs are invalid
/// - Graph building fails
pub fn execute(
    session: &SessionManager,
    params: &SqrySemanticDiffParams,
) -> Result<SqrySemanticDiffResult> {
    let root = session.resolve_path(params.path.as_deref())?;
    let base_ref = params.base.git_ref.trim();
    let target_ref = params.target.git_ref.trim();

    if base_ref.is_empty() {
        bail!("base.ref cannot be empty");
    }
    if target_ref.is_empty() {
        bail!("target.ref cannot be empty");
    }

    let include_unchanged = params.include_unchanged.unwrap_or(false);
    let max_results = params.max_results.unwrap_or(DEFAULT_MAX_RESULTS);

    log::debug!(
        "Executing semantic diff: base='{base_ref}', target='{target_ref}', root={root}",
        root = root.display()
    );

    // Phase 1: Create git worktrees using shared implementation
    let worktree_mgr = WorktreeManager::create(&root, base_ref, target_ref)
        .map_err(|e| anyhow::anyhow!("Failed to create git worktrees: {e}"))?;

    // Phase 2: Build CodeGraphs for both worktrees
    let plugins = create_plugin_manager();
    let config = BuildConfig::default();

    let base_graph = Arc::new(
        build_unified_graph(worktree_mgr.base_path(), &plugins, &config)
            .map_err(|e| anyhow::anyhow!("Failed to build base graph: {e}"))?,
    );
    let target_graph = Arc::new(
        build_unified_graph(worktree_mgr.target_path(), &plugins, &config)
            .map_err(|e| anyhow::anyhow!("Failed to build target graph: {e}"))?,
    );

    // Phase 3: Compare graphs using shared comparator
    let comparator = GraphComparator::new(
        base_graph,
        target_graph,
        root.clone(),
        worktree_mgr.base_path().to_path_buf(),
        worktree_mgr.target_path().to_path_buf(),
    );
    let result = comparator.compute_changes()?;

    // Phase 4: Convert to LSP types and apply filters
    let mut changes: Vec<SqrySymbolChange> =
        result.changes.into_iter().map(convert_change).collect();

    // Apply filters
    if let Some(ref filters) = params.filters {
        if !filters.change_types.is_empty() {
            changes.retain(|change| {
                filters
                    .change_types
                    .iter()
                    .any(|ct| ct.eq_ignore_ascii_case(&change.change_type))
            });
        }

        if !filters.symbol_kinds.is_empty() {
            changes.retain(|change| {
                filters
                    .symbol_kinds
                    .iter()
                    .any(|kind| kind.eq_ignore_ascii_case(&change.kind))
            });
        }
    }

    if !include_unchanged {
        changes.retain(|change| change.change_type != "unchanged");
    }

    // Phase 5: Compute summary before truncation
    let summary = convert_summary(&result.summary);

    // Phase 6: Apply max_results limit
    let total = changes.len() as u64;
    let truncated = changes.len() > max_results;
    changes.truncate(max_results);

    log::debug!("Semantic diff complete: {total} changes (truncated: {truncated})");

    // Worktree cleanup happens automatically when worktree_mgr drops

    Ok(SqrySemanticDiffResult {
        base_ref: base_ref.to_string(),
        target_ref: target_ref.to_string(),
        changes,
        summary,
        total,
        truncated,
    })
}

/// Convert a core `NodeChange` to an LSP `SqrySymbolChange`.
fn convert_change(change: sqry_core::graph::diff::NodeChange) -> SqrySymbolChange {
    let base_location = change.base_location.as_ref().map(convert_location);
    let target_location = change.target_location.as_ref().map(convert_location);

    SqrySymbolChange {
        symbol_name: change.name,
        qualified_name: Some(change.qualified_name),
        kind: change.kind,
        change_type: change.change_type.as_str().to_string(),
        base_location,
        target_location,
        signature_before: change.signature_before,
        signature_after: change.signature_after,
    }
}

/// Convert a core `NodeLocation` to an LSP `SqrySymbolLocationRef`.
fn convert_location(loc: &NodeLocation) -> SqrySymbolLocationRef {
    SqrySymbolLocationRef {
        file_path: loc.file_path.display().to_string(),
        start_line: loc.start_line,
        end_line: loc.end_line,
        start_column: loc.start_column,
        end_column: loc.end_column,
    }
}

/// Convert a core `DiffSummary` to an LSP `SqryDiffSummary`.
#[allow(clippy::cast_possible_truncation)] // u64->usize is safe for reasonable symbol counts
fn convert_summary(summary: &DiffSummary) -> SqryDiffSummary {
    SqryDiffSummary {
        added: summary.added as usize,
        removed: summary.removed as usize,
        modified: summary.modified as usize,
        renamed: summary.renamed as usize,
        signature_changed: summary.signature_changed as usize,
        unchanged: summary.unchanged as usize,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use sqry_core::graph::diff::{ChangeType, DiffSummary, NodeChange, NodeLocation};

    use super::*;

    #[test]
    fn test_convert_change_type() {
        assert_eq!(ChangeType::Added.as_str(), "added");
        assert_eq!(ChangeType::Removed.as_str(), "removed");
        assert_eq!(ChangeType::Modified.as_str(), "modified");
    }

    #[test]
    fn change_type_all_variants() {
        assert_eq!(ChangeType::Added.as_str(), "added");
        assert_eq!(ChangeType::Removed.as_str(), "removed");
        assert_eq!(ChangeType::Modified.as_str(), "modified");
        assert_eq!(ChangeType::Renamed.as_str(), "renamed");
        assert_eq!(ChangeType::SignatureChanged.as_str(), "signature_changed");
        assert_eq!(ChangeType::Unchanged.as_str(), "unchanged");
    }

    // ── convert_location ────────────────────────────────────────────────────

    #[test]
    fn convert_location_preserves_fields() {
        let loc = NodeLocation {
            file_path: PathBuf::from("/workspace/src/lib.rs"),
            start_line: 10,
            end_line: 20,
            start_column: 4,
            end_column: 8,
        };
        let result = convert_location(&loc);
        assert_eq!(result.file_path, "/workspace/src/lib.rs");
        assert_eq!(result.start_line, 10);
        assert_eq!(result.end_line, 20);
        assert_eq!(result.start_column, 4);
        assert_eq!(result.end_column, 8);
    }

    // ── convert_summary ─────────────────────────────────────────────────────

    #[test]
    fn convert_summary_preserves_counts() {
        let summary = DiffSummary {
            added: 5,
            removed: 3,
            modified: 7,
            renamed: 1,
            signature_changed: 2,
            unchanged: 10,
        };
        let result = convert_summary(&summary);
        assert_eq!(result.added, 5);
        assert_eq!(result.removed, 3);
        assert_eq!(result.modified, 7);
        assert_eq!(result.renamed, 1);
        assert_eq!(result.signature_changed, 2);
        assert_eq!(result.unchanged, 10);
    }

    #[test]
    fn convert_summary_zeros() {
        let summary = DiffSummary {
            added: 0,
            removed: 0,
            modified: 0,
            renamed: 0,
            signature_changed: 0,
            unchanged: 0,
        };
        let result = convert_summary(&summary);
        assert_eq!(result.added, 0);
        assert_eq!(result.removed, 0);
        assert_eq!(result.modified, 0);
        assert_eq!(result.renamed, 0);
        assert_eq!(result.signature_changed, 0);
        assert_eq!(result.unchanged, 0);
    }

    // ── convert_change ───────────────────────────────────────────────────────

    #[test]
    fn convert_change_with_locations() {
        let base_loc = NodeLocation {
            file_path: PathBuf::from("src/lib.rs"),
            start_line: 1,
            end_line: 5,
            start_column: 0,
            end_column: 0,
        };
        let target_loc = NodeLocation {
            file_path: PathBuf::from("src/lib.rs"),
            start_line: 2,
            end_line: 6,
            start_column: 0,
            end_column: 0,
        };
        let change = NodeChange {
            name: "my_fn".to_string(),
            qualified_name: "module::my_fn".to_string(),
            kind: "function".to_string(),
            change_type: ChangeType::Modified,
            base_location: Some(base_loc),
            target_location: Some(target_loc),
            signature_before: Some("fn my_fn() -> ()".to_string()),
            signature_after: Some("fn my_fn(x: i32) -> ()".to_string()),
        };

        let result = convert_change(change);
        assert_eq!(result.symbol_name, "my_fn");
        assert_eq!(result.qualified_name, Some("module::my_fn".to_string()));
        assert_eq!(result.kind, "function");
        assert_eq!(result.change_type, "modified");
        assert!(result.base_location.is_some());
        assert!(result.target_location.is_some());
        assert_eq!(
            result.signature_before,
            Some("fn my_fn() -> ()".to_string())
        );
        assert_eq!(
            result.signature_after,
            Some("fn my_fn(x: i32) -> ()".to_string())
        );
    }

    #[test]
    fn convert_change_without_locations() {
        let change = NodeChange {
            name: "removed_fn".to_string(),
            qualified_name: "module::removed_fn".to_string(),
            kind: "function".to_string(),
            change_type: ChangeType::Removed,
            base_location: None,
            target_location: None,
            signature_before: None,
            signature_after: None,
        };

        let result = convert_change(change);
        assert_eq!(result.symbol_name, "removed_fn");
        assert_eq!(result.change_type, "removed");
        assert!(result.base_location.is_none());
        assert!(result.target_location.is_none());
    }
}
