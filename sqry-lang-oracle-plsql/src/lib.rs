// Nested conditionals kept for readability when traversing PL/SQL

//! Oracle PL/SQL language plugin for sqry.
//!
//! Provides graph-native extraction via `OraclePlsqlGraphBuilder`, AST parsing,
//! and scope extraction for PL/SQL source files.
//!
//! ## Current Limitations
//!
//! The tree-sitter-plsql grammar is designed primarily for PACKAGE and PACKAGE BODY
//! parsing. Standalone procedures, functions, and triggers have limited support in
//! the current grammar version (commit 28aebef, 2022-12-11).
//!
//! These limitations will be addressed as the upstream grammar evolves.

mod relations;

pub use relations::OraclePlsqlGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::error::{ParseError, ScopeError};
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin};
use std::path::Path;
use tree_sitter::{Language, Node, Parser, Tree};

#[derive(Debug)]
pub struct OraclePlsqlPlugin {
    graph_builder: OraclePlsqlGraphBuilder,
}

impl OraclePlsqlPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: OraclePlsqlGraphBuilder,
        }
    }
}

impl Default for OraclePlsqlPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for OraclePlsqlPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "plsql",
            name: "Oracle PL/SQL",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Oracle PL/SQL language support with Oracle-specific metadata",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        // Note: Removed "sql" to avoid conflict with standard SQL plugin.
        &["pks", "pkb", "pls", "plb", "prc", "fnc", "trg"]
    }

    fn language(&self) -> Language {
        tree_sitter_plsql_sqry::language()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language())
            .map_err(|e| ParseError::LanguageSetFailed(format!("{e:?}")))?;
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
        Ok(Self::extract_plsql_scopes(tree, content, file_path))
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl OraclePlsqlPlugin {
    /// Extract scopes from PL/SQL - packages, procedures, functions, blocks.
    fn extract_plsql_scopes(tree: &Tree, content: &[u8], file_path: &Path) -> Vec<Scope> {
        let mut scopes = Vec::new();
        Self::collect_plsql_scopes(tree.root_node(), content, file_path, &mut scopes);

        // Sort by position (required for link_nested_scopes).
        scopes.sort_by_key(|s| (s.start_line, s.start_column));

        // Build parent-child relationships.
        link_nested_scopes(&mut scopes);

        scopes
    }

    fn collect_plsql_scopes(
        node: Node<'_>,
        content: &[u8],
        file_path: &Path,
        scopes: &mut Vec<Scope>,
    ) {
        let scope_info = Self::extract_scope_info(node, content);

        if let Some((scope_type, name)) = scope_info {
            let start = node.start_position();
            let end = node.end_position();
            scopes.push(Scope {
                id: ScopeId::new(0),
                scope_type,
                name,
                file_path: file_path.to_path_buf(),
                start_line: start.row + 1,
                start_column: start.column,
                end_line: end.row + 1,
                end_column: end.column,
                parent_id: None,
            });
        }

        // Recurse into children.
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::collect_plsql_scopes(child, content, file_path, scopes);
        }
    }

    fn extract_scope_info(node: Node<'_>, content: &[u8]) -> Option<(String, String)> {
        match node.kind() {
            // Package specification and body.
            "create_package" | "package_spec" | "package_specification" => {
                let name = Self::extract_name_from_children(node, content)
                    .unwrap_or_else(|| "package".to_string());
                Some(("package".to_string(), name))
            }
            "create_package_body" | "package_body" => {
                let name = Self::extract_name_from_children(node, content)
                    .unwrap_or_else(|| "package_body".to_string());
                Some(("package_body".to_string(), name))
            }
            // Procedures.
            "create_procedure"
            | "procedure_definition"
            | "procedure_declaration"
            | "procedure_body"
            | "procedure_spec" => {
                let name = Self::extract_name_from_children(node, content)
                    .unwrap_or_else(|| "procedure".to_string());
                Some(("procedure".to_string(), name))
            }
            // Functions.
            "create_function"
            | "function_definition"
            | "function_declaration"
            | "function_body"
            | "function_spec" => {
                let name = Self::extract_name_from_children(node, content)
                    .unwrap_or_else(|| "function".to_string());
                Some(("function".to_string(), name))
            }
            // Triggers.
            "create_trigger" | "trigger_definition" => {
                let name = Self::extract_name_from_children(node, content)
                    .unwrap_or_else(|| "trigger".to_string());
                Some(("trigger".to_string(), name))
            }
            // Anonymous blocks (BEGIN...END).
            // Include "body" which is what tree-sitter-plsql uses for BEGIN..END blocks.
            "block" | "plsql_block" | "anonymous_block" | "begin_end_block" | "body" => {
                Some(("block".to_string(), "anonymous".to_string()))
            }
            // Cursors.
            "cursor_definition" | "cursor_declaration" => {
                let name = Self::extract_name_from_children(node, content)
                    .unwrap_or_else(|| "cursor".to_string());
                Some(("cursor".to_string(), name))
            }
            // Exception handlers.
            "exception_handler" => Some(("exception".to_string(), "handler".to_string())),
            _ => None,
        }
    }

    fn extract_name_from_children(node: Node<'_>, content: &[u8]) -> Option<String> {
        // Try named fields first.
        for field_name in &["name", "identifier", "object_name", "package_name"] {
            if let Some(name_node) = node.child_by_field_name(field_name)
                && let Ok(text) = name_node.utf8_text(content)
            {
                let name = text.trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }

        // Fall back to looking for identifier children.
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if matches!(
                child.kind(),
                "identifier" | "name" | "simple_identifier" | "object_name"
            ) && let Ok(text) = child.utf8_text(content)
            {
                let name = text.trim();
                if !name.is_empty() {
                    return Some(name.to_string());
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
    fn test_plugin_metadata() {
        let plugin = OraclePlsqlPlugin::new();
        let metadata = plugin.metadata();
        assert_eq!(metadata.id, "plsql");
        assert_eq!(metadata.name, "Oracle PL/SQL");
    }

    #[test]
    fn test_file_extensions() {
        let plugin = OraclePlsqlPlugin::new();
        let exts = plugin.extensions();
        // Plain .sql files are handled by the generic SQL plugin.
        assert!(
            !exts.contains(&"sql"),
            "Oracle PL/SQL plugin must not claim `.sql` (handled by sqry-lang-sql)"
        );
        for ext in &["pks", "pkb", "pls", "plb", "prc", "fnc", "trg"] {
            assert!(
                exts.contains(ext),
                "Oracle PL/SQL plugin should support .{ext} files"
            );
        }
    }

    #[test]
    fn test_basic_parsing() {
        let plugin = OraclePlsqlPlugin::new();
        let plsql_code =
            b"CREATE OR REPLACE PROCEDURE test_proc IS\nBEGIN\n  NULL;\nEND test_proc;";

        let result = plugin.parse_ast(plsql_code);
        assert!(result.is_ok(), "Should parse basic PL/SQL procedure");
    }

    // ========================================================================
    // Scope Extraction Tests
    // ========================================================================

    #[test]
    fn test_extract_scopes_procedure() {
        let plugin = OraclePlsqlPlugin::new();
        let source = br"
CREATE OR REPLACE PROCEDURE my_proc IS
BEGIN
    NULL;
END my_proc;
";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("my_proc.prc"))
            .expect("Should extract scopes from procedure");

        // Grammar extracts 'body' as a block scope for BEGIN..END.
        assert!(
            !scopes.is_empty(),
            "Should extract at least one scope from procedure"
        );
        let block_scope = scopes.iter().find(|s| s.scope_type == "block");
        assert!(
            block_scope.is_some(),
            "Should find block scope for BEGIN..END, got: {:?}",
            scopes.iter().map(|s| &s.scope_type).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_extract_scopes_function() {
        let plugin = OraclePlsqlPlugin::new();
        let source = br"
CREATE OR REPLACE FUNCTION my_func RETURN NUMBER IS
BEGIN
    RETURN 42;
END my_func;
";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("my_func.fnc"))
            .expect("Should extract scopes from function");

        // Grammar extracts 'body' as a block scope for BEGIN..END.
        assert!(
            !scopes.is_empty(),
            "Should extract at least one scope from function"
        );
        let block_scope = scopes.iter().find(|s| s.scope_type == "block");
        assert!(
            block_scope.is_some(),
            "Should find block scope for BEGIN..END"
        );
    }

    #[test]
    fn test_extract_scopes_package() {
        let plugin = OraclePlsqlPlugin::new();
        let source = br"
CREATE OR REPLACE PACKAGE my_pkg AS
    PROCEDURE do_something;
END my_pkg;
";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("my_pkg.pks"))
            .expect("Should extract scopes from package");

        // Package spec may not have a body, so scopes can be empty.
        for scope in &scopes {
            assert!(
                !scope.scope_type.is_empty(),
                "Scope type should not be empty"
            );
        }
    }

    #[test]
    fn test_extract_scopes_anonymous_block() {
        let plugin = OraclePlsqlPlugin::new();
        let source = br"
BEGIN
    DBMS_OUTPUT.PUT_LINE('Hello');
END;
";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.pls"))
            .expect("Should extract scopes from anonymous block");

        // Anonymous block should be detected via 'body' node.
        assert!(
            !scopes.is_empty(),
            "Should extract at least one scope from anonymous block"
        );
        let block_scope = scopes.iter().find(|s| s.scope_type == "block");
        assert!(
            block_scope.is_some(),
            "Should find block scope for anonymous BEGIN..END"
        );
    }

    #[test]
    fn test_extract_scopes_nested() {
        let plugin = OraclePlsqlPlugin::new();
        let source = br"
CREATE OR REPLACE PACKAGE BODY my_pkg AS
    PROCEDURE inner_proc IS
    BEGIN
        NULL;
    END inner_proc;
END my_pkg;
";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("my_pkg.pkb"))
            .expect("Should extract scopes from nested PL/SQL");

        // Package body with nested procedure should have at least one block scope.
        assert!(
            !scopes.is_empty(),
            "Should extract at least one scope from package body"
        );
        let block_scope = scopes.iter().find(|s| s.scope_type == "block");
        assert!(
            block_scope.is_some(),
            "Should find block scope for inner procedure's BEGIN..END"
        );
    }

    #[test]
    fn test_extract_scopes_boundaries() {
        let plugin = OraclePlsqlPlugin::new();
        let source = br"
CREATE OR REPLACE PROCEDURE my_proc IS
BEGIN
    NULL;
END my_proc;
";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("my_proc.prc"))
            .unwrap();

        for scope in &scopes {
            assert!(scope.start_line >= 1, "start_line should be >= 1");
            assert!(
                scope.end_line >= scope.start_line,
                "end_line should be >= start_line"
            );
        }
    }

    #[test]
    fn test_extract_scopes_empty_file() {
        let plugin = OraclePlsqlPlugin::new();
        let source = b"";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("empty.pls"))
            .unwrap();

        assert!(scopes.is_empty(), "Empty file should have no scopes");
    }

    #[test]
    fn test_extract_scopes_malformed() {
        let plugin = OraclePlsqlPlugin::new();
        let source = b"CREATE OR REPLACE PROCEDURE broken IS BEGIN";
        let tree = plugin.parse_ast(source).unwrap();
        let result = plugin.extract_scopes(&tree, source, Path::new("broken.prc"));

        assert!(result.is_ok(), "Should handle malformed PL/SQL gracefully");
    }
}
