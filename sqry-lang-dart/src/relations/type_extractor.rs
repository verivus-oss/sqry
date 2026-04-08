//! Type extraction from Dart AST nodes.
//!
//! This module provides functions to extract type names from Dart type annotation nodes.
//! Dart has native type annotations (like Kotlin and Swift), enabling direct AST-based type extraction.
//!
//! # Supported Type Constructs
//!
//! - Simple types: `String`, `int`, `User`
//! - Generic types: `List<T>`, `Map<K, V>`
//! - Nullable types: `String?`, `User?`
//! - Future types: `Future<T>`, `FutureOr<T>`
//! - Function types: `Function`, `void Function(int)`, `String Function(int, bool)`
//! - Predefined types: `void`, `int`, `double`, `String`, `bool`, `dynamic`
//! - Nested generics: `List<Map<String, User>>`
//!
//! # Architecture
//!
//! Type extraction returns a `Vec<String>` of all referenced type names.
//! This enables creating both `TypeOf` edges (using the full type string)
//! and Reference edges (using individual type names).
//!
//! # Examples
//!
//! ```text
//! // Map<String, User> → vec!["Map", "String", "User"]
//! // List<Result<Data, Error>> → vec!["List", "Result", "Data", "Error"]
//! // Future<String> → vec!["Future", "String"]
//! ```

use tree_sitter::Node;

/// Extract all type names referenced in a Dart type node.
///
/// Returns a vector of type names that should have Reference edges created.
/// For simple types, returns a single element. For complex types (generics,
/// function types, etc.), returns all nested type names.
///
/// # Arguments
///
/// * `node` - The type annotation AST node from tree-sitter-dart
/// * `content` - The source file bytes for text extraction
///
/// # Returns
///
/// Vector of type names referenced in the type annotation.
/// Empty vector if the type cannot be parsed.
///
/// # Examples
///
/// ```text
/// // Simple type: String
/// extract_all_type_names_from_dart_type(node, content) → vec!["String"]
///
/// // Nullable: User?
/// extract_all_type_names_from_dart_type(node, content) → vec!["User"]
///
/// // Generic: List<String>
/// extract_all_type_names_from_dart_type(node, content) → vec!["List", "String"]
///
/// // Map: Map<String, User>
/// extract_all_type_names_from_dart_type(node, content) → vec!["Map", "String", "User"]
///
/// // Function type: void Function(int)
/// extract_all_type_names_from_dart_type(node, content) → vec!["void", "int"]
/// ```
#[must_use]
pub fn extract_all_type_names_from_dart_type(node: Node, content: &[u8]) -> Vec<String> {
    match node.kind() {
        // Simple type identifiers: String, int, User, List
        // For generic types like List<String>, the type_identifier and type_arguments
        // might be siblings under a parent node
        "type_identifier" => {
            let mut types = Vec::new();

            // Add the base type name
            if let Ok(type_name) = node.utf8_text(content) {
                types.push(clean_type_name(type_name));
            }

            // Check parent for type_arguments sibling
            if let Some(parent) = node.parent() {
                let mut cursor = parent.walk();
                for sibling in parent.children(&mut cursor) {
                    if sibling.kind() == "type_arguments" {
                        types.extend(extract_all_type_names_from_dart_type(sibling, content));
                    }
                }
            }

            types
        }

        // Predefined types: void, int, double, String, bool, dynamic
        "predefined_type" | "void_type" => {
            if let Ok(type_name) = node.utf8_text(content) {
                vec![clean_type_name(type_name)]
            } else {
                Vec::new()
            }
        }

        // Generic type arguments: <String> in List<String>
        "type_arguments" => {
            let mut types = Vec::new();
            let mut cursor = node.walk();

            for child in node.children(&mut cursor) {
                if is_type_node(child.kind()) {
                    types.extend(extract_all_type_names_from_dart_type(child, content));
                }
            }

            types
        }

        // Scoped type identifier: library.ClassName or package:foo/bar.dart.ClassName
        "scoped_type_identifier" => {
            // Extract just the type name (rightmost part after dots)
            if let Some(name_node) = node.child_by_field_name("name")
                && let Ok(type_name) = name_node.utf8_text(content)
            {
                return vec![clean_type_name(type_name)];
            }

            // Fallback: extract all nested types
            let mut types = Vec::new();
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if is_type_node(child.kind()) {
                    types.extend(extract_all_type_names_from_dart_type(child, content));
                }
            }
            types
        }

        // Function type: void Function(int), String Function(int, bool)
        "function_type" | "generic_function_type" => extract_function_type_names(node, content),

        // Nullable type: String?, User? → extract String, User (without ?)
        // Dart nullable types have the ? as part of the text, not a separate node
        // So we extract text and clean it
        _ if node
            .utf8_text(content)
            .ok()
            .is_some_and(|text| text.ends_with('?')) =>
        {
            // This is a nullable type, extract the base type
            extract_nullable_type(node, content)
        }

        // Fallback: try to extract text as a single type name
        _ => {
            if let Ok(text) = node.utf8_text(content) {
                let text = text.trim();
                if !text.is_empty() && is_valid_type_name(text) {
                    vec![clean_type_name(text)]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        }
    }
}

/// Extract the full type string from a Dart type node.
///
/// Used for creating `TypeOf` edges with the complete type signature.
///
/// # Arguments
///
/// * `node` - The type annotation AST node
/// * `content` - The source file bytes
///
/// # Returns
///
/// The full type string (e.g., "List<Map<String, User>>"), or None if extraction fails.
///
/// # Examples
///
/// ```text
/// // Simple: String → "String"
/// // Generic: List<String> → "List<String>"
/// // Nested: List<Map<String, User>> → "List<Map<String, User>>"
/// ```
#[must_use]
pub fn extract_type_string(node: Node, content: &[u8]) -> Option<String> {
    node.utf8_text(content).ok().map(str::to_string)
}

/// Extract type names from a nullable type.
///
/// Dart nullable types have ? suffix in the text: String?, User?
/// This function extracts the base type without the ?.
fn extract_nullable_type(node: Node, content: &[u8]) -> Vec<String> {
    // Try to find child type nodes first
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if is_type_node(child.kind()) {
            return extract_all_type_names_from_dart_type(child, content);
        }
    }

    // Fallback: extract text and clean
    if let Ok(text) = node.utf8_text(content) {
        vec![clean_type_name(text)]
    } else {
        Vec::new()
    }
}

