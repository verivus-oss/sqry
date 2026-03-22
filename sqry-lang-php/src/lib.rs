//! PHP language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for PHP, providing:
//! - AST parsing with tree-sitter
//! - Scope extraction
//! - Relation extraction via `PhpGraphBuilder` (calls, imports, exports, OOP edges)
//!
//! This plugin enables semantic code search for PHP codebases, the #6 priority
//! language powering 77% of websites (`WordPress`, Laravel, Symfony).

pub mod relations;

// Re-export graph builder types for testing
pub use relations::PhpGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

/// PHP language plugin
///
/// Provides language support for PHP files (.php).
///
/// # Supported Constructs
///
/// - Classes (`class Foo`)
/// - Traits (`trait Bar`)
/// - Interfaces (`interface IBaz`)
/// - Functions (global functions, methods)
/// - Namespaces (`namespace MyApp\Controllers`)
///
/// # Example
///
/// ```
/// use sqry_lang_php::PhpPlugin;
/// use sqry_core::plugin::LanguagePlugin;
///
/// let plugin = PhpPlugin::new();
/// let metadata = plugin.metadata();
/// assert_eq!(metadata.id, "php");
/// assert_eq!(metadata.name, "PHP");
/// ```
pub struct PhpPlugin {
    graph_builder: PhpGraphBuilder,
}

impl PhpPlugin {
    /// Creates a new PHP plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: PhpGraphBuilder::default(),
        }
    }
}

impl Default for PhpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for PhpPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "php",
            name: "PHP",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "PHP language support for sqry - web application code search",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["php"]
    }

    fn language(&self) -> Language {
        tree_sitter_php::LANGUAGE_PHP.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let language = self.language();

        parser.set_language(&language).map_err(|e| {
            ParseError::LanguageSetFailed(format!("Failed to set PHP language: {e}"))
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
        Self::extract_php_scopes(tree, content, file_path)
    }
    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl PhpPlugin {
    /// Extract scopes from PHP source using tree-sitter queries
    fn extract_php_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language: Language = tree_sitter_php::LANGUAGE_PHP.into();

        // PHP scope query for namespaces, classes, traits, interfaces, functions, methods
        let scope_query = r"
(namespace_definition
  name: (namespace_name) @namespace.name
) @namespace.type

(class_declaration
  name: (name) @class.name
) @class.type

(trait_declaration
  name: (name) @trait.name
) @trait.type

(interface_declaration
  name: (name) @interface.name
) @interface.type

(function_definition
  name: (name) @function.name
) @function.type

(method_declaration
  name: (name) @method.name
) @method.type
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

                if let Some((prefix, ext)) = capture_name.rsplit_once('.') {
                    if ext.eq_ignore_ascii_case("type") {
                        scope_type = Some(prefix.to_string());
                        scope_start = Some(node.start_position());
                        scope_end = Some(node.end_position());
                    } else if ext.eq_ignore_ascii_case("name") {
                        scope_name = node
                            .utf8_text(content)
                            .ok()
                            .map(std::string::ToString::to_string);
                    }
                }
            }

            if let (Some(stype), Some(sname), Some(start), Some(end)) =
                (scope_type, scope_name, scope_start, scope_end)
            {
                // Normalize scope type
                let normalized_type = match stype.as_str() {
                    "namespace" => "namespace",
                    "class" | "trait" | "interface" => "class",
                    "function" | "method" => "function",
                    other => other,
                };

                scopes.push(Scope {
                    id: ScopeId::new(0), // Will be reassigned by link_nested_scopes
                    scope_type: normalized_type.to_string(),
                    name: sname,
                    file_path: file_path.to_path_buf(),
                    start_line: start.row + 1,
                    start_column: start.column,
                    end_line: end.row + 1,
                    end_column: end.column,
                    parent_id: None,
                });
            }
        }

        // Sort by (start_line, start_column) for link_nested_scopes
        scopes.sort_by_key(|s| (s.start_line, s.start_column));

        link_nested_scopes(&mut scopes);
        Ok(scopes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata() {
        let plugin = PhpPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "php");
        assert_eq!(metadata.name, "PHP");
        assert_eq!(metadata.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(metadata.author, "Verivus Pty Ltd");
        assert_eq!(metadata.tree_sitter_version, "0.24");
    }

    #[test]
    fn test_extensions() {
        let plugin = PhpPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 1);
        assert!(extensions.contains(&"php"));
    }

    #[test]
    fn test_language() {
        let plugin = PhpPlugin::default();
        let language = plugin.language();

        // Just verify we can get a language (ABI version should be non-zero)
        assert!(language.abi_version() > 0);
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = PhpPlugin::default();
        let source = b"<?php class MyClass { }";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_plugin_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PhpPlugin>();
    }
}
