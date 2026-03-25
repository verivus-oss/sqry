//! Type extraction from Swift AST nodes.
//!
//! This module provides functions to extract type names from Swift type annotation nodes.
//! Swift has native type annotations (like Go), enabling direct AST-based type extraction.
//!
//! # Supported Type Constructs
//!
//! - Simple types: `String`, `Int`, `User`
//! - Optional types: `String?`, `User?`
//! - Array types: `[String]`, `Array<User>`
//! - Dictionary types: `[String: User]`, `Dictionary<K, V>`
//! - Tuple types: `(String, Int)`, `(x: String, y: Int)`
//! - Function types: `(Int) -> String`, `() -> Void`
//! - Generic types: `Array<T>`, `Result<T, E>`
//! - Protocol composition: `Codable & Sendable`
//! - Some/any types: `some View`, `any Codable`
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
//! // [String: User] → vec!["String", "User"]
//! // Result<Data, Error> → vec!["Result", "Data", "Error"]
//! // some View → vec!["View"]
//! ```

use tree_sitter::Node;

/// Extract all type names referenced in a Swift type node.
///
/// Returns a vector of type names that should have Reference edges created.
/// For simple types, returns a single element. For complex types (arrays,
/// dictionaries, generics), returns all nested type names.
///
/// # Arguments
///
/// * `node` - The type annotation AST node from tree-sitter-swift
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
/// extract_type_names_from_swift_type(node, content) → vec!["String"]
///
/// // Optional: User?
/// extract_type_names_from_swift_type(node, content) → vec!["User"]
///
/// // Array: [String]
/// extract_type_names_from_swift_type(node, content) → vec!["String"]
///
/// // Dictionary: [String: User]
/// extract_type_names_from_swift_type(node, content) → vec!["String", "User"]
///
/// // Generic: Result<Data, Error>
/// extract_type_names_from_swift_type(node, content) → vec!["Result", "Data", "Error"]
/// ```
#[must_use]
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub fn extract_type_names_from_swift_type(node: Node, content: &[u8]) -> Vec<String> {
    match node.kind() {
        // Simple identifier: String, Int, User
        "simple_identifier" | "type_identifier" => {
            if let Ok(type_name) = node.utf8_text(content) {
                vec![type_name.to_string()]
            } else {
                Vec::new()
            }
        }

        // User type: custom types, may have generic parameters
        // Example: Array<T>, User, Optional<String>
        "user_type" => extract_user_type(node, content),

        // Optional type: Type? → extract Type (without ?)
        // Some type: some View → extract View
        // Any type: any Codable → extract Codable
        // Opaque type: some P → extract P (similar to some_type)
        // Existential type: any P → extract P (similar to any_type)
        // Implicitly unwrapped optional: Type! → extract Type
        "optional_type"
        | "some_type"
        | "any_type"
        | "opaque_type"
        | "existential_type"
        | "implicitly_unwrapped_optional_type" => {
            if let Some(wrapped_type) = node.child_by_field_name("type") {
                extract_type_names_from_swift_type(wrapped_type, content)
            } else if let Some(wrapped_type) = node.named_child(0) {
                extract_type_names_from_swift_type(wrapped_type, content)
            } else {
                Vec::new()
            }
        }

        // Array type: [Element] → extract Element
        "array_type" => {
            if let Some(element_type) = node.child_by_field_name("element") {
                extract_type_names_from_swift_type(element_type, content)
            } else {
                // Fallback: look for first child that looks like a type
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if is_type_node(child.kind()) {
                        return extract_type_names_from_swift_type(child, content);
                    }
                }
                Vec::new()
            }
        }

        // Dictionary type: [Key: Value] → extract Key, Value
        "dictionary_type" => {
            let mut types = Vec::new();

            // Try field-based access first
            if let Some(key_type) = node.child_by_field_name("key") {
                types.extend(extract_type_names_from_swift_type(key_type, content));
            }
            if let Some(value_type) = node.child_by_field_name("value") {
                types.extend(extract_type_names_from_swift_type(value_type, content));
            }

            // Fallback: extract all type children
            if types.is_empty() {
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    if is_type_node(child.kind()) {
                        types.extend(extract_type_names_from_swift_type(child, content));
                    }
                }
            }

            types
        }

        // Tuple type: (A, B) → extract A, B
        // Also handles labeled tuples: (x: Int, y: Int)
        "tuple_type" => {
            let mut types = Vec::new();
            let mut cursor = node.walk();

            for child in node.children(&mut cursor) {
                // tuple_type_element or tuple_type_item nodes
                if child.kind().contains("tuple") && child.kind().contains("element")
                    || child.kind().contains("tuple") && child.kind().contains("item")
                {
                    // Extract type from tuple element
                    let mut elem_cursor = child.walk();
                    for elem_child in child.children(&mut elem_cursor) {
                        if is_type_node(elem_child.kind()) {
                            types.extend(extract_type_names_from_swift_type(elem_child, content));
                        }
                    }
                } else if is_type_node(child.kind()) {
                    types.extend(extract_type_names_from_swift_type(child, content));
                }
            }

            types
        }

        // Function type: (Int) -> String → extract Int, String
        "function_type" => {
            let mut types = Vec::new();
            let mut cursor = node.walk();

            for child in node.children(&mut cursor) {
                match child.kind() {
                    // Parameter list: extract parameter types
                    "tuple_type" => {
                        types.extend(extract_type_names_from_swift_type(child, content));
                    }
                    // Return type
                    _ if is_type_node(child.kind()) => {
                        types.extend(extract_type_names_from_swift_type(child, content));
                    }
                    _ => {}
                }
            }

            types
        }

        // Protocol composition: A & B → extract A, B
        "protocol_composition_type" => {
            let mut types = Vec::new();
            let mut cursor = node.walk();

            for child in node.children(&mut cursor) {
                if is_type_node(child.kind()) {
                    types.extend(extract_type_names_from_swift_type(child, content));
                }
            }

            types
        }

        // Metatype: Type.Type → extract Type
        // FIX M-2 (Iteration 3): Handle both "metatype" and "metatype_type" node kinds
        "metatype_type" | "metatype" => {
            if let Some(base_type) = node.child_by_field_name("type") {
                extract_type_names_from_swift_type(base_type, content)
            } else if let Some(base_type) = node.named_child(0) {
                extract_type_names_from_swift_type(base_type, content)
            } else {
                Vec::new()
            }
        }

        // Attributed type: @escaping (Int) -> Void → extract function type
        "attributed_type" => {
            // Skip attributes, extract the base type
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if is_type_node(child.kind()) {
                    return extract_type_names_from_swift_type(child, content);
                }
            }
            Vec::new()
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

