//! JSON output formatter
//!
//! Uses `DisplaySymbol` directly without deprecated Symbol type.

use super::{
    ContextLines, DisplaySymbol, Formatter, FormatterMetadata, OutputStreams, PreviewConfig,
    PreviewExtractor, display_qualified_name,
};
use anyhow::Result;
use serde::Serialize;
use sqry_core::json_response::{JsonResponse, QueryMeta, Stats};
use sqry_core::relations::{CallIdentityKind, CallIdentityMetadata};
use sqry_core::workspace::NodeWithRepo;
use std::collections::HashMap;
use std::path::PathBuf;

/// JSON formatter for machine-readable output
pub struct JsonFormatter {
    preview_config: Option<PreviewConfig>,
    workspace_root: PathBuf,
}

impl JsonFormatter {
    /// Create new JSON formatter
    #[must_use]
    pub fn new() -> Self {
        Self {
            preview_config: None,
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    /// Enable preview extraction
    #[must_use]
    pub fn with_preview(mut self, config: PreviewConfig, workspace_root: PathBuf) -> Self {
        self.preview_config = Some(config);
        self.workspace_root = workspace_root;
        self
    }

    /// Format workspace query results with repository metadata.
    ///
    /// # Errors
    /// Returns an error if writing to the output streams fails.
    pub fn format_workspace(symbols: &[NodeWithRepo], streams: &mut OutputStreams) -> Result<()> {
        #[derive(Serialize)]
        struct Repo<'a> {
            id: &'a str,
            name: &'a str,
            path: String,
        }

        #[derive(Serialize)]
        struct WorkspaceResult<'a> {
            repo: Repo<'a>,
            #[serde(flatten)]
            symbol: JsonSymbol,
        }

        let payload: Vec<_> = symbols
            .iter()
            .map(|entry| {
                let info = &entry.match_info;
                let mut metadata = HashMap::new();
                if let Some(language) = &info.language {
                    metadata.insert("__raw_language".to_string(), language.clone());
                }
                if info.is_static {
                    metadata.insert("static".to_string(), "true".to_string());
                }
                let qualified_name = display_qualified_name(
                    info.qualified_name.as_deref().unwrap_or(info.name.as_str()),
                    info.kind.as_str(),
                    info.language.as_deref(),
                    info.is_static,
                );
                WorkspaceResult {
                    repo: Repo {
                        id: entry.repo_id.as_str(),
                        name: &entry.repo_name,
                        path: entry.repo_path.display().to_string(),
                    },
                    symbol: JsonSymbol {
                        name: info.name.clone(),
                        qualified_name,
                        kind: info.kind.as_str().to_string(),
                        file_path: info.file_path.display().to_string(),
                        start_line: info.start_line as usize,
                        start_column: info.start_column as usize,
                        end_line: info.end_line as usize,
                        end_column: info.end_column as usize,
                        metadata,
                        caller_identity: None,
                        callee_identity: None,
                        context: None,
                    },
                }
            })
            .collect();

        let json = serde_json::to_string_pretty(&payload)?;
        streams.write_result(&json)?;
        Ok(())
    }
}

impl Default for JsonFormatter {
    fn default() -> Self {
        Self::new()
    }
}

impl Formatter for JsonFormatter {
    fn format(
        &self,
        symbols: &[DisplaySymbol],
        metadata: Option<&FormatterMetadata>,
        streams: &mut super::OutputStreams,
    ) -> Result<()> {
        // Convert symbols to JSON-friendly format
        let mut preview_extractor = self
            .preview_config
            .as_ref()
            .map(|config| PreviewExtractor::new(config.clone(), self.workspace_root.clone()));

        let mut results: Vec<JsonSymbol> = Vec::with_capacity(symbols.len());

        for display in symbols {
            let mut json_symbol = JsonSymbol::from(display);
            if let Some(ref mut extractor) = preview_extractor {
                let ctx = extractor.extract(&display.file_path, display.start_line)?;
                json_symbol.context = Some(JsonContext::from_context_lines(&ctx));
            }
            results.push(json_symbol);
        }

        // Build response based on whether we have metadata
        let json = if let Some(meta) = metadata {
            // Structured response with metadata
            let query_meta = QueryMeta::new(meta.pattern.clone(), meta.execution_time)
                .with_filters(meta.filters.clone());

            let mut stats = Stats::new(meta.total_matches, results.len());
            if let Some(age) = meta.index_age_seconds {
                stats = stats.with_index_age(age);
            }
            // Add scope info if using ancestor index
            if let Some(is_ancestor) = meta.used_ancestor_index {
                stats = stats.with_scope_info(is_ancestor, meta.filtered_to.clone());
            }

            if let Some(revision) = &meta.revision {
                serde_json::to_string_pretty(&serde_json::json!({
                    "query": query_meta,
                    "stats": stats,
                    "revision": revision,
                    "results": results,
                }))?
            } else {
                let response = JsonResponse::new(query_meta, stats, results);
                serde_json::to_string_pretty(&response)?
            }
        } else {
            // Legacy: plain array (for backward compatibility in non-search contexts)
            serde_json::to_string_pretty(&results)?
        };

        streams.write_result(&json)?;
        Ok(())
    }
}

