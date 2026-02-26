// Nested conditionals kept for readability in AST extraction

// Nested conditionals kept for readability in AST extraction

//! `ServiceNow` "Xanadu" plugin scaffold for sqry
//!
//! This initial plugin reuses the JavaScript tree-sitter grammar to parse
//! `ServiceNow` application JavaScript. Graph extraction focuses on core
//! `ServiceNow` semantics (`GlideRecord`, gs APIs, Script Includes).
//!
//! ## `GraphBuilder` Support
//!
//! The `ServiceNowGraphBuilder` extracts ServiceNow-specific relationships:
//! - `GlideRecord` table access (e.g., `new GlideRecord('incident')`)
//! - `gs.*` API calls (logging, info, error, etc.)
//! - Script Include class dependencies (`Class.create()`)
//! - `GlideAjax` for client-server communication

mod relations;

pub use relations::ServiceNowGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::error::{ParseError, ScopeError};
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin};
use std::path::Path;
use tree_sitter::Node;
use tree_sitter::{Parser, Tree};

#[derive(Debug)]
pub struct ServiceNowXanaduPlugin {
    graph_builder: ServiceNowGraphBuilder,
}

impl ServiceNowXanaduPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: ServiceNowGraphBuilder,
        }
    }
}

impl Default for ServiceNowXanaduPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for ServiceNowXanaduPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "servicenow-xanadu-js",
            name: "ServiceNow Xanadu (JS)",
            version: env!("CARGO_PKG_VERSION"),
            author: "ServiceNow Inc.",
            description: "ServiceNow-aware JavaScript parsing (scaffold)",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        // Placeholder to avoid clashing with sqry-lang-javascript
        &["snjs"]
    }

    fn language(&self) -> tree_sitter::Language {
        tree_sitter_javascript::LANGUAGE.into()
    }

    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError> {
        let mut parser = Parser::new();
        parser
            .set_language(&self.language())
            .map_err(|e| ParseError::LanguageSetFailed(format!("{e:?}")))?;
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
        Ok(Self::extract_servicenow_scopes(tree, content, file_path))
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

impl ServiceNowXanaduPlugin {
    /// Extract scopes from `ServiceNow` JS - functions, classes, methods
    fn extract_servicenow_scopes(tree: &Tree, content: &[u8], file_path: &Path) -> Vec<Scope> {
        let mut scopes = Vec::new();
        Self::collect_servicenow_scopes(tree.root_node(), content, file_path, &mut scopes);

        // Sort by position (required for link_nested_scopes)
        scopes.sort_by_key(|s| (s.start_line, s.start_column));

        // Build parent-child relationships
        link_nested_scopes(&mut scopes);

        scopes
    }

    fn collect_servicenow_scopes(
        node: Node<'_>,
        content: &[u8],
        file_path: &Path,
        scopes: &mut Vec<Scope>,
    ) {
        let scope_info = Self::extract_servicenow_scope_info(node, content);

        if let Some((scope_type, name)) = scope_info {
            let start = node.start_position();
            let end = node.end_position();
            scopes.push(Scope {
                id: ScopeId::new(0), // Will be reassigned by link_nested_scopes
                scope_type,
                name,
                file_path: file_path.to_path_buf(),
                start_line: start.row + 1,
                start_column: start.column,
                end_line: end.row + 1,
                end_column: end.column,
                parent_id: None, // Will be set by link_nested_scopes
            });
        }

        // Recurse into children
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::collect_servicenow_scopes(child, content, file_path, scopes);
        }
    }

    fn extract_servicenow_scope_info(node: Node<'_>, content: &[u8]) -> Option<(String, String)> {
        match node.kind() {
            // Classes (Class.create() pattern common in ServiceNow)
            "class_declaration" => {
                let name =
                    Self::find_js_identifier(node, content).unwrap_or_else(|| "class".to_string());
                Some(("class".to_string(), name))
            }
            // Functions
            "function_declaration" => {
                let name = Self::find_js_identifier(node, content)
                    .unwrap_or_else(|| "function".to_string());
                Some(("function".to_string(), name))
            }
            // Arrow functions and function expressions (named ones only)
            "function_expression" | "arrow_function" => {
                if let Some(parent) = node.parent() {
                    // Check if this is assigned to a variable
                    if parent.kind() == "variable_declarator"
                        && let Some(name) = Self::find_js_identifier(parent, content)
                    {
                        return Some(("function".to_string(), name));
                    }
                    // Check for assignment expression: Foo.prototype.bar = function() {}
                    // or: exports.foo = function() {}
                    if parent.kind() == "assignment_expression"
                        && let Some(left) = parent.child_by_field_name("left")
                        && let Some(name) = Self::extract_assignment_target_name(left, content)
                    {
                        // Determine if it's a prototype method or export
                        if let Ok(text) = left.utf8_text(content)
                            && text.contains(".prototype.")
                        {
                            return Some(("method".to_string(), name));
                        }
                        return Some(("function".to_string(), name));
                    }
                }
                None
            }
            // Methods inside classes
            "method_definition" => {
                let name =
                    Self::find_js_identifier(node, content).unwrap_or_else(|| "method".to_string());
                Some(("method".to_string(), name))
            }
            // Object methods (common in Script Includes)
            "pair" => {
                // Check if this is a method definition in an object literal
                let mut cursor = node.walk();
                let mut has_function_value = false;
                for child in node.children(&mut cursor) {
                    if matches!(
                        child.kind(),
                        "function_expression" | "arrow_function" | "function"
                    ) {
                        has_function_value = true;
                        break;
                    }
                }
                if has_function_value {
                    let name = Self::find_js_identifier(node, content)
                        .unwrap_or_else(|| "method".to_string());
                    return Some(("method".to_string(), name));
                }
                None
            }
            // Class.create() pattern: var Foo = Class.create({...})
            // The call_expression itself becomes a class scope
            "call_expression" => {
                if Self::is_class_create_call(node, content) {
                    // Look for parent variable_declarator to get class name
                    if let Some(parent) = node.parent()
                        && parent.kind() == "variable_declarator"
                    {
                        let name = Self::find_js_identifier(parent, content)
                            .unwrap_or_else(|| "class".to_string());
                        return Some(("class".to_string(), name));
                    }
                    // Anonymous Class.create
                    return Some(("class".to_string(), "anonymous".to_string()));
                }
                None
            }
            _ => None,
        }
    }

    /// Check if this is a Class.create(...) call
    fn is_class_create_call(node: Node<'_>, content: &[u8]) -> bool {
        if node.kind() != "call_expression" {
            return false;
        }
        if let Some(func) = node.child_by_field_name("function")
            && let Ok(text) = func.utf8_text(content)
        {
            return text == "Class.create";
        }
        false
    }

    /// Extract the final property name from an assignment target
    /// e.g., "Foo.prototype.bar" -> "bar", "exports.foo" -> "foo"
    fn extract_assignment_target_name(node: Node<'_>, content: &[u8]) -> Option<String> {
        // For member_expression, get the rightmost property
        if node.kind() == "member_expression"
            && let Some(prop) = node.child_by_field_name("property")
            && let Ok(text) = prop.utf8_text(content)
        {
            return Some(Self::strip_quotes(text.trim()));
        }
        // For identifier, just return it
        if node.kind() == "identifier"
            && let Ok(text) = node.utf8_text(content)
        {
            return Some(text.trim().to_string());
        }
        None
    }

    fn find_js_identifier(node: Node<'_>, content: &[u8]) -> Option<String> {
        // Try named field first
        if let Some(name_node) = node.child_by_field_name("name")
            && let Ok(text) = name_node.utf8_text(content)
        {
            let name = text.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }

        // Try "key" field for object properties (strip quotes for string keys)
        if let Some(key_node) = node.child_by_field_name("key")
            && let Ok(text) = key_node.utf8_text(content)
        {
            let name = Self::strip_quotes(text.trim());
            if !name.is_empty() {
                return Some(name);
            }
        }

        // Fall back to looking for identifier children
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if (child.kind() == "identifier" || child.kind() == "property_identifier")
                && let Ok(text) = child.utf8_text(content)
            {
                let name = text.trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }

        None
    }

    /// Strip surrounding single or double quotes from a string
    fn strip_quotes(s: &str) -> String {
        let s = s.trim();
        // Need at least 2 chars to strip quotes (opening + closing)
        if s.len() < 2 {
            return s.to_string();
        }
        if (s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')) {
            s[1..s.len() - 1].to_string()
        } else {
            s.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ast_smoke() {
        let plugin = ServiceNowXanaduPlugin::new();
        let tree = plugin.parse_ast(b"function demo(){};").unwrap();
        assert!(tree.root_node().child_count() > 0);
    }

    #[test]
    fn test_metadata_fields() {
        let plugin = ServiceNowXanaduPlugin::new();
        let m = plugin.metadata();
        assert_eq!(m.id, "servicenow-xanadu-js");
        assert_eq!(m.name, "ServiceNow Xanadu (JS)");
    }

    // ========================================================================
    // Scope Extraction Tests
    // ========================================================================

    #[test]
    fn test_extract_scopes_function_declaration() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = b"function myFunction() { return 42; }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.js"))
            .unwrap();

        let fn_scope = scopes.iter().find(|s| s.scope_type == "function");
        assert!(fn_scope.is_some(), "Should extract function scope");
        assert_eq!(fn_scope.unwrap().name, "myFunction");
    }

    #[test]
    fn test_extract_scopes_class_declaration() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = b"class MyClass { constructor() {} doWork() {} }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.js"))
            .unwrap();

        let class_scope = scopes.iter().find(|s| s.scope_type == "class");
        assert!(class_scope.is_some(), "Should extract class scope");
        assert_eq!(class_scope.unwrap().name, "MyClass");
    }

    #[test]
    fn test_extract_scopes_method_definition() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = b"class MyClass { myMethod() { return 1; } }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.js"))
            .unwrap();

        let method_scope = scopes.iter().find(|s| s.scope_type == "method");
        assert!(method_scope.is_some(), "Should extract method scope");
        assert_eq!(method_scope.unwrap().name, "myMethod");
    }

    #[test]
    fn test_extract_scopes_class_create_pattern() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = br#"
