// Nested conditionals kept for readability in R AST traversal

// Nested conditionals kept for readability in R AST traversal

//! R language plugin
//!
//! Provides graph-native extraction for functions, S3/S4 methods, R6 classes,
//! and captures import/export relations common in R packages.

pub mod relations;

pub use relations::RGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use tree_sitter::{Language, Node, Parser, Tree};

const LANGUAGE_ID: &str = "r";
const LANGUAGE_NAME: &str = "R";
const TREE_SITTER_VERSION: &str = "1.2.0";

/// R language plugin implementation
pub struct RPlugin {
    graph_builder: RGraphBuilder,
}

impl RPlugin {
    /// Creates a new R plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: RGraphBuilder::default(),
        }
    }
}

impl Default for RPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for RPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: LANGUAGE_ID,
            name: LANGUAGE_NAME,
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "R language support for sqry",
            tree_sitter_version: TREE_SITTER_VERSION,
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["r", "rmd", "q"]
    }

    fn language(&self) -> Language {
        tree_sitter_r::LANGUAGE.into()
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
        Ok(Self::extract_r_scopes(tree, content, file_path))
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl RPlugin {
    /// Extract scopes from R source using AST traversal
    ///
    /// R scope-creating constructs:
    /// - `function_definition` -> function scope
    /// - `R6Class` call -> class scope
    /// - setClass call -> class scope
    fn extract_r_scopes(tree: &Tree, content: &[u8], file_path: &Path) -> Vec<Scope> {
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
        match node.kind() {
            "function_definition" => {
                // Try to get function name from parent assignment
                let name = Self::get_function_name_from_parent(node, content)
                    .unwrap_or_else(|| "<anonymous>".to_string());

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
            "call" => {
                // Check for R6Class or setClass calls
                if let Some(function_node) = node.child_by_field_name("function")
                    && let Ok(function_name) = function_node.utf8_text(content)
                    && matches!(function_name.trim(), "R6Class" | "setClass")
                {
                    let class_name = Self::get_class_name_from_call(node, content)
                        .unwrap_or_else(|| "<anonymous>".to_string());

                    let start = node.start_position();
                    let end = node.end_position();

                    scopes.push(Scope {
                        id: ScopeId::new(0),
                        scope_type: "class".to_string(),
                        name: class_name,
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
            Self::collect_scopes_from_node(child, content, file_path, scopes);
        }
    }

    fn get_function_name_from_parent(node: Node<'_>, content: &[u8]) -> Option<String> {
        // Look for parent binary_operator with <- or = assignment
        let parent = node.parent()?;
        if parent.kind() == "binary_operator" {
            let operator = parent.child_by_field_name("operator")?;
            let op_text = operator.utf8_text(content).ok()?;
            if matches!(op_text.trim(), "<-" | "<<-" | "=" | ":=") {
                let lhs = parent.child_by_field_name("lhs")?;
                return lhs.utf8_text(content).ok().map(|s| s.trim().to_string());
            }
        }
        None
    }

    fn get_class_name_from_call(node: Node<'_>, content: &[u8]) -> Option<String> {
        // For R6Class/setClass, try to get name from parent assignment LHS
        let parent = node.parent()?;
        if parent.kind() == "binary_operator" {
            let lhs = parent.child_by_field_name("lhs")?;
            return lhs.utf8_text(content).ok().map(|s| s.trim().to_string());
        }
        // Fallback: try to get from first argument
        let arguments = node.child_by_field_name("arguments")?;
        let mut cursor = arguments.walk();
        for arg in arguments.named_children(&mut cursor) {
            if arg.kind() == "argument"
                && let Some(value) = arg.child_by_field_name("value")
                && value.kind() == "string"
            {
                let text = value.utf8_text(content).ok()?;
                // Trim quotes
                let trimmed = text.trim();
                if (trimmed.starts_with('"') && trimmed.ends_with('"'))
                    || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
                {
                    return Some(trimmed[1..trimmed.len() - 1].to_string());
                }
            }
        }
        None
    }
}
