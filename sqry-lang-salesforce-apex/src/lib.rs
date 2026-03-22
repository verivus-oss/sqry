//! Salesforce Apex language plugin for sqry.
//!
//! Provides graph-native extraction via `ApexGraphBuilder`, AST parsing,
//! and scope extraction for Apex source files.

mod relations;

pub use relations::ApexGraphBuilder;

use sqry_core::ast::{Scope, ScopeId, link_nested_scopes};
use sqry_core::plugin::error::{ParseError, ScopeError};
use sqry_core::plugin::{LanguageMetadata, LanguagePlugin};
use std::path::Path;
use tree_sitter::{Language, Node, Parser, Tree};

#[derive(Debug)]
pub struct SalesforceApexPlugin {
    graph_builder: ApexGraphBuilder,
}

impl SalesforceApexPlugin {
    #[must_use]
    pub fn new() -> Self {
        Self {
            graph_builder: ApexGraphBuilder,
        }
    }
}

impl Default for SalesforceApexPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguagePlugin for SalesforceApexPlugin {
    fn metadata(&self) -> LanguageMetadata {
        LanguageMetadata {
            id: "apex",
            name: "Salesforce Apex",
            version: env!("CARGO_PKG_VERSION"),
            author: "Verivus Pty Ltd",
            description: "Salesforce Apex language support with platform-specific metadata",
            tree_sitter_version: "0.24",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["cls", "trigger"]
    }

    fn language(&self) -> Language {
        tree_sitter_sfapex::apex::LANGUAGE.into()
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
        Ok(Self::extract_apex_scopes(tree, content, file_path))
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.graph_builder)
    }
}

// Scope extraction implementation
impl SalesforceApexPlugin {
    /// Extract scopes from Apex - classes, methods, triggers, inner classes.
    fn extract_apex_scopes(tree: &Tree, content: &[u8], file_path: &Path) -> Vec<Scope> {
        let mut scopes = Vec::new();
        Self::collect_apex_scopes(tree.root_node(), content, file_path, &mut scopes);

        scopes.sort_by_key(|s| (s.start_line, s.start_column));
        link_nested_scopes(&mut scopes);

        scopes
    }

    fn collect_apex_scopes(
        node: Node<'_>,
        content: &[u8],
        file_path: &Path,
        scopes: &mut Vec<Scope>,
    ) {
        let scope_info = Self::extract_apex_scope_info(node, content);

        if let Some((scope_type, name)) = scope_info {
            let start = node.start_position();
            let end = node.end_position();
            scopes.push(Scope {
                id: ScopeId::new(0),
                scope_type,
                name,
                file_path: file_path.to_path_buf(),
                start_line: start.row + 1,
                start_column: start.column,
                end_line: end.row + 1,
                end_column: end.column,
                parent_id: None,
            });
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            Self::collect_apex_scopes(child, content, file_path, scopes);
        }
    }

    fn extract_apex_scope_info(node: Node<'_>, content: &[u8]) -> Option<(String, String)> {
        match node.kind() {
            "class_declaration" => {
                let name = Self::find_apex_identifier(node, content)
                    .unwrap_or_else(|| "class".to_string());
                Some(("class".to_string(), name))
            }
            "interface_declaration" => {
                let name = Self::find_apex_identifier(node, content)
                    .unwrap_or_else(|| "interface".to_string());
                Some(("interface".to_string(), name))
            }
            "enum_declaration" => {
                let name =
                    Self::find_apex_identifier(node, content).unwrap_or_else(|| "enum".to_string());
                Some(("enum".to_string(), name))
            }
            "trigger_declaration" => {
                let name = Self::find_apex_identifier(node, content)
                    .unwrap_or_else(|| "trigger".to_string());
                Some(("trigger".to_string(), name))
            }
            "method_declaration" => {
                let name = Self::find_apex_identifier(node, content)
                    .unwrap_or_else(|| "method".to_string());
                Some(("method".to_string(), name))
            }
            "constructor_declaration" => {
                let name = Self::find_apex_identifier(node, content)
                    .unwrap_or_else(|| "constructor".to_string());
                Some(("constructor".to_string(), name))
            }
            "property_declaration" => {
                let name = Self::find_apex_identifier(node, content)
                    .unwrap_or_else(|| "property".to_string());
                Some(("property".to_string(), name))
            }
            "static_initializer" => Some(("static_block".to_string(), "static".to_string())),
            "block" | "statement_block" => {
                if let Some(parent) = node.parent() {
                    let parent_kind = parent.kind();
                    if parent_kind == "method_declaration"
                        || parent_kind == "constructor_declaration"
                        || parent_kind == "static_initializer"
                        || parent_kind == "trigger_declaration"
                        || parent_kind == "trigger_body"
                        || parent_kind == "if_statement"
                        || parent_kind == "for_statement"
                        || parent_kind == "enhanced_for_statement"
                        || parent_kind == "while_statement"
                        || parent_kind == "do_statement"
                        || parent_kind == "try_statement"
                        || parent_kind == "catch_clause"
                        || parent_kind == "finally_clause"
                    {
                        return None;
                    }
                }
                Some(("block".to_string(), "anonymous".to_string()))
            }
            _ => None,
        }
    }