var MyScriptInclude = Class.create({
    initialize: function() {},
    doWork: function() { return 'work'; }
});
"#;
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("MyScriptInclude.js"))
            .unwrap();

        // Should detect Class.create as a class scope
        let class_scope = scopes.iter().find(|s| s.scope_type == "class");
        assert!(
            class_scope.is_some(),
            "Should extract Class.create as class scope"
        );
        assert!(
            class_scope.unwrap().name.contains("MyScriptInclude"),
            "Should have class name from variable"
        );

        // Should detect methods inside Class.create
        let method_scopes: Vec<_> = scopes.iter().filter(|s| s.scope_type == "method").collect();
        assert!(
            method_scopes.len() >= 2,
            "Should extract methods from Class.create"
        );
    }

    #[test]
    fn test_extract_scopes_object_method() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = br#"
var obj = {
    foo: function() { return 'foo'; },
    bar: function() { return 'bar'; }
};
"#;
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.js"))
            .unwrap();

        let method_scopes: Vec<_> = scopes.iter().filter(|s| s.scope_type == "method").collect();
        assert_eq!(method_scopes.len(), 2, "Should have 2 object methods");
        assert!(
            method_scopes.iter().any(|s| s.name == "foo"),
            "Should have foo method"
        );
        assert!(
            method_scopes.iter().any(|s| s.name == "bar"),
            "Should have bar method"
        );
    }

    #[test]
    fn test_extract_scopes_quoted_method_name() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = br#"
