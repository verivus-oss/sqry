//! Diff command implementation
//!
//! Compares code semantics between two git refs using AST analysis.

use crate::args::Cli;
use crate::output::{CsvColumn, OutputStreams, parse_columns, resolve_theme};
use crate::plugin_defaults::{self, PluginSelectionMode};
use anyhow::{Context, Result, bail};
use colored::Colorize;
use serde::Serialize;
use sqry_core::git::WorktreeManager;
use sqry_core::graph::diff::{DiffSummary, GraphComparator, NodeChange, NodeLocation};
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

// ============================================================================
// Display Types
// ============================================================================

/// CLI result structure for diff output
#[derive(Clone, Debug, Serialize)]
pub struct DiffDisplayResult {
    pub base_ref: String,
    pub target_ref: String,
    pub changes: Vec<DiffDisplayChange>,
    pub summary: DiffDisplaySummary,
    pub total: usize,
    pub truncated: bool,
}

/// Individual change record for CLI output
#[derive(Clone, Debug, Serialize)]
pub struct DiffDisplayChange {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub change_type: String,
    pub base_location: Option<DiffLocation>,
    pub target_location: Option<DiffLocation>,
    pub signature_before: Option<String>,
    pub signature_after: Option<String>,
}

/// Location information for CLI output
#[derive(Clone, Debug, Serialize)]
pub struct DiffLocation {
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
}

/// Summary statistics for CLI output
#[derive(Clone, Debug, Serialize)]
pub struct DiffDisplaySummary {
    pub added: u64,
    pub removed: u64,
    pub modified: u64,
    pub renamed: u64,
    pub signature_changed: u64,
}

// ============================================================================
// Main Command Implementation
// ============================================================================