/// Extract type names from a `function_type` node.
///
/// Examples:
/// - `void Function(int)` → vec!["void", "int"]
/// - `String Function(int, bool)` → vec!["String", "int", "bool"]
/// - `Function` → vec![]
fn extract_function_type_names(node: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        match child.kind() {
            // Return type (appears before "Function" keyword)
            // Type arguments: generic function type parameters
            "type_identifier" | "predefined_type" | "void_type" | "type_arguments" => {
                types.extend(extract_all_type_names_from_dart_type(child, content));
            }

            // Function parameters: formal_parameter_list
            "formal_parameter_list" => {
                let mut param_cursor = child.walk();
                for param in child.children(&mut param_cursor) {
                    if is_type_node(param.kind()) {
                        types.extend(extract_all_type_names_from_dart_type(param, content));
                    } else if param.kind() == "formal_parameter"
                        || param.kind() == "normal_parameter"
                    {
                        // Descend into parameter to find type annotation
                        let mut inner_cursor = param.walk();
                        for inner_child in param.children(&mut inner_cursor) {
                            if is_type_node(inner_child.kind()) {
                                types.extend(extract_all_type_names_from_dart_type(
                                    inner_child,
                                    content,
                                ));
                            }
                        }
                    }
                }
            }

            // Recurse into other type nodes
            _ if is_type_node(child.kind()) => {
                types.extend(extract_all_type_names_from_dart_type(child, content));
            }

            _ => {}
        }
    }

    types
}

/// Check if a node kind represents a type node.
#[must_use]
pub fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "type_identifier"
            | "predefined_type"
            | "void_type"
            | "type_arguments"
            | "scoped_type_identifier"
            | "function_type"
            | "generic_function_type"
            | "nullable_type"
    )
}

