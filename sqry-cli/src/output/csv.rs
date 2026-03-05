//! CSV/TSV output formatter for tabular export
//!
//! Provides RFC 4180 compliant CSV output with optional TSV (tab-separated) mode.
//! Includes formula injection protection for Excel/LibreOffice safety.

use super::preview::{PreviewConfig, PreviewExtractor};
use super::{DisplaySymbol, Formatter, FormatterMetadata, OutputStreams};
use anyhow::Result;
use std::path::PathBuf;

/// Characters that trigger formula execution in spreadsheets
const FORMULA_CHARS: &[char] = &['=', '+', '-', '@', '\t', '\r'];

/// Default columns to include in CSV output
const DEFAULT_COLUMNS: &[CsvColumn] = &[
    CsvColumn::Name,
    CsvColumn::QualifiedName,
    CsvColumn::Kind,
    CsvColumn::File,
    CsvColumn::Line,
    CsvColumn::Column,
    CsvColumn::EndLine,
    CsvColumn::EndColumn,
    CsvColumn::Language,
];

/// Available columns for CSV/TSV output
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsvColumn {
    Name,
    QualifiedName,
    Kind,
    File,
    Line,
    Column,
    EndLine,
    EndColumn,
    Language,
    Preview,
    // Diff-specific columns
    ChangeType,
    SignatureBefore,
    SignatureAfter,
}

impl CsvColumn {
    /// Parse column name from string
    pub fn parse_column(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "name" => Some(Self::Name),
            "qualified_name" | "qualifiedname" => Some(Self::QualifiedName),
            "kind" => Some(Self::Kind),
            "file" => Some(Self::File),
            "line" => Some(Self::Line),
            "column" | "col" => Some(Self::Column),
            "end_line" | "endline" => Some(Self::EndLine),
            "end_column" | "endcolumn" => Some(Self::EndColumn),
            "language" | "lang" => Some(Self::Language),
            "preview" | "context" => Some(Self::Preview),
            "change_type" | "changetype" => Some(Self::ChangeType),
            "signature_before" | "signaturebefore" => Some(Self::SignatureBefore),
            "signature_after" | "signatureafter" => Some(Self::SignatureAfter),
            _ => None,
        }
    }

    /// Get header name for this column
    pub fn header(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::QualifiedName => "qualified_name",
            Self::Kind => "kind",
            Self::File => "file",
            Self::Line => "line",
            Self::Column => "column",
            Self::EndLine => "end_line",
            Self::EndColumn => "end_column",
            Self::Language => "language",
            Self::Preview => "preview",
            Self::ChangeType => "change_type",
            Self::SignatureBefore => "signature_before",
            Self::SignatureAfter => "signature_after",
        }
    }
}

/// Configuration for CSV/TSV output
#[derive(Debug, Clone)]
pub struct CsvConfig {
    /// Include header row
    pub headers: bool,
    /// Columns to include (None = all default columns)
    pub columns: Option<Vec<CsvColumn>>,
    /// Field delimiter (comma for CSV, tab for TSV)
    pub delimiter: char,
    /// Disable formula injection protection (raw mode)
    pub raw_mode: bool,
    /// Preview configuration (None = no preview)
    pub preview_config: Option<PreviewConfig>,
}

impl Default for CsvConfig {
    fn default() -> Self {
        Self {
            headers: false,
            columns: None,
            delimiter: ',',
            raw_mode: false,
            preview_config: None,
        }
    }
}

/// CSV/TSV formatter for tabular output
pub struct CsvFormatter {
    config: CsvConfig,
    /// Workspace root for preview path validation
    workspace_root: Option<PathBuf>,
}

impl CsvFormatter {
    /// Create CSV formatter (comma-delimited)
    #[must_use]
    pub fn csv(headers: bool, columns: Option<Vec<CsvColumn>>) -> Self {
        Self {
            config: CsvConfig {
                headers,
                columns,
                delimiter: ',',
                ..Default::default()
            },
            workspace_root: None,
        }
    }

    /// Create TSV formatter (tab-delimited)
    #[must_use]
    pub fn tsv(headers: bool, columns: Option<Vec<CsvColumn>>) -> Self {
        Self {
            config: CsvConfig {
                headers,
                columns,
                delimiter: '\t',
                ..Default::default()
            },
            workspace_root: None,
        }
    }

