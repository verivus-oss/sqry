//! Terraform (HCL) language plugin for sqry.
//!
//! Provides production-ready graph and scope extraction for Terraform/HCL files.
//! Supports: resource, data, module, variable, output, provider, locals blocks.

mod relations;

pub use relations::TerraformGraphBuilder;

use anyhow::Result;
use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::error::{ParseError, ScopeError};
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin};
use std::path::Path;
use tree_sitter::{Language, Parser, Tree};

/// Terraform/HCL language plugin
pub struct TerraformPlugin {
    graph_builder: TerraformGraphBuilder,
}

impl TerraformPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: TerraformGraphBuilder,
        }
    }
}

impl Default for TerraformPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for TerraformPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "terraform",
            name: "Terraform",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Terraform (HCL) language support with graph-native extraction",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["tf", "tfvars", "hcl"]
    }

    fn language(&self) -> Language {
        tree_sitter_hcl::LANGUAGE.into()
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

        // HCL AST structure: config_file -> body -> blocks
        let mut root_cursor = root.walk();
        let body_node = root.children(&mut root_cursor).find(|n| n.kind() == "body");

        if let Some(body) = body_node {
            let mut cursor = body.walk();

            for node in body.children(&mut cursor) {
                if node.kind() == "block" {
                    let mut block_type = None;
                    let mut block_name = None;

                    let mut block_cursor = node.walk();
                    for (idx, child) in node.children(&mut block_cursor).enumerate() {
                        if idx == 0 && child.kind() == "identifier" {
                            if let Ok(text) = child.utf8_text(content) {
                                block_type = Some(text.to_string());
                            }
                        } else if child.kind() == "string_lit"
                            && block_name.is_none()
                            && let Ok(text) = child.utf8_text(content)
                        {
                            block_name = Some(text.trim_matches('"').to_string());
                        }
                    }

                    // Create scope for this block
                    if let (Some(bt), Some(name)) = (block_type, block_name) {
                        let start = node.start_position();
                        let end = node.end_position();

                        scopes.push(Scope {
                            id: ScopeId::new(0), // Will be reassigned by link_nested_scopes
                            scope_type: bt,
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
            }
        }

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
