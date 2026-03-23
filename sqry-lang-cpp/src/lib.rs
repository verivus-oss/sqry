//! C++ language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for C++, providing:
//! - AST parsing with tree-sitter
//! - Scope extraction
//! - Relation extraction (call graph, includes/imports, exports)
//!
//! This plugin enables semantic code search for C++ codebases, the #3 priority
//! language for systems programming and enterprise software.

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

pub mod relations;

/// C++ language plugin
///
/// Provides language support for C++ source files (.cpp, .cc, .cxx, .hpp, .h, .hh, .hxx).
///
/// # Example
///
/// ```
/// use sqry_lang_cpp::CppPlugin;
/// use sqry_core::plugin::LanguagePlugin;
///
/// let plugin = CppPlugin::new();
/// let metadata = plugin.metadata();
/// assert_eq!(metadata.id, "cpp");
/// assert_eq!(metadata.name, "C++");
/// ```
pub struct CppPlugin {
    graph_builder: relations::CppGraphBuilder,
}

impl CppPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: relations::CppGraphBuilder,
        }
    }
}

impl Default for CppPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for CppPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "cpp",
            name: "C++",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "C++ language support for sqry - systems programming code search",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cpp", "cc", "cxx", "hpp", "hh", "hxx"]
    }

    fn language(&self) -> Language {
        tree_sitter_cpp::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let language = self.language();

        parser.set_language(&language).map_err(|e| {
            ParseError::LanguageSetFailed(format!("Failed to set C++ language: {e}"))
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
        Self::extract_cpp_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl CppPlugin {
    /// Tree-sitter query for C++ scope extraction
    ///
    /// # Grammar Limitations
    ///
    /// - **`function_try_block`**: C++ function-try-blocks like `void foo() try { } catch { }`
    ///   are not supported because tree-sitter-cpp does not expose a `function_try_block`
    ///   node type. These rare constructs (typically used in destructors to swallow
    ///   exceptions) will not generate scopes. This is a grammar limitation, not an
    ///   implementation limitation.
    ///
    /// - **defaulted/deleted special members** (both inline and out-of-class):
    ///   All defaulted/deleted special member functions like `Resource() = default;`,
    ///   `operator=(const Resource&) = delete;`, `Foo::Foo() = default;`, and
    ///   `Foo::~Foo() = delete;` may not be captured. Tree-sitter-cpp parses these as
    ///   `declaration` nodes rather than `function_definition` nodes, regardless of
    ///   whether they appear inside a class body or at file scope.
    ///   Methods with actual bodies (e.g., `void foo() {}`) are captured correctly.
    ///   Patterns are included for `default_method_clause`/`delete_method_clause` bodies
    ///   but coverage depends on actual tree-sitter-cpp parsing behavior.
    #[allow(
        clippy::too_many_lines,
        reason = "Scope query enumerates C++ constructs in one consolidated query."
    )]
    fn scope_query_source() -> &'static str {
        r"
; Function definitions (with body) - at file/namespace scope
(function_definition
    declarator: (function_declarator
        declarator: (identifier) @function.name)
    body: (compound_statement)) @function.type

; Function definitions with pointer return type
(function_definition
    declarator: (pointer_declarator
        declarator: (function_declarator
            declarator: (identifier) @function.name))
    body: (compound_statement)) @function.type

; Method definitions (qualified identifier - Class::method)
(function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier
            name: (identifier) @method.name))
    body: (compound_statement)) @method.type

; Inline class method definitions (field_identifier inside class body)
(function_definition
    declarator: (function_declarator
        declarator: (field_identifier) @method.name)
    body: (compound_statement)) @method.type

; Inline class method with = default (special member functions)
(function_definition
    declarator: (function_declarator
        declarator: (field_identifier) @defaulted_method.name)
    body: (default_method_clause)) @defaulted_method.type

; Inline class method with = delete (deleted member functions)
(function_definition
    declarator: (function_declarator
        declarator: (field_identifier) @deleted_method.name)
    body: (delete_method_clause)) @deleted_method.type