/// Run the diff command.
///
/// Compares symbols between two git refs by:
/// 1. Creating temporary git worktrees
/// 2. Building `CodeGraph`s for each ref
/// 3. Comparing graphs to detect changes
/// 4. Formatting and outputting results
///
/// # Errors
///
/// Returns error if:
/// - Not a git repository
/// - Git refs are invalid
/// - Graph building fails
/// - Output formatting fails
pub fn run_diff(
    cli: &Cli,
    base_ref: &str,
    target_ref: &str,
    path: Option<&str>,
    max_results: usize,
    kinds: &[String],
    change_types: &[String],
) -> Result<()> {
    // 1. Validate and resolve repository root
    let root = resolve_repo_root(path, cli)?;

    // 2. Create temporary git worktrees
    let worktree_mgr = WorktreeManager::create(&root, base_ref, target_ref)
        .context("Failed to create git worktrees")?;

    log::debug!(
        "Created worktrees for diff: base={} target={} base_path={} target_path={}",
        base_ref,
        target_ref,
        worktree_mgr.base_path().display(),
        worktree_mgr.target_path().display()
    );

    // 3. Build graphs for both refs
    let resolved_plugins =
        plugin_defaults::resolve_plugin_selection(cli, &root, PluginSelectionMode::Diff)?;
    let config = BuildConfig::default();

    let base_graph = Arc::new(
        build_unified_graph(
            worktree_mgr.base_path(),
            &resolved_plugins.plugin_manager,
            &config,
        )
        .context(format!("Failed to build graph for base ref '{base_ref}'"))?,
    );
    log::debug!("Built base graph: ref={base_ref}");

    let target_graph = Arc::new(
        build_unified_graph(
            worktree_mgr.target_path(),
            &resolved_plugins.plugin_manager,
            &config,
        )
        .context(format!(
            "Failed to build graph for target ref '{target_ref}'"
        ))?,
    );
    log::debug!("Built target graph: ref={target_ref}");

    // 4. Compare graphs
    let comparator = GraphComparator::new(
        base_graph,
        target_graph,
        root.clone(),
        worktree_mgr.base_path().to_path_buf(),
        worktree_mgr.target_path().to_path_buf(),
    );
    let result = comparator
        .compute_changes()
        .context("Failed to compute changes")?;

    log::debug!(
        "Computed changes: total={} added={} removed={} modified={} renamed={} signature_changed={}",
        result.changes.len(),
        result.summary.added,
        result.summary.removed,
        result.summary.modified,
        result.summary.renamed,
        result.summary.signature_changed
    );

    // 5. Filter results by kind and change type
    let filtered_changes = filter_changes(result.changes, kinds, change_types);

    // 6. Recompute summary from filtered changes (per Codex review: summary must match output)
    let filtered_summary = compute_summary(&filtered_changes);

    // 7. Apply limit and track truncation
    let total = filtered_changes.len();
    let truncated = filtered_changes.len() > max_results;
    let limited_changes = limit_changes(filtered_changes, max_results);

    // 8. Format and output
    format_and_output(
        cli,
        base_ref,
        target_ref,
        limited_changes,
        &filtered_summary,
        total,
        truncated,
    )?;

    // WorktreeManager drops here, automatically cleaning up worktrees
    Ok(())
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Resolve repository root path
///
/// Walks up from the given path to find the git repository root.
/// This allows --path to accept any path within a repository.
fn resolve_repo_root(path: Option<&str>, cli: &Cli) -> Result<PathBuf> {
    let start_path = if let Some(p) = path {
        PathBuf::from(p)
    } else {
        PathBuf::from(cli.search_path())
    };

    // Canonicalize to absolute path
    let start_path = start_path
        .canonicalize()
        .context(format!("Failed to resolve path: {}", start_path.display()))?;

    // Walk up to find .git directory
    let mut current = start_path.as_path();
    loop {
        if current.join(".git").exists() {
            return Ok(current.to_path_buf());
        }

        match current.parent() {
            Some(parent) => current = parent,
            None => bail!(
                "Not a git repository (or any parent up to mount point): {}",
                start_path.display()
            ),
        }
    }
}

/// Compute summary statistics from a list of changes
///
/// Used to recompute summary after filtering to ensure summary counts match actual output
/// (per Codex review feedback)
fn compute_summary(changes: &[NodeChange]) -> DiffSummary {
    use sqry_core::graph::diff::ChangeType;

    let mut summary = DiffSummary {
        added: 0,
        removed: 0,
        modified: 0,
        renamed: 0,
        signature_changed: 0,
        unchanged: 0, // Always 0 for filtered changes list
    };

    for change in changes {
        match change.change_type {
            ChangeType::Added => summary.added += 1,
            ChangeType::Removed => summary.removed += 1,
            ChangeType::Modified => summary.modified += 1,
            ChangeType::Renamed => summary.renamed += 1,
            ChangeType::SignatureChanged => summary.signature_changed += 1,
            ChangeType::Unchanged => summary.unchanged += 1,
        }
    }

    summary
}

/// Filter changes by symbol kind and change type
fn filter_changes(
    changes: Vec<NodeChange>,
    kinds: &[String],
    change_types: &[String],
) -> Vec<NodeChange> {
    changes
        .into_iter()
        .filter(|change| {
            // Filter by kind if specified
            let kind_matches =
                kinds.is_empty() || kinds.iter().any(|k| k.eq_ignore_ascii_case(&change.kind));

            // Filter by change type if specified
            let change_type_matches = change_types.is_empty()
                || change_types
                    .iter()
                    .any(|ct| ct.eq_ignore_ascii_case(change.change_type.as_str()));

            kind_matches && change_type_matches
        })
        .collect()
}

/// Limit number of changes and return truncated list
fn limit_changes(changes: Vec<NodeChange>, limit: usize) -> Vec<NodeChange> {
    if changes.len() <= limit {
        changes
    } else {
        changes.into_iter().take(limit).collect()
    }
}

/// Format and output results based on CLI flags
fn format_and_output(
    cli: &Cli,
    base_ref: &str,
    target_ref: &str,
    changes: Vec<NodeChange>,
    summary: &DiffSummary,
    total: usize,
    truncated: bool,
) -> Result<()> {
    let mut streams = OutputStreams::new();

    // Convert to display types
    let result = DiffDisplayResult {
        base_ref: base_ref.to_string(),
        target_ref: target_ref.to_string(),
        changes: changes.into_iter().map(convert_change).collect(),
        summary: convert_summary(summary),
        total,
        truncated,
    };

    // Select formatter based on CLI flags
    match (cli.json, cli.csv, cli.tsv) {
        (true, _, _) => format_json_output(&mut streams, &result),
        (_, true, _) => format_csv_output_shared(cli, &mut streams, &result, ','),
        (_, _, true) => format_csv_output_shared(cli, &mut streams, &result, '\t'),
        _ => {
            let theme = resolve_theme(cli);
            let use_color = !cli.no_color
                && theme != crate::output::ThemeName::None
                && std::env::var("NO_COLOR").is_err();
            format_text_output(&mut streams, &result, use_color)
        }
    }
}

// ============================================================================
// Conversion Functions
// ============================================================================

fn convert_change(change: NodeChange) -> DiffDisplayChange {
    DiffDisplayChange {
        name: change.name,
        qualified_name: change.qualified_name,
        kind: change.kind,
        change_type: change.change_type.as_str().to_string(),
        base_location: change.base_location.map(|loc| convert_location(&loc)),
        target_location: change.target_location.map(|loc| convert_location(&loc)),
        signature_before: change.signature_before,
        signature_after: change.signature_after,
    }
}

fn convert_location(loc: &NodeLocation) -> DiffLocation {
    DiffLocation {
        file_path: loc.file_path.display().to_string(),
        start_line: loc.start_line,
        end_line: loc.end_line,
    }
}

fn convert_summary(summary: &DiffSummary) -> DiffDisplaySummary {
    DiffDisplaySummary {
        added: summary.added,
        removed: summary.removed,
        modified: summary.modified,
        renamed: summary.renamed,
        signature_changed: summary.signature_changed,
    }
}

// ============================================================================
// Text Formatter
// ============================================================================

fn format_text_output(
    streams: &mut OutputStreams,
    result: &DiffDisplayResult,
    use_color: bool,
) -> Result<()> {
    let mut output = String::new();

    // Header
    let _ = writeln!(
        output,
        "Comparing {}...{}\n",
        result.base_ref, result.target_ref
    );

    // Summary section
    output.push_str("Summary:\n");
    let _ = writeln!(output, "  Added: {}", result.summary.added);
    let _ = writeln!(output, "  Removed: {}", result.summary.removed);
    let _ = writeln!(output, "  Modified: {}", result.summary.modified);
    let _ = writeln!(output, "  Renamed: {}", result.summary.renamed);
    let _ = writeln!(
        output,
        "  Signature Changed: {}\n",
        result.summary.signature_changed
    );

    // Group changes by type
    let by_type = group_by_change_type(&result.changes);

    // Define order for change types
    let order = vec![
        "added",
        "removed",
        "modified",
        "renamed",
        "signature_changed",
    ];

    for change_type in order {
        if let Some(changes) = by_type.get(change_type) {
            if changes.is_empty() {
                continue;
            }

            // Section header with color
            let header = capitalize_change_type(change_type);
            if use_color {
                output.push_str(&colorize_header(&header, change_type));
            } else {
                let _ = write!(output, "{header}:");
            }
            output.push('\n');

            // Print each change
            for change in changes {
                format_change_text(&mut output, change, use_color);
            }
            output.push('\n');
        }
    }

    // Truncation warning
    if result.truncated {
        let _ = writeln!(
            output,
            "Note: Output limited to {} results (total: {})",
            result.changes.len(),
            result.total
        );
    }

    streams.write_result(output.trim_end())?;
    Ok(())
}

fn group_by_change_type(changes: &[DiffDisplayChange]) -> HashMap<String, Vec<&DiffDisplayChange>> {
    let mut grouped: HashMap<String, Vec<&DiffDisplayChange>> = HashMap::new();
    for change in changes {
        grouped
            .entry(change.change_type.clone())
            .or_default()
            .push(change);
    }
    grouped
}

fn format_change_text(output: &mut String, change: &DiffDisplayChange, use_color: bool) {
    // Name and kind
    if use_color {
        let _ = writeln!(
            output,
            "  {} [{}]",
            colorize_symbol(&change.name, &change.change_type),
            change.kind
        );
    } else {
        let _ = writeln!(output, "  {} [{}]", change.name, change.kind);
    }

    // Qualified name (if different from name)
    if change.qualified_name != change.name {
        let _ = writeln!(output, "    {}", change.qualified_name);
    }

    // Location
    if let Some(loc) = change
        .target_location
        .as_ref()
        .or(change.base_location.as_ref())
    {
        let _ = writeln!(output, "    Location: {}:{}", loc.file_path, loc.start_line);
    }

    // Signatures (for signature changes)
    if let (Some(before), Some(after)) = (&change.signature_before, &change.signature_after) {
        if before != after {
            let _ = writeln!(output, "    Before: {before}");
            let _ = writeln!(output, "    After:  {after}");
        }
    } else if let Some(sig) = &change.signature_before {
        let _ = writeln!(output, "    Signature: {sig}");
    } else if let Some(sig) = &change.signature_after {
        let _ = writeln!(output, "    Signature: {sig}");
    }
}

fn capitalize_change_type(s: &str) -> String {
    match s {
        "added" => "Added".to_string(),
        "removed" => "Removed".to_string(),
        "modified" => "Modified".to_string(),
        "renamed" => "Renamed".to_string(),
        "signature_changed" => "Signature Changed".to_string(),
        _ => s.to_string(),
    }
}

fn colorize_header(header: &str, change_type: &str) -> String {
    match change_type {
        "added" => format!("{}:", header.green().bold()),
        "removed" => format!("{}:", header.red().bold()),
        "modified" | "renamed" => format!("{}:", header.yellow().bold()),
        "signature_changed" => format!("{}:", header.blue().bold()),
        _ => format!("{header}:"),
    }
}

fn colorize_symbol(name: &str, change_type: &str) -> String {
    match change_type {
        "added" => name.green().to_string(),
        "removed" => name.red().to_string(),
        "modified" | "renamed" => name.yellow().to_string(),
        "signature_changed" => name.blue().to_string(),
        _ => name.to_string(),
    }
}

// ============================================================================
// JSON Formatter
// ============================================================================

fn format_json_output(streams: &mut OutputStreams, result: &DiffDisplayResult) -> Result<()> {
    let json =
        serde_json::to_string_pretty(result).context("Failed to serialize diff results to JSON")?;
    streams.write_result(&json)?;
    Ok(())
}

// ============================================================================
// CSV/TSV Formatter
// ============================================================================

/// Format CSV/TSV output for diff command
///
/// Since `diff` has custom fields (`change_type`, `signature_before`, `signature_after`) that
/// aren't part of the standard `CsvColumn` enum, we handle CSV output manually here
/// while still using the same escaping logic as the shared formatter.
fn format_csv_output_shared(
    cli: &Cli,
    streams: &mut OutputStreams,
    result: &DiffDisplayResult,
    delimiter: char,
) -> Result<()> {
    let mut output = String::new();
    let is_tsv = delimiter == '\t';

    // Determine columns
    let columns = if let Some(cols_spec) = &cli.columns {
        // Parse user-specified columns - convert CsvColumn to DiffColumn where possible
        let csv_cols = parse_columns(Some(cols_spec)).map_err(|e| anyhow::anyhow!("{e}"))?;

        match csv_cols {
            Some(cols) => {
                let requested_count = cols.len();
                // Convert CsvColumn to DiffColumn (only standard columns are supported via --columns)
                let converted: Vec<DiffColumn> =
                    cols.into_iter().filter_map(csv_to_diff_column).collect();

                // Validate that at least some columns are supported
                if converted.is_empty() {
                    bail!(
                        "No supported columns specified for diff output.\n\
                         Supported columns: name, qualified_name, kind, file, line, change_type, signature_before, signature_after\n\
                         Unsupported for diff: column, end_line, end_column, language, preview"
                    );
                }

                // Warn if some columns were dropped
                let matched_count = converted.len();
                if matched_count < requested_count {
                    eprintln!(
                        "Warning: {} of {} requested columns are not supported by diff output",
                        requested_count - matched_count,
                        requested_count
                    );
                }

                converted
            }
            None => get_default_diff_columns(),
        }
    } else {
        get_default_diff_columns()
    };

    // Write header
    if cli.headers {
        let headers: Vec<&str> = columns.iter().copied().map(column_header).collect();
        output.push_str(&headers.join(&delimiter.to_string()));
        output.push('\n');
    }

    // Write data rows
    for change in &result.changes {
        let fields: Vec<String> = columns
            .iter()
            .map(|col| {
                let value = get_column_value(change, *col);
                escape_field(&value, delimiter, is_tsv, cli.raw_csv)
            })
            .collect();

        output.push_str(&fields.join(&delimiter.to_string()));
        output.push('\n');
    }

    streams.write_result(output.trim_end())?;
    Ok(())
}

/// Get default columns for diff CSV output
fn get_default_diff_columns() -> Vec<DiffColumn> {
    vec![
        DiffColumn::Name,
        DiffColumn::QualifiedName,
        DiffColumn::Kind,
        DiffColumn::ChangeType,
        DiffColumn::File,
        DiffColumn::Line,
        DiffColumn::SignatureBefore,
        DiffColumn::SignatureAfter,
    ]
}

/// Columns available for diff CSV output
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffColumn {
    Name,
    QualifiedName,
    Kind,
    ChangeType,
    File,
    Line,
    SignatureBefore,
    SignatureAfter,
}

/// Convert `CsvColumn` to `DiffColumn` for `--columns` support.
fn csv_to_diff_column(col: CsvColumn) -> Option<DiffColumn> {
    match col {
        CsvColumn::Name => Some(DiffColumn::Name),
        CsvColumn::QualifiedName => Some(DiffColumn::QualifiedName),
        CsvColumn::Kind => Some(DiffColumn::Kind),
        CsvColumn::File => Some(DiffColumn::File),
        CsvColumn::Line => Some(DiffColumn::Line),
        CsvColumn::ChangeType => Some(DiffColumn::ChangeType),
        CsvColumn::SignatureBefore => Some(DiffColumn::SignatureBefore),
        CsvColumn::SignatureAfter => Some(DiffColumn::SignatureAfter),
        // Columns not applicable to diff output
        CsvColumn::Column
        | CsvColumn::EndLine
        | CsvColumn::EndColumn
        | CsvColumn::Language
        | CsvColumn::Preview => None,
    }
}

/// Get header name for a diff column.
fn column_header(col: DiffColumn) -> &'static str {
    match col {
        DiffColumn::Name => "name",
        DiffColumn::QualifiedName => "qualified_name",
        DiffColumn::Kind => "kind",
        DiffColumn::ChangeType => "change_type",
        DiffColumn::File => "file",
        DiffColumn::Line => "line",
        DiffColumn::SignatureBefore => "signature_before",
        DiffColumn::SignatureAfter => "signature_after",
    }
}

