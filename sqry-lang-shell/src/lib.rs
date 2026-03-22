//! Shell language plugin for sqry.
//!
//! Provides AST parsing, scope extraction, and graph building for POSIX shell
//! and bash scripts.

pub mod relations;

pub use relations::ShellGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::graph::unified::edge::EdgeKind;
use sqry_core::metadata::keys as metadata_keys;
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin, PluginResult,
    error::{ParseError, ScopeError},
};
use sqry_core::query::results::QueryMatch;
use sqry_core::query::types::{FieldDescriptor, FieldType, Operator, Value};
use std::fs;
use std::path::Path;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree};

const LANGUAGE_ID: &str = "shell";
const LANGUAGE_NAME: &str = "Shell";
const TREE_SITTER_VERSION: &str = "0.23";

/// Shell language plugin implementation.
pub struct ShellPlugin {
    graph_builder: ShellGraphBuilder,
}

impl ShellPlugin {
    /// Creates a new Shell plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: ShellGraphBuilder::default(),
        }
    }

    fn detect_shell_variant(content: &[u8]) -> &'static str {
        if let Some(first_line) = content.split(|&b| b == b'\n').next()
            && first_line.starts_with(b"#!")
        {
            let lowered = String::from_utf8_lossy(first_line).to_lowercase();
            if lowered.contains("bash") {
                return "bash";
            }
            if lowered.contains("zsh") {
                return "zsh";
            }
            if lowered.contains("sh") {
                return "sh";
            }
        }
        "sh"
    }

    fn is_exported(entry: &QueryMatch<'_>) -> bool {
        let graph = entry.graph();
        let edges = graph.edges().edges_to(entry.id);
        edges
            .iter()
            .any(|edge| matches!(edge.kind, EdgeKind::Exports { .. }))
    }

    fn detect_shell_variant_for_entry(entry: &QueryMatch<'_>) -> Option<&'static str> {
        let path = entry.file_path()?;
        let content = fs::read(path).ok()?;
        Some(Self::detect_shell_variant(&content))
    }
}

