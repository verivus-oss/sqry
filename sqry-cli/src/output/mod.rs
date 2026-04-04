//! Output formatting for CLI results
//!
//! Uses native graph types directly without intermediate Symbol conversion.

mod csv;
mod json;
pub mod pager;
mod preview;
mod stream;
mod text;
mod theme;

pub use csv::{CsvColumn, CsvFormatter, parse_columns};
pub use json::{JsonFormatter, JsonSymbol};
pub use pager::PagerConfig;
pub use preview::PreviewConfig;
pub use stream::OutputStreams;
pub use text::TextFormatter;
pub use theme::{Palette, ThemeName};

// Re-export for testing and downstream consumers
#[allow(unused_imports)]
pub use preview::{ContextLines, GroupedContext, MatchLocation, NumberedLine, PreviewExtractor};

#[cfg(test)]
pub use stream::TestOutputStreams;

use anyhow::Result;
use sqry_core::graph::Language;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::resolution::display_graph_qualified_name;
use sqry_core::json_response::Filters;
use sqry_core::query::results::QueryMatch;
use sqry_core::relations::{CallIdentityBuilder, CallIdentityKind, CallIdentityMetadata};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Metadata about the query for structured output
#[derive(Debug, Clone)]
pub struct FormatterMetadata {
    /// The search pattern or query expression
    pub pattern: Option<String>,
    /// Total number of matches before limiting
    pub total_matches: usize,
    /// Query execution time
    pub execution_time: Duration,
    /// Active filters
    pub filters: Filters,
    /// Index age in seconds (if applicable)
    pub index_age_seconds: Option<u64>,
    /// Whether the index was found in an ancestor directory
    pub used_ancestor_index: Option<bool>,
    /// Scope filter applied (e.g., "src/**" or "main.rs")
    pub filtered_to: Option<String>,
}

/// Trait for formatting search results
pub trait Formatter {
    /// Format and output symbols using provided output streams
    ///
    /// Results should go to `streams.write_result()` (stdout)
    /// Diagnostics should go to `streams.write_diagnostic()` (stderr)
    ///
    /// Metadata is optional - used for structured JSON output with query stats
    ///
    /// # Errors
    /// Returns an error if writing to the output streams fails.
    fn format(
        &self,
        symbols: &[DisplaySymbol],
        metadata: Option<&FormatterMetadata>,
        streams: &mut OutputStreams,
    ) -> Result<()>;
}

fn parse_ruby_instance_identity(qualified: &str) -> (CallIdentityKind, String, Vec<String>) {
    let parts: Vec<&str> = qualified.rsplitn(2, '#').collect();
    let simple = parts.first().copied().unwrap_or("").to_string();
    let ns_str = parts.get(1).unwrap_or(&"");
    let namespace: Vec<String> = ns_str
        .split("::")
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect();
    (CallIdentityKind::Instance, simple, namespace)
}

fn parse_namespace_identity(
    qualified: &str,
    kind: &str,
) -> (CallIdentityKind, String, Vec<String>) {
    let parts: Vec<&str> = qualified.split("::").collect();
    if let Some(last) = parts.last() {
        if last.contains('.') {
            let method_parts: Vec<&str> = last.rsplitn(2, '.').collect();
            let simple = method_parts.first().copied().unwrap_or("").to_string();
            let mut namespace: Vec<String> = parts[..parts.len() - 1]
                .iter()
                .map(|segment| (*segment).to_string())
                .collect();
            if let Some(class_name) = method_parts.get(1) {
                namespace.push((*class_name).to_string());
            }
            (CallIdentityKind::Singleton, simple, namespace)
        } else {
            let simple = (*last).to_string();
            let namespace: Vec<String> = parts[..parts.len() - 1]
                .iter()
                .map(|segment| (*segment).to_string())
                .collect();
            let method_kind = if kind == "method" {
                CallIdentityKind::Instance
            } else {
                CallIdentityKind::Singleton
            };
            (method_kind, simple, namespace)
        }
    } else {
        (CallIdentityKind::Instance, qualified.to_string(), vec![])
    }
}

fn parse_dot_separated_identity(qualified: &str) -> (CallIdentityKind, String, Vec<String>) {
    let parts: Vec<&str> = qualified.rsplitn(2, '.').collect();
    let simple = parts.first().copied().unwrap_or("").to_string();
    let ns_str = parts.get(1).unwrap_or(&"");
    let namespace: Vec<String> = ns_str
        .split('.')
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect();
    (CallIdentityKind::Singleton, simple, namespace)
}

