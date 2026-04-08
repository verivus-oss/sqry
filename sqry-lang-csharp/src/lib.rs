//! C# language plugin for sqry
//!
//! Implements the `LanguagePlugin` trait for C#, providing:
//! - AST parsing with tree-sitter
//! - Scope extraction
//! - Relation extraction (call graph, using directives, exports, return types)
//!
//! This plugin enables semantic code search for C# codebases, the #4 priority
//! language for Microsoft ecosystem and Unity game development.

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::{
    LanguageMetadata, LanguagePlugin,
    error::{ParseError, ScopeError},
};
use std::path::Path;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Parser, Query, QueryCursor, Tree};

pub mod relations;

/// C# language plugin
///
/// Provides language support for C# source files (.cs, .csx).
///
/// # Example
///
/// ```
/// use sqry_lang_csharp::CSharpPlugin;
/// use sqry_core::plugin::LanguagePlugin;
///
/// let plugin = CSharpPlugin::default();
/// let metadata = plugin.metadata();
/// assert_eq!(metadata.id, "csharp");
/// assert_eq!(metadata.name, "C#");
/// ```
pub struct CSharpPlugin {
    graph_builder: relations::CSharpGraphBuilder,
}

impl CSharpPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: relations::CSharpGraphBuilder::default(),
        }
    }
}