    /// Enable preview column
    #[must_use]
    pub fn with_preview(mut self, config: PreviewConfig) -> Self {
        self.config.preview_config = Some(config);
        self
    }

    /// Set raw mode (disable formula injection protection)
    #[must_use]
    pub fn raw_mode(mut self, raw: bool) -> Self {
        self.config.raw_mode = raw;
        self
    }

    /// Set workspace root for preview path validation
    #[must_use]
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = Some(root);
        self
    }

    /// Get the columns to output
    fn get_columns(&self) -> Vec<CsvColumn> {
        let mut cols = self
            .config
            .columns
            .clone()
            .unwrap_or_else(|| DEFAULT_COLUMNS.to_vec());

        // Add preview column if preview is enabled and not already present
        if self.config.preview_config.is_some() && !cols.contains(&CsvColumn::Preview) {
            cols.push(CsvColumn::Preview);
        }

        cols
    }

    /// Escape a field value for CSV output (RFC 4180)
    fn escape_csv_field(&self, value: &str) -> String {
        let needs_quoting = value.contains(self.config.delimiter)
            || value.contains('"')
            || value.contains('\n')
            || value.contains('\r');

        let escaped = if needs_quoting {
            format!("\"{}\"", value.replace('"', "\"\""))
        } else {
            value.to_string()
        };

        // Apply formula injection protection unless raw mode
        if self.config.raw_mode {
            escaped
        } else {
            Self::apply_formula_protection(&escaped)
        }
    }

    /// Escape a field value for TSV output
    fn escape_tsv_field(&self, value: &str) -> String {
        // Replace tabs and newlines with spaces using char-by-char approach
        let escaped: String = value
            .chars()
            .filter_map(|c| match c {
                '\t' | '\n' => Some(' '),
                '\r' => None,
                _ => Some(c),
            })
            .collect();

        // Apply formula injection protection unless raw mode
        if self.config.raw_mode {
            escaped
        } else {
            Self::apply_formula_protection(&escaped)
        }
    }

    /// Apply formula injection protection by prefixing dangerous values
    fn apply_formula_protection(value: &str) -> String {
        if let Some(first_char) = value.chars().next()
            && FORMULA_CHARS.contains(&first_char)
        {
            return format!("'{value}");
        }
        value.to_string()
    }

    /// Escape a field based on delimiter type
    fn escape_field(&self, value: &str) -> String {
        if self.config.delimiter == '\t' {
            self.escape_tsv_field(value)
        } else {
            self.escape_csv_field(value)
        }
    }

    /// Get field value for a symbol and column
    fn get_field_value(symbol: &DisplaySymbol, column: CsvColumn, preview: Option<&str>) -> String {
        match column {
            CsvColumn::Name => symbol.name.clone(),
            CsvColumn::QualifiedName => symbol.qualified_name.clone(),
            CsvColumn::Kind => symbol.kind.clone(),
            CsvColumn::File => symbol.file_path.display().to_string(),
            CsvColumn::Line => symbol.start_line.to_string(),
            CsvColumn::Column => symbol.start_column.to_string(),
            CsvColumn::EndLine => symbol.end_line.to_string(),
            CsvColumn::EndColumn => symbol.end_column.to_string(),
            CsvColumn::Language => symbol
                .metadata
                .get("__raw_language")
                .cloned()
                .unwrap_or_else(|| "unknown".to_string()),
            CsvColumn::Preview => preview.unwrap_or("").to_string(),
            // Diff-specific columns - not applicable to regular symbol output
            // These columns are only used through the diff command's custom CSV formatter
            CsvColumn::ChangeType | CsvColumn::SignatureBefore | CsvColumn::SignatureAfter => {
                String::new()
            }
        }
    }
}

