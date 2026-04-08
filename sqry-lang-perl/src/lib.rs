//! Perl language plugin.
//!
//! Provides graph-native extraction via `PerlGraphBuilder`, AST parsing,
//! and scope extraction for Perl source files.

mod preprocess;
pub mod relations;

pub use relations::PerlGraphBuilder;

use preprocess::preprocess_content;
use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::error::ScopeError;
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin};
use std::borrow::Cow;
use std::path::Path;
use tree_sitter::{Language, Query, QueryCursor, StreamingIterator, Tree};

const LANGUAGE_ID: &str = "perl";
const LANGUAGE_NAME: &str = "Perl";
const TREE_SITTER_VERSION: &str = "0.23";

/// Perl language plugin implementation.
pub struct PerlPlugin {
    graph_builder: PerlGraphBuilder,
}

impl PerlPlugin {
    /// Creates a new Perl plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: PerlGraphBuilder,
        }
    }
}

impl Default for PerlPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for PerlPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: LANGUAGE_ID,
            name: LANGUAGE_NAME,
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Perl language support for sqry",
            tree_sitter_version: TREE_SITTER_VERSION,
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["pl", "pm", "t"]
    }

    fn language(&self) -> Language {
        tree_sitter_perl_sqry::language()
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
        extract_perl_scopes(tree, processed.as_ref(), file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

/// Extract scopes from Perl source using tree-sitter queries.
fn extract_perl_scopes(
    tree: &Tree,
    content: &[u8],
    file_path: &Path,
) -> Result<Vec<Scope>, ScopeError> {
    let root_node = tree.root_node();
    let language = tree_sitter_perl_sqry::language();

    // Perl scope query: packages, classes, subroutines, methods.
    let scope_query = r"
; Package statements (namespace scopes)
(package_statement
  name: (_) @namespace.name
) @namespace.type

; Class statements (Moose/Moo style)
(class_statement
  name: (_) @class.name
) @class.type

; Role statements
(role_statement
  name: (_) @role.name
) @role.type

; Subroutine declarations
(subroutine_declaration_statement
  name: (_) @function.name
) @function.type

; Method declarations (Moose/Moo style)
(method_declaration_statement
  name: (_) @method.name
) @method.type
";

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

            if let Some((prefix, suffix)) = capture_name.rsplit_once('.') {
                match suffix {
                    "type" => {
                        scope_type = Some(prefix.to_string());
                        scope_start = Some(node.start_position());
                        scope_end = Some(node.end_position());
                    }
                    "name" => {
                        scope_name = node.utf8_text(content).ok().map(clean_identifier);
                    }
                    _ => {}
                }
            }
        }

        if let (Some(stype), Some(sname), Some(start), Some(end)) =
            (scope_type, scope_name, scope_start, scope_end)
        {
            let normalized_type = match stype.as_str() {
                "namespace" => "namespace",
                "class" | "role" => "class",
                "function" | "method" => "function",
                other => other,
            };

            let scope = Scope {
                id: ScopeId::new(0),
                scope_type: normalized_type.to_string(),
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

fn clean_identifier(raw: &str) -> String {
    raw.trim_matches(|c: char| c == '\'' || c == '"')
        .trim_matches(|c: char| c == ';')
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn load_fixture(name: &str) -> (Vec<u8>, PathBuf) {
        let path = PathBuf::from(format!("tests/fixtures/{name}"));
        let content = fs::read(&path).expect("failed to read fixture");
        (content, path)
    }

    #[test]
    fn test_plugin_metadata() {
        let plugin = PerlPlugin::default();
        let metadata = plugin.metadata();
        assert_eq!(metadata.id, "perl");
        assert_eq!(metadata.name, "Perl");
    }

    #[test]
    fn test_extensions() {
        let plugin = PerlPlugin::default();
        assert_eq!(plugin.extensions(), &["pl", "pm", "t"]);
    }

    #[test]
    fn test_can_parse() {
        let plugin = PerlPlugin::default();
        let content = b"package Example; sub foo { return 1; }";
        let tree = plugin.parse_ast(content);
        assert!(tree.is_ok());
    }

    #[test]
    fn test_extract_scopes_from_fixture() {
        let plugin = PerlPlugin::default();
        let (content, path) = load_fixture("basic.pl");
        let tree = plugin.parse_ast(&content).expect("parse fixture");
        let scopes = plugin
            .extract_scopes(&tree, &content, &path)
            .expect("extract scopes");

        assert!(
            scopes
                .iter()
                .any(|s| s.name == "Example::App" && s.scope_type == "namespace"),
            "package scope should be extracted"
        );
        assert!(
            scopes
                .iter()
                .any(|s| s.name == "foo" && s.scope_type == "function"),
            "subroutine scope should be extracted"
        );
        assert!(
            scopes
                .iter()
                .any(|s| s.name == "bar" && s.scope_type == "function"),
            "method scope should be extracted"
        );
    }

    #[test]
    fn test_pod_is_ignored_for_scopes() {
        let plugin = PerlPlugin::default();
        let (content, path) = load_fixture("pod.pl");
        let tree = plugin.parse_ast(&content).expect("parse fixture");
        let scopes = plugin
            .extract_scopes(&tree, &content, &path)
            .expect("extract scopes");

        assert!(
            scopes
                .iter()
                .any(|s| s.name == "pod_sub" && s.scope_type == "function"),
            "pod_sub scope should be extracted"
        );
    }
}
