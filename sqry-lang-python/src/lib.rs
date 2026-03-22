//! Python language plugin
//!
//! Provides AST parsing, scope extraction, and graph builder integration.

pub mod relations;

pub use relations::PythonGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

const PLUGIN_ID: &str = "python";
const TREE_SITTER_VERSION: &str = "0.23";

struct ScopeCapture {
    scope_type: String,
    scope_name: String,
    start: tree_sitter::Point,
    end: tree_sitter::Point,
}

/// Python language plugin implementation
pub struct PythonPlugin {
    graph_builder: PythonGraphBuilder,
}

impl PythonPlugin {
    /// Creates a new Python plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: PythonGraphBuilder::default(),
        }
    }
}

impl Default for PythonPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for PythonPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: PLUGIN_ID,
            name: "Python",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Python language support for sqry",
            tree_sitter_version: TREE_SITTER_VERSION,
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["py", "pyi"]
    }

    fn language(&self) -> Language {
        tree_sitter_python::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language())
            .map_err(|e| ParseError::LanguageSetFailed(e.to_string()))?;
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
        Self::extract_python_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl PythonPlugin {
    fn extract_python_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language = tree_sitter_python::LANGUAGE.into();

        let scope_query = Self::scope_query_source();
        let query = Query::new(&language, scope_query)
            .map_err(|e| ScopeError::QueryCompilationFailed(e.to_string()))?;

        let mut scopes = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut query_matches = cursor.matches(&query, root_node, content);

        while let Some(m) = query_matches.next() {
            if let Some(scope) = Self::scope_from_match(&query, m, content, file_path) {
                scopes.push(scope);
            }
        }

        scopes.sort_by_key(|s| (s.start_line, s.start_column));

        link_nested_scopes(&mut scopes);
        Ok(scopes)
    }

    fn scope_from_match(
        query: &Query,
        match_: &tree_sitter::QueryMatch<'_, '_>,
        content: &[u8],
        file_path: &Path,
    ) -> Option<Scope> {
        let capture = Self::scope_capture_from_match(query, match_, content)?;
        Some(Self::build_scope_from_capture(capture, file_path))
    }

    fn scope_capture_from_match(
        query: &Query,
        match_: &tree_sitter::QueryMatch<'_, '_>,
        content: &[u8],
    ) -> Option<ScopeCapture> {
        let mut scope_type = None;
        let mut scope_name = None;
        let mut scope_start = None;
        let mut scope_end = None;

        for capture in match_.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            let node = capture.node;

            if let Some((prefix, suffix)) = capture_name.rsplit_once('.') {
                match suffix {
                    "type" => {
                        scope_type = Some(prefix.to_string());
                        scope_start = Some(node.start_position());
                        scope_end = Some(node.end_position());
                    }
                    "name" => {
                        scope_name = node
                            .utf8_text(content)
                            .ok()
                            .map(std::string::ToString::to_string);
                    }
                    _ => {}
                }
            }
        }

        Some(ScopeCapture {
            scope_type: scope_type?,
            scope_name: scope_name?,
            start: scope_start?,
            end: scope_end?,
        })
    }

    fn build_scope_from_capture(capture: ScopeCapture, file_path: &Path) -> Scope {
        Scope {
            id: ScopeId::new(0),
            scope_type: capture.scope_type,
            name: capture.scope_name,
            file_path: file_path.to_path_buf(),
            start_line: capture.start.row + 1,
            start_column: capture.start.column,
            end_line: capture.end.row + 1,
            end_column: capture.end.column,
            parent_id: None,
        }
    }

    fn scope_query_source() -> &'static str {
        r"
; Function scopes
(function_definition
  name: (identifier) @function.name
) @function.type

; Class scopes
(class_definition
  name: (identifier) @class.name
) @class.type
"
    }
}