var obj = {
    'quoted-name': function() { return 1; },
    "double-quoted": function() { return 2; }
};
"#;
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.js"))
            .unwrap();

        // Quoted names should have quotes stripped
        let method_scopes: Vec<_> = scopes.iter().filter(|s| s.scope_type == "method").collect();
        for scope in &method_scopes {
            assert!(
                !scope.name.starts_with('\'') && !scope.name.starts_with('"'),
                "Method name should not have quotes: {}",
                scope.name
            );
        }
    }

    #[test]
    fn test_extract_scopes_variable_function() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = b"var myFunc = function() { return 1; };";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.js"))
            .unwrap();

        let fn_scope = scopes.iter().find(|s| s.scope_type == "function");
        assert!(fn_scope.is_some(), "Should extract variable function");
        assert_eq!(fn_scope.unwrap().name, "myFunc");
    }

    #[test]
    fn test_extract_scopes_arrow_function() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = b"var myArrow = () => { return 1; };";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.js"))
            .unwrap();

        let fn_scope = scopes.iter().find(|s| s.scope_type == "function");
        assert!(fn_scope.is_some(), "Should extract arrow function");
        assert_eq!(fn_scope.unwrap().name, "myArrow");
    }

    #[test]
    fn test_extract_scopes_prototype_method() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = b"MyClass.prototype.myMethod = function() { return 1; };";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.js"))
            .unwrap();

        let method_scope = scopes.iter().find(|s| s.scope_type == "method");
        assert!(method_scope.is_some(), "Should extract prototype method");
        assert_eq!(method_scope.unwrap().name, "myMethod");
    }

    #[test]
    fn test_extract_scopes_nested() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = br#"