/// JSON-serializable symbol representation
#[derive(Debug, Serialize)]
pub struct JsonSymbol {
    name: String,
    qualified_name: String,
    kind: String,
    file_path: String,
    start_line: usize,
    start_column: usize,
    end_line: usize,
    end_column: usize,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    metadata: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    caller_identity: Option<JsonCallerIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    callee_identity: Option<JsonCallerIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<JsonContext>,
}

#[derive(Debug, Serialize)]
struct JsonCallerIdentity {
    qualified: String,
    simple: String,
    namespace: Vec<String>,
    method_kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    receiver: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct JsonContext {
    pub before: Vec<String>,
    pub line: String,
    pub after: Vec<String>,
    pub line_numbers: JsonLineNumbers,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct JsonLineNumbers {
    pub before: Vec<usize>,
    pub matched: usize,
    pub after: Vec<usize>,
}

impl JsonContext {
    fn from_context_lines(ctx: &ContextLines) -> Self {
        if ctx.is_error() {
            return Self {
                before: Vec::new(),
                line: String::new(),
                after: Vec::new(),
                line_numbers: JsonLineNumbers::empty(),
                error: ctx.error_message().map(ToOwned::to_owned),
            };
        }

        Self {
            before: ctx.before.iter().map(|l| l.content.clone()).collect(),
            line: ctx.matched.content.clone(),
            after: ctx.after.iter().map(|l| l.content.clone()).collect(),
            line_numbers: JsonLineNumbers::from_context(ctx),
            error: None,
        }
    }
}

impl JsonLineNumbers {
    fn from_context(ctx: &ContextLines) -> Self {
        Self {
            before: ctx.before.iter().map(|l| l.line_number).collect(),
            matched: ctx.matched.line_number,
            after: ctx.after.iter().map(|l| l.line_number).collect(),
        }
    }

    fn empty() -> Self {
        Self {
            before: Vec::new(),
            matched: 0,
            after: Vec::new(),
        }
    }
}

impl From<&DisplaySymbol> for JsonSymbol {
    fn from(display: &DisplaySymbol) -> Self {
        let language = display
            .metadata
            .get("__raw_language")
            .map(std::string::String::as_str)
            .filter(|language| *language != "unknown");
        let is_static = display
            .metadata
            .get("static")
            .is_some_and(|value| value == "true");

        Self {
            name: display.name.clone(),
            qualified_name: display_qualified_name(
                &display.qualified_name,
                &display.kind,
                language,
                is_static,
            ),
            kind: display.kind.clone(),
            file_path: display.file_path.display().to_string(),
            start_line: display.start_line,
            start_column: display.start_column,
            end_line: display.end_line,
            end_column: display.end_column,
            metadata: display.metadata.clone(),
            caller_identity: display
                .caller_identity
                .as_ref()
                .map(JsonCallerIdentity::from),
            callee_identity: display
                .callee_identity
                .as_ref()
                .map(JsonCallerIdentity::from),
            context: None,
        }
    }
}

impl From<&CallIdentityMetadata> for JsonCallerIdentity {
    fn from(identity: &CallIdentityMetadata) -> Self {
        Self {
            qualified: identity.qualified.clone(),
            simple: identity.simple.clone(),
            namespace: identity.namespace.clone(),
            method_kind: match identity.method_kind {
                CallIdentityKind::Instance => "instance",
                CallIdentityKind::Singleton => "singleton",
                CallIdentityKind::SingletonClass => "singleton_class",
            },
            receiver: identity.receiver.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::TestOutputStreams;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn make_display_symbol(name: &str, kind: &str, path: PathBuf, line: usize) -> DisplaySymbol {
        DisplaySymbol {
            name: name.to_string(),
            qualified_name: name.to_string(),
            kind: kind.to_string(),
            file_path: path,
            start_line: line,
            start_column: 1,
            end_line: line,
            end_column: 5,
            metadata: HashMap::new(),
            caller_identity: None,
            callee_identity: None,
        }
    }

    #[test]
    fn test_json_symbol_from_display_symbol() {
        let display = make_display_symbol(
            "test_function",
            "function",
            PathBuf::from("src/test.rs"),
            10,
        );

        let json_symbol = JsonSymbol::from(&display);
        assert_eq!(json_symbol.name, "test_function");
        assert_eq!(json_symbol.kind, "function");
        assert_eq!(json_symbol.file_path, "src/test.rs");
        assert_eq!(json_symbol.start_line, 10);
    }

    #[test]
    fn test_json_formatter_empty() {
        use crate::output::OutputStreams;

        let formatter = JsonFormatter::new();
        let symbols: Vec<DisplaySymbol> = Vec::new();
        let mut streams = OutputStreams::new();

        let result = formatter.format(&symbols, None, &mut streams);
        assert!(result.is_ok());
    }

    #[test]
    fn test_json_formatter_with_preview() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sample.rs");
        fs::write(&path, "fn sample() {}\n").unwrap();

        let sym = make_display_symbol("sample", "function", path, 1);

        let formatter =
            JsonFormatter::new().with_preview(PreviewConfig::new(1), tmp.path().to_path_buf());
        let (test, mut streams) = TestOutputStreams::new();

        formatter.format(&[sym], None, &mut streams).unwrap();

        let out = test.stdout_string();
        assert!(out.contains("\"context\""), "context missing: {out}");
        assert!(out.contains("fn sample()"), "preview line missing: {out}");
    }
}
