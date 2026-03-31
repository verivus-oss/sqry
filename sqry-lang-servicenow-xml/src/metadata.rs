//! Record metadata extraction, name sanitization, and synthetic path construction.

use std::path::{Path, PathBuf};

/// Metadata extracted from a ServiceNow XML record element.
#[derive(Debug, Clone)]
pub struct RecordMetadata {
    /// Record name (from `<name>` element).
    pub name: String,
    /// ServiceNow table type (from `<record_update table="...">` attribute).
    pub table: String,
    /// Scope display value (from `<sys_scope display_value="...">` attribute).
    pub scope: String,
    /// Target table (from `<collection>` element, for Business Rules).
    pub collection: String,
}

impl RecordMetadata {
    /// Extract metadata from a roxmltree record element.
    #[must_use]
    pub fn extract(record: &roxmltree::Node<'_, '_>, table: &str) -> Self {
        Self {
            name: child_text(record, "name").unwrap_or("").trim().to_string(),
            table: table.to_string(),
            scope: record
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "sys_scope")
                .and_then(|n| n.attribute("display_value"))
                .unwrap_or("")
                .trim()
                .to_string(),
            collection: child_text(record, "collection")
                .unwrap_or("")
                .trim()
                .to_string(),
        }
    }
}

/// Get text content of a named child element.
#[must_use]
pub fn child_text<'a>(parent: &'a roxmltree::Node<'_, '_>, name: &str) -> Option<&'a str> {
    parent
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == name)
        .and_then(|n| n.text())
}

/// Sanitize a ServiceNow record name for use in synthetic file paths.
///
/// Replaces unsafe characters (slashes, dots, spaces, special chars) with
/// underscores. Strips leading underscores/dashes, truncates to 200 chars,
/// and falls back to `"unnamed"` if the result is empty.
#[must_use]
pub fn sanitize_record_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | '.' | ' ' | '\t' | '\n' | '\r' | ':' | '*' | '?' | '"' | '<' | '>'
            | '|' => '_',
            _ => c,
        })
        .collect();

    let trimmed = sanitized.trim_start_matches(['_', '-']);

    // Truncate to 200 chars on a char boundary (not byte boundary) to avoid
    // panicking on multi-byte Unicode characters.
    let result: &str = if trimmed.chars().count() > 200 {
        let end = trimmed
            .char_indices()
            .nth(200)
            .map_or(trimmed.len(), |(i, _)| i);
        &trimmed[..end]
    } else {
        trimmed
    };

    if result.is_empty() {
        "unnamed".to_string()
    } else {
        result.to_string()
    }
}

/// Build a synthetic file path for delegated JS extraction.
///
/// Format: `<xml_dir>/<xml_stem>__<table>.<sanitized_name>.snjs`
/// Multi-record: `<xml_dir>/<xml_stem>__<table>.<sanitized_name>_<idx>.snjs`
///
/// The XML file stem prefix ensures global uniqueness across different XML files
/// that may carry the same table + record name (RT-1 from red team assessment).
/// The `ServiceNowGraphBuilder` uses `file.file_stem()` to derive the module name.
#[must_use]
pub fn synthetic_path(
    xml_path: &Path,
    metadata: &RecordMetadata,
    record_idx: usize,
    multi_record: bool,
) -> PathBuf {
    let parent = xml_path.parent().unwrap_or(Path::new(""));

    let xml_stem = xml_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let name_part = if metadata.name.is_empty() {
        sanitize_record_name(xml_stem)
    } else {
        sanitize_record_name(&metadata.name)
    };

    let table_prefix = if metadata.table.is_empty() {
        String::new()
    } else {
        format!("{}.", metadata.table)
    };

    let filename = if multi_record {
        format!("{xml_stem}__{table_prefix}{name_part}_{record_idx}.snjs")
    } else {
        format!("{xml_stem}__{table_prefix}{name_part}.snjs")
    };

    parent.join(filename)
}
