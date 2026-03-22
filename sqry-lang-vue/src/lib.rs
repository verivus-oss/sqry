//! Vue language plugin for sqry.
//!
//! Provides AST parsing, scope extraction stubs, and graph building for
//! Vue Single-File Components.

pub mod relations;

pub use relations::VueGraphBuilder;

use sqry_core::ast::Scope;
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use tree_sitter::{Parser, Tree};

/// Vue language plugin.
pub struct VuePlugin {
    graph_builder: VueGraphBuilder,
}

impl VuePlugin {
    /// Creates a new Vue plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: VueGraphBuilder::default(),
        }
    }
}

impl Default for VuePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for VuePlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "vue",
            name: "Vue",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Vue.js Single-File Component support for sqry",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["vue"]
    }

    fn language(&self) -> tree_sitter::Language {
        tree_sitter_vue_sqry::language()
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
        _tree: &Tree,
        _content: &[u8],
        _file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        Ok(Vec::new())
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = VuePlugin::default();
        let metadata = plugin.metadata();
        assert_eq!(metadata.id, "vue");
        assert_eq!(metadata.name, "Vue");
    }

    #[test]
    fn test_extensions() {
        let plugin = VuePlugin::default();
        assert_eq!(plugin.extensions(), &["vue"]);
    }

    #[test]
    fn test_can_parse() {
        let plugin = VuePlugin::default();
        let content = b"<template><div>Hello</div></template>";
        let tree = plugin.parse_ast(content);
        assert!(tree.is_ok());
    }
}