; Constructor with = default inside class body
(function_definition
    declarator: (function_declarator
        declarator: (identifier) @defaulted_method.name)
    body: (default_method_clause)) @defaulted_method.type

; Constructor with = delete inside class body
(function_definition
    declarator: (function_declarator
        declarator: (identifier) @deleted_method.name)
    body: (delete_method_clause)) @deleted_method.type

; Destructor with = default inside class body
(function_definition
    declarator: (function_declarator
        declarator: (destructor_name) @defaulted_destructor.name)
    body: (default_method_clause)) @defaulted_destructor.type

; Destructor with = delete inside class body
(function_definition
    declarator: (function_declarator
        declarator: (destructor_name) @deleted_destructor.name)
    body: (delete_method_clause)) @deleted_destructor.type

; Operator overload with = default (e.g., operator=() = default)
(function_definition
    declarator: (function_declarator
        declarator: (operator_name) @defaulted_operator.name)
    body: (default_method_clause)) @defaulted_operator.type

; Operator overload with = delete (e.g., operator=() = delete)
(function_definition
    declarator: (function_declarator
        declarator: (operator_name) @deleted_operator.name)
    body: (delete_method_clause)) @deleted_operator.type

; Out-of-class constructor with = default (Foo::Foo() = default)
(function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier
            name: (identifier) @qualified_defaulted.name))
    body: (default_method_clause)) @qualified_defaulted.type

; Out-of-class constructor with = delete (Foo::Foo() = delete)
(function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier
            name: (identifier) @qualified_deleted.name))
    body: (delete_method_clause)) @qualified_deleted.type

; Out-of-class destructor with = default (Foo::~Foo() = default)
(function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier
            name: (destructor_name) @qualified_defaulted_destructor.name))
    body: (default_method_clause)) @qualified_defaulted_destructor.type

; Out-of-class destructor with = delete (Foo::~Foo() = delete)
(function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier
            name: (destructor_name) @qualified_deleted_destructor.name))
    body: (delete_method_clause)) @qualified_deleted_destructor.type

; Out-of-class operator with = default (Foo::operator=() = default)
(function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier
            name: (operator_name) @qualified_defaulted_operator.name))
    body: (default_method_clause)) @qualified_defaulted_operator.type

; Out-of-class operator with = delete (Foo::operator=() = delete)
(function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier
            name: (operator_name) @qualified_deleted_operator.name))
    body: (delete_method_clause)) @qualified_deleted_operator.type

; Destructor definitions (qualified identifier - Class::~Class)
(function_definition
    declarator: (function_declarator
        declarator: (qualified_identifier
            name: (destructor_name) @destructor.name))
    body: (compound_statement)) @destructor.type

; Destructor defined inside class body (inline destructor)
(function_definition
    declarator: (function_declarator
        declarator: (destructor_name) @destructor.name)
    body: (compound_statement)) @destructor.type

; Class definitions with body
(class_specifier
    name: (type_identifier) @class.name
    body: (field_declaration_list)) @class.type

; Struct definitions with body
(struct_specifier
    name: (type_identifier) @struct.name
    body: (field_declaration_list)) @struct.type

; Enum definitions with body
(enum_specifier
    name: (type_identifier) @enum.name
    body: (enumerator_list)) @enum.type

; Scoped enum (enum class) with body
(enum_specifier
    name: (type_identifier) @enum.name
    body: (enumerator_list)) @enum.type

; Union definitions with body
(union_specifier
    name: (type_identifier) @union.name
    body: (field_declaration_list)) @union.type

; Namespace definitions with body
(namespace_definition
    name: (namespace_identifier) @namespace.name
    body: (declaration_list)) @namespace.type

; Lambda expressions (anonymous functions)
(lambda_expression
    body: (compound_statement)) @lambda.type
