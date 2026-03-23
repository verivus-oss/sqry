//! Zig language plugin
//!
//! Provides relation tracking for `@import`/`usingnamespace` calls.
//!
//! ## Supported Zig Versions
//!
//! Zig 0.13+ (grammar pinned to tree-sitter-zig 1.1.2)

pub mod relations;

pub use relations::ZigGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin, error::ParseError};
use std::path::Path;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree};

const LANGUAGE_ID: &str = "zig";
const LANGUAGE_NAME: &str = "Zig";
const TREE_SITTER_VERSION: &str = "1.1.2";

/// Zig plugin implementation
pub struct ZigPlugin {
    graph_builder: ZigGraphBuilder,
}

impl ZigPlugin {
    /// Creates a new Zig plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: ZigGraphBuilder::default(),
        }
    }
}

impl Default for ZigPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for ZigPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: LANGUAGE_ID,
            name: LANGUAGE_NAME,
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Zig language support for sqry",
            tree_sitter_version: TREE_SITTER_VERSION,
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["zig", "zon"]
    }

    fn language(&self) -> Language {
        tree_sitter_zig::LANGUAGE.into()
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
    ) -> Result<Vec<Scope>, sqry_core::plugin::error::ScopeError> {
        Self::extract_zig_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl ZigPlugin {
    /// Extract scopes from Zig source using tree-sitter queries
    fn extract_zig_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, sqry_core::plugin::error::ScopeError> {
        use sqry_core::plugin::error::ScopeError;

        let root_node = tree.root_node();
        let language = tree_sitter_zig::LANGUAGE.into();

        // Zig scope query: functions, structs, enums, unions, tests
        // Note: Zig containers often get names from parent variable_declaration
        let scope_query = r"
; Function declarations
(function_declaration
  (identifier) @function.name
) @function.type

; Struct declarations (containers get name from parent variable_declaration)
(variable_declaration
  (identifier) @struct.name
  (struct_declaration)
) @struct.type

; Enum declarations
(variable_declaration
  (identifier) @enum.name
  (enum_declaration)
) @enum.type

; Union declarations
(variable_declaration
  (identifier) @union.name
  (union_declaration)
) @union.type

; Test declarations
(test_declaration
  (string
    (string_content) @test.name
  )?
) @test.type
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

            // For anonymous tests, generate a name from location
            if scope_type.as_deref() == Some("test")
                && scope_name.is_none()
                && let Some(start) = scope_start
            {
                scope_name = Some(format!("test@{}", start.row + 1));
            }

            if let (Some(stype), Some(sname), Some(start), Some(end)) =
                (scope_type, scope_name, scope_start, scope_end)
            {
                // Normalize scope type
                let normalized_type = match stype.as_str() {
                    "function" | "test" => "function",
                    "struct" | "union" => "struct",
                    "enum" => "enum",
                    other => other,
                };

                let scope = Scope {
                    id: ScopeId::new(0), // Will be reassigned by link_nested_scopes
                    scope_type: normalized_type.to_string(),
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
    fn test_plugin_metadata() {
        let plugin = ZigPlugin::default();
        let metadata = plugin.metadata();
        assert_eq!(metadata.id, "zig");
        assert_eq!(metadata.name, "Zig");
    }

    #[test]
    fn test_extensions() {
        let plugin = ZigPlugin::default();
        assert_eq!(plugin.extensions(), &["zig", "zon"]);
    }

    #[test]
    fn test_can_parse() {
        let plugin = ZigPlugin::default();
        let content = b"const std = @import(\"std\");";
        let tree = plugin.parse_ast(content);
        assert!(tree.is_ok());
    }

    #[test]
    fn test_graph_builder_returns_some() {
        let plugin = ZigPlugin::default();
        assert!(plugin.graph_builder().is_some());
    }
}
