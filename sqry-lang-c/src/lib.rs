//! C language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for C, providing:
//! - AST parsing with tree-sitter
//! - Scope extraction
//! - Relation extraction via `CGraphBuilder` (calls, imports, exports, OOP edges)

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

/// Relation tracking for C (call graphs, imports, exports).
pub mod relations;

pub use relations::CGraphBuilder;

/// C language plugin
///
/// Provides language support for C source files (.c, .h).
pub struct CPlugin {
    graph_builder: relations::CGraphBuilder,
}

impl CPlugin {
    /// Create a new C plugin instance.
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: relations::CGraphBuilder::default(),
        }
    }

    /// Test/bench-only constructor whose graph builder skips the three Phase A
    /// C-plugin instrumentation walks (the callsite capture and the core
    /// pass5b resolution still run). See
    /// [`relations::CGraphBuilder::without_phase_a`]. Gated behind
    /// `cfg(any(test, feature = "phase-a-toggle"))` so release builds cannot
    /// construct it; used only by `scripts/measure/check_phase_a_perf_gate.sh`
    /// to measure the marginal build-time cost of the instrumentation walks at
    /// a single commit.
    #[cfg(any(test, feature = "phase-a-toggle"))]
    #[must_use]
    pub fn without_phase_a() -> Self {
        Self {
            graph_builder: relations::CGraphBuilder::without_phase_a(),
        }
    }
}

