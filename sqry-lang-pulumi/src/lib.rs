//! Pulumi YAML/JSON language plugin for sqry.
//!
//! Provides graph-native extraction for Pulumi stack files.

mod relations;

pub use relations::PulumiGraphBuilder;

use anyhow::Result;
use sqry_core::ast::Scope;
use sqry_core::plugin::error::{ParseError, ScopeError};
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin};
use std::path::Path;
use tree_sitter::{Language, Parser, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PulumiFormat {
    Json,
    Yaml,
}

pub(crate) fn detect_format(content: &[u8]) -> PulumiFormat {
    let first_non_ws = content
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace());

    match first_non_ws {
        Some(b'{' | b'[') => PulumiFormat::Json,
        _ => PulumiFormat::Yaml,
    }
}

fn parser_language(format: PulumiFormat) -> Language {
    match format {
        PulumiFormat::Json => tree_sitter_json::LANGUAGE.into(),
        PulumiFormat::Yaml => tree_sitter_yaml::language(),
    }
}

/// Pulumi YAML/JSON language plugin
pub struct PulumiPlugin {
    graph_builder: PulumiGraphBuilder,
}

impl PulumiPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: PulumiGraphBuilder,
        }
    }
}

impl Default for PulumiPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for PulumiPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "pulumi",
            name: "Pulumi",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Pulumi YAML/JSON language support with graph-native extraction",
            tree_sitter_version: "0.6/0.23",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pulumi.yaml", "pulumi.yml", "pulumi.json"]
    }

    fn language(&self) -> Language {
        tree_sitter_yaml::language()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let format = detect_format(content);
        let mut parser = Parser::new();
        let lang = parser_language(format);
        parser
            .set_language(&lang)
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