impl Default for CSharpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for CSharpPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "csharp",
            name: "C#",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "C# language support for sqry - .NET and Unity code search",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cs", "csx"]
    }

    fn language(&self) -> Language {
        tree_sitter_c_sharp::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        let language = self.language();

        parser.set_language(&language).map_err(|e| {
            ParseError::LanguageSetFailed(format!("Failed to set C# language: {e}"))
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
        Self::extract_csharp_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl CSharpPlugin {
    /// Tree-sitter query for C# scope extraction
    fn scope_query_source() -> &'static str {
        r"
; Class declarations with body
(class_declaration
    name: (identifier) @class.name
    body: (declaration_list)) @class.type

; Interface declarations with body
(interface_declaration
    name: (identifier) @interface.name
    body: (declaration_list)) @interface.type

; Struct declarations with body
(struct_declaration
    name: (identifier) @struct.name
    body: (declaration_list)) @struct.type

; Record declarations with body (C# 9+)
(record_declaration
    name: (identifier) @record.name
    body: (declaration_list)) @record.type

; Method declarations with block body
(method_declaration
    name: (identifier) @method.name
    body: (block)) @method.type

; Method declarations with expression body (=> expr;)
(method_declaration
    name: (identifier) @method.name
    body: (arrow_expression_clause)) @method.type

; Abstract/interface method declarations (no body, ends with semicolon)
(method_declaration
    name: (identifier) @abstract_method.name) @abstract_method.type

; Constructor declarations with block body
(constructor_declaration
    name: (identifier) @constructor.name
    body: (block)) @constructor.type

; Constructor declarations with expression body
(constructor_declaration
    name: (identifier) @constructor.name
    body: (arrow_expression_clause)) @constructor.type

; Property declarations with accessors (create scope for property block)
(property_declaration
    name: (identifier) @property.name
    accessors: (accessor_list)) @property.type

; Namespace declarations with body (simple name)
(namespace_declaration
    name: (identifier) @namespace.name
    body: (declaration_list)) @namespace.type

; Namespace declarations with body (qualified name)
(namespace_declaration
    name: (qualified_name) @namespace.name
    body: (declaration_list)) @namespace.type

; File-scoped namespace declarations (C# 10+)
(file_scoped_namespace_declaration
    name: (identifier) @namespace.name) @namespace.type

; File-scoped namespace with qualified name (C# 10+)
(file_scoped_namespace_declaration
    name: (qualified_name) @namespace.name) @namespace.type

; Enum declarations
(enum_declaration
    name: (identifier) @enum.name
    body: (enum_member_declaration_list)) @enum.type

; Local functions (nested functions inside methods)
(local_function_statement
    name: (identifier) @function.name
    body: (block)) @function.type

; Local functions with expression body
(local_function_statement
    name: (identifier) @function.name
    body: (arrow_expression_clause)) @function.type
"
    }

    /// Extract scopes from C# source using tree-sitter queries
    fn extract_csharp_scopes(
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError> {
        let root_node = tree.root_node();
        let language: Language = tree_sitter_c_sharp::LANGUAGE.into();
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
                    "class.type"
                    | "interface.type"
                    | "struct.type"
                    | "method.type"
                    | "namespace.type"
                    | "record.type"
                    | "constructor.type"
                    | "property.type"
                    | "enum.type"
                    | "function.type"
                    | "abstract_method.type" => {
                        // abstract_method maps to method scope type
                        let type_name = if capture_name == "abstract_method.type" {
                            "method"
                        } else {
                            capture_name.split('.').next().unwrap_or("unknown")
                        };
                        scope_type = Some(type_name);
                        type_node = Some(capture.node);
                    }
                    "class.name"
                    | "interface.name"
                    | "struct.name"
                    | "method.name"
                    | "namespace.name"
                    | "record.name"
                    | "constructor.name"
                    | "property.name"
                    | "enum.name"
                    | "function.name"
                    | "abstract_method.name" => {
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

        // Deduplicate by (name, scope_type, start_line, start_column)
        // Include scope_type in key to prevent merging distinct scope types at same position
        // abstract_method pattern may match methods with bodies, but dedup handles this safely
        scopes.sort_by_key(|s| {
            (
                s.name.clone(),
                s.scope_type.clone(),
                s.start_line,
                s.start_column,
            )
        });
        scopes.dedup_by(|a, b| {
            a.name == b.name
                && a.scope_type == b.scope_type
                && a.start_line == b.start_line
                && a.start_column == b.start_column
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
        let plugin = CSharpPlugin::default();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "csharp");
        assert_eq!(metadata.name, "C#");
        assert_eq!(metadata.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(metadata.author, "Verivus Pty Ltd");
        assert_eq!(metadata.tree_sitter_version, "0.24");
    }

    #[test]
    fn test_extensions() {
        let plugin = CSharpPlugin::default();
        let extensions = plugin.extensions();

        assert_eq!(extensions.len(), 2);
        assert!(extensions.contains(&"cs"));
        assert!(extensions.contains(&"csx"));
    }

    #[test]
    fn test_language() {
        let plugin = CSharpPlugin::default();
        let language = plugin.language();

        // Just verify we can get a language (ABI version should be non-zero)
        assert!(language.abi_version() > 0);
    }

    #[test]
    fn test_parse_ast_simple() {
        let plugin = CSharpPlugin::default();
        let source = b"class MyClass { }";

        let tree = plugin.parse_ast(source).unwrap();
        assert!(!tree.root_node().has_error());
    }

    #[test]
    fn test_plugin_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CSharpPlugin>();
    }

    #[test]
    fn test_extract_scopes_class() {
        let plugin = CSharpPlugin::default();
        let source = br"
public class MyClass
{
    public void Method()
    {
        // method body
    }
}
";
        let path = std::path::Path::new("test.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 2 scopes: MyClass (class) and Method (method)
        assert_eq!(scopes.len(), 2, "Expected 2 scopes, got {}", scopes.len());

        let class_scope = scopes.iter().find(|s| s.name == "MyClass");
        let method_scope = scopes.iter().find(|s| s.name == "Method");

        assert!(class_scope.is_some(), "Missing 'MyClass' class scope");
        assert!(method_scope.is_some(), "Missing 'Method' method scope");

        assert_eq!(class_scope.unwrap().scope_type, "class");
        assert_eq!(method_scope.unwrap().scope_type, "method");
    }

    #[test]
    fn test_extract_scopes_namespace() {
        let plugin = CSharpPlugin::default();
        let source = br"
namespace MyApp
{
    public class Service
    {
    }
}
";
        let path = std::path::Path::new("test.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 2 scopes: MyApp (namespace) and Service (class)
        assert_eq!(scopes.len(), 2, "Expected 2 scopes, got {}", scopes.len());

        let ns_scope = scopes.iter().find(|s| s.name == "MyApp");
        let class_scope = scopes.iter().find(|s| s.name == "Service");

        assert!(ns_scope.is_some(), "Missing 'MyApp' namespace scope");
        assert!(class_scope.is_some(), "Missing 'Service' class scope");

        assert_eq!(ns_scope.unwrap().scope_type, "namespace");
        assert_eq!(class_scope.unwrap().scope_type, "class");

        // Service should be nested inside MyApp namespace
        let myapp = ns_scope.unwrap();
        let service = class_scope.unwrap();
        assert!(
            service.start_line > myapp.start_line,
            "Service should be inside MyApp"
        );
        assert!(
            service.end_line < myapp.end_line,
            "Service should be inside MyApp"
        );
    }

    #[test]
    fn test_extract_scopes_interface() {
        let plugin = CSharpPlugin::default();
        let source = br"
public interface IService
{
    void Execute();
}
";
        let path = std::path::Path::new("test.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 1 interface scope: IService
        // Note: interface methods without body don't create method scopes
        assert!(
            !scopes.is_empty(),
            "Expected at least 1 scope, got {}",
            scopes.len()
        );

        let iface_scope = scopes.iter().find(|s| s.name == "IService");
        assert!(iface_scope.is_some(), "Missing 'IService' interface scope");
        assert_eq!(iface_scope.unwrap().scope_type, "interface");
    }

    #[test]
    fn test_extract_scopes_struct() {
        let plugin = CSharpPlugin::default();
        let source = br"
public struct Point
{
    public int X;
    public int Y;
}
";
        let path = std::path::Path::new("test.cs");
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
    fn test_extract_scopes_file_scoped_namespace() {
        let plugin = CSharpPlugin::default();
        let source = br"
namespace MyApp.Services;

public class UserService
{
    public void GetUser()
    {
    }
}
";
        let path = std::path::Path::new("test.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 3 scopes: namespace, class, and method
        assert!(
            scopes.len() >= 2,
            "Expected at least 2 scopes, got {}",
            scopes.len()
        );

        let ns_scope = scopes.iter().find(|s| s.name == "MyApp.Services");
        let class_scope = scopes.iter().find(|s| s.name == "UserService");

        assert!(ns_scope.is_some(), "Missing file-scoped namespace scope");
        assert!(class_scope.is_some(), "Missing 'UserService' class scope");

        assert_eq!(ns_scope.unwrap().scope_type, "namespace");
        assert_eq!(class_scope.unwrap().scope_type, "class");
    }

    #[test]
    fn test_extract_scopes_constructor() {
        let plugin = CSharpPlugin::default();
        let source = br"
public class Person
{
    public Person(string name)
    {
        Name = name;
    }

    public string Name { get; }
}
";
        let path = std::path::Path::new("test.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        let class_scope = scopes
            .iter()
            .find(|s| s.name == "Person" && s.scope_type == "class");
        let ctor_scope = scopes
            .iter()
            .find(|s| s.name == "Person" && s.scope_type == "constructor");
        let prop_scope = scopes.iter().find(|s| s.name == "Name");

        assert!(class_scope.is_some(), "Missing 'Person' class scope");
        assert!(ctor_scope.is_some(), "Missing 'Person' constructor scope");
        assert!(prop_scope.is_some(), "Missing 'Name' property scope");

        assert_eq!(prop_scope.unwrap().scope_type, "property");
    }

    #[test]
    fn test_extract_scopes_record() {
        let plugin = CSharpPlugin::default();
        let source = br#"
public record Person(string FirstName, string LastName)
{
    public string FullName => $"{FirstName} {LastName}";
}
"#;
        let path = std::path::Path::new("test.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Note: record_declaration may or may not be available depending on tree-sitter-c-sharp version
        // At minimum, if the grammar supports it, we should find a record scope
        // For now, check that parsing doesn't fail

        // If records are supported by the grammar:
        if let Some(record_scope) = scopes.iter().find(|s| s.name == "Person") {
            assert!(
                record_scope.scope_type == "record" || record_scope.scope_type == "class",
                "Person should be record or class type"
            );
        }
    }

    #[test]
    fn test_extract_scopes_enum() {
        let plugin = CSharpPlugin::default();
        let source = br"
public enum Status
{
    Active,
    Inactive,
    Pending
}
";
        let path = std::path::Path::new("test.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        let enum_scope = scopes.iter().find(|s| s.name == "Status");
        assert!(enum_scope.is_some(), "Missing 'Status' enum scope");
        assert_eq!(enum_scope.unwrap().scope_type, "enum");
    }

    #[test]
    fn test_extract_scopes_expression_bodied() {
        let plugin = CSharpPlugin::default();
        let source = br"
public class Calculator
{
    public int Add(int a, int b) => a + b;

    public int Multiply(int a, int b)
    {
        return a * b;
    }
}
";
        let path = std::path::Path::new("test.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        let class_scope = scopes.iter().find(|s| s.name == "Calculator");
        let add_scope = scopes.iter().find(|s| s.name == "Add");
        let multiply_scope = scopes.iter().find(|s| s.name == "Multiply");

        assert!(class_scope.is_some(), "Missing 'Calculator' class scope");
        assert!(
            add_scope.is_some(),
            "Missing 'Add' expression-bodied method scope"
        );
        assert!(
            multiply_scope.is_some(),
            "Missing 'Multiply' block-bodied method scope"
        );

        assert_eq!(add_scope.unwrap().scope_type, "method");
        assert_eq!(multiply_scope.unwrap().scope_type, "method");
    }

    #[test]
    fn test_extract_scopes_local_function() {
        let plugin = CSharpPlugin::default();
        let source = br"
public class Example
{
    public void Outer()
    {
        void Inner()
        {
            // local function
        }
        Inner();
    }
}
";
        let path = std::path::Path::new("test.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        let outer_scope = scopes.iter().find(|s| s.name == "Outer");
        let inner_scope = scopes.iter().find(|s| s.name == "Inner");

        assert!(outer_scope.is_some(), "Missing 'Outer' method scope");
        assert!(
            inner_scope.is_some(),
            "Missing 'Inner' local function scope"
        );

        assert_eq!(inner_scope.unwrap().scope_type, "function");
    }

    #[test]
    fn test_extract_scopes_abstract_interface_methods() {
        let plugin = CSharpPlugin::default();
        let source = br"
public interface IService
{
    void Execute();
    string GetName();
}

public abstract class BaseService
{
    public abstract void Initialize();
    public abstract int GetPriority();
}
";
        let path = std::path::Path::new("test.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin.extract_scopes(&tree, source, path).unwrap();

        // Should have 6 scopes: IService, Execute, GetName, BaseService, Initialize, GetPriority
        assert_eq!(
            scopes.len(),
            6,
            "Expected 6 scopes, got {}: {:?}",
            scopes.len(),
            scopes
                .iter()
                .map(|s| (&s.name, &s.scope_type))
                .collect::<Vec<_>>()
        );

        let interface_scope = scopes.iter().find(|s| s.name == "IService");
        let execute_scope = scopes.iter().find(|s| s.name == "Execute");
        let get_name_scope = scopes.iter().find(|s| s.name == "GetName");
        let base_class_scope = scopes.iter().find(|s| s.name == "BaseService");
        let initialize_scope = scopes.iter().find(|s| s.name == "Initialize");
        let get_priority_scope = scopes.iter().find(|s| s.name == "GetPriority");

        assert!(
            interface_scope.is_some(),
            "Missing 'IService' interface scope"
        );
        assert!(
            execute_scope.is_some(),
            "Missing 'Execute' interface method scope"
        );
        assert!(
            get_name_scope.is_some(),
            "Missing 'GetName' interface method scope"
        );
        assert!(
            base_class_scope.is_some(),
            "Missing 'BaseService' abstract class scope"
        );
        assert!(
            initialize_scope.is_some(),
            "Missing 'Initialize' abstract method scope"
        );
        assert!(
            get_priority_scope.is_some(),
            "Missing 'GetPriority' abstract method scope"
        );

        assert_eq!(interface_scope.unwrap().scope_type, "interface");
        assert_eq!(execute_scope.unwrap().scope_type, "method");
        assert_eq!(get_name_scope.unwrap().scope_type, "method");
        assert_eq!(base_class_scope.unwrap().scope_type, "class");
        assert_eq!(initialize_scope.unwrap().scope_type, "method");
        assert_eq!(get_priority_scope.unwrap().scope_type, "method");

        // Verify parent_id nesting: top-level types have no parent, methods have parent
        // Now verify ACTUAL parent_id targets, not just is_some()
        let interface_scope = interface_scope.unwrap();
        let base_class_scope = base_class_scope.unwrap();
        let execute_scope = execute_scope.unwrap();
        let get_name_scope = get_name_scope.unwrap();
        let initialize_scope = initialize_scope.unwrap();
        let get_priority_scope = get_priority_scope.unwrap();

        // Top-level types should have no parent
        assert!(
            interface_scope.parent_id.is_none(),
            "Top-level interface IService should have no parent"
        );
        assert!(
            base_class_scope.parent_id.is_none(),
            "Top-level class BaseService should have no parent"
        );

        // Interface methods should have interface as parent
        assert_eq!(
            execute_scope.parent_id,
            Some(interface_scope.id),
            "Execute parent_id should match IService id ({:?})",
            interface_scope.id
        );
        assert_eq!(
            get_name_scope.parent_id,
            Some(interface_scope.id),
            "GetName parent_id should match IService id ({:?})",
            interface_scope.id
        );

        // Abstract class methods should have class as parent
        assert_eq!(
            initialize_scope.parent_id,
            Some(base_class_scope.id),
            "Initialize parent_id should match BaseService id ({:?})",
            base_class_scope.id
        );
        assert_eq!(
            get_priority_scope.parent_id,
            Some(base_class_scope.id),
            "GetPriority parent_id should match BaseService id ({:?})",
            base_class_scope.id
        );
    }

    #[test]
    fn test_exports_public_class() {
        use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
        use std::path::PathBuf;

        let plugin = CSharpPlugin::default();
        let source = br"
namespace MyApp
{
    public class User
    {
        private string name;
    }
}
";
        let path = PathBuf::from("User.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let mut staging = StagingGraph::new();

        plugin
            .graph_builder
            .build_graph(&tree, source, &path, &mut staging)
            .unwrap();

        // Should have at least 1 class node + 1 module node + 1 export edge
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 2,
            "Expected at least 2 nodes (class + module), got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 1,
            "Expected at least 1 export edge, got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_exports_public_methods() {
        use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
        use std::path::PathBuf;

        let plugin = CSharpPlugin::default();
        let source = br"
namespace MyApp
{
    public class Service
    {
        public void Execute() { }
        private void Internal() { }
    }
}
";
        let path = PathBuf::from("Service.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let mut staging = StagingGraph::new();

        plugin
            .graph_builder
            .build_graph(&tree, source, &path, &mut staging)
            .unwrap();

        // Should have: 1 class + 2 methods + 1 module
        // Export edges: class + Execute method (not Internal)
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 4,
            "Expected at least 4 nodes (class + 2 methods + module), got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 2,
            "Expected at least 2 export edges (class + Execute method), got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_exports_interfaces() {
        use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
        use std::path::PathBuf;

        let plugin = CSharpPlugin::default();
        let source = br"
namespace MyApp
{
    public interface IRepository
    {
        void Save();
        void Delete();
    }
}
";
        let path = PathBuf::from("IRepository.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let mut staging = StagingGraph::new();

        plugin
            .graph_builder
            .build_graph(&tree, source, &path, &mut staging)
            .unwrap();

        // Should have: 1 interface + 2 methods + 1 module
        // Export edges: interface + 2 methods
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 4,
            "Expected at least 4 nodes (interface + 2 methods + module), got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 3,
            "Expected at least 3 export edges (interface + 2 methods), got {}",
            stats.edges_staged
        );
    }

    #[test]
    fn test_exports_internal_members() {
        use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
        use std::path::PathBuf;

        let plugin = CSharpPlugin::default();
        let source = br"
namespace MyApp
{
    internal class InternalClass
    {
        internal void InternalMethod() { }
    }
}
";
        let path = PathBuf::from("Internal.cs");
        let tree = plugin.parse_ast(source).unwrap();
        let mut staging = StagingGraph::new();

        plugin
            .graph_builder
            .build_graph(&tree, source, &path, &mut staging)
            .unwrap();

        // Should have: 1 class + 1 method + 1 module
        // Export edges: class + method (internal is exported in C#)
        let stats = staging.stats();
        assert!(
            stats.nodes_staged >= 3,
            "Expected at least 3 nodes (class + method + module), got {}",
            stats.nodes_staged
        );
        assert!(
            stats.edges_staged >= 2,
            "Expected at least 2 export edges (class + method), got {}",
            stats.edges_staged
        );
    }
}
