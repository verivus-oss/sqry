//! Puppet language plugin for sqry.
//!
//! Provides production-ready graph and scope extraction for Puppet manifests.
//! Supports: class, defined type, resource, function, include/require relations.

mod relations;

pub use relations::PuppetGraphBuilder;

use anyhow::Result;
use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::error::{ParseError, ScopeError};
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin};
use std::path::Path;
use tree_sitter::{Language, Node, Parser, Tree};

/// Puppet language plugin
pub struct PuppetPlugin {
    graph_builder: PuppetGraphBuilder,
}

impl PuppetPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: PuppetGraphBuilder,
        }
    }
}

impl Default for PuppetPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl PuppetPlugin {
    /// Walk AST to extract scopes
    fn walk_ast_scopes(node: Node, content: &[u8], file_path: &Path, scopes: &mut Vec<Scope>) {
        match node.kind() {
            "class_definition" | "defined_resource_type" => {
                let mut cursor = node.walk();
                let mut name = None;

                for child in node.children(&mut cursor) {
                    if child.kind() == "identifier" || child.kind() == "class_identifier" {
                        name = child
                            .utf8_text(content)
                            .ok()
                            .map(std::string::ToString::to_string);
                        break;
                    }
                }

                if let Some(name) = name {
                    let start = node.start_position();
                    let end = node.end_position();
                    let scope_type = if node.kind() == "class_definition" {
                        "class"
                    } else {
                        "define"
                    };

                    scopes.push(Scope {
                        id: ScopeId::new(0), // Will be reassigned by link_nested_scopes
                        scope_type: scope_type.to_string(),
                        name,
                        file_path: file_path.to_path_buf(),
                        start_line: start.row + 1,
                        start_column: start.column + 1,
                        end_line: end.row + 1,
                        end_column: end.column + 1,
                        parent_id: None,
                    });
                }
            }
            _ => {}
        }

        // Recurse into children
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::walk_ast_scopes(child, content, file_path, scopes);
        }
    }
}

impl LanguagePlugin for PuppetPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "puppet",
            name: "Puppet",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Puppet language support with graph-native extraction",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pp"]
    }

    fn language(&self) -> Language {
        tree_sitter_puppet::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let lang = self.language();
        parser
            .set_language(&lang)
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
        let root = tree.root_node();
        let mut scopes = Vec::new();
        Self::walk_ast_scopes(root, content, file_path, &mut scopes);

        // Sort scopes by position (required for link_nested_scopes)
        scopes.sort_by_key(|s| (s.start_line, s.start_column));

        // Build parent-child relationships
        link_nested_scopes(&mut scopes);

        Ok(scopes)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}
