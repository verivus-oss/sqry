//! Go language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for Go, providing:
//! - AST parsing with tree-sitter
//! - Scope extraction
//! - **Relation tracking** (calls/imports/exports); new semantics must flow through `sqry_core::graph::GraphBuilder` and `GoGraphBuilder` into `CodeGraph`

pub mod relations;

pub use relations::GoGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor, Tree};

/// Go language plugin
pub struct GoPlugin {
    graph_builder: GoGraphBuilder,
}

impl GoPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: GoGraphBuilder::default(),
        }
    }
}

impl Default for GoPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for GoPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "go",
            name: "Go",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Go language support for sqry",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["go"]
    }

    fn language(&self) -> tree_sitter::Language {
        tree_sitter_go::LANGUAGE.into()
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
        Self::extract_go_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl GoPlugin {
    fn extract_go_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language = tree_sitter_go::LANGUAGE.into();

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

                let capture_extension = std::path::Path::new(capture_name)
                    .extension()
                    .and_then(|ext| ext.to_str());
                if capture_extension.is_some_and(|ext| ext.eq_ignore_ascii_case("type")) {
                    scope_type = Some(capture_name.trim_end_matches(".type").to_string());
                    scope_start = Some(node.start_position());
                    scope_end = Some(node.end_position());
                } else if capture_extension.is_some_and(|ext| ext.eq_ignore_ascii_case("name")) {
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

; Method scopes
(method_declaration
  name: (field_identifier) @method.name
) @method.type
"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_metadata() {
        let plugin = GoPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "go");
        assert_eq!(metadata.name, "Go");
    }

    #[test]
    fn test_extensions() {
        let plugin = GoPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 1);
        assert!(extensions.contains(&"go"));
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = GoPlugin::default();
        let source = b"package main\nfunc main() {}";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_extract_scopes_simple() {
        let plugin = GoPlugin::default();
        let source = b"package main\nfunc hello() {}\nfunc world() int { return 42 }";
        let file = PathBuf::from("test.go");

        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, &file).unwrap();

        assert!(scopes.iter().any(|s| s.name == "hello"));
        assert!(scopes.iter().any(|s| s.name == "world"));
    }
}