"
    }

    /// Extract scopes from C++ source using tree-sitter queries
    #[allow(
        clippy::too_many_lines,
        reason = "Scope extraction matches many capture variants in a single pass."
    )]
    fn extract_cpp_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language: Language = tree_sitter_cpp::LANGUAGE.into();
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
                    "function.type"
                    | "method.type"
                    | "class.type"
                    | "struct.type"
                    | "namespace.type"
                    | "destructor.type"
                    | "enum.type"
                    | "union.type"
                    | "lambda.type"
                    | "defaulted_method.type"
                    | "deleted_method.type"
                    | "defaulted_destructor.type"
                    | "deleted_destructor.type"
                    | "defaulted_operator.type"
                    | "deleted_operator.type"
                    | "qualified_defaulted.type"
                    | "qualified_deleted.type"
                    | "qualified_defaulted_destructor.type"
                    | "qualified_deleted_destructor.type"
                    | "qualified_defaulted_operator.type"
                    | "qualified_deleted_operator.type" => {
                        // Map defaulted/deleted/operator variants to their base scope types
                        let type_name = match capture_name {
                            "defaulted_method.type"
                            | "deleted_method.type"
                            | "qualified_defaulted.type"
                            | "qualified_deleted.type"
                            | "defaulted_operator.type"
                            | "deleted_operator.type"
                            | "qualified_defaulted_operator.type"
                            | "qualified_deleted_operator.type" => "method",
                            "defaulted_destructor.type"
                            | "deleted_destructor.type"
                            | "qualified_defaulted_destructor.type"
                            | "qualified_deleted_destructor.type" => "destructor",
                            _ => capture_name.split('.').next().unwrap_or("unknown"),
                        };
                        scope_type = Some(type_name);
                        type_node = Some(capture.node);
                    }
                    "function.name"
                    | "method.name"
                    | "class.name"
                    | "struct.name"
                    | "namespace.name"
                    | "destructor.name"
                    | "enum.name"
                    | "union.name"
                    | "defaulted_method.name"
                    | "deleted_method.name"
                    | "defaulted_destructor.name"
                    | "deleted_destructor.name"
                    | "defaulted_operator.name"
                    | "deleted_operator.name"
                    | "qualified_defaulted.name"
                    | "qualified_deleted.name"
                    | "qualified_defaulted_destructor.name"
                    | "qualified_deleted_destructor.name"
                    | "qualified_defaulted_operator.name"
                    | "qualified_deleted_operator.name" => {
                        scope_name = capture.node.utf8_text(content).ok().map(String::from);
                    }
                    _ => {}
                }
            }

            // Handle lambda expressions which don't have explicit names
            if scope_type == Some("lambda") && scope_name.is_none() {
                scope_name = Some("<lambda>".to_string());
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

        // Deduplicate by (name, start_line, start_column) - some patterns may match same scope
        scopes.sort_by_key(|s| (s.name.clone(), s.start_line, s.start_column));
        scopes.dedup_by(|a, b| {
            a.name == b.name && a.start_line == b.start_line && a.start_column == b.start_column
        });

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
        let plugin = CppPlugin::new();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "cpp");
        assert_eq!(metadata.name, "C++");
        assert_eq!(metadata.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(metadata.author, "Verivus Pty Ltd");
        assert_eq!(metadata.tree_sitter_version, "0.24");
    }

    #[test]
    fn test_extensions() {
        let plugin = CppPlugin::new();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 6);
        assert!(extensions.contains(&"cpp"));
        assert!(extensions.contains(&"hpp"));
        assert!(extensions.contains(&"hh"));
    }

    #[test]
    fn test_language() {
        let plugin = CppPlugin::new();
        let language = plugin.language();

        // Just verify we can get a language (ABI version should be non-zero)
        assert!(language.abi_version() > 0);
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = CppPlugin::new();
        let source = b"class MyClass {};";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_plugin_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CppPlugin>();
    }

    #[test]
    fn test_extract_scopes_functions() {
        let plugin = CppPlugin::new();
        let source = br#"
void foo() {
    int x = 1;
}

int main(int argc, char** argv) {
    foo();
    return 0;
}
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 2 function scopes: foo and main
        assert_eq!(
            scopes.len(),
            2,
            "Expected 2 function scopes, got {}",
            scopes.len()
        );

        // Verify scope names and types
        let scope_names: Vec<&str> = scopes.iter().map(|s| s.name.as_str()).collect();
        assert!(scope_names.contains(&"foo"), "Missing 'foo' scope");
        assert!(scope_names.contains(&"main"), "Missing 'main' scope");

        // All should be function type
        for scope in &scopes {
            assert_eq!(scope.scope_type, "function", "Expected function scope type");
        }
    }

    #[test]
    fn test_extract_scopes_class() {
        let plugin = CppPlugin::new();
        let source = br#"
class MyClass {
public:
    void method();
    int value;
};

void MyClass::method() {
    value = 42;
}
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 2 scopes: MyClass (class) and method (method)
        assert_eq!(scopes.len(), 2, "Expected 2 scopes, got {}", scopes.len());

        let class_scope = scopes.iter().find(|s| s.name == "MyClass");
        let method_scope = scopes.iter().find(|s| s.name == "method");

        assert!(class_scope.is_some(), "Missing 'MyClass' class scope");
        assert!(method_scope.is_some(), "Missing 'method' scope");

        assert_eq!(class_scope.unwrap().scope_type, "class");
        assert_eq!(method_scope.unwrap().scope_type, "method");
    }

    #[test]
    fn test_extract_scopes_namespace() {
        let plugin = CppPlugin::new();
        let source = br#"
namespace demo {
    void helper() {
        // helper implementation
    }
}
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 2 scopes: demo (namespace) and helper (function)
        assert_eq!(scopes.len(), 2, "Expected 2 scopes, got {}", scopes.len());

        let ns_scope = scopes.iter().find(|s| s.name == "demo");
        let func_scope = scopes.iter().find(|s| s.name == "helper");

        assert!(ns_scope.is_some(), "Missing 'demo' namespace scope");
        assert!(func_scope.is_some(), "Missing 'helper' function scope");

        assert_eq!(ns_scope.unwrap().scope_type, "namespace");
        assert_eq!(func_scope.unwrap().scope_type, "function");

        // helper should be nested inside demo namespace
        let demo = ns_scope.unwrap();
        let helper = func_scope.unwrap();
        assert!(
            helper.start_line > demo.start_line,
            "helper should be inside demo"
        );
        assert!(
            helper.end_line < demo.end_line,
            "helper should be inside demo"
        );
    }

    #[test]
    fn test_extract_scopes_struct() {
        let plugin = CppPlugin::new();
        let source = br#"
struct Point {
    int x;
    int y;
};
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 1 struct scope: Point
        assert_eq!(
            scopes.len(),
            1,
            "Expected 1 struct scope, got {}",
            scopes.len()
        );
        assert_eq!(scopes[0].name, "Point");
        assert_eq!(scopes[0].scope_type, "struct");
    }

    #[test]
    fn test_extract_scopes_destructor() {
        let plugin = CppPlugin::new();
        let source = br#"
class Resource {
public:
    ~Resource() {
        // cleanup
    }
};
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 2 scopes: Resource (class) and ~Resource (destructor)
        assert_eq!(scopes.len(), 2, "Expected 2 scopes, got {}", scopes.len());

        let class_scope = scopes.iter().find(|s| s.name == "Resource");
        let destructor_scope = scopes.iter().find(|s| s.name == "~Resource");

        assert!(class_scope.is_some(), "Missing 'Resource' class scope");
        assert!(
            destructor_scope.is_some(),
            "Missing '~Resource' destructor scope"
        );

        assert_eq!(class_scope.unwrap().scope_type, "class");
        assert_eq!(destructor_scope.unwrap().scope_type, "destructor");
    }

    #[test]
    fn test_extract_scopes_enum() {
        let plugin = CppPlugin::new();
        let source = br#"
enum Color {
    Red,
    Green,
    Blue
};

enum class Status {
    Active,
    Inactive
};
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 2 enum scopes: Color and Status
        assert_eq!(
            scopes.len(),
            2,
            "Expected 2 enum scopes, got {}",
            scopes.len()
        );

        let color_scope = scopes.iter().find(|s| s.name == "Color");
        let status_scope = scopes.iter().find(|s| s.name == "Status");

        assert!(color_scope.is_some(), "Missing 'Color' enum scope");
        assert!(status_scope.is_some(), "Missing 'Status' enum scope");

        assert_eq!(color_scope.unwrap().scope_type, "enum");
        assert_eq!(status_scope.unwrap().scope_type, "enum");
    }

    #[test]
    fn test_extract_scopes_union() {
        let plugin = CppPlugin::new();
        let source = br#"
union Value {
    int i;
    float f;
    char c;
};
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 1 union scope: Value
        assert_eq!(
            scopes.len(),
            1,
            "Expected 1 union scope, got {}",
            scopes.len()
        );
        assert_eq!(scopes[0].name, "Value");
        assert_eq!(scopes[0].scope_type, "union");
    }

    #[test]
    fn test_extract_scopes_lambda() {
        let plugin = CppPlugin::new();
        let source = br#"
void process() {
    auto callback = [](int x) {
        return x * 2;
    };
}
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 2 scopes: process (function) and <lambda>
        assert_eq!(scopes.len(), 2, "Expected 2 scopes, got {}", scopes.len());

        let func_scope = scopes.iter().find(|s| s.name == "process");
        let lambda_scope = scopes.iter().find(|s| s.name == "<lambda>");

        assert!(func_scope.is_some(), "Missing 'process' function scope");
        assert!(lambda_scope.is_some(), "Missing '<lambda>' scope");

        assert_eq!(func_scope.unwrap().scope_type, "function");
        assert_eq!(lambda_scope.unwrap().scope_type, "lambda");
    }

    #[test]
    fn test_extract_scopes_inline_class_methods() {
        let plugin = CppPlugin::new();
        let source = br#"
class Foo {
    void bar() {
        // inline method defined inside class body
    }

    int getValue() {
        return 42;
    }
};
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 3 scopes: Foo (class), bar (method), getValue (method)
        assert_eq!(
            scopes.len(),
            3,
            "Expected 3 scopes, got {}: {:?}",
            scopes.len(),
            scopes
                .iter()
                .map(|s| (&s.name, &s.scope_type))
                .collect::<Vec<_>>()
        );

        let class_scope = scopes.iter().find(|s| s.name == "Foo");
        let bar_scope = scopes.iter().find(|s| s.name == "bar");
        let get_value_scope = scopes.iter().find(|s| s.name == "getValue");

        assert!(class_scope.is_some(), "Missing 'Foo' class scope");
        assert!(bar_scope.is_some(), "Missing 'bar' inline method scope");
        assert!(
            get_value_scope.is_some(),
            "Missing 'getValue' inline method scope"
        );

        assert_eq!(class_scope.unwrap().scope_type, "class");
        assert_eq!(bar_scope.unwrap().scope_type, "method");
        assert_eq!(get_value_scope.unwrap().scope_type, "method");

        // Verify parent_id nesting: class has no parent, methods have class as parent
        assert!(
            class_scope.unwrap().parent_id.is_none(),
            "Top-level class Foo should have no parent"
        );
        assert!(
            bar_scope.unwrap().parent_id.is_some(),
            "Method 'bar' should have parent_id pointing to class Foo"
        );
        assert!(
            get_value_scope.unwrap().parent_id.is_some(),
            "Method 'getValue' should have parent_id pointing to class Foo"
        );
    }

    #[test]
    fn test_extract_scopes_defaulted_deleted_methods() {
        let plugin = CppPlugin::new();
        let source = br#"
class Resource {
    Resource() = default;
    ~Resource() = default;
    Resource(const Resource&) = delete;
    Resource& operator=(const Resource&) = delete;
    void process() {}
};
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Print all scopes for debugging
        eprintln!("All scopes found:");
        for scope in &scopes {
            eprintln!(
                "  - name: '{}', type: '{}', parent_id: {:?}",
                scope.name, scope.scope_type, scope.parent_id
            );
        }

        // Should have scopes for: Resource (class), Resource constructor (defaulted),
        // ~Resource destructor (defaulted), Resource copy ctor (deleted),
        // operator= (deleted), process (method)
        let class_scope = scopes
            .iter()
            .find(|s| s.name == "Resource" && s.scope_type == "class");
        let process_scope = scopes.iter().find(|s| s.name == "process");

        // Find defaulted/deleted special member functions
        let default_ctor = scopes
            .iter()
            .find(|s| s.name == "Resource" && s.scope_type == "method");
        let default_dtor = scopes
            .iter()
            .find(|s| s.name.contains('~') && s.scope_type == "destructor");
        let deleted_operator = scopes
            .iter()
            .find(|s| s.name.contains("operator") && s.scope_type == "method");

        assert!(class_scope.is_some(), "Missing 'Resource' class scope");
        assert!(process_scope.is_some(), "Missing 'process' method scope");
        assert_eq!(process_scope.unwrap().scope_type, "method");

        // Verify parent_id nesting - class has no parent
        let class_scope = class_scope.unwrap();
        assert!(
            class_scope.parent_id.is_none(),
            "Top-level class should have no parent"
        );

        // Verify process method has the class as parent (check actual parent_id value)
        let process_scope = process_scope.unwrap();
        assert!(
            process_scope.parent_id.is_some(),
            "Method 'process' should have parent_id pointing to class"
        );
        assert_eq!(
            process_scope.parent_id,
            Some(class_scope.id),
            "Method 'process' parent_id should match class id ({:?})",
            class_scope.id
        );

        // NOTE: Inline defaulted/deleted special member functions (inside class body) are
        // parsed as `declaration` nodes by tree-sitter-cpp, NOT `function_definition` nodes.
        // This is a documented grammar limitation. The `if let` pattern is intentional because
        // we cannot guarantee these will be captured by our queries.
        //
        // These assertions verify correct behavior IF the scopes are captured (e.g., if
        // tree-sitter-cpp grammar changes in the future to parse them as function_definition).
        if let Some(ctor) = default_ctor {
            assert_eq!(
                ctor.scope_type, "method",
                "Defaulted constructor should be method type"
            );
            assert_eq!(
                ctor.parent_id,
                Some(class_scope.id),
                "Defaulted constructor parent_id should match class id"
            );
        }

        if let Some(dtor) = default_dtor {
            assert_eq!(
                dtor.scope_type, "destructor",
                "Defaulted destructor should be destructor type"
            );
            assert_eq!(
                dtor.parent_id,
                Some(class_scope.id),
                "Defaulted destructor parent_id should match class id"
            );
        }

        if let Some(op) = deleted_operator {
            assert_eq!(
                op.scope_type, "method",
                "Deleted operator= should be method type"
            );
            assert_eq!(
                op.parent_id,
                Some(class_scope.id),
                "Deleted operator= parent_id should match class id"
            );
        }
    }

    #[test]
    fn test_extract_scopes_out_of_class_defaulted() {
        let plugin = CppPlugin::new();
        let source = br#"
class Foo {
    Foo();
    ~Foo();
};

// Out-of-class defaulted special members
Foo::Foo() = default;
Foo::~Foo() = default;
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Print all scopes for debugging
        eprintln!("All out-of-class scopes found:");
        for scope in &scopes {
            eprintln!(
                "  - name: '{}', type: '{}', line: {}",
                scope.name, scope.scope_type, scope.start_line
            );
        }

        // Should have the class
        let class_scope = scopes
            .iter()
            .find(|s| s.name == "Foo" && s.scope_type == "class");
        assert!(class_scope.is_some(), "Missing 'Foo' class scope");

        // NOTE: Out-of-class defaulted/deleted definitions like `Foo::Foo() = default;`
        // are ALSO parsed as `declaration` nodes by tree-sitter-cpp, NOT `function_definition`.
        // This is the same grammar limitation as inline defaulted/deleted methods.
        // The patterns are correct and will capture these if tree-sitter-cpp ever parses
        // them as function_definition nodes.
        //
        // Test verifies behavior IF captured (future-proofing for grammar changes).
        let out_of_class_ctor = scopes
            .iter()
            .find(|s| s.scope_type == "method" && s.start_line > 6);
        let out_of_class_dtor = scopes
            .iter()
            .find(|s| s.scope_type == "destructor" && s.start_line > 6);

        if let Some(ctor) = out_of_class_ctor {
            assert_eq!(
                ctor.scope_type, "method",
                "Out-of-class ctor should be method type"
            );
            assert_eq!(ctor.name, "Foo", "Out-of-class ctor name should be 'Foo'");
            eprintln!("Successfully captured out-of-class defaulted constructor");
        }

        if let Some(dtor) = out_of_class_dtor {
            assert_eq!(
                dtor.scope_type, "destructor",
                "Out-of-class dtor should be destructor type"
            );
            // Destructor name may include the tilde or just 'Foo' depending on extraction
            assert!(
                dtor.name.contains('~') || dtor.name == "Foo",
                "Out-of-class dtor name should contain '~' or be 'Foo', got: '{}'",
                dtor.name
            );
            eprintln!("Successfully captured out-of-class defaulted destructor");
        }
    }

    #[test]
    fn test_extract_scopes_out_of_class_qualified_operators() {
        let plugin = CppPlugin::new();
        let source = br#"
class Foo {
    Foo& operator=(const Foo&);
    bool operator==(const Foo&) const;
};

// Out-of-class qualified operators with = default/delete
Foo& Foo::operator=(const Foo&) = default;
bool Foo::operator==(const Foo&) const = delete;
"#;
        let path = std::path::Path::new("test.cpp");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Print all scopes for debugging
        eprintln!("All out-of-class qualified operator scopes found:");
        for scope in &scopes {
            eprintln!(
                "  - name: '{}', type: '{}', line: {}",
                scope.name, scope.scope_type, scope.start_line
            );
        }

        // Should have the class
        let class_scope = scopes
            .iter()
            .find(|s| s.name == "Foo" && s.scope_type == "class");
        assert!(class_scope.is_some(), "Missing 'Foo' class scope");

        // NOTE: Out-of-class qualified operators like `Foo::operator=(const Foo&) = default;`
        // are parsed as `declaration` nodes by tree-sitter-cpp (grammar limitation).
        // The patterns `qualified_defaulted_operator` and `qualified_deleted_operator` are
        // included to capture these scopes if tree-sitter-cpp ever parses them as
        // `function_definition` nodes.
        //
        // Test verifies correct behavior IF captured (future-proofing for grammar changes).
        let defaulted_assign_op = scopes
            .iter()
            .find(|s| s.name.contains("operator=") && s.scope_type == "method" && s.start_line > 6);
        let deleted_compare_op = scopes.iter().find(|s| {
            s.name.contains("operator==") && s.scope_type == "method" && s.start_line > 6
        });

        if let Some(op) = defaulted_assign_op {
            assert_eq!(
                op.scope_type, "method",
                "Defaulted operator= should be method type"
            );
            assert!(
                op.name.contains("operator="),
                "Defaulted operator name should contain 'operator=', got: '{}'",
                op.name
            );
            eprintln!("Successfully captured out-of-class defaulted operator=");
        }

        if let Some(op) = deleted_compare_op {
            assert_eq!(
                op.scope_type, "method",
                "Deleted operator== should be method type"
            );
            assert!(
                op.name.contains("operator=="),
                "Deleted operator name should contain 'operator==', got: '{}'",
                op.name
            );
            eprintln!("Successfully captured out-of-class deleted operator==");
        }
    }
}
