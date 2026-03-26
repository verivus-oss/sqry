//! Rust language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for Rust, providing:
//! - Graph-native node/edge extraction via `RustGraphBuilder`
//! - AST parsing with tree-sitter
//! - Scope extraction
//!
//! This is the first plugin implementation (dogfooding) to validate the plugin system design.
//!
//! # P3 Features (in development)
//!
//! - Lifetime constraint edges
//! - Trait method binding resolution
//! - Macro expansion tracking
//! - Confidence indicators

pub mod confidence;
pub mod lifetime_extractor;
pub mod macro_expander;
pub mod module_resolver;
pub mod proc_macro_detector;
pub mod ra_bridge;
pub mod relations;
pub mod trait_binder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

/// Rust language plugin
///
/// Provides language support for Rust source files (.rs).
///
/// # Example
///
/// ```
/// use sqry_lang_rust::RustPlugin;
/// use sqry_core::plugin::LanguagePlugin;
///
/// let plugin = RustPlugin::default();
/// let metadata = plugin.metadata();
/// assert_eq!(metadata.id, "rust");
/// assert_eq!(metadata.name, "Rust");
/// ```
pub struct RustPlugin {
    graph_builder: relations::RustGraphBuilder,
}

impl RustPlugin {
    /// Create a new Rust plugin with default graph builder settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: relations::RustGraphBuilder::default(),
        }
    }
}

impl Default for RustPlugin {
    fn default() -> Self {
        Self::new()
    }
}

struct ScopeCapture {
    scope_type: String,
    scope_name: String,
    start: tree_sitter::Point,
    end: tree_sitter::Point,
}

impl LanguagePlugin for RustPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "rust",
            name: "Rust",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Rust language support for sqry",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn language(&self) -> Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let language = self.language();

        parser.set_language(&language).map_err(|e| {
            ParseError::LanguageSetFailed(format!("Failed to set Rust language: {e}"))
        })?;

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
        Self::extract_rust_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl RustPlugin {
    /// Extract scope information from Rust code
    ///
    /// Extracts named scopes (functions, impl blocks, modules, traits, structs, enums).
    /// Returns vector of top-level scopes with nested scopes linked via parent pointers.
    fn extract_rust_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language = tree_sitter_rust::LANGUAGE.into();

        let scope_query = Self::scope_query_source();

        // Compile query
        let query = Query::new(&language, scope_query)
            .map_err(|e| ScopeError::QueryCompilationFailed(e.to_string()))?;

        let mut scopes = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut query_matches = cursor.matches(&query, root_node, content);

        // Extract scopes from query matches
        while let Some(m) = query_matches.next() {
            if let Some(scope) = Self::scope_from_match(&query, m, content, file_path) {
                scopes.push(scope);
            }
        }

        // Sort scopes by start position (required by link_nested_scopes)
        scopes.sort_by_key(|s| (s.start_line, s.start_column));

        // Build parent-child relationships
        link_nested_scopes(&mut scopes);

        Ok(scopes)
    }

    fn scope_from_match(
        query: &Query,
        match_: &tree_sitter::QueryMatch<'_, '_>,
        content: &[u8],
        file_path: &Path,
    ) -> Option<Scope> {
        let capture = Self::scope_capture_from_match(query, match_, content)?;
        Some(Self::build_scope_from_capture(capture, file_path))
    }

    fn scope_capture_from_match(
        query: &Query,
        match_: &tree_sitter::QueryMatch<'_, '_>,
        content: &[u8],
    ) -> Option<ScopeCapture> {
        let mut scope_type = None;
        let mut scope_name = None;
        let mut scope_start = None;
        let mut scope_end = None;

        for capture in match_.captures {
            let capture_name = query.capture_names()[capture.index as usize];
            let node = capture.node;
            let capture_extension = std::path::Path::new(capture_name)
                .extension()
                .and_then(|ext| ext.to_str());

            if capture_extension.is_some_and(|ext| ext.eq_ignore_ascii_case("type")) {
                scope_type = Some(capture_name.trim_end_matches(".type").to_string());
                scope_start = Some(node.start_position());
                scope_end = Some(node.end_position());
            } else if capture_extension.is_some_and(|ext| ext.eq_ignore_ascii_case("name")) {
                scope_name = node
                    .utf8_text(content)
                    .ok()
                    .map(std::string::ToString::to_string);
            }
        }

        let stype = scope_type?;
        let sname = scope_name?;
        let start = scope_start?;
        let end = scope_end?;

        Some(ScopeCapture {
            scope_type: stype,
            scope_name: sname,
            start,
            end,
        })
    }

    fn build_scope_from_capture(capture: ScopeCapture, file_path: &Path) -> Scope {
        Scope {
            id: ScopeId::new(0), // Temporary ID, will be reassigned by link_nested_scopes
            scope_type: capture.scope_type,
            name: capture.scope_name,
            file_path: file_path.to_path_buf(),
            start_line: capture.start.row + 1, // tree-sitter is 0-based, we use 1-based
            start_column: capture.start.column,
            end_line: capture.end.row + 1,
            end_column: capture.end.column,
            parent_id: None, // Will be set by link_nested_scopes
        }
    }

    /// Returns tree-sitter query source for scope extraction.
    ///
    /// Extracts 6 Rust scope types:
    /// 1. Functions
    /// 2. Impl blocks
    /// 3. Traits
    /// 4. Modules
    /// 5. Structs (field scope)
    /// 6. Enums (variant scope)
    fn scope_query_source() -> &'static str {
        r"
; Function scopes
(function_item
  name: (identifier) @function.name
) @function.type

; Impl block scopes
(impl_item
  type: (type_identifier) @impl.name
) @impl.type

; Trait scopes
(trait_item
  name: (type_identifier) @trait.name
) @trait.type

; Module scopes
(mod_item
  name: (identifier) @module.name
) @module.type

; Struct scopes
(struct_item
  name: (type_identifier) @struct.name
) @struct.type

; Enum scopes
(enum_item
  name: (type_identifier) @enum.name
) @enum.type
"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata() {
        let plugin = RustPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "rust");
        assert_eq!(metadata.name, "Rust");
        assert_eq!(metadata.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(metadata.author, "Verivus Pty Ltd");
        assert_eq!(metadata.tree_sitter_version, "0.24");
    }

    #[test]
    fn test_extensions() {
        let plugin = RustPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 1);
        assert_eq!(extensions[0], "rs");
    }

    #[test]
    fn test_language() {
        let plugin = RustPlugin::default();
        let language = plugin.language();

        // Just verify we can get a language (ABI version should be non-zero)
        assert!(language.abi_version() > 0);
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = RustPlugin::default();
        let source = b"fn main() {}";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }
}