pub(crate) fn call_identity_from_qualified_name(
    qualified: &str,
    kind: &str,
    language: Option<&str>,
    is_static: bool,
) -> Option<CallIdentityMetadata> {
    if qualified.is_empty() {
        return None;
    }

    let (method_kind, simple, namespace, display_qualified) = if qualified.contains('#') {
        let (method_kind, simple, namespace) = parse_ruby_instance_identity(qualified);
        (method_kind, simple, namespace, qualified.to_string())
    } else if language == Some("ruby") && kind == "method" && qualified.contains("::") {
        let (method_kind, simple, namespace) = parse_namespace_identity(qualified, kind);
        let ruby_method_kind = if is_static {
            CallIdentityKind::Singleton
        } else {
            method_kind
        };
        let display_qualified = CallIdentityBuilder::new(simple.clone(), ruby_method_kind)
            .with_namespace(namespace.clone())
            .build()
            .qualified;
        (ruby_method_kind, simple, namespace, display_qualified)
    } else if qualified.contains("::") {
        let (method_kind, simple, namespace) = parse_namespace_identity(qualified, kind);
        (method_kind, simple, namespace, qualified.to_string())
    } else if qualified.contains('.') {
        let (method_kind, simple, namespace) = parse_dot_separated_identity(qualified);
        (method_kind, simple, namespace, qualified.to_string())
    } else {
        (
            CallIdentityKind::Instance,
            qualified.to_string(),
            vec![],
            qualified.to_string(),
        )
    };

    Some(CallIdentityMetadata {
        qualified: display_qualified,
        simple,
        namespace,
        method_kind,
        receiver: None,
    })
}

pub(crate) fn display_qualified_name(
    qualified: &str,
    kind: &str,
    language: Option<&str>,
    is_static: bool,
) -> String {
    if let (Some(language), Some(kind)) =
        (language.and_then(Language::from_id), NodeKind::parse(kind))
    {
        return display_graph_qualified_name(language, qualified, kind, is_static);
    }

    call_identity_from_qualified_name(qualified, kind, language, is_static)
        .map_or_else(|| qualified.to_string(), |identity| identity.qualified)
}

fn build_preview_config(cli: &crate::args::Cli) -> Option<PreviewConfig> {
    cli.preview.map(|lines| {
        if lines == 0 {
            PreviewConfig::no_context()
        } else {
            PreviewConfig::new(lines)
        }
    })
}

fn resolve_workspace_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn build_csv_formatter(
    cli: &crate::args::Cli,
    preview_config: Option<&PreviewConfig>,
    workspace_root: &Path,
    tsv: bool,
) -> Box<dyn Formatter> {
    let columns =
        csv::parse_columns(cli.columns.as_ref()).expect("columns validated by Cli::validate");
    let mut formatter = if tsv {
        CsvFormatter::tsv(cli.headers, columns)
    } else {
        CsvFormatter::csv(cli.headers, columns)
    };
    formatter = formatter
        .raw_mode(cli.raw_csv)
        .with_workspace_root(workspace_root.to_path_buf());
    if let Some(config) = preview_config {
        formatter = formatter.with_preview(config.clone());
    }
    Box::new(formatter)
}

fn build_json_formatter(
    preview_config: Option<&PreviewConfig>,
    workspace_root: &Path,
) -> Box<dyn Formatter> {
    let mut formatter = JsonFormatter::new();
    if let Some(config) = preview_config {
        formatter = formatter.with_preview(config.clone(), workspace_root.to_path_buf());
    }
    Box::new(formatter)
}

fn build_text_formatter(
    preview_config: Option<&PreviewConfig>,
    workspace_root: &Path,
    use_color: bool,
    mode: NameDisplayMode,
    theme: ThemeName,
) -> Box<dyn Formatter> {
    let mut formatter = TextFormatter::new(use_color, mode, theme);
    if let Some(config) = preview_config {
        formatter = formatter.with_preview(config.clone(), workspace_root.to_path_buf());
    }
    Box::new(formatter)
}

/// Create a formatter based on CLI flags
///
/// # Panics
/// Panics if column validation was skipped and `--columns` contains invalid values.
#[must_use]
pub fn create_formatter(cli: &crate::args::Cli) -> Box<dyn Formatter> {
    // Determine preview config if requested
    let preview_config = build_preview_config(cli);

    // Get workspace root for preview path validation
    let workspace_root = resolve_workspace_root();

    let theme = resolve_theme(cli);
    let use_color = !cli.no_color && theme != ThemeName::None && std::env::var("NO_COLOR").is_err();

    let mode = if cli.qualified_names {
        NameDisplayMode::Qualified
    } else {
        NameDisplayMode::Simple
    };

    match (cli.csv, cli.tsv, cli.json) {
        (true, _, _) => build_csv_formatter(cli, preview_config.as_ref(), &workspace_root, false),
        (_, true, _) => build_csv_formatter(cli, preview_config.as_ref(), &workspace_root, true),
        (_, _, true) => build_json_formatter(preview_config.as_ref(), &workspace_root),
        _ => build_text_formatter(
            preview_config.as_ref(),
            &workspace_root,
            use_color,
            mode,
            theme,
        ),
    }
}

