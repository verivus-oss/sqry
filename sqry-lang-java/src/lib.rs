//! Java language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for Java, providing:
//! - AST parsing with tree-sitter
//! - Scope extraction
//! - Relation extraction via `JavaGraphBuilder` (calls, imports, exports, OOP edges)
//!
//! This plugin enables semantic code search for Java codebases, the #1 priority
//! language for enterprise adoption (90%+ Fortune 500 companies).

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

pub mod relations;

/// Java language plugin
///
/// Provides language support for Java source files (.java).
///
/// # Supported Constructs
///
/// - Classes (`class Foo`)
/// - Interfaces (`interface Bar`)
/// - Methods (instance, static, constructors)
/// - Enums (`enum Color`)
/// - Fields (instance and static fields)
/// - Constants (`static final` fields)
///
/// # Example
///
/// ```
/// use sqry_lang_java::JavaPlugin;
/// use sqry_core::plugin::LanguagePlugin;
///
/// let plugin = JavaPlugin::default();
/// let metadata = plugin.metadata();
/// assert_eq!(metadata.id, "java");
/// assert_eq!(metadata.name, "Java");
/// ```
pub struct JavaPlugin {
    graph_builder: relations::JavaGraphBuilder,
}

impl JavaPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: relations::JavaGraphBuilder::default(),
        }
    }
}

