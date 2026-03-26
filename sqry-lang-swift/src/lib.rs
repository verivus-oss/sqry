//! Swift language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for Swift, providing:
//! - AST parsing with tree-sitter
//! - Scope extraction
//! - Graph builder for call graph extraction (Tier-2)
//!
//! # Supported Features
//!
//! - Classes (`class`, `actor`)
//! - Structs (`struct`)
//! - Enums (`enum`)
//! - Protocols (`protocol`)
//! - Functions (`func`, including async, throwing, static, mutating)
//! - Properties (`var`, `let` - stored and computed)
//! - Extensions (`extension`)
//! - Generic type parameters
//! - Visibility modifiers (open, public, internal, fileprivate, private)
//! - Async/await support (Swift 5.5+)
//! - Error handling (throws)
//! - Swift ↔ Objective-C bridging detection

pub mod relations;

// Re-export graph builder types for testing
pub use relations::{BridgingHeaderLocator, SwiftBridgingIndex, SwiftGraphBuilder};

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

/// Swift language plugin
///
/// Provides language support for Swift source files (.swift).
pub struct SwiftPlugin {
    graph_builder: SwiftGraphBuilder,
}

impl SwiftPlugin {
    /// Create a new Swift plugin instance
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: SwiftGraphBuilder::default(),
        }
    }
}

impl Default for SwiftPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for SwiftPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "swift",
            name: "Swift",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Swift language support for sqry",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["swift"]
    }

    fn language(&self) -> Language {
        tree_sitter_swift::LANGUAGE.into()
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
        Self::extract_swift_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl SwiftPlugin {
    /// Extract scope information from Swift code
    fn extract_swift_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language = tree_sitter_swift::LANGUAGE.into();

        let scope_query = Self::scope_query_source();

        let query = Query::new(&language, scope_query)
            .map_err(|e| ScopeError::QueryCompilationFailed(e.to_string()))?;

        let mut scopes = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut query_matches = cursor.matches(&query, root_node, content);

        while let Some(m) = query_matches.next() {
            let mut scope_type = None;
            let mut scope_name = None;
            let mut scope_start = None;
            let mut scope_end = None;

            for capture in m.captures {
                let capture_name = query.capture_names()[capture.index as usize];
                let node = capture.node;

                if let Some((prefix, ext)) = capture_name.rsplit_once('.') {
                    if ext.eq_ignore_ascii_case("type") {
                        scope_type = Some(prefix.to_string());
                        scope_start = Some(node.start_position());
                        scope_end = Some(node.end_position());
                    } else if ext.eq_ignore_ascii_case("name") {
                        scope_name = node
                            .utf8_text(content)
                            .ok()
                            .map(std::string::ToString::to_string);
                    }
                }
            }

            if let (Some(stype), Some(sname), Some(start), Some(end)) =
                (scope_type, scope_name, scope_start, scope_end)
            {
                let scope = Scope {
                    id: ScopeId::new(0),
                    scope_type: stype,
                    name: sname,
                    file_path: file_path.to_path_buf(),
                    start_line: start.row + 1,
                    start_column: start.column,
                    end_line: end.row + 1,
                    end_column: end.column,
                    parent_id: None,
                };
                scopes.push(scope);
            }
        }

        scopes.sort_by_key(|s| (s.start_line, s.start_column));

        link_nested_scopes(&mut scopes);

        Ok(scopes)
    }

    /// Returns tree-sitter query source for scope extraction
    fn scope_query_source() -> &'static str {
        r"
; Function scopes
(function_declaration
  name: (simple_identifier) @function.name
) @function.type

; Class scopes (includes class, struct, enum, extension, actor)
(class_declaration
  name: (type_identifier) @class.name
) @class.type

; Protocol scopes
(protocol_declaration
  name: (type_identifier) @protocol.name
) @protocol.type
"
    }
}
