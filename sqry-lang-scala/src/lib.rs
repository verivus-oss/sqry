//! Scala language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for Scala, providing:
//! - AST parsing with tree-sitter
//! - Scope extraction
//! - `GraphBuilder` implementation for code graph construction
//!
//! # Supported Features
//!
//! - Classes (`class`, `case class`, `abstract class`)
//! - Objects (`object` - singleton declarations)
//! - Traits (`trait` - Scala's interfaces/mixins)
//! - Functions (`def`)
//! - Values (`val` - immutable) and variables (`var` - mutable)
//! - Generic type parameters
//! - Visibility modifiers (public, private, protected, package-private)
//! - Sealed classes/traits
//! - Implicit methods/parameters
//!
//! # Example
//!
//! ```
//! use sqry_lang_scala::ScalaPlugin;
//! use sqry_core::plugin::LanguagePlugin;
//!
//! let plugin = ScalaPlugin::default();
//! let metadata = plugin.metadata();
//! assert_eq!(metadata.id, "scala");
//! assert_eq!(metadata.name, "Scala");
//! ```

pub mod relations;

pub use relations::ScalaGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

/// Scala language plugin
///
/// Provides language support for Scala source files (.scala, .sc).
pub struct ScalaPlugin {
    graph_builder: ScalaGraphBuilder,
}

impl ScalaPlugin {
    /// Creates a new Scala plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: ScalaGraphBuilder,
        }
    }
}

impl Default for ScalaPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for ScalaPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "scala",
            name: "Scala",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Scala language support for sqry",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["scala", "sc"]
    }

    fn language(&self) -> Language {
        tree_sitter_scala::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let language = self.language();

        parser.set_language(&language).map_err(|e| {
            ParseError::LanguageSetFailed(format!("Failed to set Scala language: {e}"))
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
        Self::extract_scala_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl ScalaPlugin {
    /// Extract scope information from Scala code
    fn extract_scala_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language = tree_sitter_scala::LANGUAGE.into();

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
; Function scopes
(function_definition
  name: (identifier) @function.name
) @function.type

; Class scopes
(class_definition
  name: (identifier) @class.name
) @class.type

; Object scopes
(object_definition
  name: (identifier) @object.name
) @object.type

; Trait scopes
(trait_definition
  name: (identifier) @trait.name
) @trait.type
"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_metadata() {
        let plugin = ScalaPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "scala");
        assert_eq!(metadata.name, "Scala");
        assert_eq!(metadata.author, "Verivus Pty Ltd");
    }

    #[test]
    fn test_extensions() {
        let plugin = ScalaPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 2);
        assert!(extensions.contains(&"scala"));
        assert!(extensions.contains(&"sc"));
    }

    #[test]
    fn test_language() {
        let plugin = ScalaPlugin::default();
        let language = plugin.language();

        assert!(language.abi_version() > 0);
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = ScalaPlugin::default();
        let source = b"def main() = ()";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }
    #[test]
    fn test_extract_scopes_simple() {
        let plugin = ScalaPlugin::default();
        let source = b"class Container[T](value: T) { def get(): T = value }";
        let file = PathBuf::from("test.scala");

        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, &file).unwrap();

        assert!(scopes.iter().any(|s| s.name == "Container"));
        assert!(scopes.iter().any(|s| s.name == "get"));
    }
}
