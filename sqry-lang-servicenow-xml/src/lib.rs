//! `ServiceNow` XML record extraction with JS delegation for sqry.
//!
//! Parses `ServiceNow` update set XML files, extracts embedded JavaScript
//! from CDATA sections, and delegates to `ServiceNowGraphBuilder` for
//! full code analysis. Also indexes table schema definitions.

pub mod detection;
pub mod extraction;
pub mod metadata;
mod relations;
pub mod replay;

pub use relations::ServiceNowXmlGraphBuilder;

use sqry_core::ast::Scope;
use sqry_core::plugin::error::{ParseError, ScopeError};
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin};
use std::path::Path;
use tree_sitter::{Parser, Tree};

/// `ServiceNow` XML plugin for sqry.
#[derive(Debug)]
pub struct ServiceNowXmlPlugin {
    graph_builder: ServiceNowXmlGraphBuilder,
}

impl ServiceNowXmlPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: ServiceNowXmlGraphBuilder,
        }
    }
}

impl Default for ServiceNowXmlPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for ServiceNowXmlPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "servicenow-xml",
            name: "ServiceNow XML",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "ServiceNow XML record extraction with JS delegation",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["xml"]
    }

    fn language(&self) -> tree_sitter::Language {
        tree_sitter_html::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        // tree-sitter-html dummy tree. Error-tolerant, always produces a tree.
        // This tree is NOT used by build_graph(). See design doc RT-8.
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_html::LANGUAGE.into())
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
    fn test_metadata() {
        let plugin = ServiceNowXmlPlugin::new();
        let m = plugin.metadata();
        assert_eq!(m.id, "servicenow-xml");
        assert_eq!(m.name, "ServiceNow XML");
    }

    #[test]
    fn test_extensions() {
        let plugin = ServiceNowXmlPlugin::new();
        assert_eq!(plugin.extensions(), &["xml"]);
    }

    #[test]
    fn test_parse_ast_produces_tree() {
        let plugin = ServiceNowXmlPlugin::new();
        let tree = plugin.parse_ast(b"<root/>");
        assert!(tree.is_ok());
    }

    #[test]
    fn test_extract_scopes_empty() {
        let plugin = ServiceNowXmlPlugin::new();
        let tree = plugin.parse_ast(b"<root/>").unwrap();
        let scopes = plugin
            .extract_scopes(&tree, b"<root/>", Path::new("test.xml"))
            .unwrap();
        assert!(scopes.is_empty());
    }

    #[test]
    fn test_graph_builder_present() {
        let plugin = ServiceNowXmlPlugin::new();
        assert!(plugin.graph_builder().is_some());
    }

    #[test]
    fn test_default() {
        let plugin = ServiceNowXmlPlugin::default();
        assert_eq!(plugin.metadata().id, "servicenow-xml");
    }

    #[test]
    fn test_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ServiceNowXmlPlugin>();
    }
}