/// Check if a string looks like a valid Dart type name.
///
/// Valid type names:
/// - Start with uppercase letter (by convention for user types)
/// - Or are lowercase builtin types (int, double, bool, etc.)
/// - Contain only alphanumeric characters and underscores
/// - Exclude Dart keywords that aren't types
fn is_valid_type_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    // Remove nullable marker for validation
    let s = s.trim_end_matches('?');

    // Exclude common keywords that aren't type names
    if matches!(
        s,
        "var"
            | "final"
            | "const"
            | "class"
            | "enum"
            | "extends"
            | "implements"
            | "with"
            | "mixin"
            | "abstract"
            | "interface"
            | "base"
            | "sealed"
            | "return"
            | "if"
            | "else"
            | "for"
            | "while"
            | "do"
            | "break"
            | "continue"
            | "switch"
            | "case"
            | "default"
            | "throw"
            | "try"
            | "catch"
            | "finally"
            | "async"
            | "await"
            | "sync"
            | "yield"
            | "import"
            | "export"
            | "part"
            | "library"
            | "show"
            | "hide"
            | "as"
            | "is"
            | "new"
            | "this"
            | "super"
            | "null"
            | "true"
            | "false"
            | "static"
            | "late"
            | "required"
            | "covariant"
            | "factory"
            | "operator"
            | "get"
            | "set"
            | "extension"
            | "on"
            | "deferred"
    ) {
        return false;
    }

    // Valid type names start with alphanumeric character
    s.chars().next().is_some_and(char::is_alphanumeric)
}

/// Clean a type name by removing common artifacts.
///
/// - Remove leading/trailing whitespace
/// - Remove `?` suffix (nullable marker)
/// - Remove generic parameter brackets if they're at the end
/// - Remove package qualifiers (keep just the type name)
#[must_use]
pub fn clean_type_name(s: &str) -> String {
    let s = s.trim();

    // Remove nullable marker
    let s = s.trim_end_matches('?');

    // If the type has generic parameters, extract just the base name
    let s = if let Some(bracket_pos) = s.find('<') {
        &s[..bracket_pos]
    } else {
        s
    };

    // Remove package qualifiers (keep just the type name)
    // Example: dart:core.String → String
    // Example: package:foo/bar.User → User
    let s = if let Some(dot_pos) = s.rfind('.') {
        &s[dot_pos + 1..]
    } else {
        s
    };

    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqry_core::plugin::LanguagePlugin;

    fn parse_dart_type(type_str: &str) -> tree_sitter::Tree {
        let source = format!("final {type_str} x = null;");
        let plugin = crate::DartPlugin::default();
        plugin
            .parse_ast(source.as_bytes())
            .expect("Failed to parse Dart code")
    }

    fn extract_from_type_str(type_str: &str) -> Vec<String> {
        let source = format!("final {type_str} x = null;");
        let tree = parse_dart_type(type_str);

        // Find the type annotation node - need to traverse deeply
        #[allow(clippy::items_after_statements)] // Items near usage for clarity
        fn find_type_node(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
            if is_type_node(node.kind()) {
                return Some(node);
            }

            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if let Some(type_node) = find_type_node(child) {
                    return Some(type_node);
                }
            }

            None
        }

        let root = tree.root_node();
        if let Some(type_node) = find_type_node(root) {
            extract_all_type_names_from_dart_type(type_node, source.as_bytes())
        } else {
            Vec::new()
        }
    }

    #[test]
    fn test_simple_type() {
        let types = extract_from_type_str("String");
        assert_eq!(types, vec!["String"]);
    }

    #[test]
    fn test_predefined_type() {
        let types = extract_from_type_str("int");
        assert_eq!(types, vec!["int"]);
    }

    #[test]
    fn test_nullable_type() {
        let types = extract_from_type_str("String?");
        assert_eq!(types, vec!["String"]);
    }

    #[test]
    fn test_generic_list() {
        let types = extract_from_type_str("List<String>");
        assert!(types.contains(&"List".to_string()));
        assert!(types.contains(&"String".to_string()));
    }

    #[test]
    fn test_generic_map() {
        let types = extract_from_type_str("Map<String, int>");
        assert!(types.contains(&"Map".to_string()));
        assert!(types.contains(&"String".to_string()));
        assert!(types.contains(&"int".to_string()));
    }

    #[test]
    fn test_clean_type_name() {
        assert_eq!(clean_type_name("String"), "String");
        assert_eq!(clean_type_name("String?"), "String");
        assert_eq!(clean_type_name("List<String>"), "List");
        assert_eq!(clean_type_name("dart:core.String"), "String");
        assert_eq!(clean_type_name("  String  "), "String");
    }

    #[test]
    fn test_is_valid_type_name() {
        assert!(is_valid_type_name("String"));
        assert!(is_valid_type_name("int"));
        assert!(is_valid_type_name("User"));
        assert!(is_valid_type_name("MyClass123"));

        assert!(!is_valid_type_name(""));
        assert!(!is_valid_type_name("var"));
        assert!(!is_valid_type_name("class"));
        assert!(!is_valid_type_name("return"));
    }
}