impl Default for JavaPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for JavaPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "java",
            name: "Java",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Java language support for sqry - enterprise code search",
            tree_sitter_version: "0.23",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["java"]
    }

    fn language(&self) -> Language {
        tree_sitter_java::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let language = self.language();

        parser.set_language(&language).map_err(|e| {
            ParseError::LanguageSetFailed(format!("Failed to set Java language: {e}"))
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
        Self::extract_java_scopes(tree, content, file_path)
    }
    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl JavaPlugin {
    /// Tree-sitter query source for Java scope extraction
    fn scope_query_source() -> &'static str {
        r"
; Class declarations with body
(class_declaration
    name: (identifier) @class.name
    body: (class_body)) @class.type

; Interface declarations with body
(interface_declaration
    name: (identifier) @interface.name
    body: (interface_body)) @interface.type

; Enum declarations with body
(enum_declaration
    name: (identifier) @enum.name
    body: (enum_body)) @enum.type

; Method declarations (both concrete and abstract)
(method_declaration
    name: (identifier) @method.name) @method.type

; Constructor declarations with body
(constructor_declaration
    name: (identifier) @constructor.name
    body: (constructor_body)) @constructor.type

; Record declarations (Java 14+)
(record_declaration
    name: (identifier) @record.name
    body: (class_body)) @record.type

; Compact constructor declarations (used in records)
(compact_constructor_declaration
    name: (identifier) @constructor.name
    body: (block)) @constructor.type
"
    }

    /// Extract scopes from Java source code using tree-sitter queries
    fn extract_java_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let language = tree_sitter_java::LANGUAGE.into();
        let scope_query = Self::scope_query_source();

        let query = Query::new(&language, scope_query).map_err(|e| {
            ScopeError::QueryCompilationFailed(format!("Failed to compile Java scope query: {e}"))
        })?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), content);
        let mut scopes = Vec::new();

        while let Some(m) = matches.next() {
            // Find the type and name captures
            let mut scope_type = None;
            let mut scope_name = None;
            let mut scope_node = None;

            for capture in m.captures {
                let capture_name = query.capture_names()[capture.index as usize];
                let capture_extension = std::path::Path::new(capture_name)
                    .extension()
                    .and_then(|ext| ext.to_str());
                if capture_extension.is_some_and(|ext| ext.eq_ignore_ascii_case("type")) {
                    scope_type = Some(capture_name.trim_end_matches(".type"));
                    scope_node = Some(capture.node);
                } else if capture_extension.is_some_and(|ext| ext.eq_ignore_ascii_case("name")) {
                    scope_name = capture.node.utf8_text(content).ok();
                }
            }

            if let (Some(stype), Some(sname), Some(node)) = (scope_type, scope_name, scope_node) {
                let start_pos = node.start_position();
                let end_pos = node.end_position();

                scopes.push(Scope {
                    id: ScopeId::new(0), // Will be assigned by link_nested_scopes
                    name: sname.to_string(),
                    scope_type: stype.to_string(),
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
        scopes.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then(a.start_column.cmp(&b.start_column))
        });

        link_nested_scopes(&mut scopes);

        Ok(scopes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata() {
        let plugin = JavaPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "java");
        assert_eq!(metadata.name, "Java");
        assert_eq!(metadata.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(metadata.author, "Verivus Pty Ltd");
        assert_eq!(metadata.tree_sitter_version, "0.23");
    }

    #[test]
    fn test_extensions() {
        let plugin = JavaPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 1);
        assert_eq!(extensions[0], "java");
    }

    #[test]
    fn test_graph_builder_returns_some() {
        let plugin = JavaPlugin::default();
        assert!(
            plugin.graph_builder().is_some(),
            "JavaPlugin::graph_builder() should return Some"
        );
    }

    #[test]
    fn test_language() {
        let plugin = JavaPlugin::default();
        let language = plugin.language();

        // Just verify we can get a language (ABI version should be non-zero)
        assert!(language.abi_version() > 0);
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = JavaPlugin::default();
        let source = b"class HelloWorld {}";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_plugin_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<JavaPlugin>();
    }

    #[test]
    fn test_extract_scopes_class() {
        let plugin = JavaPlugin::default();
        let source = b"public class MyClass {
    public void myMethod() {
        System.out.println(\"Hello\");
    }
}";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("Test.java"))
            .unwrap();

        // Should find class and method scopes
        assert_eq!(scopes.len(), 2, "Expected 2 scopes, got {scopes:?}");

        // Find class scope
        let class_scope = scopes.iter().find(|s| s.scope_type == "class");
        assert!(class_scope.is_some(), "Should have class scope");
        assert_eq!(class_scope.unwrap().name, "MyClass");

        // Find method scope
        let method_scope = scopes.iter().find(|s| s.scope_type == "method");
        assert!(method_scope.is_some(), "Should have method scope");
        assert_eq!(method_scope.unwrap().name, "myMethod");
    }

    #[test]
    fn test_extract_scopes_interface() {
        let plugin = JavaPlugin::default();
        let source = b"public interface MyInterface {
    void doSomething();
}";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("Test.java"))
            .unwrap();

        // Interface should be captured (method without body won't be)
        assert!(!scopes.is_empty(), "Expected at least 1 scope");

        let interface_scope = scopes.iter().find(|s| s.scope_type == "interface");
        assert!(interface_scope.is_some(), "Should have interface scope");
        assert_eq!(interface_scope.unwrap().name, "MyInterface");
    }

    #[test]
    fn test_extract_scopes_enum() {
        let plugin = JavaPlugin::default();
        let source = b"public enum Color {
    RED, GREEN, BLUE
}";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("Test.java"))
            .unwrap();

        assert!(!scopes.is_empty(), "Expected at least 1 scope");

        let enum_scope = scopes.iter().find(|s| s.scope_type == "enum");
        assert!(enum_scope.is_some(), "Should have enum scope");
        assert_eq!(enum_scope.unwrap().name, "Color");
    }

    #[test]
    fn test_extract_scopes_nested() {
        let plugin = JavaPlugin::default();
        let source = b"public class Outer {
    public class Inner {
        public void innerMethod() {}
    }
}";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("Test.java"))
            .unwrap();

        // Should find outer class, inner class, and method
        assert_eq!(scopes.len(), 3, "Expected 3 scopes, got {scopes:?}");

        // Outer class should have no parent
        let outer = scopes.iter().find(|s| s.name == "Outer");
        assert!(outer.is_some());
        assert!(outer.unwrap().parent_id.is_none());

        // Inner class should have Outer as parent
        let inner = scopes.iter().find(|s| s.name == "Inner");
        assert!(inner.is_some());
        assert!(inner.unwrap().parent_id.is_some());

        // Method should have Inner as parent
        let method = scopes.iter().find(|s| s.name == "innerMethod");
        assert!(method.is_some());
        assert!(method.unwrap().parent_id.is_some());
    }

    #[test]
    fn test_extract_scopes_abstract_methods() {
        let plugin = JavaPlugin::default();
        let source = b"public abstract class Shape {
    public abstract void draw();
    public abstract double area();
}";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("Shape.java"))
            .unwrap();

        // Should find abstract class and both abstract methods
        assert_eq!(scopes.len(), 3, "Expected 3 scopes, got {scopes:?}");

        // Abstract class should be present
        let class = scopes.iter().find(|s| s.name == "Shape");
        assert!(class.is_some(), "Missing 'Shape' class scope");
        assert_eq!(class.unwrap().scope_type, "class");

        // Abstract methods should be present
        let draw = scopes.iter().find(|s| s.name == "draw");
        assert!(draw.is_some(), "Missing 'draw' abstract method scope");
        assert_eq!(draw.unwrap().scope_type, "method");

        let area = scopes.iter().find(|s| s.name == "area");
        assert!(area.is_some(), "Missing 'area' abstract method scope");
        assert_eq!(area.unwrap().scope_type, "method");
    }

    #[test]
    fn test_extract_scopes_interface_methods() {
        let plugin = JavaPlugin::default();
        let source = b"public interface Drawable {
    void draw();
    default void init() { System.out.println(\"init\"); }
}";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("Drawable.java"))
            .unwrap();

        // Should find interface and both methods (abstract and default)
        assert_eq!(scopes.len(), 3, "Expected 3 scopes, got {scopes:?}");

        // Interface should be present
        let iface = scopes.iter().find(|s| s.name == "Drawable");
        assert!(iface.is_some(), "Missing 'Drawable' interface scope");
        assert_eq!(iface.unwrap().scope_type, "interface");

        // Abstract interface method should be present
        let draw = scopes.iter().find(|s| s.name == "draw");
        assert!(draw.is_some(), "Missing 'draw' method scope");
        assert_eq!(draw.unwrap().scope_type, "method");

        // Default method should be present
        let init = scopes.iter().find(|s| s.name == "init");
        assert!(init.is_some(), "Missing 'init' default method scope");
        assert_eq!(init.unwrap().scope_type, "method");
    }

    #[test]
    fn test_extract_scopes_record() {
        let plugin = JavaPlugin::default();
        let source = b"public record Point(int x, int y) {
    public double distance() {
        return Math.sqrt(x * x + y * y);
    }
}";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("Point.java"))
            .unwrap();

        // Should find record and method
        assert_eq!(scopes.len(), 2, "Expected 2 scopes, got {scopes:?}");

        // Record should be present
        let record = scopes.iter().find(|s| s.name == "Point");
        assert!(record.is_some(), "Missing 'Point' record scope");
        assert_eq!(record.unwrap().scope_type, "record");

        // Method in record should be present
        let method = scopes.iter().find(|s| s.name == "distance");
        assert!(method.is_some(), "Missing 'distance' method scope");
        assert_eq!(method.unwrap().scope_type, "method");
    }
}