impl Formatter for CsvFormatter {
    fn format(
        &self,
        symbols: &[DisplaySymbol],
        _metadata: Option<&FormatterMetadata>,
        streams: &mut OutputStreams,
    ) -> Result<()> {
        let columns = self.get_columns();
        let delimiter = self.config.delimiter.to_string();

        // Write header row if enabled
        if self.config.headers {
            let header_row: Vec<&str> = columns.iter().copied().map(CsvColumn::header).collect();
            streams.write_result(&header_row.join(&delimiter))?;
        }

        // Create preview extractor if needed
        let mut preview_extractor = if self.config.preview_config.is_some() {
            let workspace = self
                .workspace_root
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            Some(PreviewExtractor::new(
                self.config.preview_config.clone().unwrap(),
                workspace,
            ))
        } else {
            None
        };

        // Write data rows
        for symbol in symbols {
            // Get preview if configured
            let preview_str = if let Some(ref mut extractor) = preview_extractor {
                let ctx = extractor.extract(&symbol.file_path, symbol.start_line)?;
                Some(ctx.to_preview_string(500)) // Limit preview length in CSV
            } else {
                None
            };

            let fields: Vec<String> = columns
                .iter()
                .copied()
                .map(|col| {
                    let value = Self::get_field_value(symbol, col, preview_str.as_deref());
                    self.escape_field(&value)
                })
                .collect();

            streams.write_result(&fields.join(&delimiter))?;
        }

        Ok(())
    }
}