/// Get column value for a diff change.
fn get_column_value(change: &DiffDisplayChange, col: DiffColumn) -> String {
    match col {
        DiffColumn::Name => change.name.clone(),
        DiffColumn::QualifiedName => change.qualified_name.clone(),
        DiffColumn::Kind => change.kind.clone(),
        DiffColumn::ChangeType => change.change_type.clone(),
        DiffColumn::File => {
            let location = change
                .target_location
                .as_ref()
                .or(change.base_location.as_ref());
            location
                .map(|loc| loc.file_path.clone())
                .unwrap_or_default()
        }
        DiffColumn::Line => {
            let location = change
                .target_location
                .as_ref()
                .or(change.base_location.as_ref());
            location
                .map(|loc| loc.start_line.to_string())
                .unwrap_or_default()
        }
        DiffColumn::SignatureBefore => change.signature_before.clone().unwrap_or_default(),
        DiffColumn::SignatureAfter => change.signature_after.clone().unwrap_or_default(),
    }
}

/// Escape a field value for CSV/TSV output
///
/// Uses the same logic as the shared `CsvFormatter`.
fn escape_field(value: &str, delimiter: char, is_tsv: bool, raw: bool) -> String {
    if is_tsv {
        escape_tsv_field(value, raw)
    } else {
        escape_csv_field(value, delimiter, raw)
    }
}

