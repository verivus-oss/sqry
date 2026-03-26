//! SQL language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for SQL, providing:
//! - AST parsing with tree-sitter
//!
//! This plugin enables semantic code search for SQL codebases, the #5 priority
//! language for universal database query and data management (100% adoption in data-driven companies).

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use tree_sitter::{Language, Node, Parser, Tree};

/// SQL relation extraction and graph building
pub mod relations;

pub use relations::SqlGraphBuilder;

/// SQL language plugin
///
/// Provides language support for SQL files (.sql).
///
/// # Example
///
/// ```
/// use sqry_lang_sql::SqlPlugin;
/// use sqry_core::plugin::LanguagePlugin;
///
/// let plugin = SqlPlugin::new();
/// let metadata = plugin.metadata();
/// assert_eq!(metadata.id, "sql");
/// assert_eq!(metadata.name, "SQL");
/// ```
pub struct SqlPlugin {
    graph_builder: SqlGraphBuilder,
}

impl SqlPlugin {
    /// Creates a new SQL plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: SqlGraphBuilder,
        }
    }
}

impl Default for SqlPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for SqlPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "sql",
            name: "SQL",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "SQL language support for sqry - database schema and query search",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["sql"]
    }

    fn language(&self) -> Language {
        tree_sitter_sequel::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let language = self.language();

        parser.set_language(&language).map_err(|e| {
            ParseError::LanguageSetFailed(format!("Failed to set SQL language: {e}"))
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
        let mut scopes = Vec::new();
        Self::collect_scopes(tree.root_node(), content, file_path, &mut scopes);

        // Sort by position and link nested scopes
        scopes.sort_by_key(|s| (s.start_line, s.start_column));
        link_nested_scopes(&mut scopes);

        Ok(scopes)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl SqlPlugin {
    /// Collect scope information from SQL AST nodes
    ///
    /// Extracts scopes for:
    /// - Functions (`create_function`) - including stored procedures
    /// - Triggers (`create_trigger`)
    fn collect_scopes(node: Node, content: &[u8], file_path: &Path, scopes: &mut Vec<Scope>) {
        match node.kind() {
            "create_function" => {
                // Extract function name from object_reference
                if let Some(name) = Self::extract_name_from_object_reference(&node, content) {
                    let start = node.start_position();
                    let end = node.end_position();

                    scopes.push(Scope {
                        id: ScopeId::new(0),
                        scope_type: "function".to_string(),
                        name,
                        file_path: file_path.to_path_buf(),
                        start_line: start.row + 1,
                        start_column: start.column,
                        end_line: end.row + 1,
                        end_column: end.column,
                        parent_id: None,
                    });
                }
            }
            "create_trigger" => {
                // Extract trigger name from object_reference
                if let Some(name) = Self::extract_name_from_object_reference(&node, content) {
                    let start = node.start_position();
                    let end = node.end_position();

                    scopes.push(Scope {
                        id: ScopeId::new(0),
                        scope_type: "trigger".to_string(),
                        name,
                        file_path: file_path.to_path_buf(),
                        start_line: start.row + 1,
                        start_column: start.column,
                        end_line: end.row + 1,
                        end_column: end.column,
                        parent_id: None,
                    });
                }
            }
            _ => {}
        }

        // Recurse into children
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            Self::collect_scopes(child, content, file_path, scopes);
        }
    }

    /// Extract name from an `object_reference` child node
    fn extract_name_from_object_reference(node: &Node, content: &[u8]) -> Option<String> {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "object_reference" {
                // Look for the identifier with name field
                let mut inner_cursor = child.walk();
                for inner_child in child.named_children(&mut inner_cursor) {
                    if inner_child.kind() == "identifier"
                        && let Ok(text) = inner_child.utf8_text(content)
                    {
                        return Some(text.to_string());
                    }
                }
                // Also try the name field
                if let Some(name_node) = child.child_by_field_name("name")
                    && let Ok(text) = name_node.utf8_text(content)
                {
                    return Some(text.to_string());
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata() {
        let plugin = SqlPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "sql");
        assert_eq!(metadata.name, "SQL");
        assert_eq!(metadata.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(metadata.author, "Verivus Pty Ltd");
        assert_eq!(metadata.tree_sitter_version, "0.24");
    }

    #[test]
    fn test_extensions() {
        let plugin = SqlPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 1);
        assert!(extensions.contains(&"sql"));
    }

    #[test]
    fn test_language() {
        let plugin = SqlPlugin::default();
        let language = plugin.language();

        // Just verify we can get a language (ABI version should be non-zero)
        assert!(language.abi_version() > 0);
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = SqlPlugin::default();
        let source = b"CREATE TABLE users (id INT);";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_plugin_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SqlPlugin>();
    }

    #[test]
    fn test_extract_function_scope() {
        use std::path::PathBuf;

        let plugin = SqlPlugin::default();
        let source = b"CREATE FUNCTION calculate_tax(amount DECIMAL)
RETURNS DECIMAL
AS $$ BEGIN RETURN amount * 0.1; END; $$ LANGUAGE plpgsql;";
        let file = PathBuf::from("test.sql");

        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, &file).unwrap();

        // Check that function scope is extracted
        let func_scope = scopes
            .iter()
            .find(|s| s.name == "calculate_tax" && s.scope_type == "function");
        assert!(
            func_scope.is_some(),
            "calculate_tax function scope should be extracted, got: {:?}",
            scopes
                .iter()
                .map(|s| (&s.name, &s.scope_type))
                .collect::<Vec<_>>()
        );

        // Top-level function scopes should have no parent
        assert_eq!(
            func_scope.unwrap().parent_id,
            None,
            "Top-level function scope should have parent_id = None"
        );
    }

    #[test]
    fn test_extract_trigger_scope() {
        use std::path::PathBuf;

        let plugin = SqlPlugin::default();
        let source = b"CREATE TRIGGER update_timestamp
BEFORE UPDATE ON users
FOR EACH ROW
EXECUTE FUNCTION update_modified_column();";
        let file = PathBuf::from("test.sql");

        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, &file).unwrap();

        // Check that trigger scope is extracted
        let trigger_scope = scopes
            .iter()
            .find(|s| s.name == "update_timestamp" && s.scope_type == "trigger");
        assert!(
            trigger_scope.is_some(),
            "update_timestamp trigger scope should be extracted, got: {:?}",
            scopes
                .iter()
                .map(|s| (&s.name, &s.scope_type))
                .collect::<Vec<_>>()
        );

        // Top-level trigger scopes should have no parent
        assert_eq!(
            trigger_scope.unwrap().parent_id,
            None,
            "Top-level trigger scope should have parent_id = None"
        );
    }

    #[test]
    fn test_multiple_scopes() {
        use std::path::PathBuf;

        let plugin = SqlPlugin::default();
        // Use more complete function syntax with parameters
        let source = b"CREATE FUNCTION calculate_total(price DECIMAL)
RETURNS DECIMAL AS $$ BEGIN RETURN price * 1.1; END; $$ LANGUAGE plpgsql;

CREATE FUNCTION get_user_count(status VARCHAR)
RETURNS INT AS $$ BEGIN RETURN 0; END; $$ LANGUAGE plpgsql;

CREATE TRIGGER audit_changes
BEFORE UPDATE ON users
FOR EACH ROW EXECUTE FUNCTION log_update();";
        let file = PathBuf::from("test.sql");

        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, &file).unwrap();

        // Check that all scopes are extracted
        let func_scopes: Vec<_> = scopes
            .iter()
            .filter(|s| s.scope_type == "function")
            .collect();
        let trigger_scopes: Vec<_> = scopes
            .iter()
            .filter(|s| s.scope_type == "trigger")
            .collect();

        assert!(
            func_scopes.len() >= 2,
            "Should have at least 2 function scopes, got: {} - names: {:?}",
            func_scopes.len(),
            func_scopes.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        assert!(
            !trigger_scopes.is_empty(),
            "Should have at least 1 trigger scope, got: {} - names: {:?}",
            trigger_scopes.len(),
            trigger_scopes.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }
}
