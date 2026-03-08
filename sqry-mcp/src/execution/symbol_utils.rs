//! Symbol conversion and building utilities for MCP tool execution.
//!
//! This module provides functions for converting nodes to reference data,
//! building search hits, filtering nodes, and extracting code context.
//!
//! Uses native graph types (`NodeId`, `NodeEntry`) directly without intermediate
//! Symbol conversion.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::node::{NodeId, NodeKind};
use url::Url;

use crate::tools::{SearchFilters, Visibility};

use super::types::{CodeContext, NodeRefData, PositionData, RangeData, SearchHit};

/// Convert a relative file path to a forward-slash string for JSON output.
///
/// On Windows, `Path::display()` and `to_string_lossy()` produce backslashes.
/// MCP tool responses must use forward slashes for cross-platform consistency.
pub(crate) fn path_to_forward_slash(path: impl AsRef<Path>) -> String {
    let s = path.as_ref().to_string_lossy();
    if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    }
}

/// Strip workspace prefix from a path and return a forward-slash relative string.
pub(crate) fn relative_path_forward_slash(
    path: impl AsRef<Path>,
    workspace_root: impl AsRef<Path>,
) -> String {
    let path = path.as_ref();
    let workspace_root = workspace_root.as_ref();
    let relative = path.strip_prefix(workspace_root).unwrap_or(path);
    path_to_forward_slash(relative)
}

/// Convert a file path to a file:// URI.
fn path_to_uri(path: &Path) -> Result<String> {
    let url =
        Url::from_file_path(path).map_err(|()| anyhow!("Invalid file path: {}", path.display()))?;
    Ok(url.into())
}

/// Build code context around a symbol's location.
///
/// Re-exported from `execution::build_context` for external use.
pub(crate) fn build_context(
    file_path: &Path,
    start_line: usize,
    end_line: usize,
    context_lines: usize,
) -> Result<Option<CodeContext>> {
    if context_lines == 0 {
        return Ok(None);
    }

    let file =
        File::open(file_path).with_context(|| format!("Failed to open {}", file_path.display()))?;
    let reader = BufReader::new(file);
    let start = start_line.saturating_sub(context_lines).max(1);
    let end = end_line + context_lines;

    let mut collected = Vec::new();
    let mut last_line = start;

    for (idx, line) in reader.lines().enumerate() {
        let line_no = idx + 1;
        if line_no < start {
            continue;
        }
        if line_no > end {
            break;
        }
        collected.push(line?);
        last_line = line_no;
    }

    if collected.is_empty() {
        return Ok(None);
    }

    let code = collected.join("\n");
    let lines_before = start_line.saturating_sub(start);
    let lines_after = last_line.saturating_sub(end_line);

    Ok(Some(CodeContext {
        code,
        lines_before,
        lines_after,
    }))
}

// =============================================================================
// NodeId-based functions (new - use graph lookups instead of Symbol)
// =============================================================================

/// Filter a node using graph lookups.
pub(crate) fn filter_node(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    filters: &SearchFilters,
) -> bool {
    matches_language_filter_node(snapshot, node_id, filters)
        && matches_visibility_filter_node(snapshot, node_id, filters)
        && matches_kind_filter_node(snapshot, node_id, filters)
}

fn matches_language_filter_node(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    filters: &SearchFilters,
) -> bool {
    if filters.languages.is_empty() {
        return true;
    }

    let Some(entry) = snapshot.get_node(node_id) else {
        return false;
    };

    let lang = snapshot.files().language_for_file(entry.file).map_or_else(
        || "unknown".to_string(),
        |l| l.to_string().to_ascii_lowercase(),
    );

    filters
        .languages
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&lang))
}

