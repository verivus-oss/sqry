pub mod relations;

pub use relations::SvelteGraphBuilder;

use sqry_core::ast::Scope;
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin, SafeParser,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use tree_sitter::Tree;

/// Svelte language plugin.
pub struct SveltePlugin {
    graph_builder: SvelteGraphBuilder,
}

impl SveltePlugin {
    /// Creates a new Svelte plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: SvelteGraphBuilder::default(),
        }
    }
}

impl Default for SveltePlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for SveltePlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "svelte",
            name: "Svelte",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Svelte.js Single-File Component support for sqry",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["svelte"]
    }

    fn language(&self) -> tree_sitter::Language {
        tree_sitter_svelte_sqry::language()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let parser = SafeParser::with_defaults();
        parser.parse(&self.language(), content, None)
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
        let plugin = SveltePlugin::default();
        let metadata = plugin.metadata();
        assert_eq!(metadata.id, "svelte");
        assert_eq!(metadata.name, "Svelte");
    }

    #[test]
    fn test_extensions() {
        let plugin = SveltePlugin::default();
        assert_eq!(plugin.extensions(), &["svelte"]);
    }

    #[test]
    fn test_can_parse() {
        let plugin = SveltePlugin::default();
        let content = b"<template><div>Hello</div></template>";
        let tree = plugin.parse_ast(content);
        assert!(tree.is_ok());
    }
}
