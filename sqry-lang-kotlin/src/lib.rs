//! Kotlin language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for Kotlin, providing:
//! - AST parsing with tree-sitter
//! - Scope extraction
//! - Relation extraction (call graph, imports, exports, return types)
//!
//! # Supported Features
//!
//! - Classes (regular, data, enum, value, sealed)
//! - Objects (singleton declarations, companion objects)
//! - Interfaces (including functional interfaces)
//! - Functions (regular, suspend, extension, inline)
//! - Properties (val/var with getters/setters)
//! - Generic type parameters
//! - Visibility modifiers (public, private, protected, internal)
//! - Inheritance modifiers (open, abstract, final, override, sealed)

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

const PLUGIN_ID: &str = "kotlin";

pub mod relations;

/// Kotlin language plugin
///
/// Provides language support for Kotlin source files (.kt, .kts).
///
/// # Example
///
/// ```
/// use sqry_lang_kotlin::KotlinPlugin;
/// use sqry_core::plugin::LanguagePlugin;
///
/// let plugin = KotlinPlugin::default();
/// let metadata = plugin.metadata();
/// assert_eq!(metadata.id, "kotlin");
/// assert_eq!(metadata.name, "Kotlin");
/// ```
pub struct KotlinPlugin {
    graph_builder: relations::KotlinGraphBuilder,
}

impl KotlinPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: relations::KotlinGraphBuilder::new(),
        }
    }
}

impl Default for KotlinPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for KotlinPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: PLUGIN_ID,
            name: "Kotlin",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd.",
            description: "Kotlin language support for sqry",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["kt", "kts"]
    }

    fn language(&self) -> Language {
        tree_sitter_kotlin::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let language = self.language();

        parser.set_language(&language).map_err(|e| {
            ParseError::LanguageSetFailed(format!("Failed to set Kotlin language: {e}"))
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
        Self::extract_kotlin_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl KotlinPlugin {
    /// Extract scope information from Kotlin code
    fn extract_kotlin_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language = tree_sitter_kotlin::LANGUAGE.into();

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
; Function scopes
(function_declaration
  (simple_identifier) @function.name
) @function.type

; Class scopes
(class_declaration
  (type_identifier) @class.name
) @class.type

; Object scopes
(object_declaration
  (type_identifier) @object.name
) @object.type
"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_metadata() {
        let plugin = KotlinPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "kotlin");
        assert_eq!(metadata.name, "Kotlin");
        assert_eq!(metadata.author, "Verivus Pty Ltd.");
    }

    #[test]
    fn test_extensions() {
        let plugin = KotlinPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 2);
        assert!(extensions.contains(&"kt"));
        assert!(extensions.contains(&"kts"));
    }

    #[test]
    fn test_language() {
        let plugin = KotlinPlugin::default();
        let language = plugin.language();

        assert!(language.abi_version() > 0);
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = KotlinPlugin::default();
        let source = b"fun main() {}";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_extract_scopes_simple() {
        let plugin = KotlinPlugin::default();
        let source = b"class User { fun getName() = \"Alice\" }\nfun topLevel() {}";
        let file = PathBuf::from("test.kt");

        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, &file).unwrap();

        assert!(scopes.iter().any(|s| s.name == "User"));
        assert!(scopes.iter().any(|s| s.name == "getName"));
        assert!(scopes.iter().any(|s| s.name == "topLevel"));
    }
}
