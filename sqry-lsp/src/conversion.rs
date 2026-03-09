use std::fs;
use std::path::Path;

use sqry_core::graph::unified::NodeId;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use tower_lsp::lsp_types::{Location, Position, Range, Url};

use crate::protocol::SqrySearchItem;

/// Convert a graph node to LSP search item.
///
/// This is the graph-based equivalent of `symbol_to_item_resolved`, used for
/// migrating from the legacy index to `CodeGraph`.
///
/// # Arguments
///
/// * `node_id` - The graph node ID
/// * `entry` - The node entry from the graph
/// * `graph` - The code graph for resolving interned values
/// * `root` - Workspace root path for relative path resolution
pub fn node_to_search_item(
    _node_id: NodeId,
    entry: &NodeEntry,
    graph: &CodeGraph,
    root: &Path,
) -> Option<SqrySearchItem> {
    // Resolve name from string interner
    let name = graph.strings().resolve(entry.name)?.to_string();

    // Resolve file path from file registry
    let file_path = graph.files().resolve(entry.file)?;
    let path = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        root.join(&*file_path)
    };

    let uri = Url::from_file_path(&path).ok()?;

    // Convert byte columns to UTF-16 columns per LSP requirements
    let (start_char, end_char) = match fs::read_to_string(&path) {
        Ok(contents) => {
            let start_line_idx = entry.start_line.saturating_sub(1) as usize;
            let end_line_idx = entry.end_line.saturating_sub(1) as usize;

            let mut start_char = entry.start_column;
            let mut end_char = entry.end_column;

            for (idx, line_text) in contents.lines().enumerate() {
                if idx == start_line_idx {
                    let byte_col = (entry.start_column as usize).min(line_text.len());
                    let u16_col = crate::utils::position::byte_to_utf16(line_text, byte_col);
                    start_char = u16_col.try_into().unwrap_or(u32::MAX);
                }
                if idx == end_line_idx {
                    let byte_col = (entry.end_column as usize).min(line_text.len());
                    let u16_col = crate::utils::position::byte_to_utf16(line_text, byte_col);
                    end_char = u16_col.try_into().unwrap_or(u32::MAX);
                }
                if idx > end_line_idx {
                    break;
                }
            }
            (start_char, end_char)
        }
        Err(_) => (entry.start_column, entry.end_column),
    };

    let range = Range {
        start: Position {
            line: entry.start_line.saturating_sub(1),
            character: start_char,
        },
        end: Position {
            line: entry.end_line.saturating_sub(1),
            character: end_char,
        },
    };

    let location = Location { uri, range };

    // Resolve qualified name if available, fallback to name
    let qualified_name = entry
        .qualified_name
        .and_then(|qn_id| graph.strings().resolve(qn_id))
        .map_or_else(|| name.clone(), |s| s.to_string());

    // Get language from file registry
    let language = graph
        .files()
        .language_for_file(entry.file)
        .map_or_else(|| "unknown".to_string(), |l| l.to_string());

    // Convert NodeKind to string
    let kind = format!("{:?}", entry.kind).to_lowercase();

    Some(SqrySearchItem {
        name,
        kind,
        qualified_name,
        language,
        location,
        score: None,
    })
}