/// Escape CSV field (RFC 4180)
fn escape_csv_field(value: &str, delimiter: char, raw: bool) -> String {
    let needs_quoting = value.contains(delimiter)
        || value.contains('"')
        || value.contains('\n')
        || value.contains('\r');

    let escaped = if needs_quoting {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    };

    if raw {
        escaped
    } else {
        apply_formula_protection(&escaped)
    }
}

/// Escape TSV field
fn escape_tsv_field(value: &str, raw: bool) -> String {
    let escaped: String = value
        .chars()
        .filter_map(|c| match c {
            '\t' | '\n' => Some(' '),
            '\r' => None,
            _ => Some(c),
        })
        .collect();

    if raw {
        escaped
    } else {
        apply_formula_protection(&escaped)
    }
}

/// Formula injection protection
const FORMULA_CHARS: &[char] = &['=', '+', '-', '@', '\t', '\r'];

fn apply_formula_protection(value: &str) -> String {
    if let Some(first_char) = value.chars().next()
        && FORMULA_CHARS.contains(&first_char)
    {
        return format!("'{value}");
    }
    value.to_string()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::large_stack_test;
    use sqry_core::graph::diff::ChangeType;

    #[test]
    fn test_convert_change() {
        let core_change = NodeChange {
            name: "test".to_string(),
            qualified_name: "mod::test".to_string(),
            kind: "function".to_string(),
            change_type: ChangeType::Added,
            base_location: None,
            target_location: Some(NodeLocation {
                file_path: PathBuf::from("test.rs"),
                start_line: 10,
                end_line: 15,
                start_column: 0,
                end_column: 1,
            }),
            signature_before: None,
            signature_after: Some("fn test()".to_string()),
        };

        let display_change = convert_change(core_change);

        assert_eq!(display_change.name, "test");
        assert_eq!(display_change.qualified_name, "mod::test");
        assert_eq!(display_change.kind, "function");
        assert_eq!(display_change.change_type, "added");
        assert!(display_change.base_location.is_none());
        assert!(display_change.target_location.is_some());

        let loc = display_change.target_location.unwrap();
        assert_eq!(loc.file_path, "test.rs");
        assert_eq!(loc.start_line, 10);
    }

    #[test]
    fn test_filter_by_kind() {
        let changes = vec![
            NodeChange {
                name: "func1".to_string(),
                qualified_name: "func1".to_string(),
                kind: "function".to_string(),
                change_type: ChangeType::Added,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
            NodeChange {
                name: "class1".to_string(),
                qualified_name: "class1".to_string(),
                kind: "class".to_string(),
                change_type: ChangeType::Added,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
        ];

        let filtered = filter_changes(changes, &["function".to_string()], &[]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].kind, "function");
    }

    #[test]
    fn test_filter_by_change_type() {
        let changes = vec![
            NodeChange {
                name: "func1".to_string(),
                qualified_name: "func1".to_string(),
                kind: "function".to_string(),
                change_type: ChangeType::Added,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
            NodeChange {
                name: "func2".to_string(),
                qualified_name: "func2".to_string(),
                kind: "function".to_string(),
                change_type: ChangeType::Removed,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
        ];

        let filtered = filter_changes(changes, &[], &["added".to_string()]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "func1");
    }

    #[test]
    fn test_limit_changes() {
        let changes = vec![
            NodeChange {
                name: format!("func{}", 1),
                qualified_name: String::new(),
                kind: "function".to_string(),
                change_type: ChangeType::Added,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
            NodeChange {
                name: format!("func{}", 2),
                qualified_name: String::new(),
                kind: "function".to_string(),
                change_type: ChangeType::Added,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
            NodeChange {
                name: format!("func{}", 3),
                qualified_name: String::new(),
                kind: "function".to_string(),
                change_type: ChangeType::Added,
                base_location: None,
                target_location: None,
                signature_before: None,
                signature_after: None,
            },
        ];

        let limited = limit_changes(changes, 2);
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].name, "func1");
        assert_eq!(limited[1].name, "func2");
    }

    #[test]
    fn test_csv_escaping() {
        // Test CSV field escaping
        assert_eq!(escape_csv_field("simple", ',', false), "simple");
        assert_eq!(escape_csv_field("has,comma", ',', false), "\"has,comma\"");
        assert_eq!(
            escape_csv_field("has\"quote", ',', false),
            "\"has\"\"quote\""
        );

        // Test formula protection
        assert_eq!(escape_csv_field("=SUM(A1)", ',', false), "'=SUM(A1)");
        assert_eq!(escape_csv_field("+123", ',', false), "'+123");

        // Test raw mode
        assert_eq!(escape_csv_field("=SUM(A1)", ',', true), "=SUM(A1)");
    }

    #[test]
    fn test_tsv_escaping() {
        // Test TSV field escaping - tabs and newlines become spaces
        assert_eq!(escape_tsv_field("simple", false), "simple");
        assert_eq!(escape_tsv_field("has\ttab", false), "has tab");
        assert_eq!(escape_tsv_field("has\nnewline", false), "has newline");
        assert_eq!(escape_tsv_field("has\rcarriage", false), "hascarriage");
    }

    #[test]
    fn test_capitalize_change_type() {
        assert_eq!(capitalize_change_type("added"), "Added");
        assert_eq!(capitalize_change_type("removed"), "Removed");
        assert_eq!(
            capitalize_change_type("signature_changed"),
            "Signature Changed"
        );
    }

    #[test]
    fn test_csv_column_to_diff_column_mapping() {
        // Test that standard columns map correctly
        assert_eq!(csv_to_diff_column(CsvColumn::Name), Some(DiffColumn::Name));
        assert_eq!(
            csv_to_diff_column(CsvColumn::QualifiedName),
            Some(DiffColumn::QualifiedName)
        );
        assert_eq!(csv_to_diff_column(CsvColumn::Kind), Some(DiffColumn::Kind));
        assert_eq!(csv_to_diff_column(CsvColumn::File), Some(DiffColumn::File));
        assert_eq!(csv_to_diff_column(CsvColumn::Line), Some(DiffColumn::Line));

        // Test that diff-specific columns map correctly
        assert_eq!(
            csv_to_diff_column(CsvColumn::ChangeType),
            Some(DiffColumn::ChangeType)
        );
        assert_eq!(
            csv_to_diff_column(CsvColumn::SignatureBefore),
            Some(DiffColumn::SignatureBefore)
        );
        assert_eq!(
            csv_to_diff_column(CsvColumn::SignatureAfter),
            Some(DiffColumn::SignatureAfter)
        );

        // Test that unsupported columns return None
        assert_eq!(csv_to_diff_column(CsvColumn::Column), None);
        assert_eq!(csv_to_diff_column(CsvColumn::EndLine), None);
        assert_eq!(csv_to_diff_column(CsvColumn::EndColumn), None);
        assert_eq!(csv_to_diff_column(CsvColumn::Language), None);
        assert_eq!(csv_to_diff_column(CsvColumn::Preview), None);
    }

    #[test]
    fn test_diff_column_headers() {
        // Verify header names for all diff columns
        assert_eq!(column_header(DiffColumn::Name), "name");
        assert_eq!(column_header(DiffColumn::QualifiedName), "qualified_name");
        assert_eq!(column_header(DiffColumn::Kind), "kind");
        assert_eq!(column_header(DiffColumn::ChangeType), "change_type");
        assert_eq!(column_header(DiffColumn::File), "file");
        assert_eq!(column_header(DiffColumn::Line), "line");
        assert_eq!(
            column_header(DiffColumn::SignatureBefore),
            "signature_before"
        );
        assert_eq!(column_header(DiffColumn::SignatureAfter), "signature_after");
    }

    #[test]
    fn test_get_column_value_for_diff_specific_columns() {
        let change = DiffDisplayChange {
            name: "test_func".to_string(),
            qualified_name: "mod::test_func".to_string(),
            kind: "function".to_string(),
            change_type: "signature_changed".to_string(),
            base_location: Some(DiffLocation {
                file_path: "test.rs".to_string(),
                start_line: 10,
                end_line: 15,
            }),
            target_location: Some(DiffLocation {
                file_path: "test.rs".to_string(),
                start_line: 10,
                end_line: 17,
            }),
            signature_before: Some("fn test_func()".to_string()),
            signature_after: Some("fn test_func(x: i32)".to_string()),
        };

        // Test standard columns
        assert_eq!(get_column_value(&change, DiffColumn::Name), "test_func");
        assert_eq!(
            get_column_value(&change, DiffColumn::QualifiedName),
            "mod::test_func"
        );
        assert_eq!(get_column_value(&change, DiffColumn::Kind), "function");
        assert_eq!(get_column_value(&change, DiffColumn::File), "test.rs");
        assert_eq!(get_column_value(&change, DiffColumn::Line), "10");

        // Test diff-specific columns
        assert_eq!(
            get_column_value(&change, DiffColumn::ChangeType),
            "signature_changed"
        );
        assert_eq!(
            get_column_value(&change, DiffColumn::SignatureBefore),
            "fn test_func()"
        );
        assert_eq!(
            get_column_value(&change, DiffColumn::SignatureAfter),
            "fn test_func(x: i32)"
        );
    }

    #[test]
    fn test_parse_diff_specific_columns() {
        // Test that parse_columns recognizes diff-specific column names
        let spec = Some("change_type,signature_before,signature_after".to_string());
        let result = parse_columns(spec.as_ref());
        assert!(result.is_ok(), "Should parse diff-specific columns");

        let cols = result.unwrap();
        assert!(cols.is_some(), "Should return Some(vec)");

        let cols = cols.unwrap();
        assert_eq!(cols.len(), 3, "Should have 3 columns");
        assert_eq!(cols[0], CsvColumn::ChangeType);
        assert_eq!(cols[1], CsvColumn::SignatureBefore);
        assert_eq!(cols[2], CsvColumn::SignatureAfter);
    }

    #[test]
    fn test_parse_mixed_standard_and_diff_columns() {
        // Test parsing a mix of standard and diff-specific columns
        let spec = Some("name,kind,change_type,file,signature_before".to_string());
        let result = parse_columns(spec.as_ref());
        assert!(result.is_ok(), "Should parse mixed columns");

        let cols = result.unwrap().unwrap();
        assert_eq!(cols.len(), 5, "Should have 5 columns");
        assert_eq!(cols[0], CsvColumn::Name);
        assert_eq!(cols[1], CsvColumn::Kind);
        assert_eq!(cols[2], CsvColumn::ChangeType);
        assert_eq!(cols[3], CsvColumn::File);
        assert_eq!(cols[4], CsvColumn::SignatureBefore);
    }

    large_stack_test! {
    #[test]
    fn test_diff_csv_output_with_diff_columns() {
        use crate::args::Cli;
        use crate::output::OutputStreams;
        use clap::Parser;

        // Create a test change
        let change = DiffDisplayChange {
            name: "modified_func".to_string(),
            qualified_name: "test::modified_func".to_string(),
            kind: "function".to_string(),
            change_type: "modified".to_string(),
            base_location: Some(DiffLocation {
                file_path: "src/lib.rs".to_string(),
                start_line: 42,
                end_line: 45,
            }),
            target_location: Some(DiffLocation {
                file_path: "src/lib.rs".to_string(),
                start_line: 42,
                end_line: 48,
            }),
            signature_before: Some("fn modified_func(x: i32)".to_string()),
            signature_after: Some("fn modified_func(x: i32, y: i32)".to_string()),
        };

        let result = DiffDisplayResult {
            base_ref: "HEAD~1".to_string(),
            target_ref: "HEAD".to_string(),
            changes: vec![change],
            summary: DiffDisplaySummary {
                added: 0,
                removed: 0,
                modified: 1,
                renamed: 0,
                signature_changed: 0,
            },
            total: 1,
            truncated: false,
        };

        // Test CSV output with diff-specific columns
        let cli = Cli::parse_from([
            "sqry",
            "--csv",
            "--headers",
            "--columns",
            "name,change_type,signature_before,signature_after",
        ]);
        let mut streams = OutputStreams::new();

        let output_result = format_csv_output_shared(&cli, &mut streams, &result, ',');
        assert!(output_result.is_ok(), "CSV formatting should succeed");

        // Note: We can't easily verify the exact output here without capturing stdout,
        // but the fact that it doesn't error proves the columns are recognized
    }
    }
}