pub(crate) fn resolve_theme(cli: &crate::args::Cli) -> ThemeName {
    // CLI flag takes precedence unless it is the default and env is set.
    if cli.theme != ThemeName::Default {
        return cli.theme;
    }

    if let Ok(env_theme) = std::env::var("SQRY_THEME") {
        match env_theme.to_lowercase().as_str() {
            "default" => ThemeName::Default,
            "dark" => ThemeName::Dark,
            "light" => ThemeName::Light,
            "none" => ThemeName::None,
            _ => {
                eprintln!(
                    "Warning: unrecognized SQRY_THEME value '{env_theme}', using default theme"
                );
                ThemeName::Default
            }
        }
    } else {
        ThemeName::Default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::Cli;
    use crate::large_stack_test;
    use clap::Parser;
    use serial_test::serial;

    large_stack_test! {
    #[test]
    #[serial]
    fn test_resolve_theme_env_fallback() {
        // CLI default, env sets dark
        unsafe {
            std::env::set_var("SQRY_THEME", "dark");
        }
        let cli = Cli::parse_from(["sqry"]);
        assert_eq!(resolve_theme(&cli), ThemeName::Dark);
        unsafe {
            std::env::remove_var("SQRY_THEME");
        }
    }
    }

    large_stack_test! {
    #[test]
    #[serial]
    fn test_resolve_theme_cli_overrides_env() {
        unsafe {
            std::env::set_var("SQRY_THEME", "dark");
        }
        let cli = Cli::parse_from(["sqry", "--theme", "light"]);
        assert_eq!(resolve_theme(&cli), ThemeName::Light);
        unsafe {
            std::env::remove_var("SQRY_THEME");
        }
    }
    }
}

/// Name display preference for human-readable output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameDisplayMode {
    Simple,
    Qualified,
}

/// Display data for a symbol, constructed directly from graph queries.
///
/// This replaces the legacy `Symbol` wrapper, holding all display data
/// as owned values without depending on the deprecated Symbol type.
#[derive(Clone, Debug)]
pub struct DisplaySymbol {
    /// Symbol name
    pub name: String,
    /// Fully qualified name
    pub qualified_name: String,
    /// Symbol kind as string (e.g., "function", "class")
    pub kind: String,
    /// Full file path
    pub file_path: PathBuf,
    /// Start line (1-based)
    pub start_line: usize,
    /// Start column (0-based)
    pub start_column: usize,
    /// End line (1-based)
    pub end_line: usize,
    /// End column (0-based)
    pub end_column: usize,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
    /// Caller identity for relation queries
    pub caller_identity: Option<CallIdentityMetadata>,
    /// Callee identity for relation queries
    pub callee_identity: Option<CallIdentityMetadata>,
}

impl DisplaySymbol {
    /// Create a `DisplaySymbol` from a `QueryMatch`.
    #[must_use]
    pub fn from_query_match(m: &QueryMatch<'_>) -> Self {
        let name = m.name().map(|s| s.to_string()).unwrap_or_default();
        let language = m.language().map_or_else(
            || "unknown".to_string(),
            |l| l.to_string().to_ascii_lowercase(),
        );
        let qualified_name = m
            .qualified_name()
            .map_or_else(|| name.clone(), |s| s.to_string());
        let file_path = m.file_path().map(|p| p.to_path_buf()).unwrap_or_default();
        let kind = node_kind_to_string(m.kind()).to_string();

        let mut metadata = HashMap::new();
        metadata.insert(
            "__raw_file_path".to_string(),
            file_path.display().to_string(),
        );
        metadata.insert("__raw_language".to_string(), language);

        // Add visibility metadata if present
        if let Some(vis) = m.visibility() {
            metadata.insert("visibility".to_string(), vis.to_string());
        }

        // Add async/static flags if true (avoid cluttering output with false values)
        if m.is_async() {
            metadata.insert("async".to_string(), "true".to_string());
        }
        if m.is_static() {
            metadata.insert("static".to_string(), "true".to_string());
        }

        Self {
            name,
            qualified_name,
            kind,
            file_path,
            start_line: m.start_line() as usize,
            start_column: m.start_column() as usize,
            end_line: m.end_line() as usize,
            end_column: m.end_column() as usize,
            metadata,
            caller_identity: None,
            callee_identity: None,
        }
    }

    /// Create with caller identity for relation queries.
    #[must_use]
    pub fn with_caller_identity(mut self, identity: Option<CallIdentityMetadata>) -> Self {
        self.caller_identity = identity;
        self
    }

    /// Create with callee identity for relation queries.
    #[must_use]
    pub fn with_callee_identity(mut self, identity: Option<CallIdentityMetadata>) -> Self {
        self.callee_identity = identity;
        self
    }

    /// Get the symbol kind as a string.
    #[must_use]
    pub fn kind_string(&self) -> &str {
        &self.kind
    }
}

/// Convert `NodeKind` to lowercase string for display.
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
        NodeKind::TypeParameter => "type_parameter",
        NodeKind::Annotation => "annotation",
        NodeKind::AnnotationValue => "annotation_value",
        NodeKind::LambdaTarget => "lambda_target",
        NodeKind::JavaModule => "java_module",
        NodeKind::EnumConstant => "enum_constant",
        NodeKind::Other => "other",
    }
}