/// Parse column specification string into column list
///
/// # Errors
/// Returns an error message if any column name is invalid or if no valid columns are provided.
pub fn parse_columns(spec: Option<&String>) -> Result<Option<Vec<CsvColumn>>, String> {
    let Some(raw) = spec else {
        return Ok(None);
    };

    let mut columns = Vec::new();
    for name in raw.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        if let Some(col) = CsvColumn::parse_column(name) {
            if !columns.contains(&col) {
                columns.push(col);
            }
        } else {
            return Err(format!(
                "Unknown column '{name}'. Valid columns: name, qualified_name, kind, file, line, column, end_line, end_column, language, preview, change_type, signature_before, signature_after"
            ));
        }
    }

    if columns.is_empty() {
        return Err("No valid columns specified for --columns".to_string());
    }

    Ok(Some(columns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn create_test_display_symbol(name: &str, file: &str, line: usize) -> DisplaySymbol {
        let mut metadata = HashMap::new();
        metadata.insert("__raw_language".to_string(), "rust".to_string());
        metadata.insert("__raw_file_path".to_string(), file.to_string());
        DisplaySymbol {
            name: name.to_string(),
            qualified_name: format!("test::{name}"),
            kind: "function".to_string(),
            file_path: PathBuf::from(file),
            start_line: line,
            start_column: 1,
            end_line: line,
            end_column: 10,
            metadata,
            caller_identity: None,
            callee_identity: None,
        }
    }

    #[test]
    fn test_csv_escape_simple() {
        let formatter = CsvFormatter::csv(false, None).raw_mode(true);
        assert_eq!(formatter.escape_csv_field("hello"), "hello");
    }

    #[test]
    fn test_csv_escape_quotes() {
        let formatter = CsvFormatter::csv(false, None).raw_mode(true);
        assert_eq!(
            formatter.escape_csv_field("say \"hi\""),
            "\"say \"\"hi\"\"\""
        );
    }

    #[test]
    fn test_csv_escape_commas() {
        let formatter = CsvFormatter::csv(false, None).raw_mode(true);
        assert_eq!(formatter.escape_csv_field("a,b"), "\"a,b\"");
    }

    #[test]
    fn test_csv_escape_newlines() {
        let formatter = CsvFormatter::csv(false, None).raw_mode(true);
        assert_eq!(formatter.escape_csv_field("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn test_csv_escape_combined() {
        let formatter = CsvFormatter::csv(false, None).raw_mode(true);
        let result = formatter.escape_csv_field("a,\"b\"\nc");
        assert!(result.starts_with('"') && result.ends_with('"'));
    }

    #[test]
    fn test_csv_escape_empty() {
        let formatter = CsvFormatter::csv(false, None).raw_mode(true);
        assert_eq!(formatter.escape_csv_field(""), "");
    }

    #[test]
    fn test_csv_escape_unicode() {
        let formatter = CsvFormatter::csv(false, None).raw_mode(true);
        assert_eq!(formatter.escape_csv_field("日本語"), "日本語");
    }

    #[test]
    fn test_tsv_escape_tabs() {
        let formatter = CsvFormatter::tsv(false, None).raw_mode(true);
        assert_eq!(formatter.escape_tsv_field("a\tb"), "a b");
    }

    #[test]
    fn test_tsv_escape_newlines() {
        let formatter = CsvFormatter::tsv(false, None).raw_mode(true);
        assert_eq!(formatter.escape_tsv_field("a\nb"), "a b");
    }

    #[test]
    fn test_formula_injection_equals() {
        let formatter = CsvFormatter::csv(false, None);
        assert_eq!(formatter.escape_field("=SUM(1)"), "'=SUM(1)");
    }

    #[test]
    fn test_formula_injection_plus() {
        let formatter = CsvFormatter::csv(false, None);
        assert_eq!(formatter.escape_field("+1"), "'+1");
    }

    #[test]
    fn test_formula_injection_minus() {
        let formatter = CsvFormatter::csv(false, None);
        assert_eq!(formatter.escape_field("-1"), "'-1");
    }

    #[test]
    fn test_formula_injection_at() {
        let formatter = CsvFormatter::csv(false, None);
        assert_eq!(formatter.escape_field("@SUM"), "'@SUM");
    }

    #[test]
    fn test_formula_injection_raw_mode() {
        let formatter = CsvFormatter::csv(false, None).raw_mode(true);
        assert_eq!(formatter.escape_field("=SUM(1)"), "=SUM(1)");
    }

    #[test]
    fn test_normal_values_unmodified() {
        let formatter = CsvFormatter::csv(false, None);
        assert_eq!(formatter.escape_field("hello"), "hello");
        assert_eq!(formatter.escape_field("world"), "world");
    }

    #[test]
    fn test_parse_columns() {
        let spec = Some("name,file,line".to_string());
        let cols = parse_columns(spec.as_ref()).unwrap().unwrap();
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0], CsvColumn::Name);
        assert_eq!(cols[1], CsvColumn::File);
        assert_eq!(cols[2], CsvColumn::Line);
    }

    #[test]
    fn test_parse_columns_with_aliases() {
        let spec = Some("name,lang,col".to_string());
        let cols = parse_columns(spec.as_ref()).unwrap().unwrap();
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0], CsvColumn::Name);
        assert_eq!(cols[1], CsvColumn::Language);
        assert_eq!(cols[2], CsvColumn::Column);
    }

    #[test]
    fn test_parse_columns_invalid() {
        let spec = Some("name,unknown".to_string());
        let err = parse_columns(spec.as_ref()).unwrap_err();
        assert!(
            err.contains("Unknown column"),
            "Unexpected error message: {err}"
        );
    }

    #[test]
    fn test_csv_header_default() {
        let formatter = CsvFormatter::csv(true, None);
        let cols = formatter.get_columns();
        assert_eq!(cols.len(), 9); // All default columns
    }

    #[test]
    fn test_csv_header_subset() {
        let columns = Some(vec![CsvColumn::Name, CsvColumn::File, CsvColumn::Line]);
        let formatter = CsvFormatter::csv(true, columns);
        let cols = formatter.get_columns();
        assert_eq!(cols.len(), 3);
    }

    #[test]
    fn test_csv_formatter_output() {
        use crate::output::TestOutputStreams;

        let display = create_test_display_symbol("test_func", "src/lib.rs", 42);
        let formatter = CsvFormatter::csv(true, Some(vec![CsvColumn::Name, CsvColumn::Line]));

        let (test, mut streams) = TestOutputStreams::new();
        formatter.format(&[display], None, &mut streams).unwrap();

        let output = test.stdout_string();
        assert!(
            output.contains("name,line"),
            "Header should contain name,line: {}",
            output
        );
        assert!(
            output.contains("test_func,42"),
            "Output should contain test_func,42: {}",
            output
        );
    }

    #[test]
    fn test_tsv_formatter_output() {
        use crate::output::TestOutputStreams;

        let display = create_test_display_symbol("test_func", "src/lib.rs", 42);
        let formatter = CsvFormatter::tsv(false, Some(vec![CsvColumn::Name, CsvColumn::Line]));

        let (test, mut streams) = TestOutputStreams::new();
        formatter.format(&[display], None, &mut streams).unwrap();

        let output = test.stdout_string();
        assert!(
            output.contains("test_func\t42"),
            "Output should contain test_func<tab>42: {}",
            output
        );
    }
}