impl Default for CPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for CPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "c",
            name: "C",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "C language support for sqry - systems programming code search",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["c", "h"]
    }

    fn language(&self) -> Language {
        tree_sitter_c::LANGUAGE.into()
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
        Self::extract_c_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl CPlugin {
    /// Tree-sitter query for C scope extraction.
    fn scope_query_source() -> &'static str {
        r"
; Function definitions (with body)
(function_definition
    declarator: (function_declarator
        declarator: (identifier) @function.name)
    body: (compound_statement) @function.body) @function.type

; Function definitions with pointer return type
(function_definition
    declarator: (pointer_declarator
        declarator: (function_declarator
            declarator: (identifier) @function.name))
    body: (compound_statement) @function.body) @function.type

; Struct definitions with body
(struct_specifier
    name: (type_identifier) @struct.name
    body: (field_declaration_list)) @struct.type

; Enum definitions with body
(enum_specifier
    name: (type_identifier) @enum.name
    body: (enumerator_list)) @enum.type

; Union definitions with body
(union_specifier
    name: (type_identifier) @union.name
    body: (field_declaration_list)) @union.type
"
    }

    /// Extract scopes from C source using tree-sitter queries.
    fn extract_c_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language: Language = tree_sitter_c::LANGUAGE.into();
        let scope_query = Self::scope_query_source();

        let query = Query::new(&language, scope_query)
            .map_err(|e| ScopeError::QueryCompilationFailed(e.to_string()))?;

        let mut scopes = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut query_matches = cursor.matches(&query, root_node, content);

        while let Some(m) = query_matches.next() {
            let mut scope_type: Option<&str> = None;
            let mut scope_name: Option<String> = None;
            let mut type_node: Option<tree_sitter::Node> = None;

            for capture in m.captures {
                let capture_name = query.capture_names()[capture.index as usize];
                match capture_name {
                    "function.type" | "struct.type" | "enum.type" | "union.type" => {
                        scope_type = Some(capture_name.split('.').next().unwrap_or("unknown"));
                        type_node = Some(capture.node);
                    }
                    "function.name" | "struct.name" | "enum.name" | "union.name" => {
                        scope_name = capture.node.utf8_text(content).ok().map(String::from);
                    }
                    _ => {}
                }
            }

            if let (Some(scope_type_str), Some(name), Some(node)) =
                (scope_type, scope_name, type_node)
            {
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                scopes.push(Scope {
                    id: ScopeId::new(0),
                    name,
                    scope_type: scope_type_str.to_string(),
                    file_path: file_path.to_path_buf(),
                    start_line: start_pos.row + 1,
                    start_column: start_pos.column,
                    end_line: end_pos.row + 1,
                    end_column: end_pos.column,
                    parent_id: None,
                });
            }
        }

        // Sort by position and link nested scopes
        scopes.sort_by_key(|s| (s.start_line, s.start_column));
        link_nested_scopes(&mut scopes);

        Ok(scopes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata() {
        let plugin = CPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "c");
        assert_eq!(metadata.name, "C");
        assert_eq!(metadata.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(metadata.author, "Verivus Pty Ltd");
        assert_eq!(metadata.tree_sitter_version, "0.24");
    }

    #[test]
    fn test_extensions() {
        let plugin = CPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 2);
        assert_eq!(extensions[0], "c");
        assert_eq!(extensions[1], "h");
    }

    #[test]
    fn test_language() {
        let plugin = CPlugin::default();
        let language = plugin.language();

        assert!(language.abi_version() > 0);
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = CPlugin::default();
        let source = b"int main(void) { return 0; }";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_plugin_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CPlugin>();
    }

    #[test]
    fn test_extract_scopes_functions() {
        let plugin = CPlugin::default();
        let source = br"
void foo(void) {
    int x = 1;
}

int main(int argc, char **argv) {
    foo();
    return 0;
}
";
        let path = std::path::Path::new("test.c");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        assert_eq!(
            scopes.len(),
            2,
            "Expected 2 function scopes, got {}",
            scopes.len()
        );

        let scope_names: Vec<&str> = scopes.iter().map(|s| s.name.as_str()).collect();
        assert!(scope_names.contains(&"foo"), "Missing 'foo' scope");
        assert!(scope_names.contains(&"main"), "Missing 'main' scope");

        for scope in &scopes {
            assert_eq!(scope.scope_type, "function", "Expected function scope type");
        }
    }

    #[test]
    fn test_extract_scopes_struct() {
        let plugin = CPlugin::default();
        let source = br"
struct Point {
    int x;
    int y;
};

void init_point(struct Point *p) {
    p->x = 0;
    p->y = 0;
}
";
        let path = std::path::Path::new("test.c");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        assert_eq!(scopes.len(), 2, "Expected 2 scopes, got {}", scopes.len());

        let struct_scope = scopes.iter().find(|s| s.name == "Point");
        let func_scope = scopes.iter().find(|s| s.name == "init_point");

        assert!(struct_scope.is_some(), "Missing 'Point' struct scope");
        assert!(func_scope.is_some(), "Missing 'init_point' function scope");

        assert_eq!(struct_scope.unwrap().scope_type, "struct");
        assert_eq!(func_scope.unwrap().scope_type, "function");
    }

    #[test]
    fn test_extract_scopes_pointer_return() {
        let plugin = CPlugin::default();
        let source = br"
int *get_value(int *ptr) {
    return ptr;
}
";
        let path = std::path::Path::new("test.c");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        assert_eq!(
            scopes.len(),
            1,
            "Expected 1 function scope, got {}",
            scopes.len()
        );
        assert_eq!(scopes[0].name, "get_value");
        assert_eq!(scopes[0].scope_type, "function");
    }

    #[test]
    fn test_extract_scopes_enum() {
        let plugin = CPlugin::default();
        let source = br"
enum Color {
    Red,
    Green,
    Blue
};
";
        let path = std::path::Path::new("test.c");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        assert_eq!(
            scopes.len(),
            1,
            "Expected 1 enum scope, got {}",
            scopes.len()
        );
        assert_eq!(scopes[0].name, "Color");
        assert_eq!(scopes[0].scope_type, "enum");
    }

    #[test]
    fn test_extract_scopes_union() {
        let plugin = CPlugin::default();
        let source = br"
union Data {
    int i;
    float f;
    char str[20];
};
";
        let path = std::path::Path::new("test.c");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        assert_eq!(
            scopes.len(),
            1,
            "Expected 1 union scope, got {}",
            scopes.len()
        );
        assert_eq!(scopes[0].name, "Data");
        assert_eq!(scopes[0].scope_type, "union");
    }
}
