//! Haskell language plugin.
//!
//! Provides graph-native extraction via `HaskellGraphBuilder`, AST parsing,
//! scope extraction, and literate (`.lhs`) preprocessing.

mod preprocess;
pub mod relations;

pub use relations::HaskellGraphBuilder;

use preprocess::preprocess_content;
use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::LanguageMetadata;
use sqry_core::plugin::LanguagePlugin;
use sqry_core::plugin::error::ScopeError;
use std::borrow::Cow;
use std::path::Path;
use tree_sitter::{Language, Node, Tree};

const LANGUAGE_ID: &str = "haskell";
const LANGUAGE_NAME: &str = "Haskell";
const TREE_SITTER_VERSION: &str = "0.23";

/// Haskell language plugin implementation.
pub struct HaskellPlugin {
    graph_builder: HaskellGraphBuilder,
}

impl HaskellPlugin {
    /// Creates a new Haskell plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: HaskellGraphBuilder::default(),
        }
    }
}

impl Default for HaskellPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for HaskellPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: LANGUAGE_ID,
            name: LANGUAGE_NAME,
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Haskell language support for sqry",
            tree_sitter_version: TREE_SITTER_VERSION,
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["hs", "lhs", "hs-boot"]
    }

    fn language(&self) -> Language {
        tree_sitter_haskell::LANGUAGE.into()
    }

    fn preprocess<'a>(&self, content: &'a [u8]) -> Cow<'a, [u8]> {
        preprocess_content(content)
    }

    fn extract_scopes(
        &self,
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let processed = self.preprocess(content);
        Ok(extract_haskell_scopes(tree, processed.as_ref(), file_path))
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

/// Extract scopes from Haskell source using AST traversal.
fn extract_haskell_scopes(tree: &Tree, content: &[u8], file_path: &Path) -> Vec<Scope> {
    let mut scopes = Vec::new();
    let root = tree.root_node();

    let mut root_cursor = root.walk();
    for child in root.children(&mut root_cursor) {
        if child.kind() == "header" {
            if let Some(module_name) = extract_module_name_from_header(child, content) {
                let start = child.start_position();
                let end = root.end_position();
                scopes.push(Scope {
                    id: ScopeId::new(0),
                    scope_type: "module".to_string(),
                    name: module_name,
                    file_path: file_path.to_path_buf(),
                    start_line: start.row + 1,
                    start_column: start.column,
                    end_line: end.row + 1,
                    end_column: end.column,
                    parent_id: None,
                });
            }
            break;
        }
    }

    if let Some(decls) = root.child_by_field_name("declarations") {
        collect_declaration_scopes(decls, content, file_path, &mut scopes);
    }

    scopes.sort_by_key(|s| (s.start_line, s.start_column));
    link_nested_scopes(&mut scopes);
    scopes
}

fn collect_declaration_scopes(
    node: Node<'_>,
    content: &[u8],
    file_path: &Path,
    scopes: &mut Vec<Scope>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        let (scope_type, name_field) = match child.kind() {
            "function" | "bind" => ("function", Some("name")),
            "data_type" | "newtype" | "type_synomym" => ("type", Some("name")),
            "class" => ("class", Some("name")),
            "instance" => ("instance", Some("name")),
            "pattern_synonym" => ("function", Some("synonym")),
            _ => continue,
        };

        let name = name_field
            .and_then(|field| child.child_by_field_name(field))
            .and_then(|n| n.utf8_text(content).ok())
            .map_or_else(|| format!("<{}>", child.kind()), |s| s.trim().to_string());

        let start = child.start_position();
        let end = child.end_position();

        scopes.push(Scope {
            id: ScopeId::new(0),
            scope_type: scope_type.to_string(),
            name,
            file_path: file_path.to_path_buf(),
            start_line: start.row + 1,
            start_column: start.column,
            end_line: end.row + 1,
            end_column: end.column,
            parent_id: None,
        });
    }
}

fn extract_module_name_from_header(header: Node<'_>, content: &[u8]) -> Option<String> {
    let mut cursor = header.walk();
    for child in header.children(&mut cursor) {
        if matches!(child.kind(), "module" | "module_id")
            && let Ok(text) = child.utf8_text(content)
            && text != "module"
        {
            return Some(text.to_string());
        }
    }
    header
        .utf8_text(content)
        .ok()
        .and_then(parse_module_name_from_text)
}

fn parse_module_name_from_text(text: &str) -> Option<String> {
    let mut tokens = text.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "module"
            && let Some(name_token) = tokens.next()
        {
            let trimmed = name_token.trim_end_matches(['(', ';']);
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::plugin::LanguagePlugin;
    use std::fs;
    use std::path::PathBuf;

    fn load_fixture(name: &str) -> (Vec<u8>, PathBuf) {
        let path = PathBuf::from(format!("tests/fixtures/{name}"));
        let content = fs::read(&path).expect("failed to read fixture");
        (content, path)
    }

    fn extract_scopes_from_fixture(plugin: &HaskellPlugin, name: &str) -> Vec<Scope> {
        let (content, path) = load_fixture(name);
        let tree = plugin.parse_ast(&content).expect("parse fixture");
        plugin
            .extract_scopes(&tree, &content, &path)
            .expect("extract scopes")
    }

    fn has_scope(scopes: &[Scope], scope_type: &str, name: &str) -> bool {
        scopes
            .iter()
            .any(|scope| scope.scope_type == scope_type && scope.name == name)
    }

    #[test]
    fn extracts_scopes_from_basic_fixture() {
        let plugin = HaskellPlugin::default();
        let scopes = extract_scopes_from_fixture(&plugin, "basic.hs");

        assert!(has_scope(&scopes, "module", "Sample"));
        assert!(has_scope(&scopes, "function", "foo"));
        assert!(has_scope(&scopes, "function", "bar"));
        assert!(has_scope(&scopes, "class", "Run"));
    }

    #[test]
    fn parses_literate_haskell() {
        let plugin = HaskellPlugin::default();
        let scopes = extract_scopes_from_fixture(&plugin, "literate.lhs");

        assert!(has_scope(&scopes, "module", "Literate"));
        assert!(has_scope(&scopes, "function", "answer"));
    }
}