/// Extract type names from a `user_type` node, handling generic parameters.
///
/// Examples:
/// - `User` → vec!["User"]
/// - `Array<String>` → vec!["Array", "String"]
/// - `Result<Data, Error>` → vec!["Result", "Data", "Error"]
fn extract_user_type(node: Node, content: &[u8]) -> Vec<String> {
    let mut types = Vec::new();

    // Get the base type name (the identifier before <)
    if let Some(name_node) = node.child_by_field_name("name") {
        if let Ok(base_name) = name_node.utf8_text(content) {
            types.push(base_name.to_string());
        }
    } else {
        // Fallback: extract identifier from children
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if (child.kind() == "simple_identifier" || child.kind() == "type_identifier")
                && let Ok(name) = child.utf8_text(content)
            {
                types.push(name.to_string());
                break;
            }
        }
    }

    // Extract generic arguments if present
    if let Some(type_args_node) = node.child_by_field_name("type_arguments") {
        let mut cursor = type_args_node.walk();
        for child in type_args_node.children(&mut cursor) {
            if is_type_node(child.kind()) {
                types.extend(extract_type_names_from_swift_type(child, content));
            }
        }
    } else {
        // Fallback: look for generic_argument_clause
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind().contains("generic") || child.kind().contains("type_arguments") {
                let mut arg_cursor = child.walk();
                for arg_child in child.children(&mut arg_cursor) {
                    if is_type_node(arg_child.kind()) {
                        types.extend(extract_type_names_from_swift_type(arg_child, content));
                    }
                }
            }
        }
    }

    types
}

/// Check if a node kind represents a type node.
fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "simple_identifier"
            | "type_identifier"
            | "user_type"
            | "optional_type"
            | "array_type"
            | "dictionary_type"
            | "tuple_type"
            | "function_type"
            | "protocol_composition_type"
            | "metatype_type"
            | "metatype"  // FIX M-2 (Iteration 3): Handle both metatype node kinds
            | "some_type"
            | "any_type"
            | "attributed_type"
            | "implicitly_unwrapped_optional_type"
            | "opaque_type"
            | "existential_type"
    )
}