fn matches_visibility_filter_node(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    filters: &SearchFilters,
) -> bool {
    let Some(visibility) = filters.visibility else {
        return true;
    };

    let Some(entry) = snapshot.get_node(node_id) else {
        return false;
    };

    let node_visibility = entry
        .visibility
        .and_then(|id| snapshot.strings().resolve(id))
        .map(|s| s.to_ascii_lowercase());

    match visibility {
        Visibility::Public => node_visibility.as_deref() == Some("public"),
        Visibility::Private => node_visibility.as_deref() == Some("private"),
    }
}

fn matches_kind_filter_node(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    filters: &SearchFilters,
) -> bool {
    if filters.kinds.is_empty() {
        return true;
    }

    let Some(entry) = snapshot.get_node(node_id) else {
        return false;
    };

    let kind = node_kind_to_string(entry.kind);
    filters
        .kinds
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(kind))
}

/// Build search hits from node IDs using graph lookups.
pub(crate) fn build_search_hits_from_nodes(
    snapshot: &GraphSnapshot,
    nodes: &[(NodeId, f64)],
    context_lines: usize,
    workspace_root: &Path,
) -> Result<Vec<SearchHit>> {
    let mut hits = Vec::with_capacity(nodes.len());

    for &(node_id, score) in nodes {
        hits.push(build_search_hit_from_node(
            snapshot,
            node_id,
            score,
            context_lines,
            workspace_root,
        )?);
    }

    Ok(hits)
}

fn build_search_hit_from_node(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    score: f64,
    context_lines: usize,
    workspace_root: &Path,
) -> Result<SearchHit> {
    let reference = node_to_ref(snapshot, node_id, workspace_root)?;
    let entry = snapshot
        .get_node(node_id)
        .ok_or_else(|| anyhow!("Node not found"))?;

    let file_path = snapshot
        .files()
        .resolve(entry.file)
        .map(|p| workspace_root.join(p.as_ref()))
        .ok_or_else(|| anyhow!("File not found for node"))?;

    let context = build_context(
        &file_path,
        entry.start_line as usize,
        entry.end_line as usize,
        context_lines,
    )?;

    let signature = entry
        .signature
        .and_then(|sid| snapshot.strings().resolve(sid))
        .map(|s| s.to_string());

    Ok(SearchHit {
        name: reference.name.clone(),
        qualified_name: reference.qualified_name.clone(),
        kind: reference.kind.clone(),
        language: reference.language.clone(),
        file_uri: reference.file_uri.clone(),
        range: reference.range.clone(),
        score: (score * 1000.0).round() / 1000.0,
        context,
        metadata: reference.metadata.clone(),
        signature,
        relations: None,
    })
}

/// Convert a node ID to reference data using graph lookups.
pub(crate) fn node_to_ref(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    workspace_root: &Path,
) -> Result<NodeRefData> {
    let entry = snapshot
        .get_node(node_id)
        .ok_or_else(|| anyhow!("Node not found"))?;
    let strings = snapshot.strings();
    let files = snapshot.files();

    let name = strings
        .resolve(entry.name)
        .map(|s| s.to_string())
        .unwrap_or_default();
    let qualified_name = entry
        .qualified_name
        .and_then(|sid| strings.resolve(sid))
        .map_or_else(|| name.clone(), |s| s.to_string());

    let file_path = files
        .resolve(entry.file)
        .map(|p| workspace_root.join(p.as_ref()))
        .ok_or_else(|| anyhow!("File not found for node"))?;

    let file_uri = path_to_uri(&file_path)?;

    let language = files.language_for_file(entry.file).map_or_else(
        || "unknown".to_string(),
        |l| l.to_string().to_ascii_lowercase(),
    );

    let range = RangeData {
        start: PositionData {
            line: entry.start_line.saturating_sub(1),
            character: entry.start_column,
        },
        end: PositionData {
            line: entry.end_line.saturating_sub(1),
            character: entry.end_column,
        },
    };

    Ok(NodeRefData {
        name,
        qualified_name,
        kind: node_kind_to_string(entry.kind).to_string(),
        language,
        file_uri,
        range,
        metadata: None,
    })
}

