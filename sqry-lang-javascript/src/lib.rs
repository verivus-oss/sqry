//! JavaScript language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for JavaScript, providing:
//! - AST parsing with tree-sitter
//! - Scope extraction
//! - Relation extraction via `JavaScriptGraphBuilder` (calls, imports, exports, OOP edges)

pub mod relations;

pub use relations::JavaScriptGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

/// JavaScript language plugin
pub struct JavaScriptPlugin {
    graph_builder: JavaScriptGraphBuilder,
}

impl JavaScriptPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: JavaScriptGraphBuilder::default(),
        }
    }
}

impl Default for JavaScriptPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for JavaScriptPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "javascript",
            name: "JavaScript",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "JavaScript language support for sqry",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["js", "jsx", "mjs"]
    }

    fn language(&self) -> Language {
        tree_sitter_javascript::LANGUAGE.into()
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
        Self::extract_javascript_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl JavaScriptPlugin {
    fn extract_javascript_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language = tree_sitter_javascript::LANGUAGE.into();

        let scope_query = Self::scope_query_source();
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

                if std::path::Path::new(capture_name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("type"))
                {
                    scope_type = Some(capture_name.trim_end_matches(".type").to_string());
                    scope_start = Some(node.start_position());
                    scope_end = Some(node.end_position());
                } else if std::path::Path::new(capture_name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("name"))
                {
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

    fn scope_query_source() -> &'static str {
        r"
; Function scopes
(function_declaration
  name: (identifier) @function.name
) @function.type

; Class scopes
(class_declaration
  name: (identifier) @class.name
) @class.type

; Method scopes
(method_definition
  name: (property_identifier) @method.name
) @method.type
"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata() {
        let plugin = JavaScriptPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "javascript");
        assert_eq!(metadata.name, "JavaScript");
    }

    #[test]
    fn test_extensions() {
        let plugin = JavaScriptPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 3);
        assert!(extensions.contains(&"js"));
        assert!(extensions.contains(&"jsx"));
        assert!(extensions.contains(&"mjs"));
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = JavaScriptPlugin::default();
        let source = b"function hello() {}";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }
}
