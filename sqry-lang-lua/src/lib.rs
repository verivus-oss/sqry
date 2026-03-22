//! Lua language plugin.
//!
//! Provides graph-native extraction via `LuaGraphBuilder`, AST parsing,
//! and scope extraction for Lua source files.

pub mod relations;

pub use relations::LuaGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree};

const LANGUAGE_ID: &str = "lua";
const LANGUAGE_NAME: &str = "Lua";
const TREE_SITTER_VERSION: &str = "0.2.0";

/// Lua plugin implementation.
pub struct LuaPlugin {
    graph_builder: LuaGraphBuilder,
}

impl LuaPlugin {
    /// Creates a new Lua plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: LuaGraphBuilder::default(),
        }
    }
}

impl Default for LuaPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for LuaPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: LANGUAGE_ID,
            name: LANGUAGE_NAME,
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Lua language support for sqry",
            tree_sitter_version: TREE_SITTER_VERSION,
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["lua", "rockspec"]
    }

    fn language(&self) -> Language {
        tree_sitter_lua::LANGUAGE.into()
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
        Self::extract_lua_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl LuaPlugin {
    /// Extract scopes from Lua source using tree-sitter queries.
    fn extract_lua_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language = tree_sitter_lua::LANGUAGE.into();

        // Lua scope query: function definitions (both styles).
        let scope_query = r"
; Function declarations (function name() ... end)
(function_declaration
  name: [
    (identifier) @function.name
    (dot_index_expression) @function.name
    (method_index_expression) @function.name
  ]
) @function.type

; Function definitions in assignments (local f = function() ... end)
(function_definition) @anonymous_function.type
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

            if scope_type.as_deref() == Some("anonymous_function")
                && scope_name.is_none()
                && let Some(start) = scope_start
            {
                scope_name = Some(format!("<anonymous:{}:{}>", start.row + 1, start.column));
            }

            if let (Some(stype), Some(sname), Some(start), Some(end)) =
                (scope_type, scope_name, scope_start, scope_end)
            {
                let normalized_type = match stype.as_str() {
                    "function" | "anonymous_function" => "function",
                    other => other,
                };

                let scope = Scope {
                    id: ScopeId::new(0),
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

        scopes.sort_by_key(|s| (s.start_line, s.start_column));
        link_nested_scopes(&mut scopes);
        Ok(scopes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_plugin_metadata() {
        let plugin = LuaPlugin::default();
        let metadata = plugin.metadata();
        assert_eq!(metadata.id, "lua");
        assert_eq!(metadata.name, "Lua");
    }

    #[test]
    fn test_extensions() {
        let plugin = LuaPlugin::default();
        assert_eq!(plugin.extensions(), &["lua", "rockspec"]);
    }

    #[test]
    fn test_can_parse() {
        let plugin = LuaPlugin::default();
        let content = b"function foo() return 1 end";
        let tree = plugin.parse_ast(content);
        assert!(tree.is_ok());
    }

    #[test]
    fn test_extract_scopes() {
        let plugin = LuaPlugin::default();
        let content = b"function foo() end\nfunction Module.bar() end\nlocal baz = function() end";
        let file = PathBuf::from("test.lua");

        let tree = plugin.parse_ast(content).expect("parse Lua");
        let scopes = plugin.extract_scopes(&tree, content, &file).unwrap();

        assert!(
            scopes
                .iter()
                .any(|s| s.name == "foo" && s.scope_type == "function"),
            "foo function scope should be extracted"
        );

        assert!(
            scopes
                .iter()
                .any(|s| s.name.contains("Module") && s.scope_type == "function"),
            "Module.bar scope should be extracted"
        );

        assert!(
            scopes
                .iter()
                .any(|s| s.name.starts_with("<anonymous:") && s.scope_type == "function"),
            "anonymous function scope should be extracted"
        );
    }
}
