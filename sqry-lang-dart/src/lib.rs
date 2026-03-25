//! Dart language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for Dart, providing:
//! - Graph-native node/edge extraction via `DartGraphBuilder`
//! - AST parsing with tree-sitter
//! - Scope extraction for Dart constructs
//!
//! # Supported Features
//!
//! - Classes (`class`, `abstract class`)
//! - Functions (`void`, typed functions)
//! - Methods (instance and static)
//! - Variables (`var`, `final`, `const`)
//! - Async/await support
//! - Visibility modifiers (public via no underscore, private via underscore)
//!
//! # Node Attributes
//!
//! All modifiers are detected via AST node walking, avoiding false positives
//! from comments, strings, or identifiers containing modifier keywords:
//!
//! - **`is_async`**: Detected via `async` or `async*` tokens in function body
//! - **`is_static`**: Detected via `static` keyword node
//! - **visibility**: Determined by identifier name prefix (`_` = private)
//!
//! # Example
//!
//! ```
//! use sqry_lang_dart::DartPlugin;
//! use sqry_core::plugin::LanguagePlugin;
//!
//! let plugin = DartPlugin::default();
//! let metadata = plugin.metadata();
//! assert_eq!(metadata.id, "dart");
//! assert_eq!(metadata.name, "Dart");
//! ```

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

const PLUGIN_ID: &str = "dart";

/// Dart relation extraction and graph building
pub mod relations;

pub use relations::DartGraphBuilder;

/// Dart language plugin
///
/// Provides language support for Dart source files (.dart).
pub struct DartPlugin {
    graph_builder: DartGraphBuilder,
}

impl DartPlugin {
    /// Create a new Dart plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: DartGraphBuilder::new(),
        }
    }
}

impl Default for DartPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for DartPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: PLUGIN_ID,
            name: "Dart",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus",
            description: "Dart language support for sqry",
            tree_sitter_version: "0.22",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["dart"]
    }

    fn language(&self) -> Language {
        tree_sitter_dart::language()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let language = self.language();

        parser.set_language(&language).map_err(|e| {
            ParseError::LanguageSetFailed(format!("Failed to set Dart language: {e}"))
        })?;

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
        Self::extract_dart_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl DartPlugin {
    /// Extract scope information from Dart code
    fn extract_dart_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language = tree_sitter_dart::language();

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

    /// Returns tree-sitter query source for scope extraction
    fn scope_query_source() -> &'static str {
        r"
; Function scopes (includes both top-level functions and class methods)
(function_signature
  name: (identifier) @function.name
) @function.type

; Class scopes
(class_definition
  name: (identifier) @class.name
) @class.type

; Getter scopes
(getter_signature
  name: (identifier) @getter.name
) @getter.type

; Setter scopes
(setter_signature
  name: (identifier) @setter.name
) @setter.type
"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_metadata() {
        let plugin = DartPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "dart");
        assert_eq!(metadata.name, "Dart");
        assert_eq!(metadata.author, "Verivus");
    }

    #[test]
    fn test_extensions() {
        let plugin = DartPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 1);
        assert!(extensions.contains(&"dart"));
    }

    #[test]
    fn test_language() {
        let plugin = DartPlugin::default();
        let language = plugin.language();

        assert!(language.abi_version() > 0);
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = DartPlugin::default();
        let source = b"void main() {}";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_extract_scopes_basic() {
        let plugin = DartPlugin::default();
        let source = b"class User { void greet() {} } void main() {}";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("test.dart"))
            .unwrap();

        assert!(
            scopes
                .iter()
                .any(|scope| scope.scope_type == "class" && scope.name == "User"),
            "class scope not found"
        );
        assert!(
            scopes
                .iter()
                .any(|scope| scope.scope_type == "function" && scope.name == "main"),
            "function scope not found"
        );
    }
}