impl Default for ShellPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for ShellPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: LANGUAGE_ID,
            name: LANGUAGE_NAME,
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Shell script language support for sqry",
            tree_sitter_version: TREE_SITTER_VERSION,
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["sh", "bash", "bashrc", "bash_profile", "profile", "env"]
    }

    fn language(&self) -> Language {
        tree_sitter_bash::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language())
            .map_err(|err| ParseError::LanguageSetFailed(err.to_string()))?;

        parser
            .parse(content, None)
            .ok_or(ParseError::TreeSitterFailed)
    }

    fn extract_scopes(
        &self,
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        extract_shell_scopes(tree, content, file_path)
    }

    fn fields(&self) -> &'static [FieldDescriptor] {
        &[
            FieldDescriptor {
                name: metadata_keys::SHELL_VARIANT,
                field_type: FieldType::String,
                operators: &[Operator::Equal],
                indexed: false,
                doc: "Shell variant (sh, bash, zsh) detected from shebang",
            },
            FieldDescriptor {
                name: metadata_keys::IS_EXPORTED,
                field_type: FieldType::Bool,
                operators: &[Operator::Equal],
                indexed: false,
                doc: "Whether the symbol is exported (export keyword)",
            },
        ]
    }

    fn evaluate_field(
        &self,
        entry: &QueryMatch<'_>,
        field: &str,
        value: &Value,
    ) -> PluginResult<bool> {
        match field {
            metadata_keys::SHELL_VARIANT => {
                let actual = Self::detect_shell_variant_for_entry(entry).unwrap_or("sh");
                match value {
                    Value::String(expected) => Ok(actual == expected),
                    _ => Ok(false),
                }
            }
            metadata_keys::IS_EXPORTED => {
                let is_exported = Self::is_exported(entry);
                match value {
                    Value::Boolean(expected) => Ok(is_exported == *expected),
                    _ => Ok(false),
                }
            }
            _ => Ok(false),
        }
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

fn extract_shell_scopes(
    tree: &Tree,
    content: &[u8],
    file_path: &Path,
) -> Result<Vec<Scope>, ScopeError> {
    let root_node = tree.root_node();
    let language = tree_sitter_bash::LANGUAGE.into();

    let scope_query = r"
; Function definitions (both POSIX and Bash style)
(function_definition
  name: (word) @function.name
) @function.type
";

    let query = Query::new(&language, scope_query)
        .map_err(|e| ScopeError::QueryCompilationFailed(e.to_string()))?;

    let mut scopes = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut query_matches = cursor.matches(&query, root_node, content);

    while let Some(m) = query_matches.next() {
        let mut scope_type = None;
        let mut scope_name = None;
        let mut scope_start = None;
        let mut scope_end = None;

        for capture in m.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            let node = capture.node;

            let capture_ext = std::path::Path::new(capture_name)
                .extension()
                .and_then(|ext| ext.to_str());

            if capture_ext.is_some_and(|ext| ext.eq_ignore_ascii_case("type")) {
                scope_type = Some("function".to_string());
                scope_start = Some(node.start_position());
                scope_end = Some(node.end_position());
            } else if capture_ext.is_some_and(|ext| ext.eq_ignore_ascii_case("name")) {
                scope_name = node
                    .utf8_text(content)
                    .ok()
                    .map(std::string::ToString::to_string);
            }
        }

        if let (Some(stype), Some(sname), Some(start), Some(end)) =
            (scope_type, scope_name, scope_start, scope_end)
        {
            let scope = Scope {
                id: ScopeId::new(0),
                scope_type: stype,
                name: sname,
                file_path: file_path.to_path_buf(),
                start_line: start.row + 1,
                start_column: start.column,
                end_line: end.row + 1,
                end_column: end.column,
                parent_id: None,
            };
            scopes.push(scope);
        }
    }

    scopes.sort_by_key(|s| (s.start_line, s.start_column));
    link_nested_scopes(&mut scopes);
    Ok(scopes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::graph::unified::build::staging::StagingGraph;
    use sqry_core::graph::unified::build::test_helpers::{
        assert_has_export_edge, collect_export_edges,
    };
    use std::fs;
    use std::path::PathBuf;

    fn read_fixture(name: &str) -> (Vec<u8>, PathBuf) {
        let path = PathBuf::from("tests/fixtures").join(name);
        let content = fs::read(&path).expect("failed to read fixture");
        (content, path)
    }

    #[test]
    fn graph_builder_exports_functions_and_variables() {
        let (content, path) = read_fixture("basic.sh");
        let plugin = ShellPlugin::default();
        let tree = plugin.parse_ast(&content).expect("parse failed");
        let mut staging = StagingGraph::new();
        let builder = plugin.graph_builder().expect("graph builder");

        builder
            .build_graph(&tree, &content, &path, &mut staging)
            .expect("build graph");

        assert_has_export_edge(&staging, "basic::module", "foo");
        assert_has_export_edge(&staging, "basic::module", "bar");
        assert_has_export_edge(&staging, "basic::module", "DATA_PATH");
    }

    #[test]
    fn graph_builder_uses_direct_exports() {
        let (content, path) = read_fixture("basic.sh");
        let plugin = ShellPlugin::default();
        let tree = plugin.parse_ast(&content).expect("parse failed");
        let mut staging = StagingGraph::new();
        let builder = plugin.graph_builder().expect("graph builder");

        builder
            .build_graph(&tree, &content, &path, &mut staging)
            .expect("build graph");

        let exports = collect_export_edges(&staging);
        assert!(!exports.is_empty(), "expected export edges");
    }

    #[test]
    fn extract_scopes_reports_functions() {
        let (content, path) = read_fixture("basic.sh");
        let plugin = ShellPlugin::default();
        let tree = plugin.parse_ast(&content).expect("parse failed");
        let scopes = plugin
            .extract_scopes(&tree, &content, &path)
            .expect("scope extraction failed");

        let names: Vec<String> = scopes.into_iter().map(|scope| scope.name).collect();
        assert!(names.contains(&"foo".to_string()));
        assert!(names.contains(&"bar".to_string()));
    }
}