/// Convert `NodeKind` to lowercase string for output.
fn node_kind_to_string(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Function => "function",
        NodeKind::Method => "method",
        NodeKind::Class => "class",
        NodeKind::Interface => "interface",
        NodeKind::Trait => "trait",
        NodeKind::Module => "module",
        NodeKind::Variable => "variable",
        NodeKind::Constant => "constant",
        NodeKind::Type => "type",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::EnumVariant => "enum_variant",
        NodeKind::Macro => "macro",
        NodeKind::Parameter => "parameter",
        NodeKind::Property => "property",
        NodeKind::Import => "import",
        NodeKind::Export => "export",
        NodeKind::Component => "component",
        NodeKind::Service => "service",
        NodeKind::Resource => "resource",
        NodeKind::Endpoint => "endpoint",
        NodeKind::Test => "test",
        NodeKind::CallSite => "call_site",
        NodeKind::StyleRule => "style_rule",
        NodeKind::StyleAtRule => "style_at_rule",
        NodeKind::StyleVariable => "style_variable",
        NodeKind::Lifetime => "lifetime",
        NodeKind::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ==========================================================================
    // path_to_forward_slash / relative_path_forward_slash tests
    // ==========================================================================

    #[test]
    fn test_path_to_forward_slash_unix_style() {
        let path = PathBuf::from("src/lib.rs");
        assert_eq!(path_to_forward_slash(&path), "src/lib.rs");
    }

    #[test]
    fn test_path_to_forward_slash_backslash() {
        // Simulate Windows-style path via raw string
        let result = path_to_forward_slash(Path::new("src/lib.rs"));
        assert_eq!(result, "src/lib.rs");
    }

    #[test]
    fn test_relative_path_forward_slash_strips_prefix() {
        let workspace = PathBuf::from("/home/user/project");
        let full = PathBuf::from("/home/user/project/src/lib.rs");
        assert_eq!(relative_path_forward_slash(&full, &workspace), "src/lib.rs");
    }

    #[test]
    fn test_relative_path_forward_slash_no_prefix() {
        let workspace = PathBuf::from("/other/root");
        let full = PathBuf::from("/home/user/project/src/lib.rs");
        // Falls back to full path when prefix doesn't match
        let result = relative_path_forward_slash(&full, &workspace);
        assert!(result.contains("src/lib.rs"));
    }

    // ==========================================================================
    // path_to_uri tests
    // ==========================================================================

    #[test]
    fn test_path_to_uri_absolute() {
        // Use platform-appropriate absolute paths
        let path = if cfg!(windows) {
            PathBuf::from(r"C:\Users\user\project\src\main.rs")
        } else {
            PathBuf::from("/home/user/project/src/main.rs")
        };
        let uri = path_to_uri(&path).unwrap();
        assert!(uri.starts_with("file:///"));
        assert!(uri.contains("main.rs"));
    }

    #[test]
    fn test_path_to_uri_with_spaces() {
        // Use platform-appropriate absolute paths
        let path = if cfg!(windows) {
            PathBuf::from(r"C:\Users\user\my project\src\main.rs")
        } else {
            PathBuf::from("/home/user/my project/src/main.rs")
        };
        let uri = path_to_uri(&path).unwrap();
        assert!(uri.starts_with("file:///"));
        assert!(uri.contains("my%20project") || uri.contains("main.rs"));
    }

    // ==========================================================================
    // PositionData and RangeData tests
    // ==========================================================================

    #[test]
    fn test_position_data_creation() {
        let pos = PositionData {
            line: 10,
            character: 5,
        };
        assert_eq!(pos.line, 10);
        assert_eq!(pos.character, 5);
    }

    #[test]
    fn test_range_data_creation() {
        let range = RangeData {
            start: PositionData {
                line: 1,
                character: 0,
            },
            end: PositionData {
                line: 5,
                character: 10,
            },
        };
        assert_eq!(range.start.line, 1);
        assert_eq!(range.end.line, 5);
    }
}
