//! Ruby language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for Ruby, providing:
//! - AST parsing with tree-sitter
//! - Scope extraction
//! - Relation extraction via `RubyGraphBuilder` (calls, imports, exports, FFI)
//!
//! # Supported Features
//!
//! - Classes (regular, subclass, singleton class)
//! - Modules (namespacing and mixins)
//! - Methods (instance and class methods)
//! - Singleton methods (class methods via `def self.method`)
//! - Visibility modifiers (public, private, protected)
//! - `attr_reader`, `attr_writer`, `attr_accessor` (property declarations)

mod relations;

pub use relations::RubyGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

/// Ruby language plugin
///
/// Provides language support for Ruby source files (.rb).
///
/// # Supported Constructs
///
/// - Classes (`class`)
/// - Modules (`module`)
/// - Methods (`def`, instance methods)
/// - Singleton methods (`def self.method`, class methods)
///
/// # Example
///
/// ```
/// use sqry_lang_ruby::RubyPlugin;
/// use sqry_core::plugin::LanguagePlugin;
///
/// let plugin = RubyPlugin::default();
/// let metadata = plugin.metadata();
/// assert_eq!(metadata.id, "ruby");
/// assert_eq!(metadata.name, "Ruby");
/// ```
pub struct RubyPlugin {
    graph_builder: RubyGraphBuilder,
}

impl RubyPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: RubyGraphBuilder::default(),
        }
    }
}

impl Default for RubyPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for RubyPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "ruby",
            name: "Ruby",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Ruby language support for sqry",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rb", "rake", "gemspec"]
    }

    fn language(&self) -> Language {
        tree_sitter_ruby::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let language = self.language();

        parser.set_language(&language).map_err(|e| {
            ParseError::LanguageSetFailed(format!("Failed to set Ruby language: {e}"))
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
        Self::extract_ruby_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl RubyPlugin {
    /// Extract scope information from Ruby code
    fn extract_ruby_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language = tree_sitter_ruby::LANGUAGE.into();

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

                let capture_ext = std::path::Path::new(capture_name)
                    .extension()
                    .and_then(|ext| ext.to_str());

                if capture_ext.is_some_and(|ext| ext.eq_ignore_ascii_case("type")) {
                    scope_type = Some(capture_name.trim_end_matches(".type").to_string());
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

    /// Returns tree-sitter query source for scope extraction
    fn scope_query_source() -> &'static str {
        r"
; Method scopes
(method
  name: (identifier) @method.name
) @method.type

; Singleton method scopes
(singleton_method
  name: (identifier) @singleton_method.name
) @singleton_method.type

; Class scopes
(class
  name: (constant) @class.name
) @class.type

; Module scopes
(module
  name: (constant) @module.name
) @module.type
"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata() {
        let plugin = RubyPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "ruby");
        assert_eq!(metadata.name, "Ruby");
        assert_eq!(metadata.author, "Verivus Pty Ltd");
    }

    #[test]
    fn test_extensions() {
        let plugin = RubyPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 3);
        assert!(extensions.contains(&"rb"));
        assert!(extensions.contains(&"rake"));
        assert!(extensions.contains(&"gemspec"));
    }

    #[test]
    fn test_language() {
        let plugin = RubyPlugin::default();
        let language = plugin.language();

        assert!(language.abi_version() > 0);
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = RubyPlugin::default();
        let source = b"def hello; end";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }
}