    fn find_apex_identifier(node: Node<'_>, content: &[u8]) -> Option<String> {
        if let Some(name_node) = node.child_by_field_name("name")
            && let Ok(text) = name_node.utf8_text(content)
        {
            let name = text.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "identifier"
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = SalesforceApexPlugin::new();
        let metadata = plugin.metadata();

        assert_eq!(metadata.id, "apex");
        assert_eq!(metadata.name, "Salesforce Apex");
        assert!(!metadata.version.is_empty());
    }

    #[test]
    fn test_extensions() {
        let plugin = SalesforceApexPlugin::new();
        let extensions = plugin.extensions();

        assert_eq!(extensions, &["cls", "trigger"]);
    }

    #[test]
    fn test_basic_class_parsing() {
        let plugin = SalesforceApexPlugin::new();
        let apex_code = b"public class AccountService {}";

        let result = plugin.parse_ast(apex_code);
        assert!(result.is_ok(), "Should parse basic Apex class");
    }

    // ========================================================================
    // Scope Extraction Tests
    // ========================================================================

    #[test]
    fn test_extract_scopes_class() {
        let plugin = SalesforceApexPlugin::new();
        let source = b"public class AccountService {}";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("AccountService.cls"))
            .unwrap();

        let class_scope = scopes.iter().find(|s| s.scope_type == "class");
        assert!(class_scope.is_some(), "Should extract class scope");
        assert!(
            class_scope.unwrap().name.contains("AccountService"),
            "Should have class name"
        );
    }

    #[test]
    fn test_extract_scopes_method() {
        let plugin = SalesforceApexPlugin::new();
        let source = br"
public class MyClass {
    public void doSomething() {
        System.debug('hello');
    }
}
";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("MyClass.cls"))
            .unwrap();

        let method_scope = scopes.iter().find(|s| s.scope_type == "method");
        assert!(method_scope.is_some(), "Should extract method scope");
        assert!(
            method_scope.unwrap().name.contains("doSomething"),
            "Should have method name"
        );
    }

    #[test]
    fn test_extract_scopes_interface() {
        let plugin = SalesforceApexPlugin::new();
        let source = b"public interface IAccountService { void process(); }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("IAccountService.cls"))
            .unwrap();

        let interface_scope = scopes.iter().find(|s| s.scope_type == "interface");
        assert!(interface_scope.is_some(), "Should extract interface scope");
    }

    #[test]
    fn test_extract_scopes_enum() {
        let plugin = SalesforceApexPlugin::new();
        let source = b"public enum Status { ACTIVE, INACTIVE, PENDING }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("Status.cls"))
            .unwrap();

        let enum_scope = scopes.iter().find(|s| s.scope_type == "enum");
        assert!(enum_scope.is_some(), "Should extract enum scope");
    }

    #[test]
    fn test_extract_scopes_trigger() {
        let plugin = SalesforceApexPlugin::new();
        let source = b"trigger AccountTrigger on Account (before insert) { }";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("AccountTrigger.trigger"))
            .unwrap();

        let trigger_scope = scopes.iter().find(|s| s.scope_type == "trigger");
        assert!(trigger_scope.is_some(), "Should extract trigger scope");
    }

    #[test]
    fn test_extract_scopes_constructor() {
        let plugin = SalesforceApexPlugin::new();
        let source = br"
public class MyClass {
    public MyClass() {
        // constructor
    }
}
";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("MyClass.cls"))
            .unwrap();

        let constructor_scope = scopes.iter().find(|s| s.scope_type == "constructor");
        assert!(
            constructor_scope.is_some(),
            "Should extract constructor scope"
        );
    }

    #[test]
    fn test_extract_scopes_static_initializer() {
        let plugin = SalesforceApexPlugin::new();
        let source = br"
public class MyClass {
    static {
        // static initializer
    }
}
";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("MyClass.cls"))
            .unwrap();

        let static_scope = scopes.iter().find(|s| s.scope_type == "static_block");
        assert!(
            static_scope.is_some(),
            "Should extract static initializer scope"
        );
    }

    #[test]
    fn test_extract_scopes_inner_class() {
        let plugin = SalesforceApexPlugin::new();
        let source = br"
public class OuterClass {
    public class InnerClass {
        public void innerMethod() {}
    }
}
";
        let tree = plugin.parse_ast(source).unwrap();
        let scopes = plugin
            .extract_scopes(&tree, source, Path::new("OuterClass.cls"))
            .unwrap();

        let inner_class_scope = scopes
            .iter()
            .find(|s| s.scope_type == "class" && s.name == "InnerClass");
        assert!(inner_class_scope.is_some(), "Should extract inner class");
    }
}
