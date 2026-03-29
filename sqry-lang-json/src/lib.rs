//! JSON config file language plugin for sqry.
//!
//! Provides graph-native extraction for JSON configuration files with
//! format-specific profiles for `now-ui.json` and `package.json`.

mod profiles;
mod relations;

pub use relations::JsonGraphBuilder;

use sqry_core::ast::Scope;
use sqry_core::plugin::error::{ParseError, ScopeError};
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin};
use std::path::Path;
use tree_sitter::{Language, Parser, Tree};

/// JSON config file language plugin.
pub struct JsonPlugin {
    graph_builder: JsonGraphBuilder,
}

impl JsonPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: JsonGraphBuilder,
        }
    }
}

impl Default for JsonPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for JsonPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "json",
            name: "JSON",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "JSON config file support with format-specific profiles",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json"]
    }

    fn language(&self) -> Language {
        tree_sitter_json::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_json::LANGUAGE.into())
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
