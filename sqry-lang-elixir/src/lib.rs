//! Elixir language plugin
//!
//! Extracts scopes and graph relations using tree-sitter.

pub mod relations;

pub use relations::ElixirGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use tree_sitter::{Language, Node, Parser, Tree};

const LANGUAGE_ID: &str = "elixir";
const LANGUAGE_NAME: &str = "Elixir";
const TREE_SITTER_VERSION: &str = "0.23";

/// Elixir language plugin implementation
pub struct ElixirPlugin {
    graph_builder: ElixirGraphBuilder,
}

impl ElixirPlugin {
    /// Creates a new Elixir plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: ElixirGraphBuilder::default(),
        }
    }
}

impl Default for ElixirPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for ElixirPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: LANGUAGE_ID,
            name: LANGUAGE_NAME,
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Elixir language support for sqry",
            tree_sitter_version: TREE_SITTER_VERSION,
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ex", "exs"]
    }

    fn language(&self) -> Language {
        tree_sitter_elixir::LANGUAGE.into()
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
        Ok(Self::extract_elixir_scopes(tree, content, file_path))
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl ElixirPlugin {
    /// Extract scopes from Elixir source using AST traversal
    ///
    /// Elixir represents all macros (defmodule, def, etc.) as `call` nodes,
    /// so we traverse the AST to find scope-creating constructs.
    fn extract_elixir_scopes(tree: &Tree, content: &[u8], file_path: &Path) -> Vec<Scope> {
        let mut scopes = Vec::new();
        Self::collect_scopes_from_node(tree.root_node(), content, file_path, &mut scopes);

        // Sort by (start_line, start_column) for link_nested_scopes
        scopes.sort_by_key(|s| (s.start_line, s.start_column));

        link_nested_scopes(&mut scopes);
        scopes
    }

    fn collect_scopes_from_node(
        node: Node<'_>,
        content: &[u8],
        file_path: &Path,
        scopes: &mut Vec<Scope>,
    ) {
        if node.kind() == "call" {
            // Check identifier or target field for macro name
            let macro_name = node
                .child_by_field_name("identifier")
                .or_else(|| node.child_by_field_name("target"))
                .and_then(|n| n.utf8_text(content).ok());

            if let Some(name) = macro_name {
                let (scope_type, scope_name) = match name {
                    "defmodule" | "defprotocol" | "defimpl" => {
                        let module_name = Self::extract_module_name_for_scope(node, content);
                        ("module", module_name)
                    }
                    "def" | "defp" | "defmacro" | "defmacrop" => {
                        let func_name = Self::extract_function_name_for_scope(node, content);
                        ("function", func_name)
                    }
                    _ => (name, None),
                };

                // Only create scope if we have a valid scope type
                if matches!(scope_type, "module" | "function") {
                    let scope_name = scope_name.unwrap_or_else(|| "<anonymous>".to_string());
                    let start = node.start_position();
                    let end = node.end_position();

                    scopes.push(Scope {
                        id: ScopeId::new(0), // Will be reassigned by link_nested_scopes
                        scope_type: scope_type.to_string(),
                        name: scope_name,
                        file_path: file_path.to_path_buf(),
                        start_line: start.row + 1,
                        start_column: start.column,
                        end_line: end.row + 1,
                        end_column: end.column,
                        parent_id: None,
                    });
                }
            }
        }

        // Recurse into children
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.is_named() {
                Self::collect_scopes_from_node(child, content, file_path, scopes);
            }
        }
    }

    fn extract_module_name_for_scope(node: Node<'_>, content: &[u8]) -> Option<String> {
        // Look in arguments for the module alias
        let arguments = node.child_by_field_name("arguments").or_else(|| {
            let mut cursor = node.walk();
            node.children(&mut cursor).find(|c| c.kind() == "arguments")
        })?;

        let mut cursor = arguments.walk();
        arguments
            .children(&mut cursor)
            .find(|child| {
                child.is_named() && matches!(child.kind(), "alias" | "identifier" | "atom")
            })
            .and_then(|child| child.utf8_text(content).ok())
            .map(String::from)
    }

    fn extract_function_name_for_scope(node: Node<'_>, content: &[u8]) -> Option<String> {
        // Look in arguments for the function head
        let arguments = node.child_by_field_name("arguments").or_else(|| {
            let mut cursor = node.walk();
            node.children(&mut cursor).find(|c| c.kind() == "arguments")
        })?;

        let mut cursor = arguments.walk();
        for child in arguments.children(&mut cursor) {
            if !child.is_named() {
                continue;
            }
            match child.kind() {
                "call" => {
                    // def foo(args) - get function name from call target
                    if let Some(target) = child.child_by_field_name("target") {
                        return target.utf8_text(content).ok().map(String::from);
                    }
                    // Fallback: try to find identifier in call
                    let mut inner_cursor = child.walk();
                    for inner in child.children(&mut inner_cursor) {
                        if inner.is_named() && inner.kind() == "identifier" {
                            return inner.utf8_text(content).ok().map(String::from);
                        }
                    }
                }
                "identifier" => {
                    // def foo, do: ... - simple identifier
                    return child.utf8_text(content).ok().map(String::from);
                }
                "binary_operator" => {
                    // def foo(a, b) when ... - guard clause
                    if let Some(left) = child.child_by_field_name("left") {
                        if left.kind() == "call" {
                            if let Some(target) = left.child_by_field_name("target") {
                                return target.utf8_text(content).ok().map(String::from);
                            }
                        } else if left.kind() == "identifier" {
                            return left.utf8_text(content).ok().map(String::from);
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }
}
