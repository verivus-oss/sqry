use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use ropey::Rope;
use std::fs;
use std::sync::Arc;
use tower_lsp::lsp_types::{Position, Range};

use crate::documents::{DocumentSnapshot, compute_line_offsets};
use crate::file_types::classify_file;
use crate::session::{NodeMatch, SessionManager};

pub mod ask;
pub mod call_hierarchy;
pub mod code_action;
pub mod complexity_metrics;
pub mod definition;
pub mod dependency_impact;
pub mod direct_relations;
pub mod document_symbol;
pub mod execute_command;
pub mod explain_symbol;
pub mod get_insights;
pub mod graph_export;
pub mod graph_stats;
pub mod hierarchical_search;
pub mod hover;
pub mod index;
pub mod is_node_in_cycle;
pub mod pattern_search;
pub mod references;
pub mod relations;
pub mod search;
pub mod semantic_diff;
pub mod show_dependencies;
pub mod similar_symbols;
pub mod subgraph;
pub mod trace_path;
pub mod workspace_symbol;

static TEST_DELAY_MS: AtomicU64 = AtomicU64::new(0);

/// Configure an artificial delay (in milliseconds) that handler execution will respect.
/// Used exclusively by integration tests to simulate long-running operations.
pub fn configure_test_delay_ms(ms: u64) {
    TEST_DELAY_MS.store(ms, Ordering::SeqCst);
}

/// Pause handler execution when a non-zero test delay has been configured.
pub(crate) fn pause_for_test() {
    let delay = TEST_DELAY_MS.load(Ordering::SeqCst);
    if delay > 0 {
        std::thread::sleep(Duration::from_millis(delay));
    }
}

/// Load a document snapshot, falling back to disk if not in the store.
///
/// This function handles three cases:
/// 1. Document is in the store -> return it
/// 2. Document was intentionally skipped (binary, too large) -> return error
/// 3. Document not in store -> try to read from disk (with file type check)
fn load_document_snapshot(session: &SessionManager, path: &Path) -> Result<DocumentSnapshot> {
    // Check if we have it in the document store
    if let Some(snapshot) = session.document_snapshot(path) {
        return Ok(snapshot);
    }

    // Check if it was intentionally skipped
    if let Some(reason) = session.documents().get_skip_reason(path) {
        return Err(anyhow!(
            "{}: {} ({})",
            path.display(),
            reason.message(),
            classify_file(path).description()
        ));
    }

    // Not in store and not skipped - try to load from disk
    // First check if it's a supported file type
    let category = classify_file(path);
    if !category.is_supported() {
        return Err(anyhow!(
            "{}: unsupported {} - cannot process binary files",
            path.display(),
            category.description()
        ));
    }

    // Try to read from disk
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let rope = Rope::from(text.as_str());
    let line_offsets = compute_line_offsets(&rope);
    Ok(DocumentSnapshot {
        language_id: None,
        version: 0,
        rope,
        line_offsets: Arc::from(line_offsets),
    })
}

/// Convert a node's byte-offset columns to LSP UTF-16 positions.
///
/// Nodes from the graph contain byte columns (from tree-sitter).
/// LSP requires UTF-16 code unit positions. This function performs the conversion
/// by loading the file content (from `DocumentSnapshot` or disk) and translating
/// each byte column to its UTF-16 equivalent.
pub(crate) fn node_range_lsp(session: &SessionManager, node: &NodeMatch) -> Result<Range> {
    let file_path = &node.file_path;
    let snapshot = load_document_snapshot(session, file_path)?;

    let start = byte_position_to_lsp(
        &snapshot,
        node.start_line as usize,
        node.start_column as usize,
    )
    .with_context(|| {
        format!(
            "failed to convert start position for {} at {}:{}",
            node.qualified_name_or_name(),
            node.start_line,
            node.start_column
        )
    })?;
    let end = byte_position_to_lsp(&snapshot, node.end_line as usize, node.end_column as usize)
        .with_context(|| {
            format!(
                "failed to convert end position for {} at {}:{}",
                node.qualified_name_or_name(),
                node.end_line,
                node.end_column
            )
        })?;

    Ok(Range::new(start, end))
}

/// Convert a (1-based line, 0-based byte column) pair to LSP Position.
fn byte_position_to_lsp(
    snapshot: &DocumentSnapshot,
    line_1based: usize,
    byte_column: usize,
) -> Result<Position> {
    let line_idx = line_1based.saturating_sub(1);
    if line_idx >= snapshot.rope.len_lines() {
        return Err(anyhow!(
            "line {} out of bounds (file has {} lines)",
            line_1based,
            snapshot.rope.len_lines()
        ));
    }

    let line_text = snapshot.rope.line(line_idx).to_string();
    let utf16_col = crate::utils::position::line_byte_to_utf16_col(&line_text, byte_column);

    // LSP uses u32 for positions; clamp to max
    Ok(Position::new(
        line_idx.try_into().unwrap_or(u32::MAX),
        utf16_col.try_into().unwrap_or(u32::MAX),
    ))
}