/// Check if a string looks like a valid Swift type name.
///
/// Valid type names:
/// - Start with uppercase letter
/// - Contain only alphanumeric characters and underscores
/// - Exclude Swift keywords that aren't types
fn is_valid_type_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    // Exclude common keywords that aren't type names
    if matches!(
        s,
        "var"
            | "let"
            | "func"
            | "class"
            | "struct"
            | "enum"
            | "protocol"
            | "extension"
            | "import"
            | "return"
            | "if"
            | "else"
            | "for"
            | "while"
            | "switch"
            | "case"
            | "default"
            | "break"
            | "continue"
            | "guard"
            | "defer"
            | "throws"
            | "rethrows"
            | "throw"
            | "try"
            | "catch"
            | "async"
            | "await"
            | "some"
            | "any"
            | "inout"
    ) {
        return false;
    }

    // Valid type names typically start with uppercase (Swift convention)
    // But also accept lowercase for builtin types (int, string, etc.)
    s.chars().next().is_some_and(char::is_alphanumeric)
}

/// Clean a type name by removing common artifacts.
///
/// - Remove leading/trailing whitespace
/// - Remove `?` and `!` suffixes (optional/unwrapped markers)
/// - Remove generic parameter brackets if they're at the end
fn clean_type_name(s: &str) -> String {
    let s = s.trim();
    let s = s.trim_end_matches('?').trim_end_matches('!');

    // If the type has generic parameters, extract just the base name
    if let Some(bracket_pos) = s.find('<') {
        s[..bracket_pos].to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse_swift_type(type_str: &str) -> tree_sitter::Tree {
        let source = format!("var x: {type_str}");
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_swift::LANGUAGE.into())
            .expect("Failed to load Swift grammar");
        parser
            .parse(source.as_bytes(), None)
            .expect("Failed to parse Swift code")
    }

    fn extract_from_type_str(type_str: &str) -> Vec<String> {
        let source = format!("var x: {type_str}");
        let tree = parse_swift_type(type_str);

        // Find the type_annotation node
        let root = tree.root_node();
        let mut cursor = root.walk();

        for child in root.children(&mut cursor) {
            if child.kind() == "property_declaration" {
                let mut prop_cursor = child.walk();
                for prop_child in child.children(&mut prop_cursor) {
                    if prop_child.kind() == "type_annotation" {
                        // Type annotation has a child that's the actual type
                        let mut type_cursor = prop_child.walk();
                        for type_child in prop_child.children(&mut type_cursor) {
                            if is_type_node(type_child.kind()) {
                                return extract_type_names_from_swift_type(
                                    type_child,
                                    source.as_bytes(),
                                );
                            }
                        }
                    }
                }
            }
        }

        Vec::new()
    }

    #[test]
    fn test_simple_type() {
        let types = extract_from_type_str("String");
        assert_eq!(types, vec!["String"]);

        let types = extract_from_type_str("Int");
        assert_eq!(types, vec!["Int"]);

        let types = extract_from_type_str("User");
        assert_eq!(types, vec!["User"]);
    }

    #[test]
    fn test_optional_type() {
        let types = extract_from_type_str("String?");
        assert!(
            types.contains(&"String".to_string()),
            "Expected String in {types:?}"
        );
    }

    #[test]
    fn test_array_type() {
        let types = extract_from_type_str("[String]");
        assert!(
            types.contains(&"String".to_string()),
            "Expected String in {types:?}"
        );
    }

    #[test]
    fn test_dictionary_type() {
        let types = extract_from_type_str("[String: Int]");
        assert!(
            types.contains(&"String".to_string()),
            "Expected String in {types:?}"
        );
        assert!(
            types.contains(&"Int".to_string()),
            "Expected Int in {types:?}"
        );
    }

    #[test]
    fn test_generic_type() {
        let types = extract_from_type_str("Array<String>");
        assert!(
            types.contains(&"Array".to_string()),
            "Expected Array in {types:?}"
        );
        assert!(
            types.contains(&"String".to_string()),
            "Expected String in {types:?}"
        );
    }

    #[test]
    fn test_tuple_type() {
        let types = extract_from_type_str("(Int, String)");
        assert!(
            types.contains(&"Int".to_string()),
            "Expected Int in {types:?}"
        );
        assert!(
            types.contains(&"String".to_string()),
            "Expected String in {types:?}"
        );
    }

    #[test]
    fn test_is_valid_type_name() {
        assert!(is_valid_type_name("String"));
        assert!(is_valid_type_name("Int"));
        assert!(is_valid_type_name("User"));
        assert!(is_valid_type_name("MyType"));

        assert!(!is_valid_type_name("var"));
        assert!(!is_valid_type_name("let"));
        assert!(!is_valid_type_name("func"));
        assert!(!is_valid_type_name(""));
    }

    #[test]
    fn test_clean_type_name() {
        assert_eq!(clean_type_name("String"), "String");
        assert_eq!(clean_type_name("String?"), "String");
        assert_eq!(clean_type_name("String!"), "String");
        assert_eq!(clean_type_name("Array<String>"), "Array");
        assert_eq!(clean_type_name("  User  "), "User");
    }
}