class Outer {
    inner() {
        function nested() {}
    }
}
"#;
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.js"))
            .unwrap();

        // Check nesting - nested function should have parent
        let nested_fn = scopes.iter().find(|s| s.name == "nested");
        if let Some(nested) = nested_fn {
            assert!(
                nested.parent_id.is_some(),
                "Nested function should have parent"
            );
        }
    }

    #[test]
    fn test_extract_scopes_boundaries() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = br#"
function myFunc() {
    var x = 1;
    return x;
}
"#;
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.js"))
            .unwrap();

        for scope in &scopes {
            assert!(scope.start_line >= 1);
            assert!(scope.end_line >= scope.start_line);
        }
    }

    #[test]
    fn test_extract_scopes_empty_file() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = b"";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("script.js"))
            .unwrap();

        assert!(scopes.is_empty(), "Empty file should have no scopes");
    }

    #[test]
    fn test_extract_scopes_malformed() {
        let plugin = ServiceNowXanaduPlugin::new();
        let source = b"function broken( { return";
        let tree = plugin.parse_ast(source).unwrap();
        let result = plugin.extract_scopes(&tree, source, Path::new("script.js"));

        assert!(result.is_ok(), "Should handle malformed JS gracefully");
    }

    #[test]
    fn test_strip_quotes_edge_cases() {
        // Normal cases
        assert_eq!(ServiceNowXanaduPlugin::strip_quotes("'hello'"), "hello");
        assert_eq!(ServiceNowXanaduPlugin::strip_quotes("\"world\""), "world");

        // Edge cases that previously would panic
        assert_eq!(ServiceNowXanaduPlugin::strip_quotes(""), "");
        assert_eq!(ServiceNowXanaduPlugin::strip_quotes("'"), "'");
        assert_eq!(ServiceNowXanaduPlugin::strip_quotes("\""), "\"");
        assert_eq!(ServiceNowXanaduPlugin::strip_quotes("a"), "a");

        // Empty quotes
        assert_eq!(ServiceNowXanaduPlugin::strip_quotes("''"), "");
        assert_eq!(ServiceNowXanaduPlugin::strip_quotes("\"\""), "");

        // Not quotes - should return as-is
        assert_eq!(ServiceNowXanaduPlugin::strip_quotes("hello"), "hello");
        assert_eq!(ServiceNowXanaduPlugin::strip_quotes("'mixed\""), "'mixed\"");
    }
}
// Nested conditionals kept for readability in AST extraction
